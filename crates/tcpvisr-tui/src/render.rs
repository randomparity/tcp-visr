//! Pure ratatui rendering of the master list (spec §3.2). No terminal I/O.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Text;
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table, TableState};

use crate::app::{App, Mode, SortDir, SortField};

/// Draws the master list — header, table (or empty state), and footer — into `frame`.
pub fn render(frame: &mut Frame, app: &App) {
    let [main, footer] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(frame.area());
    render_main(frame, app, main);
    render_footer(frame, app, footer);
}

fn render_main(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::bordered().title(app.title().to_string());
    let rows = app.visible();
    if rows.is_empty() {
        let p = Paragraph::new("no connections in capture").block(block);
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

fn render_footer(frame: &mut Frame, app: &App, area: Rect) {
    let text = match app.mode() {
        Mode::Filter => format!("/{}", app.query()),
        Mode::Nav => {
            let arrow = match app.sort_dir() {
                SortDir::Asc => '▲',
                SortDir::Desc => '▼',
            };
            format!(
                "/ filter   s sort:{}{arrow}   S reverse   ↑↓ select   q quit",
                sort_label(app.sort_field()),
            )
        }
    };
    frame.render_widget(Paragraph::new(text), area);
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
    use tcpvisr_engine::{ConnId, ConnState, Connection, EndpointPair};

    fn ep(a: u8, port: u16) -> Endpoint {
        Endpoint {
            ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, a)),
            port,
        }
    }

    fn conn(origin: Endpoint, responder: Endpoint, inferred: bool) -> Connection {
        Connection {
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
        }
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
        let app = App::new(
            &[conn(ep(1, 5), ep(2, 443), false)],
            "tcp-visr — c.pcap  (1 connections, skipped 0)".to_string(),
        );
        let s = draw(&app, 80, 10);
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
        let app = App::new(&[conn(ep(1, 5), ep(2, 443), true)], "t".to_string());
        let s = draw(&app, 80, 6);
        assert!(s.contains("Established~"), "{s}");
    }

    #[test]
    fn filter_mode_shows_query_line() {
        let mut app = App::new(&[conn(ep(1, 5), ep(2, 443), false)], "t".to_string());
        handle_key(&mut app, key(KeyCode::Char('/')));
        handle_key(&mut app, key(KeyCode::Char('h')));
        let s = draw(&app, 80, 6);
        assert!(s.contains("/h"), "{s}");
    }

    #[test]
    fn empty_capture_shows_placeholder() {
        let app = App::new(&[], "t".to_string());
        let s = draw(&app, 40, 6);
        assert!(s.contains("no connections in capture"), "{s}");
    }

    #[test]
    fn selected_row_visible_when_viewport_shorter_than_list() {
        // 5 connections, height only fits ~1 body row; move to the last and
        // assert its peer still renders (scroll-to-selection).
        let conns: Vec<_> = (1..=5)
            .map(|i| conn(ep(1, i), ep(2, 100 + i), false))
            .collect();
        let mut app = App::new(&conns, "t".to_string());
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
