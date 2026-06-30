# M3 — Metric Derivation (Spec)

> Implements: design §10.M3 · Depends-on: ADR-0001, ADR-0002, ADR-0004, ADR-0006, ADR-0007 ·
> Touches: `area:core` `area:engine` `area:cli` · Release: v0.1 · Type: `type:epic`

## Objective

Derive the per-connection **metric time series** from the M2 connection tracker and expose it
as `tcp-visr metrics <FILE> --conn N`, which dumps one connection's series as JSON. Each
series carries, per the design data model (§4):

- **bytes in flight (outstanding)** — `highest_seq_sent − highest_ack_seen`, per direction,
  via RFC 1982 serial arithmetic (never naive `u32` subtraction);
- **throughput** — a trailing sliding-window wire bytes/sec, frozen into each sample at
  derivation time (§4.1, ADR-0004);
- **retransmit / out-of-order / SACK** — per-segment classification of the wire;
- **RTT** — round-trip samples paired under **Karn's algorithm** (no sample from a
  retransmitted segment).

This is the third load-bearing computation: the engine that turns M2's tracked
`Connection`s into the precomputed series the timeline (M5) and detail views (M6–M9) read
without re-parsing (§5). The engine stays **pure** — no I/O, no clock, no sockets
([ADR-0002](../../adr/0002-pure-engine-io-boundary.md)); time is data. The CLI streams a
capture through the M1 replay faucet into the engine and serializes the selected series.

M3 ships **replay-only, per-event metric derivation** and the **JSON dump**. It does **not**
ship the TUI, the cross-connection timeline index, goodput-vs-retransmitted separation, or any
live/`Tick`-driven decay (see Out of scope).

## Background: the decisions this spec rests on

- **Direction, orientation, grouping, instance identity** are settled in
  [ADR-0006](../../adr/0006-connection-identity-and-direction.md) and implemented by M2's
  `Tracker`. M3 reuses that machinery unchanged: metric derivation runs **per connection
  instance, per direction**, keyed by the same `EndpointPair`/`ConnId` and the engine-derived
  `Direction`.
- **Per-event sampling and frozen throughput** are settled in design §4.1 and
  [ADR-0004](../../adr/0004-seekable-timeseries-timeline.md): one `MetricSample` per processed
  `Segment`; the time index is irregular; "state as of `T`" is the last sample at or before
  `T` (last-value-carried-forward, not interpolation); throughput is a trailing window
  **frozen at derivation time** because seeking never re-parses.
- **Serial arithmetic** is `tcpvisr_core::TcpSeq` (RFC 1982), already proptest-covered. Every
  in-flight, RTT-pairing, retransmit/OOO, and frontier comparison routes through it. A forward
  `u32` wrap is an *advance* (`serial_gt`); naive subtraction is forbidden (design §4).
- **The metric-series layering, the in-flight/RTT phantom-byte accounting, the
  retransmit-vs-OOO discriminator, the Karn rule, and the capture-size ceiling** are settled
  in [ADR-0007](../../adr/0007-metric-derivation-model.md); this spec implements them.

## In scope

### `tcpvisr-core` — the sample model (pure, dependency-free)

`tcpvisr-core` stays I/O-free and **dependency-free** (no serde; JSON lives in the CLI — see
ADR-0007). M3 adds only shared model:

- **`SampleDir`**: `enum { OriginToResponder, ResponderToOrigin }` — the direction of the
  segment that produced a sample, relative to the connection's `origin` (ADR-0006). `Copy`.
- **`MetricSample`** (design §4, realized): a `Copy` struct
  ```
  MetricSample {
      t: Nanos,                 // the triggering segment's capture timestamp
      dir: SampleDir,           // direction of the triggering segment
      in_flight_bytes: u64,     // outstanding bytes in `dir` (highest_seq_sent − highest_ack_seen)
      throughput_bps: u64,      // trailing-window wire throughput of `dir` (bits/sec)
      rtt: Option<Nanos>,       // round-trip sample completed by this segment's ACK (Karn-valid)
      retransmit: bool,         // this `dir` data segment re-covers already-seen sequence space
      out_of_order: bool,       // behind-frontier but within the reorder window (reordering, not loss)
      sack: bool,               // the segment carried ≥1 SACK block
  }
  ```
  All directional fields (`in_flight_bytes`, `throughput_bps`, `retransmit`, `out_of_order`)
  pertain to `dir`. `rtt` is a **round-trip** measurement (both directions) attached to the
  sample of the acknowledging segment; `sack` reflects the triggering segment's own options.
  `retransmit` and `out_of_order` are mutually exclusive (a behind-frontier data segment is one
  or the other, never both).

