//! The window: mice and processing on the left, curve and test on the right.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use eframe::egui::{self, pos2, vec2, Align2, Color32, Rect, Sense, Ui, UiBuilder};
use ilumx_core::device::Connection;
use ilumx_core::pipeline::PipelineConfig;
use ilumx_core::tuning::Display;
use ilumx_win::windows_settings::memory_integrity_on;
use windows_sys::Win32::Graphics::Gdi::{EnumDisplaySettingsW, DEVMODEW, ENUM_CURRENT_SETTINGS};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN};

use crate::autostart;
use crate::config::{self, Backend, Config, DeviceEntry};
use crate::curve_editor::{describe, CurveEditor, READY_CURVES};
use crate::engine::{Engine, MouseInfo, MouseSetting, DEMO_KEY};
use crate::filedialog;
use crate::i18n::Lang;
use crate::interception::{library_path, Interception};
use crate::log;
use crate::model::PresetModel;
use crate::presets::{self, Library, PresetKey};
use crate::preview::Preview;
use crate::theme::{
    self, font, paint_rounded, semibold, BG, CARD_PADDING, GAP, GOOD, RADIUS_CONTROL, TEXT,
    TEXT2, TEXT3, WARN, WARN_BG, WINDOW_MARGIN,
};
use crate::tray::{self, Tray, TrayCommand};
use crate::update::{self, Failure, UpdateState, Updater};
use crate::widgets::{self, Icon, SliderSpec};
use crate::wizard::{Action, Wizard};

const LEFT_WIDTH: f32 = 372.0;
const FOOTER_HEIGHT: f32 = 34.0;
/// The switch beside the list of mice.
const TOGGLE_COLUMN: f32 = 36.0;
/// The test with its heading while the curve is open.
const TEST_SECTION_HEIGHT: f32 = 222.0;
const CURVE_MIN_HEIGHT: f32 = 120.0;
/// Room under the graph for the line that sums the curve up.
const CURVE_SUMMARY: f32 = 24.0;
/// From the card of one section to the heading of the next.
const SECTION_GAP: f32 = 6.0 + theme::ROW_GAP;

/// Saving waits for the sliders to come to rest.
const SAVE_DELAY: Duration = Duration::from_millis(700);
const CHECK_PERIOD: Duration = Duration::from_secs(1);
const NOTICE_TIME: Duration = Duration::from_secs(7);
/// The test area keeps drawing this long after the last motion.
const ANIMATE_AFTER_MOTION: Duration = Duration::from_secs(2);
/// A resting window only needs to notice changes of status now and then.
const IDLE_REPAINT: Duration = Duration::from_millis(250);
/// A hidden window only looks after the settings file and what the icon asks.
const HIDDEN_CHECK: Duration = Duration::from_secs(1);
/// The first look for a new version waits for the network to come up after a
/// logon, and for the program to settle.
const FIRST_UPDATE_CHECK: Duration = Duration::from_secs(20);
const UPDATE_CHECK_PERIOD: Duration = Duration::from_secs(12 * 60 * 60);
/// About 60 frames per second. egui takes the length of one frame off a delay,
/// so anything up to 16 ms means at once and the window would redraw flat out.
const MOTION_REPAINT: Duration = Duration::from_millis(33);
/// A mouse counts as moving for this long after its last packet.
const ACTIVE_US: u64 = 400_000;

const DRIVER_RELEASES: &str = "https://github.com/oblitum/Interception/releases";
const SETTINGS_WIDTH: f32 = 580.0;
const STATUS_ROW_HEIGHT: f32 = 32.0;
/// The mark on the settings button when a new version is out.
const UPDATE_DOT_RADIUS: f32 = 3.5;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Main,
    Settings,
    Tuning,
}

/// What is known about the driver. Asked once per visit to the settings: the
/// driver takes one client at a time, so this must not happen on every frame.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DriverState {
    NotInstalled,
    NotAnswering,
    Ready,
}

/// What the row under the preset list is being used for.
#[derive(Clone, PartialEq)]
enum NameEntry {
    None,
    New(String),
    Rename(String),
}

pub struct Startup {
    pub config: Config,
    pub config_path: PathBuf,
    pub library: Library,
    pub migrated_preset: Option<PresetKey>,
    pub notice: Option<String>,
    pub tray: Option<Tray>,
    pub start_hidden: bool,
}

pub struct SettingsApp {
    engine: Engine,
    /// Without an icon, closing the window has to end the program.
    tray: Option<Tray>,
    hidden: bool,
    quitting: bool,
    wizard: Option<Wizard>,
    /// The mouse the wizard works on and what it had before.
    tuning_mouse: Option<(String, MouseSetting)>,
    updater: Updater,
    next_update_check: Instant,
    /// The new version takes over when the window has closed.
    start_new_version: bool,
    config: Config,
    config_path: PathBuf,
    config_time: Option<SystemTime>,
    library: Library,
    mice: Vec<MouseInfo>,
    settings: HashMap<String, MouseSetting>,
    selected: Option<String>,
    model: PresetModel,
    migrated_preset: Option<PresetKey>,
    preview: Preview,
    curve_editor: CurveEditor,
    page: Page,
    name_entry: NameEntry,
    memory_integrity: bool,
    driver_state: DriverState,
    autostart: bool,
    unsaved_since: Option<Instant>,
    notice: Option<(String, Instant)>,
    last_check: Option<Instant>,
    last_motion: Option<Instant>,
}

/// The name of a mouse in the list.
fn mouse_title(mouse: &MouseInfo, lang: Lang) -> String {
    if mouse.key == DEMO_KEY {
        return lang.tr("Демо-запись", "Demo recording").to_string();
    }
    if mouse.is_mx_ergo {
        return "MX Ergo".to_string();
    }
    mouse
        .product
        .clone()
        .unwrap_or_else(|| lang.tr("Мышь", "Mouse").to_string())
}

fn mouse_subtitle(mouse: &MouseInfo, lang: Lang) -> String {
    if mouse.key == DEMO_KEY {
        return lang.tr("без реального устройства", "no real device").to_string();
    }
    let link = match mouse.connection {
        Connection::Bluetooth => "Bluetooth",
        Connection::LogitechReceiver => lang.tr("ресивер Logitech", "Logitech receiver"),
        Connection::Other => "USB",
    };
    match mouse.ids {
        Some((vid, pid)) => format!("{link} · {vid:04X}:{pid:04X}"),
        None => link.to_string(),
    }
}

impl SettingsApp {
    pub fn new(engine: Engine, startup: Startup) -> Self {
        let Startup {
            config,
            config_path,
            library,
            migrated_preset,
            notice,
            tray,
            start_hidden,
        } = startup;
        let settings = settings_from(&config, &library);
        engine.shared().set_games_direct(config.games_direct);
        let mut app = Self {
            engine,
            tray,
            hidden: start_hidden,
            quitting: false,
            wizard: None,
            tuning_mouse: None,
            updater: Updater::new(),
            next_update_check: Instant::now() + FIRST_UPDATE_CHECK,
            start_new_version: false,
            config_time: config::modified_time(&config_path),
            config,
            config_path,
            library,
            mice: Vec::new(),
            settings,
            selected: None,
            model: PresetModel::default(),
            migrated_preset,
            preview: Preview::default(),
            curve_editor: CurveEditor::default(),
            page: Page::Main,
            name_entry: NameEntry::None,
            memory_integrity: memory_integrity_on(),
            driver_state: DriverState::NotInstalled,
            autostart: autostart::is_enabled(),
            unsaved_since: None,
            notice: None,
            last_check: None,
            last_motion: None,
        };
        app.notice = notice.map(|text| (text, Instant::now()));
        app.push_all_settings();
        // Settings read from under the old name move to the new one right away.
        if !app.config_path.exists() {
            app.mark_unsaved();
        }
        app
    }

