//! libpcap live interface capture (M11, ADR-0003/0016). Feeds the *same* `decode_frame` as the
//! replay faucet, so live and replay produce identical `Item`s for identical bytes. Behind the
//! `live` Cargo feature; the default build stays libpcap-free.
//!
//! This is the impure boundary that owns the clock (ADR-0002): it stamps `Segment`s from libpcap's
//! packet timestamps and injects `Item::Tick` from the host wall clock on read-timeout, so the pure
//! engine advances idle/decay from data alone.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use pcap::{Active, Capture, Device, Precision};
use tcpvisr_core::{Item, NameObservation, Nanos};

use crate::SkipCounts;
use crate::decode::{DecodeOutcome, SkipReason, decode_frame};
use crate::link::LinkType;

/// Read-timeout for the capture handle, in milliseconds. A timeout with no packet drives a `Tick`.
const READ_TIMEOUT_MS: i32 = 100;
/// Default snaplen: full-packet so DNS answers and TCP options decode.
const DEFAULT_SNAPLEN: i32 = 262_144;

/// Options for opening a live capture. `snaplen`/`promisc` are internal knobs with sensible
/// defaults (not CLI-exposed in v0.2 per YAGNI).
#[derive(Debug, Clone)]
pub struct LiveOptions {
    /// Interface name to capture on (e.g. `eth0`).
    pub iface: String,
    /// Optional BPF filter expression.
    pub filter: Option<String>,
    /// Capture length in bytes.
    pub snaplen: i32,
    /// Whether to open the interface in promiscuous mode.
    pub promisc: bool,
}

impl LiveOptions {
    /// Options for `iface` with full-packet snaplen, non-promiscuous, no filter.
    #[must_use]
    pub fn new(iface: impl Into<String>) -> Self {
        Self {
            iface: iface.into(),
            filter: None,
            snaplen: DEFAULT_SNAPLEN,
            promisc: false,
        }
    }
}

/// Whole-capture failures opening or driving a live interface. Per-packet decode problems are
/// counted in [`crate::SkipCounts`], never surfaced here (design §7).
#[derive(Debug, thiserror::Error)]
pub enum LiveError {
    /// The interface could not be opened for capture.
    #[error("opening interface {iface} for capture: {detail}")]
    Open { iface: String, detail: String },
    /// The handle could not be activated (non-privilege reason).
    #[error("activating capture on {iface}: {detail}")]
    Activate { iface: String, detail: String },
    /// The open failed for lack of capture privilege.
    #[error(
        "insufficient privilege to capture on {iface}; grant it with \
         `sudo setcap cap_net_raw,cap_net_admin+eip $(command -v tcp-visr)` or run as root"
    )]
    Privilege { iface: String },
    /// The BPF filter expression could not be installed.
    #[error("installing BPF filter `{expr}`: {detail}")]
    Filter { expr: String, detail: String },
    /// The interface's link type is not one the shared decoder handles.
    #[error(
        "unsupported link type {dlt} on the interface \
         (supports Ethernet, SLL, SLL2, raw IP, null)"
    )]
    UnsupportedLinkType { dlt: u16 },
    /// Enumerating capture interfaces failed.
    #[error("enumerating capture interfaces: {detail}")]
    Interfaces { detail: String },
}

/// A capturable interface, for selection / `--list-interfaces`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfaceInfo {
    pub name: String,
    pub description: Option<String>,
}

/// Enumerates capturable interfaces.
///
/// # Errors
/// Returns [`LiveError::Interfaces`] if libpcap cannot enumerate devices.
pub fn list_interfaces() -> Result<Vec<InterfaceInfo>, LiveError> {
    let devices = Device::list().map_err(|e| LiveError::Interfaces {
        detail: e.to_string(),
    })?;
    Ok(devices
        .into_iter()
        .map(|d| InterfaceInfo {
            name: d.name,
            description: d.desc,
        })
        .collect())
}

