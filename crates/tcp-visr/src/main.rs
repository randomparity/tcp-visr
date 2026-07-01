//! tcp-visr: visualize TCP flow over time from live capture or pcap replay.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use serde::Serialize;
use tcpvisr_core::{Item, MetricSample, Nanos, SampleDir};
use tcpvisr_engine::{ConnectionMetrics, EngineConfig, RetentionPolicy, SeriesCollection, Tracker};

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
    /// Capture live from a network interface (requires the `live` feature).
    Live {
        /// Interface to capture on (e.g. eth0). Omit with --list-interfaces.
        #[arg(short = 'i', long)]
        iface: Option<String>,
        /// Optional BPF filter expression (e.g. "tcp port 443").
        #[arg(long)]
        filter: Option<String>,
        /// Display/eviction window in seconds (samples older than this age out).
        #[arg(long, default_value_t = 120)]
        retention_secs: u64,
        /// List capturable interfaces and exit.
        #[arg(long)]
        list_interfaces: bool,
    },
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
        Command::Live {
            iface,
            filter,
            retention_secs,
            list_interfaces,
        } => run_live_command(iface, filter, retention_secs, list_interfaces),
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
    let mut names = tcpvisr_core::NameTable::default();
    let (_link, skipped) = tcpvisr_ingest::parse_file_visit_named(
        file,
        &mut |item| tracker.observe(item),
        &mut |obs| names.observe(obs.clone()),
    )?;
    let timeline = tracker.into_timeline()?;
    let capped = if names.dropped() > 0 {
        ", names capped"
    } else {
        ""
    };
    let title = format!(
        "tcp-visr — {}  ({} connections, {} names, skipped {}{capped})",
        file.display(),
        timeline.connection_count(),
        names.len(),
        skipped.total(),
    );
    Ok(tcpvisr_tui::App::new_with_names(timeline, &names, title))
}

