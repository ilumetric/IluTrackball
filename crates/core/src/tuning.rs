//! Works out settings from what a person did with the ball.
//!
//! The test is a row of dots to reach and click, and each kind of dot answers
//! one question:
//!
//! - Ordinary dots give ordinary motion, which shows the rhythm of the link and
//!   how rough the motion is. It is played through a row of chains, the way
//!   `ilumx-replay` does it. Smoothness only ever costs delay and how much of
//!   it is worth paying is a matter of taste, so that price is a choice and not
//!   a constant.
//! - Small dots close together show the smallest correction the hand makes and
//!   how fast careful work goes. That correction should move the cursor by
//!   about one pixel of the interface, and no less: the cursor gets no finer
//!   than the hand, only slower.
//! - Far dots bring out flicks, which show how far and how fast the ball goes
//!   when the cursor should fly. They are played through the real pipeline until
//!   a flick travels the wanted distance, so the answer holds for this curve and
//!   not for an approximation.
//!
//! The result is a place to start from. It is a preset like any other.

use crate::curve::Curve;
use crate::debeat::DebeatConfig;
use crate::event_log::RawEvent;
use crate::metrics::{measure, Metrics};
use crate::pipeline::{PipelineConfig, StageConfig, SPEED_TIME_CONSTANT_S};
use crate::simulate::simulate;
use crate::vec2::Vec2;

/// Release times of the double step filter tried on a recording.
const RELEASE_TRIALS_MS: [f64; 3] = [30.0, 60.0, 120.0];
/// Smoothing times tried on a recording. Zero stands for no smoothing.
const SMOOTHING_TRIALS_MS: [f64; 6] = [0.0, 2.0, 4.0, 8.0, 12.0, 20.0];
/// Fewer double steps a second than this are noise, not a rhythm.
const MIN_DOUBLES_PER_S: f64 = 0.3;
/// A longer gap between packets means the ball was not moving.
const MOVING_GAP_US: u64 = 30_000;

/// The last stretch before a click, where the hand corrects.
const APPROACH_US: u64 = 700_000;
/// A pause this long separates two corrections.
const NUDGE_GAP_US: u64 = 60_000;
/// Share of the corrections taken as "the smallest the hand does". The very
/// smallest ones are a resting finger, not a decision.
const NUDGE_SHARE: f64 = 0.25;
const MIN_NUDGES: usize = 5;
const USUAL_NUDGE_COUNTS: f64 = 4.0;
/// The top is low on purpose. A hand that glides up to the target without a
/// pause leaves no small runs, and a long run says nothing about its limits.
const NUDGE_RANGE: (f64, f64) = (1.5, 6.0);
/// Pixels per count stay in this range whatever was measured.
const FINE_GAIN_RANGE: (f64, f64) = (0.1, 1.5);
/// The range of the overall speed in the window.
const FACTOR_RANGE: (f64, f64) = (0.1, 2.0);

/// Careful work is taken to go on below this share of its speeds.
const FINE_SPEED_SHARE: f64 = 0.9;
const USUAL_FINE_SPEED: f64 = 1.0;
const FINE_SPEED_RANGE: (f64, f64) = (0.3, 2.0);

/// A longer pause ends a flick.
const FLICK_GAP_US: u64 = 120_000;
/// A flick travels at least this far, in counts.
const MIN_FLICK_COUNTS: f64 = 80.0;
/// And reaches this speed, in counts per millisecond.
const MIN_FLICK_SPEED: f64 = 1.5;
/// The curve has reached its top at this share of a flick's top speed. A flick
/// spends most of its way below the peak.
const FULL_GAIN_SHARE: f64 = 0.6;
const USUAL_FLICK_SPEED: f64 = 3.0;
const USUAL_FLICK_COUNTS: f64 = 315.0;
/// The curve needs room between careful work and flying.
const MIN_SPEED_RATIO: f64 = 1.6;
const MAX_FULL_GAIN_SPEED: f64 = 6.0;

