# M1 — Packet Model & Replay Parser Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Decode `.pcap`/`.pcapng` captures into TCP `Segment`s through one shared header
decoder used by two faucets (pure-Rust `pcap-parser`; libpcap behind a default-off `live`
feature), exposed as `tcp-visr parse <file>`, and guarded by a cross-faucet parity test.

**Architecture:** `tcpvisr-core` defines the pure wire model (`Nanos`, `TcpSeq` + RFC 1982
arithmetic, `FlowKey`, `TcpFlags`, `TcpOptions`, `Segment`, `Item`). `tcpvisr-ingest` strips
each link layer to IP bytes and hands them to one `decode_frame` (etherparse `LaxSlicedPacket::
from_ip` → TCP `Segment` or a counted skip reason). Both faucets feed the same `decode_frame`;
they differ only in the container reader. Fixtures are code-generated, committed, drift-guarded.

**Tech Stack:** Rust 1.88.0 (edition 2024). `pcap-parser` =0.17.0, `etherparse` =0.20.2,
`thiserror` =2.0.18 (ingest); `pcap` =2.4.0 (optional, `live`); `proptest` =1.11.0 (dev).

## Global Constraints

- **Toolchain = MSRV = 1.88.0**; edition 2024; pin deps with `=`.
- **Lint policy (workspace `[lints]`)**: no `unwrap`/`expect`/`panic!`/`println!`/`eprintln!`/
  `process::exit`/`#[allow]`/`todo!` in non-test code. Errors are `Result`. `clippy::pedantic`
  is `warn`-by-default and CI runs `-D warnings`, so pedantic findings must be resolved.
- **Restriction lints relaxed in tests** via `clippy.toml` (`allow-unwrap-in-tests`, etc.) —
  tests may `unwrap`.
- **No stdout macros**: print via `writeln!(io::stdout().lock(), …)`, not `println!`.
- **Conventional Commits**, imperative ≤72-char subject, end every commit body with
  `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- **`live` feature is default-off** (ADR-0005); the default build/test must compile and pass
  with no libpcap. Anything behind `live` is `#[cfg(feature = "live")]`.
- **Guardrails before every commit** (run the ones touching changed code):
  `cargo fmt --all --check` · `cargo clippy --all-targets --all-features -- -D warnings`
  (needs `libpcap-dev`) · `cargo test --workspace` · for faucet/parity work also
  `cargo test -p tcpvisr-ingest --features live` · `cargo deny check`.

---

## File Structure

```
crates/tcpvisr-core/
  Cargo.toml                 # + [dev-dependencies] proptest = "=1.11.0"
  src/lib.rs                 # module decls + re-exports
  src/time.rs                # Nanos
  src/seq.rs                 # TcpSeq + RFC 1982 (+ proptest module)
  src/flow.rs                # FlowKey (+ Display)
  src/segment.rs             # TcpFlags, TcpOptions, Segment, Item
crates/tcpvisr-ingest/
  Cargo.toml                 # + etherparse, pcap-parser, thiserror; [features] live=["dep:pcap"]
  src/lib.rs                 # public API: parse_file, parse_file_libpcap(live), ReplayParse,
                             #   SkipCounts, SkipReason, IngestError, LinkType
  src/link.rs                # LinkType, strip_link → IP bytes or skip
  src/decode.rs              # decode_frame, DecodeOutcome, TCP/option decode
  src/replay.rs              # pure-Rust faucet over pcap-parser create_reader
  src/libpcap.rs             # #[cfg(feature="live")] libpcap from_file faucet
  tests/support/mod.rs       # fixture byte builder (pcap + pcapng)
  tests/fixtures/*.pcap[ng]  # committed generated fixtures
  tests/parse.rs             # decode/skip behavior over committed fixtures
  tests/parity.rs            # #[cfg(feature="live")] cross-faucet parity
  tests/drift.rs             # committed fixtures == builder output
crates/tcp-visr/
  src/main.rs                # Parse { file } → pure-Rust faucet → stdout
  tests/cli.rs               # + parse subcommand integration tests
.github/workflows/ci.yml     # install libpcap-dev; add --features live test step
deny.toml                    # extend license allow-list
```

---

## Task 1: Core wire model (`tcpvisr-core`)

**Files:**
- Modify: `crates/tcpvisr-core/Cargo.toml` (add proptest dev-dep)
- Create: `crates/tcpvisr-core/src/{time,seq,flow,segment}.rs`
- Modify: `crates/tcpvisr-core/src/lib.rs`

**Interfaces — Produces:**
- `Nanos(pub u64)` — `Debug,Clone,Copy,PartialEq,Eq,PartialOrd,Ord,Hash`; `impl Display`.
- `TcpSeq(pub u32)` — `Debug,Clone,Copy,PartialEq,Eq,Hash`; methods
  `serial_lt(self, TcpSeq)->bool`, `serial_gt(self, TcpSeq)->bool`,
  `serial_diff(self, earlier: TcpSeq)->u32`.
- `FlowKey { src_ip: IpAddr, src_port: u16, dst_ip: IpAddr, dst_port: u16 }` —
  `Debug,Clone,Copy,PartialEq,Eq,Hash`; `impl Display` (`a:p → b:q`, v6 bracketed).
- `TcpFlags(pub u16)` — `Debug,Clone,Copy,PartialEq,Eq,Hash`; bit consts `FIN=0x01 SYN=0x02
  RST=0x04 PSH=0x08 ACK=0x10 URG=0x20 ECE=0x40 CWR=0x80 NS=0x100`; bool accessors
  `fin/syn/rst/psh/ack/urg/ece/cwr/ns`; `impl Display` (`SYN|ACK`, `.` if none).
