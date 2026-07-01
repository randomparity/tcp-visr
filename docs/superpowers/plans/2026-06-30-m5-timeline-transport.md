# M5 — Timeline + Transport Controls Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the replay master list seekable in time — play/pause, 0.1–10× speed, seek, and step-by-event — with each row resolving its state and byte counts as of the cursor time `T` via a cross-connection interval index.

**Architecture:** A pure `Timeline` in `tcpvisr-engine` owns a per-segment `StateSample` snapshot series per connection plus an interval index and event-time index; a pure `Transport` in `tcpvisr-tui` owns the cursor/speed/play state and advances on an injected wall-clock delta; `App` resolves the active rows at the cursor each frame. Spec: [m5-timeline-transport.md](../specs/m5-timeline-transport.md). ADR: [ADR-0010](../../adr/0010-timeline-transport-as-of-t.md).

**Tech Stack:** Rust 1.88.0, ratatui 0.30.2, `tcpvisr-core`/`-engine`/`-tui` workspace crates.

## Global Constraints

- Toolchain pinned to Rust 1.88.0; dependency versions pinned exactly (`=x.y.z`).
- Zero warnings. Clippy denies `unwrap_used`/`panic`/`print_stdout`/`allow_attributes` in non-test code; scope test relaxations with a file-level `#![allow(...)]` inside `#[cfg(test)]` modules.
- The engine (`tcpvisr-engine`) is pure: no I/O, no clock reads (ADR-0002). Time enters as data.
- ≤100 lines/function, cyclomatic complexity ≤8, ≤5 positional params, 100-char lines, absolute imports only, Google-style docstrings on non-trivial public APIs.
- Guardrails before every commit: `cargo fmt --all --check` · `cargo clippy --all-targets --all-features -- -D warnings` · `cargo test --workspace` · `cargo test -p tcpvisr-ingest --features live` · `cargo deny check`. For a task that touches only one crate, run that crate's tests during the loop, but run the full set before committing.
- Conventional Commits, imperative, ≤72-char subject, trailer `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.

---

### Task 1: Engine — `StateSample` + `Timeline`

**Files:**
- Create: `crates/tcpvisr-engine/src/timeline.rs`
- Modify: `crates/tcpvisr-engine/src/lib.rs`

**Interfaces:**
- Consumes: `tcpvisr_core::Nanos`; `crate::conn::{ConnId, Connection}`; `crate::state::ConnState`.
- Produces:
  - `pub struct StateSample { pub t: Nanos, pub state: ConnState, pub bytes_o2r: u64, pub bytes_r2o: u64 }` (Copy)
  - `pub struct AsOf { pub id: ConnId, pub state: ConnState, pub bytes_o2r: u64, pub bytes_r2o: u64 }` (Copy)
  - `pub struct Timeline` with:
    - `pub fn new(conns: Vec<(Connection, Vec<StateSample>)>) -> Self`
    - `pub fn bounds(&self) -> (Nanos, Nanos)`
    - `pub fn connection_count(&self) -> usize`
    - `pub fn connections(&self) -> impl Iterator<Item = &Connection>`
    - `pub fn active_at(&self, t: Nanos) -> Vec<ConnId>`
    - `pub fn resolve_at(&self, t: Nanos) -> Vec<AsOf>`
    - `pub fn next_event(&self, t: Nanos) -> Option<Nanos>`
    - `pub fn prev_event(&self, t: Nanos) -> Option<Nanos>`

- [ ] **Step 1: Write the module skeleton + failing tests**

Create `crates/tcpvisr-engine/src/timeline.rs`:

```rust
//! The seekable replay timeline (design §5, ADR-0004, ADR-0010). Pure: no I/O, no clock.
//!
//! Resolves, for any cursor time `T`, the set of connections active at `T` and each one's
//! `(state, bytes)` as of `T`, via a cross-connection interval index over
//! `[opened_at, effective_end]` plus a per-connection binary search.

use tcpvisr_core::Nanos;

use crate::conn::{ConnId, Connection};
use crate::state::ConnState;

/// A per-segment lifecycle snapshot: the connection's `(state, cumulative bytes)` at time `t`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StateSample {
    pub t: Nanos,
    pub state: ConnState,
    pub bytes_o2r: u64,
    pub bytes_r2o: u64,
}

/// A connection's resolved state as of a cursor time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AsOf {
    pub id: ConnId,
    pub state: ConnState,
    pub bytes_o2r: u64,
    pub bytes_r2o: u64,
}

/// One connection's timeline entry: its `Connection` view, its `t`-sorted snapshot series,
/// and the right bound of its active interval (`last_at` if closed, else the capture end).
struct Entry {
    conn: Connection,
    samples: Vec<StateSample>,
    effective_end: Nanos,
}

/// The replay timeline over all connections.
pub struct Timeline {
    entries: Vec<Entry>,
    start: Nanos,
    end: Nanos,
    event_times: Vec<Nanos>,
}

impl Timeline {
    /// Builds the timeline. Each connection's `StateSample` series is sorted by `t` (stable)
    /// because capture timestamps are not guaranteed monotonic (design §14); `start` is the
    /// minimum sample time and `end` is the maximum `last_at`. A connection whose final state
    /// is `Closed`/`Reset` bounds its interval at `last_at`; any still-open connection extends
    /// to `end`.
    #[must_use]
    pub fn new(conns: Vec<(Connection, Vec<StateSample>)>) -> Self {
        let end = conns
            .iter()
            .map(|(c, _)| c.last_at)
            .max()
            .unwrap_or(Nanos(0));
        let start = conns
            .iter()
            .flat_map(|(_, s)| s.iter().map(|x| x.t))
            .min()
            .unwrap_or(Nanos(0));
        let mut entries: Vec<Entry> = Vec::with_capacity(conns.len());
        let mut event_times: Vec<Nanos> = Vec::new();
        for (conn, mut samples) in conns {
            samples.sort_by_key(|s| s.t);
            for s in &samples {
                event_times.push(s.t);
            }
            let closed = matches!(conn.state, ConnState::Closed | ConnState::Reset);
            let effective_end = if closed { conn.last_at } else { end };
            entries.push(Entry {
                conn,
                samples,
                effective_end,
            });
        }
        event_times.sort_unstable();
        event_times.dedup();
        Self {
            entries,
            start,
            end,
            event_times,
        }
    }

