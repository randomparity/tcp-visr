# Spec: M9 — Detail: Throughput/goodput

**Milestone:** M9 (design §6, §10.M9) · **Issue:** #12 · **Release:** v0.1.0 (Replay MVP)
**ADR:** [ADR-0014 — Detail Throughput/goodput: windowed total + goodput series, the goodput derivation, and the finalized four-view switcher](../../adr/0014-detail-throughput-goodput.md)
(builds on [ADR-0004](../../adr/0004-seekable-timeseries-timeline.md),
[ADR-0007](../../adr/0007-metric-derivation-model.md),
[ADR-0009](../../adr/0009-tui-master-list-architecture.md),
[ADR-0010](../../adr/0010-timeline-transport-as-of-t.md),
[ADR-0011](../../adr/0011-detail-seq-timeline-and-rendering.md),
[ADR-0012](../../adr/0012-detail-inflight-and-view-switcher.md),
[ADR-0013](../../adr/0013-detail-rtt.md))

## 1. Goal

Add the fourth and final **detail view**: a per-connection **Throughput/goodput** graph plotting
the **trailing-window throughput** (all data bytes/sec) and the **goodput** (non-retransmitted data
bytes/sec) over time, driven by the transport cursor `T` (reveal to `T`, cursor column, fixed
axes). Reach it with the existing **`Tab`** view-switcher, now cycling Time/Sequence → In-flight →
RTT → Throughput → Time/Sequence — **finalizing** the switcher (design §10.M9). The gap between the
two lines is the retransmitted rate ("goodput vs retransmitted").

## 2. Scope

### In scope

- A pure **`ThroughputSample`** series in `tcpvisr-engine`, carrying `{ t, dir, throughput_bps,
  goodput_bps }` where:
  - `t` is the segment's timestamp,
  - `dir` is the **sending data-flow direction** — the segment's own direction, i.e.
    `sample.dir` **not** flipped (throughput belongs to the sender; ADR-0014 §1),
  - `throughput_bps` is the trailing-window sum of **all** data payload bytes over
    `throughput_window`, byte-identical to the engine's `MetricSample.throughput_bps` (the
    collector reads it, does not re-derive it differently),
  - `goodput_bps` is the trailing-window sum over the **non-retransmitted** data payload bytes of
    the same window: `goodput_bps ≤ throughput_bps` always; on a loss-free flow they are equal
    (ADR-0014 §2). Both are the defensive-divide `bytes · 8 · 1e9 / window` in `u128` then narrowed
    (no float).
  - Both directions are snapshotted per segment (mirroring the M7 in-flight collector), **gated on
    a direction having sent ≥1 data byte** — a direction that never sent data contributes no sample.
  - Collected under a new `EngineConfig.collect_throughput_timeline` flag, counting against
    `max_samples`.
- `Timeline` retains each connection's `ThroughputSample` series and exposes
  **`throughput_series(id) -> &[ThroughputSample]`**; `with_seq` carries it through.
- A pure **throughput projection** in `tcpvisr-tui` (`throughput.rs`, sibling of
  `inflight.rs`/`rtt.rs`): `(wire series, focus direction, x_span, cursor T, viewport cells)` →
  resolved axis ranges + a grid of marks `{ col, row, glyph, series }` with `Series { Throughput,
  Goodput }`. Each revealed (`t ≤ T`) focus-direction sample emits a `Throughput` mark at
  `row(throughput_bps)` and a `Goodput` mark at `row(goodput_bps)`. Y = `[0, max_rate]` over the
  focus direction's `throughput_bps` and `goodput_bps` (the throughput maximum); X = `x_span`;
  **numeric-max** column bucketing per series applied only to revealed samples; a vertical cursor
  column at `T`; `None` below the minimum viewport. **No overlay parameter** — design §10.M12 does
  not overlay throughput (ADR-0014 §4).
- **`App`** gains a `DetailView::Throughput` variant; `cycle_detail_view` extends to the four-way
  cycle; `FocusConn` gains `throughput: &'a [ThroughputSample]` (borrowed from the `Timeline`).
  `Tab` cycles the view. `Enter`/`Esc` keep their open/close meaning.
