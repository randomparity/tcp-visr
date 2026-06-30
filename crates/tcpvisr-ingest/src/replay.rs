//! Pure-Rust replay faucet over pcap-parser. Streams records into `decode_frame`.

use std::fs::File;
use std::path::Path;

use pcap_parser::{Block, PcapBlockOwned, PcapError, create_reader};
use tcpvisr_core::{Item, Nanos};

use crate::decode::{DecodeOutcome, decode_frame};
use crate::link::LinkType;
use crate::{IngestError, ReplayParse, SkipCounts};

const MICRO_TICK_NS: u64 = 1_000; // nanoseconds per microsecond tick
const NANO_MAGIC: u32 = 0xa1b2_3c4d; // legacy pcap nanosecond-precision magic

#[derive(Default)]
struct State {
    link_type: Option<LinkType>,
    baseline: Option<u64>,
    nanos_per_tick: u64,
    skipped: SkipCounts,
}

/// Streams a capture's decoded `Item`s into `sink`, holding only the current frame.
///
/// Returns the file's link type and the skip counts. Per-packet problems are counted;
/// only whole-file failures return `Err` (design §7).
///
/// # Errors
///
/// Returns [`IngestError`] if the file cannot be opened, the container cannot be parsed, the
/// link type is unsupported, or the capture mixes link types across interfaces.
pub fn parse_file_visit(
    path: &Path,
    sink: &mut dyn FnMut(&Item),
) -> Result<(LinkType, SkipCounts), IngestError> {
    let file = File::open(path).map_err(|source| IngestError::Open {
        path: path.to_path_buf(),
        source,
    })?;
    let mut reader = create_reader(65_536, file).map_err(|e| IngestError::Container {
        path: path.to_path_buf(),
        detail: format!("{e:?}"),
    })?;
    let mut state = State {
        nanos_per_tick: MICRO_TICK_NS,
        ..State::default()
    };

    loop {
        match reader.next() {
            Ok((offset, block)) => {
                handle_block(&mut state, &block, path, sink)?;
                reader.consume(offset);
            }
            Err(PcapError::Eof) => break,
            Err(PcapError::Incomplete(_)) => {
                // Need more bytes for the next block. `refill` flips the reader's exhausted
                // flag on a zero-byte read, so a finite/truncated file then yields `Eof` or
                // `UnexpectedEof` on the following iteration — this cannot loop forever.
                reader.refill().map_err(|e| IngestError::Container {
                    path: path.to_path_buf(),
                    detail: format!("{e:?}"),
                })?;
            }
            Err(e) => {
                // UnexpectedEof (truncated), BufferTooSmall, ReadError, nom errors -> fatal.
                return Err(IngestError::Container {
                    path: path.to_path_buf(),
                    detail: format!("{e:?}"),
                });
            }
        }
    }

    let link_type = state.link_type.ok_or_else(|| IngestError::Container {
        path: path.to_path_buf(),
        detail: "no interface or packets in capture".to_owned(),
    })?;
    Ok((link_type, state.skipped))
}

/// Collects an entire capture into a `ReplayParse` (for tests and the parity test over
/// bounded fixtures). The CLI uses `parse_file_visit` to stream instead.
///
/// # Errors
///
/// Returns [`IngestError`] for the same whole-file failures as [`parse_file_visit`].
pub fn parse_file(path: &Path) -> Result<ReplayParse, IngestError> {
    let mut items = Vec::new();
    let (link_type, skipped) = parse_file_visit(path, &mut |item| items.push(item.clone()))?;
    Ok(ReplayParse {
        items,
        skipped,
        link_type,
    })
}

