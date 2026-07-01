//! The pure connection tracker (design §10.M2, ADR-0006).

use std::collections::{HashMap, HashSet, VecDeque};

use tcpvisr_core::{Endpoint, Item, MetricSample, Nanos, SampleDir, Segment, TcpFlags, TcpSeq};

use crate::config::EngineConfig;
#[cfg(test)]
use crate::config::RetentionPolicy;
use crate::conn::{ConnId, Connection, Direction, EndpointPair};
use crate::metrics::{ConnectionMetrics, MetricError, MetricState, SeriesCollection};
use crate::state::ConnState;
use crate::timeline::{
    ConnSeries, InFlightSample, RttSample, SeqKind, SeqSample, StateSample, ThroughputSample,
    Timeline,
};

/// `true` when `seq` sits backward of `baseline` in RFC 1982 serial space by more than
/// `threshold` — a drop to a fresh ISN, not a retransmit/reorder or a forward `u32` wrap.
pub(crate) fn is_backward_reset(baseline: TcpSeq, seq: TcpSeq, threshold: u32) -> bool {
    seq.serial_lt(baseline) && baseline.serial_diff(seq) > threshold
}

fn is_bare_syn(f: TcpFlags) -> bool {
    f.syn() && !f.ack()
}

fn is_syn_ack(f: TcpFlags) -> bool {
    f.syn() && f.ack()
}

/// Max serial seq seen in a direction: keep the more-forward of the two.
fn advance_baseline(current: Option<TcpSeq>, seq: TcpSeq) -> TcpSeq {
    match current {
        Some(base) if base.serial_gt(seq) => base,
        _ => seq,
    }
}

fn dir_index(d: Direction) -> usize {
    match d {
        Direction::OriginToResponder => 0,
        Direction::ResponderToOrigin => 1,
    }
}

fn dir_sample(d: Direction) -> SampleDir {
    match d {
        Direction::OriginToResponder => SampleDir::OriginToResponder,
        Direction::ResponderToOrigin => SampleDir::ResponderToOrigin,
    }
}

fn dir_opposite(d: Direction) -> Direction {
    match d {
        Direction::OriginToResponder => Direction::ResponderToOrigin,
        Direction::ResponderToOrigin => Direction::OriginToResponder,
    }
}

fn sdir_opposite(d: SampleDir) -> SampleDir {
    match d {
        SampleDir::OriginToResponder => SampleDir::ResponderToOrigin,
        SampleDir::ResponderToOrigin => SampleDir::OriginToResponder,
    }
}

fn sdir_index(d: SampleDir) -> usize {
    match d {
        SampleDir::OriginToResponder => 0,
        SampleDir::ResponderToOrigin => 1,
    }
}

/// Per-direction sequence-unwrap state (ADR-0011 §1): anchors the first-seen seq at `rel = 0`
/// and accumulates the bounded signed serial distance from a running frontier into an `i64`, so
/// a stream that wraps the 32-bit space many times rises monotonically instead of folding.
#[derive(Default, Clone, Copy)]
struct SeqUnwrap {
    frontier: Option<(TcpSeq, i64)>,
}

impl SeqUnwrap {
    fn offset(&mut self, seq: TcpSeq) -> i64 {
        match self.frontier {
            None => {
                self.frontier = Some((seq, 0));
                0
            }
            Some((fseq, frel)) => {
                if seq == fseq {
                    frel
                } else if seq.serial_gt(fseq) {
                    let rel = frel + i64::from(seq.serial_diff(fseq));
                    self.frontier = Some((seq, rel));
                    rel
                } else {
                    frel - i64::from(fseq.serial_diff(seq))
                }
            }
        }
    }
}

/// A retained sample carrying a time index, so [`evict_front`] can age any series generically.
trait HasT {
    fn t(&self) -> Nanos;
}
impl HasT for StateSample {
    fn t(&self) -> Nanos {
        self.t
    }
}
impl HasT for SeqSample {
    fn t(&self) -> Nanos {
        self.t
    }
}
impl HasT for InFlightSample {
    fn t(&self) -> Nanos {
        self.t
    }
}
impl HasT for RttSample {
    fn t(&self) -> Nanos {
        self.t
    }
}
impl HasT for ThroughputSample {
    fn t(&self) -> Nanos {
        self.t
    }
}
impl HasT for MetricSample {
    fn t(&self) -> Nanos {
        self.t
    }
}

/// Drops front (oldest) samples with `t < horizon` from `dq`, never below `keep_min`, decrementing
/// `count` for each removed sample.
fn evict_front<T: HasT>(dq: &mut VecDeque<T>, horizon: u64, keep_min: usize, count: &mut usize) {
    while dq.len() > keep_min {
        match dq.front() {
            Some(f) if f.t().0 < horizon => {
                dq.pop_front();
                *count = count.saturating_sub(1);
            }
            _ => break,
        }
    }
}

/// The `Evict` memory backstop: frees one slot on `c` by dropping the oldest sample from its
/// longest series, never dropping the last `states` sample. Returns whether a sample was popped.
fn pop_longest_front(c: &mut ConnTrack) -> bool {
    // Evictable length per series; `states` can only shrink above its keep-1 floor.
    let lens = [
        c.states.len().saturating_sub(1),
        c.series.len(),
        c.seq.len(),
        c.inflight.len(),
        c.rtt.len(),
        c.throughput.len(),
    ];
    let Some((which, &max)) = lens.iter().enumerate().max_by_key(|(_, l)| **l) else {
        return false;
    };
    if max == 0 {
        return false;
    }
    match which {
        0 => c.states.pop_front().is_some(),
        1 => c.series.pop_front().is_some(),
        2 => c.seq.pop_front().is_some(),
        3 => c.inflight.pop_front().is_some(),
        4 => c.rtt.pop_front().is_some(),
        _ => c.throughput.pop_front().is_some(),
    }
}

/// Full per-instance tracking state (internal). The public view is [`Connection`].
struct ConnTrack {
    id: ConnId,
    state: ConnState,
    origin: Endpoint,
    responder: Endpoint,
    origin_inferred: bool,
    opened_at: Nanos,
    last_at: Nanos,
    bytes_o2r: u64,
    bytes_r2o: u64,
    segments: u64,
    fin_o2r: bool,
    fin_r2o: bool,
    base_o2r: Option<TcpSeq>,
    base_r2o: Option<TcpSeq>,
    metrics: MetricState,
    series: VecDeque<MetricSample>,
    states: VecDeque<StateSample>,
    seq: VecDeque<SeqSample>,
    inflight: VecDeque<InFlightSample>,
    rtt: VecDeque<RttSample>,
    throughput: VecDeque<ThroughputSample>,
    srtt: [Option<Nanos>; 2],
    unwrap: [SeqUnwrap; 2],
}

impl ConnTrack {
    fn direction_of(&self, src: Endpoint) -> Direction {
        if src == self.origin {
            Direction::OriginToResponder
        } else {
            Direction::ResponderToOrigin
        }
    }

    fn baseline(&self, dir: Direction) -> Option<TcpSeq> {
        match dir {
            Direction::OriginToResponder => self.base_o2r,
            Direction::ResponderToOrigin => self.base_r2o,
        }
    }

    fn account(&mut self, seg: &Segment, dir: Direction) {
        self.last_at = Nanos(self.last_at.0.max(seg.ts.0));
        self.segments += 1;
        match dir {
            Direction::OriginToResponder => {
                self.bytes_o2r += u64::from(seg.payload_len);
                self.base_o2r = Some(advance_baseline(self.base_o2r, seg.seq));
            }
            Direction::ResponderToOrigin => {
                self.bytes_r2o += u64::from(seg.payload_len);
                self.base_r2o = Some(advance_baseline(self.base_r2o, seg.seq));
            }
        }
    }

    fn apply_state(&mut self, seg: &Segment, dir: Direction) {
        let f = seg.flags;
        if f.rst() {
            self.state = ConnState::Reset; // terminal override from any state
            return;
        }
        if self.state == ConnState::Reset {
            return; // terminal
        }
        if is_syn_ack(f) {
            if self.state == ConnState::SynSent {
                self.state = self.state.advance_to(ConnState::SynReceived);
            }
        } else if is_bare_syn(f) {
            // A bare SYN from the responder side while we have only seen the origin's SYN is
            // the second leg of a simultaneous open. From the origin side it is a duplicate.
            if self.state == ConnState::SynSent && dir == Direction::ResponderToOrigin {
                self.state = self.state.advance_to(ConnState::SynReceived);
            }
        }
        // The ACK that completes the handshake, or any data, after SYN-ACK -> Established.
        if self.state == ConnState::SynReceived && (f.ack() || seg.payload_len > 0) {
            self.state = self.state.advance_to(ConnState::Established);
        }
        if f.fin() {
            match dir {
                Direction::OriginToResponder => self.fin_o2r = true,
                Direction::ResponderToOrigin => self.fin_r2o = true,
            }
            if self.fin_o2r && self.fin_r2o {
                self.state = self.state.advance_to(ConnState::Closed);
            } else {
                self.state = self.state.advance_to(ConnState::FinWait);
            }
        }
    }

    fn snapshot(&self, t: Nanos) -> StateSample {
        StateSample {
            t,
            state: self.state,
            bytes_o2r: self.bytes_o2r,
            bytes_r2o: self.bytes_r2o,
        }
    }

    /// Appends this segment's Time/Sequence points to `out`: one `Data` point (its own
    /// direction) when the segment carries payload, and one `Sack` point (the acked/opposite
    /// direction) per SACK block. Mutates the per-direction unwrap frontiers.
    fn push_seq_points(
        &mut self,
        seg: &Segment,
        dir: Direction,
        sample: &MetricSample,
        out: &mut Vec<SeqSample>,
    ) {
        if seg.payload_len > 0 {
            let rel = self.unwrap[dir_index(dir)].offset(seg.seq);
            out.push(SeqSample {
                t: seg.ts,
                dir: dir_sample(dir),
                rel,
                len: seg.payload_len,
                kind: SeqKind::Data {
                    retransmit: sample.retransmit,
                    out_of_order: sample.out_of_order,
                },
            });
        }
        if !seg.options.sack_blocks.is_empty() {
            let acked = dir_opposite(dir);
            let ai = dir_index(acked);
            for &(left, _right) in &seg.options.sack_blocks {
                let rel = self.unwrap[ai].offset(left);
                out.push(SeqSample {
                    t: seg.ts,
                    dir: dir_sample(acked),
                    rel,
                    len: 0,
                    kind: SeqKind::Sack,
                });
            }
        }
    }

