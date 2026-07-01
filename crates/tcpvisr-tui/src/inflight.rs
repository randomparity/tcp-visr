//! Pure In-flight projection (ADR-0012 §2): maps a connection's `InFlightSample` wire series (+
//! an optional cwnd overlay) + cursor + plot-rectangle cells to a grid of glyph marks. No
//! terminal, no I/O, no serial arithmetic (the engine already produced each `bytes` value).

use tcpvisr_core::{Nanos, SampleDir};
use tcpvisr_engine::InFlightSample;

/// Minimum inner plot rectangle; below this the detail pane shows "widen terminal".
pub const MIN_W: u16 = 8;
pub const MIN_H: u16 = 3;

pub const WIRE_GLYPH: char = '#';
pub const CWND_GLYPH: char = '+';
pub const CURSOR_GLYPH: char = '\u{250a}';

/// Which series a mark belongs to: the wire-estimated in-flight, or the (M12) kernel cwnd overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Series {
    Wire,
    Cwnd,
}

/// One plotted cell. `row` is bottom-origin (0 = 0 bytes); a top-down renderer draws it at
/// screen line `height - 1 - row`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mark {
    pub col: u16,
    pub row: u16,
    pub glyph: char,
    pub series: Series,
}

/// A resolved In-flight plot over a `width x height` cell rectangle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InFlightPlot {
    pub width: u16,
    pub height: u16,
    pub max_bytes: u64,
    pub x_span: (Nanos, Nanos),
    pub cursor_col: u16,
    pub marks: Vec<Mark>,
}

/// Fixed plot geometry shared by every mark: the focus direction, time origin/span, byte axis
/// top, and cell dimensions. Computed once in `project`; the `col`/`row`/`place` methods read it.
struct Geom {
    focus: SampleDir,
    t0: u64,
    span_t: u64,
    max_bytes: u64,
    width: u16,
    height: u16,
    cursor: Nanos,
}

impl Geom {
    /// Maps a nanosecond time to a column in `0..width`, clamped; zero-width span -> column 0.
    fn col(&self, t: u64) -> u16 {
        if self.span_t == 0 {
            return 0;
        }
        let c = u128::from(t.saturating_sub(self.t0)) * u128::from(self.width - 1)
            / u128::from(self.span_t);
        u16::try_from(c)
            .unwrap_or(self.width - 1)
            .min(self.width - 1)
    }

    /// Maps a byte count to a bottom-origin row in `0..height`, clamped; `max_bytes == 0` -> row 0.
    fn row(&self, b: u64) -> u16 {
        if self.max_bytes == 0 {
            return 0;
        }
        let r = u128::from(b) * u128::from(self.height - 1) / u128::from(self.max_bytes);
        u16::try_from(r)
            .unwrap_or(self.height - 1)
            .min(self.height - 1)
    }

    fn idx(&self, col: u16, row: u16) -> usize {
        usize::from(row) * usize::from(self.width) + usize::from(col)
    }

    /// Writes the tallest revealed (`t <= cursor`) `series` mark per column into `grid`. Bucketing
    /// keeps the numeric maximum row per column (the sawtooth peak for that time bucket).
    fn place(
        &self,
        samples: &[InFlightSample],
        series: Series,
        glyph: char,
        grid: &mut [Option<Mark>],
    ) {
        let mut peak: Vec<Option<u16>> = vec![None; usize::from(self.width)];
        for s in samples
            .iter()
            .filter(|s| s.dir == self.focus && s.t.0 <= self.cursor.0)
        {
            let col = self.col(s.t.0);
            let row = self.row(s.bytes);
            let e = &mut peak[usize::from(col)];
            if e.is_none_or(|r| row > r) {
                *e = Some(row);
            }
        }
        for (col, maybe_row) in peak.into_iter().enumerate() {
            if let Some(row) = maybe_row {
                let col = u16::try_from(col).unwrap_or(self.width - 1);
                grid[self.idx(col, row)] = Some(Mark {
                    col,
                    row,
                    glyph,
                    series,
                });
            }
        }
    }
}

