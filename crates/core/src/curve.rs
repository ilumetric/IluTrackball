//! Acceleration curves: pointer gain as a function of speed.

#[derive(Clone, Debug, PartialEq)]
pub enum Curve {
    /// Gain of 1 at any speed.
    Flat,
    /// `base + slope * speed`, up to `cap`.
    Linear { base: f64, slope: f64, cap: f64 },
    /// `base + (scale * speed) ^ exponent`, up to `cap`.
    Power {
        base: f64,
        scale: f64,
        exponent: f64,
        cap: f64,
    },
    /// A smooth line through `(speed, gain)` points sorted by speed. It never
    /// rises above or falls below the points around it, and it runs flat into
    /// the first and last gains, which hold beyond the ends of the table.
    Table(Vec<(f64, f64)>),
}

impl Curve {
    /// Gain at `speed`, in counts per millisecond.
    pub fn gain(&self, speed: f64) -> f64 {
        match self {
            Curve::Flat => 1.0,
            Curve::Linear { base, slope, cap } => (base + slope * speed).min(*cap),
            Curve::Power {
                base,
                scale,
                exponent,
                cap,
            } => (base + (scale * speed).powf(*exponent)).min(*cap),
            Curve::Table(points) => table_gain(points, speed),
        }
    }
}

/// Keeps the logarithm defined whatever a table holds.
const MIN_TABLE_GAIN: f64 = 1e-6;

/// Monotone cubic interpolation of the logarithm of the gain. Working on the
/// logarithm treats doubling alike at every height, so the line looks even on
/// the graph in the settings window, whose gain axis is logarithmic too.
fn table_gain(points: &[(f64, f64)], speed: f64) -> f64 {
    let Some((&first, &last)) = points.first().zip(points.last()) else {
        return 1.0;
    };
    if speed <= first.0 {
        return first.1;
    }
    if speed >= last.0 {
        return last.1;
    }
    let upper = points.partition_point(|point| point.0 <= speed);
    let lower = upper - 1;
    let (from, to) = (points[lower], points[upper]);

    let width = to.0 - from.0;
    let t = (speed - from.0) / width;
    let (t2, t3) = (t * t, t * t * t);
    let log_gain = (2.0 * t3 - 3.0 * t2 + 1.0) * log_of(from)
        + (t3 - 2.0 * t2 + t) * width * tangent(points, lower)
        + (-2.0 * t3 + 3.0 * t2) * log_of(to)
        + (t3 - t2) * width * tangent(points, upper);
    log_gain.exp()
}

fn log_of(point: (f64, f64)) -> f64 {
    point.1.max(MIN_TABLE_GAIN).ln()
}

fn secant(from: (f64, f64), to: (f64, f64)) -> f64 {
    (log_of(to) - log_of(from)) / (to.0 - from.0)
}

