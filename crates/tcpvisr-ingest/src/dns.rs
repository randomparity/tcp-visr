//! DNS answer extraction from capture packets (design §6, §10.M10; ADR-0015 §1, §4).
//! Parses UDP/53 responses into IP→name observations. Hostile input: bounded, panic-free.

use simple_dns::Packet;
use simple_dns::rdata::RData;
use tcpvisr_core::{HostName, NameObservation, Nanos};

/// Extracts one [`NameObservation`] per A/AAAA answer in a DNS message. Returns empty on a parse
/// error, a query (no answers), or answers whose names fail sanitization. Never panics.
#[must_use]
pub fn parse_dns_answers(ts: Nanos, payload: &[u8]) -> Vec<NameObservation> {
    let Ok(packet) = Packet::parse(payload) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for answer in &packet.answers {
        let ip = match &answer.rdata {
            RData::A(a) => core::net::Ipv4Addr::from(a.address).into(),
            RData::AAAA(a) => core::net::Ipv6Addr::from(a.address).into(),
            _ => continue,
        };
        if let Some(name) = HostName::new(&answer.name.to_string()) {
            out.push(NameObservation { ts, ip, name });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use core::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    use simple_dns::rdata::{A, AAAA};
    use simple_dns::{CLASS, Name, Packet, ResourceRecord};

    fn response_with(records: Vec<ResourceRecord<'static>>) -> Vec<u8> {
        let mut p = Packet::new_reply(1);
        for r in records {
            p.answers.push(r);
        }
        p.build_bytes_vec().unwrap()
    }

    #[test]
    fn extracts_a_and_aaaa_answers() {
        let bytes = response_with(vec![
            ResourceRecord::new(
                Name::new("example.com").unwrap(),
                CLASS::IN,
                300,
                RData::A(A {
                    address: u32::from(Ipv4Addr::new(93, 184, 216, 34)),
                }),
            ),
            ResourceRecord::new(
                Name::new("v6.example.com").unwrap(),
                CLASS::IN,
                300,
                RData::AAAA(AAAA {
                    address: u128::from(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1)),
                }),
            ),
        ]);
        let obs = parse_dns_answers(Nanos(7), &bytes);
        assert_eq!(obs.len(), 2);
        assert_eq!(obs[0].ip, IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34)));
        assert_eq!(obs[0].name.as_ref(), "example.com");
        assert_eq!(obs[0].ts, Nanos(7));
        assert_eq!(
            obs[1].ip,
            IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1))
        );
    }

    #[test]
    fn query_yields_no_observations() {
        // A reply with no answers (models a query's empty answer section).
        let bytes = response_with(vec![]);
        assert!(parse_dns_answers(Nanos(0), &bytes).is_empty());
    }

    #[test]
    fn garbage_yields_no_observations_without_panicking() {
        assert!(parse_dns_answers(Nanos(0), &[0xff, 0x00, 0x13, 0x37]).is_empty());
        assert!(parse_dns_answers(Nanos(0), &[]).is_empty());
    }
}
