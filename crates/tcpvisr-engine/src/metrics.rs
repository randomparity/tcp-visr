//! Metric derivation on top of the M2 tracker (design §10.M3, ADR-0007). Pure: no I/O, no
//! serde; one `MetricSample` per processed `Segment`.

use std::collections::VecDeque;

use tcpvisr_core::{MetricSample, Nanos, SampleDir, Segment, TcpSeq};

use crate::config::EngineConfig;
use crate::conn::{ConnId, Connection, Direction};

/// A tracked connection with its derived metric series (design §4's `series`, realized).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionMetrics {
    pub conn: Connection,
    pub series: Vec<MetricSample>,
}

/// The sequence number one past the last byte `S` puts on the wire, counting the SYN/FIN
/// phantom byte (they consume sequence space). Used for in-flight/RTT frontiers, not byte
/// counters.
#[must_use]
pub(crate) fn seq_end(seg: &Segment) -> TcpSeq {
    let phantom = u32::from(seg.flags.syn()) + u32::from(seg.flags.fin());
    TcpSeq(
        seg.seq
            .0
            .wrapping_add(seg.payload_len)
            .wrapping_add(phantom),
    )
}

/// Serial-max: keep the more-forward of `current` and `candidate`.
fn serial_max(current: Option<TcpSeq>, candidate: TcpSeq) -> TcpSeq {
    match current {
        Some(c) if c.serial_gt(candidate) => c,
        _ => candidate,
    }
}

/// `earlier` is serial-≤ `later`.
fn serial_le(earlier: TcpSeq, later: TcpSeq) -> bool {
    earlier == later || earlier.serial_lt(later)
}

#[derive(Default)]
struct DirState {
    snd_nxt: Option<TcpSeq>,
    acked: Option<TcpSeq>,
    frontier: Option<TcpSeq>,
    last_data_ts: Option<Nanos>,
    pending_rtt: VecDeque<(TcpSeq, Nanos)>,
    tput: VecDeque<(Nanos, u32)>,
    tput_max_ts: Option<Nanos>,
}

/// Per-connection metric derivation state (both directions).
pub(crate) struct MetricState {
    dir: [DirState; 2],
}

fn idx(d: Direction) -> usize {
    match d {
        Direction::OriginToResponder => 0,
        Direction::ResponderToOrigin => 1,
    }
}

fn sample_dir(d: Direction) -> SampleDir {
    match d {
        Direction::OriginToResponder => SampleDir::OriginToResponder,
        Direction::ResponderToOrigin => SampleDir::ResponderToOrigin,
    }
}

impl MetricState {
    pub(crate) fn new() -> Self {
        Self {
            dir: [DirState::default(), DirState::default()],
        }
    }

