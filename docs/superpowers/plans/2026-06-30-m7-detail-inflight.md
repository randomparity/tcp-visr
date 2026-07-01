# M7 — Detail: In-flight / cwnd Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the second replay-TUI detail view — a wire-estimated in-flight (bytes-outstanding) sawtooth with a typed kernel-cwnd overlay seam — reachable via a `Tab` view-switcher, driven by the transport cursor.

**Architecture:** Mirror the M6 pattern exactly (ADR-0011 → ADR-0012). The engine collects a dedicated per-connection `InFlightSample` series on the replay path (both directions snapshotted per segment so ack-driven drains are captured), the `Timeline` carries it, a pure `tcpvisr-tui::inflight` projection maps it to a `Mark { col, row, glyph, series }` grid, and `render.rs` dispatches on a new `App::detail_view()` (`Tab`-cycled) to draw either the M6 seq view or the new in-flight view.

**Tech Stack:** Rust 1.88.0 (pinned), Cargo workspace, ratatui + `TestBackend`, `proptest` (already a dev-dep), `thiserror`. No new dependencies.

## Global Constraints

- **Spec:** `docs/superpowers/specs/m7-detail-inflight.md`. **ADR:** `docs/adr/0012-detail-inflight-and-view-switcher.md`. These are authoritative; where they disagree with this plan, they win.
- **Toolchain pinned to Rust 1.88.0** (`rust-toolchain.toml`). No new crates (a new dep must be pinned `=x.y.z` and license-listed in `deny.toml`; not needed here).
- **Pure engine (ADR-0002):** `tcpvisr-engine` gets no I/O, no clock read, no `Instant::now()`. Time enters only as `Nanos` on segments.
- **Absolute imports only**, ≤100 lines/function, cyclomatic complexity ≤8, ≤5 positional params, 100-char lines, Google-style docstrings on non-trivial public APIs.
- **Clippy is strict workspace-wide** (`unwrap_used`/`expect_used`/`panic`/`print_stdout` denied in non-test code). Tests are exempt via `clippy.toml`; in non-`#[test]` test-support code scope relaxations with a **file-level** `#![allow(...)]` (item-level `#[allow]` is denied by `allow_attributes`).
- **`MetricSample` and the `metrics` JSON stay frozen** — do not touch them or the M3 oracle goldens (ADR-0012 §1).
- **`Timeline::new` signature stays frozen** so the M4/M5 fixtures are untouched; only `Timeline::with_seq` changes (ADR-0012 §1, spec §4).
- **Per-commit guardrails (run before every commit; all must be green, zero warnings):**
  ```bash
  cargo fmt --all --check
  cargo clippy --all-targets --all-features -- -D warnings
  cargo test --workspace
  ```
  Before the first push, also run `cargo test -p tcpvisr-ingest --features live` and `cargo deny check` (the full CI gate).
- **Commits:** Conventional Commits, imperative, ≤72-char subject, one logical change per commit, ending with the trailer:
  ```
  Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
  ```
- **TDD is mandatory:** failing test first → confirm it fails for the right reason → minimal implementation → green → refactor while green.

## File Structure

| File | Change | Responsibility |
|------|--------|----------------|
| `crates/tcpvisr-engine/src/timeline.rs` | modify | Add `InFlightSample`; `Entry.inflight`; `inflight_series(id)`; extend `with_seq` to a 4-tuple |
| `crates/tcpvisr-engine/src/config.rs` | modify | Add `collect_inflight_timeline: bool` (default `false`) |
| `crates/tcpvisr-engine/src/metrics.rs` | modify | Add `pub(crate) MetricState::in_flight(dir) -> Option<u64>` |
| `crates/tcpvisr-engine/src/tracker.rs` | modify | `ConnTrack.inflight`; `record_inflight`; snapshot both directions per segment; pass through `into_timeline` |
| `crates/tcpvisr-engine/src/lib.rs` | modify | Re-export `InFlightSample` |
| `crates/tcpvisr-tui/src/inflight.rs` | create | Pure in-flight projection: `Series`, `Mark`, `InFlightPlot`, `project(...)`, glyph consts |
| `crates/tcpvisr-tui/src/app.rs` | modify | `DetailView`; `detail_view()`/`cycle_detail_view()`; `FocusConn.inflight` |
| `crates/tcpvisr-tui/src/keys.rs` | modify | Nav-mode `Tab` → `cycle_detail_view` |
| `crates/tcpvisr-tui/src/render.rs` | modify | Dispatch on `detail_view()`; `render_inflight`; footer `⇥ view` |
| `crates/tcpvisr-tui/src/lib.rs` | modify | Re-export `DetailView` |
| `crates/tcp-visr/src/main.rs` | modify | Set `collect_inflight_timeline = true` on the replay path; bin integration test |

Task order is bottom-up: each task compiles and tests green on its own, and later tasks consume only names produced by earlier ones.

---

### Task 1: `InFlightSample` type + `collect_inflight_timeline` flag

**Files:**
- Modify: `crates/tcpvisr-engine/src/timeline.rs` (add the type near `SeqSample`, ~line 34)
- Modify: `crates/tcpvisr-engine/src/config.rs` (add the flag)
- Modify: `crates/tcpvisr-engine/src/lib.rs` (re-export)
- Test: inline `#[cfg(test)]` in `timeline.rs` and `config.rs`

**Interfaces:**
- Produces:
  ```rust
  // timeline.rs
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub struct InFlightSample {
      pub t: Nanos,
      pub dir: SampleDir,
      pub bytes: u64,
  }
  // config.rs: EngineConfig gains `pub collect_inflight_timeline: bool` (default false)
  // lib.rs: `pub use timeline::{..., InFlightSample};`
  ```

- [ ] **Step 1: Write the failing test** — in `config.rs` `mod tests`, extend `defaults_match_spec` (or add a test) asserting the new default:

```rust
#[test]
fn inflight_timeline_defaults_off() {
    let c = EngineConfig::default();
    assert!(!c.collect_inflight_timeline);
}
```

And in `timeline.rs` `mod tests` add:

```rust
#[test]
fn inflight_sample_is_copy_and_holds_fields() {
    let s = InFlightSample { t: Nanos(5), dir: SampleDir::OriginToResponder, bytes: 42 };
    let copy = s; // Copy, not move
    assert_eq!(copy, s);
    assert_eq!(copy.bytes, 42);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p tcpvisr-engine inflight_ 2>&1 | tail -20`
Expected: compile error — `InFlightSample` / `collect_inflight_timeline` not found.

- [ ] **Step 3: Implement**

In `timeline.rs`, after the `SeqSample` struct:

```rust
/// One point on a connection's In-flight graph (design §6, ADR-0012 §1). `bytes` is the wire
/// bytes-outstanding for `dir` (the engine's `in_flight_bytes`) at time `t`; both directions are
/// snapshotted per segment so an ACK's drain is sampled at ack time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InFlightSample {
    pub t: Nanos,
    pub dir: SampleDir,
    pub bytes: u64,
}
```

In `config.rs`, add the field to `EngineConfig` (after `collect_seq_timeline`) with a doc line, and set `collect_inflight_timeline: false` in `Default`:

```rust
    /// Whether the tracker records a per-segment `InFlightSample` timeline (M7 detail).
    pub collect_inflight_timeline: bool,
```

In `lib.rs`, extend the timeline re-export:

