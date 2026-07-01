//! Behavior of the pure-Rust replay faucet over each link type and skip case.

// Test code: restriction lints relaxed for assertion helpers and small length casts; clippy's
// in-test exemption only reaches `#[test]` bodies, not the module helpers/casts here.
#![allow(clippy::panic, clippy::cast_possible_truncation)]

mod support;

use support::{
    DLT_EN10MB, DLT_LINUX_SLL, DLT_LINUX_SLL2, DLT_NULL, DLT_RAW, Pkt, ethernet, ipv4_tcp_syn,
    ipv4_udp, ipv4_udp_dns_query, ipv4_udp_dns_response, ipv6_fragment_tcp, ipv6_hopopt_tcp,
    ipv6_tcp, legacy_pcap, null, pcapng, sll, sll2, write_temp,
};
use tcpvisr_core::Item;
use tcpvisr_ingest::{IngestError, LinkType, parse_file};

const TS: u64 = 1_700_000_000_000_000; // microseconds

fn only_segment(items: &[Item]) -> &tcpvisr_core::Segment {
    assert_eq!(items.len(), 1, "expected exactly one decoded segment");
    match &items[0] {
        Item::Segment(s) => s,
        Item::Tick(_) => panic!("replay should not emit ticks"),
    }
}

#[test]
fn parses_ethernet_ipv4() {
    let bytes = legacy_pcap(
        DLT_EN10MB,
        &[Pkt::new(TS, ethernet(&ipv4_tcp_syn(1234, 80), false))],
    );
    let path = write_temp("eth_v4.pcap", &bytes);
    let parsed = parse_file(&path).expect("parse");
    assert_eq!(parsed.link_type, LinkType::Ethernet);
    let seg = only_segment(&parsed.items);
    assert_eq!(seg.flow.dst_port, 80);
    assert!(seg.flags.syn());
    assert_eq!(parsed.skipped.total(), 0);
}

#[test]
fn parses_sll_ipv4() {
    let bytes = legacy_pcap(
        DLT_LINUX_SLL,
        &[Pkt::new(TS, sll(&ipv4_tcp_syn(1, 80), false))],
    );
    let path = write_temp("sll_v4.pcap", &bytes);
    let parsed = parse_file(&path).expect("parse");
    assert_eq!(only_segment(&parsed.items).flow.dst_port, 80);
}

#[test]
fn parses_sll2_ipv6() {
    let bytes = legacy_pcap(
        DLT_LINUX_SLL2,
        &[Pkt::new(TS, sll2(&ipv6_tcp(1, 443), true))],
    );
    let path = write_temp("sll2_v6.pcap", &bytes);
    let parsed = parse_file(&path).expect("parse");
    assert_eq!(only_segment(&parsed.items).flow.dst_port, 443);
}

#[test]
fn parses_raw_ipv4() {
    let bytes = legacy_pcap(DLT_RAW, &[Pkt::new(TS, ipv4_tcp_syn(1, 22))]);
    let path = write_temp("raw_v4.pcap", &bytes);
    let parsed = parse_file(&path).expect("parse");
    assert_eq!(only_segment(&parsed.items).flow.dst_port, 22);
}

#[test]
fn parses_null_ipv6_via_version_nibble() {
    // AF word encodes a v6 family value; the decoder must use the IP nibble, not the AF word.
    let bytes = legacy_pcap(DLT_NULL, &[Pkt::new(TS, null(&ipv6_tcp(1, 443), true))]);
    let path = write_temp("null_v6.pcap", &bytes);
    let parsed = parse_file(&path).expect("parse");
    assert!(only_segment(&parsed.items).flow.dst_ip.is_ipv6());
}

#[test]
fn walks_ipv6_extension_header_chain() {
    let bytes = legacy_pcap(
        DLT_EN10MB,
        &[Pkt::new(TS, ethernet(&ipv6_hopopt_tcp(1, 443), true))],
    );
    let path = write_temp("eth_v6ext.pcap", &bytes);
    let parsed = parse_file(&path).expect("parse");
    assert_eq!(only_segment(&parsed.items).flow.dst_port, 443);
}

#[test]
fn parses_pcapng_container() {
    let bytes = pcapng(
        DLT_EN10MB,
        &[Pkt::new(TS, ethernet(&ipv4_tcp_syn(1, 80), false))],
    );
    let path = write_temp("eth_v4.pcapng", &bytes);
    let parsed = parse_file(&path).expect("parse");
    assert_eq!(only_segment(&parsed.items).flow.dst_port, 80);
}

#[test]
fn relative_timestamps_are_zero_based() {
    let bytes = legacy_pcap(
        DLT_RAW,
        &[
            Pkt::new(TS, ipv4_tcp_syn(1, 80)),
            Pkt::new(TS + 250_000, ipv4_tcp_syn(2, 80)), // +250ms
        ],
    );
    let path = write_temp("raw_two.pcap", &bytes);
    let parsed = parse_file(&path).expect("parse");
    let Item::Segment(first) = &parsed.items[0] else {
        panic!()
    };
    let Item::Segment(second) = &parsed.items[1] else {
        panic!()
    };
    assert_eq!(first.ts.0, 0);
    assert_eq!(second.ts.0, 250_000_000); // 250ms in ns
}

