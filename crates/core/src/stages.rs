//! Motion processing stages.
//!
//! A stage works on the flow of motion rather than on positions: motion comes
//! in, part of it may be held back, and it is let out as time passes. Nothing
//! is ever created or lost, so a stage can only shift motion in time.

use std::collections::VecDeque;
use std::f64::consts::TAU;

use crate::vec2::Vec2;

pub trait Stage: Send {
    /// Takes in motion and returns the part that passes at once.
    fn input(&mut self, motion: Vec2) -> Vec2;
    /// Lets `dt_s` seconds pass and returns the motion released meanwhile.
    fn advance(&mut self, dt_s: f64) -> Vec2;
    /// Motion that has come in and not gone out yet.
    fn held(&self) -> Vec2;
    /// Releases everything still held.
    fn flush(&mut self) -> Vec2;
}

/// Share of held motion released during `dt_s` by a low-pass filter.
pub(crate) fn release_share(dt_s: f64, time_constant_s: f64) -> f64 {
    1.0 - (-dt_s / time_constant_s).exp()
}

fn take(held: &mut Vec2) -> Vec2 {
    std::mem::take(held)
}

/// Exponential smoothing with a fixed time constant.
pub struct Ema {
    time_constant_s: f64,
    held: Vec2,
}

impl Ema {
    pub fn new(time_constant_s: f64) -> Self {
        Self {
            time_constant_s,
            held: Vec2::ZERO,
        }
    }
}

impl Stage for Ema {
    fn input(&mut self, motion: Vec2) -> Vec2 {
        self.held += motion;
        Vec2::ZERO
    }

    fn advance(&mut self, dt_s: f64) -> Vec2 {
        let released = self.held * release_share(dt_s, self.time_constant_s);
        self.held -= released;
        released
    }

    fn held(&self) -> Vec2 {
        self.held
    }

    fn flush(&mut self) -> Vec2 {
        take(&mut self.held)
    }
}

/// One Euro filter: heavy smoothing when slow, little lag when fast.
///
/// The cutoff follows the speed of the whole vector, so both axes are smoothed
/// alike and the direction of motion is kept.
pub struct OneEuro {
    min_cutoff_hz: f64,
    beta: f64,
    speed_cutoff_hz: f64,
    speed: f64,
    distance_since_advance: f64,
    held: Vec2,
}

impl OneEuro {
    pub fn new(min_cutoff_hz: f64, beta: f64, speed_cutoff_hz: f64) -> Self {
        Self {
            min_cutoff_hz,
            beta,
            speed_cutoff_hz,
            speed: 0.0,
            distance_since_advance: 0.0,
            held: Vec2::ZERO,
        }
    }

    fn cutoff_hz(&self) -> f64 {
        self.min_cutoff_hz + self.beta * self.speed
    }
}

impl Stage for OneEuro {
    fn input(&mut self, motion: Vec2) -> Vec2 {
        self.held += motion;
        self.distance_since_advance += motion.length();
        Vec2::ZERO
    }

    fn advance(&mut self, dt_s: f64) -> Vec2 {
        if dt_s <= 0.0 {
            return Vec2::ZERO;
        }
        let raw_speed = self.distance_since_advance / dt_s;
        self.distance_since_advance = 0.0;
        self.speed += (raw_speed - self.speed) * release_share(dt_s, 1.0 / (TAU * self.speed_cutoff_hz));

        let released = self.held * release_share(dt_s, 1.0 / (TAU * self.cutoff_hz()));
        self.held -= released;
        released
    }

    fn held(&self) -> Vec2 {
        self.held
    }

    fn flush(&mut self) -> Vec2 {
        take(&mut self.held)
    }
}

/// Spreads each packet evenly over a fixed window.
///
/// Packets arrive at about 114 Hz, so a faster screen shows the cursor standing
/// still on some frames and catching up on others. Spreading a packet over one
/// packet interval turns those jumps into small even steps, at the cost of half
/// the window in lag.
pub struct Spread {
    window_s: f64,
    /// Rate of motion and remaining time of every packet still being spread.
    pieces: VecDeque<(Vec2, f64)>,
}

impl Spread {
    pub fn new(window_s: f64) -> Self {
        Self {
            window_s,
            pieces: VecDeque::new(),
        }
    }
}

