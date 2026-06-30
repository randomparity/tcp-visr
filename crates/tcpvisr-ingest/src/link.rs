//! Link-layer stripping. Each branch reduces a frame to the IP bytes `decode_frame` parses.

use crate::decode::SkipReason;

/// The link types M1 supports (design §3.1). DLT values per libpcap/tcpdump.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkType {
    Ethernet,
    LinuxSll,
    LinuxSll2,
    RawIp,
    Null,
}

impl LinkType {
    /// Maps a libpcap data-link type (DLT) number to a supported `LinkType`.
    #[must_use]
    pub fn from_dlt(dlt: u16) -> Option<Self> {
        match dlt {
            1 => Some(Self::Ethernet),
            0 => Some(Self::Null),
            101 => Some(Self::RawIp),
            113 => Some(Self::LinuxSll),
            276 => Some(Self::LinuxSll2),
            _ => None,
        }
    }
}

pub(crate) enum Stripped<'a> {
    Ip(&'a [u8]),
    Skip(SkipReason),
}

const ETHERTYPE_IPV4: u16 = 0x0800;
const ETHERTYPE_IPV6: u16 = 0x86DD;
const VLAN_8021Q: u16 = 0x8100;
const VLAN_8021AD: u16 = 0x88A8;

fn be16(b: &[u8], off: usize) -> Option<u16> {
    b.get(off..off + 2)
        .map(|s| u16::from_be_bytes([s[0], s[1]]))
}

pub(crate) fn strip_link(link: LinkType, frame: &[u8]) -> Stripped<'_> {
    match link {
        // Raw IP and NULL dispatch on the IP version nibble (handled by `from_ip` downstream),
        // not on the OS-dependent DLT_NULL address-family word (M1 spec, ADR-0005).
        LinkType::RawIp => Stripped::Ip(frame),
        LinkType::Null => match frame.get(4..) {
            Some(ip) => Stripped::Ip(ip),
            None => Stripped::Skip(SkipReason::Malformed),
        },
        LinkType::LinuxSll2 => strip_after_ethertype(frame, 0, 20),
        LinkType::LinuxSll => strip_after_ethertype(frame, 14, 16),
        LinkType::Ethernet => strip_ethernet(frame),
    }
}

// SLL/SLL2: an ethertype field at `et_off`, IP payload starting at `ip_off`.
fn strip_after_ethertype(frame: &[u8], et_off: usize, ip_off: usize) -> Stripped<'_> {
    match be16(frame, et_off) {
        Some(ETHERTYPE_IPV4 | ETHERTYPE_IPV6) => match frame.get(ip_off..) {
            Some(ip) => Stripped::Ip(ip),
            None => Stripped::Skip(SkipReason::Malformed),
        },
        Some(_) => Stripped::Skip(SkipReason::NonTcp),
        None => Stripped::Skip(SkipReason::Malformed),
    }
}

fn strip_ethernet(frame: &[u8]) -> Stripped<'_> {
    let mut off = 12; // ethertype position after dst(6) + src(6)
    loop {
        match be16(frame, off) {
            Some(ETHERTYPE_IPV4 | ETHERTYPE_IPV6) => {
                return match frame.get(off + 2..) {
                    Some(ip) => Stripped::Ip(ip),
                    None => Stripped::Skip(SkipReason::Malformed),
                };
            }
            // 802.1Q / 802.1ad tag: 2-byte TCI then a 2-byte inner ethertype.
            Some(VLAN_8021Q | VLAN_8021AD) => off += 4,
            Some(_) => return Stripped::Skip(SkipReason::NonTcp),
            None => return Stripped::Skip(SkipReason::Malformed),
        }
    }
}
