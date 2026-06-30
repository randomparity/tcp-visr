# ADR-0007: Metric derivation — sampling model, RTT/Karn, and the series boundary

> Status: Accepted
> Date: 2026-06-30

## Context

M3 (design §10.M3) derives the per-connection **metric time series** from M2's tracked
`Connection`s: bytes in flight, throughput, retransmit/out-of-order/SACK, and RTT. Several
decisions are forced before code, each with viable alternatives:

1. **What is a sample, and how does a single flat series represent a two-half-stream
   connection?** A TCP connection carries two independent data streams (origin→responder and
   back), each with its own outstanding bytes, throughput, and retransmissions. Design §4 models
   a single `MetricSample` struct with single-valued fields and §4.1 mandates **one sample per
   processed `Segment`** (per-event sampling). These must be reconciled.

2. **Where does the series live, and what does it cost the `conns` command?** Design §4 places
   `series: Vec<MetricSample>` on `Connection`. But M2's `Connection` is a `Copy` scalar
   lifecycle view that `tcp-visr conns` returns by the thousand; bolting a growing `Vec` onto it
   breaks `Copy` and forces every `conns` run to allocate per-segment samples it never reads.

3. **How are retransmit and out-of-order distinguished from a *capture observer*'s vantage?** An
   endpoint knows what it sent; a passive observer sees only sequence numbers and timestamps and
   must infer whether a behind-frontier segment is a loss-driven retransmission or mere network
   reordering.

4. **How is RTT measured without being corrupted by retransmissions?** Pairing a data send with
   a later ACK is ambiguous if the data was retransmitted — the ACK could be for either
   transmission (the classic Karn problem).

5. **What stops a large capture from exhausting memory** now that the series first occupies RAM
   (design §7 names a capture-size ceiling; M2 deferred it to M3)?

6. **How is JSON produced without polluting the pure engine?** The DoD requires a JSON dump, but
   ADR-0002 keeps `tcpvisr-engine` pure and `tcpvisr-core` is dependency-free.

## Decision

**We will derive metrics as a per-event, per-direction sample stream computed on top of M2's
tracker, deliver the series in a `ConnectionMetrics` wrapper (not on the `Copy` `Connection`),
distinguish retransmit/OOO by a reorder-time window, pair RTT under Karn's algorithm, guard
memory with a configurable total-sample ceiling, and confine serde/JSON to the CLI.**

Concretely:

- **Per-event, per-direction sampling.** One `MetricSample` per observed `Segment`, tagged with
  the segment's engine-derived `dir` (`SampleDir`, ADR-0006). `in_flight_bytes`,
  `throughput_bps`, `retransmit`, and `out_of_order` pertain to `dir`'s half-stream; `sack`
  reflects the segment's own options; `rtt` is a **round-trip** measurement (inherently both
  directions) attached to the sample of the acknowledging segment. A single flat series thus
  faithfully carries both half-streams because every event is itself directional. `Tick`s
  produce no sample (replay emits none; live decay is M11).

- **Two sequence accumulators.** M2's per-direction **byte counters** stay payload-only. M3 adds
  a per-direction **sequence frontier** that counts the SYN/FIN phantom byte (`seq_end = seq +
  payload_len + SYN + FIN`), because those consume wire sequence space and in-flight/RTT would
  otherwise be off by one around the handshake and close. In-flight is `serial_diff(snd_nxt[d],
  acked[d])` (RFC 1982), **clamped to 0** when an ACK is observed ahead of the data it covers (a
  mid-path vantage artifact, design §4) — the honest *outstanding* estimate, never cwnd.

- **`ConnectionMetrics`, not `Connection.series`.** The series is returned as
  `ConnectionMetrics { conn: Connection, series: Vec<MetricSample> }` by a new
  `Tracker::into_metrics(self) -> Result<Vec<ConnectionMetrics>, MetricError>`. M2's `Connection`
  stays `Copy` and unchanged; `observe`/`into_connections` and the `conns` command are untouched.
  A `series_collection: SeriesCollection` config (`None` | `All` | `Only(ConnId)`, default
  `None`) selects which instances buffer samples: `conns` uses `None` (scalar state only, no
  samples); `metrics` uses `Only(target)` so only the requested connection buffers — resolved in
  a first lifecycle-only pass, then collected in a second pass, so a large multi-connection
  capture neither builds nor blows the sample ceiling on series the user did not ask for. This
  realizes design §4's `series` as a layered view rather than a mutation of the lifecycle type.

- **Retransmit vs out-of-order by a reorder window.** A data segment whose `seq` is serial-behind
  the direction's data frontier re-covers seen sequence space. It is **out-of-order** when its
  inter-arrival gap from the previous same-direction segment is below `reorder_window` (default
  3 ms), else a **retransmit**. This mirrors Wireshark's reordering heuristic; the value is a
  tunable. A forward move (including a `u32` wrap, which is *forward* under RFC 1982) is new data
  and never a retransmit.

- **RTT under Karn's algorithm.** Each new-data send registers `(seq_end, send_ts)` in a
  per-direction pending queue. An ACK that serial-advances the acknowledgement frontier pops every
  covered send and yields one RTT sample: `ack_ts − send_ts` of the **oldest** covered send.
  Duplicate ACKs yield none. A **retransmit clears that direction's pending queue** — conservative
  Karn: no RTT is ever paired against an ambiguous (retransmitted) send, at the cost of dropping a
  few legitimate later samples.

