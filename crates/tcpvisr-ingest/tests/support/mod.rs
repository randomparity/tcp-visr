//! Deterministic capture-fixture builder. Emits real `.pcap`/`.pcapng` bytes per link type so
//! fixtures are reviewable as source (M1 spec). No clock or randomness: all timestamps and
//! sequence numbers are constants.

#![allow(dead_code)]
// each integration-test binary uses a different subset of these helpers.
// Test-fixture support code: `expect` on infallible in-memory builds and small length casts are
// idiomatic here, but these helpers are not `#[test]` fns so clippy's in-test exemption misses
// them. Scope the relaxations to this file only.
#![allow(clippy::expect_used)]
#![allow(clippy::cast_possible_truncation)]

use etherparse::{
    IpFragOffset, IpHeaders, IpNumber, Ipv6Extensions, Ipv6FragmentHeader, Ipv6Header,
    PacketBuilder,
};

// ---- DLT link-type numbers (libpcap/tcpdump) ----
pub const DLT_NULL: u16 = 0;
pub const DLT_EN10MB: u16 = 1;
pub const DLT_RAW: u16 = 101;
pub const DLT_LINUX_SLL: u16 = 113;
pub const DLT_LINUX_SLL2: u16 = 276;

const ETHERTYPE_IPV4: [u8; 2] = [0x08, 0x00];
const ETHERTYPE_IPV6: [u8; 2] = [0x86, 0xDD];

/// Writes fixture bytes to a uniquely-named temp file and returns the path.
#[must_use]
pub fn write_temp(name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("tcpvisr-m1-{name}"));
    std::fs::write(&path, bytes).expect("write temp fixture");
    path
}

/// One packet record. `orig_len` may exceed `data.len()` to model snaplen truncation.
pub struct Pkt {
    pub ts_us: u64,
    pub data: Vec<u8>,
    pub orig_len: u32,
}

impl Pkt {
    #[must_use]
    pub fn new(ts_us: u64, data: Vec<u8>) -> Self {
        let orig_len = data.len() as u32;
        Self {
            ts_us,
            data,
            orig_len,
        }
    }

    /// A record whose captured length is smaller than its original length (truncated).
    #[must_use]
    pub fn truncated(ts_us: u64, data: Vec<u8>, orig_len: u32) -> Self {
        Self {
            ts_us,
            data,
            orig_len,
        }
    }
}

// ---- IP/TCP payload builders (IP-onward bytes) ----

#[must_use]
pub fn ipv4_tcp_syn(src_port: u16, dst_port: u16) -> Vec<u8> {
    let mut buf = Vec::new();
    PacketBuilder::ipv4([10, 0, 0, 1], [10, 0, 0, 2], 64)
        .tcp(src_port, dst_port, 1000, 64240)
        .syn()
        .write(&mut buf, &[])
        .expect("build ipv4 tcp");
    buf
}

#[must_use]
pub fn ipv6_tcp(src_port: u16, dst_port: u16) -> Vec<u8> {
    let mut buf = Vec::new();
    PacketBuilder::ipv6([0x20; 16], [0x21; 16], 64)
        .tcp(src_port, dst_port, 7, 100)
        .ack(9)
        .write(&mut buf, &[0xaa, 0xbb])
        .expect("build ipv6 tcp");
    buf
}

/// IPv6 with a hop-by-hop extension header before TCP (the ext-chain walk case).
#[must_use]
pub fn ipv6_hopopt_tcp(src_port: u16, dst_port: u16) -> Vec<u8> {
    let exts = Ipv6Extensions {
        hop_by_hop_options: Some(
            etherparse::Ipv6RawExtHeader::new_raw(IpNumber::TCP, &[0u8; 6]).expect("hopopt"),
        ),
        ..Ipv6Extensions::default()
    };
    let header = Ipv6Header {
        source: [0x20; 16],
        destination: [0x21; 16],
        hop_limit: 64,
        ..Ipv6Header::default()
    };
    let mut buf = Vec::new();
    PacketBuilder::ip(IpHeaders::Ipv6(header, exts))
        .tcp(src_port, dst_port, 7, 100)
        .write(&mut buf, &[0xcc])
        .expect("build ipv6 hopopt tcp");
    buf
}

