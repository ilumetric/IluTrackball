//! The test area: shows the motion before and after the filter.
//!
//! It reads the same motion the filter works on, so modes can be compared here
//! without touching the real cursor.

use std::collections::VecDeque;

use eframe::egui::{pos2, vec2, Align2, Color32, Pos2, Rect, Sense, Shape, Stroke, Ui};

use crate::i18n::Lang;
use crate::tap::{Flow, TapSample};
use crate::theme::{self, paint_rounded, BG, BORDER, OUT_LINE, RADIUS_CONTROL, RAW_LINE};

/// Motion kept for the trails and the chart.
const KEEP_US: u64 = 2_000_000;
const TRAIL_US: u64 = 350_000;
const CHART_US: u64 = 1_500_000;
/// One Bluetooth packet interval, so a step of the chart is one packet.
const STEP_US: u64 = 8_750;
/// The part of the test card that goes to the area, the chart gets the rest.
const AREA_SHARE: f32 = 0.54;
const LEGEND_GAP: f32 = 12.0;
/// Time the current speed of the ball is measured over.
const SPEED_WINDOW_US: u64 = 40_000;
/// The chart keeps this scale even for small movements.
const MIN_CHART_RANGE: f32 = 6.0;

const RAW_COLOR: Color32 = RAW_LINE;
const OUT_COLOR: Color32 = OUT_LINE;
const GRID: Color32 = Color32::from_rgb(0x2a, 0x2a, 0x2a);

/// Positions along one side of the filter.
#[derive(Default)]
struct Path {
    points: VecDeque<(u64, f32, f32)>,
    x: f32,
    y: f32,
}

impl Path {
    fn add(&mut self, t_us: u64, dx: i32, dy: i32) {
        self.x += dx as f32;
        self.y += dy as f32;
        self.points.push_back((t_us, self.x, self.y));
    }

    fn forget_before(&mut self, t_us: u64) {
        while self.points.front().is_some_and(|point| point.0 < t_us) {
            self.points.pop_front();
        }
    }

    /// Distance moved since `from_us`.
    fn travel_since(&self, from_us: u64) -> f32 {
        let mut travel = 0.0;
        let mut previous: Option<(f32, f32)> = None;
        for &(t_us, x, y) in &self.points {
            if let (true, Some((px, py))) = (t_us >= from_us, previous) {
                travel += (x - px).hypot(y - py);
            }
            previous = Some((x, y));
        }
        travel
    }

    /// Distance moved during each packet interval of the last `CHART_US`.
    fn steps(&self, now_us: u64) -> Vec<f32> {
        let count = (CHART_US / STEP_US) as usize;
        let start = now_us.saturating_sub(CHART_US);
        let mut steps = vec![0.0; count];
        let mut previous: Option<(f32, f32)> = None;
        for &(t_us, x, y) in &self.points {
            if let Some((px, py)) = previous {
                if t_us >= start {
                    let bin = ((t_us - start) / STEP_US) as usize;
                    if let Some(step) = steps.get_mut(bin) {
                        *step += (x - px).hypot(y - py);
                    }
                }
            }
            previous = Some((x, y));
        }
        steps
    }
}

#[derive(Default)]
pub struct Preview {
    raw: Path,
    out: Path,
    latest_us: u64,
}

impl Preview {
    pub fn add(&mut self, samples: &[TapSample]) {
        for sample in samples {
            self.latest_us = self.latest_us.max(sample.t_us);
            let path = match sample.flow {
                Flow::Input => &mut self.raw,
                Flow::Output => &mut self.out,
            };
            path.add(sample.t_us, sample.dx, sample.dy);
        }
        let keep_from = self.latest_us.saturating_sub(KEEP_US);
        self.raw.forget_before(keep_from);
        self.out.forget_before(keep_from);
    }

    /// How fast the ball is turning right now, in counts per millisecond.
    pub fn speed(&self, now_us: u64) -> f64 {
        let from_us = now_us.saturating_sub(SPEED_WINDOW_US);
        self.raw.travel_since(from_us) as f64 * 1000.0 / SPEED_WINDOW_US as f64
    }

    /// What the two colours stand for, ending at `end`.
    pub fn legend(ui: &Ui, end: Pos2, lang: Lang) {
        let font = theme::font(12.0);
        let after = lang.tr("после фильтра", "filtered");
        let painted = ui
            .painter()
            .text(end, Align2::RIGHT_CENTER, after, font.clone(), OUT_COLOR);
        let before = lang.tr("исходное", "raw");
        let place = pos2(painted.left() - LEGEND_GAP, end.y);
        ui.painter()
            .text(place, Align2::RIGHT_CENTER, before, font, RAW_COLOR);
    }

    /// Fills the height it is given, which grows when the curve is put away.
    pub fn show(&self, ui: &mut Ui, now_us: u64) {
        let room = ui.available_height() - ui.spacing().item_spacing.y;
        let area_height = room * AREA_SHARE;
        self.show_area(ui, now_us, area_height);
        self.show_chart(ui, now_us, room - area_height);
    }

