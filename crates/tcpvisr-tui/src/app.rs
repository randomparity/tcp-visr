//! Pure master-list state: rows, sort, filter, selection (spec §3, §4; ADR-0009).

use tcpvisr_core::Endpoint;
use tcpvisr_engine::{ConnId, ConnState, Connection};

use crate::service::service_name;

/// One master-list row: a display projection of a tracked [`Connection`].
#[derive(Debug, Clone)]
pub struct ConnRow {
    pub id: ConnId,
    pub peer: Endpoint,
    pub service: Option<&'static str>,
    pub state: ConnState,
    pub origin_inferred: bool,
    pub bytes_up: u64,
    pub bytes_down: u64,
    pub search: String,
}

/// The column the list is ordered by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortField {
    Peer,
    State,
    BytesUp,
    BytesDown,
}

/// Sort direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDir {
    Asc,
    Desc,
}

/// Which key-handling mode the app is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Nav,
    Filter,
}

/// Result of handling a key: keep running or quit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Continue,
    Quit,
}

/// Pure interactive state for the master list. No I/O, no clock.
#[derive(Debug, Clone)]
pub struct App {
    rows: Vec<ConnRow>,
    sort_field: SortField,
    sort_dir: SortDir,
    mode: Mode,
    query: String,
    selected: Option<ConnId>,
    title: String,
}

impl App {
    /// Builds the app from a capture's connections and a header title string.
    #[must_use]
    pub fn new(conns: &[Connection], title: String) -> Self {
        let rows: Vec<ConnRow> = conns.iter().map(ConnRow::from_connection).collect();
        let mut app = Self {
            rows,
            sort_field: SortField::Peer,
            sort_dir: SortDir::Asc,
            mode: Mode::Nav,
            query: String::new(),
            selected: None,
            title,
        };
        app.selected = app.visible().first().map(|r| r.id);
        app
    }

    /// The filtered + sorted rows, in display order.
    #[must_use]
    pub fn visible(&self) -> Vec<&ConnRow> {
        let q = self.query.to_lowercase();
        let mut rows: Vec<&ConnRow> = self
            .rows
            .iter()
            .filter(|r| is_subsequence(&q, &r.search))
            .collect();
        rows.sort_by(|a, b| self.order(a, b));
        rows
    }

    /// Advances the sort field (wrapping) and resets it to its natural direction.
    pub fn cycle_sort(&mut self) {
        self.sort_field = match self.sort_field {
            SortField::Peer => SortField::State,
            SortField::State => SortField::BytesUp,
            SortField::BytesUp => SortField::BytesDown,
            SortField::BytesDown => SortField::Peer,
        };
        self.sort_dir = natural_dir(self.sort_field);
    }

    /// Toggles ascending/descending for the current field only.
    pub fn toggle_dir(&mut self) {
        self.sort_dir = match self.sort_dir {
            SortDir::Asc => SortDir::Desc,
            SortDir::Desc => SortDir::Asc,
        };
    }

    /// Enters filter-input mode, keeping any existing query for editing.
    pub fn enter_filter(&mut self) {
        self.mode = Mode::Filter;
    }

    /// Appends a character to the filter query and reconciles the selection.
    pub fn push_filter(&mut self, c: char) {
        self.query.push(c);
        self.reconcile_selection();
    }

    /// Removes the last character of the filter query and reconciles the selection.
    pub fn pop_filter(&mut self) {
        self.query.pop();
        self.reconcile_selection();
    }

    /// Leaves filter-input mode, keeping the current filter applied.
    pub fn confirm_filter(&mut self) {
        self.mode = Mode::Nav;
    }

    /// Clears the query and leaves filter-input mode.
    pub fn cancel_filter(&mut self) {
        self.query.clear();
        self.mode = Mode::Nav;
        self.reconcile_selection();
    }

    /// Moves the selection up one row in the visible order (clamped).
    pub fn move_up(&mut self) {
        self.move_by(Step::Up);
    }

    /// Moves the selection down one row in the visible order (clamped).
    pub fn move_down(&mut self) {
        self.move_by(Step::Down);
    }

    fn move_by(&mut self, step: Step) {
        let visible = self.visible();
        let Some(last) = visible.len().checked_sub(1) else {
            self.selected = None;
            return;
        };
        let cur = self
            .selected
            .and_then(|id| visible.iter().position(|r| r.id == id))
            .unwrap_or(0);
        let next = match step {
            Step::Up => cur.saturating_sub(1),
            Step::Down => (cur + 1).min(last),
        };
        self.selected = Some(visible[next].id);
    }