/// The highest gain the curve may reach, in multiples of its slow end. Beyond
/// this the speeds in between turn so touchy that the hand cannot use them.
const MAX_CURVE_GAIN: f64 = 16.0;
const SOLVER_ROUNDS: usize = 24;

/// How much delay smoothness may cost.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Smoothness {
    Quick,
    #[default]
    Balanced,
    Smooth,
}

impl Smoothness {
    /// One millisecond of added delay has to buy this share of the roughness.
    /// On the recorded ball most of the gain comes within the first few
    /// milliseconds, and after that a millisecond buys about 1%.
    fn lag_price(self) -> f64 {
        match self {
            Smoothness::Quick => 0.03,
            Smoothness::Balanced => 0.015,
            Smoothness::Smooth => 0.005,
        }
    }
}

/// What the exercises recorded. All packets of the one mouse.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Exercises {
    /// The way to the ordinary dots.
    pub ordinary: Vec<RawEvent>,
    /// The way to the small dots.
    pub precise: Vec<RawEvent>,
    /// When each small dot was hit, on the clock of `precise`.
    pub hits: Vec<u64>,
    /// The way to the far dots.
    pub flicks: Vec<RawEvent>,
}

/// What stands between a count and the screen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Display {
    /// What Windows makes of one count.
    pub pixels_per_count: f64,
    /// Pixels in one point of the interface: 1.5 at 150%.
    pub scale: f64,
    pub refresh_hz: f64,
}

impl Default for Display {
    fn default() -> Self {
        Self {
            pixels_per_count: 1.0,
            scale: 1.0,
            refresh_hz: 60.0,
        }
    }
}

/// What the exercises showed.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Measured {
    /// Double steps a second in the bare motion.
    pub doubles_per_s: f64,
    /// Roughness of the bare motion between screen frames.
    pub roughness: f64,
    /// The smallest correction of the hand.
    pub nudge_counts: f64,
    /// Up to this speed goes careful work, in counts per millisecond.
    pub fine_speed: f64,
    /// A typical flick.
    pub flick_counts: f64,
    pub flick_speed: f64,
    pub flicks_used: usize,
}

/// One chain and what it did to the recording.
#[derive(Clone, Debug, PartialEq)]
struct Trial {
    stages: Vec<StageConfig>,
    added_lag_ms: f64,
    roughness: f64,
    doubles_per_s: f64,
}

/// Settings and what they are expected to do.
#[derive(Clone, Debug, PartialEq)]
pub struct Setup {
    pub config: PipelineConfig,
    pub added_lag_ms: f64,
    pub roughness: f64,
    pub doubles_per_s: f64,
    /// How far a typical flick goes, in pixels.
    pub reach_px: f64,
}

/// Measurements and the settings that can come out of them.
#[derive(Clone, Debug, PartialEq)]
pub struct Tuning {
    pub measured: Measured,
    trials: Vec<Trial>,
    factor: f64,
    pixels_per_count: f64,
    full_gain_speed: f64,
    flicks: Vec<Vec<RawEvent>>,
}

