//! The look of the window: the dark palette of PeekSwitcher, its sizes, and the
//! superellipse that every rounded shape is built from.

use std::f32::consts::FRAC_PI_2;
use std::fs;
use std::sync::Arc;

use eframe::egui::{
    self, Color32, FontData, FontDefinitions, FontFamily, FontId, Painter, Pos2, Rect, Shape, Stroke,
};

pub const BG: Color32 = Color32::from_rgb(0x0d, 0x0d, 0x0d);
pub const CARD: Color32 = Color32::from_rgb(0x1b, 0x1b, 0x1b);
pub const SURFACE: Color32 = Color32::from_rgb(0x26, 0x26, 0x26);
pub const RAISED: Color32 = Color32::from_rgb(0x33, 0x33, 0x33);
pub const HOVER: Color32 = Color32::from_rgba_premultiplied(15, 15, 15, 15);
pub const BORDER: Color32 = Color32::from_rgba_premultiplied(26, 26, 26, 26);
pub const TEXT: Color32 = Color32::from_rgb(0xf3, 0xf3, 0xf3);
pub const TEXT2: Color32 = Color32::from_rgb(0xb4, 0xb4, 0xb4);
pub const TEXT3: Color32 = Color32::from_rgb(0x8a, 0x8a, 0x8a);
pub const SWITCH_OFF: Color32 = Color32::from_rgb(0x3d, 0x3d, 0x3d);
pub const KNOB_OFF: Color32 = Color32::from_rgb(0xcf, 0xcf, 0xcf);
pub const WARN: Color32 = Color32::from_rgb(0xff, 0xc8, 0x61);
pub const WARN_BG: Color32 = Color32::from_rgba_premultiplied(26, 18, 3, 26);
pub const GOOD: Color32 = Color32::from_rgb(0x7f, 0xd6, 0xa0);

/// The two lines of the test area: what came in stays in the background, what
/// the filter made of it stands out.
pub const RAW_LINE: Color32 = Color32::from_rgb(0x5e, 0x5e, 0x5e);
pub const OUT_LINE: Color32 = Color32::from_rgb(0x4a, 0xa3, 0xff);

pub const RADIUS_CARD: f32 = 16.0;
pub const RADIUS_CONTROL: f32 = 10.0;
pub const WINDOW_MARGIN: f32 = 16.0;
pub const GAP: f32 = 12.0;
pub const CARD_PADDING: f32 = 12.0;
pub const ROW_HEIGHT: f32 = 34.0;
/// Between one row of a column and the next.
pub const ROW_GAP: f32 = 4.0;

const FONT_FOLDER: &str = "C:\\Windows\\Fonts\\";
const SEMIBOLD_FAMILY: &str = "semibold";

/// Points along one corner. Enough to stay smooth on a 4K screen at 150%.
const CORNER_STEPS: usize = 14;
/// 2 would be a circle. Higher gives the longer, softer shoulders of a
/// superellipse.
const SUPERELLIPSE_POWER: f32 = 4.0;
/// Points closer than this count as one. The painter takes the direction of the
/// outline from its segments, and one of zero length makes a spike.
const MIN_SEGMENT: f32 = 0.05;

/// The outline of a rounded rectangle with superellipse corners, clockwise from
/// the top right. A radius of half the height gives a capsule.
pub fn superellipse(rect: Rect, radius: f32) -> Vec<Pos2> {
    let radius = radius.min(rect.width() / 2.0).min(rect.height() / 2.0).max(0.0);
    let corners = [
        (rect.right() - radius, rect.top() + radius, 0.0),
        (rect.right() - radius, rect.bottom() - radius, 1.0),
        (rect.left() + radius, rect.bottom() - radius, 2.0),
        (rect.left() + radius, rect.top() + radius, 3.0),
    ];
    let exponent = 2.0 / SUPERELLIPSE_POWER;

    let mut points = Vec::with_capacity(corners.len() * (CORNER_STEPS + 1));
    for (center_x, center_y, quarter) in corners {
        for step in 0..=CORNER_STEPS {
            let along = step as f32 / CORNER_STEPS as f32;
            let angle = (quarter + along) * FRAC_PI_2 - FRAC_PI_2;
            let (sin, cos) = angle.sin_cos();
            let point = Pos2::new(
                center_x + radius * cos.abs().powf(exponent) * cos.signum(),
                center_y + radius * sin.abs().powf(exponent) * sin.signum(),
            );
            if points
                .last()
                .is_none_or(|last: &Pos2| last.distance(point) >= MIN_SEGMENT)
            {
                points.push(point);
            }
        }
    }
    // The outline closes on itself, so its ends are neighbours too.
    while points.len() > 1 && points[0].distance(points[points.len() - 1]) < MIN_SEGMENT {
        points.pop();
    }
    points
}