- **Layout / render**: closed → M5/M6/M7/M8 full-width master (unchanged). Open → master left /
  detail right split; the pane renders the view named by `detail_view()`. The throughput view
  titles the focus connection (shared `DETAIL <origin> → <responder>` block), draws the throughput
  and goodput marks with a Y axis in **bits/sec** (adaptive `bps`/`kbps`/`Mbps`/`Gbps`) and an X
  axis in seconds, and a one-line legend naming the two glyphs. Footer is unchanged (`⇥ view`
  already advertised in M7).
- **CLI wiring**: `replay_engine_config` sets `collect_throughput_timeline = true` alongside the
  existing four flags. No new CLI flags. The `SampleCeiling` path is unchanged.

### Out of scope (deferred, do not build)

- **A kernel throughput/goodput overlay.** Design §10.M12 overlays only M7 (cwnd) and M8 (srtt);
  no milestone fills a throughput overlay, so building an overlay seam here would be a phantom
  feature (ADR-0014 §4). The projection takes no overlay parameter.
- **Adding `goodput_bps` to `MetricSample` / the `metrics` command JSON** — churns the frozen M3
  oracle (ADR-0014 §2, rejected). A dedicated `ThroughputSample` keeps `MetricSample` frozen.
- **A separate `--goodput-window` knob** — goodput reuses the `throughput_window` (ADR-0014 §2).
- **Changes to the throughput derivation itself** (`throughput_window`, the byte-window sum) —
  frozen from M3; `throughput_bps` stays byte-identical.
- **Connected-polyline / area-fill rendering** — M9 draws top-edge marks (as M6/M7/M8); a connected
  line is later polish behind the same projection seam.
- **Per-window axis auto-scale / zoom / pan** — M9 uses fixed full-extent axes (as M6/M7/M8).
- **Reverse-direction throughput overlay** — M9 plots the one higher-byte focus direction.
- **Live timeline** (M11), **names/attribution** (M10/M12), **`Tick`-driven decay-to-zero** past
  the last segment (live only; replay has no `Tick`s).

## 3. User-facing behavior

### 3.1 Entry point

`tcp-visr replay <file>` is unchanged (invocation, non-TTY guard, `--max-samples`). The only
change: the tracker now also collects the throughput timeline, so the built `Timeline` can answer
`throughput_series(id)`. A `max_samples` overflow still exits non-zero with the actionable
`SampleCeiling` message.

### 3.2 Switching views

- With the detail pane **open**, in navigation mode, **`Tab`** cycles the detail view:
  Time/Sequence → In-flight → RTT → Throughput → Time/Sequence. The chosen view persists while
  scrubbing and while the selection moves.
- `Tab` when the pane is **closed** is a no-op on layout (it may still update the remembered view;
  nothing is drawn until `Enter` opens the pane).
- In **filter-input** mode `Tab` is inert for view-switching. All other M6/M7/M8 modality is
  unchanged: `Enter` opens (nav) / confirms filter, `Esc` closes (nav) / clears filter.
- All navigation and transport keys keep working while the pane is open, for any view.

### 3.3 Layout (Throughput view open)

```
┌ tcp-visr — capture.pcap  (47 connections, skipped 0) ────[ ▶ 2.0x  t=12.480s / 38.200s ]┐
│ PEER                SERVICE  STATE        ↑BYTES  ↓BYTES │ DETAIL 10.0.0.5:51324 → 140..:443│
│▸140.82.121.3:443    https    ESTABLISHED    1234   34000 │ Throughput  . total  # goodput   │
│ 10.0.0.9:22         ssh      ESTABLISHED~    840    2100  │ Mbps                           ╷ │
│ …                                                        │  9.4M ┤   .# .#. .               │
│                                                          │       ┤ .#  .#                    │
│                                                          │  0bps ┼───────────────────────────│
│                                                          │       0.000s              38.200s  │
├──────────────────────────────────────────────────────────┴──────────────────────────────────┤
│ space play/pause  ←→ seek  +/- speed  ,/. step  ⏎ open  esc close  ⇥ view  / filter  s sort  q quit │
└───────────────────────────────────────────────────────────────────────────────────────────────┘
```