/// Projects the focus-direction `wire` series (and optional `overlay`) onto a `width x height`
/// cell grid. Axes are fixed to `x_span` and `[0, max_bytes]` over both series' focus-direction
/// samples (so a diverging cwnd overlay is not clamped). Only samples with `t <= cursor` are
/// revealed; per (column, series) the tallest revealed mark is kept; a vertical cursor column is
/// drawn at `cursor`. Returns `None` if the rectangle is below the minimum.
#[must_use]
pub fn project(
    wire: &[InFlightSample],
    overlay: &[InFlightSample],
    focus: SampleDir,
    x_span: (Nanos, Nanos),
    cursor: Nanos,
    width: u16,
    height: u16,
) -> Option<InFlightPlot> {
    if width < MIN_W || height < MIN_H {
        return None;
    }
    let (t0, t1) = (x_span.0.0, x_span.1.0);
    let max_bytes = wire
        .iter()
        .chain(overlay.iter())
        .filter(|s| s.dir == focus)
        .map(|s| s.bytes)
        .max()
        .unwrap_or(0);
    let geom = Geom {
        focus,
        t0,
        span_t: t1.saturating_sub(t0),
        max_bytes,
        width,
        height,
        cursor,
    };

    let cells = usize::from(width) * usize::from(height);
    let mut grid: Vec<Option<Mark>> = vec![None; cells];
    geom.place(wire, Series::Wire, WIRE_GLYPH, &mut grid);
    geom.place(overlay, Series::Cwnd, CWND_GLYPH, &mut grid);

    let ct = cursor.0.clamp(t0, t1);
    let cursor_col = geom.col(ct);
    for row in 0..height {
        let cell = &mut grid[geom.idx(cursor_col, row)];
        if cell.is_none() {
            *cell = Some(Mark {
                col: cursor_col,
                row,
                glyph: CURSOR_GLYPH,
                series: Series::Wire,
            });
        }
    }

    let mut marks = Vec::new();
    for row in 0..height {
        for col in 0..width {
            if let Some(m) = grid[geom.idx(col, row)] {
                marks.push(m);
            }
        }
    }
    Some(InFlightPlot {
        width,
        height,
        max_bytes,
        x_span,
        cursor_col,
        marks,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use tcpvisr_core::SampleDir;

    fn wire(t: u64, bytes: u64) -> InFlightSample {
        InFlightSample {
            t: Nanos(t),
            dir: SampleDir::OriginToResponder,
            bytes,
        }
    }

    fn mark_at(p: &InFlightPlot, col: u16, row: u16) -> Option<Mark> {
        p.marks
            .iter()
            .find(|m| m.col == col && m.row == row)
            .copied()
    }

    #[test]
    fn too_small_viewport_yields_none() {
        let s = [wire(0, 10)];
        assert!(
            project(
                &s,
                &[],
                SampleDir::OriginToResponder,
                (Nanos(0), Nanos(10)),
                Nanos(0),
                MIN_W - 1,
                MIN_H
            )
            .is_none()
        );
        assert!(
            project(
                &s,
                &[],
                SampleDir::OriginToResponder,
                (Nanos(0), Nanos(10)),
                Nanos(0),
                MIN_W,
                MIN_H - 1
            )
            .is_none()
        );
    }

    #[test]
    fn corners_place_at_exact_indices() {
        let s = [wire(0, 0), wire(100, 40)]; // max_bytes = 40
        let p = project(
            &s,
            &[],
            SampleDir::OriginToResponder,
            (Nanos(0), Nanos(100)),
            Nanos(100),
            10,
            5,
        )
        .unwrap();
        assert_eq!(p.max_bytes, 40);
        assert_eq!(
            mark_at(&p, 0, 0).map(|m| m.glyph),
            Some(WIRE_GLYPH),
            "bottom-left"
        );
        assert_eq!(
            mark_at(&p, 9, 4).map(|m| m.glyph),
            Some(WIRE_GLYPH),
            "top-right col W-1 row H-1"
        );
    }

    #[test]
    fn reveal_hides_marks_after_cursor() {
        let s = [wire(0, 10), wire(10, 20), wire(20, 30)];
        let span = (Nanos(0), Nanos(20));
        let early = project(
            &s,
            &[],
            SampleDir::OriginToResponder,
            span,
            Nanos(10),
            20,
            10,
        )
        .unwrap();
        assert_eq!(
            early.marks.iter().filter(|m| m.glyph == WIRE_GLYPH).count(),
            2
        );
        let all = project(
            &s,
            &[],
            SampleDir::OriginToResponder,
            span,
            Nanos(20),
            20,
            10,
        )
        .unwrap();
        assert_eq!(
            all.marks.iter().filter(|m| m.glyph == WIRE_GLYPH).count(),
            3
        );
    }

    #[test]
    fn axes_fixed_regardless_of_cursor() {
        let s = [wire(0, 0), wire(100, 90)];
        let span = (Nanos(0), Nanos(100));
        let a = project(
            &s,
            &[],
            SampleDir::OriginToResponder,
            span,
            Nanos(0),
            20,
            10,
        )
        .unwrap();
        let b = project(
            &s,
            &[],
            SampleDir::OriginToResponder,
            span,
            Nanos(100),
            20,
            10,
        )
        .unwrap();
        assert_eq!((a.max_bytes, a.x_span), (b.max_bytes, b.x_span));
        assert_eq!(a.max_bytes, 90);
    }

    #[test]
    fn numeric_max_bucketing_over_revealed_only() {
        // Three samples share column 0 (t=0,1,2 with a wide span); the taller revealed one wins.
        let s = [wire(0, 10), wire(1, 40), wire(2, 90)];
        let span = (Nanos(0), Nanos(1000));
        // cursor=1 reveals t=0(10),t=1(40); t=2(90) hidden. max_bytes=90 over the whole series.
        let p = project(
            &s,
            &[],
            SampleDir::OriginToResponder,
            span,
            Nanos(1),
            20,
            11,
        )
        .unwrap();
        // row(40)=(40*10)/90=4; row(90)=10 (hidden). Column 0 peak is 4, not 10.
        let col0: Vec<u16> = p
            .marks
            .iter()
            .filter(|m| m.col == 0 && m.glyph == WIRE_GLYPH)
            .map(|m| m.row)
            .collect();
        assert_eq!(
            col0,
            vec![4],
            "one wire mark at the revealed peak, not the hidden t=2 peak"
        );
    }

    #[test]
    fn degenerate_spans_do_not_divide_by_zero() {
        let s = [wire(50, 10)];
        let p = project(
            &s,
            &[],
            SampleDir::OriginToResponder,
            (Nanos(50), Nanos(50)),
            Nanos(50),
            10,
            5,
        )
        .unwrap();
        // Zero-width span -> the single sample lands in column 0 (bytes==max_bytes -> top row).
        let single = p
            .marks
            .iter()
            .find(|m| m.glyph == WIRE_GLYPH)
            .expect("one wire mark");
        assert_eq!(single.col, 0);
        let z = [wire(0, 0), wire(10, 0)]; // all zero -> max_bytes 0 -> row 0
        let pz = project(
            &z,
            &[],
            SampleDir::OriginToResponder,
            (Nanos(0), Nanos(10)),
            Nanos(10),
            10,
            5,
        )
        .unwrap();
        assert_eq!(pz.max_bytes, 0);
        assert!(
            pz.marks
                .iter()
                .filter(|m| m.glyph == WIRE_GLYPH)
                .all(|m| m.row == 0)
        );
    }

    #[test]
    fn cursor_column_drawn_where_empty() {
        let s = [wire(0, 10)]; // occupies col 0
        let p = project(
            &s,
            &[],
            SampleDir::OriginToResponder,
            (Nanos(0), Nanos(100)),
            Nanos(50),
            11,
            5,
        )
        .unwrap();
        assert_eq!(p.cursor_col, 5);
        assert_eq!(mark_at(&p, 5, 0).map(|m| m.glyph), Some(CURSOR_GLYPH));
    }

    #[test]
    fn only_focus_direction_is_plotted() {
        let mut r2o = wire(0, 10);
        r2o.dir = SampleDir::ResponderToOrigin;
        let s = [wire(0, 10), r2o];
        let p = project(
            &s,
            &[],
            SampleDir::OriginToResponder,
            (Nanos(0), Nanos(10)),
            Nanos(10),
            10,
            5,
        )
        .unwrap();
        assert_eq!(p.marks.iter().filter(|m| m.glyph == WIRE_GLYPH).count(), 1);
    }

    #[test]
    fn overlay_is_distinct_and_unclamped_above_wire_max() {
        // wire peaks at 40; a cwnd overlay at 80 must expand max_bytes and sit above the wire.
        let w = [wire(0, 0), wire(100, 40)];
        let o = [InFlightSample {
            t: Nanos(100),
            dir: SampleDir::OriginToResponder,
            bytes: 80,
        }];
        let p = project(
            &w,
            &o,
            SampleDir::OriginToResponder,
            (Nanos(0), Nanos(100)),
            Nanos(100),
            10,
            11,
        )
        .unwrap();
        assert_eq!(p.max_bytes, 80, "axis expands to include the overlay");
        let cwnd: Vec<Mark> = p
            .marks
            .iter()
            .filter(|m| m.series == Series::Cwnd)
            .copied()
            .collect();
        assert_eq!(cwnd.len(), 1);
        assert_eq!(cwnd[0].glyph, CWND_GLYPH);
        // row(80) over max 80, H=11 -> (80*10)/80 = 10 (top). wire 40 -> row 5. Overlay above wire.
        assert!(
            cwnd[0].row > 5,
            "cwnd overlay sits above the wire, not clamped onto it"
        );
        // Empty overlay -> no Cwnd marks and max_bytes is the wire maximum.
        let pe = project(
            &w,
            &[],
            SampleDir::OriginToResponder,
            (Nanos(0), Nanos(100)),
            Nanos(100),
            10,
            11,
        )
        .unwrap();
        assert!(pe.marks.iter().all(|m| m.series == Series::Wire));
        assert_eq!(pe.max_bytes, 40);
    }
}