    fn show_area(&self, ui: &mut Ui, now_us: u64, height: f32) {
        let size = vec2(ui.available_width(), height);
        let (response, painter) = ui.allocate_painter(size, Sense::hover());
        let rect = response.rect;
        paint_rounded(&painter, rect, RADIUS_CONTROL, BG, BORDER);

        let show = |path: &Path, color: Color32| {
            let trail = wrapped_trail(path, rect, now_us.saturating_sub(TRAIL_US));
            for piece in trail {
                painter.add(Shape::line(piece, Stroke::new(1.5, color.gamma_multiply(0.7))));
            }
            painter.circle_filled(wrap(path.x, path.y, rect), 4.5, color);
        };
        show(&self.raw, RAW_COLOR);
        show(&self.out, OUT_COLOR);
    }

    fn show_chart(&self, ui: &mut Ui, now_us: u64, height: f32) {
        let size = vec2(ui.available_width(), height);
        let (response, painter) = ui.allocate_painter(size, Sense::hover());
        let rect = response.rect;
        paint_rounded(&painter, rect, RADIUS_CONTROL, BG, BORDER);

        let raw = self.raw.steps(now_us);
        let out = self.out.steps(now_us);
        let top = raw
            .iter()
            .chain(&out)
            .fold(MIN_CHART_RANGE, |top, &step| top.max(step));

        for line in 1..=2 {
            let y = rect.bottom() - rect.height() * line as f32 / 3.0;
            painter.hline(rect.x_range(), y, Stroke::new(1.0, GRID));
        }
        let plot = |steps: &[f32], color: Color32| {
            let last = (steps.len() - 1).max(1) as f32;
            let points: Vec<Pos2> = steps
                .iter()
                .enumerate()
                .map(|(i, &step)| {
                    let x = rect.left() + rect.width() * i as f32 / last;
                    let y = rect.bottom() - 3.0 - (rect.height() - 8.0) * step / top;
                    pos2(x, y)
                })
                .collect();
            painter.add(Shape::line(points, Stroke::new(1.3, color)));
        };
        plot(&raw, RAW_COLOR);
        plot(&out, OUT_COLOR);
    }
}

fn wrap(x: f32, y: f32, rect: Rect) -> Pos2 {
    let origin = rect.center();
    let px = (x + origin.x - rect.left()).rem_euclid(rect.width());
    let py = (y + origin.y - rect.top()).rem_euclid(rect.height());
    pos2(rect.left() + px, rect.top() + py)
}

/// The recent path, split where it jumps across an edge of the area.
fn wrapped_trail(path: &Path, rect: Rect, from_us: u64) -> Vec<Vec<Pos2>> {
    let mut pieces = Vec::new();
    let mut piece: Vec<Pos2> = Vec::new();
    for &(t_us, x, y) in &path.points {
        if t_us < from_us {
            continue;
        }
        let point = wrap(x, y, rect);
        let jumped = piece.last().is_some_and(|last: &Pos2| {
            (last.x - point.x).abs() > rect.width() / 2.0
                || (last.y - point.y).abs() > rect.height() / 2.0
        });
        if jumped {
            pieces.push(std::mem::take(&mut piece));
        }
        piece.push(point);
    }
    pieces.push(piece);
    pieces.retain(|piece| piece.len() > 1);
    pieces
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(t_us: u64, flow: Flow, dx: i32) -> TapSample {
        TapSample {
            t_us,
            flow,
            dx,
            dy: 0,
        }
    }

    fn area() -> Rect {
        Rect::from_min_size(pos2(10.0, 20.0), vec2(100.0, 50.0))
    }

    #[test]
    fn positions_stay_inside_the_area() {
        let rect = area();
        for (x, y) in [(0.0, 0.0), (1234.0, -987.0), (-50.0, 75.0)] {
            assert!(rect.contains(wrap(x, y, rect)), "{x} {y}");
        }
    }

    #[test]
    fn the_start_is_the_middle_of_the_area() {
        assert_eq!(wrap(0.0, 0.0, area()), area().center());
    }

    #[test]
    fn steps_add_up_per_packet_interval() {
        let mut preview = Preview::default();
        preview.add(&[
            sample(100_000, Flow::Input, 0),
            sample(1_000_000, Flow::Input, 3),
            sample(1_001_000, Flow::Input, 4),
        ]);
        let steps = preview.raw.steps(1_100_000);
        assert_eq!(steps.iter().sum::<f32>(), 7.0);
        assert_eq!(steps.iter().cloned().fold(0.0, f32::max), 7.0);
    }

    #[test]
    fn speed_counts_only_recent_motion() {
        let mut preview = Preview::default();
        preview.add(&[
            sample(1_000_000, Flow::Input, 0),
            sample(1_010_000, Flow::Input, 20),
            sample(1_020_000, Flow::Input, 20),
        ]);
        assert_eq!(preview.speed(1_030_000), 1.0);
        assert_eq!(preview.speed(5_000_000), 0.0);
    }

    #[test]
    fn old_motion_is_forgotten() {
        let mut preview = Preview::default();
        preview.add(&[sample(1, Flow::Output, 1)]);
        preview.add(&[sample(KEEP_US + 100, Flow::Output, 1)]);
        assert_eq!(preview.out.points.len(), 1);
    }

    #[test]
    fn a_trail_breaks_where_it_crosses_an_edge() {
        let mut preview = Preview::default();
        let samples: Vec<TapSample> = (1..30).map(|i| sample(i, Flow::Input, 4)).collect();
        preview.add(&samples);
        let pieces = wrapped_trail(&preview.raw, area(), 0);
        assert!(pieces.len() >= 2, "{pieces:?}");
    }
}