- The screen splits into a **left** master pane (renders exactly as M5/M6/M7/M8) and a **right**
  detail pane, a bordered block titled `DETAIL <origin> → <responder>` (shared with M6/M7/M8).
- The throughput body shows a **Y axis labeled in bits/sec** (`0bps` at the bottom, `max_rate` at
  the top in adaptive units), an X axis labeled with the time range in seconds, the plotted
  throughput and goodput marks, and a one-line **legend** naming the total and goodput glyphs.
- **Narrow terminals.** The projection is handed the inner plot rectangle after `render.rs` carves
  the gutter, X-label row, and legend row; below the minimum plot rectangle the pane shows `widen
  terminal to view graph` (shared with M6/M7/M8). The master pane keeps rendering.
- **Footer** is the M7 footer (already advertises `⇥ view`).

### 3.4 The throughput graph (semantics)

For the focus connection (§3.5) and its focus direction (§3.6):

- **X axis (time)** spans `[opened_at, effective_end]` from `Timeline::x_span(id)` (shared with
  M6/M7/M8); fixed, does not rescale with the cursor.
- **Y axis (bits/sec, labeled adaptively)** spans `[0, max_rate]` where `max_rate = max` over the
  focus direction's `throughput_bps` and `goodput_bps` (plain `u64` max; equals the throughput
  maximum since goodput ≤ throughput). Fixed; does not rescale.
- **Plot rectangle.** `W` columns × `H` rows of cells, excluding border, Y-label gutter, X-label
  row, and legend row (`render.rs` carves them). Row 0 is the **bottom** (`0bps`), row `H-1` the
  top.
- **Coordinate mapping (exact, clamped),** identical to M7/M8 with `span_t = effective_end.0 −
  opened_at.0`:
  - `col(t) = if span_t == 0 { 0 } else { ((t − t0) * (W−1)) / span_t }`, clamped to `0..=W-1`.
  - `row(v) = if max_rate == 0 { 0 } else { (v * (H−1)) / max_rate }`, clamped to `0..=H-1`.
    `v == max_rate` maps to row `H-1`; the degenerate `span_t == 0` / `max_rate == 0` cases collapse
    to column 0 / bottom row via guards, no division.
- **Marks.** Each focus-direction sample with `t ≤ T` emits a `Throughput` mark at `(col(t),
  row(throughput_bps))` with the total glyph and a `Goodput` mark at `(col(t), row(goodput_bps))`
  with the goodput glyph. Because `goodput_bps ≤ throughput_bps`, the goodput mark is at or below
  the total mark.
- **Column bucketing (downsampling).** Applied **only to revealed samples** (`t ≤ T`). Per
  (column, series) the tallest mark (max `row`) is kept — the peak for that time bucket. This
  bounds render to `O(cells)` regardless of sample count.
- **Cursor column.** A vertical cursor glyph is drawn in the column corresponding to `T`, over
  cells no mark occupies (shared with M6/M7/M8).
- **Reveal.** Marks with `t > T` are not drawn.
- **No data yet.** A focus direction with no throughput samples at or before `T` shows an empty
  plot area with axes and the cursor column, not an error.

### 3.5 Focus connection

Identical to M6/M7/M8: the detail follows `App::selected()` (the M5 `ConnId`-tracked selection).
Opening does not change the selection; moving the selection while open moves the detail. The focus
connection's `origin`/`responder` come from the `Connection`, its `[opened_at, effective_end]` X
span from `Timeline::x_span(id)`, its throughput series from `Timeline::throughput_series(id)`, all
keyed by `ConnId`.

### 3.6 Focus direction

The plotted direction is the connection's **higher-byte** direction (`bytes_o2r ≥ bytes_r2o` →
origin→responder, else responder→origin), the same deterministic rule M6/M7/M8 use, exposed as
`FocusConn::focus_dir`. Because `ThroughputSample.dir` is the sending direction (ADR-0014 §1), the
focus direction's throughput is exactly the samples tagged with that direction — the higher-byte
sender's own rate. `max_rate` is computed over that direction's samples only.

### 3.7 Rendering determinism