- `TcpOptions { mss: Option<u16>, window_scale: Option<u8>, sack_permitted: bool,
  timestamp: Option<(u32,u32)>, sack_blocks: Vec<(TcpSeq,TcpSeq)> }` —
  `Debug,Clone,PartialEq,Eq,Default`.
- `Segment { ts: Nanos, flow: FlowKey, seq: TcpSeq, ack: TcpSeq, flags: TcpFlags,
  window: u16, options: TcpOptions, payload_len: u32 }` — `Debug,Clone,PartialEq,Eq`.
- `enum Item { Segment(Segment), Tick(Nanos) }` — `Debug,Clone,PartialEq,Eq`.

- [ ] **Step 1: Add proptest dev-dependency**

In `crates/tcpvisr-core/Cargo.toml` append:
```toml
[dev-dependencies]
proptest = "=1.11.0"
```

- [ ] **Step 2: Write failing serial-arithmetic tests** — `crates/tcpvisr-core/src/seq.rs`

```rust
//! TCP serial-number arithmetic (RFC 1982). The single most error-prone area (design §4).

/// A TCP sequence or acknowledgement number. Wraps mod 2^32 under RFC 1982 comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TcpSeq(pub u32);

const HALF: u32 = 0x8000_0000;

impl TcpSeq {
    /// `true` if `self` precedes `other` in RFC 1982 serial order.
    #[must_use]
    pub fn serial_lt(self, other: TcpSeq) -> bool {
        let forward = other.0.wrapping_sub(self.0);
        forward != 0 && forward < HALF
    }
    /// `true` if `self` follows `other` in RFC 1982 serial order.
    #[must_use]
    pub fn serial_gt(self, other: TcpSeq) -> bool {
        other.serial_lt(self)
    }
    /// Forward distance from `earlier` to `self` (bytes advanced, wrapping).
    #[must_use]
    pub fn serial_diff(self, earlier: TcpSeq) -> u32 {
        self.0.wrapping_sub(earlier.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn forward_small_step_is_less() {
        assert!(TcpSeq(10).serial_lt(TcpSeq(20)));
        assert!(TcpSeq(20).serial_gt(TcpSeq(10)));
    }

    #[test]
    fn wrap_forward_is_still_less_not_greater() {
        // near u32::MAX, +16 wraps forward — must read as advance, not a new instance.
        let a = TcpSeq(u32::MAX - 5);
        let b = TcpSeq(10); // a + 16 (wrapped)
        assert!(a.serial_lt(b));
        assert!(!a.serial_gt(b));
        assert_eq!(b.serial_diff(a), 16);
    }

    #[test]
    fn irreflexive() {
        assert!(!TcpSeq(42).serial_lt(TcpSeq(42)));
        assert!(!TcpSeq(42).serial_gt(TcpSeq(42)));
    }

    proptest! {
        #[test]
        fn forward_distance_under_half_is_lt(base in any::<u32>(), d in 1u32..HALF) {
            let a = TcpSeq(base);
            let b = TcpSeq(base.wrapping_add(d));
            prop_assert!(a.serial_lt(b));
            prop_assert!(b.serial_gt(a));
            prop_assert_eq!(b.serial_diff(a), d);
        }

        #[test]
        fn antisymmetric_off_half(a in any::<u32>(), b in any::<u32>()) {
            let (x, y) = (TcpSeq(a), TcpSeq(b));
            let d = b.wrapping_sub(a);
            prop_assume!(a != b && d != HALF); // exact half is intentionally undefined
            prop_assert_ne!(x.serial_lt(y), y.serial_lt(x));
        }
    }
}
```

- [ ] **Step 3: Run tests, expect compile failure** —
  `cargo test -p tcpvisr-core seq::` ⇒ FAIL (module not wired into lib yet).

- [ ] **Step 4: Wire module + the other types**

`crates/tcpvisr-core/src/time.rs`:
```rust
//! Capture-relative time in nanoseconds (design §4.1).

use core::fmt;

/// Nanoseconds since the capture's first packet record (design §4.1, M1 spec).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Nanos(pub u64);

impl fmt::Display for Nanos {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (s, ns) = (self.0 / 1_000_000_000, self.0 % 1_000_000_000);
        write!(f, "{s}.{ns:09}s")
    }
}
```

`crates/tcpvisr-core/src/flow.rs`:
```rust
//! The TCP 4-tuple as seen on the wire (design §4). Direction is connection-relative (M2).

use core::fmt;
use core::net::IpAddr;

/// TCP 4-tuple. Protocol is implicit (TCP-only). Stored as-seen; not canonicalized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FlowKey {
    pub src_ip: IpAddr,
    pub src_port: u16,
    pub dst_ip: IpAddr,
    pub dst_port: u16,
}

fn write_endpoint(f: &mut fmt::Formatter<'_>, ip: IpAddr, port: u16) -> fmt::Result {
    match ip {
        IpAddr::V4(a) => write!(f, "{a}:{port}"),
        IpAddr::V6(a) => write!(f, "[{a}]:{port}"),
    }
}

impl fmt::Display for FlowKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_endpoint(f, self.src_ip, self.src_port)?;
        write!(f, " -> ")?;
        write_endpoint(f, self.dst_ip, self.dst_port)
    }
}
```

