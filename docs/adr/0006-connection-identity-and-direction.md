# ADR-0006: Connection identity, orientation, and direction in the pure engine

> Status: Accepted
> Date: 2026-06-30

## Context

M2 (design §10.M2) introduces the per-connection state machine in `tcpvisr-engine`. The
engine receives a flat stream of `Item`s (`Segment | Tick`) whose `Segment`s carry the
**wire-as-seen** `FlowKey` — `tcpvisr-core`'s `FlowKey` is documented as "stored as-seen;
not canonicalized" and M1 explicitly deferred any direction concept to M2 because it is
connection-relative.

Three decisions are forced before any code:

1. **Grouping.** A 4-tuple appears on the wire in two orientations (A→B and B→A). The two
   halves of one TCP connection must collapse to a single tracked entity, or every
   connection would appear twice and per-direction byte/sequence accounting would be split.

2. **Orientation & direction.** Design §4's data model lists `Segment.direction`. But the
   producer of `Segment` is `tcpvisr-ingest`, which has no connection context: for a
   mid-stream capture with no observed SYN there is no wire signal for "who is the client".
   Direction therefore cannot be a field the faucet fills. Something must own deriving it.

3. **Instance identity.** Design §4 establishes that a bare 4-tuple is *not* a stable
   identity (port reuse, `TIME_WAIT` recycling, NAT rebinding). The same socket pair can
   carry multiple independent sequence spaces within one capture. Splicing them corrupts
   sequence-derived state. The DoD requires distinguishing a benign `u32` sequence **wrap**
   (must stay one instance) from a genuine **new instance** (must split).

These are layer-boundary and identity decisions with viable alternatives, so they are
recorded here rather than buried in the engine source.

## Decision

**We will make connection grouping, orientation, direction, and instance disambiguation
purely engine-internal, derived from the `Item` stream. `tcpvisr-core::Segment` gains no
`direction` field; `FlowKey` stays wire-as-seen.**

Concretely, in `tcpvisr-engine`:

- **Grouping key (orientation-independent).** An `Endpoint` is `(IpAddr, u16)`. Two endpoints
  are canonicalized into an `EndpointPair` by ordering them (the numerically/lexically lower
  `(IpAddr, port)` is `low`, the other `high`). Both wire orientations of a connection map to
  the same `EndpointPair`. This is the grouping key; it never depends on who initiated.

- **Orientation (which side is the origin/client).** Within a grouped connection we record an
  `origin` endpoint and a `responder` endpoint:
  - If a segment with **SYN set and ACK clear** is observed, its source is the `origin`
    (authoritative: only a client sends a bare SYN). For a simultaneous open (both sides send
    a bare SYN), the source of the **first** such SYN in stream order wins.
  - Otherwise (mid-stream capture, no bare SYN seen) the `origin` is the source of the
    **first segment** observed for the connection, and the connection is flagged
    `origin_inferred = true`. The orientation is then arbitrary-but-stable, and the inference
    is surfaced (never presented as if a handshake were seen).

- **Direction is an engine-derived, per-segment property**, computed by comparing a segment's
  `(src,dst)` against the connection's `origin`/`responder`: `OriginToResponder` or
  `ResponderToOrigin`. It is *not* stored on the wire `Segment`. Per-direction sequence
  tracking and byte accounting key off it.

