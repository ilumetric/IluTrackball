//! A thin binding to `interception.dll`.
//!
//! The DLL is loaded at run time rather than linked, so the program starts and
//! works without the driver. The `Driver` trait keeps the capture logic
//! testable where no driver is installed.

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use windows_sys::core::PCSTR;
use windows_sys::Win32::Foundation::{FreeLibrary, HMODULE};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

pub type Device = i32;

pub const FIRST_KEYBOARD: Device = 1;
pub const LAST_KEYBOARD: Device = 10;
pub const FIRST_MOUSE: Device = 11;
pub const LAST_MOUSE: Device = 20;

const FILTER_KEY_NONE: u16 = 0x0000;
const FILTER_KEY_ALL: u16 = 0xFFFF;
const FILTER_MOUSE_NONE: u16 = 0x0000;
/// Motion only. Clicks and the wheel stay inside the driver and never pass
/// through this program, so it cannot lose or delay them.
const FILTER_MOUSE_MOVE: u16 = 0x1000;

const HARDWARE_ID_CHARS: usize = 512;

pub const MOUSE_MOVE_ABSOLUTE: u16 = 0x0001;

pub const KEY_UP: u16 = 0x01;
pub const KEY_E0: u16 = 0x02;
pub const KEY_E1: u16 = 0x04;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MouseStroke {
    pub state: u16,
    pub flags: u16,
    pub rolling: i16,
    pub x: i32,
    pub y: i32,
    pub information: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyStroke {
    pub code: u16,
    pub state: u16,
    pub information: u32,
}

/// The driver's stroke buffer, sized for its largest stroke.
#[repr(C)]
#[derive(Clone, Copy)]
pub union Stroke {
    mouse: MouseStroke,
    key: KeyStroke,
}

impl Stroke {
    pub fn empty() -> Self {
        Self::from_mouse(MouseStroke::default())
    }

    pub fn from_mouse(mouse: MouseStroke) -> Self {
        Self { mouse }
    }

    #[cfg(test)]
    pub fn from_key(key: KeyStroke) -> Self {
        let mut stroke = Self::empty();
        stroke.key = key;
        stroke
    }

    pub fn mouse(&self) -> MouseStroke {
        unsafe { self.mouse }
    }

    pub fn key(&self) -> KeyStroke {
        unsafe { self.key }
    }
}

pub fn is_keyboard(device: Device) -> bool {
    (FIRST_KEYBOARD..=LAST_KEYBOARD).contains(&device)
}

pub trait Driver: Send + Sync {
    /// Waits for a stroke from any captured device.
    fn wait(&self, timeout_ms: u32) -> Option<Device>;
    fn receive(&self, device: Device) -> Option<Stroke>;
    fn send(&self, device: Device, stroke: &Stroke);
    /// All hardware ids of a device joined by newlines. `None` if the slot is
    /// empty.
    fn hardware_id(&self, device: Device) -> Option<String>;
    /// Captures motion of these mice. All others stay out of it completely.
    fn capture_mice(&self, devices: &[Device]);
    fn capture_keyboards(&self, capture: bool);
}

/// Why the driver cannot be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenError {
    NoLibrary(PathBuf),
    BadLibrary(String),
    NoDriver,
}

impl OpenError {
    pub fn describe(&self) -> String {
        match self {
            OpenError::NoLibrary(path) => format!("нет файла {}", path.display()),
            OpenError::BadLibrary(name) => format!("в interception.dll нет функции {name}"),
            OpenError::NoDriver => "драйвер не установлен".to_string(),
        }
    }
}

type Predicate = unsafe extern "C" fn(Device) -> i32;

#[derive(Clone, Copy)]
struct Api {
    create_context: unsafe extern "C" fn() -> *mut c_void,
    destroy_context: unsafe extern "C" fn(*mut c_void),
    set_filter: unsafe extern "C" fn(*mut c_void, Predicate, u16),
    wait_with_timeout: unsafe extern "C" fn(*mut c_void, u32) -> Device,
    send: unsafe extern "C" fn(*mut c_void, Device, *const Stroke, u32) -> i32,
    receive: unsafe extern "C" fn(*mut c_void, Device, *mut Stroke, u32) -> i32,
    get_hardware_id: unsafe extern "C" fn(*mut c_void, Device, *mut c_void, u32) -> u32,
}

/// The mice `set_filter` should act on, one bit for each device number. Its
/// predicate cannot carry state.
static FILTER_TARGETS: AtomicU32 = AtomicU32::new(0);

fn target_bits(devices: &[Device]) -> u32 {
    devices.iter().fold(0, |bits, device| bits | 1 << device)
}

unsafe extern "C" fn is_any_mouse(device: Device) -> i32 {
    (FIRST_MOUSE..=LAST_MOUSE).contains(&device) as i32
}

unsafe extern "C" fn is_filter_target(device: Device) -> i32 {
    (FILTER_TARGETS.load(Ordering::Relaxed) & (1 << device) != 0) as i32
}

unsafe extern "C" fn is_any_keyboard(device: Device) -> i32 {
    is_keyboard(device) as i32
}

/// A driver context. Closing it, which also happens when the process dies,
/// releases every capture.
pub struct Interception {
    module: HMODULE,
    api: Api,
    context: *mut c_void,
}

// The driver handles calls from several threads. Only the capture thread
// receives, and the ticker only sends.
unsafe impl Send for Interception {}
unsafe impl Sync for Interception {}

