//! The test that finds settings for a hand and a mouse.
//!
//! Dots come up one after another, and each has to be reached and clicked. That
//! is all there is to do. The motion on the way says the rest: ordinary dots
//! show the rhythm of the link, small dots close together show the smallest
//! correction of the hand, and far dots bring out the flicks. Then comes what
//! that adds up to, already at work so it can be tried on the spot.
//!
//! The test runs on neutral settings, so what it measures does not depend on
//! the preset that was on before.

use eframe::egui::{
    self, pos2, vec2, Align2, Color32, CursorIcon, Pos2, Rect, RichText, Sense, Stroke, Ui,
    UiBuilder,
};
use ilumx_core::curve::Curve;
use ilumx_core::event_log::RawEvent;
use ilumx_core::pipeline::PipelineConfig;
use ilumx_core::tuning::{Display, Exercises, Setup, Smoothness, Tuning};

use crate::curve_editor::CurveEditor;
use crate::i18n::Lang;
use crate::preview::Preview;
use crate::tap::{Flow, TapSample};
use crate::theme::{
    self, font, paint_rounded, semibold, BG, BORDER, GAP, OUT_LINE, RADIUS_CONTROL, SURFACE, TEXT,
    TEXT2, TEXT3,
};
use crate::widgets::{self, Icon, SliderSpec};

const ORDINARY_DOTS: usize = 12;
const SMALL_DOTS: usize = 10;
const FAR_DOTS: usize = 6;
const ALL_DOTS: usize = ORDINARY_DOTS + SMALL_DOTS + FAR_DOTS;

/// Lets a click count that is off by a pixel: this is not a test of the eye.
const HIT_SLACK: f32 = 1.5;
/// A dot this small gets a ring around it, or the eye would have to hunt for it.
const RINGED_BELOW: f32 = 5.0;
const RING_RADIUS: f32 = 16.0;
const ORDINARY_JUMP: (f32, f32) = (150.0, 420.0);
/// Small dots stay close together, the way careful work goes.
const SMALL_JUMP: (f32, f32) = (40.0, 140.0);
const FIELD_MARGIN: f32 = 28.0;
const DOT_TRIES: usize = 24;

const HEADER_HEIGHT: f32 = 30.0;
const BAR_HEIGHT: f32 = 4.0;

const RESULT_LEFT_WIDTH: f32 = 372.0;
const RESULT_TEST_HEIGHT: f32 = 222.0;
const RESULT_ROW_HEIGHT: f32 = 24.0;
const SECTION_GAP: f32 = 10.0;
/// A flick should cross this share of the main screen.
const FLICK_GOAL_SHARE: f64 = 0.6;
const GOAL_RANGE: (f64, f64) = (300.0, 4000.0);
const GOAL_STEP: f64 = 50.0;
/// How far the reach may stay under the goal before that is pointed out.
const REACH_TOLERANCE: f64 = 0.95;

/// The kinds of dots, in the order they come.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Round {
    Ordinary,
    Small,
    Far,
    Done,
}

impl Round {
    fn dots(self) -> usize {
        match self {
            Round::Ordinary => ORDINARY_DOTS,
            Round::Small => SMALL_DOTS,
            Round::Far => FAR_DOTS,
            Round::Done => 0,
        }
    }

    fn radius(self) -> f32 {
        match self {
            Round::Ordinary => 8.0,
            Round::Small => 3.5,
            Round::Far | Round::Done => 10.0,
        }
    }

    fn next(self) -> Self {
        match self {
            Round::Ordinary => Round::Small,
            Round::Small => Round::Far,
            Round::Far | Round::Done => Round::Done,
        }
    }
}

/// What the page wants done.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    None,
    /// Leave, and put the old settings back.
    Cancel,
    /// Back to the first dot, on neutral settings.
    Restart,
    /// Let the mouse try these settings.
    Try(PipelineConfig),
    /// Keep these settings as a preset.
    Save(PipelineConfig),
}

pub struct Wizard {
    round: Round,
    dots_done: usize,
    display: Display,
    exercises: Exercises,
    /// Where the dot is, relative to the field.
    dot: Option<Pos2>,
    field_min: Pos2,
    random: u32,
    tuning: Option<Tuning>,
    smoothness: Smoothness,
    goal_px: f64,
    setup: Option<Setup>,
    curve_editor: CurveEditor,
}

