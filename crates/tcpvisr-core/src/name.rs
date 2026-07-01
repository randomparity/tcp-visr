//! DNS-derived host names and the capture name table (design §6, §10.M10; ADR-0015).

use core::fmt;
use core::net::IpAddr;
use std::collections::HashMap;

use crate::time::Nanos;

/// Longest legal DNS name (RFC 1035); a longer candidate is rejected rather than truncated.
const MAX_HOST_LEN: usize = 253;

/// Maximum distinct IPs a [`NameTable`] retains. A capture taken at a resolver/proxy has an
/// otherwise-unbounded set of distinct answer IPs, so past the cap new IPs are dropped-and-counted
/// rather than risking OOM (design §7; ADR-0015 §2).
pub const NAME_TABLE_CAP: usize = 65_536;

/// A sanitized, bounded host name resolved from capture DNS. Rendered into a terminal, so it is
/// printable-ASCII only (ADR-0015 §2): a DNS name is attacker-controlled input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostName(String);

impl HostName {
    /// Builds a host name from a raw DNS owner name, or `None` if it is empty, longer than
    /// [`MAX_HOST_LEN`], or sanitizes to empty. Strips one trailing root dot, then keeps only
    /// printable ASCII (`0x20..=0x7e`), dropping controls/ESC/DEL and every byte `>= 0x80`.
    #[must_use]
    pub fn new(raw: &str) -> Option<Self> {
        let trimmed = raw.strip_suffix('.').unwrap_or(raw);
        if trimmed.is_empty() || trimmed.len() > MAX_HOST_LEN {
            return None;
        }
        let clean: String = trimmed
            .bytes()
            .filter(|b| (0x20..=0x7e).contains(b))
            .map(char::from)
            .collect();
        if clean.is_empty() {
            None
        } else {
            Some(Self(clean))
        }
    }
}

impl fmt::Display for HostName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for HostName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// One IP→name mapping observed in a DNS answer (design §6; ADR-0015 §2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameObservation {
    pub ts: Nanos,
    pub ip: IpAddr,
    pub name: HostName,
}

/// Latest-wins IP→name map built from capture DNS. Static per capture (cursor-independent) and
/// bounded. Pure: no I/O, no clock. The single resolution seam the live path will also feed.
#[derive(Debug, Clone, Default)]
pub struct NameTable {
    by_ip: HashMap<IpAddr, (Nanos, HostName)>,
    dropped: u64,
}

impl NameTable {
    /// Records an observation, keeping the greatest-`ts` name per IP (ties → last seen). A new IP
    /// once [`NAME_TABLE_CAP`] distinct IPs are held is refused and counted (`dropped`); an
    /// already-present IP still updates so latest-wins keeps working for retained IPs.
    pub fn observe(&mut self, obs: NameObservation) {
        match self.by_ip.get_mut(&obs.ip) {
            Some(slot) => {
                if obs.ts >= slot.0 {
                    *slot = (obs.ts, obs.name);
                }
            }
            None => {
                if self.by_ip.len() >= NAME_TABLE_CAP {
                    self.dropped += 1;
                } else {
                    self.by_ip.insert(obs.ip, (obs.ts, obs.name));
                }
            }
        }
    }

    /// The resolved name for `ip`, or `None`.
    #[must_use]
    pub fn resolve(&self, ip: IpAddr) -> Option<&HostName> {
        self.by_ip.get(&ip).map(|(_, name)| name)
    }

