//! Pure Time/Sequence (Stevens) projection (ADR-0011 §2–§3): maps a connection's `SeqSample`
//! series + cursor + plot-rectangle cells to a grid of glyph marks. No terminal, no I/O, no
//! serial arithmetic (the engine already unwrapped each point to an `i64` `rel`).

use tcpvisr_core::{Nanos, SampleDir};
use tcpvisr_engine::{SeqKind, SeqSample};

/// Minimum inner plot rectangle; below this the detail pane shows "widen terminal".
pub const MIN_W: u16 = 8;
pub const MIN_H: u16 = 3;

pub const DATA_GLYPH: char = '#';
pub const OOO_GLYPH: char = 'o';
pub const RETRANS_GLYPH: char = '·';
pub const SACK_GLYPH: char = '╎';
pub const CURSOR_GLYPH: char = '┊';

/// One plotted cell. `row` is bottom-origin (0 = sequence 0); a top-down renderer draws it at
/// screen line `height - 1 - row`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mark {
    pub col: u16,
    pub row: u16,
    pub glyph: char,
}

/// A resolved Time/Sequence plot over a `width x height` cell rectangle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeqPlot {
    pub width: u16,
    pub height: u16,
    pub max_rel: i64,
    pub x_span: (Nanos, Nanos),
    pub cursor_col: u16,
    pub marks: Vec<Mark>,
}

/// Salience priority (higher wins a shared cell) and the glyph for a kind.
fn kind_glyph(k: SeqKind) -> (u8, char) {
    match k {
        SeqKind::Data {
            retransmit: true, ..
        } => (3, RETRANS_GLYPH),
        SeqKind::Sack => (2, SACK_GLYPH),
        SeqKind::Data {
            retransmit: false,
            out_of_order: true,
        } => (1, OOO_GLYPH),
        SeqKind::Data {
            retransmit: false,
            out_of_order: false,
        } => (0, DATA_GLYPH),
    }
}

/// Maps a nanosecond time to a column in `0..width`, clamped; zero-width span -> column 0.
fn col_of(t: u64, t0: u64, span_t: u64, width: u16) -> u16 {
    if span_t == 0 {
        return 0;
    }
    let c = u128::from(t.saturating_sub(t0)) * u128::from(width - 1) / u128::from(span_t);
    u16::try_from(c).unwrap_or(width - 1).min(width - 1)
}

/// Maps a non-negative relative sequence to a bottom-origin row in `0..height`, clamped;
/// `max_rel <= 0` -> row 0.
fn row_of(y: i64, max_rel: i64, height: u16) -> u16 {
    if max_rel <= 0 {
        return 0;
    }
    let r = i128::from(y) * i128::from(height - 1) / i128::from(max_rel);
    u16::try_from(r).unwrap_or(height - 1).min(height - 1)
}

