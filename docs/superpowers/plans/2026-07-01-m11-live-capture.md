# M11 Live Capture Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `tcp-visr live -i <iface>`: capture TCP off a live Linux interface via libpcap and drive the same pure engine + TUI the replay path uses, with bounded (time-horizon-evicting) retention, `Tick`-driven decay/idle, a per-redraw immutable `Timeline` snapshot, and a follow/freeze transport clamped to the eviction horizon.

**Architecture:** A background capture thread (the only clock reader) decodes frames with the shared `decode_frame`, stamps `Segment`s and timeout-injected `Tick`s from libpcap's wall clock, and pushes `Item`s through a bounded channel. The main thread folds them into a `Tracker` running a new `RetentionPolicy::Evict` (VecDeque series + whole-connection eviction), rebuilds an immutable `Timeline` snapshot each redraw, and `retarget`s a live `App`. The engine stays pure; replay is untouched.

**Tech Stack:** Rust 1.88.0, `pcap` 2.4.0 (`live` feature), `etherparse`/`pcap-parser` (shared decoder), `ratatui`/`crossterm`, `clap` 4.

## Global Constraints

- Toolchain pinned to Rust **1.88.0** (`rust-toolchain.toml`); dependency versions pinned exactly (`=x.y.z`); a new SPDX license id must be added to `deny.toml`'s allow-list.
- **Engine is pure** (ADR-0002): no I/O, no clock reads in `tcpvisr-engine`. `Tick` timestamps arrive as data from the faucet.
- **One decoder, both faucets** (ADR-0003/0005): live uses the same `decode_frame`; do not add a second header path. The parity test must stay green.
- **Replay path unchanged**: `RetentionPolicy::FailFast` must reproduce today's behavior byte-for-byte, including the `MetricError::SampleCeiling { samples, limit }` error.
- Clippy is strict workspace-wide: `unwrap_used`/`expect_used`/`panic`/`print_stdout`/`print_stderr` denied in non-test code; `allow_attributes` denied (use file-level `#![allow(...)]` in test-support modules, `#[expect(...)]` elsewhere with a `reason`). Line length ≤100, functions ≤100 lines, cyclomatic complexity ≤8, absolute imports only.
- **The `live` code is feature-gated** (`#[cfg(feature = "live")]`); the default build stays libpcap-free. CI compiles `--all-features` and `--features live`, so live code is clippy-linted but interface capture is never run in CI.
- CI gate (run all before pushing): `cargo fmt --all --check`; `cargo clippy --all-targets --all-features -- -D warnings`; `cargo test --workspace`; `cargo test -p tcpvisr-ingest --features live`; `cargo deny check`.
- Commits: Conventional Commits, imperative, ≤72-char subject, one logical change; trailer `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.

## File map

| File | Responsibility | Change |
|------|----------------|--------|
| `crates/tcpvisr-engine/src/config.rs` | `EngineConfig`, new `RetentionPolicy` | modify |
| `crates/tcpvisr-engine/src/tracker.rs` | VecDeque series, eviction, Tick decay/idle, whole-conn eviction, `snapshot()`, accessors | modify |
| `crates/tcpvisr-engine/src/timeline.rs` | additive `with_seq_ending` constructor | modify |
| `crates/tcpvisr-engine/src/lib.rs` | re-export `RetentionPolicy` | modify |
| `crates/tcpvisr-ingest/src/live.rs` | `LiveOptions`/`LiveError`/`LiveCapture`/`list_interfaces`/`LiveEvent`, capture loop | create |
| `crates/tcpvisr-ingest/src/lib.rs` | gate + re-export `live` module | modify |
| `crates/tcpvisr-ingest/Cargo.toml` | (already has `live`/`pcap`) | none |
| `crates/tcpvisr-tui/src/app.rs` | live fields, `retarget`, follow/freeze, `LiveStatus` | modify |
| `crates/tcpvisr-tui/src/transport.rs` | `set_domain`, live cursor clamp | modify |
| `crates/tcpvisr-tui/src/keys.rs` | live `space`/seek semantics | modify |
| `crates/tcpvisr-tui/src/render.rs` | live status line (drops/approx/follow) | modify |
| `crates/tcpvisr-tui/src/run.rs` | `run_live` event loop | modify |
| `crates/tcpvisr-tui/src/lib.rs` | export `run_live`, live types | modify |
| `crates/tcp-visr/src/main.rs` | `live` subcommand, wiring, RAII guard, drop counting, feature-off error | modify |
| `docs/design/tcp-visr-design.md`, `CLAUDE.md` | mark M11 implemented; update current-state | modify |

---

### Task 1: Engine — `RetentionPolicy`, replace the `max_samples` field

**Files:**
- Modify: `crates/tcpvisr-engine/src/config.rs`
- Modify: `crates/tcpvisr-engine/src/tracker.rs` (read sites for `max_samples`)
- Modify: `crates/tcpvisr-engine/src/lib.rs` (re-export)
- Modify call sites: `crates/tcp-visr/src/main.rs`, and every `EngineConfig { max_samples: .. }` in tests across the workspace (compiler-guided).

**Interfaces:**
- Produces:
  ```rust
  pub enum RetentionPolicy {
      FailFast { max_samples: usize },
      Evict { window: Nanos, max_samples: usize },
  }
  impl RetentionPolicy {
      pub fn max_samples(&self) -> usize;   // both arms
      pub fn window(&self) -> Option<Nanos>; // Some for Evict, None for FailFast
  }
  // EngineConfig.max_samples: usize  ->  EngineConfig.retention: RetentionPolicy
  ```
  Default retention: `FailFast { max_samples: 10_000_000 }`.

- [ ] **Step 1: Write the failing test** (append to `config.rs` `mod tests`)

```rust
#[test]
fn retention_defaults_to_failfast_ten_million() {
    let c = EngineConfig::default();
    assert_eq!(c.retention, RetentionPolicy::FailFast { max_samples: 10_000_000 });
    assert_eq!(c.retention.max_samples(), 10_000_000);
    assert_eq!(c.retention.window(), None);
}

#[test]
fn evict_policy_exposes_window_and_backstop() {
    let p = RetentionPolicy::Evict { window: Nanos(120_000_000_000), max_samples: 2_000_000 };
    assert_eq!(p.window(), Some(Nanos(120_000_000_000)));
    assert_eq!(p.max_samples(), 2_000_000);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p tcpvisr-engine config::tests::retention -- --nocapture`
Expected: FAIL — `RetentionPolicy` undefined, `EngineConfig::retention` missing.

- [ ] **Step 3: Implement** in `config.rs`:

```rust
/// How the tracker bounds retained samples (design §7, ADR-0004/0016).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetentionPolicy {
    /// Replay: exceeding `max_samples` fails fast (`MetricError::SampleCeiling`).
    FailFast { max_samples: usize },
    /// Live: evict samples older than `window`; `max_samples` is a hard memory backstop
    /// that evicts the oldest rather than erroring.
    Evict { window: Nanos, max_samples: usize },
}