    fn view(&self) -> Connection {
        Connection {
            id: self.id,
            state: self.state,
            origin: self.origin,
            responder: self.responder,
            origin_inferred: self.origin_inferred,
            opened_at: self.opened_at,
            last_at: self.last_at,
            bytes_o2r: self.bytes_o2r,
            bytes_r2o: self.bytes_r2o,
            segments: self.segments,
        }
    }
}

/// Pure per-connection tracker: fold `Item`s in, read `Connection`s out.
pub struct Tracker {
    config: EngineConfig,
    conns: Vec<ConnTrack>,
    live: HashMap<EndpointPair, usize>,
    next_instance: HashMap<EndpointPair, u32>,
    collected_samples: usize,
    overflowed: bool,
    /// The latest observed time (max of any segment/tick ts). Drives live eviction; stays at the
    /// last segment ts under replay, where `Tick`s never arrive.
    now: Nanos,
}

impl Tracker {
    #[must_use]
    pub fn new(config: EngineConfig) -> Self {
        Self {
            config,
            conns: Vec::new(),
            live: HashMap::new(),
            next_instance: HashMap::new(),
            collected_samples: 0,
            overflowed: false,
            now: Nanos(0),
        }
    }

    /// Whether the instance with `id` buffers a metric series under the current config.
    fn should_collect(&self, id: ConnId) -> bool {
        match self.config.series_collection {
            SeriesCollection::None => false,
            SeriesCollection::All => true,
            SeriesCollection::Only(target) => target == id,
        }
    }

    /// Admission control shared by every `record_*`, enforcing the retention policy. Under
    /// `FailFast`, sets `overflowed` and returns `false` when the ceiling is hit. Under `Evict`,
    /// evicts one oldest sample from connection `idx` to make room and returns `true`. Increments
    /// the collected count on a successful admit.
    fn admit(&mut self, idx: usize) -> bool {
        if self.collected_samples >= self.config.retention.max_samples() {
            if self.config.retention.window().is_some() {
                self.evict_oldest_global(idx);
            } else {
                self.overflowed = true;
                return false;
            }
        }
        self.collected_samples += 1;
        true
    }

    /// Ages out samples older than the eviction horizon (`now − window`) from every series,
    /// keeping at least the most recent `states` sample so the connection stays resolvable at
    /// "now". No-op under `FailFast` (replay).
    fn evict_samples(&mut self) {
        let Some(window) = self.config.retention.window() else {
            return;
        };
        let horizon = self.now.0.saturating_sub(window.0);
        for c in &mut self.conns {
            evict_front(&mut c.states, horizon, 1, &mut self.collected_samples);
            evict_front(&mut c.series, horizon, 0, &mut self.collected_samples);
            evict_front(&mut c.seq, horizon, 0, &mut self.collected_samples);
            evict_front(&mut c.inflight, horizon, 0, &mut self.collected_samples);
            evict_front(&mut c.rtt, horizon, 0, &mut self.collected_samples);
            evict_front(&mut c.throughput, horizon, 0, &mut self.collected_samples);
        }
    }

    /// Backstop under `Evict`: pop one oldest sample from connection `idx`'s longest series to make
    /// room at the ceiling, keeping the collected count consistent.
    fn evict_oldest_global(&mut self, idx: usize) {
        if pop_longest_front(&mut self.conns[idx]) {
            self.collected_samples = self.collected_samples.saturating_sub(1);
        }
    }

    /// Stores `sample` on the instance at `idx` when collected, enforcing the retention policy.
    fn record_sample(&mut self, idx: usize, sample: MetricSample) {
        let id = self.conns[idx].id;
        if !self.should_collect(id) {
            return;
        }
        if self.admit(idx) {
            self.conns[idx].series.push_back(sample);
        }
    }

    /// Stores a per-segment state snapshot on the instance at `idx`, enforcing the retention policy.
    /// Gated by `config.collect_state_timeline` at the call site (all instances, not per-id).
    fn record_state(&mut self, idx: usize, sample: StateSample) {
        if self.overflowed {
            return;
        }
        if self.admit(idx) {
            self.conns[idx].states.push_back(sample);
        }
    }

    /// Stores one `SeqSample` on the instance at `idx`, enforcing the retention policy.
    fn record_seq(&mut self, idx: usize, sample: SeqSample) {
        if self.overflowed {
            return;
        }
        if self.admit(idx) {
            self.conns[idx].seq.push_back(sample);
        }
    }

    /// Builds and records this segment's seq points when seq collection is on and not overflowed.
    fn collect_seq_points(
        &mut self,
        idx: usize,
        seg: &Segment,
        dir: Direction,
        sample: &MetricSample,
    ) {
        if self.overflowed || !self.config.collect_seq_timeline {
            return;
        }
        let mut points = Vec::new();
        self.conns[idx].push_seq_points(seg, dir, sample, &mut points);
        for p in points {
            self.record_seq(idx, p);
        }
    }

    /// Stores one `InFlightSample` on the instance at `idx`, enforcing the retention policy.
    fn record_inflight(&mut self, idx: usize, sample: InFlightSample) {
        if self.overflowed {
            return;
        }
        if self.admit(idx) {
            self.conns[idx].inflight.push_back(sample);
        }
    }

    /// Snapshots each direction's current outstanding for this segment when in-flight collection
    /// is on and not overflowed (ADR-0012 §1: both directions, so ACK-driven drains are sampled
    /// at ack time rather than deferred to the next same-direction send).
    fn collect_inflight_points(&mut self, idx: usize, seg: &Segment) {
        self.collect_inflight_at(idx, seg.ts);
    }

    /// In-flight snapshot for both directions stamped at time `t` — a segment ts (per-segment) or
    /// the live `now` (a decay tick).
    fn collect_inflight_at(&mut self, idx: usize, t: Nanos) {
        if self.overflowed || !self.config.collect_inflight_timeline {
            return;
        }
        for d in [Direction::OriginToResponder, Direction::ResponderToOrigin] {
            if let Some(bytes) = self.conns[idx].metrics.in_flight(d) {
                self.record_inflight(
                    idx,
                    InFlightSample {
                        t,
                        dir: dir_sample(d),
                        bytes,
                    },
                );
            }
        }
    }

    /// Stores one `RttSample` on the instance at `idx`, enforcing the retention policy.
    fn record_rtt(&mut self, idx: usize, sample: RttSample) {
        if self.overflowed {
            return;
        }
        if self.admit(idx) {
            self.conns[idx].rtt.push_back(sample);
        }
    }

    /// Records the per-ack RTT + smoothed SRTT when RTT collection is on and this segment yielded
    /// an RTT. The RTT measures the *opposite* (acked-sender) flow, so the sample is tagged with
    /// `opposite(sample.dir)` (ADR-0013 §1); `srtt` is the RFC 6298 EWMA (α = 1/8) over that
    /// direction, computed in `u128` to avoid overflow (ADR-0013 §2).
    fn collect_rtt_points(&mut self, idx: usize, sample: &MetricSample) {
        if self.overflowed || !self.config.collect_rtt_timeline {
            return;
        }
        let Some(rtt) = sample.rtt else {
            return;
        };
        let m = sdir_opposite(sample.dir);
        let mi = sdir_index(m);
        let srtt = match self.conns[idx].srtt[mi] {
            None => rtt,
            Some(prev) => {
                let v = (7u128 * u128::from(prev.0) + u128::from(rtt.0)) / 8;
                Nanos(u64::try_from(v).unwrap_or(u64::MAX))
            }
        };
        self.conns[idx].srtt[mi] = Some(srtt);
        self.record_rtt(
            idx,
            RttSample {
                t: sample.t,
                dir: m,
                rtt,
                srtt,
            },
        );
    }

    /// Stores one `ThroughputSample` on the instance at `idx`, enforcing the retention policy.
    fn record_throughput(&mut self, idx: usize, sample: ThroughputSample) {
        if self.overflowed {
            return;
        }
        if self.admit(idx) {
            self.conns[idx].throughput.push_back(sample);
        }
    }

    /// Snapshots each direction's trailing-window `(throughput, goodput)` for this segment when
    /// throughput collection is on and not overflowed. Both directions are sampled per segment (as
    /// M7 in-flight) so the sending flow's rate is captured at reverse-ACK times and shows decay; a
    /// direction that has not sent data returns `None` and contributes no sample (ADR-0014 §1).
    fn collect_throughput_points(&mut self, idx: usize, seg: &Segment) {
        self.collect_throughput_at(idx, seg.ts);
    }

    /// Trailing-window `(throughput, goodput)` for both directions stamped at time `t`. On a decay
    /// tick, a direction that sent data returns a lower (eventually zero) rate as bytes age out;
    /// one that never sent data returns `None` and contributes no sample (ADR-0014 §1).
    fn collect_throughput_at(&mut self, idx: usize, t: Nanos) {
        if self.overflowed || !self.config.collect_throughput_timeline {
            return;
        }
        for d in [Direction::OriginToResponder, Direction::ResponderToOrigin] {
            if let Some((throughput_bps, goodput_bps)) =
                self.conns[idx].metrics.throughput_at(d, t, &self.config)
            {
                self.record_throughput(
                    idx,
                    ThroughputSample {
                        t,
                        dir: dir_sample(d),
                        throughput_bps,
                        goodput_bps,
                    },
                );
            }
        }
    }

    /// Folds one `Item` into tracker state. A `Segment` advances `now` and, under `Evict`, ages
    /// out samples past the horizon. `Tick` handling is added by the live path; under `FailFast`
    /// (replay) it stays inert — replay never emits a `Tick`.
    pub fn observe(&mut self, item: &Item) {
        match item {
            Item::Segment(seg) => {
                self.now = Nanos(self.now.0.max(seg.ts.0));
                self.observe_segment(seg);
                self.evict_samples();
            }
            Item::Tick(t) => self.observe_tick(*t),
        }
    }