pub fn rounded(rect: Rect, radius: f32, fill: Color32, border: Color32) -> Shape {
    Shape::convex_polygon(superellipse(rect, radius), fill, Stroke::new(1.0, border))
}

pub fn paint_rounded(painter: &Painter, rect: Rect, radius: f32, fill: Color32, border: Color32) {
    painter.add(rounded(rect, radius, fill, border));
}

pub fn font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SEMIBOLD_FAMILY.into()))
}

fn add_system_font(fonts: &mut FontDefinitions, name: &str, file: &str) -> bool {
    match fs::read(format!("{FONT_FOLDER}{file}")) {
        Ok(bytes) => {
            fonts
                .font_data
                .insert(name.to_string(), Arc::new(FontData::from_owned(bytes)));
            true
        }
        Err(_) => false,
    }
}

/// Segoe UI, as in PeekSwitcher. The built-in font stays behind it as a fallback.
pub fn install(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let fallback = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();

    let mut regular = fallback.clone();
    if add_system_font(&mut fonts, "segoe", "segoeui.ttf") {
        regular.insert(0, "segoe".to_string());
    }
    let mut strong = fallback;
    if add_system_font(&mut fonts, "segoe-semibold", "seguisb.ttf") {
        strong.insert(0, "segoe-semibold".to_string());
    }
    fonts.families.insert(FontFamily::Proportional, regular);
    fonts
        .families
        .insert(FontFamily::Name(SEMIBOLD_FAMILY.into()), strong);
    ctx.set_fonts(fonts);

    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = BG;
    visuals.window_fill = CARD;
    visuals.extreme_bg_color = BG;
    visuals.override_text_color = Some(TEXT);
    visuals.selection.bg_fill = RAISED;
    visuals.selection.stroke = Stroke::new(1.0, TEXT);
    visuals.text_cursor.stroke = Stroke::new(1.5, TEXT);
    visuals.window_stroke = Stroke::new(1.0, BORDER);
    ctx.set_visuals(visuals);
    ctx.global_style_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, ROW_GAP);
        style.spacing.scroll = egui::style::ScrollStyle::thin();
        style.interaction.tooltip_delay = 0.4;
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{pos2, vec2};

    fn rect() -> Rect {
        Rect::from_min_size(pos2(10.0, 20.0), vec2(100.0, 40.0))
    }

    #[test]
    fn the_outline_stays_inside_the_rectangle() {
        let bounds = rect().expand(0.01);
        for point in superellipse(rect(), 12.0) {
            assert!(bounds.contains(point), "{point:?}");
        }
    }

    #[test]
    fn the_outline_touches_every_side() {
        let points = superellipse(rect(), 12.0);
        let near = |value: f32, side: f32| (value - side).abs() < 0.01;
        assert!(points.iter().any(|p| near(p.x, rect().left())));
        assert!(points.iter().any(|p| near(p.x, rect().right())));
        assert!(points.iter().any(|p| near(p.y, rect().top())));
        assert!(points.iter().any(|p| near(p.y, rect().bottom())));
    }

    #[test]
    fn the_outline_is_convex() {
        let points = superellipse(rect(), 12.0);
        let count = points.len();
        for i in 0..count {
            let (a, b, c) = (points[i], points[(i + 1) % count], points[(i + 2) % count]);
            let turn = (b - a).x * (c - b).y - (b - a).y * (c - b).x;
            assert!(turn >= -1e-3, "turns the wrong way at {i}: {turn}");
        }
    }

    #[test]
    fn no_two_neighbouring_points_coincide() {
        // A capsule and a circle have sides of zero length.
        for (size, radius) in [(vec2(36.0, 20.0), 10.0), (vec2(14.0, 14.0), 7.0), (vec2(80.0, 4.0), 2.0)] {
            let points = superellipse(Rect::from_min_size(pos2(5.0, 5.0), size), radius);
            let count = points.len();
            for i in 0..count {
                let distance = points[i].distance(points[(i + 1) % count]);
                assert!(distance >= MIN_SEGMENT, "{size:?} at {i}: {distance}");
            }
        }
    }

    #[test]
    fn corners_are_fuller_than_a_circle() {
        // At 45 degrees a superellipse sits further out than a circular arc.
        let radius = 20.0;
        let area = Rect::from_min_size(pos2(0.0, 0.0), vec2(40.0, 40.0));
        let reach = superellipse(area, radius)
            .iter()
            .map(|p| (*p - area.center()).length())
            .fold(0.0, f32::max);
        assert!(reach > radius * 1.05, "{reach}");
    }

    #[test]
    fn a_large_radius_gives_a_capsule_without_spilling_over() {
        let bounds = rect().expand(0.01);
        for point in superellipse(rect(), 500.0) {
            assert!(bounds.contains(point));
        }
    }
}
