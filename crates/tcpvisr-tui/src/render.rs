//! Pure ratatui rendering of the master list (spec §3.2). No terminal I/O.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Text};
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table, TableState};
use tcpvisr_core::Nanos;

use crate::app::{App, Mode, SortDir, SortField};
use crate::detail::{self, Mark, SeqPlot};

/// Columns reserved on the left of the detail pane for Y-axis (sequence) labels.
const GUTTER: u16 = 8;

/// Draws the master list — header, table (or empty state), and footer — into `frame`.
pub fn render(frame: &mut Frame, app: &App) {
    let [main, footer] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(frame.area());
    if app.is_detail_open() {
        let [left, right] =
            Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                .areas(main);
        render_main(frame, app, left);
        render_detail(frame, app, right);
    } else {
        render_main(frame, app, main);
    }
    render_footer(frame, app, footer);
}

fn render_main(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::bordered()
        .title(app.title().to_string())
        .title(Line::from(transport_status(app)).right_aligned());
    let rows = app.visible();
    if rows.is_empty() {
        let msg = if app.is_capture_empty() {
            "no connections in capture".to_string()
        } else {
            format!("no connections active at t={}s", fmt_seconds(app.cursor()))
        };
        let p = Paragraph::new(msg).block(block);
        frame.render_widget(p, area);
        return;
    }

    let header = Row::new([
        Cell::from("PEER"),
        Cell::from("SERVICE"),
        Cell::from("STATE"),
        Cell::from(Text::from("↑BYTES").right_aligned()),
        Cell::from(Text::from("↓BYTES").right_aligned()),
    ])
    .style(Style::default().add_modifier(Modifier::BOLD));

    let selected_idx = app
        .selected()
        .and_then(|id| rows.iter().position(|r| r.id == id));

    let body: Vec<Row> = rows
        .iter()
        .map(|r| {
            let state = if r.origin_inferred {
                format!("{:?}~", r.state)
            } else {
                format!("{:?}", r.state)
            };
            Row::new([
                Cell::from(r.peer.to_string()),
                Cell::from(r.service.unwrap_or("")),
                Cell::from(state),
                Cell::from(Text::from(r.bytes_up.to_string()).right_aligned()),
                Cell::from(Text::from(r.bytes_down.to_string()).right_aligned()),
            ])
        })
        .collect();

    let widths = [
        Constraint::Min(20),
        Constraint::Length(12),
        Constraint::Length(14),
        Constraint::Length(10),
        Constraint::Length(10),
    ];
    let table = Table::new(body, widths)
        .header(header)
        .block(block)
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▸");

    let mut state = TableState::default().with_selected(selected_idx);
    frame.render_stateful_widget(table, area, &mut state);
}

/// Draws the Time/Sequence detail pane for the focused connection into `area`.
fn render_detail(frame: &mut Frame, app: &App, area: Rect) {
    let Some(focus) = app.focus() else {
        let block = Block::bordered().title("DETAIL");
        frame.render_widget(Paragraph::new("no connection selected").block(block), area);
        return;
    };
    let title = format!("DETAIL {} \u{2192} {}", focus.origin, focus.responder);
    let block = Block::bordered().title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Reserve: legend row (top), time-label row (bottom), Y-label gutter (left).
    if inner.height < 3 || inner.width <= GUTTER {
        frame.render_widget(Paragraph::new("widen terminal to view graph"), inner);
        return;
    }
    let plot_w = inner.width - GUTTER;
    let plot_h = inner.height - 2; // legend + time labels

    let Some(plot) = detail::project(
        focus.series,
        focus.focus_dir,
        focus.x_span,
        app.cursor(),
        plot_w,
        plot_h,
    ) else {
        frame.render_widget(Paragraph::new("widen terminal to view graph"), inner);
        return;
    };

    draw_legend(frame, inner);
    draw_plot(frame, inner, GUTTER, &plot);
    draw_axes(frame, inner, GUTTER, &plot);
}

fn draw_legend(frame: &mut Frame, inner: Rect) {
    let legend = format!(
        "Time/Sequence   {} retrans  {} sack",
        detail::RETRANS_GLYPH,
        detail::SACK_GLYPH
    );
    let row = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: 1,
    };
    frame.render_widget(Paragraph::new(legend), row);
}

