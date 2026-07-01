# Spec: analyze header-only (short-snaplen) captures

> Issue: #23 · Status: Draft · Date: 2026-06-30
> Layer: ingest only (no engine/CLI/core type changes)
> ADR: [ADR-0008](../../adr/0008-usable-frame-headers-present.md)

## Problem

`tcp-visr` drops every frame whose captured length is shorter than its on-wire
length. A header-only capture (`tcpdump -s 64`) therefore decodes almost nothing:
every payload-bearing frame is counted `Truncated`, surviving connections report
`bytes=0/0`, and `metrics` emits an all-zero series. The tool only needs the
TCP/IP **headers** plus the payload **length** — never payload bytes — so these
captures should be fully analyzable.

Two defects combine to cause this:

1. **The truncation gate is frame-level and lives in the faucets.** Both
   `replay.rs` (`process_packet`) and `libpcap.rs` skip with
   `if caplen < origlen { record(Truncated); continue }` *before* the shared
   `decode_frame`. Any snaplen-limited frame is dropped, even when its headers are
   fully present.
2. **`payload_len` is measured from captured bytes.** `decode.rs` sets
   `payload_len = tcp.payload().len()`. For a snaplen-cut frame the captured TCP
   payload is shorter than the on-wire payload, so even if the gate were relaxed
   the length would be wrong.

## Goal

A frame is **usable** when the captured bytes contain the full link + IP + TCP
header (including TCP options). For usable frames, derive `payload_len` from the
IP length field so byte counts, in-flight, throughput, and seq/ack metrics are
correct. A frame whose captured bytes do not cover the full headers stays
`Truncated`.

## Non-goals

- No engine, CLI, or `tcpvisr-core` type changes. `Segment.payload_len` already
  carries the on-wire length; only its *derivation* changes.
- No reconstruction of payload **bytes** (we never need them).
- No new skip reasons. `Truncated` keeps its meaning ("headers cut off"); its
  trigger moves from "frame shorter than on-wire" to "headers shorter than
  captured can cover".
- Link-header truncation (snaplen smaller than the link header itself, e.g. < 14
  bytes for Ethernet) is out of scope; such absurd captures keep their current
  classification. The realistic floor (`-s 64`) always captures link + IPv4 +
  most TCP headers.

## Design

### Move the truncation policy into the one decoder

The faucets stop pre-filtering on `caplen < origlen`. They always call
`decode_frame`, passing the **on-wire frame length** alongside the captured
bytes:

```
decode_frame(link: LinkType, ts: Nanos, frame: &[u8], wire_len: u32) -> DecodeOutcome
```

- `frame` is the captured bytes (`frame.len()` == `caplen`).
- `wire_len` is the original on-wire length (`origlen` / `header.len`).
- `wire_len > frame.len()` means the frame was snaplen-truncated.

This makes the "usable = headers present" contract live in the single decoder the
design promises (design §3.2), removes the duplicated gate from both faucets, and
keeps both faucets in lock-step for the parity test by construction.

### Classify header truncation as `Truncated`

`decode_frame` parses with etherparse's lax slicer, which reports a `Len` error
(byte shortage) on the layer that ran off the end of the captured slice. The
classification rule:

- A header-layer parse failure (`from_ip` returns `Len`, or `transport` is `None`
  with a `Len` stop-error) **and** the frame was truncated (`wire_len >
  frame.len()`) → `Truncated`.
- The same `Len` failure on a **full** frame (`wire_len == frame.len()`) →
  `Malformed` (genuinely corrupt / too short), preserving today's behavior.
- Existing non-`Len` mappings are unchanged: unsupported IPv6 extension chain →
  `UnsupportedExtChain`; otherwise non-TCP → `NonTcp`; IPv6 fragment →
  `Ipv6Fragment`.

Tying `Truncated` to `wire_len > caplen` is what keeps a full 3-byte garbage frame
classified `Malformed` (the bytes are all present and invalid) while a snaplen-cut
TCP header is `Truncated`.

### Derive `payload_len` from the IP length field

For a decoded TCP segment, compute the on-wire payload length from the IP length
field and the header offsets, never from the captured payload slice:

