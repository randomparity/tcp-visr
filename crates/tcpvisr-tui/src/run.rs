//! The impure terminal shell: init, event loop, restore (spec §4, ADR-0009).

use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyEventKind};

use crate::app::{App, Outcome};
use crate::keys::handle_key;
use crate::render::render;

/// Runs the master-list TUI: sets up the terminal, loops until the user quits,
/// then restores the terminal. Restoration also runs on panic via the hook
/// `ratatui::init` installs.
///
/// # Errors
///
/// Returns any I/O error from drawing a frame or reading a terminal event; the
/// terminal is restored before the error propagates.
pub fn run(app: App) -> std::io::Result<()> {
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, app);
    ratatui::restore();
    result
}

fn event_loop(terminal: &mut DefaultTerminal, mut app: App) -> std::io::Result<()> {
    loop {
        terminal.draw(|frame| render(frame, &app))?;
        if let Event::Key(key) = event::read()? {
            if key.kind == KeyEventKind::Press && handle_key(&mut app, key) == Outcome::Quit {
                break;
            }
        }
    }
    Ok(())
}
