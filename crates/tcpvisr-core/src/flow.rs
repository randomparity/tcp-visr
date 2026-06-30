//! The TCP 4-tuple as seen on the wire (design §4). Direction is connection-relative (M2).

use core::fmt;
use core::net::IpAddr;

/// TCP 4-tuple. Protocol is implicit (TCP-only). Stored as-seen; not canonicalized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FlowKey {
    pub src_ip: IpAddr,
    pub src_port: u16,
    pub dst_ip: IpAddr,
    pub dst_port: u16,
}

fn write_endpoint(f: &mut fmt::Formatter<'_>, ip: IpAddr, port: u16) -> fmt::Result {
    match ip {
        IpAddr::V4(a) => write!(f, "{a}:{port}"),
        IpAddr::V6(a) => write!(f, "[{a}]:{port}"),
    }
}

impl fmt::Display for FlowKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_endpoint(f, self.src_ip, self.src_port)?;
        write!(f, " -> ")?;
        write_endpoint(f, self.dst_ip, self.dst_port)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn display_v4_uses_colon() {
        let v4 = FlowKey {
            src_ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
            src_port: 1,
            dst_ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            dst_port: 80,
        };
        assert_eq!(v4.to_string(), "127.0.0.1:1 -> 10.0.0.1:80");
    }

    #[test]
    fn display_v6_brackets_address() {
        let v6 = FlowKey {
            src_ip: IpAddr::V6(Ipv6Addr::LOCALHOST),
            src_port: 1,
            dst_ip: IpAddr::V6(Ipv6Addr::LOCALHOST),
            dst_port: 80,
        };
        assert_eq!(v6.to_string(), "[::1]:1 -> [::1]:80");
    }
}
