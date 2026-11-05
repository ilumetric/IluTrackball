//! What the window in front means for the filter.
//!
//! Two kinds of window take the mouse out of the filter's hands. While an
//! elevated program is in front, Windows lets `SendInput` report success and
//! throws the motion away, so swallowing the real motion would freeze the
//! cursor. A game reads Raw Input directly: the hook cannot take motion away
//! from it, and motion sent back in would reach it a second time.

use std::ffi::c_void;
use std::mem;
use std::ptr;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, HWND, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows_sys::Win32::Security::{
    GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TokenElevation,
    TokenElevationType, TokenElevationTypeLimited, TokenIntegrityLevel, TOKEN_ELEVATION,
    TOKEN_ELEVATION_TYPE, TOKEN_MANDATORY_LABEL, TOKEN_QUERY,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, QueryFullProcessImageNameW,
    PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetClipCursor, GetCursorInfo, GetForegroundWindow, GetSystemMetrics, GetWindowRect,
    GetWindowThreadProcessId, CURSORINFO, CURSOR_SHOWING, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
    SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

const NAME_CHARS: usize = 512;
/// The integrity level of an ordinary program.
const MEDIUM_INTEGRITY: u32 = 0x2000;
/// Room for the label of a process and its identifier.
const LABEL_WORDS: usize = 16;

/// What kind of window is in front.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Foreground {
    #[default]
    Ordinary,
    /// A program above this one: nothing can be sent to it.
    Elevated,
    /// Something that took the cursor for itself, as games do.
    Game,
}

/// Follows the window in front. Questions about a process are asked once, when
/// it comes to the front: this runs many times a second.
pub struct ForegroundWatch {
    own_integrity: u32,
    process: u32,
    above: bool,
    name: String,
}

impl ForegroundWatch {
    pub fn new() -> Self {
        Self {
            own_integrity: integrity(unsafe { GetCurrentProcess() }).unwrap_or(MEDIUM_INTEGRITY),
            process: 0,
            above: false,
            name: String::new(),
        }
    }

    pub fn look(&mut self, games_too: bool) -> Foreground {
        let window = unsafe { GetForegroundWindow() };
        if window.is_null() {
            return Foreground::Ordinary;
        }
        let mut process = 0u32;
        unsafe { GetWindowThreadProcessId(window, &mut process) };
        if process != self.process {
            self.process = process;
            self.study_process();
        }
        if self.above {
            Foreground::Elevated
        } else if games_too && took_the_cursor(window) {
            Foreground::Game
        } else {
            Foreground::Ordinary
        }
    }

    /// The name of the program in front, for the log.
    pub fn name(&self) -> &str {
        &self.name
    }

    fn study_process(&mut self) {
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, self.process) };
        if process.is_null() {
            self.name = format!("process {}", self.process);
            self.above = is_above(self.own_integrity, None);
            return;
        }
        self.name = image_name(process).unwrap_or_else(|| format!("process {}", self.process));
        self.above = is_above(self.own_integrity, integrity(process));
        unsafe { CloseHandle(process) };
    }
}

/// Windows throws away what a program sends to a higher one. Even a program with
/// administrator rights has services above it. A process that keeps its door
/// shut counts as higher: standing aside for nothing only costs the smoothing,
/// while the opposite freezes the cursor.
fn is_above(own: u32, theirs: Option<u32>) -> bool {
    theirs.is_none_or(|theirs| theirs > own)
}

fn integrity(process: HANDLE) -> Option<u32> {
    let mut token = ptr::null_mut();
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
        return None;
    }
    let mut buffer = [0u64; LABEL_WORDS];
    let mut size = 0u32;
    let read = unsafe {
        GetTokenInformation(
            token,
            TokenIntegrityLevel,
            buffer.as_mut_ptr() as *mut c_void,
            mem::size_of_val(&buffer) as u32,
            &mut size,
        )
    };
    unsafe { CloseHandle(token) };
    if read == 0 {
        return None;
    }
    let label = buffer.as_ptr() as *const TOKEN_MANDATORY_LABEL;
    unsafe {
        let sid = (*label).Label.Sid;
        let parts = *GetSidSubAuthorityCount(sid);
        (parts > 0).then(|| *GetSidSubAuthority(sid, u32::from(parts) - 1))
    }
}

/// True if this process has administrator rights.
pub fn own_process_elevated() -> bool {
    is_elevated(unsafe { GetCurrentProcess() }) == Some(true)
}

/// True if Windows can give this process full rights for the same account.
/// Another account behind the prompt would get the task, not this person.
pub fn own_account_can_elevate() -> bool {
    if own_process_elevated() {
        return true;
    }
    let mut token = ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return false;
    }
    let mut kind: TOKEN_ELEVATION_TYPE = 0;
    let mut size = 0u32;
    let read = unsafe {
        GetTokenInformation(
            token,
            TokenElevationType,
            &mut kind as *mut TOKEN_ELEVATION_TYPE as *mut c_void,
            mem::size_of::<TOKEN_ELEVATION_TYPE>() as u32,
            &mut size,
        )
    };
    unsafe { CloseHandle(token) };
    read != 0 && kind == TokenElevationTypeLimited
}

