//! The Interception capture loop.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use crate::engine::{Channel, Sink};
use crate::interception::{
    is_keyboard, Device, Driver, KeyStroke, MouseStroke, Stroke, FIRST_MOUSE, KEY_E0, KEY_E1,
    KEY_UP, LAST_MOUSE, MOUSE_MOVE_ABSOLUTE,
};

/// Longest wait for a stroke. The heartbeat depends on it.
const WAIT_MS: u32 = 20;
/// How often the slots are checked. A Bluetooth device that went to sleep can
/// come back in another slot.
const RESCAN_US: u64 = 2_000_000;

const SCAN_CTRL: u16 = 0x1D;
const SCAN_ALT: u16 = 0x38;
const SCAN_PAUSE: u16 = 0x45;
const SCAN_BREAK: u16 = 0x46;

/// What the capture loop needs from the rest of the program.
pub trait CaptureHost {
    fn now_us(&self) -> u64;
    /// The channel of a device, if that mouse is switched on.
    fn target(&self, hardware_id: &str) -> Option<Arc<Channel>>;
    fn process(&self, channel: &Channel, dx: i32, dy: i32);
    /// True once after the switches changed.
    fn take_rescan(&self) -> bool;
    fn beat(&self);
    /// True while the filter keeps its hands off: strokes pass untouched.
    fn hands_off(&self) -> bool {
        false
    }
    /// Must not return: the capture has to end at once.
    fn emergency_exit(&self);
}

/// Sends motion to the system as if it came from the captured mouse.
pub struct InterceptionSink {
    driver: Arc<dyn Driver>,
    devices: RwLock<HashMap<String, Device>>,
}

impl InterceptionSink {
    pub fn new(driver: Arc<dyn Driver>) -> Self {
        Self {
            driver,
            devices: RwLock::new(HashMap::new()),
        }
    }
}

impl Sink for InterceptionSink {
    fn emit(&self, key: &str, dx: i32, dy: i32) {
        let device = self
            .devices
            .read()
            .ok()
            .and_then(|devices| devices.get(key).copied());
        let Some(device) = device else {
            return;
        };
        let stroke = Stroke::from_mouse(MouseStroke {
            x: dx,
            y: dy,
            ..MouseStroke::default()
        });
        self.driver.send(device, &stroke);
    }
}

/// Spots `Ctrl+Alt+Pause` in the stream of key strokes.
#[derive(Debug, Default)]
pub struct EmergencyCombo {
    ctrl: bool,
    alt: bool,
}

impl EmergencyCombo {
    /// Returns true when the stroke completes the combination.
    pub fn observe(&mut self, key: KeyStroke) -> bool {
        let up = key.state & KEY_UP != 0;
        let e0 = key.state & KEY_E0 != 0;
        let e1 = key.state & KEY_E1 != 0;
        match key.code {
            // Pause arrives as E1 1D 45, so that 1D is not the Ctrl key.
            SCAN_CTRL if e1 => !up && self.ctrl && self.alt,
            SCAN_CTRL => {
                self.ctrl = !up;
                false
            }
            SCAN_ALT => {
                self.alt = !up;
                false
            }
            SCAN_PAUSE if e1 => !up && self.ctrl && self.alt,
            // With Ctrl held the same key arrives as Break.
            SCAN_BREAK if e0 => !up && self.ctrl && self.alt,
            _ => false,
        }
    }
}

/// Releases the devices however the loop ends, a panic included. Otherwise
/// motion of the captured mice would pile up unread in the driver.
struct ReleaseGuard<'a> {
    driver: &'a dyn Driver,
}

impl Drop for ReleaseGuard<'_> {
    fn drop(&mut self) {
        self.driver.capture_mice(&[]);
        self.driver.capture_keyboards(false);
    }
}

fn find_targets(driver: &dyn Driver, host: &dyn CaptureHost) -> HashMap<Device, Arc<Channel>> {
    (FIRST_MOUSE..=LAST_MOUSE)
        .filter_map(|device| {
            let id = driver.hardware_id(device)?;
            let channel = id.lines().find_map(|line| host.target(line))?;
            Some((device, channel))
        })
        .collect()
}