The projection is pure and integer-only (time→column, bits/sec→row are integer proportions, no
float; the goodput sum is integer). Axis time labels reuse M6/M7/M8's fixed-3-decimal-seconds
formatter; the Y axis labels the rate via an integer, **unit-adapting** formatter `fmt_rate(bps)` —
it picks `bps`/`kbps`/`Mbps`/`Gbps` by magnitude and prints `<whole>.<3-frac><unit>` (e.g.
`800bps`, `1.500Mbps`, `2.000Gbps`), mirroring `fmt_rtt`, so a slow flow does not collapse to
`0.000Mbps`. It is integer-only; the plotted rows are unaffected (`row()` maps raw bits/sec), only
the axis label adapts units. **Same-cell precedence is resolved in the projection**: the grid is
written in the order **throughput, then goodput**, so when the two series map to the *same*
`(col, row)` (a loss-free column, `goodput == throughput`) the goodput mark wins that cell (the
useful rate shows over the total). Where they map to *different* rows (`goodput < throughput`) both
marks survive and the renderer draws each in its own colour.

## 4. Architecture

### Engine (`tcpvisr-engine`)

- `timeline.rs`: add `ThroughputSample { t, dir, throughput_bps, goodput_bps }`; the `Timeline`
  `Entry` gains `throughput: Vec<ThroughputSample>`; add `throughput_series(&self, id) ->
  &[ThroughputSample]` (empty slice for an unknown/uncollected id). `x_span` is reused unchanged.
  `ConnSeries` becomes a 6-tuple `(Connection, Vec<StateSample>, Vec<SeqSample>,
  Vec<InFlightSample>, Vec<RttSample>, Vec<ThroughputSample>)`; `with_seq` stable-sorts the
  throughput series by `t` like the others; `new` delegates with an empty throughput vector. **This
  changes the `with_seq` signature**, so existing `with_seq` call sites (the `tracker.rs`
  `into_timeline`, and M6/M7/M8 tests in `timeline.rs`/`app.rs`/`render.rs`) get an added empty
  throughput vector — a mechanical edit confined to those files, not the M5 `new` fixtures.
- `metrics.rs`: the per-direction throughput window entry becomes `(Nanos, u32, bool)` (ts, len,
  retransmit). `MetricState::observe` passes the segment's `retransmit` classification into the
  window push. Add a pure read `throughput_at(&self, dir: Direction, t: Nanos, cfg: &EngineConfig)
  -> Option<(u64, u64)>` returning `(throughput_bps, goodput_bps)` (`None` if `dir` never sent
  data) — mirroring `in_flight(dir)`. `observe` fills `MetricSample.throughput_bps` from its `.0`,
  **mapping `None` to `0`** for a direction that has not sent data — byte-identical to the current
  `throughput()` (which sums an empty window to `0`), so the frozen M3 oracle is undisturbed. The
  window push/evict logic is unchanged except for carrying the flag.
- `config.rs`: add `collect_throughput_timeline: bool` (default `false`); add a `defaults_off` unit
  test mirroring `rtt_timeline_defaults_off`; update the `struct_excessive_bools` justification to
  five gates.
- `tracker.rs`: `ConnTrack` gains `throughput: Vec<ThroughputSample>`. Add `record_throughput`
  (mirrors `record_inflight`: counts against `max_samples`, sets `overflowed`) and
  `collect_throughput_points(idx, seg)`: when `collect_throughput_timeline` is set and not
  overflowed, for each direction call `throughput_at(dir, seg.ts, cfg)` and, when it returns
  `Some((tp, gp))`, record `ThroughputSample { t: seg.ts, dir, throughput_bps: tp, goodput_bps: gp
  }`. Call it from `observe_segment` right after `collect_rtt_points`. Extend the `want to derive`
  guard to include `collect_throughput_timeline`. `into_timeline` passes the throughput series
  through `with_seq`.
- `lib.rs`: re-export `ThroughputSample`.

### TUI (`tcpvisr-tui`)

- `throughput.rs` (new, pure): `ThroughputPlot`, its `Mark { col, row, glyph, series }` and `Series
  { Throughput, Goodput }` types, glyph constants (`THROUGHPUT_GLYPH`, `GOODPUT_GLYPH`,
  `CURSOR_GLYPH`; `MIN_W`/`MIN_H`), and `project(wire, focus_dir, x_span, cursor, width, height) ->
  Option<ThroughputPlot>`. Mirrors `rtt.rs` **minus the overlay**: `MIN_W`/`MIN_H` guard → `None`;
  `max_rate` from the focus `throughput_bps`/`goodput_bps`; numeric-max bucketing per series; cursor
  column; reveal-to-`T`. Same-cell order: throughput then goodput. Unit-tested from hand-built
  `Vec<ThroughputSample>`.