    /// The `[start, end]` cursor domain.
    #[must_use]
    pub fn bounds(&self) -> (Nanos, Nanos) {
        (self.start, self.end)
    }

    /// The number of tracked connections.
    #[must_use]
    pub fn connection_count(&self) -> usize {
        self.entries.len()
    }

    /// The tracked connections (static views), in construction order.
    pub fn connections(&self) -> impl Iterator<Item = &Connection> {
        self.entries.iter().map(|e| &e.conn)
    }

    /// The ids of connections active at `t` (`opened_at <= t <= effective_end`).
    #[must_use]
    pub fn active_at(&self, t: Nanos) -> Vec<ConnId> {
        self.active_indices(t).map(|i| self.entries[i].conn.id).collect()
    }

    /// Each active connection's `(state, bytes)` as of `t` (last sample with `sample.t <= t`).
    #[must_use]
    pub fn resolve_at(&self, t: Nanos) -> Vec<AsOf> {
        self.active_indices(t)
            .filter_map(|i| {
                let e = &self.entries[i];
                let k = e.samples.partition_point(|s| s.t.0 <= t.0);
                let s = e.samples.get(k.checked_sub(1)?)?;
                Some(AsOf {
                    id: e.conn.id,
                    state: s.state,
                    bytes_o2r: s.bytes_o2r,
                    bytes_r2o: s.bytes_r2o,
                })
            })
            .collect()
    }

    /// The nearest event time strictly after `t`, or `None` past the last event.
    #[must_use]
    pub fn next_event(&self, t: Nanos) -> Option<Nanos> {
        let k = self.event_times.partition_point(|x| x.0 <= t.0);
        self.event_times.get(k).copied()
    }

    /// The nearest event time strictly before `t`, or `None` before the first event.
    #[must_use]
    pub fn prev_event(&self, t: Nanos) -> Option<Nanos> {
        let k = self.event_times.partition_point(|x| x.0 < t.0);
        self.event_times.get(k.checked_sub(1)?).copied()
    }

    fn active_indices(&self, t: Nanos) -> impl Iterator<Item = usize> + '_ {
        (0..self.entries.len()).filter(move |&i| {
            let e = &self.entries[i];
            e.conn.opened_at.0 <= t.0 && t.0 <= e.effective_end.0
        })
    }
}
```

Add tests at the bottom of `timeline.rs` (criteria 1–4, 15). Use a `conn(...)` helper that builds a `Connection` with the given `opened_at`/`last_at`/`state`/`instance` (peers can be fixed).

```rust
#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use core::net::{IpAddr, Ipv4Addr};
    use tcpvisr_core::Endpoint;
    use crate::conn::EndpointPair;

    fn ep(a: u8, p: u16) -> Endpoint {
        Endpoint { ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, a)), port: p }
    }

    fn conn(inst: u32, opened: u64, last: u64, state: ConnState) -> Connection {
        Connection {
            id: ConnId { pair: EndpointPair::new(ep(1, 1000 + inst as u16), ep(2, 80)), instance: inst },
            state,
            origin: ep(1, 1000 + inst as u16),
            responder: ep(2, 80),
            origin_inferred: false,
            opened_at: Nanos(opened),
            last_at: Nanos(last),
            bytes_o2r: 0,
            bytes_r2o: 0,
            segments: 1,
        }
    }

    fn ss(t: u64, state: ConnState, up: u64, down: u64) -> StateSample {
        StateSample { t: Nanos(t), state, bytes_o2r: up, bytes_r2o: down }
    }

    #[test]
    fn interval_index_membership() {
        // c0 open on [100,200] (Closed at 200); c1 still open from 150 (Established, end 300).
        let tl = Timeline::new(vec![
            (conn(0, 100, 200, ConnState::Closed), vec![ss(100, ConnState::Established, 0, 0), ss(200, ConnState::Closed, 0, 0)]),
            (conn(1, 150, 300, ConnState::Established), vec![ss(150, ConnState::Established, 0, 0), ss(300, ConnState::Established, 0, 0)]),
        ]);
        assert!(tl.active_at(Nanos(50)).is_empty());
        assert_eq!(tl.active_at(Nanos(120)).len(), 1);
        assert_eq!(tl.active_at(Nanos(180)).len(), 2);
        assert_eq!(tl.active_at(Nanos(250)).len(), 1); // c0 closed at 200, only c1
    }

    #[test]
    fn resolves_state_and_bytes_as_of_t() {
        let tl = Timeline::new(vec![(
            conn(0, 100, 300, ConnState::Established),
            vec![ss(100, ConnState::SynSent, 0, 0), ss(200, ConnState::Established, 500, 0), ss(300, ConnState::Established, 500, 1000)],
        )]);
        let at = |t: u64| tl.resolve_at(Nanos(t));
        assert_eq!(at(150)[0], AsOf { id: at(150)[0].id, state: ConnState::SynSent, bytes_o2r: 0, bytes_r2o: 0 });
        assert_eq!(at(250)[0].state, ConnState::Established);
        assert_eq!(at(250)[0].bytes_o2r, 500);
        assert_eq!(at(250)[0].bytes_r2o, 0);
        assert_eq!(at(999)[0].bytes_r2o, 1000);
    }

    #[test]
    fn closed_drops_out_still_open_stays() {
        let tl = Timeline::new(vec![
            (conn(0, 0, 100, ConnState::Closed), vec![ss(0, ConnState::Established, 0, 0), ss(100, ConnState::Closed, 0, 0)]),
            (conn(1, 0, 100, ConnState::Established), vec![ss(0, ConnState::Established, 0, 0)]),
        ]);
        // end = 100; at 100 both active; strictly after their last_at only the still-open one
        // stays (its effective_end == end == 100), the closed one is bounded at 100 too, so
        // pick a still-open case with a later end via a third connection.
        let tl = Timeline::new(vec![
            (conn(0, 0, 100, ConnState::Closed), vec![ss(0, ConnState::Established, 0, 0), ss(100, ConnState::Closed, 0, 0)]),
            (conn(1, 0, 100, ConnState::Established), vec![ss(0, ConnState::Established, 0, 0)]),
            (conn(2, 0, 300, ConnState::Established), vec![ss(0, ConnState::Established, 0, 0), ss(300, ConnState::Established, 0, 0)]),
        ]);
        let ids = |t: u64| tl.active_at(Nanos(t)).len();
        assert_eq!(ids(50), 3);
        assert_eq!(ids(200), 1, "closed@100 gone; c1 (open, end=300) stays; c2 stays -> 2");
    }

    #[test]
    fn event_stepping_dedups_and_clamps() {
        let tl = Timeline::new(vec![
            (conn(0, 0, 200, ConnState::Established), vec![ss(0, ConnState::Established, 0, 0), ss(100, ConnState::Established, 0, 0)]),
            (conn(1, 0, 200, ConnState::Established), vec![ss(100, ConnState::Established, 0, 0), ss(200, ConnState::Established, 0, 0)]), // dup @100
        ]);
        assert_eq!(tl.next_event(Nanos(0)), Some(Nanos(100)));
        assert_eq!(tl.next_event(Nanos(100)), Some(Nanos(200)));
        assert_eq!(tl.next_event(Nanos(200)), None);
        assert_eq!(tl.prev_event(Nanos(200)), Some(Nanos(100)));
        assert_eq!(tl.prev_event(Nanos(0)), None);
    }

    #[test]
    fn out_of_order_samples_are_sorted_at_construction() {
        let ordered = Timeline::new(vec![(
            conn(0, 100, 300, ConnState::Established),
            vec![ss(100, ConnState::SynSent, 0, 0), ss(200, ConnState::Established, 500, 0), ss(300, ConnState::Established, 500, 1000)],
        )]);
        let shuffled = Timeline::new(vec![(
            conn(0, 100, 300, ConnState::Established),
            vec![ss(300, ConnState::Established, 500, 1000), ss(100, ConnState::SynSent, 0, 0), ss(200, ConnState::Established, 500, 0)],
        )]);
        for t in [150u64, 250, 999] {
            assert_eq!(ordered.resolve_at(Nanos(t)), shuffled.resolve_at(Nanos(t)), "t={t}");
        }
        assert_eq!(shuffled.bounds().0, Nanos(100));
    }

    #[test]
    fn empty_timeline_has_zero_bounds() {
        let tl = Timeline::new(vec![]);
        assert_eq!(tl.bounds(), (Nanos(0), Nanos(0)));
        assert_eq!(tl.connection_count(), 0);
        assert!(tl.resolve_at(Nanos(0)).is_empty());
        assert_eq!(tl.next_event(Nanos(0)), None);
    }
}
```

- [ ] **Step 2: Wire the module into the crate**

In `crates/tcpvisr-engine/src/lib.rs` add `pub mod timeline;` (after `pub mod tracker;`) and `pub use timeline::{AsOf, StateSample, Timeline};` (after the existing `pub use` lines).

- [ ] **Step 3: Run and verify**

Run: `cargo test -p tcpvisr-engine timeline`
Expected: all `timeline::tests::*` pass.

- [ ] **Step 4: Guardrails + commit**

Run the full guardrail set. Commit:
```bash
git add crates/tcpvisr-engine/src/timeline.rs crates/tcpvisr-engine/src/lib.rs
git commit -m "feat(engine): add seekable Timeline with as-of-T resolution"
```

---

### Task 2: Engine — state-timeline collection + `into_timeline`

**Files:**
- Modify: `crates/tcpvisr-engine/src/config.rs` (add field)
- Modify: `crates/tcpvisr-engine/src/tracker.rs` (collect + `into_timeline`)

**Interfaces:**
- Consumes: `crate::timeline::{StateSample, Timeline}` (Task 1).
- Produces:
  - `EngineConfig.collect_state_timeline: bool` (default `false`)
  - `Tracker::into_timeline(self) -> Result<Timeline, MetricError>`

- [ ] **Step 1: Add the config flag + its default test**

In `config.rs`, add to `EngineConfig`:
```rust
    /// Whether the tracker records a per-segment `StateSample` timeline (M5 replay).
    pub collect_state_timeline: bool,
