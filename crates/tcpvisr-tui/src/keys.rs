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
        KeyCode::Char(' ') => {
            if app.is_live() {
                app.toggle_follow();
            } else {
                app.toggle_play();
            }
        }
        KeyCode::Left => {
            app.freeze_if_live();
            app.seek(false);
        }
        KeyCode::Right => {
            app.freeze_if_live();
            app.seek(true);
        }
        KeyCode::Char('+' | '=') => app.faster(),
        KeyCode::Char('-' | '_') => app.slower(),
        KeyCode::Char('.') => {
            app.freeze_if_live();
            app.step_forward();
        }
        KeyCode::Char(',') => {
            app.freeze_if_live();
            app.step_back();
        }
        KeyCode::Enter => app.open_detail(),
        KeyCode::Esc => app.close_detail(),
        KeyCode::Tab => app.cycle_detail_view(),
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

    /// One connection open on `[0, 1000]` with events at t=0 and t=500, so seek/step move the
    /// cursor meaningfully.
    fn wide_app() -> App {
        let c = Connection {
            id: ConnId {
                pair: EndpointPair::new(ep(1, 1), ep(2, 22)),
                instance: 0,
            },
            state: ConnState::Established,
            origin: ep(1, 1),
            responder: ep(2, 22),
            origin_inferred: false,
            opened_at: Nanos(0),
            last_at: Nanos(1000),
            bytes_o2r: 0,
            bytes_r2o: 0,
            segments: 2,
        };
        let samples = vec![
            StateSample {
                t: Nanos(0),
                state: ConnState::Established,
                bytes_o2r: 0,
                bytes_r2o: 0,
            },
            StateSample {
                t: Nanos(500),
                state: ConnState::Established,
                bytes_o2r: 10,
                bytes_r2o: 0,
            },
        ];
        App::new(Timeline::new(vec![(c, samples)]), "t".to_string())
    }

    fn press(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn space_toggles_play_in_nav_mode() {
        let mut a = app();
        assert!(!a.is_playing());
        assert_eq!(handle_key(&mut a, press(' ')), Outcome::Continue);
        assert!(a.is_playing());
        handle_key(&mut a, press(' '));
        assert!(!a.is_playing());
    }

    fn live_app() -> App {
        use crate::app::LiveStatus;
        use tcpvisr_core::NameTable;
        let mut a = App::new_live(&NameTable::default(), "live".to_string());
        let (c, s) = entry(ep(1, 1), ep(2, 22));
        let tl =
            Timeline::with_seq_ending(vec![(c, s, vec![], vec![], vec![], vec![])], Nanos(1000));
        a.retarget(tl, Nanos(0), Nanos(1000), LiveStatus::default());
        a
    }

    #[test]
    fn space_toggles_follow_in_live_mode() {
        let mut a = live_app();
        assert!(a.is_following(), "live starts following");
        handle_key(&mut a, press(' '));
        assert!(!a.is_following(), "space freezes");
        assert!(
            !a.is_playing(),
            "space does not start replay playback in live mode"
        );
        handle_key(&mut a, press(' '));
        assert!(a.is_following(), "space re-follows");
    }

    #[test]
    fn seek_freezes_in_live_mode() {
        let mut a = live_app();
        assert!(a.is_following());
        handle_key(&mut a, key(KeyCode::Left));
        assert!(!a.is_following(), "a manual seek freezes the live cursor");
    }

    #[test]
    fn arrows_seek_the_cursor() {
        let mut a = wide_app(); // bounds 0..1000, seek step = 1000/50 = 20
        handle_key(&mut a, key(KeyCode::Right));
        assert_eq!(a.cursor(), Nanos(20));
        handle_key(&mut a, key(KeyCode::Left));
        assert_eq!(a.cursor(), Nanos(0));
    }

    #[test]
    fn plus_minus_change_speed() {
        let mut a = app();
        assert!((a.speed() - 1.0).abs() < 1e-9);
        handle_key(&mut a, press('+'));
        assert!((a.speed() - 2.0).abs() < 1e-9);
        handle_key(&mut a, press('='));
        assert!((a.speed() - 5.0).abs() < 1e-9);
        handle_key(&mut a, press('-'));
        assert!((a.speed() - 2.0).abs() < 1e-9);
    }

    #[test]
    fn period_and_comma_step_events() {
        let mut a = wide_app(); // events at 0 and 500
        handle_key(&mut a, press('.'));
        assert_eq!(a.cursor(), Nanos(500));
        handle_key(&mut a, press(','));
        assert_eq!(a.cursor(), Nanos(0));
    }

    #[test]
    fn transport_keys_are_inert_in_filter_mode() {
        let mut a = wide_app();
        handle_key(&mut a, press('/')); // enter filter
        for c in [' ', '+', ',', '.'] {
            handle_key(&mut a, press(c));
        }
        assert_eq!(a.query(), " +,.", "printable keys append to the query");
        assert!(!a.is_playing(), "space did not toggle play in filter mode");
        assert!((a.speed() - 1.0).abs() < 1e-9, "+ did not change speed");
        assert_eq!(a.cursor(), Nanos(0), ". did not step the cursor");
        let ev = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(handle_key(&mut a, ev), Outcome::Quit);
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
    fn enter_opens_and_esc_closes_detail_in_nav_mode() {
        let mut a = app();
        handle_key(&mut a, key(KeyCode::Enter));
        assert!(a.is_detail_open());
        handle_key(&mut a, key(KeyCode::Esc));
        assert!(!a.is_detail_open());
    }

    #[test]
    fn filter_mode_enter_and_esc_do_not_touch_detail() {
        let mut a = app();
        handle_key(&mut a, press('/')); // enter filter mode
        handle_key(&mut a, press('s'));
        handle_key(&mut a, key(KeyCode::Enter)); // confirms filter, does not open detail
        assert!(!a.is_detail_open());
        assert_eq!(a.mode(), Mode::Nav);
        handle_key(&mut a, press('/'));
        handle_key(&mut a, key(KeyCode::Esc)); // clears filter, does not close/open detail
        assert!(!a.is_detail_open());
    }

    #[test]
    fn tab_cycles_view_in_nav_mode() {
        use crate::app::DetailView;
        let mut a = app();
        assert_eq!(a.detail_view(), DetailView::TimeSequence);
        handle_key(&mut a, key(KeyCode::Tab));
        assert_eq!(a.detail_view(), DetailView::InFlight);
    }

    #[test]
    fn tab_does_not_cycle_view_in_filter_mode() {
        use crate::app::DetailView;
        let mut a = app();
        handle_key(&mut a, press('/')); // enter filter
        handle_key(&mut a, key(KeyCode::Tab));
        assert_eq!(
            a.detail_view(),
            DetailView::TimeSequence,
            "Tab inert for view-switching in filter mode"
        );
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
