//! Pure RTT projection (ADR-0013 §3): maps a connection's `RttSample` wire series (raw per-ack
//! RTT + smoothed SRTT) plus an optional kernel-srtt overlay + cursor + plot-rectangle cells to a
//! grid of glyph marks. No terminal, no I/O, no float (the EWMA was done in the engine).

use tcpvisr_core::{Nanos, SampleDir};
use tcpvisr_engine::RttSample;

/// Minimum inner plot rectangle; below this the detail pane shows "widen terminal".
pub const MIN_W: u16 = 8;
pub const MIN_H: u16 = 3;

pub const RAW_GLYPH: char = '.';
pub const SMOOTHED_GLYPH: char = '#';
pub const KERNEL_GLYPH: char = '+';
pub const CURSOR_GLYPH: char = '\u{250a}';

/// Which series a mark belongs to: raw per-ack RTT, wire-smoothed SRTT, or the (M12) kernel srtt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Series {
    Raw,
    Smoothed,
    Kernel,
}

/// One plotted cell. `row` is bottom-origin (0 = 0 ns); a top-down renderer draws it at screen
/// line `height - 1 - row`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mark {
    pub col: u16,
    pub row: u16,
    pub glyph: char,
    pub series: Series,
}

/// A resolved RTT plot over a `width x height` cell rectangle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RttPlot {
    pub width: u16,
    pub height: u16,
    pub max_rtt: u64,
    pub x_span: (Nanos, Nanos),
    pub cursor_col: u16,
    pub marks: Vec<Mark>,
}

/// Fixed plot geometry shared by every mark: the focus direction, time origin/span, RTT axis top,
/// and cell dimensions. Computed once in `project`; the `col`/`row`/`place` methods read it.
struct Geom {
    focus: SampleDir,
    t0: u64,
    span_t: u64,
    max_rtt: u64,
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

    /// Maps an RTT (ns) to a bottom-origin row in `0..height`, clamped; `max_rtt == 0` -> row 0.
    fn row(&self, v: u64) -> u16 {
        if self.max_rtt == 0 {
            return 0;
        }
        let r = u128::from(v) * u128::from(self.height - 1) / u128::from(self.max_rtt);
        u16::try_from(r)
            .unwrap_or(self.height - 1)
            .min(self.height - 1)
    }

    fn idx(&self, col: u16, row: u16) -> usize {
        usize::from(row) * usize::from(self.width) + usize::from(col)
    }

