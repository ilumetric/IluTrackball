//! The driver-free way to filter the real cursor.
//!
//! A low-level mouse hook swallows the motion of the MX Ergo, and the filtered
//! motion goes back in through `SendInput`. Nothing is installed: when the
//! program ends, Windows removes the hook. If the hook ever stalled, Windows
//! would skip it after a timeout.
//!
//! The hook cannot see which mouse an event came from. Raw Input does know, so
//! its thread reports the last device that moved. When the mouse in use changes
//! hands, one packet of the new mouse can land on the wrong side of that test.
//!
//! Three things can cut this path without a word from Windows. It removes a hook
//! that answered too slowly. Packets can stop reaching Raw Input. And while a
//! window of an elevated program is in front, `SendInput` claims success and does
//! nothing. Each one leaves the cursor unfiltered or frozen, so the counters here
//! let the watchdog notice them.

use std::io;
use std::mem;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};

use windows_sys::Win32::Foundation::{GetLastError, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::SystemInformation::GetTickCount64;
use windows_sys::Win32::System::Threading::{
    GetCurrentThread, GetCurrentThreadId, SetThreadPriority, THREAD_PRIORITY_HIGHEST,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_MOVE, MOUSEINPUT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, PostThreadMessageW, SetWindowsHookExW,
    UnhookWindowsHookEx, HC_ACTION, LLMHF_INJECTED, MSG, MSLLHOOKSTRUCT, WH_MOUSE_LL, WM_APP,
    WM_MOUSEMOVE, WM_QUIT,
};

use crate::engine::Sink;
use crate::log;

/// How long the hook stands aside after `SendInput` failed. Windows refuses
/// injection into windows of a higher integrity level, and swallowing motion
/// then would freeze the cursor over them.
const PAUSE_AFTER_FAILED_SEND_MS: u64 = 1_000;
/// Asks the hook thread to install the hook anew.
const WM_REINSTALL: u32 = WM_APP + 1;
/// Events one side must have seen before the silence of the other side means
/// something. A check period holds about twenty packets of a moving ball.
const MIN_EVENTS_FOR_CHECK: u64 = 6;
/// Marks mouse messages that a pen or a touch screen made up. Raw Input does
/// not report those as mouse packets.
const PEN_OR_TOUCH_MASK: usize = 0xFFFF_FF00;
const PEN_OR_TOUCH_SIGNATURE: usize = 0xFF51_5700;

static BLOCKING: AtomicBool = AtomicBool::new(false);
static STANDING_ASIDE: AtomicBool = AtomicBool::new(false);
static LAST_DEVICE_IS_TARGET: AtomicBool = AtomicBool::new(false);
static RESUME_AT_MS: AtomicU64 = AtomicU64::new(0);
static HOOK_THREAD: AtomicU32 = AtomicU32::new(0);
static HOOK_CALLS: AtomicU64 = AtomicU64::new(0);
static HOOK_REAL_CALLS: AtomicU64 = AtomicU64::new(0);
static TARGET_MOVES: AtomicU64 = AtomicU64::new(0);
static REAL_PACKETS: AtomicU64 = AtomicU64::new(0);
static SEND_FAILURES: AtomicU64 = AtomicU64::new(0);
static LAST_SEND_ERROR: AtomicU32 = AtomicU32::new(0);

/// Called by the Raw Input thread for each packet of a real device. A device it
/// cannot place counts as not ours: letting it through is the safe mistake.
pub fn note_last_device(is_target: bool) {
    REAL_PACKETS.fetch_add(1, Ordering::Relaxed);
    LAST_DEVICE_IS_TARGET.store(is_target, Ordering::Relaxed);
}

/// Called by the Raw Input thread for motion the hook should have seen.
pub fn note_target_motion() {
    TARGET_MOVES.fetch_add(1, Ordering::Relaxed);
}

/// Forgets which mouse moved last. If packets have stopped coming, that memory
/// would keep swallowing a mouse nothing can be sent for.
pub fn forget_last_device() {
    LAST_DEVICE_IS_TARGET.store(false, Ordering::Relaxed);
}

/// While set, all motion passes as if the program were not there.
pub fn stand_aside(aside: bool) {
    STANDING_ASIDE.store(aside, Ordering::Relaxed);
}

/// Times `SendInput` refused, and the error of the last refusal.
pub fn send_failures() -> (u64, u32) {
    (
        SEND_FAILURES.load(Ordering::Relaxed),
        LAST_SEND_ERROR.load(Ordering::Relaxed),
    )
}

/// What the counters say about the path of the motion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    /// Nothing moved, so nothing is known.
    Quiet,
    /// Motion went both ways.
    Fine,
    /// A filtered mouse moved and the hook was not called: it is gone.
    HookGone,
    /// The hook saw real motion and Raw Input saw none.
    PacketsGone,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Counts {
    target_moves: u64,
    real_packets: u64,
    hook_calls: u64,
    hook_real_calls: u64,
}

impl Counts {
    fn now() -> Self {
        Self {
            target_moves: TARGET_MOVES.load(Ordering::Relaxed),
            real_packets: REAL_PACKETS.load(Ordering::Relaxed),
            hook_calls: HOOK_CALLS.load(Ordering::Relaxed),
            hook_real_calls: HOOK_REAL_CALLS.load(Ordering::Relaxed),
        }
    }
}

/// Compares what the hook saw with what Raw Input saw.
#[derive(Debug, Default)]
pub struct HookHealth {
    last: Counts,
}

impl HookHealth {
    /// To be called now and then.
    pub fn check(&mut self) -> Link {
        self.compare(Counts::now())
    }

    fn compare(&mut self, now: Counts) -> Link {
        let moves = now.target_moves - self.last.target_moves;
        let packets = now.real_packets - self.last.real_packets;
        let calls = now.hook_calls - self.last.hook_calls;
        let real_calls = now.hook_real_calls - self.last.hook_real_calls;
        self.last = now;

        if moves >= MIN_EVENTS_FOR_CHECK && calls == 0 {
            Link::HookGone
        } else if real_calls >= MIN_EVENTS_FOR_CHECK && packets == 0 {
            Link::PacketsGone
        } else if calls > 0 || moves > 0 {
            Link::Fine
        } else {
            Link::Quiet
        }
    }
}

pub fn request_reinstall() {
    let thread = HOOK_THREAD.load(Ordering::Relaxed);
    if thread != 0 {
        unsafe { PostThreadMessageW(thread, WM_REINSTALL, 0, 0) };
    }
}

fn should_swallow(injected: bool, blocking: bool, last_is_target: bool, aside: bool) -> bool {
    blocking && last_is_target && !injected && !aside
}

fn is_pen_or_touch(extra_info: usize) -> bool {
    extra_info & PEN_OR_TOUCH_MASK == PEN_OR_TOUCH_SIGNATURE
}

fn now_ms() -> u64 {
    unsafe { GetTickCount64() }
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && wparam as u32 == WM_MOUSEMOVE {
        HOOK_CALLS.fetch_add(1, Ordering::Relaxed);
        let info = unsafe { &*(lparam as *const MSLLHOOKSTRUCT) };
        let injected = info.flags & LLMHF_INJECTED != 0;
        if !injected && !is_pen_or_touch(info.dwExtraInfo) {
            HOOK_REAL_CALLS.fetch_add(1, Ordering::Relaxed);
        }
        let swallow = should_swallow(
            injected,
            BLOCKING.load(Ordering::Relaxed),
            LAST_DEVICE_IS_TARGET.load(Ordering::Relaxed),
            STANDING_ASIDE.load(Ordering::Relaxed)
                || now_ms() < RESUME_AT_MS.load(Ordering::Relaxed),
        );
        if swallow {
            return 1;
        }
    }
    unsafe { CallNextHookEx(ptr::null_mut(), code, wparam, lparam) }
}

/// Injects filtered motion. Other programs and the hook see it as injected.
pub struct SendInputSink;

impl Sink for SendInputSink {
    fn emit(&self, _key: &str, dx: i32, dy: i32) {
        let input = INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx,
                    dy,
                    mouseData: 0,
                    dwFlags: MOUSEEVENTF_MOVE,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        let sent = unsafe { SendInput(1, &input, mem::size_of::<INPUT>() as i32) };
        if sent == 0 {
            LAST_SEND_ERROR.store(unsafe { GetLastError() }, Ordering::Relaxed);
            SEND_FAILURES.fetch_add(1, Ordering::Relaxed);
            RESUME_AT_MS.store(now_ms() + PAUSE_AFTER_FAILED_SEND_MS, Ordering::Relaxed);
        }
    }
}

/// An installed hook. Dropping it gives the mouse back.
pub struct Hook {
    thread_id: u32,
    thread: Option<JoinHandle<()>>,
}

impl Hook {
    pub fn install() -> io::Result<Self> {
        let (ready_tx, ready_rx) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("mouse-hook".to_string())
            .spawn(move || run_hook_thread(ready_tx))?;
        let thread_id = ready_rx
            .recv()
            .map_err(|_| io::Error::other("hook thread stopped during startup"))??;
        Ok(Self {
            thread_id,
            thread: Some(thread),
        })
    }

    /// Starts swallowing motion. Nothing is swallowed before this.
    pub fn start_blocking(&self) {
        BLOCKING.store(true, Ordering::Relaxed);
    }
}

impl Drop for Hook {
    fn drop(&mut self) {
        BLOCKING.store(false, Ordering::Relaxed);
        stand_aside(false);
        unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0) };
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run_hook_thread(ready: mpsc::Sender<io::Result<u32>>) {
    // A slow hook would slow every mouse in the system.
    unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST) };

    let module = unsafe { GetModuleHandleW(ptr::null()) };
    let install = || unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(hook_proc), module, 0) };
    let mut hook = install();
    if hook.is_null() {
        let _ = ready.send(Err(io::Error::last_os_error()));
        return;
    }

    let thread = unsafe { GetCurrentThreadId() };
    HOOK_THREAD.store(thread, Ordering::Relaxed);
    let _ = ready.send(Ok(thread));

    let mut message: MSG = unsafe { mem::zeroed() };
    while unsafe { GetMessageW(&mut message, ptr::null_mut(), 0, 0) } > 0 {
        if message.message == WM_REINSTALL {
            unsafe { UnhookWindowsHookEx(hook) };
            hook = install();
            if hook.is_null() {
                log::write(&format!(
                    "the mouse hook did not come back: {}",
                    io::Error::last_os_error()
                ));
            }
        }
        unsafe { DispatchMessageW(&message) };
    }

    HOOK_THREAD.store(0, Ordering::Relaxed);
    unsafe { UnhookWindowsHookEx(hook) };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counts(target_moves: u64, real_packets: u64, hook_calls: u64) -> Counts {
        Counts {
            target_moves,
            real_packets,
            hook_calls,
            hook_real_calls: hook_calls,
        }
    }

    #[test]
    fn a_hook_that_answers_is_healthy() {
        let mut health = HookHealth::default();
        assert_eq!(health.compare(counts(100, 100, 90)), Link::Fine);
        assert_eq!(health.compare(counts(200, 200, 180)), Link::Fine);
    }

    #[test]
    fn motion_without_hook_calls_means_the_hook_is_gone() {
        let mut health = HookHealth::default();
        health.compare(counts(100, 100, 90));
        assert_eq!(health.compare(counts(200, 200, 90)), Link::HookGone);
    }

    #[test]
    fn hook_calls_without_packets_mean_the_packets_are_gone() {
        let mut health = HookHealth::default();
        health.compare(counts(100, 100, 90));
        assert_eq!(health.compare(counts(100, 100, 180)), Link::PacketsGone);
    }

    #[test]
    fn our_own_output_says_nothing_about_the_packets() {
        let mut health = HookHealth::default();
        health.compare(counts(100, 100, 90));
        let injected_only = Counts {
            hook_calls: 180,
            ..counts(100, 100, 90)
        };
        assert_eq!(health.compare(injected_only), Link::Fine);
    }

    #[test]
    fn a_resting_mouse_says_nothing_about_the_hook() {
        let mut health = HookHealth::default();
        health.compare(counts(100, 100, 90));
        assert_eq!(health.compare(counts(100, 100, 90)), Link::Quiet);
        assert_eq!(health.compare(counts(101, 101, 90)), Link::Fine);
    }

    #[test]
    fn pen_and_touch_are_told_apart_from_a_mouse() {
        assert!(is_pen_or_touch(0xFF51_5701));
        assert!(!is_pen_or_touch(0));
        assert!(!is_pen_or_touch(0x1234));
    }

    #[test]
    fn swallows_only_real_motion_of_the_target() {
        assert!(should_swallow(false, true, true, false));
        assert!(!should_swallow(true, true, true, false), "injected");
        assert!(!should_swallow(false, false, true, false), "not blocking");
        assert!(!should_swallow(false, true, false, false), "another mouse");
        assert!(!should_swallow(false, true, true, true), "standing aside");
    }
}