/// Slope of the line at a point, by the Fritsch-Butland rule: a weighted
/// harmonic mean of the slopes around it, which rules out overshoot. The line
/// is level at both ends of the table and at every peak or plateau.
fn tangent(points: &[(f64, f64)], index: usize) -> f64 {
    if index == 0 || index + 1 == points.len() {
        return 0.0;
    }
    let (before, here, after) = (points[index - 1], points[index], points[index + 1]);
    let (slope_before, slope_after) = (secant(before, here), secant(here, after));
    if slope_before * slope_after <= 0.0 {
        return 0.0;
    }
    let (width_before, width_after) = (here.0 - before.0, after.0 - here.0);
    let weight_before = 2.0 * width_after + width_before;
    let weight_after = width_after + 2.0 * width_before;
    (weight_before + weight_after) / (weight_before / slope_before + weight_after / slope_after)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_never_changes_motion() {
        assert_eq!(Curve::Flat.gain(0.0), 1.0);
        assert_eq!(Curve::Flat.gain(100.0), 1.0);
    }

    #[test]
    fn linear_rises_up_to_the_cap() {
        let curve = Curve::Linear {
            base: 1.0,
            slope: 0.5,
            cap: 2.0,
        };
        assert_eq!(curve.gain(0.0), 1.0);
        assert_eq!(curve.gain(1.0), 1.5);
        assert_eq!(curve.gain(10.0), 2.0);
    }

    #[test]
    fn power_follows_the_exponent() {
        let curve = Curve::Power {
            base: 1.0,
            scale: 0.5,
            exponent: 2.0,
            cap: 5.0,
        };
        assert_eq!(curve.gain(0.0), 1.0);
        assert_eq!(curve.gain(2.0), 2.0);
        assert_eq!(curve.gain(100.0), 5.0);
    }

    fn table() -> Curve {
        Curve::Table(vec![
            (0.0, 1.0),
            (0.7, 1.05),
            (1.4, 1.8),
            (2.2, 3.3),
            (3.0, 6.2),
            (3.4, 8.0),
        ])
    }

    fn samples() -> impl Iterator<Item = f64> {
        (0..=4000).map(|step| step as f64 / 1000.0)
    }

    #[test]
    fn table_passes_through_its_points() {
        let curve = table();
        let Curve::Table(points) = &curve else {
            unreachable!()
        };
        for &(speed, gain) in points {
            assert!((curve.gain(speed) - gain).abs() < 1e-9, "at {speed}");
        }
    }

    #[test]
    fn table_holds_its_end_values_beyond_the_ends() {
        let curve = Curve::Table(vec![(1.0, 1.0), (3.0, 2.0), (5.0, 4.0)]);
        assert_eq!(curve.gain(0.0), 1.0);
        assert_eq!(curve.gain(9.0), 4.0);
    }

    #[test]
    fn table_never_turns_back_while_rising() {
        let curve = table();
        let gains: Vec<f64> = samples().map(|speed| curve.gain(speed)).collect();
        assert!(gains.windows(2).all(|pair| pair[1] >= pair[0] - 1e-12));
    }

    #[test]
    fn table_stays_between_the_points_around_it() {
        // An ordinary spline would swing past the plateau here.
        let curve = Curve::Table(vec![(0.0, 1.0), (1.0, 4.0), (2.0, 4.0), (3.0, 8.0)]);
        for step in 1000..=2000 {
            let gain = curve.gain(step as f64 / 1000.0);
            assert!((4.0 - 1e-9..=4.0 + 1e-9).contains(&gain), "{gain}");
        }
    }

    #[test]
    fn table_has_no_corners() {
        let curve = table();
        let Curve::Table(points) = &curve else {
            unreachable!()
        };
        let slope = |speed: f64| (curve.gain(speed + 1e-6) - curve.gain(speed)) / 1e-6;
        for &(speed, _) in &points[1..points.len() - 1] {
            let (before, after) = (slope(speed - 2e-6), slope(speed + 1e-6));
            assert!((before - after).abs() < 1e-3, "at {speed}: {before} {after}");
        }
    }

    #[test]
    fn table_runs_level_into_both_ends() {
        let curve = table();
        assert!((curve.gain(0.01) - 1.0).abs() < 1e-3);
        assert!((curve.gain(3.39) - 8.0).abs() < 1e-2);
    }

    #[test]
    fn two_points_make_a_smooth_step() {
        let curve = Curve::Table(vec![(0.0, 1.0), (2.0, 4.0)]);
        assert!((curve.gain(1.0) - 2.0).abs() < 1e-9);
    }

    #[test]
    fn a_table_with_a_zero_gain_stays_a_number() {
        let curve = Curve::Table(vec![(0.0, 0.0), (1.0, 1.0)]);
        assert!(curve.gain(0.5).is_finite());
    }

    #[test]
    fn empty_table_keeps_motion_unchanged() {
        assert_eq!(Curve::Table(Vec::new()).gain(3.0), 1.0);
    }
}