impl RetentionPolicy {
    /// The hard sample ceiling / memory backstop.
    #[must_use]
    pub fn max_samples(&self) -> usize {
        match self {
            Self::FailFast { max_samples } | Self::Evict { max_samples, .. } => *max_samples,
        }
    }

    /// The time-horizon eviction window, or `None` under fail-fast.
    #[must_use]
    pub fn window(&self) -> Option<Nanos> {
        match self {
            Self::FailFast { .. } => None,
            Self::Evict { window, .. } => Some(*window),
        }
    }
}
```

Replace the `EngineConfig` field `pub max_samples: usize` with `pub retention: RetentionPolicy`; update `Default` (`retention: RetentionPolicy::FailFast { max_samples: 10_000_000 }`) and remove the old `max_samples` line + its default. Update the `defaults_match_spec` test's `assert_eq!(c.max_samples, 10_000_000)` to assert on `c.retention`. Export `RetentionPolicy` from `lib.rs` alongside `EngineConfig`.

- [ ] **Step 4: Update tracker.rs read sites**

In `tracker.rs`, replace every `self.config.max_samples` with `self.config.retention.max_samples()` (there are several in `record_sample`/`record_state`/`record_seq`/`record_inflight`/`record_rtt`/`record_throughput`, and in `into_metrics`/`into_timeline` error construction). Behavior is identical.

- [ ] **Step 5: Update all remaining construction sites (compiler-guided)**

Run `cargo build --workspace` and fix each error: `max_samples: N` inside an `EngineConfig { .. }` becomes `retention: RetentionPolicy::FailFast { max_samples: N }`. This touches `crates/tcp-visr/src/main.rs` (`replay_engine_config`, `run_metrics` base config, and the two `..EngineConfig::default()` sites that set `max_samples`) and test modules in `tracker.rs`, `main.rs`, and integration tests under `crates/tcp-visr/tests/`. Import `RetentionPolicy` where needed.

- [ ] **Step 6: Run the full engine + workspace tests**

Run: `cargo test -p tcpvisr-engine && cargo test --workspace`
Expected: PASS (all pre-existing tests green; new config tests pass). Then `cargo clippy --all-targets --all-features -- -D warnings` clean.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "refactor(engine): replace max_samples field with RetentionPolicy

FailFast{max_samples} reproduces replay behavior exactly; adds Evict{window,
max_samples} for the live path. Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: Engine — VecDeque series + time-horizon sample eviction

**Files:**
- Modify: `crates/tcpvisr-engine/src/tracker.rs`

**Interfaces:**
- Consumes: `RetentionPolicy` (Task 1).
- Produces (internal): `Tracker.now: Nanos`; `ConnTrack` series become `VecDeque<_>`; private `Tracker::evict_samples(&mut self)` dropping front samples with `t < now - window` (keeping ≥1 `states`), decrementing `collected_samples`; `record_*` under `Evict` evict-oldest at the ceiling instead of setting `overflowed`. `into_metrics`/`into_timeline` iterate the deques (`.iter()`), unchanged output.

- [ ] **Step 1: Write the failing test** (new `mod evict_tests` in `tracker.rs`, using `test_support::{ep, seg}`)

```rust
#[cfg(test)]
mod evict_tests {
    use super::test_support::{ep, seg};
    use super::*;
    use crate::config::RetentionPolicy;
    use tcpvisr_core::{Nanos, TcpFlags};

    fn evict_cfg(window_ns: u64) -> EngineConfig {
        EngineConfig {
            collect_state_timeline: true,
            collect_throughput_timeline: true,
            series_collection: SeriesCollection::All,
            retention: RetentionPolicy::Evict { window: Nanos(window_ns), max_samples: 1_000_000 },
            ..EngineConfig::default()
        }
    }