    /// Fold one segment in and produce its `MetricSample` (design §10.M3 derivation contract).
    pub(crate) fn observe(
        &mut self,
        seg: &Segment,
        dir: Direction,
        cfg: &EngineConfig,
    ) -> MetricSample {
        let d = idx(dir);
        let o = 1 - d;
        let f = seg.flags;
        let end = seq_end(seg);
        let is_data = seg.payload_len > 0;
        let consumes_seq = is_data || f.syn() || f.fin();

        // Step 0: ACK advance, computed once against pre-update state.
        let ack_advances = f.ack()
            && self.dir[o].snd_nxt.is_some()
            && match self.dir[o].acked {
                None => true,
                Some(a) => seg.ack.serial_gt(a),
            };

        // Step 2 references (pre-update): frontier + last data ts.
        let frontier = self.dir[d].frontier;
        let last_data_ts = self.dir[d].last_data_ts;

        // Step 1: in-flight.
        let acked_d = *self.dir[d].acked.get_or_insert(seg.seq);
        let snd_d = serial_max(self.dir[d].snd_nxt, end);
        self.dir[d].snd_nxt = Some(snd_d);
        if ack_advances {
            self.dir[o].acked = Some(seg.ack);
        }
        let in_flight_bytes = if serial_le(acked_d, snd_d) {
            u64::from(snd_d.serial_diff(acked_d))
        } else {
            0
        };

        // Step 2: retransmit / out-of-order (data only).
        let (mut retransmit, mut out_of_order) = (false, false);
        if is_data {
            if let Some(fr) = frontier {
                if seg.seq.serial_lt(fr) {
                    let gap = match last_data_ts {
                        Some(prev) => seg.ts.0.saturating_sub(prev.0),
                        None => u64::MAX,
                    };
                    if gap < cfg.reorder_window.0 {
                        out_of_order = true;
                    } else {
                        retransmit = true;
                    }
                }
            }
            self.dir[d].frontier = Some(serial_max(frontier, end));
            self.dir[d].last_data_ts = Some(seg.ts);
        }

        // Step 3: SACK.
        let sack = !seg.options.sack_blocks.is_empty();

        // Step 4: RTT (Karn).
        if retransmit {
            self.dir[d].pending_rtt.clear();
        } else if consumes_seq {
            self.dir[d].pending_rtt.push_back((end, seg.ts));
        }
        let mut rtt = None;
        if ack_advances {
            let pend = &mut self.dir[o].pending_rtt;
            let mut oldest: Option<Nanos> = None;
            while let Some(&(es, ts)) = pend.front() {
                if serial_le(es, seg.ack) {
                    if oldest.is_none() {
                        oldest = Some(ts);
                    }
                    pend.pop_front();
                } else {
                    break;
                }
            }
            rtt = oldest.map(|send_ts| Nanos(seg.ts.0.saturating_sub(send_ts.0)));
        }

        // Step 5: throughput (frozen, window-bounded, defensive divide).
        let throughput_bps = self.throughput(d, seg, cfg);

        MetricSample {
            t: seg.ts,
            dir: sample_dir(dir),
            in_flight_bytes,
            throughput_bps,
            rtt,
            retransmit,
            out_of_order,
            sack,
        }
    }

    /// The wire bytes-outstanding for `dir` (`snd_nxt − acked`, serial, clamped ≥ 0), or `None`
    /// if `dir` has no send frontier or nothing acked yet. Pure read of current state; used by
    /// the M7 in-flight collector to snapshot both directions (ADR-0012 §1).
    pub(crate) fn in_flight(&self, dir: Direction) -> Option<u64> {
        let d = idx(dir);
        let snd = self.dir[d].snd_nxt?;
        let acked = self.dir[d].acked?;
        Some(if serial_le(acked, snd) {
            u64::from(snd.serial_diff(acked))
        } else {
            0
        })
    }

    fn throughput(&mut self, d: usize, seg: &Segment, cfg: &EngineConfig) -> u64 {
        let window = cfg.throughput_window.0;
        if seg.payload_len > 0 {
            self.dir[d].tput.push_back((seg.ts, seg.payload_len));
            self.dir[d].tput_max_ts = Some(match self.dir[d].tput_max_ts {
                Some(m) => Nanos(m.0.max(seg.ts.0)),
                None => seg.ts,
            });
        }
        if window == 0 {
            return 0;
        }
        // Membership is `ts > t - window`, written as `ts + window > t` to avoid u64 underflow
        // when the window extends before t=0 (else the first window of a capture drops its bytes).
        // Use u128 throughout so `ts + window` cannot overflow.
        let w = u128::from(window);
        // Evict entries that can never fall in any future window: an entry is excludable once
        // `ts + window <= max_ts` (the most permissive future window starts at max_ts - window).
        if let Some(max_ts) = self.dir[d].tput_max_ts {
            let max = u128::from(max_ts.0);
            while let Some(&(ts, _)) = self.dir[d].tput.front() {
                if u128::from(ts.0) + w <= max {
                    self.dir[d].tput.pop_front();
                } else {
                    break;
                }
            }
        }
        // Sum bytes in (seg.ts - window, seg.ts]  ==  ts + window > seg.ts  &&  ts <= seg.ts.
        let t = u128::from(seg.ts.0);
        let mut bytes: u128 = 0;
        for &(ts, len) in &self.dir[d].tput {
            let ts = u128::from(ts.0);
            if ts + w > t && ts <= t {
                bytes += u128::from(len);
            }
        }
        let bits = bytes.saturating_mul(8).saturating_mul(1_000_000_000);
        match u64::try_from(bits / w) {
            Ok(v) => v,
            Err(_) => u64::MAX,
        }
    }
}

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

