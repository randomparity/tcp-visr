//! Metric derivation on top of the M2 tracker (design §10.M3, ADR-0007). Pure: no I/O, no
//! serde; one `MetricSample` per processed `Segment`.

use crate::conn::ConnId;

/// Which tracked instances buffer a `MetricSample` series.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SeriesCollection {
    /// Derive only lifecycle/scalar state; store no samples (the `conns` path).
    #[default]
    None,
    /// Every instance buffers a series.
    All,
    /// Only the named instance buffers a series (the `metrics --conn N` path).
    Only(ConnId),
}

/// Whole-derivation failures (design §7). Per-segment problems are never errors.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MetricError {
    #[error(
        "metric series exceeded the sample ceiling ({samples} samples > {limit}); \
         raise it with --max-samples or analyze a smaller capture"
    )]
    SampleCeiling { samples: usize, limit: usize },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_ceiling_error_names_count_limit_and_flag() {
        let e = MetricError::SampleCeiling {
            samples: 11,
            limit: 10,
        };
        let msg = e.to_string();
        assert!(msg.contains("11"), "{msg}");
        assert!(msg.contains("10"), "{msg}");
        assert!(msg.contains("--max-samples"), "{msg}");
    }
}
