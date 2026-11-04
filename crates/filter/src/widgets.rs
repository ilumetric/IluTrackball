//! The controls of the window, drawn by hand so that all of them share the
//! same superellipse shape and the same rows and columns.

use std::f32::consts::TAU;

use eframe::egui::{
    self, pos2, vec2, Align2, Color32, CursorIcon, Id, Pos2, Rect, Response, Sense, Shape, Stroke,
    Ui, UiBuilder, Vec2,
};

use crate::theme::{
    self, font, paint_rounded, semibold, BORDER, CARD, CARD_PADDING, HOVER, KNOB_OFF, RADIUS_CARD,
    RADIUS_CONTROL, RAISED, ROW_HEIGHT, SURFACE, SWITCH_OFF, TEXT, TEXT2, TEXT3,
};

const TOGGLE_SIZE: Vec2 = vec2(36.0, 20.0);
const TOGGLE_KNOB: f32 = 14.0;
const SLIDER_ROW_HEIGHT: f32 = 28.0;
const SLIDER_LABEL_WIDTH: f32 = 112.0;
const SLIDER_VALUE_WIDTH: f32 = 46.0;
const SLIDER_KNOB: f32 = 14.0;
const SLIDER_TRACK: f32 = 4.0;
pub const SEGMENT_HEIGHT: f32 = 30.0;
const ICON_BUTTON: f32 = 30.0;
const TOGGLE_ANIMATION: f32 = 0.15;
const GEAR_TEETH: usize = 8;
const GEAR_TIP: f32 = 10.5;
const GEAR_ROOT: f32 = 7.8;
const GEAR_HOLE: f32 = 3.2;
const MOUSE_ITEM_HEIGHT: f32 = 42.0;
pub const SECTION_HEIGHT: f32 = 20.0;
/// A heading and the gap to its card, as `section` leaves them in a column.
pub const SECTION_STEP: f32 = SECTION_HEIGHT + theme::ROW_GAP;

/// A card: a rounded background behind whatever `content` adds.
pub fn card<R>(ui: &mut Ui, content: impl FnOnce(&mut Ui) -> R) -> R {
    let slot = ui.painter().add(Shape::Noop);
    let outer = ui.available_rect_before_wrap();
    let inner_rect = outer.shrink(CARD_PADDING);
    let mut inner = ui.new_child(UiBuilder::new().max_rect(inner_rect));
    let result = content(&mut inner);

    let bottom = inner.min_rect().bottom() + CARD_PADDING;
    let rect = Rect::from_min_max(outer.min, pos2(outer.right(), bottom));
    ui.painter().set(slot, theme::rounded(rect, RADIUS_CARD, CARD, BORDER));
    ui.allocate_rect(rect, Sense::hover());
    result
}

/// A card that fills a given rectangle.
pub fn card_in<R>(ui: &mut Ui, rect: Rect, content: impl FnOnce(&mut Ui) -> R) -> R {
    paint_rounded(ui.painter(), rect, RADIUS_CARD, CARD, BORDER);
    let mut inner = ui.new_child(UiBuilder::new().max_rect(rect.shrink(CARD_PADDING)));
    content(&mut inner)
}

/// The small grey heading above a card.
pub fn section(ui: &mut Ui, text: &str) {
    let size = vec2(ui.available_width(), SECTION_HEIGHT);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    paint_section(ui, rect, text);
}

/// The same heading for a section placed by hand. Returns the room for its card.
pub fn section_in(ui: &Ui, rect: Rect, text: &str) -> Rect {
    let heading = Rect::from_min_size(rect.min, vec2(rect.width(), SECTION_HEIGHT));
    paint_section(ui, heading, text);
    Rect::from_min_max(pos2(rect.left(), rect.top() + SECTION_STEP), rect.max)
}

fn paint_section(ui: &Ui, rect: Rect, text: &str) {
    ui.painter().text(
        pos2(rect.left() + 2.0, rect.center().y),
        Align2::LEFT_CENTER,
        text,
        semibold(12.0),
        TEXT3,
    );
}

pub fn small_text(ui: &mut Ui, text: &str, color: Color32) {
    ui.add(egui::Label::new(egui::RichText::new(text).font(font(12.0)).color(color)).wrap());
}