- `app.rs`: `DetailView` gains `Throughput`; `cycle_detail_view` becomes the four-way cycle;
  `FocusConn` gains `throughput: &'a [ThroughputSample]` populated from
  `timeline.throughput_series(id)`. `open_detail` / `close_detail` / selection unchanged.
- `render.rs`: when open, the `detail_view()` match gains a `Throughput` arm calling a new
  `render_throughput_body` that carves the same gutter/label/legend rows, calls
  `throughput::project`, and draws throughput then goodput marks (distinct colours) plus the
  bits/sec and time axes and legend. Add an integer `fmt_rate(bps) -> String` (bps →
  `<unit>.<3-frac>`). The footer is unchanged.
- `lib.rs`: re-export `throughput::{ThroughputPlot, Series as ThroughputSeries}` (naming to avoid
  clashing with `inflight::Series`/`rtt::Series`).
- `keys.rs`: unchanged — `Tab` already maps to `cycle_detail_view`; the four-way cycle is in
  `app.rs`.

### CLI (`tcp-visr`)

- `replay_engine_config`: set `collect_throughput_timeline = true` alongside the existing four
  flags. No new flags. The `SampleCeiling` path is unchanged.

Dependency direction is unchanged (TUI → engine → core).

## 5. Success criteria (falsifiable)

1. **Throughput is attributed to the sending flow (not flipped).** A tracker with
   `collect_throughput_timeline = true` fed an O2R data segment (100 B) produces a
   `ThroughputSample` with `dir == OriginToResponder` (the sender), not `ResponderToOrigin`.
   (Engine unit test.)
2. **`throughput_bps` matches the M3 window sum.** A direction that sends 100 B of data at `t` with
   a 1 s window yields `throughput_bps == 800` (`100·8·1e9 / 1e9`) on the sample at that time,
   equal to `MetricSample.throughput_bps`. (Engine unit test.)
3. **Goodput excludes retransmitted bytes; the gap is the retransmit rate.** A direction that sends
   100 B new data then retransmits the same 100 B within the window yields a sample whose
   `throughput_bps == 2·goodput_bps` (total counts both, goodput counts only the new 100 B). On a
   loss-free flow `goodput_bps == throughput_bps`. (Engine unit test.)
4. **Series carried through the timeline.** `into_timeline` yields a `Timeline` whose
   `throughput_series(id)` returns the connection's `ThroughputSample`s sorted by `t`;
   `throughput_series(unknown_id)` is empty. (Engine unit test.)
5. **Throughput collection counts against the ceiling.** With `collect_throughput_timeline = true`
   and a `max_samples` smaller than the samples a fixture produces, `into_timeline` returns
   `SampleCeiling`. (Engine unit test.)
6. **Flag is orthogonal / off by default.** `collect_throughput_timeline` defaults to `false`; a
   tracker with only `collect_state_timeline` set produces an empty throughput series.
   (Engine/config unit tests.)
7. **A direction that never sent data contributes no sample.** A connection where R2O sends only
   pure ACKs (no payload) yields **no** R2O `ThroughputSample`; the O2R data sender does.
   (Engine unit test.)
7a. **The sender flow is sampled at a reverse-direction segment's time, showing decay (the
    both-directions snapshot, ADR-0014 §1).** Fed one O2R data burst at `t = 0`, then an R2O pure
    ACK at a later `t` inside the window and a second R2O pure ACK *after* the window has elapsed,
    the O2R `ThroughputSample` series contains a sample **at each R2O ACK's timestamp** (not only at
    the O2R send), and the post-window sample's `throughput_bps` has **decayed** below the burst
    sample's (bytes aged out of the trailing window; the final one is `0` once every byte is older
    than the window). A sparse "sample only on the sender's own data segments" implementation would
    produce only the `t = 0` sample and fail this. (Engine unit test.)
