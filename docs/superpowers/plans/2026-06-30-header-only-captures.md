# Header-Only Capture Ingest Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make short-snaplen ("header-only", `tcpdump -s 64`) captures fully
analyzable by treating a frame as usable when its headers are captured and by
deriving `payload_len` from the IP length field instead of captured payload bytes.

**Architecture:** Move the truncation policy out of the two faucets and into the
single shared `decode_frame`, which gains an on-wire-length parameter. The decoder
classifies a header-layer byte-shortage as `Truncated` only when the frame was
snaplen-cut, and derives `payload_len` from the IP length field (with a checked
header offset and a captured-length fallback for implausible/offload frames).

**Tech Stack:** Rust 1.88 (edition 2024), `etherparse` 0.20.2 (lax slicer),
`pcap-parser` 0.17 (replay faucet), `pcap` (libpcap faucet, `live` feature). No new
dependencies. Ingest crate only — no `tcpvisr-core`/engine/CLI changes.

Derived from [the spec](../specs/2026-06-30-header-only-captures.md) and
[ADR-0008](../../adr/0008-usable-frame-headers-present.md).

## Global Constraints

- **Toolchain = MSRV = 1.88**; edition 2024. No new dependencies.
- **Lint policy (workspace `[lints]`)**: no `unwrap`/`expect`/`panic!`/`println!`/
  `eprintln!`/`process::exit`/`#[allow]`/`todo!`/`dbg!`/`unimplemented!` in non-test
  code. `clippy::pedantic` is `warn`; CI runs `-D warnings`, so pedantic findings
  (including `cast_possible_truncation`) must be resolved. `clippy.toml` allows
  `unwrap`/`expect`/`panic` in `#[test]` bodies; pedantic cast lints are **not**
  relaxed, so prefer `u32::try_from(_).unwrap_or(u32::MAX)` over `as u32`.