fn paint_toggle(ui: &Ui, center: Pos2, on: bool, id: Id) {
    let rect = Rect::from_center_size(center, TOGGLE_SIZE);
    let share = ui.ctx().animate_bool_with_time(id, on, TOGGLE_ANIMATION);
    let radius = rect.height() / 2.0;
    let track = SWITCH_OFF.lerp_to_gamma(TEXT, share);
    paint_rounded(ui.painter(), rect, radius, track, Color32::TRANSPARENT);

    let travel = rect.width() - TOGGLE_KNOB - 6.0;
    let knob_center = pos2(
        rect.left() + 3.0 + TOGGLE_KNOB / 2.0 + travel * share,
        rect.center().y,
    );
    let knob = Rect::from_center_size(knob_center, Vec2::splat(TOGGLE_KNOB));
    let color = KNOB_OFF.lerp_to_gamma(theme::BG, share);
    paint_rounded(ui.painter(), knob, TOGGLE_KNOB / 2.0, color, Color32::TRANSPARENT);
}

/// A row with a label on the left and a switch on the right. Returns true if
/// the switch changed.
pub fn toggle_row(ui: &mut Ui, label: &str, value: &mut bool, hint: Option<&str>) -> bool {
    let size = vec2(ui.available_width(), ROW_HEIGHT);
    let (rect, mut response) = ui.allocate_exact_size(size, Sense::click());
    if response.hovered() {
        response = response.on_hover_cursor(CursorIcon::PointingHand);
    }
    if let Some(hint) = hint {
        response = response.on_hover_text(hint);
    }
    if response.clicked() {
        *value = !*value;
    }
    ui.painter().text(
        pos2(rect.left(), rect.center().y),
        Align2::LEFT_CENTER,
        label,
        font(14.0),
        TEXT,
    );
    let center = pos2(rect.right() - TOGGLE_SIZE.x / 2.0, rect.center().y);
    paint_toggle(ui, center, *value, response.id);
    response.clicked()
}

/// A bare switch at the right end of `row`, for use inside other rows.
pub fn toggle_end(ui: &mut Ui, row: Rect, value: &mut bool, id: Id, hint: Option<&str>) -> bool {
    let center = row.right_center() - vec2(TOGGLE_SIZE.x / 2.0, 0.0);
    let rect = Rect::from_center_size(center, TOGGLE_SIZE);
    let mut response = ui.interact(rect, id, Sense::click());
    if response.hovered() {
        response = response.on_hover_cursor(CursorIcon::PointingHand);
    }
    if let Some(hint) = hint {
        response = response.on_hover_text(hint);
    }
    if response.clicked() {
        *value = !*value;
    }
    paint_toggle(ui, center, *value, id);
    response.clicked()
}

fn to_share(value: f64, low: f64, high: f64, logarithmic: bool) -> f32 {
    let share = if logarithmic {
        (value / low).ln() / (high / low).ln()
    } else {
        (value - low) / (high - low)
    };
    share.clamp(0.0, 1.0) as f32
}

fn from_share(share: f32, low: f64, high: f64, logarithmic: bool) -> f64 {
    let share = f64::from(share.clamp(0.0, 1.0));
    if logarithmic {
        low * (high / low).powf(share)
    } else {
        low + (high - low) * share
    }
}

/// How a slider row shows and steps its value.
#[derive(Clone, Copy)]
pub struct SliderSpec<'a> {
    pub low: f64,
    pub high: f64,
    pub step: f64,
    pub logarithmic: bool,
    pub decimals: usize,
    pub unit: &'a str,
}

/// A row of label, slider and value, in fixed columns so that rows line up.
pub fn slider_row(ui: &mut Ui, label: &str, value: &mut f64, spec: SliderSpec) -> bool {
    let size = vec2(ui.available_width(), SLIDER_ROW_HEIGHT);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let id = ui.id().with(label);

    ui.painter().text(
        pos2(rect.left(), rect.center().y),
        Align2::LEFT_CENTER,
        label,
        font(13.0),
        TEXT2,
    );
    let text = format!("{:.*}{}", spec.decimals, *value, spec.unit);
    ui.painter().text(
        pos2(rect.right(), rect.center().y),
        Align2::RIGHT_CENTER,
        text,
        font(13.0),
        TEXT,
    );

    let track = Rect::from_min_max(
        pos2(rect.left() + SLIDER_LABEL_WIDTH, rect.top()),
        pos2(rect.right() - SLIDER_VALUE_WIDTH, rect.bottom()),
    );
    let response = ui.interact(track, id, Sense::click_and_drag());
    let response = if response.hovered() {
        response.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        response
    };

    let usable = track.shrink2(vec2(SLIDER_KNOB / 2.0, 0.0));
    let mut changed = false;
    if let Some(pointer) = response.interact_pointer_pos() {
        if response.dragged() || response.clicked() {
            let share = (pointer.x - usable.left()) / usable.width();
            let raw = from_share(share, spec.low, spec.high, spec.logarithmic);
            let stepped = ((raw / spec.step).round() * spec.step).clamp(spec.low, spec.high);
            changed = (stepped - *value).abs() > f64::EPSILON;
            *value = stepped;
        }
    }

    let share = to_share(*value, spec.low, spec.high, spec.logarithmic);
    let y = track.center().y;
    let knob_x = usable.left() + usable.width() * share;
    let line = |from: f32, to: f32, color: Color32| {
        let bar = Rect::from_min_max(
            pos2(from, y - SLIDER_TRACK / 2.0),
            pos2(to, y + SLIDER_TRACK / 2.0),
        );
        paint_rounded(ui.painter(), bar, SLIDER_TRACK / 2.0, color, Color32::TRANSPARENT);
    };
    line(usable.left(), usable.right(), SWITCH_OFF);
    line(usable.left(), knob_x, TEXT);
    let knob = Rect::from_center_size(pos2(knob_x, y), Vec2::splat(SLIDER_KNOB));
    paint_rounded(ui.painter(), knob, SLIDER_KNOB / 2.0, TEXT, Color32::TRANSPARENT);
    changed
}

