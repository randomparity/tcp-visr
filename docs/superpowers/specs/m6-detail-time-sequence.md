# Spec: M6 — Detail: Time/Sequence (Stevens)

**Milestone:** M6 (design §6, §10.M6) · **Issue:** #9 · **Release:** v0.1.0 (Replay MVP)
**ADR:** [ADR-0011 — Detail Time/Sequence: dedicated seq series + character-cell rendering](../../adr/0011-detail-seq-timeline-and-rendering.md)
(builds on [ADR-0004](../../adr/0004-seekable-timeseries-timeline.md),
[ADR-0007](../../adr/0007-metric-derivation-model.md),
[ADR-0009](../../adr/0009-tui-master-list-architecture.md),
[ADR-0010](../../adr/0010-timeline-transport-as-of-t.md))

## 1. Goal

Add the first **detail pane**: a per-connection **Time/Sequence (Stevens)** graph. With a
connection selected in the M5 master list, pressing `Enter` opens a graph of that
connection's TCP **sequence number over time**, marking **retransmits** and **SACK** blocks,
and driven by the transport cursor `T` (the plot reveals data up to `T` with a cursor
column). `Esc` closes it. Playback, seek, and step redraw the graph live.

## 2. Scope

### In scope

- A pure **`SeqSample`** series in `tcpvisr-engine`: one record per data-carrying segment
  (and one per SACK block), carrying `{ t, dir, seq, len, kind }` where `kind` is
  `Data { retransmit, out_of_order }` or `Sack`, with `(dir, seq)` in the *data sender's*
  sequence space (ADR-0011 §1). Collected under a new `EngineConfig.collect_seq_timeline`
  flag, counting against `max_samples`.
- `Timeline` retains each connection's `SeqSample` series and exposes
  **`seq_series(id) -> &[SeqSample]`**; `into_timeline` carries it through.
- A pure **detail projection** in `tcpvisr-tui`: `(series, conn bounds, focus direction,
  cursor T, viewport cells)` → resolved axis ranges + a grid of marks `{ col, row, glyph }`,
  revealing marks with `t ≤ T`, a vertical cursor column at `T`, fixed full-extent axes,
  relative sequence via RFC-1982 serial arithmetic, column bucketing keeping the most-salient
  mark per cell (ADR-0011 §2–§3).
- **`App`** gains a detail-open flag and the focus-connection accessor; `Enter` opens the
  detail for the selected row, `Esc` closes it; transport/navigation keys stay live while
  open. Default (closed) reproduces the M5 full-width master list.
- **Layout / render**: closed → M5 full-width master (unchanged). Open → master left / detail
  right split; the detail block titles the focus connection (`origin → responder`) and draws
  the Stevens grid with a legend for the marks.
- **CLI wiring**: `run_replay` / `build_replay_app` set `collect_seq_timeline = true` so the
  `Timeline` carries seq series; the `max_samples` ceiling still fails fast (design §7).

### Out of scope (deferred, do not build)

- **In-flight / RTT / throughput detail views** and the **`Tab` view-switcher** — M7–M9.
- **Reverse-direction plot and an ACK-progress overlay** on the same graph — the M6 graph
  plots one (higher-byte) direction's data with retransmit/SACK marks (ADR-0011 §3).
- **Per-window axis auto-scale / zoom / pan** — M6 uses fixed full-extent axes (ADR-0011 §3);
  design §14's per-window downsampling beyond simple column bucketing is later work.
- **Always-on master+detail split** (design §6 sketch) — M6 is `Enter`-to-open (ADR-0011 §4).
- **Live timeline** (M11), **names/attribution** (M10/M12).

## 3. User-facing behavior

### 3.1 Entry point

`tcp-visr replay <file>` is unchanged (invocation, non-TTY guard, `--max-samples`). The only
change: the tracker now also collects the seq timeline, so the built `Timeline` can answer
`seq_series(id)`. A `max_samples` overflow still exits non-zero with the actionable
`SampleCeiling` message (names the count, the limit, and `--max-samples`).

### 3.2 Opening and closing the detail

- In navigation mode, **`Enter`** on the selected row opens the detail pane for that
  connection. With no selection (empty active set) `Enter` is a no-op.
