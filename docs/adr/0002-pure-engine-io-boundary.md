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

We will keep the engine **pure**: it consumes an in-memory stream of
`(Timestamp, Segment)` items and emits `MetricSample` series. It performs **no I/O** — no
file handles, no sockets, no clock reads. All I/O lives in `tcpvisr-ingest` (faucets) and
`tcpvisr-enrich`.

## Consequences

- Every TCP edge case becomes a deterministic unit test fed a hand-built `Vec<Segment>`:
  reordering, retransmission, SACK, zero-window, mid-stream capture (no handshake). No
  root, no network, no fixtures required for the hard logic.
- `proptest` can drive serial-number arithmetic (u32 wraparound) directly.
- Live vs. replay differences are confined to the faucet; the engine cannot behave
  differently between modes because it cannot tell them apart.
- Cost: timestamps must be passed in explicitly (the engine cannot read a clock), and the
  ingest layer owns buffering/retention policy rather than the engine.

## Alternatives considered

- **Engine reads files / sockets directly** — rejected: couples the hard logic to I/O,
  makes edge cases require crafted captures or a live host, and invites mode-specific
  divergence.
- **Engine reads the clock for live timing** — rejected: nondeterministic tests; instead
  the faucet stamps each segment and the engine treats time as data.