`crates/tcpvisr-core/src/segment.rs`:
```rust
//! Wire-decoded TCP segment model and the engine-input `Item` (design §3.2, §4).

use core::fmt;

use crate::flow::FlowKey;
use crate::seq::TcpSeq;
use crate::time::Nanos;

/// TCP control bits (design §4). Bit values match the on-wire flags field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TcpFlags(pub u16);

impl TcpFlags {
    pub const FIN: u16 = 0x01;
    pub const SYN: u16 = 0x02;
    pub const RST: u16 = 0x04;
    pub const PSH: u16 = 0x08;
    pub const ACK: u16 = 0x10;
    pub const URG: u16 = 0x20;
    pub const ECE: u16 = 0x40;
    pub const CWR: u16 = 0x80;
    pub const NS: u16 = 0x100;

    #[must_use] pub fn fin(self) -> bool { self.0 & Self::FIN != 0 }
    #[must_use] pub fn syn(self) -> bool { self.0 & Self::SYN != 0 }
    #[must_use] pub fn rst(self) -> bool { self.0 & Self::RST != 0 }
    #[must_use] pub fn psh(self) -> bool { self.0 & Self::PSH != 0 }
    #[must_use] pub fn ack(self) -> bool { self.0 & Self::ACK != 0 }
    #[must_use] pub fn urg(self) -> bool { self.0 & Self::URG != 0 }
    #[must_use] pub fn ece(self) -> bool { self.0 & Self::ECE != 0 }
    #[must_use] pub fn cwr(self) -> bool { self.0 & Self::CWR != 0 }
    #[must_use] pub fn ns(self) -> bool { self.0 & Self::NS != 0 }
}

impl fmt::Display for TcpFlags {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let parts = [
            (self.syn(), "SYN"), (self.ack(), "ACK"), (self.fin(), "FIN"),
            (self.rst(), "RST"), (self.psh(), "PSH"), (self.urg(), "URG"),
            (self.ece(), "ECE"), (self.cwr(), "CWR"), (self.ns(), "NS"),
        ];
        let mut wrote = false;
        for (on, name) in parts {
            if on {
                if wrote { write!(f, "|")?; }
                write!(f, "{name}")?;
                wrote = true;
            }
        }
        if !wrote { write!(f, ".")?; }
        Ok(())
    }
}

/// Parsed summary of the TCP options M1 cares about (design §4).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TcpOptions {
    pub mss: Option<u16>,
    pub window_scale: Option<u8>,
    pub sack_permitted: bool,
    pub timestamp: Option<(u32, u32)>,
    pub sack_blocks: Vec<(TcpSeq, TcpSeq)>,
}

/// One decoded TCP segment as seen on the wire (design §4). `direction` is M2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub ts: Nanos,
    pub flow: FlowKey,
    pub seq: TcpSeq,
    pub ack: TcpSeq,
    pub flags: TcpFlags,
    pub window: u16,
    pub options: TcpOptions,
    pub payload_len: u32,
}

/// Engine input (design §3.2). Replay emits `Segment`; `Tick` is live-only (M11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    Segment(Segment),
    Tick(Nanos),
}
```

`crates/tcpvisr-core/src/lib.rs`:
```rust
//! Shared types for tcp-visr: `FlowKey`, `Item`, `Segment`, `TcpSeq` serial arithmetic,
//! time units. See docs/design/tcp-visr-design.md §3.1.

pub mod flow;
pub mod seq;
pub mod segment;
pub mod time;

pub use flow::FlowKey;
pub use seq::TcpSeq;
pub use segment::{Item, Segment, TcpFlags, TcpOptions};
pub use time::Nanos;
```

- [ ] **Step 5: Add Display + flags unit tests** in `segment.rs`/`flow.rs`/`time.rs`:
```rust
// segment.rs tests
#[test]
fn flags_display_lists_set_bits() {
    assert_eq!(TcpFlags(TcpFlags::SYN | TcpFlags::ACK).to_string(), "SYN|ACK");
    assert_eq!(TcpFlags(0).to_string(), ".");
}
// flow.rs tests
#[test]
fn flow_display_brackets_v6() {
    use core::net::{IpAddr, Ipv6Addr, Ipv4Addr};
    let v4 = FlowKey { src_ip: IpAddr::V4(Ipv4Addr::LOCALHOST), src_port: 1,
                       dst_ip: IpAddr::V4(Ipv4Addr::new(10,0,0,1)), dst_port: 80 };
    assert_eq!(v4.to_string(), "127.0.0.1:1 -> 10.0.0.1:80");
    let v6 = FlowKey { src_ip: IpAddr::V6(Ipv6Addr::LOCALHOST), src_port: 1,
                       dst_ip: IpAddr::V6(Ipv6Addr::LOCALHOST), dst_port: 80 };
    assert_eq!(v6.to_string(), "[::1]:1 -> [::1]:80");
}
```

- [ ] **Step 6: Run guardrails** —
  `cargo test -p tcpvisr-core` PASS; `cargo clippy -p tcpvisr-core --all-targets -- -D warnings`
  clean; `cargo fmt --all --check` clean.

- [ ] **Step 7: Commit** — `feat(core): add wire packet model and RFC 1982 serial arithmetic`

---

## Task 2: Shared decoder + link layers (`tcpvisr-ingest`)

**Files:**
- Modify: `crates/tcpvisr-ingest/Cargo.toml` (deps + `live` feature)
- Create: `crates/tcpvisr-ingest/src/{link,decode}.rs`
- Modify: `crates/tcpvisr-ingest/src/lib.rs`

**Interfaces — Consumes:** Task 1 (`Segment`, `FlowKey`, `TcpSeq`, `TcpFlags`, `TcpOptions`,
`Nanos`, `Item`).
**Produces:**
- `enum LinkType { Ethernet, LinuxSll, LinuxSll2, RawIp, Null }` with
  `from_dlt(u16) -> Option<LinkType>` (EN10MB=1, NULL=0, RAW=101, LINUX_SLL=113,
  LINUX_SLL2=276).
- `enum SkipReason { NonTcp, Malformed, UnsupportedLinkType, Ipv6Fragment,
  UnsupportedExtChain, Truncated }` — `Debug,Clone,Copy,PartialEq,Eq,Hash`.
