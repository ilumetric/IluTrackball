//! Passive capture of mouse packets through Raw Input.
//!
//! Raw Input only observes: the registered window gets a copy of each packet
//! and the system cursor keeps working untouched.

use std::ffi::c_void;
use std::io;
use std::mem;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::Duration;

use ilumx_core::event_log::RawEvent;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows_sys::Win32::System::Threading::{
    GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_HIGHEST,
};
use windows_sys::Win32::UI::Input::{
    GetRawInputData, GetRegisteredRawInputDevices, RegisterRawInputDevices, HRAWINPUT, RAWINPUT,
    RAWINPUTDEVICE, RAWINPUTHEADER, RIDEV_INPUTSINK, RID_INPUT, RIM_TYPEMOUSE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, RegisterClassW, SetTimer, MSG,
    WM_INPUT, WM_TIMER, WNDCLASSW,
};

const HID_USAGE_PAGE_GENERIC: u16 = 0x01;
const HID_USAGE_GENERIC_MOUSE: u16 = 0x02;

const MOUSE_MOVE_ABSOLUTE: u16 = 0x0001;

const RI_MOUSE_WHEEL: u16 = 0x0400;

/// `(down flag, up flag, bit in RawEvent::buttons)` for each mouse button.
const BUTTON_FLAGS: [(u16, u16, u32); 5] = [
    (0x0001, 0x0002, 1),
    (0x0004, 0x0008, 2),
    (0x0010, 0x0020, 4),
    (0x0040, 0x0080, 8),
    (0x0100, 0x0200, 16),
];

/// How long `record` waits at most between checks of the deadline.
const POLL_SLICE: Duration = Duration::from_millis(50);

/// How often the capture thread checks that mouse packets still come to it.
const REGISTRATION_CHECK_MS: u32 = 250;
const REGISTRATION_TIMER: usize = 1;
/// More device types than any process registers.
const MAX_REGISTRATIONS: usize = 32;

/// Set when mouse packets should be coming and are not.
static RENEW_REGISTRATION: AtomicBool = AtomicBool::new(false);

/// Registers for mouse packets anew at the next check, even if the registration
/// looks right. It costs nothing, so it may be asked for on a mere suspicion.
pub fn renew_registration() {
    RENEW_REGISTRATION.store(true, Ordering::Relaxed);
}

/// One packet as delivered by Windows.
#[derive(Clone, Copy, Debug)]
pub struct RawPacket {
    /// QPC time of arrival.
    pub qpc: i64,
    /// `RAWINPUTHEADER::hDevice`. Zero for injected input.
    pub device: isize,
    pub dx: i32,
    pub dy: i32,
    pub button_flags: u16,
    pub button_data: u16,
    /// The packet holds absolute coordinates rather than motion.
    pub absolute: bool,
}

/// Packets captured during one scenario.
#[derive(Debug, Default)]
pub struct Recording {
    /// Packets of the selected device.
    pub events: Vec<RawEvent>,
    /// Packets of every other mouse. They are counted to spot a wrong device
    /// choice and are not stored.
    pub other_device_packets: usize,
    /// Packets with absolute coordinates, which the analysis cannot use.
    pub absolute_packets: usize,
}

/// Microseconds since a fixed starting point, from the QPC clock.
#[derive(Clone, Copy, Debug)]
pub struct Clock {
    origin: i64,
    frequency: i64,
}

impl Clock {
    pub fn start_now() -> Self {
        let mut frequency = 1i64;
        unsafe { QueryPerformanceFrequency(&mut frequency) };
        Self {
            origin: qpc_now(),
            frequency,
        }
    }

    pub fn now_us(&self) -> u64 {
        self.at_us(qpc_now())
    }

    /// The time of a QPC reading, which must not predate the clock.
    pub fn at_us(&self, qpc: i64) -> u64 {
        elapsed_us(qpc - self.origin, self.frequency)
    }
}

/// Runs the capture thread for the lifetime of the process. Windows allows only
/// one per process, because the sink window class is registered once.
pub struct Capture {
    packets: Receiver<RawPacket>,
    qpc_frequency: i64,
}

impl Capture {
    pub fn start() -> io::Result<Self> {
        let mut qpc_frequency = 0i64;
        if unsafe { QueryPerformanceFrequency(&mut qpc_frequency) } == 0 {
            return Err(io::Error::last_os_error());
        }

        let (packet_tx, packets) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        thread::Builder::new()
            .name("raw-input".to_string())
            .spawn(move || run_capture_thread(packet_tx, ready_tx))?;

        ready_rx
            .recv()
            .map_err(|_| io::Error::other("capture thread stopped during startup"))??;
        Ok(Self {
            packets,
            qpc_frequency,
        })
    }

    /// Waits for the next packet of any mouse. `None` means nothing arrived.
    pub fn next_packet(&self, timeout: Duration) -> io::Result<Option<RawPacket>> {
        match self.packets.recv_timeout(timeout) {
            Ok(packet) => Ok(Some(packet)),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => Err(io::Error::other("capture thread stopped")),
        }
    }