    fn lang(&self) -> Lang {
        self.config.language
    }

    fn say(&mut self, text: impl Into<String>) {
        self.notice = Some((text.into(), Instant::now()));
    }

    fn mark_unsaved(&mut self) {
        self.unsaved_since = Some(Instant::now());
    }

    fn setting_of(&self, key: &str) -> MouseSetting {
        self.settings.get(key).cloned().unwrap_or_else(|| {
            let preset = PresetKey::default_key();
            let config = self.library.config(&preset).unwrap_or_default();
            MouseSetting {
                enabled: false,
                preset,
                config,
            }
        })
    }

    fn selected_setting(&self) -> Option<MouseSetting> {
        self.selected.as_deref().map(|key| self.setting_of(key))
    }

    fn push_all_settings(&mut self) {
        let shared = self.engine.shared().clone();
        for (key, setting) in &self.settings {
            shared.set_mouse(key, setting.clone());
        }
        self.push_cycle();
        self.sync_capture();
    }

    fn push_cycle(&self) {
        let cycle = self
            .library
            .keys()
            .into_iter()
            .filter_map(|key| self.library.config(&key).map(|config| (key, config)))
            .collect();
        self.engine.shared().set_cycle(cycle);
    }

    /// Starts or stops capturing. If that fails no mouse may stay marked as
    /// filtered, or the switches would claim something that is not happening.
    fn sync_capture(&mut self) {
        let Err(error) = self.engine.sync() else {
            return;
        };
        let shared = self.engine.shared().clone();
        for (key, setting) in &mut self.settings {
            if setting.enabled {
                setting.enabled = false;
                shared.set_mouse(key, setting.clone());
            }
        }
        let text = format!(
            "{}: {error}",
            self.lang().tr("Фильтр не включился", "The filter did not start")
        );
        self.say(text);
    }

    fn select_mouse(&mut self, key: &str) {
        self.selected = Some(key.to_string());
        let config = self.setting_of(key).config;
        self.model = PresetModel::from_config(&config);
        self.preview = Preview::default();
        self.name_entry = NameEntry::None;
    }

    fn set_enabled(&mut self, key: &str, enabled: bool) {
        let mut setting = self.setting_of(key);
        setting.enabled = enabled;
        self.settings.insert(key.to_string(), setting.clone());
        self.engine.shared().set_mouse(key, setting);
        self.sync_capture();
        self.mark_unsaved();
    }

    fn assign_preset(&mut self, key: &str, preset: PresetKey) {
        let Some(config) = self.library.config(&preset) else {
            return;
        };
        let mut setting = self.setting_of(key);
        setting.preset = preset;
        setting.config = config.clone();
        self.settings.insert(key.to_string(), setting.clone());
        self.engine.shared().set_mouse(key, setting);
        if self.selected.as_deref() == Some(key) {
            self.model = PresetModel::from_config(&config);
        }
        self.mark_unsaved();
    }

    /// The sliders changed. A user preset takes it in, and so does every mouse
    /// on it. A standard preset never changes: a personal copy takes over.
    fn apply_model(&mut self) {
        let Some(key) = self.selected.clone() else {
            return;
        };
        let lang = self.lang();
        let config = self.model.to_config();
        let setting = self.setting_of(&key);
        let preset = if setting.preset.is_standard() {
            let name = format!(
                "{} ({})",
                self.library.name(&setting.preset, lang),
                lang.tr("мой", "mine")
            );
            let copy = self.library.add(&name, config.clone());
            let shown = self.library.name(&copy, lang);
            self.say(format!(
                "{} «{shown}»",
                lang.tr(
                    "Стандартный пресет не меняется, создан ваш:",
                    "Standard presets stay as they are, yours is now:"
                )
            ));
            self.push_cycle();
            copy
        } else {
            self.library.update(&setting.preset, config.clone());
            setting.preset
        };

        self.settings.insert(
            key,
            MouseSetting {
                preset: preset.clone(),
                config,
                ..setting
            },
        );
        self.spread_preset(&preset);
        self.push_cycle();
        self.mark_unsaved();
    }

    /// Hands the current state of a preset to every mouse that uses it.
    fn spread_preset(&mut self, preset: &PresetKey) {
        let Some(config) = self.library.config(preset) else {
            return;
        };
        let shared = self.engine.shared().clone();
        for (key, setting) in &mut self.settings {
            if setting.preset == *preset {
                setting.config = config.clone();
                shared.set_mouse(key, setting.clone());
            }
        }
    }