    #[test]
    fn state_samples_older_than_window_are_evicted_keeping_latest() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let mut t = Tracker::new(evict_cfg(1_000)); // 1000ns window
        // four sends 500ns apart -> ts 500,1000,1500,2000; now=2000, horizon=1000
        for k in 1..=4u64 {
            t.observe(&seg(c, s, TcpFlags::ACK, 100 + (k as u32) * 10, 1, 10, k * 500));
        }
        let idx = 0;
        let states: Vec<u64> = t.conns[idx].states.iter().map(|x| x.t.0).collect();
        // horizon = now(2000) - window(1000) = 1000; keep t >= 1000 -> {1000,1500,2000}
        assert_eq!(states, vec![1000, 1500, 2000], "front (t<1000) evicted");
    }

    #[test]
    fn states_never_evicts_the_last_sample() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let mut t = Tracker::new(evict_cfg(1)); // 1ns window: everything but latest is stale
        t.observe(&seg(c, s, TcpFlags::ACK, 100, 1, 10, 1_000));
        t.observe(&seg(c, s, TcpFlags::ACK, 110, 1, 10, 5_000));
        let states: Vec<u64> = t.conns[0].states.iter().map(|x| x.t.0).collect();
        assert_eq!(states, vec![5_000], "at least the most recent state survives");
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p tcpvisr-engine evict_tests -- --nocapture`
Expected: FAIL — no eviction happens yet (series retain all four / both samples).

- [ ] **Step 3: Implement**

1. Change the six series fields in `struct ConnTrack` from `Vec<_>` to `VecDeque<_>` (`states`, `series`, `seq`, `inflight`, `rtt`, `throughput`); add `use std::collections::VecDeque;`. Update `create_instance` to initialize them `VecDeque::new()`. Replace `.push(x)` with `.push_back(x)`. In `push_seq_points`, keep the local `out: &mut Vec<SeqSample>` (caller drains it into the deque via `record_seq`), so no signature change there.
2. Add `now: Nanos` to `Tracker` (init `Nanos(0)` in `new`); in `observe_segment`/`create_instance` set `self.now = Nanos(self.now.0.max(seg.ts.0))` before recording.
3. Add:

```rust
/// Drops front samples older than the eviction horizon from every series, keeping at least
/// the most recent `states` sample so the connection stays resolvable at "now". No-op under
/// FailFast. Decrements the global collected count for each dropped sample.
fn evict_samples(&mut self) {
    let Some(window) = self.config.retention.window() else { return };
    let horizon = self.now.0.saturating_sub(window.0);
    for c in &mut self.conns {
        Self::evict_front(&mut c.states, horizon, 1, &mut self.collected_samples);
        Self::evict_front(&mut c.series, horizon, 0, &mut self.collected_samples);
        Self::evict_front(&mut c.seq, horizon, 0, &mut self.collected_samples);
        Self::evict_front(&mut c.inflight, horizon, 0, &mut self.collected_samples);
        Self::evict_front(&mut c.rtt, horizon, 0, &mut self.collected_samples);
        Self::evict_front(&mut c.throughput, horizon, 0, &mut self.collected_samples);
    }
}
```

Because `series`/`seq`/etc. have different element types, make `evict_front` generic over a `t: Nanos` accessor. Add a small private trait or a `fn t(&self) -> Nanos` used via a closure. Simplest: a generic helper taking a key fn:

```rust
fn evict_front<T>(dq: &mut VecDeque<T>, horizon: u64, keep_min: usize, count: &mut usize)
where
    T: HasT,
{
    while dq.len() > keep_min {
        match dq.front() {
            Some(f) if f.t().0 < horizon => { dq.pop_front(); *count = count.saturating_sub(1); }
            _ => break,
        }
    }
}
```

Define `trait HasT { fn t(&self) -> Nanos; }` and `impl HasT for StateSample/SeqSample/InFlightSample/RttSample/ThroughputSample/MetricSample` (each returns its `.t`; `MetricSample` field is `.t`). Keep it in `tracker.rs` (private).

4. Call `self.evict_samples()` at the end of `observe_segment` and `create_instance` (after recording), so each new sample also triggers eviction. Note the eviction uses `self.now`, which was just advanced.
5. Under `Evict`, the `record_*` ceiling branch must evict-oldest instead of `overflowed = true`. Change the shared guard: when `collected_samples >= max_samples` and `window().is_some()`, pop one from the *longest* series first (or simply rely on `evict_samples` + a global backstop). Minimal correct approach: in each `record_*`, replace

```rust
if self.collected_samples >= self.config.retention.max_samples() { self.overflowed = true; return; }
```

with

```rust
if self.collected_samples >= self.config.retention.max_samples() {
    if self.config.retention.window().is_some() { self.evict_oldest_global(); }
    else { self.overflowed = true; return; }
}
```

where `evict_oldest_global` pops one front sample from the current connection's longest non-empty series (respecting the `states` keep-≥1 rule) and decrements the count. This keeps FailFast identical.

- [ ] **Step 4: Run to verify pass + no regression**

Run: `cargo test -p tcpvisr-engine`
Expected: PASS — new evict tests green; all existing tracker/timeline/metric tests green (FailFast unchanged: `into_timeline`/`into_metrics` still `SampleCeiling` on overflow).

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(engine): time-horizon sample eviction under Evict policy

VecDeque series; front samples older than now-window are dropped (states keeps
>=1); the max_samples backstop evicts oldest under Evict, still fails fast under
FailFast. Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: Engine — Tick decay, idle-drop, whole-connection eviction

**Files:**
- Modify: `crates/tcpvisr-engine/src/tracker.rs`

**Interfaces:**
- Consumes: eviction (Task 2), `Tick` item.
- Produces: `observe(Item::Tick(t))` active under `Evict` — advances `now`, emits a decay `ThroughputSample`/`InFlightSample` (when collected) for each active connection whose trailing window still holds bytes, runs `evict_samples`, then whole-connection eviction. `Tracker::conns`/`live`/`next_instance` shrink; `live` is rebuilt after removal so its indices stay valid.

- [ ] **Step 1: Write the failing tests** (extend `mod evict_tests`)

```rust
#[test]
fn tick_decays_throughput_toward_zero_after_silence() {
    let (c, s) = (ep(1, 1234), ep(2, 80));
    let mut cfg = evict_cfg(10_000_000_000); // 10s window (no sample eviction here)
    cfg.throughput_window = Nanos(1_000_000_000); // 1s throughput window
    let mut t = Tracker::new(cfg);
    // 1000 bytes at t=0
    t.observe(&seg(c, s, TcpFlags::ACK, 100, 1, 1000, 0));
    let last_active = t.conns[0].throughput.back().map(|x| x.throughput_bps);
    // Tick at t=2s: >1s of silence -> window empty -> a decay sample at 0 bps.
    t.observe(&tcpvisr_core::Item::Tick(Nanos(2_000_000_000)));
    let decayed = t.conns[0].throughput.back().expect("a decay sample");
    assert_eq!(decayed.t, Nanos(2_000_000_000));
    assert_eq!(decayed.throughput_bps, 0, "rate decays to zero after the window empties");
    assert!(last_active.unwrap_or(0) > 0);
}