- **Throughput is wire bytes/sec, frozen at derivation.** A trailing window (default 1 s,
  configurable) summing `payload_len` over the direction's data segments with ts in `(t − window,
  t]`, `× 8 / window`, computed in `u128` and saturating to `u64`. Retransmissions are included
  (goodput vs retransmitted is M9). Frozen into each sample because seeking never re-parses
  (ADR-0004, §5).

- **Capture-size ceiling.** A configurable `max_samples` (default 10 000 000) over the
  **collected** series stops retaining samples past the limit and makes `into_metrics` fail fast
  with `MetricError::SampleCeiling { samples, limit }` naming the count, the limit, and the
  `--max-samples` override (design §7). Under `metrics`'s `Only(target)` collection the ceiling
  bounds the single requested connection, never unrelated flows. Per-byte ceilings and streaming
  are post-v1.

- **The oracle's independence is structural, not incidental.** The committed goldens are
  hand-derived from the fixtures by RFC 1982 serial arithmetic (the plan enumerates every
  fixture's numbers), and the regenerate path is `#[ignore]`-gated so `cargo test` cannot silently
  re-bless a changed golden. CI runs the analytic goldens + drift guard; the independent
  external-tool (`tcptrace`/Wireshark) cross-check is a **release-checklist gate**, not a CI gate
  (no external tool in CI). This keeps design §8's "independent tool" promise honest without a CI
  dependency, and is why the goldens must not be code-emitted (a shared author error would pass
  both the code and a snapshot golden).

- **serde confined to the CLI.** `tcpvisr-core`/`tcpvisr-engine` stay serde-free and pure
  (ADR-0002). The `tcp-visr` binary owns `serde`/`serde_json` and defines local `Serialize` DTOs
  that borrow from the core/engine types; JSON is written to a locked stdout writer (no
  `print_stdout` macros). Hand-rolling JSON escaping is rejected as error-prone.

## Consequences

- **The ingest→engine→CLI boundary stays clean (ADR-0001/0002).** The engine remains pure and
  serde-free; metric derivation reuses M2's grouping/orientation/instance machinery rather than
  duplicating it. `conns` pays nothing for the series it does not use.
- **A single flat `MetricSample` series is honest about direction.** Consumers (M5 timeline, M6–M9
  detail) read `dir` to separate half-streams; nothing is averaged across directions silently.
- **In-flight is serial-correct across a `u32` wrap** — the DoD's seq-wrap fixture — because every
  comparison routes through `TcpSeq`. Naive subtraction is structurally absent.
- **Retransmit/OOO classification is best-effort and timing-dependent.** A capture observer cannot
  be certain; the `reorder_window` makes the rule explicit and tunable, and the gated `tcptrace`
  cross-check (design §8) is the independent reference. Mis-tuning trades false-retransmit against
  false-OOO; the default leans toward calling a fast re-send "reordering" only within 3 ms.
- **Karn favors correctness over completeness.** Clearing the pending queue on a retransmit can
  drop legitimate later RTT samples in a lossy flow; no *false* RTT is ever emitted, which matters
  because RTT feeds the smoothed line and the wire/kernel comparison.
- **Three new `EngineConfig` knobs plus `collect_series` become part of the engine contract.**
  Defaults match common practice (1 s throughput window, 3 ms reorder window, 10 M sample
  ceiling); all are configuration, documented in the spec, not magic numbers.
- **The CLI gains the project's first non-protocol runtime dependencies** (`serde`,
  `serde_json`). They are pinned (`=`) and audited by `cargo-deny`; the license allow-list grows
  to cover their tree. The engine optionally gains `thiserror` for one actionable error (already in
  the workspace lock), or a hand-written error impl — the plan picks one.

## Alternatives considered

- **One sample carrying both directions' metrics (no `dir` tag).** Rejected: it doubles every
  field (`in_flight_o2r`/`in_flight_r2o`, …), and per-event sampling (§4.1) already produces one
  sample per directional event — tagging is simpler and matches the design's single-valued
  `MetricSample`.

- **Put `series` on `Connection` and drop `Copy`.** Rejected: it forces `conns` to allocate
  per-segment samples it never reads and churns M2's `Copy`-dependent tests for no benefit. The
  `ConnectionMetrics` wrapper keeps the lifecycle view cheap and the metric view rich.

- **Distinguish retransmit/OOO by sequence alone (any behind-frontier segment = retransmit).**
  Rejected: it mislabels ordinary network reordering as loss, inflating the retransmit series.
  Timing is the standard discriminator a passive observer has.

- **Take RTT from every ack/segment pair, including retransmitted sends.** Rejected: this is the
  exact ambiguity Karn's algorithm exists to remove; retransmission-paired samples corrupt RTT.

- **Recompute throughput at scrub time from raw packets.** Rejected by ADR-0004/§5: seeking never
  re-parses; throughput must be frozen into each sample at derivation time.

- **Hand-rolled JSON writer to keep zero runtime deps.** Rejected for correctness: JSON string
  escaping and number formatting are easy to get subtly wrong; `serde`/`serde_json` are the
  audited standard. Purity is preserved by confining them to the CLI, not core/engine.

- **No capture-size ceiling (rely on the OS OOM killer).** Rejected by design §7: a large
  well-formed capture is the simplest resource-exhaustion path; a coarse, configurable,
  fail-fast sample ceiling is the v1 mitigation.
