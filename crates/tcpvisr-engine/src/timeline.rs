//! The seekable replay timeline (design §5, ADR-0004, ADR-0010). Pure: no I/O, no clock.
//!
//! Resolves, for any cursor time `T`, the set of connections active at `T` and each one's
//! `(state, bytes)` as of `T`, via a cross-connection interval index over
//! `[opened_at, effective_end]` plus a per-connection binary search.

use tcpvisr_core::{Nanos, SampleDir};

use crate::conn::{ConnId, Connection};
use crate::state::ConnState;

/// The kind of a Time/Sequence mark (design §6, ADR-0011 §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeqKind {
    /// A data-carrying segment; `retransmit`/`out_of_order` are the M3 classification.
    Data {
        retransmit: bool,
        out_of_order: bool,
    },
    /// A SACK block, plotted in the acknowledged direction's sequence space.
    Sack,
}

/// One point on a connection's Time/Sequence graph (ADR-0011 §1). `rel` is the wrap-unwrapped
/// cumulative sequence offset from `dir`'s first-seen data seq (so a multi-GB transfer rises
/// monotonically instead of folding); `len` is the payload length (0 for a `Sack` mark).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeqSample {
    pub t: Nanos,
    pub dir: SampleDir,
    pub rel: i64,
    pub len: u32,
    pub kind: SeqKind,
}

/// A per-segment lifecycle snapshot: the connection's `(state, cumulative bytes)` at time `t`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StateSample {
    pub t: Nanos,
    pub state: ConnState,
    pub bytes_o2r: u64,
    pub bytes_r2o: u64,
}

/// A connection's resolved state as of a cursor time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AsOf {
    pub id: ConnId,
    pub state: ConnState,
    pub bytes_o2r: u64,
    pub bytes_r2o: u64,
}

/// One connection's timeline entry: its `Connection` view, its `t`-sorted snapshot series,
/// and the right bound of its active interval (`last_at` if closed, else the capture end).
#[derive(Debug, Clone)]
struct Entry {
    conn: Connection,
    samples: Vec<StateSample>,
    seq: Vec<SeqSample>,
    effective_end: Nanos,
}

/// The replay timeline over all connections.
#[derive(Debug, Clone)]
pub struct Timeline {
    entries: Vec<Entry>,
    start: Nanos,
    end: Nanos,
    event_times: Vec<Nanos>,
}

impl Timeline {
    /// Builds a state-only timeline (no seq series); every connection's `seq` is empty. This
    /// preserves the M5 constructor so existing call sites and fixtures are unchanged.
    #[must_use]
    pub fn new(conns: Vec<(Connection, Vec<StateSample>)>) -> Self {
        Self::with_seq(conns.into_iter().map(|(c, s)| (c, s, Vec::new())).collect())
    }

    /// Builds the timeline from each connection, its `StateSample` series, and its `SeqSample`
    /// series. Both series are stable-sorted by `t` because capture timestamps are not
    /// guaranteed monotonic (design §14); `start` is the minimum `StateSample.t` and `end` is
    /// the maximum `last_at`. A connection whose final state is `Closed`/`Reset` bounds its
    /// interval at `last_at`; any still-open connection extends to `end`.
    #[must_use]
    pub fn with_seq(conns: Vec<(Connection, Vec<StateSample>, Vec<SeqSample>)>) -> Self {
        let end = conns
            .iter()
            .map(|(c, _, _)| c.last_at)
            .max()
            .unwrap_or(Nanos(0));
        let start = conns
            .iter()
            .flat_map(|(_, s, _)| s.iter().map(|x| x.t))
            .min()
            .unwrap_or(Nanos(0));
        let mut entries: Vec<Entry> = Vec::with_capacity(conns.len());
        let mut event_times: Vec<Nanos> = Vec::new();
        for (conn, mut samples, mut seq) in conns {
            samples.sort_by_key(|s| s.t);
            seq.sort_by_key(|s| s.t);
            for s in &samples {
                event_times.push(s.t);
            }
            let closed = matches!(conn.state, ConnState::Closed | ConnState::Reset);
            let effective_end = if closed { conn.last_at } else { end };
            entries.push(Entry {
                conn,
                samples,
                seq,
                effective_end,
            });
        }
        event_times.sort_unstable();
        event_times.dedup();
        Self {
            entries,
            start,
            end,
            event_times,
        }
    }