    /// Advances live time on a `Tick`: bumps `now`, emits decay samples for still-active flows,
    /// ages out samples, and evicts dead connections. Inert under `FailFast` (replay never emits a
    /// `Tick`, and idle is judged per-segment there).
    fn observe_tick(&mut self, t: Nanos) {
        if self.config.retention.window().is_none() {
            return;
        }
        self.now = Nanos(self.now.0.max(t.0));
        self.emit_decay_samples(self.now);
        self.evict_samples();
        self.evict_dead_connections();
    }

    /// At time `t`, records a decay throughput/in-flight sample for each connection not yet idle
    /// past `dead_after`, so a silenced flow's rate visibly ages toward zero as bytes leave the
    /// window. Idle-past-`dead_after` connections are left for `evict_dead_connections`.
    fn emit_decay_samples(&mut self, t: Nanos) {
        if self.overflowed {
            return;
        }
        let dead_after = self.config.dead_after.0;
        for idx in 0..self.conns.len() {
            if t.0.saturating_sub(self.conns[idx].last_at.0) > dead_after {
                continue;
            }
            self.collect_throughput_at(idx, t);
            self.collect_inflight_at(idx, t);
        }
    }

    /// Removes connections terminal (`Closed`/`Reset`) or idle past `dead_after` whose last
    /// activity precedes the eviction horizon, rebuilding the `live` index and pruning
    /// `next_instance`, so the tracked connection count stays bounded under churn (criterion 17).
    fn evict_dead_connections(&mut self) {
        let Some(window) = self.config.retention.window() else {
            return;
        };
        let horizon = self.now.0.saturating_sub(window.0);
        let dead_after = self.config.dead_after.0;
        let now = self.now.0;
        let before = self.conns.len();
        self.conns.retain(|c| {
            let terminal = matches!(c.state, ConnState::Closed | ConnState::Reset);
            let idle = now.saturating_sub(c.last_at.0) > dead_after;
            !((terminal || idle) && c.last_at.0 < horizon)
        });
        if self.conns.len() == before {
            return;
        }
        // Rebuild the pair->index map and prune next_instance for pairs with no survivor, else that
        // map grows unbounded under churn. A later reuse of a fully-evicted pair restarts at 0.
        self.live.clear();
        let mut survivors: HashSet<EndpointPair> = HashSet::new();
        for (i, c) in self.conns.iter().enumerate() {
            self.live.insert(c.id.pair, i);
            survivors.insert(c.id.pair);
        }
        self.next_instance
            .retain(|pair, _| survivors.contains(pair));
        self.recount_collected();
    }

    /// Recomputes `collected_samples` from the surviving connections' retained series after a bulk
    /// removal, keeping the ceiling accounting consistent.
    fn recount_collected(&mut self) {
        self.collected_samples = self
            .conns
            .iter()
            .map(|c| {
                c.states.len()
                    + c.series.len()
                    + c.seq.len()
                    + c.inflight.len()
                    + c.rtt.len()
                    + c.throughput.len()
            })
            .sum();
    }

    fn observe_segment(&mut self, seg: &Segment) {
        let src = seg.flow.source();
        let dst = seg.flow.destination();
        let pair = EndpointPair::new(src, dst);
        if let Some(&idx) = self.live.get(&pair) {
            if !self.should_split(idx, seg, src) {
                let dir = self.conns[idx].direction_of(src);
                self.conns[idx].account(seg, dir);
                self.conns[idx].apply_state(seg, dir);
                if self.config.collect_state_timeline {
                    let s = self.conns[idx].snapshot(seg.ts);
                    self.record_state(idx, s);
                }
                // Derive metrics only for collected instances: `conns` (None) pays nothing, and
                // the per-direction RTT/throughput state cannot grow for unrelated flows. Once
                // the ceiling has tripped the result is already doomed, so stop deriving entirely
                // rather than keep growing per-connection state on a discarded series.
                let want_metric = self.should_collect(self.conns[idx].id);
                if !self.overflowed
                    && (want_metric
                        || self.config.collect_seq_timeline
                        || self.config.collect_inflight_timeline
                        || self.config.collect_rtt_timeline
                        || self.config.collect_throughput_timeline)
                {
                    let sample = self.conns[idx].metrics.observe(seg, dir, &self.config);
                    if want_metric {
                        self.record_sample(idx, sample);
                    }
                    self.collect_seq_points(idx, seg, dir, &sample);
                    self.collect_inflight_points(idx, seg);
                    self.collect_rtt_points(idx, &sample);
                    self.collect_throughput_points(idx, seg);
                }
                return;
            }
        }
        self.create_instance(pair, seg, src, dst);
    }

    /// Whether `seg` should open a new instance instead of joining the live one at `idx`.
    fn should_split(&self, idx: usize, seg: &Segment, src: Endpoint) -> bool {
        let track = &self.conns[idx];
        let f = seg.flags;
        if is_bare_syn(f) {
            let terminal = matches!(track.state, ConnState::Closed | ConnState::Reset);
            let idle = seg.ts.0.saturating_sub(track.last_at.0) > self.config.dead_after.0;
            return terminal || idle;
        }
        // SYN-less mid-stream reset: only meaningful on an established, live instance.
        if track.state == ConnState::Established {
            let dir = track.direction_of(src);
            if let Some(base) = track.baseline(dir) {
                return is_backward_reset(base, seg.seq, self.config.reset_threshold);
            }
        }
        false
    }

    fn create_instance(&mut self, pair: EndpointPair, seg: &Segment, src: Endpoint, dst: Endpoint) {
        let instance = *self.next_instance.entry(pair).or_insert(0);
        self.next_instance.insert(pair, instance + 1);

        let flags = seg.flags;
        let (origin, responder, origin_inferred, state) = if is_bare_syn(flags) {
            (src, dst, false, ConnState::SynSent)
        } else if is_syn_ack(flags) {
            (dst, src, false, ConnState::SynReceived)
        } else if flags.rst() {
            (src, dst, true, ConnState::Reset)
        } else if flags.fin() {
            (src, dst, true, ConnState::FinWait)
        } else {
            (src, dst, true, ConnState::Established)
        };

        let mut track = ConnTrack {
            id: ConnId { pair, instance },
            state,
            origin,
            responder,
            origin_inferred,
            opened_at: seg.ts,
            last_at: seg.ts,
            bytes_o2r: 0,
            bytes_r2o: 0,
            segments: 0,
            fin_o2r: false,
            fin_r2o: false,
            base_o2r: None,
            base_r2o: None,
            metrics: MetricState::new(),
            series: VecDeque::new(),
            states: VecDeque::new(),
            seq: VecDeque::new(),
            inflight: VecDeque::new(),
            rtt: VecDeque::new(),
            throughput: VecDeque::new(),
            srtt: [None, None],
            unwrap: [SeqUnwrap::default(); 2],
        };
        let dir = track.direction_of(src);
        track.account(seg, dir);
        if flags.fin() {
            match dir {
                Direction::OriginToResponder => track.fin_o2r = true,
                Direction::ResponderToOrigin => track.fin_r2o = true,
            }
        }
        // Derive the first sample only for collected instances, and not past the ceiling
        // (see `observe_segment`).
        let want_metric = self.should_collect(track.id);
        let sample = (!self.overflowed
            && (want_metric
                || self.config.collect_seq_timeline
                || self.config.collect_inflight_timeline
                || self.config.collect_rtt_timeline
                || self.config.collect_throughput_timeline))
            .then(|| track.metrics.observe(seg, dir, &self.config));
        let idx = self.conns.len();
        self.conns.push(track);
        self.live.insert(pair, idx);
        if let Some(sample) = sample {
            if want_metric {
                self.record_sample(idx, sample);
            }
            self.collect_seq_points(idx, seg, dir, &sample);
            self.collect_inflight_points(idx, seg);
            self.collect_rtt_points(idx, &sample);
            self.collect_throughput_points(idx, seg);
        }
        if self.config.collect_state_timeline {
            let s = self.conns[idx].snapshot(seg.ts);
            self.record_state(idx, s);
        }
    }

    /// All tracked instances, ordered by `(opened_at, pair, instance)` for determinism.
    #[must_use]
    pub fn into_connections(self) -> Vec<Connection> {
        let mut out: Vec<Connection> = self.conns.iter().map(ConnTrack::view).collect();
        out.sort_by_key(|c| (c.opened_at, c.id.pair, c.id.instance));
        out
    }

    /// All tracked instances with their derived series, same ordering as `into_connections`.
    ///
    /// # Errors
    /// Returns [`MetricError::SampleCeiling`] if collection hit `max_samples`.
    pub fn into_metrics(self) -> Result<Vec<ConnectionMetrics>, MetricError> {
        if self.overflowed {
            return Err(MetricError::SampleCeiling {
                samples: self.collected_samples + 1,
                limit: self.config.retention.max_samples(),
            });
        }
        let mut out: Vec<ConnectionMetrics> = self
            .conns
            .iter()
            .map(|c| ConnectionMetrics {
                conn: c.view(),
                series: c.series.iter().copied().collect(),
            })
            .collect();
        out.sort_by_key(|m| (m.conn.opened_at, m.conn.id.pair, m.conn.id.instance));
        Ok(out)
    }

    /// All tracked instances with their per-segment state timeline, built into a [`Timeline`].
    ///
    /// # Errors
    /// Returns [`MetricError::SampleCeiling`] if collection hit `max_samples`.
    pub fn into_timeline(self) -> Result<Timeline, MetricError> {
        if self.overflowed {
            return Err(MetricError::SampleCeiling {
                samples: self.collected_samples + 1,
                limit: self.config.retention.max_samples(),
            });
        }
        let series: Vec<ConnSeries> = self
            .conns
            .iter()
            .map(|c| {
                (
                    c.view(),
                    c.states.iter().copied().collect(),
                    c.seq.iter().copied().collect(),
                    c.inflight.iter().copied().collect(),
                    c.rtt.iter().copied().collect(),
                    c.throughput.iter().copied().collect(),
                )
            })
            .collect();
        Ok(Timeline::with_seq(series))
    }

