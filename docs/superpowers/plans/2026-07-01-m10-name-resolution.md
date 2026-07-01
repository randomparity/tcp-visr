# M10 — Name resolution (capture-DNS host labels) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Show each connection's peer as a DNS host name (`github.com:443`) resolved from the A/AAAA answers in the replayed capture, with the pure engine and the M3 oracle untouched.

**Architecture:** DNS responses (UDP src port 53) are parsed in the one shared `decode_frame` into `NameObservation`s that ride *beside* the `Item` stream (not through the pure engine). A latest-wins, bounded `NameTable` in `tcpvisr-core` answers `resolve(ip) -> Option<&HostName>`; the TUI resolves each peer once at `App::new` and renders `host:port` over `ip:port`. The live reverse-DNS half is deferred to M11/M12 behind the same `NameTable` — no resolver code is added now.

**Tech Stack:** Rust 1.88 workspace; `simple-dns =0.11.3` (DNS message parse); `etherparse` (already present, slices UDP); ratatui TUI; `proptest` (sanitization property test).

**Spec:** [m10-name-resolution.md](../specs/m10-name-resolution.md) · **ADR:** [ADR-0015](../../adr/0015-name-resolution.md) · **Issue:** #13 · one PR on `feat/name-resolution-13`.

## Global Constraints

- **Toolchain pinned to Rust 1.88.0**; edition 2024; exact-pin every dependency (`=x.y.z`).
- **The pure engine (`tcpvisr-engine`) is not touched.** No `MetricSample`, `metrics` JSON, or engine change. The M3 oracle goldens (`crates/tcp-visr/tests/oracle/*.json`) and the render-closed snapshots stay byte-identical (spec criterion 15).
- **The `Item` stream stays TCP-only.** Names ride beside it; the cross-faucet `Item` parity is unchanged and additionally extended to `names`.
- **Clippy is `-D warnings` workspace-wide** with `unwrap_used`/`panic`/`print_stdout`/`expect_used`(warn)/`allow_attributes`(deny) etc. Tests are exempt for most, but in non-`#[test]` support modules scope relaxations with a **file-level** `#![allow(...)]` (item-level `#[allow]` is denied by `allow_attributes`).
- **`simple-dns` is a default dependency of `tcpvisr-ingest`** (not gated on `live`); the replay path stays libpcap-free (ADR-0003). MIT license is already on the `deny.toml` allow-list — do not edit `deny.toml`.
- **`NAME_TABLE_CAP = 65_536`** distinct IPs; over the cap → drop-and-count, surfaced as `names capped` in the title. Never silent (design §7).
- **`HostName` keeps only bytes `0x20`–`0x7e`**, drops all others, rejects >253 bytes, `None` on empty/fully-dropped.

## Guardrail commands (run before every commit)

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace
cargo test -p tcpvisr-ingest --features live   # libpcap parity — now also compares names
cargo deny check                                # simple-dns is MIT (already allowed)
```

Focused runs while iterating:
```bash
cargo test -p tcpvisr-core name::               # HostName + NameTable
cargo test -p tcpvisr-ingest dns::              # DNS answer parsing
cargo test -p tcpvisr-ingest decode::           # decode_frame Names outcome
cargo test -p tcpvisr-ingest --test parse       # faucet + ReplayParse.names
cargo test -p tcpvisr-tui app::                 # host label rows + filter
cargo test -p tcpvisr-tui render::              # peer/title render (TestBackend)
cargo test -p tcp-visr --test '*'               # bin + oracle (must stay green)
```

---

## Task 1: Core `HostName` newtype (sanitize + bound)

**Files:**
- Create: `crates/tcpvisr-core/src/name.rs`
- Modify: `crates/tcpvisr-core/src/lib.rs` (add `pub mod name;` + re-exports)
- Modify: `crates/tcpvisr-core/Cargo.toml` (`proptest` is already a dev-dependency — confirm, do not duplicate)

**Interfaces:**
- Produces: `HostName` with `pub fn new(raw: &str) -> Option<HostName>`, `impl Display`, `impl AsRef<str>`, `#[derive(Debug, Clone, PartialEq, Eq)]`. Sanitize rule: strip a single trailing `.`, then keep only bytes in `0x20..=0x7e`, dropping every other byte; reject (`None`) if the *input* (pre-drop, after trailing-dot strip) exceeds 253 bytes; `None` if the result is empty.

- [ ] **Step 1: Write the failing tests** in `crates/tcpvisr-core/src/name.rs`:

