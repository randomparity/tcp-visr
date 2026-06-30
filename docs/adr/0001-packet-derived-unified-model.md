# ADR-0001: Packets are the unified data model; live enrichment is additive

> Status: Accepted
> Date: 2026-06-30

## Context

The tool must visualize TCP dynamics for both a **live** Linux system and a **replayed**
packet capture. TCP metrics (in-flight/cwnd, RTT, retransmits, sequence progression) can
be sourced two fundamentally different ways:

- **From packets** — derived via sequence/ack analysis (à la `tcptrace`). Available for
  both live sniffing and file replay.
- **From the kernel** — `sock_diag`/`TCP_INFO` or eBPF give the kernel's *real* cwnd/srtt.
  Available only for sockets owned by the local host, live; a replayed capture cannot
  produce them.

If kernel data were the primary model, replay — a hard requirement — could not work.

## Decision

We will make **packet-derived metrics the single unified data model** consumed by the
engine. Live kernel data (`sock_diag` + `/proc`) is an **additive enrichment** layer,
matched to connections by an instance-aware identity (`ConnId`, not a bare 4-tuple — see
Consequences) and overlaid on the wire-derived series. It is never required for the core
experience and is simply absent during replay.

## Consequences

- Live and replay share one engine and one set of metric types; the engine is unaware of
  the source. This is the basis for [ADR-0002](0002-pure-engine-io-boundary.md).
- **Connection identity must be instance-aware.** A bare 4-tuple is not a stable identity:
  ephemeral port reuse, `TIME_WAIT` recycling, and NAT mean the same 4-tuple recurs within a
  capture for *different* connections. Keying on the 4-tuple alone would splice independent
  sequence spaces and corrupt in-flight/RTT/retransmit derivation. We key by
  `ConnId = (4-tuple, instance)`, a new instance opening on a SYN after a prior close/RST or
  after idle past the single configurable idle/dead timeout (shared with the `Tick` machinery,
  [ADR-0002](0002-pure-engine-io-boundary.md)). For SYN-less mid-stream captures, a new instance
  is inferred only from a sequence reset that is **backward in RFC 1982 serial-number space**
  (a drop to a plausible fresh ISN) — never a benign `u32` wrap, which is *forward* under serial
  comparison. Instance inference thus shares the serial arithmetic used for gap detection; a
  naive `u32` "backward jump" test would falsely split a flow on every wrap.
- The wire series is **bytes in flight (outstanding)** = highest seq sent − highest ack seen.
  This is `min(cwnd, rwnd, app-limited)` and equals cwnd *only* for an at-sender, non-rwnd-
  limited, non-app-limited sender. It is labeled "bytes in flight," never "cwnd"; the term
  **cwnd** is reserved for the kernel series. The two are overlaid, never merged (in-flight ≤ cwnd).
- **Capture vantage point is a first-class variable.** `sock_diag`/`TCP_INFO` is always the
  local sender's truth; the wire series depends on where packets were tapped. The wire-vs-kernel
  overlay is well-posed only for at-sender live capture — the one place the kernel series even
  exists. Vantage is recorded and displayed; elsewhere divergence is noise, not signal.
- **The enrichment join is specified, not assumed.** Matching kernel samples to wire
  connections requires: a defined poll cadence; timestamping kernel samples and aligning them
  to the wire timeline; handling a socket that closes between polls (no stale data); and a
  recycled-tuple guard (instance-aware keying) so a new connection never inherits a dead
  socket's `TCP_INFO`. Ephemeral connections that open and close between polls may go
  unenriched — shown as `n/a`, not guessed. Detailed in milestone M12.
- Enrichment is optional and **per-connection**: connections with no local socket (remote
  peer), no `/proc` entry, or absent `sock_diag` render `n/a` in the kernel columns. The
  degraded-state UX is specified in the design §7, not merely asserted here.

## Alternatives considered

- **Kernel/eBPF as the primary model** — rejected: cannot support pcap replay at all,
  which is a hard requirement, and raises the privilege/toolchain floor.
- **Two parallel models (one for live, one for replay)** — rejected: doubles the metric
  code and guarantees behavioral drift between the two modes.
- **Kernel-primary for live, packet-derived for replay, behind a common metric trait** —
  the strongest opposing design: trust the kernel's exact `TCP_INFO` as the primary signal
  when live, fall back to packet derivation only for replay. Rejected because the two
  sources produce subtly different series (the kernel exposes cwnd/srtt the wire cannot, and
  omits the per-packet seq/SACK detail the wire has), so a trait abstracting them would leak
  and the *same connection* would render differently live vs. replayed — reintroducing the
  drift this ADR exists to prevent. Wire-primary keeps one series everywhere; the kernel adds
  an overlay where it exists, rather than swapping the primary signal by mode.