/// Maps a libpcap open/activate error to the right [`LiveError`], recognizing the
/// permission-denied case so an unprivileged open reports a privilege problem, not a silent-empty
/// capture (design §7, `DoD`).
fn map_open_error(iface: &str, e: &pcap::Error) -> LiveError {
    let detail = e.to_string();
    let lower = detail.to_lowercase();
    if lower.contains("permission") || lower.contains("not permitted") || lower.contains("root") {
        LiveError::Privilege {
            iface: iface.to_string(),
        }
    } else {
        LiveError::Activate {
            iface: iface.to_string(),
            detail,
        }
    }
}

/// An active live capture handle plus the detected link type and timestamp precision.
pub struct LiveCapture {
    cap: Capture<Active>,
    link: LinkType,
    /// `true` when the handle supplies nanosecond timestamps; `false` for microsecond.
    nanos: bool,
}

impl LiveCapture {
    /// Opens `opts.iface` for capture: snaplen, promiscuous mode, immediate mode, a bounded read
    /// timeout, and nanosecond timestamp precision (falling back to microsecond when the device
    /// cannot supply nano); activates; installs the BPF filter if given; records the link type.
    ///
    /// # Errors
    /// Returns [`LiveError`] if the device cannot be opened/activated, the privilege is
    /// insufficient, the filter is invalid, or the link type is unsupported.
    pub fn open(opts: &LiveOptions) -> Result<Self, LiveError> {
        // Try nanosecond precision first, then microsecond; each attempt rebuilds the inactive
        // handle because the builder methods consume it.
        let (cap, nanos) = match Self::open_with(opts, Precision::Nano) {
            Ok(cap) => (cap, true),
            Err(_) => (Self::open_with(opts, Precision::Micro)?, false),
        };
        let mut cap = cap;
        let dlt = cap.get_datalink().0;
        let dlt =
            u16::try_from(dlt).map_err(|_| LiveError::UnsupportedLinkType { dlt: u16::MAX })?;
        let link = LinkType::from_dlt(dlt).ok_or(LiveError::UnsupportedLinkType { dlt })?;
        if let Some(expr) = &opts.filter {
            cap.filter(expr, true).map_err(|e| LiveError::Filter {
                expr: expr.clone(),
                detail: e.to_string(),
            })?;
        }
        Ok(Self { cap, link, nanos })
    }

    /// Builds and activates the handle at a specific timestamp precision.
    fn open_with(opts: &LiveOptions, precision: Precision) -> Result<Capture<Active>, LiveError> {
        let inactive = Capture::from_device(opts.iface.as_str()).map_err(|e| LiveError::Open {
            iface: opts.iface.clone(),
            detail: e.to_string(),
        })?;
        inactive
            .snaplen(opts.snaplen)
            .promisc(opts.promisc)
            .immediate_mode(true)
            .timeout(READ_TIMEOUT_MS)
            .precision(precision)
            .open()
            .map_err(|e| map_open_error(&opts.iface, &e))
    }

    /// The detected link type.
    #[must_use]
    pub fn link_type(&self) -> LinkType {
        self.link
    }

