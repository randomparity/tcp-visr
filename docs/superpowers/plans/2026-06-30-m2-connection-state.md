# M2 — Connection State Machine Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the pure per-connection state machine in `tcpvisr-engine` and expose it as
`tcp-visr conns <file>`, listing each connection with observed state, per-direction wire
bytes, and duration.

**Architecture:** `tcpvisr-core` gains a small `Endpoint` type. `tcpvisr-engine` (pure, no
I/O) folds the M1 `Item` stream into `Connection`s: groups both wire directions by a
canonical `EndpointPair`, derives orientation (origin/responder) from a bare SYN, else a
SYN-ACK, else the first segment; runs a passive-observer state machine; and disambiguates
connection instances via SYN-after-terminal/idle and RFC-1982 backward-sequence reset (a
forward `u32` wrap never splits). The `conns` CLI streams the capture through the M1 replay
faucet (`parse_file_visit`) into a `Tracker`.

**Tech Stack:** Rust 1.88.0 (edition 2024). No new runtime deps. Dev-only: `proptest`
`=1.11.0` (engine — split-boundary property test), `etherparse` `=0.20.2` (tcp-visr — fixture
builder). Both crates are already in the workspace lock (core / ingest), so cargo-deny and
the license allow-list are unaffected.

Derived from [the M2 spec](../../milestones/m02-connection-state/spec.md) and
[ADR-0006](../../adr/0006-connection-identity-and-direction.md).

## Global Constraints

- **Toolchain = MSRV = 1.88.0**; edition 2024; pin any dep with `=`.
- **Lint policy (workspace `[lints]`)**: no `unwrap`/`expect`/`panic!`/`println!`/`eprintln!`/
  `process::exit`/`#[allow]`/`todo!` in non-test code. `clippy::pedantic` is `warn`; CI runs
  `-D warnings`, so pedantic findings must be resolved. Restriction lints are relaxed in
  tests via `clippy.toml`.
- **No stdout macros**: print via `writeln!(io::stdout().lock(), …)`, not `println!`.
- **All TCP sequence arithmetic uses `tcpvisr_core::TcpSeq`** (RFC 1982) — never naive `u32`
  subtraction. Backward distance is `baseline.serial_diff(seq)`; "seq is backward of
  baseline" is `seq.serial_lt(baseline)`.
- **Time is non-monotonic** (design §14): every `Nanos` subtraction uses `saturating_sub`;
  `last_at` is the **max** ts seen.
- **Conventional Commits**, imperative ≤72-char subject, every commit body ends with
  `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- **Guardrails before every commit**: `cargo fmt --all --check` ·
  `cargo clippy --all-targets --all-features -- -D warnings` (needs `libpcap-dev`) ·
  `cargo test --workspace`. For the final push also `cargo test -p tcpvisr-ingest --features
  live` and `cargo deny check`.

---

## File Structure

```
crates/tcpvisr-core/
  src/lib.rs            # + pub mod endpoint; pub use endpoint::Endpoint
  src/endpoint.rs       # Endpoint { ip, port } + Display (NEW)
  src/flow.rs           # + FlowKey::source()/destination() -> Endpoint
crates/tcpvisr-engine/
  Cargo.toml            # + [dependencies] tcpvisr-core; [dev-dependencies] proptest =1.11.0
  src/lib.rs            # module decls + re-exports (replaces the stub)
  src/config.rs         # EngineConfig
  src/state.rs          # ConnState
  src/conn.rs           # EndpointPair, ConnId, Direction, Connection, ConnTrack (internal)
  src/tracker.rs        # Tracker, observe(), into_connections(), track(), is_backward_reset()
