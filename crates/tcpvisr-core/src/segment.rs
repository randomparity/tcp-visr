//! Wire-decoded TCP segment model and the engine-input `Item` (design §3.2, §4).

use core::fmt;

use crate::flow::FlowKey;
use crate::seq::TcpSeq;
use crate::time::Nanos;

/// TCP control bits (design §4). Bit values match the on-wire flags field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TcpFlags(pub u16);

impl TcpFlags {
    pub const FIN: u16 = 0x01;
    pub const SYN: u16 = 0x02;
    pub const RST: u16 = 0x04;
    pub const PSH: u16 = 0x08;
    pub const ACK: u16 = 0x10;
    pub const URG: u16 = 0x20;
    pub const ECE: u16 = 0x40;
    pub const CWR: u16 = 0x80;
    pub const NS: u16 = 0x100;

    #[must_use]
    pub fn fin(self) -> bool {
        self.0 & Self::FIN != 0
    }
    #[must_use]
    pub fn syn(self) -> bool {
        self.0 & Self::SYN != 0
    }
    #[must_use]
    pub fn rst(self) -> bool {
        self.0 & Self::RST != 0
    }
    #[must_use]
    pub fn psh(self) -> bool {
        self.0 & Self::PSH != 0
    }
    #[must_use]
    pub fn ack(self) -> bool {
        self.0 & Self::ACK != 0
    }
    #[must_use]
    pub fn urg(self) -> bool {
        self.0 & Self::URG != 0
    }
    #[must_use]
    pub fn ece(self) -> bool {
        self.0 & Self::ECE != 0
    }
    #[must_use]
    pub fn cwr(self) -> bool {
        self.0 & Self::CWR != 0
    }
    #[must_use]
    pub fn ns(self) -> bool {
        self.0 & Self::NS != 0
    }
}

impl fmt::Display for TcpFlags {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let parts = [
            (self.syn(), "SYN"),
            (self.ack(), "ACK"),
            (self.fin(), "FIN"),
            (self.rst(), "RST"),
            (self.psh(), "PSH"),
            (self.urg(), "URG"),
            (self.ece(), "ECE"),
            (self.cwr(), "CWR"),
            (self.ns(), "NS"),
        ];
        let mut wrote = false;
        for (on, name) in parts {
            if on {
                if wrote {
                    write!(f, "|")?;
                }
                write!(f, "{name}")?;
                wrote = true;
            }
        }
        if !wrote {
            write!(f, ".")?;
        }
        Ok(())
    }
}

/// Parsed summary of the TCP options M1 cares about (design §4).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TcpOptions {
    pub mss: Option<u16>,
    pub window_scale: Option<u8>,
    pub sack_permitted: bool,
    pub timestamp: Option<(u32, u32)>,
    pub sack_blocks: Vec<(TcpSeq, TcpSeq)>,
}

/// One decoded TCP segment as seen on the wire (design §4). `direction` is M2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub ts: Nanos,
    pub flow: FlowKey,
    pub seq: TcpSeq,
    pub ack: TcpSeq,
    pub flags: TcpFlags,
    pub window: u16,
    pub options: TcpOptions,
    pub payload_len: u32,
}

/// Engine input (design §3.2). Replay emits `Segment`; `Tick` is live-only (M11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    Segment(Segment),
    Tick(Nanos),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_display_lists_set_bits_in_order() {
        assert_eq!(
            TcpFlags(TcpFlags::SYN | TcpFlags::ACK).to_string(),
            "SYN|ACK"
        );
    }

    #[test]
    fn flags_display_dot_when_none() {
        assert_eq!(TcpFlags(0).to_string(), ".");
    }
}