```
Add `collect_state_timeline: false,` to the `Default` impl. In the `defaults_match_spec` test add `assert!(!c.collect_state_timeline);`.

- [ ] **Step 2: Run the config test (expect fail then pass)**

Run: `cargo test -p tcpvisr-engine config` — it fails to compile until both the struct field and the `Default` entry are present; add both, then it passes.

- [ ] **Step 3: Write the failing tracker tests**

Add a `#[cfg(test)] mod timeline_tests` to `tracker.rs`:
```rust
#[cfg(test)]
mod timeline_tests {
    use super::test_support::{ep, seg};
    use super::*;
    use crate::state::ConnState;
    use tcpvisr_core::{Nanos, TcpFlags};

    fn cfg() -> EngineConfig {
        EngineConfig { collect_state_timeline: true, ..EngineConfig::default() }
    }

    #[test]
    fn collects_one_state_sample_per_segment() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let mut t = Tracker::new(cfg());
        t.observe(&seg(c, s, TcpFlags::ACK, 100, 1, 10, 1_000)); // 10 bytes o2r
        t.observe(&seg(s, c, TcpFlags::ACK, 1, 110, 20, 2_000)); // 20 bytes r2o
        let tl = t.into_timeline().expect("no ceiling");
        assert_eq!(tl.connection_count(), 1);
        let at = tl.resolve_at(Nanos(2_000));
        assert_eq!(at[0].bytes_o2r, 10);
        assert_eq!(at[0].bytes_r2o, 20);
        assert_eq!(at[0].state, ConnState::Established);
        // As of the first segment only, the second direction's bytes are not yet counted.
        assert_eq!(tl.resolve_at(Nanos(1_000))[0].bytes_r2o, 0);
    }

    #[test]
    fn none_flag_yields_empty_series() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let mut t = Tracker::new(EngineConfig::default()); // collect_state_timeline = false
        t.observe(&seg(c, s, TcpFlags::ACK, 100, 1, 10, 1_000));
        let tl = t.into_timeline().expect("no ceiling");
        // No samples -> the connection is never active (no sample <= t) and bounds are 0.
        assert!(tl.resolve_at(Nanos(1_000)).is_empty());
        assert_eq!(tl.bounds(), (Nanos(0), Nanos(1_000)));
    }

    #[test]
    fn ceiling_exceeded_returns_error() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let mut t = Tracker::new(EngineConfig { collect_state_timeline: true, max_samples: 1, ..EngineConfig::default() });
        t.observe(&seg(c, s, TcpFlags::ACK, 100, 1, 10, 1_000));
        t.observe(&seg(c, s, TcpFlags::ACK, 110, 1, 10, 2_000)); // 2nd sample > limit 1
        let err = t.into_timeline().expect_err("should exceed");
        assert_eq!(err, MetricError::SampleCeiling { samples: 2, limit: 1 });
    }
}
```