/// The `EngineConfig` the replay path uses: all five replay timelines on (state, seq, in-flight,
/// rtt, throughput), plus the sample ceiling.
fn replay_engine_config(max_samples: usize) -> EngineConfig {
    EngineConfig {
        collect_state_timeline: true,
        collect_seq_timeline: true,
        collect_inflight_timeline: true,
        collect_rtt_timeline: true,
        collect_throughput_timeline: true,
        retention: RetentionPolicy::FailFast { max_samples },
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
        retention: RetentionPolicy::FailFast { max_samples },
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

/// The silent-empty grace window: a live capture may see zero packets this long before it hints a
/// privilege/interface/filter problem (design §7). Non-fatal; the hint clears on the first packet.
#[cfg(any(feature = "live", test))]
const LIVE_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// The advisory shown while a live capture has produced no packets past the grace window. `None`
/// once any packet arrives, or before the window elapses — a genuinely idle interface is not an
/// error.
#[cfg(any(feature = "live", test))]
fn grace_hint(first_packet_seen: bool, elapsed: std::time::Duration) -> Option<&'static str> {
    if first_packet_seen || elapsed < LIVE_GRACE {
        None
    } else {
        Some("no packets yet — check privileges, interface, or filter")
    }
}

/// Sends `item` on the bounded channel, or counts a drop when it is full — the live path never
/// blocks the wire or buffers unbounded (design §7).
#[cfg(any(feature = "live", test))]
fn try_send_or_count<T>(
    tx: &std::sync::mpsc::SyncSender<T>,
    item: T,
    dropped: &std::sync::atomic::AtomicU64,
) {
    if tx.try_send(item).is_err() {
        dropped.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// The engine config for live capture: all five detail series on, time-horizon eviction over
/// `retention_secs`, with a modest sample backstop (2M) — live evicts, never fails fast.
#[cfg(any(feature = "live", test))]
fn live_engine_config(retention_secs: u64) -> EngineConfig {
    EngineConfig {
        collect_state_timeline: true,
        collect_seq_timeline: true,
        collect_inflight_timeline: true,
        collect_rtt_timeline: true,
        collect_throughput_timeline: true,
        series_collection: SeriesCollection::All,
        retention: RetentionPolicy::Evict {
            window: Nanos(retention_secs.saturating_mul(1_000_000_000)),
            max_samples: 2_000_000,
        },
        ..EngineConfig::default()
    }
}

/// Stops and joins the background capture thread on drop, so the device is released on normal exit
/// (`q`/Ctrl-C) and on a panic/unwind alike — not just when the terminal is restored.
#[cfg(feature = "live")]
struct CaptureGuard {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

#[cfg(feature = "live")]
impl Drop for CaptureGuard {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Live capture: opens the interface, spawns the capture thread behind a bounded channel, and
/// drives the live TUI, folding each frame's items into a bounded tracker and retargeting the app.
#[cfg(feature = "live")]
fn run_live_command(
    iface: Option<String>,
    filter: Option<String>,
    retention_secs: u64,
    list_interfaces: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::mpsc::sync_channel;
    use std::time::Instant;
    use tcpvisr_core::{Item, NameTable};
    use tcpvisr_ingest::{LiveCapture, LiveEvent, LiveOptions};
    use tcpvisr_tui::{App, LiveStatus};

    if list_interfaces {
        let mut out = std::io::stdout().lock();
        for ifc in tcpvisr_ingest::list_interfaces()? {
            match ifc.description {
                Some(d) => writeln!(out, "{}  {d}", ifc.name)?,
                None => writeln!(out, "{}", ifc.name)?,
            }
        }
        return Ok(());
    }

    let iface = iface.ok_or("live requires -i <iface> (or --list-interfaces)")?;
    let mut opts = LiveOptions::new(&iface);
    opts.filter = filter;
    let capture = LiveCapture::open(&opts)?;

    let (tx, rx) = sync_channel::<LiveEvent>(65_536);
    let stop = Arc::new(AtomicBool::new(false));
    let dropped = Arc::new(AtomicU64::new(0));

    let guard = {
        let thread_stop = Arc::clone(&stop);
        let thread_dropped = Arc::clone(&dropped);
        let handle = std::thread::spawn(move || {
            let _ = capture.run(
                |ev| try_send_or_count(&tx, ev, &thread_dropped),
                &thread_stop,
            );
        });
        CaptureGuard {
            stop: Arc::clone(&stop),
            handle: Some(handle),
        }
    };

    let mut tracker = Tracker::new(live_engine_config(retention_secs));
    let base_title = format!("tcp-visr — live {iface}");
    let app = App::new_live(&NameTable::default(), base_title.clone());

    let start = Instant::now();
    let mut first_packet = false;
    let next_frame = |app: &mut App| {
        while let Ok(ev) = rx.try_recv() {
            match ev {
                LiveEvent::Item(item) => {
                    if matches!(item, Item::Segment(_)) {
                        first_packet = true;
                    }
                    tracker.observe(&item);
                }
                LiveEvent::Name(obs) => app.observe_name(obs),
            }
        }
        let d = dropped.load(Ordering::Relaxed);
        let snap = tracker.snapshot();
        app.retarget(
            snap,
            tracker.retention_horizon(),
            tracker.now(),
            LiveStatus {
                dropped: d,
                approximate: d > 0,
            },
        );
        match grace_hint(first_packet, start.elapsed()) {
            Some(hint) => app.set_title(format!("{base_title}  [{hint}]")),
            None => app.set_title(base_title.clone()),
        }
    };

    let result = tcpvisr_tui::run_live(app, next_frame);
    drop(guard); // sets stop + joins the capture thread (also on unwind)
    result.map_err(Into::into)
}

/// Feature-off stub: the default binary is built without libpcap (ADR-0003), so `live` is
/// unavailable but still listed in `--help`.
#[cfg(not(feature = "live"))]
fn run_live_command(
    _iface: Option<String>,
    _filter: Option<String>,
    _retention_secs: u64,
    _list_interfaces: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    Err(
        "live capture: this binary was built without live support; rebuild with --features live"
            .into(),
    )
}

#[cfg(test)]
mod live_cli_tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use clap::Parser;

    #[test]
    fn live_parses_iface_and_retention() {
        let cli = Cli::try_parse_from(["tcp-visr", "live", "-i", "eth0", "--retention-secs", "60"])
            .unwrap();
        match cli.command {
            Some(Command::Live {
                iface,
                retention_secs,
                ..
            }) => {
                assert_eq!(iface.as_deref(), Some("eth0"));
                assert_eq!(retention_secs, 60);
            }
            _ => unreachable!("expected Live subcommand"),
        }
    }

    #[test]
    fn live_defaults_retention_to_120s() {
        let cli = Cli::try_parse_from(["tcp-visr", "live", "-i", "eth0"]).unwrap();
        match cli.command {
            Some(Command::Live {
                retention_secs,
                list_interfaces,
                filter,
                ..
            }) => {
                assert_eq!(retention_secs, 120);
                assert!(!list_interfaces);
                assert!(filter.is_none());
            }
            _ => unreachable!("expected Live subcommand"),
        }
    }

    #[test]
    fn live_list_interfaces_parses_without_iface() {
        let cli = Cli::try_parse_from(["tcp-visr", "live", "--list-interfaces"]).unwrap();
        match cli.command {
            Some(Command::Live {
                list_interfaces,
                iface,
                ..
            }) => {
                assert!(list_interfaces);
                assert!(iface.is_none());
            }
            _ => unreachable!("expected Live subcommand"),
        }
    }

    #[test]
    fn grace_hint_is_advisory_only_while_silent_past_the_window() {
        use std::time::Duration;
        assert!(
            grace_hint(false, Duration::from_secs(1)).is_none(),
            "quiet but within the grace window is not yet a hint"
        );
        assert!(
            grace_hint(true, Duration::from_secs(30)).is_none(),
            "a packet was seen -> no hint"
        );
        assert!(
            grace_hint(false, Duration::from_secs(6)).is_some(),
            "silent past the window -> advisory hint"
        );
    }

    #[test]
    fn live_engine_config_uses_evict_over_the_retention_window() {
        let cfg = live_engine_config(120);
        assert_eq!(
            cfg.retention,
            RetentionPolicy::Evict {
                window: Nanos(120_000_000_000),
                max_samples: 2_000_000,
            }
        );
        assert!(cfg.collect_throughput_timeline && cfg.collect_state_timeline);
        assert_eq!(cfg.series_collection, SeriesCollection::All);
    }

    #[test]
    fn bounded_channel_full_send_is_dropped_and_counted() {
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::sync::mpsc::sync_channel;
        let (tx, _rx) = sync_channel::<u8>(1);
        let dropped = AtomicU64::new(0);
        try_send_or_count(&tx, 1, &dropped); // fits the 1-slot buffer
        try_send_or_count(&tx, 2, &dropped); // buffer full (rx idle) -> dropped + counted
        assert_eq!(dropped.load(Ordering::Relaxed), 1);
    }

    #[cfg(not(feature = "live"))]
    #[test]
    fn live_without_feature_errors_clearly() {
        let err = run_live_command(Some("eth0".into()), None, 120, false).expect_err("no live");
        assert!(err.to_string().contains("without live support"), "{err}");
    }
}

#[cfg(test)]
mod build_replay_tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/metrics_basic.pcap")
    }

    fn dns_fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/name_resolution.pcap")
    }

    #[test]
    fn build_replay_app_resolves_and_counts_names() {
        let cfg = replay_engine_config(10_000_000);
        let app = build_replay_app(&dns_fixture(), cfg).expect("build");
        // The responder (93.184.216.34:443) resolves to the fixture's DNS host name.
        let row = app
            .visible()
            .into_iter()
            .find(|r| r.host.is_some())
            .expect("a resolved row");
        assert_eq!(
            row.host.as_ref().map(tcpvisr_core::HostName::as_ref),
            Some("example.com")
        );
        // The title reports the resolved-name count.
        assert!(app.title().contains("1 names"), "title: {}", app.title());
    }

    #[test]
    fn build_replay_app_reports_zero_names_without_dns() {
        let cfg = replay_engine_config(10_000_000);
        let app = build_replay_app(&fixture(), cfg).expect("build");
        assert!(app.title().contains("0 names"), "title: {}", app.title());
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
            retention: RetentionPolicy::FailFast { max_samples: 1 },
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
    fn run_replay_config_enables_rtt_collection() {
        let cfg = replay_engine_config(10_000_000);
        assert!(
            cfg.collect_rtt_timeline,
            "replay must collect the RTT timeline"
        );
    }

    #[test]
    fn run_replay_config_enables_throughput_collection() {
        let cfg = replay_engine_config(10_000_000);
        assert!(
            cfg.collect_throughput_timeline,
            "replay must collect the throughput timeline"
        );
    }

    #[test]
    fn build_replay_app_collects_throughput_series_for_the_focus_connection() {
        // metrics_basic connection 0: focus dir O2R (SYN + 100 B data >> 1-B SYN-ACK). The 100 B
        // O2R data at t=2ms is not a retransmit, so the O2R throughput sample there is
        // throughput_bps == goodput_bps == 800 (100 B * 8 / 1s window), verified from the M3 oracle.
        let cfg = replay_engine_config(10_000_000);
        let app = build_replay_app(&fixture(), cfg).expect("build");
        let focus = app
            .focus()
            .expect("a connection is selected at the initial cursor");
        assert!(
            !focus.throughput.is_empty(),
            "fixture with sent data yields a non-empty focus throughput series"
        );
        let at_2ms = focus
            .throughput
            .iter()
            .find(|s| {
                s.dir == tcpvisr_core::SampleDir::OriginToResponder && s.t == Nanos(2_000_000)
            })
            .expect("an O2R throughput sample at t=2ms (the 100 B data send)");
        assert_eq!(at_2ms.throughput_bps, 800, "100 B over the 1s window");
        assert_eq!(at_2ms.goodput_bps, 800, "the 100 B is not a retransmit");
    }

    #[test]
    fn build_replay_app_collects_rtt_series_for_the_focus_connection() {
        // metrics_basic connection 0: focus dir O2R (SYN + 100 B data >> 1-B SYN-ACK) has RTT
        // samples at t=1ms and t=3ms (SYN-ACK acking the SYN; final ACK acking the O2R data),
        // verified from the M3 oracle.
        let cfg = replay_engine_config(10_000_000);
        let app = build_replay_app(&fixture(), cfg).expect("build");
        let focus = app
            .focus()
            .expect("a connection is selected at the initial cursor");
        assert!(
            !focus.rtt.is_empty(),
            "fixture with acked data yields a non-empty focus RTT series"
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
