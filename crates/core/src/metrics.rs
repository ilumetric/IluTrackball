//! Compares a filtered log with the original.

use crate::event_log::RawEvent;
use crate::spikes::{find_spikes, neighbour_count, neighbour_median};
use crate::vec2::Vec2;

/// A longer gap between input packets starts a new run for the size checks.
const RUN_GAP_US: u64 = 30_000;
/// Packets smaller than this say little about smoothness.
const MIN_TYPICAL_COUNTS: f64 = 1.5;
/// Position grid of the lag measurement.
const LAG_STEP_US: u64 = 1_000;
/// Half the window the speed is taken over in the lag measurement.
const SPEED_REACH_US: u64 = 25_000;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Metrics {
    /// Average delay of the output behind the input.
    pub lag_ms: f64,
    /// Average deviation of steps from their neighbours, relative to the steps.
    pub roughness: f64,
    /// Steps about twice the size of their neighbours.
    pub double_steps: usize,
    /// The same as `roughness`, for steps between frames of the monitor.
    pub frame_roughness: f64,
    /// Furthest the output ever was from the input, in pixels.
    pub max_error_px: f64,
    /// Difference in total travel once everything has played out, in pixels.
    pub end_error_px: f64,
}

pub fn measure(input: &[RawEvent], output: &[RawEvent], refresh_hz: f64) -> Metrics {
    let in_track = Track::new(input);
    let out_track = Track::new(output);

    let mut scan_runs = 0;
    let mut packet_spread = Spread::default();
    let mut frame_spread = Spread::default();
    for run in input.chunk_by(|a, b| b.t_us.saturating_sub(a.t_us) < RUN_GAP_US) {
        let sizes = step_sizes(run, &out_track);
        scan_runs += find_spikes(&sizes).spikes.len();
        packet_spread.add(&sizes);
        frame_spread.add(&frame_steps(run, &out_track, refresh_hz));
    }

    let end = input
        .last()
        .map_or(0, |e| e.t_us)
        .max(output.last().map_or(0, |e| e.t_us));
    Metrics {
        lag_ms: average_lag_ms(&in_track, &out_track, end),
        roughness: packet_spread.relative(),
        double_steps: scan_runs,
        frame_roughness: frame_spread.relative(),
        max_error_px: max_error(&in_track, &out_track, end),
        end_error_px: (in_track.at(end) - out_track.at(end)).length(),
    }
}

/// Deviation of steps from their neighbours, summed over several runs.
#[derive(Default)]
struct Spread {
    deviation: f64,
    travel: f64,
}

impl Spread {
    fn add(&mut self, sizes: &[f64]) {
        let reach = neighbour_count();
        if sizes.len() <= reach * 2 {
            return;
        }
        for i in reach..sizes.len() - reach {
            let typical = neighbour_median(sizes, i);
            if typical >= MIN_TYPICAL_COUNTS {
                self.deviation += (sizes[i] - typical).abs();
                self.travel += typical;
            }
        }
    }

    fn relative(&self) -> f64 {
        if self.travel > 0.0 {
            self.deviation / self.travel
        } else {
            0.0
        }
    }
}

/// How far the output moved between frames of the monitor during a run.
fn frame_steps(run: &[RawEvent], output: &Track, refresh_hz: f64) -> Vec<f64> {
    let (Some(first), Some(last)) = (run.first(), run.last()) else {
        return Vec::new();
    };
    let frame_us = 1e6 / refresh_hz;
    let frames = ((last.t_us - first.t_us) as f64 / frame_us) as u64;
    (0..frames)
        .map(|frame| {
            let time = |n: u64| first.t_us + (n as f64 * frame_us) as u64;
            (output.at(time(frame + 1)) - output.at(time(frame))).length()
        })
        .collect()
}

/// How far the output moved between consecutive input packets.
fn step_sizes(run: &[RawEvent], output: &Track) -> Vec<f64> {
    run.windows(2)
        .map(|pair| (output.at(pair[1].t_us) - output.at(pair[0].t_us)).length())
        .collect()
}

fn max_error(input: &Track, output: &Track, end_us: u64) -> f64 {
    (0..=end_us)
        .step_by(LAG_STEP_US as usize)
        .map(|t| (input.at(t) - output.at(t)).length())
        .fold(0.0, f64::max)
}

/// Fits `input - output = velocity * lag` over the whole log. Comparing shifted
/// copies instead would reward a staircase for lining up with itself and miss
/// motion that is held back between its steps.
fn average_lag_ms(input: &Track, output: &Track, end_us: u64) -> f64 {
    let window_s = (2 * SPEED_REACH_US) as f64 / 1e6;
    let (mut trailing, mut speed_squared) = (0.0, 0.0);
    for t in (SPEED_REACH_US..end_us.saturating_sub(SPEED_REACH_US)).step_by(LAG_STEP_US as usize) {
        let velocity = (input.at(t + SPEED_REACH_US) - input.at(t - SPEED_REACH_US)) * (1.0 / window_s);
        trailing += (input.at(t) - output.at(t)).dot(velocity);
        speed_squared += velocity.dot(velocity);
    }
    if speed_squared > 0.0 {
        trailing / speed_squared * 1000.0
    } else {
        0.0
    }
}