/// IPv6 fragmented TCP (first fragment): must be skipped-and-counted.
#[must_use]
pub fn ipv6_fragment_tcp() -> Vec<u8> {
    let exts = Ipv6Extensions {
        fragment: Some(Ipv6FragmentHeader {
            next_header: IpNumber::TCP,
            fragment_offset: IpFragOffset::ZERO,
            more_fragments: true,
            identification: 0xABCD,
        }),
        ..Ipv6Extensions::default()
    };
    let header = Ipv6Header {
        source: [0x20; 16],
        destination: [0x21; 16],
        hop_limit: 64,
        ..Ipv6Header::default()
    };
    let mut buf = Vec::new();
    PacketBuilder::ip(IpHeaders::Ipv6(header, exts))
        .tcp(5000, 443, 7, 100)
        .write(&mut buf, &[0u8; 8])
        .expect("build ipv6 fragment");
    buf
}

#[must_use]
pub fn ipv4_udp(src_port: u16, dst_port: u16) -> Vec<u8> {
    let mut buf = Vec::new();
    PacketBuilder::ipv4([10, 0, 0, 1], [10, 0, 0, 2], 64)
        .udp(src_port, dst_port)
        .write(&mut buf, &[1, 2, 3])
        .expect("build ipv4 udp");
    buf
}

/// A raw IPv4/UDP-53 DNS *response*: `example.com A 93.184.216.34` (source port 53 marks a
/// response, so `decode_frame` emits a `Names` outcome). Duplicated from the `decode.rs` unit-test
/// helper because `src/` unit tests and this integration-test crate cannot share a module.
pub fn ipv4_udp_dns_response() -> Vec<u8> {
    use simple_dns::rdata::{A, RData};
    use simple_dns::{CLASS, Name, Packet, ResourceRecord};
    let mut p = Packet::new_reply(1);
    p.answers.push(ResourceRecord::new(
        Name::new("example.com").expect("dns name"),
        CLASS::IN,
        300,
        RData::A(A {
            address: u32::from(core::net::Ipv4Addr::new(93, 184, 216, 34)),
        }),
    ));
    let dns = p.build_bytes_vec().expect("build dns");
    let mut buf = Vec::new();
    PacketBuilder::ipv4([93, 184, 216, 34], [10, 0, 0, 2], 64)
        .udp(53, 40000)
        .write(&mut buf, &dns)
        .expect("build ipv4 udp dns response");
    buf
}

/// A raw IPv4/UDP DNS *query* (destination port 53, source ≠ 53, empty answer section): decodes to
/// `Skipped(NonTcp)` and yields no name.
pub fn ipv4_udp_dns_query() -> Vec<u8> {
    use simple_dns::Packet;
    let dns = Packet::new_reply(2).build_bytes_vec().expect("build dns");
    let mut buf = Vec::new();
    PacketBuilder::ipv4([10, 0, 0, 2], [93, 184, 216, 34], 64)
        .udp(40000, 53)
        .write(&mut buf, &dns)
        .expect("build ipv4 udp dns query");
    buf
}

// ---- Link-layer wrappers (prepend the link header to IP bytes) ----

#[must_use]
pub fn ethernet(ip: &[u8], v6: bool) -> Vec<u8> {
    let mut f = vec![0, 0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 1];
    f.extend_from_slice(if v6 { &ETHERTYPE_IPV6 } else { &ETHERTYPE_IPV4 });
    f.extend_from_slice(ip);
    f
}

#[must_use]
pub fn sll(ip: &[u8], v6: bool) -> Vec<u8> {
    let mut f = Vec::new();
    f.extend_from_slice(&[0, 0]); // packet type
    f.extend_from_slice(&[0, 1]); // ARPHRD type
    f.extend_from_slice(&[0, 6]); // link-layer address length
    f.extend_from_slice(&[0; 8]); // link-layer address
    f.extend_from_slice(if v6 { &ETHERTYPE_IPV6 } else { &ETHERTYPE_IPV4 }); // protocol (offset 14)
    f.extend_from_slice(ip); // header is 16 bytes total
    f
}