fn same_targets(a: &HashMap<Device, Arc<Channel>>, b: &HashMap<Device, Arc<Channel>>) -> bool {
    a.len() == b.len() && a.keys().all(|device| b.contains_key(device))
}

/// Reads strokes until `stop` is set.
pub fn run_capture(
    driver: &dyn Driver,
    sink: &InterceptionSink,
    host: &dyn CaptureHost,
    stop: &AtomicBool,
) {
    let _guard = ReleaseGuard { driver };
    driver.capture_keyboards(true);

    let mut targets: HashMap<Device, Arc<Channel>> = HashMap::new();
    let mut next_scan = 0;
    let mut combo = EmergencyCombo::default();

    while !stop.load(Ordering::Relaxed) {
        host.beat();

        if host.take_rescan() || host.now_us() >= next_scan {
            next_scan = host.now_us() + RESCAN_US;
            let found = find_targets(driver, host);
            if !same_targets(&found, &targets) {
                targets = found;
                if let Ok(mut devices) = sink.devices.write() {
                    *devices = targets
                        .iter()
                        .map(|(device, channel)| (channel.key.clone(), *device))
                        .collect();
                }
                let devices: Vec<Device> = targets.keys().copied().collect();
                driver.capture_mice(&devices);
            }
        }

        let Some(device) = driver.wait(WAIT_MS) else {
            continue;
        };
        let Some(stroke) = driver.receive(device) else {
            continue;
        };

        if is_keyboard(device) {
            // The key goes on first, so it is never lost whatever comes next.
            driver.send(device, &stroke);
            if combo.observe(stroke.key()) {
                host.emergency_exit();
            }
        } else if let Some(channel) = targets.get(&device) {
            handle_target_stroke(driver, host, channel, device, &stroke);
        } else {
            driver.send(device, &stroke);
        }
    }
}

