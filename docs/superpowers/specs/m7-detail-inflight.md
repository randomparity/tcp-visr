# Spec: M7 — Detail: In-flight / cwnd

**Milestone:** M7 (design §6, §10.M7) · **Issue:** #10 · **Release:** v0.1.0 (Replay MVP)
**ADR:** [ADR-0012 — Detail In-flight: dedicated in-flight series, sawtooth rendering, and the detail view-switcher](../../adr/0012-detail-inflight-and-view-switcher.md)
(builds on [ADR-0004](../../adr/0004-seekable-timeseries-timeline.md),
[ADR-0007](../../adr/0007-metric-derivation-model.md),
[ADR-0009](../../adr/0009-tui-master-list-architecture.md),
[ADR-0010](../../adr/0010-timeline-transport-as-of-t.md),
[ADR-0011](../../adr/0011-detail-seq-timeline-and-rendering.md))

## 1. Goal

Add the second **detail view**: a per-connection **In-flight** graph plotting the
**wire-estimated bytes in flight (outstanding)** over time as a sawtooth, driven by the
transport cursor `T` (reveal to `T`, cursor column, fixed axes). Reach it with a **`Tab`**
view-switcher that cycles the open detail pane between the M6 Time/Sequence view and the new
In-flight view. Provide a typed, tested **overlay hook** so a future kernel-cwnd series (M12,
live-only) can be drawn as a distinct overlay; on replay the overlay is empty.

## 2. Scope

### In scope

- A pure **`InFlightSample`** series in `tcpvisr-engine`: one record per processed segment,
  carrying `{ t, dir, bytes }` where `bytes` is the segment's **wire in-flight
  (`MetricSample.in_flight_bytes`)** for its own direction (ADR-0012 §1 — the engine already
  derives it, serial-correct across `u32` wrap; the tracker does not recompute it). Collected
  under a new `EngineConfig.collect_inflight_timeline` flag, counting against `max_samples`.
- `Timeline` retains each connection's `InFlightSample` series and exposes
  **`inflight_series(id) -> &[InFlightSample]`**; `into_timeline` / `with_seq` carry it through.