- `enum DecodeOutcome { Decoded(Segment), Skipped(SkipReason) }`.
- `fn decode_frame(link: LinkType, ts: Nanos, frame: &[u8]) -> DecodeOutcome`.

**Cargo.toml additions:**
```toml
[dependencies]
tcpvisr-core = { path = "../tcpvisr-core" }
etherparse = "=0.20.2"
pcap-parser = "=0.17.0"
thiserror = "=2.0.18"
pcap = { version = "=2.4.0", optional = true }

[features]
default = []
live = ["dep:pcap"]
```

- [ ] **Step 1: Write failing decoder tests** — `crates/tcpvisr-ingest/src/decode.rs` test module
  drives `decode_frame` with hand-built **IP-onward** byte slices (no container), one per case.
  Build a minimal IPv4+TCP SYN by hand (20-byte IPv4 header + 20-byte TCP header) and assert:
```rust
#[test]
fn decodes_ipv4_tcp_syn() {
    let frame = ipv4_tcp_syn(); // helper building 40 bytes, src 10.0.0.1:1234 dst 10.0.0.2:80
    match decode_frame(LinkType::RawIp, Nanos(0), &frame) {
        DecodeOutcome::Decoded(seg) => {
            assert_eq!(seg.flow.src_port, 1234);
            assert_eq!(seg.flow.dst_port, 80);
            assert!(seg.flags.syn());
            assert_eq!(seg.payload_len, 0);
        }
        other => panic!("expected decode, got {other:?}"),
    }
}

#[test]
fn non_tcp_is_skipped_non_tcp() {
    let frame = ipv4_udp(); // protocol 17
    assert_eq!(decode_frame(LinkType::RawIp, Nanos(0), &frame),
               DecodeOutcome::Skipped(SkipReason::NonTcp));
}

#[test]
fn garbage_is_malformed() {
    assert_eq!(decode_frame(LinkType::RawIp, Nanos(0), &[0xff, 0x00, 0x01]),
               DecodeOutcome::Skipped(SkipReason::Malformed));
}
```
  (DecodeOutcome/SkipReason derive `PartialEq` so `assert_eq!` works.)

- [ ] **Step 2: Run, expect fail** — `cargo test -p tcpvisr-ingest decode::` ⇒ FAIL (undefined).

- [ ] **Step 3: Implement `link.rs`** — strip each link layer to IP bytes; dispatch raw/null on
  the IP-version nibble via etherparse (`from_ip` reads the nibble itself). Real code:
```rust
//! Link-layer stripping. Each branch reduces a frame to the IP bytes `decode_frame` parses.

use crate::decode::SkipReason;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkType { Ethernet, LinuxSll, LinuxSll2, RawIp, Null }

impl LinkType {
    #[must_use]
    pub fn from_dlt(dlt: u16) -> Option<Self> {
        match dlt {
            1 => Some(Self::Ethernet),
            0 => Some(Self::Null),
            101 => Some(Self::RawIp),
            113 => Some(Self::LinuxSll),
            276 => Some(Self::LinuxSll2),
            _ => None,
        }
    }
}

pub(crate) enum Stripped<'a> { Ip(&'a [u8]), Skip(SkipReason) }

const ETHERTYPE_IPV4: u16 = 0x0800;
const ETHERTYPE_IPV6: u16 = 0x86DD;

fn be16(b: &[u8], off: usize) -> Option<u16> {
    b.get(off..off + 2).map(|s| u16::from_be_bytes([s[0], s[1]]))
}

pub(crate) fn strip_link(link: LinkType, frame: &[u8]) -> Stripped<'_> {
    match link {
        LinkType::RawIp => Stripped::Ip(frame), // from_ip dispatches on the version nibble
        LinkType::Null => match frame.get(4..) {
            Some(ip) => Stripped::Ip(ip),       // skip 4-byte AF word; nibble decides v4/v6
            None => Stripped::Skip(SkipReason::Malformed),
        },
        LinkType::LinuxSll2 => strip_after_ethertype(frame, 0, 20),
        LinkType::LinuxSll => strip_after_ethertype(frame, 14, 16),
        LinkType::Ethernet => strip_ethernet(frame),
    }
}

// SLL/SLL2: ethertype at `et_off`, IP payload at `ip_off`.
fn strip_after_ethertype(frame: &[u8], et_off: usize, ip_off: usize) -> Stripped<'_> {
    match be16(frame, et_off) {
        Some(ETHERTYPE_IPV4 | ETHERTYPE_IPV6) => match frame.get(ip_off..) {
            Some(ip) => Stripped::Ip(ip),
            None => Stripped::Skip(SkipReason::Malformed),
        },
        Some(_) => Stripped::Skip(SkipReason::NonTcp),
        None => Stripped::Skip(SkipReason::Malformed),
    }
}

fn strip_ethernet(frame: &[u8]) -> Stripped<'_> {
    let mut off = 12; // ethertype position after dst(6)+src(6)
    loop {
        match be16(frame, off) {
            Some(ETHERTYPE_IPV4 | ETHERTYPE_IPV6) => {
                return match frame.get(off + 2..) {
                    Some(ip) => Stripped::Ip(ip),
                    None => Stripped::Skip(SkipReason::Malformed),
                };
            }
            Some(0x8100 | 0x88A8) => off += 4, // 802.1Q/802.1ad tag: 2 TCI + 2 inner ethertype
            Some(_) => return Stripped::Skip(SkipReason::NonTcp),
            None => return Stripped::Skip(SkipReason::Malformed),
        }
    }
}
```

