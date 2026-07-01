//! Pure TCP connection state machine + metric derivation (no I/O). M2: connection tracking.

pub mod config;
pub mod conn;
pub mod metrics;
pub mod state;
pub mod timeline;
pub mod tracker;

pub use config::EngineConfig;
pub use conn::{ConnId, Connection, EndpointPair};
pub use metrics::{ConnectionMetrics, MetricError, SeriesCollection};
pub use state::ConnState;
pub use timeline::{AsOf, InFlightSample, SeqKind, SeqSample, StateSample, Timeline};
pub use tracker::{Tracker, track};