/// A capsule with one choice picked. Returns the choice if it changed.
pub fn segmented(ui: &mut Ui, id: &str, labels: &[&str], current: usize) -> Option<usize> {
    let widths: Vec<f32> = labels
        .iter()
        .map(|label| {
            let galley =
                ui.painter()
                    .layout_no_wrap(label.to_string(), semibold(13.0), Color32::WHITE);
            galley.size().x + 24.0
        })
        .collect();
    let total: f32 = widths.iter().sum::<f32>() + 6.0 + (labels.len() - 1) as f32 * 2.0;
    let (rect, _) = ui.allocate_exact_size(vec2(total, SEGMENT_HEIGHT), Sense::hover());
    paint_rounded(ui.painter(), rect, SEGMENT_HEIGHT / 2.0, CARD, BORDER);

    let mut chosen = None;
    let mut x = rect.left() + 3.0;
    for (index, label) in labels.iter().enumerate() {
        let part = Rect::from_min_size(
            pos2(x, rect.top() + 3.0),
            vec2(widths[index], SEGMENT_HEIGHT - 6.0),
        );
        x += widths[index] + 2.0;
        let response = ui.interact(part, ui.id().with((id, index)), Sense::click());
        let response = if response.hovered() {
            response.on_hover_cursor(CursorIcon::PointingHand)
        } else {
            response
        };
        let selected = index == current;
        if selected {
            paint_rounded(ui.painter(), part, part.height() / 2.0, RAISED, Color32::TRANSPARENT);
        }
        ui.painter().text(
            part.center(),
            Align2::CENTER_CENTER,
            label,
            semibold(13.0),
            if selected || response.hovered() { TEXT } else { TEXT2 },
        );
        if response.clicked() && !selected {
            chosen = Some(index);
        }
    }
    chosen
}

/// A capsule button sized to its text.
pub fn pill_button(ui: &mut Ui, text: &str) -> Response {
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_string(), semibold(13.0), Color32::WHITE);
    let size = vec2(galley.size().x + 24.0, SEGMENT_HEIGHT);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let fill = if response.hovered() { RAISED } else { SURFACE };
    paint_rounded(ui.painter(), rect, rect.height() / 2.0, fill, BORDER);
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        text,
        semibold(13.0),
        TEXT,
    );
    if response.hovered() {
        response.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        response
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Plus,
    Import,
    Export,
    Trash,
    Settings,
    Back,
    Chevron,
    Pencil,
    Check,
    Close,
}