- A pure **in-flight projection** in `tcpvisr-tui` (a sibling module to M6's `detail.rs`):
  `(wire series, overlay series, focus direction, x_span, cursor T, viewport cells)` → resolved
  axis ranges + a grid of marks `{ col, row, glyph, series }`, revealing marks with `t ≤ T`, a
  vertical cursor column at `T`, fixed full-extent axes (Y = `[0, max_bytes]`), **numeric-max**
  column bucketing, and a distinct glyph for `series == Cwnd` overlay marks (ADR-0012 §2, §4).
- **`App`** gains a `DetailView { TimeSequence, InFlight }` field, `detail_view()`, and
  `cycle_detail_view()`; the focus accessor also exposes the in-flight series. `Tab` cycles the
  view. `Enter`/`Esc` keep their M6 open/close meaning.
- **Layout / render**: closed → M5/M6 full-width master (unchanged). Open → master left /
  detail right split; the pane renders the view named by `detail_view()`. The In-flight view
  titles the focus connection, draws the sawtooth with a Y axis in bytes and an X axis in
  seconds, and a one-line legend. Footer advertises `⇥ view`.
- **CLI wiring**: `run_replay` / `build_replay_app` set `collect_inflight_timeline = true` so
  the `Timeline` carries the in-flight series; the `max_samples` ceiling still fails fast.

### Out of scope (deferred, do not build)

- **RTT and throughput/goodput detail views** and any further `Tab` variants — M8, M9. M9
  "finalizes" the switcher/layout (design §10.M9).
- **The kernel cwnd data source.** cwnd is live-only kernel enrichment (`sock_diag`), M12. M7
  ships the typed, tested overlay *seam* only; on replay the overlay series is empty.
- **Filled column-area rendering** of the sawtooth — M7 draws the top-edge marks (ADR-0012 §2);
  area fill is later polish behind the same projection seam.
- **Per-window axis auto-scale / zoom / pan** — M7 uses fixed full-extent axes (as M6).
- **Reverse-direction in-flight overlay** — M7 plots the one higher-byte direction (as M6 §3.6).
- **Live timeline** (M11), **names/attribution** (M10/M12).

## 3. User-facing behavior

### 3.1 Entry point

`tcp-visr replay <file>` is unchanged (invocation, non-TTY guard, `--max-samples`). The only
change: the tracker now also collects the in-flight timeline, so the built `Timeline` can answer
`inflight_series(id)`. A `max_samples` overflow still exits non-zero with the actionable
`SampleCeiling` message.

### 3.2 Switching views

- With the detail pane **open**, in navigation mode, **`Tab`** cycles the detail view:
  Time/Sequence → In-flight → Time/Sequence. The chosen view persists while scrubbing and while
  the selection moves.
- `Tab` when the pane is **closed** is a no-op on layout (it may still update the remembered
  view; nothing is drawn until `Enter` opens the pane).
- In **filter-input** mode `Tab` is inert for view-switching (it is not a printable character
  handled by the filter; it does not cycle the view). All other M6 modality is unchanged:
  `Enter` opens (nav) / confirms filter, `Esc` closes (nav) / clears filter.
- All navigation and transport keys keep working while the pane is open, for either view
  (`j`/`k`/`↑`/`↓`, `space`/`←→`/`+`/`-`/`,`/`.`, `/`, `s`/`S`, `q`/`Ctrl-C`).

### 3.3 Layout (In-flight view open)

```
┌ tcp-visr — capture.pcap  (47 connections, skipped 0) ─────[ ▶ 2.0x  t=12.480s / 38.200s ]┐
│ PEER                SERVICE  STATE        ↑BYTES  ↓BYTES │ DETAIL 10.0.0.5:51324 → 140..:443│
│▸140.82.121.3:443    https    ESTABLISHED    1234   34000 │ In-flight   # wire  (cwnd overlay) │
│ 10.0.0.9:22         ssh      ESTABLISHED~    840    2100  │ bytes                            ╷ │
│ …                                                        │  3200 ┤     ##  ##   #             │
│                                                          │       ┤   ##  ##  ##  #            │
│                                                          │     0 ┼───────────────────────────│
│                                                          │       0.000s              38.200s  │
├──────────────────────────────────────────────────────────┴──────────────────────────────────┤
│ space play/pause  ←→ seek  +/- speed  ,/. step  ⏎ open  esc close  ⇥ view  / filter  s sort  q quit │
└───────────────────────────────────────────────────────────────────────────────────────────────┘
```

- The screen splits into a **left** master pane (renders exactly as M5/M6) and a **right**
  detail pane, a bordered block titled `DETAIL <origin> → <responder>` (shared with M6).
- The In-flight body shows the **bytes-outstanding sawtooth**: a Y axis labeled with the byte
  range (starting at 0, SI-abbreviated so a multi-GB peak fits the gutter, reusing M6's
  `fmt_seq`-style formatter), an X axis labeled with the time range in seconds, the plotted
  wire marks, and a one-line **legend** naming the wire glyph (and the cwnd overlay glyph).
- **Narrow terminals.** The projection is handed the inner plot rectangle after `render.rs`
  carves the gutter, X-label row, and legend row; below the minimum plot rectangle the pane
  shows `widen terminal to view graph` (shared with M6). The master pane keeps rendering.
- **Footer** advertises `⇥ view` alongside the M6 hints.

### 3.4 The In-flight graph (semantics)

For the focus connection (§3.5) and its focus direction (§3.6):

- **X axis (time)** spans `[opened_at, effective_end]` from `Timeline::x_span(id)` (shared with
  M6); fixed, does not rescale with the cursor.
- **Y axis (bytes)** spans `[0, max_bytes]` where `max_bytes = max(s.bytes)` over the focus
  direction's samples (plain `u64` max — no serial arithmetic). Fixed; does not rescale.
- **Plot rectangle.** `W` columns × `H` rows of cells, excluding border, Y-label gutter,
  X-label row, and legend row (`render.rs` carves them). Row 0 is the **bottom** (0 bytes),
  row `H-1` the top.
- **Coordinate mapping (exact, clamped),** with `span_t = effective_end.0 - opened_at.0`:
  - `col(t) = if span_t == 0 { 0 } else { ((t.0 - opened_at.0) * (W-1)) / span_t }`, clamped to
    `0..=W-1`.
  - `row(b) = if max_bytes == 0 { 0 } else { (b * (H-1)) / max_bytes }`, clamped to `0..=H-1`.
    Because the multiplier is `H-1`, `b == max_bytes` maps to row `H-1` (top), never `H`. The
    degenerate `span_t == 0` and `max_bytes == 0` cases collapse to column 0 / bottom row via
    the guards, with no division.
- **Marks.** Each focus-direction wire `InFlightSample` with `t ≤ T` maps to cell
  `(col(t), row(bytes))` with the **wire** glyph and `series == Wire`. Each overlay sample with
  `t ≤ T` maps the same way with the **cwnd** glyph and `series == Cwnd` (empty on replay).
- **Column bucketing (downsampling).** When multiple **wire** marks fall in one column, the cell
  keeps the **tallest** (max `row`) — the sawtooth peak for that time bucket. Overlay marks
  bucket independently by the same max rule. This bounds render to `O(cells)` regardless of
  sample count (ADR-0012 §2). A wire mark and an overlay mark that land in the same cell keep
  their distinct glyphs (they are not merged; the overlay is drawn over empty cells or its own
  bucket — see §3.7 for the render precedence).
- **Cursor column.** A vertical cursor glyph is drawn in the column corresponding to `T`, over
  cells no mark occupies (shared with M6).
- **Reveal.** Marks with `t > T` are not drawn; at `T = opened_at` only the earliest show, at
  `T = effective_end` the whole graph shows.
- **No data yet.** A focus direction with no samples at or before `T` shows an empty plot area
  with axes and the cursor column, not an error.

### 3.5 Focus connection

The detail follows `App::selected()` (the M5 `ConnId`-tracked selection), identical to M6:
opening does not change the selection; moving the selection while open moves the detail. The
focus connection's `origin`/`responder` come from the `Connection`, its `[opened_at,
effective_end]` X span from `Timeline::x_span(id)`, its wire series from
`Timeline::inflight_series(id)`, all keyed by `ConnId`.

### 3.6 Focus direction

The plotted direction is the connection's **higher-byte** direction (`bytes_o2r ≥ bytes_r2o` →
origin→responder, else responder→origin), the same deterministic rule M6 uses. `max_bytes` is
computed over that direction's samples only. (Both views share `FocusConn::focus_dir`.)

### 3.7 Rendering determinism

The projection is pure and integer-only (time→column, bytes→row are integer proportions, no
float), so `TestBackend` snapshots and the projection's unit tests are deterministic. Axis time
labels reuse M6's fixed-3-decimal-seconds formatter; the Y byte labels reuse M6's SI-abbreviating
integer formatter so a large peak stays inside the gutter. When a wire and an overlay mark
compete for a cell in the final glyph buffer, the renderer draws **wire first, then overlay** so
the diagnostic cwnd line is visible; both keep their own colour.

## 4. Architecture

### Engine (`tcpvisr-engine`)

- `timeline.rs`: add `InFlightSample { t, dir, bytes }`; the `Timeline` `Entry` gains
  `inflight: Vec<InFlightSample>`; add `inflight_series(&self, id) -> &[InFlightSample]` (empty
  slice for an unknown/uncollected id). `x_span` is reused unchanged.
- **`Timeline` construction.** `Timeline::new(Vec<(Connection, Vec<StateSample>)>)` is **kept**
  (empty seq + empty inflight per connection) so the M4/M5 `new` call sites need no edits.
  `Timeline::with_seq` is extended to take a 4-tuple `(Connection, Vec<StateSample>,
  Vec<SeqSample>, Vec<InFlightSample>)` and stable-sorts the in-flight series by `t` like the
  others. `new` delegates with empty seq + inflight vectors. Only `Tracker::into_timeline` and
  M6/M7 tests call `with_seq`. **This changes the `with_seq` signature**, so the ~2 existing
  `with_seq` call sites (the M6 `timeline.rs` and `render.rs`/`app.rs` tests) get an added empty
  in-flight vector — a mechanical edit confined to M6/M7 code, not the M5 `new` fixtures.
- `config.rs`: add `collect_inflight_timeline: bool` (default `false`).
- `tracker.rs`: `ConnTrack` gains `inflight: Vec<InFlightSample>`. When
  `collect_inflight_timeline` is set and not overflowed, for every processed segment push one
  `InFlightSample { t: seg.ts, dir, bytes: sample.in_flight_bytes }` from the **same**
  `MetricSample` M6 already derives (so no second `observe` call). Record it via a
  `record_inflight` helper mirroring `record_seq` (counts against `max_samples`, sets
  `overflowed`). Gate metric derivation on `collect_inflight_timeline` too, so replay derives
  once and both seq + in-flight collection consume that one `MetricSample`. `into_timeline`
  passes the in-flight series through `with_seq`.
- `lib.rs`: re-export `InFlightSample`.

### TUI (`tcpvisr-tui`)

- `inflight.rs` (new, pure): `InFlightPlot`, its `Mark { col, row, glyph, series }` and
  `Series { Wire, Cwnd }` types, glyph constants, and
  `project(wire, overlay, focus_dir, x_span, cursor, width, height) -> Option<InFlightPlot>`
  (mirrors `detail.rs`: `MIN_W`/`MIN_H` guard → `None`; `max_bytes` from the wire focus series;
  numeric-max bucketing per series; cursor column; reveal-to-`T`). Unit-tested from hand-built
  `Vec<InFlightSample>`.
- `app.rs`: `App` gains `detail_view: DetailView` (default `TimeSequence`), `detail_view()`,
  `cycle_detail_view()`. `FocusConn` gains `inflight: &'a [InFlightSample]` (borrowed from the
  `Timeline`) alongside the existing `series`. `open_detail`/`close_detail`/selection are
  unchanged.
