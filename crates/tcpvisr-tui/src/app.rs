//! Master-list state resolved as of cursor time `T` (spec §3.5; ADR-0009, ADR-0010). Pure: the
//! row set, each row's state/bytes, sort, filter, and selection are all a function of the
//! `Timeline` and the `Transport` cursor. No I/O, no clock.

use std::collections::HashMap;

use tcpvisr_core::{Endpoint, Nanos, SampleDir};
use tcpvisr_engine::{
    AsOf, ConnId, ConnState, InFlightSample, RttSample, SeqSample, ThroughputSample, Timeline,
};

use crate::service::service_name;
use crate::transport::Transport;

/// The selected connection projected for the detail pane: its endpoints, X span, focus
/// direction (higher-byte), and its `SeqSample` series (borrowed from the `Timeline`).
#[derive(Debug)]
pub struct FocusConn<'a> {
    pub origin: Endpoint,
    pub responder: Endpoint,
    pub x_span: (Nanos, Nanos),
    pub focus_dir: SampleDir,
    pub series: &'a [SeqSample],
    pub inflight: &'a [InFlightSample],
    pub rtt: &'a [RttSample],
    pub throughput: &'a [ThroughputSample],
}

/// Which detail graph the pane shows when open (`Tab` cycles it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailView {
    TimeSequence,
    InFlight,
    Rtt,
}

/// One master-list row: a connection projected as of the cursor time.
#[derive(Debug, Clone)]
pub struct ConnRow {
    pub id: ConnId,
    pub peer: Endpoint,
    pub service: Option<&'static str>,
    pub state: ConnState,
    pub origin_inferred: bool,
    pub bytes_up: u64,
    pub bytes_down: u64,
}

/// Time-invariant projection of a connection: everything a row needs that does not depend on
/// the cursor, plus the lowercased search prefix (origin/responder/service). The state portion
/// of the searchable text varies with `T` and is appended per frame.
#[derive(Debug)]
struct ConnMeta {
    peer: Endpoint,
    service: Option<&'static str>,
    origin_inferred: bool,
    search_prefix: String,
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

/// Pure interactive state for the timeline master list. No I/O, no clock.
#[derive(Debug)]
pub struct App {
    timeline: Timeline,
    transport: Transport,
    metas: HashMap<ConnId, ConnMeta>,
    sort_field: SortField,
    sort_dir: SortDir,
    mode: Mode,
    query: String,
    selected: Option<ConnId>,
    title: String,
    detail_open: bool,
    detail_view: DetailView,
}

impl App {
    /// Builds the app from a capture's [`Timeline`] and a header title string.
    #[must_use]
    pub fn new(timeline: Timeline, title: String) -> Self {
        let metas: HashMap<ConnId, ConnMeta> = timeline
            .connections()
            .map(|c| {
                let service = service_name(c.responder.port);
                let search_prefix =
                    format!("{} {} {}", c.origin, c.responder, service.unwrap_or(""))
                        .to_lowercase();
                (
                    c.id,
                    ConnMeta {
                        peer: c.responder,
                        service,
                        origin_inferred: c.origin_inferred,
                        search_prefix,
                    },
                )
            })
            .collect();
        let (start, end) = timeline.bounds();
        let mut app = Self {
            timeline,
            transport: Transport::new(start, end),
            metas,
            sort_field: SortField::Peer,
            sort_dir: SortDir::Asc,
            mode: Mode::Nav,
            query: String::new(),
            selected: None,
            title,
            detail_open: false,
            detail_view: DetailView::TimeSequence,
        };
        app.selected = app.visible().first().map(|r| r.id);
        app
    }

    /// The filtered + sorted rows active at the cursor, in display order.
    #[must_use]
    pub fn visible(&self) -> Vec<ConnRow> {
        let t = self.transport.cursor();
        let q = self.query.to_lowercase();
        let mut rows: Vec<ConnRow> = self
            .timeline
            .resolve_at(t)
            .into_iter()
            .filter_map(|a| self.row(a, &q))
            .collect();
        rows.sort_by(|x, y| self.order(x, y));
        rows
    }