impl Wizard {
    pub fn new(display: Display, screen_width_px: f64, now_us: u64) -> Self {
        let goal_px = (screen_width_px * FLICK_GOAL_SHARE / GOAL_STEP).round() * GOAL_STEP;
        Self {
            round: Round::Ordinary,
            dots_done: 0,
            display,
            exercises: Exercises::default(),
            dot: None,
            field_min: Pos2::ZERO,
            // Anything but zero will do, a xorshift never leaves zero.
            random: (now_us as u32) | 1,
            tuning: None,
            smoothness: Smoothness::default(),
            goal_px: goal_px.clamp(GOAL_RANGE.0, GOAL_RANGE.1),
            setup: None,
            curve_editor: CurveEditor::default(),
        }
    }

    /// Takes in the motion of the mouse.
    pub fn add(&mut self, samples: &[TapSample]) {
        let recording = match self.round {
            Round::Ordinary => &mut self.exercises.ordinary,
            Round::Small => &mut self.exercises.precise,
            Round::Far => &mut self.exercises.flicks,
            Round::Done => return,
        };
        let motion = samples
            .iter()
            .filter(|sample| sample.flow == Flow::Input)
            .map(|sample| RawEvent {
                t_us: sample.t_us,
                dx: sample.dx,
                dy: sample.dy,
                ..RawEvent::default()
            });
        recording.extend(motion);
    }

    /// True if dots get clicked and the chosen mouse has not moved at all.
    fn clicked_with_another_mouse(&self) -> bool {
        const DOTS_TO_BE_SURE: usize = 2;
        let exercises = &self.exercises;
        self.dots_done >= DOTS_TO_BE_SURE
            && exercises.ordinary.is_empty()
            && exercises.precise.is_empty()
            && exercises.flicks.is_empty()
    }

    fn finish(&mut self) -> Action {
        self.tuning = Some(Tuning::new(&self.exercises, &self.display));
        self.settle()
    }

    fn settle(&mut self) -> Action {
        let Some(tuning) = &self.tuning else {
            return Action::None;
        };
        let setup = tuning.settle(self.goal_px, self.smoothness);
        let action = Action::Try(setup.config.clone());
        self.setup = Some(setup);
        action
    }

    fn next_random(&mut self) -> f32 {
        let mut x = self.random;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.random = x;
        (x >> 8) as f32 / (1u32 << 24) as f32
    }

    fn next_dot(&mut self, size: egui::Vec2) -> Pos2 {
        match self.round {
            Round::Small => self.dot_within(size, SMALL_JUMP),
            Round::Far | Round::Done => self.far_dot(size),
            Round::Ordinary => self.dot_within(size, ORDINARY_JUMP),
        }
    }

    /// A place in the field at about this distance from the last dot.
    fn dot_within(&mut self, size: egui::Vec2, (near, far): (f32, f32)) -> Pos2 {
        let (low, high) = (FIELD_MARGIN, size - egui::Vec2::splat(FIELD_MARGIN));
        let center = pos2(size.x / 2.0, size.y / 2.0);
        let from = self.dot.unwrap_or(center);
        let mut place = center;
        for _ in 0..DOT_TRIES {
            let angle = self.next_random() * std::f32::consts::TAU;
            let reach = near + (far - near) * self.next_random();
            place = from + reach * egui::Vec2::angled(angle);
            if (low..=high.x).contains(&place.x) && (low..=high.y).contains(&place.y) {
                return place;
            }
        }
        place.clamp(pos2(low, low), high.to_pos2())
    }

    /// The other end of the field, as far as the cursor can be sent from here.
    fn far_dot(&mut self, size: egui::Vec2) -> Pos2 {
        let on_the_left = self.dot.is_some_and(|dot| dot.x < size.x / 2.0);
        let x = if on_the_left {
            size.x - FIELD_MARGIN
        } else {
            FIELD_MARGIN
        };
        let y = FIELD_MARGIN + self.next_random() * (size.y - 2.0 * FIELD_MARGIN).max(0.0);
        pos2(x, y)
    }

    /// A press of the button at a place in the field. A miss changes nothing:
    /// the hand simply corrects, and that is worth recording.
    fn press(&mut self, place: Pos2, size: egui::Vec2, now_us: u64) -> bool {
        let reach = self.round.radius() + HIT_SLACK;
        if !self.dot.is_some_and(|dot| dot.distance(place) <= reach) {
            return false;
        }
        if self.round == Round::Small {
            self.exercises.hits.push(now_us);
        }
        self.dots_done += 1;
        if self.dots_done >= self.dots_before_round() + self.round.dots() {
            self.round = self.round.next();
        }
        self.dot = Some(self.next_dot(size));
        true
    }