impl Icon {
    /// Stroke paths on a 24 unit grid.
    fn lines(self) -> &'static [&'static [(f32, f32)]] {
        match self {
            Icon::Plus => &[&[(12.0, 5.0), (12.0, 19.0)], &[(5.0, 12.0), (19.0, 12.0)]],
            Icon::Import => &[
                &[(12.0, 4.0), (12.0, 15.0)],
                &[(7.0, 10.5), (12.0, 15.5), (17.0, 10.5)],
                &[(4.0, 16.0), (4.0, 20.0), (20.0, 20.0), (20.0, 16.0)],
            ],
            Icon::Export => &[
                &[(12.0, 15.0), (12.0, 4.0)],
                &[(7.0, 8.5), (12.0, 3.5), (17.0, 8.5)],
                &[(4.0, 16.0), (4.0, 20.0), (20.0, 20.0), (20.0, 16.0)],
            ],
            Icon::Trash => &[
                &[(4.0, 7.0), (20.0, 7.0)],
                &[(9.0, 7.0), (9.0, 4.0), (15.0, 4.0), (15.0, 7.0)],
                &[(6.0, 7.0), (7.0, 20.0), (17.0, 20.0), (18.0, 7.0)],
            ],
            // The gear is computed, see `gear_outline`.
            Icon::Settings => &[],
            Icon::Back => &[&[(15.0, 18.0), (9.0, 12.0), (15.0, 6.0)]],
            Icon::Chevron => &[&[(6.0, 9.0), (12.0, 15.0), (18.0, 9.0)]],
            Icon::Pencil => &[
                &[(4.0, 20.0), (5.0, 15.0), (16.0, 4.0), (20.0, 8.0), (9.0, 19.0), (4.0, 20.0)],
                &[(13.5, 6.5), (17.5, 10.5)],
            ],
            Icon::Check => &[&[(5.0, 13.0), (10.0, 18.0), (19.0, 7.0)]],
            Icon::Close => &[&[(6.0, 6.0), (18.0, 18.0)], &[(18.0, 6.0), (6.0, 18.0)]],
        }
    }
}

/// A gear on the 24 unit grid: a closed outline of teeth around the centre.
fn gear_outline() -> Vec<(f32, f32)> {
    let step = TAU / GEAR_TEETH as f32;
    let mut points = Vec::with_capacity(GEAR_TEETH * 4);
    for tooth in 0..GEAR_TEETH {
        let angle = tooth as f32 * step;
        for (offset, radius) in [
            (-0.30, GEAR_ROOT),
            (-0.17, GEAR_TIP),
            (0.17, GEAR_TIP),
            (0.30, GEAR_ROOT),
        ] {
            let (sin, cos) = (angle + offset * step).sin_cos();
            points.push((12.0 + radius * cos, 12.0 + radius * sin));
        }
    }
    points
}

pub fn paint_icon(ui: &Ui, center: Pos2, size: f32, icon: Icon, color: Color32) {
    let scale = size / 24.0;
    let corner = center - Vec2::splat(size / 2.0);
    if icon == Icon::Settings {
        let outline = gear_outline()
            .into_iter()
            .map(|(x, y)| corner + vec2(x, y) * scale)
            .collect();
        let stroke = Stroke::new(1.5, color);
        ui.painter().add(Shape::closed_line(outline, stroke));
        ui.painter().circle_stroke(center, GEAR_HOLE * scale, stroke);
        return;
    }
    for line in icon.lines() {
        let points: Vec<Pos2> = line
            .iter()
            .map(|&(x, y)| corner + vec2(x, y) * scale)
            .collect();
        ui.painter().add(Shape::line(points, Stroke::new(1.6, color)));
    }
}

/// A square button with an icon and a hint.
pub fn icon_button(ui: &mut Ui, icon: Icon, hint: &str, enabled: bool) -> bool {
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(ICON_BUTTON), sense);
    if response.hovered() && enabled {
        paint_rounded(ui.painter(), rect, RADIUS_CONTROL, HOVER, Color32::TRANSPARENT);
    }
    let color = match (enabled, response.hovered()) {
        (false, _) => SWITCH_OFF,
        (true, true) => TEXT,
        (true, false) => TEXT2,
    };
    paint_icon(ui, rect.center(), 17.0, icon, color);
    let response = response.on_hover_text(hint);
    if enabled && response.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    response.clicked()
}

/// A rounded field that opens a list, with a chevron on the right and, for a
/// mouse, a dot that shows it moving.
pub fn dropdown_button(ui: &mut Ui, text: &str, width: f32, dot: Option<Color32>) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(width, SEGMENT_HEIGHT), Sense::click());
    let fill = if response.hovered() { RAISED } else { SURFACE };
    paint_rounded(ui.painter(), rect, RADIUS_CONTROL, fill, BORDER);
    let mut text_x = rect.left() + 12.0;
    if let Some(color) = dot {
        ui.painter()
            .circle_filled(pos2(text_x + 3.0, rect.center().y), 3.5, color);
        text_x += 15.0;
    }
    let room = Rect::from_min_max(
        pos2(text_x, rect.top()),
        pos2(rect.right() - 28.0, rect.bottom()),
    );
    ui.painter().with_clip_rect(room).text(
        pos2(text_x, rect.center().y),
        Align2::LEFT_CENTER,
        text,
        font(14.0),
        TEXT,
    );
    let chevron = pos2(rect.right() - 15.0, rect.center().y);
    paint_icon(ui, chevron, 15.0, Icon::Chevron, TEXT2);
    if response.hovered() {
        response.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        response
    }
}