    /// Writes the tallest revealed (`t <= cursor`) mark per column for one series into `grid`,
    /// reading each sample's plotted value via `value` (rtt for Raw, srtt for Smoothed/Kernel).
    /// Bucketing keeps the numeric maximum row per column (the peak for that time bucket).
    fn place(
        &self,
        samples: &[RttSample],
        series: Series,
        glyph: char,
        value: impl Fn(&RttSample) -> u64,
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

/// Projects the focus-direction `wire` RTT series (raw + smoothed) and an optional kernel-srtt
/// `overlay` onto a `width x height` grid. Y is `[0, max_rtt]` over the focus direction's wire
/// `rtt`/`srtt` and overlay `srtt` (so a diverging overlay is not clamped); X is `x_span`; only
/// `t <= cursor` samples are revealed; per (column, series) the tallest revealed mark is kept; a
/// vertical cursor column is drawn at `cursor`. Same-cell precedence resolves in place order
/// (raw, then smoothed, then kernel: the later wins the cell). Returns `None` below the minimum.
#[must_use]
pub fn project(
    wire: &[RttSample],
    overlay: &[RttSample],
    focus: SampleDir,
    x_span: (Nanos, Nanos),
    cursor: Nanos,
    width: u16,
    height: u16,
) -> Option<RttPlot> {
    if width < MIN_W || height < MIN_H {
        return None;
    }
    let (t0, t1) = (x_span.0.0, x_span.1.0);
    let max_rtt = wire
        .iter()
        .filter(|s| s.dir == focus)
        .flat_map(|s| [s.rtt.0, s.srtt.0])
        .chain(overlay.iter().filter(|s| s.dir == focus).map(|s| s.srtt.0))
        .max()
        .unwrap_or(0);
    let geom = Geom {
        focus,
        t0,
        span_t: t1.saturating_sub(t0),
        max_rtt,
        width,
        height,
        cursor,
    };

    let cells = usize::from(width) * usize::from(height);
    let mut grid: Vec<Option<Mark>> = vec![None; cells];
    geom.place(wire, Series::Raw, RAW_GLYPH, |s| s.rtt.0, &mut grid);
    geom.place(
        wire,
        Series::Smoothed,
        SMOOTHED_GLYPH,
        |s| s.srtt.0,
        &mut grid,
    );
    geom.place(
        overlay,
        Series::Kernel,
        KERNEL_GLYPH,
        |s| s.srtt.0,
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
                series: Series::Raw,
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
    Some(RttPlot {
        width,
        height,
        max_rtt,
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

    fn rtt(t: u64, rtt_ns: u64, srtt_ns: u64) -> RttSample {
        RttSample {
            t: Nanos(t),
            dir: SampleDir::OriginToResponder,
            rtt: Nanos(rtt_ns),
            srtt: Nanos(srtt_ns),
        }
    }

    fn marks_at(p: &RttPlot, col: u16, row: u16) -> Vec<Mark> {
        p.marks
            .iter()
            .filter(|m| m.col == col && m.row == row)
            .copied()
            .collect()
    }

    #[test]
    fn too_small_viewport_yields_none() {
        let s = [rtt(0, 10, 10)];
        let span = (Nanos(0), Nanos(10));
        assert!(
            project(
                &s,
                &[],
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
                &[],
                SampleDir::OriginToResponder,
                span,
                Nanos(0),
                MIN_W,
                MIN_H - 1
            )
            .is_none()
        );
    }

    // Criterion 7: corners — a sample at (end, max) lands a mark at top-right (col W-1, row H-1);
    // a sample at (start, 0) lands a mark at bottom-left. Both corners have rtt == srtt, so the
    // two series coincide and the single-grid projection keeps the last-placed (Smoothed);
    // assert a mark of any series lands at each corner (raw/smoothed distinctness is criterion 14).
    #[test]
    fn corners_place_at_exact_indices() {
        let s = [rtt(0, 0, 0), rtt(100, 40, 40)];
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
        assert_eq!(p.max_rtt, 40);
        assert!(!marks_at(&p, 0, 0).is_empty(), "a mark at bottom-left");
        assert!(
            !marks_at(&p, 9, 4).is_empty(),
            "a mark at top-right (col W-1, row H-1)"
        );
    }

    // Criterion 14: a single sample in its own column with rtt != srtt emits a Raw mark and a
    // Smoothed mark in the same column at different rows, with distinct glyphs.
    #[test]
    fn raw_and_smoothed_align_in_column() {
        // rtt=40 (max) -> row H-1; srtt=20 -> half. Single sample -> its own column (bucketing no-op).
        let s = [rtt(100, 40, 20)];
        let p = project(
            &s,
            &[],
            SampleDir::OriginToResponder,
            (Nanos(0), Nanos(100)),
            Nanos(100),
            10,
            11,
        )
        .unwrap();
        let raw = p
            .marks
            .iter()
            .find(|m| m.series == Series::Raw && m.glyph == RAW_GLYPH)
            .unwrap();
        let smooth = p
            .marks
            .iter()
            .find(|m| m.series == Series::Smoothed && m.glyph == SMOOTHED_GLYPH)
            .unwrap();
        assert_eq!(raw.col, smooth.col, "same column");
        assert_ne!(raw.row, smooth.row, "different rows");
        assert!(raw.row > smooth.row, "raw (40) plots above smoothed (20)");
    }

    // Criterion 9: reveal-to-T hides raw marks after the cursor.
    #[test]
    fn reveal_hides_marks_after_cursor() {
        // rtt == srtt here, so raw/smoothed coincide and Smoothed wins each cell; count the
        // surviving Smoothed marks (one per revealed sample column).
        let s = [rtt(0, 10, 10), rtt(10, 20, 20), rtt(20, 30, 30)];
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
            early
                .marks
                .iter()
                .filter(|m| m.glyph == SMOOTHED_GLYPH)
                .count(),
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
            all.marks
                .iter()
                .filter(|m| m.glyph == SMOOTHED_GLYPH)
                .count(),
            3
        );
    }

    // Criterion 8: axes are fixed regardless of the cursor.
    #[test]
    fn axes_fixed_regardless_of_cursor() {
        let s = [rtt(0, 0, 0), rtt(100, 90, 90)];
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
        assert_eq!((a.max_rtt, a.x_span), (b.max_rtt, b.x_span));
        assert_eq!(a.max_rtt, 90);
    }

    // Criterion 10: numeric-max bucketing over revealed raw samples only.
    #[test]
    fn numeric_max_bucketing_over_revealed_only() {
        let s = [rtt(0, 10, 10), rtt(1, 40, 40), rtt(2, 90, 90)];
        let span = (Nanos(0), Nanos(1000));
        // cursor=1 reveals t=0(10),t=1(40); t=2(90) hidden. max_rtt=90 over the whole series.
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
        // row(40)=(40*10)/90=4; row(90)=10 (hidden). Column 0 peak is 4, not 10. rtt==srtt here,
        // so Smoothed wins the cell; assert on the surviving Smoothed mark.
        let col0: Vec<u16> = p
            .marks
            .iter()
            .filter(|m| m.col == 0 && m.glyph == SMOOTHED_GLYPH)
            .map(|m| m.row)
            .collect();
        assert_eq!(
            col0,
            vec![4],
            "one mark at the revealed peak, not the hidden t=2 peak"
        );
    }

    // Criterion 11: degenerate spans do not divide by zero.
    #[test]
    fn degenerate_spans_do_not_divide_by_zero() {
        let s = [rtt(50, 10, 10)];
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
        // rtt==srtt, so Smoothed wins the coincident cell; assert on the surviving mark.
        let single = p
            .marks
            .iter()
            .find(|m| m.glyph == SMOOTHED_GLYPH)
            .expect("one plotted mark");
        assert_eq!(single.col, 0, "zero-width span -> column 0");
        let z = [rtt(0, 0, 0), rtt(10, 0, 0)]; // all zero -> max_rtt 0 -> row 0
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
        assert_eq!(pz.max_rtt, 0);
        assert!(
            pz.marks
                .iter()
                .filter(|m| m.glyph == SMOOTHED_GLYPH)
                .all(|m| m.row == 0)
        );
    }

    // Criterion 12: the cursor column is drawn where no mark occupies the cell.
    #[test]
    fn cursor_column_drawn_where_empty() {
        let s = [rtt(0, 10, 10)]; // occupies col 0
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
        assert_eq!(
            marks_at(&p, 5, 0).first().map(|m| m.glyph),
            Some(CURSOR_GLYPH)
        );
    }

    // Only the focus direction is plotted.
    #[test]
    fn only_focus_direction_is_plotted() {
        let mut r2o = rtt(0, 10, 10);
        r2o.dir = SampleDir::ResponderToOrigin;
        let s = [rtt(0, 10, 10), r2o];
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
        // The single O2R sample has rtt==srtt (coincident) -> one surviving mark in its cell.
        assert_eq!(
            p.marks
                .iter()
                .filter(|m| m.glyph == RAW_GLYPH || m.glyph == SMOOTHED_GLYPH)
                .count(),
            1
        );
    }

    // Criterion 15: the kernel overlay is a distinct series, unclamped above the wire maximum.
    #[test]
    fn overlay_is_distinct_and_unclamped_above_wire_max() {
        // wire peaks at 40 (rtt); a kernel srtt overlay at 80 must expand max_rtt and sit above.
        let w = [rtt(0, 0, 0), rtt(100, 40, 40)];
        let o = [RttSample {
            t: Nanos(100),
            dir: SampleDir::OriginToResponder,
            rtt: Nanos(0),
            srtt: Nanos(80),
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
        assert_eq!(p.max_rtt, 80, "axis expands to include the overlay srtt");
        let kernel: Vec<Mark> = p
            .marks
            .iter()
            .filter(|m| m.series == Series::Kernel)
            .copied()
            .collect();
        assert_eq!(kernel.len(), 1);
        assert_eq!(kernel[0].glyph, KERNEL_GLYPH);
        // row(80) over max 80, H=11 -> 10 (top). wire 40 -> row 5. Overlay above wire.
        assert!(
            kernel[0].row > 5,
            "kernel overlay sits above the wire, not clamped onto it"
        );
        // Empty overlay -> no Kernel marks and max_rtt is the wire maximum.
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
        assert!(pe.marks.iter().all(|m| m.series != Series::Kernel));
        assert_eq!(pe.max_rtt, 40);
    }
}
