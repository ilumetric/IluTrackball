//! Works out which cause of stepping shows up in the recorded logs.
//!
//! The causes look alike on screen, so each gets its own test:
//! - Polling is judged only where the ball moves fast. The sensor has data
//!   for every report there, so a gap can only come from the link.
//! - Quantization is judged on slow packets and on how many pixels one sensor
//!   count becomes.
//! - Stick-slip is a pause followed by a burst in the same direction. A link
//!   dropout looks the same at low speed, but the link fails at any speed while
//!   the ball only sticks when slow. So pauses seen during fast motion are the
//!   link's share and get subtracted from the slow ones.
//! - Double frames are packets twice the size of their neighbours, in a steady
//!   rhythm. Packet timing stays perfect, so only the sizes give them away.

use std::collections::BTreeMap;

use ilumx_core::event_log::RawEvent;
pub use ilumx_core::pointer::PointerSettings;
use ilumx_core::spikes::find_spikes;

/// Width of the window used to estimate the local speed.
const SPEED_WINDOW_US: u64 = 200_000;
/// Below this speed a packet counts as slow motion, in counts per second.
/// Motion recorded as "slow" on the real device runs at 200 to 550.
const SLOW_MAX_CPS: f64 = 600.0;
/// Above this speed the sensor has fresh data for every report.
const FAST_MIN_CPS: f64 = 400.0;
/// Both packets around a timing interval must be at least this long, so the
/// ball is really moving at that moment rather than slowing down to reverse.
const FAST_MIN_PACKET_COUNTS: f64 = 2.0;
/// A longer gap between packets means the user stopped.
const BURST_GAP_US: u64 = 100_000;
/// A longer pause is the user stopping, not the ball sticking.
const MAX_STICK_PAUSE_US: u64 = 500_000;
/// Time after a pause in which the released motion is collected.
const JUMP_WINDOW_US: u64 = 30_000;
/// How much faster than before the pause the motion must be to be a jump.
const JUMP_SPEED_RATIO: f64 = 2.0;
/// Intervals below this are packets bunched into one radio event.
const BUNCHED_INTERVAL_US: u64 = 1_000;

const MIN_TIMING_INTERVALS: usize = 100;
const MIN_SLOW_PACKETS: usize = 100;
const MIN_SLOW_SECONDS: f64 = 5.0;
/// Fewer stick-slip events than this can just be flicks of the thumb.
const MIN_STICK_SLIPS: usize = 3;
/// Fewer spikes than this cannot show a rhythm.
const MIN_DOUBLE_FRAMES: usize = 20;

/// A longer gap between packets starts a new run for the spike scan.
const RUN_GAP_US: u64 = 30_000;
/// A packet after a gap this many intervals long made up for a missed report.
const MISSED_REPORT_INTERVALS: f64 = 1.5;
/// A rhythm faster than this would be an uneven frame pattern, not single
/// doubled packets, so closer spikes are noise.
const MIN_RHYTHM_PACKETS: u32 = 8;
/// Spacings this close to the most common one define the rhythm.
const RHYTHM_WINDOW_PACKETS: f64 = 1.5;
/// How far a spacing may be from a multiple of the period to fit the rhythm.
const RHYTHM_TOLERANCE_PACKETS: f64 = 1.2;
/// Doubled packets are twice the size; allow for rounding and acceleration.
const DOUBLE_RATIO_RANGE: std::ops::RangeInclusive<f64> = 1.6..=2.8;
/// How much of the gap between chance and a perfect rhythm scores 1.0.
const RHYTHM_STRONG_EXCESS: f64 = 0.6;

/// Share of slow packets at one count from which quantization is plausible.
const UNIT_SHARE_THRESHOLD: f64 = 0.7;
/// Each of these values scores 1.0, which is the `Strong` level.
const STRONG_MISS_SHARE: f64 = 0.05;
const STRONG_BUNCHED_SHARE: f64 = 0.40;
const STRONG_PAUSES_PER_10S: f64 = 4.0;

/// Tunable thresholds of the stick-slip pattern.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Thresholds {
    /// Minimum pause before a jump.
    pub pause_ms: u64,
    /// Minimum size of a jump in sensor counts.
    pub jump_counts: f64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            pause_ms: 40,
            jump_counts: 4.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AxisHistogram {
    pub zero: usize,
    pub one: usize,
    pub two: usize,
    pub three_plus: usize,
}

impl AxisHistogram {
    fn add(&mut self, delta: i32) {
        match delta.unsigned_abs() {
            0 => self.zero += 1,
            1 => self.one += 1,
            2 => self.two += 1,
            _ => self.three_plus += 1,
        }
    }

    fn merge(&mut self, other: &Self) {
        self.zero += other.zero;
        self.one += other.one;
        self.two += other.two;
        self.three_plus += other.three_plus;
    }

