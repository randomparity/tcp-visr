//! The single shared header decoder used by both faucets (design §3.2, ADR-0003).

use etherparse::err::Layer;
use etherparse::err::ip::LaxHeaderSliceError;
use etherparse::err::packet::SliceError;
use etherparse::{LaxNetSlice, LaxSlicedPacket, TcpOptionElement, TcpSlice, TransportSlice};
use tcpvisr_core::{FlowKey, Nanos, Segment, TcpFlags, TcpOptions, TcpSeq};

use crate::link::{LinkType, Stripped, strip_link};

/// Bytes in the fixed IPv6 header (RFC 8200); IPv6 `payload_length` counts after it.
const IPV6_HEADER_LEN: usize = 40;

/// Why a packet was skipped rather than decoded to a `Segment` (design §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SkipReason {
    NonTcp,
    Malformed,
    UnsupportedLinkType,
    Ipv6Fragment,
    UnsupportedExtChain,
    Truncated,
}

/// The outcome of decoding a single frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeOutcome {
    Decoded(Segment),
    Names(Vec<tcpvisr_core::NameObservation>),
    Skipped(SkipReason),
}

/// Decodes a single link-layer frame into a TCP `Segment` or a counted skip reason.
///
/// Both faucets call this; there is no second header-parsing path (design §3.2).
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
    let (src_ip, dst_ip, fragmenting) = match net {
        LaxNetSlice::Ipv4(v4) => {
            let h = v4.header();
            (
                h.source_addr().into(),
                h.destination_addr().into(),
                v4.is_payload_fragmented(),
            )
        }
        LaxNetSlice::Ipv6(v6) => {
            let h = v6.header();
            (
                h.source_addr().into(),
                h.destination_addr().into(),
                v6.is_payload_fragmented(),
            )
        }
        LaxNetSlice::Arp(_) => return DecodeOutcome::Skipped(SkipReason::NonTcp),
    };
    if fragmenting {
        return DecodeOutcome::Skipped(SkipReason::Ipv6Fragment);
    }
    let tcp = match sliced.transport.as_ref() {
        Some(TransportSlice::Tcp(tcp)) => tcp,
        Some(TransportSlice::Udp(udp)) if udp.source_port() == 53 => {
            let obs = crate::dns::parse_dns_answers(ts, udp.payload());
            return if obs.is_empty() {
                DecodeOutcome::Skipped(SkipReason::NonTcp)
            } else {
                DecodeOutcome::Names(obs)
            };
        }
        _ => return DecodeOutcome::Skipped(classify_no_tcp(&sliced, truncated)),
    };
    DecodeOutcome::Decoded(Segment {
        ts,
        flow: FlowKey {
            src_ip,
            dst_ip,
            src_port: tcp.source_port(),
            dst_port: tcp.destination_port(),
        },
        seq: TcpSeq(tcp.sequence_number()),
        ack: TcpSeq(tcp.acknowledgment_number()),
        flags: build_flags(tcp),
        window: tcp.window_size(),
        options: parse_options(tcp),
        payload_len: derive_payload_len(ip, net, tcp),
    })
}

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