    fn dots_before_round(&self) -> usize {
        match self.round {
            Round::Ordinary => 0,
            Round::Small => ORDINARY_DOTS,
            Round::Far | Round::Done => ORDINARY_DOTS + SMALL_DOTS,
        }
    }

    /// Draws the page. Returns what has to happen next.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        area: Rect,
        mouse: &str,
        lang: Lang,
        now_us: u64,
        preview: &Preview,
    ) -> Action {
        let mut action = Action::None;
        let header = Rect::from_min_size(area.min, vec2(area.width(), HEADER_HEIGHT));
        let body = Rect::from_min_max(pos2(area.left(), header.bottom() + GAP), area.max);

        let mut row = ui.new_child(UiBuilder::new().max_rect(header));
        row.horizontal_centered(|ui| {
            if widgets::icon_button(ui, Icon::Back, lang.tr("Отмена", "Cancel"), true) {
                action = Action::Cancel;
            }
            let title = format!("{}: {mouse}", lang.tr("Подбор настроек", "Finding settings"));
            ui.label(RichText::new(title).font(semibold(16.0)));
        });

        if self.round == Round::Done {
            let shown = self.show_result(ui, body, lang, now_us, preview);
            if shown != Action::None {
                action = shown;
            }
            return action;
        }

        let progress = format!("{} {} {ALL_DOTS}", self.dots_done, lang.tr("из", "of"));
        ui.painter().text(
            pos2(header.right(), header.center().y),
            Align2::RIGHT_CENTER,
            progress,
            font(13.0),
            TEXT3,
        );
        self.show_dots(ui, body, lang, now_us);
        if self.round == Round::Done {
            action = self.finish();
        }
        action
    }

    fn round_words(&self, lang: Lang) -> (&'static str, &'static str) {
        match self.round {
            Round::Small => (
                lang.tr("Теперь мелкие точки", "Now the small dots"),
                lang.tr(
                    "Попадай аккуратно, как в точной работе. Спешить не нужно: важны твои самые маленькие движения.",
                    "Hit them with care, as in precise work. No need to hurry: it is your smallest movements that count.",
                ),
            ),
            Round::Far => (
                lang.tr("Теперь дальние точки", "Now the far dots"),
                lang.tr(
                    "Перебрасывай курсор к точке быстрым движением, как на соседний экран, и нажимай.",
                    "Throw the cursor at the dot with a quick movement, as if to the next screen, and click.",
                ),
            ),
            _ => (
                lang.tr("Доведи курсор до точки и нажми", "Move the cursor to the dot and click"),
                lang.tr(
                    "Двигай мышью как обычно. После нажатия появится следующая точка.",
                    "Move the mouse the way you always do. The next dot comes up after each click.",
                ),
            ),
        }
    }

    fn show_dots(&mut self, ui: &mut Ui, body: Rect, lang: Lang, now_us: u64) {
        let (title, words) = self.round_words(lang);
        let lost = self.clicked_with_another_mouse();
        widgets::card_in(ui, body, |ui| {
            ui.label(RichText::new(title).font(semibold(15.0)).color(TEXT));
            if lost {
                let warning = lang.tr(
                    "Этой мыши не видно. Двигай той, что выбрана в списке, или вернись и выбери другую.",
                    "This mouse is not moving. Use the one chosen in the list, or go back and choose another.",
                );
                widgets::small_text(ui, warning, theme::WARN);
            } else {
                widgets::small_text(ui, words, TEXT2);
            }
            ui.add_space(8.0);

            let spacing = ui.spacing().item_spacing.y;
            let height = ui.available_height() - BAR_HEIGHT - spacing;
            let size = vec2(ui.available_width(), height);
            let (field, _) = ui.allocate_exact_size(size, Sense::click());
            paint_rounded(ui.painter(), field, RADIUS_CONTROL, BG, BORDER);
            self.run_dots(ui, field, now_us);

            let bar = vec2(ui.available_width(), BAR_HEIGHT);
            let (track, _) = ui.allocate_exact_size(bar, Sense::hover());
            paint_rounded(ui.painter(), track, BAR_HEIGHT / 2.0, SURFACE, Color32::TRANSPARENT);
            let share = self.dots_done as f32 / ALL_DOTS as f32;
            if share > 0.0 {
                let mut filled = track;
                filled.set_width(track.width() * share);
                paint_rounded(ui.painter(), filled, BAR_HEIGHT / 2.0, OUT_LINE, Color32::TRANSPARENT);
            }
        });
    }

    fn run_dots(&mut self, ui: &Ui, field: Rect, now_us: u64) {
        let size = field.size();
        self.field_min = field.min;
        // A window made smaller can leave the dot outside the field.
        let dot = match self.dot {
            Some(dot) if dot.x < size.x && dot.y < size.y => dot,
            _ => self.next_dot(size),
        };
        self.dot = Some(dot);

        let (pressed, pointer) =
            ui.input(|input| (input.pointer.primary_pressed(), input.pointer.latest_pos()));
        if pointer.is_some_and(|pointer| field.contains(pointer)) {
            ui.ctx().set_cursor_icon(CursorIcon::Crosshair);
        }
        if let Some(pointer) = pointer.filter(|_| pressed) {
            self.press(pointer - field.min.to_vec2(), size, now_us);
        }

        // The press may have ended the test, and then there is no next dot.
        if self.round == Round::Done {
            return;
        }
        let place = field.min + self.dot.unwrap_or(dot).to_vec2();
        let radius = self.round.radius();
        let painter = ui.painter();
        if radius < RINGED_BELOW {
            painter.circle_stroke(place, RING_RADIUS, Stroke::new(1.0, theme::RAISED));
        }
        painter.circle_filled(place, radius, OUT_LINE);
    }

    fn show_result(
        &mut self,
        ui: &mut Ui,
        body: Rect,
        lang: Lang,
        now_us: u64,
        preview: &Preview,
    ) -> Action {
        let mut action = Action::None;
        let left = Rect::from_min_max(body.min, pos2(body.left() + RESULT_LEFT_WIDTH, body.bottom()));
        let right = Rect::from_min_max(pos2(left.right() + GAP, body.top()), body.max);

        let mut column = ui.new_child(UiBuilder::new().max_rect(left));
        egui::ScrollArea::vertical()
            .id_salt("tuning-result")
            .auto_shrink([false, false])
            .show(&mut column, |ui| {
                widgets::section(ui, lang.tr("Что получилось", "What came out"));
                widgets::card(ui, |ui| self.result_rows(ui, lang));
                ui.add_space(SECTION_GAP - theme::ROW_GAP);

                widgets::section(ui, lang.tr("Подстроить", "Adjust"));
                if let Some(changed) = self.adjustments(ui, lang) {
                    action = changed;
                }
                ui.add_space(SECTION_GAP);

                ui.horizontal(|ui| {
                    if widgets::pill_button(ui, lang.tr("Сохранить пресет", "Save the preset"))
                        .clicked()
                    {
                        if let Some(setup) = &self.setup {
                            action = Action::Save(setup.config.clone());
                        }
                    }
                    if widgets::pill_button(ui, lang.tr("Пройти заново", "Start over")).clicked() {
                        action = Action::Restart;
                    }
                });
            });

        let test_top = right.bottom() - RESULT_TEST_HEIGHT;
        let curve = Rect::from_min_max(right.min, pos2(right.right(), test_top - SECTION_GAP));
        let test = Rect::from_min_max(pos2(right.left(), test_top), right.max);
        self.show_curve(ui, curve, lang, now_us, preview);

        let heading = lang.tr("Попробуй: настройки уже работают", "Try it: the settings are on");
        let card = widgets::section_in(ui, test, heading);
        let legend_end = pos2(test.right() - 2.0, test.top() + widgets::SECTION_HEIGHT / 2.0);
        Preview::legend(ui, legend_end, lang);
        widgets::card_in(ui, card, |ui| preview.show(ui, now_us));
        action
    }

    fn result_rows(&self, ui: &mut Ui, lang: Lang) {
        let (Some(tuning), Some(setup)) = (&self.tuning, &self.setup) else {
            return;
        };
        let measured = &tuning.measured;
        let number = |value: f64, decimals: usize| local_number(value, decimals, lang);

        let doubles = if measured.doubles_per_s < 0.3 {
            lang.tr("не найдены", "none found").to_string()
        } else {
            format!(
                "{} → {} {}",
                number(measured.doubles_per_s, 1),
                number(setup.doubles_per_s, 1),
                lang.tr("в секунду", "per second")
            )
        };
        let steady = if measured.roughness > 0.0 {
            let saved = (1.0 - setup.roughness / measured.roughness) * 100.0;
            format!(
                "{} {}{}, +{} {}",
                lang.tr("ровнее на", "smoother by"),
                number(saved, 0),
                lang.tr(" %", "%"),
                number(setup.added_lag_ms, 0),
                lang.tr("мс", "ms")
            )
        } else {
            lang.tr("мало движения", "too little motion").to_string()
        };
        let pixels_per_count = setup.config.factor * self.display.pixels_per_count;
        let fine = format!(
            "{} {}, {} px/{}",
            number(measured.nudge_counts, 0),
            lang.tr("отсч.", "counts"),
            number(pixels_per_count, 2),
            lang.tr("отсч.", "count")
        );
        let flick = if measured.flicks_used > 0 {
            format!(
                "{} {} → {} px",
                number(measured.flick_counts, 0),
                lang.tr("отсч.", "counts"),
                number(setup.reach_px, 0)
            )
        } else {
            lang.tr("быстрых движений не было", "no quick movements").to_string()
        };

        result_row(ui, lang.tr("Двойные шаги", "Double steps"), &doubles);
        result_row(ui, lang.tr("Плавность", "Smoothness"), &steady);
        result_row(ui, lang.tr("Самый малый сдвиг", "Smallest nudge"), &fine);
        result_row(ui, lang.tr("Бросок", "Flick"), &flick);
    }

    fn adjustments(&mut self, ui: &mut Ui, lang: Lang) -> Option<Action> {
        let mut changed = false;
        let current = match self.smoothness {
            Smoothness::Quick => 0,
            Smoothness::Balanced => 1,
            Smoothness::Smooth => 2,
        };
        let labels = [
            lang.tr("Быстрее", "Quicker"),
            lang.tr("Баланс", "Balanced"),
            lang.tr("Плавнее", "Smoother"),
        ];
        let reach = self.setup.as_ref().map_or(0.0, |setup| setup.reach_px);
        let short = reach < self.goal_px * REACH_TOLERANCE && self.tuning.is_some();

        widgets::card(ui, |ui| {
            if let Some(index) = widgets::segmented(ui, "tuning-smoothness", &labels, current) {
                self.smoothness = match index {
                    0 => Smoothness::Quick,
                    1 => Smoothness::Balanced,
                    _ => Smoothness::Smooth,
                };
                changed = true;
            }
            ui.add_space(6.0);
            changed |= widgets::slider_row(
                ui,
                lang.tr("Дальность броска", "Flick reach"),
                &mut self.goal_px,
                SliderSpec {
                    low: GOAL_RANGE.0,
                    high: GOAL_RANGE.1,
                    step: GOAL_STEP,
                    logarithmic: false,
                    decimals: 0,
                    unit: " px",
                },
            );
            if short {
                widgets::small_text(
                    ui,
                    lang.tr(
                        "Дальше кривая не поднимает: иначе средние скорости стали бы слишком резкими",
                        "The curve goes no higher, or medium speeds would turn twitchy",
                    ),
                    TEXT3,
                );
            }
        });
        changed.then(|| self.settle())
    }

    fn show_curve(&mut self, ui: &mut Ui, rect: Rect, lang: Lang, now_us: u64, preview: &Preview) {
        let card = widgets::section_in(ui, rect, lang.tr("Ускорение", "Acceleration"));
        let Some(setup) = &self.setup else {
            return;
        };
        let mut points = match &setup.config.curve {
            Curve::Table(points) => points.clone(),
            _ => Vec::new(),
        };
        let speed = preview.speed(now_us);
        let editor = &mut self.curve_editor;
        widgets::card_in(ui, card, |ui| {
            // For looking only. The curve can be edited after saving.
            let height = ui.available_height();
            editor.show(ui, &mut points, speed, height, lang);
        });
    }
}

