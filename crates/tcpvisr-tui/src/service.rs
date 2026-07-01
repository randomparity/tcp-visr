//! Well-known TCP port → service name labels (design §6, M4). Static and I/O-free.

/// Returns the well-known service name for a TCP port, or `None` if unknown.
///
/// A small built-in table of common ports (design philosophy: deterministic, no
/// `/etc/services` read). Unknown ports intentionally yield `None` so the caller
/// renders a blank service cell rather than a bogus label.
#[must_use]
pub fn service_name(port: u16) -> Option<&'static str> {
    let name = match port {
        20 | 21 => "ftp",
        22 => "ssh",
        23 => "telnet",
        25 => "smtp",
        53 => "domain",
        67 | 68 => "dhcp",
        80 => "http",
        110 => "pop3",
        123 => "ntp",
        143 => "imap",
        179 => "bgp",
        389 => "ldap",
        443 => "https",
        445 => "microsoft-ds",
        465 => "smtps",
        587 => "submission",
        631 => "ipp",
        993 => "imaps",
        995 => "pop3s",
        3306 => "mysql",
        3389 => "rdp",
        5432 => "postgresql",
        6379 => "redis",
        8080 => "http-alt",
        8443 => "https-alt",
        _ => return None,
    };
    Some(name)
}

#[cfg(test)]
mod tests {
    use super::service_name;

    #[test]
    fn known_ports_map_to_names() {
        assert_eq!(service_name(22), Some("ssh"));
        assert_eq!(service_name(53), Some("domain"));
        assert_eq!(service_name(80), Some("http"));
        assert_eq!(service_name(443), Some("https"));
        assert_eq!(service_name(5432), Some("postgresql"));
    }

    #[test]
    fn unknown_port_is_none() {
        assert_eq!(service_name(51324), None);
        assert_eq!(service_name(0), None);
    }
}
