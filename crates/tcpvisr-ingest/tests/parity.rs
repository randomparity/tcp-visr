//! Cross-faucet parity (design §3.2, ADR-0005): the pure-Rust and libpcap faucets must
//! produce identical `Item` streams for the same well-formed capture. Gated behind `live`.

#![cfg(feature = "live")]
// Test code: restriction lints relaxed for the parity-assertion helper and small length casts;
// clippy's in-test exemption only reaches `#[test]` bodies, not module helpers/casts here.
#![allow(clippy::expect_used, clippy::cast_possible_truncation)]

mod support;

use support::{
    DLT_EN10MB, DLT_LINUX_SLL, DLT_LINUX_SLL2, DLT_NULL, DLT_RAW, Pkt, ethernet, ipv4_tcp_syn,
    ipv6_hopopt_tcp, ipv6_tcp, legacy_pcap, null, pcapng, sll, sll2, write_temp,
};
use tcpvisr_ingest::{parse_file, parse_file_libpcap};

const TS: u64 = 1_700_000_000_000_000;

fn assert_parity(name: &str, bytes: &[u8]) {
    let path = write_temp(name, bytes);
    let pure = parse_file(&path).expect("pure-Rust faucet");
    let lib = parse_file_libpcap(&path).expect("libpcap faucet");
    assert_eq!(
        pure.link_type, lib.link_type,
        "link type differs for {name}"
    );
    assert_eq!(pure.items, lib.items, "items differ for {name}");
    assert_eq!(pure.skipped, lib.skipped, "skip counts differ for {name}");
}

#[test]
fn parity_for_each_link_type() {
    assert_parity(
        "par_eth_v4.pcap",
        &legacy_pcap(
            DLT_EN10MB,
            &[Pkt::new(TS, ethernet(&ipv4_tcp_syn(1234, 80), false))],
        ),
    );
    assert_parity(
        "par_sll_v4.pcap",
        &legacy_pcap(
            DLT_LINUX_SLL,
            &[Pkt::new(TS, sll(&ipv4_tcp_syn(1, 80), false))],
        ),
    );
    assert_parity(
        "par_sll2_v6.pcap",
        &legacy_pcap(
            DLT_LINUX_SLL2,
            &[Pkt::new(TS, sll2(&ipv6_tcp(1, 443), true))],
        ),
    );
    assert_parity(
        "par_raw_v4.pcap",
        &legacy_pcap(DLT_RAW, &[Pkt::new(TS, ipv4_tcp_syn(1, 22))]),
    );
    assert_parity(
        "par_null_v6.pcap",
        &legacy_pcap(DLT_NULL, &[Pkt::new(TS, null(&ipv6_tcp(1, 443), true))]),
    );
    assert_parity(
        "par_v6ext.pcap",
        &legacy_pcap(
            DLT_EN10MB,
            &[Pkt::new(TS, ethernet(&ipv6_hopopt_tcp(1, 443), true))],
        ),
    );
    assert_parity(
        "par_eth.pcapng",
        &pcapng(
            DLT_EN10MB,
            &[Pkt::new(TS, ethernet(&ipv4_tcp_syn(1, 80), false))],
        ),
    );
}

#[test]
fn both_faucets_classify_truncated() {
    // Excluded from byte-for-byte parity; each faucet must still count the truncated record.
    let good = ipv4_tcp_syn(1, 80);
    let bytes = legacy_pcap(
        DLT_RAW,
        &[
            Pkt::new(TS, good.clone()),
            Pkt::truncated(TS, good[..8].to_vec(), good.len() as u32),
        ],
    );
    let path = write_temp("par_trunc.pcap", &bytes);
    assert_eq!(parse_file(&path).unwrap().skipped.truncated, 1);
    assert_eq!(parse_file_libpcap(&path).unwrap().skipped.truncated, 1);
}

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