```rust
//! DNS-derived host names and the capture name table (design §6, §10.M10; ADR-0015).

use core::fmt;

/// A sanitized, bounded host name resolved from capture DNS. Rendered into a terminal, so it
/// is printable-ASCII only (ADR-0015 §2): a DNS name is attacker-controlled input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostName(String);

/// Longest legal DNS name (RFC 1035); a longer candidate is rejected rather than truncated.
const MAX_HOST_LEN: usize = 253;

impl HostName {
    /// Builds a host name from a raw DNS owner name, or `None` if it is empty, longer than
    /// [`MAX_HOST_LEN`], or sanitizes to empty. Strips one trailing root dot, then keeps only
    /// printable ASCII (`0x20..=0x7e`), dropping controls/ESC/DEL and every byte `>= 0x80`.
    #[must_use]
    pub fn new(raw: &str) -> Option<Self> {
        let trimmed = raw.strip_suffix('.').unwrap_or(raw);
        if trimmed.is_empty() || trimmed.len() > MAX_HOST_LEN {
            return None;
        }
        let clean: String = trimmed
            .bytes()
            .filter(|b| (0x20..=0x7e).contains(b))
            .map(char::from)
            .collect();
        if clean.is_empty() {
            None
        } else {
            Some(Self(clean))
        }
    }
}

impl fmt::Display for HostName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for HostName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_trailing_root_dot() {
        assert_eq!(HostName::new("a.com.").unwrap().as_ref(), "a.com");
        assert_eq!(HostName::new("a.com").unwrap().as_ref(), "a.com");
    }

    #[test]
    fn drops_control_and_escape_bytes() {
        // "e\x1b[31mvil.com" -> escape and bracket-code bytes removed. '[' (0x5b), '3','1','m'
        // are printable and remain; the ESC (0x1b) is dropped.
        let h = HostName::new("e\u{1b}[31mvil.com").unwrap();
        assert!(!h.as_ref().contains('\u{1b}'), "no ESC survives");
        assert!(h.as_ref().bytes().all(|b| (0x20..=0x7e).contains(&b)));
    }

    #[test]
    fn drops_non_ascii() {
        // "café" -> the 'é' (multi-byte, >= 0x80) is dropped, "caf" remains.
        assert_eq!(HostName::new("café").unwrap().as_ref(), "caf");
    }

    #[test]
    fn rejects_empty_and_oversize_and_fully_dropped() {
        assert_eq!(HostName::new(""), None);
        assert_eq!(HostName::new("."), None); // trailing dot stripped -> empty
        assert_eq!(HostName::new(&"a".repeat(254)), None); // > 253
        assert_eq!(HostName::new("\u{1b}\u{7f}"), None); // all dropped -> empty
        assert!(HostName::new(&"a".repeat(253)).is_some()); // exactly 253 ok
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p tcpvisr-core name::` → FAIL (`name` module not declared in `lib.rs`).

- [ ] **Step 3: Wire the module** in `crates/tcpvisr-core/src/lib.rs`. Add `pub mod name;` in the module list and `pub use name::HostName;` in the re-exports (place both next to the existing `metric`/`MetricSample` lines). Leave `NameObservation`/`NameTable` re-exports for Task 2.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p tcpvisr-core name::` → PASS (5 tests).

- [ ] **Step 5: Add the sanitization property test** to the `tests` module in `name.rs`:

```rust
    use proptest::prelude::*;

    proptest! {
        /// Any input yields either `None` or a name that is printable-ASCII and <= 253 bytes.
        #[test]
        fn sanitized_output_is_always_printable_and_bounded(raw in ".*") {
            if let Some(h) = HostName::new(&raw) {
                let s = h.as_ref();
                prop_assert!(!s.is_empty());
                prop_assert!(s.len() <= 253);
                prop_assert!(s.bytes().all(|b| (0x20..=0x7e).contains(&b)));
            }
        }
    }
```

- [ ] **Step 6: Run the property test + guardrails**

Run: `cargo test -p tcpvisr-core name::` → PASS. Then `cargo fmt --all --check` and
`cargo clippy --all-targets --all-features -- -D warnings` → clean.

- [ ] **Step 7: Commit**

```bash
git add crates/tcpvisr-core/src/name.rs crates/tcpvisr-core/src/lib.rs
git commit -m "feat(core): add sanitized HostName newtype for DNS labels"
```

---

## Task 2: Core `NameObservation` + bounded latest-wins `NameTable`

**Files:**
- Modify: `crates/tcpvisr-core/src/name.rs`
- Modify: `crates/tcpvisr-core/src/lib.rs` (re-export `NameObservation`, `NameTable`)

**Interfaces:**
- Consumes: `HostName` (Task 1), `Nanos` (`crate::time`).
- Produces:
  - `#[derive(Debug, Clone, PartialEq, Eq)] pub struct NameObservation { pub ts: Nanos, pub ip: IpAddr, pub name: HostName }`
  - `#[derive(Debug, Clone, Default)] pub struct NameTable { … }` with
    `pub fn observe(&mut self, obs: NameObservation)`,
    `pub fn resolve(&self, ip: IpAddr) -> Option<&HostName>`,
    `pub fn len(&self) -> usize`, `pub fn is_empty(&self) -> bool`, `pub fn dropped(&self) -> u64`.
  - `pub const NAME_TABLE_CAP: usize = 65_536;`

- [ ] **Step 1: Write the failing tests** — append to `crates/tcpvisr-core/src/name.rs` (above the `#[cfg(test)]` block add the types; add tests inside `mod tests`). Types:

```rust
use core::net::IpAddr;
use std::collections::HashMap;

use crate::time::Nanos;

/// One IP→name mapping observed in a DNS answer (design §6; ADR-0015 §2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameObservation {
    pub ts: Nanos,
    pub ip: IpAddr,
    pub name: HostName,
}

/// Maximum distinct IPs retained; a DNS-heavy capture (resolver traffic) is otherwise unbounded,
/// so past the cap new IPs are dropped-and-counted (design §7 no-OOM). ADR-0015 §2.
pub const NAME_TABLE_CAP: usize = 65_536;

/// Latest-wins IP→name map built from capture DNS. Static per capture (cursor-independent) and
/// bounded. Pure: no I/O, no clock. The single resolution seam the live path will also feed.
#[derive(Debug, Clone, Default)]
pub struct NameTable {
    by_ip: HashMap<IpAddr, (Nanos, HostName)>,
    dropped: u64,
}

impl NameTable {
    /// Records an observation, keeping the greatest-`ts` name per IP (ties → last seen). A new IP
    /// once [`NAME_TABLE_CAP`] distinct IPs are held is refused and counted (`dropped`); an
    /// already-present IP still updates so latest-wins keeps working for retained IPs.
    pub fn observe(&mut self, obs: NameObservation) {
        match self.by_ip.get_mut(&obs.ip) {
            Some(slot) => {
                if obs.ts >= slot.0 {
                    *slot = (obs.ts, obs.name);
                }
            }
            None => {
                if self.by_ip.len() >= NAME_TABLE_CAP {
                    self.dropped += 1;
                } else {
                    self.by_ip.insert(obs.ip, (obs.ts, obs.name));
                }
            }
        }
    }

    /// The resolved name for `ip`, or `None`.
    #[must_use]
    pub fn resolve(&self, ip: IpAddr) -> Option<&HostName> {
        self.by_ip.get(&ip).map(|(_, name)| name)
    }

    /// Distinct IPs resolved.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_ip.len()
    }

    /// Whether no name has been resolved.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_ip.is_empty()
    }

    /// New IPs refused after the cap (observability; surfaced as `names capped`).
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped
    }
}
```

