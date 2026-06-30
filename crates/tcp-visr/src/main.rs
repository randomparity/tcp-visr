//! tcp-visr: visualize TCP flow over time from live capture or pcap replay.

use clap::{Parser, Subcommand};

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
    Parse,
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
            Command::Parse => "parse",
            Command::Conns => "conns",
            Command::Metrics => "metrics",
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let Some(command) = cli.command else {
        return Err("no subcommand given; run `tcp-visr --help`".into());
    };
    let name = command.name();
    Err(format!("`{name}` is not implemented yet (see the milestone roadmap)").into())
}
