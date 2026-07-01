# Spec: M8 — Detail: RTT

**Milestone:** M8 (design §6, §10.M8) · **Issue:** #11 · **Release:** v0.1.0 (Replay MVP)
**ADR:** [ADR-0013 — Detail RTT: per-ack RTT series, engine-smoothed SRTT, and the kernel-srtt overlay seam](../../adr/0013-detail-rtt.md)
(builds on [ADR-0004](../../adr/0004-seekable-timeseries-timeline.md),
[ADR-0007](../../adr/0007-metric-derivation-model.md),
[ADR-0009](../../adr/0009-tui-master-list-architecture.md),
[ADR-0010](../../adr/0010-timeline-transport-as-of-t.md),
[ADR-0011](../../adr/0011-detail-seq-timeline-and-rendering.md),
[ADR-0012](../../adr/0012-detail-inflight-and-view-switcher.md))

## 1. Goal

Add the third **detail view**: a per-connection **RTT** graph plotting the **per-ack round-trip
time** as raw points **and** an engine-smoothed RTT line over time, driven by the transport
cursor `T` (reveal to `T`, cursor column, fixed axes). Reach it with the existing **`Tab`**
view-switcher, now cycling Time/Sequence → In-flight → RTT → Time/Sequence. Provide a typed,
tested **overlay hook** so a future kernel-srtt series (M12, live-only) can be drawn as a distinct
overlay; on replay the overlay is empty.

## 2. Scope

### In scope

- A pure **`RttSample`** series in `tcpvisr-engine`, carrying `{ t, dir, rtt, srtt }` where:
  - `t` is the ACK's timestamp,
  - `dir` is the **measured data-flow direction** — the sender being acked, i.e. `opposite` of
    the ACK segment's own direction (so the focus-direction plot selects the right flow;
    ADR-0013 §1),
  - `rtt` is the engine's Karn-paired `MetricSample.rtt` (the collector reads it, does not
    re-derive),
  - `srtt` is the **smoothed RTT** over `dir`'s samples so far: RFC 6298 EWMA with `α = 1/8`,
    `srtt = rtt` for the first sample and `srtt = (7*srtt + rtt) / 8` thereafter, integer
    nanoseconds computed in `u128` then narrowed (deterministic, no float; ADR-0013 §2).
  - Only ACKs that yield an RTT (`MetricSample.rtt.is_some()`) produce an `RttSample`. Collected
    under a new `EngineConfig.collect_rtt_timeline` flag, counting against `max_samples`.
- `Timeline` retains each connection's `RttSample` series and exposes
  **`rtt_series(id) -> &[RttSample]`**; `with_seq` carries it through.
- A pure **RTT projection** in `tcpvisr-tui` (`rtt.rs`, sibling of `detail.rs`/`inflight.rs`):
  `(wire series, overlay series, focus direction, x_span, cursor T, viewport cells)` → resolved
  axis ranges + a grid of marks `{ col, row, glyph, series }` with `Series { Raw, Smoothed,
  Kernel }`. Each revealed (`t ≤ T`) focus-direction wire sample emits a `Raw` mark at
  `row(rtt)` and a `Smoothed` mark at `row(srtt)`; each overlay sample emits a `Kernel` mark at
  `row(srtt)`. Y = `[0, max_rtt]` over the focus direction's **wire (`rtt` and `srtt`) ∪ overlay
  (`srtt`)** samples (so a diverging kernel overlay is not clamped); X = `x_span`; **numeric-max**
  column bucketing per series applied only to revealed samples; a vertical cursor column at `T`;
  `None` below the minimum viewport (ADR-0013 §3, §4).
- **`App`** gains a `DetailView::Rtt` variant; `cycle_detail_view` extends to the three-way cycle;
  `FocusConn` gains `rtt: &'a [RttSample]` (borrowed from the `Timeline`). `Tab` cycles the view.
  `Enter`/`Esc` keep their open/close meaning.
- **Layout / render**: closed → M5/M6/M7 full-width master (unchanged). Open → master left /
  detail right split; the pane renders the view named by `detail_view()`. The RTT view titles the
  focus connection (shared `DETAIL <origin> → <responder>` block), draws the raw points and the
  smoothed line with a Y axis in milliseconds and an X axis in seconds, and a one-line legend
  naming the raw and smoothed glyphs. Footer is unchanged (`⇥ view` already advertised in M7).