    /// The latest observed time (`now`): the max of any segment/tick timestamp seen so far.
    #[must_use]
    pub fn now(&self) -> Nanos {
        self.now
    }

    /// The live eviction horizon (`now − window`, saturated at 0), or `now` under `FailFast` where
    /// nothing is evicted.
    #[must_use]
    pub fn retention_horizon(&self) -> Nanos {
        match self.config.retention.window() {
            Some(w) => Nanos(self.now.0.saturating_sub(w.0)),
            None => self.now,
        }
    }

    /// A non-consuming build of the current live [`Timeline`] from the retained series. Infallible
    /// under `Evict` (no ceiling); open connections extend to `now`.
    #[must_use]
    pub fn snapshot(&self) -> Timeline {
        let series: Vec<ConnSeries> = self
            .conns
            .iter()
            .map(|c| {
                (
                    c.view(),
                    c.states.iter().copied().collect(),
                    c.seq.iter().copied().collect(),
                    c.inflight.iter().copied().collect(),
                    c.rtt.iter().copied().collect(),
                    c.throughput.iter().copied().collect(),
                )
            })
            .collect();
        Timeline::with_seq_ending(series, self.now)
    }
}

/// Tracks every connection in `items` and returns the reported connections (test convenience).
#[must_use]
pub fn track<'a>(
    items: impl IntoIterator<Item = &'a Item>,
    config: EngineConfig,
) -> Vec<Connection> {
    let mut tracker = Tracker::new(config);
    for item in items {
        tracker.observe(item);
    }
    tracker.into_connections()
}

#[cfg(test)]
mod test_support {
    use core::net::{IpAddr, Ipv4Addr};
    use tcpvisr_core::{FlowKey, Item, Nanos, Segment, TcpFlags, TcpOptions, TcpSeq};

    pub fn ep(o: u8, p: u16) -> (IpAddr, u16) {
        (IpAddr::V4(Ipv4Addr::new(10, 0, 0, o)), p)
    }

    /// Build a one-segment `Item`. `ack` is carried for state tests; pass `0` when unused.
    pub fn seg(
        src: (IpAddr, u16),
        dst: (IpAddr, u16),
        flags: u16,
        seq: u32,
        ack: u32,
        len: u32,
        ts: u64,
    ) -> Item {
        Item::Segment(Segment {
            ts: Nanos(ts),
            flow: FlowKey {
                src_ip: src.0,
                src_port: src.1,
                dst_ip: dst.0,
                dst_port: dst.1,
            },
            seq: TcpSeq(seq),
            ack: TcpSeq(ack),
            flags: TcpFlags(flags),
            window: 0,
            options: TcpOptions::default(),
            payload_len: len,
        })
    }
}

#[cfg(test)]
mod orient_tests {
    use super::test_support::{ep, seg};
    use super::*;
    use tcpvisr_core::{Nanos, TcpFlags};

    #[test]
    fn bare_syn_sets_origin_and_groups_both_directions() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let mut t = Tracker::new(EngineConfig::default());
        t.observe(&seg(c, s, TcpFlags::SYN, 100, 0, 0, 1_000)); // client SYN
        t.observe(&seg(
            s,
            c,
            TcpFlags::SYN | TcpFlags::ACK,
            500,
            101,
            0,
            2_000,
        )); // server SYN-ACK
        t.observe(&seg(c, s, TcpFlags::ACK, 101, 501, 10, 3_000)); // 10 bytes c->s
        t.observe(&seg(s, c, TcpFlags::ACK, 501, 111, 20, 4_000)); // 20 bytes s->c
        let conns = t.into_connections();
        assert_eq!(conns.len(), 1, "both directions group into one connection");
        let conn = conns[0];
        assert_eq!((conn.origin.ip, conn.origin.port), c);
        assert_eq!((conn.responder.ip, conn.responder.port), s);
        assert!(!conn.origin_inferred);
        assert_eq!(conn.bytes_o2r, 10);
        assert_eq!(conn.bytes_r2o, 20);
        assert_eq!(conn.segments, 4);
        assert_eq!(conn.duration(), Nanos(3_000));
    }

    #[test]
    fn syn_ack_first_orients_server_as_responder() {
        let (c, s) = (ep(1, 1234), ep(2, 443));
        let mut t = Tracker::new(EngineConfig::default());
        t.observe(&seg(s, c, TcpFlags::SYN | TcpFlags::ACK, 9, 0, 0, 1_000)); // joined mid-handshake
        let conns = t.into_connections();
        assert_eq!(
            (conns[0].origin.ip, conns[0].origin.port),
            c,
            "client is origin"
        );
        assert_eq!((conns[0].responder.ip, conns[0].responder.port), s);
        assert!(!conns[0].origin_inferred, "SYN-ACK orientation is observed");
    }

    #[test]
    fn mid_stream_infers_origin_from_first_segment() {
        let (a, b) = (ep(1, 5000), ep(2, 8080));
        let mut t = Tracker::new(EngineConfig::default());
        t.observe(&seg(a, b, TcpFlags::ACK, 42, 0, 5, 1_000)); // no SYN ever
        let conns = t.into_connections();
        assert_eq!((conns[0].origin.ip, conns[0].origin.port), a);
        assert!(conns[0].origin_inferred);
        assert_eq!(conns[0].bytes_o2r, 5);
    }

    #[test]
    fn last_at_is_max_under_reordered_timestamps() {
        let (a, b) = (ep(1, 5000), ep(2, 8080));
        let mut t = Tracker::new(EngineConfig::default());
        t.observe(&seg(a, b, TcpFlags::ACK, 42, 0, 5, 5_000)); // later ts first
        t.observe(&seg(a, b, TcpFlags::ACK, 47, 0, 5, 1_000)); // reordered earlier ts
        let conns = t.into_connections();
        assert_eq!(conns[0].opened_at, Nanos(5_000));
        assert_eq!(
            conns[0].last_at,
            Nanos(5_000),
            "earlier ts must not move last_at back"
        );
        assert_eq!(conns[0].duration(), Nanos(0), "saturating, no panic");
    }
}

#[cfg(test)]
mod state_tests {
    use super::test_support::{ep, seg};
    use super::*;
    use tcpvisr_core::{Item, TcpFlags};

    fn run(items: &[Item]) -> Vec<Connection> {
        let mut t = Tracker::new(EngineConfig::default());
        for it in items {
            t.observe(it);
        }
        t.into_connections()
    }

    #[test]
    fn three_way_handshake_reaches_established() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let conns = run(&[
            seg(c, s, TcpFlags::SYN, 100, 0, 0, 1),
            seg(s, c, TcpFlags::SYN | TcpFlags::ACK, 500, 101, 0, 2),
            seg(c, s, TcpFlags::ACK, 101, 501, 0, 3),
        ]);
        assert_eq!(conns[0].state, ConnState::Established);
    }

    #[test]
    fn simultaneous_open_reaches_established() {
        let (a, b) = (ep(1, 4000), ep(2, 4001));
        let conns = run(&[
            seg(a, b, TcpFlags::SYN, 10, 0, 0, 1), // a SYN -> SynSent, a=origin
            seg(b, a, TcpFlags::SYN, 20, 0, 0, 2), // b SYN (responder) -> SynReceived
            seg(a, b, TcpFlags::ACK, 11, 21, 0, 3),
            seg(b, a, TcpFlags::ACK, 21, 11, 0, 4),
        ]);
        assert_eq!(conns.len(), 1);
        assert_eq!(conns[0].state, ConnState::Established);
        assert!(!conns[0].origin_inferred);
    }

    #[test]
    fn graceful_fin_fin_reaches_closed() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let conns = run(&[
            seg(c, s, TcpFlags::ACK, 100, 1, 5, 1), // mid-stream established
            seg(c, s, TcpFlags::FIN | TcpFlags::ACK, 105, 1, 0, 2),
            seg(s, c, TcpFlags::FIN | TcpFlags::ACK, 1, 106, 0, 3),
        ]);
        assert_eq!(conns[0].state, ConnState::Closed);
    }

    #[test]
    fn rst_overrides_to_reset() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let conns = run(&[
            seg(c, s, TcpFlags::ACK, 100, 1, 5, 1),
            seg(s, c, TcpFlags::RST, 1, 0, 0, 2),
        ]);
        assert_eq!(conns[0].state, ConnState::Reset);
    }

    #[test]
    fn retransmitted_payload_is_recounted_as_wire_bytes() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let conns = run(&[
            seg(c, s, TcpFlags::ACK, 100, 1, 10, 1), // 10 bytes c->s
            seg(c, s, TcpFlags::ACK, 100, 1, 10, 2), // retransmit of the same 10 bytes
        ]);
        assert_eq!(
            conns[0].bytes_o2r, 20,
            "wire bytes count retransmits (M3 owns goodput)"
        );
        assert_eq!(conns[0].segments, 2);
    }

    #[test]
    fn duplicate_syn_does_not_regress_established() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let conns = run(&[
            seg(c, s, TcpFlags::SYN, 100, 0, 0, 1),
            seg(s, c, TcpFlags::SYN | TcpFlags::ACK, 500, 101, 0, 2),
            seg(c, s, TcpFlags::ACK, 101, 501, 5, 3), // Established
            seg(c, s, TcpFlags::SYN, 100, 0, 0, 4),   // retransmitted SYN (dup)
        ]);
        assert_eq!(conns.len(), 1, "dup SYN on live conn does not split");
        assert_eq!(conns[0].state, ConnState::Established);
    }
}

#[cfg(test)]
mod instance_tests {
    use super::test_support::{ep, seg};
    use super::*;
    use tcpvisr_core::{Item, Nanos, TcpFlags};

    fn run_cfg(items: &[Item], config: EngineConfig) -> Vec<Connection> {
        track(items.iter(), config)
    }