- `keys.rs`: navigation mode maps `Tab` → `cycle_detail_view`. Filter mode is unchanged (`Tab`
  is not a `Char`, so it is inert there).
- `render.rs`: when open, dispatch on `app.detail_view()` — `TimeSequence` → the M6 renderer
  (unchanged), `InFlight` → a new `render_inflight` that carves the same gutter/label/legend
  rows, calls `inflight::project`, and draws wire then overlay marks (distinct colours) plus the
  byte/time axes and legend. Extend the footer with `⇥ view`.
- `lib.rs`: re-export `DetailView` and what the bin/tests need.

### CLI (`tcp-visr`)

- `build_replay_app` / `run_replay`: set `collect_inflight_timeline = true` alongside the
  existing `collect_state_timeline` / `collect_seq_timeline`. No new flags. The `SampleCeiling`
  path is unchanged.

Dependency direction is unchanged (TUI → engine → core).

## 5. Success criteria (falsifiable)

1. **In-flight sample emitted per segment.** A tracker with `collect_inflight_timeline = true`
   fed O2R data (seq 100 len 10), then an R2O ACK=110, then O2R data (seq 110 len 5) produces
   O2R `InFlightSample`s with `bytes == 10` then `bytes == 5` (outstanding grows with sent
   bytes, drains on ack) — the values matching `MetricSample.in_flight_bytes`. (Engine unit
   test.)