    /// Distinct IPs resolved.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_ip.len()
    }

    /// Whether no name has been resolved.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_ip.is_empty()
    }

    /// New IPs refused after the cap (observability; surfaced as `names capped`).
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use core::net::{IpAddr, Ipv4Addr};
    use proptest::prelude::*;

    #[test]
    fn strips_trailing_root_dot() {
        assert_eq!(HostName::new("a.com.").unwrap().as_ref(), "a.com");
        assert_eq!(HostName::new("a.com").unwrap().as_ref(), "a.com");
    }

    #[test]
    fn drops_control_and_escape_bytes() {
        let h = HostName::new("e\u{1b}[31mvil.com").unwrap();
        assert!(!h.as_ref().contains('\u{1b}'), "no ESC survives");
        assert!(h.as_ref().bytes().all(|b| (0x20..=0x7e).contains(&b)));
    }

    #[test]
    fn drops_non_ascii() {
        assert_eq!(HostName::new("café").unwrap().as_ref(), "caf");
    }

    #[test]
    fn rejects_empty_and_oversize_and_fully_dropped() {
        assert_eq!(HostName::new(""), None);
        assert_eq!(HostName::new("."), None); // trailing dot stripped -> empty
        assert_eq!(HostName::new(&"a".repeat(254)), None); // > 253
        assert_eq!(HostName::new("\u{1b}\u{7f}"), None); // all dropped -> empty
        assert!(HostName::new(&"a".repeat(253)).is_some()); // exactly 253 ok
    }

    proptest! {
        /// Any input yields either `None` or a name that is printable-ASCII and <= 253 bytes.
        #[test]
        fn sanitized_output_is_always_printable_and_bounded(raw in ".*") {
            if let Some(h) = HostName::new(&raw) {
                let s = h.as_ref();
                prop_assert!(!s.is_empty());
                prop_assert!(s.len() <= 253);
                prop_assert!(s.bytes().all(|b| (0x20..=0x7e).contains(&b)));
            }
        }
    }

    fn obs(ts: u64, ip: u8, name: &str) -> NameObservation {
        NameObservation {
            ts: Nanos(ts),
            ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, ip)),
            name: HostName::new(name).unwrap(),
        }
    }

    #[test]
    fn latest_ts_wins_regardless_of_insertion_order() {
        let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        let mut t = NameTable::default();
        t.observe(obs(1, 1, "a.com"));
        t.observe(obs(2, 1, "b.com"));
        assert_eq!(t.resolve(ip).unwrap().as_ref(), "b.com");

        let mut t2 = NameTable::default();
        t2.observe(obs(2, 1, "b.com"));
        t2.observe(obs(1, 1, "a.com")); // earlier ts must NOT overwrite
        assert_eq!(t2.resolve(ip).unwrap().as_ref(), "b.com");
    }

    #[test]
    fn tie_ts_resolves_to_last_seen() {
        let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        let mut t = NameTable::default();
        t.observe(obs(5, 1, "a.com"));
        t.observe(obs(5, 1, "b.com")); // equal ts, later observed wins
        assert_eq!(t.resolve(ip).unwrap().as_ref(), "b.com");
    }

    #[test]
    fn unknown_ip_resolves_none_and_len_counts_distinct() {
        let mut t = NameTable::default();
        t.observe(obs(1, 1, "a.com"));
        t.observe(obs(1, 2, "c.com"));
        assert_eq!(t.len(), 2);
        assert!(!t.is_empty());
        assert!(t.resolve(IpAddr::V4(Ipv4Addr::new(9, 9, 9, 9))).is_none());
    }

    #[test]
    fn cap_drops_and_counts_new_ips_but_updates_present_ones() {
        let mut t = NameTable::default();
        for i in 0..NAME_TABLE_CAP {
            let octets = u32::try_from(i).unwrap().to_be_bytes();
            t.observe(NameObservation {
                ts: Nanos(1),
                ip: IpAddr::V4(Ipv4Addr::new(octets[0], octets[1], octets[2], octets[3])),
                name: HostName::new("x.com").unwrap(),
            });
        }
        assert_eq!(t.len(), NAME_TABLE_CAP);
        assert_eq!(t.dropped(), 0);
        // A brand-new IP is refused and counted.
        let new_ip = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 1));
        t.observe(NameObservation {
            ts: Nanos(9),
            ip: new_ip,
            name: HostName::new("late.com").unwrap(),
        });
        assert_eq!(t.len(), NAME_TABLE_CAP);
        assert_eq!(t.dropped(), 1);
        assert!(t.resolve(new_ip).is_none());
        // An already-present IP still updates after the cap.
        let present = IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0));
        t.observe(NameObservation {
            ts: Nanos(9),
            ip: present,
            name: HostName::new("updated.com").unwrap(),
        });
        assert_eq!(t.resolve(present).unwrap().as_ref(), "updated.com");
    }
}