/// The DLL is taken from next to the program and nowhere else.
pub fn library_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .unwrap_or_default()
        .join("interception.dll")
}

impl Interception {
    pub fn open() -> Result<Self, OpenError> {
        let path = library_path();
        if !path.is_file() {
            return Err(OpenError::NoLibrary(path));
        }
        let wide_path: Vec<u16> = path
            .to_string_lossy()
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let module = unsafe { LoadLibraryW(wide_path.as_ptr()) };
        if module.is_null() {
            return Err(OpenError::NoLibrary(path));
        }

        let opened = load_api(module).and_then(|api| {
            let context = unsafe { (api.create_context)() };
            if context.is_null() {
                Err(OpenError::NoDriver)
            } else {
                Ok((api, context))
            }
        });
        match opened {
            Ok((api, context)) => Ok(Self {
                module,
                api,
                context,
            }),
            Err(error) => {
                unsafe { FreeLibrary(module) };
                Err(error)
            }
        }
    }
}

impl Drop for Interception {
    fn drop(&mut self) {
        unsafe {
            (self.api.destroy_context)(self.context);
            FreeLibrary(self.module);
        }
    }
}

// Each transmute takes its target type from the field of `Api` it fills.
#[allow(clippy::missing_transmute_annotations)]
fn load_api(module: HMODULE) -> Result<Api, OpenError> {
    macro_rules! function {
        ($name:literal) => {{
            let symbol = concat!($name, "\0");
            let address = unsafe { GetProcAddress(module, symbol.as_ptr() as PCSTR) };
            let address = address.ok_or_else(|| OpenError::BadLibrary($name.to_string()))?;
            unsafe { std::mem::transmute(address) }
        }};
    }
    Ok(Api {
        create_context: function!("interception_create_context"),
        destroy_context: function!("interception_destroy_context"),
        set_filter: function!("interception_set_filter"),
        wait_with_timeout: function!("interception_wait_with_timeout"),
        send: function!("interception_send"),
        receive: function!("interception_receive"),
        get_hardware_id: function!("interception_get_hardware_id"),
    })
}

impl Driver for Interception {
    fn wait(&self, timeout_ms: u32) -> Option<Device> {
        let device = unsafe { (self.api.wait_with_timeout)(self.context, timeout_ms) };
        (device != 0).then_some(device)
    }

    fn receive(&self, device: Device) -> Option<Stroke> {
        let mut stroke = Stroke::empty();
        let received = unsafe { (self.api.receive)(self.context, device, &mut stroke, 1) };
        (received > 0).then_some(stroke)
    }

    fn send(&self, device: Device, stroke: &Stroke) {
        unsafe { (self.api.send)(self.context, device, stroke, 1) };
    }

    fn hardware_id(&self, device: Device) -> Option<String> {
        let mut buffer = [0u16; HARDWARE_ID_CHARS];
        let bytes = unsafe {
            (self.api.get_hardware_id)(
                self.context,
                device,
                buffer.as_mut_ptr() as *mut c_void,
                std::mem::size_of_val(&buffer) as u32,
            )
        } as usize;
        if bytes == 0 || bytes > std::mem::size_of_val(&buffer) {
            return None;
        }
        let text = String::from_utf16_lossy(&buffer[..bytes / 2]);
        Some(text.trim_matches('\0').replace('\0', "\n"))
    }

    fn capture_mice(&self, devices: &[Device]) {
        FILTER_TARGETS.store(target_bits(devices), Ordering::Relaxed);
        unsafe {
            (self.api.set_filter)(self.context, is_any_mouse, FILTER_MOUSE_NONE);
            (self.api.set_filter)(self.context, is_filter_target, FILTER_MOUSE_MOVE);
        }
    }

    fn capture_keyboards(&self, capture: bool) {
        let filter = if capture {
            FILTER_KEY_ALL
        } else {
            FILTER_KEY_NONE
        };
        unsafe { (self.api.set_filter)(self.context, is_any_keyboard, filter) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stroke_layouts_match_the_driver() {
        assert_eq!(std::mem::size_of::<MouseStroke>(), 20);
        assert_eq!(std::mem::size_of::<KeyStroke>(), 8);
        assert_eq!(std::mem::size_of::<Stroke>(), 20);
        let offset = |field: *const i32, base: *const MouseStroke| {
            field as usize - base as usize
        };
        let stroke = MouseStroke::default();
        assert_eq!(offset(&stroke.x, &stroke), 8);
        assert_eq!(offset(&stroke.y, &stroke), 12);
    }

    #[test]
    fn device_numbers_split_into_keyboards_and_mice() {
        assert!(is_keyboard(1) && is_keyboard(10));
        assert!(!is_keyboard(11) && !is_keyboard(0));
        unsafe {
            assert_eq!(is_any_mouse(11), 1);
            assert_eq!(is_any_mouse(20), 1);
            assert_eq!(is_any_mouse(10), 0);
        }
    }

    #[test]
    fn several_mice_can_be_filter_targets() {
        assert_eq!(target_bits(&[]), 0);
        assert_eq!(target_bits(&[11, 13]), (1 << 11) | (1 << 13));
    }

    #[test]
    fn reports_a_missing_library_with_its_path() {
        // No test runs next to a real interception.dll.
        let error = Interception::open().err().expect("no driver in tests");
        assert!(matches!(error, OpenError::NoLibrary(_)), "{error:?}");
        assert!(error.describe().contains("interception.dll"));
    }
}