/// The decimal point the language writes.
fn local_number(value: f64, decimals: usize, lang: Lang) -> String {
    let text = format!("{value:.decimals$}");
    match lang {
        Lang::Ru => text.replace('.', ","),
        Lang::En => text,
    }
}

fn result_row(ui: &mut Ui, label: &str, value: &str) {
    let size = vec2(ui.available_width(), RESULT_ROW_HEIGHT);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter();
    let middle = rect.center().y;
    let text = |at: Pos2, align, words: &str, color: Color32| {
        painter.text(at, align, words, font(13.0), color);
    };
    text(pos2(rect.left(), middle), Align2::LEFT_CENTER, label, TEXT2);
    text(pos2(rect.right(), middle), Align2::RIGHT_CENTER, value, TEXT);
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{Event, Modifiers, PointerButton, RawInput};

    const FIELD: egui::Vec2 = vec2(860.0, 520.0);

    fn sample(t_us: u64, dx: i32) -> TapSample {
        TapSample {
            t_us,
            flow: Flow::Input,
            dx,
            dy: 0,
        }
    }

    fn wizard() -> Wizard {
        Wizard::new(Display::default(), 3840.0, 12_345)
    }

    /// Clicks the dot that is up, wherever it is.
    fn hit(wizard: &mut Wizard, now_us: u64) {
        let dot = wizard.dot.unwrap_or_else(|| {
            let dot = wizard.next_dot(FIELD);
            wizard.dot = Some(dot);
            dot
        });
        assert!(wizard.press(dot, FIELD, now_us));
    }

    #[test]
    fn only_a_press_on_the_dot_counts() {
        let mut wizard = wizard();
        let dot = wizard.next_dot(FIELD);
        wizard.dot = Some(dot);
        let reach = Round::Ordinary.radius() + HIT_SLACK;

        assert!(!wizard.press(dot + vec2(reach + 1.0, 0.0), FIELD, 1_000));
        assert_eq!(wizard.dots_done, 0);
        assert_eq!(wizard.dot, Some(dot), "a miss leaves the dot where it is");

        assert!(wizard.press(dot + vec2(reach - 1.0, 0.0), FIELD, 2_000));
        assert_eq!(wizard.dots_done, 1);
        assert_ne!(wizard.dot, Some(dot));
    }

    #[test]
    fn the_dots_come_in_their_order_and_end_the_test() {
        let mut wizard = wizard();
        for _ in 0..ORDINARY_DOTS {
            assert_eq!(wizard.round, Round::Ordinary);
            hit(&mut wizard, 1);
        }
        for _ in 0..SMALL_DOTS {
            assert_eq!(wizard.round, Round::Small);
            hit(&mut wizard, 2);
        }
        for _ in 0..FAR_DOTS {
            assert_eq!(wizard.round, Round::Far);
            hit(&mut wizard, 3);
        }
        assert_eq!(wizard.round, Round::Done);
        assert_eq!(wizard.dots_done, ALL_DOTS);
    }

    #[test]
    fn only_the_small_dots_are_remembered_as_careful_hits() {
        let mut wizard = wizard();
        for now_us in 0..ALL_DOTS as u64 {
            hit(&mut wizard, now_us);
        }
        assert_eq!(wizard.exercises.hits.len(), SMALL_DOTS);
        assert_eq!(wizard.exercises.hits[0], ORDINARY_DOTS as u64);
    }

    #[test]
    fn motion_is_filed_under_the_dots_it_went_to() {
        let mut wizard = wizard();
        wizard.add(&[sample(1, 5)]);
        wizard.round = Round::Small;
        wizard.add(&[sample(2, 5)]);
        wizard.round = Round::Far;
        wizard.add(&[sample(3, 5)]);
        wizard.round = Round::Done;
        wizard.add(&[sample(4, 5)]);
        assert_eq!(wizard.exercises.ordinary[0].t_us, 1);
        assert_eq!(wizard.exercises.precise[0].t_us, 2);
        assert_eq!(wizard.exercises.flicks[0].t_us, 3);
        assert_eq!(wizard.exercises.flicks.len(), 1, "nothing after the end");
    }

    #[test]
    fn output_of_the_filter_is_not_taken_for_the_ball() {
        let mut wizard = wizard();
        wizard.add(&[TapSample {
            t_us: 1,
            flow: Flow::Output,
            dx: 5,
            dy: 0,
        }]);
        assert!(wizard.exercises.ordinary.is_empty());
    }

    #[test]
    fn clicking_with_the_wrong_mouse_is_noticed() {
        let mut wizard = wizard();
        hit(&mut wizard, 1);
        assert!(!wizard.clicked_with_another_mouse());
        hit(&mut wizard, 2);
        assert!(wizard.clicked_with_another_mouse());
        wizard.add(&[sample(3, 5)]);
        assert!(!wizard.clicked_with_another_mouse());
    }

    #[test]
    fn dots_stay_in_the_field_and_small_ones_stay_close() {
        for round in [Round::Ordinary, Round::Small] {
            let mut wizard = wizard();
            wizard.round = round;
            let far = if round == Round::Small { SMALL_JUMP.1 } else { ORDINARY_JUMP.1 };
            for _ in 0..200 {
                let previous = wizard.dot;
                let dot = wizard.next_dot(FIELD);
                assert!((FIELD_MARGIN..=FIELD.x - FIELD_MARGIN).contains(&dot.x), "{dot:?}");
                assert!((FIELD_MARGIN..=FIELD.y - FIELD_MARGIN).contains(&dot.y), "{dot:?}");
                if let Some(previous) = previous {
                    assert!(previous.distance(dot) <= far + 1.0);
                }
                wizard.dot = Some(dot);
            }
        }
    }

    #[test]
    fn far_dots_go_from_end_to_end() {
        let mut wizard = wizard();
        wizard.round = Round::Far;
        wizard.dot = Some(pos2(FIELD.x / 2.0, FIELD.y / 2.0));
        for _ in 0..FAR_DOTS {
            let previous = wizard.dot.unwrap();
            let dot = wizard.next_dot(FIELD);
            assert!((dot.x - previous.x).abs() >= FIELD.x / 2.0 - FIELD_MARGIN, "{dot:?}");
            wizard.dot = Some(dot);
        }
    }

    #[test]
    fn the_flick_goal_starts_on_a_step_of_its_slider() {
        assert_eq!(wizard().goal_px, 2300.0);
        let small = Wizard::new(Display::default(), 300.0, 1);
        assert_eq!(small.goal_px, GOAL_RANGE.0);
    }

    #[test]
    fn numbers_use_the_decimal_point_of_the_language() {
        assert_eq!(local_number(1.25, 1, Lang::Ru), "1,2");
        assert_eq!(local_number(1.25, 1, Lang::En), "1.2");
        assert_eq!(local_number(1400.0, 0, Lang::Ru), "1400");
    }

    /// One frame of the real page, without a window.
    fn frame(
        ctx: &egui::Context,
        wizard: &mut Wizard,
        preview: &Preview,
        events: Vec<Event>,
    ) -> Action {
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(920.0, 700.0))),
            events,
            ..RawInput::default()
        };
        let mut action = Action::None;
        let _ = ctx.run_ui(input, |ui| {
            let area = ui.max_rect().shrink(16.0);
            let shown = wizard.show(ui, area, "MX Ergo", Lang::Ru, 1_000, preview);
            if shown != Action::None {
                action = shown;
            }
        });
        action
    }

    fn button(place: Pos2, pressed: bool) -> Event {
        Event::PointerButton {
            pos: place,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::default(),
        }
    }

    #[test]
    fn the_whole_test_can_be_clicked_through_on_the_page() {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let preview = Preview::default();
        let mut wizard = wizard();
        frame(&ctx, &mut wizard, &preview, Vec::new());

        let mut last = Action::None;
        for _ in 0..ALL_DOTS {
            let place = wizard.field_min + wizard.dot.unwrap().to_vec2();
            frame(&ctx, &mut wizard, &preview, vec![Event::PointerMoved(place)]);
            last = frame(&ctx, &mut wizard, &preview, vec![button(place, true)]);
            frame(&ctx, &mut wizard, &preview, vec![button(place, false)]);
        }
        assert_eq!(wizard.round, Round::Done);
        assert!(matches!(last, Action::Try(_)), "{last:?}");
        assert!(wizard.setup.is_some());
        // The result page draws from what came out.
        assert_eq!(frame(&ctx, &mut wizard, &preview, Vec::new()), Action::None);
    }

    #[test]
    fn a_click_beside_the_dot_on_the_page_does_nothing() {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let preview = Preview::default();
        let mut wizard = wizard();
        frame(&ctx, &mut wizard, &preview, Vec::new());

        let place = wizard.field_min + wizard.dot.unwrap().to_vec2() + vec2(40.0, 0.0);
        frame(&ctx, &mut wizard, &preview, vec![Event::PointerMoved(place)]);
        frame(&ctx, &mut wizard, &preview, vec![button(place, true)]);
        frame(&ctx, &mut wizard, &preview, vec![button(place, false)]);
        assert_eq!(wizard.dots_done, 0);
    }
}