- [ ] **Step 4: Implement `decode.rs`** — one IP+TCP decode via etherparse lax slicing. Real code:
```rust
//! The single shared header decoder used by both faucets (design §3.2, ADR-0003).

use etherparse::{LaxNetSlice, LaxSlicedPacket, TcpOptionElement, TcpSlice, TransportSlice};
use tcpvisr_core::{FlowKey, Nanos, Segment, TcpFlags, TcpOptions, TcpSeq};

use crate::link::{strip_link, LinkType, Stripped};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SkipReason {
    NonTcp, Malformed, UnsupportedLinkType, Ipv6Fragment, UnsupportedExtChain, Truncated,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeOutcome { Decoded(Segment), Skipped(SkipReason) }

#[must_use]
pub fn decode_frame(link: LinkType, ts: Nanos, frame: &[u8]) -> DecodeOutcome {
    let ip = match strip_link(link, frame) {
        Stripped::Ip(bytes) => bytes,
        Stripped::Skip(reason) => return DecodeOutcome::Skipped(reason),
    };
    let sliced = match LaxSlicedPacket::from_ip(ip) {
        Ok(s) => s,
        Err(_) => return DecodeOutcome::Skipped(SkipReason::Malformed),
    };
    let Some(net) = sliced.net.as_ref() else {
        return DecodeOutcome::Skipped(SkipReason::Malformed);
    };
    let (src_ip, dst_ip, fragmenting) = match net {
        LaxNetSlice::Ipv4(v4) => {
            let h = v4.header();
            (h.source_addr().into(), h.destination_addr().into(), v4.is_fragmenting_payload())
        }
        LaxNetSlice::Ipv6(v6) => {
            let h = v6.header();
            (h.source_addr().into(), h.destination_addr().into(), v6.is_fragmenting_payload())
        }
        LaxNetSlice::Arp(_) => return DecodeOutcome::Skipped(SkipReason::NonTcp),
    };
    if fragmenting {
        return DecodeOutcome::Skipped(SkipReason::Ipv6Fragment);
    }
    let Some(TransportSlice::Tcp(tcp)) = sliced.transport.as_ref() else {
        // No transport: distinguish an unsupported IP-ext chain from plain non-TCP.
        return DecodeOutcome::Skipped(match sliced.stop_err {
            Some((_, etherparse::Layer::Ipv6ExtHeader | etherparse::Layer::IpHeader)) => {
                SkipReason::UnsupportedExtChain
            }
            _ => SkipReason::NonTcp,
        });
    };
    DecodeOutcome::Decoded(Segment {
        ts,
        flow: FlowKey {
            src_ip, dst_ip,
            src_port: tcp.source_port(),
            dst_port: tcp.destination_port(),
        },
        seq: TcpSeq(tcp.sequence_number()),
        ack: TcpSeq(tcp.acknowledgment_number()),
        flags: build_flags(tcp),
        window: tcp.window_size(),
        options: parse_options(tcp),
        payload_len: u32::try_from(tcp.payload().len()).unwrap_or(u32::MAX),
    })
}

fn build_flags(tcp: &TcpSlice<'_>) -> TcpFlags {
    let mut bits = 0u16;
    let set = [
        (tcp.fin(), TcpFlags::FIN), (tcp.syn(), TcpFlags::SYN), (tcp.rst(), TcpFlags::RST),
        (tcp.psh(), TcpFlags::PSH), (tcp.ack(), TcpFlags::ACK), (tcp.urg(), TcpFlags::URG),
        (tcp.ece(), TcpFlags::ECE), (tcp.cwr(), TcpFlags::CWR), (tcp.ns(), TcpFlags::NS),
    ];
    for (on, bit) in set {
        if on { bits |= bit; }
    }
    TcpFlags(bits)
}

fn parse_options(tcp: &TcpSlice<'_>) -> TcpOptions {
    let mut opts = TcpOptions::default();
    for element in tcp.options_iterator() {
        let Ok(element) = element else { break }; // malformed options: take what parsed
        match element {
            TcpOptionElement::MaximumSegmentSize(v) => opts.mss = Some(v),
            TcpOptionElement::WindowScale(v) => opts.window_scale = Some(v),
            TcpOptionElement::SelectiveAcknowledgementPermitted => opts.sack_permitted = true,
            TcpOptionElement::SelectiveAcknowledgement(first, rest) => {
                opts.sack_blocks.push((TcpSeq(first.0), TcpSeq(first.1)));
                for block in rest.into_iter().flatten() {
                    opts.sack_blocks.push((TcpSeq(block.0), TcpSeq(block.1)));
                }
            }
            TcpOptionElement::Timestamp(a, b) => opts.timestamp = Some((a, b)),
            _ => {}
        }
    }
    opts
}
```
  Note: `u32::try_from(...).unwrap_or(...)` is allowed (no `unwrap`); confirm
  `etherparse::Layer` variant names against the compiler (adjust the two arms if the enum
  spells them differently — the behavior, not the spelling, is the contract).

- [ ] **Step 5: Wire modules in `lib.rs`** (`pub mod link; pub mod decode;` + re-exports) and
  add the IPv4 byte-builder test helpers (`ipv4_tcp_syn`, `ipv4_udp`). Keep helpers in the
  `#[cfg(test)]` module.

- [ ] **Step 6: Run** — `cargo test -p tcpvisr-ingest decode::` PASS; clippy `--all-features`
  clean (needs `libpcap-dev`); fmt clean.

- [ ] **Step 7: Commit** — `feat(ingest): add shared frame decoder and link-layer stripping`

---

## Task 3: Fixtures, both faucets, parity (`tcpvisr-ingest`)

**Files:**
- Create: `crates/tcpvisr-ingest/tests/support/mod.rs` (builder)
- Create: `crates/tcpvisr-ingest/tests/fixtures/*.pcap[ng]` (committed, generated)
- Create: `crates/tcpvisr-ingest/src/replay.rs`, `src/libpcap.rs`
- Modify: `crates/tcpvisr-ingest/src/lib.rs`
- Create: `crates/tcpvisr-ingest/tests/{parse,drift,parity}.rs`