#[test]
fn whole_connection_evicted_when_terminal_and_past_horizon() {
    let (c, s) = (ep(1, 1234), ep(2, 80));
    let mut t = Tracker::new(evict_cfg(1_000)); // 1000ns window
    // open + RST at t=100 -> Reset (terminal), last_at=100
    t.observe(&seg(c, s, TcpFlags::ACK, 100, 1, 10, 100));
    t.observe(&seg(s, c, TcpFlags::RST, 1, 0, 0, 100));
    assert_eq!(t.conns.len(), 1);
    // Tick far past horizon: now=5000, horizon=4000 > last_at=100 -> evict whole connection.
    t.observe(&tcpvisr_core::Item::Tick(Nanos(5_000)));
    assert!(t.conns.is_empty(), "terminal + last_at<horizon -> whole-connection eviction");
    assert!(t.live.is_empty());
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p tcpvisr-engine evict_tests::tick -v` and `::whole_connection`
Expected: FAIL — `Tick` is currently inert; no decay sample, no connection removal.

- [ ] **Step 3: Implement**

1. Change `observe`:

```rust
pub fn observe(&mut self, item: &Item) {
    match item {
        Item::Segment(seg) => self.observe_segment(seg),
        Item::Tick(t) => self.observe_tick(*t),
    }
}

fn observe_tick(&mut self, t: Nanos) {
    if self.config.retention.window().is_none() { return; } // inert under FailFast (replay)
    self.now = Nanos(self.now.0.max(t.0));
    if !self.overflowed && self.config.collect_throughput_timeline {
        self.emit_decay_samples(self.now);
    }
    self.evict_samples();
    self.evict_dead_connections();
}
```

2. `emit_decay_samples(now)`: for each connection index, if its trailing window at `now` is non-empty in a direction that has sent data, push a `ThroughputSample`/`InFlightSample` at `now` (reuse `collect_throughput_points`/`collect_inflight_points` but stamped at `now` rather than a segment ts). Factor a helper that snapshots a synthetic time. Guard on `metrics.throughput_at(dir, now, &cfg)` returning `Some` (it already computes decay as bytes age out); skip a connection idle past `dead_after` (leave it for `evict_dead_connections`).

3. `evict_dead_connections`:

```rust
/// Removes connections that are terminal (Closed/Reset) or idle past `dead_after`, and whose
/// last activity precedes the eviction horizon, then rebuilds the `live` index. Bounds the
/// tracked connection count under churn (spec §2, criterion 17).
fn evict_dead_connections(&mut self) {
    let Some(window) = self.config.retention.window() else { return };
    let horizon = self.now.0.saturating_sub(window.0);
    let dead_after = self.config.dead_after.0;
    let now = self.now.0;
    let before = self.conns.len();
    self.conns.retain(|c| {
        let terminal = matches!(c.state, ConnState::Closed | ConnState::Reset);
        let idle = now.saturating_sub(c.last_at.0) > dead_after;
        !((terminal || idle) && c.last_at.0 < horizon)
    });
    if self.conns.len() != before {
        // Rebuild the pair->index map; drop stale next_instance entries for absent pairs.
        self.live.clear();
        for (i, c) in self.conns.iter().enumerate() {
            self.live.insert(c.id.pair, i); // last instance of a pair wins the live slot
        }
        // decrement collected_samples by the removed connections' retained sample counts:
        self.recount_collected();
    }
}
```

Add `recount_collected` that sets `self.collected_samples` to the sum of all surviving connections' six deque lengths — the simplest correct way to keep the counter consistent after bulk removal. (O(N) on an eviction pass; ticks are infrequent.)

> **Index-stability note (challenge iteration 3):** `live` stores indices into `conns`; `conns.retain` shifts them, so `live` MUST be rebuilt after any removal, as above. Do not `swap_remove` without rebuilding.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p tcpvisr-engine`
Expected: PASS — decay + whole-connection eviction green; replay tests unaffected (Tick inert under FailFast).

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(engine): Tick-driven decay and whole-connection eviction (live)

Under Evict, a Tick advances now, emits decay samples for still-active flows,
and evicts terminal/idle connections past the horizon (rebuilding the live
index) so connection count stays bounded. Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: Engine — non-consuming `snapshot()` + `Timeline::with_seq_ending` + accessors

**Files:**
- Modify: `crates/tcpvisr-engine/src/timeline.rs` (additive constructor)
- Modify: `crates/tcpvisr-engine/src/tracker.rs` (`snapshot`, `now`, `retention_horizon`)

**Interfaces:**
- Produces:
  ```rust
  impl Timeline { pub fn with_seq_ending(conns: Vec<ConnSeries>, end: Nanos) -> Self; }
  impl Tracker {
      pub fn snapshot(&self) -> Timeline;         // non-consuming, infallible; open conns end at now
      pub fn now(&self) -> Nanos;
      pub fn retention_horizon(&self) -> Nanos;   // now - window, clamped >= 0; == now under FailFast
  }
  ```

- [ ] **Step 1: Write the failing tests**

In `timeline.rs` `mod tests`:

```rust
#[test]
fn with_seq_ending_extends_open_conns_to_forced_end() {
    // open connection, last sample at 100, but forced end (live "now") = 500
    let c = conn(0, 0, 100, ConnState::Established);
    let id = c.id;
    let tl = Timeline::with_seq_ending(
        vec![(c, vec![ss(0, ConnState::Established, 0, 0)], vec![], vec![], vec![], vec![])],
        Nanos(500),
    );
    assert_eq!(tl.bounds().1, Nanos(500));
    assert_eq!(tl.active_at(Nanos(400)), vec![id], "open conn active up to forced end");
}
```

In `tracker.rs` `mod evict_tests`:

```rust
#[test]
fn snapshot_is_non_consuming_and_open_extends_to_now() {
    let (c, s) = (ep(1, 1234), ep(2, 80));
    let mut t = Tracker::new(evict_cfg(10_000_000_000));
    t.observe(&seg(c, s, TcpFlags::ACK, 100, 1, 10, 1_000));
    t.observe(&tcpvisr_core::Item::Tick(Nanos(9_000)));
    let snap = t.snapshot();
    assert_eq!(t.now(), Nanos(9_000));
    assert_eq!(snap.bounds().1, Nanos(9_000), "open conn interval extends to now");
    // non-consuming: a second snapshot still works and the tracker keeps ingesting.
    let _snap2 = t.snapshot();
    t.observe(&seg(c, s, TcpFlags::ACK, 110, 1, 10, 10_000));
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p tcpvisr-engine with_seq_ending` and `snapshot_is_non_consuming`
Expected: FAIL — methods undefined.

- [ ] **Step 3: Implement**

In `timeline.rs`, refactor `with_seq` to delegate:

```rust
pub fn with_seq(conns: Vec<ConnSeries>) -> Self {
    let end = conns.iter().map(|(c, _, _, _, _, _)| c.last_at).max().unwrap_or(Nanos(0));
    Self::with_seq_ending(conns, end)
}

/// Like `with_seq`, but forces the interval end (open connections extend to `end`). Live snapshots
/// pass the tracker's `now` so still-open connections stay active at the live cursor even during a
/// quiet period with no recent sample.
#[must_use]
pub fn with_seq_ending(conns: Vec<ConnSeries>, end: Nanos) -> Self {
    // (existing with_seq body, but use the `end` parameter instead of recomputing it)
}
```

Move the current `with_seq` body into `with_seq_ending`, taking `end` as a parameter (delete the local `let end = ...`). Keep `start`, sorting, `effective_end` logic identical (`effective_end = if closed { last_at } else { end }`).

In `tracker.rs`:

```rust
#[must_use]
pub fn now(&self) -> Nanos { self.now }

#[must_use]
pub fn retention_horizon(&self) -> Nanos {
    match self.config.retention.window() {
        Some(w) => Nanos(self.now.0.saturating_sub(w.0)),
        None => self.now,
    }
}

/// Non-consuming build of the current live `Timeline` from retained series. Infallible under
/// Evict (no ceiling). Open connections extend to `now`.
#[must_use]
pub fn snapshot(&self) -> Timeline {
    let series: Vec<ConnSeries> = self.conns.iter().map(|c| (
        c.view(),
        c.states.iter().copied().collect(),
        c.seq.iter().copied().collect(),
        c.inflight.iter().copied().collect(),
        c.rtt.iter().copied().collect(),
        c.throughput.iter().copied().collect(),
    )).collect();
    Timeline::with_seq_ending(series, self.now)
}
```

(`ConnSeries` is `Vec<_>`-typed, so collect the deques into `Vec`s.)

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p tcpvisr-engine`
Expected: PASS. Then `cargo clippy --all-targets --all-features -- -D warnings` clean.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(engine): non-consuming Tracker::snapshot + live accessors

with_seq_ending lets a live snapshot extend open connections to now; snapshot(),
now(), retention_horizon() expose the live cursor domain. Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 5: Ingest — `LiveOptions`, `LiveError`, `LiveCapture::open`, `list_interfaces`

**Files:**
- Create: `crates/tcpvisr-ingest/src/live.rs`
- Modify: `crates/tcpvisr-ingest/src/lib.rs` (add `#[cfg(feature = "live")] pub mod live;` and re-exports)

**Interfaces:**
- Produces (all `#[cfg(feature = "live")]`):
  ```rust
  pub struct LiveOptions { pub iface: String, pub filter: Option<String>, pub snaplen: i32, pub promisc: bool }
  impl Default for LiveOptions { /* snaplen 262_144, promisc false */ }
  pub enum LiveError { Open{iface,detail}, Activate{iface,detail}, Privilege{iface}, Filter{expr,detail}, UnsupportedLinkType{dlt:u16}, Interfaces{detail} }  // thiserror
  pub struct InterfaceInfo { pub name: String, pub description: Option<String> }
  pub fn list_interfaces() -> Result<Vec<InterfaceInfo>, LiveError>;
  pub struct LiveCapture { /* holds pcap::Capture<Active>, link: LinkType */ }
  impl LiveCapture { pub fn open(opts: &LiveOptions) -> Result<Self, LiveError>; pub fn link_type(&self) -> LinkType; }
  ```

- [ ] **Step 1: Write the failing test** (in `live.rs`, `#[cfg(test)]`)

`LiveError` Display messages are the CI-testable surface (open needs hardware). Test them:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn privilege_error_names_the_setcap_fix() {
        let e = LiveError::Privilege { iface: "eth0".into() };
        let msg = e.to_string();
        assert!(msg.contains("eth0"));
        assert!(msg.contains("cap_net_raw"), "names the setcap fix: {msg}");
    }

    #[test]
    fn default_options_full_snaplen_non_promiscuous() {
        let o = LiveOptions::default();
        assert_eq!(o.snaplen, 262_144);
        assert!(!o.promisc);
        assert!(o.filter.is_none());
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p tcpvisr-ingest --features live live::tests`
Expected: FAIL — module/types undefined.

- [ ] **Step 3: Implement** `live.rs`:

Module doc comment (live interface capture, M11, ADR-0003/0016). Define `LiveOptions` + `Default`. Define `LiveError` with `thiserror` messages, e.g.:

```rust
#[derive(Debug, thiserror::Error)]
pub enum LiveError {
    #[error("opening interface {iface} for capture: {detail}")]
    Open { iface: String, detail: String },
    #[error("activating capture on {iface}: {detail}")]
    Activate { iface: String, detail: String },
    #[error("insufficient privilege to capture on {iface}; grant it with \
             `sudo setcap cap_net_raw,cap_net_admin+eip $(command -v tcp-visr)` or run as root")]
    Privilege { iface: String },
    #[error("installing BPF filter `{expr}`: {detail}")]
    Filter { expr: String, detail: String },
    #[error("unsupported link type {dlt} on the interface (M1 supports Ethernet, SLL, SLL2, raw IP, null)")]
    UnsupportedLinkType { dlt: u16 },
    #[error("enumerating capture interfaces: {detail}")]
    Interfaces { detail: String },
}
```

`open`: `pcap::Capture::from_device(opts.iface.as_str())` → `.snaplen(opts.snaplen).promisc(opts.promisc).immediate_mode(true).timeout(READ_TIMEOUT_MS).precision(pcap::Precision::Nano)` then `.open()`. On open error, map a permission-denied detail (string contains "permission"/"Operation not permitted") to `LiveError::Privilege`, else `LiveError::Activate`. If `precision(Nano)` open fails specifically for precision, retry without it (µs fallback) — wrap in a helper `open_with_precision` that tries Nano then Micro. After open, `cap.get_datalink()` → `LinkType::from_dlt` or `UnsupportedLinkType`. If `opts.filter` is `Some`, `cap.filter(expr, true)` mapping errors to `LiveError::Filter`. Store the active capture + link. `list_interfaces`: `pcap::Device::list()` mapped to `InterfaceInfo`.

Define `const READ_TIMEOUT_MS: i32 = 100;`. Add `#[cfg(feature = "live")] pub mod live;` and `pub use live::{LiveCapture, LiveError, LiveOptions, InterfaceInfo, list_interfaces};` (feature-gated) to `lib.rs`.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p tcpvisr-ingest --features live live::tests`
Expected: PASS. Also `cargo build -p tcpvisr-ingest --features live` and `cargo clippy -p tcpvisr-ingest --features live --all-targets -- -D warnings` clean. (`cargo deny check` should still pass — `pcap` and its `libc` dep license ids are already allow-listed for the file-faucet; if `cargo deny check` warns about a *new* transitive license, add its SPDX id to `deny.toml`.)

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(ingest): live capture handle, options, and error mapping

LiveCapture::open (snaplen/promisc/immediate/nano-with-usec-fallback/BPF),
list_interfaces, and a LiveError that names the setcap fix for unprivileged
opens. Feature-gated. Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 6: Ingest — capture loop, Tick injection, timestamp normalization, silent-empty

**Files:**
- Modify: `crates/tcpvisr-ingest/src/live.rs`

**Interfaces:**
- Produces:
  ```rust
  pub enum LiveEvent { Item(Item), Name(NameObservation) }
  impl LiveCapture {
      // Consumes self; loops until `stop` is set or the handle ends. Calls `on_event` for each
      // decoded Segment/Name and injects Item::Tick on read timeout. Skips+counts per-packet
      // decode failures. Returns the final SkipCounts.
      pub fn run(self, on_event: impl FnMut(LiveEvent), stop: &std::sync::atomic::AtomicBool) -> SkipCounts;
  }
  // pure helper, unit-testable without hardware:
  pub(crate) fn normalize_ts(abs_ns: u64, baseline: &mut Option<u64>) -> Nanos;
  pub(crate) fn wall_now_ns() -> u64; // SystemTime -> ns since epoch (CLOCK_REALTIME, pcap's domain)
  ```

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn normalize_ts_is_relative_to_first_timestamp() {
    let mut base = None;
    assert_eq!(normalize_ts(1_000_000_500, &mut base), Nanos(0));
    assert_eq!(normalize_ts(1_000_000_900, &mut base), Nanos(400));
    // a later-arriving earlier stamp saturates to 0, never negative
    assert_eq!(normalize_ts(1_000_000_100, &mut base), Nanos(0));
}
```

(The `run` loop itself needs a live device; it is exercised by the local hardware run, not CI. The timestamp/baseline logic and the tick clock helper are the CI-testable seams.)

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p tcpvisr-ingest --features live normalize_ts`
Expected: FAIL — `normalize_ts` undefined.

- [ ] **Step 3: Implement**

`normalize_ts(abs_ns, baseline)`: `let base = *baseline.get_or_insert(abs_ns); Nanos(abs_ns.saturating_sub(base))` — same convention as the libpcap file faucet (`libpcap.rs`). `wall_now_ns()`: `SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0)` — but `as u64` trips the cast lint; use `u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)`.

`LiveEvent` enum. `run(self, mut on_event, stop)`:

```rust
let mut cap = self.cap;
let link = self.link;
let mut baseline: Option<u64> = None;
let mut skipped = SkipCounts::default();
while !stop.load(Ordering::Relaxed) {
    match cap.next_packet() {
        Ok(pkt) => {
            let sec = u64::try_from(pkt.header.ts.tv_sec).unwrap_or(0);
            let sub = u64::try_from(pkt.header.ts.tv_usec).unwrap_or(0);
            // nano precision packs ns in tv_usec; micro packs us. Normalize to ns by detecting
            // the precision recorded at open (store `nanos: bool` on LiveCapture): ns already, or us*1000.
            let abs_ns = sec * 1_000_000_000 + if self.nanos { sub } else { sub * 1_000 };
            let ts = normalize_ts(abs_ns, &mut baseline);
            match decode_frame(link, ts, pkt.data, pkt.header.len) {
                DecodeOutcome::Decoded(seg) => on_event(LiveEvent::Item(Item::Segment(seg))),
                DecodeOutcome::Names(obs) => for o in obs { on_event(LiveEvent::Name(o)); },
                DecodeOutcome::Skipped(reason) => skipped.record(reason),
            }
        }
        Err(pcap::Error::TimeoutExpired) => {
            let ts = normalize_ts(wall_now_ns(), &mut baseline);
            on_event(LiveEvent::Item(Item::Tick(ts)));
        }
        Err(_) => break, // handle ended / fatal read error
    }
}
skipped
```

Store `nanos: bool` on `LiveCapture` (set in `open` from which precision succeeded). Add `use std::sync::atomic::{AtomicBool, Ordering};`.

> Silent-empty/grace detection lives in the binary's `next_frame` (it owns wall time and the packet count); the faucet just emits events. See Task 9.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p tcpvisr-ingest --features live`
Expected: PASS. Clippy clean under `--features live`.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(ingest): live capture loop with tick injection

run() decodes via the shared decode_frame, normalizes timestamps to a
monotonic origin, and injects Item::Tick from the host wall clock on read
timeout so idle/decay advance. Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 7: TUI — `App` live fields, `retarget`, follow/freeze, `LiveStatus`

**Files:**
- Modify: `crates/tcpvisr-tui/src/transport.rs` (add `set_domain`)
- Modify: `crates/tcpvisr-tui/src/app.rs` (live fields, `retarget`, follow/freeze)
- Modify: `crates/tcpvisr-tui/src/keys.rs` (live `space`/seek)

**Interfaces:**
- Consumes: `Tracker::snapshot`/`now`/`retention_horizon` (Task 4).
- Produces:
  ```rust
  impl Transport { pub fn set_domain(&mut self, start: Nanos, end: Nanos); } // clamps cursor into [start,end]
  #[derive(Clone, Copy, Default)] pub struct LiveStatus { pub dropped: u64, pub approximate: bool }
  impl App {
      pub fn new_live(names: &NameTable, title: String) -> Self; // empty timeline, live mode, follow=true
      pub fn retarget(&mut self, timeline: Timeline, horizon: Nanos, now: Nanos, status: LiveStatus);
      pub fn is_live(&self) -> bool;
      pub fn is_following(&self) -> bool;
      pub fn toggle_follow(&mut self);        // live: follow<->freeze (used by space)
      pub fn live_status(&self) -> LiveStatus;
  }
  ```

- [ ] **Step 1: Write the failing tests** (in `app.rs` `mod tests`)

```rust
#[test]
fn retarget_reconciles_metas_and_follows_now() {
    let mut app = App::new_live(&NameTable::default(), "live".into());
    assert!(app.is_live() && app.is_following());
    // frame 1: one connection, now=100
    let a = full_conn(ep(1, 1), ep(2, 443), 0, 0, 100, ConnState::Established);
    let tl = Timeline::with_seq_ending(
        vec![(a, vec![ss(0, ConnState::Established, 10, 0)], vec![], vec![], vec![], vec![])],
        Nanos(100));
    app.retarget(tl, Nanos(0), Nanos(100), LiveStatus::default());
    assert_eq!(app.cursor(), Nanos(100), "following pins cursor to now");
    assert_eq!(app.visible().len(), 1);
    // frame 2: that connection is gone (evicted); a new one appears. Metas must reconcile.
    let b = full_conn(ep(3, 3), ep(4, 80), 0, 200, 300, ConnState::Established);
    let tl2 = Timeline::with_seq_ending(
        vec![(b, vec![ss(200, ConnState::Established, 5, 0)], vec![], vec![], vec![], vec![])],
        Nanos(300));
    app.retarget(tl2, Nanos(200), Nanos(300), LiveStatus { dropped: 7, approximate: true });
    let rows = app.visible();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].peer, ep(4, 80), "old connection's row is gone");
    assert_eq!(app.live_status().dropped, 7);
}

#[test]
fn freeze_clamps_cursor_to_horizon_and_does_not_follow() {
    let mut app = App::new_live(&NameTable::default(), "live".into());
    let a = full_conn(ep(1, 1), ep(2, 443), 0, 0, 1000, ConnState::Established);
    let mk = |end: u64| Timeline::with_seq_ending(
        vec![(a, vec![ss(0, ConnState::Established, 10, 0)], vec![], vec![], vec![], vec![])], Nanos(end));
    app.retarget(mk(1000), Nanos(0), Nanos(1000), LiveStatus::default());
    app.toggle_follow(); // freeze at 1000
    assert!(!app.is_following());
    // now advances to 5000, horizon to 4000; frozen cursor (1000) is dragged to the horizon.
    app.retarget(mk(5000), Nanos(4000), Nanos(5000), LiveStatus::default());
    assert!(!app.is_following());
    assert_eq!(app.cursor(), Nanos(4000), "frozen cursor clamped up to the eviction horizon");
}
```

(`full_conn`, `ep`, `ss` already exist in the `app.rs` test module.)

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p tcpvisr-tui retarget_reconciles` and `freeze_clamps`
Expected: FAIL — `new_live`/`retarget`/`toggle_follow` undefined.

- [ ] **Step 3: Implement**

In `transport.rs`:

```rust
/// Resets the `[start, end]` domain (live: horizon..now), clamping the cursor into it.
pub fn set_domain(&mut self, start: Nanos, end: Nanos) {
    self.start = start;
    self.end = end;
    self.cursor = Nanos(self.cursor.0.clamp(start.0, end.0));
}
```

In `app.rs`: add fields `is_live: bool`, `follow: bool`, `live_status: LiveStatus` to `App`; define `LiveStatus`. `new_live` builds an empty `Timeline::new(vec![])` app (reuse `new_with_names` internals) with `is_live: true, follow: true`. Add a helper `rebuild_metas_for(&self, timeline)` refactored out of `new_with_names` that returns a fresh `HashMap<ConnId, ConnMeta>` given the names table. Keep a `names: NameTable` clone on `App` (cheap; bounded) so `retarget` can resolve new connections' hosts. `retarget`:

```rust
pub fn retarget(&mut self, timeline: Timeline, horizon: Nanos, now: Nanos, status: LiveStatus) {
    // reconcile metas to exactly the snapshot's connection set (add new, drop absent)
    let present: std::collections::HashSet<ConnId> = timeline.connections().map(|c| c.id).collect();
    self.metas.retain(|id, _| present.contains(id));
    for c in timeline.connections() {
        self.metas.entry(c.id).or_insert_with(|| Self::meta_for(c, &self.names));
    }
    self.timeline = timeline;
    self.live_status = status;
    self.transport.set_domain(horizon, now);
    if self.follow { self.transport.set_cursor(now); }
    self.reconcile_selection();
}
```

`meta_for(&Connection, &NameTable) -> ConnMeta` is the per-connection projection extracted from `new_with_names`. `toggle_follow` flips `follow`; when re-enabling, pin cursor to `end`. `is_live`/`is_following`/`live_status` accessors.

In `keys.rs`: when `app.is_live()`, `space` calls `toggle_follow()` (not `toggle_play`), and a manual `seek` sets `follow = false` (freeze on manual seek). Guard replay behavior behind `!is_live()`.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p tcpvisr-tui`
Expected: PASS — new live tests green; all replay `app`/`transport`/`keys` tests unchanged.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(tui): live App retarget with follow/freeze and meta reconcile

retarget swaps the snapshot Timeline, reconciles metas to the live connection
set, follows now or clamps a frozen cursor to the eviction horizon; space
toggles follow/freeze in live mode. Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 8: TUI — live status line render + `run_live` event loop

**Files:**
- Modify: `crates/tcpvisr-tui/src/render.rs` (status line: follow/freeze, dropped, approximate)
- Modify: `crates/tcpvisr-tui/src/run.rs` (`run_live`)
- Modify: `crates/tcpvisr-tui/src/lib.rs` (export `run_live`, `LiveStatus`)

**Interfaces:**
- Produces:
  ```rust
  // impure; sibling of `run`. Each frame: render, poll a key, handle it, then call next_frame to
  // pull the latest snapshot+status and retarget. Returns on quit or IO error; restores terminal.
  pub fn run_live(app: App, next_frame: impl FnMut(&mut App)) -> std::io::Result<()>;
  ```

- [ ] **Step 1: Write the failing test** (render is TestBackend-testable; the loop needs a TTY so is smoke-tested)

In `render.rs` tests (follow the existing `TestBackend` pattern in the crate):

```rust
#[test]
fn live_status_line_shows_dropped_and_frozen() {
    let mut app = App::new_live(&NameTable::default(), "live".into());
    let a = full_conn(ep(1, 1), ep(2, 443), 0, 0, 1000, ConnState::Established);
    let tl = Timeline::with_seq_ending(
        vec![(a, vec![ss(0, ConnState::Established, 10, 0)], vec![], vec![], vec![], vec![])], Nanos(1000));
    app.retarget(tl, Nanos(0), Nanos(1000), LiveStatus { dropped: 3, approximate: true });
    app.toggle_follow(); // frozen
    let buf = render_to_string(&app, 80, 24); // existing test helper
    assert!(buf.contains("FROZEN") || buf.contains("freeze"), "shows frozen state");
    assert!(buf.contains("dropped 3") || buf.contains("drop"), "shows drop count");
    assert!(buf.contains("approx"), "flags metrics approximate");
}
```

(If a `render_to_string` helper does not already exist, build the `TestBackend` inline as the other render tests do.)

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p tcpvisr-tui live_status_line`
Expected: FAIL — render has no live status yet.

- [ ] **Step 3: Implement**

In `render.rs`, in the status/header block, when `app.is_live()` add a segment: `▶ LIVE` when following or `⏸ FROZEN` when frozen; append `t=<now>` (or `t=<cursor>` when frozen); when `app.live_status().dropped > 0` append `dropped <n> (metrics approximate)`. Keep the replay footer/header intact for `!is_live()`.

In `run.rs`:

```rust
/// Live event loop: like `run`, but each frame pulls a fresh snapshot via `next_frame` instead of
/// advancing a precomputed timeline by wall-clock dt. The caller's closure owns the capture
/// channel drain, the tracker, snapshot construction, and `App::retarget`.
pub fn run_live(mut app: App, mut next_frame: impl FnMut(&mut App)) -> std::io::Result<()> {
    let mut terminal = ratatui::init();
    let result = live_loop(&mut terminal, &mut app, &mut next_frame);
    ratatui::restore();
    result
}

fn live_loop(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    next_frame: &mut impl FnMut(&mut App),
) -> std::io::Result<()> {
    loop {
        terminal.draw(|frame| render(frame, app))?;
        if event::poll(TICK)? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press && handle_key(app, key) == Outcome::Quit {
                    break;
                }
            }
        }
        next_frame(app);
    }
    Ok(())
}
```

Export `run_live` and `LiveStatus` from `lib.rs`.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p tcpvisr-tui`
Expected: PASS. Clippy clean.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(tui): live status line and run_live event loop

