//! Modal key handling: the single pure decision point for the master list (spec §3.5).

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{App, Mode, Outcome};

/// Maps a key press to state changes on `app`, returning whether to keep running.
///
/// Modal (spec §3.5): in navigation mode the command keys sort / move / filter /
/// quit; in filter-input mode every printable character is appended to the query
/// and only Enter/Esc/Backspace are commands. `Ctrl-C` quits in either mode.
pub fn handle_key(app: &mut App, key: KeyEvent) -> Outcome {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Outcome::Quit;
    }
    match app.mode() {
        Mode::Nav => handle_nav(app, key.code),
        Mode::Filter => {
            handle_filter(app, key.code);
            Outcome::Continue
        }
    }
}

fn handle_nav(app: &mut App, code: KeyCode) -> Outcome {
    match code {
        KeyCode::Char('q') => return Outcome::Quit,
        KeyCode::Char('s') => app.cycle_sort(),
        KeyCode::Char('S') => app.toggle_dir(),
        KeyCode::Char('/') => app.enter_filter(),
        KeyCode::Char('j') | KeyCode::Down => app.move_down(),
        KeyCode::Char('k') | KeyCode::Up => app.move_up(),
        _ => {}
    }
    Outcome::Continue
}

fn handle_filter(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Enter => app.confirm_filter(),
        KeyCode::Esc => app.cancel_filter(),
        KeyCode::Backspace => app.pop_filter(),
        KeyCode::Char(c) => app.push_filter(c),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::handle_key;
    use crate::app::{App, Mode, Outcome, SortField};
    use core::net::{IpAddr, Ipv4Addr};
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use tcpvisr_core::{Endpoint, Nanos};
    use tcpvisr_engine::{ConnId, ConnState, Connection, EndpointPair, StateSample, Timeline};

    fn ep(a: u8, port: u16) -> Endpoint {
        Endpoint {
            ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, a)),
            port,
        }
    }

    fn entry(origin: Endpoint, responder: Endpoint) -> (Connection, Vec<StateSample>) {
        let c = Connection {
            id: ConnId {
                pair: EndpointPair::new(origin, responder),
                instance: 0,
            },
            state: ConnState::Established,
            origin,
            responder,
            origin_inferred: false,
            opened_at: Nanos(0),
            last_at: Nanos(1),
            bytes_o2r: 0,
            bytes_r2o: 0,
            segments: 1,
        };
        let s = StateSample {
            t: Nanos(0),
            state: ConnState::Established,
            bytes_o2r: 0,
            bytes_r2o: 0,
        };
        (c, vec![s])
    }

    fn app() -> App {
        App::new(
            Timeline::new(vec![
                entry(ep(1, 1), ep(2, 22)),
                entry(ep(1, 2), ep(3, 443)),
            ]),
            "t".to_string(),
        )
    }

    fn press(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    #[test]
    fn q_quits_in_nav_mode() {
        let mut a = app();
        assert_eq!(handle_key(&mut a, press('q')), Outcome::Quit);
    }

    #[test]
    fn ctrl_c_quits_in_nav_mode() {
        let mut a = app();
        let ev = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(handle_key(&mut a, ev), Outcome::Quit);
    }

    #[test]
    fn q_in_filter_mode_appends_and_does_not_quit() {
        let mut a = app();
        assert_eq!(handle_key(&mut a, press('/')), Outcome::Continue);
        assert_eq!(a.mode(), Mode::Filter);
        assert_eq!(handle_key(&mut a, press('q')), Outcome::Continue);
        assert_eq!(a.query(), "q");
    }

    #[test]
    fn ctrl_c_quits_even_in_filter_mode() {
        let mut a = app();
        handle_key(&mut a, press('/'));
        let ev = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(handle_key(&mut a, ev), Outcome::Quit);
    }

    #[test]
    fn s_cycles_sort_in_nav_mode() {
        let mut a = app();
        assert_eq!(a.sort_field(), SortField::Peer);
        handle_key(&mut a, press('s'));
        assert_eq!(a.sort_field(), SortField::State);
    }

    #[test]
    fn esc_clears_filter() {
        let mut a = app();
        handle_key(&mut a, press('/'));
        handle_key(&mut a, press('s'));
        handle_key(&mut a, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(a.mode(), Mode::Nav);
        assert_eq!(a.query(), "");
    }
}
