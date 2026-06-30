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
matched to connections by 5-tuple and overlaid on the wire-derived series. It is never
required for the core experience and is simply absent during replay.

## Consequences

- Live and replay share one engine and one set of metric types; the engine is unaware of
  the source. This is the basis for [ADR-0002](0002-pure-engine-io-boundary.md).
- Wire-derived cwnd is an *estimate* (in-flight = highest seq sent − highest ack seen).
  It must be labeled and rendered as distinct from the kernel's real cwnd, never merged.
- Showing both series together is a feature: divergence indicates middleboxes, offload
  (GRO/TSO), or measurement vantage effects.
- Enrichment being optional means graceful degradation (no root, no procfs, remote
  peers) instead of failure.

## Alternatives considered

- **Kernel/eBPF as the primary model** — rejected: cannot support pcap replay at all,
  which is a hard requirement, and raises the privilege/toolchain floor.
- **Two parallel models (one for live, one for replay)** — rejected: doubles the metric
  code and guarantees behavioral drift between the two modes.
