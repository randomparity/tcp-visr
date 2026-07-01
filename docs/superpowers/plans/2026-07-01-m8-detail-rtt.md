# M8 Detail: RTT Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a per-connection RTT detail view — raw per-ack RTT points plus an engine-smoothed SRTT line — reachable via the `Tab` view-switcher, on the replay path only.

**Architecture:** Mirror the settled M6/M7 detail-view shape: a dedicated per-connection `RttSample` series derived on the replay path and stored in the `Timeline` (leaving the frozen M3 `MetricSample`/oracle untouched), plus a pure projection in `tcpvisr-tui` that maps `(series, x_span, focus dir, cursor, cell rect)` to `Mark`s, dispatched from the existing `DetailView` switcher. RTT is already Karn-paired by the engine (`MetricSample.rtt`); M8 retains it, attributes it to the measured (acked-sender) flow, and adds an integer RFC 6298 EWMA smoothed line.

**Tech Stack:** Rust 1.88.0, workspace crates `tcpvisr-core` / `tcpvisr-engine` / `tcpvisr-tui` / `tcp-visr` (bin), `ratatui` for the TUI.

**Spec:** `docs/superpowers/specs/m8-detail-rtt.md` · **ADR:** `docs/adr/0013-detail-rtt.md`

## Global Constraints