Tests (inside `mod tests`; add `use core::net::{IpAddr, Ipv4Addr};`):

```rust
    fn obs(ts: u64, ip: u8, name: &str) -> NameObservation {
        NameObservation {
            ts: Nanos(ts),
            ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, ip)),
            name: HostName::new(name).unwrap(),
        }
    }

    #[test]
    fn latest_ts_wins_regardless_of_insertion_order() {
        let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        let mut t = NameTable::default();
        t.observe(obs(1, 1, "a.com"));
        t.observe(obs(2, 1, "b.com"));
        assert_eq!(t.resolve(ip).unwrap().as_ref(), "b.com");

        let mut t2 = NameTable::default();
        t2.observe(obs(2, 1, "b.com"));
        t2.observe(obs(1, 1, "a.com")); // earlier ts must NOT overwrite
        assert_eq!(t2.resolve(ip).unwrap().as_ref(), "b.com");
    }

    #[test]
    fn tie_ts_resolves_to_last_seen() {
        let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        let mut t = NameTable::default();
        t.observe(obs(5, 1, "a.com"));
        t.observe(obs(5, 1, "b.com")); // equal ts, later observed wins
        assert_eq!(t.resolve(ip).unwrap().as_ref(), "b.com");
    }

    #[test]
    fn unknown_ip_resolves_none_and_len_counts_distinct() {
        let mut t = NameTable::default();
        t.observe(obs(1, 1, "a.com"));
        t.observe(obs(1, 2, "c.com"));
        assert_eq!(t.len(), 2);
        assert!(t.resolve(IpAddr::V4(Ipv4Addr::new(9, 9, 9, 9))).is_none());
    }

    #[test]
    fn cap_drops_and_counts_new_ips_but_updates_present_ones() {
        let mut t = NameTable::default();
        for i in 0..NAME_TABLE_CAP {
            let octets = (u32::try_from(i).unwrap()).to_be_bytes();
            t.observe(NameObservation {
                ts: Nanos(1),
                ip: IpAddr::V4(Ipv4Addr::new(octets[0], octets[1], octets[2], octets[3])),
                name: HostName::new("x.com").unwrap(),
            });
        }
        assert_eq!(t.len(), NAME_TABLE_CAP);
        assert_eq!(t.dropped(), 0);
        // A brand-new IP is refused and counted.
        let new_ip = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 1));
        t.observe(NameObservation { ts: Nanos(9), ip: new_ip, name: HostName::new("late.com").unwrap() });
        assert_eq!(t.len(), NAME_TABLE_CAP);
        assert_eq!(t.dropped(), 1);
        assert!(t.resolve(new_ip).is_none());
        // An already-present IP still updates after the cap.
        let present = IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0));
        t.observe(NameObservation { ts: Nanos(9), ip: present, name: HostName::new("updated.com").unwrap() });
        assert_eq!(t.resolve(present).unwrap().as_ref(), "updated.com");
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p tcpvisr-core name::` → FAIL (types not yet added / not compiling before you paste them). After pasting the types it should compile and pass; if a test is wrong, fix the test, not the type.

- [ ] **Step 3: Re-export** in `crates/tcpvisr-core/src/lib.rs`: `pub use name::{HostName, NameObservation, NameTable};` (replace the Task-1 `HostName`-only re-export). Do **not** re-export `NAME_TABLE_CAP` from the crate root — callers use `tcpvisr_core::name::NAME_TABLE_CAP` (it is only referenced in tests and ingest).

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p tcpvisr-core name::` → PASS. Guardrails: `cargo clippy --all-targets --all-features -- -D warnings` clean (note: `len` + `is_empty` present avoids the `len_without_is_empty` pedantic lint).

- [ ] **Step 5: Commit**

```bash
git add crates/tcpvisr-core/src/name.rs crates/tcpvisr-core/src/lib.rs
git commit -m "feat(core): add bounded latest-wins NameTable and NameObservation"
```

---

## Task 3: DNS answer parsing in ingest (`dns.rs`)

**Files:**
- Create: `crates/tcpvisr-ingest/src/dns.rs`
- Modify: `crates/tcpvisr-ingest/src/lib.rs` (add `pub mod dns;`)
- Modify: `crates/tcpvisr-ingest/Cargo.toml` (add `simple-dns = "=0.11.3"`)

**Interfaces:**
- Consumes: `tcpvisr_core::{HostName, NameObservation, Nanos}`, `simple_dns::{Packet, rdata::RData}`.
- Produces: `pub fn parse_dns_answers(ts: Nanos, payload: &[u8]) -> Vec<NameObservation>` — empty on parse error or no A/AAAA answers; never panics.

- [ ] **Step 1: Add the dependency** to `crates/tcpvisr-ingest/Cargo.toml` under `[dependencies]`:

```toml
simple-dns = "=0.11.3"
```

Run `cargo build -p tcpvisr-ingest` to fetch it, then `cargo deny check` → clean (MIT already allowed).

- [ ] **Step 2: Write the failing tests** in `crates/tcpvisr-ingest/src/dns.rs`:

```rust
//! DNS answer extraction from capture packets (design §6, §10.M10; ADR-0015 §1, §4).
//! Parses UDP/53 responses into IP→name observations. Hostile input: bounded, panic-free.

use simple_dns::rdata::RData;
use simple_dns::Packet;
use tcpvisr_core::{HostName, NameObservation, Nanos};