fn handle_target_stroke(
    driver: &dyn Driver,
    host: &dyn CaptureHost,
    channel: &Channel,
    device: Device,
    stroke: &Stroke,
) {
    let mouse = stroke.mouse();
    if mouse.flags & MOUSE_MOVE_ABSOLUTE != 0 || host.hands_off() {
        driver.send(device, stroke);
        return;
    }
    // A click sharing a packet with motion goes out at once, motion or not.
    if mouse.state != 0 {
        let click = MouseStroke {
            x: 0,
            y: 0,
            ..mouse
        };
        driver.send(device, &Stroke::from_mouse(click));
    }
    if mouse.x != 0 || mouse.y != 0 {
        host.process(channel, mouse.x, mouse.y);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::MouseSetting;
    use crate::interception::FIRST_KEYBOARD;
    use crate::presets::PresetKey;
    use ilumx_core::spec::parse_spec;
    use std::collections::VecDeque;
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::sync::Mutex;

    const MX_ERGO: Device = 13;
    const OTHER_MOUSE: Device = 12;
    const KEYBOARD: Device = FIRST_KEYBOARD;
    const MX_ERGO_ID: &str = r"HID\{00001812-0000-1000-8000-00805f9b34fb}_Dev_VID&02046d_PID&b01d";
    const OTHER_ID: &str = r"HID\VID_046D&PID_C548&MI_01&Col01";

    /// A driver that plays back scripted strokes and records what it is told.
    #[derive(Default)]
    struct FakeDriver {
        incoming: Mutex<VecDeque<(Device, Stroke)>>,
        sent: Mutex<Vec<(Device, Stroke)>>,
        captured_mice: Mutex<Vec<Vec<Device>>>,
        captured_keyboards: Mutex<Vec<bool>>,
        slots: Mutex<Vec<(Device, &'static str)>>,
        stop_when_empty: Mutex<Option<Arc<AtomicBool>>>,
    }

    impl FakeDriver {
        fn with_mice() -> Self {
            let driver = Self::default();
            *driver.slots.lock().unwrap() = vec![(OTHER_MOUSE, OTHER_ID), (MX_ERGO, MX_ERGO_ID)];
            driver
        }

        fn script(&self, device: Device, stroke: Stroke) {
            self.incoming.lock().unwrap().push_back((device, stroke));
        }

        fn sent_mouse(&self, device: Device) -> Vec<MouseStroke> {
            let sent = self.sent.lock().unwrap();
            sent.iter()
                .filter(|(d, _)| *d == device)
                .map(|(_, stroke)| stroke.mouse())
                .collect()
        }
    }

    impl Driver for FakeDriver {
        fn wait(&self, _timeout_ms: u32) -> Option<Device> {
            let next = self.incoming.lock().unwrap().front().map(|(d, _)| *d);
            if next.is_none() {
                if let Some(stop) = self.stop_when_empty.lock().unwrap().as_ref() {
                    stop.store(true, Ordering::Relaxed);
                }
            }
            next
        }

        fn receive(&self, device: Device) -> Option<Stroke> {
            let mut incoming = self.incoming.lock().unwrap();
            assert_eq!(incoming.front().map(|(d, _)| *d), Some(device));
            incoming.pop_front().map(|(_, stroke)| stroke)
        }

        fn send(&self, device: Device, stroke: &Stroke) {
            self.sent.lock().unwrap().push((device, *stroke));
        }

        fn hardware_id(&self, device: Device) -> Option<String> {
            let slots = self.slots.lock().unwrap();
            slots
                .iter()
                .find(|(d, _)| *d == device)
                .map(|(_, id)| id.to_string())
        }

        fn capture_mice(&self, devices: &[Device]) {
            self.captured_mice.lock().unwrap().push(devices.to_vec());
        }

        fn capture_keyboards(&self, capture: bool) {
            self.captured_keyboards.lock().unwrap().push(capture);
        }
    }

    /// Filters the mice whose hardware id holds `enabled`.
    struct FakeHost {
        channels: HashMap<&'static str, Arc<Channel>>,
        sink: Arc<InterceptionSink>,
        now_us: Mutex<u64>,
        emergency: AtomicBool,
    }

    impl FakeHost {
        fn new(enabled: &[&'static str], spec: &str, sink: Arc<InterceptionSink>) -> Self {
            let setting = MouseSetting {
                enabled: true,
                preset: PresetKey::default_key(),
                config: parse_spec(spec).unwrap(),
            };
            let channels = enabled
                .iter()
                .map(|id| {
                    let key = ilumx_core::device::device_key(id);
                    (*id, Arc::new(Channel::new(&key, &setting)))
                })
                .collect();
            Self {
                channels,
                sink,
                now_us: Mutex::new(0),
                emergency: AtomicBool::new(false),
            }
        }

        fn inputs(&self, id: &str) -> usize {
            self.channels[id].processor.tap.drain().len()
        }
    }

    impl CaptureHost for FakeHost {
        fn now_us(&self) -> u64 {
            let mut now = self.now_us.lock().unwrap();
            *now += 1_000;
            *now
        }

        fn target(&self, hardware_id: &str) -> Option<Arc<Channel>> {
            self.channels.get(hardware_id).cloned()
        }

        fn process(&self, channel: &Channel, dx: i32, dy: i32) {
            let out = channel.processor.packet(self.now_us(), dx, dy);
            if out != (0, 0) {
                self.sink.emit(&channel.key, out.0, out.1);
            }
        }

        fn take_rescan(&self) -> bool {
            false
        }

        fn beat(&self) {}

        fn emergency_exit(&self) {
            self.emergency.store(true, Ordering::Relaxed);
        }
    }

    /// Plays the script through the loop and returns the driver and host.
    fn play(driver: FakeDriver, spec: &str) -> (Arc<FakeDriver>, FakeHost) {
        play_with(driver, &[MX_ERGO_ID], spec)
    }

    fn play_with(
        driver: FakeDriver,
        enabled: &[&'static str],
        spec: &str,
    ) -> (Arc<FakeDriver>, FakeHost) {
        let driver = Arc::new(driver);
        let sink = Arc::new(InterceptionSink::new(driver.clone()));
        let host = FakeHost::new(enabled, spec, sink.clone());
        let stop = Arc::new(AtomicBool::new(false));
        *driver.stop_when_empty.lock().unwrap() = Some(stop.clone());
        run_capture(driver.as_ref(), &sink, &host, &stop);
        (driver, host)
    }

    fn motion(x: i32, y: i32) -> Stroke {
        Stroke::from_mouse(MouseStroke {
            x,
            y,
            ..MouseStroke::default()
        })
    }

    fn key(code: u16, state: u16) -> KeyStroke {
        KeyStroke {
            code,
            state,
            information: 0,
        }
    }

    #[test]
    fn captures_only_the_mice_switched_on() {
        let (driver, _) = play(FakeDriver::with_mice(), "passthrough");
        let captured = driver.captured_mice.lock().unwrap();
        assert_eq!(captured[0], [MX_ERGO]);
        assert_eq!(captured.len(), 2, "captured then released: {captured:?}");
    }

    #[test]
    fn captures_several_mice_at_once() {
        let (driver, _) = play_with(
            FakeDriver::with_mice(),
            &[MX_ERGO_ID, OTHER_ID],
            "passthrough",
        );
        let captured = driver.captured_mice.lock().unwrap();
        let mut first = captured[0].clone();
        first.sort_unstable();
        assert_eq!(first, [OTHER_MOUSE, MX_ERGO]);
    }

    #[test]
    fn captured_mice_keep_their_own_channels() {
        let driver = FakeDriver::with_mice();
        driver.script(MX_ERGO, motion(1, 0));
        driver.script(MX_ERGO, motion(1, 0));
        driver.script(OTHER_MOUSE, motion(1, 0));
        let (_, host) = play_with(driver, &[MX_ERGO_ID, OTHER_ID], "passthrough");
        assert_eq!(host.inputs(MX_ERGO_ID), 2);
        assert_eq!(host.inputs(OTHER_ID), 1);
    }

    #[test]
    fn captures_nothing_with_no_mouse_switched_on() {
        let (driver, _) = play_with(FakeDriver::with_mice(), &[], "passthrough");
        let captured = driver.captured_mice.lock().unwrap();
        assert_eq!(captured.len(), 1);
        assert!(captured[0].is_empty());
    }

    #[test]
    fn passthrough_is_bit_exact() {
        let driver = FakeDriver::with_mice();
        let strokes = [(3, -1), (-120, 7), (1, 1)];
        for (x, y) in strokes {
            driver.script(MX_ERGO, motion(x, y));
        }
        let (driver, _) = play(driver, "passthrough");
        let sent: Vec<(i32, i32)> = driver
            .sent_mouse(MX_ERGO)
            .iter()
            .map(|m| (m.x, m.y))
            .collect();
        assert_eq!(sent, strokes);
    }

    #[test]
    fn other_mice_pass_untouched_by_the_pipeline() {
        let driver = FakeDriver::with_mice();
        driver.script(OTHER_MOUSE, motion(50, 50));
        let (driver, host) = play(driver, "ema:tau_ms=1000");
        assert_eq!(driver.sent_mouse(OTHER_MOUSE)[0], motion(50, 50).mouse());
        assert_eq!(host.inputs(MX_ERGO_ID), 0);
    }

    #[test]
    fn a_click_in_a_motion_packet_goes_out_at_once() {
        let driver = FakeDriver::with_mice();
        let click = MouseStroke {
            state: 0x01,
            x: 40,
            y: 0,
            ..MouseStroke::default()
        };
        driver.script(MX_ERGO, Stroke::from_mouse(click));
        // The heavy smoothing holds the motion back, the click must not wait.
        let (driver, host) = play(driver, "ema:tau_ms=1000");
        let sent = driver.sent_mouse(MX_ERGO);
        assert_eq!(sent.len(), 1);
        assert_eq!((sent[0].state, sent[0].x), (0x01, 0));
        assert_eq!(host.inputs(MX_ERGO_ID), 1);
    }

    #[test]
    fn absolute_motion_passes_unchanged() {
        let driver = FakeDriver::with_mice();
        let absolute = MouseStroke {
            flags: MOUSE_MOVE_ABSOLUTE,
            x: 30_000,
            y: 20_000,
            ..MouseStroke::default()
        };
        driver.script(MX_ERGO, Stroke::from_mouse(absolute));
        let (driver, _) = play(driver, "ema:tau_ms=1000");
        assert_eq!(driver.sent_mouse(MX_ERGO), [absolute]);
    }

    #[test]
    fn keys_are_forwarded_unchanged() {
        let driver = FakeDriver::with_mice();
        let stroke = KeyStroke {
            code: 0x1E,
            state: 0,
            information: 0,
        };
        driver.script(KEYBOARD, Stroke::from_key(stroke));
        let (driver, host) = play(driver, "passthrough");
        let sent = driver.sent.lock().unwrap();
        assert_eq!(sent[0].0, KEYBOARD);
        assert_eq!(sent[0].1.key(), stroke);
        assert!(!host.emergency.load(Ordering::Relaxed));
    }

    #[test]
    fn the_emergency_combo_triggers_after_forwarding_the_key() {
        let driver = FakeDriver::with_mice();
        for stroke in [
            key(SCAN_CTRL, 0),
            key(SCAN_ALT, 0),
            key(SCAN_CTRL, KEY_E1),
        ] {
            driver.script(KEYBOARD, Stroke::from_key(stroke));
        }
        let (driver, host) = play(driver, "passthrough");
        assert!(host.emergency.load(Ordering::Relaxed));
        assert_eq!(driver.sent.lock().unwrap().len(), 3);
    }

    #[test]
    fn devices_are_released_after_a_panic() {
        struct PanickingHost(FakeHost);
        impl CaptureHost for PanickingHost {
            fn now_us(&self) -> u64 {
                self.0.now_us()
            }
            fn target(&self, hardware_id: &str) -> Option<Arc<Channel>> {
                self.0.target(hardware_id)
            }
            fn process(&self, _channel: &Channel, _dx: i32, _dy: i32) {
                panic!("simulated failure while sending");
            }
            fn take_rescan(&self) -> bool {
                false
            }
            fn beat(&self) {}
            fn emergency_exit(&self) {}
        }

        let driver = FakeDriver::with_mice();
        driver.script(MX_ERGO, motion(1, 1));
        let driver = Arc::new(driver);
        let sink = Arc::new(InterceptionSink::new(driver.clone()));
        let host = PanickingHost(FakeHost::new(&[MX_ERGO_ID], "passthrough", sink.clone()));
        let stop = AtomicBool::new(false);
        let result = catch_unwind(AssertUnwindSafe(|| {
            run_capture(driver.as_ref(), &sink, &host, &stop);
        }));
        assert!(result.is_err());
        let released = driver.captured_mice.lock().unwrap().last().cloned();
        assert_eq!(released, Some(Vec::new()));
        assert_eq!(driver.captured_keyboards.lock().unwrap().last(), Some(&false));
    }

    #[test]
    fn combo_needs_ctrl_and_alt_held() {
        let mut combo = EmergencyCombo::default();
        assert!(!combo.observe(key(SCAN_PAUSE, KEY_E1)));
        combo.observe(key(SCAN_CTRL, 0));
        assert!(!combo.observe(key(SCAN_PAUSE, KEY_E1)));
        combo.observe(key(SCAN_ALT, 0));
        assert!(combo.observe(key(SCAN_PAUSE, KEY_E1)));
        combo.observe(key(SCAN_CTRL, KEY_UP));
        assert!(!combo.observe(key(SCAN_PAUSE, KEY_E1)));
    }

    #[test]
    fn combo_accepts_right_hand_keys_and_break() {
        let mut combo = EmergencyCombo::default();
        combo.observe(key(SCAN_CTRL, KEY_E0));
        combo.observe(key(SCAN_ALT, KEY_E0));
        assert!(combo.observe(key(SCAN_BREAK, KEY_E0)));
    }

    #[test]
    fn the_pause_prefix_is_not_mistaken_for_ctrl() {
        let mut combo = EmergencyCombo::default();
        combo.observe(key(SCAN_ALT, 0));
        combo.observe(key(SCAN_CTRL, KEY_E1));
        assert!(!combo.observe(key(SCAN_PAUSE, KEY_E1)));
    }

    #[test]
    fn releasing_the_key_does_not_trigger() {
        let mut combo = EmergencyCombo::default();
        combo.observe(key(SCAN_CTRL, 0));
        combo.observe(key(SCAN_ALT, 0));
        assert!(!combo.observe(key(SCAN_PAUSE, KEY_E1 | KEY_UP)));
    }
}