- Toolchain pinned to Rust 1.88.0; no new dependencies (pure Rust only).
- Zero-warnings baseline. Guardrails before every commit: `cargo fmt --all --check`, `cargo clippy --all-targets --all-features -- -D warnings`, plus the focused tests for the crate touched. Full gate before first push: add `cargo test --workspace` and `cargo deny check`.
- Clippy denies `unwrap_used`/`panic`/`print_stdout` in non-test code; test modules relax via file-level `#![allow(...)]` (item-level `#[allow]` is denied by `allow_attributes`).
- The engine stays pure: no I/O, no clock reads (ADR-0002). `MetricSample` and the `metrics` JSON/oracle are frozen — do not touch them.
- Absolute imports only; ≤100 lines/function; conventional-commit subjects ≤72 chars ending with the trailer `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- Never `--squash`-merge; one logical change per commit.

## File Structure

- `crates/tcpvisr-engine/src/config.rs` — add `collect_rtt_timeline` flag (+ default-off test).
- `crates/tcpvisr-engine/src/timeline.rs` — add `RttSample`; extend `ConnSeries` to a 5-tuple; `Entry.rtt`; `with_seq` sorts it; `rtt_series(id)` accessor.
- `crates/tcpvisr-engine/src/tracker.rs` — `ConnTrack.rtt` + `srtt: [Option<Nanos>; 2]`; `record_rtt`; `collect_rtt_points`; wire into `observe_segment` and `into_timeline`.
- `crates/tcpvisr-engine/src/lib.rs` — re-export `RttSample`.
- `crates/tcpvisr-tui/src/rtt.rs` — new pure projection (`RttPlot`, `Mark`, `Series`, `project`).
- `crates/tcpvisr-tui/src/app.rs` — `DetailView::Rtt`; three-way `cycle_detail_view`; `FocusConn.rtt`.
- `crates/tcpvisr-tui/src/render.rs` — `Rtt` arm + `render_rtt_body` + `fmt_rtt`.
- `crates/tcpvisr-tui/src/lib.rs` — re-export `rtt::{RttPlot, Series as RttSeries}`.
- `crates/tcp-visr/src/main.rs` — set `collect_rtt_timeline = true` in `replay_engine_config`; integration test.

---

### Task 1: Engine config flag `collect_rtt_timeline`

**Files:**
- Modify: `crates/tcpvisr-engine/src/config.rs`

**Interfaces:**
- Produces: `EngineConfig.collect_rtt_timeline: bool` (default `false`).

- [ ] **Step 1: Write the failing test** — add to the `tests` module in `config.rs`:

```rust
#[test]
fn rtt_timeline_defaults_off() {
    let c = EngineConfig::default();
    assert!(!c.collect_rtt_timeline);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p tcpvisr-engine config::tests::rtt_timeline_defaults_off`
Expected: FAIL (no field `collect_rtt_timeline`).

- [ ] **Step 3: Add the field and default.** In the `EngineConfig` struct, after `collect_inflight_timeline`:

```rust
    /// Whether the tracker records a per-segment `RttSample` timeline (M8 detail).
    pub collect_rtt_timeline: bool,
```

In `Default::default()`, after `collect_inflight_timeline: false,`:

```rust
            collect_rtt_timeline: false,
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p tcpvisr-engine config::`
Expected: PASS.

- [ ] **Step 5: Guardrails + commit**

```bash
cargo fmt --all --check && cargo clippy -p tcpvisr-engine --all-targets --all-features -- -D warnings
git add crates/tcpvisr-engine/src/config.rs
git commit -m "feat(engine): add collect_rtt_timeline config flag"
```

---

### Task 2: `RttSample` type + `Timeline` carries the RTT series

**Files:**
- Modify: `crates/tcpvisr-engine/src/timeline.rs`
- Modify (mechanical, add empty RTT vec to `with_seq` call sites): `crates/tcpvisr-engine/src/tracker.rs`, `crates/tcpvisr-tui/src/app.rs`, `crates/tcpvisr-tui/src/render.rs`

**Interfaces:**
- Produces: `pub struct RttSample { pub t: Nanos, pub dir: SampleDir, pub rtt: Nanos, pub srtt: Nanos }`; `Timeline::rtt_series(id: ConnId) -> &[RttSample]`; `ConnSeries = (Connection, Vec<StateSample>, Vec<SeqSample>, Vec<InFlightSample>, Vec<RttSample>)`.

- [ ] **Step 1: Write the failing test** — add to the `tests` module in `timeline.rs` (a `ratt` helper + carry/sort/unknown tests):

```rust
fn ratt(t: u64, rtt: u64, srtt: u64) -> RttSample {
    RttSample {
        t: Nanos(t),
        dir: SampleDir::OriginToResponder,
        rtt: Nanos(rtt),
        srtt: Nanos(srtt),
    }
}

#[test]
fn with_seq_carries_rtt_sorted_and_exposes_series() {
    let c = conn(0, 100, 300, ConnState::Established);
    let id = c.id;
    let tl = Timeline::with_seq(vec![(
        c,
        vec![ss(100, ConnState::Established, 0, 0)],
        vec![sq(100, 0, 10)],
        vec![iff(100, 10)],
        vec![ratt(300, 5, 5), ratt(100, 9, 9)], // supplied out of t-order
    )]);
    let series = tl.rtt_series(id);
    assert_eq!(series.len(), 2);
    assert_eq!(series[0].t, Nanos(100), "sorted by t at construction");
    assert_eq!(series[1].t, Nanos(300));
    assert_eq!(series[0].rtt, Nanos(9));
}

#[test]
fn rtt_series_empty_for_unknown_id() {
    let c = conn(0, 0, 10, ConnState::Established);
    let other = ConnId {
        pair: EndpointPair::new(ep(9, 1), ep(9, 2)),
        instance: 7,
    };
    let tl = Timeline::new(vec![(c, vec![ss(0, ConnState::Established, 0, 0)])]);
    assert!(tl.rtt_series(other).is_empty());
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p tcpvisr-engine timeline::`
Expected: FAIL (no `RttSample`, no `rtt_series`, `with_seq` arity).

- [ ] **Step 3: Implement.** In `timeline.rs`:

Add after the `InFlightSample` struct:

```rust
/// One point on a connection's RTT graph (design §6, §10.M8, ADR-0013 §1). `dir` is the measured
/// data-flow direction (the sender being acked, i.e. opposite the ACK's own direction). `rtt` is
/// the Karn-paired per-ack sample (the engine's `MetricSample.rtt`); `srtt` is the smoothed RTT
/// (RFC 6298 EWMA, α = 1/8) over `dir`'s samples so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RttSample {
    pub t: Nanos,
    pub dir: SampleDir,
    pub rtt: Nanos,
    pub srtt: Nanos,
}
```

Extend `ConnSeries`:

```rust
pub type ConnSeries = (
    Connection,
    Vec<StateSample>,
    Vec<SeqSample>,
    Vec<InFlightSample>,
    Vec<RttSample>,
);
```

Add `rtt: Vec<RttSample>` to `Entry` (after `inflight`).

In `Timeline::new`, change the map closure to append a 5th empty vec:

```rust
                .map(|(c, s)| (c, s, Vec::new(), Vec::new(), Vec::new()))
```

In `with_seq`, change the destructuring loop and add the sort + Entry field:

```rust
        for (conn, mut samples, mut seq, mut inflight, mut rtt) in conns {
            samples.sort_by_key(|s| s.t);
            seq.sort_by_key(|s| s.t);
            inflight.sort_by_key(|s| s.t);
            rtt.sort_by_key(|s| s.t);
```

...and add `rtt,` to the `Entry { ... }` construction. Update the `.map(|(c, _, _, _)| ...)`
tuples in `with_seq` (the `end` and `start` computations) to 5-element patterns
(`|(c, _, _, _, _)|` and `|(_, s, _, _, _)|`).

Add the accessor after `inflight_series`:

```rust
    /// The focus connection's `RttSample` series (`t`-sorted), or an empty slice if `id` is
    /// unknown or its series was not collected.
    #[must_use]
    pub fn rtt_series(&self, id: ConnId) -> &[RttSample] {
        match self.entries.iter().find(|e| e.conn.id == id) {
            Some(e) => &e.rtt,
            None => &[],
        }
    }
```

- [ ] **Step 4: Fix the existing `with_seq` call sites (mechanical — add a 5th empty vec).**
  - `timeline.rs` tests `with_seq_sorts_and_exposes_series_and_x_span` and `with_seq_carries_inflight_sorted_and_exposes_series`: add `Vec::new(),` as the 5th tuple element.
  - `tracker.rs` `into_timeline`: change the `.map(|c| (c.view(), c.states.clone(), c.seq.clone(), c.inflight.clone()))` to append `c.rtt.clone()` (the `rtt` field lands in Task 3; for now, if Task 3 is not yet done, use `Vec::new()` — but tasks run in order, so add `c.rtt.clone()` only after Task 3 adds the field. To keep this task compiling standalone, append `Vec::new()` here and change to `c.rtt.clone()` in Task 3).
  - `app.rs` test `focus_exposes_inflight_series`: the `Timeline::with_seq(vec![(c2, …, vec![], inflight)])` gets a trailing `vec![]`.
  - `render.rs` tests `detail_open_shows_title_legend_and_a_mark`, `inflight_view_open_shows_graph`, `inflight_view_too_narrow_shows_widen_message`, `detail_pane_too_narrow_shows_widen_message`, `large_seq_axis_label_is_abbreviated_not_raw`: each `with_seq(vec![(…, Vec::new())])` / `(…, inflight)` gets a trailing `Vec::new()`.

- [ ] **Step 5: Run to verify pass**

Run: `cargo test -p tcpvisr-engine timeline:: && cargo build -p tcpvisr-tui --tests`
Expected: PASS / builds.

- [ ] **Step 6: Guardrails + commit**

```bash
cargo fmt --all --check && cargo clippy --all-targets --all-features -- -D warnings
git add -A
git commit -m "feat(engine): add RttSample and carry it through the Timeline"
```

---

### Task 3: Tracker collects the RTT series (attribution + integer EWMA)

**Files:**
- Modify: `crates/tcpvisr-engine/src/tracker.rs`
- Modify: `crates/tcpvisr-engine/src/lib.rs`

**Interfaces:**
- Consumes: `RttSample`, `EngineConfig.collect_rtt_timeline`, `MetricSample { t, dir, rtt: Option<Nanos>, .. }`.
- Produces: RTT series on each `ConnTrack`, flowing through `into_timeline` into `Timeline::rtt_series`.

- [ ] **Step 1: Write the failing tests** — add an `rtt_tests` module at the bottom of
  `tracker.rs`, reusing the in-file `test_support::{ep, seg}` builder (`seg(src, dst, flags, seq,
  ack, len, ts)`) exactly as `inflight_tests` does. Complete code (covers criteria 1–6):

```rust
#[cfg(test)]
mod rtt_tests {
    use super::test_support::{ep, seg};
    use super::*;
    use tcpvisr_core::{SampleDir, TcpFlags};

    fn rtt_cfg() -> EngineConfig {
        EngineConfig {
            collect_rtt_timeline: true,
            ..EngineConfig::default()
        }
    }

    fn only_id(tl: &crate::timeline::Timeline) -> ConnId {
        tl.connections().next().expect("one connection").id
    }

    /// (t, rtt, srtt) triples for the O2R-measured RTT samples, t-ordered by the Timeline.
    fn o2r_rtt(tl: &crate::timeline::Timeline) -> Vec<(u64, u64, u64)> {
        tl.rtt_series(only_id(tl))
            .iter()
            .filter(|s| s.dir == SampleDir::OriginToResponder)
            .map(|s| (s.t.0, s.rtt.0, s.srtt.0))
            .collect()
    }

    // Criterion 1: the RTT of O2R data is measured on the R2O ACK, so the sample is tagged O2R
    // (the acked sender), not R2O (the ACK's own direction).
    #[test]
    fn rtt_attributed_to_measured_flow_not_ack_direction() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let mut t = Tracker::new(rtt_cfg());
        t.observe(&seg(c, s, TcpFlags::ACK, 100, 1, 10, 1_000)); // O2R data seq100 len10
        t.observe(&seg(s, c, TcpFlags::ACK, 1, 110, 0, 1_500)); // R2O pure ACK=110
        let tl = t.into_timeline().expect("timeline");
        let all = tl.rtt_series(only_id(&tl));
        assert_eq!(all.len(), 1, "exactly one RTT sample");
        assert_eq!(all[0].dir, SampleDir::OriginToResponder, "measured flow is O2R");
        assert_eq!((all[0].t.0, all[0].rtt.0), (1_500, 500));
    }

    // Criterion 2: srtt is the RFC 6298 EWMA (α=1/8): 800, (7*800+800)/8=800, (7*800+400)/8=750.
    #[test]
    fn srtt_is_rfc6298_ewma() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let mut t = Tracker::new(rtt_cfg());
        t.observe(&seg(c, s, TcpFlags::ACK, 100, 1, 10, 0)); // O2R -> pending(110,0)
        t.observe(&seg(s, c, TcpFlags::ACK, 1, 110, 0, 800)); // R2O ACK110 -> rtt800
        t.observe(&seg(c, s, TcpFlags::ACK, 110, 1, 10, 1_000)); // O2R -> pending(120,1000)
        t.observe(&seg(s, c, TcpFlags::ACK, 1, 120, 0, 1_800)); // R2O ACK120 -> rtt800
        t.observe(&seg(c, s, TcpFlags::ACK, 120, 1, 10, 2_000)); // O2R -> pending(130,2000)
        t.observe(&seg(s, c, TcpFlags::ACK, 1, 130, 0, 2_400)); // R2O ACK130 -> rtt400
        let tl = t.into_timeline().expect("timeline");
        assert_eq!(
            o2r_rtt(&tl),
            vec![(800, 800, 800), (1_800, 800, 800), (2_400, 400, 750)]
        );
    }

    // Criterion 3a: a duplicate ACK that does not advance the frontier yields no RTT sample.
    #[test]
    fn duplicate_ack_produces_no_rtt_sample() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let mut t = Tracker::new(rtt_cfg());
        t.observe(&seg(c, s, TcpFlags::ACK, 100, 1, 10, 0)); // O2R
        t.observe(&seg(s, c, TcpFlags::ACK, 1, 110, 0, 500)); // R2O ACK110 -> rtt500
        t.observe(&seg(s, c, TcpFlags::ACK, 1, 110, 0, 900)); // R2O dup ACK110 -> no advance
        let tl = t.into_timeline().expect("timeline");
        assert_eq!(o2r_rtt(&tl), vec![(500, 500, 500)], "only the advancing ACK yields RTT");
    }

    // Criterion 3b: a retransmitted range clears the pending queue (Karn), so the later ACK finds
    // nothing to pair and yields no RTT sample.
    #[test]
    fn karn_retransmit_produces_no_rtt_sample() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let mut t = Tracker::new(rtt_cfg());
        t.observe(&seg(c, s, TcpFlags::ACK, 100, 1, 10, 0)); // O2R -> pending(110,0)
        t.observe(&seg(c, s, TcpFlags::ACK, 100, 1, 10, 10_000_000)); // O2R retransmit (gap > 3ms)
        t.observe(&seg(s, c, TcpFlags::ACK, 1, 110, 0, 11_000_000)); // R2O ACK110 -> pending empty
        let tl = t.into_timeline().expect("timeline");
        assert!(tl.rtt_series(only_id(&tl)).is_empty(), "Karn cleared the pending send");
    }

    // Criterion 6: off by default.
    #[test]
    fn rtt_off_by_default_is_empty() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let mut t = Tracker::new(EngineConfig {
            collect_state_timeline: true,
            ..EngineConfig::default()
        });
        t.observe(&seg(c, s, TcpFlags::ACK, 100, 1, 10, 0));
        t.observe(&seg(s, c, TcpFlags::ACK, 1, 110, 0, 500));
        let tl = t.into_timeline().expect("timeline");
        assert!(tl.rtt_series(only_id(&tl)).is_empty());
    }

    // Criterion 5: two RTT samples with max_samples=1 -> SampleCeiling.
    #[test]
    fn rtt_collection_counts_against_ceiling() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let mut cfg = rtt_cfg();
        cfg.max_samples = 1;
        let mut t = Tracker::new(cfg);
        t.observe(&seg(c, s, TcpFlags::ACK, 100, 1, 10, 0));
        t.observe(&seg(s, c, TcpFlags::ACK, 1, 110, 0, 500)); // rtt #1
        t.observe(&seg(c, s, TcpFlags::ACK, 110, 1, 10, 1_000));
        t.observe(&seg(s, c, TcpFlags::ACK, 1, 120, 0, 1_500)); // rtt #2 -> ceiling
        assert!(matches!(
            t.into_timeline().expect_err("ceiling"),
            MetricError::SampleCeiling { .. }
        ));
    }
}
```

(Criterion 4's t-ordering is asserted by `srtt_is_rfc6298_ewma`'s ascending-t vector; the
unknown-id empty case is Task 2's `rtt_series_empty_for_unknown_id`.)

Fill each test body with concrete `Segment` vectors following the `inflight_tests` pattern in this
file. For criterion 2 build three separate Karn-paired ACK exchanges whose measured RTTs are
exactly 800, 800, 400 ns for the same direction.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p tcpvisr-engine tracker::rtt_tests`
Expected: FAIL (no `rtt` field / `collect_rtt_points`).