8. **Point placement (exact indices).** In a plot rectangle `W×H`: a sample at `(effective_end,
   throughput = goodput = max_rate)` lands a mark at `(col W-1, row H-1)` (the two coincide, so the
   single-grid projection keeps the last-placed Goodput — §3.7); a sample at `(opened_at, rate =
   0)` lands a mark at `(col 0, row 0)`; a sample at `opened_at + span_t/2` lands at `col (W-1)/2`.
   No index reaches `W` or `H`. (Projection unit test.)
9. **Fixed axes.** The projected X range is `[opened_at, effective_end]` and Y range is
   `[0, max_rate]` regardless of the cursor; moving the cursor changes which marks are revealed, not
   the axis ranges. (Projection unit test.)
10. **Reveal to `T`.** For samples at `t = {0, 10, 20}` and cursor `t = 10`, the projection emits
    the marks at `t = 0` and `t = 10` and omits `t = 20`; at `t = 20` all three appear. (Projection
    unit test.)
11. **Numeric-max column bucketing over revealed samples, per series.** Two revealed samples in the
    same column with `throughput` mapping to rows `r1 < r2` render a single `Throughput` mark at
    `r2`; a taller sample in that column with `t > T` does not raise the column. The `Goodput`
    series buckets independently. (Projection unit test.)
12. **Degenerate spans.** A focus connection with a single sample (`opened_at == effective_end`)
    projects to `(col 0, …)` with no divide-by-zero; a focus direction whose samples are all
    `throughput == goodput == 0` (`max_rate == 0`) projects to row 0. (Projection unit test.)
13. **Cursor column.** The projection marks the `T` column with the cursor glyph where no mark
    occupies that cell. (Projection unit test.)
14. **Narrow-terminal guard.** Projecting into a viewport below the minimum inner width/height
    yields `None`; `render` shows `widen terminal`. (Projection unit test + TestBackend test.)
15. **Total and goodput are distinct, aligned series (per sample, pre-bucketing).** A **single**
    revealed sample occupying its own column, with `goodput < throughput` (e.g. `throughput =
    max_rate`, `goodput = max_rate/2`), emits a `Throughput` mark and a `Goodput` mark at the
    **same column** but **different rows**, with distinct glyphs, the goodput below the total.
    (Projection unit test.)
15a. **Sub-Mbps axis label stays informative.** `fmt_rate` renders a `max_rate` of `800` bps as a
    non-zero `bps`-unit label (e.g. `800bps`), not `0.000Mbps`; `1_500_000` bps renders as
    `1.500Mbps`; `2_000_000_000` bps as `2.000Gbps`. (Formatter unit test.)
16. **`Tab` cycles the four views; `Enter`/`Esc` unchanged.** In navigation mode `Tab` advances
    `detail_view()` TimeSequence → InFlight → Rtt → Throughput → TimeSequence. `Enter` still opens
    and `Esc` still closes the pane; in filter mode `Tab` does not cycle the view. (App/keys unit
    tests.)
17. **Detail view follows selection.** With the throughput view open, `move_down` changes `focus()`
    to the newly selected connection's id and throughput series. (App unit test.)
18. **Render — closed is byte-identical to M5/M6/M7/M8.** With the detail closed, `render` into a
    `TestBackend` produces the same buffer as before (existing render assertions still pass;
    `Timeline::new` preserved). (TestBackend test.)
19. **Render — Throughput view open shows the graph.** With the detail open, the view switched to
    Throughput, over a connection with throughput data (`goodput < throughput` so the goodput mark
    plots away from the total), `render` shows the `DETAIL <origin> → <responder>` title, the
    throughput legend (naming the total and goodput glyphs), an axis time label, and a bits/sec unit
    label. For the plotted-glyph evidence: the goodput glyph `#` already appears **once** in the
    legend (`# goodput`), so the test requires **at least two** `#` — the extra one is a plotted
    goodput mark. The `.` total glyph is **not** usable as evidence (it also appears in the time
    labels `0.000s` and in `fmt_rate` output), so the assertion must key on `#`. (TestBackend test.)
