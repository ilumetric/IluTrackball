//! A roughly one millisecond sleep for the ticker.

use std::ptr;
use std::thread;
use std::time::Duration;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::Threading::{
    CreateWaitableTimerExW, SetWaitableTimer, WaitForSingleObject,
    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, TIMER_ALL_ACCESS,
};

/// One millisecond from now, in the 100 ns units of a waitable timer.
const ONE_MS_RELATIVE: i64 = -10_000;
/// Longest wait for the timer, so a failed timer cannot stop the ticker.
const WAIT_LIMIT_MS: u32 = 10;

/// A high resolution timer sleeps precisely without changing the timer
/// resolution of the whole system.
pub struct MillisecondTimer {
    handle: HANDLE,
}

impl MillisecondTimer {
    pub fn new() -> Self {
        let handle = unsafe {
            CreateWaitableTimerExW(
                ptr::null(),
                ptr::null(),
                CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                TIMER_ALL_ACCESS,
            )
        };
        Self { handle }
    }

    pub fn wait(&self) {
        if self.handle.is_null() {
            thread::sleep(Duration::from_millis(1));
            return;
        }
        unsafe {
            SetWaitableTimer(
                self.handle,
                &ONE_MS_RELATIVE,
                0,
                None,
                ptr::null(),
                0,
            );
            WaitForSingleObject(self.handle, WAIT_LIMIT_MS);
        }
    }
}

impl Drop for MillisecondTimer {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe { CloseHandle(self.handle) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn waits_about_a_millisecond() {
        let timer = MillisecondTimer::new();
        let start = Instant::now();
        for _ in 0..50 {
            timer.wait();
        }
        let elapsed = start.elapsed();
        assert!(elapsed >= Duration::from_millis(40), "{elapsed:?}");
        assert!(elapsed < Duration::from_millis(500), "{elapsed:?}");
    }
}
