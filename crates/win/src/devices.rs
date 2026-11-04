//! Enumerates mice through the Raw Input API.

use std::ffi::c_void;
use std::io;
use std::mem;
use std::ptr;

use ilumx_core::device::{
    classify_connection, is_mx_ergo, mx_ergo_rank, parse_vid_pid, Connection,
};
use windows_sys::Win32::Devices::HumanInterfaceDevice::HidD_GetProductString;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::UI::Input::{
    GetRawInputDeviceInfoW, GetRawInputDeviceList, RAWINPUTDEVICELIST, RIDI_DEVICENAME,
    RIM_TYPEMOUSE,
};

#[derive(Clone, Debug)]
pub struct MouseDevice {
    /// Value of `RAWINPUTHEADER::hDevice` for packets from this device.
    pub handle: isize,
    pub path: String,
    pub product: Option<String>,
}

impl MouseDevice {
    /// Short human-readable name for reports and prompts.
    pub fn label(&self) -> String {
        // The MX Ergo reports no product string over Bluetooth.
        let fallback = if is_mx_ergo(&self.path, None) {
            "MX Ergo"
        } else {
            "без имени"
        };
        let name = self.product.as_deref().unwrap_or(fallback);
        let ids = match parse_vid_pid(&self.path) {
            Some((vid, pid)) => format!("VID {vid:04X} PID {pid:04X}"),
            None => "VID/PID неизвестны".to_string(),
        };
        let link = match classify_connection(&self.path) {
            Connection::Bluetooth => "Bluetooth",
            Connection::LogitechReceiver => "ресивер Logitech",
            Connection::Other => "USB/другое",
        };
        format!("{name}, {ids}, {link}")
    }
}

pub fn enumerate() -> io::Result<Vec<MouseDevice>> {
    let list = raw_device_list()?;
    let mut mice = Vec::new();
    for entry in list.iter().filter(|d| d.dwType == RIM_TYPEMOUSE) {
        // A device can vanish between the two calls; just skip it.
        let Some(path) = device_path(entry.hDevice) else {
            continue;
        };
        let product = product_string(&path);
        mice.push(MouseDevice {
            handle: entry.hDevice as isize,
            path,
            product,
        });
    }
    Ok(mice)
}

/// The device to offer by default: the best MX Ergo match, if there is one.
pub fn default_index(devices: &[MouseDevice]) -> Option<usize> {
    devices
        .iter()
        .enumerate()
        .map(|(i, d)| (mx_ergo_rank(&d.path, d.product.as_deref()), i))
        .filter(|(rank, _)| *rank > 0)
        // Highest rank first, then the lowest index.
        .min_by_key(|(rank, i)| (std::cmp::Reverse(*rank), *i))
        .map(|(_, i)| i)
}

fn raw_device_list() -> io::Result<Vec<RAWINPUTDEVICELIST>> {
    let entry_size = mem::size_of::<RAWINPUTDEVICELIST>() as u32;
    let mut count = 0u32;
    let status = unsafe { GetRawInputDeviceList(ptr::null_mut(), &mut count, entry_size) };
    if status == u32::MAX {
        return Err(io::Error::last_os_error());
    }
    let mut list = vec![unsafe { mem::zeroed::<RAWINPUTDEVICELIST>() }; count as usize];
    let written = unsafe { GetRawInputDeviceList(list.as_mut_ptr(), &mut count, entry_size) };
    if written == u32::MAX {
        return Err(io::Error::last_os_error());
    }
    list.truncate(written as usize);
    Ok(list)
}

/// The device path behind a Raw Input device handle.
pub fn path_of(handle: isize) -> Option<String> {
    device_path(handle as HANDLE)
}

fn device_path(handle: HANDLE) -> Option<String> {
    let mut chars = 0u32;
    let status =
        unsafe { GetRawInputDeviceInfoW(handle, RIDI_DEVICENAME, ptr::null_mut(), &mut chars) };
    if status == u32::MAX || chars == 0 {
        return None;
    }
    let mut buffer = vec![0u16; chars as usize];
    let written = unsafe {
        GetRawInputDeviceInfoW(
            handle,
            RIDI_DEVICENAME,
            buffer.as_mut_ptr() as *mut c_void,
            &mut chars,
        )
    };
    if written == u32::MAX {
        return None;
    }
    Some(from_wide(&buffer))
}

/// Asks the HID driver for the product name. Zero access rights are enough
/// for this query and work even though Windows keeps mice open exclusively.
fn product_string(path: &str) -> Option<String> {
    let wide_path: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
    let file = unsafe {
        CreateFileW(
            wide_path.as_ptr(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            ptr::null(),
            OPEN_EXISTING,
            0,
            ptr::null_mut(),
        )
    };
    if file == INVALID_HANDLE_VALUE {
        return None;
    }
    let mut buffer = [0u16; 128];
    let ok = unsafe {
        HidD_GetProductString(
            file,
            buffer.as_mut_ptr() as *mut c_void,
            mem::size_of_val(&buffer) as u32,
        )
    };
    unsafe { CloseHandle(file) };
    if !ok {
        return None;
    }
    let name = from_wide(&buffer).trim().to_string();
    (!name.is_empty()).then_some(name)
}

fn from_wide(buffer: &[u16]) -> String {
    let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..len])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mouse(handle: isize, path: &str, product: Option<&str>) -> MouseDevice {
        MouseDevice {
            handle,
            path: path.to_string(),
            product: product.map(str::to_string),
        }
    }

    #[test]
    fn prefers_the_mx_ergo_over_other_logitech_mice() {
        let devices = [
            mouse(1, r"\\?\HID#VID_1532&PID_0084#x", Some("Razer")),
            mouse(2, r"\\?\HID#VID_046D&PID_C08B#x", Some("G502")),
            mouse(3, r"\\?\HID#VID_046D&PID_C52B&MI_01#x", Some("MX Ergo")),
        ];
        assert_eq!(default_index(&devices), Some(2));
    }

    #[test]
    fn falls_back_to_any_logitech_mouse() {
        let devices = [
            mouse(1, r"\\?\HID#VID_1532&PID_0084#x", Some("Razer")),
            mouse(2, r"\\?\HID#VID_046D&PID_C52B&MI_01#x", Some("USB Receiver")),
        ];
        assert_eq!(default_index(&devices), Some(1));
    }

    #[test]
    fn has_no_default_without_a_logitech_mouse() {
        let devices = [mouse(1, r"\\?\HID#VID_1532&PID_0084#x", Some("Razer"))];
        assert_eq!(default_index(&devices), None);
    }

    #[test]
    fn stops_wide_strings_at_the_terminator() {
        assert_eq!(from_wide(&[0x41, 0x42, 0, 0x43]), "AB");
    }
}
