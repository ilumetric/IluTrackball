//! ilumx-filter: filters the motion of the mice you pick and leaves the rest.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod autostart;
mod capture;
mod config;
mod curve_editor;
mod elevation;
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
mod update;
mod widgets;
mod wizard;

use std::path::Path;
use std::ptr;
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui;
use ilumx_core::event_log::{read_csv, RawEvent};
use windows_sys::core::PCWSTR;
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS,
};
use windows_sys::Win32::System::Threading::{CreateMutexW, OpenMutexW};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    FindWindowW, MessageBoxW, SetForegroundWindow, ShowWindow, MB_ICONERROR, MB_OK, SW_RESTORE,
};

use config::Config;
use elevation::TaskMode;
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
const LOCK_NAME: &str = "Local\\IluTrackball.SingleInstance";
/// How long a copy waits for the one it asked the task to start.
const TASK_START_LIMIT: Duration = Duration::from_secs(3);
/// The right to wait on a mutex: all it takes to learn that it is there.
const SYNCHRONIZE: u32 = 0x0010_0000;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == update::SELF_TEST_FLAG) {
        std::process::exit(update::SELF_TEST_CODE);
    }
    if let Some(code) = elevation::run_as_helper(&args) {
        std::process::exit(code);
    }
    let replaces_a_copy = recovery::note_start(&args);
    if !replaces_a_copy && hand_over_to_task(&args) {
        return;
    }
    if !take_the_program_lock(replaces_a_copy) {
        show_other_copy();
        return;
    }
    recovery::watch_for_failures();
    update::tidy_up(args.iter().any(|arg| arg == update::UPDATED_FLAG));
    if let Err(error) = run(&args) {
        log::write(&error);
        show_error(&error);
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

/// With the administrator task in place, a start by hand goes through it, so
/// the rights do not depend on how the program was started. If the task starts
/// nothing, this copy carries on: a filter without rights beats no filter.
fn hand_over_to_task(args: &[String]) -> bool {
    let not_needed = args.iter().any(|arg| arg == DEMO_FLAG)
        || foreground::own_process_elevated()
        || elevation::current() == TaskMode::Off
        || a_copy_is_running();
    if not_needed {
        return false;
    }
    if !args.iter().any(|arg| arg == autostart::MINIMIZED_FLAG) {
        elevation::ask_for_window();
    }
    if elevation::start_through_task() {
        let give_up_at = Instant::now() + TASK_START_LIMIT;
        while Instant::now() < give_up_at {
            if a_copy_is_running() {
                return true;
            }
            thread::sleep(LOCK_RETRY);
        }
    }
    log::write("the administrator task did not start, working without its rights");
    false
}

/// A copy with full rights keeps its lock shut to this one, which also says
/// that it is there.
fn a_copy_is_running() -> bool {
    let name = wide(LOCK_NAME);
    let handle = unsafe { OpenMutexW(SYNCHRONIZE, 0, name.as_ptr() as PCWSTR) };
    if handle.is_null() {
        return unsafe { GetLastError() } == ERROR_ACCESS_DENIED;
    }
    unsafe { CloseHandle(handle) };
    true
}

/// Two copies would both swallow and resend the same motion. The system closes
/// the mutex with the process, so a crash cannot lock the program out. A copy
/// that replaces a failed one gives it a moment to go.
fn take_the_program_lock(wait: bool) -> bool {
    let name = wide(LOCK_NAME);
    let give_up_at = Instant::now() + LOCK_WAIT;
    loop {
        let handle = unsafe { CreateMutexW(ptr::null(), 1, name.as_ptr() as PCWSTR) };
        let error = unsafe { GetLastError() };
        if !handle.is_null() {
            if error != ERROR_ALREADY_EXISTS {
                return true;
            }
            unsafe { CloseHandle(handle) };
        } else if error != ERROR_ACCESS_DENIED {
            return true;
        }
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
    if foreground::own_process_elevated() {
        log::write("running with administrator rights");
    }
    let window_asked_for = elevation::take_window_request();
    let start_minimized =
        !window_asked_for && args.iter().any(|arg| arg == autostart::MINIMIZED_FLAG);
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