- **Ingest layer only** (issue #23): `tcpvisr-core::Segment`, the engine, and the
  CLI are untouched. `Segment.payload_len` already carries the on-wire length; only
  its derivation changes.
- **One decoder** (design §3.2, ADR-0008): the usable/skip decision and
  `payload_len` derivation live in `decode_frame`; the faucets only pass bytes +
  on-wire length and record the returned outcome. No duplicated truncation logic.
- **Parity is load-bearing** (ADR-0005): both faucets must produce identical `Item`
  streams and skip counts. Both pass the same on-wire length so they cannot drift.
- **No behavior change for full frames**: existing decode/parse/parity/drift tests
  and full-frame `payload_len` assertions stay green and unchanged.
- **Conventional Commits**, imperative ≤72-char subject, every commit body ends with
  `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- **Guardrails before every commit**: `cargo fmt --all --check` ·
  `cargo clippy --all-targets --all-features -- -D warnings` · `cargo test
  --workspace`. Before the final push also run `cargo test -p tcpvisr-ingest
  --features live` and `cargo deny check` (these gate CI individually). `libpcap-dev`
  must be installed for `--all-features` / `--features live`.

---

## File Structure

```
crates/tcpvisr-ingest/src/decode.rs    # MODIFY: decode_frame gains wire_len; new
                                        #   classify_no_tcp + derive_payload_len helpers;
                                        #   truncation classification; new unit tests
crates/tcpvisr-ingest/src/replay.rs     # MODIFY: drop caplen guard; pass origlen as wire_len
crates/tcpvisr-ingest/src/libpcap.rs    # MODIFY: drop caplen guard; pass header.len; prune import
crates/tcpvisr-ingest/tests/parse.rs    # MODIFY: add header-only-vs-full equivalence test
crates/tcpvisr-ingest/tests/parity.rs   # MODIFY: add both-faucets header-only parity test
crates/tcpvisr-ingest/tests/support/mod.rs  # (reuse existing helpers; no change expected)
```

`decode_frame` signature changes from
`decode_frame(link: LinkType, ts: Nanos, frame: &[u8]) -> DecodeOutcome`
to
`decode_frame(link: LinkType, ts: Nanos, frame: &[u8], wire_len: u32) -> DecodeOutcome`.

---

## Task 1: Thread on-wire length through `decode_frame` and classify header truncation

Move truncation classification into the decoder. Add `wire_len`, remove the
faucet `caplen < origlen` guards, and map a header-layer byte-shortage to
`Truncated` only when the frame was snaplen-cut. `payload_len` derivation is
unchanged in this task (still `tcp.payload().len()`); Task 2 changes it.

**Files:**
- Modify: `crates/tcpvisr-ingest/src/decode.rs` (signature, imports, `classify_no_tcp`, `from_ip` arm, test call sites, new tests)
- Modify: `crates/tcpvisr-ingest/src/replay.rs` (`process_packet`, `handle_block` call sites)
- Modify: `crates/tcpvisr-ingest/src/libpcap.rs` (loop body, import)

**Interfaces:**
- Produces: `decode_frame(link: LinkType, ts: Nanos, frame: &[u8], wire_len: u32) -> DecodeOutcome`. `frame` is captured bytes (`frame.len() == caplen`); `wire_len` is the on-wire frame length (`origlen`). `wire_len as usize > frame.len()` ⇒ truncated.

- [ ] **Step 1: Write the failing tests in `decode.rs`**

Add a test helper and two tests to the `#[cfg(test)] mod tests` block. The helper
calls `decode_frame` with `wire_len == frame.len()` (a full frame):

`ipv4_tcp_with_options()` is an **existing** helper already defined in the
decode.rs `#[cfg(test)] mod tests` block (it builds a SYN with MSS/WS/SACK-perm/TS
options, a 40-byte TCP header) — reuse it as-is; do not redefine it.

```rust
fn decode_full(link: LinkType, frame: &[u8]) -> DecodeOutcome {
    let wire_len = u32::try_from(frame.len()).unwrap_or(u32::MAX);
    decode_frame(link, Nanos(0), frame, wire_len)
}

#[test]
fn tcp_header_cut_on_truncated_frame_is_truncated() {
    // Full IPv4/TCP SYN with options; cut the captured bytes inside the TCP
    // header so the data offset points past the captured slice. wire_len is the
    // full on-wire length, so this is a snaplen-cut frame.
    let full = ipv4_tcp_with_options();
    let cut = &full[..full.len() - 8]; // drop 8 bytes from inside the TCP options
    let wire_len = u32::try_from(full.len()).unwrap_or(u32::MAX);
    assert_eq!(
        decode_frame(LinkType::RawIp, Nanos(0), cut, wire_len),
        DecodeOutcome::Skipped(SkipReason::Truncated)
    );
}
```

Also update the existing tests that call `decode_frame(LinkType::X, Nanos(0), &frame)`
to call `decode_full(LinkType::X, &frame)` instead (`decodes_ipv4_tcp_syn`,
`parses_tcp_options`, `non_tcp_is_skipped_non_tcp`, `garbage_is_malformed`,
`decodes_ipv6_tcp`, `ipv6_fragmented_tcp_is_skipped`, `decodes_through_ethernet`,
`decodes_through_sll2`). The updated `garbage_is_malformed` (`&[0xff, 0x00, 0x01]`,
`wire_len == caplen == 3`) already covers the full-frame-Malformed path, so no
separate test is added for it — only the new `Truncated` path is genuinely new.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p tcpvisr-ingest --lib decode`
Expected: compile error (`decode_frame` takes 3 args, the new tests pass 4 and use
`decode_full` which does not exist yet). Compile failure is the failing-test signal
for a signature change.

- [ ] **Step 3: Change the signature and classification in `decode.rs`**

Add imports near the top of the file (alongside `use etherparse::err::Layer;`):

```rust
use etherparse::err::ip::LaxHeaderSliceError;
use etherparse::err::packet::SliceError;
```

Change the signature and the two early failure arms:

```rust
#[must_use]
pub fn decode_frame(link: LinkType, ts: Nanos, frame: &[u8], wire_len: u32) -> DecodeOutcome {
    let truncated = wire_len as usize > frame.len();
    let ip = match strip_link(link, frame) {
        Stripped::Ip(bytes) => bytes,
        Stripped::Skip(reason) => return DecodeOutcome::Skipped(reason),
    };
    let sliced = match LaxSlicedPacket::from_ip(ip) {
        Ok(sliced) => sliced,
        Err(LaxHeaderSliceError::Len(_)) if truncated => {
            return DecodeOutcome::Skipped(SkipReason::Truncated);
        }
        Err(_) => return DecodeOutcome::Skipped(SkipReason::Malformed),
    };
    let Some(net) = sliced.net.as_ref() else {
        return DecodeOutcome::Skipped(SkipReason::Malformed);
    };
    // ... src_ip/dst_ip/fragmenting match and the `if fragmenting` guard stay unchanged ...
```

Replace the no-TCP `else` arm so it delegates to a helper:

```rust
    let Some(TransportSlice::Tcp(tcp)) = sliced.transport.as_ref() else {
        return DecodeOutcome::Skipped(classify_no_tcp(&sliced, truncated));
    };
```

Add the helper (after `decode_frame`):

```rust
/// Classifies a frame that parsed an IP header but produced no TCP transport.
///
/// A byte-shortage (`Len`) on a snaplen-cut frame means the TCP header itself was
/// cut off -> `Truncated`. Otherwise the existing mappings apply: an unsupported
/// IPv6 extension chain -> `UnsupportedExtChain`, anything else -> `NonTcp`.
fn classify_no_tcp(sliced: &LaxSlicedPacket<'_>, truncated: bool) -> SkipReason {
    match sliced.stop_err {
        Some((SliceError::Len(_), _)) if truncated => SkipReason::Truncated,
        Some((_, Layer::Ipv6ExtHeader | Layer::IpHeader)) => SkipReason::UnsupportedExtChain,
        _ => SkipReason::NonTcp,
    }
}
```

Leave `payload_len: u32::try_from(tcp.payload().len()).unwrap_or(u32::MAX)` unchanged
for now (Task 2 replaces it).

- [ ] **Step 4: Update the faucets to drop the guard and pass `wire_len`**

In `crates/tcpvisr-ingest/src/replay.rs`, change `process_packet` to drop the
`caplen` parameter and the guard, passing `origlen` as `wire_len`:

```rust
fn process_packet(
    state: &mut State,
    abs_ns: u64,
    origlen: u32,
    data: &[u8],
    path: &Path,
    sink: &mut dyn FnMut(&Item),
) -> Result<(), IngestError> {
    let baseline = *state.baseline.get_or_insert(abs_ns);
    let ts = Nanos(abs_ns.saturating_sub(baseline));
    let link = state.link_type.ok_or_else(|| IngestError::Container {
        path: path.to_path_buf(),
        detail: "packet before any interface description".to_owned(),
    })?;
    match decode_frame(link, ts, data, origlen) {
        DecodeOutcome::Decoded(seg) => sink(&Item::Segment(seg)),
        DecodeOutcome::Skipped(reason) => state.skipped.record(reason),
    }
    Ok(())
}
```

Update the two `process_packet` call sites in `handle_block` to drop the `caplen`
argument:
- Legacy block: `process_packet(state, abs_ns, b.origlen, b.data, path, sink)`
- Enhanced packet block: `process_packet(state, abs_ns, epb.origlen, epb.data, path, sink)`

In `crates/tcpvisr-ingest/src/libpcap.rs`, remove the `if header.caplen < header.len`
guard and pass `header.len` as `wire_len`:

```rust
                let header = packet.header;
                let sec = u64::try_from(header.ts.tv_sec).unwrap_or(0);
                let usec = u64::try_from(header.ts.tv_usec).unwrap_or(0);
                let abs_ns = sec * 1_000_000_000 + usec * 1_000;
                let base = *baseline.get_or_insert(abs_ns);
                let ts = Nanos(abs_ns.saturating_sub(base));
                match decode_frame(link, ts, packet.data, header.len) {
                    DecodeOutcome::Decoded(seg) => items.push(Item::Segment(seg)),
                    DecodeOutcome::Skipped(reason) => skipped.record(reason),
                }
```

Then change the `libpcap.rs` import from
`use crate::decode::{DecodeOutcome, SkipReason, decode_frame};` to
`use crate::decode::{DecodeOutcome, decode_frame};` (`SkipReason` is no longer named
directly; leaving it triggers an unused-import warning, which CI fails on).

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p tcpvisr-ingest --lib`
Expected: PASS (new `tcp_header_cut_on_truncated_frame_is_truncated`,
`short_full_garbage_stays_malformed`, and all updated existing tests).

Run: `cargo test -p tcpvisr-ingest --test parse --test drift`
Expected: PASS — `skips_non_tcp_and_truncated_and_counts_them` still sees
`truncated == 1` (the 8-byte truncated record now classifies `Truncated` inside the
decoder), and committed fixtures are unchanged.

- [ ] **Step 6: Guardrails**

Run: `cargo fmt --all --check`
Run: `cargo clippy --all-targets --all-features -- -D warnings`
Run: `cargo test --workspace`
Run: `cargo test -p tcpvisr-ingest --features live` (parity + libpcap paths)
Expected: all green.

- [ ] **Step 7: Commit**

```bash
git add crates/tcpvisr-ingest/src/decode.rs crates/tcpvisr-ingest/src/replay.rs crates/tcpvisr-ingest/src/libpcap.rs
git commit -m "feat(ingest): move truncation policy into the shared decoder

decode_frame now takes the on-wire frame length and classifies a snaplen-cut
header as Truncated; the faucets drop their duplicated caplen<origlen guard.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 2: Derive `payload_len` from the IP length field

Replace the captured-bytes payload length with an on-wire length derived from the
IP length field, using a checked sub-slice offset for the TCP header and a
captured-length fallback for implausible/offload frames.

**Files:**
- Modify: `crates/tcpvisr-ingest/src/decode.rs` (`derive_payload_len` + `subslice_offset` helpers; use in `Segment`; new tests)

**Interfaces:**
- Consumes: `decode_frame(.., wire_len)` from Task 1.
- Produces: `derive_payload_len(ip: &[u8], net: &LaxNetSlice<'_>, tcp: &TcpSlice<'_>) -> u32` returning the on-wire TCP payload length.

- [ ] **Step 1: Write the failing tests in `decode.rs`**

Add tests to the `#[cfg(test)] mod tests` block. The first proves a header-only
frame (full headers, payload cut) reports the on-wire payload length:

```rust
fn ipv4_tcp_with_payload(payload: &[u8]) -> Vec<u8> {
    let mut buf = Vec::new();
    PacketBuilder::ipv4([10, 0, 0, 1], [10, 0, 0, 2], 64)
        .tcp(1234, 80, 1000, 64240)
        .ack(1)
        .write(&mut buf, payload)
        .unwrap();
    buf
}

#[test]
fn header_only_frame_reports_onwire_payload_len() {
    // Full frame has 100 payload bytes; the captured frame keeps only the headers
    // (cut all payload). wire_len is the full on-wire length.
    let full = ipv4_tcp_with_payload(&[0x5a; 100]);
    let header_end = full.len() - 100; // IPv4(20) + TCP(20)
    let captured = &full[..header_end];
    let wire_len = u32::try_from(full.len()).unwrap_or(u32::MAX);
    let DecodeOutcome::Decoded(seg) = decode_frame(LinkType::RawIp, Nanos(0), captured, wire_len)
    else {
        panic!("expected decode of a header-only frame");
    };
    assert_eq!(seg.payload_len, 100);
    assert_eq!(seg.flow.dst_port, 80);
}

#[test]
fn full_frame_payload_len_matches_captured() {
    // Deriving from the IP length field yields the same value as the captured
    // payload for a well-formed full frame.
    let full = ipv4_tcp_with_payload(&[0x11; 42]);
    let DecodeOutcome::Decoded(seg) = decode_full(LinkType::RawIp, &full) else {
        panic!("expected decode");
    };
    assert_eq!(seg.payload_len, 42);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p tcpvisr-ingest --lib header_only_frame_reports_onwire_payload_len`
Expected: FAIL — `payload_len` is `0` (captured payload is empty in the truncated
frame) instead of `100`, because derivation still reads `tcp.payload().len()`.

- [ ] **Step 3: Add the derivation helpers and use them**

Add `LaxNetSlice` and `TcpSlice` are already imported (`use etherparse::{..,
LaxNetSlice, .., TcpSlice, ..}`). Add the helpers after `classify_no_tcp`:

```rust
/// On-wire TCP payload length, derived from the IP length field rather than the
/// captured payload (which is short for a snaplen-cut frame). Falls back to the
/// captured payload length when the IP length field is implausible — e.g. hardware
/// offload (`total_len`/`payload_length` 0) or an IPv6 jumbogram (ADR-0008).
fn derive_payload_len(ip: &[u8], net: &LaxNetSlice<'_>, tcp: &TcpSlice<'_>) -> u32 {
    let captured = u32::try_from(tcp.payload().len()).unwrap_or(u32::MAX);
    let onwire_ip_len = match net {
        LaxNetSlice::Ipv4(v4) => usize::from(v4.header().total_len()),
        LaxNetSlice::Ipv6(v6) => 40 + usize::from(v6.header().payload_length()),
        LaxNetSlice::Arp(_) => return captured, // ARP is skipped before decode reaches here
    };
    let Some(tcp_offset) = subslice_offset(ip, tcp.slice()) else {
        return captured;
    };
    let need = tcp_offset + tcp.slice().len();
    if need <= ip.len() && onwire_ip_len >= need {
        u32::try_from(onwire_ip_len - need).unwrap_or(u32::MAX)
    } else {
        captured
    }
}

/// Byte offset of `inner` within `outer`, when `inner` borrows from `outer`'s
/// allocation (ADR-0008 invariant: etherparse's lax slices borrow the input).
/// Returns `None` if `inner` does not lie within `outer`, so callers fall back
/// instead of trusting an out-of-range offset.
fn subslice_offset(outer: &[u8], inner: &[u8]) -> Option<usize> {
    let outer_start = outer.as_ptr() as usize;
    let inner_start = inner.as_ptr() as usize;
    let offset = inner_start.checked_sub(outer_start)?;
    (offset <= outer.len()).then_some(offset)
}
```

Change the `Segment` construction field from
`payload_len: u32::try_from(tcp.payload().len()).unwrap_or(u32::MAX),`
to
`payload_len: derive_payload_len(ip, net, tcp),`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p tcpvisr-ingest --lib`
Expected: PASS — `header_only_frame_reports_onwire_payload_len` now sees `100`,
`full_frame_payload_len_matches_captured` sees `42`, and the existing
`decodes_ipv4_tcp_syn` (0), `decodes_ipv6_tcp` (2) assertions still hold.

- [ ] **Step 5: Guardrails**

Run: `cargo fmt --all --check`
Run: `cargo clippy --all-targets --all-features -- -D warnings`
Run: `cargo test --workspace`
Run: `cargo test -p tcpvisr-ingest --features live`
Expected: all green. (Watch for `cast_possible_truncation`/`cast_sign_loss` on the
pointer `as usize` casts; they are lossless on 64-bit and are not flagged, but if a
pedantic lint fires, resolve it rather than suppressing.)

- [ ] **Step 6: Commit**

```bash
git add crates/tcpvisr-ingest/src/decode.rs
git commit -m "feat(ingest): derive payload_len from the IP length field

Header-only captures now report the on-wire TCP payload length instead of the
truncated captured length, with a checked offset and a captured-length fallback
for offload/jumbo frames.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 3: Equivalence and parity tests for header-only captures

Prove at the container/faucet level that a header-only capture yields the same
decoded segments as the equivalent full-snaplen capture, and that both faucets
agree on a header-only capture (acceptance criteria 3 and "both faucets agree").

**Files:**
- Modify: `crates/tcpvisr-ingest/tests/parse.rs` (pure-faucet equivalence test)
- Modify: `crates/tcpvisr-ingest/tests/parity.rs` (both-faucets parity test, `live`-gated)

**Interfaces:**
- Consumes: `support::{Pkt, ipv4_tcp_syn, legacy_pcap, write_temp, DLT_RAW}` (existing fixture builders); `tcpvisr_ingest::{parse_file, parse_file_libpcap}`.

- [ ] **Step 1: Write the failing equivalence test in `parse.rs`**

Build a one-record full capture and a header-only variant of the *same* on-wire
frame (same `orig_len`, captured bytes truncated to the headers), parse both with
the pure faucet, and assert identical decoded items. Add a payload-bearing builder
inline in the test since `support` only has a zero-payload SYN:

```rust
#[test]
fn header_only_capture_decodes_like_full_snaplen() {
    use etherparse::PacketBuilder;

    // One IPv4/TCP data segment with 80 payload bytes, raw-IP link.
    let mut full = Vec::new();
    PacketBuilder::ipv4([10, 0, 0, 1], [10, 0, 0, 2], 64)
        .tcp(1234, 80, 1000, 64240)
        .ack(1)
        .write(&mut full, &[0x42; 80])
        .unwrap();
    let orig_len = full.len() as u32;
    let header_end = full.len() - 80; // headers only

    let full_cap = legacy_pcap(DLT_RAW, &[Pkt::new(TS, full.clone())]);
    let hdr_cap = legacy_pcap(
        DLT_RAW,
        &[Pkt::truncated(TS, full[..header_end].to_vec(), orig_len)],
    );

    let full_parsed = parse_file(&write_temp("hdr_full.pcap", &full_cap)).unwrap();
    let hdr_parsed = parse_file(&write_temp("hdr_only.pcap", &hdr_cap)).unwrap();

    assert_eq!(full_parsed.items, hdr_parsed.items, "items differ");
    assert_eq!(full_parsed.skipped, hdr_parsed.skipped, "skip counts differ");
    assert_eq!(full_parsed.skipped.total(), 0, "nothing should be skipped");
    // The header-only segment must carry the *non-zero on-wire* payload length
    // (acceptance criterion 3), not the truncated captured length.
    assert_eq!(hdr_parsed.items.len(), 1);
    let tcpvisr_core::Item::Segment(seg) = &hdr_parsed.items[0] else {
        panic!("expected a decoded segment");
    };
    assert_eq!(seg.payload_len, 80);
}
```

If `TS`, `parse_file`, `legacy_pcap`, `Pkt`, `write_temp`, or `DLT_RAW` are not
already imported in `parse.rs`, add them to the existing `use` lines (check the
top of the file before editing).

- [ ] **Step 2: Run the test to verify it fails or passes for the right reason**

Run: `cargo test -p tcpvisr-ingest --test parse header_only_capture_decodes_like_full_snaplen`
Expected: PASS once Tasks 1–2 are in (this test is the integration-level proof).

Then **verify it is a real regression guard** (fail-first): temporarily reinstate
the old gate by adding `if data.len() < origlen as usize { state.skipped.record(
crate::decode::SkipReason::Truncated); return Ok(()); }` at the top of
`process_packet` in `replay.rs`, re-run the test, and confirm it FAILS (`items`
differ and `hdr_parsed.skipped.truncated == 1`). Remove the temporary gate and
confirm PASS again. This proves the test would have caught the pre-fix behavior.

- [ ] **Step 3: Write the both-faucets parity test in `parity.rs`**

Add a `live`-gated test asserting both faucets decode a header-only capture
identically:

```rust
#[test]
fn parity_for_header_only_capture() {
    use etherparse::PacketBuilder;

    let mut full = Vec::new();
    PacketBuilder::ipv4([10, 0, 0, 1], [10, 0, 0, 2], 64)
        .tcp(1234, 80, 1000, 64240)
        .ack(1)
        .write(&mut full, &[0x42; 80])
        .unwrap();
    let orig_len = full.len() as u32;
    let header_end = full.len() - 80;

    let bytes = legacy_pcap(
        DLT_RAW,
        &[Pkt::truncated(TS, full[..header_end].to_vec(), orig_len)],
    );
    let path = write_temp("par_hdr_only.pcap", &bytes);
    let pure = parse_file(&path).expect("pure-Rust faucet");
    let lib = parse_file_libpcap(&path).expect("libpcap faucet");
    assert_eq!(pure.items, lib.items, "items differ");
    assert_eq!(pure.skipped, lib.skipped, "skip counts differ");
    assert_eq!(pure.skipped.total(), 0, "nothing should be skipped");
}
```

`parity.rs` already imports `DLT_RAW`, `Pkt`, `legacy_pcap`, `write_temp`,
`parse_file`, `parse_file_libpcap`, and defines `const TS`. Confirm before editing.

- [ ] **Step 4: Run the parity test**

Run: `cargo test -p tcpvisr-ingest --features live --test parity parity_for_header_only_capture`
Expected: PASS — both faucets read the same `orig_len` and captured bytes and
produce identical single-segment streams with zero skips.

- [ ] **Step 5: Full guardrails**

Run: `cargo fmt --all --check`
Run: `cargo clippy --all-targets --all-features -- -D warnings`
Run: `cargo test --workspace`
Run: `cargo test -p tcpvisr-ingest --features live`
Run: `cargo deny check`
Expected: all green.

- [ ] **Step 6: Commit**

```bash
git add crates/tcpvisr-ingest/tests/parse.rs crates/tcpvisr-ingest/tests/parity.rs
git commit -m "test(ingest): header-only capture decodes like full snaplen

Pure-faucet equivalence (header-only vs full capture of the same frame) and
both-faucets parity on a header-only capture.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Self-Review

**Spec coverage:**
- "Move truncation policy into the one decoder" → Task 1 (signature + faucet guard removal).
- "Classify header truncation as Truncated" → Task 1 (`classify_no_tcp`, `from_ip` Len arm).
- "Derive payload_len from the IP length field" with checked offset + fallback → Task 2.
- Acceptance: short-snaplen full-headers → correct payload_len → Task 2 test + Task 3 integration. Header-cut → Truncated → Task 1 test. conns/metrics == full snaplen → Task 3 equivalence (identical Segment stream ⇒ identical engine output). Full-frame unchanged → Tasks 1–2 keep existing assertions; guardrails each commit. Both faucets agree → Task 3 parity test.

**Placeholder scan:** every code step shows the exact code; no TBD/TODO/"handle
edge cases".

**Type consistency:** `decode_frame(link, ts, frame, wire_len)`,
`classify_no_tcp(&LaxSlicedPacket, bool)`, `derive_payload_len(&[u8], &LaxNetSlice,
&TcpSlice) -> u32`, `subslice_offset(&[u8], &[u8]) -> Option<usize>` are used
consistently across tasks. `decode_full` is a test-only helper defined in Task 1 and
reused in Task 2.

**Risks / rollback:** the change is additive at the decoder boundary and reversible
by reverting the three commits; no schema, persistence, or external-service state is
touched. The one accepted limitation (a frame both offloaded and snaplen-cut
undercounts `payload_len`) is documented in the spec/ADR and needs hardware offload
plus a short snaplen simultaneously.
