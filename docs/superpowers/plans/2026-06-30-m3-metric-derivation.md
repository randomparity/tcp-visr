# M3 — Metric Derivation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Derive the per-connection metric time series (in-flight, throughput,
retransmit/OOO/SACK, Karn-paired RTT) on top of M2's tracker and dump one connection's series
as JSON via `tcp-visr metrics <FILE> --conn N`.

**Architecture:** `tcpvisr-core` gains a pure, dependency-free `MetricSample`/`SampleDir`.
`tcpvisr-engine` (pure, no I/O) folds per-direction derivation state into each tracked
connection — sequence frontier with SYN/FIN phantom bytes, in-flight via RFC 1982 serial
arithmetic, reorder-window retransmit/OOO split, conservative Karn RTT, and a frozen
trailing-window throughput — collected behind a `SeriesCollection` switch (`None`/`All`/
`Only(ConnId)`) and finalized by `into_metrics`, capped by a `max_samples` ceiling. The
`metrics` CLI resolves the target connection in a lifecycle-only pass, then re-runs collecting
only that connection, and serializes it with serde-derived CLI-local DTOs.

**Tech Stack:** Rust 1.88.0 (edition 2024). New **runtime** deps in the `tcp-visr` CLI only:
`serde` (derive) and `serde_json`, pinned `=`. Engine gains `thiserror` `=2` (already in the
workspace lock via ingest) for one error type. Dev-only: `proptest` `=1.11.0` (engine),
`etherparse` `=0.20.2` (tcp-visr fixtures) — both already in the lock.

Derived from [the M3 spec](../../milestones/m03-metric-derivation/spec.md) and
[ADR-0007](../../adr/0007-metric-derivation-model.md).

## Global Constraints

- **Toolchain = MSRV = 1.88.0**; edition 2024; pin every new dep with `=`.
- **Lint policy (workspace `[lints]`)**: no `unwrap`/`expect`/`panic!`/`println!`/`eprintln!`/
  `process::exit`/`#[allow]`/`todo!`/`dbg!` in non-test code. `clippy::pedantic` is `warn`; CI
  runs `-D warnings`, so pedantic findings must be resolved. Restriction lints are relaxed in
  `#[test]` bodies via `clippy.toml`; a non-`#[test]` test **helper** still needs a
  module-level `#![allow(...)]` (see M1/M2 `tests/support`, `tests/conns.rs`).
- **No stdout macros**: serialize via `serde_json::to_writer_pretty(io::stdout().lock(), …)`,
  not `println!`. `print_stdout`/`print_stderr` are denied.
- **All TCP sequence arithmetic uses `tcpvisr_core::TcpSeq`** (RFC 1982) — never naive `u32`
  subtraction. `seq_end = seq + payload_len + SYN + FIN` uses `wrapping_add`. "earlier serial-≤
  later" is `earlier.serial_lt(later) || earlier == later`. Forward distance is
  `later.serial_diff(earlier)`.
- **Time is non-monotonic** (design §14): every `Nanos` subtraction uses `saturating_sub`.
- **`throughput_window` must be `> 0`**: the engine divides defensively (0 ⇒ `throughput_bps =
  0`, never a division panic); the CLI rejects `--throughput-window-ms 0`.
- **Engine and core stay pure** (ADR-0002): no I/O, no clock, **no serde**. serde/serde_json
  live only in the `tcp-visr` binary.
- **No M2 regressions**: `Connection`, `ConnState`, `EndpointPair`, `ConnId`,
  `Tracker::observe`, `Tracker::into_connections`, the `conns` command, and every M2
  test/fixture stay byte-for-byte unchanged. M3 only **adds** API.
- **Conventional Commits**, imperative ≤72-char subject, every commit body ends with
  `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- **Guardrails before every commit** (libpcap-dev must be installed for `--all-features`):
  `cargo fmt --all --check` · `cargo clippy --all-targets --all-features -- -D warnings` ·
  `cargo test --workspace`. For the final push also `cargo test -p tcpvisr-ingest --features
  live` and `cargo deny check`.

---

## File Structure

```
crates/tcpvisr-core/
  src/lib.rs            # + pub mod metric; pub use metric::{MetricSample, SampleDir}
  src/metric.rs         # MetricSample, SampleDir (NEW, pure, no deps)
crates/tcpvisr-engine/
  Cargo.toml            # + [dependencies] thiserror = "=2.0.17"
  src/lib.rs            # + pub mod metrics; re-export ConnectionMetrics, MetricError, SeriesCollection
  src/config.rs         # + SeriesCollection, throughput_window, reorder_window, max_samples
  src/metrics.rs        # DirState, MetricState, seq_end, derivation, ConnectionMetrics, MetricError (NEW)
  src/conn.rs           # ConnTrack gains `metrics: MetricState` (internal)  [Modify]
  src/tracker.rs        # observe path calls metrics.observe; into_metrics() finalizer  [Modify]