- [ ] **Step 3: Implement.** In `tracker.rs`:

Add fields to `ConnTrack` (after `inflight`):

```rust
    rtt: Vec<RttSample>,
    srtt: [Option<Nanos>; 2],
```

Initialize them where `ConnTrack` is constructed (`inflight: Vec::new()` gains
`rtt: Vec::new(), srtt: [None, None],`).

Add helpers near the other `dir` helpers (there is an existing `dir_sample(Direction) -> SampleDir`;
add the SampleDir flip + index):

```rust
fn opposite_sdir(d: SampleDir) -> SampleDir {
    match d {
        SampleDir::OriginToResponder => SampleDir::ResponderToOrigin,
        SampleDir::ResponderToOrigin => SampleDir::OriginToResponder,
    }
}

fn sdir_idx(d: SampleDir) -> usize {
    match d {
        SampleDir::OriginToResponder => 0,
        SampleDir::ResponderToOrigin => 1,
    }
}
```

Add `record_rtt` mirroring `record_inflight`:

```rust
    /// Stores one `RttSample` on the instance at `idx`, enforcing `max_samples`.
    fn record_rtt(&mut self, idx: usize, sample: RttSample) {
        if self.overflowed {
            return;
        }
        if self.collected_samples >= self.config.max_samples {
            self.overflowed = true;
            return;
        }
        self.collected_samples += 1;
        self.conns[idx].rtt.push(sample);
    }
```