/// On-wire TCP payload length, derived from the IP length field rather than the
/// captured payload (which is short for a snaplen-cut frame). Falls back to the
/// captured payload length when the IP length field is implausible — e.g. hardware
/// offload (`total_len`/`payload_length` 0) or an IPv6 jumbogram (ADR-0008).
fn derive_payload_len(ip: &[u8], net: &LaxNetSlice<'_>, tcp: &TcpSlice<'_>) -> u32 {
    let captured = u32::try_from(tcp.payload().len()).unwrap_or(u32::MAX);
    let onwire_ip_len = match net {
        LaxNetSlice::Ipv4(v4) => usize::from(v4.header().total_len()),
        LaxNetSlice::Ipv6(v6) => IPV6_HEADER_LEN + usize::from(v6.header().payload_length()),
        LaxNetSlice::Arp(_) => return captured, // ARP is skipped before decode reaches here
    };
    let Some(tcp_offset) = subslice_offset(ip, tcp.slice()) else {
        return captured;
    };
    // `tcp.slice()` is the whole segment (header + payload); the on-wire payload is
    // everything in the IP packet past the end of the TCP header.
    let tcp_header_len = usize::from(tcp.data_offset()) * 4;
    let need = tcp_offset + tcp_header_len;
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

fn build_flags(tcp: &TcpSlice<'_>) -> TcpFlags {
    let mut bits = 0u16;
    let set = [
        (tcp.fin(), TcpFlags::FIN),
        (tcp.syn(), TcpFlags::SYN),
        (tcp.rst(), TcpFlags::RST),
        (tcp.psh(), TcpFlags::PSH),
        (tcp.ack(), TcpFlags::ACK),
        (tcp.urg(), TcpFlags::URG),
        (tcp.ece(), TcpFlags::ECE),
        (tcp.cwr(), TcpFlags::CWR),
        (tcp.ns(), TcpFlags::NS),
    ];
    for (on, bit) in set {
        if on {
            bits |= bit;
        }
    }
    TcpFlags(bits)
}

fn parse_options(tcp: &TcpSlice<'_>) -> TcpOptions {
    let mut opts = TcpOptions::default();
    for element in tcp.options_iterator() {
        let Ok(element) = element else { break }; // malformed options: keep what parsed
        match element {
            TcpOptionElement::MaximumSegmentSize(v) => opts.mss = Some(v),
            TcpOptionElement::WindowScale(v) => opts.window_scale = Some(v),
            TcpOptionElement::SelectiveAcknowledgementPermitted => opts.sack_permitted = true,
            TcpOptionElement::SelectiveAcknowledgement(first, rest) => {
                opts.sack_blocks.push((TcpSeq(first.0), TcpSeq(first.1)));
                for block in rest.into_iter().flatten() {
                    opts.sack_blocks.push((TcpSeq(block.0), TcpSeq(block.1)));
                }
            }
            TcpOptionElement::Timestamp(a, b) => opts.timestamp = Some((a, b)),
            TcpOptionElement::Noop => {}
        }
    }
    opts
}

#[cfg(test)]
mod tests {
    use super::*;
    use etherparse::{
        IpFragOffset, IpHeaders, IpNumber, Ipv6Extensions, Ipv6FragmentHeader, Ipv6Header,
        PacketBuilder, TcpOptionElement,
    };
    use tcpvisr_core::Nanos;

    fn decode_full(link: LinkType, frame: &[u8]) -> DecodeOutcome {
        let wire_len = u32::try_from(frame.len()).unwrap_or(u32::MAX);
        decode_frame(link, Nanos(0), frame, wire_len)
    }

    fn ipv4_tcp_syn() -> Vec<u8> {
        let mut buf = Vec::new();
        PacketBuilder::ipv4([10, 0, 0, 1], [10, 0, 0, 2], 64)
            .tcp(1234, 80, 1000, 64240)
            .syn()
            .write(&mut buf, &[])
            .unwrap();
        buf
    }

    fn ipv4_tcp_with_options() -> Vec<u8> {
        let mut buf = Vec::new();
        PacketBuilder::ipv4([10, 0, 0, 1], [10, 0, 0, 2], 64)
            .tcp(1234, 80, 1000, 64240)
            .syn()
            .options(&[
                TcpOptionElement::MaximumSegmentSize(1460),
                TcpOptionElement::WindowScale(7),
                TcpOptionElement::SelectiveAcknowledgementPermitted,
                TcpOptionElement::Timestamp(111, 222),
            ])
            .unwrap()
            .write(&mut buf, &[])
            .unwrap();
        buf
    }

    fn ipv4_udp() -> Vec<u8> {
        let mut buf = Vec::new();
        PacketBuilder::ipv4([10, 0, 0, 1], [10, 0, 0, 2], 64)
            .udp(1234, 80)
            .write(&mut buf, &[1, 2, 3])
            .unwrap();
        buf
    }

    fn ipv6_tcp() -> Vec<u8> {
        let mut buf = Vec::new();
        PacketBuilder::ipv6([0x20; 16], [0x21; 16], 64)
            .tcp(5000, 443, 7, 100)
            .ack(9)
            .write(&mut buf, &[0xaa, 0xbb])
            .unwrap();
        buf
    }

    fn ipv6_fragment_tcp() -> Vec<u8> {
        let exts = Ipv6Extensions {
            fragment: Some(Ipv6FragmentHeader {
                next_header: IpNumber::TCP,
                fragment_offset: IpFragOffset::ZERO,
                more_fragments: true, // first of several -> is_payload_fragmented() is true
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
            .unwrap();
        buf
    }

    fn ethernet(ip: &[u8], v6: bool) -> Vec<u8> {
        let mut f = vec![0, 0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 1];
        f.extend_from_slice(if v6 { &[0x86, 0xDD] } else { &[0x08, 0x00] });
        f.extend_from_slice(ip);
        f
    }

    fn sll2(ip: &[u8], v6: bool) -> Vec<u8> {
        let mut f = Vec::new();
        f.extend_from_slice(if v6 { &[0x86, 0xDD] } else { &[0x08, 0x00] }); // protocol type
        f.extend_from_slice(&[0, 0]); // reserved
        f.extend_from_slice(&[0, 0, 0, 1]); // interface index
        f.extend_from_slice(&[0, 1]); // ARPHRD type
        f.push(0); // packet type
        f.push(6); // link-layer address length
        f.extend_from_slice(&[0; 8]); // link-layer address
        f.extend_from_slice(ip); // header is 20 bytes total
        f
    }

    #[test]
    fn decodes_ipv4_tcp_syn() {
        let frame = ipv4_tcp_syn();
        match decode_full(LinkType::RawIp, &frame) {
            DecodeOutcome::Decoded(seg) => {
                assert_eq!(seg.flow.src_port, 1234);
                assert_eq!(seg.flow.dst_port, 80);
                assert!(seg.flags.syn() && !seg.flags.ack());
                assert_eq!(seg.window, 64240);
                assert_eq!(seg.payload_len, 0);
            }
            other => panic!("expected decode, got {other:?}"),
        }
    }

    #[test]
    fn parses_tcp_options() {
        let frame = ipv4_tcp_with_options();
        let DecodeOutcome::Decoded(seg) = decode_full(LinkType::RawIp, &frame) else {
            panic!("expected decode");
        };
        assert_eq!(seg.options.mss, Some(1460));
        assert_eq!(seg.options.window_scale, Some(7));
        assert!(seg.options.sack_permitted);
        assert_eq!(seg.options.timestamp, Some((111, 222)));
    }

    #[test]
    fn non_tcp_is_skipped_non_tcp() {
        let frame = ipv4_udp();
        assert_eq!(
            decode_full(LinkType::RawIp, &frame),
            DecodeOutcome::Skipped(SkipReason::NonTcp)
        );
    }

    fn ipv4_udp_dns_response() -> Vec<u8> {
        use simple_dns::rdata::{A, RData};
        use simple_dns::{CLASS, Name, Packet, ResourceRecord};
        let mut p = Packet::new_reply(1);
        p.answers.push(ResourceRecord::new(
            Name::new("example.com").unwrap(),
            CLASS::IN,
            300,
            RData::A(A {
                address: u32::from(core::net::Ipv4Addr::new(93, 184, 216, 34)),
            }),
        ));
        let dns = p.build_bytes_vec().unwrap();
        let mut buf = Vec::new();
        // Server (:53) -> client (:40000): source port 53 marks a response.
        etherparse::PacketBuilder::ipv4([93, 184, 216, 34], [10, 0, 0, 2], 64)
            .udp(53, 40000)
            .write(&mut buf, &dns)
            .unwrap();
        buf
    }

    #[test]
    fn decodes_dns_response_to_names() {
        let frame = ipv4_udp_dns_response();
        match decode_full(LinkType::RawIp, &frame) {
            DecodeOutcome::Names(obs) => {
                assert_eq!(obs.len(), 1);
                assert_eq!(
                    obs[0].ip,
                    core::net::IpAddr::V4(core::net::Ipv4Addr::new(93, 184, 216, 34))
                );
                assert_eq!(obs[0].name.as_ref(), "example.com");
            }
            other => panic!("expected Names, got {other:?}"),
        }
    }

    #[test]
    fn garbage_is_malformed() {
        assert_eq!(
            decode_full(LinkType::RawIp, &[0xff, 0x00, 0x01]),
            DecodeOutcome::Skipped(SkipReason::Malformed)
        );
    }

    #[test]
    fn decodes_ipv6_tcp() {
        let frame = ipv6_tcp();
        let DecodeOutcome::Decoded(seg) = decode_full(LinkType::RawIp, &frame) else {
            panic!("expected decode");
        };
        assert_eq!(seg.flow.dst_port, 443);
        assert_eq!(seg.payload_len, 2);
        assert!(seg.flow.src_ip.is_ipv6());
    }

    #[test]
    fn ipv6_fragmented_tcp_is_skipped() {
        let frame = ipv6_fragment_tcp();
        assert_eq!(
            decode_full(LinkType::RawIp, &frame),
            DecodeOutcome::Skipped(SkipReason::Ipv6Fragment)
        );
    }

    #[test]
    fn decodes_through_ethernet() {
        let frame = ethernet(&ipv4_tcp_syn(), false);
        let DecodeOutcome::Decoded(seg) = decode_full(LinkType::Ethernet, &frame) else {
            panic!("expected decode");
        };
        assert_eq!(seg.flow.dst_port, 80);
    }

    #[test]
    fn decodes_through_sll2() {
        let frame = sll2(&ipv6_tcp(), true);
        let DecodeOutcome::Decoded(seg) = decode_full(LinkType::LinuxSll2, &frame) else {
            panic!("expected decode");
        };
        assert_eq!(seg.flow.dst_port, 443);
    }

    #[test]
    fn tcp_header_cut_on_truncated_frame_is_truncated() {
        // Full IPv4/TCP SYN with options (40-byte TCP header); cut the captured
        // bytes inside the TCP options so the data offset points past the captured
        // slice. wire_len is the full on-wire length, so this is a snaplen-cut frame.
        let full = ipv4_tcp_with_options();
        let cut = &full[..full.len() - 8];
        let wire_len = u32::try_from(full.len()).unwrap_or(u32::MAX);
        assert_eq!(
            decode_frame(LinkType::RawIp, Nanos(0), cut, wire_len),
            DecodeOutcome::Skipped(SkipReason::Truncated)
        );
    }

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
        // Full frame has 100 payload bytes; the captured frame keeps only the
        // headers (cut all payload). wire_len is the full on-wire length.
        let full = ipv4_tcp_with_payload(&[0x5a; 100]);
        let header_end = full.len() - 100; // IPv4(20) + TCP(20)
        let captured = &full[..header_end];
        let wire_len = u32::try_from(full.len()).unwrap_or(u32::MAX);
        let DecodeOutcome::Decoded(seg) =
            decode_frame(LinkType::RawIp, Nanos(0), captured, wire_len)
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

    #[test]
    fn implausible_total_len_falls_back_to_captured() {
        // Hardware offload (TSO/GRO) reports IPv4 total_len == 0. The on-wire length
        // is unknowable, so a full frame must fall back to the captured payload
        // length (here 30) rather than underflow (ADR-0008).
        let mut frame = ipv4_tcp_with_payload(&[0x33; 30]);
        frame[2] = 0; // zero the IPv4 total_len field (bytes 2..4)
        frame[3] = 0;
        let DecodeOutcome::Decoded(seg) = decode_full(LinkType::RawIp, &frame) else {
            panic!("expected decode of an offload-style frame");
        };
        assert_eq!(seg.payload_len, 30);
    }
}