- [ ] **Step 4: Implement collection in the tracker**

In `ConnTrack`, add field `states: Vec<StateSample>,` and initialize it (`states: Vec::new(),`) in `create_instance`. Add the import `use crate::timeline::{StateSample, Timeline};` at the top. Add a snapshot helper to `impl ConnTrack`:
```rust
    fn snapshot(&self, t: Nanos) -> StateSample {
        StateSample {
            t,
            state: self.state,
            bytes_o2r: self.bytes_o2r,
            bytes_r2o: self.bytes_r2o,
        }
    }
```
Add a recorder to `impl Tracker` (mirrors `record_sample`, but gated only by the global flag at the call site and reusing the same ceiling counters):
```rust
    fn record_state(&mut self, idx: usize, sample: StateSample) {
        if self.overflowed {
            return;
        }
        if self.collected_samples >= self.config.max_samples {
            self.overflowed = true;
            return;
        }
        self.collected_samples += 1;
        self.conns[idx].states.push(sample);
    }
```
In `observe_segment`, on the join path, after `self.conns[idx].apply_state(seg, dir);` and before the metric block, add:
```rust
                if self.config.collect_state_timeline {
                    let s = self.conns[idx].snapshot(seg.ts);
                    self.record_state(idx, s);
                }
```
In `create_instance`, after `self.live.insert(pair, idx);` (and alongside the existing metric `record_sample`), add:
```rust
        if self.config.collect_state_timeline {
            let s = self.conns[idx].snapshot(seg.ts);
            self.record_state(idx, s);
        }
```
Add the public method:
```rust
    /// All tracked instances with their per-segment state timeline, built into a [`Timeline`].
    ///
    /// # Errors
    /// Returns [`MetricError::SampleCeiling`] if collection hit `max_samples`.
    pub fn into_timeline(self) -> Result<Timeline, MetricError> {
        if self.overflowed {
            return Err(MetricError::SampleCeiling {
                samples: self.collected_samples + 1,
                limit: self.config.max_samples,
            });
        }
        let pairs: Vec<(Connection, Vec<StateSample>)> = self
            .conns
            .iter()
            .map(|c| (c.view(), c.states.clone()))
            .collect();
        Ok(Timeline::new(pairs))
    }
```

- [ ] **Step 5: Run and verify**

Run: `cargo test -p tcpvisr-engine` — the new `timeline_tests` pass and no existing test regresses (existing tests use `collect_state_timeline = false` via `Default`).

- [ ] **Step 6: Guardrails + commit**

```bash
git add crates/tcpvisr-engine/src/config.rs crates/tcpvisr-engine/src/tracker.rs
git commit -m "feat(engine): collect per-segment state timeline in the tracker"
```

---

### Task 3: TUI — pure `Transport`

**Files:**
- Create: `crates/tcpvisr-tui/src/transport.rs`
- Modify: `crates/tcpvisr-tui/src/lib.rs`

**Interfaces:**
- Consumes: `tcpvisr_core::Nanos`.
- Produces: `pub struct Transport` with `new`, `cursor`, `speed`, `is_playing`, `bounds`, `toggle_play`, `faster`, `slower`, `seek`, `set_cursor`, `tick`.

- [ ] **Step 1: Write `transport.rs` with failing tests**

