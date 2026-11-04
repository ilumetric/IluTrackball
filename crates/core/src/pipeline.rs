//! The whole processing chain: stages, then the gain, then whole pixels.
//!
//! A ticker keeps calling `tick` while motion is held back, so a smoothing
//! stage can finish its tail after the ball has stopped.

use crate::accumulator::SubpixelAccumulator;
use crate::curve::Curve;
use crate::debeat::{Debeat, DebeatConfig};
use crate::stages::{Ema, OneEuro, Spread, Stage};
use crate::vec2::Vec2;

/// Below this much held motion the ticker may go to sleep.
const SETTLED_COUNTS: f64 = 0.5;
/// Averaging time of the speed that drives the curve.
pub const SPEED_TIME_CONSTANT_S: f64 = 0.02;

pub const DEFAULT_TAIL_TIMEOUT_MS: f64 = 200.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StageConfig {
    Ema { tau_ms: f64 },
    OneEuro {
        min_cutoff_hz: f64,
        beta: f64,
        d_cutoff_hz: f64,
    },
    Debeat(DebeatConfig),
    Spread { window_ms: f64 },
}

impl StageConfig {
    fn build(&self) -> Box<dyn Stage> {
        match *self {
            StageConfig::Ema { tau_ms } => Box::new(Ema::new(tau_ms / 1000.0)),
            StageConfig::OneEuro {
                min_cutoff_hz,
                beta,
                d_cutoff_hz,
            } => Box::new(OneEuro::new(min_cutoff_hz, beta, d_cutoff_hz)),
            StageConfig::Debeat(config) => Box::new(Debeat::new(config)),
            StageConfig::Spread { window_ms } => Box::new(Spread::new(window_ms / 1000.0)),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PipelineConfig {
    pub stages: Vec<StageConfig>,
    /// Constant gain, below 1 for precision work.
    pub factor: f64,
    pub curve: Curve,
    /// Held motion is flushed this long after the last packet.
    pub tail_timeout_ms: f64,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            stages: Vec::new(),
            factor: 1.0,
            curve: Curve::Flat,
            tail_timeout_ms: DEFAULT_TAIL_TIMEOUT_MS,
        }
    }
}

pub struct Pipeline {
    stages: Vec<Box<dyn Stage>>,
    factor: f64,
    curve: Curve,
    tail_timeout_us: u64,
    /// Speed of the outgoing motion, in counts per second.
    speed: f64,
    accumulator: SubpixelAccumulator,
    last_t_us: Option<u64>,
    last_packet_us: u64,
}

impl Pipeline {
    pub fn new(config: &PipelineConfig) -> Self {
        Self {
            stages: config.stages.iter().map(StageConfig::build).collect(),
            factor: config.factor,
            curve: config.curve.clone(),
            tail_timeout_us: (config.tail_timeout_ms * 1000.0) as u64,
            speed: 0.0,
            accumulator: SubpixelAccumulator::default(),
            last_t_us: None,
            last_packet_us: 0,
        }
    }

    /// Takes a packet of motion and returns the pixels to emit now.
    pub fn packet(&mut self, t_us: u64, dx: i32, dy: i32) -> (i32, i32) {
        let dt_s = self.elapsed_s(t_us);
        // Time passes first, so a stage cannot release the new packet using
        // the possibly long pause before it.
        let before = self.run(dt_s, Vec2::ZERO);
        let after = self.run(0.0, Vec2::new(dx as f64, dy as f64));
        self.last_packet_us = t_us;
        self.emit(dt_s, before + after)
    }

    /// Advances time and returns the pixels to emit now.
    pub fn tick(&mut self, t_us: u64) -> (i32, i32) {
        let dt_s = self.elapsed_s(t_us);
        let mut flow = self.run(dt_s, Vec2::ZERO);
        if t_us.saturating_sub(self.last_packet_us) >= self.tail_timeout_us {
            for stage in &mut self.stages {
                flow += stage.flush();
            }
        }
        self.emit(dt_s, flow)
    }

    /// Releases all held motion at once, for example before a change of mode.
    pub fn flush(&mut self, t_us: u64) -> (i32, i32) {
        let dt_s = self.elapsed_s(t_us);
        let mut flow = self.run(dt_s, Vec2::ZERO);
        for stage in &mut self.stages {
            flow += stage.flush();
        }
        self.emit(dt_s, flow)
    }

    /// True when the ticker has nothing left to do.
    pub fn is_settled(&self) -> bool {
        self.held().length() < SETTLED_COUNTS
    }

    /// Motion taken in and not let out yet.
    pub fn held(&self) -> Vec2 {
        self.stages
            .iter()
            .fold(Vec2::ZERO, |total, stage| total + stage.held())
    }

    fn elapsed_s(&mut self, t_us: u64) -> f64 {
        let elapsed = self.last_t_us.map_or(0, |last| t_us.saturating_sub(last));
        self.last_t_us = Some(t_us);
        elapsed as f64 / 1e6
    }

    fn run(&mut self, dt_s: f64, input: Vec2) -> Vec2 {
        let mut flow = input;
        for stage in &mut self.stages {
            let passed = if flow == Vec2::ZERO {
                Vec2::ZERO
            } else {
                stage.input(flow)
            };
            flow = passed + stage.advance(dt_s);
        }
        flow
    }