    /// Runs the capture loop until `stop` is set or the handle ends, calling `on_event` for each
    /// decoded `Segment`/`NameObservation` and injecting an `Item::Tick` on each read-timeout so
    /// idle/decay advance during silence. Per-packet decode problems are counted, never fatal
    /// (design §7). `diagnostic_skips` accumulates the *unexpected* skips (malformed, truncated,
    /// unsupported link/ext-chain, fragments) so the live UI can surface them; ordinary non-TCP
    /// traffic (ARP, UDP, ICMP) is expected on an unfiltered interface and is left out of that
    /// running total (though still tallied in the returned [`SkipCounts`]). Returns the full skip
    /// counts on exit.
    pub fn run(
        mut self,
        mut on_event: impl FnMut(LiveEvent),
        stop: &AtomicBool,
        diagnostic_skips: &AtomicU64,
    ) -> SkipCounts {
        let mut baseline: Option<u64> = None;
        let mut skipped = SkipCounts::default();
        while !stop.load(Ordering::Relaxed) {
            match self.cap.next_packet() {
                Ok(packet) => {
                    let sec = u64::try_from(packet.header.ts.tv_sec).unwrap_or(0);
                    let sub = u64::try_from(packet.header.ts.tv_usec).unwrap_or(0);
                    // Nanosecond precision packs ns in the usec field; microsecond packs us.
                    let abs_ns = sec * 1_000_000_000 + if self.nanos { sub } else { sub * 1_000 };
                    let ts = normalize_ts(abs_ns, &mut baseline);
                    match decode_frame(self.link, ts, packet.data, packet.header.len) {
                        DecodeOutcome::Decoded(seg) => {
                            on_event(LiveEvent::Item(Item::Segment(seg)));
                        }
                        DecodeOutcome::Names(obs) => {
                            for o in obs {
                                on_event(LiveEvent::Name(o));
                            }
                        }
                        DecodeOutcome::Skipped(reason) => {
                            skipped.record(reason);
                            if reason != SkipReason::NonTcp {
                                diagnostic_skips.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    }
                }
                Err(pcap::Error::TimeoutExpired) => {
                    let ts = normalize_ts(wall_now_ns(), &mut baseline);
                    on_event(LiveEvent::Item(Item::Tick(ts)));
                }
                Err(_) => break, // handle ended or a fatal read error
            }
        }
        skipped
    }
}

/// One event from the live capture loop: an engine `Item` (a `Segment` or an injected `Tick`), or a
/// DNS name observation that rides beside the item stream (M10 seam).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveEvent {
    Item(Item),
    Name(NameObservation),
}

/// Normalizes an absolute nanosecond timestamp to `Nanos` relative to the first observed timestamp
/// (the replay faucet's baseline-relative convention). A later-arriving earlier stamp saturates to
/// 0 rather than going negative.
pub(crate) fn normalize_ts(abs_ns: u64, baseline: &mut Option<u64>) -> Nanos {
    let base = *baseline.get_or_insert(abs_ns);
    Nanos(abs_ns.saturating_sub(base))
}

/// The host wall clock (`CLOCK_REALTIME`) in nanoseconds since the epoch — the same domain libpcap
/// stamps packets with, so an injected tick shares one time axis with segments.
pub(crate) fn wall_now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn privilege_error_names_the_setcap_fix() {
        let e = LiveError::Privilege {
            iface: "eth0".into(),
        };
        let msg = e.to_string();
        assert!(msg.contains("eth0"));
        assert!(msg.contains("cap_net_raw"), "names the setcap fix: {msg}");
    }

    #[test]
    fn default_options_full_snaplen_non_promiscuous() {
        let o = LiveOptions::new("eth0");
        assert_eq!(o.iface, "eth0");
        assert_eq!(o.snaplen, 262_144);
        assert!(!o.promisc);
        assert!(o.filter.is_none());
    }

    #[test]
    fn map_open_error_recognizes_permission_denied() {
        let e = pcap::Error::PcapError("eth0: You don't have permission to capture".into());
        assert!(matches!(
            map_open_error("eth0", &e),
            LiveError::Privilege { .. }
        ));
    }

    #[test]
    fn map_open_error_defaults_to_activate() {
        let e = pcap::Error::PcapError("No such device exists".into());
        assert!(matches!(
            map_open_error("eth0", &e),
            LiveError::Activate { .. }
        ));
    }

    #[test]
    fn normalize_ts_is_relative_to_first_timestamp() {
        let mut base = None;
        assert_eq!(normalize_ts(1_000_000_500, &mut base), Nanos(0));
        assert_eq!(normalize_ts(1_000_000_900, &mut base), Nanos(400));
        // a later-arriving earlier stamp saturates to 0, never negative
        assert_eq!(normalize_ts(1_000_000_100, &mut base), Nanos(0));
    }
}
