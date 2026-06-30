# ADR-0003: Live capture uses libpcap; replay parsing is pure-Rust

> Status: Accepted
> Date: 2026-06-30

## Context

Live mode must pull packets off a Linux interface. Replay mode must read `.pcap`/`.pcapng`
files. Capture options on Linux:

- **libpcap** (via the `pcap` crate) — mature, BPF filtering, broad format support;
  requires the `libpcap` system library and `CAP_NET_RAW`.
- **AF_PACKET raw socket** (pure Rust) — no C dependency, fully static binary; but ring
  buffer / BPF handling and edge cases must be implemented by us.
- **eBPF** (e.g. `aya`) — lowest overhead, richest signal; heaviest toolchain and
  kernel-version sensitivity.

File parsing is a separate concern from live capture and need not use libpcap at all.

## Decision

We will use **libpcap (the `pcap` crate) for live capture** in v1. We will parse
`.pcap`/`.pcapng` **files in pure Rust** (`pcap-parser` for the container, `etherparse`
for headers), independent of libpcap.

## Consequences

- Lowest-risk live path: BPF filtering and format handling come for free; well-trodden by
  peer tools.
- The replay path has **no libpcap dependency**, so it builds and runs anywhere and is
  fully testable with committed fixtures — the bulk of v1 (M1–M10) needs no C library.
- Cost: live mode adds a `libpcap-dev` system dependency and a `CAP_NET_RAW` requirement,
  both documented with the `setcap` fix.
- AF_PACKET (static binary) and eBPF (low overhead) remain open as future enhancements
  behind the same ingest faucet interface.

## Alternatives considered

- **AF_PACKET for v1** — rejected for v1: more code and edge cases (ring buffers, our own
  BPF) for no v1-visible benefit; revisit when a fully static binary is a priority.
- **eBPF for v1** — rejected: toolchain and kernel-version complexity is overkill for the
  v1 feature set; revisit if per-packet overhead or kernel-side signal becomes a need.
- **libpcap for file parsing too** — rejected: would impose the libpcap dependency on the
  entire replay path and reduce testability.