impl Tuning {
    pub fn new(exercises: &Exercises, display: &Display) -> Self {
        let (bare, trials) = try_chains(&exercises.ordinary, display.refresh_hz);
        let nudge_counts = smallest_nudge(&exercises.precise, &exercises.hits);
        let fine_speed = careful_speed(&exercises.precise, &exercises.hits);
        let flicks = find_flicks(&exercises.flicks);
        let flick_speed = median_of(flicks.iter().map(|flick| flick.peak_speed));
        let flick_counts = median_of(flicks.iter().map(|flick| flick.counts));

        let fine_gain = (display.scale / nudge_counts).clamp(FINE_GAIN_RANGE.0, FINE_GAIN_RANGE.1);
        let top = if flicks.is_empty() {
            USUAL_FLICK_SPEED
        } else {
            flick_speed
        };
        let full_gain_speed = (top * FULL_GAIN_SHARE)
            .max(fine_speed * MIN_SPEED_RATIO)
            .min(MAX_FULL_GAIN_SPEED);

        Self {
            measured: Measured {
                doubles_per_s: bare.doubles_per_s,
                roughness: bare.roughness,
                nudge_counts,
                fine_speed,
                flick_counts,
                flick_speed,
                flicks_used: flicks.len(),
            },
            trials: std::iter::once(bare).chain(trials).collect(),
            factor: (fine_gain / display.pixels_per_count.max(f64::EPSILON))
                .clamp(FACTOR_RANGE.0, FACTOR_RANGE.1),
            pixels_per_count: display.pixels_per_count,
            full_gain_speed,
            flicks: flicks.into_iter().map(|flick| flick.events).collect(),
        }
    }

    /// The settings that pay `smoothness` and make a typical flick travel
    /// `goal_px` pixels, or as close to that as the curve goes.
    pub fn settle(&self, goal_px: f64, smoothness: Smoothness) -> Setup {
        let trial = self.choose(smoothness);
        let config = self.config_for(trial, goal_px);
        Setup {
            reach_px: self.reach_px(&config),
            added_lag_ms: trial.added_lag_ms,
            roughness: trial.roughness,
            doubles_per_s: trial.doubles_per_s,
            config,
        }
    }

    /// The first trial is the bare motion, which costs nothing and so stays
    /// unless a chain is worth its price.
    fn choose(&self, smoothness: Smoothness) -> &Trial {
        let bare = self.measured.roughness.max(f64::EPSILON);
        let cost = |trial: &Trial| {
            trial.roughness / bare + trial.added_lag_ms * smoothness.lag_price()
        };
        self.trials
            .iter()
            .min_by(|a, b| cost(a).total_cmp(&cost(b)))
            .expect("the bare motion is always there")
    }

    fn config_for(&self, trial: &Trial, goal_px: f64) -> PipelineConfig {
        if self.flicks.is_empty() {
            return self.config_with(trial, self.usual_top(goal_px));
        }
        let (mut low, mut high) = (1.0f64, MAX_CURVE_GAIN);
        if self.reach_px(&self.config_with(trial, high)) <= goal_px {
            return self.config_with(trial, high);
        }
        if self.reach_px(&self.config_with(trial, low)) >= goal_px {
            return self.config_with(trial, low);
        }
        for _ in 0..SOLVER_ROUNDS {
            let middle = (low * high).sqrt();
            if self.reach_px(&self.config_with(trial, middle)) < goal_px {
                low = middle;
            } else {
                high = middle;
            }
        }
        self.config_with(trial, (low * high).sqrt())
    }

    /// How far a typical flick goes with these settings, in pixels.
    fn reach_px(&self, config: &PipelineConfig) -> f64 {
        if self.flicks.is_empty() {
            let top = config.curve.gain(self.full_gain_speed);
            return USUAL_FLICK_COUNTS * config.factor * top * self.pixels_per_count;
        }
        let reaches: Vec<f64> = self
            .flicks
            .iter()
            .map(|flick| {
                let total = simulate(flick, config)
                    .iter()
                    .fold(Vec2::ZERO, |sum, event| {
                        sum + Vec2::new(event.dx as f64, event.dy as f64)
                    });
                total.length() * self.pixels_per_count
            })
            .collect();
        median_of(reaches.into_iter())
    }

    /// Without flicks to play, the gain comes from a flick of the usual size.
    fn usual_top(&self, goal_px: f64) -> f64 {
        let pixels_per_count = self.factor * self.pixels_per_count;
        (goal_px / USUAL_FLICK_COUNTS / pixels_per_count).clamp(1.0, MAX_CURVE_GAIN)
    }