**Interfaces — Produces:**
- `struct SkipCounts { pub non_tcp, malformed, unsupported_link_type, ipv6_fragment,
  unsupported_ext_chain, truncated: u64 }` — `Debug,Clone,Default,PartialEq,Eq`; method
  `record(&mut self, SkipReason)`.
- `struct ReplayParse { pub items: Vec<Item>, pub skipped: SkipCounts, pub link_type: LinkType }`.
- `#[derive(thiserror::Error)] enum IngestError` — variants `Open { path, source }`,
  `Container { path, detail }`, `UnknownLinkType { dlt: u16 }`, `MixedLinkTypes`.
- `fn parse_file_visit(path: &Path, sink: &mut dyn FnMut(&Item)) -> Result<(LinkType,
  SkipCounts), IngestError>` — the streaming core; calls `sink` once per decoded `Item`,
  holding only the current frame. **The CLI uses this** (constant memory, per spec).
- `fn parse_file(path: &Path) -> Result<ReplayParse, IngestError>` — thin collector over
  `parse_file_visit` (pushes each item into a `Vec`); used by tests and the parity test over
  bounded fixtures.
- `#[cfg(feature="live")] fn parse_file_libpcap(path: &Path) -> Result<ReplayParse, IngestError>`.

**Fixture builder contract (`tests/support/mod.rs`):** pure functions returning `Vec<u8>`:
- `legacy_pcap(linktype_dlt: u16, frames: &[&[u8]]) -> Vec<u8>` — writes the 24-byte global
  header with the **microsecond** magic `0xa1b2c3d4`, `network=dlt`, then per-frame 16-byte
  record headers using **fixed timestamps** (e.g. base `1_700_000_000` + index seconds, usec 0).
- `pcapng(linktype_dlt: u16, frames: &[&[u8]]) -> Vec<u8>` — SHB + one IDB (`if_tsresol`
  default = 6, microsecond) + one EPB per frame, fixed timestamps.
- Link-frame builders that prepend the correct link header to an IP packet:
  `ethernet(ip)`, `sll(ip)`, `sll2(ip)`, `null(ip, is_v6)`, plus IP/TCP builders
  `ipv4_tcp(...)`, `ipv6_tcp(...)`, `ipv6_ext_tcp(...)` (hop-by-hop then TCP), `ipv4_udp(...)`.
- Determinism: no clock/random; all timestamps and ISNs are constants.

- [ ] **Step 1: Write the builder** and a unit assertion that a built legacy pcap round-trips
  through `parse_file` (write to a tempdir path under `std::env::temp_dir()` keyed by test name,
  or `tests/fixtures` regeneration — see drift test). Start failing because `parse_file` is
  undefined.

- [ ] **Step 2: Implement `replay.rs`** — `create_reader(65536, File)` loop. Real shape:
```rust
//! Pure-Rust replay faucet over pcap-parser. Streams records into `decode_frame`.

use std::fs::File;
use std::path::Path;

use pcap_parser::{create_reader, PcapBlockOwned, PcapError};
use pcap_parser::Block;
use tcpvisr_core::{Item, Nanos};

use crate::decode::{decode_frame, DecodeOutcome};
use crate::link::LinkType;
use crate::{IngestError, ReplayParse, SkipCounts};

// Streaming core: `sink` is called once per decoded Item; only the current frame is held.
pub fn parse_file_visit(
    path: &Path,
    sink: &mut dyn FnMut(&Item),
) -> Result<(LinkType, SkipCounts), IngestError> {
    let file = File::open(path).map_err(|source| IngestError::Open {
        path: path.to_path_buf(), source,
    })?;
    let mut reader = create_reader(65536, file)
        .map_err(|e| IngestError::Container { path: path.to_path_buf(), detail: format!("{e:?}") })?;

    let mut state = ParseState::default(); // link_type: Option<LinkType>, baseline: Option<u64>,
                                           // skipped: SkipCounts
    loop {
        match reader.next() {
            Ok((offset, block)) => {
                handle_block(&mut state, &block, sink)?; // sets link type from header/IDB; per
                                                         // packet: abs_ns, Nanos, decode_frame,
                                                         // record skip or call sink(&item)
                reader.consume(offset);
            }
            Err(PcapError::Eof) => break,
            Err(PcapError::Incomplete(_)) => {
                // No-progress guard: a refill that adds no buffered bytes means a
                // truncated/garbage container at EOF — fail fast instead of looping forever on
                // hostile input. (`data()` is the unconsumed buffer; `refill` grows it from disk.)
                let before = reader.data().len();
                reader.refill().map_err(|e| IngestError::Container {
                    path: path.to_path_buf(), detail: format!("{e:?}") })?;
                if reader.data().len() == before {
                    return Err(IngestError::Container {
                        path: path.to_path_buf(), detail: "incomplete/truncated container".into() });
                }
            }
            Err(e) => return Err(IngestError::Container { path: path.to_path_buf(),
                                                          detail: format!("{e:?}") }),
        }
    }
    let link_type = state.link_type.ok_or(IngestError::Container {
        path: path.to_path_buf(), detail: "no packets / interface in capture".into(),
    })?;
    Ok((link_type, state.skipped))
}

// Thin collector for tests / parity over bounded fixtures.
pub fn parse_file(path: &Path) -> Result<ReplayParse, IngestError> {
    let mut items = Vec::new();
    let (link_type, skipped) = parse_file_visit(path, &mut |item| items.push(item.clone()))?;
    Ok(ReplayParse { items, skipped, link_type })
}
```
  `handle_block` rules:
  - `PcapBlockOwned::LegacyHeader(h)` → `LinkType::from_dlt(h.network.0 as u16)` or
    `UnknownLinkType`; record microsecond precision (magic `0xa1b2c3d4`).
  - `PcapBlockOwned::Legacy(b)` → `abs_ns = b.ts_sec as u64 * 1_000_000_000 + b.ts_usec as u64
    * 1000`; truncation = `b.caplen < b.origlen`; frame = `b.data`.
  - `PcapBlockOwned::NG(Block::SectionHeader(_))` → ignore.
  - `Block::InterfaceDescription(idb)` → `LinkType::from_dlt(idb.linktype.0 as u16)`; **if a
    second IDB has a different link type → `IngestError::MixedLinkTypes`**; record
    microsecond resolution (assert `if_tsresol == 6`).
  - `Block::EnhancedPacket(epb)` → `ticks = (epb.ts_high as u64) << 32 | epb.ts_low as u64`;
    `abs_ns = ticks * 1000` (microsecond IDB); truncation = `epb.caplen < epb.origlen`;
    frame = `epb.data`.
  - `Block::SimplePacket(spb)` → no timestamp; use `Nanos(0)` baseline-relative (M1 fixtures
    avoid SPB; if encountered, `abs_ns = baseline`).
  - First packet record (in file order, decoded or skipped) sets `baseline`; per packet
    `ts = Nanos(abs_ns.saturating_sub(baseline))`.
  - For each packet: if truncated → `skipped.record(Truncated)` (skip decode); else match
    `decode_frame(link, ts, frame)` → push `Item::Segment` or `skipped.record(reason)`.

