//! Engine tuning knobs (design §10.M2/§10.M3, ADR-0006, ADR-0007).

use tcpvisr_core::Nanos;

use crate::metrics::SeriesCollection;

/// Connection-tracker + metric-derivation configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "four orthogonal collect-<series> timeline gates (state/seq/inflight/rtt); each is an \
              independently unit-tested on/off knob per ADR-0010/0011/0012/0013, not a state flag \
              cluster that would read better as an enum"
)]
pub struct EngineConfig {
    /// Idle gap after which a fresh SYN on the same pair starts a new instance.
    pub dead_after: Nanos,
    /// Minimum backward serial distance that reads as a fresh-ISN reset. Must be `< 2^31`
    /// (no backward serial distance can exceed the midpoint) or the rule is unreachable.
    pub reset_threshold: u32,
    /// Which instances buffer a metric series (M3).
    pub series_collection: SeriesCollection,
    /// Trailing window for `throughput_bps`; must be `> 0` (the engine divides defensively).
    pub throughput_window: Nanos,
    /// A behind-frontier data segment within this inter-arrival gap is out-of-order, else a
    /// retransmit.
    pub reorder_window: Nanos,
    /// Ceiling on retained samples across the collected series; exceeding it fails fast.
    pub max_samples: usize,
    /// Whether the tracker records a per-segment `StateSample` timeline (M5 replay).
    pub collect_state_timeline: bool,
    /// Whether the tracker records a per-segment `SeqSample` Time/Sequence series (M6 detail).
    pub collect_seq_timeline: bool,
    /// Whether the tracker records a per-segment `InFlightSample` timeline (M7 detail).
    pub collect_inflight_timeline: bool,
    /// Whether the tracker records a per-ack `RttSample` timeline (M8 detail).
    pub collect_rtt_timeline: bool,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            dead_after: Nanos(120_000_000_000),
            reset_threshold: 1 << 30,
            series_collection: SeriesCollection::None,
            throughput_window: Nanos(1_000_000_000),
            reorder_window: Nanos(3_000_000),
            max_samples: 10_000_000,
            collect_state_timeline: false,
            collect_seq_timeline: false,
            collect_inflight_timeline: false,
            collect_rtt_timeline: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::SeriesCollection;

    #[test]
    fn defaults_match_spec() {
        let c = EngineConfig::default();
        assert_eq!(c.series_collection, SeriesCollection::None);
        assert_eq!(c.throughput_window, Nanos(1_000_000_000));
        assert_eq!(c.reorder_window, Nanos(3_000_000));
        assert_eq!(c.max_samples, 10_000_000);
        assert!(!c.collect_state_timeline);
        assert!(!c.collect_seq_timeline);
        // M2 defaults unchanged:
        assert_eq!(c.dead_after, Nanos(120_000_000_000));
        assert_eq!(c.reset_threshold, 1 << 30);
    }

    #[test]
    fn inflight_timeline_defaults_off() {
        let c = EngineConfig::default();
        assert!(!c.collect_inflight_timeline);
    }

    #[test]
    fn rtt_timeline_defaults_off() {
        let c = EngineConfig::default();
        assert!(!c.collect_rtt_timeline);
    }

    #[test]
    fn config_is_copy() {
        let c = EngineConfig::default();
        let d = c; // Copy
        assert_eq!(c, d);
    }
}
