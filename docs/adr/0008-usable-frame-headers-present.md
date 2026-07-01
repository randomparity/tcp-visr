# ADR-0008: A frame is usable when its headers are captured; payload length comes from the IP length field

> Status: Accepted
> Date: 2026-06-30

## Context

`tcp-visr` must analyze **header-only captures** — files taken with a short
snaplen (`tcpdump -s 64`), a common way to capture cheaply. The tool derives every
metric from TCP/IP **headers** plus the payload **length**; it never reads payload
bytes. A header-only capture therefore contains everything the tool needs, yet
today it is unusable:

- The replay and libpcap faucets both skip a frame with
  `if caplen < origlen { record(Truncated) }` *before* the shared `decode_frame`,
  so every snaplen-limited frame is dropped as `Truncated`.
- `decode_frame` sets `payload_len = tcp.payload().len()`, measuring the
  **captured** payload. For a snaplen-cut frame that is shorter than the on-wire
  payload, so even relaxing the skip would yield wrong byte counts.

Two decisions are forced, each with viable alternatives:

1. **What makes a frame usable, and where does that decision live?**
2. **How is on-wire `payload_len` derived when the payload bytes are absent?**

## Decision

We will treat a frame as **usable when the captured bytes contain the full link +
IP + TCP header (TCP options included)**, regardless of whether payload bytes were
captured. Only a frame whose *headers* are cut off is counted `Truncated`.

We will move this policy into the single shared `decode_frame`, which now receives
the **on-wire frame length** next to the captured bytes:

```
decode_frame(link, ts, frame, wire_len)
```

The faucets stop pre-filtering on `caplen < origlen`; they pass `frame` (captured)
and `wire_len` (`origlen`) and let the one decoder classify. A header-layer `Len`
parse error from etherparse's lax slicer, on a frame where `wire_len >
frame.len()`, is `Truncated`; the same failure on a full frame stays `Malformed`.

We will derive `payload_len` from the **IP length field** (IPv4 `total_len`, IPv6
`40 + payload_length`) minus the captured header offset up to the end of the TCP
header, falling back to the captured payload length only when the IP length field
is implausible (smaller than the headers it must contain — e.g. hardware-offload
`total_len == 0` or an IPv6 jumbogram).

## Consequences

- **The "one decoder" contract (design §3.2) is honored for truncation too.** The
  usable/skip decision and the payload-length derivation now live in `decode_frame`
  alone. The duplicated `caplen < origlen` gate is removed from both faucets, so
  the two faucets cannot drift on truncation handling — the parity test guards this
  by construction.
- **`Truncated` is redefined from "frame shorter than on-wire" to "headers cut
  off".** Skip counts for genuinely header-truncated frames are unchanged; frames
  that were previously dropped only because their *payload* was cut are now decoded.
  This is the intended behavior change.
- **Full-frame behavior is unchanged.** For a well-formed full frame the
  IP-length-derived value equals `tcp.payload().len()` (etherparse already bounds
  `tcp.payload()` by the IP length field), so existing decodes, the parity test,
  and full-frame `payload_len` assertions are unaffected.
- **`decode_frame` gains a parameter.** Callers (both faucets and the decoder unit
  tests) must pass `wire_len`. This is a small, contained ripple and makes the
  on-wire length explicit at the one place that needs it.
- **Offload/jumbo frames degrade honestly.** When the IP length field cannot give
  the on-wire payload length, we fall back to the captured length rather than
  fabricating a value — the same number reported today, now documented.

## Alternatives considered

- **Keep the `caplen < origlen` gate in the faucets, only relax its threshold.**
  Rejected: it leaves the truncation policy duplicated across the two faucets (the
  exact drift risk design §3.2 exists to prevent) and still needs the decoder to
  fix `payload_len`, so the policy would be split across three places.

- **Detect truncation purely from etherparse's `incomplete` payload flag, without
  passing `wire_len`.** Rejected: the `incomplete` flag is set only once the IP
  header parses, so a snaplen that cuts inside the IP header could not be
  distinguished from genuine garbage, and a full malformed frame and a truncated
  one would be indistinguishable — collapsing the `Malformed`/`Truncated`
  distinction the spec requires. Passing the on-wire length makes the distinction
  exact and layer-independent.

- **Always derive `payload_len` from the IP length field, with no captured-length
  fallback.** Rejected: hardware-offload frames report `total_len == 0` and IPv6
  jumbograms report `payload_length == 0`; deriving blindly would underflow or
  fabricate a wrong length. The fallback preserves today's behavior for those
  frames.

- **Reconstruct payload length from `origlen` (the pcap on-wire frame length)
  minus header sizes.** Rejected: `origlen` is a container-level frame length that
  includes link-layer framing and any trailer/padding, so it is a less precise
  source than the IP length field, which is exactly the on-wire IP packet size.
  `wire_len` is used only to decide *truncated vs. full*, not to measure payload.
