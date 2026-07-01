//! tcp-visr: visualize TCP flow over time from live capture or pcap replay.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use serde::Serialize;
use tcpvisr_core::{Item, MetricSample, Nanos, SampleDir};
use tcpvisr_engine::{ConnectionMetrics, EngineConfig, SeriesCollection, Tracker};

/// Visualize TCP flow over time from a live system or a pcap/pcapng replay.
#[derive(Parser)]
#[command(name = "tcp-visr", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Replay a pcap/pcapng capture in the interactive TUI.
    Replay {
        /// The `.pcap`/`.pcapng` capture file to browse.
        file: PathBuf,
        /// Ceiling on retained per-segment state samples across all connections (must be >= 1).
        /// Exceeding it fails fast rather than risking OOM on a very large capture.
        #[arg(long, default_value_t = 10_000_000)]
        max_samples: usize,
    },
    /// Capture live from a network interface.
    Live,
    /// Print decoded TCP segments from a capture.
    Parse {
        /// The `.pcap`/`.pcapng` capture file to decode.
        file: PathBuf,
    },
    /// List connections in a capture.
    Conns {
        /// The `.pcap`/`.pcapng` capture file to analyze.
        file: PathBuf,
    },
    /// Dump a connection's metric series as JSON.
    Metrics {
        /// The `.pcap`/`.pcapng` capture file to analyze.
        file: PathBuf,
        /// 0-based index of the connection (the order `tcp-visr conns` prints).
        #[arg(long)]
        conn: usize,
        /// Trailing throughput window in milliseconds (must be >= 1).
        #[arg(long, default_value_t = 1000)]
        throughput_window_ms: u64,
        /// Reorder window in milliseconds (a behind-frontier gap below this is out-of-order).
        #[arg(long, default_value_t = 3)]
        reorder_window_ms: u64,
        /// Ceiling on retained samples for the selected connection (must be >= 1).
        #[arg(long, default_value_t = 10_000_000)]
        max_samples: usize,
    },
}

impl Command {
    fn name(&self) -> &'static str {
        match self {
            Command::Replay { .. } => "replay",
            Command::Live => "live",
            Command::Parse { .. } => "parse",
            Command::Conns { .. } => "conns",
            Command::Metrics { .. } => "metrics",
        }
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // Print the actionable Display message (not the Debug form `main`'s Termination
            // would use), then exit non-zero without `process::exit`.
            let _ = writeln!(std::io::stderr().lock(), "error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let Some(command) = cli.command else {
        return Err("no subcommand given; run `tcp-visr --help`".into());
    };
    match command {
        Command::Replay { file, max_samples } => run_replay(&file, max_samples),
        Command::Parse { file } => run_parse(&file),
        Command::Conns { file } => run_conns(&file),
        Command::Metrics {
            file,
            conn,
            throughput_window_ms,
            reorder_window_ms,
            max_samples,
        } => run_metrics(
            &file,
            conn,
            throughput_window_ms,
            reorder_window_ms,
            max_samples,
        ),
        other @ Command::Live => Err(format!(
            "`{}` is not implemented yet (see the milestone roadmap)",
            other.name()
        )
        .into()),
    }
}

/// Decodes `file` and prints one line per TCP segment plus a skip summary, streaming so a large
/// capture does not have to be held in memory at once.
fn run_parse(file: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut out = std::io::stdout().lock();
    let mut count: u64 = 0;
    let mut write_err: Option<std::io::Error> = None;
    let (_link, skipped) = tcpvisr_ingest::parse_file_visit(file, &mut |item| {
        if write_err.is_some() {
            return;
        }
        if let Item::Segment(s) = item {
            count += 1;
            if let Err(e) = writeln!(
                out,
                "{} {} {} seq={} ack={} win={} len={}",
                s.ts, s.flow, s.flags, s.seq.0, s.ack.0, s.window, s.payload_len
            ) {
                write_err = Some(e);
            }
        }
    })?;
    if let Some(e) = write_err {
        return Err(e.into());
    }
    let reasons: Vec<String> = skipped
        .nonzero()
        .into_iter()
        .map(|(reason, n)| format!("{reason}={n}"))
        .collect();
    let breakdown = if reasons.is_empty() {
        String::new()
    } else {
        format!(" ({})", reasons.join(", "))
    };
    writeln!(
        out,
        "{count} segments, skipped: {} total{breakdown}",
        skipped.total()
    )?;
    Ok(())
}

