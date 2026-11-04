//! The acceleration curve as a graph with points to drag.
//!
//! Across is the speed of the ball, up is how much the motion gets multiplied.
//! A marker shows the current speed, so the place on the curve that matters
//! can be found by simply moving the ball.

use eframe::egui::{pos2, vec2, Align2, Color32, FontId, Pos2, Rect, Sense, Shape, Stroke, Ui};
use ilumx_core::curve::Curve;

use crate::i18n::Lang;
use crate::theme::{self, paint_rounded, RADIUS_CONTROL};

/// `(speed in counts per ms, gain)`. All three stay flat at slow speed. The
/// recorded logs put precise motion below 1.2 and flicks above 3.
pub const MEDIUM_CURVE: &[(f64, f64)] = &[
    (0.0, 1.0),
    (0.7, 1.05),
    (1.4, 1.8),
    (2.2, 3.3),
    (3.0, 6.2),
    (3.4, 8.0),
];
/// A ready-made curve: Russian name, English name, points.
pub type ReadyCurve = (&'static str, &'static str, &'static [(f64, f64)]);

pub const READY_CURVES: [ReadyCurve; 3] = [
    (
        "Лёгкое",
        "Gentle",
        &[(0.0, 1.0), (1.0, 1.0), (3.0, 2.5), (5.0, 3.0)],
    ),
    ("Среднее", "Medium", MEDIUM_CURVE),
    (
        "Сильное",
        "Strong",
        &[
            (0.0, 1.0),
            (0.6, 1.03),
            (1.2, 1.9),
            (2.0, 5.0),
            (2.6, 9.0),
            (3.0, 10.0),
        ],
    ),
];

/// Where the logs put slow, careful motion and flicks of the ball.
const PRECISE_UP_TO: f64 = 1.2;
const FLICKS_FROM: f64 = 3.0;

/// A typical flick in the logs and the speed that stands for all of it.
const TYPICAL_FLICK_COUNTS: f64 = 315.0;
const TYPICAL_FLICK_SPEED: f64 = 2.7;

const MAX_SPEED: f64 = 6.0;
/// Gain runs on a log scale, so that doubling looks the same everywhere and
/// the area around 1, where precision lives, is not squeezed.
const MIN_GAIN: f64 = 0.25;
const MAX_GAIN: f64 = 16.0;
const GAIN_GRID: [f64; 5] = [0.5, 1.0, 2.0, 4.0, 8.0];

const SPEED_STEP: f64 = 0.05;
const GAIN_STEP: f64 = 0.05;
/// Points stay apart, because the curve needs speeds that strictly go up.
const MIN_SPEED_GAP: f64 = 0.1;
const MIN_POINTS: usize = 2;

const LEFT_MARGIN: f32 = 38.0;
const BOTTOM_MARGIN: f32 = 20.0;
const TOP_MARGIN: f32 = 4.0;
const GRAB_RADIUS: f32 = 14.0;
const CURVE_SAMPLES: usize = 120;

const BACKGROUND: Color32 = theme::BG;
const GRID: Color32 = Color32::from_rgb(0x2a, 0x2a, 0x2a);
const LABEL: Color32 = theme::TEXT3;
const CURVE: Color32 = theme::TEXT;
const POINT: Color32 = theme::TEXT;
const MARKER: Color32 = theme::WARN;
const PRECISE_ZONE: Color32 = Color32::from_rgba_premultiplied(9, 9, 9, 9);
const FLICK_ZONE: Color32 = Color32::from_rgba_premultiplied(15, 15, 15, 15);

/// Maps between curve values and places on the graph.
#[derive(Clone, Copy, Debug)]
struct Axes {
    plot: Rect,
}

impl Axes {
    fn new(full: Rect) -> Self {
        let mut plot = full;
        plot.min.x += LEFT_MARGIN;
        plot.min.y += TOP_MARGIN;
        plot.max.y -= BOTTOM_MARGIN;
        Self { plot }
    }

    fn x(&self, speed: f64) -> f32 {
        self.plot.left() + self.plot.width() * (speed / MAX_SPEED) as f32
    }

    fn y(&self, gain: f64) -> f32 {
        let share = (gain / MIN_GAIN).ln() / (MAX_GAIN / MIN_GAIN).ln();
        self.plot.bottom() - self.plot.height() * share as f32
    }

    fn position(&self, speed: f64, gain: f64) -> Pos2 {
        pos2(self.x(speed), self.y(gain))
    }

    fn value_at(&self, position: Pos2) -> (f64, f64) {
        let across = ((position.x - self.plot.left()) / self.plot.width()) as f64;
        let up = ((self.plot.bottom() - position.y) / self.plot.height()) as f64;
        let speed = across * MAX_SPEED;
        let gain = MIN_GAIN * (MAX_GAIN / MIN_GAIN).powf(up);
        (speed, gain)
    }
}

fn snap(value: f64, step: f64) -> f64 {
    (value / step).round() * step
}

fn nearest_point(points: &[(f64, f64)], axes: &Axes, position: Pos2) -> Option<usize> {
    points
        .iter()
        .enumerate()
        .map(|(index, &(speed, gain))| (index, axes.position(speed, gain).distance(position)))
        .filter(|&(_, distance)| distance <= GRAB_RADIUS)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(index, _)| index)
}