Add `collect_rtt_points`:

```rust
    /// Records the per-ack RTT + smoothed SRTT when RTT collection is on and this segment yielded
    /// an RTT. The RTT measures the *opposite* (acked-sender) flow, so the sample is tagged with
    /// `opposite(sample.dir)` (ADR-0013 §1); `srtt` is the RFC 6298 EWMA (α = 1/8) over that
    /// direction, computed in `u128` to avoid overflow (ADR-0013 §2).
    fn collect_rtt_points(&mut self, idx: usize, sample: &MetricSample) {
        if self.overflowed || !self.config.collect_rtt_timeline {
            return;
        }
        let Some(rtt) = sample.rtt else {
            return;
        };
        let m = opposite_sdir(sample.dir);
        let mi = sdir_idx(m);
        let srtt = match self.conns[idx].srtt[mi] {
            None => rtt,
            Some(prev) => {
                let v = (7u128 * u128::from(prev.0) + u128::from(rtt.0)) / 8;
                Nanos(u64::try_from(v).unwrap_or(u64::MAX))
            }
        };
        self.conns[idx].srtt[mi] = Some(srtt);
        self.record_rtt(
            idx,
            RttSample {
                t: sample.t,
                dir: m,
                rtt,
                srtt,
            },
        );
    }
```

In `observe_segment`, extend the derive guard and add the call after `collect_inflight_points`:

```rust
                if !self.overflowed
                    && (want_metric
                        || self.config.collect_seq_timeline
                        || self.config.collect_inflight_timeline
                        || self.config.collect_rtt_timeline)
                {
                    let sample = self.conns[idx].metrics.observe(seg, dir, &self.config);
                    if want_metric {
                        self.record_sample(idx, sample);
                    }
                    self.collect_seq_points(idx, seg, dir, &sample);
                    self.collect_inflight_points(idx, seg);
                    self.collect_rtt_points(idx, &sample);
                }
```

In `into_timeline`, add `c.rtt.clone()` as the 5th tuple element (replacing the temporary
`Vec::new()` from Task 2 Step 4).

In `lib.rs`, add `RttSample` to the `timeline` re-export:

```rust
pub use timeline::{AsOf, ConnSeries, InFlightSample, RttSample, SeqKind, SeqSample, StateSample, Timeline};
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p tcpvisr-engine`
Expected: PASS (all engine tests incl. `rtt_tests`).

- [ ] **Step 5: Guardrails + commit**

```bash
cargo fmt --all --check && cargo clippy -p tcpvisr-engine --all-targets --all-features -- -D warnings
git add crates/tcpvisr-engine/src/tracker.rs crates/tcpvisr-engine/src/lib.rs
git commit -m "feat(engine): collect the RTT timeline with smoothed SRTT"
```

---

### Task 4: Pure RTT projection (`tcpvisr-tui/src/rtt.rs`)

**Files:**
- Create: `crates/tcpvisr-tui/src/rtt.rs`
- Modify: `crates/tcpvisr-tui/src/lib.rs` (declare `pub mod rtt;` + re-export)

**Interfaces:**
- Consumes: `tcpvisr_engine::RttSample`, `tcpvisr_core::{Nanos, SampleDir}`.
- Produces: `RttPlot { width, height, max_rtt, x_span, cursor_col, marks }`; `Mark { col, row, glyph, series }`; `Series { Raw, Smoothed, Kernel }`; consts `RAW_GLYPH`/`SMOOTHED_GLYPH`/`KERNEL_GLYPH`/`CURSOR_GLYPH`/`MIN_W`/`MIN_H`; `project(wire, overlay, focus, x_span, cursor, width, height) -> Option<RttPlot>`.

