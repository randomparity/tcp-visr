//! ratatui master/detail UI, timeline cursor, and graph views.

pub mod app;
pub mod service;

pub use app::{App, ConnRow, Mode, Outcome, SortDir, SortField};
pub use service::service_name;