    /// The `[start, end]` cursor domain.
    #[must_use]
    pub fn bounds(&self) -> (Nanos, Nanos) {
        (self.start, self.end)
    }

    /// The number of tracked connections.
    #[must_use]
    pub fn connection_count(&self) -> usize {
        self.entries.len()
    }

    /// The tracked connections (static views), in construction order.
    pub fn connections(&self) -> impl Iterator<Item = &Connection> {
        self.entries.iter().map(|e| &e.conn)
    }

    /// The focus connection's `SeqSample` series (`t`-sorted), or an empty slice if `id` is
    /// unknown or its series was not collected.
    #[must_use]
    pub fn seq_series(&self, id: ConnId) -> &[SeqSample] {
        match self.entries.iter().find(|e| e.conn.id == id) {
            Some(e) => &e.seq,
            None => &[],
        }
    }

    /// The connection's `[opened_at, effective_end]` time span for the detail X axis
    /// (`effective_end` is `last_at` if closed, else the capture end), or `None` if unknown.
    #[must_use]
    pub fn x_span(&self, id: ConnId) -> Option<(Nanos, Nanos)> {
        self.entries
            .iter()
            .find(|e| e.conn.id == id)
            .map(|e| (e.conn.opened_at, e.effective_end))
    }

    /// The ids of connections active at `t` (`opened_at <= t <= effective_end`).
    #[must_use]
    pub fn active_at(&self, t: Nanos) -> Vec<ConnId> {
        self.active_indices(t)
            .map(|i| self.entries[i].conn.id)
            .collect()
    }

    /// Each active connection's `(state, bytes)` as of `t` (last sample with `sample.t <= t`).
    #[must_use]
    pub fn resolve_at(&self, t: Nanos) -> Vec<AsOf> {
        self.active_indices(t)
            .filter_map(|i| {
                let e = &self.entries[i];
                let k = e.samples.partition_point(|s| s.t.0 <= t.0);
                let s = e.samples.get(k.checked_sub(1)?)?;
                Some(AsOf {
                    id: e.conn.id,
                    state: s.state,
                    bytes_o2r: s.bytes_o2r,
                    bytes_r2o: s.bytes_r2o,
                })
            })
            .collect()
    }

    /// The nearest event time strictly after `t`, or `None` past the last event.
    #[must_use]
    pub fn next_event(&self, t: Nanos) -> Option<Nanos> {
        let k = self.event_times.partition_point(|x| x.0 <= t.0);
        self.event_times.get(k).copied()
    }

    /// The nearest event time strictly before `t`, or `None` before the first event.
    #[must_use]
    pub fn prev_event(&self, t: Nanos) -> Option<Nanos> {
        let k = self.event_times.partition_point(|x| x.0 < t.0);
        self.event_times.get(k.checked_sub(1)?).copied()
    }

    // A linear O(N) stab query over all tracked connections, not a sublinear interval tree.
    // ADR-0004 frames playback cost as O(N_T) per frame (N_T = connections active at T); this
    // scan is O(N) in the total connection count. That is acceptable for v1's target captures
    // (interactive diagnostics, modest concurrency; ADR-0004 "bounded N_T"); a true interval
    // tree (O(log N + N_T)) is a post-v1 optimization if large-N captures need it.
    fn active_indices(&self, t: Nanos) -> impl Iterator<Item = usize> + '_ {
        (0..self.entries.len()).filter(move |&i| {
            let e = &self.entries[i];
            e.conn.opened_at.0 <= t.0 && t.0 <= e.effective_end.0
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::conn::EndpointPair;
    use core::net::{IpAddr, Ipv4Addr};
    use tcpvisr_core::Endpoint;

    fn ep(a: u8, p: u16) -> Endpoint {
        Endpoint {
            ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, a)),
            port: p,
        }
    }

