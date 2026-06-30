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
`.pcap`/`.pcapng` **files in pure Rust** (`pcap-parser` for the container), independent of
libpcap.

The two faucets differ **only in the container/source layer**. Both hand raw link-layer
frames to a **single shared decoder** (`etherparse`, plus our own SLL/SLL2 cooked-header
decode since `etherparse` 0.20.2 does not handle SLL2) that produces `Segment`s. There is
no second header-parsing path, so libpcap and `pcap-parser` cannot decode identical bytes
into different segments. Live capture requests **nanosecond timestamp precision**
(`PCAP_TSTAMP_PRECISION_NANO`, falling back to microsecond when the device cannot supply
it); both faucets normalize to one internal time unit so RTT fidelity follows the source
data, not the faucet.

## Consequences

- Lowest-risk live path: BPF filtering and format handling come for free; well-trodden by
  peer tools.
- The replay path has **no libpcap dependency**, so it builds and runs anywhere and is
  fully testable with committed fixtures — the bulk of v1 (M1–M10) needs no C library.
- **One header decoder serves both faucets**, so live and replay produce identical
  `Segment`s for identical bytes. This is enforced by an ingest parity test (one capture
  through both faucets → identical `Item` streams; design §8).
- **v1.0 release binaries are split (M13).** libpcap is a dynamic C library, awkward to
  cross-compile and link statically. So the release ships a **statically-linked,
  dependency-free binary that does replay only**, plus a **libpcap-dynamic binary for
  `live`**. Mechanism: libpcap is an **optional Cargo dependency gated behind a `live`
  feature (default on)**; the static binary is built `--no-default-features`, which
  `#[cfg(feature = "live")]`-excludes both the libpcap link and the `live` subcommand.
  M13 install docs state the per-platform libpcap requirement for live mode. This resolves the
  tension between "ship static binaries" (M13) and the libpcap dependency.
- Cost: live mode adds a `libpcap`/`libpcap-dev` dependency and a `CAP_NET_RAW` requirement.
  An unprivileged open that returns zero packets instead of erroring is detected and surfaced
  as a privilege problem (verified by an M11 test), not shown as an idle network.
- AF_PACKET (fully static live binary) and eBPF (low overhead) remain open as future
  enhancements behind the same ingest faucet interface — and would let `live` join the static
  binary if static distribution becomes a priority.

## Alternatives considered

- **AF_PACKET for v1** — rejected for v1: more code and edge cases (ring buffers, our own
  BPF) for no v1-visible benefit; revisit when a fully static binary is a priority.
- **eBPF for v1** — rejected: toolchain and kernel-version complexity is overkill for the
  v1 feature set; revisit if per-packet overhead or kernel-side signal becomes a need.
- **libpcap for file parsing too** — rejected: would impose the libpcap dependency on the
  entire replay path and reduce testability.