20. **CLI wiring.** `build_replay_app(<fixture>, cfg with collect_throughput_timeline)` returns an
    `App` whose focus connection's `throughput_series` is non-empty. For the committed
    `metrics_basic.pcap` (single connection, index 0) the focus direction is O2R (SYN + 100 B data
    ≫ the 1-byte SYN-ACK), and O2R has a throughput sample at `t = 2 ms` with `throughput_bps ==
    800` and `goodput_bps == 800` (the 100 B O2R data, not a retransmit) — verified from the M3
    oracle. The test pins connection 0 so it cannot silently pass on the wrong flow.
    `replay_engine_config` turns the flag on; the ceiling path still returns `SampleCeiling`. (Bin
    integration test driving the seam.)

## 6. Failure modes handled

- **No connection selected / none active at `T`** → `Enter` inert; an open detail shows the shared
  empty-state message. (§3.2, §3.5.)
- **Focus direction has no throughput samples ≤ `T`** (never sent data, or all samples after `T`) →
  empty plot area with axes + cursor column, not an error. (§3.4.)
- **Single-sample / zero-width span / all-zero rate** → plots into column 0 / the bottom row with no
  divide-by-zero. (§3.4, criterion 12.)
- **Throughput attributed to the sender** → `ThroughputSample.dir` is the sending flow, so the
  focus-direction filter selects the correct samples. (§3.6, criterion 1.)
- **Retransmitted bytes** → counted in `throughput_bps`, excluded from `goodput_bps`; the gap is
  the retransmit rate. (§2, criterion 3.)
- **Direction that only ACKs (no payload)** → no throughput sample for that direction. (§2,
  criterion 7.)
- **Very high rate / large window** → the window sum and the `·8·1e9` scaling are computed in `u128`
  then narrowed (saturating), so no overflow. (§2, mirroring M3's throughput.)
- **Dense capture (many samples per column)** → numeric-max bucketing per series bounds render to
  `O(cells)`. (§3.4, criterion 11.)
- **Terminal too narrow/short** → explicit "widen terminal" message, master pane unaffected. (§3.3,
  criterion 14.)
- **Sample-ceiling overflow** (state + seq + in-flight + RTT + throughput series) → existing
  fail-fast `SampleCeiling`. (§3.1, criterion 5.)
- **Non-monotonic capture time** → each connection's `ThroughputSample` series is stable-sorted by
  `t` at `Timeline` construction (as for the other four series), so reveal/bucketing see a
  `t`-ordered slice.

## 7. Testing

- **Engine unit tests** (criteria 1–7a) from hand-built segment vectors through `Tracker` with
  `collect_throughput_timeline = true`, asserting the emitted `ThroughputSample.dir`/
  `throughput_bps`/`goodput_bps` (including the goodput-excludes-retransmit split and the
  sending-flow attribution), the `Timeline` accessor, the ceiling, the default-off flag, the
  no-sample-for-ACK-only-direction gate, and the both-directions decay sampling (a sender sample at
  a reverse-ACK time whose rate has decayed). Assert the sample values, not how the window is
  computed (reuse the M3 derivation).
- **Projection unit tests** (criteria 8–15a) from hand-built `Vec<ThroughputSample>` and explicit
  viewport sizes, asserting axis ranges and specific `(col, row, glyph, series)` marks — placement,
  fixed axes, reveal-to-`T`, numeric-max bucketing, degenerate spans, cursor column, narrow-terminal
  `None`, the total/goodput alignment, and the `fmt_rate` unit adaptation. No terminal needed.
- **App / keys unit tests** (criteria 16–17) for the four-way `Tab` cycle (nav vs filter),
  `Enter`/`Esc` unchanged, and detail-follows-selection, from hand-built timelines.
- **`TestBackend` render tests** (criteria 18–19): closed reproduces the prior master; Throughput
  open shows the title, throughput legend, an axis label, a bits/sec unit, and a plotted glyph.
- **Bin integration test** (criterion 20): `build_replay_app` over the `metrics_basic` fixture
  yields a non-empty focus `throughput_series` with the oracle-derived `800` bps total/goodput at
  `t = 2 ms`, and the ceiling seam still fails fast.
- Test behavior, not implementation: assert the emitted samples, the projected marks, and the
  rendered buffer — not how the goodput sum or the bucketing is computed.
