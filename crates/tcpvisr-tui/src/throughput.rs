//! Pure throughput/goodput projection (ADR-0014 §3): maps a connection's `ThroughputSample` wire
//! series (trailing-window throughput + goodput) plus a cursor + plot-rectangle cells to a grid of
//! glyph marks. No terminal, no I/O, no float. Two wire series and no kernel overlay (design
//! §10.M12 overlays only M7/M8, so a throughput overlay would be a phantom seam; ADR-0014 §4).

use tcpvisr_core::{Nanos, SampleDir};
use tcpvisr_engine::ThroughputSample;

/// Minimum inner plot rectangle; below this the detail pane shows "widen terminal".
pub const MIN_W: u16 = 8;
pub const MIN_H: u16 = 3;

pub const THROUGHPUT_GLYPH: char = '.';
pub const GOODPUT_GLYPH: char = '#';
pub const CURSOR_GLYPH: char = '\u{250a}';

/// Which series a mark belongs to: the total trailing-window throughput or the goodput
/// (non-retransmitted) subset. Goodput is always ≤ throughput, so it plots at or below the total.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Series {
    Throughput,
    Goodput,
}

/// One plotted cell. `row` is bottom-origin (0 = 0 bps); a top-down renderer draws it at screen
/// line `height - 1 - row`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mark {
    pub col: u16,
    pub row: u16,
    pub glyph: char,
    pub series: Series,
}

/// A resolved throughput plot over a `width x height` cell rectangle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThroughputPlot {
    pub width: u16,
    pub height: u16,
    pub max_rate: u64,
    pub x_span: (Nanos, Nanos),
    pub cursor_col: u16,
    pub marks: Vec<Mark>,
}

/// Fixed plot geometry shared by every mark: the focus direction, time origin/span, rate axis top,
/// and cell dimensions. Computed once in `project`; the `col`/`row`/`place` methods read it.
struct Geom {
    focus: SampleDir,
    t0: u64,
    span_t: u64,
    max_rate: u64,
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

    /// Maps a rate (bps) to a bottom-origin row in `0..height`, clamped; `max_rate == 0` -> row 0.
    fn row(&self, v: u64) -> u16 {
        if self.max_rate == 0 {
            return 0;
        }
        let r = u128::from(v) * u128::from(self.height - 1) / u128::from(self.max_rate);
        u16::try_from(r)
            .unwrap_or(self.height - 1)
            .min(self.height - 1)
    }

    fn idx(&self, col: u16, row: u16) -> usize {
        usize::from(row) * usize::from(self.width) + usize::from(col)
    }