fn handle_block(
    state: &mut State,
    block: &PcapBlockOwned<'_>,
    path: &Path,
    sink: &mut dyn FnMut(&Item),
) -> Result<(), IngestError> {
    match block {
        PcapBlockOwned::LegacyHeader(header) => {
            state.link_type = Some(dlt_to_link(header.network.0, path)?);
            state.nanos_per_tick = if header.magic_number == NANO_MAGIC {
                1
            } else {
                MICRO_TICK_NS
            };
            Ok(())
        }
        PcapBlockOwned::Legacy(b) => {
            // Saturating so a crafted timestamp clamps instead of overflow-panicking (debug)
            // or wrapping (release) in a parser that must stay hostile-input-safe (design §7).
            let abs_ns = u64::from(b.ts_sec)
                .saturating_mul(1_000_000_000)
                .saturating_add(u64::from(b.ts_usec).saturating_mul(state.nanos_per_tick));
            process_packet(state, abs_ns, b.origlen, b.data, path, sink)
        }
        PcapBlockOwned::NG(Block::InterfaceDescription(idb)) => {
            let link = dlt_to_link(idb.linktype.0, path)?;
            if let Some(existing) = state.link_type {
                if existing != link {
                    return Err(IngestError::MixedLinkTypes);
                }
            }
            state.link_type = Some(link);
            // Honor the interface's declared timestamp resolution rather than assuming
            // microsecond; surface a resolution we cannot represent in `Nanos` (design §7,
            // "no silent fallbacks") instead of mis-scaling silently.
            let ticks_per_sec = idb.ts_resolution().ok_or_else(|| IngestError::Container {
                path: path.to_path_buf(),
                detail: "unreadable interface timestamp resolution".to_owned(),
            })?;
            state.nanos_per_tick =
                nanos_per_tick(ticks_per_sec).ok_or_else(|| IngestError::Container {
                    path: path.to_path_buf(),
                    detail: format!(
                        "unsupported timestamp resolution ({ticks_per_sec} ticks/s); \
                         M1 supports nanosecond or coarser"
                    ),
                })?;
            Ok(())
        }
        PcapBlockOwned::NG(Block::EnhancedPacket(epb)) => {
            let ticks = (u64::from(epb.ts_high) << 32) | u64::from(epb.ts_low);
            let abs_ns = ticks.saturating_mul(state.nanos_per_tick);
            process_packet(state, abs_ns, epb.origlen, epb.data, path, sink)
        }
        // Section headers and other pcapng blocks (statistics, name resolution, ...) carry no
        // packet for M1 and are ignored.
        PcapBlockOwned::NG(_) => Ok(()),
    }
}

/// Nanoseconds per timestamp tick for a resolution of `ticks_per_sec`, or `None` if it is
/// finer than nanosecond or does not divide evenly into 1e9 (unrepresentable in `Nanos`).
fn nanos_per_tick(ticks_per_sec: u64) -> Option<u64> {
    const NS_PER_SEC: u64 = 1_000_000_000;
    if ticks_per_sec == 0 || NS_PER_SEC % ticks_per_sec != 0 {
        None
    } else {
        Some(NS_PER_SEC / ticks_per_sec)
    }
}

fn dlt_to_link(dlt: i32, _path: &Path) -> Result<LinkType, IngestError> {
    let dlt = u16::try_from(dlt).map_err(|_| IngestError::UnknownLinkType { dlt: u16::MAX })?;
    LinkType::from_dlt(dlt).ok_or(IngestError::UnknownLinkType { dlt })
}

fn process_packet(
    state: &mut State,
    abs_ns: u64,
    origlen: u32,
    data: &[u8],
    path: &Path,
    sink: &mut dyn FnMut(&Item),
) -> Result<(), IngestError> {
    let baseline = *state.baseline.get_or_insert(abs_ns);
    let ts = Nanos(abs_ns.saturating_sub(baseline));
    let link = state.link_type.ok_or_else(|| IngestError::Container {
        path: path.to_path_buf(),
        detail: "packet before any interface description".to_owned(),
    })?;
    match decode_frame(link, ts, data, origlen) {
        DecodeOutcome::Decoded(seg) => sink(&Item::Segment(seg)),
        DecodeOutcome::Skipped(reason) => state.skipped.record(reason),
    }
    Ok(())
}