- **Instance disambiguation** keys connections by `ConnId = (EndpointPair, instance: u32)`.
  A new `instance` epoch (incrementing counter per pair) begins only when, for the pair's
  current live instance, one of these holds:
  1. **SYN after termination/idle.** A bare SYN is observed while the current instance is in
     a terminal state (`Closed` or `Reset`), **or** after the current instance has been idle
     longer than the configurable dead-connection timeout (`dead_after`, default 120 s). This
     is the `TIME_WAIT`/port-reuse case.
  2. **Backward sequence reset (mid-stream, no SYN).** For an `Established` instance, a
     segment in a given direction whose `seq` lies **backward in RFC 1982 serial space**
     relative to that direction's established sequence baseline by **more than
     `reset_threshold`** is read as a drop to a fresh ISN — a new instance. A **forward** move
     (`serial_gt`, which is how a `u32` wrap reads under RFC 1982) is an advance and never
     splits. Small backward moves (retransmits, reordering) are below the threshold and never
     split. Because any backward serial distance is in `(0, 2^31)`, `reset_threshold` is the
     **minimum** backward distance that counts as a reset and **must be `< 2^31`** — a
     midpoint (`2^31`) threshold would be unreachable, silently disabling the rule. The
     default is `2^30`, the largest representable TCP window with scaling, above which a
     backward jump cannot be in-flight retransmit/reorder data. A fresh ISN that lands within
     `2^31` *forward* of the prior sequence is indistinguishable from an advance and will not
     split; this is the inherent limit of SYN-less inference (rule 1's SYN is the
     authoritative signal).

  All sequence comparisons use `tcpvisr-core::TcpSeq` RFC 1982 arithmetic — the same code the
  wrap-vs-reset distinction depends on. Instance inference and gap/wrap detection share one
  serial-number implementation by construction.

## Consequences

- **The ingest→engine boundary stays clean (ADR-0001, ADR-0002).** Faucets emit wire-as-seen
  segments and know nothing of connections; the engine owns all connection semantics and
  stays pure (no I/O, time is data). Live and replay continue to share the faucet path
  unchanged.
- **Direction is always well-defined**, including for mid-stream captures, because it is
  derived from the engine's chosen orientation rather than requiring a wire signal the
  capture may not contain. The cost is that for `origin_inferred` connections the
  origin/responder labels are a stable convention, not ground truth; this is surfaced, not
  hidden (design §7, "no silent fallbacks").
- **The wrap-vs-new-instance distinction is a single, testable rule** grounded in RFC 1982,
  satisfying the DoD's `seq-wrap-vs-new-instance` fixture: a forward wrap is an advance; only
  a large backward jump splits.
- **Two tunable thresholds become part of the engine's contract** (`dead_after`,
  `reset_threshold`). Defaults are chosen to match common practice (tcptrace-style dead
  timeout; `2^30` — the max scaled window — for the reset band). They are configuration, not
  magic numbers, and are documented in the spec. `reset_threshold` must stay `< 2^31` or the
  backward-reset rule becomes unreachable. Mis-tuning trades false splits against false
  merges; the defaults are conservative toward *not* splitting.
- **`Segment` does not grow a field that the faucet cannot populate**, avoiding an `Option`
  or sentinel that every faucet would have to fill with "unknown". The design §4
  `Segment.direction` entry is realized as the engine's derived `Direction`, and the design
  doc's model note is read accordingly.
- **Idle-based splitting works in replay without `Tick`s.** Replay's "now" is the last
  segment's timestamp (design §4.1); the dead-timeout comparison uses inter-segment gaps, so
  the rule needs no `Tick` injection (which remains an M11 live-only concern).

## Alternatives considered

- **Add `direction` to `tcpvisr-core::Segment`, filled by ingest.** Rejected: ingest has no
  connection context. For a mid-stream capture it would have to emit "unknown", pushing the
  orientation decision into the engine anyway while polluting the wire model with a field no
  faucet can authoritatively set.

- **Key connections on the bare 4-tuple (as-seen orientation).** Rejected by design §4: it
  doubles every connection (one entry per direction) and splices reused tuples into one
  corrupted sequence space.

- **Canonicalize by always treating the lower port as the server.** Rejected: ephemeral-port
  heuristics are wrong often enough (server-to-server, both-ephemeral, non-standard ports)
  that orientation must come from the SYN when present and from a neutral stable ordering
  otherwise — not from a port-magnitude guess.

- **Split instances on any backward sequence movement.** Rejected: retransmissions and
  reordering routinely move `seq` backward by a little; only a backward jump past the
  serial-space midpoint plausibly indicates a fresh ISN. A naive "any backward = new" rule
  would shatter normal connections and would also misread a `u32` wrap (which is *forward*
  under RFC 1982) — the exact bug the DoD fixture guards against.

- **Detect new instances only from an explicit SYN.** Rejected: mid-stream captures reuse a
  tuple with no observed handshake; the backward-reset rule is required to split those, and
  design §4 mandates it.
