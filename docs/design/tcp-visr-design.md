# tcp-visr — Unified Design

> Status: **Accepted** · Date: 2026-06-30 · Owner: David Christensen
>
> This is the umbrella design and source of truth for the project. Per-milestone
> `spec.md` and `plan.md` documents refine it; cross-cutting decisions live as
> [ADRs](#13-decision-index-adrs). When this document and an ADR disagree, the ADR
> is authoritative for that decision and this document should be updated to match.

## 1. Summary

`tcp-visr` is a Rust terminal UI for visualizing **TCP connection dynamics over time**,
from either a **live** Linux system or a **replayed** packet capture (`.pcap` / `.pcapng`).

It occupies a gap in the existing tooling landscape:

- **Connection monitors** (`rustnet`, `oryx`, `netwatch`, `ss`) show *who is talking to
  whom and how much* — connection tables, bandwidth, process attribution. They do not
  show how a single connection *behaves* over time.
- **Dynamics analyzers** (Wireshark TCP Stream Graphs, `tcptrace` + `xplot`) show
  per-connection dynamics — sequence progression, window, RTT — but are desktop GUI /
  static-plot tools, not live, and offer no variable-speed replay.

`tcp-visr` combines a **connection landscape** (master list) with deep **per-connection
dynamics** (detail views), in a TUI, working identically for live and replayed data, with
full transport controls (play/pause, 0.1–10× speed, seek/scrub, step).

## 2. Goals and non-goals

### Goals (v1)
- TCP-only analysis, IPv4 and IPv6. IPv6 in scope covers the common extension-header chains
  (hop-by-hop, routing, destination-options, fragment); unsupported/abnormal chains and
  IPv6-fragmented TCP segments are skipped-and-counted, not silently mis-parsed.
- One analysis engine shared by live capture and pcap replay.
- Master/detail UX: browse connections, drill into dynamics.
- Four dynamics views: Time/Sequence (Stevens), In-flight/cwnd, RTT, Throughput/goodput.
- Full transport controls including arbitrary seek/scrub.
- Service (port→name) and host (DNS) labels.
- Live-only enrichment: real kernel `cwnd`/`srtt`/retransmits (via `sock_diag`) and
  process attribution (via `/proc`), overlaid on the wire-derived series.

### Non-goals (v1)
- UDP / QUIC analysis. *(QUIC is encrypted and a distinct dynamics model.)*
- Deep L7 decode (TLS SNI, HTTP). *(Deferred to keep the hostile-input surface small.)*
- Non-Linux platforms. *(Enrichment is Linux-specific; revisit post-1.0.)*
- Packet editing, injection, or capture-to-disk authoring. *(Read/visualize only.)*

## 3. Architecture

A Cargo workspace of focused crates. Each crate has one responsibility, a small public
interface, and is testable in isolation.

```
                 ┌─────────────┐       ┌──────────────────┐
  .pcap/.pcapng ─►│ tcpvisr-     │      │ tcpvisr-enrich    │  (live only)
  (replay)        │ ingest       │      │ sock_diag + /proc │
                 │ (pure-Rust   │      │ real cwnd/srtt,   │
  live capture ──►│  parse +     │      │ process names     │
  (libpcap)       │  libpcap)    │      └────────┬──────────┘
                 └─────┬────────┘                │ keyed by ConnId
                        │ Item (Segment|Tick)      │
                        ▼                          ▼
                 ┌──────────────────────────────────────────┐
                 │ tcpvisr-engine  (pure: no I/O)             │
                 │ per-connection state machine               │
                 │   → time-indexed MetricSample series        │
                 └──────────────────┬─────────────────────────┘
                                    ▼
                 ┌──────────────────────────────────────────┐
                 │ tcpvisr-tui (ratatui)                      │
                 │ timeline cursor (play/pause/seek/speed)    │
                 │ master list + 4 detail graph views         │
                 └──────────────────────────────────────────┘
```

### 3.1 Crates

| crate | responsibility | depends on | I/O |
|-------|----------------|------------|-----|
| `tcpvisr-core` | shared types: `FlowKey` (TCP 4-tuple, v4/v6), `ConnId`, `Item`, `Segment`, `MetricSample`; time as nanoseconds-since-capture-start (`u64`); serial-number arithmetic | — | none |
| `tcpvisr-ingest` | `.pcap`/`.pcapng` parse (replay) + libpcap capture (live) → `Item` stream (§4.1); link types: Ethernet II, Linux SLL & SLL2, raw IP, loopback | core | files; libpcap (optional, `live` feature) |
| `tcpvisr-engine` | TCP connection state machine + metric derivation → per-connection time-indexed series | core | **none (pure)** |
| `tcpvisr-enrich` | live-only: `sock_diag` (real cwnd/srtt/retrans) + `/proc` (process attribution), matched by `ConnId` (instance-aware) | core | netlink, procfs |
| `tcpvisr-tui` | ratatui master/detail UI, timeline cursor, the four graph views | core, engine | terminal |
| `tcp-visr` (bin) | clap CLI; subcommands `replay`, `live`, `parse`, `conns`, `metrics`; wires faucet → engine → tui | all | — |

### 3.2 The load-bearing interfaces

1. **Ingest boundary — `Item` (`Segment | Tick`) stream.** Both faucets (file, wire) emit
   the same item type. The engine never knows the source. This is what makes live and
   replay share ~90% of the code. See [ADR-0001](../adr/0001-packet-derived-unified-model.md).
2. **Pure engine.** The engine takes timestamped `Item`s and emits metric samples with
   no file handles, no sockets, and no clock reads. Event-driven TCP edge cases are pure
   unit tests fed a hand-built `Vec<Item>`; time-driven cases (idle, RTO, throughput decay)
   are exercised by injecting `Tick` items. See
   [ADR-0002](../adr/0002-pure-engine-io-boundary.md).
3. **One header decoder for both faucets.** libpcap (live) and `pcap-parser` (replay) differ
   only in the *container/source* layer; both hand raw link-layer frames to the **same**
   `etherparse`-based decoder that produces `Segment`s. This prevents the live and replay
   paths from decoding identical on-wire bytes into different segments. A parity test (§8)
   feeds one capture through both faucets and asserts identical `Item` streams.
   `etherparse` 0.20.2 parses Ethernet II and SLL v1 natively, but **not** the SLL2 cooked
   header (the default link type for `tcpdump -i any` on modern libpcap), so ingest hand-decodes
   only the SLL2 header and hands the IP payload to `etherparse`. See
   [ADR-0003](../adr/0003-libpcap-for-live-capture.md).

## 4. Data model

```
FlowKey      = (src_ip, src_port, dst_ip, dst_port)   // TCP 4-tuple; protocol implicit (TCP-only)
ConnId       = (FlowKey, instance)                    // see "connection identity" below
Item         = Segment(ts, …) | Tick(ts)              // engine input; see §4.1
Segment      = { seq, ack, flags, window, win_scale?, ts_opt?, sack_blocks, payload_len, direction }
MetricSample = { t, in_flight_bytes, rtt?, throughput_bps, retransmit, out_of_order, sack }
Connection   = { id, state, opened_at, closed_at?, isn, series: Vec<MetricSample>, labels }
KernelInfo   = { snd_cwnd, srtt, rttvar, retrans, delivery_rate, process? }  // live enrich
```

**Connection identity.** A bare 4-tuple is *not* a stable connection identity: within one
capture the same socket pair is reused (ephemeral port reuse, `TIME_WAIT` recycling, NAT
rebinding). Keying solely on the 4-tuple would splice two independent sequence spaces into
one series and corrupt in-flight/RTT/retransmit derivation. We therefore key connections by
`ConnId = (FlowKey, instance)`: a new `instance` epoch begins on a SYN that follows a prior
close/RST, or after the connection has been idle past the same configurable idle/dead timeout
used by the `Tick` machinery (§4.1) — one knob, not two. For mid-stream captures with no
observed SYN, a new instance is inferred only from a sequence reset that is **backward in RFC
1982 serial-number space** (a drop to a plausible fresh ISN), which is explicitly *not* a
benign `u32` wrap — a wrap is *forward* under serial comparison and must never split a flow.
This is why instance inference and gap detection share the serial-number arithmetic below.
`tcptrace` disambiguates instances the same way and for the same reason.

**Wire metrics are "bytes in flight", not cwnd.** `in_flight_bytes` = `highest_seq_sent −
highest_ack_seen` measures *outstanding* bytes on the wire, which is
`min(cwnd, receiver_window, app-limited send rate)` — it equals the sender's congestion
window only for an at-sender capture of a non-rwnd-limited, non-app-limited sender. The wire
series is therefore labeled **bytes in flight (outstanding)** everywhere; the term **cwnd**
is reserved for the kernel series (`KernelInfo.snd_cwnd`, live only). The two are overlaid
for comparison but never conflated: `in_flight ≤ cwnd` always.

**Capture vantage point** (at-sender vs. mid-path) determines what is observable: RTT halves,
which retransmits are seen, and whether the wire/kernel comparison is well-posed. The vantage
is recorded and shown; the wire-vs-kernel overlay is only meaningful for at-sender live
capture (the one configuration where `KernelInfo` exists at all).

Sequence and ack numbers are `u32` and **wrap**. All arithmetic on them (in-flight,
RTT pairing, gap detection) uses serial-number comparison per RFC 1982, in
`tcpvisr-core`, never naive subtraction. This is the single most error-prone area and is
covered by `proptest`.

### 4.1 Sampling, time, and as-of-T semantics

- **Cadence**: one `MetricSample` per processed `Segment` (per-event sampling). The series
  time index is therefore irregular; "state as of `T`" means the **last sample at or before
  `T`** (last-value-carried-forward), not interpolation.
- **Throughput** is a trailing sliding window of fixed length (default 1 s, configurable),
  computed and frozen into each sample at ingest time — it cannot be recomputed at scrub time
  because seeking never re-parses (§5).
- **Time advances without packets via `Tick` items.** A connection that goes silent (no FIN,
  no RST, no data) emits no segments, so in live mode `ingest` injects periodic `Tick(ts)`
  items into the same stream. Ticks drive idle/dead transitions, inferred-RTO expiry, and
  throughput decay-to-zero. The engine stays pure: time is still data
  ([ADR-0002](../adr/0002-pure-engine-io-boundary.md)). In replay, "now" is simply the last
  segment's timestamp and ticks are unnecessary.

## 5. The seekable timeline

Arbitrary seek requires resolving **every active connection's** state at any time `T`.
Two structures, two costs:

- **Within one connection**: binary search to the last sample ≤ `T` — O(log m), m = samples.
- **Across connections**: an interval index over `[opened_at, closed_at]` yields the set of
  connections active at `T`. Still-open connections (and any open at capture EOF) have no
  `closed_at`; they are indexed with an open right bound (`+∞`, i.e. the running "now") so they
  match every `T ≥ opened_at`. Let `N_T` be the connections active at `T`. A random seek that
  must order the master list by a *time-varying* column resolves all `N_T` —
  **O(N_T·log m)** per frame; this is the honest worst case and the cost the headline owns,
  not a display-capped subset. When the sort key is static (process, peer, port), only the
  visible rows are resolved (lazy, display-capped). During monotonic playback each connection
  advances O(1) from its prior sample (O(N_T) per frame). "Constant-feeling" holds for bounded
  `N_T`, degrading to O(N_T·log m) on a random seek with a time-varying sort.
- **Replay**: ingest parses the capture up front; the engine produces complete per-connection
  series. Subject to the capture-size policy in §7 (bounded, with fail-fast).
- **Live**: the same engine is fed from libpcap + `Tick` items (§4.1). The live timeline
  (bounded ring buffer, pause/freeze, eviction) is delivered and tested in **M11**, not the
  replay-only M5 — see §10.
- **Speed** (0.1–10×) only controls how fast the cursor advances over precomputed series.
  It never re-parses, so 10× and 0.1× cost the same. See
  [ADR-0004](../adr/0004-seekable-timeseries-timeline.md).

## 6. TUI layout

```
┌ tcp-visr ──────────────────────────────[ ▶ 2.0x  t=12.480s / 38.2s ]┐
│ CONNECTIONS (47)                    │ DETAIL: 10.0.0.5:51324 → github.com:443
│ ▸curl   github.com:443  ESTAB  ▓▓ │ ┌ Time/Sequence (Stevens) ───────────────┐
│  ssh    10.0.0.9:22     ESTAB  ▁  │ │  seq ╱──── · = retransmit ╎ = SACK      │
│  chrome cdn:443         TIME_W ·  │ └──────────────────────────────────────────┘
│  ...                              │ ┌ In-flight / cwnd ──┐┌ RTT ───────────────┐
│                                   │ │ wire ╱╲╱╲ kern ──  ││  ▁▂▃▂▁▂▅▂  42ms      │
│                                   │ └────────────────────┘└────────────────────┘
└──────[ / filter  s sort  ⏎ open  space pause  ←→ seek  +/- speed  q quit ]───────┘
```

- **Master pane**: connection table (process/peer/state/↑↓ bytes), sort, `/` fuzzy
  filter, selection; reflects state "as of cursor time `T`".
- **Detail pane**: tab between the four graph views; drawn with ratatui `Canvas`/braille,
  axes auto-scaled to the visible time window.
- The in-flight chart plots **bytes in flight (outstanding)** from the wire; in at-sender
  live mode it overlays the kernel's true **cwnd** (`KernelInfo.snd_cwnd`) as a distinct
  series. They are never conflated (in-flight ≤ cwnd; see §4). Divergence is a diagnostic
  signal only when both share a vantage — i.e. at-sender live capture; elsewhere the kernel
  series is simply absent.

## 7. Error handling

- Fail fast with actionable messages (what operation, what input, suggested fix).
- Malformed packets in a capture are **skipped and counted**, not fatal; the count is
  surfaced in the UI status line. A truncated capture renders what parsed.
- Missing `CAP_NET_RAW` for live capture produces a clear message naming the `setcap`
  fix. If the unprivileged handle opens but yields no packets, that is detected and
  surfaced as a privilege problem (not silent "idle network") — verified in M11.
- **Live capture cannot back-pressure the wire.** If the bounded input buffer fills under
  load (the engine can't keep up with libpcap), segments are **dropped and counted**, never
  silently lost or buffered unbounded; the drop count is surfaced in the status line and flags
  affected connections' derived metrics as approximate. (Replay has genuine flow control —
  §3.2, ADR-0002 — so it never drops.) The dropped-segment counter is owned by M11.
- Enrichment is per-connection optional: a connection with no local socket (remote peer),
  no `/proc` entry, or missing `sock_diag` shows an explicit `n/a` in the kernel columns
  rather than blank or stale data. No silent fallbacks: any degraded mode is shown.
- **Capture-size policy (memory).** Replay holds per-connection series in memory (§5,
  [ADR-0004](../adr/0004-seekable-timeseries-timeline.md)). v1 enforces a configurable
  sample/size ceiling; exceeding it fails fast with a clear message (size, ceiling, and the
  `--max-*` override) rather than risking OOM. Streaming/indexing of very large captures is
  a post-v1 enhancement, explicitly out of v1 scope.

## 8. Testing strategy

- **engine**: pure unit tests fed hand-built `Vec<Item>` — reorder, retransmit, SACK,
  zero-window, mid-stream (no handshake), simultaneous-open, mid-stream RST, 4-tuple reuse
  (instance disambiguation), `u32` seq wraparound, and `Tick`-driven idle/RTO/decay.
  `proptest` for serial-number arithmetic.
- **validation oracle**: derived metrics for a fixture set are cross-checked against an
  independent tool (`tcptrace`, Wireshark TCP stream graphs, or live `ss`/`TCP_INFO` on a
  self-generated capture). This catches a *systematically wrong* estimate — which the
  hand-built unit tests cannot, since they assert the author's own expectation, and the
  kernel overlay that would expose it does not exist in replay-only v0.1.
- **ingest parity**: one capture fed through both faucets (libpcap and `pcap-parser`) must
  produce identical `Item` streams, guarding the "one engine, identical behavior" promise.
- **ingest**: small committed `.pcap`/`.pcapng` fixtures, one per link type (incl. SLL2 and
  an IPv6 extension-header chain).
- **tui**: `ratatui::TestBackend` frame snapshot tests.
- **enrich**: behind a trait so it is mockable; the only component needing a live host.
- Behavior, not implementation: tests assert what the metrics say, not how they are computed.

## 9. Dependencies (verified current 2026-06-30; pinned exactly in M0)

| crate | version | role |
|-------|---------|------|
| `ratatui` | 0.30.2 | TUI rendering |
| `crossterm` | (ratatui-compatible) | terminal backend |
| `pcap` | 2.4.0 | live capture (libpcap binding) |
| `pcap-parser` | latest | pure-Rust `.pcap`/`.pcapng` file container parse |
| `etherparse` | 0.20.2 | Ethernet/SLL/IPv4/IPv6/TCP header parse |
| `clap` | 4.x | CLI |
| `netlink-packet-sock-diag` (+ `netlink-sys`) | latest | kernel TCP_INFO via sock_diag |
| `procfs` | latest | process attribution |
| `hickory-resolver` | latest | async reverse DNS (live) |
| `proptest` | latest | property tests |

Each new dependency is justified; exact versions are pinned (`=`) and audited with
`cargo-deny` in CI.

## 10. Milestone roadmap

Each milestone is scoped to a single PR. Milestones map 1:1 to GitHub **epic issues**;
tasks within a milestone map to **sub-issues**. See [§12](#12-development-workflow).

| ID | Title | Definition of Done | Release |
|----|-------|--------------------|---------|
| **M0** | Repo & toolchain | green CI (fmt, clippy `-D warnings`, test); `cargo run -- --help`; lint set, `cargo-deny`, prek hooks, templates, LICENSE | v0.1 |
| **M1** | Packet model & replay parser | `tcp-visr parse f.pcap` prints decoded TCP segments; fixtures per link type incl. SLL2 and an IPv6 extension-header chain; both faucets pass the parity test (§8). The libpcap faucet enters here as a default-off, file-reading faucet for the parity test ([ADR-0005](../adr/0005-libpcap-file-faucet-at-m1.md)); live interface capture stays M11 | v0.1 |
| **M2** | Connection state machine | `tcp-visr conns f.pcap` lists connections with state, bytes, duration; passes fixtures for mid-stream (no SYN), simultaneous-open, mid-stream RST, 4-tuple reuse (distinct instances), and seq-wrap-vs-new-instance disambiguation (§4) | v0.1 |
| **M3** | Metric derivation | `tcp-visr metrics f.pcap --conn N` dumps the series (JSON); in-flight, throughput (defined window), retransmit/OOO/SACK; RTT paired under Karn's algorithm; passes `u32` seq-wrap fixtures and the validation oracle (§8) | v0.1 |
| **M4** | TUI shell: master list | browse a capture's connections; sort, `/` filter, selection; port→service labels | v0.1 |
| **M5** | Timeline + transport controls (replay) | scrub a *replayed* capture; play/pause, 0.1–10× speed, seek, step; master list resolves all active connections "as of T" via the cross-connection interval index (§5). Live-timeline semantics are M11 | v0.1 |
| **M6** | Detail: Time/Sequence (Stevens) | seq-vs-time graph with retransmit/SACK marks, cursor-driven | v0.1 |
| **M7** | Detail: In-flight / cwnd | wire-estimated in-flight sawtooth; overlay hooks for kernel cwnd | v0.1 |
| **M8** | Detail: RTT | per-ack RTT samples + smoothed line | v0.1 |
| **M9** | Detail: Throughput/goodput | sliding-window bytes/sec, goodput vs retransmitted; detail view switcher finalized | v0.1 |
| **M10** | Name resolution | capture-DNS (IP→name from DNS packets) + live reverse-DNS with caching | v0.1 |
| **M11** | Live capture (libpcap) | `tcp-visr live -i eth0`; interface select, BPF filter, nanosecond-precision timestamps (fallback micro); `Tick` injection drives idle/decay; bounded ring buffer with eviction; pause/freeze with cursor clamped to the eviction horizon; running per-connection baseline retained for connection life independent of display retention (§5, ADR-0004); unprivileged open errors rather than yielding silent-empty | v0.2 |
| **M12** | Live kernel enrichment | `sock_diag` real cwnd/srtt/retrans + `/proc` attribution joined by `ConnId` (instance-aware, §4); defined poll cadence, sample-to-wire-timeline alignment, socket-disappearance and recycled-tuple guards; `n/a` for unenriched connections; overlays on M7/M8; absent on replay | v0.2 |
| **M13** | Ship 1.0 | config + themes, error UX, man page, asciinema README, install docs (incl. per-platform libpcap requirement for `live`), crates.io metadata, release CI (static `--no-default-features` replay-only binary + default libpcap-dynamic binary with `live`; checksums), CHANGELOG | v1.0 |

**Ordering rationale**: replay-first (M1–M10) exercises the engine against deterministic
files before introducing the nondeterminism of a live wire, root, and a real NIC
(M11–M12).

## 11. Documentation architecture

```
docs/
  design/tcp-visr-design.md   ← this file (umbrella, source of truth)
  adr/
    0000-template.md
    0001-packet-derived-unified-model.md
    0002-pure-engine-io-boundary.md
    0003-libpcap-for-live-capture.md
    0004-seekable-timeseries-timeline.md
  milestones/
    m00-repo-setup/{spec.md, plan.md}
    m01-replay-parser/{spec.md, plan.md}
    …
  CHANGELOG.md                ← keyed by release milestones
```

| Doc | Answers | Created | Becomes |
|-----|---------|---------|---------|
| unified design | what the whole system is + why | now | repo source of truth |
| ADR `NNNN` | why one cross-cutting decision | now (settled forks) + as they arise | referenced by milestones |
| milestone `spec.md` | what's in this PR, when it's done | when the milestone starts | epic issue body + acceptance criteria |
| milestone `plan.md` | how to build it, step by step | **just-in-time**, per milestone (via writing-plans) | sub-issues + PR checklist |

Conventions:
- `plan.md` is generated **just before** a milestone is implemented, not all 14 up front
  (plans written far ahead go stale).
- Each `spec.md` carries a traceability header:
  `Implements: design §10.M3 · Depends-on: ADR-0001, ADR-0002 · Touches: area:engine`.
- ADRs are immutable; a reversal is a new ADR that supersedes, recorded in the
  [decision index](#13-decision-index-adrs).
- Definition of Done lives in `spec.md` and is mirrored (not re-authored) into the PR
  template checklist.

## 12. Development workflow

GitHub primitives map onto three layers:

| Layer | Primitive | Mapping |
|-------|-----------|---------|
| Release | native **Milestone** field | `v0.1 Replay MVP` (M0–M10), `v0.2 Live` (M11–M12), `v1.0 Ship` (M13) |
| Epic | **issue with sub-issues** | one per `M0…M13` |
| Task | **sub-issue** | discrete tasks within a milestone |

A milestone's delivery PR closes its epic; each sub-issue is closed by a commit in that PR.

### Label taxonomy (prefix-grouped, lean)

- `area:` — one per crate/boundary: `area:core` `area:ingest` `area:engine` `area:enrich`
  `area:tui` `area:cli` `area:ci` `area:docs`
- `type:` — `type:epic` `type:feature` `type:task` `type:bug` `type:chore` `type:adr`
- `status:` — only what a board doesn't show: `status:blocked` `status:needs-design`
- specials — `breaking-change` `security` `good-first-issue`

Deliberately omitted: `priority:` and `milestone:` labels — the native Milestone field
and roadmap ordering already encode those, and duplicate state rots.

### Commit / PR conventions
- Conventional Commits 1.0.0; imperative mood; ≤72-char subject; one logical change.
- Feature branches and PRs only; never push to `main`.
- Code PRs are merged `--rebase` or `--merge`, **never `--squash`** (preserves
  `git bisect` granularity).

## 13. Decision index (ADRs)

| ADR | Decision | Status |
|-----|----------|--------|
| [0001](../adr/0001-packet-derived-unified-model.md) | Packets are the unified data model; live enrichment is additive | Accepted |
| [0002](../adr/0002-pure-engine-io-boundary.md) | The analysis engine is pure (no I/O) | Accepted |
| [0003](../adr/0003-libpcap-for-live-capture.md) | Live capture uses libpcap (`pcap` crate); replay parsing is pure-Rust | Accepted |
| [0004](../adr/0004-seekable-timeseries-timeline.md) | Precomputed time-indexed series + cursor for seek/speed | Accepted |
| [0005](../adr/0005-libpcap-file-faucet-at-m1.md) | libpcap enters at M1 as a default-off file faucet for the parity test (amends ADR-0003 timing) | Accepted |

## 14. Risks

- **Outstanding-bytes vs. real cwnd** — the wire series is *bytes in flight*, not cwnd;
  labeled and kept distinct (§4), never presented as the kernel's cwnd.
- **Large-capture memory** — full per-connection series in RAM (ADR-0004) scales with
  packet count; a large well-formed capture is the simplest resource-exhaustion path.
  Mitigated by the v1 capture-size ceiling with fail-fast (§7); streaming is post-v1.
- **Capture vantage point** — at-sender vs. mid-path changes what is observable (RTT halves,
  visible retransmits) and whether the wire/kernel overlay is meaningful. Vantage is recorded
  and shown; the overlay is gated to at-sender live capture (§4).
- **Faucet timestamp skew** — libpcap defaults to microsecond precision while pcapng files
  may carry nanosecond; live nanosecond is requested explicitly (fallback micro) and both
  faucets normalize to one internal time unit, so RTT fidelity tracks the source, not the
  faucet. Wall-clock timestamps are not assumed monotonic.
- **TUI chart resolution** — terminal cells limit graph fidelity; braille/`Canvas`
  mitigate, but dense captures may need downsampling per visible window.
- **libpcap system dependency** — documented install + `setcap`; replay path stays
  libpcap-free so the tool (and its static binary) is useful without it.
- **Hostile capture input** — parsing untrusted pcaps; mitigated by malformed-packet
  skip-and-count and `etherparse`'s allocation-free, fuzz-tested slicing. Crafted IPv6
  extension-header chains are skipped-and-counted when unsupported (§2 notes ext-header scope).
