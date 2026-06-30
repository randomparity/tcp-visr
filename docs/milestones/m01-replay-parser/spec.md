# M1 — Packet Model & Replay Parser (Spec)

> Implements: design §10.M1 · Depends-on: ADR-0001, ADR-0003, ADR-0005 ·
> Touches: `area:core` `area:ingest` `area:cli` `area:ci` · Release: v0.1 · Type: `type:epic`

## Objective

Stand up the wire-derived packet model in `tcpvisr-core` and the replay parser in
`tcpvisr-ingest`, so that `tcp-visr parse <file>` decodes a `.pcap`/`.pcapng` capture into
TCP `Segment`s and prints them. This is the first load-bearing interface of the system: the
`Item` (`Segment | Tick`) stream that every later milestone consumes (design §3.2). M1 also
delivers the **shared header decoder** used by both faucets and the **cross-faucet parity
test** that guards the "one engine, identical behavior" promise (design §8).

M1 ships no connection tracking, no metric derivation, and no TUI — those are M2/M3/M4+.

## Background: the doc conflict this spec resolves

The design §10 M1 row lists *"both faucets pass the parity test (§8)"* in the Definition of
Done. The parity test compares the pure-Rust `pcap-parser` faucet against the **libpcap**
faucet — but ADR-0003 states libpcap is the M11 milestone and *"the bulk of v1 (M1–M10)
needs no C library."* These disagree about whether libpcap enters at M1.