    /// The close button hides the window: the program lives on in the tray and
    /// leaves through its menu.
    fn window_events(&mut self, ctx: &egui::Context) {
        for command in Tray::take_commands() {
            match command {
                TrayCommand::Show => self.show_window(ctx),
                TrayCommand::ShowUpdates => {
                    self.show_window(ctx);
                    self.open_settings();
                }
                TrayCommand::Quit => {
                    self.quitting = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
        let closing = ctx.input(|input| input.viewport().close_requested());
        if closing && !self.quitting && self.tray.is_some() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.hide_window(ctx);
        }
    }

    fn hide_window(&mut self, ctx: &egui::Context) {
        self.hidden = true;
        self.end_tuning();
        self.save_now();
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
    }

    fn show_window(&mut self, ctx: &egui::Context) {
        self.hidden = false;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    /// Looks for new versions, and lets the new one take over once it is in place.
    fn look_after_updates(&mut self, ctx: &egui::Context) {
        let state = self.updater.state();
        let new_version = match &state {
            UpdateState::Available(release) => Some(release.version.clone()),
            _ => None,
        };
        self.engine.shared().set_new_version(new_version);

        if state == UpdateState::Ready && !self.start_new_version {
            self.start_new_version = true;
            self.quitting = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if self.config.check_updates && Instant::now() >= self.next_update_check {
            self.next_update_check = Instant::now() + UPDATE_CHECK_PERIOD;
            self.updater.check(false);
        }
    }

    fn refresh(&mut self) {
        self.engine.shared().set_english(self.lang() == Lang::En);
        self.mice = self.engine.shared().mice();
        self.apply_migration();
        self.keep_selection();

        if let Some(key) = &self.selected {
            let samples = self.engine.shared().channel(key).processor.tap.drain();
            if !samples.is_empty() {
                self.last_motion = Some(Instant::now());
            }
            self.preview.add(&samples);
            if let Some(wizard) = &mut self.wizard {
                wizard.add(&samples);
            }
        }

        if let Some(switch) = self.engine.shared().take_switch() {
            self.assign_preset(&switch.mouse, switch.preset);
        }
        if self.engine.poll() {
            let text = self.lang().tr(
                "Перехват остановился из-за ошибки, мышь работает напрямую",
                "Capture stopped after an error, the mouse works directly",
            );
            self.say(text);
        }
        self.save_when_settled();

        let due = self
            .last_check
            .is_none_or(|last| last.elapsed() >= CHECK_PERIOD);
        if due {
            self.last_check = Some(Instant::now());
            self.reload_if_changed();
        }
        if self
            .notice
            .as_ref()
            .is_some_and(|(_, since)| since.elapsed() >= NOTICE_TIME)
        {
            self.notice = None;
        }
    }

    /// The old format knew one mouse, the MX Ergo. It carries on as it was.
    fn apply_migration(&mut self) {
        if self.migrated_preset.is_none() {
            return;
        }
        let Some(mouse) = self.mice.iter().find(|mouse| mouse.is_mx_ergo) else {
            return;
        };
        let key = mouse.key.clone();
        if let Some(preset) = self.migrated_preset.take() {
            self.assign_preset(&key, preset);
            self.set_enabled(&key, true);
        }
    }

    fn keep_selection(&mut self) {
        let still_there = self
            .selected
            .as_ref()
            .is_some_and(|key| self.mice.iter().any(|mouse| mouse.key == *key));
        if still_there {
            return;
        }
        let enabled = |mouse: &&MouseInfo| {
            self.settings
                .get(&mouse.key)
                .is_some_and(|setting| setting.enabled)
        };
        let choice = self
            .mice
            .iter()
            .find(|mouse| mouse.key == DEMO_KEY)
            .or_else(|| self.mice.iter().find(enabled))
            .or_else(|| self.mice.iter().find(|mouse| mouse.is_mx_ergo))
            .or_else(|| self.mice.first())
            .map(|mouse| mouse.key.clone());
        if let Some(key) = choice {
            self.select_mouse(&key);
        }
    }

    fn current_config(&self) -> Config {
        let mut device: Vec<DeviceEntry> = self
            .settings
            .iter()
            .filter(|(key, _)| key.as_str() != DEMO_KEY)
            .map(|(key, setting)| DeviceEntry {
                key: key.clone(),
                enabled: setting.enabled,
                preset: setting.preset.to_text(),
            })
            .collect();
        device.sort_by(|a, b| a.key.cmp(&b.key));
        let config = Config {
            device,
            ..self.config.clone()
        };
        config::with_library(&config, &self.library)
    }

    fn save_now(&mut self) {
        self.unsaved_since = None;
        self.config = self.current_config();
        match config::save(&self.config_path, &self.config) {
            Ok(()) => self.config_time = config::modified_time(&self.config_path),
            Err(error) => {
                let text = format!(
                    "{}: {error}",
                    self.lang().tr("Настройки не сохранены", "Settings not saved")
                );
                self.say(text);
            }
        }
    }

    fn save_when_settled(&mut self) {
        let ready = self
            .unsaved_since
            .is_some_and(|since| since.elapsed() >= SAVE_DELAY);
        if ready {
            self.save_now();
        }
    }

    fn reload_if_changed(&mut self) {
        let time = config::modified_time(&self.config_path);
        // The wizard has borrowed the mouse: a reload would take it back.
        if time == self.config_time || self.unsaved_since.is_some() || self.wizard.is_some() {
            return;
        }
        self.config_time = time;
        match config::load(&self.config_path) {
            Ok(config) => {
                let loaded = config::resolve(config);
                self.config = loaded.config;
                self.library = loaded.library;
                self.settings = settings_from(&self.config, &self.library);
                self.engine.shared().set_games_direct(self.config.games_direct);
                self.push_all_settings();
                if let Some(key) = self.selected.clone() {
                    self.select_mouse(&key);
                }
                let text = self.lang().tr("Настройки перечитаны из файла", "Settings reloaded");
                self.say(text);
            }
            Err(error) => {
                let text = format!(
                    "{} {}: {error}",
                    self.lang().tr("Ошибка в", "Error in"),
                    config::FILE_NAME
                );
                self.say(text);
            }
        }
    }

    /// Starts the test on the selected mouse. Everything it does to the mouse
    /// goes to the engine only, so a crash halfway leaves the settings as they were.
    fn start_tuning(&mut self, ctx: &egui::Context) {
        let Some(key) = self.selected.clone() else {
            return;
        };
        let original = self.setting_of(&key);
        let (screen_width_px, refresh_hz) = primary_screen();
        let display = Display {
            pixels_per_count: self.engine.shared().pointer().multiplier().unwrap_or(1.0),
            scale: f64::from(ctx.pixels_per_point()),
            refresh_hz,
        };
        let now_us = self.engine.shared().clock.now_us();
        self.wizard = Some(Wizard::new(display, screen_width_px, now_us));
        let enabled = original.enabled;
        self.tuning_mouse = Some((key, original));
        self.set_trial(PipelineConfig::default(), enabled);
        self.name_entry = NameEntry::None;
        self.page = Page::Tuning;
    }

    /// Puts settings on the mouse without keeping them.
    fn set_trial(&mut self, config: PipelineConfig, enabled: bool) {
        let Some((key, original)) = self.tuning_mouse.clone() else {
            return;
        };
        let trial = MouseSetting {
            enabled: enabled && may_reach_the_cursor(&key),
            config,
            ..original
        };
        self.engine.shared().set_mouse(&key, trial);
        self.sync_capture();
    }

    /// Leaves the test and gives the mouse back what it had.
    fn end_tuning(&mut self) {
        self.wizard = None;
        if self.page == Page::Tuning {
            self.page = Page::Main;
        }
        if let Some((key, original)) = self.tuning_mouse.take() {
            self.engine.shared().set_mouse(&key, original);
            self.sync_capture();
        }
    }

    fn tuning_page(&mut self, ui: &mut Ui, area: Rect) {
        let lang = self.lang();
        let now_us = self.engine.shared().clock.now_us();
        let mouse = self
            .tuning_mouse
            .as_ref()
            .and_then(|(key, _)| self.mice.iter().find(|mouse| mouse.key == *key))
            .map_or_else(String::new, |mouse| mouse_title(mouse, lang));
        let Some(wizard) = &mut self.wizard else {
            self.page = Page::Main;
            return;
        };
        match wizard.show(ui, area, &mouse, lang, now_us, &self.preview) {
            Action::None => {}
            Action::Cancel => self.end_tuning(),
            Action::Restart => self.restart_tuning(ui.ctx()),
            // The mouse takes part even if it was off: there has to be
            // something to feel.
            Action::Try(config) => self.set_trial(config, true),
            Action::Save(config) => self.save_tuning(config),
        }
    }

    fn restart_tuning(&mut self, ctx: &egui::Context) {
        let Some((key, original)) = self.tuning_mouse.take() else {
            return;
        };
        self.engine.shared().set_mouse(&key, original);
        self.start_tuning(ctx);
    }

    fn save_tuning(&mut self, config: PipelineConfig) {
        let lang = self.lang();
        let Some((key, _)) = self.tuning_mouse.clone() else {
            return;
        };
        self.end_tuning();
        let preset = self.library.add(lang.tr("Подбор", "Tuned"), config);
        self.assign_preset(&key, preset.clone());
        if may_reach_the_cursor(&key) {
            self.set_enabled(&key, true);
        }
        self.push_cycle();
        self.mark_unsaved();
        let name = self.library.name(&preset, lang);
        self.say(format!("{} «{name}»", lang.tr("Сохранён пресет", "Saved preset")));
    }

    fn export_preset(&mut self) {
        let Some(setting) = self.selected_setting() else {
            return;
        };
        let lang = self.lang();
        let name = self.library.name(&setting.preset, lang);
        let description = lang.tr("Пресет IluTrackball", "IluTrackball preset");
        let Some(path) = filedialog::save_path(description, presets::FILE_EXTENSION, &name) else {
            return;
        };
        let written = presets::export_text(&name, &setting.config)
            .and_then(|text| fs::write(&path, text).map_err(|error| error.to_string()));
        match written {
            Ok(()) => self.say(format!("{} «{name}»", lang.tr("Сохранён пресет", "Saved preset"))),
            Err(error) => self.say(format!("{}: {error}", lang.tr("Не сохранён", "Not saved"))),
        }
    }

    fn import_preset(&mut self) {
        let lang = self.lang();
        let description = lang.tr("Пресет IluTrackball", "IluTrackball preset");
        let Some(path) = filedialog::open_path(description, presets::FILE_EXTENSION) else {
            return;
        };
        let imported = fs::read_to_string(&path)
            .map_err(|error| error.to_string())
            .and_then(|text| presets::import_text(&text));
        match imported {
            Ok(preset) => {
                let key = self.library.add(&preset.name, preset.config);
                if let Some(mouse) = self.selected.clone() {
                    self.assign_preset(&mouse, key.clone());
                }
                self.push_cycle();
                self.mark_unsaved();
                let name = self.library.name(&key, lang);
                self.say(format!("{} «{name}»", lang.tr("Загружен пресет", "Loaded preset")));
            }
            Err(error) => {
                self.say(format!("{}: {error}", lang.tr("Файл не подошёл", "Bad file")));
            }
        }
    }

    fn finish_name_entry(&mut self) {
        let entry = std::mem::replace(&mut self.name_entry, NameEntry::None);
        let Some(mouse) = self.selected.clone() else {
            return;
        };
        let setting = self.setting_of(&mouse);
        match entry {
            NameEntry::None => {}
            NameEntry::New(name) => {
                let key = self.library.add(&name, setting.config);
                self.assign_preset(&mouse, key);
            }
            NameEntry::Rename(name) => {
                if let Some(key) = self.library.rename(&setting.preset, &name) {
                    let old = setting.preset;
                    for item in self.settings.values_mut() {
                        if item.preset == old {
                            item.preset = key.clone();
                        }
                    }
                }
            }
        }
        self.push_cycle();
        self.mark_unsaved();
    }

    fn delete_preset(&mut self) {
        let Some(setting) = self.selected_setting() else {
            return;
        };
        if !self.library.remove(&setting.preset) {
            return;
        }
        let fallback = PresetKey::default_key();
        let users: Vec<String> = self
            .settings
            .iter()
            .filter(|(_, item)| item.preset == setting.preset)
            .map(|(key, _)| key.clone())
            .collect();
        for key in users {
            self.assign_preset(&key, fallback.clone());
        }
        self.push_cycle();
        self.mark_unsaved();
    }

    fn mouse_dot(&self, key: &str) -> Color32 {
        let shared = self.engine.shared();
        let channel = shared.channel(key);
        let idle_for = shared.clock.now_us().saturating_sub(channel.last_packet_us());
        if channel.packets() > 0 && idle_for < ACTIVE_US {
            GOOD
        } else {
            theme::SWITCH_OFF
        }
    }

    fn mice_section(&mut self, ui: &mut Ui) {
        let lang = self.lang();
        widgets::section(ui, lang.tr("Мышь", "Mouse"));
        let mice = self.mice.clone();
        let current = self
            .selected
            .as_ref()
            .and_then(|key| mice.iter().find(|mouse| mouse.key == *key))
            .cloned();
        let mut select = None;
        let mut toggled = None;

        widgets::card(ui, |ui| {
            let Some(current) = &current else {
                widgets::small_text(ui, lang.tr("Мыши не найдены", "No mice found"), TEXT3);
                return;
            };
            let mut on = self.setting_of(&current.key).enabled;
            let spacing = ui.spacing().item_spacing.x;
            let width = ui.available_width() - TOGGLE_COLUMN - spacing;
            let list_width = width + TOGGLE_COLUMN + spacing;

            ui.horizontal(|ui| {
                let title = mouse_title(current, lang);
                let dot = self.mouse_dot(&current.key);
                let response = widgets::dropdown_button(ui, &title, width, Some(dot));
                egui::Popup::menu(&response)
                    .frame(egui::Frame::NONE)
                    .width(list_width)
                    .show(|ui| {
                        widgets::popup_body(ui, |ui| {
                            ui.set_width(list_width - 12.0);
                            for mouse in &mice {
                                let filter_on = self.setting_of(&mouse.key).enabled;
                                let clicked = widgets::mouse_item(
                                    ui,
                                    &mouse_title(mouse, lang),
                                    &mouse_subtitle(mouse, lang),
                                    self.mouse_dot(&mouse.key),
                                    filter_on.then(|| lang.tr("фильтр", "filter")),
                                    mouse.key == current.key,
                                );
                                if clicked {
                                    select = Some(mouse.key.clone());
                                }
                            }
                        });
                    });

                let (rect, _) = ui.allocate_exact_size(vec2(TOGGLE_COLUMN, 30.0), Sense::hover());
                let id = ui.id().with("mouse-switch");
                if widgets::toggle_end(ui, rect, &mut on, id, None) {
                    toggled = Some((current.key.clone(), on));
                }
            });
            ui.add_space(4.0);
            widgets::small_text(ui, &mouse_subtitle(current, lang), TEXT3);
        });

        if let Some(key) = select {
            self.select_mouse(&key);
        }
        if let Some((key, on)) = toggled {
            self.set_enabled(&key, on);
        }
    }

    fn preset_section(&mut self, ui: &mut Ui) {
        let lang = self.lang();
        widgets::section(ui, lang.tr("Пресет", "Preset"));
        let Some(mouse) = self.selected.clone() else {
            return;
        };
        let setting = self.setting_of(&mouse);
        let is_user = !setting.preset.is_standard();
        let mut chosen = None;
        let mut delete = false;
        let mut export = false;
        let mut import = false;
        let mut new_entry = None;
        let mut tune = false;

        widgets::card(ui, |ui| {
            ui.horizontal(|ui| {
                let actions = 5.0 * 30.0 + 5.0 * ui.spacing().item_spacing.x;
                let width = ui.available_width() - actions;
                let name = self.library.name(&setting.preset, lang);
                let response = widgets::dropdown_button(ui, &name, width, None);
                egui::Popup::menu(&response)
                    .frame(egui::Frame::NONE)
                    .width(width.max(240.0))
                    .show(|ui| {
                        widgets::popup_body(ui, |ui| {
                            ui.set_width(width.max(240.0) - 12.0);
                            widgets::section(ui, lang.tr("Стандартные", "Standard"));
                            for key in Library::standard_keys() {
                                let label = self.library.name(&key, lang);
                                if widgets::list_item(ui, &label, key == setting.preset) {
                                    chosen = Some(key);
                                }
                            }
                            if !self.library.user.is_empty() {
                                widgets::section(ui, lang.tr("Мои", "Mine"));
                                for key in self.library.user_keys() {
                                    let label = self.library.name(&key, lang);
                                    if widgets::list_item(ui, &label, key == setting.preset) {
                                        chosen = Some(key);
                                    }
                                }
                            }
                        });
                    });

                let new_hint = lang.tr("Сохранить как новый", "Save as new");
                if widgets::icon_button(ui, Icon::Plus, new_hint, true) {
                    new_entry = Some(NameEntry::New(format!(
                        "{} {}",
                        self.library.name(&setting.preset, lang),
                        lang.tr("копия", "copy")
                    )));
                }
                let rename_hint = lang.tr("Переименовать", "Rename");
                if widgets::icon_button(ui, Icon::Pencil, rename_hint, is_user) {
                    new_entry = Some(NameEntry::Rename(self.library.name(&setting.preset, lang)));
                }
                let delete_hint = lang.tr("Удалить", "Delete");
                delete = widgets::icon_button(ui, Icon::Trash, delete_hint, is_user);
                let import_hint = lang.tr("Загрузить из файла", "Load from a file");
                import = widgets::icon_button(ui, Icon::Import, import_hint, true);
                let export_hint = lang.tr("Сохранить в файл", "Save to a file");
                export = widgets::icon_button(ui, Icon::Export, export_hint, true);
            });

            if self.name_entry != NameEntry::None {
                ui.add_space(6.0);
                self.name_row(ui);
            }
            ui.add_space(6.0);
            let words = lang.tr("Подобрать под себя", "Find my settings");
            let hint = lang.tr(
                "Минутный тест: круги, точные попадания и броски. Получится пресет, от которого удобно отталкиваться",
                "A one minute test: circles, precise hits and flicks. It gives you a preset to start from",
            );
            tune = widgets::pill_button(ui, words).on_hover_text(hint).clicked();
        });

        if tune {
            self.start_tuning(ui.ctx());
        }
        if let Some(entry) = new_entry {
            self.name_entry = entry;
        }
        if let Some(key) = chosen {
            self.assign_preset(&mouse, key);
        }
        if delete {
            self.delete_preset();
        }
        if export {
            self.export_preset();
        }
        if import {
            self.import_preset();
        }
    }

    fn name_row(&mut self, ui: &mut Ui) {
        let lang = self.lang();
        let mut finish = false;
        let mut cancel = false;
        ui.horizontal(|ui| {
            let text = match &mut self.name_entry {
                NameEntry::New(text) | NameEntry::Rename(text) => text,
                NameEntry::None => return,
            };
            let width = ui.available_width() - 2.0 * 30.0 - 2.0 * ui.spacing().item_spacing.x;
            let (rect, _) = ui.allocate_exact_size(vec2(width, 30.0), Sense::hover());
            paint_rounded(ui.painter(), rect, RADIUS_CONTROL, BG, theme::BORDER);
            let field = egui::TextEdit::singleline(text)
                .frame(egui::Frame::NONE)
                .font(font(14.0))
                .desired_width(width - 24.0);
            let inner = rect.shrink2(vec2(12.0, 6.0));
            let response = ui.put(inner, field);
            if response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
                finish = true;
            }
            finish |= widgets::icon_button(ui, Icon::Check, lang.tr("Сохранить", "Save"), true);
            cancel = widgets::icon_button(ui, Icon::Close, lang.tr("Отмена", "Cancel"), true);
        });
        if finish {
            self.finish_name_entry();
        } else if cancel {
            self.name_entry = NameEntry::None;
        }
    }

    fn processing_section(&mut self, ui: &mut Ui) {
        let lang = self.lang();
        widgets::section(ui, lang.tr("Обработка", "Processing"));
        let m = &mut self.model;
        let mut changed = false;

        widgets::card(ui, |ui| {
            changed |= widgets::toggle_row(
                ui,
                lang.tr("Убрать двойные шаги", "Remove double steps"),
                &mut m.debeat_on,
                Some(lang.tr(
                    "Срезает пакет с двумя кадрами сенсора и раздаёт излишек дальше",
                    "Trims a packet that carries two sensor frames and spreads the excess",
                )),
            );
            if m.debeat_on {
                let mut release_ms = m.debeat.release_s * 1000.0;
                if widgets::slider_row(
                    ui,
                    lang.tr("Раздача излишка", "Excess release"),
                    &mut release_ms,
                    SliderSpec {
                        low: 10.0,
                        high: 120.0,
                        step: 1.0,
                        logarithmic: false,
                        decimals: 0,
                        unit: lang.tr(" мс", " ms"),
                    },
                ) {
                    m.debeat.release_s = release_ms / 1000.0;
                    changed = true;
                }
            }

            changed |= widgets::toggle_row(
                ui,
                lang.tr("Сглаживание", "Smoothing"),
                &mut m.ema_on,
                Some(lang.tr(
                    "Простое сглаживание с постоянной задержкой",
                    "Plain smoothing with a fixed delay",
                )),
            );
            if m.ema_on {
                changed |= widgets::slider_row(
                    ui,
                    lang.tr("Задержка", "Delay"),
                    &mut m.tau_ms,
                    SliderSpec {
                        low: 1.0,
                        high: 40.0,
                        step: 0.5,
                        logarithmic: true,
                        decimals: 1,
                        unit: lang.tr(" мс", " ms"),
                    },
                );
            }

            changed |= widgets::toggle_row(
                ui,
                lang.tr("Размазывание", "Spreading"),
                &mut m.spread_on,
                Some(lang.tr(
                    "Растягивает пакет на окно времени, чтобы кадрам монитора доставалось поровну",
                    "Stretches each packet over a window so that screen frames get even steps",
                )),
            );
            if m.spread_on {
                changed |= widgets::slider_row(
                    ui,
                    lang.tr("Окно", "Window"),
                    &mut m.spread_ms,
                    SliderSpec {
                        low: 2.0,
                        high: 34.0,
                        step: 0.5,
                        logarithmic: false,
                        decimals: 1,
                        unit: lang.tr(" мс", " ms"),
                    },
                );
            }

            changed |= widgets::toggle_row(
                ui,
                "One Euro",
                &mut m.one_euro_on,
                Some(lang.tr(
                    "Сильно сглаживает медленное движение и почти не трогает быстрое",
                    "Smooths slow motion hard and leaves fast motion nearly alone",
                )),
            );
            if m.one_euro_on {
                changed |= widgets::slider_row(
                    ui,
                    lang.tr("Срез", "Cutoff"),
                    &mut m.min_cutoff_hz,
                    SliderSpec {
                        low: 0.5,
                        high: 30.0,
                        step: 0.1,
                        logarithmic: true,
                        decimals: 1,
                        unit: lang.tr(" Гц", " Hz"),
                    },
                );
                changed |= widgets::slider_row(
                    ui,
                    "Beta",
                    &mut m.beta,
                    SliderSpec {
                        low: 0.001,
                        high: 0.5,
                        step: 0.001,
                        logarithmic: true,
                        decimals: 3,
                        unit: "",
                    },
                );
            }

            ui.add_space(2.0);
            changed |= widgets::slider_row(
                ui,
                lang.tr("Общая скорость", "Overall speed"),
                &mut m.factor,
                SliderSpec {
                    low: 0.1,
                    high: 2.0,
                    step: 0.05,
                    logarithmic: false,
                    decimals: 2,
                    unit: "",
                },
            );
        });

        if changed {
            self.apply_model();
        }
    }

    fn accel_section(&mut self, ui: &mut Ui, rect: Rect) {
        let lang = self.lang();
        let card = widgets::section_in(ui, rect, lang.tr("Ускорение", "Acceleration"));
        let now_us = self.engine.shared().clock.now_us();
        let speed_now = self.preview.speed(now_us);
        let m = &mut self.model;
        let editor = &mut self.curve_editor;
        // The card was sized for this state, so a click takes effect next frame.
        let open = m.accel_on;
        let mut changed = false;

        widgets::card_in(ui, card, |ui| {
            let size = vec2(ui.available_width(), widgets::SEGMENT_HEIGHT);
            let centred = egui::Layout::left_to_right(egui::Align::Center);
            ui.allocate_ui_with_layout(size, centred, |ui| {
                if open {
                    let current = READY_CURVES
                        .iter()
                        .position(|curve| m.accel_points == curve.2)
                        .unwrap_or(usize::MAX);
                    let names: Vec<&str> = READY_CURVES
                        .iter()
                        .map(|curve| lang.tr(curve.0, curve.1))
                        .collect();
                    if let Some(index) = widgets::segmented(ui, "curves", &names, current) {
                        m.accel_points = READY_CURVES[index].2.to_vec();
                        changed = true;
                    }
                } else {
                    let off = egui::RichText::new(lang.tr("Выключено", "Off"));
                    ui.label(off.font(font(13.0)).color(TEXT3));
                }
                let rest = vec2(ui.available_width(), widgets::SEGMENT_HEIGHT);
                let (row, _) = ui.allocate_exact_size(rest, Sense::hover());
                let id = ui.id().with("accel-switch");
                let hint = lang.tr(
                    "Быстрые движения улетают дальше, медленные остаются точными",
                    "Fast motion goes further, slow motion stays precise",
                );
                changed |= widgets::toggle_end(ui, row, &mut m.accel_on, id, Some(hint));
            });
            if !open {
                return;
            }

            ui.add_space(6.0);
            let height = (ui.available_height() - CURVE_SUMMARY).max(CURVE_MIN_HEIGHT);
            changed |= editor.show(ui, &mut m.accel_points, speed_now, height, lang);
            ui.add_space(4.0);
            widgets::small_text(ui, &describe(&m.accel_points, m.factor, lang), TEXT2);
        });

        if changed {
            self.apply_model();
        }
    }

    fn test_section(&mut self, ui: &mut Ui, rect: Rect) {
        let lang = self.lang();
        let now_us = self.engine.shared().clock.now_us();
        let heading = lang.tr("Тест: подвигай мышью", "Test: move the mouse");
        let card = widgets::section_in(ui, rect, heading);
        let legend_end = pos2(rect.right() - 2.0, rect.top() + widgets::SECTION_HEIGHT / 2.0);
        Preview::legend(ui, legend_end, lang);
        widgets::card_in(ui, card, |ui| {
            self.preview.show(ui, now_us);
        });
    }

    fn footer(&mut self, ui: &mut Ui, rect: Rect) {
        let lang = self.lang();
        let mut ui = ui.new_child(UiBuilder::new().max_rect(rect));
        ui.horizontal_centered(|ui| {
            let current = usize::from(lang == Lang::En);
            if let Some(index) = widgets::segmented(ui, "language", &["RU", "EN"], current) {
                self.config.language = if index == 1 { Lang::En } else { Lang::Ru };
                self.mark_unsaved();
            }
            ui.add_space(6.0);

            let settings_x = rect.right() - 30.0;
            let room = settings_x - ui.cursor().left() - 8.0;
            let (status_rect, _) = ui.allocate_exact_size(vec2(room, 30.0), Sense::hover());
            self.paint_status(ui, status_rect);

            let hint = lang.tr("Настройки", "Settings");
            if widgets::icon_button(ui, Icon::Settings, hint, true) {
                self.open_settings();
            }
            if matches!(self.updater.state(), UpdateState::Available(_)) {
                let at = pos2(rect.right() - 6.0, rect.center().y - 8.0);
                ui.painter().circle_filled(at, UPDATE_DOT_RADIUS, GOOD);
            }
        });
    }

    fn open_settings(&mut self) {
        let in_use = self.engine.is_capturing() && self.engine.backend() == Backend::Interception;
        self.driver_state = if in_use {
            DriverState::Ready
        } else if !library_path().is_file() {
            DriverState::NotInstalled
        } else if Interception::open().is_ok() {
            DriverState::Ready
        } else {
            DriverState::NotAnswering
        };
        self.page = Page::Settings;
        if self.config.check_updates && self.updater.state() == UpdateState::Unknown {
            self.updater.check(false);
        }
    }

    fn paint_status(&self, ui: &Ui, rect: Rect) {
        let lang = self.lang();
        let (text, color) = match &self.notice {
            Some((text, _)) => (text.clone(), WARN),
            None => self.status_line(lang),
        };
        ui.painter().with_clip_rect(rect).text(
            pos2(rect.left(), rect.center().y),
            Align2::LEFT_CENTER,
            text,
            font(13.0),
            color,
        );
    }

    fn status_line(&self, lang: Lang) -> (String, Color32) {
        let standing = self.engine.shared().standing();
        if standing.hands_off() {
            return (tray::status_text(standing, lang).to_string(), WARN);
        }
        let on: Vec<String> = self
            .mice
            .iter()
            .filter(|mouse| self.setting_of(&mouse.key).enabled)
            .map(|mouse| mouse_title(mouse, lang))
            .collect();
        if on.is_empty() {
            let text = lang.tr("Все мыши работают напрямую", "All mice work directly");
            return (text.to_string(), TEXT3);
        }
        (
            format!("{}: {}", lang.tr("Фильтр включён", "Filter on"), on.join(", ")),
            TEXT2,
        )
    }

    fn main_page(&mut self, ui: &mut Ui, area: Rect) {
        let lang = self.lang();
        let body = Rect::from_min_max(area.min, pos2(area.right(), area.bottom() - FOOTER_HEIGHT - GAP));
        let left = Rect::from_min_max(body.min, pos2(body.left() + LEFT_WIDTH, body.bottom()));
        let right = Rect::from_min_max(pos2(left.right() + GAP, body.top()), body.max);

        let mut column = ui.new_child(UiBuilder::new().max_rect(left));
        egui::ScrollArea::vertical()
            .id_salt("left")
            .auto_shrink([false, false])
            .show(&mut column, |ui| {
                if self.engine.shared().pointer().enhanced_precision == Some(true) {
                    banner(
                        ui,
                        lang.tr(
                            "Включена «Повышенная точность указателя»: со сглаживанием курсор станет медленнее",
                            "“Enhance pointer precision” is on: smoothing will slow the cursor down",
                        ),
                    );
                    ui.add_space(6.0);
                }
                self.mice_section(ui);
                ui.add_space(6.0);
                self.preset_section(ui);
                ui.add_space(6.0);
                self.processing_section(ui);
            });

        let (accel, test) = split_right(right, self.model.accel_on);
        self.accel_section(ui, accel);
        self.test_section(ui, test);

        let footer = Rect::from_min_max(pos2(area.left(), area.bottom() - FOOTER_HEIGHT), area.max);
        self.footer(ui, footer);
    }

    fn settings_page(&mut self, ui: &mut Ui, area: Rect) {
        let lang = self.lang();
        let width = area.width().min(SETTINGS_WIDTH);
        let column = Rect::from_center_size(area.center(), vec2(width, area.height()));
        let mut page = ui.new_child(UiBuilder::new().max_rect(column));
        let ui = &mut page;

        ui.horizontal(|ui| {
            if widgets::icon_button(ui, Icon::Back, lang.tr("Назад", "Back"), true) {
                self.page = Page::Main;
            }
            ui.label(egui::RichText::new(lang.tr("Настройки", "Settings")).font(semibold(16.0)));
        });
        ui.add_space(6.0);

        egui::ScrollArea::vertical()
            .id_salt("settings")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if let Some((text, _)) = self.notice.clone() {
                    banner(ui, &text);
                    ui.add_space(6.0);
                }
                self.general_section(ui);
                ui.add_space(6.0);
                self.update_section(ui);
                ui.add_space(6.0);
                self.backend_section(ui);
                ui.add_space(6.0);
                self.keys_section(ui);
                ui.add_space(6.0);
                self.about_section(ui);
            });
    }

    fn general_section(&mut self, ui: &mut Ui) {
        let lang = self.lang();
        widgets::section(ui, lang.tr("Общие", "General"));
        let mut wanted = self.autostart;
        let mut changed = false;
        let mut games_direct = self.config.games_direct;
        let mut games_changed = false;
        widgets::card(ui, |ui| {
            changed = widgets::toggle_row(
                ui,
                lang.tr("Запускать вместе с Windows", "Start with Windows"),
                &mut wanted,
                Some(lang.tr(
                    "Программа запустится в трее и сразу начнёт работать",
                    "The program starts in the tray and goes to work at once",
                )),
            );
            games_changed = widgets::toggle_row(
                ui,
                lang.tr("Не мешать играм", "Leave games alone"),
                &mut games_direct,
                Some(lang.tr(
                    "Игры читают мышь напрямую и получили бы движение дважды, поэтому, пока игра держит курсор, фильтр отходит в сторону",
                    "Games read the mouse directly and would get the motion twice, so the filter steps aside while a game holds the cursor",
                )),
            );
        });
        if games_changed {
            self.config.games_direct = games_direct;
            self.engine.shared().set_games_direct(games_direct);
            self.mark_unsaved();
        }
        if changed {
            match autostart::set_enabled(wanted) {
                Ok(()) => self.autostart = wanted,
                Err(error) => {
                    let text = lang.tr("Автозапуск не изменён", "Autostart not changed");
                    self.say(format!("{text}: {error}"));
                }
            }
        }
    }

    fn update_section(&mut self, ui: &mut Ui) {
        let lang = self.lang();
        widgets::section(ui, lang.tr("Обновления", "Updates"));
        let state = self.updater.state();
        let (words, color) = update_words(&state, lang);
        let mut wanted = self.config.check_updates;
        let mut switched = false;
        let mut check = false;
        let mut install = false;
        let mut notes = None;

        widgets::card(ui, |ui| {
            switched = widgets::toggle_row(
                ui,
                lang.tr("Проверять обновления", "Check for updates"),
                &mut wanted,
                Some(lang.tr(
                    "Дважды в сутки программа спрашивает у GitHub, не вышла ли новая версия. Скачивается она только по кнопке",
                    "Twice a day the program asks GitHub about a new version. It is only downloaded when you press the button",
                )),
            );
            ui.add_space(2.0);
            widgets::small_text(ui, &words, color);
            match &state {
                UpdateState::Available(release) => {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        let words = lang.tr("Обновить и перезапустить", "Update and restart");
                        install = widgets::pill_button(ui, words).clicked();
                        if widgets::pill_button(ui, lang.tr("Что нового", "What is new")).clicked() {
                            notes = Some(release.page.clone());
                        }
                    });
                }
                UpdateState::Unknown | UpdateState::UpToDate | UpdateState::Failed(_) => {
                    ui.add_space(4.0);
                    let words = lang.tr("Проверить сейчас", "Check now");
                    check = widgets::pill_button(ui, words).clicked();
                }
                UpdateState::Checking
                | UpdateState::Downloading { .. }
                | UpdateState::Ready => {}
            }
        });