#[test]
fn skips_non_tcp_and_truncated_and_counts_them() {
    let good = ipv4_tcp_syn(1, 80);
    let bytes = legacy_pcap(
        DLT_RAW,
        &[
            Pkt::new(TS, good.clone()),
            Pkt::new(TS, ipv4_udp(1, 80)), // non-TCP
            Pkt::truncated(TS, good[..8].to_vec(), good.len() as u32), // truncated
            Pkt::new(TS, ipv6_fragment_tcp()), // fragmented TCP
        ],
    );
    let path = write_temp("raw_skips.pcap", &bytes);
    let parsed = parse_file(&path).expect("parse");
    assert_eq!(parsed.items.len(), 1);
    assert_eq!(parsed.skipped.non_tcp, 1);
    assert_eq!(parsed.skipped.truncated, 1);
    assert_eq!(parsed.skipped.ipv6_fragment, 1);
}

#[test]
fn pcapng_nanosecond_resolution_scales_timestamps() {
    // if_tsresol = 9 (nanoseconds): a 250-tick gap is 250 ns, not 250 us. Guards the silent
    // microsecond assumption.
    let bytes = support::pcapng_with_tsresol(
        DLT_EN10MB,
        9,
        &[
            Pkt::new(0, ethernet(&ipv4_tcp_syn(1, 80), false)),
            Pkt::new(250, ethernet(&ipv4_tcp_syn(2, 80), false)),
        ],
    );
    let path = write_temp("ns.pcapng", &bytes);
    let parsed = parse_file(&path).expect("parse");
    let Item::Segment(second) = &parsed.items[1] else {
        panic!()
    };
    assert_eq!(second.ts.0, 250);
}

#[test]
fn pcapng_unsupported_subnanosecond_resolution_errors() {
    // if_tsresol = 12 (picoseconds): finer than nanosecond is not representable in Nanos and
    // must surface, not silently mis-scale.
    let bytes = support::pcapng_with_tsresol(
        DLT_EN10MB,
        12,
        &[Pkt::new(0, ethernet(&ipv4_tcp_syn(1, 80), false))],
    );
    let path = write_temp("pico.pcapng", &bytes);
    assert!(matches!(
        parse_file(&path),
        Err(IngestError::Container { .. })
    ));
}

#[test]
fn rejects_mixed_link_types_in_pcapng() {
    // Two interfaces with different link types -> the two faucets could not agree; error out.
    let bytes = support::pcapng_two_interfaces(
        DLT_EN10MB,
        DLT_RAW,
        &[Pkt::new(TS, ethernet(&ipv4_tcp_syn(1, 80), false))],
    );
    let path = write_temp("mixed.pcapng", &bytes);
    match parse_file(&path) {
        Err(IngestError::MixedLinkTypes) => {}
        other => panic!("expected MixedLinkTypes, got {other:?}"),
    }
}

#[test]
fn missing_file_errors_with_open_context() {
    let err = parse_file(std::path::Path::new("/no/such/file.pcap")).unwrap_err();
    assert!(matches!(err, IngestError::Open { .. }));
    assert!(err.to_string().contains("opening capture"));
}

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
    assert_eq!(
        full_parsed.skipped, hdr_parsed.skipped,
        "skip counts differ"
    );
    assert_eq!(full_parsed.skipped.total(), 0, "nothing should be skipped");
    // The header-only segment must carry the *non-zero on-wire* payload length
    // (acceptance criterion 3), not the truncated captured length.
    assert_eq!(only_segment(&hdr_parsed.items).payload_len, 80);
}

#[test]
fn faucet_routes_names_and_excludes_them_from_non_tcp() {
    // One TCP SYN + one DNS response (A example.com -> 93.184.216.34).
    let bytes = legacy_pcap(
        DLT_RAW,
        &[
            Pkt::new(TS, ipv4_tcp_syn(1234, 80)),
            Pkt::new(TS + 1000, ipv4_udp_dns_response()),
        ],
    );
    let path = write_temp("dns_route.pcap", &bytes);
    let parsed = parse_file(&path).expect("parse");
    assert_eq!(parsed.items.len(), 1, "only the SYN is an Item");
    assert_eq!(parsed.names.len(), 1, "the DNS answer is a name");
    assert_eq!(parsed.names[0].name.as_ref(), "example.com");
    assert_eq!(
        parsed.skipped.non_tcp, 0,
        "the DNS packet is used, not skipped"
    );

    // A UDP/53 query (no answers) is counted non_tcp, yields no name.
    let q = legacy_pcap(DLT_RAW, &[Pkt::new(TS, ipv4_udp_dns_query())]);
    let qpath = write_temp("dns_query.pcap", &q);
    let qp = parse_file(&qpath).expect("parse");
    assert_eq!(qp.names.len(), 0);
    assert_eq!(qp.skipped.non_tcp, 1);
}