- **`Esc`** in navigation mode closes the detail pane (returns to full-width master). In
  filter-input mode `Esc` keeps its M4 meaning (clear the filter); it does not close the
  detail.
- While the detail is open, all navigation and transport keys keep working: `j`/`k`/`↑`/`↓`
  move the selection (and the detail follows the newly selected connection), `space`/`←→`/
  `+`/`-`/`,`/`.` drive the transport, `/` filters, `s`/`S` sort, `q`/`Ctrl-C` quit.
- If the selected connection stops being active at `T` (a seek/step reconciles the selection,
  M5 §3.5), the detail follows the new selection; if nothing is active, the detail pane shows
  an empty-state message and the master list shows its own "no connections active" state.

### 3.3 Layout (detail open)

```
┌ tcp-visr — capture.pcap  (47 connections, skipped 0) ─────[ ▶ 2.0x  t=12.480s / 38.200s ]┐
│ PEER                SERVICE  STATE        ↑BYTES  ↓BYTES │ DETAIL 10.0.0.5:51324 → 140..:443│
│▸140.82.121.3:443    https    ESTABLISHED    1234   34000 │ Time/Sequence   · retrans  ╎ sack │
│ 10.0.0.9:22         ssh      ESTABLISHED~    840    2100  │ seq                              ╷ │
│ …                                                        │  3200 ┤        ░░░▓·               │
│                                                          │       ┤   ░░░▓▓                    │
│                                                          │     0 ┼───────────────────────────│
│                                                          │       0.000s              38.200s  │
├──────────────────────────────────────────────────────────┴──────────────────────────────────┤
│ space play/pause  ←→ seek  +/- speed  ,/. step  ⏎ open  esc close  / filter  s sort  q quit    │
└───────────────────────────────────────────────────────────────────────────────────────────────┘
```

- The screen splits into a **left** master pane and a **right** detail pane. The master pane
  renders exactly as M5 (header title kept on the outer block; rows resolved as of `T`;
  selection marker; the M5 columns). The right pane is a bordered block titled
  `DETAIL <origin> → <responder>`.
- The detail body shows the **Time/Sequence** graph: a Y axis labeled with the sequence range
  (relative, starting at 0), an X axis labeled with the time range in seconds, the plotted
  marks, and a one-line **legend** naming the retransmit and SACK glyphs.
- **Narrow terminals.** If the detail pane is too narrow to draw axes and a plot area (fewer
  than a minimum inner width/height), the pane shows `widen terminal to view graph` instead
  of a truncated plot. The master pane keeps rendering.
- **Footer** advertises `⏎ open` and `esc close` alongside the M5 transport/sort/filter hints.

### 3.4 The Stevens graph (semantics)

For the focus connection (§3.5) and its focus direction (§3.6):

- **X axis (time)** spans `[opened_at, effective_end]` — the connection's full active
  interval (M5 `effective_end`: `last_at` if closed, else capture end). This span is **not**
  `Connection.last_at` (which, for a still-open connection, precedes the capture end); it is
  read from the new `Timeline::x_span(id)` accessor (§4) so the detail's time axis matches the
  master header's `t=… / total`. It is **fixed**; scrubbing does not rescale it.
- **Y axis (sequence)** spans `[0, max_rel]`. A sample's **relative sequence** (its row
  position) is `rel(s) = s.seq.serial_diff(baseline)`, where `baseline` is the RFC-1982
  serial-**minimum** `seq` over the focus direction's samples (ADR-0006; never naive
  subtraction). The axis top `max_rel` is `max(rel(s) + s.len)` over those samples — i.e. the
  furthest byte the stream reached, so a point is never plotted above the axis (a `Sack`
  sample has `len = 0`, so it contributes only its own row). Fixed; scrubbing does not rescale.
- **Degenerate spans.** If the time span is zero (`opened_at == effective_end`, a
  single-timestamp connection) every revealed mark and the cursor column go in **column 0**.
  If `max_rel == 0` (the focus direction has no byte extent — e.g. only `Sack` marks at the
  baseline) every mark goes on the **bottom row**. Neither case is an error; the mapping
  divides only when the span is non-zero.