        if switched {
            self.config.check_updates = wanted;
            self.next_update_check = Instant::now();
            self.mark_unsaved();
        }
        if check {
            self.updater.check(true);
        }
        if install {
            self.updater.install();
        }
        if let Some(page) = notes {
            filedialog::open_external(&page);
        }
    }

    fn backend_section(&mut self, ui: &mut Ui) {
        let lang = self.lang();
        widgets::section(ui, lang.tr("Способ работы", "How it works"));
        let current = usize::from(self.engine.backend() == Backend::Interception);
        let mut chosen = None;
        let driver_state = self.driver_state;
        let memory_integrity = self.memory_integrity;

        widgets::card(ui, |ui| {
            let labels = [
                lang.tr("Без драйвера", "No driver"),
                lang.tr("Драйвер Interception", "Interception driver"),
            ];
            chosen = widgets::segmented(ui, "backend", &labels, current);
            ui.add_space(6.0);
            let text = if current == 0 {
                lang.tr(
                    "Ничего не устанавливается и работает в обычных программах. Игры с Raw Input читают мышь напрямую, на них фильтр не действует.",
                    "Nothing is installed and it works in ordinary programs. Games using Raw Input read the mouse directly, so the filter does not reach them.",
                )
            } else {
                lang.tr(
                    "Фильтрует на уровне драйвера, в том числе для игр. Античиты могут реагировать на этот драйвер.",
                    "Filters at driver level, games included. Anti-cheat systems may object to this driver.",
                )
            };
            widgets::small_text(ui, text, TEXT2);

            if memory_integrity {
                ui.add_space(6.0);
                banner(
                    ui,
                    lang.tr(
                        "Включена изоляция ядра. Windows может не загрузить старый драйвер, и тогда после перезагрузки перестанут работать все мыши и клавиатуры. Поэтому в программе нет кнопки установки: ставьте его, только если он правда нужен, и сначала создайте точку восстановления.",
                        "Memory integrity is on. Windows may refuse this old driver, and then every mouse and keyboard stops working after a reboot. That is why there is no install button here: only install it if you really need it, and make a restore point first.",
                    ),
                );
            }

            ui.add_space(6.0);
            let status = match driver_state {
                DriverState::Ready => {
                    lang.tr("Драйвер установлен и готов", "The driver is installed and ready")
                }
                DriverState::NotAnswering => lang.tr(
                    "interception.dll на месте, но драйвер не отвечает",
                    "interception.dll is here, but the driver does not answer",
                ),
                DriverState::NotInstalled => {
                    lang.tr("Драйвер не установлен", "The driver is not installed")
                }
            };
            // A centred layout would fill all the height there is, so the row
            // gets a height of its own.
            let row = vec2(ui.available_width(), STATUS_ROW_HEIGHT);
            let centred = egui::Layout::left_to_right(egui::Align::Center);
            ui.allocate_ui_with_layout(row, centred, |ui| {
                widgets::small_text(ui, status, TEXT3);
                if widgets::pill_button(ui, lang.tr("Страница драйвера", "Driver page")).clicked() {
                    filedialog::open_external(DRIVER_RELEASES);
                }
            });
        });

        if let Some(index) = chosen {
            let backend = if index == 1 { Backend::Interception } else { Backend::Hook };
            match self.engine.set_backend(backend) {
                Ok(()) => {
                    self.config.backend = backend;
                    self.mark_unsaved();
                }
                Err(error) => {
                    let _ = self.engine.set_backend(Backend::Hook);
                    self.say(format!(
                        "{}: {error}",
                        lang.tr("Драйвер не включился", "The driver did not start")
                    ));
                }
            }
        }
    }

    fn keys_section(&mut self, ui: &mut Ui) {
        let lang = self.lang();
        widgets::section(ui, lang.tr("Клавиши", "Shortcuts"));
        let hotkey = self.config.hotkey.clone();
        widgets::card(ui, |ui| {
            key_row(ui, &hotkey, lang.tr("Следующий пресет для мыши в работе", "Next preset for the mouse in use"));
            key_row(
                ui,
                "Ctrl+Alt+Pause",
                lang.tr("Немедленно отпустить мышь и выйти", "Let go of the mouse and quit at once"),
            );
        });
    }

    fn about_section(&mut self, ui: &mut Ui) {
        let lang = self.lang();
        widgets::section(ui, lang.tr("О программе", "About"));
        let raw = self.engine.shared().raw_packets();
        let log_path = log::path();
        widgets::card(ui, |ui| {
            let version = env!("CARGO_PKG_VERSION");
            widgets::small_text(ui, &format!("IluTrackball {version}"), TEXT2);
            let events = format!(
                "{}: {raw}",
                lang.tr("Событий мыши получено", "Mouse events received")
            );
            widgets::small_text(ui, &events, TEXT3);
            if let Some(path) = &log_path {
                if path.is_file() {
                    ui.add_space(4.0);
                    if widgets::pill_button(ui, lang.tr("Открыть журнал", "Open the log")).clicked() {
                        filedialog::open_external(&path.to_string_lossy());
                    }
                }
            }
        });
    }
}