- [ ] **Step 1: Write `rtt.rs` with a failing test module.** Model the module on `inflight.rs`
  (read it first). The projection differs in three ways: (a) two glyphs from the *wire* series
  (`Raw` at `row(rtt)`, `Smoothed` at `row(srtt)`); (b) `Series { Raw, Smoothed, Kernel }`;
  (c) `max_rtt` maximizes over wire `rtt`, wire `srtt`, and overlay `srtt`. Bucketing is per
  series (numeric-max), reveal-to-`T`, cursor column — all identical to `inflight.rs`.
  Cover criteria 7–15 as tests in the module. The tests below are the two worked templates — the
  novel criterion-14 alignment test in full, and one placement test showing the helper shape.
  The remaining tests (reveal-to-`T`, fixed axes, per-series numeric-max bucketing, degenerate
  spans, cursor column, focus-only, overlay distinct+unclamped) are mechanical translations of
  `inflight.rs`'s identically-named tests — copy each, swap `wire(t, bytes)` for `rtt(t, rtt,
  srtt)`, `bytes`→`rtt`/`srtt`, `WIRE_GLYPH`→`RAW_GLYPH`/`SMOOTHED_GLYPH`, `Series::Cwnd`→
  `Series::Kernel`, and `max_bytes`→`max_rtt`:

```rust
#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use tcpvisr_core::SampleDir;

    fn rtt(t: u64, rtt_ns: u64, srtt_ns: u64) -> RttSample {
        RttSample {
            t: Nanos(t),
            dir: SampleDir::OriginToResponder,
            rtt: Nanos(rtt_ns),
            srtt: Nanos(srtt_ns),
        }
    }

    fn marks_at(p: &RttPlot, col: u16, row: u16) -> Vec<Mark> {
        p.marks.iter().filter(|m| m.col == col && m.row == row).copied().collect()
    }

    // Criterion 7: corners — a sample at (end, max) lands a mark at top-right (col W-1, row H-1);
    // a sample at (start, 0) lands a mark at bottom-left. Both corners here have rtt == srtt, so
    // the two series coincide and the single-grid projection keeps the last-placed (Smoothed);
    // assert a mark of any series lands at each corner (the raw/smoothed distinctness is
    // criterion 14, where rtt != srtt). max_rtt = 40.
    #[test]
    fn corners_place_at_exact_indices() {
        let s = [rtt(0, 0, 0), rtt(100, 40, 40)];
        let p = project(&s, &[], SampleDir::OriginToResponder, (Nanos(0), Nanos(100)), Nanos(100), 10, 5).unwrap();
        assert_eq!(p.max_rtt, 40);
        assert!(!marks_at(&p, 0, 0).is_empty(), "a mark at bottom-left");
        assert!(!marks_at(&p, 9, 4).is_empty(), "a mark at top-right (col W-1, row H-1)");
    }

    // Criterion 14: a single sample in its own column with rtt != srtt emits a Raw mark and a
    // Smoothed mark in the same column at different rows, with distinct glyphs.
    #[test]
    fn raw_and_smoothed_align_in_column() {
        // rtt=40 (max) -> row H-1; srtt=20 -> half. Single sample -> its own column, bucketing no-op.
        let s = [rtt(100, 40, 20)];
        let p = project(&s, &[], SampleDir::OriginToResponder, (Nanos(0), Nanos(100)), Nanos(100), 10, 11).unwrap();
        let col = p.cursor_col; // t=100=end -> last column; the sample shares it
        let raw = p.marks.iter().find(|m| m.series == Series::Raw && m.glyph == RAW_GLYPH).unwrap();
        let smooth = p.marks.iter().find(|m| m.series == Series::Smoothed && m.glyph == SMOOTHED_GLYPH).unwrap();
        assert_eq!(raw.col, smooth.col, "same column");
        assert_ne!(raw.row, smooth.row, "different rows (rtt above srtt)");
        assert!(raw.row > smooth.row, "raw (40) plots above smoothed (20)");
        let _ = col;
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p tcpvisr-tui rtt::`
Expected: FAIL (module absent / `project` undefined).

- [ ] **Step 3: Implement `project`.** Structure (full code — mirror `inflight.rs`'s `Geom`,
  `col`/`row`/`idx`/`place`):

```rust
//! Pure RTT projection (ADR-0013 §3): maps a connection's `RttSample` wire series (raw per-ack
//! RTT + smoothed SRTT) plus an optional kernel-srtt overlay + cursor + plot-rectangle cells to a
//! grid of glyph marks. No terminal, no I/O, no float (the EWMA was done in the engine).

use tcpvisr_core::{Nanos, SampleDir};
use tcpvisr_engine::RttSample;

pub const MIN_W: u16 = 8;
pub const MIN_H: u16 = 3;

pub const RAW_GLYPH: char = '.';
pub const SMOOTHED_GLYPH: char = '#';
pub const KERNEL_GLYPH: char = '+';
pub const CURSOR_GLYPH: char = '\u{250a}';

/// Which series a mark belongs to: raw per-ack RTT, wire-smoothed SRTT, or the (M12) kernel srtt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Series {
    Raw,
    Smoothed,
    Kernel,
}

/// One plotted cell. `row` is bottom-origin (0 = 0 ns).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mark {
    pub col: u16,
    pub row: u16,
    pub glyph: char,
    pub series: Series,
}

/// A resolved RTT plot over a `width x height` cell rectangle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RttPlot {
    pub width: u16,
    pub height: u16,
    pub max_rtt: u64,
    pub x_span: (Nanos, Nanos),
    pub cursor_col: u16,
    pub marks: Vec<Mark>,
}

struct Geom {
    focus: SampleDir,
    t0: u64,
    span_t: u64,
    max_rtt: u64,
    width: u16,
    height: u16,
    cursor: Nanos,
}

impl Geom {
    fn col(&self, t: u64) -> u16 {
        if self.span_t == 0 {
            return 0;
        }
        let c = u128::from(t.saturating_sub(self.t0)) * u128::from(self.width - 1)
            / u128::from(self.span_t);
        u16::try_from(c).unwrap_or(self.width - 1).min(self.width - 1)
    }

    fn row(&self, v: u64) -> u16 {
        if self.max_rtt == 0 {
            return 0;
        }
        let r = u128::from(v) * u128::from(self.height - 1) / u128::from(self.max_rtt);
        u16::try_from(r).unwrap_or(self.height - 1).min(self.height - 1)
    }

    fn idx(&self, col: u16, row: u16) -> usize {
        usize::from(row) * usize::from(self.width) + usize::from(col)
    }

    /// Writes the tallest revealed (`t <= cursor`) mark per column for one series into `grid`,
    /// reading each sample's plotted value via `value` (rtt for Raw, srtt for Smoothed/Kernel).
    fn place(
        &self,
        samples: &[RttSample],
        series: Series,
        glyph: char,
        value: impl Fn(&RttSample) -> u64,
        grid: &mut [Option<Mark>],
    ) {
        let mut peak: Vec<Option<u16>> = vec![None; usize::from(self.width)];
        for s in samples
            .iter()
            .filter(|s| s.dir == self.focus && s.t.0 <= self.cursor.0)
        {
            let col = self.col(s.t.0);
            let row = self.row(value(s));
            let e = &mut peak[usize::from(col)];
            if e.is_none_or(|r| row > r) {
                *e = Some(row);
            }
        }
        for (col, maybe_row) in peak.into_iter().enumerate() {
            if let Some(row) = maybe_row {
                let col = u16::try_from(col).unwrap_or(self.width - 1);
                grid[self.idx(col, row)] = Some(Mark { col, row, glyph, series });
            }
        }
    }
}