No other core change. `Segment`, `FlowKey`, `Endpoint`, `TcpSeq`, `Nanos`, `TcpOptions`
(already carrying `sack_blocks`) are unchanged.

### `tcpvisr-engine` — metric derivation on top of the M2 tracker

The metric series is the M3 realization of design §4's `Connection.series`. To keep M2's
lifecycle view cheap (the `conns` command needs only scalars) and the metric view rich, the
series is **not** added to M2's `Copy` `Connection`; it is delivered alongside it
(ADR-0007):

- **`ConnectionMetrics`**: `{ conn: Connection, series: Vec<MetricSample> }` — the M2
  `Connection` (lifecycle/scalar view, unchanged) bundled with its derived series.
- **`EngineConfig`** gains the knobs below (all with `Default`, all configurable so tests can
  pin small values):
  - `series_collection: SeriesCollection` (**default `None`**) — an enum controlling which
    instances buffer a series: `None` (the `conns` path — derive only M2 lifecycle/scalar
    state, store **no** samples, zero added cost), `All` (every instance buffers), or
    `Only(ConnId)` (only the named instance buffers). The `metrics` command uses `Only(target)`
    so a large multi-connection capture never builds — or blows the ceiling on — series the
    user did not ask for (see the subcommand's two-pass below). Metric *state* the buffering
    instance needs is advanced only when that instance is collected.
  - `throughput_window: Nanos` (**default 1 s** = `1_000_000_000`) — the trailing window
    length for `throughput_bps` (§4.1).
  - `reorder_window: Nanos` (**default 3 ms** = `3_000_000`) — a behind-frontier data segment
    whose inter-arrival gap from the previous same-direction segment is **below** this window
    is classified **out-of-order** (reordering); at or above it, **retransmit** (loss). This
    mirrors Wireshark's reordering heuristic (ADR-0007) and makes the gated external
    cross-check meaningful.
  - `max_samples: usize` (**default `10_000_000`**) — retained-sample ceiling across the
    **collected** series (one connection under `metrics`'s `Only`, all under `All`), a coarse
    OOM guard (§7). A non-collected instance contributes nothing to it, so `metrics --conn N`
    is bounded by connection `N`'s own series, never by unrelated flows.
- **`Tracker`** (M2) gains, internal to each tracked instance, the per-direction derivation
  state and (when collecting) the series buffer. Public surface added:
  - `into_metrics(self) -> Result<Vec<ConnectionMetrics>, MetricError>` — finalizes and returns
    every tracked instance with its series, ordered by the same deterministic
    `(opened_at, pair, instance)` key as `into_connections`. Returns
    `Err(MetricError::SampleCeiling { … })` if derivation hit `max_samples`. `observe` and
    `into_connections` are **unchanged** (M2 API and tests are untouched).
- **`MetricError`** (`thiserror`, already a workspace dep via ingest — but engine has no deps
  yet; see Decisions): `SampleCeiling { samples: usize, limit: usize }` with an actionable
  `Display` naming the count, the limit, and the `--max-samples` override.

#### Derivation contract (per connection instance)

All sequence comparisons use `TcpSeq` (RFC 1982). Two **separate** sequence accumulators per
direction `d` (ADR-0007):

- **byte counters** (`bytes_o2r`/`bytes_r2o`, M2): wire **payload** bytes only; SYN/FIN phantom
  bytes are **not** counted. Unchanged.
- **sequence frontier** (M3, for in-flight/RTT/retransmit): consumes one phantom sequence byte
  for SYN and for FIN, because they advance `seq` on the wire. `seq_end(S) = S.seq +
  payload_len + (SYN?1:0) + (FIN?1:0)`.

State per direction `d` (lazily initialized on the first segment seen in `d`):

- `snd_nxt[d]`: `Option<TcpSeq>`, serial-max of `seq_end(S)` over segments in `d` — the highest
  sequence the `d` sender has put on the wire. `None` until the first segment in `d` is observed.
- `acked[d]`: `Option<TcpSeq>`, serial-max of the ACK field carried by segments in the
  **opposite** direction (an ACK in `¬d` acknowledges data sent in `d`). Initialized to the
  **first observed `seq`** in `d` (so before any acknowledgement, in-flight measures bytes put
  on the wire since the capture began — mid-stream has no ISN to anchor to). It can also be
  initialized by the first acknowledgement that arrives once `d` has a tracked send (below);
  before `d` has any tracked send it stays `None`.
- `frontier[d]`: serial-max of `seq_end(S)` over **data** segments (`payload_len > 0`) seen so
  far in `d`, evaluated **before** incorporating the current segment — the retransmit/OOO
  reference.
- `last_data_ts[d]`: capture ts of the previous data segment in `d`, for the reorder-window gap.
- `pending_rtt[d]`: FIFO of `(seq_end, send_ts)` for **RTT-eligible** (new-data, non-retransmit)
  sends in `d`, awaiting acknowledgement.

On observing segment `S` in direction `d` (with the metric state above advanced **after**
reading the references it needs), produce exactly one `MetricSample`:

0. **ACK advance, computed once.** Before any state mutation, compute the single predicate
   `ack_advances = S.flags.ack() && snd_nxt[¬d].is_some()
   && (acked[¬d].is_none() || S.ack.serial_gt(acked[¬d]))` against the **pre-update** state. An
   ACK can advance (and yield RTT) only once the opposite direction has a **tracked send** to
   acknowledge (`snd_nxt[¬d].is_some()`); an ACK observed before any data in `¬d` acknowledges
   nothing we track, so `ack_advances = false` and `acked[¬d]` stays `None`. This is the case
   the flagship `seq_wrap` derivation hits at sample 1 (an o2r `ACK=1` while `r2o` has no send
   yet). Otherwise `ack_advances` is true on the first real ACK (initializing `acked[¬d]`) and on
   any later ACK that serial-advances it; a duplicate/old ACK is `false`. Both the in-flight
   update (step 1) and the RTT gate (step 4) read this one value, so the read-before-write
   ordering is unambiguous.
1. **in-flight.** If this is the first segment in `d`, initialize `acked[d] = Some(S.seq)`. Then
   advance `snd_nxt[d]` with `seq_end(S)` (now `Some`). If `ack_advances`, set
   `acked[¬d] = Some(S.ack)`. `in_flight_bytes = serial_diff(snd_nxt[d], acked[d])` when
   `snd_nxt[d]` is serial-≥ `acked[d]`, else **0** (clamp — at a mid-path vantage an ACK can be
   observed before the data it covers; design §4). Both operands are `Some` here: `snd_nxt[d]`
   was just advanced and `acked[d]` was just initialized. **Pure-ACK / drain semantics:** the sample reports
   `dir`'s own outstanding, so a pure ACK (which advances `acked[¬d]`, draining the *opposite*
   direction) records `dir`'s in-flight (~0 for a one-way receiver), **not** the drained
   direction. The drained direction's reduction surfaces on its **next data sample** (whose
   `snd_nxt − acked` already nets out the intervening ACKs), so the in-flight sawtooth is
   reconstructable at each same-direction send. The one value not sampled in replay is the
   **final drain after a direction's last data segment** (no later same-direction event to
   carry it); live mode samples it via `Tick` decay (M11). This is the per-event-sampling
   contract (§4.1, ADR-0004), made explicit here and asserted by a data-then-only-ACKs test.