```rust
//! The pure playback transport (ADR-0010): cursor time, speed ladder, play/pause. Advances on
//! an injected wall-clock delta — it never reads a clock (the impure `run` loop supplies `dt`).

use tcpvisr_core::Nanos;

/// The selectable playback speeds (0.1–10×; every rung renders exactly at one decimal).
const SPEEDS: [f64; 6] = [0.1, 0.5, 1.0, 2.0, 5.0, 10.0];
const DEFAULT_SPEED_IDX: usize = 2; // 1.0x

/// Cursor + speed + play state over a capture's `[start, end]` time domain.
#[derive(Debug, Clone, Copy)]
pub struct Transport {
    start: Nanos,
    end: Nanos,
    cursor: Nanos,
    speed_idx: usize,
    playing: bool,
}

impl Transport {
    /// A paused transport at `start`, speed 1.0×, over `[start, end]`.
    #[must_use]
    pub fn new(start: Nanos, end: Nanos) -> Self {
        Self { start, end, cursor: start, speed_idx: DEFAULT_SPEED_IDX, playing: false }
    }

    #[must_use]
    pub fn cursor(&self) -> Nanos {
        self.cursor
    }

    #[must_use]
    pub fn speed(&self) -> f64 {
        SPEEDS[self.speed_idx]
    }

    #[must_use]
    pub fn is_playing(&self) -> bool {
        self.playing
    }

    #[must_use]
    pub fn bounds(&self) -> (Nanos, Nanos) {
        (self.start, self.end)
    }

    /// Toggles play/pause. Starting playback from the end rewinds to `start` first.
    pub fn toggle_play(&mut self) {
        if self.playing {
            self.playing = false;
        } else {
            if self.cursor.0 >= self.end.0 {
                self.cursor = self.start;
            }
            self.playing = true;
        }
    }

    /// Steps one rung up the speed ladder (clamped at 10×).
    pub fn faster(&mut self) {
        self.speed_idx = (self.speed_idx + 1).min(SPEEDS.len() - 1);
    }

    /// Steps one rung down the speed ladder (clamped at 0.1×).
    pub fn slower(&mut self) {
        self.speed_idx = self.speed_idx.saturating_sub(1);
    }

    /// Moves the cursor by ~2% of the span (min 1ns), clamped to `[start, end]`.
    pub fn seek(&mut self, forward: bool) {
        let step = ((self.end.0.saturating_sub(self.start.0)) / 50).max(1);
        let next = if forward {
            self.cursor.0.saturating_add(step)
        } else {
            self.cursor.0.saturating_sub(step)
        };
        self.set_cursor(Nanos(next));
    }

    /// Sets the cursor, clamped to `[start, end]`.
    pub fn set_cursor(&mut self, t: Nanos) {
        self.cursor = Nanos(t.0.clamp(self.start.0, self.end.0));
    }

    /// When playing, advances the cursor by `round(speed * dt)` ns, clamped to `end`; reaching
    /// `end` auto-pauses. `dt` is injected wall-clock nanoseconds. A no-op when paused.
    pub fn tick(&mut self, dt: Nanos) {
        if !self.playing {
            return;
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]
        let adv = (dt.0 as f64 * self.speed()).round().max(0.0) as u64;
        self.cursor = Nanos(self.cursor.0.saturating_add(adv).min(self.end.0));
        if self.cursor.0 >= self.end.0 {
            self.playing = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn play_then_tick_advances_by_speed() {
        let mut tr = Transport::new(Nanos(0), Nanos(10_000_000_000)); // 0..10s
        assert!(!tr.is_playing());
        tr.toggle_play();
        assert!(tr.is_playing());
        tr.tick(Nanos(1_000_000_000)); // 1s at 1.0x
        assert_eq!(tr.cursor(), Nanos(1_000_000_000));
    }

    #[test]
    fn paused_tick_is_noop() {
        let mut tr = Transport::new(Nanos(0), Nanos(10_000_000_000));
        tr.tick(Nanos(1_000_000_000));
        assert_eq!(tr.cursor(), Nanos(0));
    }

    #[test]
    fn speed_ladder_clamps_and_scales() {
        let mut tr = Transport::new(Nanos(0), Nanos(100_000_000_000));
        for _ in 0..10 {
            tr.faster();
        }
        assert_eq!(tr.speed(), 10.0);
        tr.toggle_play();
        tr.tick(Nanos(1_000_000_000)); // 1s at 10x -> 10s
        assert_eq!(tr.cursor(), Nanos(10_000_000_000));
        for _ in 0..10 {
            tr.slower();
        }
        assert_eq!(tr.speed(), 0.1);
    }

    #[test]
    fn tick_auto_pauses_at_end() {
        let mut tr = Transport::new(Nanos(0), Nanos(1_000_000_000));
        tr.toggle_play();
        tr.tick(Nanos(5_000_000_000)); // overshoots
        assert_eq!(tr.cursor(), Nanos(1_000_000_000));
        assert!(!tr.is_playing());
    }

    #[test]
    fn toggle_at_end_rewinds_and_plays() {
        let mut tr = Transport::new(Nanos(1_000), Nanos(5_000));
        tr.set_cursor(Nanos(5_000));
        tr.toggle_play();
        assert_eq!(tr.cursor(), Nanos(1_000));
        assert!(tr.is_playing());
    }

    #[test]
    fn seek_moves_two_percent_and_clamps() {
        let mut tr = Transport::new(Nanos(0), Nanos(5_000)); // step = 5000/50 = 100
        tr.seek(true);
        assert_eq!(tr.cursor(), Nanos(100));
        tr.seek(false);
        tr.seek(false);
        assert_eq!(tr.cursor(), Nanos(0), "clamped at start");
        for _ in 0..100 {
            tr.seek(true);
        }
        assert_eq!(tr.cursor(), Nanos(5_000), "clamped at end");
    }
}
```

Note: the item-level `#[allow(...)]` on the cast in `tick` is required because clippy pedantic denies these casts and `allow_attributes` forbids bare item-level allows only in the *lint* sense — this is a cast-lint allow, which is permitted with the inline justification. If clippy still rejects `allow_attributes`, hoist the three casts behind a small helper with a `#![allow(...)]` at a `#[cfg]`-free module scope is NOT possible here; instead compute `adv` as: `let adv = (dt.0 as f64 * self.speed()).round(); let adv = if adv <= 0.0 { 0 } else if adv >= u64::MAX as f64 { u64::MAX } else { adv as u64 };` and keep the single `#[allow]`. Verify against clippy in Step 2.

- [ ] **Step 2: Run tests + clippy**

Run: `cargo test -p tcpvisr-tui transport` and `cargo clippy -p tcpvisr-tui --all-targets --all-features -- -D warnings`. If clippy rejects the cast allow, apply the fallback in the note above.

- [ ] **Step 3: Export from the crate**

In `crates/tcpvisr-tui/src/lib.rs` add `pub mod transport;` and `pub use transport::Transport;`.

- [ ] **Step 4: Guardrails + commit**

```bash
git add crates/tcpvisr-tui/src/transport.rs crates/tcpvisr-tui/src/lib.rs
git commit -m "feat(tui): add pure playback Transport"
```

---

### Task 4: TUI + CLI — `App` as-of-T projection and wiring

This task swaps `App` from a static `&[Connection]` list to a `Timeline` + `Transport`
projection, updates every call site (`keys.rs`, `render.rs`, `run.rs`, `main.rs`) so the
workspace compiles, and adds the `build_replay_app` seam. `keys.rs`/`render.rs` get their new
transport features in Tasks 5–6; here they change only as much as compilation requires, plus
`App` gains the transport-delegating methods.

**Files:**
- Modify: `crates/tcpvisr-tui/src/app.rs` (rewrite state + `visible()`; rewrite tests)
- Modify: `crates/tcpvisr-tui/src/render.rs` (construct `App` from a `Timeline` in tests; state cell from the resolved row)
- Modify: `crates/tcpvisr-tui/src/keys.rs` (construct `App` from a `Timeline` in tests)
- Modify: `crates/tcpvisr-tui/src/run.rs` (poll + `tick` loop)
- Modify: `crates/tcpvisr-tui/src/lib.rs` (re-exports unchanged names)
- Modify: `crates/tcp-visr/src/main.rs` (`build_replay_app`, `run_replay`)