/// Shares out the right column. With acceleration off its card shrinks to one
/// row and the test takes the room.
fn split_right(column: Rect, accel_on: bool) -> (Rect, Rect) {
    let accel_bottom = if accel_on {
        column.bottom() - TEST_SECTION_HEIGHT - SECTION_GAP
    } else {
        column.top() + widgets::SECTION_STEP + 2.0 * CARD_PADDING + widgets::SEGMENT_HEIGHT
    };
    let accel = Rect::from_min_max(column.min, pos2(column.right(), accel_bottom));
    let test = Rect::from_min_max(pos2(column.left(), accel_bottom + SECTION_GAP), column.max);
    (accel, test)
}

/// The state of updates in words.
fn update_words(state: &UpdateState, lang: Lang) -> (String, Color32) {
    let version = update::VERSION;
    match state {
        UpdateState::Unknown => (format!("{} {version}", lang.tr("Версия", "Version")), TEXT2),
        UpdateState::Checking => (lang.tr("Проверяю…", "Checking…").to_string(), TEXT2),
        UpdateState::UpToDate => (
            format!("{} {version} {}", lang.tr("Версия", "Version"), lang.tr("последняя", "is the latest")),
            TEXT2,
        ),
        UpdateState::Available(release) => {
            let new = &release.version;
            let words = match lang {
                Lang::Ru => format!("Вышла версия {new}, у вас {version}"),
                Lang::En => format!("Version {new} is out, you have {version}"),
            };
            (words, GOOD)
        }
        UpdateState::Downloading { release, share } => (
            format!(
                "{} {}: {:.0} %",
                lang.tr("Загружаю версию", "Downloading version"),
                release.version,
                share * 100.0
            ),
            TEXT2,
        ),
        UpdateState::Ready => (lang.tr("Перезапускаю…", "Restarting…").to_string(), TEXT2),
        UpdateState::Failed(failure) => (failure_words(*failure, lang).to_string(), WARN),
    }
}