run_live renders + polls keys + pulls a fresh snapshot per frame via a caller
closure; the status line shows follow/freeze, drop count, and the approximate
flag. Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 9: CLI — `live` subcommand, wiring, RAII teardown, drop counting, feature-off

**Files:**
- Modify: `crates/tcp-visr/src/main.rs`

**Interfaces:**
- Consumes: `LiveCapture`/`LiveOptions`/`list_interfaces`/`LiveEvent` (Tasks 5–6, feature-gated), `Tracker` Evict + `snapshot`/`now`/`retention_horizon` (Tasks 1–4), `App::new_live`/`retarget` + `run_live` (Tasks 7–8).
- Produces: `Command::Live { iface: Option<String>, filter: Option<String>, retention_secs: u64, list_interfaces: bool }`; a `run_live` binary fn (feature-gated) and a feature-off stub returning a clear error.

- [ ] **Step 1: Write the failing tests**

CLI parsing + the feature-off error are the CI-testable seams. Add a `#[cfg(test)]` block in `main.rs`:

```rust
#[test]
fn live_requires_iface_unless_listing() {
    use clap::Parser;
    let cli = Cli::try_parse_from(["tcp-visr", "live", "-i", "eth0", "--retention-secs", "60"]).unwrap();
    match cli.command {
        Some(Command::Live { iface, retention_secs, .. }) => {
            assert_eq!(iface.as_deref(), Some("eth0"));
            assert_eq!(retention_secs, 60);
        }
        _ => panic!("expected Live"),
    }
}

#[test]
fn live_defaults_retention_to_120s() {
    use clap::Parser;
    let cli = Cli::try_parse_from(["tcp-visr", "live", "-i", "eth0"]).unwrap();
    match cli.command {
        Some(Command::Live { retention_secs, .. }) => assert_eq!(retention_secs, 120),
        _ => panic!("expected Live"),
    }
}
```