```
onwire_ip_len = match net {
    Ipv4(v4) => v4.header().total_len() as usize,          // counts from IP header start
    Ipv6(v6) => 40 + v6.header().payload_length() as usize, // fixed header + payload
};
tcp_offset = checked sub-slice offset of tcp.slice() within the IP slice; // IP hdr + ext hdrs
need = tcp_offset + tcp.slice().len();                       // bytes up to end of TCP header
payload_len = if onwire_ip_len >= need {
    (onwire_ip_len - need) as u32                            // on-wire payload length
} else {
    tcp.payload().len() as u32                               // implausible length field: fall back
};
```

- **Invariant (load-bearing):** `tcp.slice()` is a sub-slice of the exact stripped
  IP slice passed to `from_ip` — same allocation — so the TCP header's byte offset
  within that slice is well-defined. This holds for etherparse 0.20.2 (the lax
  slicer returns borrows into the input, never copies). The implementation must not
  assume it blindly: compute the offset with a **checked** operation
  (`tcp.slice().as_ptr() as usize` minus `ip.as_ptr() as usize`, guarded by
  `checked_sub` and an upper-bound check `tcp_offset + tcp.slice().len() <=
  ip.len()`); if the invariant ever fails to hold, fall back to
  `tcp.payload().len()` rather than underflowing. This offset accounts for IPv4
  options and IPv6 extension headers uniformly without re-summing per-layer
  lengths.
- The **fallback** covers an implausible IP length field — notably hardware
  offload (TSO/GRO) where IPv4 `total_len` is 0, and IPv6 jumbograms
  (`payload_length` 0). There the on-wire length is unknowable from the header, so
  we use the captured payload length, exactly as today.

For a well-formed **full** frame the derived value equals `tcp.payload().len()`
(etherparse already bounds `tcp.payload()` by the IP length field, so Ethernet
padding and total-length bounding are handled identically). Existing full-frame
decodes — including the parity test and the `payload_len` assertions in the
decoder unit tests — are unchanged.

## Acceptance criteria → verification

| Criterion | Test |
| --- | --- |
| Short-snaplen frame with full headers decodes with correct on-wire `payload_len` | New decoder unit test: build a full IPv4/TCP frame with N payload bytes, pass `frame = &full[..header_end]` and `wire_len = full.len()`; assert decoded `payload_len == N`. |
| Frame whose captured bytes do not cover the full TCP header is `Truncated` | New decoder unit test: pass a frame cut inside the TCP header (data-offset claims options that are not captured) with `wire_len > caplen`; assert `Skipped(Truncated)`. |
| `conns`/`metrics` on a `-s 64` capture match the full-snaplen equivalent | New ingest integration test: a fixture pair (full vs header-only) over the same synthetic flow yields identical decoded `Segment` streams (and thus identical engine output). |
| Existing full-frame behavior and skip counts unchanged | Existing `parse.rs`, `parity.rs`, `drift.rs`, and decoder unit tests stay green; full-frame `payload_len` assertions unchanged. |
| Both faucets agree on the new behavior | `parity.rs`: a header-only fixture produces identical `Item` streams and skip counts from both faucets. |

## Risks

- **etherparse lax-parse semantics.** The design relies on `from_ip` surfacing a
  `Len` error for header shortage and on `tcp.slice()` being a sub-slice of the
  passed IP slice. Both are verified against etherparse 0.20.2 source. A version
  bump must re-verify (covered by the unit tests above).
- **Implausible length fields.** For a *full* offload/jumbo frame the captured
  length equals the on-wire length, so the fallback reports the same value as
  today. The one genuinely lossy corner is a frame that is **both** offloaded
  (`total_len`/`payload_length` 0) **and** snaplen-cut: there the IP length field
  is unusable *and* the captured payload is truncated, so `payload_len` is
  undercounted with no signal. No header source can recover the on-wire length in
  that case; this is an accepted limitation, not a bug to fix. It requires hardware
  offload and a short snaplen simultaneously, which is rare.
- **IP-header truncation at tiny snaplens.** A snaplen that cuts inside the IP
  header still classifies `Truncated` because `from_ip` returns `Len` and the
  frame is truncated. A snaplen that cuts inside the *link* header is out of scope
  (see Non-goals).