fn failure_words(failure: Failure, lang: Lang) -> &'static str {
    match failure {
        Failure::Network => lang.tr(
            "Не удалось связаться с GitHub. Проверьте интернет и попробуйте ещё раз",
            "Could not reach GitHub. Check the connection and try again",
        ),
        Failure::BadAnswer => lang.tr(
            "GitHub ответил что-то непонятное, обновление отменено",
            "GitHub gave an unexpected answer, the update was called off",
        ),
        Failure::Damaged => lang.tr(
            "Файл пришёл повреждённым и не установлен. Текущая версия не тронута",
            "The file arrived damaged and was not installed. This version is untouched",
        ),
        Failure::WontStart => lang.tr(
            "Новая версия не запускается и не установлена. Текущая версия не тронута",
            "The new version does not start and was not installed. This version is untouched",
        ),
        Failure::CannotWrite => lang.tr(
            "Нет права записи в папку программы. Запустите её от имени администратора или перенесите в другую папку",
            "The program cannot write to its folder. Run it as administrator or move it elsewhere",
        ),
    }
}

/// A recording played for show must never move the real cursor.
fn may_reach_the_cursor(key: &str) -> bool {
    key != DEMO_KEY
}

/// Width of the main screen in pixels, and how often it redraws.
fn primary_screen() -> (f64, f64) {
    const FALLBACK_HZ: f64 = 60.0;
    let width = unsafe { GetSystemMetrics(SM_CXSCREEN) };
    let mut mode: DEVMODEW = unsafe { std::mem::zeroed() };
    mode.dmSize = std::mem::size_of::<DEVMODEW>() as u16;
    let read = unsafe { EnumDisplaySettingsW(std::ptr::null(), ENUM_CURRENT_SETTINGS, &mut mode) };
    let refresh_hz = if read != 0 && mode.dmDisplayFrequency > 1 {
        f64::from(mode.dmDisplayFrequency)
    } else {
        FALLBACK_HZ
    };
    (f64::from(width), refresh_hz)
}

