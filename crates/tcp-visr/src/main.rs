//! tcp-visr: visualize TCP flow over time from live capture or pcap replay.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use tcpvisr_core::Item;

/// Visualize TCP flow over time from a live system or a pcap/pcapng replay.
#[derive(Parser)]
#[command(name = "tcp-visr", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Replay a pcap/pcapng capture.
    Replay,
    /// Capture live from a network interface.
    Live,
    /// Print decoded TCP segments from a capture.
    Parse {
        /// The `.pcap`/`.pcapng` capture file to decode.
        file: PathBuf,
    },
    /// List connections in a capture.
    Conns,
    /// Dump a connection's metric series.
    Metrics,
}

impl Command {
    fn name(&self) -> &'static str {
        match self {
            Command::Replay => "replay",
            Command::Live => "live",
            Command::Parse { .. } => "parse",
            Command::Conns => "conns",
            Command::Metrics => "metrics",
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
        Command::Parse { file } => run_parse(&file),
        other => Err(format!(
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
    writeln!(out, "{count} segments, skipped: {} total", skipped.total())?;
    Ok(())
}