/// A line in a dropdown list.
pub fn list_item(ui: &mut Ui, text: &str, selected: bool) -> bool {
    let size = vec2(ui.available_width(), 30.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    if response.hovered() {
        paint_rounded(ui.painter(), rect, 8.0, HOVER, Color32::TRANSPARENT);
    }
    ui.painter().text(
        pos2(rect.left() + 10.0, rect.center().y),
        Align2::LEFT_CENTER,
        text,
        font(14.0),
        TEXT,
    );
    if selected {
        let check = pos2(rect.right() - 14.0, rect.center().y);
        paint_icon(ui, check, 14.0, Icon::Check, TEXT2);
    }
    if response.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    response.clicked()
}

/// A mouse in the list of mice: name and connection on the left, whether the
/// filter is on and whether it is the chosen one on the right.
pub fn mouse_item(
    ui: &mut Ui,
    title: &str,
    subtitle: &str,
    dot: Color32,
    filter_label: Option<&str>,
    selected: bool,
) -> bool {
    let size = vec2(ui.available_width(), MOUSE_ITEM_HEIGHT);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    if response.hovered() {
        paint_rounded(ui.painter(), rect, 8.0, HOVER, Color32::TRANSPARENT);
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    ui.painter()
        .circle_filled(pos2(rect.left() + 13.0, rect.center().y), 3.5, dot);
    let text_x = rect.left() + 26.0;
    ui.painter().text(
        pos2(text_x, rect.center().y - 9.0),
        Align2::LEFT_CENTER,
        title,
        font(14.0),
        TEXT,
    );
    ui.painter().text(
        pos2(text_x, rect.center().y + 9.0),
        Align2::LEFT_CENTER,
        subtitle,
        font(12.0),
        TEXT3,
    );
    if selected {
        let check = pos2(rect.right() - 14.0, rect.center().y);
        paint_icon(ui, check, 14.0, Icon::Check, TEXT2);
    }
    if let Some(label) = filter_label {
        ui.painter().text(
            pos2(rect.right() - 30.0, rect.center().y),
            Align2::RIGHT_CENTER,
            label,
            font(12.0),
            TEXT2,
        );
    }
    response.clicked()
}

/// A rounded panel behind the contents of a popup.
pub fn popup_body<R>(ui: &mut Ui, content: impl FnOnce(&mut Ui) -> R) -> R {
    let slot = ui.painter().add(Shape::Noop);
    let mut inner = ui.new_child(
        UiBuilder::new().max_rect(ui.available_rect_before_wrap().shrink(6.0)),
    );
    let result = content(&mut inner);
    let rect = inner.min_rect().expand(6.0);
    ui.painter().set(slot, theme::rounded(rect, 12.0, SURFACE, BORDER));
    ui.allocate_rect(rect, Sense::hover());
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slider_positions_map_back_to_values() {
        for (value, logarithmic) in [(4.0, false), (4.0, true), (1.0, true), (40.0, true)] {
            let share = to_share(value, 1.0, 40.0, logarithmic);
            let back = from_share(share, 1.0, 40.0, logarithmic);
            assert!((back - value).abs() < 1e-4, "{value} {logarithmic}: {back}");
        }
    }

    #[test]
    fn slider_positions_stay_on_the_track() {
        assert_eq!(to_share(100.0, 1.0, 40.0, false), 1.0);
        assert_eq!(to_share(0.5, 1.0, 40.0, true), 0.0);
        assert_eq!(from_share(2.0, 1.0, 40.0, false), 40.0);
    }

    #[test]
    fn a_logarithmic_slider_gives_room_to_small_values() {
        assert!(to_share(4.0, 1.0, 40.0, true) > to_share(4.0, 1.0, 40.0, false));
    }

    #[test]
    fn the_gear_stays_on_its_grid_with_all_its_teeth() {
        let outline = gear_outline();
        assert_eq!(outline.len(), GEAR_TEETH * 4);
        assert!(outline
            .iter()
            .all(|&(x, y)| (0.0..=24.0).contains(&x) && (0.0..=24.0).contains(&y)));
    }

    #[test]
    fn icons_stay_on_their_grid() {
        for icon in [
            Icon::Plus,
            Icon::Import,
            Icon::Export,
            Icon::Trash,
            Icon::Settings,
            Icon::Back,
            Icon::Chevron,
            Icon::Pencil,
            Icon::Check,
            Icon::Close,
        ] {
            for line in icon.lines() {
                assert!(line.len() >= 2);
                assert!(line
                    .iter()
                    .all(|&(x, y)| (0.0..=24.0).contains(&x) && (0.0..=24.0).contains(&y)));
            }
        }
    }
}