2. **retransmit / out-of-order.** Only for a **data** segment (`payload_len > 0`); pure ACKs and
   bare control segments are neither. Let `f = frontier[d]` (pre-`S`). If `S.seq` is serial-< `f`
   (re-covers seen sequence space): it is `out_of_order` when `S.ts − last_data_ts[d]` (saturating)
   is **<** `reorder_window`, else `retransmit`. Otherwise (`S.seq` serial-≥ `f`, new data or a
   forward gap) both are `false`. Then advance `frontier[d]` and set `last_data_ts[d]`.
3. **SACK.** `sack = !S.options.sack_blocks.is_empty()`.
4. **RTT (Karn).** RTT-eligibility is per **sequence-consuming** segment: a send that consumes
   sequence space (`payload_len > 0`, or a SYN/FIN phantom byte) **and** is not a retransmit
   pushes `(seq_end(S), S.ts)` onto `pending_rtt[d]`; pure ACKs (no payload, no SYN/FIN)
   register nothing. A **retransmit** in `d` **clears** `pending_rtt[d]` (Karn: the
   retransmission makes every outstanding sample ambiguous — conservative, no false RTT). If
   `ack_advances` (step 0), pop from `pending_rtt[¬d]` every entry with `seq_end` serial-≤
   `S.ack`; if any were popped, `rtt = Some(S.ts − send_ts)` of the **oldest** popped entry,
   else no RTT. The oldest popped entry is the segment that was **outstanding the longest among
   those this ACK newly acknowledges** — i.e. the byte at the old `acked[¬d]` frontier — so
   `S.ts − send_ts` is that segment's true round trip (RFC 6298 times the segment at `snd.una`).
   It biases the single per-ACK sample toward the largest in-burst RTT when one delayed
   cumulative ACK covers a burst; this is the documented definition the gated `tcptrace`
   cross-check must compare against (tcptrace emits per-segment samples, so the comparison
   filters to the oldest-acked segment per cumulative ACK). A dup ACK (`!ack_advances`) yields
   no RTT.
