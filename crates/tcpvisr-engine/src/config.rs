//! Engine tuning knobs (design §10.M2/§10.M3, ADR-0006, ADR-0007).

use tcpvisr_core::Nanos;

use crate::metrics::SeriesCollection;

/// How the tracker bounds retained samples (design §7, ADR-0004/0016).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetentionPolicy {
    /// Replay: exceeding `max_samples` fails fast (`MetricError::SampleCeiling`).
    FailFast { max_samples: usize },
    /// Live: evict samples older than `window`; `max_samples` is a hard memory backstop that
    /// evicts the oldest rather than erroring.
    Evict { window: Nanos, max_samples: usize },
}

impl RetentionPolicy {
    /// The hard sample ceiling / memory backstop.
    #[must_use]
    pub fn max_samples(&self) -> usize {
        match self {
            Self::FailFast { max_samples } | Self::Evict { max_samples, .. } => *max_samples,
        }
    }

    /// The time-horizon eviction window, or `None` under fail-fast.
    #[must_use]
    pub fn window(&self) -> Option<Nanos> {
        match self {
            Self::FailFast { .. } => None,
            Self::Evict { window, .. } => Some(*window),
        }
    }
}

/// Connection-tracker + metric-derivation configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "five orthogonal collect-<series> timeline gates (state/seq/inflight/rtt/throughput); \
              each is an independently unit-tested on/off knob per ADR-0010/0011/0012/0013/0014, not \
              a state flag cluster that would read better as an enum"
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
    /// How retained samples are bounded: fail-fast on a ceiling (replay) or time-horizon eviction
    /// (live). The `max_samples` ceiling/backstop lives inside the policy.
    pub retention: RetentionPolicy,
    /// Whether the tracker records a per-segment `StateSample` timeline (M5 replay).
    pub collect_state_timeline: bool,
    /// Whether the tracker records a per-segment `SeqSample` Time/Sequence series (M6 detail).
    pub collect_seq_timeline: bool,
    /// Whether the tracker records a per-segment `InFlightSample` timeline (M7 detail).
    pub collect_inflight_timeline: bool,
    /// Whether the tracker records a per-ack `RttSample` timeline (M8 detail).
    pub collect_rtt_timeline: bool,
    /// Whether the tracker records a per-segment `ThroughputSample` timeline (M9 detail).
    pub collect_throughput_timeline: bool,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            dead_after: Nanos(120_000_000_000),
            reset_threshold: 1 << 30,
            series_collection: SeriesCollection::None,
            throughput_window: Nanos(1_000_000_000),
            reorder_window: Nanos(3_000_000),
            retention: RetentionPolicy::FailFast {
                max_samples: 10_000_000,
            },
            collect_state_timeline: false,
            collect_seq_timeline: false,
            collect_inflight_timeline: false,
            collect_rtt_timeline: false,
            collect_throughput_timeline: false,
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
        assert_eq!(
            c.retention,
            RetentionPolicy::FailFast {
                max_samples: 10_000_000
            }
        );
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
    fn throughput_timeline_defaults_off() {
        let c = EngineConfig::default();
        assert!(!c.collect_throughput_timeline);
    }

    #[test]
    fn config_is_copy() {
        let c = EngineConfig::default();
        let d = c; // Copy
        assert_eq!(c, d);
    }

    #[test]
    fn retention_defaults_to_failfast_ten_million() {
        let c = EngineConfig::default();
        assert_eq!(
            c.retention,
            RetentionPolicy::FailFast {
                max_samples: 10_000_000
            }
        );
        assert_eq!(c.retention.max_samples(), 10_000_000);
        assert_eq!(c.retention.window(), None);
    }

    #[test]
    fn evict_policy_exposes_window_and_backstop() {
        let p = RetentionPolicy::Evict {
            window: Nanos(120_000_000_000),
            max_samples: 2_000_000,
        };
        assert_eq!(p.window(), Some(Nanos(120_000_000_000)));
        assert_eq!(p.max_samples(), 2_000_000);
    }
}