    fn config_with(&self, trial: &Trial, top_gain: f64) -> PipelineConfig {
        PipelineConfig {
            stages: trial.stages.clone(),
            factor: round_to(self.factor, 1000.0),
            curve: Curve::Table(vec![
                (0.0, 1.0),
                (round_to(self.measured.fine_speed, 20.0), 1.0),
                (round_to(self.full_gain_speed, 20.0), round_to(top_gain, 100.0)),
            ]),
            ..PipelineConfig::default()
        }
    }
}

/// Rounds to a whole number of `1 / per_unit` steps. Dividing by the whole
/// number gives the shortest decimal, which is what a preset file should hold.
fn round_to(value: f64, per_unit: f64) -> f64 {
    (value * per_unit).round() / per_unit
}

fn percentile(values: &mut [f64], share: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(f64::total_cmp);
    values[((values.len() - 1) as f64 * share).round() as usize]
}

fn median_of(values: impl Iterator<Item = f64>) -> f64 {
    let mut values: Vec<f64> = values.collect();
    percentile(&mut values, 0.5)
}

/// Time the ball spent moving. The pauses at the dots say nothing about how
/// often double steps come.
fn moving_s(events: &[RawEvent]) -> f64 {
    let moving_us: u64 = events
        .windows(2)
        .map(|pair| pair[1].t_us.saturating_sub(pair[0].t_us))
        .filter(|gap| *gap < MOVING_GAP_US)
        .sum();
    moving_us as f64 / 1e6
}

/// Plays the recording through a row of chains. Returns the bare motion and
/// what each chain made of it.
fn try_chains(motion: &[RawEvent], refresh_hz: f64) -> (Trial, Vec<Trial>) {
    let seconds = moving_s(motion);
    if seconds <= 0.0 {
        let bare = Trial {
            stages: Vec::new(),
            added_lag_ms: 0.0,
            roughness: 0.0,
            doubles_per_s: 0.0,
        };
        return (bare, Vec::new());
    }
    let play = |stages: &[StageConfig]| -> Metrics {
        let config = PipelineConfig {
            stages: stages.to_vec(),
            ..PipelineConfig::default()
        };
        measure(motion, &simulate(motion, &config), refresh_hz)
    };
    let trial = |stages: Vec<StageConfig>, metrics: Metrics, base_lag_ms: f64| Trial {
        stages,
        added_lag_ms: (metrics.lag_ms - base_lag_ms).max(0.0),
        roughness: metrics.frame_roughness,
        doubles_per_s: metrics.double_steps as f64 / seconds,
    };

    let bare_metrics = play(&[]);
    let bare = trial(Vec::new(), bare_metrics, bare_metrics.lag_ms);
    let trials = chains(bare.doubles_per_s >= MIN_DOUBLES_PER_S)
        .into_iter()
        .map(|stages| {
            let metrics = play(&stages);
            trial(stages, metrics, bare_metrics.lag_ms)
        })
        .collect();
    (bare, trials)
}

/// The chains worth trying. The double step filter comes first in all of them,
/// or not at all if the motion has no double steps.
fn chains(has_doubles: bool) -> Vec<Vec<StageConfig>> {
    let smoothing = |tau_ms: f64| (tau_ms > 0.0).then_some(StageConfig::Ema { tau_ms });
    if !has_doubles {
        return SMOOTHING_TRIALS_MS
            .iter()
            .filter_map(|&tau_ms| smoothing(tau_ms))
            .map(|stage| vec![stage])
            .collect();
    }
    let mut chains = Vec::new();
    for release_ms in RELEASE_TRIALS_MS {
        let debeat = StageConfig::Debeat(DebeatConfig {
            release_s: release_ms / 1000.0,
            ..DebeatConfig::default()
        });
        for tau_ms in SMOOTHING_TRIALS_MS {
            let mut chain = vec![debeat];
            chain.extend(smoothing(tau_ms));
            chains.push(chain);
        }
    }
    chains
}