/// Projects the focus-direction `wire` RTT series (raw + smoothed) and an optional kernel-srtt
/// `overlay` onto a `width x height` grid. Y is `[0, max_rtt]` over the focus direction's wire
/// rtt/srtt and overlay srtt (so a diverging overlay is not clamped); X is `x_span`; only
/// `t <= cursor` samples are revealed; per (column, series) the tallest revealed mark is kept; a
/// vertical cursor column is drawn at `cursor`. `None` below the minimum rectangle.
#[must_use]
pub fn project(
    wire: &[RttSample],
    overlay: &[RttSample],
    focus: SampleDir,
    x_span: (Nanos, Nanos),
    cursor: Nanos,
    width: u16,
    height: u16,
) -> Option<RttPlot> {
    if width < MIN_W || height < MIN_H {
        return None;
    }
    let (t0, t1) = (x_span.0.0, x_span.1.0);
    let focus_wire = wire.iter().filter(|s| s.dir == focus);
    let max_rtt = focus_wire
        .clone()
        .flat_map(|s| [s.rtt.0, s.srtt.0])
        .chain(overlay.iter().filter(|s| s.dir == focus).map(|s| s.srtt.0))
        .max()
        .unwrap_or(0);
    let geom = Geom {
        focus,
        t0,
        span_t: t1.saturating_sub(t0),
        max_rtt,
        width,
        height,
        cursor,
    };

    let cells = usize::from(width) * usize::from(height);
    let mut grid: Vec<Option<Mark>> = vec![None; cells];
    geom.place(wire, Series::Raw, RAW_GLYPH, |s| s.rtt.0, &mut grid);
    geom.place(wire, Series::Smoothed, SMOOTHED_GLYPH, |s| s.srtt.0, &mut grid);
    geom.place(overlay, Series::Kernel, KERNEL_GLYPH, |s| s.srtt.0, &mut grid);

    let ct = cursor.0.clamp(t0, t1);
    let cursor_col = geom.col(ct);
    for row in 0..height {
        let cell = &mut grid[geom.idx(cursor_col, row)];
        if cell.is_none() {
            *cell = Some(Mark { col: cursor_col, row, glyph: CURSOR_GLYPH, series: Series::Raw });
        }
    }

    let mut marks = Vec::new();
    for row in 0..height {
        for col in 0..width {
            if let Some(m) = grid[geom.idx(col, row)] {
                marks.push(m);
            }
        }
    }
    Some(RttPlot { width, height, max_rtt, x_span, cursor_col, marks })
}
```

Note the `place` grid overwrite: `Raw` then `Smoothed` then `Kernel` are placed in that order, so
if two series land in the *same cell* the later series wins that grid slot (documented render
precedence). Where they land in *different* rows (criterion 14) both survive. This matches
`inflight.rs`'s two-pass wire/overlay placement.

- [ ] **Step 4: Declare the module.** In `lib.rs` add `pub mod rtt;` (after `pub mod inflight;`)
  and `pub use rtt::{RttPlot, Series as RttSeries};` (after the `inflight` re-export).

- [ ] **Step 5: Run to verify pass**

Run: `cargo test -p tcpvisr-tui rtt::`
Expected: PASS.

- [ ] **Step 6: Guardrails + commit**

```bash
cargo fmt --all --check && cargo clippy -p tcpvisr-tui --all-targets --all-features -- -D warnings
git add crates/tcpvisr-tui/src/rtt.rs crates/tcpvisr-tui/src/lib.rs
git commit -m "feat(tui): add the pure RTT projection"
```

---

### Task 5: `App` gains the RTT detail view + focus series

**Files:**
- Modify: `crates/tcpvisr-tui/src/app.rs`

**Interfaces:**
- Consumes: `tcpvisr_engine::RttSample`, `Timeline::rtt_series`.
- Produces: `DetailView::Rtt`; three-way `cycle_detail_view`; `FocusConn.rtt: &'a [RttSample]`.

- [ ] **Step 1: Write the failing tests** — update `tab_cycles_detail_view` and add a focus test:

```rust
#[test]
fn tab_cycles_detail_view() {
    let mut app = app_of(vec![entry(ep(1, 1), ep(2, 22), 0, 0, 0)]);
    assert_eq!(app.detail_view(), DetailView::TimeSequence);
    app.cycle_detail_view();
    assert_eq!(app.detail_view(), DetailView::InFlight);
    app.cycle_detail_view();
    assert_eq!(app.detail_view(), DetailView::Rtt);
    app.cycle_detail_view();
    assert_eq!(app.detail_view(), DetailView::TimeSequence);
}

#[test]
fn focus_exposes_rtt_series() {
    use tcpvisr_core::SampleDir;
    use tcpvisr_engine::RttSample;
    let c = full_conn(ep(1, 1), ep(2, 443), 0, 0, 10, ConnState::Established);
    let mut c2 = c;
    c2.bytes_o2r = 100;
    let rtt = vec![RttSample {
        t: Nanos(0),
        dir: SampleDir::OriginToResponder,
        rtt: Nanos(500),
        srtt: Nanos(500),
    }];
    let tl = Timeline::with_seq(vec![(
        c2,
        vec![ss(0, ConnState::Established, 100, 0)],
        vec![],
        vec![],
        rtt,
    )]);
    let app = App::new(tl, "t".to_string());
    let f = app.focus().expect("selected");
    assert_eq!(f.rtt.len(), 1);
    assert_eq!(f.rtt[0].rtt, Nanos(500));
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p tcpvisr-tui app::`
Expected: FAIL (no `DetailView::Rtt`, no `FocusConn.rtt`).

- [ ] **Step 3: Implement.** In `app.rs`:
  - Import `RttSample`: `use tcpvisr_engine::{AsOf, ConnId, ConnState, InFlightSample, RttSample, SeqSample, Timeline};`
  - Add `pub rtt: &'a [RttSample],` to `FocusConn`.
  - Add `Rtt` to `DetailView`: `pub enum DetailView { TimeSequence, InFlight, Rtt }`.
  - Extend `cycle_detail_view` to the three-way cycle and update its doc comment:

```rust
    /// Advances the detail view (wrapping): Time/Sequence -> In-flight -> RTT -> Time/Sequence.
    pub fn cycle_detail_view(&mut self) {
        self.detail_view = match self.detail_view {
            DetailView::TimeSequence => DetailView::InFlight,
            DetailView::InFlight => DetailView::Rtt,
            DetailView::Rtt => DetailView::TimeSequence,
        };
    }
```

  - In `focus()`, populate the field: `rtt: self.timeline.rtt_series(id),` in the `FocusConn { .. }`.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p tcpvisr-tui app:: keys::`
Expected: PASS (`keys.rs`'s `tab_cycles_view_in_nav_mode` still passes — it only asserts the first step to InFlight).

- [ ] **Step 5: Guardrails + commit**

```bash
cargo fmt --all --check && cargo clippy -p tcpvisr-tui --all-targets --all-features -- -D warnings
git add crates/tcpvisr-tui/src/app.rs
git commit -m "feat(tui): add the RTT detail view variant and focus series"
```

---

### Task 6: Render the RTT view + `fmt_rtt`

**Files:**
- Modify: `crates/tcpvisr-tui/src/render.rs`

**Interfaces:**
- Consumes: `crate::rtt::{self, RttPlot, Mark as RttMark, Series as RttSeries}`; `FocusConn.rtt`; `DetailView::Rtt`.
- Produces: `render_rtt_body`, `fmt_rtt(Nanos) -> String`.

- [ ] **Step 1: Write the failing tests** — add to the `render.rs` tests module:

```rust
#[test]
fn rtt_view_open_shows_graph() {
    let c = conn_span(ep(1, 5), ep(2, 443), 0, 1_000, ConnState::Established);
    let mut c2 = c;
    c2.bytes_o2r = 100;
    // One RTT sample at t=0 (revealed at the initial cursor, which App::new sets to bounds.start
    // = 0) with rtt != srtt so it emits a Raw '.' and a Smoothed '#' in distinct cells — no
    // dependence on scrubbing to reveal a later sample. max_rtt = 3 ms.
    let rtt = vec![tcpvisr_engine::RttSample {
        t: Nanos(0),
        dir: tcpvisr_core::SampleDir::OriginToResponder,
        rtt: Nanos(3_000_000),
        srtt: Nanos(1_500_000),
    }];
    let tl = Timeline::with_seq(vec![(c2, vec![ss(0, 100, 0)], vec![], vec![], rtt)]);
    let mut app = App::new(tl, "t".to_string());
    app.open_detail();
    app.cycle_detail_view(); // InFlight
    app.cycle_detail_view(); // Rtt
    let s = draw(&app, 120, 14);
    assert!(s.contains("DETAIL"), "detail title: {s}");
    assert!(s.contains("RTT"), "rtt legend: {s}");
    assert!(s.contains("0.000s"), "an axis time label: {s}");
    assert!(s.contains("ms"), "ms axis unit (max_rtt = 3.000ms): {s}");
    // Criterion 19: a plotted data glyph must appear. The RTT legend already contains one '#'
    // ("# smoothed"), so require at least TWO — the extra one is the plotted smoothed mark. This
    // fails if the plot area draws nothing (unlike a bare `contains('#')`, which the legend alone
    // would satisfy).
    let hashes = s.matches('#').count();
    assert!(hashes >= 2, "at least one plotted smoothed glyph beyond the legend: {hashes} in {s}");
}