    /// Projects one resolved connection into a row, or `None` if it fails the filter.
    fn row(&self, a: AsOf, q: &str) -> Option<ConnRow> {
        let m = self.metas.get(&a.id)?;
        let search = format!(
            "{} {}",
            m.search_prefix,
            format!("{:?}", a.state).to_lowercase()
        );
        if !is_subsequence(q, &search) {
            return None;
        }
        Some(ConnRow {
            id: a.id,
            peer: m.peer,
            service: m.service,
            state: a.state,
            origin_inferred: m.origin_inferred,
            bytes_up: a.bytes_o2r,
            bytes_down: a.bytes_r2o,
        })
    }

    /// Opens the detail pane for the selected row (no-op when nothing is selected).
    pub fn open_detail(&mut self) {
        if self.selected.is_some() {
            self.detail_open = true;
        }
    }

    /// Closes the detail pane.
    pub fn close_detail(&mut self) {
        self.detail_open = false;
    }

    /// Whether the detail pane is open.
    #[must_use]
    pub fn is_detail_open(&self) -> bool {
        self.detail_open
    }

    /// The selected connection projected for the detail pane, or `None` if nothing is selected
    /// or the connection is unknown. The focus direction is the higher-byte direction (tie ->
    /// O2R).
    #[must_use]
    pub fn focus(&self) -> Option<FocusConn<'_>> {
        let id = self.selected?;
        let c = self.timeline.connections().find(|c| c.id == id)?;
        let x_span = self.timeline.x_span(id)?;
        let focus_dir = if c.bytes_o2r >= c.bytes_r2o {
            SampleDir::OriginToResponder
        } else {
            SampleDir::ResponderToOrigin
        };
        Some(FocusConn {
            origin: c.origin,
            responder: c.responder,
            x_span,
            focus_dir,
            series: self.timeline.seq_series(id),
            inflight: self.timeline.inflight_series(id),
            rtt: self.timeline.rtt_series(id),
            throughput: self.timeline.throughput_series(id),
        })
    }

    /// The detail graph shown when the pane is open.
    #[must_use]
    pub fn detail_view(&self) -> DetailView {
        self.detail_view
    }

    /// Advances the detail view (wrapping): Time/Sequence -> In-flight -> RTT -> Time/Sequence.
    pub fn cycle_detail_view(&mut self) {
        self.detail_view = match self.detail_view {
            DetailView::TimeSequence => DetailView::InFlight,
            DetailView::InFlight => DetailView::Rtt,
            DetailView::Rtt => DetailView::TimeSequence,
        };
    }

    /// Toggles play/pause; reconciles the selection because rewinding can change the row set.
    pub fn toggle_play(&mut self) {
        self.transport.toggle_play();
        self.reconcile_selection();
    }

    /// Seeks the cursor (forward/back) by a fixed step and reconciles the selection.
    pub fn seek(&mut self, forward: bool) {
        self.transport.seek(forward);
        self.reconcile_selection();
    }

    /// Steps up the speed ladder (no cursor move).
    pub fn faster(&mut self) {
        self.transport.faster();
    }

    /// Steps down the speed ladder (no cursor move).
    pub fn slower(&mut self) {
        self.transport.slower();
    }

    /// Moves the cursor to the next event time (if any) and reconciles the selection.
    pub fn step_forward(&mut self) {
        if let Some(t) = self.timeline.next_event(self.transport.cursor()) {
            self.transport.set_cursor(t);
            self.reconcile_selection();
        }
    }

    /// Moves the cursor to the previous event time (if any) and reconciles the selection.
    pub fn step_back(&mut self) {
        if let Some(t) = self.timeline.prev_event(self.transport.cursor()) {
            self.transport.set_cursor(t);
            self.reconcile_selection();
        }
    }