crates/tcp-visr/
  Cargo.toml            # + [dependencies] tcpvisr-engine; [dev-dependencies] etherparse =0.20.2
  src/main.rs           # Conns { file } variant body
  tests/support/mod.rs  # M2 fixture byte builder (NEW)
  tests/fixtures/*.pcap  # 5 committed generated fixtures (NEW)
  tests/conns.rs        # CLI integration over fixtures (NEW)
  tests/drift.rs        # committed fixtures == builder output (NEW)
```

---

### Task 1: core `Endpoint`

**Files:**
- Create: `crates/tcpvisr-core/src/endpoint.rs`
- Modify: `crates/tcpvisr-core/src/lib.rs`, `crates/tcpvisr-core/src/flow.rs`

**Interfaces:**
- Produces: `tcpvisr_core::Endpoint { pub ip: IpAddr, pub port: u16 }` deriving
  `Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord` with `Display`
  (`1.2.3.4:80` / `[::1]:80`); `FlowKey::source() -> Endpoint`,
  `FlowKey::destination() -> Endpoint`.

- [ ] **Step 1: Write the failing test** — append to `crates/tcpvisr-core/src/endpoint.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use core::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn display_v4_uses_colon() {
        let e = Endpoint { ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), port: 80 };
        assert_eq!(e.to_string(), "10.0.0.1:80");
    }

    #[test]
    fn display_v6_brackets_address() {
        let e = Endpoint { ip: IpAddr::V6(Ipv6Addr::LOCALHOST), port: 443 };
        assert_eq!(e.to_string(), "[::1]:443");
    }

    #[test]
    fn ord_is_ip_then_port() {
        let a = Endpoint { ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), port: 9 };
        let b = Endpoint { ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), port: 10 };
        assert!(a < b);
    }
}
```

- [ ] **Step 2: Run, expect fail** — `cargo test -p tcpvisr-core endpoint` → FAIL (no `Endpoint`).

- [ ] **Step 3: Implement** — prepend to `crates/tcpvisr-core/src/endpoint.rs`:

```rust
//! One side of a TCP connection: an IP address and port (design §4, M2).

use core::fmt;
use core::net::IpAddr;

/// A connection endpoint (`ip:port`). Ordered by `(ip, port)` for canonicalization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Endpoint {
    pub ip: IpAddr,
    pub port: u16,
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.ip {
            IpAddr::V4(a) => write!(f, "{a}:{}", self.port),
            IpAddr::V6(a) => write!(f, "[{a}]:{}", self.port),
        }
    }
}
```

Add to `crates/tcpvisr-core/src/lib.rs` (module list + re-export):

```rust
pub mod endpoint;
```
```rust
pub use endpoint::Endpoint;
```

Add to `crates/tcpvisr-core/src/flow.rs` (after the `FlowKey` struct, before `Display`):

```rust
use crate::endpoint::Endpoint;

impl FlowKey {
    /// The source endpoint as seen on the wire.
    #[must_use]
    pub fn source(&self) -> Endpoint {
        Endpoint { ip: self.src_ip, port: self.src_port }
    }

    /// The destination endpoint as seen on the wire.
    #[must_use]
    pub fn destination(&self) -> Endpoint {
        Endpoint { ip: self.dst_ip, port: self.dst_port }
    }
}
```

- [ ] **Step 4: Run, expect pass** — `cargo test -p tcpvisr-core` → PASS.

- [ ] **Step 5: Guardrails + commit**

```bash
cargo fmt --all --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test -p tcpvisr-core
git add crates/tcpvisr-core
git commit -m "feat(core): add Endpoint and FlowKey endpoint accessors"
```

---

### Task 2: engine foundation — config, state, conn types, split helper

**Files:**
- Modify: `crates/tcpvisr-engine/Cargo.toml`, `crates/tcpvisr-engine/src/lib.rs`
- Create: `crates/tcpvisr-engine/src/config.rs`, `src/state.rs`, `src/conn.rs`,
  `src/tracker.rs`

**Interfaces:**
- Produces:
  - `EngineConfig { pub dead_after: Nanos, pub reset_threshold: u32 }`, `Default` =
    `{ dead_after: Nanos(120_000_000_000), reset_threshold: 1 << 30 }`.
  - `ConnState { SynSent, SynReceived, Established, FinWait, Closed, Reset }` (`Copy`,
    `PartialEq`).
  - `EndpointPair { pub low: Endpoint, pub high: Endpoint }` with
    `EndpointPair::new(a, b)` ordering the two; `ConnId { pub pair: EndpointPair, pub
    instance: u32 }`.
  - crate-private `fn is_backward_reset(baseline: TcpSeq, seq: TcpSeq, threshold: u32) ->
    bool` in `tracker.rs`.

- [ ] **Step 1: Add deps** — `crates/tcpvisr-engine/Cargo.toml`:

```toml
[dependencies]
tcpvisr-core = { path = "../tcpvisr-core" }

[dev-dependencies]
proptest = "=1.11.0"
```
(Keep the existing `[package]` and `[lints] workspace = true`.)

- [ ] **Step 2: Write the failing test** — `crates/tcpvisr-engine/src/tracker.rs`:

```rust
#[cfg(test)]
mod split_tests {
    use super::is_backward_reset;
    use proptest::prelude::*;
    use tcpvisr_core::TcpSeq;

    const HALF: u32 = 1 << 31;

    #[test]
    fn forward_wrap_is_not_a_reset() {
        // baseline near the top; seq wrapped forward by 0x300 — an advance, not a reset.
        assert!(!is_backward_reset(TcpSeq(u32::MAX - 0xFF), TcpSeq(0x200), 1 << 30));
    }

    #[test]
    fn small_backward_is_not_a_reset() {
        assert!(!is_backward_reset(TcpSeq(1_000_000), TcpSeq(999_000), 1 << 30));
    }

    #[test]
    fn large_backward_is_a_reset() {
        // 0x6000_0000 backward (> 2^30, < 2^31).
        assert!(is_backward_reset(TcpSeq(0x7000_0000), TcpSeq(0x1000_0000), 1 << 30));
    }

    proptest! {
        #[test]
        fn forward_delta_never_resets(base in any::<u32>(), d in 1u32..HALF) {
            let seq = TcpSeq(base.wrapping_add(d));
            prop_assert!(!is_backward_reset(TcpSeq(base), seq, 1 << 30));
        }

        #[test]
        fn backward_delta_splits_iff_over_threshold(
            base in any::<u32>(), b in 1u32..HALF, thr in 0u32..HALF
        ) {
            let seq = TcpSeq(base.wrapping_sub(b));
            prop_assert_eq!(is_backward_reset(TcpSeq(base), seq, thr), b > thr);
        }
    }
}
```

- [ ] **Step 3: Run, expect fail** — `cargo test -p tcpvisr-engine` → FAIL (unresolved items).

- [ ] **Step 4: Implement the foundation**

`crates/tcpvisr-engine/src/config.rs`:

```rust
//! Engine tuning knobs (design §10.M2, ADR-0006).

use tcpvisr_core::Nanos;

/// Connection-tracker configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineConfig {
    /// Idle gap after which a fresh SYN on the same pair starts a new instance.
    pub dead_after: Nanos,
    /// Minimum backward serial distance that reads as a fresh-ISN reset. Must be `< 2^31`
    /// (no backward serial distance can exceed the midpoint) or the rule is unreachable.
    pub reset_threshold: u32,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self { dead_after: Nanos(120_000_000_000), reset_threshold: 1 << 30 }
    }
}
```

`crates/tcpvisr-engine/src/state.rs`:

```rust
//! Observed connection lifecycle (design §10.M2). Coarser than RFC 793 endpoint states:
//! a wire observer sees both directions but not TIME_WAIT/LAST_ACK.

/// The lifecycle point a connection instance has reached, as observed from the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnState {
    SynSent,
    SynReceived,
    Established,
    FinWait,
    Closed,
    Reset,
}

impl ConnState {
    /// Monotonic rank along the graceful path; `Reset` is a terminal override outside it.
    fn rank(self) -> u8 {
        match self {
            ConnState::SynSent => 0,
            ConnState::SynReceived => 1,
            ConnState::Established => 2,
            ConnState::FinWait => 3,
            ConnState::Closed => 4,
            ConnState::Reset => 5,
        }
    }

    /// Advance to `to` only if it does not move backward along the graceful path. `Reset`
    /// is applied by the caller as an unconditional override, not through here.
    pub(crate) fn advance_to(self, to: ConnState) -> ConnState {
        if to.rank() > self.rank() { to } else { self }
    }
}
```

`crates/tcpvisr-engine/src/conn.rs`:

```rust
//! Connection identity and the reported `Connection` view (design §4, §10.M2).

use tcpvisr_core::{Endpoint, Nanos};

use crate::state::ConnState;

/// The two endpoints of a connection in canonical order (orientation-independent grouping key).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EndpointPair {
    pub low: Endpoint,
    pub high: Endpoint,
}

impl EndpointPair {
    /// Orders the two endpoints so both wire directions map to the same pair.
    #[must_use]
    pub fn new(a: Endpoint, b: Endpoint) -> Self {
        if a <= b { Self { low: a, high: b } } else { Self { low: b, high: a } }
    }
}

/// Instance-aware connection identity (design §4): a pair can carry several instances.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConnId {
    pub pair: EndpointPair,
    pub instance: u32,
}

/// Per-segment direction relative to the connection's chosen orientation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Direction {
    OriginToResponder,
    ResponderToOrigin,
}

/// A tracked connection instance, as reported by [`crate::Tracker::into_connections`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Connection {
    pub id: ConnId,
    pub state: ConnState,
    pub origin: Endpoint,
    pub responder: Endpoint,
    pub origin_inferred: bool,
    pub opened_at: Nanos,
    pub last_at: Nanos,
    pub bytes_o2r: u64,
    pub bytes_r2o: u64,
    pub segments: u64,
}

impl Connection {
    /// Wall span of the instance; saturating because capture time is non-monotonic (§14).
    #[must_use]
    pub fn duration(&self) -> Nanos {
        Nanos(self.last_at.0.saturating_sub(self.opened_at.0))
    }
}
```

`crates/tcpvisr-engine/src/tracker.rs` (foundation only — full `Tracker` lands in Tasks 3–5):

```rust
//! The pure connection tracker (design §10.M2, ADR-0006).

use tcpvisr_core::TcpSeq;

/// `true` when `seq` sits backward of `baseline` in RFC 1982 serial space by more than
/// `threshold` — a drop to a fresh ISN, not a retransmit/reorder or a forward `u32` wrap.
pub(crate) fn is_backward_reset(baseline: TcpSeq, seq: TcpSeq, threshold: u32) -> bool {
    seq.serial_lt(baseline) && baseline.serial_diff(seq) > threshold
}
```

`crates/tcpvisr-engine/src/lib.rs` (replace the stub):

```rust
//! Pure TCP connection state machine + metric derivation (no I/O). M2: connection tracking.

pub mod conn;
pub mod config;
pub mod state;
pub mod tracker;

pub use conn::{ConnId, Connection, EndpointPair};
pub use config::EngineConfig;
pub use state::ConnState;
```
(`tracker`'s public `Tracker`/`track` re-exports are added in Task 5.)

- [ ] **Step 5: Run, expect pass** — `cargo test -p tcpvisr-engine` → PASS (the split tests).

- [ ] **Step 6: Guardrails + commit**

```bash
cargo fmt --all --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test -p tcpvisr-engine
git add crates/tcpvisr-engine
git commit -m "feat(engine): add config, state, conn types, and reset-split helper"
```

---

### Task 3: Tracker — grouping, orientation, byte/duration accounting

**Files:** Modify `crates/tcpvisr-engine/src/tracker.rs`, `src/lib.rs`.

**Interfaces:**
- Produces: `Tracker::new(EngineConfig) -> Tracker`; `Tracker::observe(&mut self, &Item)`;
  `Tracker::into_connections(self) -> Vec<Connection>` (Task 5 adds final ordering);
  internal `ConnTrack` carrying full per-instance state.
- Consumes: `tcpvisr_core::{Item, Segment, TcpFlags, TcpSeq, Nanos, Endpoint}`,
  Task 2's types.

This task creates connections from the **first** segment of each pair, fixes orientation
(bare SYN → SYN-ACK → first-segment fallback), counts per-direction wire payload bytes,
tracks `opened_at`/`last_at` (max) and per-direction sequence baselines (the **max** serial
seq seen per direction). State transitions and instance splitting are stubbed to "single
live instance, state from creation only" here and completed in Tasks 4–5.

- [ ] **Step 1: Write failing tests** — append to `tracker.rs`:

```rust
#[cfg(test)]
mod orient_tests {
    use super::*;
    use core::net::{IpAddr, Ipv4Addr};
    use tcpvisr_core::{FlowKey, Item, Nanos, Segment, TcpFlags, TcpOptions, TcpSeq};

    fn ep(o: u8, p: u16) -> (IpAddr, u16) { (IpAddr::V4(Ipv4Addr::new(10, 0, 0, o)), p) }

    fn seg(src: (IpAddr, u16), dst: (IpAddr, u16), flags: u16, seq: u32, len: u32, ts: u64)
        -> Item {
        Item::Segment(Segment {
            ts: Nanos(ts),
            flow: FlowKey { src_ip: src.0, src_port: src.1, dst_ip: dst.0, dst_port: dst.1 },
            seq: TcpSeq(seq),
            ack: TcpSeq(0),
            flags: TcpFlags(flags),
            window: 0,
            options: TcpOptions::default(),
            payload_len: len,
        })
    }

    #[test]
    fn bare_syn_sets_origin_and_groups_both_directions() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let mut t = Tracker::new(EngineConfig::default());
        t.observe(&seg(c, s, TcpFlags::SYN, 100, 0, 1_000));        // client SYN
        t.observe(&seg(s, c, TcpFlags::SYN | TcpFlags::ACK, 500, 0, 2_000)); // server SYN-ACK
        t.observe(&seg(c, s, TcpFlags::ACK, 101, 10, 3_000));       // 10 bytes c->s
        t.observe(&seg(s, c, TcpFlags::ACK, 501, 20, 4_000));       // 20 bytes s->c
        let conns = t.into_connections();
        assert_eq!(conns.len(), 1, "both directions group into one connection");
        let conn = conns[0];
        assert_eq!((conn.origin.ip, conn.origin.port), c);
        assert_eq!((conn.responder.ip, conn.responder.port), s);
        assert!(!conn.origin_inferred);
        assert_eq!(conn.bytes_o2r, 10);
        assert_eq!(conn.bytes_r2o, 20);
        assert_eq!(conn.segments, 4);
        assert_eq!(conn.duration(), Nanos(3_000));
    }

    #[test]
    fn syn_ack_first_orients_server_as_responder() {
        let (c, s) = (ep(1, 1234), ep(2, 443));
        let mut t = Tracker::new(EngineConfig::default());
        t.observe(&seg(s, c, TcpFlags::SYN | TcpFlags::ACK, 9, 0, 1_000)); // joined mid-handshake
        let conns = t.into_connections();
        assert_eq!((conns[0].origin.ip, conns[0].origin.port), c, "client is origin");
        assert_eq!((conns[0].responder.ip, conns[0].responder.port), s);
        assert!(!conns[0].origin_inferred, "SYN-ACK orientation is observed");
    }

    #[test]
    fn mid_stream_infers_origin_from_first_segment() {
        let (a, b) = (ep(1, 5000), ep(2, 8080));
        let mut t = Tracker::new(EngineConfig::default());
        t.observe(&seg(a, b, TcpFlags::ACK, 42, 5, 1_000)); // no SYN ever
        let conns = t.into_connections();
        assert_eq!((conns[0].origin.ip, conns[0].origin.port), a);
        assert!(conns[0].origin_inferred);
        assert_eq!(conns[0].bytes_o2r, 5);
    }

    #[test]
    fn last_at_is_max_under_reordered_timestamps() {
        let (a, b) = (ep(1, 5000), ep(2, 8080));
        let mut t = Tracker::new(EngineConfig::default());
        t.observe(&seg(a, b, TcpFlags::ACK, 42, 5, 5_000));  // later ts first
        t.observe(&seg(a, b, TcpFlags::ACK, 47, 5, 1_000));  // reordered earlier ts
        let conns = t.into_connections();
        assert_eq!(conns[0].opened_at, Nanos(5_000));
        assert_eq!(conns[0].last_at, Nanos(5_000), "earlier ts must not move last_at back");
        assert_eq!(conns[0].duration(), Nanos(0), "saturating, no panic");
    }
}
```

- [ ] **Step 2: Run, expect fail** — `cargo test -p tcpvisr-engine orient_tests` → FAIL (no `Tracker`).

- [ ] **Step 3: Implement Tracker creation/orientation/accounting** — add to `tracker.rs`:

```rust
use std::collections::HashMap;

use tcpvisr_core::{Endpoint, Item, Nanos, Segment, TcpFlags, TcpSeq};

use crate::conn::{ConnId, Connection, Direction, EndpointPair};
use crate::config::EngineConfig;
use crate::state::ConnState;

fn is_bare_syn(f: TcpFlags) -> bool { f.syn() && !f.ack() }
fn is_syn_ack(f: TcpFlags) -> bool { f.syn() && f.ack() }

/// Full per-instance tracking state (internal). The public view is [`Connection`].
struct ConnTrack {
    id: ConnId,
    state: ConnState,
    origin: Endpoint,
    responder: Endpoint,
    origin_inferred: bool,
    opened_at: Nanos,
    last_at: Nanos,
    bytes_o2r: u64,
    bytes_r2o: u64,
    segments: u64,
    fin_o2r: bool,
    fin_r2o: bool,
    base_o2r: Option<TcpSeq>,
    base_r2o: Option<TcpSeq>,
}

impl ConnTrack {
    fn direction_of(&self, src: Endpoint) -> Direction {
        if src == self.origin {
            Direction::OriginToResponder
        } else {
            Direction::ResponderToOrigin
        }
    }

    fn baseline(&self, dir: Direction) -> Option<TcpSeq> {
        match dir {
            Direction::OriginToResponder => self.base_o2r,
            Direction::ResponderToOrigin => self.base_r2o,
        }
    }

    fn account(&mut self, seg: &Segment, dir: Direction) {
        self.last_at = Nanos(self.last_at.0.max(seg.ts.0));
        self.segments += 1;
        match dir {
            Direction::OriginToResponder => {
                self.bytes_o2r += u64::from(seg.payload_len);
                self.base_o2r = Some(advance_baseline(self.base_o2r, seg.seq));
            }
            Direction::ResponderToOrigin => {
                self.bytes_r2o += u64::from(seg.payload_len);
                self.base_r2o = Some(advance_baseline(self.base_r2o, seg.seq));
            }
        }
    }

    fn view(&self) -> Connection {
        Connection {
            id: self.id,
            state: self.state,
            origin: self.origin,
            responder: self.responder,
            origin_inferred: self.origin_inferred,
            opened_at: self.opened_at,
            last_at: self.last_at,
            bytes_o2r: self.bytes_o2r,
            bytes_r2o: self.bytes_r2o,
            segments: self.segments,
        }
    }
}

/// Max serial seq seen in a direction: keep the more-forward of the two.
fn advance_baseline(current: Option<TcpSeq>, seq: TcpSeq) -> TcpSeq {
    match current {
        Some(base) if base.serial_gt(seq) => base,
        _ => seq,
    }
}

/// Pure per-connection tracker: fold `Item`s in, read `Connection`s out.
pub struct Tracker {
    config: EngineConfig,
    conns: Vec<ConnTrack>,
    live: HashMap<EndpointPair, usize>,
    next_instance: HashMap<EndpointPair, u32>,
}

impl Tracker {
    #[must_use]
    pub fn new(config: EngineConfig) -> Self {
        Self {
            config,
            conns: Vec::new(),
            live: HashMap::new(),
            next_instance: HashMap::new(),
        }
    }

    /// Folds one `Item` into tracker state. `Tick`s are inert in replay (idle is evaluated
    /// per-segment from each segment's own ts); they never create a connection.
    pub fn observe(&mut self, item: &Item) {
        if let Item::Segment(seg) = item {
            self.observe_segment(seg);
        }
    }

    fn observe_segment(&mut self, seg: &Segment) {
        let src = seg.flow.source();
        let dst = seg.flow.destination();
        let pair = EndpointPair::new(src, dst);
        // Task 5 inserts the should-start-new-instance check here. For now: append to the
        // live instance if one exists, else create the first.
        if let Some(&idx) = self.live.get(&pair) {
            let dir = self.conns[idx].direction_of(src);
            self.conns[idx].account(seg, dir);
            // Task 4 applies state transitions here.
            return;
        }
        self.create_instance(pair, seg, src, dst);
    }

    fn create_instance(&mut self, pair: EndpointPair, seg: &Segment, src: Endpoint, dst: Endpoint) {
        let instance = *self.next_instance.entry(pair).or_insert(0);
        self.next_instance.insert(pair, instance + 1);

        let flags = seg.flags;
        let (origin, responder, origin_inferred, state) = if is_bare_syn(flags) {
            (src, dst, false, ConnState::SynSent)
        } else if is_syn_ack(flags) {
            (dst, src, false, ConnState::SynReceived)
        } else if flags.rst() {
            (src, dst, true, ConnState::Reset)
        } else if flags.fin() {
            (src, dst, true, ConnState::FinWait)
        } else {
            (src, dst, true, ConnState::Established)
        };

        let mut track = ConnTrack {
            id: ConnId { pair, instance },
            state,
            origin,
            responder,
            origin_inferred,
            opened_at: seg.ts,
            last_at: seg.ts,
            bytes_o2r: 0,
            bytes_r2o: 0,
            segments: 0,
            fin_o2r: false,
            fin_r2o: false,
            base_o2r: None,
            base_r2o: None,
        };
        let dir = track.direction_of(src);
        track.account(seg, dir);
        if flags.fin() {
            match dir {
                Direction::OriginToResponder => track.fin_o2r = true,
                Direction::ResponderToOrigin => track.fin_r2o = true,
            }
        }
        let idx = self.conns.len();
        self.conns.push(track);
        self.live.insert(pair, idx);
    }

    /// All tracked instances. Final ordering is added in Task 5.
    #[must_use]
    pub fn into_connections(self) -> Vec<Connection> {
        self.conns.iter().map(ConnTrack::view).collect()
    }
}
```

- [ ] **Step 4: Run, expect pass** — `cargo test -p tcpvisr-engine` → PASS.

- [ ] **Step 5: Guardrails + commit**

```bash
cargo fmt --all --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test -p tcpvisr-engine
git add crates/tcpvisr-engine
git commit -m "feat(engine): track connection grouping, orientation, and byte accounting"
```

---

### Task 4: Tracker — state transitions

**Files:** Modify `crates/tcpvisr-engine/src/tracker.rs`.

**Interfaces:** Adds `ConnTrack::apply_state(&mut self, seg, dir)` and calls it from
`observe_segment`'s live-instance branch and from `create_instance` is **not** changed
(creation already sets the initial state). Implements the spec's state-transition table.

- [ ] **Step 1: Write failing tests** — append a `state_tests` module to `tracker.rs` (reuses
  the `ep`/`seg` helpers; declare `use super::orient_tests::{...}` is not possible across
  modules, so duplicate the two tiny helpers or lift them to a shared `#[cfg(test)]` module
  `test_support` at the top of `tracker.rs`). Lift them:

```rust
#[cfg(test)]
mod test_support {
    use core::net::{IpAddr, Ipv4Addr};
    use tcpvisr_core::{FlowKey, Item, Nanos, Segment, TcpFlags, TcpOptions, TcpSeq};

    pub fn ep(o: u8, p: u16) -> (IpAddr, u16) { (IpAddr::V4(Ipv4Addr::new(10, 0, 0, o)), p) }

    #[allow(clippy::too_many_arguments)]
    pub fn seg(
        src: (IpAddr, u16), dst: (IpAddr, u16), flags: u16, seq: u32, ack: u32, len: u32, ts: u64,
    ) -> Item {
        Item::Segment(Segment {
            ts: Nanos(ts),
            flow: FlowKey { src_ip: src.0, src_port: src.1, dst_ip: dst.0, dst_port: dst.1 },
            seq: TcpSeq(seq),
            ack: TcpSeq(ack),
            flags: TcpFlags(flags),
            window: 0,
            options: TcpOptions::default(),
            payload_len: len,
        })
    }
}
```
(Update `orient_tests` to `use super::test_support::{ep, seg}` and drop its inline copies; its
`seg` calls gain an `ack` arg of `0`.)

```rust
#[cfg(test)]
mod state_tests {
    use super::test_support::{ep, seg};
    use super::*;
    use tcpvisr_core::TcpFlags;

    fn run(items: &[tcpvisr_core::Item]) -> Vec<Connection> {
        let mut t = Tracker::new(EngineConfig::default());
        for it in items { t.observe(it); }
        t.into_connections()
    }

    #[test]
    fn three_way_handshake_reaches_established() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let conns = run(&[
            seg(c, s, TcpFlags::SYN, 100, 0, 0, 1),
            seg(s, c, TcpFlags::SYN | TcpFlags::ACK, 500, 101, 0, 2),
            seg(c, s, TcpFlags::ACK, 101, 501, 0, 3),
        ]);
        assert_eq!(conns[0].state, ConnState::Established);
    }

    #[test]
    fn simultaneous_open_reaches_established() {
        let (a, b) = (ep(1, 4000), ep(2, 4001));
        let conns = run(&[
            seg(a, b, TcpFlags::SYN, 10, 0, 0, 1),                 // a SYN -> SynSent, a=origin
            seg(b, a, TcpFlags::SYN, 20, 0, 0, 2),                 // b SYN (responder) -> SynReceived
            seg(a, b, TcpFlags::ACK, 11, 21, 0, 3),
            seg(b, a, TcpFlags::ACK, 21, 11, 0, 4),
        ]);
        assert_eq!(conns.len(), 1);
        assert_eq!(conns[0].state, ConnState::Established);
        assert!(!conns[0].origin_inferred);
    }

    #[test]
    fn graceful_fin_fin_reaches_closed() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let conns = run(&[
            seg(c, s, TcpFlags::ACK, 100, 1, 5, 1),               // mid-stream established
            seg(c, s, TcpFlags::FIN | TcpFlags::ACK, 105, 1, 0, 2),
            seg(s, c, TcpFlags::FIN | TcpFlags::ACK, 1, 106, 0, 3),
        ]);
        assert_eq!(conns[0].state, ConnState::Closed);
    }

    #[test]
    fn rst_overrides_to_reset() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let conns = run(&[
            seg(c, s, TcpFlags::ACK, 100, 1, 5, 1),
            seg(s, c, TcpFlags::RST, 1, 0, 0, 2),
        ]);
        assert_eq!(conns[0].state, ConnState::Reset);
    }

    #[test]
    fn retransmitted_payload_is_recounted_as_wire_bytes() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let conns = run(&[
            seg(c, s, TcpFlags::ACK, 100, 1, 10, 1),  // 10 bytes c->s
            seg(c, s, TcpFlags::ACK, 100, 1, 10, 2),  // retransmit of the same 10 bytes
        ]);
        assert_eq!(conns[0].bytes_o2r, 20, "wire bytes count retransmits (M3 owns goodput)");
        assert_eq!(conns[0].segments, 2);
    }

    #[test]
    fn duplicate_syn_does_not_regress_established() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let conns = run(&[
            seg(c, s, TcpFlags::SYN, 100, 0, 0, 1),
            seg(s, c, TcpFlags::SYN | TcpFlags::ACK, 500, 101, 0, 2),
            seg(c, s, TcpFlags::ACK, 101, 501, 5, 3),             // Established
            seg(c, s, TcpFlags::SYN, 100, 0, 0, 4),               // retransmitted SYN (dup)
        ]);
        assert_eq!(conns.len(), 1, "dup SYN on live conn does not split");
        assert_eq!(conns[0].state, ConnState::Established);
    }
}
```

- [ ] **Step 2: Run, expect fail** — `cargo test -p tcpvisr-engine state_tests` → FAIL
  (states wrong: handshake stuck in `SynSent`, sim-open not `Established`, no `Closed`/`Reset`).

- [ ] **Step 3: Implement `apply_state` and wire it in.** Add to `impl ConnTrack`:

```rust
fn apply_state(&mut self, seg: &Segment, dir: Direction) {
    let f = seg.flags;
    if f.rst() {
        self.state = ConnState::Reset; // terminal override from any state
        return;
    }
    if self.state == ConnState::Reset {
        return; // terminal
    }
    if is_syn_ack(f) {
        if self.state == ConnState::SynSent {
            self.state = self.state.advance_to(ConnState::SynReceived);
        }
    } else if is_bare_syn(f) {
        // A bare SYN from the responder side while we have only seen the origin's SYN is the
        // second leg of a simultaneous open. From the origin side it is a duplicate.
        if self.state == ConnState::SynSent && dir == Direction::ResponderToOrigin {
            self.state = self.state.advance_to(ConnState::SynReceived);
        }
    }
    // The ACK that completes the handshake, or any data, after SYN-ACK -> Established.
    if self.state == ConnState::SynReceived && (f.ack() || seg.payload_len > 0) {
        self.state = self.state.advance_to(ConnState::Established);
    }
    if f.fin() {
        match dir {
            Direction::OriginToResponder => self.fin_o2r = true,
            Direction::ResponderToOrigin => self.fin_r2o = true,
        }
        if self.fin_o2r && self.fin_r2o {
            self.state = self.state.advance_to(ConnState::Closed);
        } else {
            self.state = self.state.advance_to(ConnState::FinWait);
        }
    }
}
```

Update the live-instance branch in `observe_segment` to call it after `account`:

```rust
        if let Some(&idx) = self.live.get(&pair) {
            let dir = self.conns[idx].direction_of(src);
            self.conns[idx].account(seg, dir);
            self.conns[idx].apply_state(seg, dir);
            return;
        }
```

Note: `create_instance` already sets the FIN flag for a FIN-first segment and the initial
state, so it does not call `apply_state` (calling it would double-set; creation is the first
event).

- [ ] **Step 4: Run, expect pass** — `cargo test -p tcpvisr-engine` → PASS.

- [ ] **Step 5: Guardrails + commit**

```bash
cargo fmt --all --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test -p tcpvisr-engine
git add crates/tcpvisr-engine
git commit -m "feat(engine): implement observed connection state transitions"
```

---

### Task 5: Tracker — instance disambiguation + deterministic ordering + `track`

**Files:** Modify `crates/tcpvisr-engine/src/tracker.rs`, `src/lib.rs`.

**Interfaces:**
- Produces: `pub fn track<'a>(items: impl IntoIterator<Item = &'a Item>, config:
  EngineConfig) -> Vec<Connection>`; `Tracker`/`track` re-exported from `lib.rs`;
  `into_connections` sorted by `(opened_at, pair, instance)`.

- [ ] **Step 1: Write failing tests** — append `instance_tests` to `tracker.rs`:

```rust
#[cfg(test)]
mod instance_tests {
    use super::test_support::{ep, seg};
    use super::*;
    use tcpvisr_core::{Item, Nanos, TcpFlags};

    fn run_cfg(items: &[Item], config: EngineConfig) -> Vec<Connection> {
        track(items.iter(), config)
    }

    #[test]
    fn tuple_reuse_new_syn_after_close_splits() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let conns = run_cfg(&[
            seg(c, s, TcpFlags::SYN, 100, 0, 0, 1),
            seg(s, c, TcpFlags::SYN | TcpFlags::ACK, 500, 101, 0, 2),
            seg(c, s, TcpFlags::ACK, 101, 501, 0, 3),
            seg(c, s, TcpFlags::FIN | TcpFlags::ACK, 101, 501, 0, 4),
            seg(s, c, TcpFlags::FIN | TcpFlags::ACK, 501, 102, 0, 5), // Closed
            seg(c, s, TcpFlags::SYN, 9000, 0, 0, 6),                 // reuse: new SYN
        ], EngineConfig::default());
        assert_eq!(conns.len(), 2, "reuse after close is a second instance");
        assert_eq!(conns[0].id.instance, 0);
        assert_eq!(conns[1].id.instance, 1);
        assert_eq!(conns[1].state, ConnState::SynSent);
    }

    #[test]
    fn forward_wrap_stays_one_instance() {
        let (a, b) = (ep(1, 5000), ep(2, 8080));
        let conns = run_cfg(&[
            seg(a, b, TcpFlags::ACK, u32::MAX - 100, 1, 50, 1), // baseline near top
            seg(a, b, TcpFlags::ACK, 200, 1, 50, 2),            // wrapped forward — advance
        ], EngineConfig::default());
        assert_eq!(conns.len(), 1, "a u32 wrap must not split the flow");
    }

    #[test]
    fn large_backward_reset_splits_mid_stream() {
        let (a, b) = (ep(1, 5000), ep(2, 8080));
        let conns = run_cfg(&[
            seg(a, b, TcpFlags::ACK, 0x7000_0000, 1, 50, 1),    // established baseline
            seg(a, b, TcpFlags::ACK, 0x1000_0000, 1, 50, 2),    // 0x6000_0000 backward -> reset
        ], EngineConfig::default());
        assert_eq!(conns.len(), 2, "fresh ISN far below baseline splits");
    }

    #[test]
    fn small_backward_retransmit_does_not_split() {
        let (a, b) = (ep(1, 5000), ep(2, 8080));
        let conns = run_cfg(&[
            seg(a, b, TcpFlags::ACK, 1_000_000, 1, 50, 1),
            seg(a, b, TcpFlags::ACK, 999_000, 1, 50, 2),        // retransmit
        ], EngineConfig::default());
        assert_eq!(conns.len(), 1);
    }

    #[test]
    fn idle_syn_past_dead_after_splits_even_without_close() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let cfg = EngineConfig { dead_after: Nanos(1_000), ..EngineConfig::default() };
        let conns = run_cfg(&[
            seg(c, s, TcpFlags::SYN, 100, 0, 0, 1),
            seg(s, c, TcpFlags::SYN | TcpFlags::ACK, 500, 101, 0, 2), // SynReceived, live
            seg(c, s, TcpFlags::SYN, 9000, 0, 0, 10_000),            // 9998ns later: idle reuse
        ], cfg);
        assert_eq!(conns.len(), 2, "SYN after idle > dead_after starts a new instance");
    }
}
```

- [ ] **Step 2: Run, expect fail** — `cargo test -p tcpvisr-engine instance_tests` → FAIL
  (currently one instance for all; no split, no `track`).

- [ ] **Step 3: Implement splitting, ordering, and `track`.** Add the decision method and
  rewire `observe_segment`:

```rust
impl Tracker {
    /// Whether `seg` should open a new instance instead of joining the live one at `idx`.
    fn should_split(&self, idx: usize, seg: &Segment, src: Endpoint) -> bool {
        let track = &self.conns[idx];
        let f = seg.flags;
        if is_bare_syn(f) {
            let terminal = matches!(track.state, ConnState::Closed | ConnState::Reset);
            let idle = seg.ts.0.saturating_sub(track.last_at.0) > self.config.dead_after.0;
            return terminal || idle;
        }
        // SYN-less mid-stream reset: only meaningful on an established, live instance.
        if track.state == ConnState::Established {
            let dir = track.direction_of(src);
            if let Some(base) = track.baseline(dir) {
                return is_backward_reset(base, seg.seq, self.config.reset_threshold);
            }
        }
        false
    }
}
```

Rewrite `observe_segment`:

```rust
    fn observe_segment(&mut self, seg: &Segment) {
        let src = seg.flow.source();
        let dst = seg.flow.destination();
        let pair = EndpointPair::new(src, dst);
        if let Some(&idx) = self.live.get(&pair) {
            if !self.should_split(idx, seg, src) {
                let dir = self.conns[idx].direction_of(src);
                self.conns[idx].account(seg, dir);
                self.conns[idx].apply_state(seg, dir);
                return;
            }
        }
        self.create_instance(pair, seg, src, dst);
    }
```

Replace `into_connections` with a sorted version:

```rust
    /// All tracked instances, ordered by `(opened_at, pair, instance)` for determinism.
    #[must_use]
    pub fn into_connections(self) -> Vec<Connection> {
        let mut out: Vec<Connection> = self.conns.iter().map(ConnTrack::view).collect();
        out.sort_by_key(|c| (c.opened_at, c.id.pair, c.id.instance));
        out
    }
```

Add the free function at the end of `tracker.rs`:

```rust
/// Tracks every connection in `items` and returns the reported connections (test convenience).
#[must_use]
pub fn track<'a>(items: impl IntoIterator<Item = &'a Item>, config: EngineConfig) -> Vec<Connection> {
    let mut tracker = Tracker::new(config);
    for item in items {
        tracker.observe(item);
    }
    tracker.into_connections()
}
```

Add to `lib.rs`:

```rust
pub use tracker::{Tracker, track};
```

`sort_by_key` needs `Nanos: Ord` (already derived) and `EndpointPair: Ord` (derived in Task
2). `Connection` is `Copy`.

- [ ] **Step 4: Run, expect pass** — `cargo test -p tcpvisr-engine` → PASS (all engine tests
  + proptests).

- [ ] **Step 5: Guardrails + commit**

```bash
cargo fmt --all --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test -p tcpvisr-engine
git add crates/tcpvisr-engine
git commit -m "feat(engine): disambiguate connection instances and order output"
```

---

### Task 6: fixtures — builder, five committed `.pcap`, drift guard

**Files:**
- Modify: `crates/tcp-visr/Cargo.toml` (add `[dev-dependencies] etherparse = "=0.20.2"`).
- Create: `crates/tcp-visr/tests/support/mod.rs`, `crates/tcp-visr/tests/fixtures/*.pcap`,
  `crates/tcp-visr/tests/drift.rs`.

**Interfaces:**
- Produces a `support` module with `fixture_set() -> Vec<(&'static str, Vec<u8>)>` and a
  `write_fixture` helper, mirroring M1's `tests/support` (`legacy_pcap`, `ethernet`, `Pkt`)
  but with a flexible `tcp(...)` builder taking explicit flags/seq/ack/payload and **strictly
  increasing** per-packet microsecond timestamps.

- [ ] **Step 1: Write the drift-guard test first** — `crates/tcp-visr/tests/drift.rs`:

```rust
//! The committed M2 fixtures must byte-match the builder output (regenerate on change).
mod support;

#[test]
fn committed_fixtures_match_builder() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
    for (name, bytes) in support::fixture_set() {
        let path = std::path::Path::new(dir).join(name);
        let on_disk = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        assert_eq!(on_disk, bytes, "committed {name} is stale; regenerate fixtures");
    }
}
```

- [ ] **Step 2: Run, expect fail** — `cargo test -p tcp-visr --test drift` → FAIL (no
  `support`, no fixtures).

- [ ] **Step 3: Write the builder** — `crates/tcp-visr/tests/support/mod.rs`:

```rust
//! Deterministic M2 capture-fixture builder. Emits real legacy `.pcap` bytes (Ethernet II,
//! IPv4, TCP) with explicit flags/seq/ack/payload and strictly increasing microsecond
//! timestamps, so fixtures are reviewable as source. No clock or randomness.

#![allow(dead_code)]
#![allow(clippy::expect_used, clippy::cast_possible_truncation)]

use etherparse::PacketBuilder;

const DLT_EN10MB: u16 = 1;
const C: [u8; 4] = [10, 0, 0, 1]; // client
const S: [u8; 4] = [10, 0, 0, 2]; // server

/// TCP flag bits, OR-combined.
pub mod flag {
    pub const FIN: u16 = 0x01;
    pub const SYN: u16 = 0x02;
    pub const RST: u16 = 0x04;
    pub const ACK: u16 = 0x10;
}

/// One built Ethernet+IPv4+TCP frame with `n` payload bytes.
#[must_use]
pub fn tcp(src: [u8; 4], dst: [u8; 4], sp: u16, dp: u16, flags: u16, seq: u32, ack: u32, n: usize)
    -> Vec<u8> {
    let mut b = PacketBuilder::ethernet2([2, 0, 0, 0, 0, 1], [2, 0, 0, 0, 0, 2])
        .ipv4(src, dst, 64)
        .tcp(sp, dp, seq, 64240);
    if flags & flag::SYN != 0 { b = b.syn(); }
    if flags & flag::ACK != 0 { b = b.ack(ack); }
    if flags & flag::FIN != 0 { b = b.fin(); }
    if flags & flag::RST != 0 { b = b.rst(); }
    let mut buf = Vec::new();
    b.write(&mut buf, &vec![0xab; n]).expect("build tcp frame");
    buf
}

/// A legacy `.pcap` (microsecond magic, little-endian). `frames[i] = (ts_us, bytes)`.
#[must_use]
pub fn legacy_pcap(frames: &[(u64, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&0xa1b2_c3d4u32.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&4u16.to_le_bytes());
    out.extend_from_slice(&0i32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&65_535u32.to_le_bytes());
    out.extend_from_slice(&u32::from(DLT_EN10MB).to_le_bytes());
    for (ts_us, data) in frames {
        out.extend_from_slice(&((ts_us / 1_000_000) as u32).to_le_bytes());
        out.extend_from_slice(&((ts_us % 1_000_000) as u32).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(data);
    }
    out
}

/// The five committed M2 fixtures (one per DoD scenario). Strictly increasing timestamps.
#[must_use]
pub fn fixture_set() -> Vec<(&'static str, Vec<u8>)> {
    use flag::{ACK, FIN, RST, SYN};
    let (cp, sp) = (1234u16, 80u16);
    vec![
        ("mid_stream.pcap", legacy_pcap(&[
            (1_000, tcp(C, S, cp, sp, ACK, 100, 1, 10)),
            (2_000, tcp(S, C, cp_swap_ack(), 1, 0)),
        ].into_iter().map(annotate).collect::<Vec<_>>())),
        // ... see Step 3b for the full, explicit fixture_set
    ]
}
```

  **Step 3b — write the real `fixture_set` (no helper shortcuts).** Replace the sketch above
  with explicit frames; each fixture's expected outcome is the comment:

```rust
#[must_use]
pub fn fixture_set() -> Vec<(&'static str, Vec<u8>)> {
    use flag::{ACK, FIN, RST, SYN};
    let (cp, sp) = (1234u16, 80u16);
    vec![
        // 1 connection, Established, origin_inferred (no SYN seen).
        ("mid_stream.pcap", legacy_pcap(&[
            (1_000, tcp(C, S, cp, sp, ACK, 100, 1, 10)),
            (2_000, tcp(S, C, sp, cp, ACK, 1, 110, 20)),
            (3_000, tcp(C, S, cp, sp, ACK, 110, 21, 30)),
        ])),
        // 1 connection reaching Established, NOT origin_inferred (simultaneous open).
        ("sim_open.pcap", legacy_pcap(&[
            (1_000, tcp(C, S, cp, sp, SYN, 10, 0, 0)),
            (2_000, tcp(S, C, sp, cp, SYN, 20, 0, 0)),
            (3_000, tcp(C, S, cp, sp, ACK, 11, 21, 0)),
            (4_000, tcp(S, C, sp, cp, ACK, 21, 11, 0)),
        ])),
        // 1 connection, terminal Reset (mid-stream then RST).
        ("mid_rst.pcap", legacy_pcap(&[
            (1_000, tcp(C, S, cp, sp, ACK, 100, 1, 40)),
            (2_000, tcp(S, C, sp, cp, ACK, 1, 140, 0)),
            (3_000, tcp(S, C, sp, cp, RST, 1, 0, 0)),
        ])),
        // 2 connections (instance 0 then 1) for one pair: close, then a new SYN reuse.
        ("tuple_reuse.pcap", legacy_pcap(&[
            (1_000, tcp(C, S, cp, sp, SYN, 100, 0, 0)),
            (2_000, tcp(S, C, sp, cp, SYN | ACK, 500, 101, 0)),
            (3_000, tcp(C, S, cp, sp, ACK, 101, 501, 0)),
            (4_000, tcp(C, S, cp, sp, FIN | ACK, 101, 501, 0)),
            (5_000, tcp(S, C, sp, cp, FIN | ACK, 501, 102, 0)),
            (6_000, tcp(C, S, cp, sp, SYN, 9000, 0, 0)),
        ])),
        // 1 connection, 1 instance: seq advances across the u32 boundary (forward wrap).
        ("seq_wrap.pcap", legacy_pcap(&[
            (1_000, tcp(C, S, cp, sp, ACK, u32::MAX - 100, 1, 50)),
            (2_000, tcp(C, S, cp, sp, ACK, 200, 1, 50)),
            (3_000, tcp(S, C, sp, cp, ACK, 1, 300, 10)),
        ])),
    ]
}
```
(Delete the Step-3a sketch and `annotate`/`cp_swap_ack` placeholders entirely — they were
illustrative. Only the explicit `fixture_set` above ships.)

- [ ] **Step 4: Generate and commit the fixtures.** Add a tiny ignored generator test so the
  bytes are reproducible, then run it to write the files:

```rust
// in crates/tcp-visr/tests/drift.rs
#[test]
#[ignore = "regenerates committed fixtures; run explicitly"]
fn regenerate_fixtures() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
    std::fs::create_dir_all(dir).unwrap();
    for (name, bytes) in support::fixture_set() {
        std::fs::write(std::path::Path::new(dir).join(name), bytes).unwrap();
    }
}
```

Run: `cargo test -p tcp-visr --test drift regenerate_fixtures -- --ignored`

- [ ] **Step 5: Run the drift guard, expect pass** —
  `cargo test -p tcp-visr --test drift committed_fixtures_match_builder` → PASS.

- [ ] **Step 6: Guardrails + commit**

```bash
cargo fmt --all --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test -p tcp-visr --test drift
git add crates/tcp-visr/Cargo.toml crates/tcp-visr/tests/support crates/tcp-visr/tests/drift.rs crates/tcp-visr/tests/fixtures
git commit -m "test(cli): add M2 connection fixtures and drift guard"
```

---

### Task 7: `conns` CLI

**Files:** Modify `crates/tcp-visr/Cargo.toml` (add `tcpvisr-engine` dep),
`crates/tcp-visr/src/main.rs`; create `crates/tcp-visr/tests/conns.rs`.

**Interfaces:** Consumes `tcpvisr_engine::{Tracker, EngineConfig, ConnState}` and
`tcpvisr_ingest::parse_file_visit`. Produces the `conns` subcommand behavior.

- [ ] **Step 1: Write failing integration tests** — `crates/tcp-visr/tests/conns.rs`:

```rust
use std::process::Command;

fn bin() -> Command { Command::new(env!("CARGO_BIN_EXE_tcp-visr")) }

fn fixture(name: &str) -> String {
    format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
}

fn conns(name: &str) -> String {
    let out = bin().args(["conns", &fixture(name)]).output().unwrap();
    assert!(out.status.success(), "conns {name} exited nonzero");
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn mid_stream_is_one_established_inferred_connection() {
    let o = conns("mid_stream.pcap");
    assert!(o.contains("state=Established"), "{o}");
    assert!(o.contains("(mid-stream)"), "{o}");
    assert!(o.contains("1 connections"), "{o}");
}

#[test]
fn sim_open_reaches_established_not_inferred() {
    let o = conns("sim_open.pcap");
    assert!(o.contains("state=Established"), "{o}");
    assert!(!o.contains("(mid-stream)"), "{o}");
}

#[test]
fn mid_rst_is_reset() {
    assert!(conns("mid_rst.pcap").contains("state=Reset"));
}

#[test]
fn tuple_reuse_lists_two_instances() {
    let o = conns("tuple_reuse.pcap");
    assert!(o.contains("inst=0"), "{o}");
    assert!(o.contains("inst=1"), "{o}");
    assert!(o.contains("2 connections"), "{o}");
}

#[test]
fn seq_wrap_stays_one_connection() {
    assert!(conns("seq_wrap.pcap").contains("1 connections"));
}

#[test]
fn missing_file_exits_nonzero_with_actionable_message() {
    let out = bin().args(["conns", "/no/such/file.pcap"]).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8(out.stderr).unwrap().contains("opening capture"));
}
```

- [ ] **Step 2: Run, expect fail** — `cargo test -p tcp-visr --test conns` → FAIL
  (`conns` still prints "not implemented").

- [ ] **Step 3: Implement.** Add to `crates/tcp-visr/Cargo.toml` `[dependencies]`:

```toml
tcpvisr-engine = { path = "../tcpvisr-engine" }
```

In `crates/tcp-visr/src/main.rs`, change the `Conns` variant and add a handler.

Replace the variant:

```rust
    /// List connections in a capture.
    Conns {
        /// The `.pcap`/`.pcapng` capture file to analyze.
        file: PathBuf,
    },
```

Update `Command::name` arm: `Command::Conns { .. } => "conns",`.

Add `Command::Conns { file } => run_conns(&file),` to the `match command` in `run`.

Add the handler (mirrors `run_parse`'s streaming + lint-safe stdout):

```rust
/// Streams `file` through the replay faucet into the engine and prints one line per
/// connection plus a skip summary.
fn run_conns(file: &Path) -> Result<(), Box<dyn std::error::Error>> {
    use tcpvisr_engine::{EngineConfig, Tracker};

    let mut tracker = Tracker::new(EngineConfig::default());
    let (_link, skipped) = tcpvisr_ingest::parse_file_visit(file, &mut |item| {
        tracker.observe(item);
    })?;

    let mut out = std::io::stdout().lock();
    let conns = tracker.into_connections();
    for c in &conns {
        let marker = if c.origin_inferred { " (mid-stream)" } else { "" };
        writeln!(
            out,
            "{} -> {}  state={:?}  inst={}  bytes={}/{}  segs={}  dur={}{marker}",
            c.origin, c.responder, c.state, c.id.instance,
            c.bytes_o2r, c.bytes_r2o, c.segments, c.duration(),
        )?;
    }
    let reasons: Vec<String> = skipped
        .nonzero()
        .into_iter()
        .map(|(reason, n)| format!("{reason}={n}"))
        .collect();
    let breakdown = if reasons.is_empty() {
        String::new()
    } else {
        format!(" ({})", reasons.join(", "))
    };
    writeln!(
        out,
        "{} connections, skipped: {} total{breakdown}",
        conns.len(),
        skipped.total()
    )?;
    Ok(())
}
```

`ConnState` prints via `{:?}` (derives `Debug`), yielding `Established`/`Reset`/… exactly as
the tests expect. No new `Display` impl is needed.

- [ ] **Step 4: Run, expect pass** —
  `cargo test -p tcp-visr` → PASS (existing `cli.rs` unchanged + new `conns.rs`).

- [ ] **Step 5: Full workspace guardrails + commit**

```bash
cargo fmt --all --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test --workspace
git add crates/tcp-visr/Cargo.toml crates/tcp-visr/src/main.rs crates/tcp-visr/tests/conns.rs
git commit -m "feat(cli): list connections with conns subcommand"
```

---

## Self-Review

**Spec coverage:** Endpoint (T1) · EngineConfig/ConnState/ConnId/EndpointPair/Connection/
Tracker (T2–T5) · orientation incl. SYN-ACK (T3) · non-monotonic time (T3) · state table incl.
sim-open + FIN/FIN + RST + monotonic (T4) · instance split: SYN-after-terminal/idle,
backward-reset, forward-wrap (T5) · proptest split boundary (T2) · per-direction wire bytes incl.
retransmit (T3 byte test; retransmit re-count covered by `small_backward_retransmit` segment in
T5 which re-accounts) · five fixtures + drift guard (T6) · `conns` CLI + missing-file error (T7).
**Carried note honored:** baseline = max serial seq per direction (`advance_baseline`, T3).

**Placeholder scan:** Step 3a of Task 6 is explicitly a discarded sketch; Step 3b is the
shipping code. No `TODO`/`TBD`/"handle edge cases" remain.

**Type consistency:** `Tracker::new`/`observe`/`into_connections`/`track`, `ConnTrack::account`/
`apply_state`/`direction_of`/`baseline`/`view`, `advance_baseline`, `is_backward_reset`,
`EndpointPair::new`, `ConnState::advance_to` are referenced consistently across tasks.

**Gap fixed during review:** the retransmit byte-recount assertion the spec calls for now has
its own test (`retransmitted_payload_is_recounted_as_wire_bytes` in Task 4's `state_tests`).