    #[test]
    fn tuple_reuse_new_syn_after_close_splits() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let conns = run_cfg(
            &[
                seg(c, s, TcpFlags::SYN, 100, 0, 0, 1),
                seg(s, c, TcpFlags::SYN | TcpFlags::ACK, 500, 101, 0, 2),
                seg(c, s, TcpFlags::ACK, 101, 501, 0, 3),
                seg(c, s, TcpFlags::FIN | TcpFlags::ACK, 101, 501, 0, 4),
                seg(s, c, TcpFlags::FIN | TcpFlags::ACK, 501, 102, 0, 5), // Closed
                seg(c, s, TcpFlags::SYN, 9000, 0, 0, 6),                  // reuse: new SYN
            ],
            EngineConfig::default(),
        );
        assert_eq!(conns.len(), 2, "reuse after close is a second instance");
        assert_eq!(conns[0].id.instance, 0);
        assert_eq!(conns[1].id.instance, 1);
        assert_eq!(conns[1].state, ConnState::SynSent);
    }

    #[test]
    fn forward_wrap_stays_one_instance() {
        let (a, b) = (ep(1, 5000), ep(2, 8080));
        let conns = run_cfg(
            &[
                seg(a, b, TcpFlags::ACK, u32::MAX - 100, 1, 50, 1), // baseline near top
                seg(a, b, TcpFlags::ACK, 200, 1, 50, 2),            // wrapped forward — advance
            ],
            EngineConfig::default(),
        );
        assert_eq!(conns.len(), 1, "a u32 wrap must not split the flow");
    }

    #[test]
    fn large_backward_reset_splits_mid_stream() {
        let (a, b) = (ep(1, 5000), ep(2, 8080));
        let conns = run_cfg(
            &[
                seg(a, b, TcpFlags::ACK, 0x7000_0000, 1, 50, 1), // established baseline
                seg(a, b, TcpFlags::ACK, 0x1000_0000, 1, 50, 2), // 0x6000_0000 backward -> reset
            ],
            EngineConfig::default(),
        );
        assert_eq!(conns.len(), 2, "fresh ISN far below baseline splits");
    }

    #[test]
    fn small_backward_retransmit_does_not_split() {
        let (a, b) = (ep(1, 5000), ep(2, 8080));
        let conns = run_cfg(
            &[
                seg(a, b, TcpFlags::ACK, 1_000_000, 1, 50, 1),
                seg(a, b, TcpFlags::ACK, 999_000, 1, 50, 2), // retransmit
            ],
            EngineConfig::default(),
        );
        assert_eq!(conns.len(), 1);
    }

    #[test]
    fn idle_syn_past_dead_after_splits_even_without_close() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let cfg = EngineConfig {
            dead_after: Nanos(1_000),
            ..EngineConfig::default()
        };
        let conns = run_cfg(
            &[
                seg(c, s, TcpFlags::SYN, 100, 0, 0, 1),
                seg(s, c, TcpFlags::SYN | TcpFlags::ACK, 500, 101, 0, 2), // SynReceived, live
                seg(c, s, TcpFlags::SYN, 9000, 0, 0, 10_000), // 9998ns later: idle reuse
            ],
            cfg,
        );
        assert_eq!(
            conns.len(),
            2,
            "SYN after idle > dead_after starts a new instance"
        );
    }
}

#[cfg(test)]
mod split_tests {
    use super::is_backward_reset;
    use proptest::prelude::*;
    use tcpvisr_core::TcpSeq;

    const HALF: u32 = 1 << 31;

    #[test]
    fn forward_wrap_is_not_a_reset() {
        // baseline near the top; seq wrapped forward by 0x300 — an advance, not a reset.
        assert!(!is_backward_reset(
            TcpSeq(u32::MAX - 0xFF),
            TcpSeq(0x200),
            1 << 30
        ));
    }

    #[test]
    fn small_backward_is_not_a_reset() {
        assert!(!is_backward_reset(
            TcpSeq(1_000_000),
            TcpSeq(999_000),
            1 << 30
        ));
    }

    #[test]
    fn large_backward_is_a_reset() {
        // 0x6000_0000 backward (> 2^30, < 2^31).
        assert!(is_backward_reset(
            TcpSeq(0x7000_0000),
            TcpSeq(0x1000_0000),
            1 << 30
        ));
    }

    proptest! {
        #[test]
        fn forward_delta_never_resets(base in any::<u32>(), d in 1u32..HALF) {
            let seq = TcpSeq(base.wrapping_add(d));
            prop_assert!(!is_backward_reset(TcpSeq(base), seq, 1 << 30));
        }

        #[test]
        fn backward_delta_splits_iff_over_threshold(
            base in any::<u32>(), b in 1u32..HALF, thr in 0u32..HALF
        ) {
            let seq = TcpSeq(base.wrapping_sub(b));
            prop_assert_eq!(is_backward_reset(TcpSeq(base), seq, thr), b > thr);
        }
    }
}

#[cfg(test)]
mod timeline_tests {
    use super::test_support::{ep, seg};
    use super::*;
    use crate::state::ConnState;
    use tcpvisr_core::{Nanos, TcpFlags};

    fn cfg() -> EngineConfig {
        EngineConfig {
            collect_state_timeline: true,
            ..EngineConfig::default()
        }
    }

    #[test]
    fn collects_one_state_sample_per_segment() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let mut t = Tracker::new(cfg());
        t.observe(&seg(c, s, TcpFlags::ACK, 100, 1, 10, 1_000)); // 10 bytes o2r
        t.observe(&seg(s, c, TcpFlags::ACK, 1, 110, 20, 2_000)); // 20 bytes r2o
        let tl = t.into_timeline().expect("no ceiling");
        assert_eq!(tl.connection_count(), 1);
        let at = tl.resolve_at(Nanos(2_000));
        assert_eq!(at[0].bytes_o2r, 10);
        assert_eq!(at[0].bytes_r2o, 20);
        assert_eq!(at[0].state, ConnState::Established);
        // As of the first segment only, the second direction's bytes are not yet counted.
        assert_eq!(tl.resolve_at(Nanos(1_000))[0].bytes_r2o, 0);
    }

    #[test]
    fn none_flag_yields_empty_series() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let mut t = Tracker::new(EngineConfig::default()); // collect_state_timeline = false
        t.observe(&seg(c, s, TcpFlags::ACK, 100, 1, 10, 1_000));
        let tl = t.into_timeline().expect("no ceiling");
        // No samples -> the connection is never resolvable and bounds fall back to 0..last_at.
        assert!(tl.resolve_at(Nanos(1_000)).is_empty());
        assert_eq!(tl.bounds(), (Nanos(0), Nanos(1_000)));
    }

    #[test]
    fn ceiling_exceeded_returns_error() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let mut t = Tracker::new(EngineConfig {
            collect_state_timeline: true,
            retention: RetentionPolicy::FailFast { max_samples: 1 },
            ..EngineConfig::default()
        });
        t.observe(&seg(c, s, TcpFlags::ACK, 100, 1, 10, 1_000));
        t.observe(&seg(c, s, TcpFlags::ACK, 110, 1, 10, 2_000)); // 2nd sample > limit 1
        let err = t.into_timeline().expect_err("should exceed");
        assert_eq!(
            err,
            MetricError::SampleCeiling {
                samples: 2,
                limit: 1
            }
        );
    }
}

#[cfg(test)]
mod metric_wire_tests {
    use super::test_support::{ep, seg};
    use super::*;
    use crate::metrics::SeriesCollection;
    use tcpvisr_core::TcpFlags;

    fn run(items: &[tcpvisr_core::Item], coll: SeriesCollection) -> Vec<ConnectionMetrics> {
        let cfg = EngineConfig {
            series_collection: coll,
            ..EngineConfig::default()
        };
        let mut t = Tracker::new(cfg);
        for it in items {
            t.observe(it);
        }
        t.into_metrics().expect("no ceiling")
    }

    #[test]
    fn none_collection_yields_empty_series() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let m = run(
            &[seg(c, s, TcpFlags::ACK, 100, 1, 10, 1_000)],
            SeriesCollection::None,
        );
        assert_eq!(m.len(), 1);
        assert!(m[0].series.is_empty());
    }

    #[test]
    fn none_collection_does_not_accumulate_metric_state_for_unacked_flow() {
        // A long one-directional, never-acknowledged flow under `None` (the `conns` path) must
        // not buffer samples or grow per-connection RTT state — guards the cross-mode OOM path.
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let mut t = Tracker::new(EngineConfig {
            series_collection: SeriesCollection::None,
            ..EngineConfig::default()
        });
        for i in 0..10_000u32 {
            // distinct, advancing, never-acked data segments
            t.observe(&seg(
                c,
                s,
                TcpFlags::ACK,
                100 + i * 10,
                1,
                10,
                u64::from(i) + 1,
            ));
        }
        let m = t.into_metrics().expect("no ceiling under None");
        assert_eq!(m.len(), 1);
        assert!(m[0].series.is_empty(), "None must store no samples");
    }

    #[test]
    fn all_collection_yields_one_sample_per_segment() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let m = run(
            &[
                seg(c, s, TcpFlags::ACK, 100, 1, 10, 1_000),
                seg(s, c, TcpFlags::ACK, 1, 110, 0, 2_000),
            ],
            SeriesCollection::All,
        );
        assert_eq!(m[0].series.len(), 2);
        assert_eq!(m[0].series[0].in_flight_bytes, 10);
    }

    #[test]
    fn only_collection_buffers_just_the_target() {
        // Two distinct connections; collect only the first one's ConnId.
        let (c1, s1) = (ep(1, 1111), ep(2, 80));
        let (c2, s2) = (ep(3, 2222), ep(4, 80));
        let items = [
            seg(c1, s1, TcpFlags::ACK, 100, 1, 10, 1_000),
            seg(c2, s2, TcpFlags::ACK, 100, 1, 10, 2_000),
        ];
        // Resolve target id via a None pass.
        let conns = {
            let mut t = Tracker::new(EngineConfig::default());
            for it in &items {
                t.observe(it);
            }
            t.into_connections()
        };
        let target = conns[0].id;
        let m = run(&items, SeriesCollection::Only(target));
        let by_target: Vec<_> = m.iter().filter(|cm| cm.conn.id == target).collect();
        let others: Vec<_> = m.iter().filter(|cm| cm.conn.id != target).collect();
        assert_eq!(by_target[0].series.len(), 1);
        assert!(others.iter().all(|cm| cm.series.is_empty()));
    }

    #[test]
    fn ceiling_exceeded_returns_error() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let cfg = EngineConfig {
            series_collection: SeriesCollection::All,
            retention: RetentionPolicy::FailFast { max_samples: 1 },
            ..EngineConfig::default()
        };
        let mut t = Tracker::new(cfg);
        t.observe(&seg(c, s, TcpFlags::ACK, 100, 1, 10, 1_000));
        t.observe(&seg(c, s, TcpFlags::ACK, 110, 1, 10, 2_000)); // 2nd sample > limit 1
        let err = t.into_metrics().expect_err("should exceed");
        assert_eq!(
            err,
            MetricError::SampleCeiling {
                samples: 2,
                limit: 1
            }
        );
    }

    #[test]
    fn metrics_ordering_matches_into_connections() {
        let (c1, s1) = (ep(1, 1111), ep(2, 80));
        let (c2, s2) = (ep(3, 2222), ep(4, 80));
        let items = [
            seg(c2, s2, TcpFlags::ACK, 100, 1, 10, 2_000),
            seg(c1, s1, TcpFlags::ACK, 100, 1, 10, 1_000),
        ];
        let m = run(&items, SeriesCollection::All);
        // opened_at 1_000 (c1) sorts before 2_000 (c2).
        assert_eq!(m[0].conn.opened_at, tcpvisr_core::Nanos(1_000));
        assert_eq!(m[1].conn.opened_at, tcpvisr_core::Nanos(2_000));
    }
}

