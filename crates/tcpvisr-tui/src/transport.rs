//! The pure playback transport (ADR-0010): cursor time, speed ladder, play/pause. Advances on
//! an injected wall-clock delta — it never reads a clock (the impure `run` loop supplies `dt`).

use tcpvisr_core::Nanos;

/// The selectable playback speeds (0.1–10×; every rung renders exactly at one decimal). The
/// `SPEEDS` array is for display only; `RATIOS` is the exact `(numerator, denominator)` used
/// for cursor math so `tick` needs no float cast (which the crate's `allow_attributes = deny`
/// plus the pedantic cast lints would otherwise force an `#[expect]` around).
const SPEEDS: [f64; 6] = [0.1, 0.5, 1.0, 2.0, 5.0, 10.0];
const RATIOS: [(u64, u64); 6] = [(1, 10), (1, 2), (1, 1), (2, 1), (5, 1), (10, 1)];
const DEFAULT_SPEED_IDX: usize = 2; // 1.0x

/// Cursor + speed + play state over a capture's `[start, end]` time domain.
#[derive(Debug, Clone, Copy)]
pub struct Transport {
    start: Nanos,
    end: Nanos,
    cursor: Nanos,
    speed_idx: usize,
    playing: bool,
}

impl Transport {
    /// A paused transport at `start`, speed 1.0×, over `[start, end]`.
    #[must_use]
    pub fn new(start: Nanos, end: Nanos) -> Self {
        Self {
            start,
            end,
            cursor: start,
            speed_idx: DEFAULT_SPEED_IDX,
            playing: false,
        }
    }

    /// The current cursor time.
    #[must_use]
    pub fn cursor(&self) -> Nanos {
        self.cursor
    }

    /// The current playback speed multiplier.
    #[must_use]
    pub fn speed(&self) -> f64 {
        SPEEDS[self.speed_idx]
    }

    /// Whether playback is running.
    #[must_use]
    pub fn is_playing(&self) -> bool {
        self.playing
    }

    /// The `[start, end]` cursor domain.
    #[must_use]
    pub fn bounds(&self) -> (Nanos, Nanos) {
        (self.start, self.end)
    }

    /// Toggles play/pause. Starting playback from the end rewinds to `start` first.
    pub fn toggle_play(&mut self) {
        if self.playing {
            self.playing = false;
        } else {
            if self.cursor.0 >= self.end.0 {
                self.cursor = self.start;
            }
            self.playing = true;
        }
    }

    /// Steps one rung up the speed ladder (clamped at 10×).
    pub fn faster(&mut self) {
        self.speed_idx = (self.speed_idx + 1).min(SPEEDS.len() - 1);
    }

    /// Steps one rung down the speed ladder (clamped at 0.1×).
    pub fn slower(&mut self) {
        self.speed_idx = self.speed_idx.saturating_sub(1);
    }

    /// Moves the cursor by ~2% of the span (min 1ns), clamped to `[start, end]`.
    pub fn seek(&mut self, forward: bool) {
        let step = (self.end.0.saturating_sub(self.start.0) / 50).max(1);
        let next = if forward {
            self.cursor.0.saturating_add(step)
        } else {
            self.cursor.0.saturating_sub(step)
        };
        self.set_cursor(Nanos(next));
    }

    /// Sets the cursor, clamped to `[start, end]`.
    pub fn set_cursor(&mut self, t: Nanos) {
        self.cursor = Nanos(t.0.clamp(self.start.0, self.end.0));
    }

    /// When playing, advances the cursor by `speed * dt` ns (exact integer ratio math), clamped
    /// to `end`; reaching `end` auto-pauses. `dt` is injected wall-clock nanoseconds. A no-op
    /// when paused.
    pub fn tick(&mut self, dt: Nanos) {
        if !self.playing {
            return;
        }
        let (num, den) = RATIOS[self.speed_idx];
        // u128 intermediate: dt.0 (u64) * num (<=10) cannot overflow; try_from clamps the
        // (already-bounded) result back into u64 without a lint-tripping `as` cast.
        let prod = u128::from(dt.0) * u128::from(num) / u128::from(den);
        let adv = u64::try_from(prod).unwrap_or(u64::MAX);
        self.cursor = Nanos(self.cursor.0.saturating_add(adv).min(self.end.0));
        if self.cursor.0 >= self.end.0 {
            self.playing = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn play_then_tick_advances_by_speed() {
        let mut tr = Transport::new(Nanos(0), Nanos(10_000_000_000)); // 0..10s
        assert!(!tr.is_playing());
        tr.toggle_play();
        assert!(tr.is_playing());
        tr.tick(Nanos(1_000_000_000)); // 1s at 1.0x
        assert_eq!(tr.cursor(), Nanos(1_000_000_000));
    }

    #[test]
    fn paused_tick_is_noop() {
        let mut tr = Transport::new(Nanos(0), Nanos(10_000_000_000));
        tr.tick(Nanos(1_000_000_000));
        assert_eq!(tr.cursor(), Nanos(0));
    }

    #[test]
    fn speed_ladder_clamps_and_scales() {
        let mut tr = Transport::new(Nanos(0), Nanos(100_000_000_000));
        for _ in 0..10 {
            tr.faster();
        }
        assert!((tr.speed() - 10.0).abs() < 1e-9, "clamped at 10x");
        tr.toggle_play();
        tr.tick(Nanos(1_000_000_000)); // 1s at 10x -> 10s
        assert_eq!(tr.cursor(), Nanos(10_000_000_000));
        for _ in 0..10 {
            tr.slower();
        }
        assert!((tr.speed() - 0.1).abs() < 1e-9, "clamped at 0.1x");
    }

    #[test]
    fn tick_auto_pauses_at_end() {
        let mut tr = Transport::new(Nanos(0), Nanos(1_000_000_000));
        tr.toggle_play();
        tr.tick(Nanos(5_000_000_000)); // overshoots
        assert_eq!(tr.cursor(), Nanos(1_000_000_000));
        assert!(!tr.is_playing());
    }

    #[test]
    fn toggle_at_end_rewinds_and_plays() {
        let mut tr = Transport::new(Nanos(1_000), Nanos(5_000));
        tr.set_cursor(Nanos(5_000));
        tr.toggle_play();
        assert_eq!(tr.cursor(), Nanos(1_000));
        assert!(tr.is_playing());
    }

    #[test]
    fn seek_moves_two_percent_and_clamps() {
        let mut tr = Transport::new(Nanos(0), Nanos(5_000)); // step = 5000/50 = 100
        tr.seek(true);
        assert_eq!(tr.cursor(), Nanos(100));
        tr.seek(false);
        tr.seek(false);
        assert_eq!(tr.cursor(), Nanos(0), "clamped at start");
        for _ in 0..100 {
            tr.seek(true);
        }
        assert_eq!(tr.cursor(), Nanos(5_000), "clamped at end");
    }
}
