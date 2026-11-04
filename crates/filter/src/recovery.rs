//! Coming back after a failure.
//!
//! A program that lives in the tray has nobody watching it. If it dies, the
//! mouse simply goes back to normal and nothing says why. So a failure that can
//! only end the process starts a fresh copy first. A copy that fails again right
//! away stays down: a restart loop on the mouse path would be worse.

use std::env;
use std::panic::{self, PanicHookInfo};
use std::process::Command;
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

use windows_sys::Win32::System::Diagnostics::Debug::{
    SetUnhandledExceptionFilter, EXCEPTION_POINTERS,
};

use crate::autostart::MINIMIZED_FLAG;
use crate::engine::emergency_exit;
use crate::log;
use crate::update;

/// Tells a new copy that it replaces one that failed.
pub const RESTARTED_FLAG: &str = "--restarted";
/// A copy that was restarted or updated has to live this long to be trusted.
/// If it fails sooner it will not start another.
pub const TRIAL_LIFE: Duration = Duration::from_secs(60);
/// What Rust names the thread that runs `main`, and so the window.
const MAIN_THREAD: &str = "main";

struct Start {
    at: Instant,
    restarted: bool,
    updated: bool,
}

static START: OnceLock<Start> = OnceLock::new();

/// Remembers how this copy came to life. Returns true if it replaces another,
/// which may still be on its way out.
pub fn note_start(args: &[String]) -> bool {
    let has = |flag: &str| args.iter().any(|arg| arg == flag);
    let (restarted, updated) = (has(RESTARTED_FLAG), has(update::UPDATED_FLAG));
    let _ = START.set(Start {
        at: Instant::now(),
        restarted,
        updated,
    });
    restarted || updated
}

fn may_restart(on_trial: bool, lived: Duration) -> bool {
    !on_trial || lived >= TRIAL_LIFE
}

/// Starts a fresh copy, if that can still help. The copy waits for this one to
/// let go of the single instance lock.
fn start_fresh_copy() {
    let start = START.get();
    let new_version_failed =
        start.is_some_and(|start| start.updated && start.at.elapsed() < TRIAL_LIFE);
    if new_version_failed && update::roll_back() {
        log::write("the new version failed, went back to the old one");
        return;
    }
    let allowed = start.is_none_or(|start| {
        may_restart(start.restarted || start.updated, start.at.elapsed())
    });
    if !allowed {
        log::write("not restarting: the last restart did not help");
        return;
    }
    let Ok(program) = env::current_exe() else {
        return;
    };
    let mut command = Command::new(&program);
    command.args([MINIMIZED_FLAG, RESTARTED_FLAG]);
    if let Some(folder) = program.parent() {
        command.current_dir(folder);
    }
    match command.spawn() {
        Ok(_) => log::write("started a fresh copy"),
        Err(error) => log::write(&format!("could not start a fresh copy: {error}")),
    }
}

/// Ends this copy at once and lets a fresh one take over.
pub fn restart(reason: &str) -> ! {
    log::write(reason);
    start_fresh_copy();
    emergency_exit();
}

/// Ends the process here and now. Left to itself, Windows would hold it for its
/// error report and keep the lock from the fresh copy meanwhile.
unsafe extern "system" fn on_crash(_info: *const EXCEPTION_POINTERS) -> i32 {
    restart("the program crashed");
}

fn on_panic(info: &PanicHookInfo) {
    log::write(&format!("panic: {info}"));
    // The event loop of the window takes panics in and carries on with no
    // window, so it is no use waiting for this one to reach `main`.
    if thread::current().name() == Some(MAIN_THREAD) {
        restart("the window thread failed, starting over");
    }
}

/// Logs panics, the release build having no console, and starts over if the
/// window is lost. The other threads look after themselves. A crash in native
/// code is covered too, a graphics driver being the likely one.
pub fn watch_for_failures() {
    panic::set_hook(Box::new(on_panic));
    unsafe { SetUnhandledExceptionFilter(Some(on_crash)) };
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMIT: Duration = TRIAL_LIFE;

    #[test]
    fn a_first_failure_restarts_at_any_age() {
        assert!(may_restart(false, Duration::ZERO));
    }

    #[test]
    fn a_copy_that_fails_right_after_a_restart_stays_down() {
        assert!(!may_restart(true, LIMIT - Duration::from_secs(1)));
    }

    #[test]
    fn a_restarted_copy_that_lived_for_a_while_may_restart() {
        assert!(may_restart(true, LIMIT));
    }

    #[test]
    fn both_ways_of_replacing_a_copy_make_the_new_one_wait_its_turn() {
        // `note_start` keeps its first answer, so only the result is looked at.
        let arguments = |flag: &str| vec!["--minimized".to_string(), flag.to_string()];
        assert!(note_start(&arguments(RESTARTED_FLAG)));
        assert!(note_start(&arguments(update::UPDATED_FLAG)));
        assert!(!note_start(&arguments("--demo")));
    }
}