#[cfg(test)]
mod seq_tests {
    use super::test_support::{ep, seg};
    use super::*;
    use tcpvisr_core::{FlowKey, Item, Nanos, SampleDir, Segment, TcpFlags, TcpOptions, TcpSeq};

    fn seq_cfg() -> EngineConfig {
        EngineConfig {
            collect_state_timeline: true,
            collect_seq_timeline: true,
            ..EngineConfig::default()
        }
    }

    fn only_id(tl: &crate::timeline::Timeline) -> ConnId {
        tl.connections().next().expect("one connection").id
    }

    // A segment carrying a single SACK block (L, R), in src->dst direction.
    fn seg_sack(
        src: (core::net::IpAddr, u16),
        dst: (core::net::IpAddr, u16),
        flags: u16,
        seq: u32,
        ack: u32,
        ts: u64,
        block: (u32, u32),
    ) -> Item {
        let mut options = TcpOptions::default();
        options.sack_blocks.push((TcpSeq(block.0), TcpSeq(block.1)));
        Item::Segment(Segment {
            ts: Nanos(ts),
            flow: FlowKey {
                src_ip: src.0,
                src_port: src.1,
                dst_ip: dst.0,
                dst_port: dst.1,
            },
            seq: TcpSeq(seq),
            ack: TcpSeq(ack),
            flags: TcpFlags(flags),
            window: 0,
            options,
            payload_len: 0,
        })
    }

    #[test]
    fn data_points_carry_unwrapped_rel_and_len() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let mut t = Tracker::new(seq_cfg());
        t.observe(&seg(c, s, TcpFlags::ACK, 100, 1, 10, 1_000));
        t.observe(&seg(c, s, TcpFlags::ACK, 110, 1, 20, 2_000));
        let tl = t.into_timeline().expect("timeline");
        let series: Vec<_> = tl
            .seq_series(only_id(&tl))
            .iter()
            .filter(|p| p.dir == SampleDir::OriginToResponder)
            .copied()
            .collect();
        assert_eq!(series.len(), 2);
        assert_eq!((series[0].rel, series[0].len), (0, 10));
        assert_eq!((series[1].rel, series[1].len), (10, 20));
        assert_eq!(
            series[1].kind,
            SeqKind::Data {
                retransmit: false,
                out_of_order: false
            }
        );
    }

    #[test]
    fn rel_unwraps_across_a_u32_wrap() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let mut t = Tracker::new(seq_cfg());
        t.observe(&seg(c, s, TcpFlags::ACK, u32::MAX - 100, 1, 50, 1_000));
        t.observe(&seg(c, s, TcpFlags::ACK, 200, 1, 50, 2_000));
        let tl = t.into_timeline().expect("timeline");
        let rels: Vec<i64> = tl
            .seq_series(only_id(&tl))
            .iter()
            .filter(|p| p.dir == SampleDir::OriginToResponder)
            .map(|p| p.rel)
            .collect();
        // 200.serial_diff(u32::MAX-100) == 301 — a forward advance, not a fold.
        assert_eq!(rels, vec![0, 301]);
    }

    #[test]
    fn rel_rises_monotonically_across_multiple_wraps() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let mut t = Tracker::new(seq_cfg());
        let step: u32 = 1_200_000_000; // ~1.2 GB per segment; 4 segments wrap u32 twice
        let mut seq: u32 = 0;
        let mut ts = 0u64;
        for _ in 0..4 {
            ts += 1_000;
            t.observe(&seg(c, s, TcpFlags::ACK, seq, 1, step, ts));
            seq = seq.wrapping_add(step);
        }
        let tl = t.into_timeline().expect("timeline");
        let rels: Vec<i64> = tl
            .seq_series(only_id(&tl))
            .iter()
            .filter(|p| p.dir == SampleDir::OriginToResponder)
            .map(|p| p.rel)
            .collect();
        assert_eq!(rels.len(), 4);
        assert!(
            rels.windows(2).all(|w| w[1] > w[0]),
            "rel strictly increases across wraps: {rels:?}"
        );
        assert_eq!(rels[3], 3 * i64::from(step), "no fold: 3 steps forward");
    }

    #[test]
    fn sack_point_lands_in_the_acked_direction_frame() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let mut t = Tracker::new(seq_cfg());
        // O2R data anchors the O2R frame at seq 1000 (rel 0).
        t.observe(&seg(c, s, TcpFlags::ACK, 1000, 1, 100, 1_000));
        // R2O ack carrying a SACK block for O2R bytes [1200, 1300).
        t.observe(&seg_sack(s, c, TcpFlags::ACK, 1, 1101, 2_000, (1200, 1300)));
        let tl = t.into_timeline().expect("timeline");
        let sacks: Vec<_> = tl
            .seq_series(only_id(&tl))
            .iter()
            .filter(|p| p.kind == SeqKind::Sack)
            .copied()
            .collect();
        assert_eq!(sacks.len(), 1);
        assert_eq!(
            sacks[0].dir,
            SampleDir::OriginToResponder,
            "acked direction"
        );
        assert_eq!(sacks[0].rel, 200, "1200 - 1000 in the O2R frame");
        assert_eq!(sacks[0].len, 0);
    }

    #[test]
    fn seq_collection_counts_against_the_ceiling() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let mut cfg = seq_cfg();
        cfg.retention = RetentionPolicy::FailFast { max_samples: 1 }; // first segment already produces state + seq samples
        let mut t = Tracker::new(cfg);
        t.observe(&seg(c, s, TcpFlags::ACK, 100, 1, 10, 1_000));
        t.observe(&seg(c, s, TcpFlags::ACK, 110, 1, 20, 2_000));
        let err = t.into_timeline().expect_err("ceiling");
        assert!(matches!(err, MetricError::SampleCeiling { .. }));
    }

    #[test]
    fn retransmit_and_ooo_classified_on_seq_points() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        // Retransmit: behind-frontier re-send after a gap >= reorder_window (3ms default).
        let mut t = Tracker::new(seq_cfg());
        t.observe(&seg(c, s, TcpFlags::ACK, 200, 1, 100, 1_000_000));
        t.observe(&seg(c, s, TcpFlags::ACK, 100, 1, 100, 4_000_000)); // 3ms gap -> retransmit
        let tl = t.into_timeline().expect("timeline");
        let kinds: Vec<_> = tl.seq_series(only_id(&tl)).iter().map(|p| p.kind).collect();
        assert_eq!(
            kinds[1],
            SeqKind::Data {
                retransmit: true,
                out_of_order: false
            }
        );

        // Out-of-order: behind-frontier within the reorder window.
        let mut t2 = Tracker::new(seq_cfg());
        t2.observe(&seg(c, s, TcpFlags::ACK, 200, 1, 100, 1_000));
        t2.observe(&seg(c, s, TcpFlags::ACK, 100, 1, 100, 1_001)); // 1us gap -> out-of-order
        let tl2 = t2.into_timeline().expect("timeline");
        let kinds2: Vec<_> = tl2
            .seq_series(only_id(&tl2))
            .iter()
            .map(|p| p.kind)
            .collect();
        assert_eq!(
            kinds2[1],
            SeqKind::Data {
                retransmit: false,
                out_of_order: true
            }
        );
    }
}

#[cfg(test)]
mod inflight_tests {
    use super::test_support::{ep, seg};
    use super::*;
    use tcpvisr_core::{SampleDir, TcpFlags};

    fn iff_cfg() -> EngineConfig {
        EngineConfig {
            collect_state_timeline: true,
            collect_seq_timeline: true,
            collect_inflight_timeline: true,
            ..EngineConfig::default()
        }
    }

    fn only_id(tl: &crate::timeline::Timeline) -> ConnId {
        tl.connections().next().expect("one connection").id
    }

    fn o2r_inflight(tl: &crate::timeline::Timeline) -> Vec<(u64, u64)> {
        tl.inflight_series(only_id(tl))
            .iter()
            .filter(|s| s.dir == SampleDir::OriginToResponder)
            .map(|s| (s.t.0, s.bytes))
            .collect()
    }