    /// Ensures the selected id is still visible; otherwise selects the first
    /// visible row (or `None` if nothing is visible).
    fn reconcile_selection(&mut self) {
        let visible = self.visible();
        let still_visible = self
            .selected
            .is_some_and(|id| visible.iter().any(|r| r.id == id));
        if !still_visible {
            self.selected = visible.first().map(|r| r.id);
        }
    }

    /// The header title (file + connection/skip counts).
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// The current sort field.
    #[must_use]
    pub fn sort_field(&self) -> SortField {
        self.sort_field
    }

    /// The current sort direction.
    #[must_use]
    pub fn sort_dir(&self) -> SortDir {
        self.sort_dir
    }

    /// The current key-handling mode.
    #[must_use]
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// The current filter query.
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// The selected connection, if any.
    #[must_use]
    pub fn selected(&self) -> Option<ConnId> {
        self.selected
    }

    fn order(&self, a: &ConnRow, b: &ConnRow) -> std::cmp::Ordering {
        let base = match self.sort_field {
            SortField::Peer => a.peer.cmp(&b.peer),
            SortField::State => rank(a.state).cmp(&rank(b.state)),
            SortField::BytesUp => a.bytes_up.cmp(&b.bytes_up),
            SortField::BytesDown => a.bytes_down.cmp(&b.bytes_down),
        };
        let base = if self.sort_dir == SortDir::Desc {
            base.reverse()
        } else {
            base
        };
        // Deterministic tie-break by ConnId (pair then instance), never reversed.
        base.then_with(|| a.id.pair.cmp(&b.id.pair))
            .then_with(|| a.id.instance.cmp(&b.id.instance))
    }
}

/// Direction of a one-row selection move.
#[derive(Debug, Clone, Copy)]
enum Step {
    Up,
    Down,
}

/// The default direction a field sorts in when first selected.
fn natural_dir(field: SortField) -> SortDir {
    match field {
        SortField::Peer | SortField::State => SortDir::Asc,
        SortField::BytesUp | SortField::BytesDown => SortDir::Desc,
    }
}

/// True if `needle` is a subsequence of `haystack` (chars appear in order).
/// Both are expected already-lowercased by the caller. Empty needle matches.
fn is_subsequence(needle: &str, haystack: &str) -> bool {
    let mut hay = haystack.chars();
    'next: for nc in needle.chars() {
        for hc in hay.by_ref() {
            if hc == nc {
                continue 'next;
            }
        }
        return false;
    }
    true
}

/// Stable numeric rank for ordering `ConnState` (lifecycle order).
fn rank(s: ConnState) -> u8 {
    match s {
        ConnState::SynSent => 0,
        ConnState::SynReceived => 1,
        ConnState::Established => 2,
        ConnState::FinWait => 3,
        ConnState::Closed => 4,
        ConnState::Reset => 5,
    }
}

impl ConnRow {
    fn from_connection(c: &Connection) -> Self {
        let service = service_name(c.responder.port);
        let search = format!(
            "{} {} {} {:?}",
            c.origin,
            c.responder,
            service.unwrap_or(""),
            c.state,
        )
        .to_lowercase();
        Self {
            id: c.id,
            peer: c.responder,
            service,
            state: c.state,
            origin_inferred: c.origin_inferred,
            bytes_up: c.bytes_o2r,
            bytes_down: c.bytes_r2o,
            search,
        }
    }
}

#[cfg(test)]
mod tests {
    // Test helpers take `Vec<Connection>` by value for call-site ergonomics.
    #![allow(clippy::unwrap_used, clippy::needless_pass_by_value)]
    use super::*;
    use core::net::{IpAddr, Ipv4Addr};
    use tcpvisr_core::{Endpoint, Nanos};
    use tcpvisr_engine::EndpointPair;

