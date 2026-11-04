//! ilumx-filter: filters the motion of the mice you pick and leaves the rest.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod autostart;
mod capture;
mod config;
mod curve_editor;
mod engine;
mod filedialog;
mod foreground;
mod hook;
mod hotkey;
mod i18n;
mod interception;
mod log;
mod model;
mod presets;
mod preview;
mod processor;
mod recovery;
mod tap;
mod theme;
mod timer;
mod tray;
mod ui;
mod widgets;
mod wizard;

use std::path::Path;
use std::ptr;
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui;
use ilumx_core::event_log::{read_csv, RawEvent};
use windows_sys::core::PCWSTR;
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS};
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    FindWindowW, MessageBoxW, SetForegroundWindow, ShowWindow, MB_ICONERROR, MB_OK, SW_RESTORE,
};

use config::Config;
use engine::Engine;
use hotkey::parse_hotkey;
use tray::Tray;
use ui::{SettingsApp, Startup};

const APP_NAME: &str = "IluTrackball";
const WINDOW_SIZE: [f32; 2] = [920.0, 700.0];
const MIN_WINDOW_SIZE: [f32; 2] = [860.0, 560.0];
const DEMO_FLAG: &str = "--demo";
/// The red ball, as 64 by 64 RGBA pixels. Explorer takes `assets/icon.ico`.
pub const ICON_RGBA: &[u8] = include_bytes!("../assets/icon_64.rgba");
pub const ICON_SIZE: u32 = 64;
/// How long a restarted copy waits for the failed one to be gone.
const LOCK_WAIT: Duration = Duration::from_secs(5);
const LOCK_RETRY: Duration = Duration::from_millis(50);

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let restarted = recovery::note_start(&args);
    if !take_the_program_lock(restarted) {
        show_other_copy();
        return;
    }
    recovery::watch_for_failures();
    if let Err(error) = run(&args) {
        log::write(&error);
        show_error(&error);
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

/// Two copies would both swallow and resend the same motion. The system closes
/// the mutex with the process, so a crash cannot lock the program out. A copy
/// that replaces a failed one gives it a moment to go.
fn take_the_program_lock(wait: bool) -> bool {
    let name = wide("Local\\IluTrackball.SingleInstance");
    let give_up_at = Instant::now() + LOCK_WAIT;
    loop {
        let handle = unsafe { CreateMutexW(ptr::null(), 1, name.as_ptr() as PCWSTR) };
        if handle.is_null() || unsafe { GetLastError() } != ERROR_ALREADY_EXISTS {
            return true;
        }
        unsafe { CloseHandle(handle) };
        if !wait || Instant::now() >= give_up_at {
            return false;
        }
        thread::sleep(LOCK_RETRY);
    }
}

fn show_other_copy() {
    if Tray::show_running_copy() {
        return;
    }
    let title = wide(APP_NAME);
    let window = unsafe { FindWindowW(ptr::null(), title.as_ptr() as PCWSTR) };
    if !window.is_null() {
        unsafe {
            ShowWindow(window, SW_RESTORE);
            SetForegroundWindow(window);
        }
    }
}

fn run(args: &[String]) -> Result<(), String> {
    let path = config::config_path();
    let mut problems = Vec::new();

    let config = config::load(&path).unwrap_or_else(|error| {
        problems.push(format!("{}: {error}", config::FILE_NAME));
        Config::default()
    });
    let loaded = config::resolve(config);
    problems.extend(loaded.problems);

    let hotkey = parse_hotkey(&loaded.config.hotkey)
        .map_err(|error| problems.push(error))
        .ok();

    autostart::follow_program();
    let start_minimized = args.iter().any(|arg| arg == autostart::MINIMIZED_FLAG);
    let demo = demo_log(args)?;
    let (engine, problem) = Engine::start(loaded.config.backend, hotkey, demo);
    problems.extend(problem);
    let notice = (!problems.is_empty()).then(|| problems.join("; "));

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(APP_NAME)
            .with_inner_size(WINDOW_SIZE)
            .with_min_inner_size(MIN_WINDOW_SIZE)
            .with_icon(window_icon()),
        ..Default::default()
    };
    let mut startup = Startup {
        config: loaded.config,
        config_path: path,
        library: loaded.library,
        migrated_preset: loaded.migrated_preset,
        notice,
        tray: None,
        start_hidden: false,
    };
    eframe::run_native(
        APP_NAME,
        options,
        Box::new(move |creation| {
            theme::install(&creation.egui_ctx);
            let ctx = creation.egui_ctx.clone();
            startup.tray = Tray::start(engine.shared().clone(), move || ctx.request_repaint())
                .map_err(|error| log::write(&format!("no tray icon: {error}")))
                .ok();
            // A window with no icon to bring it back must stay reachable.
            startup.start_hidden = start_minimized && startup.tray.is_some();
            if startup.start_hidden {
                creation
                    .egui_ctx
                    .send_viewport_cmd(egui::ViewportCommand::Visible(false));
            } else if start_minimized {
                creation
                    .egui_ctx
                    .send_viewport_cmd(egui::ViewportCommand::Minimized(true));
            }
            Ok(Box::new(SettingsApp::new(engine, startup)))
        }),
    )
    .map_err(|error| error.to_string())
}

/// `--demo <log.csv>` plays a recorded log in the test area.
fn demo_log(args: &[String]) -> Result<Option<Vec<RawEvent>>, String> {
    let Some(position) = args.iter().position(|arg| arg == DEMO_FLAG) else {
        return Ok(None);
    };
    let file = args.get(position + 1).ok_or("--demo: no file given")?;
    read_csv(Path::new(file))
        .map(Some)
        .map_err(|error| format!("{file}: {error}"))
}

fn window_icon() -> egui::IconData {
    egui::IconData {
        rgba: ICON_RGBA.to_vec(),
        width: ICON_SIZE,
        height: ICON_SIZE,
    }
}

/// The release build has no console, so a fatal error needs its own window.
fn show_error(message: &str) {
    let text = wide(message);
    let title = wide(APP_NAME);
    unsafe {
        MessageBoxW(
            ptr::null_mut(),
            text.as_ptr() as PCWSTR,
            title.as_ptr() as PCWSTR,
            MB_OK | MB_ICONERROR,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_icon_has_the_size_it_claims() {
        assert_eq!(ICON_RGBA.len() as u32, ICON_SIZE * ICON_SIZE * 4);
    }

    #[test]
    fn the_icon_is_round_and_red() {
        let pixel = |x: u32, y: u32| {
            let at = ((y * ICON_SIZE + x) * 4) as usize;
            &ICON_RGBA[at..at + 4]
        };
        assert_eq!(pixel(0, 0)[3], 0, "the corners are transparent");
        let middle = pixel(ICON_SIZE / 2, ICON_SIZE / 2);
        assert_eq!(middle[3], 255);
        assert!(middle[0] > middle[1], "{middle:?}");
    }
}