2. **In-flight is serial-correct across a `u32` wrap.** A direction sending 50 bytes at start
   seq `u32::MAX-100` then 50 bytes at seq `200` (never acked) yields a second `InFlightSample`
   with `bytes == 351` (serial distance across the wrap, not naive subtraction). (Engine unit
   test.)
3. **Series carried through the timeline.** `into_timeline` yields a `Timeline` whose
   `inflight_series(id)` returns the connection's `InFlightSample`s sorted by `t`;
   `inflight_series(unknown_id)` is empty. (Engine unit test.)
4. **In-flight collection counts against the ceiling.** With `collect_inflight_timeline = true`
   and a `max_samples` smaller than the samples a fixture produces, `into_timeline` returns
   `SampleCeiling`. (Engine unit test.)
5. **Flag is orthogonal / off by default.** `collect_inflight_timeline` defaults to `false`; a
   tracker with only `collect_state_timeline` set produces empty in-flight series (the `conns`
   and `metrics` paths retain nothing new). (Engine unit test.)
6. **Point placement (exact indices).** In a plot rectangle `W×H`: a wire sample at
   `(effective_end, bytes = max_bytes)` lands at `(col W-1, row H-1)` (top-right); a sample at
   `(opened_at, bytes 0)` lands at `(col 0, row 0)` (bottom-left); a sample at
   `opened_at + span_t/2` lands at `col (W-1)/2`. No index reaches `W` or `H`. (Projection unit
   test.)
7. **Fixed axes.** The projected X range is `[opened_at, effective_end]` and Y range is
   `[0, max_bytes]` regardless of the cursor; moving the cursor changes which marks are revealed,
   not the axis ranges. (Projection unit test.)
8. **Reveal to `T`.** For wire samples at `t = {0, 10, 20}` and cursor `t = 10`, the projection
   emits the marks at `t = 0` and `t = 10` and omits `t = 20`; at `t = 20` all three appear.
   (Projection unit test.)
9. **Numeric-max column bucketing.** Two wire samples in the same column with `bytes` mapping to
   rows `r1 < r2` render a single wire mark at row `r2` (the peak), not `r1`. (Projection unit
   test.)
10. **Degenerate spans.** A focus connection with a single sample (`opened_at ==
    effective_end`) projects that mark to `(col 0, …)` with no divide-by-zero; a focus direction
    whose samples are all `bytes == 0` (`max_bytes == 0`) projects to row 0. (Projection unit
    test.)
11. **Cursor column.** The projection marks the `T` column with the cursor glyph where no mark
    occupies that cell. (Projection unit test.)
12. **Narrow-terminal guard.** Projecting into a viewport below the minimum inner width/height
    yields `None` (no marks), and `render` shows `widen terminal`. (Projection unit test +
    TestBackend test.)