- **Marks.** Each `SeqSample` of the focus direction with `t ≤ T` maps to a cell
  `(col, row)`: `col` from `t` linearly across the X span, `row` from `rel(s)` linearly across
  `[0, max_rel]` (row 0 at the bottom = sequence 0; the point plots at the segment **start**
  seq, so a retransmit dips back to its earlier row). The glyph is by kind:
  - `Data { retransmit: false, out_of_order: false }` → the plain data glyph,
  - `Data { out_of_order: true, retransmit: false }` → the out-of-order glyph,
  - `Data { retransmit: true, .. }` → the retransmit glyph,
  - `Sack` → the SACK glyph.
- **Column bucketing (downsampling).** When multiple marks fall in one cell, the cell shows
  the **most salient** glyph by priority `retransmit > sack > out_of_order > data`. This keeps
  the plot legible and its cost `O(cells)` regardless of sample count (ADR-0011 §2).
- **Cursor column.** A vertical cursor glyph is drawn in the column corresponding to `T`
  (over empty cells; a mark in that column keeps its mark glyph).
- **Reveal.** Marks with `t > T` are not drawn. At `T = opened_at` only the earliest
  mark(s) show; at `T = effective_end` the whole graph shows.
- **No data yet.** A focus connection whose focus direction has no samples at or before `T`
  (e.g. cursor at the very start, or a direction that has sent nothing) shows an empty plot
  area with axes and the cursor column, not an error.

### 3.5 Focus connection

The detail follows `App::selected()` (the M5 `ConnId`-tracked selection). Opening the detail
does not change which connection is selected; moving the selection while open moves the
detail. The focus connection's static view (`origin`, `responder`) comes from the
`Connection`; its `[opened_at, effective_end]` X span from `Timeline::x_span(id)` (§4); and
its `seq_series` from `Timeline::seq_series(id)` — all keyed by `ConnId`.

### 3.6 Focus direction

The plotted direction is the connection's **higher-byte** direction: origin→responder if
`bytes_o2r ≥ bytes_r2o`, else responder→origin. This is a deterministic function of the
`Connection` view (the end-of-capture totals). The Y-axis baseline and `max_rel` are computed
over that direction's samples only.

### 3.7 Rendering determinism

The projection is pure and integer-only (time→column and sequence→row are integer
proportions, no float), so `ratatui::TestBackend` snapshots and the projection's own unit
tests are deterministic. Axis time labels reuse the M5 fixed-3-decimal-seconds formatter.

## 4. Architecture

### Engine (`tcpvisr-engine`)

- `timeline.rs` (or a new `seq.rs` re-exported alongside): add `SeqKind`, `SeqSample`; the
  `Timeline` `Entry` gains a `seq: Vec<SeqSample>`; add `seq_series(&self, id) -> &[SeqSample]`
  (empty slice for an unknown/uncollected id) and `x_span(&self, id) -> Option<(Nanos, Nanos)>`
  returning the connection's `[opened_at, effective_end]` (the private `effective_end` is not
  otherwise reachable, and `Connection.last_at` is the wrong right edge for a still-open
  connection). `Timeline::new` takes the seq series per connection (extend its input tuple)
  and sorts each by `t` (stable), like `StateSample`.
- `config.rs`: add `collect_seq_timeline: bool` (default `false`).
- `tracker.rs`: when the flag is set, for every connection and every processed segment, run
  the existing metric derivation and, from its result + the segment, push `SeqSample`s: one
  `Data` point per data-carrying segment (payload > 0) in its own direction, and one `Sack`
  point per SACK block (direction = opposite of the carrying segment; `seq` = block left;
  `len` = 0). Each retained `SeqSample` counts against `max_samples` via the shared counter;
  `into_timeline` passes the seq series through.
- `lib.rs`: re-export `SeqSample`, `SeqKind`.

### TUI (`tcpvisr-tui`)

- `detail.rs` (new, pure): the `SeqPlot` projection and its `Mark { col, row, glyph }` /
  axis-range types; `SeqPlot::project(series, x_span, focus_dir, baseline/max_rel, cursor,
  width, height) -> SeqPlot`. Glyph constants live here. Unit-tested from hand-built
  `Vec<SeqSample>`.