- **CLI wiring**: `replay_engine_config` sets `collect_rtt_timeline = true` alongside the existing
  three flags. No new CLI flags. The `SampleCeiling` path is unchanged.

### Out of scope (deferred, do not build)

- **Throughput/goodput detail view** and the fourth `Tab` variant — M9. M9 "finalizes" the
  switcher/layout (design §10.M9).
- **The kernel srtt data source.** Kernel srtt is live-only enrichment (`sock_diag`), M12. M8
  ships the typed, tested overlay *seam* only; on replay the overlay series is empty.
- **Connected-polyline rendering** of the smoothed line — M8 draws top-edge marks (ADR-0013 §3);
  a connected line is later polish behind the same projection seam.
- **Per-window axis auto-scale / zoom / pan** — M8 uses fixed full-extent axes (as M6/M7).
- **Reverse-direction RTT overlay** — M8 plots the one higher-byte focus direction (as M6/M7).
- **Changes to the RTT derivation itself** (Karn pairing, `MetricSample.rtt`) — frozen from M3;
  M8 retains and smooths, it does not re-derive.
- **Live timeline** (M11), **names/attribution** (M10/M12).

## 3. User-facing behavior

### 3.1 Entry point

`tcp-visr replay <file>` is unchanged (invocation, non-TTY guard, `--max-samples`). The only
change: the tracker now also collects the RTT timeline, so the built `Timeline` can answer
`rtt_series(id)`. A `max_samples` overflow still exits non-zero with the actionable
`SampleCeiling` message.

### 3.2 Switching views

- With the detail pane **open**, in navigation mode, **`Tab`** cycles the detail view:
  Time/Sequence → In-flight → RTT → Time/Sequence. The chosen view persists while scrubbing and
  while the selection moves.
- `Tab` when the pane is **closed** is a no-op on layout (it may still update the remembered
  view; nothing is drawn until `Enter` opens the pane).
- In **filter-input** mode `Tab` is inert for view-switching. All other M6/M7 modality is
  unchanged: `Enter` opens (nav) / confirms filter, `Esc` closes (nav) / clears filter.
- All navigation and transport keys keep working while the pane is open, for any view.

### 3.3 Layout (RTT view open)

```
┌ tcp-visr — capture.pcap  (47 connections, skipped 0) ─────[ ▶ 2.0x  t=12.480s / 38.200s ]┐
│ PEER                SERVICE  STATE        ↑BYTES  ↓BYTES │ DETAIL 10.0.0.5:51324 → 140..:443│
│▸140.82.121.3:443    https    ESTABLISHED    1234   34000 │ RTT   . raw  # smoothed          │
│ 10.0.0.9:22         ssh      ESTABLISHED~    840    2100  │ ms                             ╷ │
│ …                                                        │  45.0 ┤   . #  .# .              │
│                                                          │       ┤ .## .# #                 │
│                                                          │   0.0 ┼───────────────────────────│
│                                                          │       0.000s              38.200s  │
├──────────────────────────────────────────────────────────┴──────────────────────────────────┤
│ space play/pause  ←→ seek  +/- speed  ,/. step  ⏎ open  esc close  ⇥ view  / filter  s sort  q quit │
└───────────────────────────────────────────────────────────────────────────────────────────────┘
```

- The screen splits into a **left** master pane (renders exactly as M5/M6/M7) and a **right**
  detail pane, a bordered block titled `DETAIL <origin> → <responder>` (shared with M6/M7).
- The RTT body shows a **Y axis labeled in milliseconds** (0 at the bottom, `max_rtt` at the
  top), an X axis labeled with the time range in seconds, the plotted raw points, the smoothed
  line marks, and a one-line **legend** naming the raw glyph and the smoothed glyph.
- **Narrow terminals.** The projection is handed the inner plot rectangle after `render.rs`
  carves the gutter, X-label row, and legend row; below the minimum plot rectangle the pane
  shows `widen terminal to view graph` (shared with M6/M7). The master pane keeps rendering.
- **Footer** is the M7 footer (already advertises `⇥ view`).

### 3.4 The RTT graph (semantics)

For the focus connection (§3.5) and its focus direction (§3.6):

- **X axis (time)** spans `[opened_at, effective_end]` from `Timeline::x_span(id)` (shared with
  M6/M7); fixed, does not rescale with the cursor.
