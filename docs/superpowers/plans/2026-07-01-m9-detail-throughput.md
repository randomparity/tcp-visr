# Plan: M9 — Detail: Throughput/goodput

**Milestone:** M9 · **Issue:** #12 · **Spec:** [m9-detail-throughput.md](../specs/m9-detail-throughput.md) · **ADR:** [ADR-0014](../../adr/0014-detail-throughput-goodput.md)

One PR on `feat/detail-throughput-12`. TDD throughout: write the failing test first, confirm it
fails for the expected reason, write the minimal implementation, re-run the focused test + relevant
guardrails, refactor only while green. Tasks are ordered by the dependency flow (core → engine →
timeline → tracker → CLI → TUI projection → app → render → bin), so each task compiles on the
previous. Implement in this session (tasks are tightly coupled through shared types).

## Guardrail commands (run before every commit)

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace
cargo test -p tcpvisr-ingest --features live   # libpcap parity (unaffected, but part of CI gate)
cargo deny check
```

Focused runs while iterating:
```bash
cargo test -p tcpvisr-engine metrics::           # engine goodput derivation
cargo test -p tcpvisr-engine tracker::throughput # tracker collector
cargo test -p tcpvisr-engine timeline::          # Timeline carriage
cargo test -p tcpvisr-tui throughput::           # projection
cargo test -p tcpvisr-tui render::               # TestBackend
cargo test -p tcp-visr --test '*'                # bin + integration (oracle must stay green)
```

**The M3 oracle (`crates/tcp-visr/tests/oracle/*.json`) must stay byte-identical.** If any oracle
test fails, the goodput refactor changed `throughput_bps` — that is a bug in the refactor, not the
golden. Roll back the window-sum change until `throughput_bps` is byte-identical.

---

## Task 1 — Goodput derivation in the pure engine (`metrics.rs`)

**Fits:** ADR-0014 §2; spec §4 (Engine), criteria 2, 3. The foundational derivation everything else
consumes. No new public type yet — this task only teaches `MetricState` to compute goodput and
expose the pair, keeping `MetricSample.throughput_bps` byte-identical.

**Files:** `crates/tcpvisr-engine/src/metrics.rs`.

**Steps:**
1. Change the per-direction throughput window `DirState.tput` from `VecDeque<(Nanos, u32)>` to
   `VecDeque<(Nanos, u32, bool)>` — the third field is the segment's `retransmit` classification.
2. `MetricState::observe`: `retransmit` is already computed in Step 2 before Step 5. Thread it into
   the throughput call so the pushed window entry carries it.
3. Split the throughput logic:
   - A mutating window step (push the `(ts, len, retransmit)` entry when `payload_len > 0`; update
     `tput_max_ts`; evict entries with `ts + window <= max_ts`) — the existing eviction, unchanged.
   - A **pure read** `throughput_at(&self, dir: Direction, t: Nanos, cfg: &EngineConfig) ->
     Option<(u64, u64)>`: `None` if that direction's `tput_max_ts` is `None` (never sent data);
     else `Some((throughput_bps, goodput_bps))` where `throughput_bps` sums `len` over entries in
     `(t − window, t]` (membership `ts + window > t && ts <= t`) and `goodput_bps` sums `len` over
     the subset whose `retransmit` flag is `false`. Both scale as `bytes · 8 · 1e9 / window` in
     `u128`, narrowed with the existing saturating `u64::try_from`. `window == 0` → `Some((0, 0))`.
   - `observe` fills `MetricSample.throughput_bps` from `throughput_at(dir, seg.ts, cfg)` mapping
     `None` → `0` (byte-identical to the old empty-window `0`).
4. Keep `throughput_at` `pub(crate)` (consumed by `tracker.rs`, like `in_flight`).

**Tests (write first):**
- `throughput_bps` unchanged: an O2R 100 B send with the 1 s window → the sample's
  `throughput_bps == 800` (criterion 2). Add/keep a test asserting the existing
  `throughput_sums_window_bytes_and_excludes_older` behavior is preserved.
- Goodput split (criterion 3): O2R 100 B new at `t=0`, O2R 100 B retransmit at `t=4ms` (gap ≥
  reorder_window) → `throughput_at(O2R, 4ms)` returns `Some((1600, 800))`. A loss-free repeat →
  `throughput_bps == goodput_bps`.
- `throughput_at` returns `None` for a direction that never sent data (e.g. R2O pure ACK only).

**Acceptance:** `throughput_bps` byte-identical (the whole `metrics.rs` test module + the bin oracle
test pass unchanged); `throughput_at` returns the `(total, good)` pair with goodput excluding
retransmit; `None` for a data-less direction. `cargo test -p tcpvisr-engine metrics::` green;
`cargo test -p tcp-visr --test metrics` (oracle) green.

**Rollback:** revert `metrics.rs`; no other file depends on `throughput_at` yet.

---

## Task 2 — `ThroughputSample` type, `Timeline` carriage, config flag

**Fits:** ADR-0014 §1; spec §4 (Engine), criteria 4, 6. Adds the sample type, threads it through the
`ConnSeries` tuple and `Timeline`, and adds the orthogonal collect flag.

**Files:** `crates/tcpvisr-engine/src/timeline.rs`, `config.rs`, `lib.rs`.

**Steps:**
1. `timeline.rs`: add `pub struct ThroughputSample { pub t: Nanos, pub dir: SampleDir, pub
   throughput_bps: u64, pub goodput_bps: u64 }` (derive `Debug, Clone, Copy, PartialEq, Eq`), with
   a doc comment referencing design §6/§10.M9/ADR-0014 §1.
2. Extend `ConnSeries` to the 6-tuple `(Connection, Vec<StateSample>, Vec<SeqSample>,
   Vec<InFlightSample>, Vec<RttSample>, Vec<ThroughputSample>)`. `Entry` gains `throughput:
   Vec<ThroughputSample>`. `with_seq` destructures the 6th element, stable-sorts it by `t`, stores
   it. `new` delegates with a trailing `Vec::new()`.
3. Add `pub fn throughput_series(&self, id: ConnId) -> &[ThroughputSample]` (empty slice for
   unknown id), mirroring `rtt_series`.
4. Update the two in-file `with_seq` test call sites (`with_seq_carries_rtt_...`,
   `with_seq_sorts_and_exposes_...`, `with_seq_carries_inflight_...`) with a trailing `Vec::new()`
   (or the throughput vec for the new test).
5. `config.rs`: add `collect_throughput_timeline: bool` (default `false`); update the
   `struct_excessive_bools` `reason` string to say *five* orthogonal gates
   (state/seq/inflight/rtt/throughput) and cite ADR-0014; add a `throughput_timeline_defaults_off`
   unit test mirroring `rtt_timeline_defaults_off`.
6. `lib.rs`: re-export `ThroughputSample` in the `timeline::{…}` list.

**Tests (write first):**
- `with_seq_carries_throughput_sorted_and_exposes_series`: supply throughput samples out of `t`
  order → `throughput_series(id)` is `t`-sorted (criterion 4).
- `throughput_series_empty_for_unknown_id` (criterion 4).
- `throughput_timeline_defaults_off` (criterion 6).

**Acceptance:** engine compiles; `Timeline::new` unchanged in behavior (empty throughput series);
`throughput_series` sorted + empty-for-unknown; flag defaults off. `cargo test -p tcpvisr-engine
timeline:: config::` green. (The workspace won't fully build until Task 4 updates `tracker.rs`'s
`with_seq` call and the TUI's `with_seq` test sites — do Tasks 2–4 as one green step if needed, or
temporarily stub the tracker call.)

**Rollback:** revert the three files; `ThroughputSample` is unreferenced elsewhere until Task 3.

---

## Task 3 — Tracker collector (`tracker.rs`)

**Fits:** ADR-0014 §1; spec §4 (Engine tracker), criteria 1, 5, 7, 7a. Emits `ThroughputSample`s for
both directions per segment (gated on having sent data), counting against the ceiling.

**Files:** `crates/tcpvisr-engine/src/tracker.rs`.

**Steps:**
1. `ConnTrack` gains `throughput: Vec<ThroughputSample>` (init `Vec::new()` in `create_instance`).
2. Add `record_throughput(&mut self, idx, sample: ThroughputSample)` mirroring `record_inflight`
   (bail on `overflowed`; trip `overflowed` at the ceiling; else push + increment
   `collected_samples`).
3. Add `collect_throughput_points(&mut self, idx, seg: &Segment)`: bail if `overflowed ||
   !collect_throughput_timeline`; for each `Direction` call `self.conns[idx].metrics.throughput_at(dir,
   seg.ts, &self.config)` and, on `Some((tp, gp))`, `record_throughput(idx, ThroughputSample { t:
   seg.ts, dir: dir_sample(dir), throughput_bps: tp, goodput_bps: gp })`.
4. Call `collect_throughput_points(idx, seg)` in **both** `observe_segment` (right after
   `collect_rtt_points`) **and** `create_instance` (right after its `collect_rtt_points`) — the
   first data segment of an instance is handled in `create_instance` (mirror M8's `collect_rtt_points`
   call sites exactly).
5. Extend the two `want to derive` guards (in `observe_segment` and `create_instance`) to include
   `|| self.config.collect_throughput_timeline`.
6. `into_timeline`: add `c.throughput.clone()` as the 6th tuple element passed to `with_seq`.
7. Import `ThroughputSample` from `crate::timeline`.

**Tests (write first) — new `mod throughput_tests` mirroring `mod rtt_tests`:**
- Attribution (criterion 1): O2R data → the O2R sample carries `dir == OriginToResponder`.
- Goodput split reaches the timeline (criterion 3, end-to-end): the retransmit fixture yields a
  sample with `throughput_bps == 2 * goodput_bps`.
- ACK-only direction (criterion 7): R2O sends only pure ACKs → no R2O `ThroughputSample`.
- **Both-directions decay (criterion 7a):** O2R 100 B at `t=0`; R2O pure ACK at `t=500ms` (in
  window) and R2O pure ACK at `t=1_500ms` (past the 1 s window). The O2R throughput series has a
  sample at both `t=500ms` (`throughput_bps == 800`) and `t=1_500ms` (`throughput_bps == 0`,
  decayed). Assert the `t=1_500ms` sample exists and is `0` — a sparse impl produces neither.
- Ceiling (criterion 5): `max_samples` small enough that the fixture overflows → `into_timeline`
  returns `SampleCeiling`.
- Off by default (criterion 6): only `collect_state_timeline` set → empty throughput series.

**Acceptance:** all `throughput_tests` pass; `into_timeline` carries the series; ceiling fails fast.
`cargo test -p tcpvisr-engine` green (Task 2 + 3 together make the engine build). Update the TUI's
`with_seq` test call sites (Task 5 pre-req) if the workspace build is checked here.

**Rollback:** revert `tracker.rs`; Task 2's `Timeline` still compiles (empty throughput vecs).

---

## Task 4 — CLI wiring (`main.rs`)

**Fits:** spec §4 (CLI), criterion 20. Turns the flag on for the replay path.

**Files:** `crates/tcp-visr/src/main.rs`.

**Steps:**
1. `replay_engine_config`: add `collect_throughput_timeline: true`; update the doc comment to "all
   five replay timelines on (state, seq, in-flight, rtt, throughput)".
2. Add `run_replay_config_enables_throughput_collection` mirroring the RTT one.
3. Add `build_replay_app_collects_throughput_series_for_the_focus_connection` (criterion 20): over
   `metrics_basic.pcap`, the focus connection's `throughput_series` is non-empty, and contains an
   O2R sample at `t = 2_000_000` ns with `throughput_bps == 800` and `goodput_bps == 800` (from the
   oracle). Pin connection 0 / focus dir O2R so it cannot pass on the wrong flow.

**Acceptance:** `cargo test -p tcp-visr` green (including the oracle tests, still byte-identical).
The focus throughput series is non-empty with the oracle-derived 800/800 sample.

**Rollback:** revert the `main.rs` changes; replay still works without the throughput series.

---

## Task 5 — Pure throughput projection (`throughput.rs`)

**Fits:** ADR-0014 §3; spec §4 (TUI), criteria 8–15a. A new pure module mirroring `rtt.rs` **minus
the overlay** and with two wire series.

**Files:** new `crates/tcpvisr-tui/src/throughput.rs`; `crates/tcpvisr-tui/src/lib.rs`. Also fix the
`with_seq` test call sites in `app.rs`/`render.rs` (add the trailing throughput `Vec::new()`).

**Steps:**
1. `throughput.rs`: `pub const MIN_W: u16 = 8; MIN_H: u16 = 3;`, `THROUGHPUT_GLYPH: char = '.'`,
   `GOODPUT_GLYPH: char = '#'`, `CURSOR_GLYPH = '\u{250a}'`. `pub enum Series { Throughput, Goodput
   }`. `pub struct Mark { col, row, glyph, series }`, `pub struct ThroughputPlot { width, height,
   max_rate, x_span, cursor_col, marks }`. A private `Geom` with `col`/`row`/`idx`/`place` copied
   from `rtt.rs` (integer-only, clamped, `place` keeps the numeric-max revealed mark per column).
2. `pub fn project(wire: &[ThroughputSample], focus: SampleDir, x_span: (Nanos, Nanos), cursor:
   Nanos, width: u16, height: u16) -> Option<ThroughputPlot>`: `None` below `MIN_W`/`MIN_H`;
   `max_rate` = max over focus samples' `throughput_bps` and `goodput_bps`; place `Throughput` (glyph
   `.`, value `throughput_bps`) **then** `Goodput` (glyph `#`, value `goodput_bps`) into one grid
   (goodput wins a coincident cell); draw the cursor column where empty; flatten to `marks`.
   **No overlay parameter** (ADR-0014 §4).
3. `lib.rs`: `pub mod throughput;` and `pub use throughput::{ThroughputPlot, Series as
   ThroughputSeries};`.

**Tests (write first) — `mod tests` mirroring `rtt.rs`:**
- `too_small_viewport_yields_none` (criterion 14).
- `corners_place_at_exact_indices` (criterion 8): sample at `(end, tp=gp=max)` → mark at `(W-1,
  H-1)`; sample at `(start, 0)` → `(0, 0)`.
- `total_and_goodput_align_in_column` (criterion 15): one sample `tp=max, gp=max/2` in its own
  column → a `Throughput` mark and a `Goodput` mark, same col, goodput row < throughput row,
  distinct glyphs.
- `reveal_hides_marks_after_cursor` (criterion 10).
- `axes_fixed_regardless_of_cursor` (criterion 9).
- `numeric_max_bucketing_over_revealed_only` (criterion 11).
- `degenerate_spans_do_not_divide_by_zero` (criterion 12): zero-width span → col 0; all-zero rate →
  `max_rate == 0`, row 0.
- `cursor_column_drawn_where_empty` (criterion 13).
- `only_focus_direction_is_plotted`.

**Acceptance:** projection is pure/integer-only; all marks land at the asserted indices; goodput ≤
throughput row per column; `None` below minimum. `cargo test -p tcpvisr-tui throughput::` green.

**Rollback:** delete `throughput.rs`, revert the `lib.rs` re-export.

---

## Task 6 — App view-switcher + `FocusConn` (`app.rs`)

**Fits:** ADR-0014 §5; spec §4 (TUI app), criteria 16, 17.

**Files:** `crates/tcpvisr-tui/src/app.rs`.

**Steps:**
1. `DetailView` gains `Throughput`. `cycle_detail_view`: `TimeSequence → InFlight → Rtt → Throughput
   → TimeSequence`.
2. `FocusConn` gains `throughput: &'a [ThroughputSample]`; `focus()` populates it from
   `self.timeline.throughput_series(id)`. Import `ThroughputSample`.
3. Update any `FocusConn` construction in existing tests.

**Tests (write first):**
- Extend `tab_cycles_detail_view` to assert the four-way cycle ending back at `TimeSequence`
  (criterion 16).
- `focus_exposes_throughput_series` mirroring `focus_exposes_rtt_series` (criterion 17, and
  detail-follows-selection is already covered by `detail_follows_selection` once the field exists).

**Acceptance:** `cargo test -p tcpvisr-tui app::` green; the four-way cycle and the exposed series
verified.

**Rollback:** revert `app.rs`.

---

## Task 7 — Render body + `fmt_rate` (`render.rs`) and keys check

**Fits:** ADR-0014 §3; spec §3.3, §3.7, §4 (TUI render), criteria 18, 19.

**Files:** `crates/tcpvisr-tui/src/render.rs`. (`keys.rs` needs no change — `Tab` already maps to
`cycle_detail_view`; add a keys test only if one is missing for the four-way cycle in filter mode.)

**Steps:**
1. Add the `DetailView::Throughput` arm in `render_detail`'s match, calling `render_throughput_body`.
2. `render_throughput_body` mirrors `render_rtt_body`: carve gutter + legend + time-label rows, call
   `throughput::project(focus.throughput, focus.focus_dir, focus.x_span, app.cursor(), plot_w,
   plot_h)`, then `draw_throughput_legend` / `draw_throughput_plot` / `draw_throughput_axes`.
3. Legend: `format!("Throughput  {} total  {} goodput", throughput::THROUGHPUT_GLYPH,
   throughput::GOODPUT_GLYPH)`.
4. Plot colours: `Series::Goodput => Color::Green`, `Series::Throughput => Color::Reset`.
5. Y axis: top label `fmt_rate(plot.max_rate)`, bottom `fmt_rate(0)` (→ `0bps`); X axis start/end
   seconds — copy `draw_rtt_axes`.
6. Add `fn fmt_rate(bps: u64) -> String` mirroring `fmt_rtt`: units `[(1e9,"Gbps"),(1e6,"Mbps"),
   (1e3,"kbps")]`, else `"{bps}bps"`; `<whole>.<3-frac><unit>`, integer-only.
7. Fix the `with_seq` 5-tuple test call sites in `render.rs` tests (add trailing throughput vec).

**Tests (write first):**
- `fmt_rate_adapts_units`: `800 → "800bps"`, `1_500_000 → "1.500Mbps"`, `2_000_000_000 →
  "2.000Gbps"` (criterion 15a).
- `throughput_view_open_shows_graph` (criterion 19): a connection with one sample `throughput=
  3_000_000, goodput=1_500_000` (so goodput plots below total); open + `Tab` ×3 to Throughput;
  assert the buffer contains `DETAIL`, `Throughput`, `0.000s`, a bits/sec unit (`Mbps`), and **≥ 2**
  `#` (legend `# goodput` + ≥ 1 plotted goodput mark). Do **not** assert `.` (it appears in labels).
- `throughput_view_too_narrow_shows_widen_message` mirroring the inflight one (criterion 14 render).
- Confirm existing `detail_closed_still_renders_full_master` and the other render tests still pass
  unchanged (criterion 18).

**Acceptance:** `cargo test -p tcpvisr-tui render::` green; the throughput view renders title,
legend, ms/bits axis, and a plotted goodput glyph; closed render byte-identical.

**Rollback:** revert `render.rs`.

---

## Task 8 — Full guardrails, docs, and PR

**Fits:** spec §5 all criteria; workflow steps 5–7.

**Steps:**
1. Run the full guardrail suite (all five commands above). Zero warnings.
2. Update `CLAUDE.md`'s "Current state" paragraph: mark M9 implemented, note the throughput/goodput
   view completes the four-view `Tab` switcher, and update "remaining detail view" language (M9 is
   done; the next unbuilt work is M10 name resolution / `live`).
3. Update design §10 roadmap only if it carries per-milestone "implemented" markers (it does not —
   leave the table; the CLAUDE.md current-state is the status of record, matching how M8 was
   marked).
4. Adversarial-review the branch diff (`/challenge --base main`), address findings, run
   `security-review` if required.
5. Open the PR against `main` with `Closes #12`.

**Acceptance:** full CI-equivalent green locally; PR green and mergeable.

---

## Cross-cutting notes

- **Purity preserved:** no I/O, no clock in the engine (ADR-0002). `throughput_at` is a pure read.
- **Oracle frozen:** `MetricSample.throughput_bps` unchanged; the oracle goldens are the tripwire.
- **`ConnSeries` stays a tuple** (now 6-wide) to match M6/M7/M8; the struct conversion is a noted
  future cleanup, not this PR.
- **No new dependencies, no new CLI flags, no `deny.toml` change.**
- **`cargo test -p tcpvisr-ingest --features live`** is part of the gate but untouched by M9 (no
  ingest/decode change); run it once before pushing to satisfy the CI gate.