/// The speed the curve sees at each packet, in counts per millisecond. It is
/// read right after a packet has added to it, so at a steady pace it runs about
/// a quarter above the true average.
fn speeds(events: &[RawEvent]) -> Vec<f64> {
    let mut speed = 0.0;
    let mut last_us = None;
    events
        .iter()
        .filter(|event| event.is_motion())
        .map(|event| {
            let elapsed_s = last_us.map_or(0.0, |last| event.t_us.saturating_sub(last) as f64 / 1e6);
            last_us = Some(event.t_us);
            speed *= (-elapsed_s / SPEED_TIME_CONSTANT_S).exp();
            speed += distance(event) / SPEED_TIME_CONSTANT_S;
            speed / 1000.0
        })
        .collect()
}

fn distance(event: &RawEvent) -> f64 {
    Vec2::new(event.dx as f64, event.dy as f64).length()
}

fn approach(events: &[RawEvent], hit_us: u64) -> &[RawEvent] {
    let from = events.partition_point(|event| event.t_us + APPROACH_US < hit_us);
    let to = events.partition_point(|event| event.t_us <= hit_us);
    &events[from..to]
}

/// The size of each correction before the clicks, in counts.
fn nudges(precise: &[RawEvent], hits: &[u64]) -> Vec<f64> {
    hits.iter()
        .flat_map(|&hit_us| {
            approach(precise, hit_us)
                .chunk_by(|a, b| b.t_us.saturating_sub(a.t_us) < NUDGE_GAP_US)
                .map(|run| {
                    run.iter().fold(Vec2::ZERO, |sum, event| {
                        sum + Vec2::new(event.dx as f64, event.dy as f64)
                    })
                })
                .map(|motion| motion.length())
                .filter(|size| *size > 0.0)
                .collect::<Vec<f64>>()
        })
        .collect()
}

fn smallest_nudge(precise: &[RawEvent], hits: &[u64]) -> f64 {
    let mut sizes = nudges(precise, hits);
    if sizes.len() < MIN_NUDGES {
        return USUAL_NUDGE_COUNTS;
    }
    percentile(&mut sizes, NUDGE_SHARE).clamp(NUDGE_RANGE.0, NUDGE_RANGE.1)
}

fn careful_speed(precise: &[RawEvent], hits: &[u64]) -> f64 {
    let mut all: Vec<f64> = hits
        .iter()
        .flat_map(|&hit_us| speeds(approach(precise, hit_us)))
        .collect();
    if all.is_empty() {
        return USUAL_FINE_SPEED;
    }
    percentile(&mut all, FINE_SPEED_SHARE).clamp(FINE_SPEED_RANGE.0, FINE_SPEED_RANGE.1)
}

/// One flick of the ball.
#[derive(Clone, Debug, PartialEq)]
struct Flick {
    events: Vec<RawEvent>,
    counts: f64,
    peak_speed: f64,
}