/// Moves a point, keeping it between its neighbours. Returns true if it moved.
fn move_point(points: &mut [(f64, f64)], index: usize, speed: f64, gain: f64) -> bool {
    let low = if index == 0 {
        0.0
    } else {
        points[index - 1].0 + MIN_SPEED_GAP
    };
    // The curve always starts at zero speed, or the area left of it would be
    // undefined on the graph.
    let high = match index {
        0 => 0.0,
        _ => points
            .get(index + 1)
            .map_or(MAX_SPEED, |next| next.0 - MIN_SPEED_GAP),
    };
    let moved = (
        snap(speed.clamp(low, high), SPEED_STEP).clamp(low, high),
        snap(gain.clamp(MIN_GAIN, MAX_GAIN), GAIN_STEP).clamp(MIN_GAIN, MAX_GAIN),
    );
    let changed = points[index] != moved;
    points[index] = moved;
    changed
}

/// Adds a point on the graph, unless it would crowd a neighbour.
fn add_point(points: &mut Vec<(f64, f64)>, speed: f64, gain: f64) -> bool {
    let speed = snap(speed.clamp(0.0, MAX_SPEED), SPEED_STEP);
    if points
        .iter()
        .any(|point| (point.0 - speed).abs() < MIN_SPEED_GAP)
    {
        return false;
    }
    let index = points.partition_point(|point| point.0 < speed);
    let gain = snap(gain.clamp(MIN_GAIN, MAX_GAIN), GAIN_STEP).clamp(MIN_GAIN, MAX_GAIN);
    points.insert(index, (speed, gain));
    true
}

/// The first point and the minimum number of points stay.
fn remove_point(points: &mut Vec<(f64, f64)>, index: usize) -> bool {
    if index == 0 || points.len() <= MIN_POINTS {
        return false;
    }
    points.remove(index);
    true
}

/// A plain summary of what the curve does.
pub fn describe(points: &[(f64, f64)], factor: f64, lang: Lang) -> String {
    let curve = Curve::Table(points.to_vec());
    let at = |speed: f64| curve.gain(speed) * factor;
    let slow = at(PRECISE_UP_TO / 2.0);
    let flick = at(TYPICAL_FLICK_SPEED);
    let pixels = TYPICAL_FLICK_COUNTS * flick;
    match lang {
        Lang::Ru => format!("Точно ×{slow:.1}, бросок ×{flick:.1}, примерно {pixels:.0} px за бросок"),
        Lang::En => format!("Precise ×{slow:.1}, flick ×{flick:.1}, about {pixels:.0} px per flick"),
    }
}

#[derive(Default)]
pub struct CurveEditor {
    dragging: Option<usize>,
}

impl CurveEditor {
    /// Shows the graph. Returns true if the curve changed.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        points: &mut Vec<(f64, f64)>,
        speed_now: f64,
        height: f32,
        lang: Lang,
    ) -> bool {
        let size = vec2(ui.available_width(), height);
        let (response, painter) = ui.allocate_painter(size, Sense::click_and_drag());
        let axes = Axes::new(response.rect);
        let mut changed = false;

        if response.drag_started() {
            let origin = ui.input(|input| input.pointer.press_origin());
            self.dragging = origin.and_then(|position| nearest_point(points, &axes, position));
        }
        if let (true, Some(index), Some(position)) = (
            response.dragged(),
            self.dragging,
            response.interact_pointer_pos(),
        ) {
            let (speed, gain) = axes.value_at(position);
            changed |= move_point(points, index, speed, gain);
        }
        if response.drag_stopped() {
            self.dragging = None;
        }
        if response.double_clicked() {
            if let Some(position) = response.interact_pointer_pos() {
                let (speed, gain) = axes.value_at(position);
                changed |= add_point(points, speed, gain);
            }
        }
        if response.secondary_clicked() {
            let hit = response
                .interact_pointer_pos()
                .and_then(|position| nearest_point(points, &axes, position));
            if let Some(index) = hit {
                changed |= remove_point(points, index);
            }
        }

        let hovered = self.dragging.or_else(|| {
            response
                .hover_pos()
                .and_then(|position| nearest_point(points, &axes, position))
        });
        draw(&painter, response.rect, &axes, points, hovered, speed_now, lang);
        changed
    }
}