- **Y axis (nanoseconds, labeled in ms)** spans `[0, max_rtt]` where `max_rtt = max` over the
  focus direction's wire `rtt`, wire `srtt`, and overlay `srtt` samples (plain `u64` max).
  Including `srtt` and the overlay keeps the smoothed line and a diverging kernel overlay from
  clamping; on replay the overlay is empty. Fixed; does not rescale.
- **Plot rectangle.** `W` columns × `H` rows of cells, excluding border, Y-label gutter, X-label
  row, and legend row (`render.rs` carves them). Row 0 is the **bottom** (0 ns), row `H-1` the
  top.
- **Coordinate mapping (exact, clamped),** identical to M7 with `span_t = effective_end.0 -
  opened_at.0`:
  - `col(t) = if span_t == 0 { 0 } else { ((t - t0) * (W-1)) / span_t }`, clamped to `0..=W-1`.
  - `row(v) = if max_rtt == 0 { 0 } else { (v * (H-1)) / max_rtt }`, clamped to `0..=H-1`.
    `v == max_rtt` maps to row `H-1`; the degenerate `span_t == 0` / `max_rtt == 0` cases collapse
    to column 0 / bottom row via guards, no division.
- **Marks.** Each focus-direction wire `RttSample` with `t ≤ T` emits a `Raw` mark at `(col(t),
  row(rtt))` with the raw glyph and a `Smoothed` mark at `(col(t), row(srtt))` with the smoothed
  glyph. Each overlay sample with `t ≤ T` emits a `Kernel` mark at `(col(t), row(srtt))` with the
  kernel glyph (empty on replay).
- **Column bucketing (downsampling).** Applied **only to revealed samples** (`t ≤ T`). Per
  (column, series) the tallest mark (max `row`) is kept — the peak for that time bucket. This
  bounds render to `O(cells)` regardless of sample count. Marks of different series in the same
  cell keep their distinct glyphs (see §3.7 for render precedence).
- **Cursor column.** A vertical cursor glyph is drawn in the column corresponding to `T`, over
  cells no mark occupies (shared with M6/M7).
- **Reveal.** Marks with `t > T` are not drawn.
- **No data yet.** A focus direction with no RTT samples at or before `T` shows an empty plot area
  with axes and the cursor column, not an error. (RTT samples are sparse — a connection that
  never had an advancing, non-retransmitted ACK has none at all; this is honest, not a failure.)

### 3.5 Focus connection