#[must_use]
pub fn sll2(ip: &[u8], v6: bool) -> Vec<u8> {
    let mut f = Vec::new();
    f.extend_from_slice(if v6 { &ETHERTYPE_IPV6 } else { &ETHERTYPE_IPV4 }); // protocol (offset 0)
    f.extend_from_slice(&[0, 0]); // reserved
    f.extend_from_slice(&[0, 0, 0, 1]); // interface index
    f.extend_from_slice(&[0, 1]); // ARPHRD type
    f.push(0); // packet type
    f.push(6); // link-layer address length
    f.extend_from_slice(&[0; 8]); // link-layer address
    f.extend_from_slice(ip); // header is 20 bytes total
    f
}

#[must_use]
pub fn null(ip: &[u8], v6: bool) -> Vec<u8> {
    // 4-byte BSD loopback address family (host order). AF_INET=2; we use a v6 value just to
    // prove the decoder ignores it and dispatches on the IP-version nibble.
    let af: u32 = if v6 { 30 } else { 2 };
    let mut f = af.to_ne_bytes().to_vec();
    f.extend_from_slice(ip);
    f
}

// ---- Container builders ----

/// The canonical committed fixture set: one per link type (incl. SLL2 and an IPv6
/// extension-header chain), a `.pcapng`, and a skip-and-count fixture. All use microsecond
/// timestamps so both faucets agree (M1 spec precision contract).
#[must_use]
pub fn fixture_set() -> Vec<(&'static str, Vec<u8>)> {
    const TS: u64 = 1_700_000_000_000_000;
    let good = ipv4_tcp_syn(1234, 80);
    vec![
        (
            "ethernet.pcap",
            legacy_pcap(
                DLT_EN10MB,
                &[Pkt::new(TS, ethernet(&ipv4_tcp_syn(1234, 80), false))],
            ),
        ),
        (
            "sll.pcap",
            legacy_pcap(
                DLT_LINUX_SLL,
                &[Pkt::new(TS, sll(&ipv4_tcp_syn(1, 80), false))],
            ),
        ),
        (
            "sll2.pcap",
            legacy_pcap(
                DLT_LINUX_SLL2,
                &[Pkt::new(TS, sll2(&ipv6_tcp(1, 443), true))],
            ),
        ),
        (
            "raw_ip.pcap",
            legacy_pcap(DLT_RAW, &[Pkt::new(TS, ipv4_tcp_syn(1, 22))]),
        ),
        (
            "null.pcap",
            legacy_pcap(DLT_NULL, &[Pkt::new(TS, null(&ipv6_tcp(1, 443), true))]),
        ),
        (
            "ipv6_ext.pcap",
            legacy_pcap(
                DLT_EN10MB,
                &[Pkt::new(TS, ethernet(&ipv6_hopopt_tcp(1, 443), true))],
            ),
        ),
        (
            "ethernet.pcapng",
            pcapng(
                DLT_EN10MB,
                &[Pkt::new(TS, ethernet(&ipv4_tcp_syn(1234, 80), false))],
            ),
        ),
        (
            "skip.pcap",
            legacy_pcap(
                DLT_RAW,
                &[
                    Pkt::new(TS, good.clone()),
                    Pkt::new(TS, ipv4_udp(1, 80)),
                    Pkt::truncated(TS, good[..8].to_vec(), good.len() as u32),
                ],
            ),
        ),
    ]
}

/// A legacy `.pcap` (microsecond magic `0xa1b2c3d4`, little-endian).
#[must_use]
pub fn legacy_pcap(dlt: u16, pkts: &[Pkt]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&0xa1b2_c3d4u32.to_le_bytes()); // magic (microsecond)
    out.extend_from_slice(&2u16.to_le_bytes()); // version major
    out.extend_from_slice(&4u16.to_le_bytes()); // version minor
    out.extend_from_slice(&0i32.to_le_bytes()); // thiszone
    out.extend_from_slice(&0u32.to_le_bytes()); // sigfigs
    out.extend_from_slice(&65_535u32.to_le_bytes()); // snaplen
    out.extend_from_slice(&u32::from(dlt).to_le_bytes()); // network
    for pkt in pkts {
        let ts_sec = (pkt.ts_us / 1_000_000) as u32;
        let ts_usec = (pkt.ts_us % 1_000_000) as u32;
        out.extend_from_slice(&ts_sec.to_le_bytes());
        out.extend_from_slice(&ts_usec.to_le_bytes());
        out.extend_from_slice(&(pkt.data.len() as u32).to_le_bytes()); // incl_len
        out.extend_from_slice(&pkt.orig_len.to_le_bytes()); // orig_len
        out.extend_from_slice(&pkt.data);
    }
    out
}