    #[test]
    fn inflight_rises_on_send_and_drains_at_ack_time() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let mut t = Tracker::new(iff_cfg());
        t.observe(&seg(c, s, TcpFlags::ACK, 100, 1, 10, 1_000)); // O2R +10
        t.observe(&seg(s, c, TcpFlags::ACK, 1, 110, 0, 2_000)); // R2O ACK drains O2R
        t.observe(&seg(c, s, TcpFlags::ACK, 110, 1, 5, 3_000)); // O2R +5
        let tl = t.into_timeline().expect("timeline");
        assert_eq!(o2r_inflight(&tl), vec![(1_000, 10), (2_000, 0), (3_000, 5)]);
    }

    #[test]
    fn inflight_is_serial_correct_across_u32_wrap() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let mut t = Tracker::new(iff_cfg());
        t.observe(&seg(c, s, TcpFlags::ACK, u32::MAX - 100, 1, 50, 1_000));
        t.observe(&seg(c, s, TcpFlags::ACK, 200, 1, 50, 2_000)); // never acked
        let tl = t.into_timeline().expect("timeline");
        let bytes: Vec<u64> = o2r_inflight(&tl).iter().map(|(_, b)| *b).collect();
        assert_eq!(bytes, vec![50, 351]); // serial distance across the wrap
    }

    #[test]
    fn inflight_off_by_default_is_empty() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let mut t = Tracker::new(EngineConfig {
            collect_state_timeline: true,
            ..EngineConfig::default()
        });
        t.observe(&seg(c, s, TcpFlags::ACK, 100, 1, 10, 1_000));
        let tl = t.into_timeline().expect("timeline");
        assert!(tl.inflight_series(only_id(&tl)).is_empty());
    }

    #[test]
    fn inflight_collection_counts_against_ceiling() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let mut cfg = iff_cfg();
        cfg.retention = RetentionPolicy::FailFast { max_samples: 1 }; // first segment already produces state + seq + inflight samples
        let mut t = Tracker::new(cfg);
        t.observe(&seg(c, s, TcpFlags::ACK, 100, 1, 10, 1_000));
        t.observe(&seg(c, s, TcpFlags::ACK, 110, 1, 20, 2_000));
        assert!(matches!(
            t.into_timeline().expect_err("ceiling"),
            MetricError::SampleCeiling { .. }
        ));
    }
}

#[cfg(test)]
mod rtt_tests {
    use super::test_support::{ep, seg};
    use super::*;
    use tcpvisr_core::{SampleDir, TcpFlags};

    fn rtt_cfg() -> EngineConfig {
        EngineConfig {
            collect_rtt_timeline: true,
            ..EngineConfig::default()
        }
    }

    fn only_id(tl: &crate::timeline::Timeline) -> ConnId {
        tl.connections().next().expect("one connection").id
    }

    /// (t, rtt, srtt) triples for the O2R-measured RTT samples, t-ordered by the Timeline.
    fn o2r_rtt(tl: &crate::timeline::Timeline) -> Vec<(u64, u64, u64)> {
        tl.rtt_series(only_id(tl))
            .iter()
            .filter(|s| s.dir == SampleDir::OriginToResponder)
            .map(|s| (s.t.0, s.rtt.0, s.srtt.0))
            .collect()
    }

    // Criterion 1: the RTT of O2R data is measured on the R2O ACK, so the sample is tagged O2R
    // (the acked sender), not R2O (the ACK's own direction).
    #[test]
    fn rtt_attributed_to_measured_flow_not_ack_direction() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let mut t = Tracker::new(rtt_cfg());
        t.observe(&seg(c, s, TcpFlags::ACK, 100, 1, 10, 1_000)); // O2R data seq100 len10
        t.observe(&seg(s, c, TcpFlags::ACK, 1, 110, 0, 1_500)); // R2O pure ACK=110
        let tl = t.into_timeline().expect("timeline");
        let all = tl.rtt_series(only_id(&tl));
        assert_eq!(all.len(), 1, "exactly one RTT sample");
        assert_eq!(
            all[0].dir,
            SampleDir::OriginToResponder,
            "measured flow is O2R"
        );
        assert_eq!((all[0].t.0, all[0].rtt.0), (1_500, 500));
    }

    // Criterion 2: srtt is the RFC 6298 EWMA (α=1/8): 800, (7*800+800)/8=800, (7*800+400)/8=750.
    #[test]
    fn srtt_is_rfc6298_ewma() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let mut t = Tracker::new(rtt_cfg());
        t.observe(&seg(c, s, TcpFlags::ACK, 100, 1, 10, 0)); // O2R -> pending(110,0)
        t.observe(&seg(s, c, TcpFlags::ACK, 1, 110, 0, 800)); // R2O ACK110 -> rtt800
        t.observe(&seg(c, s, TcpFlags::ACK, 110, 1, 10, 1_000)); // O2R -> pending(120,1000)
        t.observe(&seg(s, c, TcpFlags::ACK, 1, 120, 0, 1_800)); // R2O ACK120 -> rtt800
        t.observe(&seg(c, s, TcpFlags::ACK, 120, 1, 10, 2_000)); // O2R -> pending(130,2000)
        t.observe(&seg(s, c, TcpFlags::ACK, 1, 130, 0, 2_400)); // R2O ACK130 -> rtt400
        let tl = t.into_timeline().expect("timeline");
        assert_eq!(
            o2r_rtt(&tl),
            vec![(800, 800, 800), (1_800, 800, 800), (2_400, 400, 750)]
        );
    }

    // Criterion 3a: a duplicate ACK that does not advance the frontier yields no RTT sample.
    #[test]
    fn duplicate_ack_produces_no_rtt_sample() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let mut t = Tracker::new(rtt_cfg());
        t.observe(&seg(c, s, TcpFlags::ACK, 100, 1, 10, 0)); // O2R
        t.observe(&seg(s, c, TcpFlags::ACK, 1, 110, 0, 500)); // R2O ACK110 -> rtt500
        t.observe(&seg(s, c, TcpFlags::ACK, 1, 110, 0, 900)); // R2O dup ACK110 -> no advance
        let tl = t.into_timeline().expect("timeline");
        assert_eq!(
            o2r_rtt(&tl),
            vec![(500, 500, 500)],
            "only the advancing ACK yields RTT"
        );
    }

    // Criterion 3b: a retransmitted range clears the pending queue (Karn), so the later ACK finds
    // nothing to pair and yields no RTT sample.
    #[test]
    fn karn_retransmit_produces_no_rtt_sample() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let mut t = Tracker::new(rtt_cfg());
        t.observe(&seg(c, s, TcpFlags::ACK, 100, 1, 10, 0)); // O2R -> pending(110,0)
        t.observe(&seg(c, s, TcpFlags::ACK, 100, 1, 10, 10_000_000)); // O2R retransmit (gap > 3ms)
        t.observe(&seg(s, c, TcpFlags::ACK, 1, 110, 0, 11_000_000)); // R2O ACK110 -> pending empty
        let tl = t.into_timeline().expect("timeline");
        assert!(
            tl.rtt_series(only_id(&tl)).is_empty(),
            "Karn cleared the pending send"
        );
    }

    // Criterion 6: off by default.
    #[test]
    fn rtt_off_by_default_is_empty() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let mut t = Tracker::new(EngineConfig {
            collect_state_timeline: true,
            ..EngineConfig::default()
        });
        t.observe(&seg(c, s, TcpFlags::ACK, 100, 1, 10, 0));
        t.observe(&seg(s, c, TcpFlags::ACK, 1, 110, 0, 500));
        let tl = t.into_timeline().expect("timeline");
        assert!(tl.rtt_series(only_id(&tl)).is_empty());
    }

    // Criterion 5: two RTT samples with max_samples=1 -> SampleCeiling.
    #[test]
    fn rtt_collection_counts_against_ceiling() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let mut cfg = rtt_cfg();
        cfg.retention = RetentionPolicy::FailFast { max_samples: 1 };
        let mut t = Tracker::new(cfg);
        t.observe(&seg(c, s, TcpFlags::ACK, 100, 1, 10, 0));
        t.observe(&seg(s, c, TcpFlags::ACK, 1, 110, 0, 500)); // rtt #1
        t.observe(&seg(c, s, TcpFlags::ACK, 110, 1, 10, 1_000));
        t.observe(&seg(s, c, TcpFlags::ACK, 1, 120, 0, 1_500)); // rtt #2 -> ceiling
        assert!(matches!(
            t.into_timeline().expect_err("ceiling"),
            MetricError::SampleCeiling { .. }
        ));
    }
}

#[cfg(test)]
mod throughput_tests {
    use super::test_support::{ep, seg};
    use super::*;
    use tcpvisr_core::{SampleDir, TcpFlags};

    fn tput_cfg() -> EngineConfig {
        EngineConfig {
            collect_throughput_timeline: true,
            ..EngineConfig::default()
        }
    }

    fn only_id(tl: &crate::timeline::Timeline) -> ConnId {
        tl.connections().next().expect("one connection").id
    }

    /// `(t, throughput_bps, goodput_bps)` triples for the O2R-attributed throughput samples.
    fn o2r_throughput(tl: &crate::timeline::Timeline) -> Vec<(u64, u64, u64)> {
        tl.throughput_series(only_id(tl))
            .iter()
            .filter(|s| s.dir == SampleDir::OriginToResponder)
            .map(|s| (s.t.0, s.throughput_bps, s.goodput_bps))
            .collect()
    }