5. **throughput.** `throughput_bps` = `8 ·` (sum of `payload_len` over `d` **data** segments
   whose ts ∈ `(S.ts − throughput_window, S.ts]`) `· 1e9 / throughput_window_ns`, computed in
   `u128` and saturating-cast to `u64`. Wire bytes (retransmissions **included**); goodput is
   M9. The window is relative to `S`'s own ts, so a reordered earlier-ts segment computes its
   own trailing window over bytes accumulated so far (non-monotonic-safe, saturating). The
   per-direction `(ts, len)` history this sum reads from is a bounded deque, evicted to entries
   with `ts > (max_observed_ts[d] − throughput_window)` (lazy prune; non-monotonic-safe via the
   running max), so its memory is bounded by **one window** of traffic regardless of capture
   size — it is **not** governed by `max_samples` (which bounds the output series).

`Tick` items produce **no** sample (replay emits none; live decay-to-zero is M11). The
sample's `t` is `S.ts`. When the instance is being collected (`series_collection` is `All`, or
`Only` names it), push the sample to its buffer; if the retained count across collected series
would exceed `max_samples`, stop pushing and record the overflow so `into_metrics` returns
`MetricError::SampleCeiling`.

### `tcp-visr metrics` subcommand

- `tcp-visr metrics <FILE> --conn <N> [--throughput-window-ms <MS>] [--reorder-window-ms <MS>]
  [--max-samples <K>]` resolves and serializes one connection's series in **two passes** over
  the capture through the **pure-Rust replay faucet** (`parse_file_visit`, no libpcap):
  **pass 1** runs a `series_collection = None` tracker to `into_connections()`, fixing the
  deterministic order and resolving `N` to its `ConnId` (and rejecting an out-of-range `N`
  before any series is built); **pass 2** re-runs with `series_collection = Only(target)` so
  only connection `N` buffers samples (bounded by `N`'s own `max_samples`, never by unrelated
  flows). The selected `ConnectionMetrics` is serialized as JSON to a locked `io::stdout()` via
  `serde_json::to_writer_pretty` (the `print_stdout` lint is denied; serde writes through the
  writer, no macros). Two passes are cheap and safe because replay is deterministic — the
  connection order is identical across passes (design §3.2/§5; the faucet re-reads the file).
