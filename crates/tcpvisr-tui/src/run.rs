//! The impure terminal shell: init, poll/tick event loop, restore (spec §3.8, ADR-0002/0010).
//! This is the only code that reads a clock; the pure `App`/`Transport`/`Timeline` take the
//! wall-clock delta as data.

use std::time::{Duration, Instant};

use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use tcpvisr_core::Nanos;

use crate::app::{App, Outcome};
use crate::keys::handle_key;
use crate::render::render;

/// Poll timeout between frames; also the playback frame cadence.
const TICK: Duration = Duration::from_millis(50);

/// Runs the timeline TUI: sets up the terminal, loops (render → poll a key → advance the
/// cursor by the elapsed wall time) until the user quits, then restores the terminal.
/// Restoration also runs on panic via the hook `ratatui::init` installs.
///
/// # Errors
///
/// Returns any I/O error from drawing a frame or reading a terminal event; the terminal is
/// restored before the error propagates.
pub fn run(app: App) -> std::io::Result<()> {
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, app);
    ratatui::restore();
    result
}

fn event_loop(terminal: &mut DefaultTerminal, mut app: App) -> std::io::Result<()> {
    let mut last = Instant::now();
    loop {
        terminal.draw(|frame| render(frame, &app))?;
        if event::poll(TICK)? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press && handle_key(&mut app, key) == Outcome::Quit {
                    break;
                }
            }
        }
        let now = Instant::now();
        let dt = u64::try_from(now.duration_since(last).as_nanos()).unwrap_or(u64::MAX);
        last = now;
        app.tick(Nanos(dt));
    }
    Ok(())
}