    /// Advances playback by a wall-clock delta; reconciles the selection if the cursor moved.
    pub fn tick(&mut self, dt: Nanos) {
        let before = self.transport.cursor();
        self.transport.tick(dt);
        if self.transport.cursor() != before {
            self.reconcile_selection();
        }
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

    /// Ensures the selected id is still visible; otherwise selects the first visible row (or
    /// `None` if nothing is visible).
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

    /// The current cursor time.
    #[must_use]
    pub fn cursor(&self) -> Nanos {
        self.transport.cursor()
    }

    /// The current playback speed multiplier.
    #[must_use]
    pub fn speed(&self) -> f64 {
        self.transport.speed()
    }

    /// Whether playback is running.
    #[must_use]
    pub fn is_playing(&self) -> bool {
        self.transport.is_playing()
    }

    /// The capture's `[start, end]` time bounds.
    #[must_use]
    pub fn bounds(&self) -> (Nanos, Nanos) {
        self.transport.bounds()
    }

    /// Whether the capture has no connections at all (distinct from none active at `T`).
    #[must_use]
    pub fn is_capture_empty(&self) -> bool {
        self.metas.is_empty()
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use core::net::{IpAddr, Ipv4Addr};
    use tcpvisr_engine::{Connection, EndpointPair, StateSample};

    fn ep(a: u8, port: u16) -> Endpoint {
        Endpoint {
            ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, a)),
            port,
        }
    }

    fn ss(t: u64, state: ConnState, up: u64, down: u64) -> StateSample {
        StateSample {
            t: Nanos(t),
            state,
            bytes_o2r: up,
            bytes_r2o: down,
        }
    }

    fn full_conn(
        origin: Endpoint,
        responder: Endpoint,
        inst: u32,
        opened: u64,
        last: u64,
        state: ConnState,
    ) -> Connection {
        Connection {
            id: ConnId {
                pair: EndpointPair::new(origin, responder),
                instance: inst,
            },
            state,
            origin,
            responder,
            origin_inferred: false,
            opened_at: Nanos(opened),
            last_at: Nanos(last),
            bytes_o2r: 0,
            bytes_r2o: 0,
            segments: 1,
        }
    }

    /// A connection open on `[0, 1]` (Established) with a single sample at `t=0` carrying the
    /// given up/down bytes — so every such connection is active at the initial cursor (0).
    fn entry(
        origin: Endpoint,
        responder: Endpoint,
        up: u64,
        down: u64,
        inst: u32,
    ) -> (Connection, Vec<StateSample>) {
        let c = full_conn(origin, responder, inst, 0, 1, ConnState::Established);
        (c, vec![ss(0, ConnState::Established, up, down)])
    }

    fn app_of(entries: Vec<(Connection, Vec<StateSample>)>) -> App {
        App::new(Timeline::new(entries), "t".to_string())
    }

    fn id_of(origin: Endpoint, responder: Endpoint, inst: u32) -> ConnId {
        ConnId {
            pair: EndpointPair::new(origin, responder),
            instance: inst,
        }
    }