/// Projects `series` (only `focus`-direction samples with `t <= cursor` are revealed) onto a
/// `width x height` cell grid. Axes are fixed to `x_span` and to `[0, max_rel]` over the focus
/// direction's full sample set. Returns `None` if the rectangle is below the minimum.
#[must_use]
pub fn project(
    series: &[SeqSample],
    focus: SampleDir,
    x_span: (Nanos, Nanos),
    cursor: Nanos,
    width: u16,
    height: u16,
) -> Option<SeqPlot> {
    if width < MIN_W || height < MIN_H {
        return None;
    }
    let (t0, t1) = (x_span.0.0, x_span.1.0);
    let span_t = t1.saturating_sub(t0);
    let base = series
        .iter()
        .filter(|s| s.dir == focus)
        .map(|s| s.rel)
        .min()
        .unwrap_or(0);
    let max_rel = series
        .iter()
        .filter(|s| s.dir == focus)
        .map(|s| (s.rel - base) + i64::from(s.len))
        .max()
        .unwrap_or(0);

    let cells = usize::from(width) * usize::from(height);
    let mut grid: Vec<Option<(u8, char)>> = vec![None; cells];
    let idx = |col: u16, row: u16| usize::from(row) * usize::from(width) + usize::from(col);

    for s in series
        .iter()
        .filter(|s| s.dir == focus && s.t.0 <= cursor.0)
    {
        let col = col_of(s.t.0, t0, span_t, width);
        let row = row_of(s.rel - base, max_rel, height);
        let (prio, glyph) = kind_glyph(s.kind);
        let cell = &mut grid[idx(col, row)];
        match cell {
            Some((p, _)) if *p >= prio => {}
            _ => *cell = Some((prio, glyph)),
        }
    }

    let ct = cursor.0.clamp(t0, t1);
    let cursor_col = col_of(ct, t0, span_t, width);
    for row in 0..height {
        let cell = &mut grid[idx(cursor_col, row)];
        if cell.is_none() {
            *cell = Some((0, CURSOR_GLYPH));
        }
    }

    let mut marks = Vec::new();
    for row in 0..height {
        for col in 0..width {
            if let Some((_, glyph)) = grid[idx(col, row)] {
                marks.push(Mark { col, row, glyph });
            }
        }
    }
    Some(SeqPlot {
        width,
        height,
        max_rel,
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

    fn data(t: u64, rel: i64, len: u32) -> SeqSample {
        SeqSample {
            t: Nanos(t),
            dir: SampleDir::OriginToResponder,
            rel,
            len,
            kind: SeqKind::Data {
                retransmit: false,
                out_of_order: false,
            },
        }
    }

    fn kind(t: u64, rel: i64, k: SeqKind) -> SeqSample {
        SeqSample {
            t: Nanos(t),
            dir: SampleDir::OriginToResponder,
            rel,
            len: 0,
            kind: k,
        }
    }

    fn glyph_at(p: &SeqPlot, col: u16, row: u16) -> Option<char> {
        p.marks
            .iter()
            .find(|m| m.col == col && m.row == row)
            .map(|m| m.glyph)
    }

    #[test]
    fn too_small_viewport_yields_none() {
        let s = [data(0, 0, 10)];
        assert!(
            project(
                &s,
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
        // one point at (opened_at, rel 0), one at (effective_end, rel = max_rel via len).
        let s = [data(0, 0, 0), data(100, 40, 0)]; // max_rel = 40
        let p = project(
            &s,
            SampleDir::OriginToResponder,
            (Nanos(0), Nanos(100)),
            Nanos(100),
            10,
            5,
        )
        .expect("plot");
        assert_eq!(p.max_rel, 40);
        assert_eq!(glyph_at(&p, 0, 0), Some(DATA_GLYPH), "bottom-left");
        assert_eq!(
            glyph_at(&p, 9, 4),
            Some(DATA_GLYPH),
            "top-right: col W-1, row H-1"
        );
    }

    #[test]
    fn wrap_rel_places_without_folding() {
        // Engine already produced rel 0 and 301; max_rel = 301 + 50 = 351.
        let s = [data(0, 0, 50), data(100, 301, 50)];
        let p = project(
            &s,
            SampleDir::OriginToResponder,
            (Nanos(0), Nanos(100)),
            Nanos(100),
            20,
            10,
        )
        .expect("plot");
        assert_eq!(p.max_rel, 351);
        // rel 301 of 351 over height 10 -> row (301*9)/351 = 7.
        assert!(
            glyph_at(&p, 19, 7).is_some(),
            "second point near the top, not folded low"
        );
    }

    #[test]
    fn reveal_hides_marks_after_cursor() {
        let s = [data(0, 0, 10), data(10, 10, 10), data(20, 20, 10)];
        let span = (Nanos(0), Nanos(20));
        let early =
            project(&s, SampleDir::OriginToResponder, span, Nanos(10), 20, 10).expect("plot");
        let n_early = early.marks.iter().filter(|m| m.glyph == DATA_GLYPH).count();
        assert_eq!(n_early, 2, "t=0 and t=10 revealed, t=20 hidden");
        let all = project(&s, SampleDir::OriginToResponder, span, Nanos(20), 20, 10).expect("plot");
        let n_all = all.marks.iter().filter(|m| m.glyph == DATA_GLYPH).count();
        assert_eq!(n_all, 3);
    }

    #[test]
    fn axes_are_fixed_regardless_of_cursor() {
        let s = [data(0, 0, 10), data(100, 90, 10)];
        let span = (Nanos(0), Nanos(100));
        let a = project(&s, SampleDir::OriginToResponder, span, Nanos(0), 20, 10).expect("plot");
        let b = project(&s, SampleDir::OriginToResponder, span, Nanos(100), 20, 10).expect("plot");
        assert_eq!((a.max_rel, a.x_span), (b.max_rel, b.x_span));
        assert_eq!(a.max_rel, 100);
    }

    #[test]
    fn bucketing_prefers_the_salient_glyph() {
        // A plain data point and a retransmit in the same cell -> retransmit wins.
        let s = [
            data(0, 0, 0),
            kind(
                0,
                0,
                SeqKind::Data {
                    retransmit: true,
                    out_of_order: false,
                },
            ),
        ];
        let p = project(
            &s,
            SampleDir::OriginToResponder,
            (Nanos(0), Nanos(10)),
            Nanos(10),
            10,
            5,
        )
        .expect("plot");
        assert_eq!(glyph_at(&p, 0, 0), Some(RETRANS_GLYPH));
        // A data point and a SACK in one cell -> SACK wins over plain data.
        let s2 = [data(0, 0, 0), kind(0, 0, SeqKind::Sack)];
        let p2 = project(
            &s2,
            SampleDir::OriginToResponder,
            (Nanos(0), Nanos(10)),
            Nanos(10),
            10,
            5,
        )
        .expect("plot");
        assert_eq!(glyph_at(&p2, 0, 0), Some(SACK_GLYPH));
    }

    #[test]
    fn degenerate_spans_do_not_divide_by_zero() {
        // Single data segment: zero-width time span, one sample.
        let s = [data(50, 0, 10)];
        let p = project(
            &s,
            SampleDir::OriginToResponder,
            (Nanos(50), Nanos(50)),
            Nanos(50),
            10,
            5,
        )
        .expect("plot");
        assert_eq!(glyph_at(&p, 0, 0), Some(DATA_GLYPH));
        // Only a SACK at the baseline: max_rel == 0 -> row 0.
        let s2 = [kind(0, 0, SeqKind::Sack)];
        let p2 = project(
            &s2,
            SampleDir::OriginToResponder,
            (Nanos(0), Nanos(10)),
            Nanos(10),
            10,
            5,
        )
        .expect("plot");
        assert_eq!(p2.max_rel, 0);
        assert_eq!(glyph_at(&p2, 0, 0), Some(SACK_GLYPH));
    }

    #[test]
    fn cursor_column_drawn_where_empty() {
        let s = [data(0, 0, 10)]; // occupies col 0
        let p = project(
            &s,
            SampleDir::OriginToResponder,
            (Nanos(0), Nanos(100)),
            Nanos(50),
            11,
            5,
        )
        .expect("plot");
        // cursor at t=50 of [0,100] over width 11 -> col (50*10)/100 = 5.
        assert_eq!(p.cursor_col, 5);
        assert_eq!(
            glyph_at(&p, 5, 0),
            Some(CURSOR_GLYPH),
            "cursor fills its empty column"
        );
        assert_eq!(
            glyph_at(&p, 0, 0),
            Some(DATA_GLYPH),
            "data cell not overwritten by cursor"
        );
    }

    #[test]
    fn only_focus_direction_is_plotted() {
        let mut r2o = data(0, 0, 10);
        r2o.dir = SampleDir::ResponderToOrigin;
        let s = [data(0, 0, 10), r2o];
        let p = project(
            &s,
            SampleDir::OriginToResponder,
            (Nanos(0), Nanos(10)),
            Nanos(10),
            10,
            5,
        )
        .expect("plot");
        let data_marks = p.marks.iter().filter(|m| m.glyph == DATA_GLYPH).count();
        assert_eq!(data_marks, 1, "only the O2R data point");
    }
}
