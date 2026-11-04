//! Turns fractional motion into whole pixels without losing the remainder.

use crate::vec2::Vec2;

/// Largest remainder kept back, which is also the largest position error.
const MAX_REMAINDER: f64 = 0.5;

#[derive(Clone, Copy, Debug, Default)]
pub struct SubpixelAccumulator {
    remainder: Vec2,
}

impl SubpixelAccumulator {
    /// Adds motion and returns the whole pixels that are ready to emit.
    pub fn push(&mut self, motion: Vec2) -> (i32, i32) {
        self.remainder += motion;
        (
            take_whole(&mut self.remainder.x),
            take_whole(&mut self.remainder.y),
        )
    }

    pub fn remainder(&self) -> Vec2 {
        self.remainder
    }
}

/// A remainder of exactly one half stays put. Rounding it would emit a pixel
/// and leave minus one half, which would then emit a pixel back, forever.
fn take_whole(value: &mut f64) -> i32 {
    if value.abs() <= MAX_REMAINDER {
        return 0;
    }
    let whole = value.round();
    *value -= whole;
    whole as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_numbers_pass_unchanged() {
        let mut accumulator = SubpixelAccumulator::default();
        assert_eq!(accumulator.push(Vec2::new(3.0, -7.0)), (3, -7));
        assert_eq!(accumulator.remainder(), Vec2::ZERO);
    }

    #[test]
    fn fractions_add_up_to_pixels() {
        let mut accumulator = SubpixelAccumulator::default();
        let total: i32 = (0..8)
            .map(|_| accumulator.push(Vec2::new(0.25, 0.0)).0)
            .sum();
        assert_eq!(total, 2);
    }

    #[test]
    fn does_not_drift_over_a_long_run() {
        let mut accumulator = SubpixelAccumulator::default();
        let (mut emitted_x, mut emitted_y) = (0i64, 0i64);
        let (mut true_x, mut true_y) = (0.0, 0.0);
        for i in 0..1_000_000 {
            let motion = Vec2::new(0.37 + (i % 7) as f64 * 0.01, -0.73);
            true_x += motion.x;
            true_y += motion.y;
            let (x, y) = accumulator.push(motion);
            emitted_x += x as i64;
            emitted_y += y as i64;
            assert!(accumulator.remainder().x.abs() <= MAX_REMAINDER);
        }
        assert!((emitted_x as f64 - true_x).abs() <= 0.5 + 1e-6);
        assert!((emitted_y as f64 - true_y).abs() <= 0.5 + 1e-6);
    }

    #[test]
    fn exactly_one_half_does_not_oscillate() {
        let mut accumulator = SubpixelAccumulator::default();
        accumulator.push(Vec2::new(0.5, -0.5));
        for _ in 0..4 {
            assert_eq!(accumulator.push(Vec2::ZERO), (0, 0));
        }
    }

    #[test]
    fn opposite_motion_cancels_out() {
        let mut accumulator = SubpixelAccumulator::default();
        assert_eq!(accumulator.push(Vec2::new(0.4, 0.0)), (0, 0));
        assert_eq!(accumulator.push(Vec2::new(-0.4, 0.0)), (0, 0));
        assert_eq!(accumulator.remainder(), Vec2::ZERO);
    }
}
