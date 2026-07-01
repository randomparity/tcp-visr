//! Capture faucets: pcap/pcapng replay and libpcap live capture -> Item stream.

pub mod decode;
pub mod dns;
#[cfg(feature = "live")]
pub mod libpcap;
pub mod link;
pub mod replay;

use std::path::PathBuf;

pub use decode::{DecodeOutcome, SkipReason, decode_frame};
pub use dns::parse_dns_answers;
#[cfg(feature = "live")]
pub use libpcap::parse_file_libpcap;
pub use link::LinkType;
pub use replay::{parse_file, parse_file_visit};

use tcpvisr_core::Item;

/// Counts of packets skipped during a parse, keyed by reason (design §7).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkipCounts {
    pub non_tcp: u64,
    pub malformed: u64,
    pub unsupported_link_type: u64,
    pub ipv6_fragment: u64,
    pub unsupported_ext_chain: u64,
    pub truncated: u64,
}

impl SkipCounts {
    /// Increments the counter for `reason`.
    pub fn record(&mut self, reason: SkipReason) {
        match reason {
            SkipReason::NonTcp => self.non_tcp += 1,
            SkipReason::Malformed => self.malformed += 1,
            SkipReason::UnsupportedLinkType => self.unsupported_link_type += 1,
            SkipReason::Ipv6Fragment => self.ipv6_fragment += 1,
            SkipReason::UnsupportedExtChain => self.unsupported_ext_chain += 1,
            SkipReason::Truncated => self.truncated += 1,
        }
    }

    /// The non-zero per-reason counts, for surfacing why packets were skipped (design §7).
    #[must_use]
    pub fn nonzero(&self) -> Vec<(&'static str, u64)> {
        let all = [
            ("non_tcp", self.non_tcp),
            ("malformed", self.malformed),
            ("unsupported_link_type", self.unsupported_link_type),
            ("ipv6_fragment", self.ipv6_fragment),
            ("unsupported_ext_chain", self.unsupported_ext_chain),
            ("truncated", self.truncated),
        ];
        all.into_iter().filter(|&(_, count)| count > 0).collect()
    }

    /// Total number of skipped packets.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.non_tcp
            + self.malformed
            + self.unsupported_link_type
            + self.ipv6_fragment
            + self.unsupported_ext_chain
            + self.truncated
    }
}

/// The result of parsing a capture file through a faucet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayParse {
    pub items: Vec<Item>,
    pub skipped: SkipCounts,
    pub link_type: LinkType,
}

/// Whole-file ingest failures. Per-packet problems are counted, not errors (design §7).
#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    #[error("opening capture {path}: {source} (check the path and read permissions)")]
    Open {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("parsing capture container {path}: {detail}")]
    Container { path: PathBuf, detail: String },
    #[error("unsupported link type {dlt} (M1 supports Ethernet, SLL, SLL2, raw IP, null)")]
    UnknownLinkType { dlt: u16 },
    #[error("capture mixes link types across interfaces; M1 supports a single link type")]
    MixedLinkTypes,
}