Identical to M6/M7: the detail follows `App::selected()` (the M5 `ConnId`-tracked selection).
Opening does not change the selection; moving the selection while open moves the detail. The
focus connection's `origin`/`responder` come from the `Connection`, its `[opened_at,
effective_end]` X span from `Timeline::x_span(id)`, its RTT series from `Timeline::rtt_series(id)`,
all keyed by `ConnId`.

### 3.6 Focus direction

The plotted direction is the connection's **higher-byte** direction (`bytes_o2r ≥ bytes_r2o` →
origin→responder, else responder→origin), the same deterministic rule M6/M7 use, exposed as
`FocusConn::focus_dir`. Because `RttSample.dir` is the measured data-flow direction (ADR-0013 §1),
the focus direction's RTTs are exactly the samples tagged with that direction — the higher-byte
sender's round-trips, measured on the opposite direction's ACKs. `max_rtt` is computed over that
direction's samples only.

### 3.7 Rendering determinism

The projection is pure and integer-only (time→column, nanos→row are integer proportions, no
float; the EWMA is integer), so `TestBackend` snapshots and the projection's unit tests are
deterministic. Axis time labels reuse M6/M7's fixed-3-decimal-seconds formatter; the Y axis labels
RTT in milliseconds via an integer formatter (nanoseconds → `<ms>.<3-frac>` with no float). When
marks of different series compete for a cell in the final glyph buffer, the renderer draws **raw
first, then smoothed, then kernel** so the smoothed line and the diagnostic kernel overlay stay
visible; each keeps its own colour.

## 4. Architecture

### Engine (`tcpvisr-engine`)

- `timeline.rs`: add `RttSample { t, dir, rtt, srtt }`; the `Timeline` `Entry` gains `rtt:
  Vec<RttSample>`; add `rtt_series(&self, id) -> &[RttSample]` (empty slice for an
  unknown/uncollected id). `x_span` is reused unchanged. `ConnSeries` becomes a 5-tuple
  `(Connection, Vec<StateSample>, Vec<SeqSample>, Vec<InFlightSample>, Vec<RttSample>)`;
  `with_seq` stable-sorts the RTT series by `t` like the others; `new` delegates with an empty RTT
  vector. **This changes the `with_seq` signature**, so existing `with_seq` call sites (the
  `tracker.rs` `into_timeline`, and M6/M7 tests in `timeline.rs`/`app.rs`/`render.rs`) get an
  added empty RTT vector — a mechanical edit confined to those files, not the M5 `new` fixtures.
- `config.rs`: add `collect_rtt_timeline: bool` (default `false`); add a `defaults_off` unit test
  mirroring `inflight_timeline_defaults_off`.
- `tracker.rs`: `ConnTrack` gains `rtt: Vec<RttSample>` and `srtt: [Option<Nanos>; 2]` (the
  per-measured-direction EWMA accumulator). Add `record_rtt` (mirrors `record_inflight`: counts
  against `max_samples`, sets `overflowed`) and `collect_rtt_points(idx, sample: &MetricSample)`:
  when `collect_rtt_timeline` is set and not overflowed and `sample.rtt` is `Some(rtt)`, compute
  the measured direction `m = opposite(sample.dir)`, update `srtt[m]` by the EWMA, and record
  `RttSample { t: sample.t, dir: m, rtt, srtt }`. Call it from `observe_segment` right after
  `collect_inflight_points`, using the **same** `MetricState::observe` output (no second
  derivation). Extend the `want to derive` guard to include `collect_rtt_timeline`.
  `into_timeline` passes the RTT series through `with_seq`.
- `lib.rs`: re-export `RttSample`.

### TUI (`tcpvisr-tui`)

- `rtt.rs` (new, pure): `RttPlot`, its `Mark { col, row, glyph, series }` and `Series { Raw,
  Smoothed, Kernel }` types, glyph constants (`RAW_GLYPH`, `SMOOTHED_GLYPH`, `KERNEL_GLYPH`,
  `CURSOR_GLYPH`; `MIN_W`/`MIN_H`), and `project(wire, overlay, focus_dir, x_span, cursor, width,
  height) -> Option<RttPlot>`. Mirrors `inflight.rs`: `MIN_W`/`MIN_H` guard → `None`; `max_rtt`
  from the focus wire `rtt`/`srtt` and overlay `srtt`; numeric-max bucketing per series; cursor
  column; reveal-to-`T`. Unit-tested from hand-built `Vec<RttSample>`.
- `app.rs`: `DetailView` gains `Rtt`; `cycle_detail_view` becomes the three-way cycle; `FocusConn`
  gains `rtt: &'a [RttSample]` populated from `timeline.rtt_series(id)`. `open_detail` /
  `close_detail` / selection unchanged.
- `render.rs`: when open, the `detail_view()` match gains a `Rtt` arm calling a new
  `render_rtt_body` that carves the same gutter/label/legend rows, calls `rtt::project`, and draws
  raw then smoothed then kernel marks (distinct colours) plus the ms/time axes and legend. Add an
  integer `fmt_rtt(Nanos) -> String` (ns → `<ms>.<3-frac>`). The footer is unchanged.
- `lib.rs`: re-export `rtt::{RttPlot, Series as RttSeries}` as the bin/tests need (naming to avoid
  clashing with `inflight::Series`).
- `keys.rs`: unchanged — `Tab` already maps to `cycle_detail_view`; the three-way cycle is in
  `app.rs`.

### CLI (`tcp-visr`)

- `replay_engine_config`: set `collect_rtt_timeline = true` alongside the existing three flags. No
  new flags. The `SampleCeiling` path is unchanged.

Dependency direction is unchanged (TUI → engine → core).

## 5. Success criteria (falsifiable)

1. **RTT is attributed to the measured flow, not the ACK's direction.** A tracker with
   `collect_rtt_timeline = true` fed O2R data (seq 100 len 10, t=1000) then an R2O ACK=110
   (t=1500) produces exactly one `RttSample` with `dir == OriginToResponder` (the sender that was
   acked), `t == 1500`, `rtt == 500`. No R2O-tagged sample is produced. (Engine unit test.)
