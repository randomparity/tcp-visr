//! The per-event metric sample (design §4, ADR-0007). Pure, dependency-free; JSON lives in
//! the CLI. One sample is produced per processed `Segment` (design §4.1).

use crate::time::Nanos;

/// The direction of the segment that produced a sample, relative to the connection's origin
/// (ADR-0006). Directional sample fields pertain to this direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleDir {
    OriginToResponder,
    ResponderToOrigin,
}

/// One metric sample (design §4). `in_flight_bytes`, `throughput_bps`, `retransmit`, and
/// `out_of_order` pertain to `dir`; `rtt` is a round-trip measurement carried by the
/// acknowledging segment; `sack` reflects the triggering segment's own options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetricSample {
    pub t: Nanos,
    pub dir: SampleDir,
    pub in_flight_bytes: u64,
    pub throughput_bps: u64,
    pub rtt: Option<Nanos>,
    pub retransmit: bool,
    pub out_of_order: bool,
    pub sack: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::Nanos;

    #[test]
    fn sample_is_copy_and_holds_fields() {
        let s = MetricSample {
            t: Nanos(1_000),
            dir: SampleDir::OriginToResponder,
            in_flight_bytes: 50,
            throughput_bps: 400,
            rtt: Some(Nanos(2_000)),
            retransmit: false,
            out_of_order: false,
            sack: true,
        };
        let copy = s; // Copy, not move
        assert_eq!(copy, s);
        assert_eq!(copy.dir, SampleDir::OriginToResponder);
        assert_ne!(SampleDir::OriginToResponder, SampleDir::ResponderToOrigin);
    }
}