fn settings_from(config: &Config, library: &Library) -> HashMap<String, MouseSetting> {
    config
        .device
        .iter()
        .map(|entry| {
            let preset = PresetKey::parse(&entry.preset)
                .filter(|key| library.config(key).is_some())
                .unwrap_or_else(PresetKey::default_key);
            let config: PipelineConfig = library.config(&preset).unwrap_or_default();
            (
                entry.key.clone(),
                MouseSetting {
                    enabled: entry.enabled,
                    preset,
                    config,
                },
            )
        })
        .collect()
}

fn banner(ui: &mut Ui, text: &str) {
    let slot = ui.painter().add(egui::Shape::Noop);
    let outer = ui.available_rect_before_wrap();
    let mut inner = ui.new_child(UiBuilder::new().max_rect(outer.shrink2(vec2(12.0, 8.0))));
    widgets::small_text(&mut inner, text, WARN);
    let rect = Rect::from_min_max(
        outer.min,
        pos2(outer.right(), inner.min_rect().bottom() + 8.0),
    );
    ui.painter().set(
        slot,
        theme::rounded(rect, RADIUS_CONTROL, WARN_BG, Color32::TRANSPARENT),
    );
    ui.allocate_rect(rect, Sense::hover());
}

fn key_row(ui: &mut Ui, keys: &str, caption: &str) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::hover());
    let galley = ui
        .painter()
        .layout_no_wrap(keys.to_string(), semibold(12.0), TEXT2);
    let chip = Rect::from_min_size(
        pos2(rect.left(), rect.center().y - 11.0),
        vec2(galley.size().x + 16.0, 22.0),
    );
    paint_rounded(ui.painter(), chip, 7.0, theme::SURFACE, theme::BORDER);
    ui.painter().text(chip.center(), Align2::CENTER_CENTER, keys, semibold(12.0), TEXT2);
    ui.painter().text(
        pos2(chip.right() + 10.0, rect.center().y),
        Align2::LEFT_CENTER,
        caption,
        font(13.0),
        TEXT,
    );
}