- `app.rs`: `App` gains `detail_open: bool`, `open_detail()`, `close_detail()`,
  `is_detail_open()`, and a `focus() -> Option<FocusConn>` that resolves the selected
  connection's static view + `seq_series` from the `Timeline`. Selection/reconciliation is
  unchanged (M5).
- `keys.rs`: navigation mode maps `Enter` → `open_detail`, `Esc` → `close_detail`. Filter
  mode is unchanged (`Esc` still clears the filter; `Enter` still confirms).
- `render.rs`: when the detail is closed, render the M5 master full-width (unchanged). When
  open, split horizontally and render the master on the left and the detail (title, axes,
  projected marks, legend, or the narrow-terminal message) on the right; extend the footer
  hints.
- `lib.rs`: re-export what the bin/tests need.

### CLI (`tcp-visr`)

- `build_replay_app` / `run_replay`: set `collect_seq_timeline = true` in the `EngineConfig`
  (alongside the existing `collect_state_timeline = true`). No new flags. The
  `SampleCeiling` path is unchanged.

Dependency direction is unchanged (TUI → engine → core).

## 5. Success criteria (falsifiable)

1. **Data point emitted per data segment.** A tracker with `collect_seq_timeline = true` fed a
   connection with two data segments in one direction (seq 100 len 10, seq 110 len 20)
   produces two `Data` `SeqSample`s in that direction with those `seq`/`len` and
   `retransmit == false`. (Engine unit test.)
2. **Retransmit classified on the seq point.** A behind-frontier re-send after a gap ≥
   `reorder_window` yields a `Data { retransmit: true }` `SeqSample`; an in-window
   behind-frontier segment yields `Data { out_of_order: true, retransmit: false }`. (Engine
   unit test, reusing the M3 derivation.)
3. **SACK block emitted in the acked direction.** A segment in direction R2O carrying a SACK
   block `(L, R)` produces a `Sack` `SeqSample` with `dir == OriginToResponder` (the acked
   direction), `seq == L`, `len == 0`. (Engine unit test.)
4. **Seq series carried through the timeline.** `into_timeline` on such a tracker yields a
   `Timeline` whose `seq_series(id)` returns the connection's `SeqSample`s sorted by `t`;
   `seq_series(unknown_id)` is empty. (Engine unit test.)
5. **Seq collection counts against the ceiling.** With `collect_seq_timeline = true` and a
   `max_samples` smaller than the number of state+seq samples a fixture produces,
   `into_timeline` returns `SampleCeiling`. (Engine unit test.)