/// Extracts one [`NameObservation`] per A/AAAA answer in a DNS message. Returns empty on a parse
/// error, a query (no answers), or answers whose names fail sanitization. Never panics.
#[must_use]
pub fn parse_dns_answers(ts: Nanos, payload: &[u8]) -> Vec<NameObservation> {
    let Ok(packet) = Packet::parse(payload) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for answer in &packet.answers {
        let ip = match &answer.rdata {
            RData::A(a) => core::net::Ipv4Addr::from(a.address).into(),
            RData::AAAA(a) => core::net::Ipv6Addr::from(a.address).into(),
            _ => continue,
        };
        if let Some(name) = HostName::new(&answer.name.to_string()) {
            out.push(NameObservation { ts, ip, name });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use core::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    use simple_dns::rdata::{A, AAAA};
    use simple_dns::{Name, Packet, ResourceRecord, CLASS};

    fn response_with(records: Vec<ResourceRecord<'static>>) -> Vec<u8> {
        let mut p = Packet::new_reply(1);
        for r in records {
            p.answers.push(r);
        }
        p.build_bytes_vec().unwrap()
    }

    #[test]
    fn extracts_a_and_aaaa_answers() {
        let bytes = response_with(vec![
            ResourceRecord::new(
                Name::new("example.com").unwrap(),
                CLASS::IN,
                300,
                RData::A(A { address: u32::from(Ipv4Addr::new(93, 184, 216, 34)) }),
            ),
            ResourceRecord::new(
                Name::new("v6.example.com").unwrap(),
                CLASS::IN,
                300,
                RData::AAAA(AAAA { address: u128::from(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1)) }),
            ),
        ]);
        let obs = parse_dns_answers(Nanos(7), &bytes);
        assert_eq!(obs.len(), 2);
        assert_eq!(obs[0].ip, IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34)));
        assert_eq!(obs[0].name.as_ref(), "example.com");
        assert_eq!(obs[0].ts, Nanos(7));
        assert_eq!(obs[1].ip, IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1)));
    }

    #[test]
    fn query_yields_no_observations() {
        // A reply with no answers (models a query's empty answer section).
        let bytes = response_with(vec![]);
        assert!(parse_dns_answers(Nanos(0), &bytes).is_empty());
    }

    #[test]
    fn garbage_yields_no_observations_without_panicking() {
        assert!(parse_dns_answers(Nanos(0), &[0xff, 0x00, 0x13, 0x37]).is_empty());
        assert!(parse_dns_answers(Nanos(0), &[]).is_empty());
    }
}
```

- [ ] **Step 3: Wire the module** — add `pub mod dns;` to `crates/tcpvisr-ingest/src/lib.rs` and `pub use dns::parse_dns_answers;`.

- [ ] **Step 4: Run to verify** — since the implementation is in the same file as the tests, run `cargo test -p tcpvisr-ingest dns::` → PASS (3 tests). If `simple_dns` API names differ (e.g. `A.address` type), adjust the test builders to the crate's actual API; the `parse_dns_answers` body uses only `Packet::parse`, `packet.answers`, `answer.rdata`, `answer.name`, `RData::A/AAAA` with `.address`, which are stable in `0.11`.

- [ ] **Step 5: Guardrails + commit**

Run `cargo clippy -p tcpvisr-ingest --all-targets --all-features -- -D warnings` → clean.

```bash
git add crates/tcpvisr-ingest/src/dns.rs crates/tcpvisr-ingest/src/lib.rs crates/tcpvisr-ingest/Cargo.toml Cargo.lock
git commit -m "feat(ingest): parse A/AAAA answers from DNS responses via simple-dns"
```

---

## Task 4: `decode_frame` emits `Names` for UDP/53 responses

**Files:**
- Modify: `crates/tcpvisr-ingest/src/decode.rs`

**Interfaces:**
- Consumes: `parse_dns_answers` (Task 3), `NameObservation` (core).
- Produces: `DecodeOutcome::Names(Vec<NameObservation>)` variant; `decode_frame` returns it for a UDP packet with `source_port() == 53` that yields ≥1 answer, else the existing `Skipped(NonTcp)`.

- [ ] **Step 1: Write the failing test** — add to the `tests` module in `decode.rs`. First a UDP/53 DNS response builder and the assertion:

```rust
    fn ipv4_udp_dns_response() -> Vec<u8> {
        use simple_dns::rdata::{RData, A};
        use simple_dns::{Name, Packet, ResourceRecord, CLASS};
        let mut p = Packet::new_reply(1);
        p.answers.push(ResourceRecord::new(
            Name::new("example.com").unwrap(),
            CLASS::IN,
            300,
            RData::A(A { address: u32::from(core::net::Ipv4Addr::new(93, 184, 216, 34)) }),
        ));
        let dns = p.build_bytes_vec().unwrap();
        let mut buf = Vec::new();
        // Server (:53) -> client (:40000): source port 53 marks a response.
        etherparse::PacketBuilder::ipv4([93, 184, 216, 34], [10, 0, 0, 2], 64)
            .udp(53, 40000)
            .write(&mut buf, &dns)
            .unwrap();
        buf
    }

    #[test]
    fn decodes_dns_response_to_names() {
        let frame = ipv4_udp_dns_response();
        match decode_full(LinkType::RawIp, &frame) {
            DecodeOutcome::Names(obs) => {
                assert_eq!(obs.len(), 1);
                assert_eq!(
                    obs[0].ip,
                    core::net::IpAddr::V4(core::net::Ipv4Addr::new(93, 184, 216, 34))
                );
                assert_eq!(obs[0].name.as_ref(), "example.com");
            }
            other => panic!("expected Names, got {other:?}"),
        }
    }

    #[test]
    fn dns_query_and_non_dns_udp_are_non_tcp() {
        // Non-DNS UDP (existing behavior) and a UDP/53 packet with no answers both skip NonTcp.
        assert_eq!(
            decode_full(LinkType::RawIp, &ipv4_udp()),
            DecodeOutcome::Skipped(SkipReason::NonTcp)
        );
    }