- **`--conn N`** is **required** and is the **0-based index** into the deterministic connection
  order (the same order `tcp-visr conns` prints). `N ≥ count` is an actionable error:
  `connection index N out of range (capture has K connections, 0..K-1); run `tcp-visr conns
  <FILE>` to list them` and a non-zero exit. (`conns` output is **unchanged**; M3 does not
  modify M2's contract.)
- The `Metrics` clap variant changes from a unit variant to a struct variant
  (`Metrics { file, conn, … }`); it is removed from the "not implemented" arm.
- **JSON shape** (stable; pinned by the oracle goldens), pretty-printed with a trailing
  newline:
  ```json
  {
    "connection": {
      "index": 0,
      "origin": "10.0.0.1:1234",
      "responder": "10.0.0.2:80",
      "instance": 0,
      "state": "Established",
      "origin_inferred": false,
      "opened_at_ns": 1000,
      "last_at_ns": 3000
    },
    "throughput_window_ns": 1000000000,
    "reorder_window_ns": 3000000,
    "samples": [
      { "t_ns": 1000, "dir": "o2r", "in_flight": 50, "throughput_bps": 400,
        "rtt_ns": null, "retransmit": false, "out_of_order": false, "sack": false }
    ]
  }
  ```
  `dir` serializes as `"o2r"` / `"r2o"`; `rtt_ns` is `null` when absent. Endpoint/state strings
  reuse the existing `Display`/`Debug` renderings (`Endpoint` `Display`, `ConnState` `Debug`).
  Serde `Serialize` is derived on **CLI-local DTO structs** that borrow from
  core/engine types (core/engine stay serde-free; ADR-0007).
- Errors propagate as `Result` (no `panic!`/`process::exit`); a missing/unreadable file yields
  the M1 actionable `IngestError`; an exceeded ceiling yields the `MetricError` message; both
  exit non-zero.

### Fixtures + validation oracle

Built by the **same code-generated, drift-guarded builder** as M1/M2 (`tests/support`), so each
fixture is reviewable as source. M3 adds metric-specific fixtures and **golden oracle files**:

- **Fixtures** (legacy `.pcap`, strictly increasing microsecond timestamps unless a scenario
  needs reordering):
  1. **`metrics_basic.pcap`** — a SYN handshake + a few data segments + ACKs: exercises
     in-flight growth/drain, a handshake RTT and a data RTT, and throughput within the window.
  2. **`metrics_retransmit.pcap`** — data, then a re-send of an earlier sequence range after a
     gap **≥ `reorder_window`**: a `retransmit = true` sample, and Karn suppresses the RTT for
     the retransmitted range.
  3. **`metrics_ooo.pcap`** — two data segments whose **capture order** is reversed within a gap
     **< `reorder_window`**: an `out_of_order = true` sample (not retransmit).
  4. **`metrics_sack.pcap`** — a segment carrying a SACK block (TCP option emitted by the
     builder): a `sack = true` sample.
  5. **`seq_wrap.pcap`** — **reused from M2** (the DoD's "`u32` seq-wrap fixtures"): in-flight
     across the `u32` boundary must be serial-correct (a naive subtraction would be grossly
     wrong); an RTT pairs across the wrap.
- **Oracle goldens** (`crates/tcp-visr/tests/oracle/<fixture>.metrics.json`): the expected
  `metrics --conn N` JSON for each fixture. To be a real check and not a re-snapshot of the
  code's own output, every load-bearing value (in-flight at/around the wrap, the RTT pairings,
  the retransmit/OOO/SACK flags, the throughput in a known window) is **hand-derived from the
  fixture's segments by RFC 1982 serial arithmetic** — the plan enumerates the full per-segment
  numbers for **all five** fixtures (not just `seq_wrap`) in the same form as this spec's Oracle
  derivation appendix, and that enumeration, not program output, is what the committed golden is
  written from. An integration test asserts `metrics` output **byte-matches** the golden. The
  drift guard's `regenerate` path is **explicitly gated** (`#[ignore]`, run only after a
  reviewed derivation change), so a casual `cargo test` can never silently bless a changed
  golden; a derivation change requires re-deriving the numbers by hand and reviewing the diff.
- **Gated external cross-check (release gate)**: an `#[ignore]`d test (`tcptrace_cross_check`)
  documents how to regenerate an independent reference with `tcptrace`/Wireshark on the same
  fixtures (using the oldest-acked-per-cumulative-ACK RTT definition above). It is run by
  maintainers, **skipped in CI** (no external tool), mirroring the existing `live`-gate pattern.
  **CI therefore provides drift-guarding and the hand-derived analytic check, not an independent
  tool comparison** — the external cross-check is a documented **release-checklist gate** (run
  before tagging a release; record the reference). This is the honest realization of design §8's
  "independent tool" oracle without a CI dependency, and it is why the analytic goldens must be
  hand-derived rather than code-emitted (a shared author error would otherwise pass both).

## Testing (design §8: engine is pure unit tests fed hand-built `Vec<Item>`)

