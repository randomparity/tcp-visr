//! Connection identity and the reported `Connection` view (design §4, §10.M2).

use tcpvisr_core::{Endpoint, Nanos};

use crate::state::ConnState;

/// The two endpoints of a connection in canonical order (orientation-independent grouping key).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EndpointPair {
    pub low: Endpoint,
    pub high: Endpoint,
}

impl EndpointPair {
    /// Orders the two endpoints so both wire directions map to the same pair.
    #[must_use]
    pub fn new(a: Endpoint, b: Endpoint) -> Self {
        if a <= b {
            Self { low: a, high: b }
        } else {
            Self { low: b, high: a }
        }
    }
}

/// Instance-aware connection identity (design §4): a pair can carry several instances.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConnId {
    pub pair: EndpointPair,
    pub instance: u32,
}

/// Per-segment direction relative to the connection's chosen orientation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Direction {
    OriginToResponder,
    ResponderToOrigin,
}

/// A tracked connection instance, as reported by [`crate::Tracker::into_connections`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Connection {
    pub id: ConnId,
    pub state: ConnState,
    pub origin: Endpoint,
    pub responder: Endpoint,
    pub origin_inferred: bool,
    pub opened_at: Nanos,
    pub last_at: Nanos,
    pub bytes_o2r: u64,
    pub bytes_r2o: u64,
    pub segments: u64,
}

impl Connection {
    /// Wall span of the instance; saturating because capture time is non-monotonic (§14).
    #[must_use]
    pub fn duration(&self) -> Nanos {
        Nanos(self.last_at.0.saturating_sub(self.opened_at.0))
    }
}