2. **Smoothed RTT is the RFC 6298 EWMA (α = 1/8).** A direction with raw RTTs `[800, 800, 400]`
   (three Karn-paired ACKs) yields `srtt` values `[800, 800, 750]` (`(7*800+400)/8 = 750`). The
   first sample's `srtt == rtt`. (Engine unit test.)
3. **Retransmit-blocked / non-advancing ACKs contribute no RTT sample.** A retransmitted range
   (Karn) and a duplicate ACK that does not advance the frontier both leave the RTT series without
   a sample for that segment (the series length equals the count of advancing, non-retransmitted
   ACKs). (Engine unit test.)
4. **Series carried through the timeline.** `into_timeline` yields a `Timeline` whose
   `rtt_series(id)` returns the connection's `RttSample`s sorted by `t`; `rtt_series(unknown_id)`
   is empty. (Engine unit test.)
5. **RTT collection counts against the ceiling.** With `collect_rtt_timeline = true` and a
   `max_samples` smaller than the samples a fixture produces, `into_timeline` returns
   `SampleCeiling`. (Engine unit test.)
6. **Flag is orthogonal / off by default.** `collect_rtt_timeline` defaults to `false`; a tracker
   with only `collect_state_timeline` set produces an empty RTT series. (Engine/config unit
   tests.)
7. **Point placement (exact indices).** In a plot rectangle `W×H`: a wire sample at
   `(effective_end, rtt = srtt = max_rtt)` lands its `Raw` and `Smoothed` marks at `(col W-1, row
   H-1)`; a sample at `(opened_at, rtt = 0)` lands its `Raw` mark at `(col 0, row 0)`; a sample at
   `opened_at + span_t/2` lands at `col (W-1)/2`. No index reaches `W` or `H`. (Projection unit
   test.)
8. **Fixed axes.** The projected X range is `[opened_at, effective_end]` and Y range is
   `[0, max_rtt]` regardless of the cursor; moving the cursor changes which marks are revealed,
   not the axis ranges. (Projection unit test.)
9. **Reveal to `T`.** For wire samples at `t = {0, 10, 20}` and cursor `t = 10`, the projection
   emits the marks at `t = 0` and `t = 10` and omits `t = 20`; at `t = 20` all three appear.
   (Projection unit test.)
10. **Numeric-max column bucketing over revealed samples, per series.** Two revealed wire samples
    in the same column with `rtt` mapping to rows `r1 < r2` render a single `Raw` mark at `r2`; a
    taller sample in that column with `t > T` does not raise the column. The `Smoothed` series
    buckets independently. (Projection unit test.)
11. **Degenerate spans.** A focus connection with a single sample (`opened_at == effective_end`)
    projects to `(col 0, …)` with no divide-by-zero; a focus direction whose samples are all
    `rtt == srtt == 0` (`max_rtt == 0`) projects to row 0. (Projection unit test.)
12. **Cursor column.** The projection marks the `T` column with the cursor glyph where no mark
    occupies that cell. (Projection unit test.)
13. **Narrow-terminal guard.** Projecting into a viewport below the minimum inner width/height
    yields `None`; `render` shows `widen terminal`. (Projection unit test + TestBackend test.)
14. **Raw and smoothed are distinct, aligned series.** A wire sample with `rtt != srtt` (e.g.
    `rtt = max_rtt`, `srtt = max_rtt/2`) emits a `Raw` mark and a `Smoothed` mark at the **same
    column** but **different rows**, with distinct glyphs. (Projection unit test.)
15. **Kernel overlay hook draws a distinct, unclamped series.** The projection given a non-empty
    overlay series (synthetic kernel srtt) emits marks tagged `Series::Kernel` with the kernel
    glyph, distinct from wire marks. An overlay `srtt` **above** the wire maximum expands
    `max_rtt` so that overlay mark is not clamped to row `H-1`. With an **empty** overlay (the
    replay case) no `Kernel` marks are emitted and `max_rtt` equals the wire maximum. (Projection
    unit test.)
16. **`Tab` cycles the three views; `Enter`/`Esc` unchanged.** In navigation mode `Tab` advances
    `detail_view()` TimeSequence → InFlight → Rtt → TimeSequence. `Enter` still opens and `Esc`
    still closes the pane; in filter mode `Tab` does not cycle the view. (App/keys unit tests.)
17. **Detail view follows selection.** With the RTT view open, `move_down` changes `focus()` to
    the newly selected connection's id and RTT series. (App unit test.)
