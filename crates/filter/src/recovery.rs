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

/// Tells a new copy that it replaces one that failed.
pub const RESTARTED_FLAG: &str = "--restarted";
/// A restarted copy that fails sooner than this will not start another.
const MIN_LIFE_AFTER_RESTART: Duration = Duration::from_secs(60);
/// What Rust names the thread that runs `main`, and so the window.
const MAIN_THREAD: &str = "main";

struct Start {
    at: Instant,
    restarted: bool,
}

static START: OnceLock<Start> = OnceLock::new();

/// Remembers how this copy came to life. Returns true if it replaces another.
pub fn note_start(args: &[String]) -> bool {
    let restarted = args.iter().any(|arg| arg == RESTARTED_FLAG);
    let _ = START.set(Start {
        at: Instant::now(),
        restarted,
    });
    restarted
}

fn may_restart(restarted: bool, lived: Duration) -> bool {
    !restarted || lived >= MIN_LIFE_AFTER_RESTART
}

/// Starts a fresh copy, if that can still help. The copy waits for this one to
/// let go of the single instance lock.
fn start_fresh_copy() {
    let allowed = START
        .get()
        .is_none_or(|start| may_restart(start.restarted, start.at.elapsed()));
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

    const LIMIT: Duration = MIN_LIFE_AFTER_RESTART;

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
}