    fn ep(a: u8, port: u16) -> Endpoint {
        Endpoint {
            ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, a)),
            port,
        }
    }

    /// Builds a Connection with the given endpoints/bytes/instance for tests.
    fn conn(origin: Endpoint, responder: Endpoint, up: u64, down: u64, inst: u32) -> Connection {
        Connection {
            id: ConnId {
                pair: EndpointPair::new(origin, responder),
                instance: inst,
            },
            state: ConnState::Established,
            origin,
            responder,
            origin_inferred: false,
            opened_at: Nanos(0),
            last_at: Nanos(1),
            bytes_o2r: up,
            bytes_r2o: down,
            segments: 1,
        }
    }

    fn app_of(conns: Vec<Connection>) -> App {
        App::new(&conns, "t".to_string())
    }

    #[test]
    fn new_builds_one_row_per_connection_with_service_label() {
        let c = conn(ep(1, 51324), ep(2, 443), 10, 20, 0);
        let app = app_of(vec![c]);
        let rows = app.visible();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].peer, ep(2, 443));
        assert_eq!(rows[0].service, Some("https"));
        assert_eq!(rows[0].bytes_up, 10);
        assert_eq!(rows[0].bytes_down, 20);
    }

    #[test]
    fn unknown_responder_port_has_no_service() {
        let app = app_of(vec![conn(ep(1, 40000), ep(2, 40001), 0, 0, 0)]);
        assert_eq!(app.visible()[0].service, None);
    }

    #[test]
    fn initial_selection_is_first_visible_row() {
        let c1 = conn(ep(1, 1), ep(2, 22), 0, 0, 0); // peer 10.0.0.2:22
        let c2 = conn(ep(1, 2), ep(3, 22), 0, 0, 0); // peer 10.0.0.3:22
        let app = app_of(vec![c2, c1]);
        // Peer-ascending → 10.0.0.2:22 first.
        assert_eq!(app.selected(), Some(c1.id));
        assert_eq!(app.visible()[0].id, c1.id);
    }

    #[test]
    fn empty_capture_has_no_rows_or_selection() {
        let app = app_of(vec![]);
        assert!(app.visible().is_empty());
        assert_eq!(app.selected(), None);
    }

    #[test]
    fn cycle_sort_visits_fields_and_resets_natural_direction() {
        let mut app = app_of(vec![conn(ep(1, 1), ep(2, 80), 0, 0, 0)]);
        assert_eq!(
            (app.sort_field(), app.sort_dir()),
            (SortField::Peer, SortDir::Asc)
        );
        app.cycle_sort();
        assert_eq!(
            (app.sort_field(), app.sort_dir()),
            (SortField::State, SortDir::Asc)
        );
        app.cycle_sort();
        assert_eq!(
            (app.sort_field(), app.sort_dir()),
            (SortField::BytesUp, SortDir::Desc)
        );
        app.cycle_sort();
        assert_eq!(
            (app.sort_field(), app.sort_dir()),
            (SortField::BytesDown, SortDir::Desc)
        );
        app.cycle_sort();
        assert_eq!(
            (app.sort_field(), app.sort_dir()),
            (SortField::Peer, SortDir::Asc)
        );
    }

    #[test]
    fn cycle_resets_direction_even_after_toggle() {
        let mut app = app_of(vec![conn(ep(1, 1), ep(2, 80), 0, 0, 0)]);
        app.toggle_dir(); // Peer now Desc
        assert_eq!(app.sort_dir(), SortDir::Desc);
        app.cycle_sort(); // → State, reset to natural Asc
        assert_eq!(
            (app.sort_field(), app.sort_dir()),
            (SortField::State, SortDir::Asc)
        );
    }

    #[test]
    fn bytes_up_sorts_descending_by_default() {
        let small = conn(ep(1, 1), ep(2, 80), 5, 0, 0);
        let big = conn(ep(1, 2), ep(3, 80), 500, 0, 0);
        let mut app = app_of(vec![small, big]);
        app.cycle_sort(); // State
        app.cycle_sort(); // BytesUp, Desc
        let order: Vec<_> = app.visible().iter().map(|r| r.bytes_up).collect();
        assert_eq!(order, vec![500, 5]);
    }

    #[test]
    fn toggle_dir_reverses_without_changing_field() {
        let a = conn(ep(1, 1), ep(2, 80), 0, 0, 0); // peer 10.0.0.2:80
        let b = conn(ep(1, 2), ep(3, 80), 0, 0, 0); // peer 10.0.0.3:80
        let mut app = app_of(vec![a, b]);
        assert_eq!(app.visible()[0].id, a.id); // Asc
        app.toggle_dir();
        assert_eq!(app.sort_field(), SortField::Peer);
        assert_eq!(app.visible()[0].id, b.id); // Desc
    }

    // db: responder :5432 → service "postgresql", peer 10.0.0.2 (sorts first)
    // web: responder :443 → service "https", peer 10.0.0.4
    fn db_conn() -> Connection {
        conn(ep(1, 1111), ep(2, 5432), 0, 0, 0)
    }
    fn web_conn() -> Connection {
        conn(ep(3, 2222), ep(4, 443), 0, 0, 0)
    }

    #[test]
    fn filter_narrows_to_one_then_clears() {
        let mut app = app_of(vec![db_conn(), web_conn()]);
        app.enter_filter();
        assert_eq!(app.mode(), Mode::Filter);
        for c in "https".chars() {
            app.push_filter(c);
        }
        // "https" has no 'h' in the db row → web only.
        let ids: Vec<_> = app.visible().iter().map(|r| r.id).collect();
        assert_eq!(ids, vec![web_conn().id]);
        app.pop_filter(); // "http" still matches only the web row
        assert_eq!(app.visible().len(), 1);
        app.cancel_filter();
        assert_eq!(app.mode(), Mode::Nav);
        assert_eq!(app.query(), "");
        assert_eq!(app.visible().len(), 2);
    }

    #[test]
    fn subsequence_matches_non_adjacent_chars() {
        let mut app = app_of(vec![web_conn()]); // search contains "https"
        app.enter_filter();
        for c in "hts".chars() {
            // h,t,s appear in order within "https" though not adjacently.
            app.push_filter(c);
        }
        assert_eq!(app.visible().len(), 1);
    }

    #[test]
    fn confirm_keeps_filter_esc_clears_it() {
        let mut app = app_of(vec![db_conn(), web_conn()]);
        app.enter_filter();
        for c in "postgres".chars() {
            app.push_filter(c);
        }
        app.confirm_filter();
        assert_eq!(app.mode(), Mode::Nav);
        assert_eq!(app.query(), "postgres");
        // The web row has no 'o' after its 'p', so "postgres" → the db row only.
        assert_eq!(app.visible().len(), 1);
    }

    #[test]
    fn selection_falls_back_to_first_visible_when_filtered_out() {
        let mut app = app_of(vec![db_conn(), web_conn()]);
        // db is first (peer 10.0.0.2:5432 < 10.0.0.4:443) → initially selected.
        assert_eq!(app.selected(), Some(db_conn().id));
        app.enter_filter();
        for c in "https".chars() {
            app.push_filter(c);
        }
        // db filtered out → selection falls back to the only visible row (web).
        assert_eq!(app.selected(), Some(web_conn().id));
    }

    #[test]
    fn selection_becomes_none_when_nothing_matches() {
        let mut app = app_of(vec![db_conn()]);
        app.enter_filter();
        for c in "zzz".chars() {
            app.push_filter(c);
        }
        assert!(app.visible().is_empty());
        assert_eq!(app.selected(), None);
    }

    #[test]
    fn move_down_and_up_clamp_at_ends() {
        let a = conn(ep(1, 1), ep(2, 22), 0, 0, 0);
        let b = conn(ep(1, 2), ep(3, 22), 0, 0, 0);
        let c = conn(ep(1, 3), ep(4, 22), 0, 0, 0);
        let mut app = app_of(vec![a, b, c]);
        assert_eq!(app.selected(), Some(a.id)); // first
        app.move_up(); // clamp at top
        assert_eq!(app.selected(), Some(a.id));
        app.move_down();
        assert_eq!(app.selected(), Some(b.id));
        app.move_down();
        assert_eq!(app.selected(), Some(c.id));
        app.move_down(); // clamp at bottom
        assert_eq!(app.selected(), Some(c.id));
        app.move_up();
        assert_eq!(app.selected(), Some(b.id));
    }

    #[test]
    fn resort_keeps_same_conn_selected() {
        let a = conn(ep(1, 1), ep(2, 22), 10, 0, 0); // peer 10.0.0.2:22, up 10
        let b = conn(ep(1, 2), ep(3, 22), 20, 0, 0); // peer 10.0.0.3:22, up 20
        let mut app = app_of(vec![a, b]);
        app.move_down(); // select b
        assert_eq!(app.selected(), Some(b.id));
        app.cycle_sort(); // State
        app.cycle_sort(); // BytesUp Desc → order [b(20), a(10)]
        assert_eq!(app.selected(), Some(b.id)); // still b
        assert_eq!(app.visible()[0].id, b.id);
    }

    #[test]
    fn move_is_noop_when_empty() {
        let mut app = app_of(vec![]);
        app.move_down();
        app.move_up();
        assert_eq!(app.selected(), None);
    }
}
