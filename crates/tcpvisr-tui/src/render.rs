//! Pure ratatui rendering of the master list (spec §3.2). No terminal I/O.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Text};
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table, TableState};
use tcpvisr_core::Nanos;

use crate::app::{App, DetailView, FocusConn, Mode, SortDir, SortField};
use crate::detail::{self, Mark, SeqPlot};
use crate::inflight::{self, InFlightPlot, Mark as InFlightMark, Series};
use crate::rtt::{self, Mark as RttMark, RttPlot, Series as RttSeries};
use crate::throughput::{self, Mark as ThroughputMark, Series as ThroughputSeries, ThroughputPlot};

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

/// Draws the detail pane (the view named by `app.detail_view()`) for the focused connection.
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
    match app.detail_view() {
        DetailView::TimeSequence => render_seq_body(frame, app, inner, &focus),
        DetailView::InFlight => render_inflight_body(frame, app, inner, &focus),
        DetailView::Rtt => render_rtt_body(frame, app, inner, &focus),
        DetailView::Throughput => render_throughput_body(frame, app, inner, &focus),
    }
}

/// Draws the Time/Sequence (Stevens) graph into the reserved pane interior (M6).
fn render_seq_body(frame: &mut Frame, app: &App, inner: Rect, focus: &FocusConn<'_>) {
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

/// Draws the In-flight (bytes-outstanding) sawtooth into the reserved pane interior (M7). The
/// cwnd overlay series is empty on replay; M12 fills it (ADR-0012 §4).
fn render_inflight_body(frame: &mut Frame, app: &App, inner: Rect, focus: &FocusConn<'_>) {
    let plot_w = inner.width - GUTTER;
    let plot_h = inner.height - 2; // legend + time labels
    let Some(plot) = inflight::project(
        focus.inflight,
        &[],
        focus.focus_dir,
        focus.x_span,
        app.cursor(),
        plot_w,
        plot_h,
    ) else {
        frame.render_widget(Paragraph::new("widen terminal to view graph"), inner);
        return;
    };
    draw_inflight_legend(frame, inner);
    draw_inflight_plot(frame, inner, GUTTER, &plot);
    draw_inflight_axes(frame, inner, GUTTER, &plot);
}

fn draw_inflight_legend(frame: &mut Frame, inner: Rect) {
    let legend = format!("In-flight   {} wire", inflight::WIRE_GLYPH);
    let row = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: 1,
    };
    frame.render_widget(Paragraph::new(legend), row);
}

fn draw_inflight_plot(frame: &mut Frame, inner: Rect, gutter: u16, plot: &InFlightPlot) {
    let buf = frame.buffer_mut();
    let x0 = inner.x + gutter;
    let y_top = inner.y + 1; // below the legend row
    for &InFlightMark {
        col,
        row,
        glyph,
        series,
    } in &plot.marks
    {
        let screen_row = plot.height - 1 - row; // bottom-origin row -> screen line
        let x = x0 + col;
        let y = y_top + screen_row;
        let color = match series {
            Series::Cwnd => Color::Cyan,
            Series::Wire => Color::Reset,
        };
        buf.set_string(x, y, glyph.to_string(), Style::default().fg(color));
    }
}