Add a backpressure unit test for the drop-counting channel drain. Factor the drain into a pure helper `drain_into(rx, tracker, names, dropped) -> ()` and test that a full/stalled bounded channel increments `dropped` without blocking. (Drive `sync_channel(1)` directly; no NIC.)

```rust
#[test]
fn bounded_channel_full_send_is_dropped_and_counted() {
    use std::sync::mpsc::sync_channel;
    let (tx, _rx) = sync_channel::<u8>(1);
    let mut dropped = 0u64;
    assert!(tx.try_send(1).is_ok());
    // second send with a full buffer (rx not draining) -> would-block -> drop + count
    if tx.try_send(2).is_err() { dropped += 1; }
    assert_eq!(dropped, 1);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p tcp-visr live_requires_iface` (and the others)
Expected: FAIL — `Command::Live` still the unit variant.

- [ ] **Step 3: Implement**

Replace `Live,` in the `Command` enum with:

```rust
/// Capture live from a network interface (requires the `live` feature).
Live {
    /// Interface to capture on (e.g. eth0). Omit with --list-interfaces.
    #[arg(short = 'i', long)]
    iface: Option<String>,
    /// Optional BPF filter expression.
    #[arg(long)]
    filter: Option<String>,
    /// Display/eviction window in seconds.
    #[arg(long, default_value_t = 120)]
    retention_secs: u64,
    /// List capturable interfaces and exit.
    #[arg(long)]
    list_interfaces: bool,
},
```