impl Stage for Spread {
    fn input(&mut self, motion: Vec2) -> Vec2 {
        self.pieces
            .push_back((motion * (1.0 / self.window_s), self.window_s));
        Vec2::ZERO
    }

    fn advance(&mut self, dt_s: f64) -> Vec2 {
        let mut released = Vec2::ZERO;
        for (rate, left_s) in &mut self.pieces {
            let spent = dt_s.min(*left_s);
            released += *rate * spent;
            *left_s -= spent;
        }
        self.pieces.retain(|(_, left_s)| *left_s > 0.0);
        released
    }

    fn held(&self) -> Vec2 {
        self.pieces
            .iter()
            .fold(Vec2::ZERO, |total, &(rate, left_s)| total + rate * left_s)
    }

    fn flush(&mut self) -> Vec2 {
        let held = self.held();
        self.pieces.clear();
        held
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STEP_S: f64 = 0.001;

    fn run_out(stage: &mut dyn Stage, seconds: f64) -> Vec2 {
        let mut total = Vec2::ZERO;
        for _ in 0..(seconds / STEP_S) as usize {
            total += stage.advance(STEP_S);
        }
        total
    }

    /// Feeds equal packets and returns the motion that came out in between.
    fn feed(stage: &mut dyn Stage, motion: Vec2, packets: usize, gap_s: f64) -> Vec2 {
        let mut out = Vec2::ZERO;
        for _ in 0..packets {
            out += stage.input(motion);
            out += run_out(stage, gap_s);
        }
        out
    }

    #[test]
    fn ema_releases_everything_in_the_end() {
        let mut ema = Ema::new(0.01);
        assert_eq!(ema.input(Vec2::new(10.0, -4.0)), Vec2::ZERO);
        let out = run_out(&mut ema, 0.2);
        assert!((out.x - 10.0).abs() < 1e-6);
        assert!((out.y + 4.0).abs() < 1e-6);
    }

    #[test]
    fn ema_time_constant_sets_the_pace() {
        let mut ema = Ema::new(0.01);
        ema.input(Vec2::new(100.0, 0.0));
        let out = run_out(&mut ema, 0.01);
        assert!((out.x - 63.2).abs() < 0.5, "{out:?}");
    }

    #[test]
    fn flush_returns_what_is_held() {
        let mut ema = Ema::new(10.0);
        ema.input(Vec2::new(5.0, 0.0));
        assert_eq!(ema.flush(), Vec2::new(5.0, 0.0));
        assert_eq!(ema.held(), Vec2::ZERO);
    }

    #[test]
    fn one_euro_lags_less_when_fast() {
        let mut slow = OneEuro::new(1.0, 0.02, 1.0);
        let mut fast = OneEuro::new(1.0, 0.02, 1.0);
        feed(&mut slow, Vec2::new(1.0, 0.0), 100, 0.008);
        feed(&mut fast, Vec2::new(20.0, 0.0), 100, 0.008);
        let slow_lag = slow.held().length() / (1.0 / 0.008);
        let fast_lag = fast.held().length() / (20.0 / 0.008);
        assert!(fast_lag < slow_lag / 2.0, "{fast_lag} vs {slow_lag}");
    }

    #[test]
    fn one_euro_keeps_the_direction() {
        let mut filter = OneEuro::new(1.0, 0.02, 1.0);
        let out = feed(&mut filter, Vec2::new(3.0, 4.0), 50, 0.008);
        assert!((out.y / out.x - 4.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn spread_shares_a_packet_evenly_over_its_window() {
        let mut spread = Spread::new(0.008);
        assert_eq!(spread.input(Vec2::new(8.0, 0.0)), Vec2::ZERO);
        for _ in 0..8 {
            let out = spread.advance(0.001);
            assert!((out.x - 1.0).abs() < 1e-9, "{out:?}");
        }
        assert_eq!(spread.advance(0.001), Vec2::ZERO);
        assert_eq!(spread.held(), Vec2::ZERO);
    }

    #[test]
    fn spread_keeps_overlapping_packets_apart() {
        let mut spread = Spread::new(0.008);
        spread.input(Vec2::new(8.0, 0.0));
        spread.advance(0.004);
        spread.input(Vec2::new(8.0, 0.0));
        assert!((spread.held().x - 12.0).abs() < 1e-9);
        assert!((spread.flush().x - 12.0).abs() < 1e-9);
        assert_eq!(spread.held(), Vec2::ZERO);
    }
}