```rust
pub use timeline::{AsOf, InFlightSample, SeqKind, SeqSample, StateSample, Timeline};
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p tcpvisr-engine inflight_ 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Guardrails + commit**

```bash
cargo fmt --all --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test --workspace
git add crates/tcpvisr-engine/src/timeline.rs crates/tcpvisr-engine/src/config.rs crates/tcpvisr-engine/src/lib.rs
git commit -m "feat(engine): add InFlightSample and collect_inflight_timeline flag"
```
(Trailer as in Global Constraints.)

---

### Task 2: `MetricState::in_flight(dir)` outstanding-bytes query

**Files:**
- Modify: `crates/tcpvisr-engine/src/metrics.rs` (add the method on `MetricState`, near `observe`)
- Test: inline `derive_tests` in `metrics.rs`

**Interfaces:**
- Consumes: existing `DirState { snd_nxt, acked }`, `serial_le`, `TcpSeq::serial_diff` (all in `metrics.rs`).
- Produces:
  ```rust
  impl MetricState {
      pub(crate) fn in_flight(&self, dir: Direction) -> Option<u64>;
  }
  ```
  Returns `snd_nxt − acked` (serial, clamped ≥ 0) for `dir`, or `None` if `dir` has no `snd_nxt` or no `acked` yet.

- [ ] **Step 1: Write the failing test** — in `metrics.rs` `mod derive_tests`:

```rust
#[test]
fn in_flight_query_matches_sample_and_snapshots_opposite_drain() {
    let mut m = MetricState::new();
    let c = cfg();
    // O2R sends 10 bytes @seq100; own outstanding == 10, query agrees.
    let s1 = m.observe(&seg(ACK, 100, 1, 10, 1_000, false), Direction::OriginToResponder, &c);
    assert_eq!(s1.in_flight_bytes, 10);
    assert_eq!(m.in_flight(Direction::OriginToResponder), Some(10));
    // R2O has no send frontier yet -> None.
    assert_eq!(m.in_flight(Direction::ResponderToOrigin), None);
    // R2O ACK=110 drains O2R: querying O2R now reads 0 (the ack-time drain).
    m.observe(&seg(ACK, 1, 110, 0, 2_000, false), Direction::ResponderToOrigin, &c);
    assert_eq!(m.in_flight(Direction::OriginToResponder), Some(0));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p tcpvisr-engine in_flight_query 2>&1 | tail -20`
Expected: compile error — no method `in_flight`.

- [ ] **Step 3: Implement** — add to `impl MetricState` in `metrics.rs`:

```rust
/// The wire bytes-outstanding for `dir` (`snd_nxt − acked`, serial, clamped ≥ 0), or `None`
/// if `dir` has no send frontier or nothing acked yet. Pure read of current state; used by
/// the M7 in-flight collector to snapshot both directions (ADR-0012 §1).
pub(crate) fn in_flight(&self, dir: Direction) -> Option<u64> {
    let d = idx(dir);
    let snd = self.dir[d].snd_nxt?;
    let acked = self.dir[d].acked?;
    Some(if serial_le(acked, snd) {
        u64::from(snd.serial_diff(acked))
    } else {
        0
    })
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p tcpvisr-engine in_flight_query 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Guardrails + commit**

```bash
cargo fmt --all --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test --workspace
git add crates/tcpvisr-engine/src/metrics.rs
git commit -m "feat(engine): add MetricState::in_flight outstanding-bytes query"
```

---

### Task 3: Timeline carries the in-flight series (`with_seq` 4-tuple + `inflight_series`)

**Files:**
- Modify: `crates/tcpvisr-engine/src/timeline.rs` (`Entry.inflight`, `with_seq` signature, `new` delegation, `inflight_series`)
- Test: inline `mod tests` in `timeline.rs`

**Interfaces:**
- Consumes: `InFlightSample` (Task 1).
- Produces:
  ```rust
  impl Timeline {
      pub fn with_seq(
          conns: Vec<(Connection, Vec<StateSample>, Vec<SeqSample>, Vec<InFlightSample>)>,
      ) -> Self;
      pub fn inflight_series(&self, id: ConnId) -> &[InFlightSample];
  }
  ```
  `Timeline::new` is UNCHANGED (delegates with empty seq **and** empty inflight vectors).

- [ ] **Step 1: Write the failing test** — in `timeline.rs` `mod tests` (reuse the existing `conn`, `ss`, `sq` helpers):

```rust
fn iff(t: u64, bytes: u64) -> InFlightSample {
    InFlightSample { t: Nanos(t), dir: SampleDir::OriginToResponder, bytes }
}

#[test]
fn with_seq_carries_inflight_sorted_and_exposes_series() {
    let c = conn(0, 100, 300, ConnState::Established);
    let id = c.id;
    let tl = Timeline::with_seq(vec![(
        c,
        vec![ss(100, ConnState::Established, 0, 0)],
        vec![sq(100, 0, 10)],
        vec![iff(300, 5), iff(100, 10)], // supplied out of t-order
    )]);
    let series = tl.inflight_series(id);
    assert_eq!(series.len(), 2);
    assert_eq!(series[0].t, Nanos(100), "sorted by t at construction");
    assert_eq!(series[1].t, Nanos(300));
    assert_eq!(series[0].bytes, 10);
}

#[test]
fn inflight_series_empty_for_unknown_id() {
    let c = conn(0, 0, 10, ConnState::Established);
    let other = ConnId { pair: EndpointPair::new(ep(9, 1), ep(9, 2)), instance: 7 };
    let tl = Timeline::new(vec![(c, vec![ss(0, ConnState::Established, 0, 0)])]);
    assert!(tl.inflight_series(other).is_empty());
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p tcpvisr-engine with_seq_carries_inflight inflight_series_empty 2>&1 | tail -20`
Expected: compile error — `with_seq` takes 3-tuples, no `inflight_series`.

- [ ] **Step 3: Implement**

- Add `inflight: Vec<InFlightSample>` to `struct Entry`.
- Change `Timeline::new` to delegate with an empty inflight vector:
  ```rust
  pub fn new(conns: Vec<(Connection, Vec<StateSample>)>) -> Self {
      Self::with_seq(
          conns.into_iter().map(|(c, s)| (c, s, Vec::new(), Vec::new())).collect(),
      )
  }
  ```
- Change `with_seq` to accept the 4-tuple, stable-sort the inflight series by `t`, and store it:
  ```rust
  pub fn with_seq(
      conns: Vec<(Connection, Vec<StateSample>, Vec<SeqSample>, Vec<InFlightSample>)>,
  ) -> Self {
      // ... existing end/start computation over (c, s, _, _) ...
      for (conn, mut samples, mut seq, mut inflight) in conns {
          samples.sort_by_key(|s| s.t);
          seq.sort_by_key(|s| s.t);
          inflight.sort_by_key(|s| s.t);
          // ... existing event_times push over samples ...
          entries.push(Entry { conn, samples, seq, inflight, effective_end });
      }
      // ... unchanged tail ...
  }
  ```
  Update the `end`/`start` closures to destructure the 4-tuple (`(c, _, _, _)` and `(_, s, _, _)`).
- Add the accessor after `seq_series`:
  ```rust
  /// The focus connection's `InFlightSample` series (`t`-sorted), or an empty slice if `id` is
  /// unknown or its series was not collected.
  #[must_use]
  pub fn inflight_series(&self, id: ConnId) -> &[InFlightSample] {
      match self.entries.iter().find(|e| e.conn.id == id) {
          Some(e) => &e.inflight,
          None => &[],
      }
  }
  ```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p tcpvisr-engine 2>&1 | tail -20`
Expected: PASS (all engine tests, including the existing M6 `with_seq` test which now needs the 4th tuple element — see Step 4b).

- [ ] **Step 4b: Fix the one existing `with_seq` call site in `timeline.rs` tests**

The M6 test `with_seq_sorts_and_exposes_series_and_x_span` calls `with_seq` with a 3-tuple. Add a trailing `Vec::new()` (empty inflight) to its single tuple so it compiles. Re-run Step 4.

- [ ] **Step 5: Guardrails + commit**

```bash
cargo fmt --all --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test --workspace
git add crates/tcpvisr-engine/src/timeline.rs
git commit -m "feat(engine): carry the in-flight series through the Timeline"
```

---

### Task 4: Tracker collects the in-flight series (both directions per segment)

**Files:**
- Modify: `crates/tcpvisr-engine/src/tracker.rs` (`ConnTrack.inflight`, init, `record_inflight`, `collect_inflight_points`, call sites, `into_timeline`)
- Test: inline `mod seq_tests` (or a new `mod inflight_tests`) in `tracker.rs`

**Interfaces:**
- Consumes: `MetricState::in_flight` (Task 2), `InFlightSample` (Task 1), `Timeline::with_seq` 4-tuple (Task 3), the `MetricSample` already derived in `observe_segment`/`create_instance`.
- Produces: `into_timeline` now yields a `Timeline` whose `inflight_series(id)` is populated when `collect_inflight_timeline` is set.

**Design note (from spec §4 / ADR-0012 §1):** after the existing `metrics.observe(...)` call, for each `Direction` in `[OriginToResponder, ResponderToOrigin]`, if `metrics.in_flight(d)` is `Some(b)`, record `InFlightSample { t: seg.ts, dir: dir_sample(d), bytes: b }`. This snapshots the sender (own) and, when acked, the opposite direction — capturing ack-time drains. Recording is gated by `collect_inflight_timeline` and `!overflowed`, and each sample counts against `max_samples` via the shared counter.

- [ ] **Step 1: Write the failing tests** — add to `tracker.rs` a `#[cfg(test)] mod inflight_tests` (reuse `test_support::{ep, seg}`):

```rust
#[cfg(test)]
mod inflight_tests {
    use super::test_support::{ep, seg};
    use super::*;
    use tcpvisr_core::{Nanos, SampleDir, TcpFlags};

    fn iff_cfg() -> EngineConfig {
        EngineConfig {
            collect_state_timeline: true,
            collect_seq_timeline: true,
            collect_inflight_timeline: true,
            ..EngineConfig::default()
        }
    }

    fn only_id(tl: &crate::timeline::Timeline) -> ConnId {
        tl.connections().next().expect("one connection").id
    }

    fn o2r_inflight(tl: &crate::timeline::Timeline) -> Vec<(u64, u64)> {
        tl.inflight_series(only_id(tl))
            .iter()
            .filter(|s| s.dir == SampleDir::OriginToResponder)
            .map(|s| (s.t.0, s.bytes))
            .collect()
    }

    #[test]
    fn inflight_rises_on_send_and_drains_at_ack_time() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let mut t = Tracker::new(iff_cfg());
        t.observe(&seg(c, s, TcpFlags::ACK, 100, 1, 10, 1_000)); // O2R +10
        t.observe(&seg(s, c, TcpFlags::ACK, 1, 110, 0, 2_000)); // R2O ACK drains O2R
        t.observe(&seg(c, s, TcpFlags::ACK, 110, 1, 5, 3_000)); // O2R +5
        let tl = t.into_timeline().expect("timeline");
        assert_eq!(o2r_inflight(&tl), vec![(1_000, 10), (2_000, 0), (3_000, 5)]);
    }

    #[test]
    fn inflight_is_serial_correct_across_u32_wrap() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let mut t = Tracker::new(iff_cfg());
        t.observe(&seg(c, s, TcpFlags::ACK, u32::MAX - 100, 1, 50, 1_000));
        t.observe(&seg(c, s, TcpFlags::ACK, 200, 1, 50, 2_000)); // never acked
        let tl = t.into_timeline().expect("timeline");
        let bytes: Vec<u64> = o2r_inflight(&tl).iter().map(|(_, b)| *b).collect();
        assert_eq!(bytes, vec![50, 351]); // serial distance across the wrap
    }

    #[test]
    fn inflight_off_by_default_is_empty() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let mut t = Tracker::new(EngineConfig {
            collect_state_timeline: true,
            ..EngineConfig::default()
        });
        t.observe(&seg(c, s, TcpFlags::ACK, 100, 1, 10, 1_000));
        let tl = t.into_timeline().expect("timeline");
        assert!(tl.inflight_series(only_id(&tl)).is_empty());
    }

    #[test]
    fn inflight_collection_counts_against_ceiling() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let mut cfg = iff_cfg();
        cfg.max_samples = 1; // first segment already produces state + seq + inflight samples
        let mut t = Tracker::new(cfg);
        t.observe(&seg(c, s, TcpFlags::ACK, 100, 1, 10, 1_000));
        t.observe(&seg(c, s, TcpFlags::ACK, 110, 1, 20, 2_000));
        assert!(matches!(
            t.into_timeline().expect_err("ceiling"),
            MetricError::SampleCeiling { .. }
        ));
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p tcpvisr-engine inflight_tests 2>&1 | tail -25`
Expected: compile error — `ConnTrack` has no `inflight`, `into_timeline` still builds 3-tuples.

- [ ] **Step 3: Implement**

- Add `inflight: Vec<InFlightSample>` to `struct ConnTrack`, initialise `inflight: Vec::new()` in `create_instance`.
- Import `InFlightSample` at the top (`use crate::timeline::{InFlightSample, SeqKind, SeqSample, StateSample, Timeline};`).
- Add a `record_inflight` helper mirroring `record_seq`:
  ```rust
  /// Stores one `InFlightSample` on the instance at `idx`, enforcing `max_samples`.
  fn record_inflight(&mut self, idx: usize, sample: InFlightSample) {
      if self.overflowed {
          return;
      }
      if self.collected_samples >= self.config.max_samples {
          self.overflowed = true;
          return;
      }
      self.collected_samples += 1;
      self.conns[idx].inflight.push(sample);
  }
  ```
- Add a collector mirroring `collect_seq_points`, snapshotting both directions:
  ```rust
  /// Snapshots each direction's current outstanding for this segment when in-flight collection
  /// is on and not overflowed (ADR-0012 §1: both directions, so ACK-driven drains are sampled).
  fn collect_inflight_points(&mut self, idx: usize, seg: &Segment) {
      if self.overflowed || !self.config.collect_inflight_timeline {
          return;
      }
      for d in [Direction::OriginToResponder, Direction::ResponderToOrigin] {
          if let Some(bytes) = self.conns[idx].metrics.in_flight(d) {
              self.record_inflight(idx, InFlightSample { t: seg.ts, dir: dir_sample(d), bytes });
          }
      }
  }
  ```
- Widen the "derive metrics?" gate in **both** `observe_segment` and `create_instance` to also fire when `collect_inflight_timeline` is set, and call the collector right after `collect_seq_points`. In `observe_segment`:
  ```rust
  let want_metric = self.should_collect(self.conns[idx].id);
  if !self.overflowed
      && (want_metric || self.config.collect_seq_timeline || self.config.collect_inflight_timeline)
  {
      let sample = self.conns[idx].metrics.observe(seg, dir, &self.config);
      if want_metric {
          self.record_sample(idx, sample);
      }
      self.collect_seq_points(idx, seg, dir, &sample);
      self.collect_inflight_points(idx, seg);
  }
  ```
  In `create_instance`, widen the `.then(...)` guard the same way and, after `collect_seq_points`, call `self.collect_inflight_points(idx, seg);` (inside the `if let Some(sample)` block).
- In `into_timeline`, build 4-tuples: `(c.view(), c.states.clone(), c.seq.clone(), c.inflight.clone())`.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p tcpvisr-engine 2>&1 | tail -25`
Expected: PASS (new `inflight_tests` + all existing engine tests).

- [ ] **Step 5: Guardrails + commit**

```bash
cargo fmt --all --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test --workspace
git add crates/tcpvisr-engine/src/tracker.rs
git commit -m "feat(engine): collect the in-flight timeline on the replay path"
```

---

### Task 5: Pure in-flight projection (`tcpvisr-tui::inflight`)

**Files:**
- Create: `crates/tcpvisr-tui/src/inflight.rs`
- Modify: `crates/tcpvisr-tui/src/lib.rs` (add `pub mod inflight;` and re-exports)
- Test: inline `mod tests` in `inflight.rs`

**Interfaces:**
- Consumes: `tcpvisr_core::{Nanos, SampleDir}`, `tcpvisr_engine::InFlightSample`.
- Produces:
  ```rust
  pub const MIN_W: u16 = 8;
  pub const MIN_H: u16 = 3;
  pub const WIRE_GLYPH: char = '#';
  pub const CWND_GLYPH: char = '+';
  pub const CURSOR_GLYPH: char = '\u{250a}'; // ┊

  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum Series { Wire, Cwnd }

  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub struct Mark { pub col: u16, pub row: u16, pub glyph: char, pub series: Series }

  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct InFlightPlot {
      pub width: u16,
      pub height: u16,
      pub max_bytes: u64,
      pub x_span: (Nanos, Nanos),
      pub cursor_col: u16,
      pub marks: Vec<Mark>,
  }

  pub fn project(
      wire: &[InFlightSample],
      overlay: &[InFlightSample],
      focus: SampleDir,
      x_span: (Nanos, Nanos),
      cursor: Nanos,
      width: u16,
      height: u16,
  ) -> Option<InFlightPlot>;
  ```

**Semantics (spec §3.4):** `max_bytes` = max `bytes` over the focus-direction wire **and** overlay samples (0 → row 0). `col(t)`/`row(b)` are the integer-proportion maps from the spec. Reveal keeps only `t ≤ cursor`; numeric-max bucketing per `(col, series)` keeps the tallest revealed mark; the cursor column fills empty cells with `CURSOR_GLYPH`. Below `MIN_W`/`MIN_H` → `None`.

- [ ] **Step 1: Write the failing tests** — create `inflight.rs` with `mod tests` (mirror `detail.rs`'s test helpers):

```rust
#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use tcpvisr_core::SampleDir;

    fn wire(t: u64, bytes: u64) -> InFlightSample {
        InFlightSample { t: Nanos(t), dir: SampleDir::OriginToResponder, bytes }
    }

    fn mark_at(p: &InFlightPlot, col: u16, row: u16) -> Option<Mark> {
        p.marks.iter().find(|m| m.col == col && m.row == row).copied()
    }

    #[test]
    fn too_small_viewport_yields_none() {
        let s = [wire(0, 10)];
        assert!(project(&s, &[], SampleDir::OriginToResponder, (Nanos(0), Nanos(10)), Nanos(0), MIN_W - 1, MIN_H).is_none());
        assert!(project(&s, &[], SampleDir::OriginToResponder, (Nanos(0), Nanos(10)), Nanos(0), MIN_W, MIN_H - 1).is_none());
    }

    #[test]
    fn corners_place_at_exact_indices() {
        let s = [wire(0, 0), wire(100, 40)]; // max_bytes = 40
        let p = project(&s, &[], SampleDir::OriginToResponder, (Nanos(0), Nanos(100)), Nanos(100), 10, 5).unwrap();
        assert_eq!(p.max_bytes, 40);
        assert_eq!(mark_at(&p, 0, 0).map(|m| m.glyph), Some(WIRE_GLYPH), "bottom-left");
        assert_eq!(mark_at(&p, 9, 4).map(|m| m.glyph), Some(WIRE_GLYPH), "top-right col W-1 row H-1");
    }

    #[test]
    fn reveal_hides_marks_after_cursor() {
        let s = [wire(0, 10), wire(10, 20), wire(20, 30)];
        let span = (Nanos(0), Nanos(20));
        let early = project(&s, &[], SampleDir::OriginToResponder, span, Nanos(10), 20, 10).unwrap();
        assert_eq!(early.marks.iter().filter(|m| m.series == Series::Wire).count(), 2);
        let all = project(&s, &[], SampleDir::OriginToResponder, span, Nanos(20), 20, 10).unwrap();
        assert_eq!(all.marks.iter().filter(|m| m.series == Series::Wire).count(), 3);
    }

    #[test]
    fn axes_fixed_regardless_of_cursor() {
        let s = [wire(0, 0), wire(100, 90)];
        let span = (Nanos(0), Nanos(100));
        let a = project(&s, &[], SampleDir::OriginToResponder, span, Nanos(0), 20, 10).unwrap();
        let b = project(&s, &[], SampleDir::OriginToResponder, span, Nanos(100), 20, 10).unwrap();
        assert_eq!((a.max_bytes, a.x_span), (b.max_bytes, b.x_span));
        assert_eq!(a.max_bytes, 90);
    }

    #[test]
    fn numeric_max_bucketing_over_revealed_only() {
        // Two samples share column 0 (t=0,t=1 with a wide span); the taller wins.
        let s = [wire(0, 10), wire(1, 40), wire(2, 90)];
        let span = (Nanos(0), Nanos(1000));
        // cursor=1 reveals t=0,1 (rows for 10,40); t=2 (90) hidden -> column 0 peak is 40's row.
        let p = project(&s, &[], SampleDir::OriginToResponder, span, Nanos(1), 20, 11).unwrap();
        // max_bytes=90 over the whole series; row(40)= (40*10)/90 = 4; row(90)=10.
        let col0: Vec<u16> = p.marks.iter().filter(|m| m.col == 0 && m.series == Series::Wire).map(|m| m.row).collect();
        assert_eq!(col0, vec![4], "one wire mark at the revealed peak row, not the hidden t=2 peak");
    }

    #[test]
    fn degenerate_spans_do_not_divide_by_zero() {
        let s = [wire(50, 10)];
        let p = project(&s, &[], SampleDir::OriginToResponder, (Nanos(50), Nanos(50)), Nanos(50), 10, 5).unwrap();
        assert_eq!(mark_at(&p, 0, 0).map(|m| m.glyph), Some(WIRE_GLYPH));
        let z = [wire(0, 0), wire(10, 0)]; // all zero -> max_bytes 0 -> row 0
        let pz = project(&z, &[], SampleDir::OriginToResponder, (Nanos(0), Nanos(10)), Nanos(10), 10, 5).unwrap();
        assert_eq!(pz.max_bytes, 0);
        assert!(pz.marks.iter().filter(|m| m.series == Series::Wire).all(|m| m.row == 0));
    }

    #[test]
    fn cursor_column_drawn_where_empty() {
        let s = [wire(0, 10)]; // occupies col 0
        let p = project(&s, &[], SampleDir::OriginToResponder, (Nanos(0), Nanos(100)), Nanos(50), 11, 5).unwrap();
        assert_eq!(p.cursor_col, 5);
        assert_eq!(mark_at(&p, 5, 0).map(|m| m.glyph), Some(CURSOR_GLYPH));
    }

    #[test]
    fn only_focus_direction_is_plotted() {
        let mut r2o = wire(0, 10);
        r2o.dir = SampleDir::ResponderToOrigin;
        let s = [wire(0, 10), r2o];
        let p = project(&s, &[], SampleDir::OriginToResponder, (Nanos(0), Nanos(10)), Nanos(10), 10, 5).unwrap();
        assert_eq!(p.marks.iter().filter(|m| m.series == Series::Wire).count(), 1);
    }

    #[test]
    fn overlay_is_distinct_and_unclamped_above_wire_max() {
        // wire peaks at 40; a cwnd overlay at 80 must expand max_bytes and sit above the wire.
        let w = [wire(0, 0), wire(100, 40)];
        let o = [InFlightSample { t: Nanos(100), dir: SampleDir::OriginToResponder, bytes: 80 }];
        let p = project(&w, &o, SampleDir::OriginToResponder, (Nanos(0), Nanos(100)), Nanos(100), 10, 11).unwrap();
        assert_eq!(p.max_bytes, 80, "axis expands to include the overlay");
        let cwnd: Vec<Mark> = p.marks.iter().filter(|m| m.series == Series::Cwnd).copied().collect();
        assert_eq!(cwnd.len(), 1);
        assert_eq!(cwnd[0].glyph, CWND_GLYPH);
        // row(80) over max 80, H=11 -> (80*10)/80 = 10 (top). wire 40 -> row 5. Overlay above wire.
        assert!(cwnd[0].row > 5, "cwnd overlay sits above the wire, not clamped onto it");
        // Empty overlay -> no Cwnd marks.
        let pe = project(&w, &[], SampleDir::OriginToResponder, (Nanos(0), Nanos(100)), Nanos(100), 10, 11).unwrap();
        assert!(pe.marks.iter().all(|m| m.series == Series::Wire));
        assert_eq!(pe.max_bytes, 40);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Add `pub mod inflight;` to `lib.rs` first (so the module compiles into the test target), then:
Run: `cargo test -p tcpvisr-tui inflight:: 2>&1 | tail -25`
Expected: compile error — `project`/types not defined.

- [ ] **Step 3: Implement** — module body of `inflight.rs` (above the `mod tests`):

```rust
//! Pure In-flight projection (ADR-0012 §2): maps a connection's `InFlightSample` wire series (+
//! an optional cwnd overlay) + cursor + plot-rectangle cells to a grid of glyph marks. No
//! terminal, no I/O, no serial arithmetic (the engine already produced each `bytes` value).

use tcpvisr_core::{Nanos, SampleDir};
use tcpvisr_engine::InFlightSample;

/// Minimum inner plot rectangle; below this the detail pane shows "widen terminal".
pub const MIN_W: u16 = 8;
pub const MIN_H: u16 = 3;

pub const WIRE_GLYPH: char = '#';
pub const CWND_GLYPH: char = '+';
pub const CURSOR_GLYPH: char = '\u{250a}';

/// Which series a mark belongs to: the wire-estimated in-flight, or the (M12) kernel cwnd overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Series {
    Wire,
    Cwnd,
}

/// One plotted cell. `row` is bottom-origin (0 = 0 bytes); a top-down renderer draws it at
/// screen line `height - 1 - row`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mark {
    pub col: u16,
    pub row: u16,
    pub glyph: char,
    pub series: Series,
}

/// A resolved In-flight plot over a `width x height` cell rectangle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InFlightPlot {
    pub width: u16,
    pub height: u16,
    pub max_bytes: u64,
    pub x_span: (Nanos, Nanos),
    pub cursor_col: u16,
    pub marks: Vec<Mark>,
}

/// Maps a nanosecond time to a column in `0..width`, clamped; zero-width span -> column 0.
fn col_of(t: u64, t0: u64, span_t: u64, width: u16) -> u16 {
    if span_t == 0 {
        return 0;
    }
    let c = u128::from(t.saturating_sub(t0)) * u128::from(width - 1) / u128::from(span_t);
    u16::try_from(c).unwrap_or(width - 1).min(width - 1)
}

/// Maps a byte count to a bottom-origin row in `0..height`, clamped; `max_bytes == 0` -> row 0.
fn row_of(b: u64, max_bytes: u64, height: u16) -> u16 {
    if max_bytes == 0 {
        return 0;
    }
    let r = u128::from(b) * u128::from(height - 1) / u128::from(max_bytes);
    u16::try_from(r).unwrap_or(height - 1).min(height - 1)
}

/// Projects the focus-direction `wire` series (and optional `overlay`) onto a `width x height`
/// cell grid. Axes are fixed to `x_span` and `[0, max_bytes]` over both series' focus-direction
/// samples. Only samples with `t <= cursor` are revealed; per (column, series) the tallest
/// revealed mark is kept. Returns `None` if the rectangle is below the minimum.
#[must_use]
pub fn project(
    wire: &[InFlightSample],
    overlay: &[InFlightSample],
    focus: SampleDir,
    x_span: (Nanos, Nanos),
    cursor: Nanos,
    width: u16,
    height: u16,
) -> Option<InFlightPlot> {
    if width < MIN_W || height < MIN_H {
        return None;
    }
    let (t0, t1) = (x_span.0.0, x_span.1.0);
    let span_t = t1.saturating_sub(t0);
    let max_bytes = wire
        .iter()
        .chain(overlay.iter())
        .filter(|s| s.dir == focus)
        .map(|s| s.bytes)
        .max()
        .unwrap_or(0);

    let cells = usize::from(width) * usize::from(height);
    // Per cell: the winning (row-priority, glyph, series). We bucket by max row per series, so
    // store the current best row for each (col, series) via two grids keyed by series.
    let idx = |col: u16, row: u16| usize::from(row) * usize::from(width) + usize::from(col);
    let mut grid: Vec<Option<Mark>> = vec![None; cells];

    let mut place = |series: Series, glyph: char, samples: &[InFlightSample], grid: &mut Vec<Option<Mark>>| {
        // Track the tallest revealed row per column for this series.
        let mut peak: Vec<Option<u16>> = vec![None; usize::from(width)];
        for s in samples.iter().filter(|s| s.dir == focus && s.t.0 <= cursor.0) {
            let col = col_of(s.t.0, t0, span_t, width);
            let row = row_of(s.bytes, max_bytes, height);
            let e = &mut peak[usize::from(col)];
            if e.map_or(true, |r| row > r) {
                *e = Some(row);
            }
        }
        for (col, maybe_row) in peak.into_iter().enumerate() {
            if let Some(row) = maybe_row {
                let col = u16::try_from(col).unwrap_or(width - 1);
                grid[idx(col, row)] = Some(Mark { col, row, glyph, series });
            }
        }
    };
    place(Series::Wire, WIRE_GLYPH, wire, &mut grid);
    place(Series::Cwnd, CWND_GLYPH, overlay, &mut grid);

    let ct = cursor.0.clamp(t0, t1);
    let cursor_col = col_of(ct, t0, span_t, width);
    for row in 0..height {
        let cell = &mut grid[idx(cursor_col, row)];
        if cell.is_none() {
            *cell = Some(Mark { col: cursor_col, row, glyph: CURSOR_GLYPH, series: Series::Wire });
        }
    }

    let mut marks = Vec::new();
    for row in 0..height {
        for col in 0..width {
            if let Some(m) = grid[idx(col, row)] {
                marks.push(m);
            }
        }
    }
    Some(InFlightPlot { width, height, max_bytes, x_span, cursor_col, marks })
}
```

> Note on clippy: the closure captures `width`/`t0`/`span_t`/`max_bytes`/`cursor`/`focus`/`idx` by reference. If `clippy::too_many_arguments` or borrow issues arise, inline `place` as two explicit loops instead of a closure. Keep each function ≤100 lines / complexity ≤8; if `project` trips the complexity lint, extract the per-series placement into a free `fn place_series(...)`.

Then add to `lib.rs`:
```rust
pub mod inflight;
// ...
pub use inflight::{InFlightPlot, Series};
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p tcpvisr-tui inflight:: 2>&1 | tail -25`
Expected: PASS.

- [ ] **Step 5: Guardrails + commit**

```bash
cargo fmt --all --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test --workspace
git add crates/tcpvisr-tui/src/inflight.rs crates/tcpvisr-tui/src/lib.rs
git commit -m "feat(tui): add the pure in-flight sawtooth projection"
```

---

### Task 6: `App` gains the detail-view switcher + in-flight focus series

**Files:**
- Modify: `crates/tcpvisr-tui/src/app.rs` (`DetailView`, field, methods, `FocusConn.inflight`)
- Modify: `crates/tcpvisr-tui/src/lib.rs` (re-export `DetailView`)
- Test: inline `mod tests` in `app.rs`

**Interfaces:**
- Consumes: `tcpvisr_engine::InFlightSample`, `Timeline::inflight_series` (Task 3).
- Produces:
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum DetailView { TimeSequence, InFlight }

  impl App {
      pub fn detail_view(&self) -> DetailView;
      pub fn cycle_detail_view(&mut self);
  }
  // FocusConn gains: pub inflight: &'a [InFlightSample],
  ```

- [ ] **Step 1: Write the failing tests** — add to `app.rs` `mod tests`:

```rust
#[test]
fn tab_cycles_detail_view() {
    let mut app = app_of(vec![entry(ep(1, 1), ep(2, 22), 0, 0, 0)]);
    assert_eq!(app.detail_view(), DetailView::TimeSequence);
    app.cycle_detail_view();
    assert_eq!(app.detail_view(), DetailView::InFlight);
    app.cycle_detail_view();
    assert_eq!(app.detail_view(), DetailView::TimeSequence);
}

#[test]
fn focus_exposes_inflight_series() {
    let c = full_conn(ep(1, 1), ep(2, 443), 0, 0, 10, ConnState::Established);
    let mut c2 = c;
    c2.bytes_o2r = 100;
    let inflight = vec![InFlightSample { t: Nanos(0), dir: SampleDir::OriginToResponder, bytes: 100 }];
    let tl = Timeline::with_seq(vec![(c2, vec![ss(0, ConnState::Established, 100, 0)], vec![], inflight)]);
    let app = App::new(tl, "t".to_string());
    let f = app.focus().expect("selected");
    assert_eq!(f.inflight.len(), 1);
    assert_eq!(f.inflight[0].bytes, 100);
}
```

(Add `use tcpvisr_engine::InFlightSample;` and `use tcpvisr_core::SampleDir;` to the test module if not already imported.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p tcpvisr-tui tab_cycles_detail_view focus_exposes_inflight 2>&1 | tail -25`
Expected: compile error — `DetailView` / `cycle_detail_view` / `FocusConn.inflight` missing.

- [ ] **Step 3: Implement**

- Add the enum near `Mode`:
  ```rust
  /// Which detail graph the pane shows when open (`Tab` cycles it).
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum DetailView {
      TimeSequence,
      InFlight,
  }
  ```
- Extend `FocusConn` with `pub inflight: &'a [InFlightSample],` and import `InFlightSample` in `app.rs`'s `use tcpvisr_engine::{...}` line.
- Add `detail_view: DetailView` to `App`; initialise `detail_view: DetailView::TimeSequence` in `App::new`.
- Add methods:
  ```rust
  /// The detail graph shown when the pane is open.
  #[must_use]
  pub fn detail_view(&self) -> DetailView {
      self.detail_view
  }

  /// Advances the detail view (wrapping): Time/Sequence -> In-flight -> Time/Sequence.
  pub fn cycle_detail_view(&mut self) {
      self.detail_view = match self.detail_view {
          DetailView::TimeSequence => DetailView::InFlight,
          DetailView::InFlight => DetailView::TimeSequence,
      };
  }
  ```
- In `focus()`, populate the new field: `inflight: self.timeline.inflight_series(id),`.
- In `lib.rs`, add `DetailView` to the `app` re-export: `pub use app::{App, ConnRow, DetailView, Mode, Outcome, SortDir, SortField};`.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p tcpvisr-tui 2>&1 | tail -25`
Expected: PASS.

- [ ] **Step 5: Guardrails + commit**

```bash
cargo fmt --all --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test --workspace
git add crates/tcpvisr-tui/src/app.rs crates/tcpvisr-tui/src/lib.rs
git commit -m "feat(tui): add the Tab detail-view switcher and in-flight focus series"
```

---

### Task 7: `Tab` key binding

**Files:**
- Modify: `crates/tcpvisr-tui/src/keys.rs` (nav-mode `Tab`)
- Test: inline `mod tests` in `keys.rs`

**Interfaces:**
- Consumes: `App::cycle_detail_view`, `App::detail_view` (Task 6), `DetailView`.

- [ ] **Step 1: Write the failing tests** — add to `keys.rs` `mod tests` (import `DetailView`):

```rust
#[test]
fn tab_cycles_view_in_nav_mode() {
    use crate::app::DetailView;
    let mut a = app();
    assert_eq!(a.detail_view(), DetailView::TimeSequence);
    handle_key(&mut a, key(KeyCode::Tab));
    assert_eq!(a.detail_view(), DetailView::InFlight);
}

#[test]
fn tab_does_not_cycle_view_in_filter_mode() {
    use crate::app::DetailView;
    let mut a = app();
    handle_key(&mut a, press('/')); // enter filter
    handle_key(&mut a, key(KeyCode::Tab));
    assert_eq!(a.detail_view(), DetailView::TimeSequence, "Tab inert for view-switching in filter mode");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p tcpvisr-tui tab_cycles_view tab_does_not_cycle 2>&1 | tail -20`
Expected: FAIL — `Tab` currently hits the `_ => {}` arm, view stays `TimeSequence` in the first test.

- [ ] **Step 3: Implement** — in `handle_nav`, add before the `_ => {}` arm:

```rust
        KeyCode::Tab => app.cycle_detail_view(),
```

(Do NOT touch `handle_filter`: `KeyCode::Tab` is not a `Char`, so it already falls through the filter's `_ => {}` and is inert — the second test verifies this.)

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p tcpvisr-tui 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Guardrails + commit**

```bash
cargo fmt --all --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test --workspace
git add crates/tcpvisr-tui/src/keys.rs
git commit -m "feat(tui): bind Tab to cycle the detail view in nav mode"
```

---

### Task 8: Render the In-flight view + footer hint

**Files:**
- Modify: `crates/tcpvisr-tui/src/render.rs` (dispatch on `detail_view()`; `render_inflight`; footer)
- Test: inline `mod tests` in `render.rs`

**Interfaces:**
- Consumes: `App::detail_view`, `DetailView`, `FocusConn.inflight` (Task 6), `crate::inflight::{self, Mark, Series, InFlightPlot, project}` (Task 5), the existing `fmt_seconds`/`fmt_seq`/`GUTTER` helpers in `render.rs`.

**Design note:** factor the current `render_detail` body so the shared block/title/guard stays, then dispatch: `TimeSequence` → the existing seq drawing (unchanged), `InFlight` → `render_inflight`. `render_inflight` mirrors the seq path: carve legend row (top), time-label row (bottom), Y gutter (left); `inflight::project(focus.inflight, &[], focus.focus_dir, focus.x_span, app.cursor(), plot_w, plot_h)`; draw marks (wire `Color::Reset`/default, cwnd `Color::Cyan`), then Y byte labels (`fmt_seq(max_bytes as i64)` at top, `0` at bottom) and X time labels (reuse the seq path's X-label code). The overlay arg is `&[]` on replay.

- [ ] **Step 1: Write the failing tests** — add to `render.rs` `mod tests`:

```rust
#[test]
fn inflight_view_open_shows_graph() {
    let c = conn_span(ep(1, 5), ep(2, 443), 0, 1_000, ConnState::Established);
    let mut c2 = c;
    c2.bytes_o2r = 100;
    let inflight = vec![
        tcpvisr_engine::InFlightSample { t: Nanos(0), dir: tcpvisr_core::SampleDir::OriginToResponder, bytes: 50 },
        tcpvisr_engine::InFlightSample { t: Nanos(1_000), dir: tcpvisr_core::SampleDir::OriginToResponder, bytes: 100 },
    ];
    let tl = Timeline::with_seq(vec![(c2, vec![ss(0, 100, 0)], vec![], inflight)]);
    let mut app = App::new(tl, "t".to_string());
    app.open_detail();
    app.cycle_detail_view(); // -> InFlight
    let s = draw(&app, 120, 14);
    assert!(s.contains("DETAIL"), "detail title: {s}");
    assert!(s.contains("In-flight"), "in-flight legend: {s}");
    assert!(s.contains('#'), "at least one wire glyph: {s}");
    assert!(s.contains("0.000s"), "an axis time label: {s}");
}

#[test]
fn footer_advertises_view_switch() {
    let app = app_span(1_000_000_000);
    let s = draw(&app, 120, 8);
    assert!(s.contains("view"), "footer view-switch hint: {s}");
}

#[test]
fn inflight_view_too_narrow_shows_widen_message() {
    let c = conn_span(ep(1, 5), ep(2, 443), 0, 1_000, ConnState::Established);
    let mut c2 = c;
    c2.bytes_o2r = 100;
    let inflight = vec![tcpvisr_engine::InFlightSample { t: Nanos(0), dir: tcpvisr_core::SampleDir::OriginToResponder, bytes: 100 }];
    let tl = Timeline::with_seq(vec![(c2, vec![ss(0, 100, 0)], vec![], inflight)]);
    let mut app = App::new(tl, "t".to_string());
    app.open_detail();
    app.cycle_detail_view();
    let s = draw(&app, 34, 12); // right pane inner plot < MIN_W after the gutter
    assert!(s.contains("widen terminal"), "narrow in-flight guidance: {s}");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p tcpvisr-tui inflight_view footer_advertises_view 2>&1 | tail -25`
Expected: FAIL — the in-flight branch is not drawn (no `In-flight` string / no `#`), footer lacks `view`.

- [ ] **Step 3: Implement**

- Refactor `render_detail`: keep the `let Some(focus) = app.focus() else {...}` guard, the block/title, and the `inner.height < 3 || inner.width <= GUTTER` widen guard. Then:
  ```rust
  match app.detail_view() {
      DetailView::TimeSequence => render_seq_body(frame, app, inner, &focus),
      DetailView::InFlight => render_inflight_body(frame, app, inner, &focus),
  }
  ```
  Move the existing seq projection + `draw_legend`/`draw_plot`/`draw_axes` calls into `render_seq_body` (behavior unchanged, so the M6 render tests keep passing).
- Add `render_inflight_body` mirroring the seq body:
  ```rust
  fn render_inflight_body(frame: &mut Frame, app: &App, inner: Rect, focus: &crate::app::FocusConn<'_>) {
      let plot_w = inner.width - GUTTER;
      let plot_h = inner.height - 2; // legend + time labels
      let Some(plot) = crate::inflight::project(
          focus.inflight, &[], focus.focus_dir, focus.x_span, app.cursor(), plot_w, plot_h,
      ) else {
          frame.render_widget(Paragraph::new("widen terminal to view graph"), inner);
          return;
      };
      draw_inflight_legend(frame, inner);
      draw_inflight_plot(frame, inner, GUTTER, &plot);
      draw_inflight_axes(frame, inner, GUTTER, &plot);
  }
  ```
  With:
  - `draw_inflight_legend`: `Paragraph::new(format!("In-flight   {} wire", crate::inflight::WIRE_GLYPH))` on the top row (same Rect math as `draw_legend`).
  - `draw_inflight_plot`: like `draw_plot` but colour by `Series`: `Series::Cwnd => Color::Cyan`, `Series::Wire => Color::Reset`. Iterate `plot.marks` using `crate::inflight::Mark`.
  - `draw_inflight_axes`: like `draw_axes` but the Y-top label is `fmt_seq(i64::try_from(plot.max_bytes).unwrap_or(i64::MAX))`; Y-bottom `0`; X labels identical (reuse `plot.x_span` + `fmt_seconds`). (If sharing the X-label block cleanly is awkward, duplicate the ~6 lines — do not refactor `draw_axes`, whose seq label semantics differ.)
- Footer: add `⇥ view` to the nav hint string in `render_footer`, e.g. after `esc close`:
  ```
  "space play/pause  ←→ seek  +/- speed  ,/. step  ⏎ open  esc close  ⇥ view  / filter  s sort:{}{arrow}  q quit"
  ```

- [ ] **Step 4: Run to verify pass (and the M6 tests stay green)**

Run: `cargo test -p tcpvisr-tui 2>&1 | tail -25`
Expected: PASS — the new in-flight render tests AND every existing M6 render test (`detail_open_shows_title_legend_and_a_mark`, etc.).

- [ ] **Step 5: Guardrails + commit**

```bash
cargo fmt --all --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test --workspace
git add crates/tcpvisr-tui/src/render.rs
git commit -m "feat(tui): render the In-flight detail view and view-switch hint"
```

---

### Task 9: CLI wiring + bin integration test

**Files:**
- Modify: `crates/tcp-visr/src/main.rs` (`run_replay` sets `collect_inflight_timeline = true`; `build_replay_tests`)
- Test: inline `mod build_replay_tests` in `main.rs`

**Interfaces:**
- Consumes: `EngineConfig.collect_inflight_timeline` (Task 1), `App::focus().inflight` (Task 6), the existing `build_replay_app` seam.

- [ ] **Step 1: Write the failing test** — add to `main.rs` `mod build_replay_tests`:

```rust
#[test]
fn build_replay_app_collects_inflight_series_for_the_focus_connection() {
    let cfg = EngineConfig {
        collect_state_timeline: true,
        collect_seq_timeline: true,
        collect_inflight_timeline: true,
        ..EngineConfig::default()
    };
    let app = build_replay_app(&fixture(), cfg).expect("build");
    let focus = app.focus().expect("a connection is selected at the initial cursor");
    assert!(
        !focus.inflight.is_empty(),
        "fixture with data segments yields a non-empty focus in-flight series"
    );
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p tcp-visr build_replay_app_collects_inflight 2>&1 | tail -20`
Expected: FAIL — with the flag off in the default `EngineConfig` the series is empty; here the test sets it, so it should pass once `focus.inflight` exists... Actually it exercises the app seam directly, so run it: if `focus.inflight` field exists (Task 6) it already passes. If so, this test only *documents* the seam. To make it a genuine RED first, instead assert against `run_replay`'s config path — see Step 3 note. Expected here: the test passes only after Step 3 wires the flag; before Step 3 it still passes because the test sets `cfg` itself. So treat Step 1's test as the guard and add the RED via the config assertion below.

Add this second test which is RED until Step 3:

```rust
#[test]
fn run_replay_config_enables_inflight_collection() {
    // The replay path must turn the flag on; guard against a regression that drops it.
    // We can't run the TUI here, so assert the config the replay path builds.
    let cfg = replay_engine_config(10_000_000);
    assert!(cfg.collect_inflight_timeline, "replay must collect the in-flight timeline");
    assert!(cfg.collect_seq_timeline && cfg.collect_state_timeline, "M5/M6 series still on");
}
```

- [ ] **Step 3: Implement** — extract the replay config into a small helper so it is testable, and set the flag:

In `main.rs`, add near `run_replay`:
```rust
/// The `EngineConfig` the replay path uses: all three replay timelines on (state, seq,
/// in-flight), plus the sample ceiling.
fn replay_engine_config(max_samples: usize) -> EngineConfig {
    EngineConfig {
        collect_state_timeline: true,
        collect_seq_timeline: true,
        collect_inflight_timeline: true,
        max_samples,
        ..EngineConfig::default()
    }
}
```
and change `run_replay` to use it:
```rust
    let cfg = replay_engine_config(max_samples);
    let app = build_replay_app(file, cfg)?;
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p tcp-visr 2>&1 | tail -20`
Expected: PASS (both new tests + the existing `build_replay_tests`).

- [ ] **Step 5: Guardrails + commit**

```bash
cargo fmt --all --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test --workspace
git add crates/tcp-visr/src/main.rs
git commit -m "feat(cli): collect the in-flight timeline on the replay path"
```

---

### Task 10: Docs — mark M7 implemented

**Files:**
- Modify: `docs/design/tcp-visr-design.md` (§10 roadmap / status, wherever M6 is marked implemented)
- Modify: `CLAUDE.md` (the "Current state" paragraph: M0–M6 → M0–M7; move in-flight out of the "not built yet" list; note the `Tab` switcher is partial)

**Interfaces:** none (documentation only; no guardrail beyond fmt/clippy skipping non-Rust).

- [ ] **Step 1: Update the design roadmap** — find how M6 is marked implemented (e.g. a status note near §10.M6 or a §-status line) and mark M7 the same way. If there is a "milestones M0–M6 are implemented" style line, extend it to M7.

- [ ] **Step 2: Update `CLAUDE.md`** — in "Current state": change "milestones M0–M6 are implemented" → "M0–M7"; add a sentence that `Tab` switches the detail pane between Time/Sequence and In-flight (wire bytes-outstanding sawtooth with a cwnd overlay seam, empty on replay); remove "in-flight" from the "remaining detail views … not built yet" list and note the `Tab` switcher is introduced (RTT/throughput still pending, M8–M9).

- [ ] **Step 3: Commit**

```bash
git add docs/design/tcp-visr-design.md CLAUDE.md
git commit -m "docs: mark M7 (In-flight detail) implemented"
```

---

## Self-Review

**Spec coverage** (each numbered success criterion → task):
- 1 (rise + ack-time drain) → Task 4 `inflight_rises_on_send_and_drains_at_ack_time`
- 2 (u32 wrap) → Task 4 `inflight_is_serial_correct_across_u32_wrap`
- 3 (carried through timeline, sorted, unknown empty) → Task 3
- 4 (ceiling) → Task 4 `inflight_collection_counts_against_ceiling`
- 5 (default-off / orthogonal) → Task 1 (`inflight_timeline_defaults_off`) + Task 4 `inflight_off_by_default_is_empty`
- 6 (point placement) → Task 5 `corners_place_at_exact_indices`
- 7 (fixed axes) → Task 5 `axes_fixed_regardless_of_cursor`
- 8 (reveal to T) → Task 5 `reveal_hides_marks_after_cursor`
- 9 (numeric-max bucketing over revealed) → Task 5 `numeric_max_bucketing_over_revealed_only`
- 10 (degenerate spans) → Task 5 `degenerate_spans_do_not_divide_by_zero`
- 11 (cursor column) → Task 5 `cursor_column_drawn_where_empty`
- 12 (narrow-terminal guard) → Task 5 `too_small_viewport_yields_none` + Task 8 `inflight_view_too_narrow_shows_widen_message`
- 13 (overlay distinct + unclamped, empty→none) → Task 5 `overlay_is_distinct_and_unclamped_above_wire_max`
- 14 (Tab cycles; Enter/Esc unchanged; filter inert) → Task 6 `tab_cycles_detail_view` + Task 7 `tab_cycles_view_in_nav_mode`/`tab_does_not_cycle_view_in_filter_mode` (Enter/Esc unchanged already covered by existing M6 keys tests)
- 15 (detail follows selection) → Task 6 `focus_exposes_inflight_series` (+ existing M6 `detail_follows_selection` which exercises `focus()` across `move_down`)
- 16 (closed byte-identical to M6/M5) → existing M6 render tests remain green (Task 8 Step 4 gate) — no code path changes when `detail_open == false`
- 17 (In-flight open shows graph) → Task 8 `inflight_view_open_shows_graph`
- 18 (CLI wiring + ceiling) → Task 9 + the existing `sample_ceiling_is_fatal` test (unchanged, still exercises the ceiling on the replay build)

Gap check: criterion 15's "focus() changes to the newly selected connection's in-flight series" is only partially covered by `focus_exposes_inflight_series` (single connection). The existing M6 `detail_follows_selection` test already asserts `focus()` follows `move_down`; since `focus()` now also fills `inflight` from the same `id`, that path is covered. If a reviewer wants it explicit, add a two-connection `move_down` test asserting `focus().inflight` changes — optional, low value.

**Placeholder scan:** no TBD/TODO; every code step shows the code; test bodies are complete.

**Type consistency:** `InFlightSample { t, dir, bytes }` used identically in Tasks 1/3/4/5/6/8/9; `project(wire, overlay, focus, x_span, cursor, width, height) -> Option<InFlightPlot>` signature matches every call site (Task 8 passes `&[]` for overlay); `DetailView { TimeSequence, InFlight }` and `cycle_detail_view` consistent across Tasks 6/7/8; `Series { Wire, Cwnd }` and `Mark { col, row, glyph, series }` consistent Tasks 5/8; `replay_engine_config(max_samples)` defined and used in Task 9.

## Notes for the implementer

- **Run the focused test first, then the workspace.** The per-task `cargo test -p <crate> <filter>` is for the fast red/green loop; the Step-5 `cargo test --workspace` is the regression gate before each commit.
- **The M6 seq view must stay byte-identical.** Tasks 3, 6, 8 touch shared files (`timeline.rs`, `app.rs`, `render.rs`); after each, confirm the existing M6/M5 tests pass unchanged. Do not alter M6 glyphs, the seq projection, or the master-list rendering.
- **Clippy will bite on the projection.** If `project` trips `too_many_lines`/complexity or the closure trips a borrow lint, extract `place_series` as a free function (spec §4 allows it; keep it pure). Never silence a lint with a bare `#[allow]` (denied by `allow_attributes`); fix the code or use a justified file-level `#![allow(...)]` only in test modules.
- **Do not un-gate anything.** `cargo test -p tcpvisr-ingest --features live` stays a separate CI step; you do not need `live` for M7. Run the full gate (incl. `cargo deny check`) once before the first push.