**Interfaces:**
- Consumes: `tcpvisr_engine::{Timeline, AsOf, ConnId, ConnState}`; `tcpvisr_tui::Transport` (Task 3); `tcpvisr_core::{Endpoint, Nanos}`.
- Produces:
  - `App::new(timeline: Timeline, title: String) -> App`
  - `App::visible(&self) -> Vec<ConnRow>` where `ConnRow { id: ConnId, peer: Endpoint, service: Option<&'static str>, state: ConnState, origin_inferred: bool, bytes_up: u64, bytes_down: u64 }`
  - transport delegators on `App`: `toggle_play`, `seek(forward: bool)`, `faster`, `slower`, `step_forward`, `step_back`, `tick(dt: Nanos)`
  - accessors: `cursor() -> Nanos`, `speed() -> f64`, `is_playing() -> bool`, `bounds() -> (Nanos, Nanos)`, `is_capture_empty() -> bool`, plus the M4 accessors (`title`, `sort_field`, `sort_dir`, `mode`, `query`, `selected`)
  - `build_replay_app(file: &Path, cfg: EngineConfig) -> Result<tcpvisr_tui::App, Box<dyn std::error::Error>>`

- [ ] **Step 1: Rewrite `app.rs` state and projection**

Replace the top of `app.rs` (the imports, `ConnRow`, and `App` struct + `impl`) so that:
- Imports: `use std::collections::HashMap; use tcpvisr_core::{Endpoint, Nanos}; use tcpvisr_engine::{AsOf, ConnId, ConnState, Timeline}; use crate::service::service_name; use crate::transport::Transport;`
- `ConnRow` becomes the dynamic row above (drop the stored `search` field; keep `id, peer, service, state, origin_inferred, bytes_up, bytes_down`).
- Add a private `struct ConnMeta { peer: Endpoint, service: Option<&'static str>, origin_inferred: bool, search_prefix: String }` built once per connection: `search_prefix = format!("{} {} {}", origin, responder, service.unwrap_or("")).to_lowercase()`.
- `App` holds: `timeline: Timeline, transport: Transport, metas: HashMap<ConnId, ConnMeta>, sort_field, sort_dir, mode, query, selected, title`.
- `App::new(timeline, title)`: build `metas` from `timeline.connections()`; `transport = Transport::new(start, end)` from `timeline.bounds()`; defaults `SortField::Peer`/`SortDir::Asc`/`Mode::Nav`/empty query/`selected = None`; then `selected = visible().first().map(|r| r.id)`.
- `visible(&self)`:
  ```rust
  pub fn visible(&self) -> Vec<ConnRow> {
      let t = self.transport.cursor();
      let q = self.query.to_lowercase();
      let mut rows: Vec<ConnRow> = self
          .timeline
          .resolve_at(t)
          .into_iter()
          .filter_map(|a| self.row(a, &q))
          .collect();
      rows.sort_by(|x, y| self.order(x, y));
      rows
  }

  fn row(&self, a: AsOf, q: &str) -> Option<ConnRow> {
      let m = self.metas.get(&a.id)?;
      let search = format!("{} {}", m.search_prefix, format!("{:?}", a.state).to_lowercase());
      if !is_subsequence(q, &search) {
          return None;
      }
      Some(ConnRow {
          id: a.id,
          peer: m.peer,
          service: m.service,
          state: a.state,
          origin_inferred: m.origin_inferred,
          bytes_up: a.bytes_o2r,
          bytes_down: a.bytes_r2o,
      })
  }
  ```
- Keep `is_subsequence`, `natural_dir`, `rank`, `Step`, and the M4 methods (`cycle_sort`, `toggle_dir`, `enter_filter`, `push_filter`, `pop_filter`, `confirm_filter`, `cancel_filter`, `move_up`, `move_down`, `reconcile_selection`, `order`, the M4 accessors). `order` and `move_by`/`reconcile_selection` now call `self.visible()` (unchanged shape). `order` uses `ConnRow.bytes_up`/`bytes_down`/`state`/`peer` (same field names → same body).
- Add transport delegators (each reconciles the selection because the active set can change):
  ```rust
  pub fn toggle_play(&mut self) { self.transport.toggle_play(); self.reconcile_selection(); }
  pub fn seek(&mut self, forward: bool) { self.transport.seek(forward); self.reconcile_selection(); }
  pub fn faster(&mut self) { self.transport.faster(); }
  pub fn slower(&mut self) { self.transport.slower(); }
  pub fn step_forward(&mut self) {
      if let Some(t) = self.timeline.next_event(self.transport.cursor()) {
          self.transport.set_cursor(t);
          self.reconcile_selection();
      }
  }
  pub fn step_back(&mut self) {
      if let Some(t) = self.timeline.prev_event(self.transport.cursor()) {
          self.transport.set_cursor(t);
          self.reconcile_selection();
      }
  }
  pub fn tick(&mut self, dt: Nanos) {
      let before = self.transport.cursor();
      self.transport.tick(dt);
      if self.transport.cursor() != before {
          self.reconcile_selection();
      }
  }
  ```
- Add accessors: `cursor()`, `speed()`, `is_playing()`, `bounds()` (delegate to transport), and `is_capture_empty(&self) -> bool { self.metas.is_empty() }`.

- [ ] **Step 2: Rewrite `app.rs` tests**

Replace the `#[cfg(test)]` module. Add a helper that builds an `App` from hand-made connections + samples via a `Timeline`:
```rust
fn app_of(entries: Vec<(Connection, Vec<StateSample>)>) -> App {
    App::new(Timeline::new(entries), "t".to_string())
}
```
Port the M4 behaviors that still hold (sort cycle, direction toggle, filter narrowing, selection movement/clamp, resort-keeps-selection) but build each connection with a matching `StateSample` at `opened_at` carrying the same `state`/bytes the row should show, and set the cursor to a `T ≥ opened_at` so the row is active (default cursor is `start`, which for a single-open-time capture already shows every row). Add the new criteria:
- **Criterion 9 (as-of-T rows):** two connections, one opening at 0 and one at 100, samples reflecting their bytes; at cursor `start` (0) only the first is active; after `app.seek(true)` enough to pass 100 (or `app.step_forward()`), both appear and byte columns update.
- **Criterion 10 (selection reconciles):** select the later connection, `step_back`/seek to a `T` before it opens → selection falls back to the first visible (or none); step forward → valid again.

Because the default cursor is `start` and many M4 tests used a single `opened_at`, most ported tests need every connection's `opened_at` equal (e.g. all `Nanos(0)`) and one `StateSample` at `t=0` so all rows are active at the initial cursor. Keep byte/state values in the sample, not the `Connection` (the row reads the sample).

- [ ] **Step 3: Fix `keys.rs` and `render.rs` test constructors to compile**