```

(`ipv4_udp()` already exists in the module and builds a `1234 -> 80` UDP packet.)

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p tcpvisr-ingest decode::decodes_dns_response_to_names` → FAIL (`Names` variant does not exist / decode returns `Skipped(NonTcp)`).

- [ ] **Step 3: Add the variant and the UDP branch.** In `decode.rs`:

  a. Extend the enum:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeOutcome {
    Decoded(Segment),
    Names(Vec<tcpvisr_core::NameObservation>),
    Skipped(SkipReason),
}
```

  b. In `decode_frame`, replace the transport match so UDP/53 is handled before the `classify_no_tcp` fallthrough. The current code is:

```rust
    let Some(TransportSlice::Tcp(tcp)) = sliced.transport.as_ref() else {
        return DecodeOutcome::Skipped(classify_no_tcp(&sliced, truncated));
    };
```

  Replace with:

```rust
    let tcp = match sliced.transport.as_ref() {
        Some(TransportSlice::Tcp(tcp)) => tcp,
        Some(TransportSlice::Udp(udp)) if udp.source_port() == 53 => {
            let obs = crate::dns::parse_dns_answers(ts, udp.payload());
            return if obs.is_empty() {
                DecodeOutcome::Skipped(SkipReason::NonTcp)
            } else {
                DecodeOutcome::Names(obs)
            };
        }
        _ => return DecodeOutcome::Skipped(classify_no_tcp(&sliced, truncated)),
    };
```

  Add `Udp` to the `etherparse::{…, TransportSlice}` import list if not already glob-imported (the file imports `TransportSlice` already; `TransportSlice::Udp` needs no extra import).

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p tcpvisr-ingest decode::` → PASS (all existing decode tests + the 2 new ones). The existing `non_tcp_is_skipped_non_tcp` test (1234→80 UDP) still passes because its source port is not 53.

- [ ] **Step 5: Guardrails + commit**

`cargo clippy -p tcpvisr-ingest --all-targets --all-features -- -D warnings` → clean.

```bash
git add crates/tcpvisr-ingest/src/decode.rs
git commit -m "feat(ingest): decode UDP/53 DNS responses to a Names outcome"
```

---

## Task 5: Route `Names` through the faucets; `ReplayParse.names`; `parse_file_visit_named`

**Files:**
- Modify: `crates/tcpvisr-ingest/src/lib.rs` (`ReplayParse.names`)
- Modify: `crates/tcpvisr-ingest/src/replay.rs` (`parse_file_visit_named`, routing)
- Modify: `crates/tcpvisr-ingest/src/libpcap.rs` (routing into `ReplayParse.names`)

**Interfaces:**
- Consumes: `DecodeOutcome::Names` (Task 4), `NameObservation` (core).
- Produces:
  - `ReplayParse { items, skipped, link_type, names: Vec<NameObservation> }`.
  - `pub fn parse_file_visit_named(path: &Path, item_sink: &mut dyn FnMut(&Item), name_sink: &mut dyn FnMut(&NameObservation)) -> Result<(LinkType, SkipCounts), IngestError>`.
  - `parse_file_visit` retained, delegating with a no-op name sink.

- [ ] **Step 1: Write the failing test** — add to `crates/tcpvisr-ingest/tests/parse.rs` (or a new `dns_faucet.rs` integration test). Build a capture with one TCP SYN and one DNS response, then assert routing. Use the `support` module's pcap builders (mirror how `parse.rs`/`parity.rs` already build fixtures). Add a helper in `crates/tcpvisr-ingest/tests/support/mod.rs` if a UDP/DNS frame builder is missing — reuse the `ipv4_udp_dns_response`-style bytes from Task 4 (a UDP/53 response). Test body:

```rust
#[test]
fn faucet_routes_names_and_excludes_them_from_non_tcp() {
    // One TCP SYN + one DNS response (A example.com -> 93.184.216.34).
    let bytes = legacy_pcap(
        DLT_RAW,
        &[
            Pkt::new(TS, ipv4_tcp_syn(1234, 80)),
            Pkt::new(TS + 1000, ipv4_udp_dns_response()),
        ],
    );
    let path = write_temp("dns_route.pcap", &bytes);
    let parsed = parse_file(&path).expect("parse");
    assert_eq!(parsed.items.len(), 1, "only the SYN is an Item");
    assert_eq!(parsed.names.len(), 1, "the DNS answer is a name");
    assert_eq!(parsed.names[0].name.as_ref(), "example.com");
    assert_eq!(parsed.skipped.non_tcp, 0, "the DNS packet is used, not skipped");

    // A UDP/53 query (no answers) is counted non_tcp, yields no name.
    let q = legacy_pcap(DLT_RAW, &[Pkt::new(TS, ipv4_udp_dns_query())]);
    let qpath = write_temp("dns_query.pcap", &q);
    let qp = parse_file(&qpath).expect("parse");
    assert_eq!(qp.names.len(), 0);
    assert_eq!(qp.skipped.non_tcp, 1);
}
```