    /// Records `device` for `duration`. Nothing is written to disk meanwhile,
    /// so file I/O cannot disturb the timestamps.
    pub fn record(&self, device: isize, duration: Duration) -> io::Result<Recording> {
        while self.packets.try_recv().is_ok() {}

        let start = qpc_now();
        let end = start + (duration.as_secs_f64() * self.qpc_frequency as f64) as i64;
        let mut recording = Recording::default();
        let mut buttons = ButtonState::default();

        while qpc_now() < end {
            let Some(packet) = self.next_packet(POLL_SLICE)? else {
                continue;
            };
            if packet.qpc < start || packet.qpc >= end {
                continue;
            }
            if packet.device != device {
                recording.other_device_packets += 1;
                continue;
            }
            if packet.absolute {
                recording.absolute_packets += 1;
                continue;
            }
            recording.events.push(RawEvent {
                t_us: elapsed_us(packet.qpc - start, self.qpc_frequency),
                dx: packet.dx,
                dy: packet.dy,
                buttons: buttons.apply(packet.button_flags),
                wheel: wheel_delta(packet.button_flags, packet.button_data),
            });
        }
        Ok(recording)
    }
}

fn elapsed_us(ticks: i64, frequency: i64) -> u64 {
    (ticks as i128 * 1_000_000 / frequency as i128) as u64
}

/// Tracks held buttons from the press and release flags of each packet.
#[derive(Debug, Default)]
struct ButtonState {
    held: u32,
}

impl ButtonState {
    fn apply(&mut self, flags: u16) -> u32 {
        for (down, up, bit) in BUTTON_FLAGS {
            if flags & down != 0 {
                self.held |= bit;
            }
            if flags & up != 0 {
                self.held &= !bit;
            }
        }
        self.held
    }
}

fn wheel_delta(flags: u16, data: u16) -> i32 {
    if flags & RI_MOUSE_WHEEL != 0 {
        data as i16 as i32
    } else {
        0
    }
}

fn qpc_now() -> i64 {
    let mut ticks = 0i64;
    unsafe { QueryPerformanceCounter(&mut ticks) };
    ticks
}

fn run_capture_thread(packets: Sender<RawPacket>, ready: Sender<io::Result<()>>) {
    // Packets are stamped on arrival, so this thread has to wake up promptly.
    unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST) };

    match create_sink_window() {
        Ok(window) => {
            let _ = ready.send(Ok(()));
            unsafe { SetTimer(window, REGISTRATION_TIMER, REGISTRATION_CHECK_MS, None) };
            pump_messages(window, &packets);
        }
        Err(error) => {
            let _ = ready.send(Err(error));
        }
    }
}

/// Creates a window that is never shown and registers it for mouse packets.
fn create_sink_window() -> io::Result<HWND> {
    let class_name: Vec<u16> = "IluMxRawInputSink\0".encode_utf16().collect();
    let instance = unsafe { GetModuleHandleW(ptr::null()) };

    let mut class: WNDCLASSW = unsafe { mem::zeroed() };
    class.lpfnWndProc = Some(DefWindowProcW);
    class.hInstance = instance;
    class.lpszClassName = class_name.as_ptr();
    if unsafe { RegisterClassW(&class) } == 0 {
        return Err(io::Error::last_os_error());
    }

    let window = unsafe {
        CreateWindowExW(
            0,
            class_name.as_ptr(),
            class_name.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            ptr::null_mut(),
            ptr::null_mut(),
            instance,
            ptr::null(),
        )
    };
    if window.is_null() {
        return Err(io::Error::last_os_error());
    }

    register_for_mouse_packets(window)?;
    Ok(window)
}

