//! ratatui master/detail UI, timeline cursor, and graph views.

pub mod app;
pub mod detail;
pub mod inflight;
pub mod keys;
pub mod render;
pub mod rtt;
pub mod run;
pub mod service;
pub mod transport;

pub use app::{App, ConnRow, DetailView, Mode, Outcome, SortDir, SortField};
pub use inflight::{InFlightPlot, Series};
pub use keys::handle_key;
pub use render::render;
pub use rtt::{RttPlot, Series as RttSeries};
pub use run::run;
pub use service::service_name;
pub use transport::Transport;
