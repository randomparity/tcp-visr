//! Deterministic M2 capture-fixture builder. Emits real legacy `.pcap` bytes (Ethernet II,
//! IPv4, TCP) with explicit flags/seq/ack/payload and strictly increasing microsecond
//! timestamps, so fixtures are reviewable as source. No clock or randomness.

#![allow(dead_code)]
// `tcp()` takes 8 params (a full frame spec); module-level relaxation matches M1's
// tests/support pattern (item-level `#[allow]` is avoided — the workspace denies
// `clippy::allow_attributes`).
#![allow(
    clippy::expect_used,
    clippy::cast_possible_truncation,
    clippy::too_many_arguments
)]

use etherparse::{PacketBuilder, TcpOptionElement};

const DLT_EN10MB: u16 = 1;
const C: [u8; 4] = [10, 0, 0, 1]; // client
const S: [u8; 4] = [10, 0, 0, 2]; // server

/// TCP flag bits, OR-combined.
pub mod flag {
    pub const FIN: u16 = 0x01;
    pub const SYN: u16 = 0x02;
    pub const RST: u16 = 0x04;
    pub const ACK: u16 = 0x10;
}

/// One built Ethernet+IPv4+TCP frame with `n` payload bytes.
#[must_use]
pub fn tcp(
    src: [u8; 4],
    dst: [u8; 4],
    sp: u16,
    dp: u16,
    flags: u16,
    seq: u32,
    ack: u32,
    n: usize,
) -> Vec<u8> {
    let mut b = PacketBuilder::ethernet2([2, 0, 0, 0, 0, 1], [2, 0, 0, 0, 0, 2])
        .ipv4(src, dst, 64)
        .tcp(sp, dp, seq, 64240);
    if flags & flag::SYN != 0 {
        b = b.syn();
    }
    if flags & flag::ACK != 0 {
        b = b.ack(ack);
    }
    if flags & flag::FIN != 0 {
        b = b.fin();
    }
    if flags & flag::RST != 0 {
        b = b.rst();
    }
    let mut buf = Vec::new();
    b.write(&mut buf, &vec![0xab; n]).expect("build tcp frame");
    buf
}

/// One Ethernet+IPv4+TCP frame carrying a single SACK block `[left,right)`, `n` payload bytes.
#[must_use]
pub fn tcp_with_sack(
    src: [u8; 4],
    dst: [u8; 4],
    sp: u16,
    dp: u16,
    seq: u32,
    ack: u32,
    left: u32,
    right: u32,
    n: usize,
) -> Vec<u8> {
    let builder = PacketBuilder::ethernet2([2, 0, 0, 0, 0, 1], [2, 0, 0, 0, 0, 2])
        .ipv4(src, dst, 64)
        .tcp(sp, dp, seq, 64240)
        .ack(ack)
        .options(&[TcpOptionElement::SelectiveAcknowledgement(
            (left, right),
            [None, None, None],
        )])
        .expect("valid SACK option");
    let mut buf = Vec::new();
    builder
        .write(&mut buf, &vec![0xab; n])
        .expect("build sack frame");
    buf
}