fn draw_inflight_axes(frame: &mut Frame, inner: Rect, gutter: u16, plot: &InFlightPlot) {
    let buf = frame.buffer_mut();
    let y_top = inner.y + 1;
    // Y labels: max_bytes at the top, 0 at the bottom. `fmt_seq` keeps a large (multi-GB) value
    // inside the gutter so it never overwrites plot columns.
    let top = i64::try_from(plot.max_bytes).unwrap_or(i64::MAX);
    buf.set_string(
        inner.x,
        y_top,
        format!("{:>7}", fmt_seq(top)),
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

/// Draws the RTT graph (raw per-ack points + smoothed SRTT line) into the reserved pane interior
/// (M8). The kernel-srtt overlay series is empty on replay; M12 fills it (ADR-0013 §4).
fn render_rtt_body(frame: &mut Frame, app: &App, inner: Rect, focus: &FocusConn<'_>) {
    let plot_w = inner.width - GUTTER;
    let plot_h = inner.height - 2; // legend + time labels
    let Some(plot) = rtt::project(
        focus.rtt,
        &[],
        focus.focus_dir,
        focus.x_span,
        app.cursor(),
        plot_w,
        plot_h,
    ) else {
        frame.render_widget(Paragraph::new("widen terminal to view graph"), inner);
        return;
    };
    draw_rtt_legend(frame, inner);
    draw_rtt_plot(frame, inner, GUTTER, &plot);
    draw_rtt_axes(frame, inner, GUTTER, &plot);
}

fn draw_rtt_legend(frame: &mut Frame, inner: Rect) {
    let legend = format!(
        "RTT   {} raw  {} smoothed",
        rtt::RAW_GLYPH,
        rtt::SMOOTHED_GLYPH
    );
    let row = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: 1,
    };
    frame.render_widget(Paragraph::new(legend), row);
}

fn draw_rtt_plot(frame: &mut Frame, inner: Rect, gutter: u16, plot: &RttPlot) {
    let buf = frame.buffer_mut();
    let x0 = inner.x + gutter;
    let y_top = inner.y + 1; // below the legend row
    for &RttMark {
        col,
        row,
        glyph,
        series,
    } in &plot.marks
    {
        let screen_row = plot.height - 1 - row; // bottom-origin row -> screen line
        let x = x0 + col;
        let y = y_top + screen_row;
        let color = match series {
            RttSeries::Kernel => Color::Cyan,
            RttSeries::Smoothed => Color::Green,
            RttSeries::Raw => Color::Reset,
        };
        buf.set_string(x, y, glyph.to_string(), Style::default().fg(color));
    }
}

fn draw_rtt_axes(frame: &mut Frame, inner: Rect, gutter: u16, plot: &RttPlot) {
    let buf = frame.buffer_mut();
    let y_top = inner.y + 1;
    // Y labels: max_rtt at the top, 0 at the bottom, in adaptive ns/µs/ms/s units.
    buf.set_string(
        inner.x,
        y_top,
        format!("{:>7}", fmt_rtt(Nanos(plot.max_rtt))),
        Style::default(),
    );
    let y_bottom = y_top + plot.height - 1;
    buf.set_string(
        inner.x,
        y_bottom,
        format!("{:>7}", fmt_rtt(Nanos(0))),
        Style::default(),
    );
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

/// Draws the Throughput/goodput graph (windowed total + goodput rates) into the reserved pane
/// interior (M9). There is no kernel overlay (design §10.M12 overlays only M7/M8; ADR-0014 §4).
fn render_throughput_body(frame: &mut Frame, app: &App, inner: Rect, focus: &FocusConn<'_>) {
    let plot_w = inner.width - GUTTER;
    let plot_h = inner.height - 2; // legend + time labels
    let Some(plot) = throughput::project(
        focus.throughput,
        focus.focus_dir,
        focus.x_span,
        app.cursor(),
        plot_w,
        plot_h,
    ) else {
        frame.render_widget(Paragraph::new("widen terminal to view graph"), inner);
        return;
    };
    draw_throughput_legend(frame, inner);
    draw_throughput_plot(frame, inner, GUTTER, &plot);
    draw_throughput_axes(frame, inner, GUTTER, &plot);
}

fn draw_throughput_legend(frame: &mut Frame, inner: Rect) {
    let legend = format!(
        "Throughput  {} total  {} goodput",
        throughput::THROUGHPUT_GLYPH,
        throughput::GOODPUT_GLYPH
    );
    let row = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: 1,
    };
    frame.render_widget(Paragraph::new(legend), row);
}

fn draw_throughput_plot(frame: &mut Frame, inner: Rect, gutter: u16, plot: &ThroughputPlot) {
    let buf = frame.buffer_mut();
    let x0 = inner.x + gutter;
    let y_top = inner.y + 1; // below the legend row
    for &ThroughputMark {
        col,
        row,
        glyph,
        series,
    } in &plot.marks
    {
        let screen_row = plot.height - 1 - row; // bottom-origin row -> screen line
        let x = x0 + col;
        let y = y_top + screen_row;
        let color = match series {
            ThroughputSeries::Goodput => Color::Green,
            ThroughputSeries::Throughput => Color::Reset,
        };
        buf.set_string(x, y, glyph.to_string(), Style::default().fg(color));
    }
}

fn draw_throughput_axes(frame: &mut Frame, inner: Rect, gutter: u16, plot: &ThroughputPlot) {
    let buf = frame.buffer_mut();
    let y_top = inner.y + 1;
    // Y labels: max_rate at the top, 0bps at the bottom, in adaptive bps/kbps/Mbps/Gbps units.
    buf.set_string(
        inner.x,
        y_top,
        format!("{:>7}", fmt_rate(plot.max_rate)),
        Style::default(),
    );
    let y_bottom = y_top + plot.height - 1;
    buf.set_string(
        inner.x,
        y_bottom,
        format!("{:>7}", fmt_rate(0)),
        Style::default(),
    );
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
    // Y labels: max_rel at the top of the plot, 0 at the bottom. `fmt_seq` keeps a large
    // (multi-GB) offset inside the gutter so it never overwrites plot columns.
    buf.set_string(
        inner.x,
        y_top,
        format!("{:>7}", fmt_seq(plot.max_rel)),
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
                "space play/pause  ←→ seek  +/- speed  ,/. step  ⏎ open  esc close  ⇥ view  / filter  s sort:{}{arrow}  q quit",
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

/// Formats a Y-axis sequence offset compactly so it fits the detail pane's gutter: raw below
/// 100 000, else an SI-suffixed `<whole>.<tenth><K|M|G|T>`. Integer-only (deterministic
/// snapshots). This keeps a multi-GB `max_rel` from overflowing the gutter into the plot.
fn fmt_seq(n: i64) -> String {
    const UNITS: [(i64, char); 4] = [
        (1_000_000_000_000, 'T'),
        (1_000_000_000, 'G'),
        (1_000_000, 'M'),
        (1_000, 'K'),
    ];
    if n < 100_000 {
        return n.to_string();
    }
    for (div, suffix) in UNITS {
        if n >= div {
            return format!("{}.{}{suffix}", n / div, (n % div) * 10 / div);
        }
    }
    n.to_string()
}

/// Formats a nanosecond RTT with an adaptive unit (ns/µs/ms/s) so a sub-millisecond value does
/// not collapse to `0.000ms`. Integer-only (deterministic snapshots). `< 1 µs` prints whole ns.
fn fmt_rtt(t: Nanos) -> String {
    const UNITS: [(u64, &str); 3] = [(1_000_000_000, "s"), (1_000_000, "ms"), (1_000, "\u{b5}s")];
    let n = t.0;
    for (div, unit) in UNITS {
        if n >= div {
            return format!("{}.{:03}{unit}", n / div, (n % div) * 1000 / div);
        }
    }
    format!("{n}ns")
}

/// Formats a bits/second rate with an adaptive SI unit (bps/kbps/Mbps/Gbps) so a slow flow does not
/// collapse to `0.000Mbps`. Integer-only (deterministic snapshots). `< 1 kbps` prints whole bps.
fn fmt_rate(bps: u64) -> String {
    const UNITS: [(u64, &str); 3] = [
        (1_000_000_000, "Gbps"),
        (1_000_000, "Mbps"),
        (1_000, "kbps"),
    ];
    for (div, unit) in UNITS {
        if bps >= div {
            return format!("{}.{:03}{unit}", bps / div, (bps % div) * 1000 / div);
        }
    }
    format!("{bps}bps")
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
    use super::{fmt_rate, fmt_rtt, render};
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
        // Width 120: the M7 footer gained the `⇥ view` switcher hint (spec §3.3), so the tail
        // (`q quit`) needs a wider viewport than M5's 80/100 — matching the other footer tests.
        let s = draw(&app, 120, 8);
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
        let tl = Timeline::with_seq(vec![(
            c2,
            vec![ss(0, 100, 0)],
            vec![sq],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )]);
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
    fn inflight_view_open_shows_graph() {
        let c = conn_span(ep(1, 5), ep(2, 443), 0, 1_000, ConnState::Established);
        let mut c2 = c;
        c2.bytes_o2r = 100;
        let inflight = vec![
            tcpvisr_engine::InFlightSample {
                t: Nanos(0),
                dir: tcpvisr_core::SampleDir::OriginToResponder,
                bytes: 50,
            },
            tcpvisr_engine::InFlightSample {
                t: Nanos(1_000),
                dir: tcpvisr_core::SampleDir::OriginToResponder,
                bytes: 100,
            },
        ];
        let tl = Timeline::with_seq(vec![(
            c2,
            vec![ss(0, 100, 0)],
            vec![],
            inflight,
            Vec::new(),
            Vec::new(),
        )]);
        let mut app = App::new(tl, "t".to_string());
        app.open_detail();
        app.cycle_detail_view(); // -> InFlight
        let s = draw(&app, 120, 14);
        assert!(s.contains("DETAIL"), "detail title: {s}");
        assert!(s.contains("In-flight"), "in-flight legend: {s}");
        assert!(s.contains('#'), "at least one wire glyph: {s}");
        assert!(s.contains("0.000s"), "an axis time label: {s}");
    }

    #[test]
    fn footer_advertises_view_switch() {
        let app = app_span(1_000_000_000);
        let s = draw(&app, 120, 8);
        assert!(s.contains("view"), "footer view-switch hint: {s}");
    }

    #[test]
    fn rtt_view_open_shows_graph() {
        let c = conn_span(ep(1, 5), ep(2, 443), 0, 1_000, ConnState::Established);
        let mut c2 = c;
        c2.bytes_o2r = 100;
        // One RTT sample at t=0 (revealed at the initial cursor = bounds.start = 0) with rtt !=
        // srtt so it emits a Raw '.' and a Smoothed '#' in distinct cells. max_rtt = 3 ms.
        let rtt = vec![tcpvisr_engine::RttSample {
            t: Nanos(0),
            dir: tcpvisr_core::SampleDir::OriginToResponder,
            rtt: Nanos(3_000_000),
            srtt: Nanos(1_500_000),
        }];
        let tl = Timeline::with_seq(vec![(c2, vec![ss(0, 100, 0)], vec![], vec![], rtt, vec![])]);
        let mut app = App::new(tl, "t".to_string());
        app.open_detail();
        app.cycle_detail_view(); // -> InFlight
        app.cycle_detail_view(); // -> Rtt
        let s = draw(&app, 120, 14);
        assert!(s.contains("DETAIL"), "detail title: {s}");
        assert!(s.contains("RTT"), "rtt legend: {s}");
        assert!(s.contains("0.000s"), "an axis time label: {s}");
        assert!(s.contains("ms"), "ms axis unit (max_rtt = 3.000ms): {s}");
        // Criterion 19: a plotted data glyph must appear. The RTT legend already contains one '#'
        // ("# smoothed"), so require at least TWO — the extra one is the plotted smoothed mark.
        let hashes = s.matches('#').count();
        assert!(
            hashes >= 2,
            "at least one plotted smoothed glyph beyond the legend: {hashes} in {s}"
        );
    }

    #[test]
    fn fmt_rtt_adapts_units() {
        assert_eq!(fmt_rtt(Nanos(450)), "450ns");
        assert_eq!(fmt_rtt(Nanos(1_500_000)), "1.500ms");
        assert_eq!(fmt_rtt(Nanos(2_000_000_000)), "2.000s");
    }

    // Criterion 15a: a sub-Mbps rate stays informative rather than collapsing to 0.000Mbps.
    #[test]
    fn fmt_rate_adapts_units() {
        assert_eq!(fmt_rate(0), "0bps");
        assert_eq!(fmt_rate(800), "800bps");
        assert_eq!(fmt_rate(1_500_000), "1.500Mbps");
        assert_eq!(fmt_rate(2_000_000_000), "2.000Gbps");
    }

    // Criterion 19: the Throughput view renders title, legend, an axis label, a bits/sec unit, and a
    // plotted goodput glyph. The goodput glyph '#' appears once in the legend ('# goodput'), so
    // require at least TWO '#' — the extra one is the plotted goodput mark. The '.' total glyph is
    // not usable as evidence (it appears in the time labels and fmt_rate output).
    #[test]
    fn throughput_view_open_shows_graph() {
        let c = conn_span(ep(1, 5), ep(2, 443), 0, 1_000, ConnState::Established);
        let mut c2 = c;
        c2.bytes_o2r = 100;
        // One sample at t=0 (revealed at the initial cursor = 0) with goodput < throughput so the
        // goodput mark plots below the total. max_rate = 3 Mbps.
        let throughput = vec![tcpvisr_engine::ThroughputSample {
            t: Nanos(0),
            dir: tcpvisr_core::SampleDir::OriginToResponder,
            throughput_bps: 3_000_000,
            goodput_bps: 1_500_000,
        }];
        let tl = Timeline::with_seq(vec![(
            c2,
            vec![ss(0, 100, 0)],
            vec![],
            vec![],
            vec![],
            throughput,
        )]);
        let mut app = App::new(tl, "t".to_string());
        app.open_detail();
        app.cycle_detail_view(); // -> InFlight
        app.cycle_detail_view(); // -> Rtt
        app.cycle_detail_view(); // -> Throughput
        let s = draw(&app, 120, 14);
        assert!(s.contains("DETAIL"), "detail title: {s}");
        assert!(s.contains("Throughput"), "throughput legend: {s}");
        assert!(s.contains("0.000s"), "an axis time label: {s}");
        assert!(
            s.contains("Mbps"),
            "bits/sec axis unit (max_rate = 3.000Mbps): {s}"
        );
        let hashes = s.matches('#').count();
        assert!(
            hashes >= 2,
            "at least one plotted goodput glyph beyond the legend: {hashes} in {s}"
        );
    }

    #[test]
    fn throughput_view_too_narrow_shows_widen_message() {
        let c = conn_span(ep(1, 5), ep(2, 443), 0, 1_000, ConnState::Established);
        let mut c2 = c;
        c2.bytes_o2r = 100;
        let throughput = vec![tcpvisr_engine::ThroughputSample {
            t: Nanos(0),
            dir: tcpvisr_core::SampleDir::OriginToResponder,
            throughput_bps: 800,
            goodput_bps: 800,
        }];
        let tl = Timeline::with_seq(vec![(
            c2,
            vec![ss(0, 100, 0)],
            vec![],
            vec![],
            vec![],
            throughput,
        )]);
        let mut app = App::new(tl, "t".to_string());
        app.open_detail();
        app.cycle_detail_view(); // InFlight
        app.cycle_detail_view(); // Rtt
        app.cycle_detail_view(); // Throughput
        let s = draw(&app, 34, 12); // right pane inner plot < MIN_W after the gutter
        assert!(
            s.contains("widen terminal"),
            "narrow throughput guidance: {s}"
        );
    }

    #[test]
    fn inflight_view_too_narrow_shows_widen_message() {
        let c = conn_span(ep(1, 5), ep(2, 443), 0, 1_000, ConnState::Established);
        let mut c2 = c;
        c2.bytes_o2r = 100;
        let inflight = vec![tcpvisr_engine::InFlightSample {
            t: Nanos(0),
            dir: tcpvisr_core::SampleDir::OriginToResponder,
            bytes: 100,
        }];
        let tl = Timeline::with_seq(vec![(
            c2,
            vec![ss(0, 100, 0)],
            vec![],
            inflight,
            Vec::new(),
            Vec::new(),
        )]);
        let mut app = App::new(tl, "t".to_string());
        app.open_detail();
        app.cycle_detail_view();
        let s = draw(&app, 34, 12); // right pane inner plot < MIN_W after the gutter
        assert!(
            s.contains("widen terminal"),
            "narrow in-flight guidance: {s}"
        );
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
        let tl = Timeline::with_seq(vec![(
            c2,
            vec![ss(0, 100, 0)],
            vec![sq],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )]);
        let mut app = App::new(tl, "t".to_string());
        app.open_detail();
        // Width 34 -> right pane 17, inner 15, plot_w = 15 - 8 gutter = 7 < MIN_W(8) -> the guard
        // fires, and the 15-wide inner still fits the "widen terminal" message.
        let s = draw(&app, 34, 12);
        assert!(s.contains("widen terminal"), "narrow detail guidance: {s}");
    }

    #[test]
    fn large_seq_axis_label_is_abbreviated_not_raw() {
        // Two O2R data points 5 GB apart -> max_rel ~5e9. The Y label must be SI-abbreviated
        // (e.g. "5.0G") so it fits the gutter, never the raw 10-digit number (which would
        // overflow into the plot columns).
        let c = conn_span(ep(1, 5), ep(2, 443), 0, 1_000, ConnState::Established);
        let mut c2 = c;
        c2.bytes_o2r = 5_000_000_000;
        let d = |t: u64, rel: i64| tcpvisr_engine::SeqSample {
            t: Nanos(t),
            dir: tcpvisr_core::SampleDir::OriginToResponder,
            rel,
            len: 100,
            kind: tcpvisr_engine::SeqKind::Data {
                retransmit: false,
                out_of_order: false,
            },
        };
        let tl = Timeline::with_seq(vec![(
            c2,
            vec![ss(0, 5_000_000_000, 0)],
            vec![d(0, 0), d(1_000, 5_000_000_000)],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )]);
        let mut app = App::new(tl, "t".to_string());
        app.open_detail();
        let s = draw(&app, 120, 14);
        assert!(s.contains('G'), "Y axis label is SI-abbreviated: {s}");
        assert!(
            !s.contains("5000000000"),
            "raw 10-digit seq number must not be printed into the gutter: {s}"
        );
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