fn draw_plot(frame: &mut Frame, inner: Rect, gutter: u16, plot: &SeqPlot) {
    let buf = frame.buffer_mut();
    let x0 = inner.x + gutter;
    let y_top = inner.y + 1; // below the legend row
    for &Mark { col, row, glyph } in &plot.marks {
        let screen_row = plot.height - 1 - row; // bottom-origin row -> screen line
        let x = x0 + col;
        let y = y_top + screen_row;
        let color = match glyph {
            detail::RETRANS_GLYPH => Color::Red,
            detail::SACK_GLYPH => Color::Yellow,
            _ => Color::Reset,
        };
        buf.set_string(x, y, glyph.to_string(), Style::default().fg(color));
    }
}

fn draw_axes(frame: &mut Frame, inner: Rect, gutter: u16, plot: &SeqPlot) {
    let buf = frame.buffer_mut();
    let y_top = inner.y + 1;
    // Y labels: max_rel at the top of the plot, 0 at the bottom.
    buf.set_string(
        inner.x,
        y_top,
        format!("{:>7}", plot.max_rel),
        Style::default(),
    );
    let y_bottom = y_top + plot.height - 1;
    buf.set_string(inner.x, y_bottom, format!("{:>7}", 0), Style::default());
    // X labels: start / end seconds on the bottom label row.
    let label_row = inner.y + inner.height - 1;
    let start = fmt_seconds(plot.x_span.0);
    let end = fmt_seconds(plot.x_span.1);
    buf.set_string(
        inner.x + gutter,
        label_row,
        format!("{start}s"),
        Style::default(),
    );
    let end_label = format!("{end}s");
    let end_x = inner
        .x
        .saturating_add(inner.width)
        .saturating_sub(u16::try_from(end_label.len()).unwrap_or(0));
    buf.set_string(end_x, label_row, end_label, Style::default());
}

fn render_footer(frame: &mut Frame, app: &App, area: Rect) {
    let text = match app.mode() {
        Mode::Filter => format!("/{}", app.query()),
        Mode::Nav => {
            let arrow = match app.sort_dir() {
                SortDir::Asc => '▲',
                SortDir::Desc => '▼',
            };
            format!(
                "space play/pause  ←→ seek  +/- speed  ,/. step  ⏎ open  esc close  / filter  s sort:{}{arrow}  q quit",
                sort_label(app.sort_field()),
            )
        }
    };
    frame.render_widget(Paragraph::new(text), area);
}

/// The right-aligned header segment: play state, speed, and `t=cursor / total` in seconds.
fn transport_status(app: &App) -> String {
    let glyph = if app.is_playing() { "▶" } else { "⏸" };
    let (_, end) = app.bounds();
    format!(
        "[ {glyph} {:.1}x  t={}s / {}s ]",
        app.speed(),
        fmt_seconds(app.cursor()),
        fmt_seconds(end),
    )
}

/// Formats a nanosecond timestamp as fixed 3-decimal seconds via integer arithmetic (no locale,
/// no float) so `TestBackend` snapshots stay deterministic.
fn fmt_seconds(t: Nanos) -> String {
    let ms = t.0 / 1_000_000;
    format!("{}.{:03}", ms / 1000, ms % 1000)
}