/// A legacy `.pcap` (microsecond magic, little-endian). `frames[i] = (ts_us, bytes)`.
#[must_use]
pub fn legacy_pcap(frames: &[(u64, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&0xa1b2_c3d4u32.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&4u16.to_le_bytes());
    out.extend_from_slice(&0i32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&65_535u32.to_le_bytes());
    out.extend_from_slice(&u32::from(DLT_EN10MB).to_le_bytes());
    for (ts_us, data) in frames {
        out.extend_from_slice(&((ts_us / 1_000_000) as u32).to_le_bytes());
        out.extend_from_slice(&((ts_us % 1_000_000) as u32).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(data);
    }
    out
}

/// The five committed M2 fixtures (one per `DoD` scenario). Strictly increasing timestamps.
#[must_use]
pub fn fixture_set() -> Vec<(&'static str, Vec<u8>)> {
    use flag::{ACK, FIN, RST, SYN};
    let (cp, sp) = (1234u16, 80u16);
    vec![
        // 1 connection, Established, origin_inferred (no SYN seen).
        (
            "mid_stream.pcap",
            legacy_pcap(&[
                (1_000, tcp(C, S, cp, sp, ACK, 100, 1, 10)),
                (2_000, tcp(S, C, sp, cp, ACK, 1, 110, 20)),
                (3_000, tcp(C, S, cp, sp, ACK, 110, 21, 30)),
            ]),
        ),
        // 1 connection reaching Established, NOT origin_inferred (simultaneous open).
        (
            "sim_open.pcap",
            legacy_pcap(&[
                (1_000, tcp(C, S, cp, sp, SYN, 10, 0, 0)),
                (2_000, tcp(S, C, sp, cp, SYN, 20, 0, 0)),
                (3_000, tcp(C, S, cp, sp, ACK, 11, 21, 0)),
                (4_000, tcp(S, C, sp, cp, ACK, 21, 11, 0)),
            ]),
        ),
        // 1 connection, terminal Reset (mid-stream then RST).
        (
            "mid_rst.pcap",
            legacy_pcap(&[
                (1_000, tcp(C, S, cp, sp, ACK, 100, 1, 40)),
                (2_000, tcp(S, C, sp, cp, ACK, 1, 140, 0)),
                (3_000, tcp(S, C, sp, cp, RST, 1, 0, 0)),
            ]),
        ),
        // 2 connections (instance 0 then 1) for one pair: close, then a new SYN reuse.
        (
            "tuple_reuse.pcap",
            legacy_pcap(&[
                (1_000, tcp(C, S, cp, sp, SYN, 100, 0, 0)),
                (2_000, tcp(S, C, sp, cp, SYN | ACK, 500, 101, 0)),
                (3_000, tcp(C, S, cp, sp, ACK, 101, 501, 0)),
                (4_000, tcp(C, S, cp, sp, FIN | ACK, 101, 501, 0)),
                (5_000, tcp(S, C, sp, cp, FIN | ACK, 501, 102, 0)),
                (6_000, tcp(C, S, cp, sp, SYN, 9000, 0, 0)),
            ]),
        ),
        // 1 connection, 1 instance: seq advances across the u32 boundary (forward wrap).
        (
            "seq_wrap.pcap",
            legacy_pcap(&[
                (1_000, tcp(C, S, cp, sp, ACK, u32::MAX - 100, 1, 50)),
                (2_000, tcp(C, S, cp, sp, ACK, 200, 1, 50)),
                (3_000, tcp(S, C, sp, cp, ACK, 1, 300, 10)),
            ]),
        ),
    ]
}

/// The four M3 metric fixtures (`seq_wrap` is reused from the M2 set). Strictly increasing
/// microsecond timestamps; reorder cases reverse SEQ, not time.
#[must_use]
pub fn metrics_fixture_set() -> Vec<(&'static str, Vec<u8>)> {
    use flag::{ACK, SYN};
    let (cp, sp) = (1234u16, 80u16);
    vec![
        // SYN handshake + data + ACKs: in-flight, handshake RTT, data RTT, throughput.
        (
            "metrics_basic.pcap",
            legacy_pcap(&[
                (1_000, tcp(C, S, cp, sp, SYN, 1000, 0, 0)), // SYN seq=1000
                (2_000, tcp(S, C, sp, cp, SYN | ACK, 5000, 1001, 0)), // SYN-ACK
                (3_000, tcp(C, S, cp, sp, ACK, 1001, 5001, 100)), // data 100B o2r
                (4_000, tcp(S, C, sp, cp, ACK, 5001, 1101, 0)), // ACK of o2r data
            ]),
        ),
        // data, then a retransmit of the same range after a long gap (>= reorder_window).
        (
            "metrics_retransmit.pcap",
            legacy_pcap(&[
                (1_000, tcp(C, S, cp, sp, ACK, 100, 1, 100)), // data 100..200
                (3_001_000, tcp(C, S, cp, sp, ACK, 100, 1, 100)), // retransmit (gap 3.0001s)
            ]),
        ),
        // out-of-order: a behind-frontier segment within reorder_window (1us gap).
        (
            "metrics_ooo.pcap",
            legacy_pcap(&[
                (1_000, tcp(C, S, cp, sp, ACK, 200, 1, 100)), // frontier 300
                (1_001, tcp(C, S, cp, sp, ACK, 100, 1, 100)), // behind, gap 1us -> OOO
            ]),
        ),
        // a segment carrying a SACK block.
        (
            "metrics_sack.pcap",
            legacy_pcap(&[
                (1_000, tcp(C, S, cp, sp, ACK, 100, 1, 50)),
                (2_000, tcp_with_sack(S, C, sp, cp, 1, 151, 200, 260, 0)), // SACK [200,260)
            ]),
        ),
    ]
}
