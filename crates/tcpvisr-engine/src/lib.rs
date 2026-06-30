//! Pure TCP connection state machine + metric derivation (no I/O). M2: connection tracking.

pub mod config;
pub mod conn;
pub mod state;
pub mod tracker;

pub use config::EngineConfig;
pub use conn::{ConnId, Connection, EndpointPair};
pub use state::ConnState;