crates/tcp-visr/
  Cargo.toml            # + [dependencies] serde (derive), serde_json (=)
  src/main.rs           # Metrics struct variant; run_metrics; JSON DTOs  [Modify]
  tests/support/mod.rs  # + tcp_with_sack(), metrics fixture_set entries  [Modify]
  tests/metrics.rs      # CLI/oracle integration tests (NEW)
  tests/drift.rs        # + oracle-golden drift guard  [Modify]
  tests/fixtures/*.pcap  # metrics_basic, metrics_retransmit, metrics_ooo, metrics_sack (NEW; seq_wrap reused)
  tests/oracle/*.metrics.json  # committed goldens (NEW)
  tests/oracle/README.md       # hand-derived golden derivations (NEW)
deny.toml                # + license allow-list entries for serde's tree  [Modify]
```

Note: `tcpvisr-engine/src/conn.rs` currently defines `ConnTrack`? No — `ConnTrack` lives in
`tracker.rs`. `conn.rs` defines `Connection`, `ConnId`, `EndpointPair`, `Direction`. The
metric state is added to `ConnTrack` in `tracker.rs`; `MetricState`/`DirState`/derivation live
in the new `metrics.rs`. Keep `conn.rs` unchanged except adding `ConnectionMetrics` is in
`metrics.rs`, not `conn.rs`.

---

### Task 1: core `MetricSample` + `SampleDir`

**Files:**
- Create: `crates/tcpvisr-core/src/metric.rs`
- Modify: `crates/tcpvisr-core/src/lib.rs`

**Interfaces:**
- Consumes: `crate::time::Nanos`.
- Produces: `MetricSample { t: Nanos, dir: SampleDir, in_flight_bytes: u64, throughput_bps: u64,
  rtt: Option<Nanos>, retransmit: bool, out_of_order: bool, sack: bool }` (derives
  `Debug, Clone, Copy, PartialEq, Eq`); `SampleDir { OriginToResponder, ResponderToOrigin }`
  (derives `Debug, Clone, Copy, PartialEq, Eq`).

- [ ] **Step 1: Write the failing test**

Append to `crates/tcpvisr-core/src/metric.rs` (create the file with the test + types together;
write the test first conceptually, then the types in step 3):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::Nanos;

    #[test]
    fn sample_is_copy_and_holds_fields() {
        let s = MetricSample {
            t: Nanos(1_000),
            dir: SampleDir::OriginToResponder,
            in_flight_bytes: 50,
            throughput_bps: 400,
            rtt: Some(Nanos(2_000)),
            retransmit: false,
            out_of_order: false,
            sack: true,
        };
        let copy = s; // Copy, not move
        assert_eq!(copy, s);
        assert_eq!(copy.dir, SampleDir::OriginToResponder);
        assert_ne!(SampleDir::OriginToResponder, SampleDir::ResponderToOrigin);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p tcpvisr-core metric`
Expected: FAIL — `cannot find type MetricSample`.

- [ ] **Step 3: Write minimal implementation**

Prepend to `crates/tcpvisr-core/src/metric.rs` (above the test module):

```rust
//! The per-event metric sample (design §4, ADR-0007). Pure, dependency-free; JSON lives in
//! the CLI. One sample is produced per processed `Segment` (design §4.1).

use crate::time::Nanos;

/// The direction of the segment that produced a sample, relative to the connection's origin
/// (ADR-0006). Directional sample fields pertain to this direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleDir {
    OriginToResponder,
    ResponderToOrigin,
}

/// One metric sample (design §4). `in_flight_bytes`, `throughput_bps`, `retransmit`, and
/// `out_of_order` pertain to `dir`; `rtt` is a round-trip measurement carried by the
/// acknowledging segment; `sack` reflects the triggering segment's own options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetricSample {
    pub t: Nanos,
    pub dir: SampleDir,
    pub in_flight_bytes: u64,
    pub throughput_bps: u64,
    pub rtt: Option<Nanos>,
    pub retransmit: bool,
    pub out_of_order: bool,
    pub sack: bool,
}
```

Then add to `crates/tcpvisr-core/src/lib.rs` after `pub mod flow;` (keep modules alphabetical
as in the existing file ordering — insert `pub mod metric;` after `pub mod flow;`):

```rust
pub mod metric;
```

and in the `pub use` block add:

```rust
pub use metric::{MetricSample, SampleDir};
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p tcpvisr-core metric`
Expected: PASS.

- [ ] **Step 5: Format, lint, commit**

```bash
cargo fmt --all --check
cargo clippy -p tcpvisr-core --all-targets -- -D warnings
git add crates/tcpvisr-core/src/metric.rs crates/tcpvisr-core/src/lib.rs
git commit -m "feat(core): add MetricSample and SampleDir types

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: engine config knobs + `MetricError` + `SeriesCollection`

**Files:**
- Modify: `crates/tcpvisr-engine/Cargo.toml`
- Modify: `crates/tcpvisr-engine/src/config.rs`
- Create: `crates/tcpvisr-engine/src/metrics.rs` (error + collection enum only in this task;
  derivation added in Task 3)
- Modify: `crates/tcpvisr-engine/src/lib.rs`

**Interfaces:**
- Consumes: `crate::conn::ConnId`, `tcpvisr_core::Nanos`.
- Produces:
  - `SeriesCollection { None, All, Only(ConnId) }` (derives `Debug, Clone, Copy, PartialEq, Eq`;
    `Default` = `None`).
  - `EngineConfig` extended with `series_collection: SeriesCollection`, `throughput_window:
    Nanos`, `reorder_window: Nanos`, `max_samples: usize`. `Default`: `None`,
    `Nanos(1_000_000_000)`, `Nanos(3_000_000)`, `10_000_000`. **`EngineConfig` stays `Copy`**
    (every field is `Copy`; `SeriesCollection` is `Copy` because `ConnId` is `Copy`).
  - `MetricError::SampleCeiling { samples: usize, limit: usize }` (`thiserror::Error`).

- [ ] **Step 1: Write the failing test**

Append to `crates/tcpvisr-engine/src/config.rs` test module (create one if absent):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::SeriesCollection;
    use tcpvisr_core::Nanos;

    #[test]
    fn defaults_match_spec() {
        let c = EngineConfig::default();
        assert_eq!(c.series_collection, SeriesCollection::None);
        assert_eq!(c.throughput_window, Nanos(1_000_000_000));
        assert_eq!(c.reorder_window, Nanos(3_000_000));
        assert_eq!(c.max_samples, 10_000_000);
        // M2 defaults unchanged:
        assert_eq!(c.dead_after, Nanos(120_000_000_000));
        assert_eq!(c.reset_threshold, 1 << 30);
    }

    #[test]
    fn config_is_copy() {
        let c = EngineConfig::default();
        let d = c; // Copy
        assert_eq!(c, d);
    }
}
```

And in a new `crates/tcpvisr-engine/src/metrics.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_ceiling_error_names_count_limit_and_flag() {
        let e = MetricError::SampleCeiling { samples: 11, limit: 10 };
        let msg = e.to_string();
        assert!(msg.contains("11"), "{msg}");
        assert!(msg.contains("10"), "{msg}");
        assert!(msg.contains("--max-samples"), "{msg}");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p tcpvisr-engine config:: metrics::`
Expected: FAIL — unresolved `SeriesCollection`, `MetricError`, and missing config fields.

- [ ] **Step 3: Write minimal implementation**

Add the engine runtime dep. In `crates/tcpvisr-engine/Cargo.toml`, add under
`[dependencies]` (after the `tcpvisr-core` line):

```toml
thiserror = "=2.0.17"
```

Create `crates/tcpvisr-engine/src/metrics.rs` (prepend above its test module):

```rust
//! Metric derivation on top of the M2 tracker (design §10.M3, ADR-0007). Pure: no I/O, no
//! serde; one `MetricSample` per processed `Segment`.

use crate::conn::ConnId;

/// Which tracked instances buffer a `MetricSample` series.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SeriesCollection {
    /// Derive only lifecycle/scalar state; store no samples (the `conns` path).
    #[default]
    None,
    /// Every instance buffers a series.
    All,
    /// Only the named instance buffers a series (the `metrics --conn N` path).
    Only(ConnId),
}

/// Whole-derivation failures (design §7). Per-segment problems are never errors.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MetricError {
    #[error(
        "metric series exceeded the sample ceiling ({samples} samples > {limit}); \
         raise it with --max-samples or analyze a smaller capture"
    )]
    SampleCeiling { samples: usize, limit: usize },
}
```

Extend `crates/tcpvisr-engine/src/config.rs`. Replace the struct and `Default` impl:

```rust
//! Engine tuning knobs (design §10.M2/§10.M3, ADR-0006, ADR-0007).

use tcpvisr_core::Nanos;

use crate::metrics::SeriesCollection;

/// Connection-tracker + metric-derivation configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineConfig {
    /// Idle gap after which a fresh SYN on the same pair starts a new instance.
    pub dead_after: Nanos,
    /// Minimum backward serial distance that reads as a fresh-ISN reset. Must be `< 2^31`.
    pub reset_threshold: u32,
    /// Which instances buffer a metric series (M3).
    pub series_collection: SeriesCollection,
    /// Trailing window for `throughput_bps`; must be `> 0` (the engine divides defensively).
    pub throughput_window: Nanos,
    /// A behind-frontier data segment within this inter-arrival gap is out-of-order, else a
    /// retransmit.
    pub reorder_window: Nanos,
    /// Ceiling on retained samples across the collected series; exceeding it fails fast.
    pub max_samples: usize,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            dead_after: Nanos(120_000_000_000),
            reset_threshold: 1 << 30,
            series_collection: SeriesCollection::None,
            throughput_window: Nanos(1_000_000_000),
            reorder_window: Nanos(3_000_000),
            max_samples: 10_000_000,
        }
    }
}
```

Add the module + re-exports to `crates/tcpvisr-engine/src/lib.rs` (insert `pub mod metrics;`
after `pub mod config;` and add to the `pub use` lines):

```rust
pub mod metrics;
```
```rust
pub use metrics::{MetricError, SeriesCollection};
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p tcpvisr-engine config:: metrics::`
Expected: PASS.

- [ ] **Step 5: Verify the new dep is license-clean and commit**

```bash
cargo deny check
cargo fmt --all --check
cargo clippy -p tcpvisr-engine --all-targets --all-features -- -D warnings
git add crates/tcpvisr-engine/Cargo.toml crates/tcpvisr-engine/src/config.rs \
        crates/tcpvisr-engine/src/metrics.rs crates/tcpvisr-engine/src/lib.rs Cargo.lock
git commit -m "feat(engine): add M3 config knobs, SeriesCollection, MetricError

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

`thiserror` is already in the workspace lock (ingest uses it), so `cargo deny check` needs no
new license entry. If `cargo deny check` reports a missing license, stop and add the SPDX id to
`deny.toml` before committing.

---

### Task 3: metric derivation state + per-segment derivation

**Files:**
- Modify: `crates/tcpvisr-engine/src/metrics.rs`

**Interfaces:**
- Consumes: `tcpvisr_core::{MetricSample, SampleDir, Nanos, Segment, TcpSeq}`,
  `crate::conn::Direction`, `crate::config::EngineConfig`.
- Produces:
  - `pub(crate) struct MetricState` with `fn new() -> Self` and
    `fn observe(&mut self, seg: &Segment, dir: Direction, cfg: &EngineConfig) -> MetricSample`.
  - `pub(crate) fn seq_end(seg: &Segment) -> TcpSeq`.

This task is pure derivation logic with hand-built `Segment`s — no `Tracker` wiring yet (Task 4).

- [ ] **Step 1: Write the failing tests**

Add a test-support helper + the behavior tests to `crates/tcpvisr-engine/src/metrics.rs`. The
helper is a non-`#[test]` fn, so guard the test module with a module-level allow:

```rust
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // test helpers; restriction lints relaxed
mod derive_tests {
    use super::*;
    use core::net::{IpAddr, Ipv4Addr};
    use tcpvisr_core::{FlowKey, Nanos, Segment, TcpFlags, TcpOptions, TcpSeq};
    use crate::conn::Direction;

    fn ip(o: u8) -> IpAddr { IpAddr::V4(Ipv4Addr::new(10, 0, 0, o)) }

    // Build a segment in O2R (origin 10.0.0.1:1234 -> responder 10.0.0.2:80) or R2O.
    fn seg(flags: u16, seq: u32, ack: u32, len: u32, ts: u64, sack: bool) -> Segment {
        let mut options = TcpOptions::default();
        if sack {
            options.sack_blocks.push((TcpSeq(1), TcpSeq(2)));
        }
        Segment {
            ts: Nanos(ts),
            flow: FlowKey { src_ip: ip(1), src_port: 1234, dst_ip: ip(2), dst_port: 80 },
            seq: TcpSeq(seq), ack: TcpSeq(ack), flags: TcpFlags(flags),
            window: 0, options, payload_len: len,
        }
    }

    const ACK: u16 = TcpFlags::ACK;
    const SYN: u16 = TcpFlags::SYN;

    fn cfg() -> EngineConfig { EngineConfig::default() }

    #[test]
    fn in_flight_grows_with_sent_bytes_and_drains_on_ack() {
        let mut m = MetricState::new();
        let c = cfg();
        // O2R data: 10 bytes, no ack seen for o2r yet.
        let s1 = m.observe(&seg(ACK, 100, 1, 10, 1_000, false), Direction::OriginToResponder, &c);
        assert_eq!(s1.in_flight_bytes, 10);
        assert_eq!(s1.dir, SampleDir::OriginToResponder);
        // R2O segment acks o2r up to 110 -> o2r drained, but THIS sample reports r2o's own.
        let s2 = m.observe(&seg(ACK, 1, 110, 0, 2_000, false), Direction::ResponderToOrigin, &c);
        assert_eq!(s2.in_flight_bytes, 0, "r2o sender has nothing outstanding");
        // Next o2r data shows the drained base: send 5 more from 110.
        let s3 = m.observe(&seg(ACK, 110, 1, 5, 3_000, false), Direction::OriginToResponder, &c);
        assert_eq!(s3.in_flight_bytes, 5, "ack=110 drained the first 10");
    }

    #[test]
    fn in_flight_is_serial_correct_across_u32_wrap() {
        let mut m = MetricState::new();
        let c = cfg();
        let s1 = m.observe(&seg(ACK, u32::MAX - 100, 1, 50, 1_000, false),
                           Direction::OriginToResponder, &c);
        assert_eq!(s1.in_flight_bytes, 50);
        let s2 = m.observe(&seg(ACK, 200, 1, 50, 2_000, false),
                           Direction::OriginToResponder, &c);
        assert_eq!(s2.in_flight_bytes, 351, "serial diff across the wrap, not a naive subtraction");
    }

    #[test]
    fn ack_before_any_data_in_acked_direction_yields_no_rtt_and_no_advance() {
        let mut m = MetricState::new();
        let c = cfg();
        // First segment is o2r data+ACK=1; r2o has no tracked send -> ack acks nothing.
        let s1 = m.observe(&seg(ACK, 5000, 1, 50, 1_000, false),
                           Direction::OriginToResponder, &c);
        assert_eq!(s1.rtt, None);
        assert_eq!(s1.in_flight_bytes, 50);
    }

    #[test]
    fn rtt_pairs_oldest_acked_send_under_karn() {
        let mut m = MetricState::new();
        let c = cfg();
        // o2r sends A(seq 100,len 100) @1000 and B(seq 200,len 100) @2000.
        m.observe(&seg(ACK, 100, 1, 100, 1_000, false), Direction::OriginToResponder, &c);
        m.observe(&seg(ACK, 200, 1, 100, 2_000, false), Direction::OriginToResponder, &c);
        // r2o cumulative ACK=300 @5000 acks both; RTT pairs the oldest (A @1000).
        let s = m.observe(&seg(ACK, 1, 300, 0, 5_000, false), Direction::ResponderToOrigin, &c);
        assert_eq!(s.rtt, Some(Nanos(4_000)));
    }

    #[test]
    fn karn_drops_rtt_for_retransmitted_range() {
        let mut m = MetricState::new();
        let c = cfg(); // reorder_window = 3ms = 3_000_000 ns
        m.observe(&seg(ACK, 100, 1, 100, 1_000, false), Direction::OriginToResponder, &c); // A @1us
        // Retransmit of A after a gap >= reorder_window (3ms): gap = 3_001_000 - 1_000 = 3_000_000.
        let r = m.observe(&seg(ACK, 100, 1, 100, 3_001_000, false),
                          Direction::OriginToResponder, &c);
        assert!(r.retransmit, "behind-frontier re-send after a >= 3ms gap is a retransmit");
        let s = m.observe(&seg(ACK, 1, 200, 0, 3_002_000, false), Direction::ResponderToOrigin, &c);
        assert_eq!(s.rtt, None, "Karn: no RTT after a retransmit");
    }

    #[test]
    fn dup_ack_yields_no_rtt() {
        let mut m = MetricState::new();
        let c = cfg();
        m.observe(&seg(ACK, 100, 1, 100, 1_000, false), Direction::OriginToResponder, &c);
        let s1 = m.observe(&seg(ACK, 1, 200, 0, 2_000, false), Direction::ResponderToOrigin, &c);
        assert_eq!(s1.rtt, Some(Nanos(1_000)));
        // Same ACK again (dup): no new RTT.
        let s2 = m.observe(&seg(ACK, 1, 200, 0, 3_000, false), Direction::ResponderToOrigin, &c);
        assert_eq!(s2.rtt, None);
    }

    #[test]
    fn out_of_order_within_window_not_retransmit() {
        let mut m = MetricState::new();
        let c = cfg();
        m.observe(&seg(ACK, 200, 1, 100, 1_000, false), Direction::OriginToResponder, &c); // frontier 300
        // Behind-frontier seq 100, gap 1us < 3ms -> out-of-order.
        let s = m.observe(&seg(ACK, 100, 1, 100, 1_001, false), Direction::OriginToResponder, &c);
        assert!(s.out_of_order && !s.retransmit);
    }

    #[test]
    fn reorder_window_boundary_is_retransmit() {
        let mut m = MetricState::new();
        let c = cfg();
        m.observe(&seg(ACK, 200, 1, 100, 1_000_000, false), Direction::OriginToResponder, &c);
        // Gap exactly reorder_window (3ms) -> retransmit (boundary is inclusive-at-or-above).
        let s = m.observe(&seg(ACK, 100, 1, 100, 4_000_000, false),
                          Direction::OriginToResponder, &c);
        assert!(s.retransmit && !s.out_of_order);
    }

    #[test]
    fn sack_flag_reflects_segment_blocks() {
        let mut m = MetricState::new();
        let c = cfg();
        let s = m.observe(&seg(ACK, 100, 1, 0, 1_000, true), Direction::OriginToResponder, &c);
        assert!(s.sack);
    }

    #[test]
    fn syn_consumes_phantom_byte_in_flight() {
        let mut m = MetricState::new();
        let c = cfg();
        let s = m.observe(&seg(SYN, 100, 0, 0, 1_000, false), Direction::OriginToResponder, &c);
        assert_eq!(s.in_flight_bytes, 1, "SYN consumes one sequence byte");
    }

    #[test]
    fn throughput_sums_window_bytes_and_excludes_older() {
        let mut m = MetricState::new();
        let c = cfg(); // 1s window
        // 100 bytes at t=0.
        m.observe(&seg(ACK, 0, 1, 100, 0, false), Direction::OriginToResponder, &c);
        // 100 bytes at t=0.5s: both in the 1s window ending at 0.5s -> 200 bytes -> 1600 bps.
        let s = m.observe(&seg(ACK, 100, 1, 100, 500_000_000, false),
                          Direction::OriginToResponder, &c);
        assert_eq!(s.throughput_bps, 1_600);
        // 100 bytes at t=2s: window (1s,2s] excludes the t=0 and t=0.5s bytes -> 100 -> 800 bps.
        let s2 = m.observe(&seg(ACK, 200, 1, 100, 2_000_000_000, false),
                           Direction::OriginToResponder, &c);
        assert_eq!(s2.throughput_bps, 800);
    }

    #[test]
    fn zero_throughput_window_does_not_panic() {
        let mut m = MetricState::new();
        let c = EngineConfig { throughput_window: Nanos(0), ..EngineConfig::default() };
        let s = m.observe(&seg(ACK, 0, 1, 100, 0, false), Direction::OriginToResponder, &c);
        assert_eq!(s.throughput_bps, 0);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p tcpvisr-engine derive_tests`
Expected: FAIL — `MetricState`, `seq_end` not found.

- [ ] **Step 3: Write the derivation implementation**

Add to `crates/tcpvisr-engine/src/metrics.rs` (above the test modules):

```rust
use std::collections::VecDeque;

use tcpvisr_core::{MetricSample, Nanos, SampleDir, Segment, TcpSeq};

use crate::config::EngineConfig;
use crate::conn::Direction;

/// The sequence number one past the last byte `S` puts on the wire, counting the SYN/FIN
/// phantom byte (they consume sequence space). Used for in-flight/RTT frontiers, not byte
/// counters.
#[must_use]
pub(crate) fn seq_end(seg: &Segment) -> TcpSeq {
    let phantom = u32::from(seg.flags.syn()) + u32::from(seg.flags.fin());
    TcpSeq(seg.seq.0.wrapping_add(seg.payload_len).wrapping_add(phantom))
}

/// Serial-max: keep the more-forward of `current` and `candidate`.
fn serial_max(current: Option<TcpSeq>, candidate: TcpSeq) -> TcpSeq {
    match current {
        Some(c) if c.serial_gt(candidate) => c,
        _ => candidate,
    }
}

/// `earlier` is serial-≤ `later`.
fn serial_le(earlier: TcpSeq, later: TcpSeq) -> bool {
    earlier == later || earlier.serial_lt(later)
}

#[derive(Default)]
struct DirState {
    snd_nxt: Option<TcpSeq>,
    acked: Option<TcpSeq>,
    frontier: Option<TcpSeq>,
    last_data_ts: Option<Nanos>,
    pending_rtt: VecDeque<(TcpSeq, Nanos)>,
    tput: VecDeque<(Nanos, u32)>,
    tput_max_ts: Option<Nanos>,
}

/// Per-connection metric derivation state (both directions).
pub(crate) struct MetricState {
    dir: [DirState; 2],
}

fn idx(d: Direction) -> usize {
    match d {
        Direction::OriginToResponder => 0,
        Direction::ResponderToOrigin => 1,
    }
}

fn sample_dir(d: Direction) -> SampleDir {
    match d {
        Direction::OriginToResponder => SampleDir::OriginToResponder,
        Direction::ResponderToOrigin => SampleDir::ResponderToOrigin,
    }
}

impl MetricState {
    pub(crate) fn new() -> Self {
        Self { dir: [DirState::default(), DirState::default()] }
    }

    /// Fold one segment in and produce its `MetricSample` (design §10.M3 derivation contract).
    pub(crate) fn observe(
        &mut self,
        seg: &Segment,
        dir: Direction,
        cfg: &EngineConfig,
    ) -> MetricSample {
        let d = idx(dir);
        let o = 1 - d;
        let f = seg.flags;
        let end = seq_end(seg);
        let is_data = seg.payload_len > 0;
        let consumes_seq = is_data || f.syn() || f.fin();

        // Step 0: ACK advance, computed once against pre-update state.
        let ack_advances = f.ack()
            && self.dir[o].snd_nxt.is_some()
            && match self.dir[o].acked {
                None => true,
                Some(a) => seg.ack.serial_gt(a),
            };

        // Step 2 references (pre-update): frontier + last data ts.
        let frontier = self.dir[d].frontier;
        let last_data_ts = self.dir[d].last_data_ts;

        // Step 1: in-flight.
        let acked_d = *self.dir[d].acked.get_or_insert(seg.seq);
        let snd_d = serial_max(self.dir[d].snd_nxt, end);
        self.dir[d].snd_nxt = Some(snd_d);
        if ack_advances {
            self.dir[o].acked = Some(seg.ack);
        }
        let in_flight_bytes = if serial_le(acked_d, snd_d) {
            u64::from(snd_d.serial_diff(acked_d))
        } else {
            0
        };

        // Step 2: retransmit / out-of-order (data only).
        let (mut retransmit, mut out_of_order) = (false, false);
        if is_data {
            if let Some(fr) = frontier {
                if seg.seq.serial_lt(fr) {
                    let gap = match last_data_ts {
                        Some(prev) => seg.ts.0.saturating_sub(prev.0),
                        None => u64::MAX,
                    };
                    if gap < cfg.reorder_window.0 {
                        out_of_order = true;
                    } else {
                        retransmit = true;
                    }
                }
            }
            self.dir[d].frontier = Some(serial_max(frontier, end));
            self.dir[d].last_data_ts = Some(seg.ts);
        }

        // Step 3: SACK.
        let sack = !seg.options.sack_blocks.is_empty();

        // Step 4: RTT (Karn).
        if retransmit {
            self.dir[d].pending_rtt.clear();
        } else if consumes_seq {
            self.dir[d].pending_rtt.push_back((end, seg.ts));
        }
        let mut rtt = None;
        if ack_advances {
            let pend = &mut self.dir[o].pending_rtt;
            let mut oldest: Option<Nanos> = None;
            while let Some(&(es, ts)) = pend.front() {
                if serial_le(es, seg.ack) {
                    if oldest.is_none() {
                        oldest = Some(ts);
                    }
                    pend.pop_front();
                } else {
                    break;
                }
            }
            rtt = oldest.map(|send_ts| Nanos(seg.ts.0.saturating_sub(send_ts.0)));
        }

        // Step 5: throughput (frozen, window-bounded, defensive divide).
        let throughput_bps = self.throughput(d, seg, cfg);

        MetricSample {
            t: seg.ts,
            dir: sample_dir(dir),
            in_flight_bytes,
            throughput_bps,
            rtt,
            retransmit,
            out_of_order,
            sack,
        }
    }

    fn throughput(&mut self, d: usize, seg: &Segment, cfg: &EngineConfig) -> u64 {
        let window = cfg.throughput_window.0;
        if seg.payload_len > 0 {
            self.dir[d].tput.push_back((seg.ts, seg.payload_len));
            self.dir[d].tput_max_ts = Some(match self.dir[d].tput_max_ts {
                Some(m) => Nanos(m.0.max(seg.ts.0)),
                None => seg.ts,
            });
        }
        if window == 0 {
            return 0;
        }
        // Membership is `ts > t - window`, written as `ts + window > t` to avoid u64 underflow
        // when the window extends before t=0 (else the first window of a capture drops its bytes).
        // Use u128 throughout so `ts + window` cannot overflow.
        let w = u128::from(window);
        // Evict entries that can never fall in any future window: an entry is excludable once
        // `ts + window <= max_ts` (the most permissive future window starts at max_ts - window).
        if let Some(max_ts) = self.dir[d].tput_max_ts {
            let max = u128::from(max_ts.0);
            while let Some(&(ts, _)) = self.dir[d].tput.front() {
                if u128::from(ts.0) + w <= max {
                    self.dir[d].tput.pop_front();
                } else {
                    break;
                }
            }
        }
        // Sum bytes in (seg.ts - window, seg.ts]  ==  ts + window > seg.ts  &&  ts <= seg.ts.
        let t = u128::from(seg.ts.0);
        let mut bytes: u128 = 0;
        for &(ts, len) in &self.dir[d].tput {
            let ts = u128::from(ts.0);
            if ts + w > t && ts <= t {
                bytes += u128::from(len);
            }
        }
        let bits = bytes.saturating_mul(8).saturating_mul(1_000_000_000);
        match u64::try_from(bits / w) {
            Ok(v) => v,
            Err(_) => u64::MAX,
        }
    }
}
```

Note: the final saturating cast uses an explicit `match` (not `Result::unwrap`) to satisfy the
`unwrap_used` lint without an `#[allow]`. The `bits / w` cannot divide by zero (the `window ==
0` early return guarantees `w > 0`).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p tcpvisr-engine derive_tests`
Expected: PASS (all 12 derivation tests).

- [ ] **Step 5: Add the in-flight proptest**

Append to the `derive_tests` module:

```rust
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn in_flight_equals_serial_distance_for_forward_sends(
            base in any::<u32>(), deltas in proptest::collection::vec(1u32..10_000, 1..20)
        ) {
            let mut m = MetricState::new();
            let c = cfg();
            let mut seq = base;
            let mut total: u64 = 0;
            let mut ts = 0u64;
            for d in deltas {
                ts += 1_000;
                let s = m.observe(&seg(ACK, seq, 1, d, ts, false),
                                  Direction::OriginToResponder, &c);
                total += u64::from(d);
                prop_assert_eq!(s.in_flight_bytes, total, "no ack yet: all sent is outstanding");
                seq = seq.wrapping_add(d);
            }
        }
    }
```

Run: `cargo test -p tcpvisr-engine derive_tests`
Expected: PASS.

- [ ] **Step 6: Format, lint, commit**

```bash
cargo fmt --all --check
cargo clippy -p tcpvisr-engine --all-targets --all-features -- -D warnings
git add crates/tcpvisr-engine/src/metrics.rs
git commit -m "feat(engine): derive per-segment metric samples

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: `ConnectionMetrics` + `into_metrics` + Tracker wiring

**Files:**
- Modify: `crates/tcpvisr-engine/src/metrics.rs` (add `ConnectionMetrics`)
- Modify: `crates/tcpvisr-engine/src/tracker.rs` (ConnTrack metric field + wiring + `into_metrics`)
- Modify: `crates/tcpvisr-engine/src/lib.rs` (re-export `ConnectionMetrics`)

**Interfaces:**
- Consumes: `MetricState`, `crate::conn::{Connection, ConnId}`, `SeriesCollection`, `MetricError`.
- Produces:
  - `pub struct ConnectionMetrics { pub conn: Connection, pub series: Vec<MetricSample> }`
    (derives `Debug, Clone, PartialEq, Eq`).
  - `Tracker::into_metrics(self) -> Result<Vec<ConnectionMetrics>, MetricError>` — same
    `(opened_at, pair, instance)` ordering as `into_connections`.

Implementation notes for the wiring (read before coding):
- `ConnTrack` (in `tracker.rs`) gains `metrics: MetricState` and `series: Vec<MetricSample>` and
  an `overflow: bool`. In `observe_segment`, after resolving `dir` and **before/within**
  `account`, call `let sample = self.conns[idx].metrics.observe(seg, dir, &self.config);` then,
  if this instance is collected (`should_collect`) and not overflowed, push to its `series`,
  incrementing a tracker-wide `collected_samples` counter; when it would exceed
  `config.max_samples`, set `overflow = true` and stop pushing.
- `create_instance` also derives the first sample (the creating segment) the same way.
- `should_collect(id)` = `match config.series_collection { None => false, All => true, Only(t) =>
  t == id }`.
- `into_metrics` returns `Err(SampleCeiling { samples: collected_samples, limit: max_samples })`
  if `overflow`, else maps each `ConnTrack` to `ConnectionMetrics { conn: track.view(), series }`
  sorted by the existing key.

- [ ] **Step 1: Write the failing tests**

Append to `crates/tcpvisr-engine/src/tracker.rs` a new test module:

```rust
#[cfg(test)]
mod metric_wire_tests {
    use super::test_support::{ep, seg};
    use super::*;
    use crate::metrics::SeriesCollection;
    use tcpvisr_core::TcpFlags;

    fn run(items: &[tcpvisr_core::Item], coll: SeriesCollection) -> Vec<ConnectionMetrics> {
        let cfg = EngineConfig { series_collection: coll, ..EngineConfig::default() };
        let mut t = Tracker::new(cfg);
        for it in items {
            t.observe(it);
        }
        t.into_metrics().expect("no ceiling")
    }

    #[test]
    fn none_collection_yields_empty_series() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let m = run(&[seg(c, s, TcpFlags::ACK, 100, 1, 10, 1_000)], SeriesCollection::None);
        assert_eq!(m.len(), 1);
        assert!(m[0].series.is_empty());
    }

    #[test]
    fn all_collection_yields_one_sample_per_segment() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let m = run(
            &[
                seg(c, s, TcpFlags::ACK, 100, 1, 10, 1_000),
                seg(s, c, TcpFlags::ACK, 1, 110, 0, 2_000),
            ],
            SeriesCollection::All,
        );
        assert_eq!(m[0].series.len(), 2);
        assert_eq!(m[0].series[0].in_flight_bytes, 10);
    }

    #[test]
    fn only_collection_buffers_just_the_target() {
        // Two distinct connections; collect only the first one's ConnId.
        let (c1, s1) = (ep(1, 1111), ep(2, 80));
        let (c2, s2) = (ep(3, 2222), ep(4, 80));
        let items = [
            seg(c1, s1, TcpFlags::ACK, 100, 1, 10, 1_000),
            seg(c2, s2, TcpFlags::ACK, 100, 1, 10, 2_000),
        ];
        // Resolve target id via a None pass.
        let conns = {
            let mut t = Tracker::new(EngineConfig::default());
            for it in &items { t.observe(it); }
            t.into_connections()
        };
        let target = conns[0].id;
        let m = run(&items, SeriesCollection::Only(target));
        let by_target: Vec<_> = m.iter().filter(|cm| cm.conn.id == target).collect();
        let others: Vec<_> = m.iter().filter(|cm| cm.conn.id != target).collect();
        assert_eq!(by_target[0].series.len(), 1);
        assert!(others.iter().all(|cm| cm.series.is_empty()));
    }

    #[test]
    fn ceiling_exceeded_returns_error() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let cfg = EngineConfig {
            series_collection: SeriesCollection::All,
            max_samples: 1,
            ..EngineConfig::default()
        };
        let mut t = Tracker::new(cfg);
        t.observe(&seg(c, s, TcpFlags::ACK, 100, 1, 10, 1_000));
        t.observe(&seg(c, s, TcpFlags::ACK, 110, 1, 10, 2_000)); // 2nd sample > limit 1
        let err = t.into_metrics().expect_err("should exceed");
        assert_eq!(err, MetricError::SampleCeiling { samples: 2, limit: 1 });
    }

    #[test]
    fn metrics_ordering_matches_into_connections() {
        let (c1, s1) = (ep(1, 1111), ep(2, 80));
        let (c2, s2) = (ep(3, 2222), ep(4, 80));
        let items = [
            seg(c2, s2, TcpFlags::ACK, 100, 1, 10, 2_000),
            seg(c1, s1, TcpFlags::ACK, 100, 1, 10, 1_000),
        ];
        let m = run(&items, SeriesCollection::All);
        // opened_at 1_000 (c1) sorts before 2_000 (c2).
        assert_eq!(m[0].conn.opened_at, tcpvisr_core::Nanos(1_000));
        assert_eq!(m[1].conn.opened_at, tcpvisr_core::Nanos(2_000));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p tcpvisr-engine metric_wire_tests`
Expected: FAIL — `ConnectionMetrics`, `into_metrics` not found.

- [ ] **Step 3: Add `ConnectionMetrics` and the wiring**

In `crates/tcpvisr-engine/src/metrics.rs`, add (above the test modules):

```rust
use crate::conn::Connection;

/// A tracked connection with its derived metric series (design §4's `series`, realized).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionMetrics {
    pub conn: Connection,
    pub series: Vec<MetricSample>,
}
```

In `crates/tcpvisr-engine/src/lib.rs`, extend the metrics re-export:

```rust
pub use metrics::{ConnectionMetrics, MetricError, SeriesCollection};
```

In `crates/tcpvisr-engine/src/tracker.rs`:

1. Add imports near the top:
```rust
use tcpvisr_core::MetricSample;

use crate::metrics::{ConnectionMetrics, MetricError, MetricState, SeriesCollection};
```

2. Add fields to `struct ConnTrack` (after `base_r2o`):
```rust
    metrics: MetricState,
    series: Vec<MetricSample>,
    overflow: bool,
```

3. Initialize them in `create_instance`'s `ConnTrack { … }` literal (after `base_r2o: None,`):
```rust
            metrics: MetricState::new(),
            series: Vec::new(),
            overflow: false,
```

4. Add a tracker-wide counter to `struct Tracker` (after `next_instance`):
```rust
    collected_samples: usize,
```
and initialize it `collected_samples: 0,` in `Tracker::new`.

5. Add a helper on `Tracker`:
```rust
    fn should_collect(&self, id: ConnId) -> bool {
        match self.config.series_collection {
            SeriesCollection::None => false,
            SeriesCollection::All => true,
            SeriesCollection::Only(target) => target == id,
        }
    }

    fn record_sample(&mut self, idx: usize, sample: MetricSample) {
        let id = self.conns[idx].id;
        if !self.should_collect(id) || self.conns[idx].overflow {
            return;
        }
        if self.collected_samples >= self.config.max_samples {
            self.conns[idx].overflow = true;
            return;
        }
        self.collected_samples += 1;
        self.conns[idx].series.push(sample);
    }
```

6. In `observe_segment`, in the existing-connection branch, after the `dir` is computed and
   `account`/`apply_state` run, derive + record the sample. Replace:
```rust
            if !self.should_split(idx, seg, src) {
                let dir = self.conns[idx].direction_of(src);
                self.conns[idx].account(seg, dir);
                self.conns[idx].apply_state(seg, dir);
                return;
            }
```
   with:
```rust
            if !self.should_split(idx, seg, src) {
                let dir = self.conns[idx].direction_of(src);
                self.conns[idx].account(seg, dir);
                self.conns[idx].apply_state(seg, dir);
                let sample = self.conns[idx].metrics.observe(seg, dir, &self.config);
                self.record_sample(idx, sample);
                return;
            }
```

7. In `create_instance`, after `track.account(seg, dir);` and the FIN bookkeeping, before
   pushing the track, derive the first sample and store it post-push. Change the tail of
   `create_instance` from:
```rust
        let idx = self.conns.len();
        self.conns.push(track);
        self.live.insert(pair, idx);
```
   to:
```rust
        let sample = track.metrics.observe(seg, dir, &self.config);
        let idx = self.conns.len();
        self.conns.push(track);
        self.live.insert(pair, idx);
        self.record_sample(idx, sample);
```
   (`dir` is already in scope from `let dir = track.direction_of(src);` earlier in
   `create_instance`.)

8. Add the finalizer after `into_connections`:
```rust
    /// All tracked instances with their derived series, same ordering as `into_connections`.
    ///
    /// # Errors
    /// Returns `MetricError::SampleCeiling` if collection hit `max_samples`.
    pub fn into_metrics(self) -> Result<Vec<ConnectionMetrics>, MetricError> {
        if self.conns.iter().any(|c| c.overflow) {
            return Err(MetricError::SampleCeiling {
                samples: self.collected_samples + 1,
                limit: self.config.max_samples,
            });
        }
        let mut out: Vec<ConnectionMetrics> = self
            .conns
            .iter()
            .map(|c| ConnectionMetrics { conn: c.view(), series: c.series.clone() })
            .collect();
        out.sort_by_key(|m| (m.conn.opened_at, m.conn.id.pair, m.conn.id.instance));
        Ok(out)
    }
```

Note on the `samples` count in the error: the ceiling trips when a push *would* exceed the
limit, so the reported count is `collected_samples + 1` (the sample that overflowed). The test
`ceiling_exceeded_returns_error` asserts `samples: 2, limit: 1` — with `max_samples = 1`, the
first sample is stored (`collected_samples = 1`), the second sets `overflow` (reported as
`1 + 1 = 2`). Confirm the arithmetic against the test; if it mismatches, adjust the `+ 1` here,
not the test's intent (the count must be `> limit`).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p tcpvisr-engine`
Expected: PASS (metric_wire_tests + all existing M2 tests still green — confirm no M2 test
changed).

- [ ] **Step 5: Format, lint, commit**

```bash
cargo fmt --all --check
cargo clippy -p tcpvisr-engine --all-targets --all-features -- -D warnings
git add crates/tcpvisr-engine/src/metrics.rs crates/tcpvisr-engine/src/tracker.rs \
        crates/tcpvisr-engine/src/lib.rs
git commit -m "feat(engine): collect metric series and into_metrics finalizer

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 5: fixtures + builder SACK support + oracle goldens + drift guard

**Files:**
- Modify: `crates/tcp-visr/tests/support/mod.rs` (add `tcp_with_sack`, metrics fixtures)
- Create: `crates/tcp-visr/tests/fixtures/{metrics_basic,metrics_retransmit,metrics_ooo,metrics_sack}.pcap`
- Create: `crates/tcp-visr/tests/oracle/<fixture>.metrics.json` (5 goldens; `seq_wrap` reused)
- Create: `crates/tcp-visr/tests/oracle/README.md` (hand-derived derivations)
- Modify: `crates/tcp-visr/tests/drift.rs` (oracle golden drift guard)

**Interfaces:**
- Consumes: the M2 `tests/support` builder helpers (`tcp`, `legacy_pcap`, `fixture_set`).
- Produces: a `metrics_fixture_set() -> Vec<(&'static str, Vec<u8>)>` and an
  `oracle_set() -> Vec<(&'static str, String)>` used by the drift guard and Task 6's CLI tests.

This task is sequenced **after Tasks 3–4** so the engine derivation exists to compute the
goldens, but the golden NUMBERS are **hand-derived** (below), not copied from program output.

- [ ] **Step 1: Add the SACK-capable builder + metrics fixtures (no test yet — builder code)**

In `crates/tcp-visr/tests/support/mod.rs`, the module already has
`#![allow(clippy::expect_used, clippy::cast_possible_truncation, clippy::too_many_arguments)]`.
Add a SACK-emitting frame builder and the metrics fixture set. Add near `tcp`:

```rust
use etherparse::TcpOptionElement;

/// One Ethernet+IPv4+TCP frame carrying a single SACK block `[left,right)`, `n` payload bytes.
#[must_use]
pub fn tcp_with_sack(
    src: [u8; 4],
    dst: [u8; 4],
    sp: u16,
    dp: u16,
    seq: u32,
    ack: u32,
    left: u32,
    right: u32,
    n: usize,
) -> Vec<u8> {
    let builder = PacketBuilder::ethernet2([2, 0, 0, 0, 0, 1], [2, 0, 0, 0, 0, 2])
        .ipv4(src, dst, 64)
        .tcp(sp, dp, seq, 64240)
        .ack(ack)
        .options(&[TcpOptionElement::SelectiveAcknowledgement(
            (left, right),
            [None, None, None],
        )])
        .expect("valid SACK option");
    let mut buf = Vec::new();
    builder.write(&mut buf, &vec![0xab; n]).expect("build sack frame");
    buf
}
```

Then add the metrics fixtures:

```rust
/// The four M3 metric fixtures (seq_wrap is reused from the M2 set). Strictly increasing
/// microsecond timestamps; reorder cases reverse SEQ, not time.
#[must_use]
pub fn metrics_fixture_set() -> Vec<(&'static str, Vec<u8>)> {
    use flag::{ACK, SYN};
    let (cp, sp) = (1234u16, 80u16);
    vec![
        // SYN handshake + data + ACKs: in-flight, handshake RTT, data RTT, throughput.
        (
            "metrics_basic.pcap",
            legacy_pcap(&[
                (1_000, tcp(C, S, cp, sp, SYN, 1000, 0, 0)),               // SYN seq=1000
                (2_000, tcp(S, C, sp, cp, SYN | ACK, 5000, 1001, 0)),      // SYN-ACK
                (3_000, tcp(C, S, cp, sp, ACK, 1001, 5001, 100)),          // data 100B o2r
                (4_000, tcp(S, C, sp, cp, ACK, 5001, 1101, 0)),           // ACK of o2r data
            ]),
        ),
        // data, then a retransmit of the same range after a long gap (>= reorder_window).
        (
            "metrics_retransmit.pcap",
            legacy_pcap(&[
                (1_000, tcp(C, S, cp, sp, ACK, 100, 1, 100)),             // data 100..200
                (3_001_000, tcp(C, S, cp, sp, ACK, 100, 1, 100)),        // retransmit (gap 3.0001s)
            ]),
        ),
        // out-of-order: a behind-frontier segment within reorder_window (1us gap).
        (
            "metrics_ooo.pcap",
            legacy_pcap(&[
                (1_000, tcp(C, S, cp, sp, ACK, 200, 1, 100)),            // frontier 300
                (1_001, tcp(C, S, cp, sp, ACK, 100, 1, 100)),           // behind, gap 1us -> OOO
            ]),
        ),
        // a segment carrying a SACK block.
        (
            "metrics_sack.pcap",
            legacy_pcap(&[
                (1_000, tcp(C, S, cp, sp, ACK, 100, 1, 50)),
                (2_000, tcp_with_sack(S, C, sp, cp, 1, 151, 200, 260, 0)), // SACK [200,260)
            ]),
        ),
    ]
}
```

(`C`, `S`, `flag`, `legacy_pcap`, `tcp` already exist in this module from M2.)

- [ ] **Step 2: Write the drift-guard test (failing — fixtures not committed yet)**

In `crates/tcp-visr/tests/drift.rs`, add (it already has `mod support;`):

```rust
#[test]
fn committed_metrics_fixtures_match_builder() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
    for (name, bytes) in support::metrics_fixture_set() {
        let path = std::path::Path::new(dir).join(name);
        let on_disk =
            std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        assert_eq!(on_disk, bytes, "committed {name} is stale; regenerate fixtures");
    }
}

#[test]
#[ignore = "regenerates committed metrics fixtures; run explicitly after a reviewed change"]
fn regenerate_metrics_fixtures() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
    std::fs::create_dir_all(dir).unwrap();
    for (name, bytes) in support::metrics_fixture_set() {
        std::fs::write(std::path::Path::new(dir).join(name), bytes).unwrap();
    }
}
```

- [ ] **Step 3: Generate the fixtures, then verify the drift guard passes**

```bash
cargo test -p tcp-visr --test drift regenerate_metrics_fixtures -- --ignored
cargo test -p tcp-visr --test drift committed_metrics_fixtures_match_builder
```
Expected: the first writes the four `.pcap`s; the second PASSES.

- [ ] **Step 4: Hand-derive and write the oracle goldens + README**

The goldens are produced **after** Task 6 wires the `metrics` command (so the JSON shape
exists), but the **numbers** are derived here by hand. Create
`crates/tcp-visr/tests/oracle/README.md` documenting each fixture's load-bearing values, e.g.:

```
# Oracle derivations (hand-computed, RFC 1982 serial arithmetic)

seq_wrap.pcap (--conn 0), o2r = client(10.0.0.1:1234)->server(10.0.0.2:80):
  seg1 o2r seq=2^32-101 len=50  ts=1us:  acked[o2r]=2^32-101, snd_nxt=2^32-51,
        in_flight = serial_diff(2^32-51, 2^32-101) = 50. ACK=1 but r2o has no send -> no rtt.
  seg2 o2r seq=200 len=50 ts=2us:  snd_nxt=250 (forward wrap), in_flight=serial_diff(250,2^32-101)=351.
  seg3 r2o seq=1 ack=300 len=10 ts=3us: r2o in_flight=serial_diff(11,1)=10; ack=300 advances acked[o2r],
        covers o2r sends (seq_end 2^32-51, 250 <= 300) -> rtt pairs oldest (ts1=1us): rtt = 3us-1us = 2000ns.

metrics_basic.pcap (--conn 0):
  seg1 o2r SYN seq=1000 ts=1us: snd_nxt=1001 (phantom), acked[o2r]=1000, in_flight=1. eligible RTT send (end 1001).
  seg2 r2o SYN-ACK seq=5000 ack=1001 ts=2us: r2o snd_nxt=5001, acked[r2o]=5000, in_flight=1;
        ack=1001 advances acked[o2r], covers SYN(end 1001) -> rtt = 2us-1us = 1000ns (handshake RTT).
  seg3 o2r data seq=1001 len=100 ts=3us: snd_nxt=1101, in_flight=serial_diff(1101,1001)=100. eligible send (end 1101).
  seg4 r2o ACK ack=1101 ts=4us: ack covers o2r data(end 1101) -> rtt = 4us-3us = 1000ns (data RTT). r2o in_flight: snd_nxt=5001, acked[r2o]=5001? ack handling: this is r2o, carries ack=1101 for o2r; r2o's own in_flight = serial_diff(5001, 5000)=1 (the SYN-ACK phantom, unacked).
  throughput: window 1s; at seg3, 100 bytes in (3us-1s,3us] -> 8*100*1e9/1e9 = 800 bps.

metrics_retransmit.pcap (--conn 0): seg2 seq=100 < frontier 200, gap 3.0001s >= 3ms -> retransmit=true;
        clears pending RTT. (Both o2r, no reverse ACK, so no RTT anywhere.)
metrics_ooo.pcap (--conn 0): seg2 seq=100 < frontier 300, gap 1us < 3ms -> out_of_order=true.
metrics_sack.pcap (--conn 0): seg2 (r2o) carries SACK [200,260) -> sack=true on that sample.
```

Write the five golden JSON files by running the command once the `metrics` subcommand exists
(Task 6) **and confirming each value against the README above** before committing. The golden
files are named `<fixture-stem>.metrics.json` (e.g. `seq_wrap.metrics.json`).

> This step's commit happens at the end of Task 6 (the goldens need the command). Do **not**
> commit empty/placeholder goldens here. Commit the builder + fixtures + README now:

```bash
cargo fmt --all --check
cargo clippy -p tcp-visr --all-targets --all-features -- -D warnings
git add crates/tcp-visr/tests/support/mod.rs crates/tcp-visr/tests/drift.rs \
        crates/tcp-visr/tests/fixtures/metrics_basic.pcap \
        crates/tcp-visr/tests/fixtures/metrics_retransmit.pcap \
        crates/tcp-visr/tests/fixtures/metrics_ooo.pcap \
        crates/tcp-visr/tests/fixtures/metrics_sack.pcap \
        crates/tcp-visr/tests/oracle/README.md
git commit -m "test(cli): add M3 metric fixtures, SACK builder, oracle derivations

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 6: `metrics` CLI subcommand + JSON DTOs + oracle integration tests

**Files:**
- Modify: `crates/tcp-visr/Cargo.toml` (serde, serde_json)
- Modify: `crates/tcp-visr/src/main.rs` (Metrics variant, run_metrics, DTOs, flag validation)
- Modify: `deny.toml` (license allow-list for serde's tree, if needed)
- Create: `crates/tcp-visr/tests/metrics.rs` (CLI/oracle integration)
- Create: `crates/tcp-visr/tests/oracle/*.metrics.json` (5 committed goldens)
- Modify: `crates/tcp-visr/tests/drift.rs` (golden drift guard)

**Interfaces:**
- Consumes: `tcpvisr_engine::{Tracker, EngineConfig, SeriesCollection, ConnectionMetrics,
  MetricError}`, `tcpvisr_core::{MetricSample, SampleDir}`, `tcpvisr_ingest::parse_file_visit`.
- Produces: the `tcp-visr metrics` command and its stable JSON.

- [ ] **Step 1: Add deps and the Metrics clap variant**

In `crates/tcp-visr/Cargo.toml` `[dependencies]`, add (look up the current stable `serde`/
`serde_json` patch versions at implementation time; pin `=`):

```toml
serde = { version = "=1.0.228", features = ["derive"] }
serde_json = "=1.0.145"
```

Run `cargo build -p tcp-visr` then `cargo deny check`. If `cargo deny check` reports a license
not in the allow-list (serde's tree pulls `itoa`, `ryu`, `memchr` — `memchr` is
`Unlicense OR MIT`, which resolves to the allowed `MIT`; `itoa`/`ryu` are `MIT OR Apache-2.0`),
no change is needed. If a NEW SPDX id is genuinely required, add it to `deny.toml`'s
`[licenses] allow` list with a comment, then re-run.

In `crates/tcp-visr/src/main.rs`, change the `Metrics` variant from a unit variant to:

```rust
    /// Dump a connection's metric series as JSON.
    Metrics {
        /// The `.pcap`/`.pcapng` capture file to analyze.
        file: PathBuf,
        /// 0-based index of the connection (the order `tcp-visr conns` prints).
        #[arg(long)]
        conn: usize,
        /// Trailing throughput window in milliseconds (must be >= 1).
        #[arg(long, default_value_t = 1000)]
        throughput_window_ms: u64,
        /// Reorder window in milliseconds (a behind-frontier gap below this is out-of-order).
        #[arg(long, default_value_t = 3)]
        reorder_window_ms: u64,
        /// Ceiling on retained samples for the selected connection (must be >= 1).
        #[arg(long, default_value_t = 10_000_000)]
        max_samples: usize,
    },
```

Update `Command::name`'s `Metrics` arm to `Command::Metrics { .. } => "metrics",` and the `run`
dispatch `match` to call `run_metrics(...)` instead of the "not implemented" arm.

- [ ] **Step 2: Write the failing integration test**

Create `crates/tcp-visr/tests/metrics.rs`:

```rust
// `unwrap` in non-`#[test]` helpers: scope the relaxation to this file (matches conns.rs).
#![allow(clippy::unwrap_used)]

use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_tcp-visr"))
}

fn fixture(name: &str) -> String {
    format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
}

fn golden(stem: &str) -> String {
    let path = format!("{}/tests/oracle/{stem}.metrics.json", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(path).unwrap()
}

fn metrics_ok(fixture_name: &str, conn: &str) -> String {
    let out = bin()
        .args(["metrics", &fixture(fixture_name), "--conn", conn])
        .output()
        .unwrap();
    assert!(out.status.success(), "metrics {fixture_name} exited nonzero: {:?}",
            String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn seq_wrap_matches_golden() {
    assert_eq!(metrics_ok("seq_wrap.pcap", "0"), golden("seq_wrap"));
}

#[test]
fn metrics_basic_matches_golden() {
    assert_eq!(metrics_ok("metrics_basic.pcap", "0"), golden("metrics_basic"));
}

#[test]
fn metrics_retransmit_matches_golden() {
    assert_eq!(metrics_ok("metrics_retransmit.pcap", "0"), golden("metrics_retransmit"));
}

#[test]
fn metrics_ooo_matches_golden() {
    assert_eq!(metrics_ok("metrics_ooo.pcap", "0"), golden("metrics_ooo"));
}

#[test]
fn metrics_sack_matches_golden() {
    assert_eq!(metrics_ok("metrics_sack.pcap", "0"), golden("metrics_sack"));
}

#[test]
fn out_of_range_conn_exits_nonzero() {
    let out = bin().args(["metrics", &fixture("seq_wrap.pcap"), "--conn", "99"]).output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(err.contains("out of range"), "{err}");
    assert!(err.contains("conns"), "{err}");
}

#[test]
fn missing_file_exits_nonzero() {
    let out = bin().args(["metrics", "/nonexistent.pcap", "--conn", "0"]).output().unwrap();
    assert!(!out.status.success());
}

#[test]
fn zero_throughput_window_rejected() {
    let out = bin()
        .args(["metrics", &fixture("seq_wrap.pcap"), "--conn", "0", "--throughput-window-ms", "0"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(err.contains("throughput-window-ms"), "{err}");
}
```

- [ ] **Step 3: Implement `run_metrics` + DTOs**

Add to `crates/tcp-visr/src/main.rs`. First, the dispatch arm in `run`:

```rust
        Command::Metrics {
            file,
            conn,
            throughput_window_ms,
            reorder_window_ms,
            max_samples,
        } => run_metrics(&file, conn, throughput_window_ms, reorder_window_ms, max_samples),
```

Then the DTOs + function:

```rust
use serde::Serialize;
use tcpvisr_core::{MetricSample, Nanos, SampleDir};
use tcpvisr_engine::{ConnectionMetrics, EngineConfig, SeriesCollection, Tracker};

#[derive(Serialize)]
struct ConnectionJson {
    index: usize,
    origin: String,
    responder: String,
    instance: u32,
    state: String,
    origin_inferred: bool,
    opened_at_ns: u64,
    last_at_ns: u64,
}

#[derive(Serialize)]
struct SampleJson {
    t_ns: u64,
    dir: &'static str,
    in_flight: u64,
    throughput_bps: u64,
    rtt_ns: Option<u64>,
    retransmit: bool,
    out_of_order: bool,
    sack: bool,
}

#[derive(Serialize)]
struct MetricsJson {
    connection: ConnectionJson,
    throughput_window_ns: u64,
    reorder_window_ns: u64,
    samples: Vec<SampleJson>,
}

fn dir_str(d: SampleDir) -> &'static str {
    match d {
        SampleDir::OriginToResponder => "o2r",
        SampleDir::ResponderToOrigin => "r2o",
    }
}

fn sample_json(s: &MetricSample) -> SampleJson {
    SampleJson {
        t_ns: s.t.0,
        dir: dir_str(s.dir),
        in_flight: s.in_flight_bytes,
        throughput_bps: s.throughput_bps,
        rtt_ns: s.rtt.map(|n| n.0),
        retransmit: s.retransmit,
        out_of_order: s.out_of_order,
        sack: s.sack,
    }
}

fn run_metrics(
    file: &Path,
    conn: usize,
    throughput_window_ms: u64,
    reorder_window_ms: u64,
    max_samples: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    if throughput_window_ms == 0 {
        return Err("--throughput-window-ms must be at least 1 (got 0)".into());
    }
    if max_samples == 0 {
        return Err("--max-samples must be at least 1 (got 0)".into());
    }

    let base = EngineConfig {
        throughput_window: Nanos(throughput_window_ms.saturating_mul(1_000_000)),
        reorder_window: Nanos(reorder_window_ms.saturating_mul(1_000_000)),
        max_samples,
        ..EngineConfig::default()
    };

    // Pass 1: resolve the target connection (lifecycle only, no series).
    let mut pass1 = Tracker::new(base);
    let (_link, _skipped) = tcpvisr_ingest::parse_file_visit(file, &mut |item| pass1.observe(item))?;
    let conns = pass1.into_connections();
    let target = conns.get(conn).ok_or_else(|| {
        format!(
            "connection index {conn} out of range (capture has {} connections, 0..{}); \
             run `tcp-visr conns {}` to list them",
            conns.len(),
            conns.len().saturating_sub(1),
            file.display()
        )
    })?;
    let target_id = target.id;

    // Pass 2: collect only the target's series.
    let cfg = EngineConfig { series_collection: SeriesCollection::Only(target_id), ..base };
    let mut pass2 = Tracker::new(cfg);
    let (_link, _skipped) = tcpvisr_ingest::parse_file_visit(file, &mut |item| pass2.observe(item))?;
    let metrics = pass2.into_metrics()?;
    let selected: &ConnectionMetrics = metrics
        .iter()
        .find(|m| m.conn.id == target_id)
        .ok_or("internal: target connection vanished between passes")?;

    let c = &selected.conn;
    let json = MetricsJson {
        connection: ConnectionJson {
            index: conn,
            origin: c.origin.to_string(),
            responder: c.responder.to_string(),
            instance: c.id.instance,
            state: format!("{:?}", c.state),
            origin_inferred: c.origin_inferred,
            opened_at_ns: c.opened_at.0,
            last_at_ns: c.last_at.0,
        },
        throughput_window_ns: cfg.throughput_window.0,
        reorder_window_ns: cfg.reorder_window.0,
        samples: selected.series.iter().map(sample_json).collect(),
    };

    let mut out = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut out, &json)?;
    writeln!(out)?; // trailing newline
    Ok(())
}
```

`MetricError` implements `std::error::Error` (via `thiserror`), so `pass2.into_metrics()?`
converts into the `Box<dyn Error>` return automatically and prints its actionable `Display`.

- [ ] **Step 4: Run the integration tests (goldens still missing → generate them)**

First generate goldens from the command and CHECK each against `tests/oracle/README.md`:

```bash
for f in seq_wrap metrics_basic metrics_retransmit metrics_ooo metrics_sack; do
  cargo run -q -p tcp-visr -- metrics crates/tcp-visr/tests/fixtures/$f.pcap --conn 0 \
    > crates/tcp-visr/tests/oracle/$f.metrics.json
done
```

Open each `*.metrics.json` and verify the load-bearing numbers match the hand-derived README
(in particular: `seq_wrap` sample 2 `in_flight: 351` and sample 3 `rtt_ns: 2000`;
`metrics_basic` handshake `rtt_ns: 1000` and data `rtt_ns: 1000`; `metrics_retransmit`
`retransmit: true`; `metrics_ooo` `out_of_order: true`; `metrics_sack` `sack: true`). If any
number disagrees with the derivation, the **code** is wrong — fix it, do not bless the output.

Then:

```bash
cargo test -p tcp-visr --test metrics
```
Expected: PASS (all goldens byte-match; error paths exit non-zero).

- [ ] **Step 5: Add the golden drift guard**

In `crates/tcp-visr/tests/drift.rs`, add:

```rust
#[test]
fn committed_oracle_goldens_match_command_shape() {
    // The goldens are byte-matched by tests/metrics.rs against live output; here we only
    // assert they exist and are non-empty valid JSON, so an accidental deletion is caught.
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/oracle");
    for stem in ["seq_wrap", "metrics_basic", "metrics_retransmit", "metrics_ooo", "metrics_sack"] {
        let path = std::path::Path::new(dir).join(format!("{stem}.metrics.json"));
        let body = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        assert!(body.trim_start().starts_with('{'), "{stem} golden is not JSON");
        assert!(body.ends_with('\n'), "{stem} golden must end with a newline");
    }
}
```

- [ ] **Step 6: Add the gated external cross-check stub**

Append to `crates/tcp-visr/tests/metrics.rs`:

```rust
#[test]
#[ignore = "release gate: cross-check RTT/retransmit against tcptrace/Wireshark on the fixtures"]
fn tcptrace_cross_check() {
    // Run by maintainers before a release (no external tool in CI). For each fixture, run
    // `tcptrace -lr <fixture>` (or Wireshark TCP stream graphs) and confirm the RTT samples and
    // retransmit/OOO counts agree with the committed goldens, using the oldest-acked-per-
    // cumulative-ACK RTT definition (spec §"RTT (Karn)"). Document the reference in the release
    // notes. This test intentionally does nothing in CI.
}
```

- [ ] **Step 7: Full guardrails + commit**

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace
cargo test -p tcpvisr-ingest --features live
cargo deny check
git add crates/tcp-visr/Cargo.toml crates/tcp-visr/src/main.rs crates/tcp-visr/tests/metrics.rs \
        crates/tcp-visr/tests/drift.rs crates/tcp-visr/tests/oracle/*.metrics.json \
        Cargo.lock deny.toml
git commit -m "feat(cli): dump connection metric series as JSON with metrics subcommand

Closes #6 sub-task: metrics --conn N two-pass JSON dump, oracle goldens,
flag validation, gated tcptrace cross-check.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Self-Review

**Spec coverage (each spec section → task):**
- core `MetricSample`/`SampleDir` → Task 1. ✓
- `EngineConfig` knobs, `SeriesCollection`, `MetricError` → Task 2. ✓
- Derivation contract (steps 0–5: in-flight, retransmit/OOO, SACK, Karn RTT, throughput;
  phantom bytes; Option init; zero-window guard) → Task 3. ✓
- `ConnectionMetrics`, `into_metrics`, collection scoping, ceiling → Task 4. ✓
- Fixtures (incl. SACK builder), drift guard, oracle goldens + README, gated cross-check →
  Tasks 5–6. ✓
- `metrics` subcommand (two-pass, `--conn` required 0-based, window/ceiling flags + validation,
  JSON shape, error paths) → Task 6. ✓
- DoD guardrails (fmt/clippy/test/`--features live`/deny) → run in every task and the final
  Task 6 step. ✓

**Placeholder scan:** no TBD/"add error handling"/"similar to" — every code step shows code. ✓

**Type consistency:** `MetricSample`/`SampleDir` (core) used identically in engine and CLI;
`SeriesCollection`/`EngineConfig`/`ConnectionMetrics`/`MetricError` names consistent across
Tasks 2/4/6; `MetricState::observe(&mut self, &Segment, Direction, &EngineConfig) ->
MetricSample` and `seq_end(&Segment) -> TcpSeq` consistent Task 3 → Task 4;
`Tracker::into_metrics(self) -> Result<Vec<ConnectionMetrics>, MetricError>` consistent Task 4 →
Task 6. ✓

**Cross-pass config invariant (challenge follow-up):** both passes in `run_metrics` build from
the same `base` `EngineConfig` (identical `dead_after`/`reset_threshold`), so the connection
order is identical across passes; only `series_collection` differs (it does not affect
ordering). ✓

**Known risk to watch during execution:** the ceiling error count arithmetic (`collected_samples
+ 1`) in Task 4 step 3 — verify against the `ceiling_exceeded_returns_error` test and adjust the
`+ 1`, not the test, if it mismatches. The golden numbers in Task 6 must be confirmed against the
Task 5 README before committing (code is wrong if they disagree, not the golden).