Update `Command::name` and the `run` match arm. Under `#[cfg(feature = "live")]`, implement `run_live_cmd(iface, filter, retention_secs, list_interfaces)`:
- `--list-interfaces` → `tcpvisr_ingest::list_interfaces()` → print `name  description`, return Ok.
- else require `iface` (error "live requires -i <iface> (or --list-interfaces)").
- Build `LiveOptions { iface, filter, ..Default::default() }`; `LiveCapture::open(&opts)?`.
- `sync_channel::<LiveEvent>(CHANNEL_CAP)` (e.g. 65_536); `Arc<AtomicBool>` stop; `Arc<AtomicU64>` dropped.
- Spawn the capture thread: `cap.run(|ev| { if tx.try_send(ev).is_err() { dropped.fetch_add(1, Relaxed); } }, &stop)`.
- Build the main-thread `Tracker` with `EngineConfig { retention: Evict { window: retention_secs*1e9, max_samples: 2_000_000 }, collect_* all true, series_collection: All, ..default }` and a `NameTable`; `App::new_live(&names, title)`.
- Guard: `struct CaptureGuard { stop: Arc<AtomicBool>, handle: Option<JoinHandle<_>> }` with `Drop` setting `stop` + joining. Own it for the capture lifetime so panic/unwind tears down.
- `run_live(app, |app| { drain the channel via try_recv into tracker+names, count drops from the atomic, track packet count + first-packet time for the 5s grace advisory, build `tracker.snapshot()`, then `app.retarget(snapshot, tracker.retention_horizon(), tracker.now(), LiveStatus{ dropped, approximate: dropped>0 })` })`.
- Grace advisory: if no `LiveEvent::Item(Segment)` seen within 5s of start, set a status hint (e.g. fold into `LiveStatus` or the title) — non-fatal; clears on first packet. Keep it simple: track `first_packet_seen: bool` and `start: Instant`; surface the hint in the title until the first packet.