18. **Render — closed is byte-identical to M5/M6/M7.** With the detail closed, `render` into a
    `TestBackend` produces the same buffer as before (existing render assertions still pass;
    `Timeline::new` preserved). (TestBackend test.)
19. **Render — RTT view open shows the graph.** With the detail open, the view switched to RTT,
    over a connection with RTT data, `render` shows the `DETAIL <origin> → <responder>` title, the
    RTT legend (naming the raw and smoothed glyphs), an axis time label, and at least one plotted
    glyph. (TestBackend test.)
20. **CLI wiring.** `build_replay_app(<fixture>, cfg with collect_rtt_timeline)` returns an `App`
    whose focus connection's `rtt_series` is non-empty for a fixture with acked data on the focus
    direction; `replay_engine_config` turns the flag on; the ceiling path still returns
    `SampleCeiling`. (Bin integration test driving the seam.)

## 6. Failure modes handled

- **No connection selected / none active at `T`** → `Enter` inert; an open detail shows the
  shared empty-state message. (§3.2, §3.5.)
- **Focus direction has no RTT samples ≤ `T`** (sparse RTT, or a direction never acked) → empty
  plot area with axes + cursor column, not an error. (§3.4.)
- **Single-sample / zero-width span / all-zero rtt** → plots into column 0 / the bottom row with
  no divide-by-zero. (§3.4, criterion 11.)
- **RTT measured on the opposite direction's ACK** → `RttSample.dir` is the acked sender's flow,
  so the focus-direction filter selects the correct samples. (§3.6, criterion 1.)
- **Retransmit (Karn) / duplicate ACK** → no RTT sample, exactly as M3's `MetricSample.rtt`
  decides; the series is sparse, not padded. (§2, criterion 3.)
- **Very large RTT** → the EWMA is computed in `u128` then narrowed, so `7*srtt + rtt` cannot
  overflow. (§2.)
- **Dense capture (many RTT samples per column)** → numeric-max bucketing per series bounds render
  to `O(cells)`. (§3.4, criterion 10.)
- **Terminal too narrow/short** → explicit "widen terminal" message, master pane unaffected.
  (§3.3, criterion 13.)
- **Sample-ceiling overflow** (state + seq + in-flight + RTT series) → existing fail-fast
  `SampleCeiling`. (§3.1, criterion 5.)
- **Non-monotonic capture time** → each connection's `RttSample` series is stable-sorted by `t` at
  `Timeline` construction (as for the other three series), so reveal/bucketing see a `t`-ordered
  slice.
- **Kernel srtt overlay absent on replay** → the overlay series is empty, so no `Kernel` marks are
  drawn; the seam is exercised only by a synthetic-overlay unit test until M12. (§3.4, criterion
  15.)

## 7. Testing

- **Engine unit tests** (criteria 1–6) from hand-built segment vectors through `Tracker` with
  `collect_rtt_timeline = true`, asserting the emitted `RttSample.dir`/`rtt`/`srtt` (including the
  EWMA sequence and the measured-flow attribution), the `Timeline` accessor, the ceiling, and the
  default-off flag. Assert the sample values, not how `MetricSample.rtt` is computed (reuse the M3
  derivation).
- **Projection unit tests** (criteria 7–15) from hand-built `Vec<RttSample>` and explicit viewport
  sizes, asserting axis ranges and specific `(col, row, glyph, series)` marks — placement, fixed
  axes, reveal-to-`T`, numeric-max bucketing, degenerate spans, cursor column, narrow-terminal
  `None`, the raw/smoothed alignment, and the overlay series. No terminal needed.
- **App / keys unit tests** (criteria 16–17) for the three-way `Tab` cycle (nav vs filter),
  `Enter`/`Esc` unchanged, and detail-follows-selection, from hand-built timelines.
- **`TestBackend` render tests** (criteria 18–19): closed reproduces the prior master; RTT open
  shows the title, RTT legend, an axis label, and a plotted glyph.
- **Bin integration test** (criterion 20): `build_replay_app` over the `metrics_basic` fixture
  yields a non-empty focus `rtt_series` (choosing a connection/direction with acked data), and the
  ceiling seam still fails fast.
- Test behavior, not implementation: assert the emitted samples, the projected marks, and the
  rendered buffer — not how the EWMA or the bucketing is computed.