    fn emit(&mut self, dt_s: f64, flow: Vec2) -> (i32, i32) {
        self.speed *= (-dt_s / SPEED_TIME_CONSTANT_S).exp();
        self.speed += flow.length() / SPEED_TIME_CONSTANT_S;
        let gain = self.curve.gain(self.speed / 1000.0) * self.factor;
        self.accumulator.push(flow * gain)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TICK_US: u64 = 1_000;
    const PACKET_US: u64 = 8_000;

    fn config(stages: Vec<StageConfig>) -> PipelineConfig {
        PipelineConfig {
            stages,
            ..PipelineConfig::default()
        }
    }

    /// Plays packets with a ticker running and returns all pixels emitted, the
    /// time of the last emitted pixel and whether the tail ever settled.
    fn play(pipeline: &mut Pipeline, packets: &[(i32, i32)]) -> ((i64, i64), u64) {
        let mut total = (0i64, 0i64);
        let mut last_motion = 0;
        let mut add = |t: u64, out: (i32, i32)| {
            total.0 += out.0 as i64;
            total.1 += out.1 as i64;
            if out != (0, 0) {
                last_motion = t;
            }
        };

        let mut t = 0;
        for &(dx, dy) in packets {
            t += PACKET_US;
            add(t, pipeline.packet(t, dx, dy));
            for step in 1..PACKET_US / TICK_US {
                add(t + step * TICK_US, pipeline.tick(t + step * TICK_US));
            }
        }
        t += PACKET_US - TICK_US;
        while !pipeline.is_settled() {
            t += TICK_US;
            add(t, pipeline.tick(t));
            assert!(t < 60_000_000, "tail never settled");
        }
        (total, last_motion)
    }

    #[test]
    fn passthrough_is_bit_exact_and_needs_no_ticker() {
        let mut pipeline = Pipeline::new(&PipelineConfig::default());
        let packets = [(3, -1), (0, 0), (-120, 7), (1, 1), (2047, -2048)];
        for (i, &(dx, dy)) in packets.iter().enumerate() {
            let t = i as u64 * PACKET_US;
            assert_eq!(pipeline.packet(t, dx, dy), (dx, dy));
            assert_eq!(pipeline.tick(t + TICK_US), (0, 0));
            assert!(pipeline.is_settled());
        }
    }

    #[test]
    fn smoothing_keeps_every_count() {
        let mut pipeline = Pipeline::new(&config(vec![StageConfig::Ema { tau_ms: 10.0 }]));
        let packets: Vec<(i32, i32)> = (0..500).map(|i| (3, (i % 3) - 1)).collect();
        let (total, _) = play(&mut pipeline, &packets);
        assert!((total.0 - 1500).abs() <= 1, "{total:?}");
        assert!(total.1.abs() <= 1, "{total:?}");
    }

    #[test]
    fn the_tail_converges_after_the_last_packet() {
        let mut pipeline = Pipeline::new(&config(vec![StageConfig::Ema { tau_ms: 20.0 }]));
        let packets = [(10, 0); 20];
        let (total, last_motion) = play(&mut pipeline, &packets);
        let end_of_input = 20 * PACKET_US;
        assert!(last_motion > end_of_input, "there was no tail");
        assert!(last_motion < end_of_input + 150_000, "{last_motion}");
        assert!((total.0 - 200).abs() <= 1);
    }

    #[test]
    fn the_tail_timeout_cuts_a_slow_tail_short() {
        let mut pipeline = Pipeline::new(&PipelineConfig {
            tail_timeout_ms: 50.0,
            ..config(vec![StageConfig::Ema { tau_ms: 10_000.0 }])
        });
        let packets = [(10, 0); 5];
        let (total, last_motion) = play(&mut pipeline, &packets);
        assert_eq!(total.0, 50);
        assert!(last_motion <= 5 * PACKET_US + 51_000, "{last_motion}");
    }

    #[test]
    fn a_long_pause_does_not_let_a_packet_skip_smoothing() {
        let mut pipeline = Pipeline::new(&config(vec![StageConfig::Ema { tau_ms: 50.0 }]));
        pipeline.packet(0, 1, 0);
        let (x, _) = pipeline.packet(10_000_000, 100, 0);
        assert!(x < 5, "the packet went straight through: {x}");
    }

    #[test]
    fn flush_lets_go_of_everything() {
        let mut pipeline = Pipeline::new(&config(vec![StageConfig::Ema { tau_ms: 10_000.0 }]));
        pipeline.packet(0, 40, -8);
        assert_eq!(pipeline.flush(1_000), (40, -8));
        assert!(pipeline.is_settled());
    }

    #[test]
    fn stages_can_be_chained() {
        let mut pipeline = Pipeline::new(&config(vec![
            StageConfig::Debeat(DebeatConfig::default()),
            StageConfig::Ema { tau_ms: 5.0 },
        ]));
        let mut packets = vec![(4, 0); 40];
        packets[20] = (8, 0);
        let (total, _) = play(&mut pipeline, &packets);
        assert!((total.0 - 164).abs() <= 1, "{total:?}");
    }

    #[test]
    fn precision_divides_motion_without_drift() {
        let mut pipeline = Pipeline::new(&PipelineConfig {
            factor: 0.25,
            ..PipelineConfig::default()
        });
        let (total, _) = play(&mut pipeline, &vec![(1, -1); 4001]);
        assert!((total.0 - 1000).abs() <= 1, "{total:?}");
        assert!((total.1 + 1000).abs() <= 1, "{total:?}");
    }

    #[test]
    fn the_curve_speeds_up_fast_motion_only() {
        let curve = Curve::Linear {
            base: 1.0,
            slope: 0.5,
            cap: 3.0,
        };
        let accelerated = |dx| {
            let mut pipeline = Pipeline::new(&PipelineConfig {
                curve: curve.clone(),
                ..PipelineConfig::default()
            });
            let (total, _) = play(&mut pipeline, &[(dx, 0); 200]);
            total.0 as f64 / (dx * 200) as f64
        };
        let slow_gain = accelerated(1);
        let fast_gain = accelerated(40);
        assert!(slow_gain < 1.2, "{slow_gain}");
        assert!(fast_gain > 2.5, "{fast_gain}");
    }
}
