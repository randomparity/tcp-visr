//! libpcap file faucet (ADR-0005). File reading only; live interface capture is M11.
//!
//! Reads the same capture files as the pure-Rust faucet through libpcap's container parser,
//! feeding the identical `decode_frame`. Used by the parity test to guard the one-decoder
//! promise (design §3.2).

use std::path::Path;

use pcap::Capture;
use tcpvisr_core::{Item, Nanos};

use crate::decode::{DecodeOutcome, decode_frame};
use crate::link::LinkType;
use crate::{IngestError, ReplayParse, SkipCounts};

/// Parses a capture file via libpcap's offline reader.
///
/// # Errors
///
/// Returns [`IngestError`] if libpcap cannot open or read the file or the link type is
/// unsupported.
pub fn parse_file_libpcap(path: &Path) -> Result<ReplayParse, IngestError> {
    let mut cap = Capture::from_file(path).map_err(|e| IngestError::Container {
        path: path.to_path_buf(),
        detail: e.to_string(),
    })?;
    let dlt = cap.get_datalink().0;
    let dlt = u16::try_from(dlt).map_err(|_| IngestError::UnknownLinkType { dlt: u16::MAX })?;
    let link = LinkType::from_dlt(dlt).ok_or(IngestError::UnknownLinkType { dlt })?;

    let mut items = Vec::new();
    let mut names = Vec::new();
    let mut skipped = SkipCounts::default();
    let mut baseline: Option<u64> = None;

    loop {
        match cap.next_packet() {
            Ok(packet) => {
                let header = packet.header;
                // Access timeval fields directly so we don't need to name `libc::timeval`.
                let sec = u64::try_from(header.ts.tv_sec).unwrap_or(0);
                let usec = u64::try_from(header.ts.tv_usec).unwrap_or(0);
                let abs_ns = sec * 1_000_000_000 + usec * 1_000;
                let base = *baseline.get_or_insert(abs_ns);
                let ts = Nanos(abs_ns.saturating_sub(base));
                match decode_frame(link, ts, packet.data, header.len) {
                    DecodeOutcome::Decoded(seg) => items.push(Item::Segment(seg)),
                    DecodeOutcome::Names(obs) => names.extend(obs),
                    DecodeOutcome::Skipped(reason) => skipped.record(reason),
                }
            }
            Err(pcap::Error::NoMorePackets) => break,
            Err(e) => {
                return Err(IngestError::Container {
                    path: path.to_path_buf(),
                    detail: e.to_string(),
                });
            }
        }
    }

    Ok(ReplayParse {
        items,
        skipped,
        link_type: link,
        names,
    })
}