In both test modules, replace `App::new(&[conn(...)], "t")` with `App::new(Timeline::new(vec![(conn(...), vec![state_sample_at_open])]), "t")`. Add the needed imports (`tcpvisr_engine::{StateSample, Timeline}`, `ConnState`, `Nanos`). Keep each connection's `opened_at = Nanos(0)` and a `StateSample { t: Nanos(0), state: ConnState::Established, bytes_o2r, bytes_r2o }` so existing assertions about rows/bytes still hold at the initial cursor. `render.rs`'s state cell now comes from `ConnRow.state` (the resolved row) — the `Established`/`Established~` assertions still pass because the sample carries `Established` and the meta carries `origin_inferred`.

- [ ] **Step 4: Update `run.rs` to a poll + tick loop**

```rust
//! The impure terminal shell: init, poll/tick event loop, restore (spec §3.8, ADR-0010).

use std::time::{Duration, Instant};

use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use tcpvisr_core::Nanos;

use crate::app::{App, Outcome};
use crate::keys::handle_key;
use crate::render::render;

const TICK: Duration = Duration::from_millis(50);

pub fn run(app: App) -> std::io::Result<()> {
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, app);
    ratatui::restore();
    result
}

fn event_loop(terminal: &mut DefaultTerminal, mut app: App) -> std::io::Result<()> {
    let mut last = Instant::now();
    loop {
        terminal.draw(|frame| render(frame, &app))?;
        if event::poll(TICK)? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press && handle_key(&mut app, key) == Outcome::Quit {
                    break;
                }
            }
        }
        let now = Instant::now();
        let dt = u64::try_from(now.duration_since(last).as_nanos()).unwrap_or(u64::MAX);
        last = now;
        app.tick(Nanos(dt));
    }
    Ok(())
}
```
Keep the module doc comment describing that `run` is the only clock reader (ADR-0002/0010).

- [ ] **Step 5: Wire the CLI (`main.rs`)**

Add `use tcpvisr_engine::Timeline;`? Not needed — `build_replay_app` returns `tcpvisr_tui::App`. Add `use tcpvisr_tui::App;` near the other imports. Add the seam and rewrite `run_replay`:
```rust
/// Parses `file` into a seekable [`Timeline`] and builds the replay [`App`]. No TTY, no event
/// loop — this is the testable seam behind `run_replay` (spec §4, criteria 13–14).
fn build_replay_app(file: &Path, cfg: EngineConfig) -> Result<App, Box<dyn std::error::Error>> {
    let mut tracker = Tracker::new(cfg);
    let (_link, skipped) = tcpvisr_ingest::parse_file_visit(file, &mut |item| tracker.observe(item))?;
    let timeline = tracker.into_timeline()?;
    let title = format!(
        "tcp-visr — {}  ({} connections, skipped {})",
        file.display(),
        timeline.connection_count(),
        skipped.total(),
    );
    Ok(App::new(timeline, title))
}

fn run_replay(file: &Path) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::IsTerminal;
    if !std::io::stdout().is_terminal() {
        return Err("replay requires an interactive terminal (stdout is not a tty)".into());
    }
    let cfg = EngineConfig { collect_state_timeline: true, ..EngineConfig::default() };
    let app = build_replay_app(file, cfg)?;
    tcpvisr_tui::run(app)?;
    Ok(())
}
```
`into_timeline()` returns `MetricError` which implements `std::error::Error` (thiserror), so `?` boxes it. Remove the now-unused `use tcpvisr_tui::App;` conflict if `App` was not previously imported (it was not — M4 used `tcpvisr_tui::App::new` fully qualified; either keep fully-qualified `tcpvisr_tui::App` in the signature or add the `use`). Prefer fully-qualified to minimize churn: signature `-> Result<tcpvisr_tui::App, ...>` and body `Ok(tcpvisr_tui::App::new(timeline, title))`; drop the `use tcpvisr_tui::App;`.

- [ ] **Step 6: Run the whole workspace**

Run: `cargo test --workspace` and `cargo test -p tcpvisr-ingest --features live`. Fix any compile/test fallout (most churn is the test constructors in Step 3).

- [ ] **Step 7: Guardrails + commit**

```bash
git add crates/tcpvisr-tui/src/app.rs crates/tcpvisr-tui/src/render.rs crates/tcpvisr-tui/src/keys.rs crates/tcpvisr-tui/src/run.rs crates/tcp-visr/src/main.rs
git commit -m "feat(tui): resolve the master list as of cursor time T"
```

---

### Task 5: TUI — transport key bindings

**Files:**
- Modify: `crates/tcpvisr-tui/src/keys.rs`

**Interfaces:**
- Consumes: `App::{toggle_play, seek, faster, slower, step_forward, step_back}` (Task 4).

- [ ] **Step 1: Write failing key tests**

Add to `keys.rs` tests (build the `App` via a `Timeline` with two connections opening at different times so seek/step change the active set). Assert:
- `space` in nav mode returns `Continue` and flips `app.is_playing()`.
- `Right` advances `app.cursor()` by a seek step; `Left` moves it back.
- `+` (and `=`) increase `app.speed()`; `-` decreases it.
- `.` moves the cursor to the next event; `,` to the previous.
- In filter mode, `space`/`+`/`,`/`.` append to the query and do **not** change the cursor/speed/play; `Ctrl-C` still quits.

- [ ] **Step 2: Implement the mappings**

In `handle_nav`, add before the `_ => {}` arm:
```rust
        KeyCode::Char(' ') => app.toggle_play(),
        KeyCode::Left => app.seek(false),
        KeyCode::Right => app.seek(true),
        KeyCode::Char('+') | KeyCode::Char('=') => app.faster(),
        KeyCode::Char('-') | KeyCode::Char('_') => app.slower(),
        KeyCode::Char('.') => app.step_forward(),
        KeyCode::Char(',') => app.step_back(),
```
`handle_filter` is unchanged — its `KeyCode::Char(c) => app.push_filter(c)` already captures `space`/`+`/`,`/`.`.

- [ ] **Step 3: Run + guardrails + commit**

Run: `cargo test -p tcpvisr-tui keys`, then full guardrails.
```bash
git add crates/tcpvisr-tui/src/keys.rs
git commit -m "feat(tui): bind transport keys (play/seek/speed/step)"
```