/// Streams `file` through the replay faucet into the engine and prints one line per
/// connection plus a skip summary.
fn run_conns(file: &Path) -> Result<(), Box<dyn std::error::Error>> {
    use tcpvisr_engine::{EngineConfig, Tracker};

    let mut tracker = Tracker::new(EngineConfig::default());
    let (_link, skipped) = tcpvisr_ingest::parse_file_visit(file, &mut |item| {
        tracker.observe(item);
    })?;

    let mut out = std::io::stdout().lock();
    let conns = tracker.into_connections();
    for c in &conns {
        let marker = if c.origin_inferred {
            " (mid-stream)"
        } else {
            ""
        };
        writeln!(
            out,
            "{} -> {}  state={:?}  inst={}  bytes={}/{}  segs={}  dur={}{marker}",
            c.origin,
            c.responder,
            c.state,
            c.id.instance,
            c.bytes_o2r,
            c.bytes_r2o,
            c.segments,
            c.duration(),
        )?;
    }
    let reasons: Vec<String> = skipped
        .nonzero()
        .into_iter()
        .map(|(reason, n)| format!("{reason}={n}"))
        .collect();
    let breakdown = if reasons.is_empty() {
        String::new()
    } else {
        format!(" ({})", reasons.join(", "))
    };
    writeln!(
        out,
        "{} connections, skipped: {} total{breakdown}",
        conns.len(),
        skipped.total()
    )?;
    Ok(())
}

/// Parses `file` into a seekable [`tcpvisr_engine::Timeline`] and builds the replay TUI
/// [`tcpvisr_tui::App`]. No TTY guard, no event loop — this is the testable seam behind
/// `run_replay` (spec §4, criteria 13–14).
fn build_replay_app(
    file: &Path,
    cfg: EngineConfig,
) -> Result<tcpvisr_tui::App, Box<dyn std::error::Error>> {
    let mut tracker = Tracker::new(cfg);
    let (_link, skipped) =
        tcpvisr_ingest::parse_file_visit(file, &mut |item| tracker.observe(item))?;
    let timeline = tracker.into_timeline()?;
    let title = format!(
        "tcp-visr — {}  ({} connections, skipped {})",
        file.display(),
        timeline.connection_count(),
        skipped.total(),
    );
    Ok(tcpvisr_tui::App::new(timeline, title))
}

/// The `EngineConfig` the replay path uses: all three replay timelines on (state, seq,
/// in-flight), plus the sample ceiling.
fn replay_engine_config(max_samples: usize) -> EngineConfig {
    EngineConfig {
        collect_state_timeline: true,
        collect_seq_timeline: true,
        collect_inflight_timeline: true,
        max_samples,
        ..EngineConfig::default()
    }
}

/// Streams `file` into the engine, then browses the resulting connections in the interactive
/// timeline TUI. Requires an interactive terminal; refuses to run when stdout is redirected so
/// it never blocks a pipe.
fn run_replay(file: &Path, max_samples: usize) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::IsTerminal;
    if max_samples == 0 {
        return Err("--max-samples must be at least 1 (got 0)".into());
    }
    if !std::io::stdout().is_terminal() {
        return Err("replay requires an interactive terminal (stdout is not a tty)".into());
    }
    let cfg = replay_engine_config(max_samples);
    let app = build_replay_app(file, cfg)?;
    tcpvisr_tui::run(app)?;
    Ok(())
}

#[derive(Serialize)]
struct ConnectionJson {
    index: usize,
    origin: String,
    responder: String,
    instance: u32,
    state: String,
    origin_inferred: bool,
    opened_at_ns: u64,
    last_at_ns: u64,
}

#[derive(Serialize)]
struct SampleJson {
    t_ns: u64,
    dir: &'static str,
    in_flight: u64,
    throughput_bps: u64,
    rtt_ns: Option<u64>,
    retransmit: bool,
    out_of_order: bool,
    sack: bool,
}

#[derive(Serialize)]
struct MetricsJson {
    connection: ConnectionJson,
    throughput_window_ns: u64,
    reorder_window_ns: u64,
    samples: Vec<SampleJson>,
}

fn dir_str(d: SampleDir) -> &'static str {
    match d {
        SampleDir::OriginToResponder => "o2r",
        SampleDir::ResponderToOrigin => "r2o",
    }
}

fn sample_json(s: &MetricSample) -> SampleJson {
    SampleJson {
        t_ns: s.t.0,
        dir: dir_str(s.dir),
        in_flight: s.in_flight_bytes,
        throughput_bps: s.throughput_bps,
        rtt_ns: s.rtt.map(|n| n.0),
        retransmit: s.retransmit,
        out_of_order: s.out_of_order,
        sack: s.sack,
    }
}