fn sort_label(field: SortField) -> &'static str {
    match field {
        SortField::Peer => "peer",
        SortField::State => "state",
        SortField::BytesUp => "up",
        SortField::BytesDown => "down",
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::render;
    use crate::app::App;
    use crate::handle_key;
    use core::net::{IpAddr, Ipv4Addr};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use tcpvisr_core::{Endpoint, Nanos};
    use tcpvisr_engine::{ConnId, ConnState, Connection, EndpointPair, StateSample, Timeline};

    fn ep(a: u8, port: u16) -> Endpoint {
        Endpoint {
            ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, a)),
            port,
        }
    }

    fn entry(
        origin: Endpoint,
        responder: Endpoint,
        inferred: bool,
    ) -> (Connection, Vec<StateSample>) {
        let c = Connection {
            id: ConnId {
                pair: EndpointPair::new(origin, responder),
                instance: 0,
            },
            state: ConnState::Established,
            origin,
            responder,
            origin_inferred: inferred,
            opened_at: Nanos(0),
            last_at: Nanos(1),
            bytes_o2r: 10,
            bytes_r2o: 20,
            segments: 1,
        };
        let s = StateSample {
            t: Nanos(0),
            state: ConnState::Established,
            bytes_o2r: 10,
            bytes_r2o: 20,
        };
        (c, vec![s])
    }

    fn app_of(entries: Vec<(Connection, Vec<StateSample>)>, title: &str) -> App {
        App::new(Timeline::new(entries), title.to_string())
    }

    fn ss(t: u64, up: u64, down: u64) -> StateSample {
        StateSample {
            t: Nanos(t),
            state: ConnState::Established,
            bytes_o2r: up,
            bytes_r2o: down,
        }
    }

    fn conn_span(
        origin: Endpoint,
        responder: Endpoint,
        opened: u64,
        last: u64,
        state: ConnState,
    ) -> Connection {
        Connection {
            id: ConnId {
                pair: EndpointPair::new(origin, responder),
                instance: 0,
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

    /// A single connection open on `[0, end_ns]` with one sample at t=0.
    fn app_span(end_ns: u64) -> App {
        let c = conn_span(ep(1, 5), ep(2, 443), 0, end_ns, ConnState::Established);
        App::new(
            Timeline::new(vec![(c, vec![ss(0, 10, 20)])]),
            "t".to_string(),
        )
    }

    /// Two connections with a gap: A closed on `[0,100]`, B open on `[200,300]`. Between them
    /// no connection is active.
    fn gapped_app() -> App {
        let a = conn_span(ep(1, 1), ep(2, 80), 0, 100, ConnState::Closed);
        let a_s = vec![
            ss(0, 0, 0),
            StateSample {
                t: Nanos(100),
                state: ConnState::Closed,
                bytes_o2r: 0,
                bytes_r2o: 0,
            },
        ];
        let b = conn_span(ep(1, 2), ep(3, 80), 200, 300, ConnState::Established);
        let b_s = vec![ss(200, 0, 0), ss(300, 0, 0)];
        App::new(Timeline::new(vec![(a, a_s), (b, b_s)]), "t".to_string())
    }

    #[test]
    fn header_shows_transport_status() {
        let app = app_span(2_000_000_000); // 2.000s total
        let s = draw(&app, 100, 8);
        assert!(s.contains("⏸"), "paused glyph: {s}");
        assert!(s.contains("1.0x"), "speed: {s}");
        assert!(s.contains("t=0.000s / 2.000s"), "cursor readout: {s}");
    }

    #[test]
    fn header_shows_playing_glyph_after_toggle() {
        let mut app = app_span(2_000_000_000);
        app.toggle_play();
        let s = draw(&app, 100, 8);
        assert!(s.contains("▶"), "playing glyph: {s}");
    }

    #[test]
    fn footer_shows_transport_hints() {
        let app = app_span(1_000_000_000);
        let s = draw(&app, 100, 8);
        assert!(s.contains("space"), "play/pause hint: {s}");
        assert!(s.contains("seek"), "seek hint: {s}");
        assert!(s.contains("speed"), "speed hint: {s}");
        assert!(s.contains("q quit"), "quit hint: {s}");
    }

    #[test]
    fn empty_active_set_shows_gap_message() {
        let mut app = gapped_app();
        app.step_forward(); // cursor 100 (A still active at its close)
        app.seek(true); // cursor 106 -> in the gap, nothing active
        assert!(app.visible().is_empty(), "cursor is in the gap");
        let s = draw(&app, 60, 6);
        assert!(s.contains("no connections active"), "{s}");
    }

    fn buffer_string(buf: &Buffer) -> String {
        let area = buf.area;
        let mut out = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                if let Some(cell) = buf.cell((x, y)) {
                    out.push_str(cell.symbol());
                }
            }
            out.push('\n');
        }
        out
    }

    fn draw(app: &App, w: u16, h: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| render(f, app)).unwrap();
        buffer_string(terminal.backend().buffer())
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn renders_header_columns_selection_and_footer() {
        let app = app_of(
            vec![entry(ep(1, 5), ep(2, 443), false)],
            "tcp-visr — c.pcap  (1 connections, skipped 0)",
        );
        // Width 120: the M6 footer gained `⏎ open  esc close` hints (spec §3.3), so `q quit`
        // and the sort indicator at the footer tail need a wider viewport than M5's 80.
        let s = draw(&app, 120, 10);
        assert!(s.contains("tcp-visr — c.pcap"), "{s}");
        assert!(s.contains("PEER"), "{s}");
        assert!(s.contains("SERVICE"), "{s}");
        assert!(s.contains("https"), "{s}");
        assert!(s.contains("▸"), "selection marker: {s}");
        assert!(s.contains("q quit"), "footer: {s}");
        assert!(s.contains("sort:peer▲"), "sort indicator: {s}");
    }

    #[test]
    fn renders_mid_stream_marker() {
        let app = app_of(vec![entry(ep(1, 5), ep(2, 443), true)], "t");
        let s = draw(&app, 80, 6);
        assert!(s.contains("Established~"), "{s}");
    }

    #[test]
    fn filter_mode_shows_query_line() {
        let mut app = app_of(vec![entry(ep(1, 5), ep(2, 443), false)], "t");
        handle_key(&mut app, key(KeyCode::Char('/')));
        handle_key(&mut app, key(KeyCode::Char('h')));
        let s = draw(&app, 80, 6);
        assert!(s.contains("/h"), "{s}");
    }

    #[test]
    fn empty_capture_shows_placeholder() {
        let app = app_of(vec![], "t");
        let s = draw(&app, 40, 6);
        assert!(s.contains("no connections in capture"), "{s}");
    }

    #[test]
    fn detail_closed_still_renders_full_master() {
        let app = app_span(2_000_000_000);
        let s = draw(&app, 100, 10);
        assert!(
            s.contains("PEER"),
            "master header present when detail closed: {s}"
        );
        assert!(!s.contains("DETAIL"), "no detail pane when closed");
    }

    #[test]
    fn detail_open_shows_title_legend_and_a_mark() {
        // A connection with one O2R data segment so the focus series is non-empty.
        let c = conn_span(ep(1, 5), ep(2, 443), 0, 1_000, ConnState::Established);
        let sq = tcpvisr_engine::SeqSample {
            t: Nanos(0),
            dir: tcpvisr_core::SampleDir::OriginToResponder,
            rel: 0,
            len: 100,
            kind: tcpvisr_engine::SeqKind::Data {
                retransmit: false,
                out_of_order: false,
            },
        };
        let mut c2 = c;
        c2.bytes_o2r = 100;
        let tl = Timeline::with_seq(vec![(c2, vec![ss(0, 100, 0)], vec![sq])]);
        let mut app = App::new(tl, "t".to_string());
        app.open_detail();
        let s = draw(&app, 120, 14);
        assert!(s.contains("DETAIL"), "detail title: {s}");
        assert!(
            s.contains("retrans") && s.contains("sack"),
            "mark legend: {s}"
        );
        assert!(s.contains('#'), "at least one plotted data glyph: {s}");
    }

    #[test]
    fn detail_pane_too_narrow_shows_widen_message() {
        let c = conn_span(ep(1, 5), ep(2, 443), 0, 1_000, ConnState::Established);
        let mut c2 = c;
        c2.bytes_o2r = 100;
        let sq = tcpvisr_engine::SeqSample {
            t: Nanos(0),
            dir: tcpvisr_core::SampleDir::OriginToResponder,
            rel: 0,
            len: 100,
            kind: tcpvisr_engine::SeqKind::Data {
                retransmit: false,
                out_of_order: false,
            },
        };
        let tl = Timeline::with_seq(vec![(c2, vec![ss(0, 100, 0)], vec![sq])]);
        let mut app = App::new(tl, "t".to_string());
        app.open_detail();
        // Width 34 -> right pane 17, inner 15, plot_w = 15 - 8 gutter = 7 < MIN_W(8) -> the guard
        // fires, and the 15-wide inner still fits the "widen terminal" message.
        let s = draw(&app, 34, 12);
        assert!(s.contains("widen terminal"), "narrow detail guidance: {s}");
    }

    #[test]
    fn footer_advertises_open_and_close() {
        let app = app_span(1_000_000_000);
        let s = draw(&app, 120, 8);
        assert!(s.contains("open"), "open hint: {s}");
        assert!(s.contains("close"), "close hint: {s}");
    }

    #[test]
    fn selected_row_visible_when_viewport_shorter_than_list() {
        // 5 connections, height only fits ~1 body row; move to the last and
        // assert its peer still renders (scroll-to-selection).
        let entries: Vec<_> = (1..=5)
            .map(|i| entry(ep(1, i), ep(2, 100 + i), false))
            .collect();
        let mut app = app_of(entries, "t");
        for _ in 0..4 {
            handle_key(&mut app, key(KeyCode::Down));
        }
        let s = draw(&app, 60, 5);
        assert!(
            s.contains(":105"),
            "last row must be scrolled into view: {s}"
        );
    }
}