    fn conn(inst: u32, opened: u64, last: u64, state: ConnState) -> Connection {
        let port = 1000 + u16::try_from(inst).unwrap_or(0);
        Connection {
            id: ConnId {
                pair: EndpointPair::new(ep(1, port), ep(2, 80)),
                instance: inst,
            },
            state,
            origin: ep(1, port),
            responder: ep(2, 80),
            origin_inferred: false,
            opened_at: Nanos(opened),
            last_at: Nanos(last),
            bytes_o2r: 0,
            bytes_r2o: 0,
            segments: 1,
        }
    }

    fn ss(t: u64, state: ConnState, up: u64, down: u64) -> StateSample {
        StateSample {
            t: Nanos(t),
            state,
            bytes_o2r: up,
            bytes_r2o: down,
        }
    }

    #[test]
    fn interval_index_membership() {
        // c0 open on [100,200] (Closed at 200); c1 still open from 150 (Established, end 300).
        let tl = Timeline::new(vec![
            (
                conn(0, 100, 200, ConnState::Closed),
                vec![
                    ss(100, ConnState::Established, 0, 0),
                    ss(200, ConnState::Closed, 0, 0),
                ],
            ),
            (
                conn(1, 150, 300, ConnState::Established),
                vec![
                    ss(150, ConnState::Established, 0, 0),
                    ss(300, ConnState::Established, 0, 0),
                ],
            ),
        ]);
        assert!(tl.active_at(Nanos(50)).is_empty());
        assert_eq!(tl.active_at(Nanos(120)).len(), 1);
        assert_eq!(tl.active_at(Nanos(180)).len(), 2);
        assert_eq!(tl.active_at(Nanos(250)).len(), 1); // c0 closed at 200, only c1
    }

    #[test]
    fn resolves_state_and_bytes_as_of_t() {
        let tl = Timeline::new(vec![(
            conn(0, 100, 300, ConnState::Established),
            vec![
                ss(100, ConnState::SynSent, 0, 0),
                ss(200, ConnState::Established, 500, 0),
                ss(300, ConnState::Established, 500, 1000),
            ],
        )]);
        let at = |t: u64| tl.resolve_at(Nanos(t));
        assert_eq!(at(150)[0].state, ConnState::SynSent);
        assert_eq!(at(150)[0].bytes_o2r, 0);
        assert_eq!(at(250)[0].state, ConnState::Established);
        assert_eq!(at(250)[0].bytes_o2r, 500);
        assert_eq!(at(250)[0].bytes_r2o, 0);
        // At the capture end (300, the connection's effective bound) the last sample carries.
        assert_eq!(at(300)[0].bytes_r2o, 1000);
    }

    #[test]
    fn closed_drops_out_still_open_stays() {
        // c0 closes at 100; c1 and c2 stay Established with a later capture end (300).
        let tl = Timeline::new(vec![
            (
                conn(0, 0, 100, ConnState::Closed),
                vec![
                    ss(0, ConnState::Established, 0, 0),
                    ss(100, ConnState::Closed, 0, 0),
                ],
            ),
            (
                conn(1, 0, 100, ConnState::Established),
                vec![ss(0, ConnState::Established, 0, 0)],
            ),
            (
                conn(2, 0, 300, ConnState::Established),
                vec![
                    ss(0, ConnState::Established, 0, 0),
                    ss(300, ConnState::Established, 0, 0),
                ],
            ),
        ]);
        let ids = |t: u64| tl.active_at(Nanos(t)).len();
        assert_eq!(ids(50), 3, "all three open at 50");
        assert_eq!(
            ids(200),
            2,
            "closed@100 gone; c1 and c2 (open, end=300) stay"
        );
    }

