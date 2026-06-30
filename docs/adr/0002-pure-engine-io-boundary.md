# ADR-0002: The analysis engine is pure (no I/O)

> Status: Accepted
> Date: 2026-06-30

## Context

The analysis engine (`tcpvisr-engine`) holds the most intricate logic in the project: the
TCP connection state machine and metric derivation (in-flight, RTT pairing, retransmit /
out-of-order / SACK detection, serial-number arithmetic). Bugs here are subtle and
data-dependent. The engine must serve both live capture and file replay
([ADR-0001](0001-packet-derived-unified-model.md)).

## Decision

We will keep the engine **pure**: it consumes an in-memory stream of `Item`s
(`Item = Segment(ts, …) | Tick(ts)`) and emits `MetricSample` series. It performs **no I/O**
— no file handles, no sockets, no clock reads. All I/O lives in `tcpvisr-ingest` (faucets)
and `tcpvisr-enrich`.

Because the engine never reads a clock, "now" advances only via the timestamps it is fed.
For event-driven behavior, segment timestamps suffice. For time-driven behavior in live mode
— declaring a silent connection idle/dead, expiring an inferred RTO when no retransmit
arrives, decaying throughput toward zero — there may be no segment for a long interval, so
`tcpvisr-ingest` injects `Tick(ts)` items at a configurable cadence (default 250 ms) carrying
the current time; the cadence bounds the resolution of idle/RTO/decay detection. The engine's
timers fire off `Tick`s. Replay needs no ticks: "now" is the last segment's timestamp.

## Consequences

- **Event-driven** TCP edge cases become deterministic unit tests fed a hand-built
  `Vec<Item>`: reordering, retransmission, SACK, zero-window, mid-stream capture (no
  handshake), 4-tuple reuse. **Time-driven** cases (idle timeout, RTO expiry with no
  retransmit, throughput decay) are equally deterministic by injecting `Tick` items — a
  capability the segment-only model lacked. No root, no network for the hard logic.
- `proptest` can drive serial-number arithmetic (u32 wraparound) directly.
- Live vs. replay differences are confined to the faucet; the engine cannot behave
  differently between modes because it cannot tell them apart.
- **Input buffering vs. output retention are different concerns with different owners.**
  `tcpvisr-ingest` owns *input* buffering (raw `Item`s before the engine). The live
  *output* retention ring buffer holds `MetricSample` series — the engine's output — and is
  governed by [ADR-0004](0004-seekable-timeseries-timeline.md); it is owned by the engine /
  a series-store layer, and exactly one component trims it. The engine's per-connection
  *running baseline* (ISN, highest seq/ack) is retained for the connection's life regardless
  of display retention, so eviction of old samples never corrupts in-flight derivation.
- **Purity precludes engine-side spilling.** A pure engine cannot page series to disk, so
  unbounded replay memory is bounded only by the external capture-size policy (design §7,
  ADR-0004), not by the engine. The per-connection running baseline set also grows with the
  number of concurrently-open connections; that growth is bounded by the active-connection cap
  in [ADR-0004](0004-seekable-timeseries-timeline.md).
- **Flow control differs by mode.** The engine is push-driven and never blocks (it does no
  I/O). For **replay** the file faucet is a synchronous read→push chain, so a slow engine
  naturally throttles reads — genuine flow control. For **live** the producer is the NIC and
  cannot be throttled; when the bounded input buffer fills, segments are **dropped and
  counted** (surfaced in the UI per design §7), never silently lost or buffered unbounded.
  "Back-pressure" applies only to replay; live degrades by counted drop.
- Cost: timestamps (including `Tick`s) must be supplied explicitly; the engine cannot read a
  clock.

## Alternatives considered

- **Engine reads files / sockets directly** — rejected: couples the hard logic to I/O,
  makes edge cases require crafted captures or a live host, and invites mode-specific
  divergence.
- **Engine reads the clock for live timing** — rejected: nondeterministic tests. Instead the
  faucet stamps each segment and injects `Tick(ts)` items during silence, so the engine treats
  time — including the passage of time between packets — purely as data.