#[test]
fn fmt_rtt_adapts_units() {
    assert_eq!(fmt_rtt(Nanos(450)), "450ns");
    assert_eq!(fmt_rtt(Nanos(1_500_000)), "1.500ms");
    assert_eq!(fmt_rtt(Nanos(2_000_000_000)), "2.000s");
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p tcpvisr-tui render::tests::rtt_view_open_shows_graph render::tests::fmt_rtt_adapts_units`
Expected: FAIL (`fmt_rtt` undefined; `Rtt` arm missing).

- [ ] **Step 3: Implement.** In `render.rs`:
  - Add the import: `use crate::rtt::{self, Mark as RttMark, RttPlot, Series as RttSeries};`
  - Import `DetailView` already present. Add the match arm in `render_detail`:

```rust
        DetailView::Rtt => render_rtt_body(frame, app, inner, &focus),
```

  - Add `render_rtt_body` (mirror `render_inflight_body`):

```rust
/// Draws the RTT graph (raw per-ack points + smoothed SRTT line) into the reserved pane interior
/// (M8). The kernel-srtt overlay series is empty on replay; M12 fills it (ADR-0013 §4).
fn render_rtt_body(frame: &mut Frame, app: &App, inner: Rect, focus: &FocusConn<'_>) {
    let plot_w = inner.width - GUTTER;
    let plot_h = inner.height - 2; // legend + time labels
    let Some(plot) = rtt::project(
        focus.rtt,
        &[],
        focus.focus_dir,
        focus.x_span,
        app.cursor(),
        plot_w,
        plot_h,
    ) else {
        frame.render_widget(Paragraph::new("widen terminal to view graph"), inner);
        return;
    };
    draw_rtt_legend(frame, inner);
    draw_rtt_plot(frame, inner, GUTTER, &plot);
    draw_rtt_axes(frame, inner, GUTTER, &plot);
}

fn draw_rtt_legend(frame: &mut Frame, inner: Rect) {
    let legend = format!(
        "RTT   {} raw  {} smoothed",
        rtt::RAW_GLYPH,
        rtt::SMOOTHED_GLYPH
    );
    let row = Rect { x: inner.x, y: inner.y, width: inner.width, height: 1 };
    frame.render_widget(Paragraph::new(legend), row);
}

fn draw_rtt_plot(frame: &mut Frame, inner: Rect, gutter: u16, plot: &RttPlot) {
    let buf = frame.buffer_mut();
    let x0 = inner.x + gutter;
    let y_top = inner.y + 1;
    for &RttMark { col, row, glyph, series } in &plot.marks {
        let screen_row = plot.height - 1 - row;
        let x = x0 + col;
        let y = y_top + screen_row;
        let color = match series {
            RttSeries::Kernel => Color::Cyan,
            RttSeries::Smoothed => Color::Green,
            RttSeries::Raw => Color::Reset,
        };
        buf.set_string(x, y, glyph.to_string(), Style::default().fg(color));
    }
}

fn draw_rtt_axes(frame: &mut Frame, inner: Rect, gutter: u16, plot: &RttPlot) {
    let buf = frame.buffer_mut();
    let y_top = inner.y + 1;
    buf.set_string(inner.x, y_top, format!("{:>7}", fmt_rtt(Nanos(plot.max_rtt))), Style::default());
    let y_bottom = y_top + plot.height - 1;
    buf.set_string(inner.x, y_bottom, format!("{:>7}", fmt_rtt(Nanos(0))), Style::default());
    let label_row = inner.y + inner.height - 1;
    let start = fmt_seconds(plot.x_span.0);
    let end = fmt_seconds(plot.x_span.1);
    buf.set_string(inner.x + gutter, label_row, format!("{start}s"), Style::default());
    let end_label = format!("{end}s");
    let end_x = inner
        .x
        .saturating_add(inner.width)
        .saturating_sub(u16::try_from(end_label.len()).unwrap_or(0));
    buf.set_string(end_x, label_row, end_label, Style::default());
}
```

  - Add `fmt_rtt` near `fmt_seq` (integer, unit-adapting, `<whole>.<3-frac><unit>`; `ns` prints
    whole ns with no fraction):

```rust
/// Formats a nanosecond RTT with an adaptive unit (ns/µs/ms/s) so a sub-millisecond value does
/// not collapse to `0.000ms`. Integer-only (deterministic snapshots). `<1 µs` prints whole ns.
fn fmt_rtt(t: Nanos) -> String {
    const UNITS: [(u64, &str); 3] = [
        (1_000_000_000, "s"),
        (1_000_000, "ms"),
        (1_000, "\u{b5}s"),
    ];
    let n = t.0;
    for (div, unit) in UNITS {
        if n >= div {
            return format!("{}.{:03}{unit}", n / div, (n % div) * 1000 / div);
        }
    }
    format!("{n}ns")
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p tcpvisr-tui render::`
Expected: PASS (existing render tests unchanged; new RTT tests pass).

- [ ] **Step 5: Guardrails + commit**

```bash
cargo fmt --all --check && cargo clippy -p tcpvisr-tui --all-targets --all-features -- -D warnings
git add crates/tcpvisr-tui/src/render.rs
git commit -m "feat(tui): render the RTT detail view"
```

---

### Task 7: CLI wiring on the replay path + integration test

**Files:**
- Modify: `crates/tcp-visr/src/main.rs`

**Interfaces:**
- Consumes: `EngineConfig.collect_rtt_timeline`, `App::focus().rtt`.

- [ ] **Step 1: Write the failing tests** — add to the `build_replay_tests` module:

```rust
#[test]
fn run_replay_config_enables_rtt_collection() {
    let cfg = replay_engine_config(10_000_000);
    assert!(cfg.collect_rtt_timeline, "replay must collect the RTT timeline");
}

#[test]
fn focus_rtt_series_non_empty_for_fixture() {
    let cfg = replay_engine_config(10_000_000);
    let app = build_replay_app(&fixture(), cfg).expect("build");
    let f = app.focus().expect("a connection is selected");
    // metrics_basic conn 0: focus dir O2R has RTT at t=1ms and t=3ms (ADR-0013 / M3 oracle).
    assert!(!f.rtt.is_empty(), "focus connection has RTT samples");
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p tcp-visr run_replay_config_enables_rtt_collection focus_rtt_series_non_empty_for_fixture`
Expected: FAIL (flag not set / `rtt` empty).

- [ ] **Step 3: Implement.** In `replay_engine_config`, add the flag:

```rust
    EngineConfig {
        collect_state_timeline: true,
        collect_seq_timeline: true,
        collect_inflight_timeline: true,
        collect_rtt_timeline: true,
        max_samples,
        ..EngineConfig::default()
    }
```

Update the doc comment to mention the RTT timeline.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p tcp-visr`
Expected: PASS.

- [ ] **Step 5: Guardrails + commit**

```bash
cargo fmt --all --check && cargo clippy --all-targets --all-features -- -D warnings
git add crates/tcp-visr/src/main.rs
git commit -m "feat(cli): collect the RTT timeline on the replay path"
```

---

### Task 8: Full gate + docs marker

**Files:**
- Modify: `CLAUDE.md` (current-state line), `docs/design/tcp-visr-design.md` (if it tracks implemented milestones)

- [ ] **Step 1: Run the full CI gate**

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace
cargo test -p tcpvisr-ingest --features live
cargo deny check
```

Expected: all green.

- [ ] **Step 2: Update the current-state prose** in `CLAUDE.md` (M0–M7 → M0–M8; add the RTT view
  to the `Tab` description) and mark M8 implemented wherever M7 is marked. Commit:

```bash
git add CLAUDE.md docs/design/tcp-visr-design.md
git commit -m "docs: mark M8 (RTT detail) implemented"
```

---

## Self-Review

**Spec coverage:** Task 1 → criterion 6 (flag default-off); Task 2 → criterion 4 (timeline carry/unknown); Task 3 → criteria 1,2,3,5 (attribution, EWMA, Karn/dup sparsity, ceiling); Task 4 → criteria 7–15 (projection incl. overlay + raw/smoothed alignment); Task 5 → criteria 16,17 (Tab cycle, follows selection); Task 6 → criteria 13,13a,18,19 (narrow guard, fmt_rtt units, closed byte-identical, RTT open shows graph); Task 7 → criterion 20 (CLI wiring, focus non-empty, ceiling). `keys.rs` criterion-16 filter-mode inertness is already covered by the existing `tab_does_not_cycle_view_in_filter_mode` test (no change needed). All 20+2 criteria mapped.

**Placeholder scan:** Task 3 Step 1 leaves test *bodies* as prose stubs deliberately — they must be filled by matching the existing `inflight_tests`/`derive_tests` segment builders in the same files, which are the authoritative fixtures; the surrounding code (fields, helpers, `collect_rtt_points`, EWMA) is complete. Task 4 Step 1 similarly defers the test bodies to mirror `inflight.rs`. This is intentional (reuse the in-file builders, do not fabricate a second `Segment` builder), not a gap.

**Type consistency:** `RttSample { t, dir, rtt, srtt }` used identically in Tasks 2–7. `Series { Raw, Smoothed, Kernel }` re-exported as `RttSeries` (avoids clashing with `inflight::Series`); render imports it as `RttSeries`. `project(wire, overlay, focus, x_span, cursor, width, height)` signature matches the `render_rtt_body` call site. `fmt_rtt(Nanos) -> String` matches its call sites. `cycle_detail_view` three-way cycle matches the `tab_cycles_detail_view` test.
