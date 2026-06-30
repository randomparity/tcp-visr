# ADR-0005: libpcap enters at M1 as a default-off file faucet for the parity test

> Status: Accepted
> Date: 2026-06-30
> Amends: [ADR-0003](0003-libpcap-for-live-capture.md) (scopes *when* libpcap enters and the
> `live` feature default; does not change the libpcap-for-live / pure-Rust-for-replay decision)

## Context

The design §10 M1 Definition of Done requires *"both faucets pass the parity test (§8)."* The
parity test (design §8, §3.2) feeds one capture through both ingest faucets — the pure-Rust
`pcap-parser` container path and the **libpcap** path — and asserts identical `Item` streams,
guarding the "one decoder, identical behavior" promise.

[ADR-0003](0003-libpcap-for-live-capture.md) places libpcap in milestone **M11** ("Live
capture") and states *"the replay path has no libpcap dependency… the bulk of v1 (M1–M10)
needs no C library."* It also says the `live` Cargo feature is *"default on"* (a statement
made in the context of the M13 release-binary split).

So two facts in the source docs are in tension at M1:
1. M1's DoD needs the libpcap faucet (to have a second faucet to compare against).
2. ADR-0003 says M1–M10 needs no C library, and `live` is default-on.

The design doc's own rule (preamble) is that the ADR is authoritative on conflict. Taken
literally that would *defer* the parity test to M11. But libpcap can read capture **files**
(`pcap::Capture::from_file`), not only live interfaces — so the parity test is achievable at
M1 without any live-capture machinery, and exercising the shared decoder through both
container parsers this early is high-value (it catches decode drift before three milestones of
engine work build on top of the `Item` stream).

## Decision

Introduce libpcap at **M1**, but only as a **file-reading faucet** and only behind an
**optional `live` Cargo feature that is default-off**:

- The `pcap` crate is an optional dependency of `tcpvisr-ingest`, gated `#[cfg(feature =
  "live")]`. At M1 it is used solely via `Capture::from_file` to drive the parity test.
- The feature is **default-off** at M1. `cargo build/test --workspace` (default features) is
  libpcap-free, preserving ADR-0003's property that the replay path builds anywhere and the
  static replay-only binary needs no C library.
- The cross-faucet parity test is `#[cfg(feature = "live")]` and runs in CI under a step that
  installs `libpcap-dev` and invokes `cargo test -p tcpvisr-ingest --features live`. The
  default `cargo test --workspace` job remains C-free.
- Both faucets call the **same** `decode_frame` (design §3.2); the libpcap faucet adds only a
  container/source layer, never a second header decoder.

Live *interface* capture (`Capture::from_device`, `Tick` injection, ring buffer, privilege
handling) remains **M11**. Flipping `live` to **default-on** and the static/dynamic release
binary split remain **M13**, per ADR-0003.

## Consequences

- M1's DoD is satisfied literally: both faucets exist and the parity test runs.
- The default developer/build experience stays libpcap-free; only the explicit
  `--features live` path (and the one CI job that runs it) requires `libpcap-dev`. This keeps
  ADR-0003's "M1–M10 needs no C library" true for the *default* build, which is the property
  that statement protects.
- `clippy --all-targets --all-features` now compiles the `live` feature, so any CI/local
  invocation of `--all-features` requires `libpcap-dev` present. CI installs it in the lint/test
  job; this is documented in the M1 spec.
- One more knob to keep honest: the `live` feature must stay genuinely optional (no
  non-test/default code path depends on it) until M11. A `--no-default-features`-style
  replay-only build must keep compiling.
- ADR-0003's `live`-default-on statement is re-scoped to M13 (release engineering), not M1.
  This ADR records that narrowing explicitly so the docs are consistent rather than silently
  contradictory.

## Alternatives considered

- **Defer the parity test to M11** (strict reading of "ADR-0003 is authoritative") — rejected:
  it leaves the shared-decoder contract unguarded through M1–M10, exactly the window where
  engine/metric code calcifies around the `Item` stream; and libpcap-file-reading makes the
  test achievable now with no live-capture risk.
- **Introduce libpcap default-on at M1** (literal ADR-0003 default) — rejected for M1: it
  forces `libpcap-dev` onto every default build and CI job and breaks the "replay builds
  anywhere" property three milestones before live capture needs it. Default-off defers that
  cost to when live capture actually lands.
- **A decoder-level determinism test instead of cross-faucet parity** — rejected: it proves
  the decoder is deterministic but not that the two *container* parsers agree, which is the
  specific drift the parity test exists to catch.