6. **Relative sequence is serial-correct across a `u32` wrap.** A focus direction with data
   segments at start seq `u32::MAX-100` (len 50) then `200` (len 50) projects the two points
   at relative rows `0` and `301` (`200.serial_diff(u32::MAX-100)`, a forward advance, not a
   fold), and the Y-axis top `max_rel` is `351` (`301 + 50`, the second segment's extent).
   (Projection unit test.)
7. **Reveal to `T`.** For a series with marks at `t = {0, 10, 20}` and a cursor at `t = 10`,
   the projection emits the marks at `t = 0` and `t = 10` and omits the one at `t = 20`; at
   `t = 20` all three appear. (Projection unit test.)
8. **Fixed axes.** The projected X range is `[opened_at, effective_end]` and the Y range is
   `[0, max_rel]` regardless of the cursor value; moving the cursor changes which marks are
   revealed but not the axis ranges. (Projection unit test.)
9. **Column bucketing keeps the most-salient glyph.** A plain data point and a retransmit
   point that fall in the same cell render the retransmit glyph; a data point and a SACK in
   one cell render the SACK glyph (priority `retransmit > sack > out_of_order > data`).
   (Projection unit test.)
10. **Point placement.** A data point whose `rel(s)` equals `max_rel` at `t = midpoint` lands
    in the middle column and the top row of the plot area; a point at `(opened_at, rel 0)`
    lands at the bottom-left. (Projection unit test.)
10a. **Degenerate spans.** A focus connection with a single data segment (`opened_at ==
    effective_end`, one sample) projects that mark to column 0 and the bottom row without a
    divide-by-zero; a focus direction with only a `Sack` mark at the baseline (`max_rel == 0`)
    projects it to the bottom row. (Projection unit test.)
11. **Cursor column.** The projection marks the column corresponding to `T` with the cursor
    glyph where no data mark occupies that cell. (Projection unit test.)
12. **Narrow-terminal guard.** Projecting into a viewport below the minimum inner
    width/height yields the "too small" outcome (no marks), and `render` shows
    `widen terminal`. (Projection unit test + TestBackend test.)
13. **`Enter` opens, `Esc` closes.** In navigation mode `Enter` sets `is_detail_open()` true
    (when a row is selected) and `Esc` sets it false; with no selection `Enter` is a no-op. In
    filter mode `Enter` confirms and `Esc` clears the filter, neither toggling the detail.
    (App/keys unit tests.)
14. **Detail follows selection.** With the detail open, `move_down` changes `focus()` to the
    newly selected connection's id/series. (App unit test.)
15. **Render — closed is unchanged.** With the detail closed, `render` into a `TestBackend`
    is the M5 full-width master (existing M5 render assertions still hold). (TestBackend test.)
16. **Render — open shows the graph.** With the detail open over a connection that has data,
    `render` shows the `DETAIL <origin> → <responder>` title, the mark legend, an axis time
    label, and at least one plotted mark glyph. (TestBackend test.)
17. **CLI wiring.** `build_replay_app(<fixture>, cfg with collect_seq_timeline)` returns an
    `App` whose focus connection's `seq_series` is non-empty for a fixture with data segments;
    the ceiling path still returns `SampleCeiling`. (Bin integration test driving the seam.)

## 6. Failure modes handled

- **No connection selected / none active at `T`** → `Enter` is inert; an open detail shows an
  empty-state message; the master list keeps its M5 empty states. (§3.2, §3.4.)
- **Focus direction has no samples ≤ `T`** → empty plot area with axes + cursor column, not an
  error. (§3.4.)
- **Single-sample / zero-width span** → a connection whose focus direction has one data
  segment (`opened_at == effective_end`, or `max_rel == 0`) plots into column 0 / the bottom
  row with no divide-by-zero. (§3.4, criterion 10a.)
- **`u32` sequence wrap within a connection** → relative sequence via RFC-1982 serial
  arithmetic; a wrap is a forward advance, never a fold. (§3.4, criterion 6.)
- **Dense capture (many segments per column)** → column bucketing bounds render to
  `O(cells)` and keeps the most-salient glyph; no unbounded work per frame. (§3.4.)
- **Terminal too narrow/short for a graph** → explicit "widen terminal" message, master pane
  unaffected. (§3.3, criterion 12.)
- **Sample-ceiling overflow** (state + seq series) → existing fail-fast `SampleCeiling`
  (§3.1, criterion 5).
- **Non-monotonic capture time** → each connection's `SeqSample` series is stable-sorted by
  `t` at `Timeline` construction (as for `StateSample`, M5 §3.3), so reveal/bucketing see a
  `t`-ordered slice.

## 7. Testing

- **Engine unit tests** (criteria 1–5) from hand-built segment vectors through `Tracker` with
  `collect_seq_timeline = true`, asserting the emitted `SeqSample`s and the `Timeline`
  accessor. Reuse the M3 derivation fixtures for retransmit/out-of-order/SACK classification —
  assert the *seq point's* kind, not how it is computed.
- **Projection unit tests** (criteria 6–12, incl. 10a) from hand-built `Vec<SeqSample>` and
  explicit viewport sizes, asserting axis ranges and specific `(col, row, glyph)` marks —
  including the `u32`-wrap relative-sequence case, reveal-to-`T`, bucketing priority,
  placement, degenerate (zero-width / `max_rel == 0`) spans, the cursor column, and the
  narrow-terminal outcome. No terminal needed.
- **App / keys unit tests** (criteria 13–14) for open/close modality and detail-follows-
  selection, from hand-built timelines.
- **`ratatui::TestBackend` render tests** (criteria 15–16): closed reproduces the M5 master;
  open shows the title, legend, an axis label, and a mark glyph.
- **Bin integration test** (criterion 17): `build_replay_app` over the `metrics_basic`
  fixture yields a non-empty focus `seq_series`, and the ceiling seam still fails fast.
- Test behavior, not implementation: assert the emitted samples, the projected marks, and the
  rendered buffer — not how the interval math, bucketing, or serial arithmetic is computed.