    pub fn total(&self) -> usize {
        self.zero + self.one + self.two + self.three_plus
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StickSlip {
    pub t_us: u64,
    pub pause_ms: f64,
    pub jump_counts: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScenarioAnalysis {
    pub name: String,
    pub duration_s: f64,
    pub motion_packets: usize,
    /// Intervals between packets while the ball moved fast.
    pub fast_intervals_us: Vec<u64>,
    /// How many of those intervals are as long as a stick-slip pause.
    pub fast_pauses: usize,
    pub slow_packets: usize,
    pub slow_active_s: f64,
    pub slow_median_cps: Option<f64>,
    pub slow_dx: AxisHistogram,
    pub slow_dy: AxisHistogram,
    /// Slow packets whose larger axis delta is exactly one count.
    pub slow_unit_packets: usize,
    pub stick_slips: Vec<StickSlip>,
    /// Median interval between consecutive packets.
    pub link_interval_us: Option<u64>,
    /// Packets where a doubled packet would be detectable.
    pub spike_checked: usize,
    pub spike_ratios: Vec<f64>,
    /// Distance between neighbouring spikes, in link intervals.
    pub spike_spacings: Vec<f64>,
}

/// Packets that carry twice the motion of their neighbours in a steady rhythm.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DoubleFrames {
    pub spikes: usize,
    pub checked_packets: usize,
    pub median_ratio: f64,
    pub link_interval_us: f64,
    pub period_packets: f64,
    /// Share of spike spacings that fit the rhythm.
    pub regular_share: f64,
}

impl DoubleFrames {
    pub fn period_ms(&self) -> f64 {
        self.period_packets * self.link_interval_us / 1000.0
    }

    pub fn rate_hz(&self) -> f64 {
        1000.0 / self.period_ms()
    }

    pub fn link_hz(&self) -> f64 {
        1e6 / self.link_interval_us
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimingStats {
    pub intervals: usize,
    /// Median interval between regular packets.
    pub nominal_us: u64,
    pub p95_us: u64,
    pub p99_us: u64,
    /// Standard deviation of the intervals.
    pub jitter_us: f64,
    /// Effective packet rate while moving.
    pub rate_hz: f64,
    /// Share of intervals longer than twice the nominal one.
    pub miss_share: f64,
    /// Share of packets arriving bunched with the previous one.
    pub bunched_share: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    None,
    Weak,
    Likely,
    Strong,
}

impl Level {
    fn from_score(score: f64) -> Self {
        if score >= 1.0 {
            Level::Strong
        } else if score >= 0.4 {
            Level::Likely
        } else if score >= 0.15 {
            Level::Weak
        } else {
            Level::None
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cause {
    DoubleFrames,
    Quantization,
    StickSlip,
    Polling,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Finding {
    pub cause: Cause,
    pub score: f64,
    pub level: Level,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Analysis {
    pub scenarios: Vec<ScenarioAnalysis>,
    pub settings: PointerSettings,
    pub timing: Option<TimingStats>,
    pub slow_packets: usize,
    pub slow_active_s: f64,
    pub slow_median_cps: Option<f64>,
    pub slow_dx: AxisHistogram,
    pub slow_dy: AxisHistogram,
    /// `None` when there are too few slow packets to tell.
    pub slow_unit_share: Option<f64>,
    /// `None` when there is too little slow motion to tell.
    pub stick_slips_per_10s: Option<f64>,
    /// Pauses during fast motion. These can only come from the link, which
    /// then accounts for the same rate of pauses during slow motion.
    /// `None` when there is too little fast motion to tell.
    pub link_pauses_per_10s: Option<f64>,
    /// `None` when there are too few spikes to look for a rhythm.
    pub double_frames: Option<DoubleFrames>,
    /// Scored causes, most likely first.
    pub findings: Vec<Finding>,
}

impl Analysis {
    /// The most likely cause, if any of them stands out at all.
    pub fn primary(&self) -> Option<&Finding> {
        self.findings.first().filter(|f| f.level > Level::None)
    }

    #[cfg(test)]
    pub fn finding(&self, cause: Cause) -> Option<&Finding> {
        self.findings.iter().find(|f| f.cause == cause)
    }
}

/// One motion packet. Packets without motion say nothing about the sensor.
#[derive(Clone, Copy)]
struct Motion {
    t: u64,
    dx: f64,
    dy: f64,
    len: f64,
}

impl Motion {
    fn from_event(e: &RawEvent) -> Self {
        let (dx, dy) = (e.dx as f64, e.dy as f64);
        Self {
            t: e.t_us,
            dx,
            dy,
            len: dx.hypot(dy),
        }
    }

    fn same_direction(&self, other: &Motion) -> bool {
        self.dx * other.dx + self.dy * other.dy > 0.0
    }
}

pub fn analyze_scenario(
    name: &str,
    events: &[RawEvent],
    thresholds: &Thresholds,
) -> ScenarioAnalysis {
    let motion: Vec<Motion> = events
        .iter()
        .filter(|e| e.is_motion())
        .map(Motion::from_event)
        .collect();
    let speeds = centered_speeds(&motion);

    let mut result = ScenarioAnalysis {
        name: name.to_string(),
        duration_s: events.last().map_or(0.0, |e| e.t_us as f64 / 1e6),
        motion_packets: motion.len(),
        stick_slips: find_stick_slips(&motion, thresholds),
        ..Default::default()
    };

    let mut slow_speeds = Vec::new();
    for (i, packet) in motion.iter().enumerate() {
        if speeds[i] < SLOW_MAX_CPS {
            result.slow_packets += 1;
            slow_speeds.push(speeds[i]);
            result.slow_dx.add(packet.dx as i32);
            result.slow_dy.add(packet.dy as i32);
            if packet.dx.abs().max(packet.dy.abs()) == 1.0 {
                result.slow_unit_packets += 1;
            }
        }
        let Some(previous) = i.checked_sub(1).map(|p| &motion[p]) else {
            continue;
        };
        let interval = packet.t.saturating_sub(previous.t);

        if speeds[i] < SLOW_MAX_CPS && interval <= MAX_STICK_PAUSE_US {
            result.slow_active_s += interval as f64 / 1e6;
        }

        let moving_fast = speeds[i] >= FAST_MIN_CPS
            && previous.len >= FAST_MIN_PACKET_COUNTS
            && packet.len >= FAST_MIN_PACKET_COUNTS
            && packet.same_direction(previous);
        if moving_fast && interval <= BURST_GAP_US {
            result.fast_intervals_us.push(interval);
            if interval >= thresholds.pause_ms * 1000 {
                result.fast_pauses += 1;
            }
        }
    }
    result.slow_median_cps = median_f64(&mut slow_speeds);
    scan_double_frames(events, &mut result);
    result
}

/// Looks for spikes in each run of consecutive packets. Packets without motion
/// stay in, they are valid samples of a ball that did not move.
fn scan_double_frames(events: &[RawEvent], result: &mut ScenarioAnalysis) {
    let mut intervals: Vec<u64> = events
        .windows(2)
        .map(|pair| pair[1].t_us.saturating_sub(pair[0].t_us))
        .filter(|&dt| dt < RUN_GAP_US)
        .collect();
    intervals.sort_unstable();
    if intervals.is_empty() {
        return;
    }
    let link_interval = percentile(&intervals, 0.5);
    result.link_interval_us = Some(link_interval);

    let runs = events.chunk_by(|a, b| b.t_us.saturating_sub(a.t_us) < RUN_GAP_US);
    for run in runs {
        let sizes: Vec<f64> = run
            .iter()
            .enumerate()
            .map(|(i, event)| packet_size(run, i, event, link_interval))
            .collect();
        let scan = find_spikes(&sizes);
        result.spike_checked += scan.checked;
        result
            .spike_ratios
            .extend(scan.spikes.iter().map(|spike| spike.ratio));
        result.spike_spacings.extend(scan.spikes.windows(2).map(|pair| {
            let gap = run[pair[1].index].t_us - run[pair[0].index].t_us;
            gap as f64 / link_interval as f64
        }));
    }
}

/// Size of a packet, spread over the reports it stands for. After a missed
/// report one packet legitimately carries two intervals' worth of motion.
fn packet_size(run: &[RawEvent], i: usize, event: &RawEvent, link_interval: u64) -> f64 {
    let size = (event.dx as f64).hypot(event.dy as f64);
    let Some(previous) = i.checked_sub(1).map(|p| &run[p]) else {
        return size;
    };
    let intervals = event.t_us.saturating_sub(previous.t_us) as f64 / link_interval as f64;
    if intervals >= MISSED_REPORT_INTERVALS {
        size / intervals
    } else {
        size
    }
}

fn double_frames(scenarios: &[ScenarioAnalysis]) -> Option<DoubleFrames> {
    let mut ratios: Vec<f64> = Vec::new();
    let mut spacings: Vec<f64> = Vec::new();
    let mut link_intervals: Vec<f64> = Vec::new();
    let mut checked_packets = 0;
    for s in scenarios {
        ratios.extend_from_slice(&s.spike_ratios);
        spacings.extend_from_slice(&s.spike_spacings);
        link_intervals.extend(s.link_interval_us.map(|us| us as f64));
        checked_packets += s.spike_checked;
    }
    if ratios.len() < MIN_DOUBLE_FRAMES {
        return None;
    }

    let mut histogram: BTreeMap<u32, usize> = BTreeMap::new();
    for &spacing in &spacings {
        *histogram.entry(spacing.round() as u32).or_default() += 1;
    }
    let (&mode, _) = histogram
        .iter()
        .filter(|(&packets, _)| packets >= MIN_RHYTHM_PACKETS)
        .max_by_key(|(_, &count)| count)?;

    let near_mode: Vec<f64> = spacings
        .iter()
        .copied()
        .filter(|s| (s - mode as f64).abs() <= RHYTHM_WINDOW_PACKETS)
        .collect();
    let period_packets = near_mode.iter().sum::<f64>() / near_mode.len() as f64;
    let fits_rhythm = |spacing: f64| {
        let multiple = (spacing / period_packets).round();
        multiple >= 1.0 && (spacing - multiple * period_packets).abs() <= RHYTHM_TOLERANCE_PACKETS
    };
    let regular = spacings.iter().filter(|&&s| fits_rhythm(s)).count();

    Some(DoubleFrames {
        spikes: ratios.len(),
        checked_packets,
        median_ratio: median_f64(&mut ratios)?,
        link_interval_us: median_f64(&mut link_intervals)?,
        period_packets,
        regular_share: regular as f64 / spacings.len() as f64,
    })
}

fn double_frame_score(frames: &DoubleFrames) -> f64 {
    if !DOUBLE_RATIO_RANGE.contains(&frames.median_ratio) {
        return 0.0;
    }
    // Spacings fall into the tolerance windows this often by chance alone.
    let chance = (2.0 * RHYTHM_TOLERANCE_PACKETS / frames.period_packets).min(0.99);
    let excess = (frames.regular_share - chance) / (1.0 - chance);
    (excess / RHYTHM_STRONG_EXCESS).max(0.0)
}

/// Speed around each packet, in counts per second.
fn centered_speeds(motion: &[Motion]) -> Vec<f64> {
    let half = SPEED_WINDOW_US / 2;
    let window_s = SPEED_WINDOW_US as f64 / 1e6;
    let (mut low, mut high, mut sum) = (0, 0, 0.0);
    let mut speeds = Vec::with_capacity(motion.len());
    for packet in motion {
        while high < motion.len() && motion[high].t <= packet.t + half {
            sum += motion[high].len;
            high += 1;
        }
        while motion[low].t + half < packet.t {
            sum -= motion[low].len;
            low += 1;
        }
        speeds.push(sum / window_s);
    }
    speeds
}

fn find_stick_slips(motion: &[Motion], thresholds: &Thresholds) -> Vec<StickSlip> {
    let pause_us = thresholds.pause_ms * 1000;
    let mut found = Vec::new();
    let mut i = 1;
    while i < motion.len() {
        let pause = motion[i].t - motion[i - 1].t;
        if pause < pause_us || pause > MAX_STICK_PAUSE_US {
            i += 1;
            continue;
        }

        let before = sum_motion(motion[..i].iter().rev().take_while(|m| {
            motion[i - 1].t - m.t <= SPEED_WINDOW_US
        }));
        let jump = sum_motion(
            motion[i..]
                .iter()
                .take_while(|m| m.t - motion[i].t <= JUMP_WINDOW_US),
        );

        let speed_before = before.path / (SPEED_WINDOW_US as f64 / 1e6);
        let speed_after = jump.path / (JUMP_WINDOW_US as f64 / 1e6);
        let is_jump = before.path >= 2.0
            && speed_before < SLOW_MAX_CPS
            && jump.path >= thresholds.jump_counts
            && speed_after >= speed_before * JUMP_SPEED_RATIO
            && before.dx * jump.dx + before.dy * jump.dy > 0.0;

        if is_jump {
            found.push(StickSlip {
                t_us: motion[i].t,
                pause_ms: pause as f64 / 1000.0,
                jump_counts: jump.path,
            });
            i += jump.packets;
        } else {
            i += 1;
        }
    }
    found
}

#[derive(Default)]
struct MotionSum {
    dx: f64,
    dy: f64,
    path: f64,
    packets: usize,
}

fn sum_motion<'a>(packets: impl Iterator<Item = &'a Motion>) -> MotionSum {
    let mut sum = MotionSum::default();
    for packet in packets {
        sum.dx += packet.dx;
        sum.dy += packet.dy;
        sum.path += packet.len;
        sum.packets += 1;
    }
    sum
}

/// Nearest-rank percentile of a sorted slice.
fn percentile(sorted: &[u64], fraction: f64) -> u64 {
    let rank = (fraction * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

fn median_f64(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    Some(values[(values.len() - 1) / 2])
}

fn timing_stats(intervals: &[u64]) -> Option<TimingStats> {
    if intervals.len() < MIN_TIMING_INTERVALS {
        return None;
    }
    let mut sorted = intervals.to_vec();
    sorted.sort_unstable();

    // Bunched packets would pull the median towards zero, so the nominal
    // interval comes from the regular ones.
    let first_regular = sorted.partition_point(|&dt| dt < BUNCHED_INTERVAL_US);
    let regular = &sorted[first_regular..];
    let nominal_us = if regular.is_empty() {
        percentile(&sorted, 0.5)
    } else {
        percentile(regular, 0.5)
    };

    let count = sorted.len() as f64;
    let total: u64 = sorted.iter().sum();
    let mean = total as f64 / count;
    let variance = sorted
        .iter()
        .map(|&dt| (dt as f64 - mean).powi(2))
        .sum::<f64>()
        / count;
    let misses = sorted.iter().filter(|&&dt| dt > 2 * nominal_us).count();

    Some(TimingStats {
        intervals: sorted.len(),
        nominal_us,
        p95_us: percentile(&sorted, 0.95),
        p99_us: percentile(&sorted, 0.99),
        jitter_us: variance.sqrt(),
        rate_hz: 1e6 / mean,
        miss_share: misses as f64 / count,
        bunched_share: first_regular as f64 / count,
    })
}

fn polling_score(timing: &TimingStats, link_pauses_per_10s: f64) -> f64 {
    let misses = timing.miss_share / STRONG_MISS_SHARE;
    let bunching = timing.bunched_share / STRONG_BUNCHED_SHARE;
    let pauses = link_pauses_per_10s / STRONG_PAUSES_PER_10S;
    // A heavy tail: p95 at 1.5x nominal scores 0, at 3x it scores 1.
    let tail_ratio = timing.p95_us as f64 / timing.nominal_us.max(1) as f64;
    let tail = (tail_ratio - 1.5).max(0.0) / 1.5;
    misses.max(bunching).max(pauses).max(tail)
}

fn quantization_score(unit_share: f64, pixels_per_count: f64) -> f64 {
    // Below one pixel per count the system absorbs the steps entirely.
    if unit_share < UNIT_SHARE_THRESHOLD || pixels_per_count < 1.0 {
        return 0.0;
    }
    // 1:1 is at the sensor's limit but steps are only a pixel. 1.5 is strong.
    0.2 + (pixels_per_count - 1.0) * 1.6
}

pub fn analyze(scenarios: Vec<ScenarioAnalysis>, settings: PointerSettings) -> Analysis {
    let mut slow_dx = AxisHistogram::default();
    let mut slow_dy = AxisHistogram::default();
    let mut slow_packets = 0;
    let mut slow_unit_packets = 0;
    let mut slow_active_s = 0.0;
    let mut stick_slips = 0;
    let mut fast_pauses = 0;
    let mut intervals = Vec::new();
    let mut weighted_speed = 0.0;

    for s in &scenarios {
        slow_dx.merge(&s.slow_dx);
        slow_dy.merge(&s.slow_dy);
        slow_packets += s.slow_packets;
        slow_unit_packets += s.slow_unit_packets;
        slow_active_s += s.slow_active_s;
        stick_slips += s.stick_slips.len();
        fast_pauses += s.fast_pauses;
        intervals.extend_from_slice(&s.fast_intervals_us);
        weighted_speed += s.slow_median_cps.unwrap_or(0.0) * s.slow_packets as f64;
    }

    let timing = timing_stats(&intervals);
    let slow_unit_share = (slow_packets >= MIN_SLOW_PACKETS)
        .then(|| slow_unit_packets as f64 / slow_packets as f64);
    let stick_slips_per_10s =
        (slow_active_s >= MIN_SLOW_SECONDS).then(|| stick_slips as f64 * 10.0 / slow_active_s);

    let fast_seconds = intervals.iter().sum::<u64>() as f64 / 1e6;
    let link_pauses_per_10s = timing
        .is_some()
        .then(|| fast_pauses as f64 * 10.0 / fast_seconds);

    let poll = timing.as_ref().map_or(0.0, |t| {
        polling_score(t, link_pauses_per_10s.unwrap_or(0.0))
    });
    let quant = slow_unit_share.map_or(0.0, |share| {
        quantization_score(share, settings.multiplier().unwrap_or(1.0))
    });
    let stick = stick_slips_per_10s
        .filter(|_| stick_slips >= MIN_STICK_SLIPS)
        .map_or(0.0, |rate| {
            (rate - link_pauses_per_10s.unwrap_or(0.0)).max(0.0) / STRONG_PAUSES_PER_10S
        });
    let double_frames = double_frames(&scenarios);
    let doubled = double_frames.as_ref().map_or(0.0, double_frame_score);

    let mut findings: Vec<Finding> = [
        (Cause::DoubleFrames, doubled),
        (Cause::Quantization, quant),
        (Cause::StickSlip, stick),
        (Cause::Polling, poll),
    ]
    .into_iter()
    .map(|(cause, score)| Finding {
        cause,
        score,
        level: Level::from_score(score),
    })
    .collect();
    findings.sort_by(|a, b| b.score.total_cmp(&a.score));

    Analysis {
        scenarios,
        settings,
        timing,
        slow_packets,
        slow_active_s,
        slow_median_cps: (slow_packets > 0).then(|| weighted_speed / slow_packets as f64),
        slow_dx,
        slow_dy,
        slow_unit_share,
        stick_slips_per_10s,
        link_pauses_per_10s,
        double_frames,
        findings,
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// Deterministic generator for link and timing noise.
    pub struct Rng(u64);

    impl Rng {
        pub fn new(seed: u64) -> Self {
            Self(seed)
        }

        pub fn unit(&mut self) -> f64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1);
            (self.0 >> 40) as f64 / (1u64 << 24) as f64
        }
    }

    /// A model of the trackball: the ball turns at `cps`, the sensor gathers
    /// counts and the link reports whole counts every `poll_us`.
    pub struct Model {
        pub seconds: f64,
        pub cps: f64,
        pub poll_us: u64,
        /// The direction flips with this period.
        pub reverse_every_s: f64,
        /// `(period, length)` of the ball sticking while the thumb keeps
        /// pushing; the stored motion is released at once afterwards.
        pub stick_us: Option<(u64, u64)>,
        /// Chance that a report is lost, and how many reports each loss takes.
        pub link_loss: Option<(f64, u32)>,
        /// The sensor's own frame period, when it differs from the link's.
        /// Motion only reaches a report in whole frames.
        pub frame_us: Option<u64>,
    }

    impl Model {
        pub fn new(seconds: f64, cps: f64) -> Self {
            Self {
                seconds,
                cps,
                poll_us: 8_000,
                reverse_every_s: 2.0,
                stick_us: None,
                link_loss: None,
                frame_us: None,
            }
        }

        pub fn run(&self, seed: u64) -> Vec<RawEvent> {
            let mut rng = Rng::new(seed);
            let mut events = Vec::new();
            let (mut pending, mut stored) = (0.0, 0.0);
            let mut skip_reports = 0;
            let total_us = (self.seconds * 1e6) as u64;
            let step_s = self.poll_us as f64 / 1e6;

            let mut t = 0;
            while t < total_us {
                t += self.poll_us;
                let phase = (t as f64 / 1e6 / self.reverse_every_s) as u64;
                let direction = if phase.is_multiple_of(2) { 1.0 } else { -1.0 };
                let travel = match self.frame_us {
                    Some(frame) => {
                        let frames = t / frame - (t - self.poll_us) / frame;
                        self.cps * (frames * frame) as f64 / 1e6 * direction
                    }
                    None => self.cps * step_s * direction,
                };

                let stuck = self
                    .stick_us
                    .is_some_and(|(period, length)| t % period < length);
                if stuck {
                    stored += travel;
                } else {
                    pending += travel + stored;
                    stored = 0.0;
                }

                if skip_reports > 0 {
                    skip_reports -= 1;
                    continue;
                }
                if let Some((chance, length)) = self.link_loss {
                    if rng.unit() < chance {
                        skip_reports = length - 1;
                        continue;
                    }
                }

                let whole = pending.trunc();
                pending -= whole;
                if whole != 0.0 {
                    let jitter = (rng.unit() * 200.0) as u64;
                    events.push(RawEvent {
                        t_us: t + jitter,
                        dx: whole as i32,
                        ..Default::default()
                    });
                }
            }
            events
        }
    }

    pub fn session(
        slow: &Model,
        fast: &Model,
        settings: PointerSettings,
    ) -> Analysis {
        let thresholds = Thresholds::default();
        analyze(
            vec![
                analyze_scenario("slow_h", &slow.run(1), &thresholds),
                analyze_scenario("slow_v", &slow.run(2), &thresholds),
                analyze_scenario("fast", &fast.run(3), &thresholds),
            ],
            settings,
        )
    }

    pub fn unit_speed() -> PointerSettings {
        PointerSettings {
            speed: Some(10),
            enhanced_precision: Some(false),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;

    const SLOW_CPS: f64 = 100.0;
    const FAST_CPS: f64 = 1500.0;

    fn slow() -> Model {
        Model::new(10.0, SLOW_CPS)
    }

    fn fast() -> Model {
        Model::new(10.0, FAST_CPS)
    }

    fn event(t_us: u64, dx: i32) -> RawEvent {
        RawEvent {
            t_us,
            dx,
            ..Default::default()
        }
    }

    #[test]
    fn clean_session_blames_nothing_serious() {
        let analysis = session(&slow(), &fast(), unit_speed());
        for finding in &analysis.findings {
            assert!(
                finding.level <= Level::Weak,
                "unexpected finding {finding:?}"
            );
        }
        assert_eq!(analysis.stick_slips_per_10s, Some(0.0));
    }

    #[test]
    fn high_windows_speed_is_quantization() {
        let settings = PointerSettings {
            speed: Some(16),
            enhanced_precision: Some(false),
        };
        let analysis = session(&slow(), &fast(), settings);
        let primary = analysis.primary().unwrap();
        assert_eq!(primary.cause, Cause::Quantization);
        assert_eq!(primary.level, Level::Strong);
    }

    #[test]
    fn low_windows_speed_hides_quantization() {
        let settings = PointerSettings {
            speed: Some(6),
            enhanced_precision: Some(false),
        };
        let analysis = session(&slow(), &fast(), settings);
        assert_eq!(
            analysis.finding(Cause::Quantization).unwrap().level,
            Level::None
        );
    }

    #[test]
    fn slow_only_pauses_are_stick_slip() {
        let sticky = Model {
            stick_us: Some((400_000, 80_000)),
            ..slow()
        };
        let analysis = session(&sticky, &fast(), unit_speed());
        let primary = analysis.primary().unwrap();
        assert_eq!(primary.cause, Cause::StickSlip);
        assert_eq!(primary.level, Level::Strong);
    }

    #[test]
    fn lost_reports_at_speed_are_polling() {
        let lossy = |model: Model| Model {
            link_loss: Some((0.08, 2)),
            ..model
        };
        let analysis = session(&lossy(slow()), &lossy(fast()), unit_speed());
        let primary = analysis.primary().unwrap();
        assert_eq!(primary.cause, Cause::Polling);
        assert_eq!(primary.level, Level::Strong);
        assert!(analysis.timing.unwrap().miss_share > STRONG_MISS_SHARE);
    }

    #[test]
    fn long_dropouts_are_blamed_on_the_link_not_the_ball() {
        // Dropouts this long look exactly like stick-slip at low speed.
        let lossy = |model: Model| Model {
            link_loss: Some((0.03, 8)),
            ..model
        };
        let analysis = session(&lossy(slow()), &lossy(fast()), unit_speed());
        assert_eq!(analysis.primary().unwrap().cause, Cause::Polling);
        assert!(analysis.stick_slips_per_10s.unwrap() > STRONG_PAUSES_PER_10S);
        assert_eq!(analysis.finding(Cause::StickSlip).unwrap().level, Level::None);
    }

    #[test]
    fn stick_slip_stands_out_over_a_mildly_bad_link() {
        let sticky = Model {
            stick_us: Some((400_000, 80_000)),
            link_loss: Some((0.002, 8)),
            ..slow()
        };
        let lossy_fast = Model {
            link_loss: Some((0.002, 8)),
            ..fast()
        };
        let analysis = session(&sticky, &lossy_fast, unit_speed());
        assert_eq!(analysis.primary().unwrap().cause, Cause::StickSlip);
    }

    #[test]
    fn beating_sensor_and_link_are_double_frames() {
        let beating = |cps| Model {
            poll_us: 8_750,
            frame_us: Some(8_060),
            ..Model::new(10.0, cps)
        };
        let analysis = session(&beating(350.0), &beating(1500.0), unit_speed());

        let primary = analysis.primary().unwrap();
        assert_eq!(primary.cause, Cause::DoubleFrames);
        assert_eq!(primary.level, Level::Strong);

        let frames = analysis.double_frames.unwrap();
        assert!((frames.period_ms() - 102.0).abs() < 3.0, "{frames:?}");
        assert!((frames.median_ratio - 2.0).abs() < 0.4, "{frames:?}");
    }

    #[test]
    fn random_spikes_have_no_rhythm() {
        let mut rng = Rng::new(7);
        let events: Vec<RawEvent> = (1..4000)
            .map(|i| {
                let size = if rng.unit() < 0.08 { 8 } else { 4 };
                event(i * 8_750, size)
            })
            .collect();
        let scenario = analyze_scenario("circle", &events, &Thresholds::default());
        let analysis = analyze(vec![scenario], unit_speed());
        assert!(analysis.double_frames.unwrap().spikes >= MIN_DOUBLE_FRAMES);
        assert_eq!(
            analysis.finding(Cause::DoubleFrames).unwrap().level,
            Level::None
        );
    }

    #[test]
    fn a_missed_report_is_not_a_double_frame() {
        // The packet after the gap catches up, which is the link's doing.
        let mut events = Vec::new();
        let mut t = 0;
        for i in 0..2000 {
            t += if i % 12 == 0 { 17_500 } else { 8_750 };
            let size = if i % 12 == 0 { 8 } else { 4 };
            events.push(event(t, size));
        }
        let scenario = analyze_scenario("circle", &events, &Thresholds::default());
        assert!(scenario.spike_ratios.is_empty());
    }

    #[test]
    fn a_lone_flick_is_not_stick_slip() {
        let mut slow_motion: Vec<RawEvent> = (1..800).map(|i| event(i * 8_000, 2)).collect();
        // One deliberate pause and flick in ten seconds.
        slow_motion[400].dx = 40;
        for e in &mut slow_motion[401..] {
            e.t_us += 100_000;
        }
        let scenario = analyze_scenario("slow_h", &slow_motion, &Thresholds::default());
        let analysis = analyze(vec![scenario], unit_speed());
        assert_eq!(
            analysis.finding(Cause::StickSlip).unwrap().level,
            Level::None
        );
    }

    #[test]
    fn bunched_packets_do_not_hide_the_nominal_interval() {
        let intervals: Vec<u64> = (0..400)
            .map(|i| if i % 2 == 0 { 200 } else { 15_000 })
            .collect();
        let stats = timing_stats(&intervals).unwrap();
        assert_eq!(stats.nominal_us, 15_000);
        assert!((stats.bunched_share - 0.5).abs() < 1e-9);
        assert!(polling_score(&stats, 0.0) >= 1.0);
    }

    #[test]
    fn reversals_are_not_counted_as_lost_reports() {
        let mut events = Vec::new();
        let mut t = 0;
        for swing in 0..40 {
            let sign = if swing % 2 == 0 { 1 } else { -1 };
            for _ in 0..30 {
                t += 8_000;
                events.push(event(t, 12 * sign));
            }
            t += 30_000; // the ball stops for a moment to change direction
        }
        let scenario = analyze_scenario("fast", &events, &Thresholds::default());
        assert!(scenario.fast_intervals_us.iter().all(|&dt| dt == 8_000));
    }

    #[test]
    fn stopping_and_starting_again_is_not_stick_slip() {
        let mut events = Vec::new();
        let mut t = 0;
        for burst in 0..3 {
            for _ in 0..40 {
                t += 16_000;
                events.push(event(t, 1));
            }
            t += 1_000_000; // the user lifts off the ball
            if burst < 2 {
                events.push(event(t, 8));
            }
        }
        let scenario = analyze_scenario("slow_h", &events, &Thresholds::default());
        assert!(scenario.stick_slips.is_empty());
    }

    #[test]
    fn a_very_slow_ball_with_sparse_counts_is_not_stick_slip() {
        // 20 counts per second: a 50 ms gap before each single count.
        let events: Vec<RawEvent> = (1..200).map(|i| event(i * 50_000, 1)).collect();
        let scenario = analyze_scenario("slow_h", &events, &Thresholds::default());
        assert!(scenario.stick_slips.is_empty());
    }

    #[test]
    fn a_jump_in_the_opposite_direction_is_a_reversal() {
        let mut events: Vec<RawEvent> = (1..20).map(|i| event(i * 16_000, 1)).collect();
        events.push(event(380_000, -8));
        let scenario = analyze_scenario("slow_h", &events, &Thresholds::default());
        assert!(scenario.stick_slips.is_empty());
    }

    #[test]
    fn stick_slip_thresholds_are_respected() {
        let mut events: Vec<RawEvent> = (1..20).map(|i| event(i * 16_000, 1)).collect();
        events.push(event(304_000 + 60_000, 8));

        let found = analyze_scenario("slow_h", &events, &Thresholds::default());
        assert_eq!(found.stick_slips.len(), 1);
        assert_eq!(found.stick_slips[0].pause_ms, 60.0);
        assert_eq!(found.stick_slips[0].jump_counts, 8.0);

        let strict = Thresholds {
            pause_ms: 80,
            ..Thresholds::default()
        };
        let missed = analyze_scenario("slow_h", &events, &strict);
        assert!(missed.stick_slips.is_empty());
    }

    #[test]
    fn too_little_data_gives_no_verdict() {
        let analysis = session(&Model::new(0.2, SLOW_CPS), &Model::new(0.1, FAST_CPS), unit_speed());
        assert!(analysis.timing.is_none());
        assert!(analysis.slow_unit_share.is_none());
        assert!(analysis.stick_slips_per_10s.is_none());
        assert!(analysis.primary().is_none());
    }

    #[test]
    fn empty_scenario_is_harmless() {
        let scenario = analyze_scenario("slow_h", &[], &Thresholds::default());
        assert_eq!(scenario.motion_packets, 0);
        assert_eq!(scenario.duration_s, 0.0);
    }

    #[test]
    fn packets_without_motion_are_ignored() {
        let click = RawEvent {
            t_us: 4_000,
            buttons: 1,
            ..Default::default()
        };
        let events = [event(0, 1), click, event(8_000, 1)];
        let scenario = analyze_scenario("slow_h", &events, &Thresholds::default());
        assert_eq!(scenario.motion_packets, 2);
    }

    #[test]
    fn percentile_uses_the_nearest_rank() {
        let sorted = [10, 20, 30, 40];
        assert_eq!(percentile(&sorted, 0.5), 20);
        assert_eq!(percentile(&sorted, 0.95), 40);
        assert_eq!(percentile(&sorted, 0.0), 10);
    }
}