13. **Overlay hook draws a distinct series.** The projection given a non-empty overlay series
    (synthetic cwnd) emits marks tagged `series == Cwnd` with the cwnd glyph, distinct from the
    `Wire` marks, at their own `(col, row)`; with an **empty** overlay (the replay case) no
    `Cwnd` marks are emitted. (Projection unit test.)
14. **`Tab` cycles the view; `Enter`/`Esc` unchanged.** In navigation mode `Tab` advances
    `detail_view()` `TimeSequence → InFlight → TimeSequence`. `Enter` still opens and `Esc`
    still closes the pane; in filter mode `Tab` does not cycle the view and `Enter`/`Esc` keep
    their filter meaning. (App/keys unit tests.)
15. **Detail view follows selection.** With the In-flight view open, `move_down` changes
    `focus()` to the newly selected connection's id and in-flight series. (App unit test.)
16. **Render — closed is byte-identical to M6/M5.** With the detail closed, `render` into a
    `TestBackend` produces the same buffer as before (existing render assertions still pass;
    `Timeline::new` preserved so M5 fixtures are untouched). (TestBackend test.)
17. **Render — In-flight view open shows the graph.** With the detail open, the view switched to
    In-flight, over a connection with data, `render` shows the `DETAIL <origin> → <responder>`
    title, the In-flight legend (naming the wire glyph), an axis time label, and at least one
    plotted wire glyph. (TestBackend test.)
18. **CLI wiring.** `build_replay_app(<fixture>, cfg with collect_inflight_timeline)` returns an
    `App` whose focus connection's `inflight_series` is non-empty for a fixture with data
    segments; the ceiling path still returns `SampleCeiling`. (Bin integration test driving the
    seam.)

## 6. Failure modes handled

- **No connection selected / none active at `T`** → `Enter` inert; an open detail shows the
  shared empty-state message; the master keeps its M5 empty states. (§3.2, §3.5.)
- **Focus direction has no samples ≤ `T`** → empty plot area with axes + cursor column, not an
  error. (§3.4.)
- **Single-sample / zero-width span / all-zero bytes** → plots into column 0 / the bottom row
  with no divide-by-zero. (§3.4, criterion 10.)
- **`u32` sequence wrap within a connection** → `bytes` is the engine's serial-correct
  `in_flight_bytes`; the TUI does no serial arithmetic. (§2, criterion 2.)
- **Dense capture (many segments per column)** → numeric-max bucketing bounds render to
  `O(cells)`; no unbounded work per frame. (§3.4, criterion 9.)
- **Terminal too narrow/short** → explicit "widen terminal" message, master pane unaffected.
  (§3.3, criterion 12.)
- **Sample-ceiling overflow** (state + seq + in-flight series) → existing fail-fast
  `SampleCeiling` (§3.1, criterion 4).
- **Non-monotonic capture time** → each connection's `InFlightSample` series is stable-sorted by
  `t` at `Timeline` construction (as for `StateSample`/`SeqSample`), so reveal/bucketing see a
  `t`-ordered slice.
- **cwnd overlay absent on replay** → the overlay series is empty, so no `Cwnd` marks are drawn;
  the seam is exercised only by a synthetic-overlay unit test until M12. (§3.4, criterion 13.)

## 7. Testing

- **Engine unit tests** (criteria 1–5) from hand-built segment vectors through `Tracker` with
  `collect_inflight_timeline = true`, asserting the emitted `InFlightSample.bytes` (including
  the wrap case), the `Timeline` accessor, the ceiling, and the default-off flag. Assert the
  sample values, not how `in_flight_bytes` is computed (reuse the M3 derivation).
- **Projection unit tests** (criteria 6–13) from hand-built `Vec<InFlightSample>` and explicit
  viewport sizes, asserting axis ranges and specific `(col, row, glyph, series)` marks —
  placement, fixed axes, reveal-to-`T`, numeric-max bucketing, degenerate spans, cursor column,
  narrow-terminal `None`, and the overlay series. No terminal needed.
- **App / keys unit tests** (criteria 14–15) for `Tab` view-cycling modality (nav vs filter),
  `Enter`/`Esc` unchanged, and detail-follows-selection, from hand-built timelines.
- **`TestBackend` render tests** (criteria 16–17): closed reproduces the prior master;
  In-flight open shows the title, In-flight legend, an axis label, and a wire glyph.
- **Bin integration test** (criterion 18): `build_replay_app` over the `metrics_basic` fixture
  yields a non-empty focus `inflight_series`, and the ceiling seam still fails fast.
- Test behavior, not implementation: assert the emitted samples, the projected marks, and the
  rendered buffer — not how the bucketing or the in-flight derivation is computed.
