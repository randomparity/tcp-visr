//! One side of a TCP connection: an IP address and port (design §4, M2).

use core::fmt;
use core::net::IpAddr;

/// A connection endpoint (`ip:port`). Ordered by `(ip, port)` for canonicalization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Endpoint {
    pub ip: IpAddr,
    pub port: u16,
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.ip {
            IpAddr::V4(a) => write!(f, "{a}:{}", self.port),
            IpAddr::V6(a) => write!(f, "[{a}]:{}", self.port),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn display_v4_uses_colon() {
        let e = Endpoint {
            ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            port: 80,
        };
        assert_eq!(e.to_string(), "10.0.0.1:80");
    }

    #[test]
    fn display_v6_brackets_address() {
        let e = Endpoint {
            ip: IpAddr::V6(Ipv6Addr::LOCALHOST),
            port: 443,
        };
        assert_eq!(e.to_string(), "[::1]:443");
    }

    #[test]
    fn ord_is_ip_then_port() {
        let a = Endpoint {
            ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            port: 9,
        };
        let b = Endpoint {
            ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            port: 10,
        };
        assert!(a < b);
    }
}