/// A `.pcapng` with one section, one interface (microsecond `if_tsresol` default), and one
/// Enhanced Packet Block per record. Little-endian.
#[must_use]
pub fn pcapng(dlt: u16, pkts: &[Pkt]) -> Vec<u8> {
    let mut out = Vec::new();
    push_shb(&mut out);
    push_idb(&mut out, dlt);
    for pkt in pkts {
        push_epb(&mut out, pkt);
    }
    out
}

/// A `.pcapng` with two interfaces of differing link types (an unsupported, must-error case).
#[must_use]
pub fn pcapng_two_interfaces(dlt0: u16, dlt1: u16, pkts_if0: &[Pkt]) -> Vec<u8> {
    let mut out = Vec::new();
    push_shb(&mut out);
    push_idb(&mut out, dlt0);
    push_idb(&mut out, dlt1);
    for pkt in pkts_if0 {
        push_epb(&mut out, pkt);
    }
    out
}

fn push_block(out: &mut Vec<u8>, block_type: u32, body: &[u8]) {
    let total = 12 + body.len() as u32; // type + len + body + trailing len
    out.extend_from_slice(&block_type.to_le_bytes());
    out.extend_from_slice(&total.to_le_bytes());
    out.extend_from_slice(body);
    out.extend_from_slice(&total.to_le_bytes());
}

fn push_shb(out: &mut Vec<u8>) {
    let mut body = Vec::new();
    body.extend_from_slice(&0x1A2B_3C4Du32.to_le_bytes()); // byte-order magic
    body.extend_from_slice(&1u16.to_le_bytes()); // major
    body.extend_from_slice(&0u16.to_le_bytes()); // minor
    body.extend_from_slice(&(-1i64).to_le_bytes()); // section length (unknown)
    push_block(out, 0x0A0D_0D0A, &body);
}

fn push_idb(out: &mut Vec<u8>, dlt: u16) {
    push_idb_opts(out, dlt, None);
}

fn push_idb_opts(out: &mut Vec<u8>, dlt: u16, tsresol: Option<u8>) {
    let mut body = Vec::new();
    body.extend_from_slice(&dlt.to_le_bytes()); // linktype
    body.extend_from_slice(&0u16.to_le_bytes()); // reserved
    body.extend_from_slice(&65_535u32.to_le_bytes()); // snaplen
    if let Some(resol) = tsresol {
        body.extend_from_slice(&9u16.to_le_bytes()); // option code: if_tsresol
        body.extend_from_slice(&1u16.to_le_bytes()); // option length
        body.push(resol);
        body.extend_from_slice(&[0, 0, 0]); // pad value to 32-bit boundary
        body.extend_from_slice(&0u16.to_le_bytes()); // opt_endofopt code
        body.extend_from_slice(&0u16.to_le_bytes()); // opt_endofopt length
    }
    push_block(out, 0x0000_0001, &body);
}

/// A `.pcapng` whose interface declares an explicit `if_tsresol` (timestamp resolution).
/// `pkts[i].ts_us` is interpreted as ticks in that resolution.
#[must_use]
pub fn pcapng_with_tsresol(dlt: u16, tsresol: u8, pkts: &[Pkt]) -> Vec<u8> {
    let mut out = Vec::new();
    push_shb(&mut out);
    push_idb_opts(&mut out, dlt, Some(tsresol));
    for pkt in pkts {
        push_epb(&mut out, pkt);
    }
    out
}

fn push_epb(out: &mut Vec<u8>, pkt: &Pkt) {
    let mut body = Vec::new();
    body.extend_from_slice(&0u32.to_le_bytes()); // interface id
    let ticks = pkt.ts_us; // microsecond resolution (if_tsresol default = 6)
    body.extend_from_slice(&((ticks >> 32) as u32).to_le_bytes()); // timestamp high
    body.extend_from_slice(&(ticks as u32).to_le_bytes()); // timestamp low
    body.extend_from_slice(&(pkt.data.len() as u32).to_le_bytes()); // captured len
    body.extend_from_slice(&pkt.orig_len.to_le_bytes()); // original len
    body.extend_from_slice(&pkt.data);
    while body.len() % 4 != 0 {
        body.push(0); // pad packet data to 32-bit boundary
    }
    push_block(out, 0x0000_0006, &body);
}
