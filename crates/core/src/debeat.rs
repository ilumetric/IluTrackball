//! Evens out packets that carry two sensor frames.
//!
//! Over Bluetooth the sensor and the radio tick at slightly different rates, so
//! at a steady beat one packet carries two frames and the cursor jumps a double
//! step. The beat comes from two clocks and is stable, so the next doubled
//! packet can be predicted. Packet sizes alone are too noisy for that.

use crate::stages::{release_share, Stage};
use crate::vec2::Vec2;

/// Tuning of the `Debeat` stage.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DebeatConfig {
    /// Without a beat expected, a packet this many times the typical size
    /// counts as doubled.
    pub ratio: f64,
    /// When a beat is expected, this much is enough.
    pub window_ratio: f64,
    /// A larger packet is a real change of speed and passes untouched.
    pub max_ratio: f64,
    /// Time constant for releasing the held excess.
    pub release_s: f64,
    /// Starting estimate of the beat period. It adapts to what is observed.
    pub period_s: f64,
}

impl Default for DebeatConfig {
    fn default() -> Self {
        Self {
            ratio: 1.75,
            window_ratio: 1.4,
            max_ratio: 3.5,
            release_s: 0.04,
            period_s: 0.102,
        }
    }
}

/// Below this typical size a doubled packet is lost in rounding.
const MIN_TYPICAL: f64 = 1.0;
/// Recent packets the typical size is taken from.
const HISTORY: usize = 5;
/// Fewer known packets than this say nothing about the typical size.
const MIN_HISTORY: usize = 3;
/// A doubled packet stands out from the one before it too, unlike a ramp.
const MIN_RISE: f64 = 1.3;
/// Doubled packets are about a dozen packets apart, so a closer one means the
/// ball is really speeding up.
const MIN_SPACING: u32 = 8;
/// After this long without packets the history says nothing.
const STALE_S: f64 = 0.03;

/// How far from the predicted time a beat may land. Beats land on the packet
/// grid, so a packet interval of slack is needed on each side.
const BEAT_WINDOW_S: f64 = 0.0105;
/// How many beats the prediction may run on without a new observation.
const MAX_PREDICTED_BEATS: f64 = 6.0;
/// How quickly the period follows observed beats.
const PERIOD_GAIN: f64 = 0.05;

/// Predicts when the next doubled packet is due.
#[derive(Clone, Copy, Debug)]
struct BeatClock {
    period_s: f64,
    last_beat_s: Option<f64>,
}

impl BeatClock {
    fn new(period_s: f64) -> Self {
        Self {
            period_s,
            last_beat_s: None,
        }
    }

    /// Number of whole beats between the last observation and `now_s`.
    fn beats_since_last(&self, now_s: f64) -> Option<f64> {
        let elapsed = now_s - self.last_beat_s?;
        let beats = (elapsed / self.period_s).round();
        (1.0..=MAX_PREDICTED_BEATS).contains(&beats).then_some(beats)
    }

    fn expects_beat(&self, now_s: f64) -> bool {
        self.beats_since_last(now_s).is_some_and(|beats| {
            let predicted = self.last_beat_s.unwrap_or(0.0) + beats * self.period_s;
            (now_s - predicted).abs() <= BEAT_WINDOW_S
        })
    }

    fn observe(&mut self, now_s: f64) {
        if self.expects_beat(now_s) {
            if let (Some(beats), Some(last)) = (self.beats_since_last(now_s), self.last_beat_s) {
                let measured = (now_s - last) / beats;
                self.period_s += (measured - self.period_s) * PERIOD_GAIN;
            }
        }
        self.last_beat_s = Some(now_s);
    }
}

/// A doubled packet is cut down to the typical size and the excess is released
/// over the next packets. Ordinary packets pass at once. It needs whole packets
/// as input and therefore has to come first in a chain.
pub struct Debeat {
    config: DebeatConfig,
    clock: BeatClock,
    now_s: f64,
    /// Latest packet sizes, oldest overwritten first.
    sizes: [f64; HISTORY],
    known_sizes: usize,
    next_size: usize,
    last_motion: Vec2,
    packets_since_hold: u32,
    quiet_s: f64,
    held: Vec2,
}

impl Debeat {
    pub fn new(config: DebeatConfig) -> Self {
        Self {
            config,
            clock: BeatClock::new(config.period_s),
            now_s: 0.0,
            sizes: [0.0; HISTORY],
            known_sizes: 0,
            next_size: 0,
            last_motion: Vec2::ZERO,
            packets_since_hold: MIN_SPACING,
            quiet_s: 0.0,
            held: Vec2::ZERO,
        }
    }

    /// Median of the recent packet sizes. A median ignores earlier doubled
    /// packets and does not fall behind while the ball speeds up.
    fn typical_size(&self) -> Option<f64> {
        if self.known_sizes < MIN_HISTORY {
            return None;
        }
        let mut sizes = self.sizes;
        let known = &mut sizes[..self.known_sizes];
        known.sort_by(f64::total_cmp);
        Some(known[known.len() / 2])
    }

    fn remember(&mut self, size: f64) {
        self.sizes[self.next_size] = size;
        self.next_size = (self.next_size + 1) % HISTORY;
        self.known_sizes = (self.known_sizes + 1).min(HISTORY);
    }

    fn is_doubled(&self, motion: Vec2, typical: f64) -> bool {
        let size = motion.length();
        let needed = if self.clock.expects_beat(self.now_s) {
            self.config.window_ratio
        } else {
            self.config.ratio
        };
        typical >= MIN_TYPICAL
            && size >= needed * typical
            && size <= self.config.max_ratio * typical
            && size >= MIN_RISE * self.last_motion.length()
            && motion.dot(self.last_motion) > 0.0
            && self.packets_since_hold >= MIN_SPACING
    }
}