fn is_elevated(process: HANDLE) -> Option<bool> {
    let mut token = ptr::null_mut();
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
        return None;
    }
    let mut elevation = TOKEN_ELEVATION {
        TokenIsElevated: 0,
    };
    let mut size = 0u32;
    let read = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            &mut elevation as *mut TOKEN_ELEVATION as *mut c_void,
            mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut size,
        )
    };
    unsafe { CloseHandle(token) };
    (read != 0).then_some(elevation.TokenIsElevated != 0)
}

fn image_name(process: HANDLE) -> Option<String> {
    let mut buffer = [0u16; NAME_CHARS];
    let mut length = NAME_CHARS as u32;
    let read = unsafe {
        QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, buffer.as_mut_ptr(), &mut length)
    };
    if read == 0 {
        return None;
    }
    let path = String::from_utf16_lossy(&buffer[..length as usize]);
    path.rsplit('\\').next().map(str::to_string)
}

/// A hidden cursor alone proves little: a video player hides it too, and so
/// does a text editor during typing. Taking a whole monitor or locking the
/// cursor in is what a program does when it wants the mouse for itself.
fn looks_like_a_game(cursor_hidden: bool, whole_monitor: bool, cursor_locked: bool) -> bool {
    cursor_hidden && (whole_monitor || cursor_locked)
}

fn took_the_cursor(window: HWND) -> bool {
    cursor_hidden() && looks_like_a_game(true, covers_a_monitor(window), cursor_locked())
}

fn cursor_hidden() -> bool {
    let mut info: CURSORINFO = unsafe { mem::zeroed() };
    info.cbSize = mem::size_of::<CURSORINFO>() as u32;
    if unsafe { GetCursorInfo(&mut info) } == 0 {
        return false;
    }
    info.flags & CURSOR_SHOWING == 0 || info.hCursor.is_null()
}

fn cursor_locked() -> bool {
    let mut clip: RECT = unsafe { mem::zeroed() };
    if unsafe { GetClipCursor(&mut clip) } == 0 {
        return false;
    }
    let desktop = unsafe {
        let (left, top) = (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN));
        RECT {
            left,
            top,
            right: left + GetSystemMetrics(SM_CXVIRTUALSCREEN),
            bottom: top + GetSystemMetrics(SM_CYVIRTUALSCREEN),
        }
    };
    (clip.right - clip.left) < (desktop.right - desktop.left)
        || (clip.bottom - clip.top) < (desktop.bottom - desktop.top)
}

fn covers_a_monitor(window: HWND) -> bool {
    let mut area: RECT = unsafe { mem::zeroed() };
    if unsafe { GetWindowRect(window, &mut area) } == 0 {
        return false;
    }
    let monitor = unsafe { MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST) };
    let mut info: MONITORINFO = unsafe { mem::zeroed() };
    info.cbSize = mem::size_of::<MONITORINFO>() as u32;
    if unsafe { GetMonitorInfoW(monitor, &mut info) } == 0 {
        return false;
    }
    let screen = info.rcMonitor;
    area.left <= screen.left
        && area.top <= screen.top
        && area.right >= screen.right
        && area.bottom >= screen.bottom
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hidden_cursor_alone_is_no_game() {
        assert!(!looks_like_a_game(true, false, false));
        assert!(!looks_like_a_game(false, true, true));
    }

    #[test]
    fn a_hidden_cursor_on_a_whole_monitor_or_locked_in_is_a_game() {
        assert!(looks_like_a_game(true, true, false));
        assert!(looks_like_a_game(true, false, true));
    }

    #[test]
    fn only_a_higher_process_or_a_closed_one_is_above() {
        const HIGH: u32 = 0x3000;
        const SYSTEM: u32 = 0x4000;
        assert!(!is_above(MEDIUM_INTEGRITY, Some(MEDIUM_INTEGRITY)));
        assert!(is_above(MEDIUM_INTEGRITY, Some(HIGH)));
        assert!(!is_above(HIGH, Some(HIGH)));
        assert!(is_above(HIGH, Some(SYSTEM)));
        assert!(is_above(HIGH, None));
    }

    #[test]
    fn this_process_has_a_level_of_integrity() {
        let level = integrity(unsafe { GetCurrentProcess() }).unwrap();
        assert!(level >= MEDIUM_INTEGRITY, "{level:#x}");
    }

    #[test]
    fn this_process_is_not_elevated_or_can_tell() {
        let answer = is_elevated(unsafe { GetCurrentProcess() });
        assert!(answer.is_some());
    }

    #[test]
    fn the_program_in_front_gets_a_name_or_a_number() {
        let name = image_name(unsafe { GetCurrentProcess() }).unwrap();
        assert!(name.to_ascii_lowercase().ends_with(".exe"), "{name}");
    }
}