- **Engine unit tests** (in `tcpvisr-engine`, hand-built `Vec<Item>`, no files), one per M3
  behavior, asserting **what** the series reports (not how):
  - in-flight grows by sent bytes and drains on ACK; **clamps to 0** when an ACK covers more
    than `snd_nxt` (mid-path vantage);
  - in-flight is **serial-correct across a `u32` wrap** (the seq-wrap case as `Vec<Item>`), and a
    naive-subtraction value would differ — asserted explicitly;
  - **retransmit** vs **out-of-order** split by the reorder window (one fixture-equivalent each,
    plus the boundary: gap exactly `reorder_window` ⇒ retransmit);
  - **SACK** flag set iff the segment carried SACK blocks;
  - **RTT** pairs a data send with the ACK that first covers it (handshake RTT and data RTT);
    **Karn**: no RTT from a retransmitted range; a **dup ACK** produces no RTT; an **ACK before
    any data in the acked direction** advances nothing and yields no RTT (`snd_nxt[¬d] = None`);
  - **throughput** sums wire bytes (retransmits included) in the trailing window; bytes outside
    the window are excluded; the boundary (`ts − window`, exclusive) is asserted;
  - phantom-byte accounting: a SYN/FIN advances the sequence frontier (in-flight reflects it) but
    is **not** counted in `bytes_*`;
  - `series_collection = None` yields **empty** series (and no ceiling cost); `All` yields one
    sample per segment per connection; `Only(id)` yields samples for **only** the named instance
    (others stay empty and contribute nothing to the ceiling);
  - **drain visibility**: a data-then-only-ACKs tail samples the sawtooth peaks at each data
    send, the same-direction drain on the next send, and **no** post-last-data drain in replay
    (the documented per-event-sampling limit);
  - **`max_samples`** ceiling: a small limit makes `into_metrics` return
    `MetricError::SampleCeiling` with the count/limit; below the limit it returns `Ok`; an
    `Only` collection is bounded by the named connection alone (unrelated large flows do not
    trip it);
  - **non-monotonic time**: a reordered earlier-ts segment does not panic and computes its own
    trailing window (saturating).
- **`proptest`** for the in-flight/serial property: for any baseline and any sequence of forward
  sends and advancing ACKs, `in_flight_bytes` equals the serial distance and never panics; for a
  send crossing the `u32` boundary, in-flight stays the true outstanding count (guards the
  wrap directly).
- **CLI/oracle integration** (`tcp-visr` crate): `tcp-visr metrics <fixture> --conn N` exits 0
  and its stdout **byte-matches** the committed golden for every fixture; an out-of-range
  `--conn` (rejected in pass 1, before any series is built), a missing file, and an exceeded
  `--max-samples` each exit non-zero with the actionable message. A multi-connection capture with
  a small `--max-samples` still succeeds for a small target connection (the `Only` ceiling is not
  tripped by unrelated flows).
- **Drift guard**: committed fixture bytes **and** committed oracle goldens match their
  generators (regenerate on change), matching M1/M2.

## Out of scope

- **TUI / detail views** (M4–M9): rendering the series as Stevens/in-flight/RTT/throughput
  graphs. M3 produces the series; the graphs read it.
- **Cross-connection interval index & "as of `T`" resolution** (M5).
- **Goodput vs. retransmitted** separation and the throughput/goodput detail switcher (M9). M3's
  `throughput_bps` is **wire** bytes/sec (retransmissions included).
- **Kernel `cwnd`/`srtt` overlay** (`KernelInfo`, M12); in-flight here is the **wire**
  outstanding estimate, never conflated with cwnd (design §4).
- **Live `Tick` injection and decay-to-zero** (M11); M3 ignores `Tick`s for the series.
- **TCP-timestamp-option RTT and per-vantage RTT halving** (live/M11–M12); M3 RTT uses **capture
  timestamps** and the observed ACK, vantage-agnostic.
- **Streaming/indexing very large captures** (post-v1, §7); M3 enforces only the coarse
  `max_samples` ceiling.
- Modifying M2's `conns` output, `Connection` type, or the `Tracker::observe`/`into_connections`
  API.

## Definition of Done

1. `cargo build --workspace` and `cargo build --workspace --features live` both succeed
   (toolchain 1.88.0).
2. `cargo fmt --all --check` clean.
3. `cargo clippy --all-targets --all-features -- -D warnings` clean.
4. `cargo test --workspace` passes (engine unit + proptests, core, ingest, CLI/oracle tests).
5. `cargo test -p tcpvisr-ingest --features live` still passes (M1 parity unaffected).
6. `cargo deny check` passes with the serde/serde_json runtime deps added to the CLI and any new
   SPDX ids added to the `deny.toml` allow-list (no license-not-encountered warning).
7. `tcp-visr metrics <fixture> --conn N` dumps the JSON series for every DoD fixture, exits 0 on a
   valid capture + in-range index, and non-zero with an actionable message on a missing file, an
   out-of-range index, or an exceeded `--max-samples`.
8. The series carries in-flight, throughput, retransmit/OOO/SACK, and Karn-paired RTT; the
   **`u32` seq-wrap fixture** and the **hand-derived analytic oracle goldens** pass (byte-match),
   and the drift guards (fixtures + goldens) pass. (The independent external-tool cross-check is
   a release-checklist gate, not a CI gate — see Fixtures + validation oracle.)

## Task breakdown (→ sub-issues)

