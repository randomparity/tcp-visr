# M2 — Connection State Machine (Spec)

> Implements: design §10.M2 · Depends-on: ADR-0001, ADR-0002, ADR-0006 ·
> Touches: `area:core` `area:engine` `area:cli` · Release: v0.1 · Type: `type:epic`

## Objective

Build the pure per-connection state machine in `tcpvisr-engine` and expose it as
`tcp-visr conns <file>`, which lists each connection in a capture with its observed state,
per-direction byte counts, and duration. This is the second load-bearing interface: the
engine that turns the M1 `Item` stream into tracked `Connection`s (design §3.2, ADR-0002).
The engine is **pure** — no I/O, no clock, no sockets; it consumes a `Vec<Item>` / a stream
of `&Item` and produces `Connection`s. The CLI streams a capture through the M1 replay
faucet into the engine.

M2 ships connection **tracking and lifecycle state** only. It does **not** ship metric
derivation (in-flight, RTT, throughput, retransmit/OOO/SACK time series) — that is M3 — nor
any TUI.

## Background: the decisions this spec rests on

Direction, orientation, grouping, and instance identity are settled in
[ADR-0006](../../adr/0006-connection-identity-and-direction.md):

- Connections are grouped by a canonicalized, orientation-independent `EndpointPair`.
- Orientation (`origin`/`responder`) comes from a bare SYN when present, else from the first
  observed segment with `origin_inferred = true`.
- **Direction is engine-derived**, not a field on the wire `Segment`; `tcpvisr-core` is
  unchanged except for the additions below.
- Instance identity is `ConnId = (EndpointPair, instance)`; a new instance begins on a
  SYN-after-termination/idle or a backward RFC-1982 sequence reset. A forward `u32` wrap is
  never a new instance.

## In scope

### `tcpvisr-core` — minimal additions

`tcpvisr-core` already carries `FlowKey`, `Segment`, `Item`, `TcpSeq`, `Nanos`, `TcpFlags`.
M2 adds only what is shared model, keeping I/O-free purity:

- **`Endpoint`**: `{ ip: IpAddr, port: u16 }` newtype-style struct with `Display`
  (`addr:port`, `[v6]:port`). Derived from a `FlowKey`'s source/destination halves.

No other core changes. `FlowKey` stays wire-as-seen (ADR-0006). `MetricSample` and the
metric `series` field of design §4's `Connection` are **not** added (M3).

### `tcpvisr-engine` — the connection tracker

A pure tracker with this public surface (exact names may be refined in the plan, behavior is
fixed here):

- **`EngineConfig`**: `{ dead_after: Nanos, reset_threshold: u32 }` with a `Default`
  (`dead_after = 120 s`, `reset_threshold = 2^30`). Constructible so tests can pin small
  timeouts/thresholds. `reset_threshold` is the **minimum backward serial distance** that
  counts as a fresh-ISN reset; `2^30` is the largest representable TCP in-flight window
  (window 65535 × max window-scale 2^14 ≈ 2^30), so a backward jump exceeding it cannot be
  in-flight retransmit/reorder data. It must be `< 2^31` (the serial-space midpoint) to be
  satisfiable at all (see instance disambiguation).