/// Cumulative position over time.
pub struct Track {
    times: Vec<u64>,
    positions: Vec<Vec2>,
}

impl Track {
    pub fn new(events: &[RawEvent]) -> Self {
        let mut position = Vec2::ZERO;
        let mut times = Vec::with_capacity(events.len());
        let mut positions = Vec::with_capacity(events.len());
        for event in events {
            position += Vec2::new(event.dx as f64, event.dy as f64);
            times.push(event.t_us);
            positions.push(position);
        }
        Self { times, positions }
    }

    /// Position once every event up to `t_us` has happened.
    pub fn at(&self, t_us: u64) -> Vec2 {
        let count = self.times.partition_point(|&t| t <= t_us);
        count
            .checked_sub(1)
            .map_or(Vec2::ZERO, |last| self.positions[last])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simulate::simulate;
    use crate::pipeline::PipelineConfig;
    use crate::spec::parse_spec;

    fn motion(t_us: u64, dx: i32) -> RawEvent {
        RawEvent {
            t_us,
            dx,
            ..Default::default()
        }
    }

    /// A ball at a steady speed that doubles every twelfth packet.
    fn doubled_motion() -> Vec<RawEvent> {
        (1..1200)
            .map(|i| motion(i * 8_750, if i % 12 == 0 { 8 } else { 4 }))
            .collect()
    }

    #[test]
    fn identical_logs_measure_as_identical() {
        let events = doubled_motion();
        let metrics = measure(&events, &events, 60.0);
        assert_eq!(metrics.lag_ms, 0.0);
        assert_eq!(metrics.max_error_px, 0.0);
        assert_eq!(metrics.end_error_px, 0.0);
        assert!(metrics.double_steps > 80);
    }

    #[test]
    fn finds_a_known_delay() {
        let events = doubled_motion();
        let delayed: Vec<RawEvent> = events
            .iter()
            .map(|e| motion(e.t_us + 20_000, e.dx))
            .collect();
        let lag = measure(&events, &delayed, 60.0).lag_ms;
        assert!((lag - 20.0).abs() < 1.0, "{lag}");
    }

    #[test]
    fn counts_motion_held_back_between_steps_as_lag() {
        // Same staircase, always two steps behind: 8 counts at 457 counts/s.
        let events: Vec<RawEvent> = (1..1200).map(|i| motion(i * 8_750, 4)).collect();
        let behind: Vec<RawEvent> = events[2..]
            .iter()
            .map(|e| motion(e.t_us - 2 * 8_750 + 8_750 * 2, e.dx))
            .collect();
        let lag = measure(&events, &behind, 60.0).lag_ms;
        assert!((lag - 17.5).abs() < 1.5, "{lag}");
    }

    #[test]
    fn debeat_removes_double_steps_with_little_lag() {
        let events = doubled_motion();
        let config = parse_spec("debeat").unwrap();
        let raw = measure(&events, &simulate(&events, &PipelineConfig::default()), 60.0);
        let filtered = measure(&events, &simulate(&events, &config), 60.0);
        assert!(filtered.double_steps < raw.double_steps / 10, "{filtered:?}");
        assert!(filtered.roughness < raw.roughness, "{filtered:?}");
        assert!(filtered.lag_ms <= 6.0, "{filtered:?}");
    }

    #[test]
    fn spreading_evens_out_steps_on_a_fast_screen() {
        // Packets at 114 Hz on a 240 Hz screen leave many frames without motion.
        let events: Vec<RawEvent> = (1..1200).map(|i| motion(i * 8_750, 6)).collect();
        let raw = measure(&events, &events, 240.0);
        let config = parse_spec("spread").unwrap();
        let spread = measure(&events, &simulate(&events, &config), 240.0);
        assert!(spread.frame_roughness < raw.frame_roughness / 2.0, "{spread:?}");
    }

    #[test]
    fn track_reports_the_position_at_any_time() {
        let track = Track::new(&[motion(10, 2), motion(20, 3)]);
        assert_eq!(track.at(5), Vec2::ZERO);
        assert_eq!(track.at(10), Vec2::new(2.0, 0.0));
        assert_eq!(track.at(15), Vec2::new(2.0, 0.0));
        assert_eq!(track.at(99), Vec2::new(5.0, 0.0));
    }

    #[test]
    fn empty_logs_are_harmless() {
        assert_eq!(measure(&[], &[], 60.0), Metrics::default());
    }
}
