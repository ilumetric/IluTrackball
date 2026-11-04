//! Read-only access to the Windows pointer settings.

use std::ffi::c_void;
use std::mem;

use ilumx_core::pointer::PointerSettings;
use windows_sys::core::PCWSTR;
use windows_sys::Win32::Foundation::{ERROR_SUCCESS, SYSTEMTIME};
use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD};
use windows_sys::Win32::System::SystemInformation::GetLocalTime;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    SystemParametersInfoW, SPI_GETMOUSE, SPI_GETMOUSESPEED,
};

pub fn read_pointer_settings() -> PointerSettings {
    let mut speed = 0i32;
    let speed_ok = unsafe {
        SystemParametersInfoW(
            SPI_GETMOUSESPEED,
            0,
            &mut speed as *mut i32 as *mut c_void,
            0,
        )
    } != 0;

    // [threshold1, threshold2, acceleration]; nonzero acceleration means
    // "Enhance pointer precision" is on.
    let mut mouse = [0i32; 3];
    let mouse_ok =
        unsafe { SystemParametersInfoW(SPI_GETMOUSE, 0, mouse.as_mut_ptr() as *mut c_void, 0) }
            != 0;

    PointerSettings {
        speed: speed_ok.then_some(speed as u32),
        enhanced_precision: mouse_ok.then_some(mouse[2] != 0),
    }
}

/// True if "Memory integrity" (core isolation) is on. Windows then refuses
/// drivers built before it existed.
pub fn memory_integrity_on() -> bool {
    const KEY: &str =
        "SYSTEM\\CurrentControlSet\\Control\\DeviceGuard\\Scenarios\\HypervisorEnforcedCodeIntegrity\0";
    const VALUE: &str = "Enabled\0";
    let key: Vec<u16> = KEY.encode_utf16().collect();
    let value: Vec<u16> = VALUE.encode_utf16().collect();
    let mut enabled = 0u32;
    let mut size = mem::size_of::<u32>() as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr() as PCWSTR,
            value.as_ptr() as PCWSTR,
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            &mut enabled as *mut u32 as *mut c_void,
            &mut size,
        )
    };
    status == ERROR_SUCCESS && enabled != 0
}

/// Local time as `2026-11-04_19-52-10`, safe to use in a folder name.
pub fn local_timestamp() -> String {
    let mut t: SYSTEMTIME = unsafe { mem::zeroed() };
    unsafe { GetLocalTime(&mut t) };
    format!(
        "{:04}-{:02}-{:02}_{:02}-{:02}-{:02}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond
    )
}
