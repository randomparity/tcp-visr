# ADR-0014: Detail Throughput/Goodput — windowed total + goodput series, the goodput derivation, and the finalized four-view switcher (M9)

> Status: Accepted
> Date: 2026-07-01

## Context

M6 ([ADR-0011](0011-detail-seq-timeline-and-rendering.md)), M7
([ADR-0012](0012-detail-inflight-and-view-switcher.md)), and M8
([ADR-0013](0013-detail-rtt.md)) established the detail-pane shape: a **dedicated per-connection
series** derived on the replay path and stored in the `Timeline` (rather than retaining the full
`MetricSample` series, which would churn the frozen M3 oracle goldens), plus a **pure projection**
in `tcpvisr-tui` mapping `(series, x_span, focus dir, cursor, cell rectangle)` to a grid of
`Mark`s, reached through a `Tab` view-switcher.

M9 (design §6, §10.M9, issue #12) adds the fourth and final detail view: the per-connection
**Throughput/goodput** graph, and "finalizes the detail view switcher". Its Definition of Done is
**"sliding-window bytes/sec, goodput vs retransmitted; detail view switcher finalized"** — so the
view plots two wire-derived rates over time: the trailing-window **throughput** (all data bytes
per second) and the **goodput** (only *new*, non-retransmitted data bytes per second). The gap
between the two lines is the retransmitted rate — "goodput vs retransmitted".

This ADR decides:

1. **What data the throughput graph consumes, where it is derived and stored, and how each sample
   is directionally attributed.** Throughput belongs to the *sending* flow; unlike M8's RTT (which
   is measured on the opposite direction's ACK and so is attributed to the acked sender), a data
   segment's throughput is attributed to that segment's *own* direction.
2. **What "goodput" is and where it is derived.** Goodput is a *new derived quantity*, not present
   in `MetricSample`: the same trailing window as `throughput_bps` but summing only
   non-retransmitted data bytes.
3. **How the two wire rates are rendered** in the M7/M8 mark-grid model, with a bits/sec axis.
4. **Why there is no kernel overlay seam** on this view (unlike M7/M8).
5. **How `Tab` finalizes the switcher** with the fourth variant.

The engine already computes, per segment, a frozen trailing-window `MetricSample.throughput_bps`
(design §4.1, §10.M3, ADR-0007): a defensive-divide sum of data payload bytes over
`throughput_window` (default 1 s), *including* retransmitted bytes (a retransmit re-sends payload,
which is real wire traffic). M9 does not change `throughput_bps`; it **retains** it and derives
**goodput** alongside it from the same window state.

## Decision

### 1. A dedicated per-connection `ThroughputSample` series, attributed to the sending flow

Mirroring `StateSample`/`SeqSample`/`InFlightSample`/`RttSample`, we add a **`ThroughputSample`**
record and collect one series per connection on the replay path:

```rust
pub struct ThroughputSample {
    pub t: Nanos,             // the segment's timestamp (when the rate was sampled)
    pub dir: SampleDir,       // the sending data-flow direction (NOT flipped)
    pub throughput_bps: u64,  // all data bytes/sec over the trailing window (incl. retransmits)
    pub goodput_bps: u64,     // non-retransmitted data bytes/sec over the same window (≤ throughput)
}
```

- **`dir` is the sender's own direction, not flipped.** A data segment's throughput is the rate of
  the flow that *sent* it; `MetricSample.dir` is already tagged with the sending segment's
  direction, so `ThroughputSample.dir == sample.dir`. (This is the opposite of M8's RTT, whose
  measured flow is `opposite(sample.dir)`; the asymmetry is deliberate and is why the two are
  separate collectors.) The detail views plot a single **focus direction** (the higher-byte data
  flow; ADR-0011/0012/0013 §3.6); a mostly-O2R transfer's throughput is on the O2R sends, so
  attributing to the sender selects the right flow.
- **Both directions are snapshotted per segment** (mirroring M7's in-flight collector, ADR-0012
  §1), *gated on a direction having sent data*. Throughput is a *decaying* windowed rate: after a
  flow stops sending, its rate falls as bytes age out of the trailing window. Sampling **both**
  directions at every segment `t` (not only on the sender's own data segments) means the focus
  flow's decay is sampled at the *reverse* direction's ACK times too, so the graph shows the rate
  falling after a burst rather than freezing at the last send. A direction that has never sent a
  data byte contributes no sample (its rate is identically zero and uninteresting), exactly as M7
  skips a direction with no send frontier.
- **`goodput_bps ≤ throughput_bps` always**, because goodput sums a subset (the non-retransmitted
  bytes) of what throughput sums over the same window. On a loss-free flow the two are equal and
  the lines coincide; divergence is exactly the retransmitted rate.
- **Collection is gated by a new `EngineConfig.collect_throughput_timeline` flag** (default
  `false`), orthogonal to the state/seq/inflight/rtt flags. On the replay path all five are on.
  Each retained `ThroughputSample` counts against the existing `max_samples` ceiling and shares
  the fail-fast `SampleCeiling` path (design §7).
- **`Timeline` owns the series and exposes `throughput_series(id) -> &[ThroughputSample]`**, keyed
  by `ConnId`, exactly like `rtt_series`. `Timeline::with_seq` is extended to carry the fifth
  series; `Timeline::new` still delegates with empty vectors so the M4/M5 fixtures are untouched.

`MetricSample` and the `metrics` command's JSON stay untouched, so the hand-derived M3 oracle
goldens are undisturbed (the property ADR-0010/0011/0012/0013 all preserved).

### 2. Goodput is an engine-derived windowed sum over non-retransmitted bytes

Goodput is a **new derived metric**, computed in the engine (the metric-derivation home; ADR-0002,
ADR-0007), not in the TUI. It reuses the *same* trailing window as `throughput_bps`:

- The engine's per-direction throughput window (`VecDeque` of `(ts, len)`) gains the segment's
  **retransmit classification** — the same `retransmit` bool M3 already computes in
  `MetricState::observe` (the reorder-window rule, ADR-0007) — so each window entry is
  `(ts, len, retransmit)`.
- **Throughput** sums `len` over every entry in `(t − window, t]` — *unchanged* from M3, so
  `MetricSample.throughput_bps` is byte-identical and the oracle is frozen. **Goodput** sums `len`
  over the entries in that window whose `retransmit` flag is `false`. Both are the standard
  defensive-divide `bytes · 8 · 1e9 / window`, evaluated in `u128` then narrowed (no float).
- The pair is exposed as a **pure read `throughput_at(dir, t) -> Option<(throughput_bps,
  goodput_bps)>`** on `MetricState` (mirroring `in_flight(dir)`): `None` if `dir` never sent data,
  else both rates as of `t` over that direction's window. `observe` uses it to fill
  `MetricSample.throughput_bps` (its `.0`, unchanged); the collector uses it to snapshot both
  directions for the timeline. One window state, two consumers — the timeline rate is not a second,
  divergent derivation but the *same* window read for both directions.

Reusing the throughput window (rather than adding a `goodput_window` knob) keeps the two rates
directly comparable on one axis and adds no configuration surface with no requirement behind it.

### 3. Render total and goodput as two distinct marks in the M7/M8 grid, on a bits/sec axis

A **pure projection** in `tcpvisr-tui` (`throughput.rs`, a sibling of `inflight.rs`/`rtt.rs`)
returns the same `Mark { col, row, glyph, series }` grid model:

- **`Series { Throughput, Goodput }`.** Each focus-direction `ThroughputSample` with `t ≤ T` emits
  a `Throughput` mark at `(col(t), row(throughput_bps))` and a `Goodput` mark at `(col(t),
  row(goodput_bps))`, each with its own glyph. Because `goodput ≤ throughput`, the goodput mark is
  at or below the total; the vertical gap is the retransmitted rate.
- **Y axis spans `[0, max_rate]`** where `max_rate` is the maximum over the focus direction's
  `throughput_bps` and `goodput_bps` (i.e. the throughput maximum). X spans `[opened_at,
  effective_end]` from `Timeline::x_span`, shared with M6/M7/M8. Neither rescales with the cursor.
- **Column bucketing keeps the numeric maximum per (column, series)** — the same rule and
  `O(cells)` bound as M7/M8, applied **only to revealed samples** (`t ≤ T`) so scrubbing never
  shows a future peak.
- **Fixed axes, reveal-to-`T`, and a vertical cursor column** are reused unchanged from M6/M7/M8.
- **Same-cell precedence** resolves in the single grid in place order **throughput, then goodput**:
  where the two coincide (a loss-free column, `goodput == throughput`) the goodput mark shows
  (goodput is the more informative "useful rate"). Where they differ both survive at distinct rows.
- The Y axis labels the rate via an integer, **unit-adapting** formatter `fmt_rate(bps)` — it picks
  `bps`/`kbps`/`Mbps`/`Gbps` by magnitude and prints `<whole>.<3-frac><unit>`, mirroring `fmt_rtt`,
  so a slow flow does not collapse to `0.000Mbps`. Integer-only; the plotted rows are unaffected
  (`row()` maps raw bits/sec), only the label adapts units.

### 4. No kernel overlay seam on this view

M7 and M8 each carry a typed, empty-on-replay kernel overlay seam because design §10.M12 says
kernel enrichment "overlays on M7/M8" — cwnd on the in-flight view, srtt on the RTT view. Design
§10.M12 does **not** list a throughput/goodput overlay, and `KernelInfo` (design §4) has no wire-
rate field to overlay (its `delivery_rate` is a separate future concern, not scoped by any
milestone). Adding an overlay seam here would be a **phantom feature** (a hook no milestone fills),
which the project's standards forbid. The throughput projection therefore takes only the wire
series — no overlay parameter — and this ADR records the *absence* as deliberate, not an oversight.

### 5. `Tab` finalizes the four-view switcher

`DetailView` gains the fourth variant and `cycle_detail_view` closes the cycle:

```rust
pub enum DetailView { TimeSequence, InFlight, Rtt, Throughput }
// Tab: TimeSequence -> InFlight -> Rtt -> Throughput -> TimeSequence
```

`Enter`/`Esc` keep their open/close meaning; `Tab` in filter mode stays inert. With the fourth
view reachable, all four design §6 detail views (Time/Sequence, In-flight, RTT, Throughput) are
present — this is the "detail view switcher finalized" of the M9 Definition of Done.

## Consequences

- The replay path now retains a fifth per-connection series (`ThroughputSample`) alongside state,
  seq, in-flight, and RTT, all bounded by `max_samples` with the existing fail-fast. Throughput
  samples are per-segment (both directions that have sent data), comparable in count to the
  in-flight series.
- The engine gains no I/O and no clock read (ADR-0002 preserved); goodput is derived purely from
  segments via the existing `MetricState` window and the M3 retransmit classification.
- `MetricSample.throughput_bps` is byte-identical (the window sum is unchanged; goodput is an
  additive second sum), so the frozen M3 oracle goldens are undisturbed.
- The throughput projection is a fourth pure module. The three numeric-max projections
  (in-flight, RTT, throughput) now share a `col`/`row`/numeric-max-bucketing shape; this ADR keeps
  them as separate modules (the RTT overlay, the throughput two-wire-no-overlay shape, and the
  in-flight single-wire-plus-overlay shape differ enough that a forced abstraction would trade
  clarity for a shared helper). Extracting a shared grid helper is a future cleanup, tracked but
  not done here to bound the M9 diff.
- `Timeline`'s `ConnSeries` tuple grows to six elements. It stays a tuple (not a named struct) to
  match the established M6/M7/M8 carriage and keep the diff mechanical; converting it to a struct
  is noted as a future cleanup.
- With four views reachable, the detail switcher is complete for v0.1; M10–M13 add no detail view.

## Considered & rejected

- **Flip the direction like M8's RTT (`opposite(sample.dir)`).** Rejected: throughput is the
  *sender's* rate, and the data segment's own direction *is* the sender. Flipping would attribute a
  flow's throughput to the peer that only ACKs it. RTT flips because an RTT is *measured* on the
  opposite ACK; throughput is not.
- **Sample only on the sender's own data segments (sparse, no reverse-ACK snapshot).** Rejected:
  throughput is a decaying windowed rate; sampling only at sends freezes the curve at the last
  burst and hides the decay that the reverse-direction ACK stream would reveal. M7 set the
  both-direction snapshot precedent for exactly this "sample at ACK time" reason.
- **Add `goodput_bps` to `MetricSample` / the `metrics` JSON.** Rejected for the same reason
  ADR-0010/0011/0012/0013 rejected the analogous move: it churns the frozen M3 oracle goldens. A
  dedicated `ThroughputSample` keeps `MetricSample` and the oracle frozen. (Exposing goodput in the
  `metrics` command's JSON is a possible *separate*, additive future change; it is out of M9's
  scope and not required by the DoD.)
- **Compute goodput in the TUI projection.** Rejected: goodput is metric derivation, which lives in
  the pure engine (ADR-0002/0007). Deriving it in the engine keeps the projection a pure plotter
  with no metric logic, consistent with M6/M7/M8.
- **Two separate windows (one total, one goodput).** Rejected: one window whose entries carry the
  retransmit flag is simpler and guarantees the two rates share the same eviction and membership;
  the total sum ignores the flag so `throughput_bps` is unchanged.
- **A different goodput definition (exclude out-of-order too, or count acked-only bytes).**
  Rejected: the DoD is "goodput vs retransmitted", so goodput excludes exactly the *retransmitted*
  bytes. Out-of-order segments carry *new* data (delivered, just reordered) and count as goodput;
  M3 already separates `retransmit` from `out_of_order`, so goodput = throughput − retransmitted.
- **A kernel-rate overlay seam (mirror M7/M8).** Rejected: design §10.M12 overlays only M7 (cwnd)
  and M8 (srtt); no milestone fills a throughput overlay, so a seam here would be a phantom feature.
- **A `--goodput-window` CLI knob.** Rejected: goodput reuses the throughput window; a separate knob
  adds configuration surface with no requirement.
- **Convert the `ConnSeries` tuple to a named struct now.** Deferred: it would be a broader refactor
  across every `with_seq` call site; M9 keeps the tuple to stay consistent with M6/M7/M8 and bound
  the diff, noting the struct as a future cleanup.