    #[test]
    fn event_stepping_dedups_and_clamps() {
        let tl = Timeline::new(vec![
            (
                conn(0, 0, 200, ConnState::Established),
                vec![
                    ss(0, ConnState::Established, 0, 0),
                    ss(100, ConnState::Established, 0, 0),
                ],
            ),
            (
                conn(1, 0, 200, ConnState::Established),
                vec![
                    ss(100, ConnState::Established, 0, 0), // dup @100
                    ss(200, ConnState::Established, 0, 0),
                ],
            ),
        ]);
        assert_eq!(tl.next_event(Nanos(0)), Some(Nanos(100)));
        assert_eq!(tl.next_event(Nanos(100)), Some(Nanos(200)));
        assert_eq!(tl.next_event(Nanos(200)), None);
        assert_eq!(tl.prev_event(Nanos(200)), Some(Nanos(100)));
        assert_eq!(tl.prev_event(Nanos(0)), None);
    }

    #[test]
    fn out_of_order_samples_are_sorted_at_construction() {
        let ordered = Timeline::new(vec![(
            conn(0, 100, 300, ConnState::Established),
            vec![
                ss(100, ConnState::SynSent, 0, 0),
                ss(200, ConnState::Established, 500, 0),
                ss(300, ConnState::Established, 500, 1000),
            ],
        )]);
        let shuffled = Timeline::new(vec![(
            conn(0, 100, 300, ConnState::Established),
            vec![
                ss(300, ConnState::Established, 500, 1000),
                ss(100, ConnState::SynSent, 0, 0),
                ss(200, ConnState::Established, 500, 0),
            ],
        )]);
        for t in [150u64, 250, 300] {
            assert_eq!(
                ordered.resolve_at(Nanos(t)),
                shuffled.resolve_at(Nanos(t)),
                "t={t}"
            );
            assert!(!ordered.resolve_at(Nanos(t)).is_empty(), "t={t} active");
        }
        assert_eq!(shuffled.bounds().0, Nanos(100));
    }

    #[test]
    fn empty_timeline_has_zero_bounds() {
        let tl = Timeline::new(vec![]);
        assert_eq!(tl.bounds(), (Nanos(0), Nanos(0)));
        assert_eq!(tl.connection_count(), 0);
        assert!(tl.resolve_at(Nanos(0)).is_empty());
        assert_eq!(tl.next_event(Nanos(0)), None);
    }

    fn sq(t: u64, rel: i64, len: u32) -> SeqSample {
        SeqSample {
            t: Nanos(t),
            dir: SampleDir::OriginToResponder,
            rel,
            len,
            kind: SeqKind::Data {
                retransmit: false,
                out_of_order: false,
            },
        }
    }

    #[test]
    fn with_seq_sorts_and_exposes_series_and_x_span() {
        let c = conn(0, 100, 300, ConnState::Established);
        let id = c.id;
        let tl = Timeline::with_seq(vec![(
            c,
            vec![ss(100, ConnState::Established, 0, 0)],
            vec![sq(300, 20, 10), sq(100, 0, 10)], // supplied out of t-order
        )]);
        let series = tl.seq_series(id);
        assert_eq!(series.len(), 2);
        assert_eq!(series[0].t, Nanos(100), "sorted by t at construction");
        assert_eq!(series[1].t, Nanos(300));
        assert_eq!(tl.x_span(id), Some((Nanos(100), Nanos(300))));
    }

    #[test]
    fn seq_series_and_x_span_are_empty_none_for_unknown_id() {
        let c = conn(0, 0, 10, ConnState::Established);
        let other = ConnId {
            pair: EndpointPair::new(ep(9, 1), ep(9, 2)),
            instance: 7,
        };
        let tl = Timeline::new(vec![(c, vec![ss(0, ConnState::Established, 0, 0)])]);
        assert!(tl.seq_series(other).is_empty());
        assert_eq!(tl.x_span(other), None);
    }

    #[test]
    fn seq_sample_is_copy_and_holds_fields() {
        let s = SeqSample {
            t: Nanos(5),
            dir: SampleDir::OriginToResponder,
            rel: 42,
            len: 10,
            kind: SeqKind::Data {
                retransmit: true,
                out_of_order: false,
            },
        };
        let copy = s; // Copy, not move
        assert_eq!(copy, s);
        assert_eq!(copy.rel, 42);
        assert_ne!(SeqKind::Sack, s.kind);
    }
}