- **Task 1 — core `MetricSample` + `SampleDir`** (`area:core`): the `Copy` sample struct and
  direction enum, pure, unit-tested. No deps added to core.
- **Task 2 — engine derivation state** (`area:engine`): per-direction `snd_nxt`/`acked`/
  `frontier`/`last_data_ts`/`pending_rtt`, the `EngineConfig` knobs (`series_collection`,
  `throughput_window`, `reorder_window`, `max_samples`) with the `SeriesCollection` enum,
  `MetricError`, the phantom-byte `seq_end`, the once-computed `ack_advances` predicate, and the
  in-flight/retransmit/OOO/SACK/RTT/throughput derivation, wired into the existing
  `observe`/`account` path behind `series_collection`. Hand-built `Vec<Item>` unit tests +
  the in-flight `proptest`. Depends on Task 1.
- **Task 3 — `ConnectionMetrics` + `into_metrics`** (`area:engine`): the bundled view, the
  deterministic ordering, and the ceiling-enforcing finalizer. Depends on Task 2.
- **Task 4 — fixtures + oracle goldens + drift guard** (`area:cli`/`area:engine`): the four new
  `.pcap` fixtures (incl. SACK-option emission in the builder), reuse of `seq_wrap.pcap`, the
  committed goldens with documented derivation, and the drift-guard test. Depends on Tasks 2–3
  (so the expected outcomes are known).
- **Task 5 — `metrics` CLI + JSON DTOs** (`area:cli`): serde/serde_json deps, the CLI-local
  `Serialize` DTOs, the `metrics` subcommand (`--conn` required, window/ceiling flags), the
  out-of-range/missing-file/ceiling error paths, and the CLI/oracle integration tests; `deny.toml`
  license updates. Depends on Tasks 3–4.

## Decisions & assumptions

- **Per-event, per-direction sampling (ADR-0007).** One `MetricSample` per observed segment,
  tagged with the segment's `dir`; `in_flight`/`throughput`/`retransmit`/`out_of_order` pertain to
  `dir`. `rtt` is a round-trip measurement placed on the acknowledging segment's sample; `sack` is
  the segment's own option. A single flat series faithfully carries a two-half-stream connection
  because each event is directional. A **pure ACK** reports its own (`dir`) outstanding, not the
  opposite direction it drains; the drained direction's reduction appears on that direction's
  next data sample, and the final post-last-data drain is unsampled in replay (per-event
  sampling, §4.1) — stated so the in-flight series' behavior on a bulk transfer is not read as a
  bug. The `ack_advances` predicate (step 0) is computed once against pre-update `acked` so the
  in-flight update and the RTT dup-ACK gate cannot disagree.
- **Two sequence accumulators.** Byte counters stay payload-only (M2, unchanged); the in-flight/
  RTT/retransmit **frontier** counts SYN/FIN phantom bytes, because they consume wire sequence
  space — otherwise in-flight is off by one around the handshake/close. Stated so the review does
  not read the divergence as an inconsistency.
- **Retransmit vs out-of-order by a reorder window (ADR-0007).** A behind-frontier data segment is
  reordering if it arrives within `reorder_window` of the previous same-direction segment, else a
  retransmission. This is the standard observer discriminator (Wireshark uses the same idea); the
  exact value is a tunable, default 3 ms. A capture observer cannot distinguish the two with
  certainty; this is best-effort by design, and the gated `tcptrace` cross-check is the
  independent check.
- **Karn is conservative.** A retransmit in a direction clears that direction's pending-RTT
  queue, guaranteeing no RTT is paired against an ambiguous (retransmitted) send. This can drop a
  few legitimate later samples; correctness (no false RTT) is preferred over completeness, and the
  rule is documented, not silent.
- **In-flight clamps to 0** rather than reporting a negative/huge wrap when an ACK is observed
  ahead of the data it covers (a mid-path vantage artifact, design §4). The vantage caveat is a
  property of replay (unknown vantage); the series is the honest **outstanding** estimate, never
  presented as cwnd.
- **Throughput is wire bytes/sec, frozen at derivation (ADR-0004, §4.1).** Retransmissions are
  included (goodput separation is M9). The window default is 1 s, configurable. Frozen because
  seeking never re-parses (§5).