impl Stage for Debeat {
    fn input(&mut self, motion: Vec2) -> Vec2 {
        self.quiet_s = 0.0;
        self.packets_since_hold = self.packets_since_hold.saturating_add(1);
        let typical = self.typical_size().filter(|&t| self.is_doubled(motion, t));
        self.last_motion = motion;
        let Some(typical) = typical else {
            self.remember(motion.length());
            return motion;
        };

        self.clock.observe(self.now_s);
        self.packets_since_hold = 0;
        self.remember(typical);
        let passed = motion * (typical / motion.length());
        self.held += motion - passed;
        passed
    }

    fn advance(&mut self, dt_s: f64) -> Vec2 {
        self.now_s += dt_s;
        self.quiet_s += dt_s;
        if self.quiet_s >= STALE_S {
            self.known_sizes = 0;
        }
        let released = self.held * release_share(dt_s, self.config.release_s);
        self.held -= released;
        released
    }

    fn held(&self) -> Vec2 {
        self.held
    }

    fn flush(&mut self) -> Vec2 {
        std::mem::take(&mut self.held)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PACKET_S: f64 = 0.00875;

    fn debeat() -> Debeat {
        Debeat::new(DebeatConfig::default())
    }

    /// Plays one packet the way the pipeline does: time first, then motion.
    fn play(stage: &mut Debeat, x: f64) -> Vec2 {
        stage.advance(PACKET_S);
        stage.input(Vec2::new(x, 0.0))
    }

    fn feed(stage: &mut Debeat, x: f64, packets: usize) {
        for _ in 0..packets {
            play(stage, x);
        }
    }

    #[test]
    fn cuts_a_doubled_packet_and_gives_the_excess_back() {
        let mut stage = debeat();
        feed(&mut stage, 4.0, 12);
        let passed = play(&mut stage, 8.0);
        assert!((passed.x - 4.0).abs() < 1e-6, "{passed:?}");
        assert!((stage.held().x - 4.0).abs() < 1e-6);

        let released = stage.advance(1.0);
        assert!((released.x - 4.0).abs() < 1e-3, "{released:?}");
    }

    #[test]
    fn passes_ordinary_packets_at_once() {
        let mut stage = debeat();
        for x in [3.0, 4.0, 3.0, 4.0, 3.0, 3.0, 4.0] {
            assert_eq!(play(&mut stage, x), Vec2::new(x, 0.0));
        }
        assert_eq!(stage.held(), Vec2::ZERO);
    }

    #[test]
    fn leaves_a_flick_alone() {
        let mut stage = debeat();
        feed(&mut stage, 3.0, 12);
        assert_eq!(play(&mut stage, 30.0).x, 30.0);
    }

    #[test]
    fn holds_only_once_while_speeding_up() {
        let mut stage = debeat();
        feed(&mut stage, 4.0, 12);
        assert!(play(&mut stage, 8.0).x < 8.0);
        assert_eq!(play(&mut stage, 8.0).x, 8.0);
    }

    #[test]
    fn ignores_a_reversal() {
        let mut stage = debeat();
        feed(&mut stage, 4.0, 12);
        assert_eq!(play(&mut stage, -8.0).x, -8.0);
    }

    #[test]
    fn forgets_the_typical_size_after_a_stop() {
        let mut stage = debeat();
        feed(&mut stage, 4.0, 12);
        stage.advance(0.1);
        assert_eq!(stage.input(Vec2::new(8.0, 0.0)).x, 8.0);
    }

    #[test]
    fn does_not_mistake_a_ramp_for_a_doubled_packet() {
        let mut stage = debeat();
        for x in [2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0] {
            assert_eq!(play(&mut stage, x).x, x);
        }
    }

    #[test]
    fn needs_some_history_before_acting() {
        let mut stage = debeat();
        assert_eq!(play(&mut stage, 8.0).x, 8.0);
    }

    #[test]
    fn a_weak_rise_only_counts_when_a_beat_is_due() {
        let mut stage = debeat();
        feed(&mut stage, 4.0, 12);
        play(&mut stage, 8.0);
        // Six packets later no beat is due, so 1.5x is an ordinary wobble.
        feed(&mut stage, 4.0, 5);
        assert_eq!(play(&mut stage, 6.0).x, 6.0);
    }

    #[test]
    fn a_weak_rise_is_cut_when_a_beat_is_due() {
        let mut stage = debeat();
        feed(&mut stage, 4.0, 12);
        play(&mut stage, 8.0);
        feed(&mut stage, 4.0, 10);
        // Packet 11 lands 96 ms later, inside the window around 102 ms.
        assert!(play(&mut stage, 6.0).x < 6.0);
    }

    #[test]
    fn the_clock_predicts_several_beats_ahead() {
        let mut clock = BeatClock::new(0.1);
        clock.observe(1.0);
        assert!(!clock.expects_beat(1.05));
        assert!(clock.expects_beat(1.1));
        assert!(clock.expects_beat(1.305));
        assert!(!clock.expects_beat(1.25));
        assert!(!clock.expects_beat(2.0));
    }

    #[test]
    fn the_clock_adapts_its_period() {
        let mut clock = BeatClock::new(0.1);
        let mut now = 0.0;
        clock.observe(now);
        for _ in 0..200 {
            now += 0.105;
            clock.observe(now);
        }
        assert!((clock.period_s - 0.105).abs() < 1e-4, "{}", clock.period_s);
    }

    #[test]
    fn an_off_beat_spike_moves_the_clock() {
        let mut clock = BeatClock::new(0.1);
        clock.observe(0.0);
        clock.observe(0.15);
        assert_eq!(clock.period_s, 0.1);
        assert!(clock.expects_beat(0.25));
    }
}