- **`ConnState`** (observed lifecycle, design §10.M2):
  - `SynSent` — a bare SYN (SYN set, ACK clear) from the origin observed; nothing back yet.
  - `SynReceived` — a SYN-ACK observed (or, in a simultaneous open, both sides' bare SYNs).
  - `Established` — handshake-completing ACK or any data observed; **or** a mid-stream
    connection (no SYN seen) on which any segment is observed (inferred established).
  - `FinWait` — exactly one direction's FIN observed; winding down.
  - `Closed` — both directions' FIN observed (graceful full close).
  - `Reset` — an RST observed (terminal; overrides any non-`Reset` state).
- **`ConnId`**: `{ pair: EndpointPair, instance: u32 }`. `EndpointPair` is the canonical
  ordered endpoint pair (ADR-0006); `instance` starts at 0 and increments per pair.
- **`Connection`** (M2 view; design §4's `series`/`labels` deferred to M3/M4):
  - `id: ConnId`
  - `state: ConnState`
  - `origin: Endpoint`, `responder: Endpoint`
  - `origin_inferred: bool` (true when no bare SYN was observed)
  - `opened_at: Nanos` (first observed segment ts for the instance)
  - `last_at: Nanos` (last observed segment ts for the instance)
  - `bytes_o2r: u64`, `bytes_r2o: u64` (**wire** TCP payload bytes per direction —
    `payload_len` summed as observed, retransmissions included; unique/goodput accounting is
    M3)
  - `segments: u64` (count of segments attributed to the instance)
  - `duration(&self) -> Nanos` = `last_at − opened_at` (saturating).
- **`Tracker`**:
  - `Tracker::new(EngineConfig) -> Self`
  - `observe(&mut self, item: &Item)` — folds one `Item` into state. `Segment`s update the
    matching connection (creating/splitting instances per ADR-0006). `Tick`s advance "now"
    for idle bookkeeping but, in replay, are not emitted (design §4.1); a `Tick` never
    creates a connection.
  - `into_connections(self) -> Vec<Connection>` — all tracked connections (every instance),
    ordered deterministically by `(opened_at, pair, instance)`.
- A convenience `track(items, config) -> Vec<Connection>` over an `IntoIterator<Item=&Item>`
  for unit tests.

**State-transition contract** (per instance; RST is a terminal override at any point). A
**bare SYN** is SYN-set/ACK-clear; the two SYN columns distinguish a SYN whose source is the
recorded `origin` (the connection's first SYN sender) from one whose source is the
`responder` side (the *second* SYN of a simultaneous open):

| Current → event | bare SYN (origin side) | bare SYN (responder side) | SYN-ACK | other w/ no handshake seen | ACK completing handshake / data after SynReceived | FIN (1st dir) | FIN (2nd dir) | RST |
|---|---|---|---|---|---|---|---|---|
| (new) | `SynSent` | `SynSent` | `SynReceived` | `Established` (`origin_inferred`) | — | `FinWait`* | — | `Reset` |
| `SynSent` | — (dup) | `SynReceived` (sim-open) | `SynReceived` | — | — | `FinWait` | — | `Reset` |
| `SynReceived` | — (dup) | — (dup) | — (dup) | — | `Established` | `FinWait` | — | `Reset` |
| `Established` | see instance rules† | — (dup) | — (dup) | — | — | `FinWait` | — | `Reset` |
| `FinWait` | see instance rules† | — | — | — | — | — | `Closed` | `Reset` |
| `Closed` | new instance | new instance | — | — | — | — | — | `Reset` |
| `Reset` | new instance | new instance | — | — | — | — | — | — |

\* A FIN as the very first observed segment is a degenerate mid-stream tail: the connection
is created `origin_inferred`, `Established` is implied, and the FIN moves it to `FinWait`.

† On a live (non-terminal, non-idle) instance a bare SYN is an absorbed **duplicate**
(retransmitted SYN): no transition, no new instance. A new instance is opened only per the
instance-disambiguation rules below (terminal/idle, or backward reset).

"— (dup)" cells are absorbed no-ops (duplicate/retransmitted control segment, or a segment
that does not advance the lifecycle). State is **monotonic** along
`SynSent → SynReceived → Established → FinWait → Closed` (an out-of-order or duplicate event
never moves state backward); `Reset` overrides from any non-`Reset` state and is terminal.
The "responder side" of a SYN is determined by the already-recorded `origin` (set by the
first bare SYN); when no `origin` is yet recorded the first bare SYN sets it (the `(new)`
row).

**Instance disambiguation** (ADR-0006), all serial comparisons via `TcpSeq`:

- A bare SYN observed for a pair whose current instance is `Closed`/`Reset`, or has been idle
  (`now − last_at > dead_after`), opens a **new** instance (next `instance`).
- For an `Established` instance with a tracked per-direction sequence baseline, a segment
  whose `seq` is **backward** in RFC 1982 serial space from that baseline (i.e.
  `seq.serial_lt(baseline)`) by **more than `reset_threshold`** opens a new instance (a
  SYN-less fresh start). A forward move (including a `u32` wrap, which reads *forward* under
  RFC 1982 — `serial_gt`) advances the same instance and never splits. Backward moves of
  `≤ reset_threshold` (retransmit/reorder, bounded by the in-flight window) do not split.
  Because any backward serial distance is in `(0, 2^31)`, `reset_threshold` must be `< 2^31`
  for the rule to be reachable; the `2^30` default leaves the band `(2^30, 2^31)` as "reset".
  **Inherent limit:** a fresh ISN that happens to land within `2^31` *forward* of the prior
  sequence is indistinguishable from an advance and will not split — acceptable because the
  authoritative split signal is the SYN (rule 1); the backward-reset rule is the SYN-less
  best effort design §4 mandates, not a guarantee.
- Bytes and `segments` are attributed to the instance a segment resolves to.

### `tcp-visr conns` subcommand

- `tcp-visr conns <FILE>` streams the capture through the **pure-Rust replay faucet**
  (`parse_file_visit`, no libpcap) into a `Tracker`, then prints one line per connection
  followed by a one-line summary, all via `writeln!` to a locked `io::stdout()` (the
  `print_stdout` lint is denied, so macro forms are not used).
- The `Conns` clap variant changes from a unit variant to `Conns { file: PathBuf }`.
- **Per-connection line** (stable, greppable; exact column text fixed by tests):
  `<origin> -> <responder>  state=<STATE>  inst=<n>  bytes=<o2r>/<r2o>  segs=<k>  dur=<duration>`
  plus a trailing ` (mid-stream)` marker when `origin_inferred`.
- **Summary line**: `<N> connections, skipped: <total> total[ (reason=count, …)]`, reusing
  the M1 `SkipCounts` surfacing so non-TCP/malformed/truncated packets are visible.
- Errors propagate as `Result` (no `panic!`/`process::exit`); a missing/unreadable file
  yields the M1 actionable `IngestError` message and a non-zero exit.

### Fixtures

Five committed `.pcap` fixtures, one per DoD scenario, built by the **same code-generated,
drift-guarded builder** approach as M1 (`tests/support`), so each is reviewable as source.
Built with microsecond timestamps, and — unlike M1's single-`TS` fixtures — using **strictly
increasing per-packet timestamps** so `opened_at`, `last_at`, and `duration` are non-zero and
testable (M1's `Pkt::new(ts_us, …)` already takes a per-packet timestamp). Fixtures live in
the crate that owns the end-to-end test (see "Testing"):

1. **`mid_stream.pcap`** — data/ACK segments with no SYN: one connection, `Established`,
   `origin_inferred`.
2. **`sim_open.pcap`** — both endpoints send a bare SYN, then SYN-ACKs/ACKs: one connection
   reaching `Established`, not `origin_inferred`.
3. **`mid_rst.pcap`** — an established exchange then an RST: one connection, terminal `Reset`.
4. **`tuple_reuse.pcap`** — a full open/close (or RST) on a 4-tuple, then a **new SYN** on the
   same 4-tuple: **two** connections (`instance` 0 and 1) for one `EndpointPair`.
5. **`seq_wrap.pcap`** — an established connection whose `seq` advances across the `u32`
   boundary (forward wrap): **one** connection, one instance — the wrap must not split it.

## Testing (design §8: engine is pure unit tests fed hand-built `Vec<Item>`)

- **Engine unit tests** (in `tcpvisr-engine`, hand-built `Vec<Item>`, no files) are the
  primary behavior coverage, one per edge case in design §8 that M2 owns:
  mid-stream (no handshake), simultaneous-open, mid-stream RST, 4-tuple reuse (distinct
  instances), `u32` seq wraparound (single instance), plus: graceful FIN/FIN close to
  `Closed`, RST as terminal override, monotonic state under duplicate/reordered events,
  per-direction byte attribution (with a retransmit, asserting the duplicate payload **is**
  re-counted per the wire-bytes decision), SYN-less mid-stream **backward-reset split**
  (backward distance in `(reset_threshold, 2^31)`) vs. small-backward (retransmit) no-split,
  and idle-`dead_after` reuse. Tests assert **what** the tracker reports
  (state, instance count, bytes, duration), not how it is computed.
- **`proptest`** for the instance-split decision boundary: for any baseline and any forward
  delta in `[1, 2^31)` (including deltas that cross the `u32` boundary), the segment never
  splits; for a backward delta in `(reset_threshold, 2^31)`, it splits. This guards the
  wrap-vs-reset rule directly and pins the satisfiable band.
- **CLI/fixture integration** (`tcp-visr` crate, the five committed fixtures): `tcp-visr
  conns <fixture>` exits 0 and prints the expected connection count, states, instance
  numbers, and the `(mid-stream)` marker where applicable. A missing file exits non-zero with
  the actionable message.
- **Drift guard**: committed fixture bytes match the builder output (regenerate on change),
  matching M1's pattern.

## Out of scope

- Metric derivation: `MetricSample`, in-flight, RTT (Karn), throughput window,
  retransmit/OOO/SACK series (M3).
- The seekable cross-connection interval index and "as of T" resolution (M5).
- TUI / master list / `metrics`, `replay`, `live` subcommand bodies (M4+/M11).
- `Tick` *injection* and live idle/decay (M11); M2 defines `Tick` handling in `observe` for
  forward-compat but replay emits none.
- Enrichment / `KernelInfo` (M12); `Connection.labels`/process attribution (M4/M12).
- Capture-size / retained-series ceiling (M3, where series first occupy RAM — design §7/§14).
- Nanosecond-precision fixtures (M11) and `live` faucet involvement (M2 uses the pure faucet
  only; no parity-test changes).

## Definition of Done

1. `cargo build --workspace` and `cargo build --workspace --features live` both succeed
   (toolchain 1.88.0).
2. `cargo fmt --all --check` clean.
3. `cargo clippy --all-targets --all-features -- -D warnings` clean.
4. `cargo test --workspace` passes (engine unit + proptests, core, ingest, CLI/fixture
   tests).
5. `cargo test -p tcpvisr-ingest --features live` still passes (M1 parity unaffected).
6. `cargo deny check` passes (no new runtime deps expected; `proptest` is dev-only).
7. `tcp-visr conns <fixture>` lists connections with state, per-direction bytes, and duration
   for every DoD fixture; exits 0 on a valid capture and non-zero with an actionable message
   on a missing/unreadable file.
8. The five DoD fixtures exist, the drift guard passes, and the engine + CLI tests assert the
   expected outcomes for each (mid-stream, simultaneous-open, mid-stream RST, 4-tuple reuse =
   two instances, seq-wrap = one instance).

## Task breakdown (→ sub-issues)

- **Task 1 — core `Endpoint`** (`area:core`): `Endpoint { ip, port }` + `Display`, derivable
  from `FlowKey` halves. Pure type; unit-tested. No other core change.
- **Task 2 — engine tracker** (`area:engine`): `EngineConfig`, `ConnState`, `EndpointPair`,
  `ConnId`, `Connection`, `Tracker` (`observe`/`into_connections`), the internal `Direction`
  and per-direction sequence baselines, the state-transition table, and instance
  disambiguation (SYN-after-terminal/idle, backward-reset, forward-wrap). Depends on Task 1.
  Hand-built `Vec<Item>` unit tests + `proptest` per design §8.
- **Task 3 — fixtures + drift guard** (`area:engine`/`area:cli`): the five committed `.pcap`
  fixtures via the code-generated builder, drift-guard test. Reuses M1's builder helpers.
  Depends on Task 2 (so the expected outcomes are known) but the builder itself is
  independent.
- **Task 4 — `conns` CLI** (`area:cli`): wire `conns` to `parse_file_visit` → `Tracker` →
  printed lines + summary, lint-safe stdout; clap variant change; CLI/fixture integration
  tests. Depends on Tasks 2 and 3.

## Decisions & assumptions

- **Direction/orientation/instance identity per [ADR-0006]** — engine-derived, `Segment`
  unchanged, RFC-1982 wrap-vs-reset rule.
- **Observed-state model is passive-observer, not endpoint TCP states.** A wire observer sees
  both directions; the six `ConnState`s above are the lifecycle points observable from the
  wire, deliberately coarser than RFC 793's eleven endpoint states (no `TIME_WAIT`/`LAST_ACK`
  distinction — unobservable and unneeded for M2's DoD). Stated so the challenge review does
  not read missing endpoint states as a gap.
- **`dead_after = 120 s`, `reset_threshold = 2^30` defaults**, both configurable
  (`EngineConfig`). `reset_threshold` is the **minimum** backward serial distance that counts
  as a fresh-ISN reset and **must be `< 2^31`** (no backward distance can exceed the
  serial-space midpoint, so a midpoint threshold would make the rule unreachable); `2^30`
  is the largest representable TCP window with scaling, above which a backward jump cannot be
  in-flight data. The dead timeout is a conservative tcptrace-style default; it only matters
  for SYN-after-idle reuse.
- **Bytes are *wire* TCP payload bytes per direction** (each segment's `payload_len`
  summed as observed, **including retransmissions**), a connection-level scalar summary, not
  a time series (that is M3). SYN/FIN phantom sequence bytes are **not** counted as payload;
  unique-bytes/goodput-vs-retransmitted accounting is deferred to M3.
- **A lone RST (or any single segment) creates a tracked connection.** The first observed
  segment of any kind for a pair opens an instance; a contextless RST therefore yields a
  one-segment `Reset` connection. This favors *visibility* (the analyzer shows RST activity)
  over noise suppression; a capture with many unsolicited RSTs (scan/backscatter) will list
  one connection each. Bulk-RST/scan suppression is a later-milestone heuristic, deliberately
  out of M2 scope — recorded here, not silently dropped.
- **Engine takes `&Item` by reference and is fed by the streaming faucet**, so `conns` holds
  only the current frame plus the connection table (bounded by connection count, not capture
  size) — consistent with M1's streaming `parse` and deferring the series ceiling to M3.
- **Fixtures and end-to-end test live in the `tcp-visr` bin crate** (alongside `tests/cli.rs`)
  so the integration test drives the real `conns` command; the builder helpers are shared
  with / mirrored from M1's `tests/support`. (Plan picks the exact location to avoid
  duplicating the builder.)
- **No new runtime dependencies.** Only `proptest` (dev) is added to `tcpvisr-engine`,
  matching `tcpvisr-core`. Versions pinned with `=` per repo policy.

## Acceptance verification commands

```bash
cargo build --workspace
cargo build --workspace --features live
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace
cargo test -p tcpvisr-ingest --features live
cargo deny check
cargo run -p tcp-visr -- conns crates/tcp-visr/tests/fixtures/tuple_reuse.pcap
```
