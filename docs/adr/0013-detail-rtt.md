# ADR-0013: Detail RTT — per-ack RTT series, engine-smoothed SRTT, and the kernel-srtt overlay seam (M8)

> Status: Accepted
> Date: 2026-07-01

## Context

M6 ([ADR-0011](0011-detail-seq-timeline-and-rendering.md)) and M7
([ADR-0012](0012-detail-inflight-and-view-switcher.md)) established the detail-pane shape: a
**dedicated per-connection series** derived on the replay path and stored in the `Timeline`
(rather than retaining the full `MetricSample` series, which would churn the frozen M3 oracle
goldens), plus a **pure projection** in `tcpvisr-tui` mapping `(series, x_span, focus dir,
cursor, cell rectangle)` to a grid of `Mark`s, reached through a `Tab` view-switcher.

M8 (design §6, §10.M8, issue #11) adds the third detail view: the per-connection **RTT** graph.
Its Definition of Done is **"per-ack RTT samples + smoothed line"** — so unlike M7's single wire
sawtooth, M8 plots two wire-derived quantities: the raw per-ack RTT measurements and a *smoothed*
RTT line. This ADR decides:

1. **What data the RTT graph consumes, where it is derived and stored, and how each sample is
   directionally attributed** — the RTT of a data flow is measured on ACKs that travel in the
   *opposite* direction, so the naive "tag the sample with the ACK's own direction" (what
   `MetricSample.dir` already does) attributes every RTT to the wrong flow for a focus-direction
   plot.
2. **What "smoothed line" is, and where the smoothing lives.** M8 introduces a *new derived
   quantity* (a smoothed RTT), not present in `MetricSample`.
3. **How the two wire series plus a future kernel-srtt overlay are rendered** in the M6/M7
   mark-grid model.
4. **What the kernel-srtt overlay seam means concretely** on the replay-only path — design
   §10.M12 says kernel enrichment "overlays on M7/M8", so M8's view, like M7's, carries a typed,
   tested overlay hook that is empty until M12.

The engine already computes a per-ACK RTT under Karn's algorithm as `MetricSample.rtt:
Option<Nanos>` (design §10.M3, ADR-0007): on an ACK that advances the acked direction's
cumulative frontier, the oldest unacked send in that direction is paired with the ACK time; a
retransmit clears the pending queue so no RTT is sampled across it. M8 does not re-derive RTT; it
**retains** and **smooths** what the engine already produces.

## Decision

### 1. A dedicated per-connection `RttSample` series in the engine, attributed to the measured flow

Mirroring `StateSample`/`SeqSample`/`InFlightSample`, we add an **`RttSample`** record and collect
one series per connection on the replay path:

```rust
pub struct RttSample {
    pub t: Nanos,       // the ACK's timestamp (when the round-trip was observed)
    pub dir: SampleDir, // the measured data-flow direction (the sender being acked)
    pub rtt: Nanos,     // the raw per-ack RTT (Karn-paired; the engine's MetricSample.rtt)
    pub srtt: Nanos,    // the smoothed RTT over `dir`'s samples so far (see §2)
}
```

- **`dir` is the measured flow, not the ACK's own direction.** `MetricSample.rtt` is produced
  while processing an ACK segment, and `MetricSample.dir` is tagged with that ACK segment's
  direction. But the round-trip that RTT measures belongs to the *opposite* direction — the
  sender whose data this ACK acknowledges. The detail views plot a single **focus direction**
  (the higher-byte data flow; ADR-0011/0012 §3.6); a mostly-O2R transfer's RTTs arrive on R2O
  ACKs. If `RttSample.dir` copied `MetricSample.dir`, filtering the series by the focus direction
  would select the *wrong* flow's RTTs (or none). So the collector tags each `RttSample` with
  **`opposite(sample.dir)`** — the flow whose data was acked. This is the load-bearing
  correctness decision of M8; success criterion 1 asserts it.
- **`rtt` is the engine's Karn-paired `MetricSample.rtt`** — the collector reads it, it does not
  re-derive RTT. Only samples where `rtt.is_some()` become `RttSample`s (retransmit-blocked and
  non-advancing ACKs contribute nothing, exactly as M3 already decides).
- **Collection is gated by a new `EngineConfig.collect_rtt_timeline` flag** (default `false`),
  orthogonal to the state/seq/inflight flags. On the replay path all four are on. Each retained
  `RttSample` counts against the existing `max_samples` ceiling and shares the fail-fast
  `SampleCeiling` path (design §7).
- **`Timeline` owns the series and exposes `rtt_series(id) -> &[RttSample]`**, keyed by `ConnId`,
  exactly like `inflight_series`. `Timeline::with_seq` is extended to carry the fourth series;
  `Timeline::new` still delegates with empty vectors so the M4/M5 fixtures are untouched.

`MetricSample` and the `metrics` command's JSON stay untouched, so the hand-derived M3 oracle
goldens are undisturbed (the property ADR-0010/0011/0012 all preserved).

### 2. The smoothed line is an engine-derived integer EWMA (RFC 6298, α = 1/8)

The "smoothed line" is a **new derived metric**, computed in the engine (the metric-derivation
home; ADR-0002, ADR-0007), not in the TUI. For each measured direction the collector maintains a
running smoothed RTT and stores it on every `RttSample`:

- **First measurement:** `srtt = rtt`.
- **Subsequent:** `srtt = (7 * srtt + rtt) / 8` — the standard TCP exponentially-weighted moving
  average of RFC 6298 with `α = 1/8`, evaluated in integer nanoseconds (computed in `u128` to
  avoid intermediate overflow, then narrowed) so the result is exact and deterministic, with no
  floating point. State is per measured direction (`[Option<Nanos>; 2]` on the tracker's
  connection record), seeded independently for each flow.

Choosing the kernel's own smoothing formula is deliberate: design §10.M12 will **overlay the
kernel's real `srtt`** (from `sock_diag`) on this same view. A wire-smoothed line computed with
the *same* EWMA is directly comparable to that kernel line; a different smoother (boxcar mean,
median) would make the wire and kernel lines incommensurable. Carrying both `rtt` and `srtt` on
one `RttSample` (they share the ACK's timestamp — each raw measurement produces exactly one
smoothed update) lets the projection plot two time-aligned series from a single vector.

### 3. Render raw points and the smoothed line as distinct marks in the M6/M7 grid

A **pure projection** in `tcpvisr-tui` (`rtt.rs`, a sibling of `detail.rs`/`inflight.rs`) returns
the same `Mark { col, row, glyph, series }` grid model:

- **`Series { Raw, Smoothed, Kernel }`.** Each focus-direction `RttSample` with `t ≤ T` emits a
  `Raw` mark at `(col(t), row(rtt))` and a `Smoothed` mark at `(col(t), row(srtt))`, each with its
  own glyph. Across columns the `Raw` marks scatter the per-ack measurements and the `Smoothed`
  marks trace the EWMA line.
- **Y axis spans `[0, max_rtt]`** where `max_rtt` is the maximum over the focus direction's `rtt`,
  `srtt`, **and** overlay samples (§4) — so a diverging kernel overlay is not clamped. X spans
  `[opened_at, effective_end]` from `Timeline::x_span`, shared with M6/M7. Neither rescales with
  the cursor.
- **Column bucketing keeps the numeric maximum per (column, series)** — the same rule and
  `O(cells)` bound as M7 (RTT has no glyph taxonomy; the meaningful summary of a dense column is
  its peak). Bucketing is applied **only to revealed samples** (`t ≤ T`), so scrubbing never
  shows a future peak.
- **Fixed axes, reveal-to-`T`, and a vertical cursor column** are reused unchanged from M6/M7.

Rendering all series as top-edge marks (rather than a braille `Canvas` line, or a
baseline-connected polyline) follows ADR-0012 §2: it keeps the projection pure, integer-only, and
directly unit-testable. A connected polyline for the smoothed series is deferred visual polish
behind the same projection seam, exactly as M7 deferred area fill.

### 4. A typed, tested kernel-srtt overlay seam — empty on replay

Design §10.M12 lists kernel `srtt` among the enrichment that "overlays on M7/M8". Following M7's
precedent (ADR-0012 §4), M8 ships the overlay **seam** now — concrete and non-phantom — so M12
fills it without touching the frozen projection:

- The projection accepts a **second, optional overlay series** of `RttSample`s; each overlay
  sample's `srtt` is plotted as a `Kernel`-tagged mark with a distinct glyph/colour. Wire RTT and
  kernel srtt are never conflated.
- **`max_rtt` is scaled over wire ∪ overlay**, so a kernel srtt above the wire maximum is not
  clamped to the top row. On replay the overlay is empty, so `max_rtt` equals the wire maximum;
  the correction only matters once M12 fills the overlay.
- On the replay path the overlay is **empty**, so no `Kernel` marks are drawn. A projection unit
  test passes a synthetic overlay and asserts the distinctly-tagged, unclamped marks, so the hook
  is exercised code, not dead code.

### 5. `Tab` gains the third view; M9 still finalizes the switcher

`DetailView` gains a third variant and `cycle_detail_view` extends the cycle:

```rust
pub enum DetailView { TimeSequence, InFlight, Rtt }
// Tab: TimeSequence -> InFlight -> Rtt -> TimeSequence
```

`Enter`/`Esc` keep their open/close meaning; `Tab` in filter mode stays inert. M9 (throughput)
adds the fourth variant and "finalizes" the switcher/layout (design §10.M9); M8 is the minimal
increment that makes the RTT view reachable.

## Consequences

- The replay path now retains a fourth per-connection series (`RttSample`) alongside state, seq,
  and in-flight, all bounded by `max_samples` with the existing fail-fast. RTT samples are sparse
  (one per Karn-paired advancing ACK), so the added memory is smaller than the seq/in-flight
  series.
- The engine gains no I/O and no clock read (ADR-0002 preserved); the RTT series and its EWMA are
  derived purely from segments via the existing `MetricState` output.
- The RTT projection is a third pure module, unit-testable without a terminal; it duplicates the
  small `col`/`row` helpers of M6/M7 rather than prematurely extracting a shared utility (the
  numeric-max bucketing matches M7, but the two-wire-series-plus-overlay shape and the RTT Y-axis
  formatter differ enough that a forced abstraction would be premature — written three times is
  the threshold, and the shared shape can be extracted in M9 when the fourth view lands).
- The kernel-srtt overlay seam is typed and tested but unpopulated until M12; the ADR records it
  as a deliberate hook so it is not mistaken for a phantom feature.

## Considered & rejected

- **Tag `RttSample.dir` with the ACK segment's direction (copy `MetricSample.dir`).** Rejected:
  the RTT measures the *opposite* (acked-sender) flow, so a focus-direction plot would select the
  wrong flow's samples or none. Tagging by the measured flow is the correctness fix (§1).
- **Compute the smoothed line in the TUI projection.** Rejected: smoothing is metric derivation,
  which lives in the pure engine (ADR-0002/0007), and the M12 kernel-srtt overlay must be
  computed the *same* way to be comparable. Deriving `srtt` in the engine keeps the projection a
  pure plotter with no metric logic, consistent with M6/M7.
- **Use a different smoother (simple moving average, median, or a window).** Rejected: RFC 6298's
  EWMA (α = 1/8) is TCP's own smoothing and matches the kernel `srtt` the overlay will show; any
  other smoother makes the wire and kernel lines incommensurable and adds a windowing knob with
  no requirement behind it.
- **Add `srtt` (or a `rtt` series) to `MetricSample` / the `metrics` JSON.** Rejected for the same
  reason ADR-0010/0011/0012 rejected the analogous move: it churns the frozen M3 oracle goldens.
  A dedicated `RttSample` keeps `MetricSample` and the oracle frozen.
- **Plot only the smoothed line (drop the raw points), or only the raw points (drop smoothing).**
  Rejected: the DoD is explicitly "per-ack RTT samples **and** smoothed line"; the raw scatter
  shows variance and outliers the EWMA hides, and the line shows the trend the scatter obscures.
- **Fold RTT collection into an existing collect flag.** Rejected: the flags stay orthogonal and
  independently unit-testable, as the other three already are.
- **Render the smoothed line as a connected polyline / braille `Canvas`.** Deferred, not adopted:
  top-edge marks are the minimal, M6/M7-consistent, pure/testable form (ADR-0012 §2); a connected
  line is later polish behind the same projection seam.
- **Defer the kernel-srtt overlay seam to M12 and ship the RTT view with no overlay hook.**
  Rejected: design §10.M12 says enrichment overlays M8, and M7 set the precedent of building the
  typed, tested seam at the view's own milestone so M12 fills data without reworking the frozen
  projection. The seam is exercised by a synthetic-overlay unit test, so it is not dead code.