Add `ipv4_udp_dns_response()` and `ipv4_udp_dns_query()` builders to `tests/support/mod.rs` (the query is a `Packet::new_query`-based UDP/53 packet, or simply a UDP/53 packet with an empty DNS answer section, built the same way as the response but with `Packet::new_reply` and no answers pushed).

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p tcpvisr-ingest --test parse faucet_routes_names` → FAIL (`ReplayParse` has no `names`; builders missing).

- [ ] **Step 3: Add `names` to `ReplayParse`** in `crates/tcpvisr-ingest/src/lib.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayParse {
    pub items: Vec<Item>,
    pub skipped: SkipCounts,
    pub link_type: LinkType,
    pub names: Vec<tcpvisr_core::NameObservation>,
}
```

Add `pub use replay::{parse_file, parse_file_visit, parse_file_visit_named};`.

- [ ] **Step 4: Add `parse_file_visit_named` and routing** in `crates/tcpvisr-ingest/src/replay.rs`:

  a. Rename the existing `parse_file_visit` body to `parse_file_visit_named`, adding a `name_sink: &mut dyn FnMut(&NameObservation)` parameter, threaded into `process_packet`.
  b. Reintroduce `parse_file_visit` as a wrapper:

```rust
pub fn parse_file_visit(
    path: &Path,
    sink: &mut dyn FnMut(&Item),
) -> Result<(LinkType, SkipCounts), IngestError> {
    parse_file_visit_named(path, sink, &mut |_| {})
}
```

  c. In `process_packet`, add the `name_sink` parameter and extend the match:

```rust
    match decode_frame(link, ts, data, origlen) {
        DecodeOutcome::Decoded(seg) => sink(&Item::Segment(seg)),
        DecodeOutcome::Names(obs) => obs.iter().for_each(&mut *name_sink),
        DecodeOutcome::Skipped(reason) => state.skipped.record(reason),
    }
