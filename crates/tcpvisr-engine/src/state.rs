//! Observed connection lifecycle (design §10.M2). Coarser than RFC 793 endpoint states:
//! a wire observer sees both directions but not `TIME_WAIT`/`LAST_ACK`.

/// The lifecycle point a connection instance has reached, as observed from the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnState {
    SynSent,
    SynReceived,
    Established,
    FinWait,
    Closed,
    Reset,
}

impl ConnState {
    /// Monotonic rank along the graceful path; `Reset` is a terminal override outside it.
    fn rank(self) -> u8 {
        match self {
            ConnState::SynSent => 0,
            ConnState::SynReceived => 1,
            ConnState::Established => 2,
            ConnState::FinWait => 3,
            ConnState::Closed => 4,
            ConnState::Reset => 5,
        }
    }

    /// Advance to `to` only if it does not move backward along the graceful path. `Reset`
    /// is applied by the caller as an unconditional override, not through here.
    pub(crate) fn advance_to(self, to: ConnState) -> ConnState {
        if to.rank() > self.rank() { to } else { self }
    }
}