[ADR-0005](../../adr/0005-libpcap-file-faucet-at-m1.md) resolves it: M1 introduces the
libpcap faucet **for file reading only** (`pcap::Capture::from_file`), gated behind an
**optional `live` Cargo feature that is default-off**. The default workspace build/test
stays libpcap-free (preserving ADR-0003's static replay-only property); the parity test is
gated behind `--features live` and exercised by a dedicated CI job that installs
`libpcap-dev`. Live *interface* capture and flipping the feature default-on remain M11/M13.

## In scope

### `tcpvisr-core` — wire packet model

- **Time**: `Nanos(u64)` newtype — nanoseconds since the **baseline**: the timestamp of the
  **first packet record in file order** (whether or not that packet decodes to a `Segment`),
  computed with `saturating_sub`. Both faucets MUST use this same baseline rule or per-packet
  times diverge. Wall-clock is not assumed monotonic (design §14): a packet earlier than the
  baseline clamps to `0` (`saturating_sub`); M1's `parse` therefore prints in file order, not
  sorted time order, and this clamp is documented in the parse output's behavior.
- **Serial-number arithmetic** (design §4, "the single most error-prone area"):
  `TcpSeq(u32)` with RFC 1982 serial comparison (`serial_lt`/`serial_gt`) and forward
  distance (`serial_diff`). `proptest`-covered.
- **`FlowKey`**: `{ src_ip, src_port, dst_ip, dst_port }` (`core::net::IpAddr` + `u16`),
  stored as-seen on the wire. Connection-relative canonicalization/direction is M2.
- **`TcpFlags`**: newtype over the TCP flag bits with boolean accessors (no new dependency).
- **`TcpOptions`**: parsed summary — `mss: Option<u16>`, `window_scale: Option<u8>`,
  `sack_permitted: bool`, `timestamp: Option<(u32, u32)>` (TSval, TSecr),
  `sack_blocks: Vec<(TcpSeq, TcpSeq)>`.
- **`Segment`**: `{ ts: Nanos, flow: FlowKey, seq: TcpSeq, ack: TcpSeq, flags: TcpFlags,
  window: u16, options: TcpOptions, payload_len: u32 }`. The `direction` field of the design
  §4 model is **deferred to M2** (it is connection-relative and requires connection grouping).
- **`Item`**: `enum Item { Segment(Segment), Tick(Nanos) }` — the engine-input boundary
  (design §3.2). Replay emits only `Segment`; `Tick` is defined now because it is part of the
  documented contract the parity test asserts on, and is produced (live) in M11.

`ConnId`, `MetricSample`, and `Connection` are **not** added in M1 (M2/M3).

### `tcpvisr-ingest` — replay parser

- **Link types** (design §3.1, §8): Ethernet II (`DLT_EN10MB`), Linux SLL v1
  (`DLT_LINUX_SLL`), Linux SLL2 (`DLT_LINUX_SLL2`), raw IP (`DLT_RAW`), and BSD/NULL loopback
  (`DLT_NULL`). `etherparse` handles Ethernet II and SLL v1 natively; **SLL2 is hand-decoded**
  (the 20-byte cooked header) per design §3.2, then the IP payload is handed to `etherparse`.
  Raw IP (`DLT_RAW`) and NULL loopback (`DLT_NULL`) strip their link layer (0 and 4 bytes
  respectively) and **dispatch on the IP-version nibble of the payload (4 or 6), not on the
  `DLT_NULL` address-family word** — the AF word is the capture host's byte order and its
  `AF_INET6` value is OS-dependent (Linux 10, macOS 30, FreeBSD 28), so it is unreliable for a
  foreign capture; the IP-version nibble is unambiguous. A nibble that is neither 4 nor 6 is
  skipped-and-counted as `Malformed`.
- **IPv6 extension-header chains**: hop-by-hop, routing, destination-options, and fragment
  headers are walked (via `etherparse`) to reach the TCP header. An **IPv6-fragmented TCP
  segment** (a fragment header indicating this is not a complete datagram) and any
  **unsupported/abnormal chain** are *skipped-and-counted*, never mis-parsed (design §2, §7).
- **Shared decoder**: a single `decode_frame(link_type, ts, frame_bytes) -> DecodeOutcome`
  function (`Segment` or `Skipped{reason}`). Both faucets call it; there is no second
  header-parsing path (design §3.2, ADR-0003).
- **Pure-Rust faucet** (always available): `pcap-parser` reads the `.pcap`/`.pcapng`
  container and **streams** `(link_type, ts, frame)` records into `decode_frame` (a visitor /
  iterator, so `parse` holds only the current frame, not the whole capture — see resource
  note below). A collect helper returns `(Vec<Item>, SkipCounts)` for tests and the parity
  test, which operate on bounded fixtures.
- **libpcap faucet** (`#[cfg(feature = "live")]`): `pcap::Capture::from_file` yields the same
  records into the **same** `decode_frame`. File reading only; live capture is M11.
- **Single-interface assumption.** M1 supports captures with one link type. A legacy `.pcap`
  has exactly one. A `.pcapng` with multiple Interface Description Blocks of **differing** link
  types is rejected with `IngestError` (libpcap's `from_file` exposes only one `datalink()`,
  so the two faucets could not agree on a per-packet link type — keeping them in lockstep is
  why this is an error, not a silent partial parse). Multiple IDBs that all share one link type
  are accepted.
- **Timestamp-precision contract (parity-critical).** The two container readers can expose
  different precision (`pcap-parser` surfaces the file's native precision — microsecond vs
  nanosecond legacy-`pcap` magic, or pcapng `if_tsresol`; `pcap::Capture::from_file` defaults
  to microsecond). To keep the parity test falsifiable: **M1 fixtures use microsecond
  precision**, the drift-guard test asserts that precision, and `Nanos` is computed as
  `micros · 1000` from both faucets so they agree exactly. (Nanosecond-precision capture
  support is deferred to M11 with the live faucet, where the libpcap handle is opened
  `Precision::Nano`.)
- **Skip-and-count**: parsing returns the decoded items plus a `SkipCounts` keyed by
  reason (`NonTcp`, `Malformed`, `UnsupportedLinkType`, `Ipv6Fragment`,
  `UnsupportedExtChain`, `Truncated`). Malformed/unsupported packets never abort the parse
  (design §7); a truncated capture renders what parsed. **`Truncated` is detected uniformly
  from the container** (a record whose captured length `incl_len` is less than its original
  length `orig_len`), before `decode_frame` runs, so both faucets classify it identically.
- **Errors**: `thiserror`-based `IngestError` for whole-file failures (file open, unreadable
  container, unknown link type for the *whole* file), with actionable messages (operation,
  input, suggested fix). Per-packet problems are counts, not errors.

### `tcp-visr parse` subcommand

- `tcp-visr parse <FILE>` uses the **pure-Rust faucet** (no libpcap needed) to decode and
  print one line per TCP segment (relative ts, `src→dst`, flags, seq, ack, window, payload
  len) followed by a one-line skip summary. Output is written via `writeln!` to a locked
  `io::stdout()` handle — the `print_stdout`/`println!` lint is denied, so the macro forms are
  not used. Errors propagate as `Result` (no `panic!`/`process::exit`).
- The existing `Parse` clap variant changes from a unit variant to `Parse { file: PathBuf }`.

### Fixtures & parity

- One committed capture fixture **per link type**: Ethernet II, SLL v1, SLL2, raw IP, NULL
  loopback, plus an **IPv6 extension-header chain** fixture, plus at least one **`.pcapng`**
  fixture (to exercise both container formats), plus a **skip-and-count** fixture (a non-TCP
  packet and a malformed/truncated packet).
- Fixtures are produced by a committed pure-Rust **builder** (`tests/support`) that emits the
  exact pcap/pcapng bytes, so every fixture is reviewable as source, not an opaque blob. The
  generated bytes are written to committed `tests/fixtures/*.pcap[ng]` files; a **drift-guard
  test** asserts the committed files byte-match the builder output (regenerate on change).
- **Parity test** (`#[cfg(feature = "live")]`): each **well-formed** fixture (every link-type
  fixture, including the IPv6 ext-header and `.pcapng` ones) is fed through both faucets and
  the resulting `Vec<Item>` and `SkipCounts` must be identical (`Item` derives `PartialEq` over
  all fields). Guards design §3.2. The **truncated/snaplen** fixture is exercised for skip
  classification **per faucet** (each must report it `Truncated`) but is excluded from the
  byte-for-byte cross-faucet equality set, because the two readers' presentation of a
  short-captured record can legitimately differ below `decode_frame`; uniform pre-decode
  `Truncated` detection (above) is what keeps the *classification* in agreement.

### CI

- The `test` job installs `libpcap-dev` (required because `clippy --all-features` compiles the
  `live` feature's `pcap` dependency), runs the existing fmt/clippy/test steps, and adds
  `cargo test -p tcpvisr-ingest --features live` so the parity test is enforced.
- `deny.toml` license allow-list is extended for the new dependencies' SPDX IDs.

## Out of scope

- Connection tracking / `ConnId` / instance disambiguation (M2).
- Metric derivation, `MetricSample`, RTT/in-flight/throughput (M3).
- TUI, `conns`, `metrics`, `replay`, `live` subcommand bodies (M2+/M4+).
- Live *interface* capture, `Tick` injection, ring buffer (M11).
- Flipping the `live` feature default-on and release binary split (M11/M13).
- VLAN (802.1Q) decode beyond what `etherparse` does transparently; PPP/other link types.

## Definition of Done

1. `cargo build --workspace` and `cargo build --workspace --features live` both succeed on
   the pinned toolchain (1.88.0).
2. `cargo fmt --all --check` clean.
3. `cargo clippy --all-targets --all-features -- -D warnings` clean.
4. `cargo test --workspace` passes (decoder + core + CLI tests, libpcap-free).
5. `cargo test -p tcpvisr-ingest --features live` passes (the cross-faucet parity test).
6. `cargo deny check` passes (advisories, bans, licenses, sources) with the new deps.
7. `tcp-visr parse <fixture>` prints decoded TCP segments for every link-type fixture and a
   skip summary; exits 0 on a valid capture, non-zero with an actionable message on a missing
   or unreadable file.
8. A fixture exists per link type incl. SLL2 and an IPv6 extension-header chain; the
   drift-guard test passes.

## Task breakdown (→ sub-issues)

- **Task 1 — core packet model** (`area:core`): `Nanos`, `TcpSeq` + RFC 1982 serial
  arithmetic (proptest), `FlowKey`, `TcpFlags`, `TcpOptions`, `Segment`, `Item`. Pure types,
  no I/O. Independently testable by unit + property tests.
- **Task 2 — shared decoder + link layers** (`area:ingest`): `decode_frame`, the per-link-type
  strip (Ethernet II / SLL v1 / SLL2 hand-decode / raw IP / NULL loopback), IPv6 ext-chain
  walk, TCP option parse, `SkipCounts`, `IngestError`. Depends on Task 1. Tested with the
  fixture builder fed as bytes.
- **Task 3 — fixtures + both faucets + parity** (`area:ingest`): fixture builder, committed
  fixtures, drift-guard test, pure-Rust `parse_file`, libpcap `parse_file_libpcap` (`live`),
  parity test. Depends on Task 2.
- **Task 4 — `parse` CLI + CI** (`area:cli` `area:ci`): wire `parse` to the pure-Rust faucet
  with lint-safe stdout writing; CI `libpcap-dev` install + `--features live` test step;
  `deny.toml` license updates. Depends on Task 3.

## Decisions & assumptions

- **libpcap at M1, default-off** — [ADR-0005]. Resolves the design/ADR-0003 conflict; the
  default build stays C-free, parity runs under `--features live`.
- **`direction` deferred to M2** — it is connection-relative; M1's `Segment` carries the
  wire-as-seen `FlowKey` only.
- **Fixtures are code-generated and committed, with a drift guard** — transparent (readable
  builder) and literal to the DoD ("committed fixtures"), and libpcap needs real files.
- **NULL loopback (`DLT_NULL`) is the loopback variant** decoded (host-endian AF word); BSD
  `DLT_LOOP` (big-endian) is out of scope unless a fixture needs it.
- **`parse` streams; retained-series ceiling deferred.** M1's `parse` decodes and prints
  per-frame, holding only the current frame (constant memory beyond it), so a large capture
  does not OOM the CLI. The full in-memory `Vec<Item>` is built only by tests/the parity test
  over bounded fixtures. The design §7/§14 capture-size ceiling governs **retained
  per-connection series**, which first live in RAM when ADR-0004's precomputed series are
  built (M3 derivation / M5 timeline); it is **deferred to M3** and recorded here and in the
  PR as a known M1 limitation, not silently dropped.
- **Single-interface captures only at M1** — multi-link-type `.pcapng` is an `IngestError`
  (see ingest section); multi-interface support tracks the live faucet work (M11).
- **Pinned versions** (`=`, verified 2026-06-30): `pcap-parser` 0.17.0, `etherparse` 0.20.2,
  `pcap` 2.4.0, `thiserror` 2.0.18, `proptest` 1.11.0.

## Acceptance verification commands

```bash
cargo build --workspace
cargo build --workspace --features live
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace
cargo test -p tcpvisr-ingest --features live
cargo deny check
cargo run -p tcp-visr -- parse crates/tcpvisr-ingest/tests/fixtures/ethernet.pcap
```