impl eframe::App for SettingsApp {
    /// Runs for a hidden window too, unlike `ui`.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.window_events(ctx);
        self.refresh();
        self.look_after_updates(ctx);
        let moving = self.wizard.is_some()
            || self
                .last_motion
                .is_some_and(|last| last.elapsed() < ANIMATE_AFTER_MOTION);
        ctx.request_repaint_after(if self.hidden {
            HIDDEN_CHECK
        } else if moving {
            MOTION_REPAINT
        } else {
            IDLE_REPAINT
        });
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(BG))
            .show(ui, |ui| {
                let area = ui.max_rect().shrink(WINDOW_MARGIN);
                match self.page {
                    Page::Main => self.main_page(ui, area),
                    Page::Settings => self.settings_page(ui, area),
                    Page::Tuning => self.tuning_page(ui, area),
                }
            });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save_now();
        if self.start_new_version {
            self.updater.start_installed(self.hidden);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column() -> Rect {
        Rect::from_min_size(pos2(400.0, 16.0), vec2(500.0, 620.0))
    }

    #[test]
    fn every_state_of_an_update_has_words_in_both_languages() {
        let states = [
            UpdateState::Unknown,
            UpdateState::Checking,
            UpdateState::UpToDate,
            UpdateState::Ready,
            UpdateState::Failed(Failure::Network),
            UpdateState::Failed(Failure::BadAnswer),
            UpdateState::Failed(Failure::Damaged),
            UpdateState::Failed(Failure::WontStart),
            UpdateState::Failed(Failure::CannotWrite),
        ];
        for state in states {
            let (ru, _) = update_words(&state, Lang::Ru);
            let (en, _) = update_words(&state, Lang::En);
            assert!(!ru.is_empty() && !en.is_empty(), "{state:?}");
            assert_ne!(ru, en, "{state:?}");
        }
    }

    #[test]
    fn a_demo_recording_stays_away_from_the_real_cursor() {
        assert!(!may_reach_the_cursor(DEMO_KEY));
        assert!(may_reach_the_cursor("VID_046D&PID_B01D"));
    }

    #[test]
    fn the_test_keeps_its_height_under_an_open_curve() {
        let (accel, test) = split_right(column(), true);
        assert_eq!(test.height(), TEST_SECTION_HEIGHT);
        assert_eq!(test.bottom(), column().bottom());
        assert_eq!(test.top() - accel.bottom(), SECTION_GAP);
    }

    #[test]
    fn with_acceleration_off_the_test_takes_the_room() {
        let (accel, test) = split_right(column(), false);
        assert_eq!(accel.top(), column().top());
        assert_eq!(accel.height(), 78.0);
        assert_eq!(test.top() - accel.bottom(), SECTION_GAP);
        assert_eq!(test.bottom(), column().bottom());
    }
}