    /// Writes the tallest revealed (`t <= cursor`) mark per column for one series into `grid`,
    /// reading each sample's plotted value via `value` (`throughput_bps` or `goodput_bps`). Bucketing
    /// keeps the numeric maximum row per column (the peak for that time bucket).
    fn place(
        &self,
        samples: &[ThroughputSample],
        series: Series,
        glyph: char,
        value: impl Fn(&ThroughputSample) -> u64,
        grid: &mut [Option<Mark>],
    ) {
        let mut peak: Vec<Option<u16>> = vec![None; usize::from(self.width)];
        for s in samples
            .iter()
            .filter(|s| s.dir == self.focus && s.t.0 <= self.cursor.0)
        {
            let col = self.col(s.t.0);
            let row = self.row(value(s));
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

/// Projects the focus-direction `wire` throughput series (total + goodput) onto a `width x height`
/// grid. Y is `[0, max_rate]` over the focus direction's throughput and goodput; X is
/// `x_span`; only `t <= cursor` samples are revealed; per (column, series) the tallest revealed mark
/// is kept; a vertical cursor column is drawn at `cursor`. Same-cell precedence resolves in place
/// order (throughput, then goodput: the goodput wins a coincident cell, so the useful rate shows
/// over the total). Returns `None` below the minimum viewport.
#[must_use]
pub fn project(
    wire: &[ThroughputSample],
    focus: SampleDir,
    x_span: (Nanos, Nanos),
    cursor: Nanos,
    width: u16,
    height: u16,
) -> Option<ThroughputPlot> {
    if width < MIN_W || height < MIN_H {
        return None;
    }
    let (t0, t1) = (x_span.0.0, x_span.1.0);
    let max_rate = wire
        .iter()
        .filter(|s| s.dir == focus)
        .flat_map(|s| [s.throughput_bps, s.goodput_bps])
        .max()
        .unwrap_or(0);
    let geom = Geom {
        focus,
        t0,
        span_t: t1.saturating_sub(t0),
        max_rate,
        width,
        height,
        cursor,
    };

    let cells = usize::from(width) * usize::from(height);
    let mut grid: Vec<Option<Mark>> = vec![None; cells];
    geom.place(
        wire,
        Series::Throughput,
        THROUGHPUT_GLYPH,
        |s| s.throughput_bps,
        &mut grid,
    );
    geom.place(
        wire,
        Series::Goodput,
        GOODPUT_GLYPH,
        |s| s.goodput_bps,
        &mut grid,
    );

    let ct = cursor.0.clamp(t0, t1);
    let cursor_col = geom.col(ct);
    for row in 0..height {
        let cell = &mut grid[geom.idx(cursor_col, row)];
        if cell.is_none() {
            *cell = Some(Mark {
                col: cursor_col,
                row,
                glyph: CURSOR_GLYPH,
                series: Series::Throughput,
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
    Some(ThroughputPlot {
        width,
        height,
        max_rate,
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

    fn tp(t: u64, throughput: u64, goodput: u64) -> ThroughputSample {
        ThroughputSample {
            t: Nanos(t),
            dir: SampleDir::OriginToResponder,
            throughput_bps: throughput,
            goodput_bps: goodput,
        }
    }

    fn marks_at(p: &ThroughputPlot, col: u16, row: u16) -> Vec<Mark> {
        p.marks
            .iter()
            .filter(|m| m.col == col && m.row == row)
            .copied()
            .collect()
    }

    // Criterion 14: below the minimum viewport, no plot.
    #[test]
    fn too_small_viewport_yields_none() {
        let s = [tp(0, 10, 10)];
        let span = (Nanos(0), Nanos(10));
        assert!(
            project(
                &s,
                SampleDir::OriginToResponder,
                span,
                Nanos(0),
                MIN_W - 1,
                MIN_H
            )
            .is_none()
        );
        assert!(
            project(
                &s,
                SampleDir::OriginToResponder,
                span,
                Nanos(0),
                MIN_W,
                MIN_H - 1
            )
            .is_none()
        );
    }

    // Criterion 8: corners — a sample at (end, throughput=goodput=max) lands a mark at top-right
    // (the two series coincide, so the single grid keeps the last-placed Goodput); a sample at
    // (start, 0) lands a mark at bottom-left.
    #[test]
    fn corners_place_at_exact_indices() {
        let s = [tp(0, 0, 0), tp(100, 40, 40)];
        let p = project(
            &s,
            SampleDir::OriginToResponder,
            (Nanos(0), Nanos(100)),
            Nanos(100),
            10,
            5,
        )
        .unwrap();
        assert_eq!(p.max_rate, 40);
        assert!(!marks_at(&p, 0, 0).is_empty(), "a mark at bottom-left");
        assert!(
            !marks_at(&p, 9, 4).is_empty(),
            "a mark at top-right (col W-1, row H-1)"
        );
    }

    // Criterion 15: a single sample in its own column with goodput < throughput emits a Throughput
    // mark and a Goodput mark in the same column at different rows, goodput below, distinct glyphs.
    #[test]
    fn total_and_goodput_align_in_column() {
        // throughput=40 (max) -> row H-1; goodput=20 -> half. Single sample -> own column.
        let s = [tp(100, 40, 20)];
        let p = project(
            &s,
            SampleDir::OriginToResponder,
            (Nanos(0), Nanos(100)),
            Nanos(100),
            10,
            11,
        )
        .unwrap();
        let total = p
            .marks
            .iter()
            .find(|m| m.series == Series::Throughput && m.glyph == THROUGHPUT_GLYPH)
            .unwrap();
        let good = p
            .marks
            .iter()
            .find(|m| m.series == Series::Goodput && m.glyph == GOODPUT_GLYPH)
            .unwrap();
        assert_eq!(total.col, good.col, "same column");
        assert_ne!(total.row, good.row, "different rows");
        assert!(total.row > good.row, "total (40) plots above goodput (20)");
    }

    // Criterion 10: reveal-to-T hides marks after the cursor.
    #[test]
    fn reveal_hides_marks_after_cursor() {
        // throughput == goodput here, so the two coincide and Goodput wins each cell; count the
        // surviving Goodput marks (one per revealed sample column).
        let s = [tp(0, 10, 10), tp(10, 20, 20), tp(20, 30, 30)];
        let span = (Nanos(0), Nanos(20));
        let early = project(&s, SampleDir::OriginToResponder, span, Nanos(10), 20, 10).unwrap();
        assert_eq!(
            early
                .marks
                .iter()
                .filter(|m| m.glyph == GOODPUT_GLYPH)
                .count(),
            2
        );
        let all = project(&s, SampleDir::OriginToResponder, span, Nanos(20), 20, 10).unwrap();
        assert_eq!(
            all.marks
                .iter()
                .filter(|m| m.glyph == GOODPUT_GLYPH)
                .count(),
            3
        );
    }

    // Criterion 9: axes are fixed regardless of the cursor.
    #[test]
    fn axes_fixed_regardless_of_cursor() {
        let s = [tp(0, 0, 0), tp(100, 90, 90)];
        let span = (Nanos(0), Nanos(100));
        let a = project(&s, SampleDir::OriginToResponder, span, Nanos(0), 20, 10).unwrap();
        let b = project(&s, SampleDir::OriginToResponder, span, Nanos(100), 20, 10).unwrap();
        assert_eq!((a.max_rate, a.x_span), (b.max_rate, b.x_span));
        assert_eq!(a.max_rate, 90);
    }

    // Criterion 11: numeric-max bucketing over revealed throughput samples only.
    #[test]
    fn numeric_max_bucketing_over_revealed_only() {
        let s = [tp(0, 10, 10), tp(1, 40, 40), tp(2, 90, 90)];
        let span = (Nanos(0), Nanos(1000));
        // cursor=1 reveals t=0(10),t=1(40); t=2(90) hidden. max_rate=90 over the whole series.
        let p = project(&s, SampleDir::OriginToResponder, span, Nanos(1), 20, 11).unwrap();
        // row(40)=(40*10)/90=4; row(90)=10 (hidden). Column 0 peak is 4. throughput==goodput here,
        // so Goodput wins the cell; assert on the surviving Goodput mark.
        let col0: Vec<u16> = p
            .marks
            .iter()
            .filter(|m| m.col == 0 && m.glyph == GOODPUT_GLYPH)
            .map(|m| m.row)
            .collect();
        assert_eq!(
            col0,
            vec![4],
            "one mark at the revealed peak, not the hidden t=2 peak"
        );
    }

    // Criterion 12: degenerate spans do not divide by zero.
    #[test]
    fn degenerate_spans_do_not_divide_by_zero() {
        let s = [tp(50, 10, 10)];
        let p = project(
            &s,
            SampleDir::OriginToResponder,
            (Nanos(50), Nanos(50)),
            Nanos(50),
            10,
            5,
        )
        .unwrap();
        // throughput==goodput, so Goodput wins the coincident cell.
        let single = p
            .marks
            .iter()
            .find(|m| m.glyph == GOODPUT_GLYPH)
            .expect("one plotted mark");
        assert_eq!(single.col, 0, "zero-width span -> column 0");
        let z = [tp(0, 0, 0), tp(10, 0, 0)]; // all zero -> max_rate 0 -> row 0
        let pz = project(
            &z,
            SampleDir::OriginToResponder,
            (Nanos(0), Nanos(10)),
            Nanos(10),
            10,
            5,
        )
        .unwrap();
        assert_eq!(pz.max_rate, 0);
        assert!(
            pz.marks
                .iter()
                .filter(|m| m.glyph == GOODPUT_GLYPH)
                .all(|m| m.row == 0)
        );
    }

    // Criterion 13: the cursor column is drawn where no mark occupies the cell.
    #[test]
    fn cursor_column_drawn_where_empty() {
        let s = [tp(0, 10, 10)]; // occupies col 0
        let p = project(
            &s,
            SampleDir::OriginToResponder,
            (Nanos(0), Nanos(100)),
            Nanos(50),
            11,
            5,
        )
        .unwrap();
        assert_eq!(p.cursor_col, 5);
        assert_eq!(
            marks_at(&p, 5, 0).first().map(|m| m.glyph),
            Some(CURSOR_GLYPH)
        );
    }

    // Only the focus direction is plotted.
    #[test]
    fn only_focus_direction_is_plotted() {
        let mut r2o = tp(0, 10, 10);
        r2o.dir = SampleDir::ResponderToOrigin;
        let s = [tp(0, 10, 10), r2o];
        let p = project(
            &s,
            SampleDir::OriginToResponder,
            (Nanos(0), Nanos(10)),
            Nanos(10),
            10,
            5,
        )
        .unwrap();
        // The single O2R sample has throughput==goodput (coincident) -> one surviving mark.
        assert_eq!(
            p.marks
                .iter()
                .filter(|m| m.glyph == THROUGHPUT_GLYPH || m.glyph == GOODPUT_GLYPH)
                .count(),
            1
        );
    }

    // The goodput gap: when goodput < throughput both marks survive at distinct rows.
    #[test]
    fn goodput_gap_shows_both_series() {
        let s = [tp(50, 80, 40)];
        let p = project(
            &s,
            SampleDir::OriginToResponder,
            (Nanos(0), Nanos(100)),
            Nanos(100),
            10,
            11,
        )
        .unwrap();
        assert_eq!(p.max_rate, 80);
        // Filter by glyph, not series: cursor-column cells also carry Series::Throughput.
        let total = p
            .marks
            .iter()
            .find(|m| m.glyph == THROUGHPUT_GLYPH)
            .unwrap();
        let good = p.marks.iter().find(|m| m.glyph == GOODPUT_GLYPH).unwrap();
        assert_eq!(total.series, Series::Throughput);
        assert_eq!(good.series, Series::Goodput);
        // row(80) over max 80 -> 10 (top); row(40) -> 5. The gap between them is the retransmit rate.
        assert_eq!(total.row, 10);
        assert_eq!(good.row, 5);
    }
}
