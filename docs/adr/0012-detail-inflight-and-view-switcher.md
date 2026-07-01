# ADR-0012: Detail In-flight — dedicated in-flight series, sawtooth rendering, and the detail view-switcher (M7)

> Status: Accepted
> Date: 2026-06-30

## Context

M6 ([ADR-0011](0011-detail-seq-timeline-and-rendering.md)) delivered the first **detail
pane**: a per-connection Time/Sequence (Stevens) graph, entered with `Enter` and closed with
`Esc`, driven by the transport cursor. It established a repeatable shape for a detail view: a
**dedicated per-connection series** derived on the replay path and stored in the `Timeline`
(rather than collecting the full `MetricSample` series, which would churn the frozen M3 oracle
goldens), plus a **pure projection** in `tcpvisr-tui` that maps `(series, bounds, focus dir,
cursor, cell rectangle)` to a grid of `Mark { col, row, glyph }`.

M7 (design §6, §10.M7, issue #10) adds the second detail view: the **wire-estimated in-flight
(bytes-outstanding) sawtooth**, with **overlay hooks for the kernel's cwnd**. This ADR decides:

1. **What data the in-flight graph consumes, and where it is derived and stored.** The engine
   already computes per-segment `in_flight_bytes` inside `MetricState::observe` (ADR-0007), but
   that `MetricSample` series is deliberately *not* retained on the replay path (ADR-0011).
2. **How a step/sawtooth curve is rendered** in a character-cell grid, given the pure-core /
   thin-shell discipline (ADR-0002, ADR-0009) and M6's mark-grid model (ADR-0011 §2).
3. **How the second view is reached** now that there is more than one detail view — i.e. the
   `Tab` view-switcher, which ADR-0011 §4 deferred while there was only one view.
4. **What "overlay hooks for kernel cwnd" means concretely** on the replay-only path, where no
   kernel series exists yet (kernel enrichment is M12, live-only; design §10.M12).

## Decision

### 1. A dedicated per-connection `InFlightSample` series in the engine

Mirroring the ADR-0010 `StateSample` and ADR-0011 `SeqSample` decisions, we add an
**`InFlightSample`** record and collect one series per connection on the replay path:

```rust
pub struct InFlightSample {
    pub t: Nanos,
    pub dir: SampleDir, // the sender direction this outstanding-bytes value belongs to
    pub bytes: u64,     // bytes in flight (outstanding) for `dir` as of this segment
}
```

- **`bytes` is the wire-estimated bytes-outstanding for `dir`**, the same quantity the engine
  already derives as `MetricSample.in_flight_bytes` (ADR-0007, serial-correct across `u32`
  wrap).
- **Both directions' outstanding are snapshotted at every segment — not just the segment's own
  direction.** In-flight is a two-sided quantity: it *rises* when a direction sends and *falls*
  when the opposite direction's ACK advances the sender's acknowledged frontier. If the series
  recorded only the segment's own-direction outstanding (the naive mirror of `SeqSample`), an
  ACK — which arrives in the opposite direction — would never sample the *sender's* now-lower
  outstanding, and a burst-then-idle transfer would render a false high plateau on its tail
  instead of the sawtooth's downstroke. So the tracker snapshots **each direction that has an
  established send frontier** at every processed segment `t`, via a small
  `MetricState::in_flight(dir) -> Option<u64>` query (`snd_nxt − acked`, serial, ≥ 0; `None`
  before that direction has a frontier). The own direction's snapshot equals the
  `MetricSample.in_flight_bytes` `observe` returns; the opposite direction's snapshot captures
  the ack-driven drain at ack time. Redundant equal-valued snapshots (a segment that does not
  change a direction's outstanding) are collapsed by the plot's per-column bucketing (§2), so
  they cost only bounded memory, not visual noise.
- **Classification is not duplicated.** The values come from the same `MetricState` the engine
  already advances on the replay path to classify M6 seq points; the tracker reads outstanding
  from it instead of buffering the `MetricSample`.
- **Collection is gated by a new `EngineConfig.collect_inflight_timeline` flag** (default
  `false`), orthogonal to `collect_state_timeline` and `collect_seq_timeline`. On the replay
  path all three are on. Each retained `InFlightSample` counts against the existing
  `max_samples` ceiling and shares the fail-fast `SampleCeiling` path (design §7).
- **`Timeline` owns the series and exposes `inflight_series(id) -> &[InFlightSample]`**, keyed
  by `ConnId`, exactly like `seq_series`. `Timeline::with_seq` is extended to carry the third
  series; `Timeline::new` still delegates with empty vectors so the M4/M5 fixtures are
  untouched.

`MetricSample` and the `metrics` command's JSON stay untouched, so the hand-derived M3 oracle
goldens are undisturbed — the property ADR-0010 and ADR-0011 both preserved.

### 2. Render the sawtooth as top-edge marks in the same character-cell grid

The in-flight curve is produced by a **pure projection** in `tcpvisr-tui` (a sibling of M6's
`detail.rs`), returning the same `Mark { col, row, glyph }` grid model:

- **One mark per populated column, at the outstanding-bytes height.** Each focus-direction
  `InFlightSample` with `t ≤ T` maps to `(col(t), row(bytes))` with bottom-origin rows, and the
  cell gets a wire glyph. Across columns the varying heights trace the classic in-flight
  sawtooth (rise as data is sent, drop on ACK). This reuses M6's fixed-axis, reveal-to-`T`,
  cursor-column, and `O(cells)` bucketing model, so the projection is directly unit-testable
  ("outstanding `b` at time `t` lands at cell `(col, row)`") without a terminal.
- **Column bucketing keeps the numeric maximum, not a salience rank.** Where several samples
  fall in one column the cell shows the tallest (the sawtooth *peak* for that time bucket).
  This differs from M6's `retransmit > sack > …` salience ordering because in-flight has no
  glyph taxonomy — the meaningful summary of a dense column is its peak outstanding value.
- **Fixed axes.** X spans `[opened_at, effective_end]` (from `Timeline::x_span`, shared with
  M6); Y spans `[0, max_bytes]` over the focus direction's wire ∪ overlay series (§4). Neither
  rescales as the cursor moves.

A braille `Canvas` line was rejected for the same reasons ADR-0011 gave: it is impure, gives
one marker per layer, and is hard to place/assert. A filled column-area rendering (bars from
baseline to the height) was considered and deferred: the top-edge marks are the minimal,
M6-consistent, testable form of the sawtooth; area fill is later visual polish behind the same
projection seam.

### 3. `Tab` switches detail views; the switcher enters minimally at M7

`App` gains a `DetailView` enum and a current-view field:

```rust
pub enum DetailView { TimeSequence, InFlight }
```

- **`Tab`** (navigation mode) cycles the current detail view. `Enter`/`Esc` keep their M6
  meaning (open/close the pane). The transport and navigation keys stay live, so the in-flight
  graph is cursor-driven exactly like the seq graph.
- `render.rs` draws the view named by `App::detail_view()` when the pane is open; the master
  pane and the closed-pane layout are byte-identical to M6.
- M8 (RTT) and M9 (throughput) add variants to `DetailView`; M9 "finalizes" the switcher/layout
  (design §10.M9). Introducing a two-way switcher now is the smallest mechanism that makes the
  new view reachable — the incremental step, in the spirit of ADR-0011 §4's `Enter`-to-open.

ADR-0011 §4 called the switcher "M9's concern" while M6 had a single view; M7 introduces the
second view and therefore the minimal switch. This refines, and does not reopen, that note.

### 4. "Overlay hooks for kernel cwnd" is a typed, tested seam — not a live feature

On replay there is no kernel cwnd (it is a live-only, at-sender signal derived from
`sock_diag`; design §4, §10.M12). The M7 deliverable is the **seam**, made concrete and
non-phantom:

- The in-flight projection accepts a **second, optional overlay series** of the same
  bytes-over-time shape, and each `Mark` carries a `series` tag (`Wire` vs `Cwnd`) so the
  renderer draws the overlay with a distinct glyph/colour. Wire in-flight and cwnd are **never
  conflated** (design §4: in-flight ≤ cwnd; divergence is the diagnostic signal).
- **The Y axis is scaled over wire ∪ overlay, not wire alone.** Because cwnd ≥ in-flight, an
  overlay scaled against the wire maximum would clamp at the top row exactly when it diverges
  upward — flattening the signal the overlay exists to show. `max_bytes` is therefore the
  maximum over both the wire and overlay focus-direction samples. On replay the overlay is
  empty, so this is identical to the wire maximum; the correction only matters once M12 fills
  the overlay.
- On the replay path the overlay is **empty**, so no cwnd is drawn. A projection **unit test**
  passes a synthetic overlay and asserts the distinctly-tagged marks, so the hook is exercised
  code, not dead code. M12 fills the overlay from the kernel series without touching the
  projection.

## Consequences

- The replay path now retains a third per-connection series (`InFlightSample`) alongside
  `StateSample` and `SeqSample`, all bounded by `max_samples` with the existing fail-fast. The
  extra memory is up to two small samples per segment (both directions' outstanding when each
  has a send frontier); the ceiling protects against a hostile/large capture (§7, §14).
- The engine gains no I/O and no clock read (ADR-0002 preserved); the in-flight series is
  derived purely from segments via the existing `MetricState`.
- The in-flight projection is a second pure module, unit-testable without a terminal; only the
  thin glyph-writing pass needs `TestBackend`. It duplicates M6's small `col_of`/`row_of`
  helpers rather than prematurely extracting a shared utility (written twice, not three times);
  the bucketing rule and the overlay differ enough that a shared abstraction would be forced.
- The `DetailView` switcher makes the pane extensible: M8/M9 add variants without reworking the
  open/close modality or the master pane.
- The cwnd overlay seam is typed and tested but unpopulated until M12; the ADR records it as a
  deliberate hook so it is not mistaken for a phantom feature.

## Considered & rejected

- **Collect the full `MetricSample` series on replay and read `in_flight_bytes` from it.**
  Rejected for the same reason ADR-0010/0011 rejected the analogous moves: it churns the M3
  `metrics` JSON schema and the hand-derived oracle goldens, and retains heavier per-sample
  data than the view needs. A dedicated `InFlightSample` keeps `MetricSample` and the oracle
  frozen.
- **Sample outstanding only at same-direction send times (the exact `SeqSample` mirror).**
  Rejected: in-flight falls on ACKs, which travel in the opposite direction, so a send-only
  series never captures the drain — a burst-then-idle transfer would show a false high plateau
  on its tail and place every downstroke at the next send's timestamp instead of the ack's.
  Snapshotting both directions' outstanding per segment is the small correction that makes the
  sawtooth honest; the extra equal-valued snapshots are bucketed away in the plot and bounded
  by `max_samples`.
- **Fold in-flight collection into the existing `collect_seq_timeline` flag.** Rejected: the
  flags stay orthogonal and independently testable (as `collect_state_timeline` and
  `collect_seq_timeline` already are), even though replay sets all three. One flag per series
  keeps each knob's on/off behaviour a single unit test.
- **Render the sawtooth with a braille `Canvas` (`Line`/`Points`).** Rejected: impure, one
  marker per layer, hard to place/assert, and it would fork the detail-rendering model away
  from M6's testable mark grid. Revisit for continuous-curve fidelity once all four views exist.
- **Fill the column area (bars from baseline to the height).** Deferred, not adopted: the
  top-edge marks are the minimal M6-consistent sawtooth; area fill is visual polish that can be
  layered behind the same projection seam without changing the data path.
- **Reuse M6's `detail.rs::project` parametrised by a "series kind".** Rejected: it would push
  numeric-max bucketing, a second Y source, and the overlay into the frozen M6 projection and
  its tests. A sibling module keeps M6 unchanged (the ADR-0011 "freeze what works" property).
- **Defer the view-switcher to M9 (per ADR-0011 §4) and ship the in-flight view unreachable.**
  Rejected: a view with no way to reach it is not a shippable DoD. The minimal two-way `Tab`
  switch is the smallest reachable increment; M9 still finalizes the full four-view switcher.
- **Build the kernel cwnd source now.** Rejected: cwnd is live-only kernel enrichment (M12);
  building it here would be out-of-milestone and, on replay, a phantom feature. M7 ships the
  typed, tested overlay seam only.