fn draw(
    painter: &eframe::egui::Painter,
    full: Rect,
    axes: &Axes,
    points: &[(f64, f64)],
    highlighted: Option<usize>,
    speed_now: f64,
    lang: Lang,
) {
    let plot = axes.plot;
    paint_rounded(painter, full, RADIUS_CONTROL, BACKGROUND, theme::BORDER);
    draw_zones(painter, axes, lang);
    draw_grid(painter, axes, lang);

    let curve = Curve::Table(points.to_vec());
    let line: Vec<Pos2> = (0..=CURVE_SAMPLES)
        .map(|step| {
            let speed = MAX_SPEED * step as f64 / CURVE_SAMPLES as f64;
            axes.position(speed, curve.gain(speed).clamp(MIN_GAIN, MAX_GAIN))
        })
        .collect();
    painter.add(Shape::line(line, Stroke::new(2.0, CURVE)));

    for (index, &(speed, gain)) in points.iter().enumerate() {
        let radius = if highlighted == Some(index) { 7.0 } else { 5.0 };
        painter.circle_filled(axes.position(speed, gain), radius, POINT);
    }

    if speed_now > 0.05 {
        let speed = speed_now.min(MAX_SPEED);
        let x = axes.x(speed);
        painter.vline(x, plot.y_range(), Stroke::new(1.0, MARKER));
        let gain = curve.gain(speed).clamp(MIN_GAIN, MAX_GAIN);
        painter.circle_filled(axes.position(speed, gain), 4.0, MARKER);
    }

    if let Some(&(speed, gain)) = highlighted.and_then(|index| points.get(index)) {
        let position = axes.position(speed, gain) + vec2(10.0, -12.0);
        let align = if position.x > plot.right() - 100.0 {
            Align2::RIGHT_BOTTOM
        } else {
            Align2::LEFT_BOTTOM
        };
        let text = format!("×{gain:.2} · {speed:.2}");
        painter.text(position, align, text, FontId::proportional(13.0), POINT);
    }
}

fn draw_zones(painter: &eframe::egui::Painter, axes: &Axes, lang: Lang) {
    let plot = axes.plot;
    let small = FontId::proportional(11.0);

    let mut precise = plot;
    precise.max.x = axes.x(PRECISE_UP_TO);
    painter.rect_filled(precise, 0.0, PRECISE_ZONE);
    let top_left = precise.left_top() + vec2(4.0, 3.0);
    let label = lang.tr("точно", "precise");
    painter.text(top_left, Align2::LEFT_TOP, label, small.clone(), LABEL);

    let mut flicks = plot;
    flicks.min.x = axes.x(FLICKS_FROM);
    painter.rect_filled(flicks, 0.0, FLICK_ZONE);
    let top_right = flicks.right_top() + vec2(-4.0, 3.0);
    painter.text(top_right, Align2::RIGHT_TOP, lang.tr("броски", "flicks"), small, LABEL);
}