/// Resolves connection `conn` in a lifecycle-only pass, then collects only its series in a
/// second pass, and serializes it as JSON. Two passes are safe because replay is deterministic.
fn run_metrics(
    file: &Path,
    conn: usize,
    throughput_window_ms: u64,
    reorder_window_ms: u64,
    max_samples: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    if throughput_window_ms == 0 {
        return Err("--throughput-window-ms must be at least 1 (got 0)".into());
    }
    if max_samples == 0 {
        return Err("--max-samples must be at least 1 (got 0)".into());
    }

    let base = EngineConfig {
        throughput_window: Nanos(throughput_window_ms.saturating_mul(1_000_000)),
        reorder_window: Nanos(reorder_window_ms.saturating_mul(1_000_000)),
        max_samples,
        ..EngineConfig::default()
    };

    // Pass 1: resolve the target connection (lifecycle only, no series).
    let mut pass1 = Tracker::new(base);
    let _ = tcpvisr_ingest::parse_file_visit(file, &mut |item| pass1.observe(item))?;
    let conns = pass1.into_connections();
    let target = conns.get(conn).ok_or_else(|| {
        format!(
            "connection index {conn} out of range (capture has {} connections, 0..{}); \
             run `tcp-visr conns {}` to list them",
            conns.len(),
            conns.len().saturating_sub(1),
            file.display()
        )
    })?;
    let target_id = target.id;

    // Pass 2: collect only the target's series.
    let cfg = EngineConfig {
        series_collection: SeriesCollection::Only(target_id),
        ..base
    };
    let mut pass2 = Tracker::new(cfg);
    let _ = tcpvisr_ingest::parse_file_visit(file, &mut |item| pass2.observe(item))?;
    let metrics = pass2.into_metrics()?;
    let selected: &ConnectionMetrics = metrics
        .iter()
        .find(|m| m.conn.id == target_id)
        .ok_or("internal: target connection vanished between passes")?;

    let c = &selected.conn;
    let json = MetricsJson {
        connection: ConnectionJson {
            index: conn,
            origin: c.origin.to_string(),
            responder: c.responder.to_string(),
            instance: c.id.instance,
            state: format!("{:?}", c.state),
            origin_inferred: c.origin_inferred,
            opened_at_ns: c.opened_at.0,
            last_at_ns: c.last_at.0,
        },
        throughput_window_ns: cfg.throughput_window.0,
        reorder_window_ns: cfg.reorder_window.0,
        samples: selected.series.iter().map(sample_json).collect(),
    };

    let mut out = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut out, &json)?;
    writeln!(out)?; // trailing newline
    Ok(())
}

#[cfg(test)]
mod build_replay_tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/metrics_basic.pcap")
    }

    #[test]
    fn builds_a_timeline_app_with_rows() {
        let cfg = EngineConfig {
            collect_state_timeline: true,
            ..EngineConfig::default()
        };
        let app = build_replay_app(&fixture(), cfg).expect("build");
        assert!(
            !app.visible().is_empty(),
            "fixture has connections active at the initial cursor"
        );
    }

    #[test]
    fn sample_ceiling_is_fatal() {
        let cfg = EngineConfig {
            collect_state_timeline: true,
            max_samples: 1,
            ..EngineConfig::default()
        };
        let err = build_replay_app(&fixture(), cfg).expect_err("ceiling");
        let msg = err.to_string();
        assert!(msg.contains("--max-samples"), "actionable: {msg}");
    }

    #[test]
    fn run_replay_config_enables_inflight_collection() {
        // The replay path must turn the flag on; guard against a regression that drops it.
        // We cannot run the TUI here, so assert the config the replay path builds.
        let cfg = replay_engine_config(10_000_000);
        assert!(
            cfg.collect_inflight_timeline,
            "replay must collect the in-flight timeline"
        );
        assert!(
            cfg.collect_seq_timeline && cfg.collect_state_timeline,
            "M5/M6 series still on"
        );
    }

    #[test]
    fn build_replay_app_collects_inflight_series_for_the_focus_connection() {
        let cfg = EngineConfig {
            collect_state_timeline: true,
            collect_seq_timeline: true,
            collect_inflight_timeline: true,
            ..EngineConfig::default()
        };
        let app = build_replay_app(&fixture(), cfg).expect("build");
        let focus = app
            .focus()
            .expect("a connection is selected at the initial cursor");
        assert!(
            !focus.inflight.is_empty(),
            "fixture with data segments yields a non-empty focus in-flight series"
        );
    }

    #[test]
    fn build_replay_app_collects_seq_series_for_the_focus_connection() {
        let cfg = EngineConfig {
            collect_state_timeline: true,
            collect_seq_timeline: true,
            ..EngineConfig::default()
        };
        let app = build_replay_app(&fixture(), cfg).expect("build");
        let focus = app
            .focus()
            .expect("a connection is selected at the initial cursor");
        assert!(
            !focus.series.is_empty(),
            "fixture with data segments yields a non-empty focus seq series"
        );
    }
}