- **`--conn N` is a 0-based index into the deterministic connection order**, the same order
  `conns` prints; `metrics` echoes the selected connection's identity in the JSON `connection`
  block so the user can confirm the selection without an index column being added to `conns`
  (M2's output stays stable). Out-of-range is an actionable error pointing at `conns`.
- **serde/serde_json are the JSON mechanism, confined to the CLI.** Hand-rolling JSON escaping is
  error-prone; serde is the audited standard (MIT/Apache). Core and engine stay
  **dependency-free / serde-free** (ADR-0002 purity); the CLI owns serialization via local DTOs.
  Versions pinned with `=`; `deny.toml`'s license allow-list is extended to cover serde's tree
  (e.g. `Unlicense OR MIT` deps resolve to the allowed `MIT`).
- **`thiserror` for `MetricError`.** The engine gains its first runtime dep (`thiserror`, already
  in the workspace lock via ingest) for the one actionable ceiling error; alternatively a
  hand-written `Display`/`Error` impl avoids it. The plan picks one; either keeps the engine
  pure (no I/O). *(Spec leaves the mechanism to the plan; the **behavior** — an actionable
  ceiling error — is fixed here.)*
- **Capture-size ceiling lands in M3 (design §7, deferred here by M2).** `max_samples` is a coarse
  OOM guard over the **collected** series, failing fast at finalize with the count, the limit, and
  the `--max-samples` override. Because `metrics` collects only the requested connection
  (`Only(target)`, resolved in a first lifecycle-only pass), the ceiling bounds that one
  connection and a large multi-connection capture cannot make `metrics --conn N` fail on series
  the user never asked for. Per-byte ceilings and streaming are post-v1.
- **No M2 regressions.** `Connection`, `observe`, `into_connections`, the `conns` command, and all
  M2 tests/fixtures are untouched; M3 adds `into_metrics` and new fixtures/goldens only.
- **No new runtime deps in core/engine beyond the optional `thiserror`; serde/serde_json in the
  CLI only.** Dev-only `proptest` (engine, already present) and `etherparse` (tcp-visr fixtures,
  already present) are reused.

## Oracle derivation appendix (hand-derived golden values)

Each load-bearing golden value is derived here from the fixture's segments by RFC 1982 serial
arithmetic and the contract above; the committed `metrics --conn N` JSON must match. (Concrete
per-segment numbers are finalized with the fixtures in the plan; this appendix fixes the
**method** the goldens are checked against, so a golden is never "whatever the code emitted".)

- **`seq_wrap.pcap`** (C→S `seq=2^32−101 len=50`, C→S `seq=200 len=50`, S→C `seq=1 ack=300 len=10`):
  - Sample 1 (o2r): `acked[o2r]` init = `2^32−101` (first o2r seq); `snd_nxt[o2r] = 2^32−51`;
    `in_flight = serial_diff(2^32−51, 2^32−101) = 50`. The segment also carries `ACK=1`, but
    `r2o` has no tracked send yet (`snd_nxt[r2o] = None`), so `ack_advances = false`: `acked[r2o]`
    stays `None` and no RTT is produced. (This is the unobserved-opposite-direction case from the
    derivation contract; `r2o` is anchored later at sample 3.)
  - Sample 2 (o2r): `seq=200` is a **forward** advance of `2^32−51` (serial, the wrap);
    `snd_nxt[o2r] = 250`; `in_flight = serial_diff(250, 2^32−101) = 351` (the 50 + the 301-byte
    serial span across the wrap). A naive `250 − (2^32−101)` is **negative/≈4.29e9** — the test
    asserts the serial value `351`, not that.
  - Sample 3 (r2o): carries `ack=300`, which serial-advances `acked[o2r]` to `300`, covering both
    o2r sends (`seq_end` `2^32−51` and `250`, both serial-≤ `300`); RTT pairs the **oldest**
    pending o2r send (ts of sample 1) ⇒ `rtt = ts₃ − ts₁`. This sample's own `dir=r2o`,
    `in_flight[r2o] = serial_diff(11, 1) = 10`.
- **`metrics_basic` / `metrics_retransmit` / `metrics_ooo` / `metrics_sack`**: derived the same
  way; the retransmit/OOO split is checked against the `reorder_window` gap, SACK against the
  emitted option, RTT against the documented send/ACK timestamps, and throughput against the
  bytes-in-window sum for the fixture's chosen window. Exact numbers are committed with the
  fixtures and re-stated in `tests/oracle/README.md`.

## Acceptance verification commands

```bash
cargo build --workspace
cargo build --workspace --features live
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace
cargo test -p tcpvisr-ingest --features live
cargo deny check
cargo run -p tcp-visr -- metrics crates/tcp-visr/tests/fixtures/seq_wrap.pcap --conn 0
```
