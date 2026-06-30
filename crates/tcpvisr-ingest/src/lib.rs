//! Capture faucets: pcap/pcapng replay and libpcap live capture -> Item stream.

pub mod decode;
pub mod link;

pub use decode::{DecodeOutcome, SkipReason, decode_frame};
pub use link::LinkType;
