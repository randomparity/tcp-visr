# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`tcp-visr` is a Rust terminal UI for visualizing **TCP connection dynamics over time**, from
either a live Linux system or a replayed `.pcap`/`.pcapng` capture. Pre-release (v0.0.0),
building toward v0.1 (replay-only). The full design is the source of truth:
[docs/design/tcp-visr-design.md](docs/design/tcp-visr-design.md); cross-cutting decisions are
[ADRs](docs/adr/) and are authoritative when they disagree with the design doc.

**Current state:** milestones M0–M6 are implemented. Working CLI subcommands are `parse`,
`conns`, `metrics`, and `replay` (all replay path only). `replay` opens the interactive TUI
over a capture with a seekable timeline: play/pause, 0.1–10× speed, seek, and step, with the
master list resolving each connection's state and bytes "as of" the cursor time via the
cross-connection interval index (M5). `Enter` opens a per-connection Time/Sequence (Stevens)
detail pane (`Esc` closes it): a cursor-driven seq-vs-time graph with retransmit/SACK marks,
plotted from an engine-unwrapped `i64` sequence offset so multi-GB transfers do not fold (M6).
The remaining detail views — in-flight, RTT, throughput — and the `Tab` view-switcher (M7–M9),
`live`, and kernel enrichment are not built yet; `live` returns "not implemented yet". Do not
assume a feature exists because the design describes it — check the roadmap (design §10) and
the code.

## Commands

```bash
cargo build --workspace
cargo run -p tcp-visr -- --help
cargo run -p tcp-visr -- parse   crates/tcp-visr/tests/fixtures/metrics_basic.pcap
cargo run -p tcp-visr -- conns   crates/tcp-visr/tests/fixtures/metrics_basic.pcap
cargo run -p tcp-visr -- metrics crates/tcp-visr/tests/fixtures/metrics_basic.pcap --conn 0

# The full CI gate — run all of these before pushing (zero warnings is the baseline):
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace
cargo test -p tcpvisr-ingest --features live   # exercises the libpcap faucet + parity test
cargo deny check

# Single test / focused runs:
cargo test -p tcpvisr-engine tracker::           # one module's tests
cargo test -p tcp-visr --test metrics            # one integration-test binary
cargo test -p tcp-visr --test metrics -- seq_wrap  # one test by name filter
```

The `live` feature (libpcap binding) requires `libpcap-dev` system headers. The default build
and the entire replay path are libpcap-free by design — keep it that way (ADR-0003).

**Toolchain is pinned to Rust 1.88.0** (`rust-toolchain.toml`). Dependency versions are pinned
exactly (`=x.y.z`) and audited by `cargo-deny`; when adding a crate, pin it and add its SPDX
license id to `deny.toml`'s allow-list (an unused allow entry warns, which the zero-warnings
policy forbids). Install git hooks with `prek install`.

## Architecture

A Cargo workspace of single-responsibility crates. Dependency direction flows toward
`tcpvisr-core`; the two load-bearing boundaries below are what make the whole thing hang
together, so preserve them.

| crate | responsibility | I/O |
|-------|----------------|-----|
| `tcpvisr-core` | shared types: `FlowKey`, `Item`, `Segment`, `TcpSeq` (RFC 1982 serial arithmetic), `Nanos`, `MetricSample` | none |
| `tcpvisr-ingest` | pcap/pcapng replay (`pcap-parser`) + libpcap live (`live` feature) → `Item` stream; link decode (Ethernet, SLL, SLL2, raw IP, null) | files; libpcap (optional) |
| `tcpvisr-engine` | **pure** TCP state machine + metric derivation → per-connection time-indexed series | **none** |
| `tcpvisr-enrich` | live-only kernel enrichment (`sock_diag`, `/proc`) — stub | netlink, procfs |
| `tcpvisr-tui` | ratatui master/detail UI — stub | terminal |
| `tcp-visr` (bin) | clap CLI, wires faucet → engine → (future) tui | — |

### The two invariants that must not be broken

1. **The `Item = Segment | Tick` boundary.** Both faucets (file and wire) emit the same `Item`
   type, and both hand raw link-layer frames to the *same* `decode_frame` in
   `tcpvisr-ingest/src/decode.rs`. The engine never knows the source. A parity test
   (`tcpvisr-ingest/tests/parity.rs`) feeds one capture through both faucets and asserts
   byte-identical `Item` streams. If you touch decoding, both faucets must stay in lock-step.
   (`etherparse` 0.20.2 does not parse the SLL2 cooked header, so ingest hand-decodes only the
   SLL2 header and hands the IP payload to `etherparse` — see `decode.rs`/`link.rs`.)

2. **The engine is pure — no I/O, no clock reads.** Time advances *as data* via `Tick(ts)`
   items, never by reading a clock. This is why event-driven TCP edge cases are unit-testable
   from a hand-built `Vec<Item>` and time-driven cases (idle, RTO, throughput decay) are driven
   by injecting `Tick`s. Do not add a file handle, socket, or `Instant::now()` to this crate
   (ADR-0002).

### Correctness hot-spot: sequence-number arithmetic

Seq/ack numbers are `u32` and **wrap**. All comparison (in-flight, RTT pairing, gap detection,
new-instance-vs-wrap disambiguation) goes through `TcpSeq` serial-number comparison per RFC 1982
in `tcpvisr-core/src/seq.rs` — **never naive subtraction**. This is the single most error-prone
area and is covered by `proptest`. A `u32` wrap is *forward* in serial space and must never split
a connection; a genuinely new connection instance is a *backward* jump to a fresh ISN.

Connections are keyed by `ConnId = (FlowKey, instance)`, not the bare 4-tuple, because socket
pairs get reused within one capture (see `tracker.rs` and ADR-0006). Wire metrics are **bytes in
flight (outstanding)**, deliberately *never* labeled `cwnd` (which is reserved for the live kernel
series). See design §4.

## Testing conventions

- **Test behavior, not implementation** — assert what the metrics say, not how they are computed.
- **Fixtures are built from source, then committed as bytes.** `tests/support/mod.rs` in the
  `ingest` and `tcp-visr` crates builds real `.pcap`/`.pcapng` bytes with `etherparse`
  (deterministic — no clock, no randomness), so the committed `.pcap` fixtures are reviewable as
  code. A `drift.rs` test asserts the committed fixtures still match the builder output.
- **The oracle goldens are hand-derived, not snapshots.** `crates/tcp-visr/tests/oracle/*.json`
  are computed by hand from RFC 1982 arithmetic (see `tests/oracle/README.md`, which shows the
  full derivation). If code output disagrees with a golden, the derivation is authoritative and
  the code is the suspect. Regenerating a golden means re-deriving the numbers by hand.
- Clippy lints are strict workspace-wide (`unwrap_used`/`panic`/`print_stdout` denied). Tests are
  exempt via `clippy.toml`; in non-`#[test]` test-support modules, scope relaxations with a
  file-level `#![allow(...)]` (item-level `#[allow]` is denied by `allow_attributes`).

## Workflow

- Milestones (M0–M13) map 1:1 to GitHub epic issues; `plan.md` is written just-in-time per
  milestone (design §11–§12). Each milestone is one PR.
- **Never push to `main`** — feature branches and PRs only. Conventional Commits, imperative,
  ≤72-char subject, one logical change per commit.
- **Never `--squash`-merge code PRs** — merge `--rebase` or `--merge` to preserve `git bisect`
  granularity.
- Per-packet problems in a capture are **skipped and counted** (`SkipCounts`), never fatal; only
  whole-file failures are `IngestError` (design §7).