```

  d. `parse_file` collects both:

```rust
pub fn parse_file(path: &Path) -> Result<ReplayParse, IngestError> {
    let mut items = Vec::new();
    let mut names = Vec::new();
    let (link_type, skipped) = parse_file_visit_named(
        path,
        &mut |item| items.push(item.clone()),
        &mut |obs| names.push(obs.clone()),
    )?;
    Ok(ReplayParse { items, skipped, link_type, names })
}
```

  Update the `DecodeOutcome`/`NameObservation` imports at the top of `replay.rs`.

- [ ] **Step 5: Route names in the libpcap faucet** — in `crates/tcpvisr-ingest/src/libpcap.rs`, add `let mut names = Vec::new();`, extend the decode match with `DecodeOutcome::Names(obs) => names.extend(obs),`, and set `names` in the returned `ReplayParse`.

- [ ] **Step 6: Run to verify it passes**

Run: `cargo test -p tcpvisr-ingest --test parse faucet_routes_names` → PASS. Then full `cargo test -p tcpvisr-ingest` → PASS (existing tests still green; any test that constructs `ReplayParse` literally now needs the `names` field — grep for `ReplayParse {` and add `names: vec![]` where constructed in tests).

- [ ] **Step 7: Guardrails + commit**

```bash
git add crates/tcpvisr-ingest/src/lib.rs crates/tcpvisr-ingest/src/replay.rs crates/tcpvisr-ingest/src/libpcap.rs crates/tcpvisr-ingest/tests/
git commit -m "feat(ingest): route DNS names through both faucets into ReplayParse"
```

---

## Task 6: Parity test covers names; committed DNS fixture + drift guard

**Files:**
- Modify: `crates/tcpvisr-ingest/tests/parity.rs`
- Modify: `crates/tcpvisr-ingest/tests/support/mod.rs` (if the DNS builders live only in `parse.rs`, move them to `support` so parity can use them)
- (Optional) a committed `.pcap` DNS fixture if the drift pattern requires committed bytes — follow the existing `drift.rs` convention.

**Interfaces:**
- Consumes: the DNS-response frame builder (Task 5) and `parse_file`/`parse_file_libpcap`.

- [ ] **Step 1: Extend the parity assertion.** In `crates/tcpvisr-ingest/tests/parity.rs`, add `names` to `assert_parity`:

```rust
    assert_eq!(pure.names, lib.names, "names differ for {name}");
```

- [ ] **Step 2: Add a DNS parity case** in `parity_for_each_link_type` (or a new `#[test]`):

```rust
    assert_parity(
        "par_dns_v4.pcap",
        &legacy_pcap(
            DLT_RAW,
            &[Pkt::new(TS, ipv4_udp_dns_response())],
        ),
    );
```

Import `ipv4_udp_dns_response` from `support`.

- [ ] **Step 3: Run to verify** (requires the `live` feature for the libpcap faucet):

Run: `cargo test -p tcpvisr-ingest --features live parity` → PASS (`names` equal across faucets).

- [ ] **Step 4: Confirm drift.** If the ingest `drift.rs` test asserts committed fixtures match the builder, ensure any new committed fixture is regenerated and matches. If M10 adds no *committed* fixture in ingest (the parity/parse tests build bytes in-memory), `drift.rs` is unaffected — run it to confirm:

Run: `cargo test -p tcpvisr-ingest drift` → PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/tcpvisr-ingest/tests/
git commit -m "test(ingest): assert cross-faucet name parity for DNS captures"
```

---

## Task 7: TUI resolves host labels on rows, search, and the detail title

**Files:**
- Modify: `crates/tcpvisr-tui/src/app.rs` (`App::new` takes a `NameTable`; `ConnMeta`/`ConnRow` gain `host`; search includes host; `FocusConn` exposes responder host)
- Modify: `crates/tcpvisr-tui/src/render.rs` (peer cell + detail title render host)

**Interfaces:**
- Consumes: `tcpvisr_core::{HostName, NameTable}`.
- Produces:
  - `App::new_with_names(timeline: Timeline, names: NameTable, title: String) -> App` — the new
    constructor. `App::new(timeline, title)` is **kept unchanged** and delegates:
    `Self::new_with_names(timeline, NameTable::default(), title)`. This avoids re-touching the ~19
    existing `App::new` call sites (render.rs, app.rs, keys.rs), which keep their current
    no-host behavior; only `build_replay_app` and the new host tests use `new_with_names`.
  - `ConnRow { …, pub host: Option<HostName> }`.
  - `FocusConn { …, pub responder_host: Option<HostName> }`.

- [ ] **Step 1: Write the failing tests** in `app.rs` `tests` module. Add a helper that builds a `NameTable` and a resolved `App`, then:

```rust
    fn table_of(pairs: &[(Endpoint, &str)]) -> tcpvisr_core::NameTable {
        let mut t = tcpvisr_core::NameTable::default();
        for (ep, name) in pairs {
            t.observe(tcpvisr_core::NameObservation {
                ts: Nanos(1),
                ip: ep.ip,
                name: tcpvisr_core::HostName::new(name).unwrap(),
            });
        }
        t
    }

    #[test]
    fn row_carries_resolved_host_over_ip() {
        let (c, s) = entry(ep(1, 51324), ep(2, 443), 10, 20, 0);
        let names = table_of(&[(ep(2, 443), "github.com")]);
        let app = App::new_with_names(Timeline::new(vec![(c, s)]), names, "t".to_string());
        let row = &app.visible()[0];
        assert_eq!(row.host.as_ref().map(|h| h.as_ref()), Some("github.com"));
    }

    #[test]
    fn unresolved_peer_has_no_host() {
        // app_of uses the plain App::new -> empty NameTable -> no host.
        let app = app_of(vec![entry(ep(1, 1), ep(2, 443), 0, 0, 0)]);
        assert!(app.visible()[0].host.is_none());
    }

    #[test]
    fn filter_matches_resolved_host_name() {
        let (c, s) = entry(ep(1, 51324), ep(2, 443), 10, 20, 0);
        let names = table_of(&[(ep(2, 443), "github.com")]);
        let mut app = App::new_with_names(Timeline::new(vec![(c, s)]), names, "t".to_string());
        app.enter_filter();
        for ch in "github".chars() {
            app.push_filter(ch);
        }
        assert_eq!(app.visible().len(), 1, "host name matches the filter");
    }
```

The existing `app_of` helper and all other `App::new(...)` call sites stay **unchanged** — `App::new` keeps its 2-arg signature and delegates to `new_with_names` with an empty table.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p tcpvisr-tui app::` → FAIL (`App::new` arity; `ConnRow.host` missing).

- [ ] **Step 3: Implement.** In `app.rs`:
  a. `ConnMeta` gains `host: Option<HostName>`; `ConnRow` gains `pub host: Option<HostName>`.
  b. Split the constructor: keep `App::new(timeline, title)` as a 2-arg wrapper, and move the body
     into `new_with_names`:

```rust
    #[must_use]
    pub fn new(timeline: Timeline, title: String) -> Self {
        Self::new_with_names(timeline, tcpvisr_core::NameTable::default(), title)
    }

    #[must_use]
    pub fn new_with_names(timeline: Timeline, names: tcpvisr_core::NameTable, title: String) -> Self {
        // ... existing body, but build each ConnMeta with a resolved host (see c).
    }
```

  c. When building each `ConnMeta`, set the host and fold it into `search_prefix`:

```rust
    let host = names.resolve(c.responder.ip).cloned();
    let host_str = host.as_ref().map(HostName::as_ref).unwrap_or("");
    let search_prefix =
        format!("{} {} {} {}", c.origin, c.responder, host_str, service.unwrap_or("")).to_lowercase();
    // ... ConnMeta { peer, service, origin_inferred, search_prefix, host }
```

  d. In `row(...)`, set `host: m.host.clone()` on the produced `ConnRow`.
  e. `FocusConn` gains `pub responder_host: Option<HostName>`; in `focus()` set it from the meta
     (`self.metas.get(&id).and_then(|m| m.host.clone())`).
  f. The `NameTable` is consumed at construction (resolved into `metas`); it is **not** stored on
     `App`. Import `tcpvisr_core::HostName` (and `NameTable` if not fully-qualified).

- [ ] **Step 4: Render the host** in `render.rs`:
  a. The master row peer cell (currently `Cell::from(r.peer.to_string())`, line ~73): render
     `format!("{host}:{}", r.peer.port)` when `r.host` is `Some`, else `r.peer.to_string()`. No
     manual truncation is needed — the peer is a ratatui `Table` cell, which **clips to the column
     width** automatically, so a 253-char name cannot overflow the pane (spec criterion 18).
  b. The detail title (currently `format!("DETAIL {} → {}", focus.origin, focus.responder)`, line
     ~106): render the responder as `format!("{host}:{}", focus.responder.port)` when
     `focus.responder_host` is `Some`, else `focus.responder.to_string()`. Keep the
     `DETAIL <origin> → <responder>` shape and the origin as-is.

- [ ] **Step 5: Run to verify it passes**

Run: `cargo test -p tcpvisr-tui app::` and `cargo test -p tcpvisr-tui render::` → PASS. No existing
`App::new` call site needs editing (the 2-arg wrapper is preserved). If `ConnRow` is constructed
literally anywhere in tests, add `host: None`.

- [ ] **Step 6: Add the render tests** (spec criteria 12, 18) in `render.rs` `tests`:
  - Title contains `→ github.com:443` for a resolved responder (build the `App` with a name table).
  - A ~253-char resolved name renders truncated: assert the rendered buffer width equals the terminal width (no overflow) and the peer cell contains a prefix of the name.

- [ ] **Step 7: Guardrails + commit**

`cargo clippy -p tcpvisr-tui --all-targets --all-features -- -D warnings` → clean.

```bash
git add crates/tcpvisr-tui/src/app.rs crates/tcpvisr-tui/src/render.rs
git commit -m "feat(tui): render DNS host labels on rows, filter, and detail title"
```

---

## Task 8: CLI wiring — build the `NameTable`, title name count, integration tests

**Files:**
- Modify: `crates/tcp-visr/src/main.rs` (`build_replay_app`)
- Modify: `crates/tcp-visr/tests/support/mod.rs` (DNS fixture builder, if needed for the bin test)
- Create/Modify: a committed DNS fixture under `crates/tcp-visr/tests/fixtures/` + `crates/tcp-visr/tests/drift.rs` guard (follow the existing fixture-as-bytes convention)

**Interfaces:**
- Consumes: `parse_file_visit_named`, `NameTable`, `App::new` (3-arg).

- [ ] **Step 1: Write the failing bin test** in `crates/tcp-visr/src/main.rs` `build_replay_tests`. Add a committed DNS fixture (a TCP connection + a DNS response resolving the responder IP to a host name), built by the support module like `metrics_basic.pcap`. Test:

```rust
    fn dns_fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/name_resolution.pcap")
    }

    #[test]
    fn build_replay_app_resolves_and_counts_names() {
        let cfg = replay_engine_config(10_000_000);
        let app = build_replay_app(&dns_fixture(), cfg).expect("build");
        // The responder resolves to the fixture's host name.
        let row = app
            .visible()
            .into_iter()
            .find(|r| r.host.is_some())
            .expect("a resolved row");
        assert_eq!(row.host.as_ref().map(|h| h.as_ref()), Some("example.com"));
        // The title reports the name count.
        assert!(app.title().contains("1 names"), "title: {}", app.title());
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p tcp-visr build_replay_app_resolves_and_counts_names` → FAIL (fixture missing / title has no name count / `build_replay_app` does not build a table).

- [ ] **Step 3: Build the fixture.** Add a `name_resolution.pcap` builder to `crates/tcp-visr/tests/support/mod.rs` (reuse the etherparse + `simple-dns` approach: a SYN/SYN-ACK/ACK to `93.184.216.34:443` plus a UDP/53 response `example.com A 93.184.216.34`). Add `simple-dns` as a `[dev-dependencies]` of `tcp-visr` if the support module builds DNS bytes. Write the fixture bytes to `crates/tcp-visr/tests/fixtures/name_resolution.pcap` and add a `drift.rs` case asserting the committed bytes equal the builder output (mirror the existing drift entries).

- [ ] **Step 4: Wire `build_replay_app`** in `main.rs`:

```rust
fn build_replay_app(
    file: &Path,
    cfg: EngineConfig,
) -> Result<tcpvisr_tui::App, Box<dyn std::error::Error>> {
    let mut tracker = Tracker::new(cfg);
    let mut names = tcpvisr_core::NameTable::default();
    let (_link, skipped) = tcpvisr_ingest::parse_file_visit_named(
        file,
        &mut |item| tracker.observe(item),
        &mut |obs| names.observe(obs.clone()),
    )?;
    let timeline = tracker.into_timeline()?;
    let capped = if names.dropped() > 0 { ", names capped" } else { "" };
    let title = format!(
        "tcp-visr — {}  ({} connections, {} names, skipped {}{})",
        file.display(),
        timeline.connection_count(),
        names.len(),
        skipped.total(),
        capped,
    );
    Ok(tcpvisr_tui::App::new_with_names(timeline, names, title))
}
```

Fully-qualify `tcpvisr_core::NameTable` (or add a `use`). `parse_file_visit_named` clones each `&NameObservation` into the table. `build_replay_app`'s public signature is unchanged (only its body), so `run_replay` and the existing bin tests are unaffected.

- [ ] **Step 5: Run to verify it passes**

Run: `cargo test -p tcp-visr build_replay_app_resolves_and_counts_names` → PASS. Then the whole bin crate `cargo test -p tcp-visr` → PASS (the existing `builds_a_timeline_app_with_rows`, throughput/rtt/inflight/seq focus tests, and the oracle tests must stay green; update any other `App::new` / `build_replay_app` call as needed — `build_replay_app` signature is unchanged, only its body, so callers are unaffected).

- [ ] **Step 6: Confirm the oracle is byte-identical**

Run: `cargo test -p tcp-visr --test metrics` and any `--test oracle`/render-closed tests → PASS unchanged (spec criterion 15). If an oracle golden changed, a non-engine file was wrongly touched — revert it.

- [ ] **Step 7: Guardrails + commit**

```bash
git add crates/tcp-visr/src/main.rs crates/tcp-visr/tests/ crates/tcp-visr/Cargo.toml Cargo.lock
git commit -m "feat(cli): resolve DNS host labels and surface the name count"
```

---

## Task 9: Full-suite green + CLAUDE.md milestone note

**Files:**
- Modify: `CLAUDE.md` (bump the "Current state" line: M0–M10 implemented; describe capture-DNS labels)

- [ ] **Step 1: Run the entire CI gate locally**

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace
cargo test -p tcpvisr-ingest --features live
cargo deny check
```

All green. If `cargo test --workspace` surfaces an untouched `App::new`/`ReplayParse` literal in a test elsewhere, fix it to the new arity/field.

- [ ] **Step 2: Update `CLAUDE.md`** — change the "Current state: milestones M0–M9 are implemented" sentence to include M10, and add one sentence: capture-DNS host labels resolve each peer to a DNS name from the capture's A/AAAA answers via a bounded latest-wins `NameTable`; the pure engine is untouched and live reverse-DNS is deferred to M11/M12 (ADR-0015).

- [ ] **Step 3: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: mark M10 (capture-DNS name resolution) implemented"
```

---

## Self-review checklist (run before opening the PR)

- **Spec coverage:** criteria 1-5 → Tasks 3-4; 6-7 → Tasks 5-6; 8-9 → Tasks 1-2; 10-12 → Task 7; 13-14 → Task 8; 15 → Tasks 8 step 6 (oracle green); 16 → Tasks 6/8 drift; 17 → Task 2; 18 → Task 7 step 6.
- **Engine untouched:** no file under `crates/tcpvisr-engine/` is in any task's file list. Confirm `git diff --stat main -- crates/tcpvisr-engine` is empty before pushing.
- **`Item` parity intact:** Task 6 extends parity to `names`; `items` assertions unchanged.
- **No phantom live code:** no resolver trait / `hickory-resolver` dep added (grep the diff).
- **deny.toml untouched:** `simple-dns` is MIT (already allowed); confirm `cargo deny check` is green without editing `deny.toml`.