- [ ] **Step 3: `SkipCounts`, `ReplayParse`, `IngestError` in `lib.rs`**:
```rust
use std::path::PathBuf;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkipCounts {
    pub non_tcp: u64, pub malformed: u64, pub unsupported_link_type: u64,
    pub ipv6_fragment: u64, pub unsupported_ext_chain: u64, pub truncated: u64,
}
impl SkipCounts {
    pub fn record(&mut self, reason: crate::decode::SkipReason) {
        use crate::decode::SkipReason::*;
        match reason {
            NonTcp => self.non_tcp += 1,
            Malformed => self.malformed += 1,
            UnsupportedLinkType => self.unsupported_link_type += 1,
            Ipv6Fragment => self.ipv6_fragment += 1,
            UnsupportedExtChain => self.unsupported_ext_chain += 1,
            Truncated => self.truncated += 1,
        }
    }
    #[must_use] pub fn total(&self) -> u64 {
        self.non_tcp + self.malformed + self.unsupported_link_type
            + self.ipv6_fragment + self.unsupported_ext_chain + self.truncated
    }
}

#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    #[error("opening capture {path}: {source} (check the path and read permissions)")]
    Open { path: PathBuf, #[source] source: std::io::Error },
    #[error("parsing capture container {path}: {detail}")]
    Container { path: PathBuf, detail: String },
    #[error("unsupported link type {dlt} (M1 supports Ethernet, SLL, SLL2, raw IP, null)")]
    UnknownLinkType { dlt: u16 },
    #[error("capture mixes link types across interfaces; M1 supports a single link type")]
    MixedLinkTypes,
}
```

- [ ] **Step 4: Implement `libpcap.rs`** (`#[cfg(feature="live")]`):
```rust
//! libpcap file faucet (ADR-0005). File reading only; live interface capture is M11.

use std::path::Path;
use pcap::Capture;
use tcpvisr_core::{Item, Nanos};
use crate::decode::{decode_frame, DecodeOutcome};
use crate::link::LinkType;
use crate::{IngestError, ReplayParse, SkipCounts};

pub fn parse_file_libpcap(path: &Path) -> Result<ReplayParse, IngestError> {
    let mut cap = Capture::from_file(path).map_err(|e| IngestError::Container {
        path: path.to_path_buf(), detail: e.to_string() })?;
    let dlt = cap.get_datalink().0; // i32 DLT
    let link = LinkType::from_dlt(u16::try_from(dlt).unwrap_or(u16::MAX))
        .ok_or(IngestError::UnknownLinkType { dlt: u16::try_from(dlt).unwrap_or(u16::MAX) })?;
    let mut items = Vec::new();
    let mut skipped = SkipCounts::default();
    let mut baseline: Option<u64> = None;
    while let Ok(packet) = cap.next_packet() {
        let h = packet.header;
        let abs_ns = (h.ts.tv_sec as u64) * 1_000_000_000 + (h.ts.tv_usec as u64) * 1000;
        let base = *baseline.get_or_insert(abs_ns);
        let ts = Nanos(abs_ns.saturating_sub(base));
        if h.caplen < h.len { skipped.record(crate::decode::SkipReason::Truncated); continue; }
        match decode_frame(link, ts, packet.data) {
            DecodeOutcome::Decoded(seg) => items.push(Item::Segment(seg)),
            DecodeOutcome::Skipped(reason) => skipped.record(reason),
        }
    }
    Ok(ReplayParse { items, skipped, link_type: link })
}
```
  (Confirm `pcap` field names `ts.tv_sec`/`tv_usec`, `header.caplen`/`len`,
  `get_datalink().0` against the compiler; behavior is the contract.)

- [ ] **Step 5: Generate + commit fixtures + drift test** — `tests/drift.rs` calls the builder
  for each link type and asserts the committed `tests/fixtures/<name>.pcap[ng]` equals the
  builder output; a documented env-gated branch (`UPDATE_FIXTURES=1`) writes them. Generate
  once, commit. Fixtures: `ethernet.pcap sll.pcap sll2.pcap raw_ip.pcap null.pcap
  ipv6_ext.pcap ethernet.pcapng skip.pcap` (skip = one UDP + one truncated record).

