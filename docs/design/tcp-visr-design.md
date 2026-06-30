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
- TCP-only analysis, IPv4 and IPv6.
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
                 └─────┬────────┘                │ keyed by FlowKey
                        │ (ts, Segment) stream     │
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
| `tcpvisr-core` | shared types: `FlowKey` (5-tuple, v4/v6), `Segment`, `MetricSample`, time units, serial-number arithmetic | — | none |
| `tcpvisr-ingest` | pure-Rust `.pcap`/`.pcapng` parse (replay) + libpcap capture (live) → `(Timestamp, Segment)` stream; link types: Ethernet II, Linux SLL/SLL2, raw IP, loopback | core | files, libpcap |
| `tcpvisr-engine` | TCP connection state machine + metric derivation → per-connection time-indexed series | core | **none (pure)** |
| `tcpvisr-enrich` | live-only: `sock_diag` (real cwnd/srtt/retrans) + `/proc` (process attribution), matched by `FlowKey` | core | netlink, procfs |
| `tcpvisr-tui` | ratatui master/detail UI, timeline cursor, the four graph views | core, engine | terminal |
| `tcp-visr` (bin) | clap CLI; subcommands `replay`, `live`, `parse`, `conns`, `metrics`; wires faucet → engine → tui | all | — |

### 3.2 The two load-bearing interfaces

1. **Ingest boundary — `(Timestamp, Segment)` stream.** Both faucets (file, wire) emit
   the same item type. The engine never knows the source. This is what makes live and
   replay share ~90% of the code. See [ADR-0001](../adr/0001-packet-derived-unified-model.md).
2. **Pure engine.** The engine takes timestamped segments and emits metric samples with
   no file handles and no sockets. Every TCP edge case is a pure unit test fed a
   hand-built `Vec<Segment>`. See [ADR-0002](../adr/0002-pure-engine-io-boundary.md).

## 4. Data model

```
FlowKey      = (src_ip, src_port, dst_ip, dst_port)   // canonicalized direction-independent
Segment      = { seq, ack, flags, window, win_scale?, ts_opt?, sack_blocks, payload_len, direction }
MetricSample = { t, in_flight_bytes, rtt?, throughput_bps, retransmit, out_of_order, sack }
Connection   = { key, state, opened_at, closed_at?, series: Vec<MetricSample>, labels }
KernelInfo   = { snd_cwnd, srtt, rttvar, retrans, delivery_rate, process? }  // live enrich
```

Sequence and ack numbers are `u32` and **wrap**. All arithmetic on them (in-flight,
RTT pairing, gap detection) uses serial-number comparison per RFC 1982, in
`tcpvisr-core`, never naive subtraction. This is the single most error-prone area and is
covered by `proptest`.

## 5. The seekable timeline

Arbitrary seek requires knowing every connection's state at any time `T` cheaply.

- **Replay**: ingest fully parses the capture up front; the engine produces complete
  per-connection series. Seeking is a binary search into the series (O(log n)).
- **Live**: the same engine is fed from libpcap; series are held in a bounded ring
  buffer (configurable retention window). Pause freezes the cursor while the series keeps
  appending; scroll-back is bounded by retention.
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
- In live mode, wire-derived series and kernel series are drawn as distinct overlays —
  never conflated. Divergence between them is itself a diagnostic signal.

## 7. Error handling

- Fail fast with actionable messages (what operation, what input, suggested fix).
- Malformed packets in a capture are **skipped and counted**, not fatal; the count is
  surfaced in the UI status line. A truncated capture renders what parsed.
- Missing `CAP_NET_RAW` for live capture produces a clear message naming the `setcap`
  fix. Absent `sock_diag`/`/proc` enrichment degrades gracefully (wire-only series).
- No silent fallbacks: any degraded mode is shown to the user.

## 8. Testing strategy

- **engine**: pure unit tests fed hand-built segment vectors — reorder, retransmit, SACK,
  zero-window, mid-stream (no handshake) captures. `proptest` for serial-number arithmetic.
- **ingest**: small committed `.pcap`/`.pcapng` fixtures, one per link type.
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
| **M1** | Packet model & replay parser | `tcp-visr parse f.pcap` prints decoded TCP segments; fixtures per link type | v0.1 |
| **M2** | Connection state machine | `tcp-visr conns f.pcap` lists connections with state, bytes, duration; handles mid-stream captures | v0.1 |
| **M3** | Metric derivation | `tcp-visr metrics f.pcap --conn N` dumps the time series (JSON); in-flight, RTT, throughput, retransmit/OOO/SACK | v0.1 |
| **M4** | TUI shell: master list | browse a capture's connections; sort, `/` filter, selection; port→service labels | v0.1 |
| **M5** | Timeline + transport controls | scrub a capture; play/pause, 0.1–10× speed, seek, step; list updates "as of T" | v0.1 |
| **M6** | Detail: Time/Sequence (Stevens) | seq-vs-time graph with retransmit/SACK marks, cursor-driven | v0.1 |
| **M7** | Detail: In-flight / cwnd | wire-estimated in-flight sawtooth; overlay hooks for kernel cwnd | v0.1 |
| **M8** | Detail: RTT | per-ack RTT samples + smoothed line | v0.1 |
| **M9** | Detail: Throughput/goodput | sliding-window bytes/sec, goodput vs retransmitted; detail view switcher finalized | v0.1 |
| **M10** | Name resolution | capture-DNS (IP→name from DNS packets) + live reverse-DNS with caching | v0.1 |
| **M11** | Live capture (libpcap) | `tcp-visr live -i eth0`; interface select, BPF filter, ring-buffer retention, tail + pause/freeze + bounded scrollback | v0.2 |
| **M12** | Live kernel enrichment | `sock_diag` real cwnd/srtt/retrans + `/proc` attribution by 5-tuple; overlays on M7/M8; graceful on replay | v0.2 |
| **M13** | Ship 1.0 | config + themes, error UX, man page, asciinema README, install docs, crates.io metadata, release CI (cross-compiled binaries + checksums), CHANGELOG | v1.0 |

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

## 14. Risks

- **Estimated vs. real cwnd** — wire-derived in-flight is an estimate; kept as a distinct
  series, never presented as the kernel's cwnd.
- **TUI chart resolution** — terminal cells limit graph fidelity; braille/`Canvas`
  mitigate, but dense captures may need downsampling per visible window.
- **libpcap system dependency** — documented install + `setcap`; replay path stays
  libpcap-free so the tool is useful without it.
- **Hostile capture input** — parsing untrusted pcaps; mitigated by malformed-packet
  skip-and-count and `etherparse`'s allocation-free, fuzz-tested slicing.
