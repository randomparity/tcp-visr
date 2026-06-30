//! Behavior of the pure-Rust replay faucet over each link type and skip case.

// Test code: restriction lints relaxed for assertion helpers and small length casts; clippy's
// in-test exemption only reaches `#[test]` bodies, not the module helpers/casts here.
#![allow(clippy::panic, clippy::cast_possible_truncation)]

mod support;

use support::{
    DLT_EN10MB, DLT_LINUX_SLL, DLT_LINUX_SLL2, DLT_NULL, DLT_RAW, Pkt, ethernet, ipv4_tcp_syn,
    ipv4_udp, ipv6_fragment_tcp, ipv6_hopopt_tcp, ipv6_tcp, legacy_pcap, null, pcapng, sll, sll2,
    write_temp,
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