fn draw_grid(painter: &eframe::egui::Painter, axes: &Axes, lang: Lang) {
    let plot = axes.plot;
    let font = FontId::proportional(11.0);
    for gain in GAIN_GRID {
        let y = axes.y(gain);
        let width = if gain == 1.0 { 1.5 } else { 1.0 };
        painter.hline(plot.x_range(), y, Stroke::new(width, GRID));
        let label_at = pos2(plot.left() - 8.0, y);
        let text = format!("×{gain}");
        painter.text(label_at, Align2::RIGHT_CENTER, text, font.clone(), LABEL);
    }
    let bottom = plot.bottom() + 3.0;
    let middle = pos2(plot.center().x, bottom);
    let speed = lang.tr("скорость", "speed");
    painter.text(middle, Align2::CENTER_TOP, speed, font.clone(), LABEL);
    let slow = lang.tr("медленно", "slow");
    painter.text(pos2(plot.left(), bottom), Align2::LEFT_TOP, slow, font.clone(), LABEL);
    let fast = lang.tr("быстро", "fast");
    painter.text(pos2(plot.right() - 4.0, bottom), Align2::RIGHT_TOP, fast, font, LABEL);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn axes() -> Axes {
        Axes::new(Rect::from_min_size(pos2(10.0, 20.0), vec2(400.0, 210.0)))
    }

    fn sample() -> Vec<(f64, f64)> {
        vec![(0.0, 1.0), (2.0, 2.0), (4.0, 4.0)]
    }

    #[test]
    fn ready_curves_are_valid_tables() {
        for (name, _, points) in READY_CURVES {
            assert_eq!(points[0].0, 0.0, "{name}");
            assert!(points.windows(2).all(|p| p[0].0 < p[1].0), "{name}");
            let text: Vec<String> = points.iter().map(|(s, g)| format!("{s}/{g}")).collect();
            let spec = format!("accel:type=table,points={}", text.join(";"));
            ilumx_core::spec::parse_spec(&spec).unwrap();
        }
    }

    #[test]
    fn ready_curves_leave_slow_motion_alone() {
        for (name, _, points) in READY_CURVES {
            let curve = Curve::Table(points.to_vec());
            assert!(curve.gain(0.5) < 1.05, "{name}: {}", curve.gain(0.5));
        }
    }

    #[test]
    fn places_on_the_graph_map_back_to_values() {
        let axes = axes();
        for (speed, gain) in [(0.0, 1.0), (3.0, 2.5), (6.0, 12.0), (1.5, 0.25)] {
            let (s, g) = axes.value_at(axes.position(speed, gain));
            assert!((s - speed).abs() < 1e-4, "{s}");
            assert!((g - gain).abs() < 1e-4, "{g}");
        }
    }

    #[test]
    fn higher_gain_is_higher_on_the_graph() {
        let axes = axes();
        assert!(axes.y(4.0) < axes.y(1.0));
    }

    #[test]
    fn finds_the_point_under_the_pointer() {
        let axes = axes();
        let points = sample();
        let near = axes.position(2.0, 2.0) + vec2(5.0, 5.0);
        assert_eq!(nearest_point(&points, &axes, near), Some(1));
        let far = axes.position(3.0, 8.0);
        assert_eq!(nearest_point(&points, &axes, far), None);
    }

    #[test]
    fn a_point_moves_and_snaps_to_steps() {
        let mut points = sample();
        assert!(move_point(&mut points, 1, 2.52, 3.03));
        assert!((points[1].0 - 2.5).abs() < 1e-9);
        assert!((points[1].1 - 3.05).abs() < 1e-9);
    }

    #[test]
    fn a_point_cannot_pass_its_neighbours() {
        let mut points = sample();
        move_point(&mut points, 1, 5.0, 2.0);
        assert!(points[1].0 < points[2].0);
        move_point(&mut points, 1, -1.0, 2.0);
        assert!(points[1].0 > points[0].0);
    }

    #[test]
    fn the_first_point_stays_at_zero_speed() {
        let mut points = sample();
        move_point(&mut points, 0, 1.0, 1.5);
        assert_eq!(points[0], (0.0, 1.5));
    }

    #[test]
    fn gain_stays_inside_the_graph() {
        let mut points = sample();
        move_point(&mut points, 1, 2.0, 100.0);
        assert_eq!(points[1].1, MAX_GAIN);
        move_point(&mut points, 1, 2.0, 0.0);
        assert_eq!(points[1].1, MIN_GAIN);
    }

    #[test]
    fn a_point_left_alone_reports_no_change() {
        let mut points = sample();
        assert!(!move_point(&mut points, 1, 2.0, 2.0));
    }

    #[test]
    fn added_points_keep_the_order() {
        let mut points = sample();
        assert!(add_point(&mut points, 3.0, 3.0));
        assert_eq!(points[2].0, 3.0);
        assert!(points.windows(2).all(|p| p[0].0 < p[1].0));
    }

    #[test]
    fn a_point_cannot_land_on_a_neighbour() {
        let mut points = sample();
        assert!(!add_point(&mut points, 2.0, 5.0));
        assert_eq!(points.len(), 3);
    }

    #[test]
    fn the_curve_keeps_its_start_and_two_points() {
        let mut points = sample();
        assert!(!remove_point(&mut points, 0));
        assert!(remove_point(&mut points, 1));
        assert!(!remove_point(&mut points, 1));
        assert_eq!(points.len(), 2);
    }

    #[test]
    fn edited_curves_stay_valid_for_the_settings_file() {
        let mut points = sample();
        add_point(&mut points, 1.0, 1.1);
        move_point(&mut points, 2, 1.0, 3.0);
        let curve = Curve::Table(points.clone());
        let config = ilumx_core::pipeline::PipelineConfig {
            curve,
            ..Default::default()
        };
        let text = ilumx_core::spec::format_spec(&config);
        assert_eq!(ilumx_core::spec::parse_spec(&text).unwrap().curve, config.curve);
    }

    #[test]
    fn the_summary_counts_the_speed_multiplier_in() {
        let text = describe(&[(0.0, 2.0), (6.0, 2.0)], 0.5, Lang::Ru);
        assert!(text.contains("×1.0"), "{text}");
        assert!(text.contains("315 px"), "{text}");
        assert!(describe(&[(0.0, 1.0), (6.0, 1.0)], 1.0, Lang::En).contains("flick"));
    }
}