- [ ] **Step 6: `tests/parse.rs`** — for each well-formed fixture, `parse_file` yields the
  expected number of `Segment`s with expected ports/flags; `skip.pcap` yields
  `non_tcp == 1 && truncated == 1`. Assert `MixedLinkTypes` on a hand-built 2-IDB pcapng.

- [ ] **Step 7: `tests/parity.rs`** (`#![cfg(feature = "live")]`) — for each **well-formed**
  fixture, `assert_eq!(parse_file(p)?.items, parse_file_libpcap(p)?.items)` and equal
  `skipped`; exclude `skip.pcap` from equality but assert each faucet reports its `truncated`.

- [ ] **Step 8: Run** — `cargo test -p tcpvisr-ingest` (default) PASS;
  `cargo test -p tcpvisr-ingest --features live` PASS; clippy `--all-features` clean; fmt clean.

- [ ] **Step 9: Commit** — `feat(ingest): add replay/libpcap faucets, fixtures, parity test`

---

## Task 4: `parse` CLI + CI + deny (`tcp-visr`, CI)

**Files:**
- Modify: `crates/tcp-visr/Cargo.toml` (add `tcpvisr-ingest`, `tcpvisr-core`)
- Modify: `crates/tcp-visr/src/main.rs`
- Modify: `crates/tcp-visr/tests/cli.rs`
- Modify: `.github/workflows/ci.yml`, `deny.toml`

**Interfaces — Consumes:** `tcpvisr_ingest::{parse_file, ReplayParse}`, core `Item`.

- [ ] **Step 1: Update CLI integration test** — `crates/tcp-visr/tests/cli.rs`:
```rust
#[test]
fn parse_prints_segments_and_skip_summary() {
    let fixture = concat!(env!("CARGO_MANIFEST_DIR"),
        "/../tcpvisr-ingest/tests/fixtures/ethernet.pcap");
    let output = bin().args(["parse", fixture]).output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("->"));        // a decoded segment line
    assert!(stdout.contains("skipped"));   // the summary line
}

#[test]
fn parse_missing_file_exits_nonzero_with_message() {
    let output = bin().args(["parse", "/no/such.pcap"]).output().unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("opening capture"));
}
```

- [ ] **Step 2: Run, expect fail** (Parse is still a unit variant).

- [ ] **Step 3: Implement `parse` in `main.rs`** — change `Parse` to `Parse { file: PathBuf }`,
  add a `run` that decodes and writes via `writeln!`:
```rust
Command::Parse { file } => run_parse(&file),
// ...
fn run_parse(file: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut out = std::io::stdout().lock();
    let mut count: u64 = 0;
    let mut sink_err: Option<std::io::Error> = None;
    // Stream: hold only the current item. Capture a write error to surface after the walk.
    let (_link, skipped) = tcpvisr_ingest::parse_file_visit(file, &mut |item| {
        if sink_err.is_some() { return; }
        if let tcpvisr_core::Item::Segment(s) = item {
            count += 1;
            if let Err(e) = writeln!(out, "{} {} {} seq={} ack={} win={} len={}",
                s.ts, s.flow, s.flags, s.seq.0, s.ack.0, s.window, s.payload_len) {
                sink_err = Some(e);
            }
        }
    })?;
    if let Some(e) = sink_err { return Err(e.into()); }
    writeln!(out, "{count} segments, skipped: {} total", skipped.total())?;
    Ok(())
}
```
  (`use std::io::Write;` for `writeln!`.)

- [ ] **Step 4: Run** — `cargo test -p tcp-visr` PASS.

- [ ] **Step 5: CI + deny** — in `.github/workflows/ci.yml` `test` job, add before clippy:
  `- run: sudo apt-get update && sudo apt-get install -y libpcap-dev`, and after
  `cargo test --workspace` add `- run: cargo test -p tcpvisr-ingest --features live`.
  Run `cargo deny check` locally; add any new SPDX IDs to `deny.toml` `licenses.allow`.

- [ ] **Step 6: Full guardrails** — `cargo fmt --all --check`, `cargo clippy --all-targets
  --all-features -- -D warnings`, `cargo test --workspace`,
  `cargo test -p tcpvisr-ingest --features live`, `cargo deny check`, and a manual
  `cargo run -p tcp-visr -- parse crates/tcpvisr-ingest/tests/fixtures/ethernet.pcap`.

- [ ] **Step 7: Commit** — `feat(cli): wire parse to the replay faucet; gate parity in CI`

---

## Self-review notes

- **Spec coverage:** core model (Task 1); link types incl. SLL2 + IPv6 ext + skip-and-count
  (Task 2); both faucets + fixtures + parity + drift + single-interface error + precision
  contract (Task 3); `parse` CLI + CI `live` job + deny (Task 4). DoD #1–#8 map to Task 4 step 6.
- **Timestamp precision:** fixtures are microsecond (magic `0xa1b2c3d4`, IDB `if_tsresol=6`);
  both faucets compute `abs_ns = … * 1000`, so parity holds (spec precision contract).
- **Truncation:** detected uniformly as `caplen < origlen` before decode in both faucets;
  `skip.pcap` excluded from byte-equality parity.
- **Lint risk:** `clippy::pedantic` is warn — watch `cast_possible_truncation` (use
  `u16::try_from`/`u32::try_from`), `must_use`, and `missing_panics_doc` (no panics).
- **Library spellings to confirm at the compiler** (behavior is the contract, not the name):
  `etherparse::Layer` ext-header variant names; `pcap` `PacketHeader` field names and
  `get_datalink().0`; `pcap_parser` `Linktype` newtype field `.0` and `PcapError` variants.
```