#[cfg(test)]
mod derive_tests {
    use super::*;
    use crate::conn::Direction;
    use core::net::{IpAddr, Ipv4Addr};
    use tcpvisr_core::{FlowKey, Nanos, Segment, TcpFlags, TcpOptions, TcpSeq};

    fn ip(o: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(10, 0, 0, o))
    }

    // Build a segment in O2R (origin 10.0.0.1:1234 -> responder 10.0.0.2:80) or R2O.
    fn seg(flags: u16, seq: u32, ack: u32, len: u32, ts: u64, sack: bool) -> Segment {
        let mut options = TcpOptions::default();
        if sack {
            options.sack_blocks.push((TcpSeq(1), TcpSeq(2)));
        }
        Segment {
            ts: Nanos(ts),
            flow: FlowKey {
                src_ip: ip(1),
                src_port: 1234,
                dst_ip: ip(2),
                dst_port: 80,
            },
            seq: TcpSeq(seq),
            ack: TcpSeq(ack),
            flags: TcpFlags(flags),
            window: 0,
            options,
            payload_len: len,
        }
    }

    const ACK: u16 = TcpFlags::ACK;
    const SYN: u16 = TcpFlags::SYN;

    fn cfg() -> EngineConfig {
        EngineConfig::default()
    }

    #[test]
    fn in_flight_query_matches_sample_and_snapshots_opposite_drain() {
        let mut m = MetricState::new();
        let c = cfg();
        // O2R sends 10 bytes @seq100; own outstanding == 10, query agrees.
        let s1 = m.observe(
            &seg(ACK, 100, 1, 10, 1_000, false),
            Direction::OriginToResponder,
            &c,
        );
        assert_eq!(s1.in_flight_bytes, 10);
        assert_eq!(m.in_flight(Direction::OriginToResponder), Some(10));
        // R2O has no send frontier yet -> None.
        assert_eq!(m.in_flight(Direction::ResponderToOrigin), None);
        // R2O ACK=110 drains O2R: querying O2R now reads 0 (the ack-time drain).
        m.observe(
            &seg(ACK, 1, 110, 0, 2_000, false),
            Direction::ResponderToOrigin,
            &c,
        );
        assert_eq!(m.in_flight(Direction::OriginToResponder), Some(0));
    }

    #[test]
    fn in_flight_grows_with_sent_bytes_and_drains_on_ack() {
        let mut m = MetricState::new();
        let c = cfg();
        // O2R data: 10 bytes, no ack seen for o2r yet.
        let s1 = m.observe(
            &seg(ACK, 100, 1, 10, 1_000, false),
            Direction::OriginToResponder,
            &c,
        );
        assert_eq!(s1.in_flight_bytes, 10);
        assert_eq!(s1.dir, SampleDir::OriginToResponder);
        // R2O segment acks o2r up to 110 -> o2r drained, but THIS sample reports r2o's own.
        let s2 = m.observe(
            &seg(ACK, 1, 110, 0, 2_000, false),
            Direction::ResponderToOrigin,
            &c,
        );
        assert_eq!(s2.in_flight_bytes, 0, "r2o sender has nothing outstanding");
        // Next o2r data shows the drained base: send 5 more from 110.
        let s3 = m.observe(
            &seg(ACK, 110, 1, 5, 3_000, false),
            Direction::OriginToResponder,
            &c,
        );
        assert_eq!(s3.in_flight_bytes, 5, "ack=110 drained the first 10");
    }

    #[test]
    fn in_flight_is_serial_correct_across_u32_wrap() {
        let mut m = MetricState::new();
        let c = cfg();
        let s1 = m.observe(
            &seg(ACK, u32::MAX - 100, 1, 50, 1_000, false),
            Direction::OriginToResponder,
            &c,
        );
        assert_eq!(s1.in_flight_bytes, 50);
        let s2 = m.observe(
            &seg(ACK, 200, 1, 50, 2_000, false),
            Direction::OriginToResponder,
            &c,
        );
        assert_eq!(
            s2.in_flight_bytes, 351,
            "serial diff across the wrap, not a naive subtraction"
        );
    }

    #[test]
    fn ack_before_any_data_in_acked_direction_yields_no_rtt_and_no_advance() {
        let mut m = MetricState::new();
        let c = cfg();
        // First segment is o2r data+ACK=1; r2o has no tracked send -> ack acks nothing.
        let s1 = m.observe(
            &seg(ACK, 5000, 1, 50, 1_000, false),
            Direction::OriginToResponder,
            &c,
        );
        assert_eq!(s1.rtt, None);
        assert_eq!(s1.in_flight_bytes, 50);
    }

    #[test]
    fn rtt_pairs_oldest_acked_send_under_karn() {
        let mut m = MetricState::new();
        let c = cfg();
        // o2r sends A(seq 100,len 100) @1000 and B(seq 200,len 100) @2000.
        m.observe(
            &seg(ACK, 100, 1, 100, 1_000, false),
            Direction::OriginToResponder,
            &c,
        );
        m.observe(
            &seg(ACK, 200, 1, 100, 2_000, false),
            Direction::OriginToResponder,
            &c,
        );
        // r2o cumulative ACK=300 @5000 acks both; RTT pairs the oldest (A @1000).
        let s = m.observe(
            &seg(ACK, 1, 300, 0, 5_000, false),
            Direction::ResponderToOrigin,
            &c,
        );
        assert_eq!(s.rtt, Some(Nanos(4_000)));
    }

    #[test]
    fn karn_drops_rtt_for_retransmitted_range() {
        let mut m = MetricState::new();
        let c = cfg(); // reorder_window = 3ms = 3_000_000 ns
        m.observe(
            &seg(ACK, 100, 1, 100, 1_000, false),
            Direction::OriginToResponder,
            &c,
        ); // A @1us
        // Retransmit of A after a gap >= reorder_window (3ms): gap = 3_001_000 - 1_000 = 3_000_000.
        let r = m.observe(
            &seg(ACK, 100, 1, 100, 3_001_000, false),
            Direction::OriginToResponder,
            &c,
        );
        assert!(
            r.retransmit,
            "behind-frontier re-send after a >= 3ms gap is a retransmit"
        );
        let s = m.observe(
            &seg(ACK, 1, 200, 0, 3_002_000, false),
            Direction::ResponderToOrigin,
            &c,
        );
        assert_eq!(s.rtt, None, "Karn: no RTT after a retransmit");
    }

    #[test]
    fn dup_ack_yields_no_rtt() {
        let mut m = MetricState::new();
        let c = cfg();
        m.observe(
            &seg(ACK, 100, 1, 100, 1_000, false),
            Direction::OriginToResponder,
            &c,
        );
        let s1 = m.observe(
            &seg(ACK, 1, 200, 0, 2_000, false),
            Direction::ResponderToOrigin,
            &c,
        );
        assert_eq!(s1.rtt, Some(Nanos(1_000)));
        // Same ACK again (dup): no new RTT.
        let s2 = m.observe(
            &seg(ACK, 1, 200, 0, 3_000, false),
            Direction::ResponderToOrigin,
            &c,
        );
        assert_eq!(s2.rtt, None);
    }

    #[test]
    fn out_of_order_within_window_not_retransmit() {
        let mut m = MetricState::new();
        let c = cfg();
        m.observe(
            &seg(ACK, 200, 1, 100, 1_000, false),
            Direction::OriginToResponder,
            &c,
        ); // frontier 300
        // Behind-frontier seq 100, gap 1us < 3ms -> out-of-order.
        let s = m.observe(
            &seg(ACK, 100, 1, 100, 1_001, false),
            Direction::OriginToResponder,
            &c,
        );
        assert!(s.out_of_order && !s.retransmit);
    }

    #[test]
    fn reorder_window_boundary_is_retransmit() {
        let mut m = MetricState::new();
        let c = cfg();
        m.observe(
            &seg(ACK, 200, 1, 100, 1_000_000, false),
            Direction::OriginToResponder,
            &c,
        );
        // Gap exactly reorder_window (3ms) -> retransmit (boundary is inclusive-at-or-above).
        let s = m.observe(
            &seg(ACK, 100, 1, 100, 4_000_000, false),
            Direction::OriginToResponder,
            &c,
        );
        assert!(s.retransmit && !s.out_of_order);
    }

    #[test]
    fn sack_flag_reflects_segment_blocks() {
        let mut m = MetricState::new();
        let c = cfg();
        let s = m.observe(
            &seg(ACK, 100, 1, 0, 1_000, true),
            Direction::OriginToResponder,
            &c,
        );
        assert!(s.sack);
    }

    #[test]
    fn syn_consumes_phantom_byte_in_flight() {
        let mut m = MetricState::new();
        let c = cfg();
        let s = m.observe(
            &seg(SYN, 100, 0, 0, 1_000, false),
            Direction::OriginToResponder,
            &c,
        );
        assert_eq!(s.in_flight_bytes, 1, "SYN consumes one sequence byte");
    }

    #[test]
    fn throughput_sums_window_bytes_and_excludes_older() {
        let mut m = MetricState::new();
        let c = cfg(); // 1s window
        // 100 bytes at t=0.
        m.observe(
            &seg(ACK, 0, 1, 100, 0, false),
            Direction::OriginToResponder,
            &c,
        );
        // 100 bytes at t=0.5s: both in the 1s window ending at 0.5s -> 200 bytes -> 1600 bps.
        let s = m.observe(
            &seg(ACK, 100, 1, 100, 500_000_000, false),
            Direction::OriginToResponder,
            &c,
        );
        assert_eq!(s.throughput_bps, 1_600);
        // 100 bytes at t=2s: window (1s,2s] excludes the t=0 and t=0.5s bytes -> 100 -> 800 bps.
        let s2 = m.observe(
            &seg(ACK, 200, 1, 100, 2_000_000_000, false),
            Direction::OriginToResponder,
            &c,
        );
        assert_eq!(s2.throughput_bps, 800);
    }

    #[test]
    fn zero_throughput_window_does_not_panic() {
        let mut m = MetricState::new();
        let c = EngineConfig {
            throughput_window: Nanos(0),
            ..EngineConfig::default()
        };
        let s = m.observe(
            &seg(ACK, 0, 1, 100, 0, false),
            Direction::OriginToResponder,
            &c,
        );
        assert_eq!(s.throughput_bps, 0);
    }

    use proptest::prelude::*;

    proptest! {
        #[test]
        fn in_flight_equals_serial_distance_for_forward_sends(
            base in any::<u32>(), deltas in proptest::collection::vec(1u32..10_000, 1..20)
        ) {
            let mut m = MetricState::new();
            let c = cfg();
            let mut seq = base;
            let mut total: u64 = 0;
            let mut ts = 0u64;
            for d in deltas {
                ts += 1_000;
                let s = m.observe(&seg(ACK, seq, 1, d, ts, false),
                                  Direction::OriginToResponder, &c);
                total += u64::from(d);
                prop_assert_eq!(s.in_flight_bytes, total, "no ack yet: all sent is outstanding");
                seq = seq.wrapping_add(d);
            }
        }
    }
}