Under `#[cfg(not(feature = "live"))]`, the arm returns `Err("live capture: this binary was built without live support; rebuild with --features live".into())`.

Note: `Item::Tick` still flows through the channel as a `LiveEvent::Item(Item::Tick)`.

- [ ] **Step 4: Run to verify pass + full gate**

Run:
```
cargo test -p tcp-visr
cargo build --workspace                       # default (no live) builds
cargo run -p tcp-visr -- live 2>&1 | head     # without --features live -> clear error
cargo test --workspace && cargo test -p tcpvisr-ingest --features live
cargo fmt --all --check && cargo clippy --all-targets --all-features -- -D warnings && cargo deny check
```
Expected: all green; the feature-off `live` prints the "built without live support" error.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(cli): live subcommand wiring capture thread to the live TUI

tcp-visr live -i/--filter/--retention-secs/--list-interfaces; bounded channel
with drop counting, an RAII guard that stops+joins the capture thread on exit
or panic, and a feature-off error. Closes the M11 path. Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 10: Docs — mark M11 implemented; refresh current-state

**Files:**
- Modify: `docs/design/tcp-visr-design.md` (roadmap M11 row note, as prior milestones did)
- Modify: `CLAUDE.md` (the "Current state" paragraph: M0–M11 implemented; `live` now works)

- [ ] **Step 1: Update the design roadmap** — mark M11 implemented consistent with how M10 was marked (the recent commit `docs: mark M10 ... implemented`); note the un-CI-tested surface is verified by a documented local hardware run.

- [ ] **Step 2: Update `CLAUDE.md`** — change "milestones M0–M10 are implemented" to include M11; update the working-subcommands sentence to include `live` (interface capture, BPF filter, ns timestamps, bounded retention with eviction, follow/freeze); note live kernel enrichment (M12) and live reverse-DNS remain deferred.

- [ ] **Step 3: Commit**

```bash
git add docs/design/tcp-visr-design.md CLAUDE.md
git commit -m "docs: mark M11 (live capture) implemented

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Verification (before push — work-issue step 7)

Run the **full** gate, not just focused tests:
```
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace
cargo test -p tcpvisr-ingest --features live
cargo deny check
```
Then a **documented local hardware run** (not CI), stated as a limitation in the PR:
```
sudo setcap cap_net_raw,cap_net_admin+eip "$(cargo build -q --features live && echo target/debug/tcp-visr)"
target/debug/tcp-visr live -i lo &   # in another shell: curl/nc over loopback
# confirm: rows appear, follow/freeze (space), seek-to-horizon (←/→), throughput decay on silence,
# --list-interfaces lists lo, and an unprivileged run prints the setcap message (not silent-empty).
```

## Self-review (spec coverage)

- Faucet open/BPF/ns/µs, list-interfaces, privilege error → Tasks 5, 9 (criteria 1–3).
- Silent-empty advisory → Task 9 grace hint (criterion 4).
- Monotonic-origin ts + timeout Tick → Task 6 (criterion 5).
- Window eviction + states-keeps-latest → Task 2 (criterion 6).
- Baseline survives eviction → Tasks 2–3 (criterion 7); whole-connection eviction → Task 3 (criterion 17).
- max_samples evict-vs-fail-fast → Tasks 1–2 (criterion 8).
- Tick decay + idle-drop → Task 3 (criterion 9).
- snapshot non-consuming + equivalence → Task 4 (criterion 10).
- retarget preserves UI + cursor follow/clamp → Task 7 (criterion 11); live space/seek/clamp → Task 7 (criterion 12).
- Status drop/approximate → Task 8 (criterion 13); backpressure drop-count → Task 9 (criterion 14).
- Feature-off error + libpcap-free default → Task 9 (criterion 15).
- q/Ctrl-C + panic teardown (RAII) → Task 9 (criterion 16).