fn register_for_mouse_packets(window: HWND) -> io::Result<()> {
    // RIDEV_INPUTSINK delivers packets while the window is in the background.
    // RIDEV_NOLEGACY is deliberately absent: it would cut the mouse off from
    // every other application in this process's input queue.
    let device = RAWINPUTDEVICE {
        usUsagePage: HID_USAGE_PAGE_GENERIC,
        usUsage: HID_USAGE_GENERIC_MOUSE,
        dwFlags: RIDEV_INPUTSINK,
        hwndTarget: window,
    };
    let registered =
        unsafe { RegisterRawInputDevices(&device, 1, mem::size_of::<RAWINPUTDEVICE>() as u32) };
    if registered == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The window this process receives mouse packets on.
fn mouse_packet_receiver() -> Option<HWND> {
    let mut devices: [RAWINPUTDEVICE; MAX_REGISTRATIONS] = unsafe { mem::zeroed() };
    let mut count = MAX_REGISTRATIONS as u32;
    let found = unsafe {
        GetRegisteredRawInputDevices(
            devices.as_mut_ptr(),
            &mut count,
            mem::size_of::<RAWINPUTDEVICE>() as u32,
        )
    };
    if found == u32::MAX {
        return None;
    }
    devices[..found as usize]
        .iter()
        .find(|device| {
            device.usUsagePage == HID_USAGE_PAGE_GENERIC && device.usUsage == HID_USAGE_GENERIC_MOUSE
        })
        .map(|device| device.hwndTarget)
}

/// Windows keeps one receiver per device type in a process, and the latest
/// registration wins silently. Window libraries register too: winit does when
/// its event loop starts, and packets would stop arriving here from then on.
/// Nothing announces that, so the registration has to be checked.
fn keep_mouse_packets_coming(window: HWND) {
    let asked = RENEW_REGISTRATION.swap(false, Ordering::Relaxed);
    if asked || mouse_packet_receiver() != Some(window) {
        let _ = register_for_mouse_packets(window);
    }
}

fn pump_messages(window: HWND, packets: &Sender<RawPacket>) {
    let mut message: MSG = unsafe { mem::zeroed() };
    loop {
        let status = unsafe { GetMessageW(&mut message, ptr::null_mut(), 0, 0) };
        if status <= 0 {
            return;
        }
        if message.message == WM_INPUT {
            let qpc = qpc_now();
            if let Some(packet) = read_packet(message.lParam as HRAWINPUT, qpc) {
                if packets.send(packet).is_err() {
                    return;
                }
            }
        } else if message.message == WM_TIMER {
            keep_mouse_packets_coming(window);
        }
        // The default window procedure releases the packet's kernel buffer.
        unsafe { DispatchMessageW(&message) };
    }
}

fn read_packet(handle: HRAWINPUT, qpc: i64) -> Option<RawPacket> {
    let mut raw: RAWINPUT = unsafe { mem::zeroed() };
    let mut size = mem::size_of::<RAWINPUT>() as u32;
    let copied = unsafe {
        GetRawInputData(
            handle,
            RID_INPUT,
            &mut raw as *mut RAWINPUT as *mut c_void,
            &mut size,
            mem::size_of::<RAWINPUTHEADER>() as u32,
        )
    };
    if copied == u32::MAX || raw.header.dwType != RIM_TYPEMOUSE {
        return None;
    }

    let mouse = unsafe { raw.data.mouse };
    let buttons = unsafe { mouse.Anonymous.Anonymous };
    Some(RawPacket {
        qpc,
        device: raw.header.hDevice as isize,
        dx: mouse.lLastX,
        dy: mouse.lLastY,
        button_flags: buttons.usButtonFlags,
        button_data: buttons.usButtonData,
        absolute: mouse.usFlags & MOUSE_MOVE_ABSOLUTE != 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracks_held_buttons_across_packets() {
        let mut state = ButtonState::default();
        assert_eq!(state.apply(0x0001), 1); // left down
        assert_eq!(state.apply(0x0004), 3); // right down
        assert_eq!(state.apply(0x0000), 3); // plain motion keeps them held
        assert_eq!(state.apply(0x0002), 2); // left up
        assert_eq!(state.apply(0x0040), 10); // X1 down
        assert_eq!(state.apply(0x0088), 0); // right and X1 up together
    }

    #[test]
    fn wheel_delta_is_signed_and_needs_the_wheel_flag() {
        assert_eq!(wheel_delta(RI_MOUSE_WHEEL, 120), 120);
        assert_eq!(wheel_delta(RI_MOUSE_WHEEL, (-120i16) as u16), -120);
        assert_eq!(wheel_delta(0, 120), 0);
    }

    #[test]
    fn converts_ticks_without_overflow() {
        assert_eq!(elapsed_us(10_000_000, 10_000_000), 1_000_000);
        // A week of uptime at 10 MHz would overflow an i64 multiplication.
        let week = 7 * 24 * 3600 * 10_000_000i64;
        assert_eq!(elapsed_us(week, 10_000_000), 604_800_000_000);
    }

    /// Registers a window of another thread for mouse packets, as winit does
    /// when its event loop starts.
    fn take_over_mouse_packets() -> HWND {
        let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
        let rival = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                class.as_ptr(),
                0,
                0,
                0,
                0,
                0,
                ptr::null_mut(),
                ptr::null_mut(),
                GetModuleHandleW(ptr::null()),
                ptr::null(),
            )
        };
        assert!(!rival.is_null());
        register_for_mouse_packets(rival).unwrap();
        rival
    }

    #[test]
    fn takes_mouse_packets_back_from_a_later_registration() {
        let _capture = Capture::start().unwrap();
        let own = mouse_packet_receiver().expect("the capture registered itself");

        let rival = take_over_mouse_packets();
        assert_eq!(mouse_packet_receiver(), Some(rival));

        let limit = Duration::from_millis(u64::from(REGISTRATION_CHECK_MS) * 8);
        let start = std::time::Instant::now();
        while mouse_packet_receiver() != Some(own) {
            assert!(start.elapsed() < limit, "the capture did not take packets back");
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn the_clock_runs_forward_from_zero() {
        let clock = Clock::start_now();
        let first = clock.now_us();
        thread::sleep(Duration::from_millis(2));
        let second = clock.now_us();
        assert!(second > first);
        assert!(second - first >= 1_000, "{first} {second}");
    }
}