/// Picks the flicks out of a recording. What is too small or too slow to be
/// one, such as the cursor being brought back, does not count.
fn find_flicks(events: &[RawEvent]) -> Vec<Flick> {
    let motion: Vec<RawEvent> = events.iter().filter(|e| e.is_motion()).copied().collect();
    motion
        .chunk_by(|a, b| b.t_us.saturating_sub(a.t_us) < FLICK_GAP_US)
        .filter_map(|run| {
            let total = run.iter().fold(Vec2::ZERO, |sum, event| {
                sum + Vec2::new(event.dx as f64, event.dy as f64)
            });
            let peak_speed = speeds(run).into_iter().fold(0.0, f64::max);
            let is_flick = total.length() >= MIN_FLICK_COUNTS && peak_speed >= MIN_FLICK_SPEED;
            is_flick.then(|| Flick {
                events: run.to_vec(),
                counts: total.length(),
                peak_speed,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_log::read_csv;
    use std::path::Path;

    fn motion(t_us: u64, dx: i32) -> RawEvent {
        RawEvent {
            t_us,
            dx,
            ..Default::default()
        }
    }

    /// An even ball, doubling every twelfth packet if asked to.
    fn circles(doubling: bool) -> Vec<RawEvent> {
        (1..1100)
            .map(|i| {
                let doubled = doubling && i % 12 == 0;
                motion(i * 8_750, if doubled { 10 } else { 5 })
            })
            .collect()
    }

    /// Fast at the middle, the way a real flick goes.
    fn flick(start_us: u64, peak: i32) -> Vec<RawEvent> {
        (0..24)
            .map(|i: i32| {
                let hump = 12 - (i - 12).abs();
                motion(start_us + i as u64 * 8_750, 1 + peak * hump / 12)
            })
            .collect()
    }

    fn flicks(count: u64) -> Vec<RawEvent> {
        (0..count)
            .flat_map(|i| flick(1_000_000 * (i + 1), 30))
            .collect()
    }

    /// Small corrections with pauses before every hit.
    fn careful_clicks(nudge: i32) -> (Vec<RawEvent>, Vec<u64>) {
        let mut events = Vec::new();
        let mut hits = Vec::new();
        for target in 0..10u64 {
            let start = 1_000_000 + target * 2_000_000;
            for step in 0..6u64 {
                let at = start + step * 100_000;
                events.push(motion(at, nudge));
                events.push(motion(at + 8_750, 0));
            }
            hits.push(start + 600_000);
        }
        (events, hits)
    }

    fn display() -> Display {
        Display {
            pixels_per_count: 1.5,
            scale: 1.5,
            refresh_hz: 60.0,
        }
    }

    fn exercises() -> Exercises {
        let (precise, hits) = careful_clicks(4);
        Exercises {
            ordinary: circles(true),
            precise,
            hits,
            flicks: flicks(6),
        }
    }

    fn curve_points(config: &PipelineConfig) -> Vec<(f64, f64)> {
        match &config.curve {
            Curve::Table(points) => points.clone(),
            other => panic!("not a table: {other:?}"),
        }
    }

    fn tuning() -> Tuning {
        Tuning::new(&exercises(), &display())
    }

    fn ordinary_only(ordinary: Vec<RawEvent>) -> Tuning {
        let exercises = Exercises {
            ordinary,
            ..Exercises::default()
        };
        Tuning::new(&exercises, &display())
    }

    fn recorded_ball() -> Tuning {
        ordinary_only(read_csv(Path::new("tests/data/circle.csv")).unwrap())
    }

    #[test]
    fn double_steps_get_the_double_step_filter() {
        let tuning = ordinary_only(circles(true));
        let setup = tuning.settle(1500.0, Smoothness::Balanced);
        let first = setup.config.stages.first();
        assert!(matches!(first, Some(StageConfig::Debeat(_))), "{first:?}");
        assert!(tuning.measured.doubles_per_s > 5.0, "{:?}", tuning.measured);
        assert!(setup.roughness < tuning.measured.roughness);
    }

    #[test]
    fn a_smooth_wired_mouse_is_left_alone() {
        let wired: Vec<RawEvent> = (1..12_000).map(|i| motion(i * 1_000, 1)).collect();
        let setup = ordinary_only(wired).settle(1500.0, Smoothness::Balanced);
        assert!(setup.config.stages.is_empty(), "{:?}", setup.config.stages);
        assert_eq!(setup.added_lag_ms, 0.0);
    }

    #[test]
    fn pauses_at_the_dots_do_not_thin_out_the_double_steps() {
        // The same motion in two parts, with a long stop at a dot between them.
        let mut parted = circles(true);
        let middle = parted.len() / 2;
        for event in &mut parted[middle..] {
            event.t_us += 30_000_000;
        }
        let together = ordinary_only(circles(true)).measured.doubles_per_s;
        let apart = ordinary_only(parted).measured.doubles_per_s;
        assert!((apart - together).abs() < together * 0.1, "{apart} {together}");
    }

    #[test]
    fn no_recording_means_no_filter() {
        let setup = Tuning::new(&Exercises::default(), &display())
            .settle(1500.0, Smoothness::Smooth);
        assert!(setup.config.stages.is_empty());
    }

    #[test]
    fn the_recorded_ball_gets_its_double_steps_removed() {
        let tuning = recorded_ball();
        let setup = tuning.settle(1500.0, Smoothness::Balanced);
        let first = setup.config.stages.first();
        assert!(matches!(first, Some(StageConfig::Debeat(_))), "{first:?}");
        assert!(tuning.measured.doubles_per_s > 1.0, "{:?}", tuning.measured);
        assert!(setup.doubles_per_s < tuning.measured.doubles_per_s / 2.0, "{setup:?}");
        assert!(setup.roughness < tuning.measured.roughness * 0.8, "{setup:?}");
        assert!(setup.added_lag_ms < 12.0, "{setup:?}");
    }

    #[test]
    fn paying_more_delay_buys_more_smoothness() {
        let tuning = recorded_ball();
        let [quick, balanced, smooth] = [Smoothness::Quick, Smoothness::Balanced, Smoothness::Smooth]
            .map(|smoothness| tuning.settle(1500.0, smoothness));
        assert!(quick.added_lag_ms <= balanced.added_lag_ms);
        assert!(balanced.added_lag_ms < smooth.added_lag_ms);
        assert!(smooth.roughness < balanced.roughness);
        assert!(balanced.roughness <= quick.roughness);
    }

    #[test]
    fn the_smallest_correction_sets_the_slow_end() {
        let (precise, hits) = careful_clicks(4);
        assert_eq!(smallest_nudge(&precise, &hits), 4.0);
        // Four counts make one point of 1.5 pixels, and Windows adds 1.5 by itself.
        let setup = tuning().settle(1000.0, Smoothness::Balanced);
        assert!((setup.config.factor - 0.25).abs() < 1e-9, "{}", setup.config.factor);
    }

    #[test]
    fn a_shaking_hand_is_not_taken_for_precision() {
        let (precise, hits) = careful_clicks(1);
        assert_eq!(smallest_nudge(&precise, &hits), NUDGE_RANGE.0);
    }

    #[test]
    fn a_long_glide_is_not_taken_for_a_correction() {
        // One unbroken run up to each target.
        let (precise, hits) = careful_clicks(20);
        assert_eq!(smallest_nudge(&precise, &hits), NUDGE_RANGE.1);
    }

    #[test]
    fn a_fast_pointer_in_windows_keeps_the_factor_in_its_range() {
        let display = Display {
            pixels_per_count: 2.0,
            ..display()
        };
        let exercises = Exercises {
            precise: careful_clicks(6).0,
            hits: careful_clicks(6).1,
            ..Exercises::default()
        };
        let factor = Tuning::new(&exercises, &display)
            .settle(1000.0, Smoothness::Balanced)
            .config
            .factor;
        assert!((factor - 0.125).abs() < 1e-9, "{factor}");
        let very_fast = Display {
            pixels_per_count: 20.0,
            ..display
        };
        let factor = Tuning::new(&exercises, &very_fast)
            .settle(1000.0, Smoothness::Balanced)
            .config
            .factor;
        assert_eq!(factor, FACTOR_RANGE.0);
    }

    #[test]
    fn without_corrections_the_usual_size_stands_in() {
        assert_eq!(smallest_nudge(&[], &[]), USUAL_NUDGE_COUNTS);
    }

    #[test]
    fn the_speed_of_careful_work_stays_within_reason() {
        let (precise, hits) = careful_clicks(4);
        let speed = careful_speed(&precise, &hits);
        assert!((FINE_SPEED_RANGE.0..=FINE_SPEED_RANGE.1).contains(&speed), "{speed}");
    }

    #[test]
    fn slow_motion_is_no_flick_and_neither_is_a_nudge() {
        let slow: Vec<RawEvent> = (0..100).map(|i| motion(i * 8_750, 1)).collect();
        assert!(find_flicks(&slow).is_empty());
        let short = vec![motion(0, 40), motion(8_750, 30)];
        assert!(find_flicks(&short).is_empty());
    }

    #[test]
    fn flicks_are_found_one_by_one() {
        let found = find_flicks(&flicks(6));
        assert_eq!(found.len(), 6);
        assert!(found[0].peak_speed > MIN_FLICK_SPEED);
    }

    #[test]
    fn the_recorded_ball_makes_real_flicks() {
        let fast = read_csv(Path::new("tests/data/fast.csv")).unwrap();
        let found = find_flicks(&fast);
        assert!(found.len() >= 3, "{}", found.len());
        let speed = median_of(found.iter().map(|flick| flick.peak_speed));
        assert!(speed > 2.0, "{speed}");
    }

    #[test]
    fn a_flick_travels_the_wanted_distance() {
        let tuning = tuning();
        for goal in [300.0, 500.0, 800.0] {
            let reach = tuning.settle(goal, Smoothness::Balanced).reach_px;
            assert!((reach - goal).abs() < goal * 0.03, "{goal}: {reach}");
        }
    }

    #[test]
    fn a_further_goal_needs_a_higher_curve() {
        let tuning = tuning();
        let top = |goal| curve_points(&tuning.settle(goal, Smoothness::Balanced).config)[2].1;
        assert!(top(1400.0) > top(800.0));
    }

    #[test]
    fn the_curve_never_leaves_its_range() {
        let tuning = tuning();
        let top = |goal| curve_points(&tuning.settle(goal, Smoothness::Balanced).config)[2].1;
        assert_eq!(top(1e7), MAX_CURVE_GAIN);
        assert_eq!(top(1.0), 1.0);
    }

    #[test]
    fn the_curve_stays_flat_for_careful_work() {
        let tuning = tuning();
        let config = tuning.settle(1400.0, Smoothness::Balanced).config;
        let points = curve_points(&config);
        assert_eq!(points[0].1, 1.0);
        assert_eq!(points[1].1, 1.0);
        assert!(points[1].0 < points[2].0);
        assert!(config.curve.gain(tuning.measured.fine_speed * 0.9) <= 1.0 + 1e-9);
    }

    #[test]
    fn a_skipped_exercise_still_gives_usable_settings() {
        let setup = Tuning::new(&Exercises::default(), &display())
            .settle(1200.0, Smoothness::Balanced);
        assert!(setup.config.stages.is_empty());
        assert!(setup.config.factor > 0.0);
        let points = curve_points(&setup.config);
        assert!(points[0].0 < points[1].0 && points[1].0 < points[2].0, "{points:?}");
        assert!((setup.reach_px - 1200.0).abs() < 30.0, "{}", setup.reach_px);
    }

    #[test]
    fn the_speed_matches_what_the_pipeline_feeds_its_curve() {
        // 9 counts every 8.75 ms: the curve reads 9 / 0.02 / (1 - exp(-0.4375)).
        let events: Vec<RawEvent> = (0..200).map(|i| motion(i * 8_750, 9)).collect();
        let last = *speeds(&events).last().unwrap();
        assert!((last - 1.27).abs() < 0.01, "{last}");
    }

    #[test]
    fn settings_survive_as_text() {
        let config = tuning().settle(1400.0, Smoothness::Smooth).config;
        let text = crate::spec::format_spec(&config);
        assert_eq!(crate::spec::parse_spec(&text).unwrap(), config);
    }
}