---

### Task 6: TUI — transport header, footer, and empty state

**Files:**
- Modify: `crates/tcpvisr-tui/src/render.rs`

**Interfaces:**
- Consumes: `App::{cursor, speed, is_playing, bounds, is_capture_empty, title, visible, mode, query, sort_field, sort_dir}`.

- [ ] **Step 1: Write failing render tests**

Add `TestBackend` tests (criterion 12): build an `App` with a playing/paused transport and a known cursor, then assert the rendered buffer contains:
- the paused glyph `⏸` (default) or `▶` after `app.toggle_play()`,
- the speed `1.0x` (and `2.0x` after `faster`),
- a `t=` readout with the fixed-precision seconds (e.g. `t=0.000s`),
- the transport hints `space` and `seek` in the footer,
- for a cursor before the first connection opens: `no connections active`.

- [ ] **Step 2: Implement the header + footer + empty state**

Add a seconds formatter and a status string:
```rust
fn fmt_seconds(t: tcpvisr_core::Nanos) -> String {
    let ms = t.0 / 1_000_000;
    format!("{}.{:03}", ms / 1000, ms % 1000)
}

fn transport_status(app: &App) -> String {
    let glyph = if app.is_playing() { "▶" } else { "⏸" };
    let (_, end) = app.bounds();
    format!("[ {glyph} {:.1}x  t={}s / {}s ]", app.speed(), fmt_seconds(app.cursor()), fmt_seconds(end))
}
```
In `render_main`, build the block with both a left title and a right-aligned transport title:
```rust
    use ratatui::text::Line;
    let block = Block::bordered()
        .title(app.title().to_string())
        .title(Line::from(transport_status(app)).right_aligned());
```
Empty state: when `rows.is_empty()`, show `no connections in capture` if `app.is_capture_empty()`, else `format!("no connections active at t={}s", fmt_seconds(app.cursor()))`.
Row state cell now reads `ConnRow.state` (already the resolved state) — keep the `~` suffix on `origin_inferred`.
Footer (nav mode): replace the M4 string with:
```rust
    format!(
        "space play/pause  ←→ seek  +/- speed  ,/. step  / filter  s sort:{}{arrow}  q quit",
        sort_label(app.sort_field()),
    )
```
Filter-mode footer (`/query`) is unchanged.

- [ ] **Step 3: Run + guardrails + commit**

Run: `cargo test -p tcpvisr-tui render`, then full guardrails.
```bash
git add crates/tcpvisr-tui/src/render.rs
git commit -m "feat(tui): render transport status, hints, and empty state"
```

---

### Task 7: CLI — `build_replay_app` seam tests

**Files:**
- Modify: `crates/tcp-visr/src/main.rs` (add `#[cfg(test)]` module)

**Interfaces:**
- Consumes: `build_replay_app` (Task 4); the committed fixture `crates/tcp-visr/tests/fixtures/metrics_basic.pcap`.

- [ ] **Step 1: Write the failing inline tests (criteria 13–14)**

Append to `main.rs`:
```rust
#[cfg(test)]
mod build_replay_tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn fixture() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/metrics_basic.pcap")
    }

    #[test]
    fn builds_a_timeline_app_with_rows() {
        let cfg = EngineConfig { collect_state_timeline: true, ..EngineConfig::default() };
        let app = build_replay_app(&fixture(), cfg).expect("build");
        assert!(!app.visible().is_empty(), "fixture has connections active at start");
    }

    #[test]
    fn sample_ceiling_is_fatal() {
        let cfg = EngineConfig { collect_state_timeline: true, max_samples: 1, ..EngineConfig::default() };
        let err = build_replay_app(&fixture(), cfg).expect_err("ceiling");
        let msg = err.to_string();
        assert!(msg.contains("--max-samples"), "actionable: {msg}");
    }
}
```
`App`'s `visible()` is public; `EngineConfig` is already imported in `main.rs`.

- [ ] **Step 2: Run + guardrails + commit**

Run: `cargo test -p tcp-visr build_replay`, then full guardrails.
```bash
git add crates/tcp-visr/src/main.rs
git commit -m "test(cli): cover build_replay_app happy path and ceiling"
```

---

### Task 8: Docs — refresh current-state notes

**Files:**
- Modify: `CLAUDE.md` (the "Current state" paragraph)
- Modify: `docs/design/tcp-visr-design.md` (§10 roadmap M5 status, if the table marks status)

- [ ] **Step 1: Update `CLAUDE.md` current state**

The "Current state" paragraph says milestones M0–M3 are implemented and `replay` is a stub. M4 (#7) shipped the replay TUI shell and M5 (this work) makes it seekable. Update it to: M0–M5 implemented; `replay` opens a seekable TUI over a capture; `live` and kernel enrichment remain stubs. Keep it factual and short.

- [ ] **Step 2: Update the design roadmap note (only if it tracks status)**

If design §10's table or surrounding text marks per-milestone status, mark M5 done consistently with how M4 is marked. If it does not track status, make no change.

- [ ] **Step 3: Commit**

```bash
git add CLAUDE.md docs/design/tcp-visr-design.md
git commit -m "docs: mark M5 (timeline + transport) implemented"
```

---

## Self-Review Notes

- **Spec coverage:** §3.3 Timeline → Task 1; §3.1/§4 collection + ceiling → Task 2; §3.4 Transport → Task 3; §3.5 as-of-T master list + §3.8 run loop + §4 CLI seam → Task 4; §3.7 keys → Task 5; §3.2/§3.6 header/footer/empty → Task 6; criteria 13–14 → Task 7. Criteria 1–4,15 → Task 1; 5–8 → Task 3; 9–10 → Task 4; 11 → Task 5; 12 → Task 6; 13–14 → Task 7.
- **Green-per-commit:** Tasks 1–3 are additive (engine/tui compile independently; `Transport` is exported-but-unused, which is fine for `pub` lib items). Task 4 is the atomic API swap that updates every `App` call site at once. Tasks 5–7 are additive on top.
- **Non-monotonic time:** handled in `Timeline::new` (sort each series by `t`; `start` = min sample time), tested by criterion 15.
- **Purity:** only `run.rs` reads `Instant::now()`; `Transport`/`App`/`Timeline` take `dt`/`t` as data.