    #[test]
    fn new_builds_one_row_per_connection_with_service_label() {
        let app = app_of(vec![entry(ep(1, 51324), ep(2, 443), 10, 20, 0)]);
        let rows = app.visible();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].peer, ep(2, 443));
        assert_eq!(rows[0].service, Some("https"));
        assert_eq!(rows[0].bytes_up, 10);
        assert_eq!(rows[0].bytes_down, 20);
    }

    #[test]
    fn unknown_responder_port_has_no_service() {
        let app = app_of(vec![entry(ep(1, 40000), ep(2, 40001), 0, 0, 0)]);
        assert_eq!(app.visible()[0].service, None);
    }

    #[test]
    fn initial_selection_is_first_visible_row() {
        let c1 = entry(ep(1, 1), ep(2, 22), 0, 0, 0); // peer 10.0.0.2:22
        let c2 = entry(ep(1, 2), ep(3, 22), 0, 0, 0); // peer 10.0.0.3:22
        let app = app_of(vec![c2, c1]);
        // Peer-ascending -> 10.0.0.2:22 first.
        assert_eq!(app.selected(), Some(id_of(ep(1, 1), ep(2, 22), 0)));
        assert_eq!(app.visible()[0].id, id_of(ep(1, 1), ep(2, 22), 0));
    }

    #[test]
    fn empty_capture_has_no_rows_or_selection() {
        let app = app_of(vec![]);
        assert!(app.visible().is_empty());
        assert_eq!(app.selected(), None);
        assert!(app.is_capture_empty());
    }

    #[test]
    fn cycle_sort_visits_fields_and_resets_natural_direction() {
        let mut app = app_of(vec![entry(ep(1, 1), ep(2, 80), 0, 0, 0)]);
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
        let mut app = app_of(vec![entry(ep(1, 1), ep(2, 80), 0, 0, 0)]);
        app.toggle_dir(); // Peer now Desc
        assert_eq!(app.sort_dir(), SortDir::Desc);
        app.cycle_sort(); // -> State, reset to natural Asc
        assert_eq!(
            (app.sort_field(), app.sort_dir()),
            (SortField::State, SortDir::Asc)
        );
    }

    #[test]
    fn bytes_up_sorts_descending_by_default() {
        let small = entry(ep(1, 1), ep(2, 80), 5, 0, 0);
        let big = entry(ep(1, 2), ep(3, 80), 500, 0, 0);
        let mut app = app_of(vec![small, big]);
        app.cycle_sort(); // State
        app.cycle_sort(); // BytesUp, Desc
        let order: Vec<_> = app.visible().iter().map(|r| r.bytes_up).collect();
        assert_eq!(order, vec![500, 5]);
    }

    #[test]
    fn toggle_dir_reverses_without_changing_field() {
        let a = entry(ep(1, 1), ep(2, 80), 0, 0, 0); // peer 10.0.0.2:80
        let b = entry(ep(1, 2), ep(3, 80), 0, 0, 0); // peer 10.0.0.3:80
        let mut app = app_of(vec![a, b]);
        assert_eq!(app.visible()[0].id, id_of(ep(1, 1), ep(2, 80), 0)); // Asc
        app.toggle_dir();
        assert_eq!(app.sort_field(), SortField::Peer);
        assert_eq!(app.visible()[0].id, id_of(ep(1, 2), ep(3, 80), 0)); // Desc
    }

    // db: responder :5432 -> service "postgresql", peer 10.0.0.2 (sorts first)
    // web: responder :443 -> service "https", peer 10.0.0.4
    fn db_entry() -> (Connection, Vec<StateSample>) {
        entry(ep(1, 1111), ep(2, 5432), 0, 0, 0)
    }
    fn web_entry() -> (Connection, Vec<StateSample>) {
        entry(ep(3, 2222), ep(4, 443), 0, 0, 0)
    }
    fn db_id() -> ConnId {
        id_of(ep(1, 1111), ep(2, 5432), 0)
    }
    fn web_id() -> ConnId {
        id_of(ep(3, 2222), ep(4, 443), 0)
    }

    #[test]
    fn filter_narrows_to_one_then_clears() {
        let mut app = app_of(vec![db_entry(), web_entry()]);
        app.enter_filter();
        assert_eq!(app.mode(), Mode::Filter);
        for c in "https".chars() {
            app.push_filter(c);
        }
        // "https" has no 'h' in the db row -> web only.
        let ids: Vec<_> = app.visible().iter().map(|r| r.id).collect();
        assert_eq!(ids, vec![web_id()]);
        app.pop_filter(); // "http" still matches only the web row
        assert_eq!(app.visible().len(), 1);
        app.cancel_filter();
        assert_eq!(app.mode(), Mode::Nav);
        assert_eq!(app.query(), "");
        assert_eq!(app.visible().len(), 2);
    }

    #[test]
    fn subsequence_matches_non_adjacent_chars() {
        let mut app = app_of(vec![web_entry()]); // search contains "https"
        app.enter_filter();
        for c in "hts".chars() {
            app.push_filter(c);
        }
        assert_eq!(app.visible().len(), 1);
    }

    #[test]
    fn confirm_keeps_filter_esc_clears_it() {
        let mut app = app_of(vec![db_entry(), web_entry()]);
        app.enter_filter();
        for c in "postgres".chars() {
            app.push_filter(c);
        }
        app.confirm_filter();
        assert_eq!(app.mode(), Mode::Nav);
        assert_eq!(app.query(), "postgres");
        // The web row has no 'o' after its 'p', so "postgres" -> the db row only.
        assert_eq!(app.visible().len(), 1);
    }

    #[test]
    fn selection_falls_back_to_first_visible_when_filtered_out() {
        let mut app = app_of(vec![db_entry(), web_entry()]);
        assert_eq!(app.selected(), Some(db_id()));
        app.enter_filter();
        for c in "https".chars() {
            app.push_filter(c);
        }
        assert_eq!(app.selected(), Some(web_id()));
    }

    #[test]
    fn selection_becomes_none_when_nothing_matches() {
        let mut app = app_of(vec![db_entry()]);
        app.enter_filter();
        for c in "zzz".chars() {
            app.push_filter(c);
        }
        assert!(app.visible().is_empty());
        assert_eq!(app.selected(), None);
    }

    #[test]
    fn move_down_and_up_clamp_at_ends() {
        let a = entry(ep(1, 1), ep(2, 22), 0, 0, 0);
        let b = entry(ep(1, 2), ep(3, 22), 0, 0, 0);
        let c = entry(ep(1, 3), ep(4, 22), 0, 0, 0);
        let mut app = app_of(vec![a, b, c]);
        assert_eq!(app.selected(), Some(id_of(ep(1, 1), ep(2, 22), 0))); // first
        app.move_up(); // clamp at top
        assert_eq!(app.selected(), Some(id_of(ep(1, 1), ep(2, 22), 0)));
        app.move_down();
        assert_eq!(app.selected(), Some(id_of(ep(1, 2), ep(3, 22), 0)));
        app.move_down();
        assert_eq!(app.selected(), Some(id_of(ep(1, 3), ep(4, 22), 0)));
        app.move_down(); // clamp at bottom
        assert_eq!(app.selected(), Some(id_of(ep(1, 3), ep(4, 22), 0)));
        app.move_up();
        assert_eq!(app.selected(), Some(id_of(ep(1, 2), ep(3, 22), 0)));
    }

    #[test]
    fn resort_keeps_same_conn_selected() {
        let a = entry(ep(1, 1), ep(2, 22), 10, 0, 0); // peer 10.0.0.2:22, up 10
        let b = entry(ep(1, 2), ep(3, 22), 20, 0, 0); // peer 10.0.0.3:22, up 20
        let mut app = app_of(vec![a, b]);
        app.move_down(); // select b
        assert_eq!(app.selected(), Some(id_of(ep(1, 2), ep(3, 22), 0)));
        app.cycle_sort(); // State
        app.cycle_sort(); // BytesUp Desc -> order [b(20), a(10)]
        assert_eq!(app.selected(), Some(id_of(ep(1, 2), ep(3, 22), 0))); // still b
        assert_eq!(app.visible()[0].id, id_of(ep(1, 2), ep(3, 22), 0));
    }

    #[test]
    fn move_is_noop_when_empty() {
        let mut app = app_of(vec![]);
        app.move_down();
        app.move_up();
        assert_eq!(app.selected(), None);
    }

    #[test]
    fn master_list_resolves_active_set_and_bytes_as_of_t() {
        // early opens at 0 with bytes 10, growing to 50 at t=150; late opens at 100.
        let early = full_conn(ep(1, 1), ep(2, 80), 0, 0, 200, ConnState::Established);
        let early_s = vec![
            ss(0, ConnState::Established, 10, 0),
            ss(150, ConnState::Established, 50, 0),
        ];
        let late = full_conn(ep(1, 2), ep(3, 80), 0, 100, 200, ConnState::Established);
        let late_s = vec![ss(100, ConnState::Established, 5, 0)];
        let mut app = App::new(
            Timeline::new(vec![(early, early_s), (late, late_s)]),
            "t".to_string(),
        );
        let early_id = id_of(ep(1, 1), ep(2, 80), 0);
        // At the initial cursor (start = 0) only the early connection is active.
        let v0 = app.visible();
        assert_eq!(v0.len(), 1);
        assert_eq!(v0[0].id, early_id);
        assert_eq!(v0[0].bytes_up, 10);
        // Step to 100: both connections are active.
        app.step_forward();
        assert_eq!(app.cursor(), Nanos(100));
        assert_eq!(app.visible().len(), 2);
        // Step to 150: the early row's bytes advance to its later sample.
        app.step_forward();
        assert_eq!(app.cursor(), Nanos(150));
        let early_row = app
            .visible()
            .into_iter()
            .find(|r| r.id == early_id)
            .unwrap();
        assert_eq!(early_row.bytes_up, 50);
    }

    #[test]
    fn enter_opens_only_with_a_selection_esc_closes() {
        let mut app = app_of(vec![entry(ep(1, 51324), ep(2, 443), 10, 20, 0)]);
        assert!(!app.is_detail_open());
        app.open_detail();
        assert!(app.is_detail_open(), "opens when a row is selected");
        app.close_detail();
        assert!(!app.is_detail_open());

        let mut empty = app_of(vec![]);
        empty.open_detail();
        assert!(!empty.is_detail_open(), "no selection -> stays closed");
    }

    #[test]
    fn focus_resolves_selected_connection_and_higher_byte_direction() {
        // O2R 5 bytes, R2O 500 bytes -> focus direction is R2O.
        let mut down = full_conn(ep(1, 1), ep(2, 443), 0, 0, 10, ConnState::Established);
        down.bytes_o2r = 5;
        down.bytes_r2o = 500;
        let samples = vec![ss(0, ConnState::Established, 5, 500)];
        let app = App::new(Timeline::new(vec![(down, samples)]), "t".to_string());
        let f = app.focus().expect("selected connection resolves");
        assert_eq!(f.origin, ep(1, 1));
        assert_eq!(f.responder, ep(2, 443));
        assert_eq!(f.x_span, (Nanos(0), Nanos(10)));
        assert_eq!(f.focus_dir, SampleDir::ResponderToOrigin);
    }

    #[test]
    fn tab_cycles_detail_view() {
        let mut app = app_of(vec![entry(ep(1, 1), ep(2, 22), 0, 0, 0)]);
        assert_eq!(app.detail_view(), DetailView::TimeSequence);
        app.cycle_detail_view();
        assert_eq!(app.detail_view(), DetailView::InFlight);
        app.cycle_detail_view();
        assert_eq!(app.detail_view(), DetailView::Rtt);
        app.cycle_detail_view();
        assert_eq!(app.detail_view(), DetailView::TimeSequence);
    }

    #[test]
    fn focus_exposes_rtt_series() {
        use tcpvisr_core::SampleDir;
        use tcpvisr_engine::RttSample;
        let c = full_conn(ep(1, 1), ep(2, 443), 0, 0, 10, ConnState::Established);
        let mut c2 = c;
        c2.bytes_o2r = 100;
        let rtt = vec![RttSample {
            t: Nanos(0),
            dir: SampleDir::OriginToResponder,
            rtt: Nanos(500),
            srtt: Nanos(500),
        }];
        let tl = Timeline::with_seq(vec![(
            c2,
            vec![ss(0, ConnState::Established, 100, 0)],
            vec![],
            vec![],
            rtt,
            vec![],
        )]);
        let app = App::new(tl, "t".to_string());
        let f = app.focus().expect("selected");
        assert_eq!(f.rtt.len(), 1);
        assert_eq!(f.rtt[0].rtt, Nanos(500));
    }

    #[test]
    fn focus_exposes_throughput_series() {
        use tcpvisr_core::SampleDir;
        use tcpvisr_engine::ThroughputSample;
        let c = full_conn(ep(1, 1), ep(2, 443), 0, 0, 10, ConnState::Established);
        let mut c2 = c;
        c2.bytes_o2r = 100;
        let throughput = vec![ThroughputSample {
            t: Nanos(0),
            dir: SampleDir::OriginToResponder,
            throughput_bps: 800,
            goodput_bps: 400,
        }];
        let tl = Timeline::with_seq(vec![(
            c2,
            vec![ss(0, ConnState::Established, 100, 0)],
            vec![],
            vec![],
            vec![],
            throughput,
        )]);
        let app = App::new(tl, "t".to_string());
        let f = app.focus().expect("selected");
        assert_eq!(f.throughput.len(), 1);
        assert_eq!(f.throughput[0].throughput_bps, 800);
        assert_eq!(f.throughput[0].goodput_bps, 400);
    }

    #[test]
    fn focus_exposes_inflight_series() {
        use tcpvisr_core::SampleDir;
        use tcpvisr_engine::InFlightSample;
        let c = full_conn(ep(1, 1), ep(2, 443), 0, 0, 10, ConnState::Established);
        let mut c2 = c;
        c2.bytes_o2r = 100;
        let inflight = vec![InFlightSample {
            t: Nanos(0),
            dir: SampleDir::OriginToResponder,
            bytes: 100,
        }];
        let tl = Timeline::with_seq(vec![(
            c2,
            vec![ss(0, ConnState::Established, 100, 0)],
            vec![],
            inflight,
            vec![],
            vec![],
        )]);
        let app = App::new(tl, "t".to_string());
        let f = app.focus().expect("selected");
        assert_eq!(f.inflight.len(), 1);
        assert_eq!(f.inflight[0].bytes, 100);
    }

    #[test]
    fn detail_follows_selection() {
        let a = entry(ep(1, 1), ep(2, 22), 0, 0, 0); // peer 10.0.0.2
        let b = entry(ep(1, 2), ep(3, 22), 0, 0, 0); // peer 10.0.0.3
        let mut app = app_of(vec![a, b]);
        app.open_detail();
        let first = app.focus().expect("focus").responder;
        app.move_down();
        let second = app.focus().expect("focus").responder;
        assert_ne!(first, second, "focus follows the moved selection");
        assert_eq!(second, ep(3, 22));
    }

    #[test]
    fn selection_reconciles_across_the_cursor() {
        let early = full_conn(ep(1, 1), ep(2, 80), 0, 0, 200, ConnState::Established);
        let early_s = vec![ss(0, ConnState::Established, 0, 0)];
        let late = full_conn(ep(1, 2), ep(3, 80), 0, 100, 200, ConnState::Established);
        let late_s = vec![ss(100, ConnState::Established, 0, 0)];
        let mut app = App::new(
            Timeline::new(vec![(early, early_s), (late, late_s)]),
            "t".to_string(),
        );
        let early_id = id_of(ep(1, 1), ep(2, 80), 0);
        let late_id = id_of(ep(1, 2), ep(3, 80), 0);
        app.step_forward(); // cursor 100, both active
        app.move_down(); // select the late connection (peer 10.0.0.3 sorts after 10.0.0.2)
        assert_eq!(app.selected(), Some(late_id));
        app.step_back(); // cursor 0: late not active -> fall back to early
        assert_eq!(app.cursor(), Nanos(0));
        assert_eq!(app.selected(), Some(early_id));
    }
}
