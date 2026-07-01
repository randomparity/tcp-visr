# ADR-0011: Detail Time/Sequence — dedicated seq series + character-cell rendering (M6)

> Status: Accepted
> Date: 2026-06-30

## Context

M5 ([ADR-0010](0010-timeline-transport-as-of-t.md)) delivered a seekable **master list**:
the replay TUI holds a `Timeline` of per-connection `StateSample`s and a `Transport` cursor,
and resolves every connection's `(state, bytes)` as of cursor time `T`. ADR-0010 explicitly
left `Enter` (open a connection's detail) inert, deferred to M6.

M6 (design §6, §10.M6, issue #9) adds the first **detail pane**: a per-connection
Time/Sequence (Stevens) graph — TCP sequence number on the Y axis against time on the X
axis — with **retransmit** and **SACK** marks, driven by the transport cursor. This ADR
decides:

1. **What per-connection data the graph consumes, and where it is derived and stored.** The
   M5 `StateSample` carries lifecycle state and cumulative byte counts, not per-segment
   sequence positions or retransmit/SACK classification. The M3 `MetricSample` carries the
   retransmit/out-of-order/SACK flags but **not** the segment's sequence number, and it is
   not collected on the replay path.
2. **How the graph is rendered** in a terminal, given the project's pure-core / thin-shell
   discipline (ADR-0002, ADR-0009) and the testing convention (pure logic unit-tested from
   hand-built vectors; `ratatui::TestBackend` for the shell).
3. **How the detail pane is entered and laid out** without disturbing the M5 master list.

## Decision

### 1. A dedicated per-connection `SeqSample` series in the engine

We add a **`SeqSample`** record and collect one series per connection on the replay path,
mirroring the ADR-0010 `StateSample` decision (a dedicated series rather than extending
`MetricSample`):

```rust
pub enum SeqKind {
    Data { retransmit: bool, out_of_order: bool },
    Sack,
}
pub struct SeqSample {
    pub t: Nanos,
    pub dir: SampleDir, // the DATA direction this point lives in
    pub rel: i64,       // unwrapped cumulative offset from `dir`'s first-seen data seq
    pub len: u32,       // payload length (0 for a Sack mark)
    pub kind: SeqKind,
}
```

- **Every `SeqSample`'s `(dir, rel)` is expressed in `dir`'s data-sender sequence space, as
  an unwrapped `i64` offset.** A `Data` point is emitted for a data-carrying segment
  (payload > 0) in its own direction. A `Sack` point is emitted for **each SACK block**
  carried by a segment: its `dir` is the *opposite* of the carrying segment (the direction
  whose data is being acknowledged), and its position is the block's left edge — which already
  lives in that data sender's space. So a consumer focusing on one direction filters
  `SeqSample`s by that `dir` and both the data points and the SACK marks share one Y axis.
- **`rel` is the wrap-unwrapped cumulative sequence offset, computed in the engine.** The
  32-bit TCP sequence number wraps every 4 GB; a Stevens axis must *unwrap* it or a
  multi-GB transfer folds on top of itself. The engine anchors each direction at its
  first-seen data seq (`rel = 0`) and, for every later seq `S`, adds the **bounded signed
  serial distance** from the running frontier (always within ±2^31, so RFC-1982 comparison is
  well-defined; ADR-0006) to the frontier's cumulative offset. A retransmit/reorder therefore
  lands at its true *earlier* `rel` (a negative step from the frontier), and a stream that
  wraps the 32-bit space many times accumulates monotonically in `i64` (headroom for any
  capture the `max_samples` ceiling admits). This keeps the error-prone serial arithmetic in
  the engine; the TUI plots plain integers and never does `serial_diff` itself.
- **Classification is not duplicated.** `retransmit` / `out_of_order` are taken from the
  same `MetricState::observe` derivation that produces `MetricSample` (ADR-0007); the
  tracker runs that derivation and transforms its result into a `SeqSample` instead of (for
  replay) buffering the `MetricSample` itself.
- **Collection is gated by a new `EngineConfig.collect_seq_timeline` flag** (default
  `false`), independent of `collect_state_timeline` and of `series_collection`. On the
  replay path both `collect_state_timeline` and `collect_seq_timeline` are on. Each retained
  `SeqSample` counts against the existing `max_samples` ceiling and shares the fail-fast
  `SampleCeiling` path (design §7). Replay therefore retains up to one `StateSample` plus one
  or more `SeqSample`s (one per data segment, plus one per SACK block) per segment; the
  default ceiling (10,000,000) leaves ample headroom, and overflow fails fast with the
  existing actionable message.
- **`Timeline` owns the series and exposes `seq_series(id) -> &[SeqSample]`** so the detail
  view queries the selected connection's series by `ConnId` without re-parsing (ADR-0004).

`MetricSample` and the `metrics` command's JSON stay untouched, so the hand-derived M3 oracle
goldens are not disturbed — the same property ADR-0010 preserved.

### 2. Render the graph as a pure character-cell grid, not a braille `Canvas`

The graph is produced by a **pure projection** in `tcpvisr-tui`: given the focus
connection's `&[SeqSample]`, its time bounds, the cursor `T`, and the pane's cell dimensions
`(width, height)`, it returns a grid of **marks** — `{ col, row, glyph }` — plus the
resolved axis ranges. Rendering writes those glyphs into the terminal buffer.

- **One glyph per cell, not sub-cell braille.** The distinct retransmit and SACK marks the
  DoD requires are per-character glyphs (the design's own vocabulary: `· = retransmit`,
  `╎ = SACK`). A braille `Canvas` packs 2×4 monochrome sub-dots per cell with a single
  marker per layer, which makes *distinguishable* per-point glyphs awkward and their exact
  placement hard to assert. A character grid places an unambiguous glyph at a known
  `(col, row)`, so the projection is **directly unit-testable** (“a retransmit at `(t, seq)`
  lands at grid cell `(col, row)` with glyph `·`”) in the project's pure-core style, and the
  render is a thin, snapshot-testable pass.
- **Density is handled by column bucketing, not sub-cell resolution.** Samples map to
  columns by time; when several land in one column the projection keeps the **most salient**
  mark for that cell (retransmit > SACK > out-of-order > data). This is the downsampling the
  design's §14 “TUI chart resolution” risk anticipates, made explicit and testable, and it
  bounds render cost to `O(cells)` independent of sample count.

### 3. Cursor-driven reveal with stable axes

- **Fixed axes.** The X axis spans the focus connection's full active interval
  `[opened_at, effective_end]`; the Y axis spans `[0, max relative sequence]` over the whole
  connection. Axes do **not** rescale as the cursor moves, so scrubbing does not make the
  plot jump.
- **Relative sequence.** Each point already carries an unwrapped `i64` `rel` (above). The
  projection's Y baseline is the plain integer **minimum** of `rel` over the focus direction's
  revealed-and-unrevealed samples, and the axis top is `max(rel − baseline + len)` — ordinary
  `i64` min/max, a total order, so it is deterministic and cannot fold. No `serial_diff` runs
  in the TUI; a multi-wrap (>4 GB) transfer plots as a continuously rising line, not a fold.
- **Reveal to `T`.** Only marks with `t ≤ cursor` are drawn; a vertical **cursor column** is
  drawn at `T`'s X position. Playing or seeking animates the sequence graph filling in.
- **Focus direction.** The plotted direction is the connection's higher-byte direction
  (`bytes_o2r` vs `bytes_r2o`, tie → origin→responder), a deterministic function of the
  `Connection`. The reverse-direction plot and an ACK-line overlay are deferred (not in the
  M6 DoD).

### 4. `Enter` opens a split detail pane; `Esc` closes it

`App` gains a detail-open flag. When closed (the default) the master list renders full-width
exactly as M5 — so every M5 render snapshot is unchanged. `Enter` on a selected row opens the
detail pane and the layout splits (master left, detail right); `Esc` closes it. The transport
and navigation keys stay active while the detail is open, so the graph is cursor-driven in
the literal sense: play/seek/step redraw it. There is a single detail view in M6; the
view-switcher (`Tab`) across the four graphs is M9's concern.

## Consequences

- The replay path now retains a second per-connection series (`SeqSample`) alongside the
  M5 `StateSample` series, both bounded by `max_samples` with the existing fail-fast. The
  extra memory is proportional to data segments + SACK blocks; the ceiling protects against
  a hostile/large capture (§7, §14).
- The engine gains no I/O and no clock read (ADR-0002 preserved); the seq series is derived
  purely from segments, and the render/cursor math stays in the pure TUI projection with the
  clock still read only in `run()` (ADR-0010).
- The detail projection is unit-testable without a terminal; only the thin glyph-writing pass
  needs `TestBackend`. This keeps the M6 surface inside the project's testing convention.
- Rendering as a character grid means graph fidelity is one glyph per cell rather than
  braille's 2×4 sub-cells. For M6's mark-oriented Stevens view that is a deliberate trade for
  legible, distinguishable, testable marks; a higher-resolution line rendering can be layered
  later for the continuous curves (M7 in-flight sawtooth, M8 RTT) without disturbing the seq
  view's mark model.

## Considered & rejected

- **Extend `MetricSample` with the segment's `seq`/`len` and collect the full metric series
  for all connections on replay.** Rejected for the same reasons ADR-0010 rejected the
  analogous move for lifecycle data: it churns the M3 `metrics` JSON schema and the
  hand-derived oracle goldens, and collects heavier per-sample data than M6 needs. A
  dedicated `SeqSample` keeps `MetricSample` and the oracle frozen.
- **Render with a braille `Canvas` (`Points`/`Line` shapes).** Rejected: sub-cell braille
  gives one marker per layer and no clean way to place or assert distinct per-point
  retransmit/SACK glyphs, and it pushes the plotting logic into an impure widget that only
  `TestBackend` can exercise. The pure marks→grid projection is the testable, glyph-faithful
  choice; braille is revisitable for later continuous-curve views.
- **Auto-scale the axes to a window around `T`.** Rejected for M6: a moving window rescales
  the plot on every cursor move, which is disorienting while scrubbing and harder to assert.
  Fixed full-extent axes with a moving cursor column are simpler and deterministic;
  per-window auto-scale/downsampling (design §14) can be added later behind the same
  projection seam.
- **Always-on split (master + detail simultaneously, per the §6 sketch).** Rejected for the
  first cut: it re-flows every M5 render snapshot and crowds narrow terminals. `Enter`-to-open
  preserves M5 behavior by default and is the incremental step; an always-on layout can
  follow once all four detail views exist (M9).
- **Plot both directions on one axis, or store raw segments and derive in the TUI.**
  Rejected: two directions live in two independent sequence spaces (two ISNs) and sharing one
  Y axis is misleading; deriving in the TUI would move sequence classification out of the pure
  engine (ADR-0002). The engine stores both directions' `SeqSample`s and the TUI *selects* one
  to plot — policy in the view, derivation in the engine.
- **Re-derive the seq series on each selection/seek.** Rejected by ADR-0004: series are
  precomputed once and held in memory; the cursor only moves.
```