    // Criterion 1: throughput of O2R data is attributed to the sending flow (O2R), not flipped.
    #[test]
    fn throughput_attributed_to_the_sending_flow() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let mut t = Tracker::new(tput_cfg());
        t.observe(&seg(c, s, TcpFlags::ACK, 100, 1, 100, 0)); // O2R 100 B data
        let tl = t.into_timeline().expect("timeline");
        let all = tl.throughput_series(only_id(&tl));
        assert!(!all.is_empty());
        assert!(
            all.iter().all(|s| s.dir == SampleDir::OriginToResponder),
            "the O2R sender's rate is tagged O2R, not the peer"
        );
    }

    // Criterion 3: goodput excludes retransmitted bytes; the gap is the retransmit rate.
    #[test]
    fn goodput_excludes_retransmit_end_to_end() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let mut t = Tracker::new(tput_cfg());
        t.observe(&seg(c, s, TcpFlags::ACK, 100, 1, 100, 0)); // 100 B new
        t.observe(&seg(c, s, TcpFlags::ACK, 100, 1, 100, 4_000_000)); // retransmit (gap 4ms >= 3ms)
        let tl = t.into_timeline().expect("timeline");
        let last = *o2r_throughput(&tl)
            .last()
            .expect("a sample at the retransmit");
        assert_eq!(last.1, 1_600, "throughput counts both 100 B");
        assert_eq!(
            last.2, 800,
            "goodput counts only the non-retransmitted 100 B"
        );
        assert_eq!(last.1, 2 * last.2, "the gap is the retransmit rate");
    }

    // Criterion 7: a direction that only ACKs (no payload) yields no throughput sample.
    #[test]
    fn ack_only_direction_yields_no_sample() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let mut t = Tracker::new(tput_cfg());
        t.observe(&seg(c, s, TcpFlags::ACK, 100, 1, 100, 0)); // O2R data
        t.observe(&seg(s, c, TcpFlags::ACK, 1, 200, 0, 1_000)); // R2O pure ACK, no payload
        let tl = t.into_timeline().expect("timeline");
        let r2o: Vec<_> = tl
            .throughput_series(only_id(&tl))
            .iter()
            .filter(|s| s.dir == SampleDir::ResponderToOrigin)
            .collect();
        assert!(
            r2o.is_empty(),
            "the ACK-only direction has no throughput sample"
        );
        assert!(
            !o2r_throughput(&tl).is_empty(),
            "the data sender does have samples"
        );
    }

    // Criterion 7a: the sending flow is sampled at a reverse-direction segment's time and shows
    // decay once its bytes age out of the window. A sparse (own-data-segment-only) impl fails this.
    #[test]
    fn sender_flow_sampled_at_reverse_ack_times_and_decays() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let mut t = Tracker::new(tput_cfg()); // 1s window
        t.observe(&seg(c, s, TcpFlags::ACK, 100, 1, 100, 0)); // O2R 100 B burst at t=0
        t.observe(&seg(s, c, TcpFlags::ACK, 1, 200, 0, 500_000_000)); // R2O ACK inside the window
        t.observe(&seg(s, c, TcpFlags::ACK, 1, 200, 0, 1_500_000_000)); // R2O ACK past the window
        let tl = t.into_timeline().expect("timeline");
        let o2r = o2r_throughput(&tl);
        // A sample exists at each reverse-ACK time (both-directions snapshot), not only at t=0.
        assert!(
            o2r.iter().any(|&(t, _, _)| t == 500_000_000),
            "sampled at the in-window reverse ACK: {o2r:?}"
        );
        let past = o2r
            .iter()
            .find(|&&(t, _, _)| t == 1_500_000_000)
            .expect("sampled at the past-window reverse ACK");
        assert_eq!(
            (past.1, past.2),
            (0, 0),
            "the rate has decayed to zero once every byte aged out"
        );
    }

    // Criterion 5: throughput collection counts against the ceiling.
    #[test]
    fn throughput_collection_counts_against_ceiling() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let mut cfg = tput_cfg();
        cfg.retention = RetentionPolicy::FailFast { max_samples: 1 }; // first data segment already produces a throughput sample
        let mut t = Tracker::new(cfg);
        t.observe(&seg(c, s, TcpFlags::ACK, 100, 1, 100, 1_000));
        t.observe(&seg(c, s, TcpFlags::ACK, 200, 1, 100, 2_000)); // 2nd sample -> ceiling
        assert!(matches!(
            t.into_timeline().expect_err("ceiling"),
            MetricError::SampleCeiling { .. }
        ));
    }

    // Criterion 6: off by default.
    #[test]
    fn throughput_off_by_default_is_empty() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let mut t = Tracker::new(EngineConfig {
            collect_state_timeline: true,
            ..EngineConfig::default()
        });
        t.observe(&seg(c, s, TcpFlags::ACK, 100, 1, 100, 0));
        let tl = t.into_timeline().expect("timeline");
        assert!(tl.throughput_series(only_id(&tl)).is_empty());
    }
}

#[cfg(test)]
mod evict_tests {
    use super::test_support::{ep, seg};
    use super::*;
    use crate::config::RetentionPolicy;
    use crate::metrics::SeriesCollection;
    use tcpvisr_core::{Nanos, TcpFlags};

    fn evict_cfg(window_ns: u64) -> EngineConfig {
        EngineConfig {
            collect_state_timeline: true,
            collect_throughput_timeline: true,
            series_collection: SeriesCollection::All,
            retention: RetentionPolicy::Evict {
                window: Nanos(window_ns),
                max_samples: 1_000_000,
            },
            ..EngineConfig::default()
        }
    }

    #[test]
    fn state_samples_older_than_window_are_evicted_keeping_latest() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let mut t = Tracker::new(evict_cfg(1_000)); // 1000ns window
        // four sends 500ns apart -> ts 500,1000,1500,2000; now=2000, horizon=1000
        for k in 1..=4u64 {
            let seq = 100 + u32::try_from(k).unwrap_or(0) * 10;
            t.observe(&seg(c, s, TcpFlags::ACK, seq, 1, 10, k * 500));
        }
        let states: Vec<u64> = t.conns[0].states.iter().map(|x| x.t.0).collect();
        // horizon = now(2000) - window(1000) = 1000; keep t >= 1000 -> {1000,1500,2000}
        assert_eq!(states, vec![1000, 1500, 2000], "front (t<1000) evicted");
    }

    #[test]
    fn states_never_evicts_the_last_sample() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let mut t = Tracker::new(evict_cfg(1)); // 1ns window: everything but latest is stale
        t.observe(&seg(c, s, TcpFlags::ACK, 100, 1, 10, 1_000));
        t.observe(&seg(c, s, TcpFlags::ACK, 110, 1, 10, 5_000));
        let states: Vec<u64> = t.conns[0].states.iter().map(|x| x.t.0).collect();
        assert_eq!(
            states,
            vec![5_000],
            "at least the most recent state survives"
        );
    }

    #[test]
    fn max_samples_backstop_evicts_oldest_under_evict() {
        // A window so wide it never triggers, so only the max_samples backstop bounds the series.
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let cfg = EngineConfig {
            collect_state_timeline: true,
            retention: RetentionPolicy::Evict {
                window: Nanos(u64::MAX),
                max_samples: 3,
            },
            ..EngineConfig::default()
        };
        let mut t = Tracker::new(cfg);
        for k in 1..=6u64 {
            let seq = 100 + u32::try_from(k).unwrap_or(0) * 10;
            t.observe(&seg(c, s, TcpFlags::ACK, seq, 1, 10, k * 1_000));
        }
        // state-only collection: 1 sample/segment, bounded at 3 by the backstop (not the window).
        let states: Vec<u64> = t.conns[0].states.iter().map(|x| x.t.0).collect();
        assert_eq!(
            states,
            vec![4_000, 5_000, 6_000],
            "oldest evicted, latest kept"
        );
    }

    #[test]
    fn failfast_still_fails_fast() {
        // Regression: FailFast must still trip SampleCeiling, unaffected by the evict path.
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let cfg = EngineConfig {
            collect_state_timeline: true,
            retention: RetentionPolicy::FailFast { max_samples: 1 },
            ..EngineConfig::default()
        };
        let mut t = Tracker::new(cfg);
        t.observe(&seg(c, s, TcpFlags::ACK, 100, 1, 10, 1_000));
        t.observe(&seg(c, s, TcpFlags::ACK, 110, 1, 10, 2_000));
        let err = t.into_timeline().expect_err("ceiling");
        assert_eq!(
            err,
            MetricError::SampleCeiling {
                samples: 2,
                limit: 1
            }
        );
    }

    #[test]
    fn tick_decays_throughput_toward_zero_after_silence() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let mut cfg = evict_cfg(10_000_000_000); // 10s window (no sample eviction here)
        cfg.throughput_window = Nanos(1_000_000_000); // 1s throughput window
        let mut t = Tracker::new(cfg);
        // 1000 bytes at t=0
        t.observe(&seg(c, s, TcpFlags::ACK, 100, 1, 1000, 0));
        let last_active = t.conns[0].throughput.back().map(|x| x.throughput_bps);
        // Tick at t=2s: >1s of silence -> window empty -> a decay sample at 0 bps.
        t.observe(&tcpvisr_core::Item::Tick(Nanos(2_000_000_000)));
        let decayed = *t.conns[0].throughput.back().expect("a decay sample");
        assert_eq!(decayed.t, Nanos(2_000_000_000));
        assert_eq!(
            decayed.throughput_bps, 0,
            "rate decays to zero after the window empties"
        );
        assert!(last_active.unwrap_or(0) > 0);
    }

    #[test]
    fn whole_connection_evicted_when_terminal_and_past_horizon() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let mut t = Tracker::new(evict_cfg(1_000)); // 1000ns window
        // open + RST at t=100 -> Reset (terminal), last_at=100
        t.observe(&seg(c, s, TcpFlags::ACK, 100, 1, 10, 100));
        t.observe(&seg(s, c, TcpFlags::RST, 1, 0, 0, 100));
        assert_eq!(t.conns.len(), 1);
        // Tick far past horizon: now=5000, horizon=4000 > last_at=100 -> evict whole connection.
        t.observe(&tcpvisr_core::Item::Tick(Nanos(5_000)));
        assert!(
            t.conns.is_empty(),
            "terminal + last_at<horizon -> whole-connection eviction"
        );
        assert!(t.live.is_empty());
        assert!(
            t.next_instance.is_empty(),
            "next_instance pruned too (no unbounded map growth)"
        );
    }

    #[test]
    fn snapshot_is_non_consuming_and_open_extends_to_now() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let mut t = Tracker::new(evict_cfg(10_000_000_000));
        t.observe(&seg(c, s, TcpFlags::ACK, 100, 1, 10, 1_000));
        t.observe(&tcpvisr_core::Item::Tick(Nanos(9_000)));
        let snap = t.snapshot();
        assert_eq!(t.now(), Nanos(9_000));
        assert_eq!(
            snap.bounds().1,
            Nanos(9_000),
            "open conn interval extends to now"
        );
        assert_eq!(t.retention_horizon(), Nanos(0), "now(9_000) - window >= 0");
        // non-consuming: a second snapshot still works and the tracker keeps ingesting.
        let _snap2 = t.snapshot();
        t.observe(&seg(c, s, TcpFlags::ACK, 110, 1, 10, 10_000));
    }
}
