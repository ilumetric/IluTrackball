//! Owns the input threads and the channel of every mouse.
//!
//! - The Raw Input thread reads all mice while the hook does the filtering.
//! - The capture thread does the same job with Interception.
//! - The ticker finishes filter tails after a ball has stopped.
//! - The watchdog polls the hotkeys, decides whether the filter may touch the
//!   mouse at all, and mends or restarts what has broken.
//! - The status thread looks up the mice and the Windows pointer settings.
//!   These calls can take a while, so they stay away from all the others.
//!
//! Every mouse has a channel of its own. A switched off mouse still runs through
//! its channel, which is what the test area shows, but nothing it does reaches
//! the system: its motion is left alone. Nothing here waits for the window, so
//! a stuck window cannot hold up the cursor, and a hidden one changes nothing.

use std::collections::HashMap;
use std::io;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::thread::{self, JoinHandle, Thread};
use std::time::Duration;

use ilumx_core::device::{classify_connection, device_key, is_mx_ergo, parse_vid_pid, Connection};
use ilumx_core::event_log::RawEvent;
use ilumx_core::pipeline::PipelineConfig;
use ilumx_core::pointer::PointerSettings;
use ilumx_win::devices::{self, MouseDevice};
use ilumx_win::rawinput::{self, Capture, Clock, RawPacket};
use ilumx_win::windows_settings::read_pointer_settings;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentThread, SetThreadPriority, TerminateProcess,
    THREAD_PRIORITY_HIGHEST,
};

use crate::capture::{run_capture, CaptureHost, InterceptionSink};
use crate::config::Backend;
use crate::foreground::{Foreground, ForegroundWatch};
use crate::hook::{self, Hook, HookHealth, Link, SendInputSink};
use crate::hotkey::{self, Hotkey};
use crate::interception::{Driver, Interception};
use crate::log;
use crate::presets::PresetKey;
use crate::processor::Processor;
use crate::recovery;
use crate::tap::{Flow, TapSample};
use crate::timer::MillisecondTimer;

const RAW_WAIT: Duration = Duration::from_millis(20);
/// How long the ticker sleeps while nothing is held.
const TICKER_IDLE: Duration = Duration::from_millis(50);
const WATCHDOG_PERIOD: Duration = Duration::from_millis(50);
const STATUS_PERIOD: Duration = Duration::from_secs(3);
/// The sources wake up many times a second even without input. After this much
/// silence the hook steps aside: the cursor must not hang on a stuck thread.
const STAND_ASIDE_AFTER_US: u64 = 300_000;
/// Silent this long, a source is stuck for good and the program starts over.
/// The driver holds the mouse all that time. The hook stepped aside long ago,
/// so it can afford to wait for a thread that may still come back.
const DRIVER_STALL_LIMIT_US: u64 = 1_500_000;
const HOOK_STALL_LIMIT_US: u64 = 5_000_000;
/// A watchdog round this much longer than planned means the whole computer
/// stood still: asleep, or in the debugger. That is no stall of ours.
const SUSPEND_GAP_US: u64 = 700_000;
/// How many watchdog rounds pass between checks of the path of the motion.
const LINK_CHECK_ROUNDS: u32 = 4;
/// Windows hands out a new hook in no time, so asking more often is pointless.
const REPAIR_PERIOD_US: u64 = 1_000_000;
/// How long the owner of a Raw Input handle is believed. Windows reuses handles
/// after a Bluetooth device comes back.
const KNOWN_DEVICE_US: u64 = 10_000_000;
/// A handle that could not be placed is asked about again soon: a device that
/// has just connected may not be able to answer yet.
const UNKNOWN_DEVICE_US: u64 = 1_000_000;
const THREAD_RESTART_DELAY: Duration = Duration::from_millis(500);
/// Exit code of an emergency stop.
const EMERGENCY_EXIT_CODE: u32 = 3;

pub const DEMO_KEY: &str = "DEMO";

/// Where the filtered motion goes.
pub trait Sink: Send + Sync {
    fn emit(&self, key: &str, dx: i32, dy: i32);
}

/// End the process at once. The system then closes the driver handles and
/// removes the hook, whatever state the threads are in.
pub fn emergency_exit() -> ! {
    unsafe { TerminateProcess(GetCurrentProcess(), EMERGENCY_EXIT_CODE) };
    std::process::abort();
}

/// What the filter is doing with the mouse right now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum Standing {
    /// No mouse is switched on.
    #[default]
    Off,
    Filtering,
    /// The user paused the filter.
    Paused,
    /// An elevated window is in front, so nothing can be sent to the system.
    AdminWindow,
    /// A game has the mouse.
    Game,
    /// Something broke and is being mended. The mouse works directly meanwhile.
    Repairing,
}

impl Standing {
    const ALL: [Standing; 6] = [
        Standing::Off,
        Standing::Filtering,
        Standing::Paused,
        Standing::AdminWindow,
        Standing::Game,
        Standing::Repairing,
    ];

    fn from_index(index: u8) -> Self {
        Self::ALL.get(index as usize).copied().unwrap_or_default()
    }

    /// True if the mouse is left alone although it is switched on.
    pub fn hands_off(self) -> bool {
        !matches!(self, Standing::Off | Standing::Filtering)
    }
}

/// What decides the standing of the filter.
#[derive(Clone, Copy, Debug, Default)]
struct Situation {
    capturing: bool,
    use_driver: bool,
    paused: bool,
    foreground: Foreground,
    /// The path of the motion is broken, or a thread on it has gone quiet.
    trouble: bool,
}

/// The driver sits below all of this: nothing above it can get in its way, so
/// only the user can pause it.
fn decide(now: Situation) -> Standing {
    if !now.capturing {
        return Standing::Off;
    }
    if now.paused {
        return Standing::Paused;
    }
    if now.use_driver {
        return Standing::Filtering;
    }
    match now.foreground {
        Foreground::Elevated => Standing::AdminWindow,
        Foreground::Game => Standing::Game,
        Foreground::Ordinary if now.trouble => Standing::Repairing,
        Foreground::Ordinary => Standing::Filtering,
    }
}

/// One mouse on its way through the program.
pub struct Channel {
    pub key: String,
    pub processor: Processor,
    enabled: AtomicBool,
    packets: AtomicU64,
    last_packet_us: AtomicU64,
    preset: Mutex<PresetKey>,
}

impl Channel {
    pub fn new(key: &str, setting: &MouseSetting) -> Self {
        Self {
            key: key.to_string(),
            processor: Processor::new(&setting.config),
            enabled: AtomicBool::new(setting.enabled),
            packets: AtomicU64::new(0),
            last_packet_us: AtomicU64::new(0),
            preset: Mutex::new(setting.preset.clone()),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    pub fn packets(&self) -> u64 {
        self.packets.load(Ordering::Relaxed)
    }

    pub fn last_packet_us(&self) -> u64 {
        self.last_packet_us.load(Ordering::Relaxed)
    }

    fn note_packet(&self, t_us: u64) {
        self.packets.fetch_add(1, Ordering::Relaxed);
        self.last_packet_us.store(t_us, Ordering::Relaxed);
    }

    fn apply(&self, setting: &MouseSetting) {
        self.enabled.store(setting.enabled, Ordering::Relaxed);
        self.processor.set_config(setting.config.clone());
        if let Ok(mut preset) = self.preset.lock() {
            *preset = setting.preset.clone();
        }
    }
}

/// What the window wants for one mouse.
#[derive(Clone, Debug, PartialEq)]
pub struct MouseSetting {
    pub enabled: bool,
    pub preset: PresetKey,
    pub config: PipelineConfig,
}

impl Default for MouseSetting {
    fn default() -> Self {
        Self {
            enabled: false,
            preset: PresetKey::default_key(),
            config: PipelineConfig::default(),
        }
    }
}

/// A mouse as the list in the window shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct MouseInfo {
    pub key: String,
    pub product: Option<String>,
    pub connection: Connection,
    pub ids: Option<(u16, u16)>,
    pub is_mx_ergo: bool,
}

impl MouseInfo {
    fn from_device(device: &MouseDevice) -> Self {
        Self {
            key: device_key(&device.path),
            product: device.product.clone(),
            connection: classify_connection(&device.path),
            ids: parse_vid_pid(&device.path),
            is_mx_ergo: is_mx_ergo(&device.path, device.product.as_deref()),
        }
    }

    fn demo() -> Self {
        Self {
            key: DEMO_KEY.to_string(),
            product: None,
            connection: Connection::Other,
            ids: None,
            is_mx_ergo: false,
        }
    }
}

/// The next preset, after the hotkey asked for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresetSwitch {
    pub mouse: String,
    pub preset: PresetKey,
}

/// State shared by all threads.
pub struct Shared {
    pub clock: Clock,
    capturing: AtomicBool,
    use_driver: AtomicBool,
    paused: AtomicBool,
    games_direct: AtomicBool,
    /// Follows `standing`, for the paths that read it with every packet.
    hands_off: AtomicBool,
    standing: AtomicU8,
    english: AtomicBool,
    new_version: Mutex<Option<String>>,
    sink: RwLock<Option<Arc<dyn Sink>>>,
    ticker: OnceLock<Thread>,
    last_beat_us: AtomicU64,
    channels: RwLock<Vec<Arc<Channel>>>,
    settings: Mutex<HashMap<String, MouseSetting>>,
    /// The presets the hotkey walks through.
    cycle: Mutex<Vec<(PresetKey, PipelineConfig)>>,
    switch: Mutex<Option<PresetSwitch>>,
    rescan: AtomicBool,
    /// Everything Raw Input delivered, from any mouse. Zero while a mouse
    /// moves means the packets are not reaching this program at all.
    raw_packets: AtomicU64,
    mice: Mutex<Vec<MouseInfo>>,
    pointer: Mutex<PointerSettings>,
}

impl Shared {
    fn new() -> Self {
        Self {
            clock: Clock::start_now(),
            capturing: AtomicBool::new(false),
            use_driver: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            games_direct: AtomicBool::new(true),
            hands_off: AtomicBool::new(false),
            standing: AtomicU8::new(Standing::Off as u8),
            english: AtomicBool::new(false),
            new_version: Mutex::new(None),
            sink: RwLock::new(None),
            ticker: OnceLock::new(),
            last_beat_us: AtomicU64::new(0),
            channels: RwLock::new(Vec::new()),
            settings: Mutex::new(HashMap::new()),
            cycle: Mutex::new(Vec::new()),
            switch: Mutex::new(None),
            rescan: AtomicBool::new(false),
            raw_packets: AtomicU64::new(0),
            mice: Mutex::new(Vec::new()),
            pointer: Mutex::new(PointerSettings::default()),
        }
    }

    pub fn raw_packets(&self) -> u64 {
        self.raw_packets.load(Ordering::Relaxed)
    }

    pub fn standing(&self) -> Standing {
        Standing::from_index(self.standing.load(Ordering::Relaxed))
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::Relaxed);
    }

    /// Whether games get the mouse untouched. On the hook path only.
    pub fn set_games_direct(&self, direct: bool) {
        self.games_direct.store(direct, Ordering::Relaxed);
    }

    pub fn is_english(&self) -> bool {
        self.english.load(Ordering::Relaxed)
    }

    /// A version that can be installed, for the menu of the tray icon.
    pub fn new_version(&self) -> Option<String> {
        self.new_version.lock().ok()?.clone()
    }

    pub fn set_new_version(&self, version: Option<String>) {
        if let Ok(mut current) = self.new_version.lock() {
            *current = version;
        }
    }

    /// The language for what is shown outside the window.
    pub fn set_english(&self, english: bool) {
        self.english.store(english, Ordering::Relaxed);
    }

    /// Takes on a new standing and says if it changed. What is held goes out
    /// first, while the way out is still open.
    fn apply_standing(&self, standing: Standing) -> bool {
        if self.standing() == standing {
            return false;
        }
        if standing.hands_off() {
            self.release_all();
        }
        self.hands_off.store(standing.hands_off(), Ordering::Relaxed);
        hook::stand_aside(standing.hands_off());
        self.standing.store(standing as u8, Ordering::Relaxed);
        true
    }

    pub fn mice(&self) -> Vec<MouseInfo> {
        self.mice.lock().map(|mice| mice.clone()).unwrap_or_default()
    }

    pub fn pointer(&self) -> PointerSettings {
        self.pointer.lock().map(|pointer| *pointer).unwrap_or_default()
    }

    /// The channel of a mouse, opened on first use.
    pub fn channel(&self, key: &str) -> Arc<Channel> {
        if let Ok(channels) = self.channels.read() {
            if let Some(channel) = channels.iter().find(|channel| channel.key == key) {
                return channel.clone();
            }
        }
        let setting = self
            .settings
            .lock()
            .ok()
            .and_then(|settings| settings.get(key).cloned())
            .unwrap_or_default();
        let channel = Arc::new(Channel::new(key, &setting));
        if let Ok(mut channels) = self.channels.write() {
            // Another thread may have opened it in the meantime.
            if let Some(existing) = channels.iter().find(|existing| existing.key == key) {
                return existing.clone();
            }
            channels.push(channel.clone());
        }
        channel
    }

    /// Applies what the window wants for a mouse.
    pub fn set_mouse(&self, key: &str, setting: MouseSetting) {
        if let Ok(mut settings) = self.settings.lock() {
            settings.insert(key.to_string(), setting.clone());
        }
        let channel = self.channel(key);
        if channel.is_enabled() && !setting.enabled {
            // The way out closes for this mouse: what it still holds goes first.
            let held = channel.processor.flush(self.clock.now_us());
            self.emit(&channel, held);
        }
        channel.apply(&setting);
        self.rescan.store(true, Ordering::Relaxed);
        self.wake_ticker();
    }

    /// Tells the hotkey which presets there are.
    pub fn set_cycle(&self, cycle: Vec<(PresetKey, PipelineConfig)>) {
        if let Ok(mut current) = self.cycle.lock() {
            *current = cycle;
        }
    }

    /// The preset change the hotkey made since the window last asked.
    pub fn take_switch(&self) -> Option<PresetSwitch> {
        self.switch.lock().ok()?.take()
    }

    fn any_enabled(&self) -> bool {
        self.channels
            .read()
            .is_ok_and(|channels| channels.iter().any(|channel| channel.is_enabled()))
    }

    /// The mouse the hotkey means: of those switched on, the last one moved.
    fn mouse_in_use(&self) -> Option<Arc<Channel>> {
        let channels = self.channels.read().ok()?;
        channels
            .iter()
            .filter(|channel| channel.is_enabled())
            .max_by_key(|channel| channel.last_packet_us())
            .cloned()
    }

    fn next_preset(&self) {
        let Some(channel) = self.mouse_in_use() else {
            return;
        };
        let Ok(cycle) = self.cycle.lock() else {
            return;
        };
        let current = channel.preset.lock().ok().map(|preset| preset.clone());
        let position = cycle.iter().position(|(key, _)| Some(key) == current.as_ref());
        let next = position.map_or(0, |index| index + 1) % cycle.len().max(1);
        let Some((key, config)) = cycle.get(next).cloned() else {
            return;
        };
        drop(cycle);

        channel.processor.set_config(config.clone());
        if let Ok(mut preset) = channel.preset.lock() {
            *preset = key.clone();
        }
        if let Ok(mut settings) = self.settings.lock() {
            if let Some(setting) = settings.get_mut(&channel.key) {
                setting.preset = key.clone();
                setting.config = config;
            }
        }
        if let Ok(mut switch) = self.switch.lock() {
            *switch = Some(PresetSwitch {
                mouse: channel.key.clone(),
                preset: key,
            });
        }
        self.wake_ticker();
    }

    /// Sends motion out and shows it in the test area.
    fn emit(&self, channel: &Channel, (dx, dy): (i32, i32)) {
        if dx == 0 && dy == 0 {
            return;
        }
        channel.processor.tap.push(TapSample {
            t_us: self.clock.now_us(),
            flow: Flow::Output,
            dx,
            dy,
        });
        if !channel.is_enabled() || self.hands_off.load(Ordering::Relaxed) {
            return;
        }
        if let Ok(sink) = self.sink.read() {
            if let Some(sink) = sink.as_ref() {
                sink.emit(&channel.key, dx, dy);
            }
        }
    }

    fn process(&self, channel: &Channel, t_us: u64, dx: i32, dy: i32) {
        channel.note_packet(t_us);
        let out = channel.processor.packet(t_us, dx, dy);
        self.emit(channel, out);
        self.wake_ticker();
    }

    fn set_sink(&self, sink: Option<Arc<dyn Sink>>) {
        if let Ok(mut current) = self.sink.write() {
            *current = sink;
        }
    }

    fn set_capturing(&self, capturing: bool, use_driver: bool) {
        self.beat();
        self.use_driver.store(use_driver, Ordering::Relaxed);
        self.capturing.store(capturing, Ordering::Relaxed);
    }

    fn beat(&self) {
        self.last_beat_us
            .store(self.clock.now_us(), Ordering::Relaxed);
    }

    fn wake_ticker(&self) {
        if let Some(ticker) = self.ticker.get() {
            ticker.unpark();
        }
    }

    /// True if the hook is in use and the thread behind it has gone quiet.
    fn hook_source_quiet(&self, now_us: u64) -> bool {
        self.hook_in_use() && self.silent_for(now_us) > STAND_ASIDE_AFTER_US
    }

    fn stalled_at(&self, now_us: u64) -> bool {
        if !self.capturing.load(Ordering::Relaxed) {
            return false;
        }
        let limit = if self.use_driver.load(Ordering::Relaxed) {
            DRIVER_STALL_LIMIT_US
        } else {
            HOOK_STALL_LIMIT_US
        };
        self.silent_for(now_us) > limit
    }

    fn silent_for(&self, now_us: u64) -> u64 {
        now_us.saturating_sub(self.last_beat_us.load(Ordering::Relaxed))
    }

    fn hook_in_use(&self) -> bool {
        self.capturing.load(Ordering::Relaxed) && !self.use_driver.load(Ordering::Relaxed)
    }

    /// The capture thread ended without being asked to. Its devices are
    /// released, so the filter stands down and nothing counts as stuck.
    fn capture_ended(&self) {
        log::write("the capture thread ended by itself");
        self.set_sink(None);
        self.set_capturing(false, false);
    }

    fn release_all(&self) {
        let Ok(channels) = self.channels.read() else {
            return;
        };
        for channel in channels.iter() {
            let held = channel.processor.flush(self.clock.now_us());
            self.emit(channel, held);
        }
    }
}

impl CaptureHost for Shared {
    fn now_us(&self) -> u64 {
        self.clock.now_us()
    }

    fn target(&self, hardware_id: &str) -> Option<Arc<Channel>> {
        Some(self.channel(&device_key(hardware_id))).filter(|channel| channel.is_enabled())
    }

    fn process(&self, channel: &Channel, dx: i32, dy: i32) {
        Shared::process(self, channel, self.clock.now_us(), dx, dy);
    }

    fn take_rescan(&self) -> bool {
        self.rescan.swap(false, Ordering::Relaxed)
    }

    fn beat(&self) {
        Shared::beat(self);
    }

    fn hands_off(&self) -> bool {
        self.hands_off.load(Ordering::Relaxed)
    }

    fn emergency_exit(&self) {
        log::write("emergency exit from the keyboard");
        emergency_exit();
    }
}

/// What is known about a Raw Input handle.
struct KnownDevice {
    channel: Option<Arc<Channel>>,
    checked_us: u64,
}

impl KnownDevice {
    fn is_current(&self, now_us: u64) -> bool {
        let trust_us = if self.channel.is_some() {
            KNOWN_DEVICE_US
        } else {
            UNKNOWN_DEVICE_US
        };
        now_us.saturating_sub(self.checked_us) < trust_us
    }
}

/// Finds the channel of each Raw Input handle. New handles appear when a
/// Bluetooth device reconnects, and a look-up can fail while it does. So a
/// failure is never final, and an answer does not last for ever.
#[derive(Default)]
struct DeviceMatcher {
    known: HashMap<isize, KnownDevice>,
}

impl DeviceMatcher {
    fn channel(&mut self, handle: isize, shared: &Shared, now_us: u64) -> Option<Arc<Channel>> {
        self.place(handle, now_us, || {
            devices::path_of(handle).map(|path| shared.channel(&device_key(&path)))
        })
    }

    fn place(
        &mut self,
        handle: isize,
        now_us: u64,
        look_up: impl FnOnce() -> Option<Arc<Channel>>,
    ) -> Option<Arc<Channel>> {
        let current = self
            .known
            .get(&handle)
            .filter(|device| device.is_current(now_us));
        if let Some(device) = current {
            return device.channel.clone();
        }
        let found = look_up();
        let device = self.known.entry(handle).or_insert(KnownDevice {
            channel: None,
            checked_us: now_us,
        });
        device.checked_us = now_us;
        // A failed look-up says nothing against what was known already.
        if found.is_some() {
            device.channel = found;
        }
        device.channel.clone()
    }
}

fn run_raw_thread(capture: Capture, shared: Arc<Shared>) {
    let mut matcher = DeviceMatcher::default();
    loop {
        if !shared.use_driver.load(Ordering::Relaxed) {
            shared.beat();
        }
        match capture.next_packet(RAW_WAIT) {
            Ok(Some(packet)) => {
                // A failure here may cost one packet, not the whole input path.
                let handled = catch_unwind(AssertUnwindSafe(|| {
                    handle_raw_packet(&packet, &mut matcher, &shared);
                }));
                if handled.is_err() {
                    log::write("a mouse packet could not be handled");
                }
            }
            Ok(None) => {}
            Err(error) => {
                log::write(&format!("raw input stopped: {error}"));
                return;
            }
        }
    }
}

fn handle_raw_packet(packet: &RawPacket, matcher: &mut DeviceMatcher, shared: &Shared) {
    shared.raw_packets.fetch_add(1, Ordering::Relaxed);
    // Injected input has no device. That includes our own output.
    if packet.device == 0 {
        return;
    }
    let Some(channel) = matcher.channel(packet.device, shared, shared.clock.now_us()) else {
        hook::note_last_device(false);
        return;
    };
    let enabled = channel.is_enabled();
    hook::note_last_device(enabled);

    let moved = packet.dx != 0 || packet.dy != 0;
    if !moved || packet.absolute {
        return;
    }
    if enabled {
        hook::note_target_motion();
    }
    // The driver feeds a mouse it captures. Raw Input only sees the result.
    if enabled && shared.use_driver.load(Ordering::Relaxed) {
        return;
    }
    let t_us = shared.clock.at_us(packet.qpc);
    shared.process(&channel, t_us, packet.dx, packet.dy);
}

fn run_ticker(shared: Arc<Shared>) {
    let timer = MillisecondTimer::new();
    loop {
        let unsettled = shared.channels.read().is_ok_and(|channels| {
            channels
                .iter()
                .any(|channel| !channel.processor.is_settled())
        });
        if !unsettled {
            thread::park_timeout(TICKER_IDLE);
            continue;
        }
        timer.wait();
        let Ok(channels) = shared.channels.read() else {
            continue;
        };
        for channel in channels.iter() {
            if !channel.processor.is_settled() {
                let out = channel.processor.tick(shared.clock.now_us());
                shared.emit(channel, out);
            }
        }
    }
}

fn run_status_thread(shared: Arc<Shared>, demo: bool) {
    loop {
        let mut mice: Vec<MouseInfo> = devices::enumerate()
            .unwrap_or_default()
            .iter()
            .map(MouseInfo::from_device)
            .collect();
        mice.dedup_by(|a, b| a.key == b.key);
        if demo {
            mice.push(MouseInfo::demo());
        }
        if let Ok(mut current) = shared.mice.lock() {
            *current = mice;
        }
        if let Ok(mut current) = shared.pointer.lock() {
            *current = read_pointer_settings();
        }
        thread::sleep(STATUS_PERIOD);
    }
}

/// Watches the path of the motion and mends it.
#[derive(Default)]
struct LinkGuard {
    health: HookHealth,
    hook_gone: bool,
    packets_gone: bool,
    next_repair_us: u64,
    send_failures: u64,
}

impl LinkGuard {
    fn trouble(&self) -> bool {
        self.hook_gone
    }

    fn check(&mut self, shared: &Shared, now_us: u64) {
        let link = self.health.check();
        self.note_refused_motion();
        // Standing aside on purpose explains any silence.
        let standing = shared.standing();
        let on_purpose = standing.hands_off() && standing != Standing::Repairing;
        if !shared.hook_in_use() || on_purpose {
            self.hook_gone = false;
            self.packets_gone = false;
            return;
        }
        match link {
            Link::Quiet => {}
            Link::Fine => {
                if self.hook_gone || self.packets_gone {
                    log::write("the path of the motion works again");
                }
                self.hook_gone = false;
                self.packets_gone = false;
            }
            Link::HookGone => self.mend_hook(now_us),
            Link::PacketsGone => self.mend_packets(),
        }
    }

    fn mend_hook(&mut self, now_us: u64) {
        if !self.hook_gone {
            log::write("windows dropped the mouse hook, installing it again");
        }
        self.hook_gone = true;
        if now_us >= self.next_repair_us {
            self.next_repair_us = now_us + REPAIR_PERIOD_US;
            hook::request_reinstall();
        }
    }

    /// Without packets nothing can be sent for the motion the hook takes, so
    /// the hook has to let it pass until packets come again.
    fn mend_packets(&mut self) {
        if !self.packets_gone {
            log::write("mouse packets stopped coming, registering for them again");
        }
        self.packets_gone = true;
        hook::forget_last_device();
        rawinput::renew_registration();
    }

    fn note_refused_motion(&mut self) {
        let (failures, error) = hook::send_failures();
        if failures != self.send_failures {
            self.send_failures = failures;
            log::write(&format!("the system refused motion, error {error}"));
        }
    }
}

fn run_watchdog(shared: Arc<Shared>, preset_hotkey: Option<Hotkey>) {
    let mut preset_key_was_down = false;
    let mut last_round_us = shared.clock.now_us();
    let mut guard = LinkGuard::default();
    let mut foreground = ForegroundWatch::new();
    let mut rounds = 0u32;
    loop {
        thread::sleep(WATCHDOG_PERIOD);
        let now_us = shared.clock.now_us();
        if now_us.saturating_sub(last_round_us) > SUSPEND_GAP_US {
            log::write("the computer was suspended");
            shared.beat();
        }
        last_round_us = now_us;

        if hotkey::emergency_is_down() {
            log::write("emergency exit from the hotkey");
            emergency_exit();
        }
        if shared.stalled_at(now_us) {
            recovery::restart("input thread stalled, starting over");
        }
        if let Some(key) = preset_hotkey {
            let down = key.is_down();
            if down && !preset_key_was_down {
                shared.next_preset();
            }
            preset_key_was_down = down;
        }

        rounds += 1;
        if rounds == LINK_CHECK_ROUNDS {
            rounds = 0;
            guard.check(&shared, now_us);
        }

        let seen = foreground.look(shared.games_direct.load(Ordering::Relaxed));
        let standing = decide(Situation {
            capturing: shared.capturing.load(Ordering::Relaxed),
            use_driver: shared.use_driver.load(Ordering::Relaxed),
            paused: shared.is_paused(),
            foreground: seen,
            trouble: guard.trouble() || shared.hook_source_quiet(now_us),
        });
        if shared.apply_standing(standing) {
            log::write(&format!("{standing:?}, in front: {}", foreground.name()));
        }
    }
}

/// Feeds a recorded log into the test area in a loop, as a demonstration that
/// needs neither a trackball nor a hand on it.
fn run_demo(shared: Arc<Shared>, events: Vec<RawEvent>) {
    let duration_us = events.last().map_or(0, |e| e.t_us);
    if duration_us == 0 {
        return;
    }
    let channel = shared.channel(DEMO_KEY);
    let mut start = shared.clock.now_us();
    loop {
        for event in events.iter().filter(|e| e.is_motion()) {
            let due = start + event.t_us;
            let now = shared.clock.now_us();
            if due > now {
                thread::sleep(Duration::from_micros(due - now));
            }
            shared.process(&channel, due, event.dx, event.dy);
        }
        start += duration_us;
    }
}

struct DriverRun {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

enum Session {
    None,
    Hook(Hook),
    Driver(DriverRun),
}

pub struct Engine {
    shared: Arc<Shared>,
    session: Session,
    backend: Backend,
}

impl Engine {
    /// Starts the threads. Returns the engine and, if mice cannot be read, the
    /// reason.
    pub fn start(
        backend: Backend,
        preset_hotkey: Option<Hotkey>,
        demo: Option<Vec<RawEvent>>,
    ) -> (Self, Option<String>) {
        let shared = Arc::new(Shared::new());

        let ticker = spawn_guarded("ticker", {
            let shared = shared.clone();
            move || {
                raise_priority();
                run_ticker(shared.clone());
            }
        });
        if let Ok(handle) = &ticker {
            let _ = shared.ticker.set(handle.thread().clone());
        }

        let watchdog = spawn_guarded("watchdog", {
            let shared = shared.clone();
            move || run_watchdog(shared.clone(), preset_hotkey)
        });

        let status = spawn_guarded("status", {
            let shared = shared.clone();
            let demo = demo.is_some();
            move || run_status_thread(shared.clone(), demo)
        });

        let mut problem = ticker
            .err()
            .or(watchdog.err())
            .or(status.err())
            .map(|error| error.to_string());
        match Capture::start() {
            Ok(capture) => {
                let thread = spawn("raw-source", {
                    let shared = shared.clone();
                    move || {
                        raise_priority();
                        run_raw_thread(capture, shared);
                    }
                });
                if let Err(error) = thread {
                    problem = Some(error.to_string());
                }
            }
            Err(error) => problem = Some(format!("Raw Input: {error}")),
        }

        if let Some(events) = demo {
            let shared = shared.clone();
            let _ = spawn("demo", move || run_demo(shared, events));
        }

        let engine = Self {
            shared,
            session: Session::None,
            backend,
        };
        (engine, problem)
    }

    pub fn shared(&self) -> &Arc<Shared> {
        &self.shared
    }

    pub fn backend(&self) -> Backend {
        self.backend
    }

    pub fn is_capturing(&self) -> bool {
        !matches!(self.session, Session::None)
    }

    pub fn set_backend(&mut self, backend: Backend) -> Result<(), String> {
        self.stop_session();
        self.backend = backend;
        self.sync()
    }

    /// Starts or stops capturing to fit the switches. With every mouse off
    /// there is no hook and no driver filter left in the path of any mouse.
    pub fn sync(&mut self) -> Result<(), String> {
        match (self.shared.any_enabled(), self.is_capturing()) {
            (true, false) => match self.backend {
                Backend::Hook => self.start_hook(),
                Backend::Interception => self.start_driver(),
            },
            (false, true) => {
                self.stop_session();
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Tidies up after a capture thread that ended by itself. The thread has
    /// already released its devices and told the watchdog, so nothing depends
    /// on how soon this runs.
    pub fn poll(&mut self) -> bool {
        let finished = matches!(&self.session, Session::Driver(run) if run.thread.is_finished());
        if finished {
            self.stop_session();
        }
        finished
    }

    fn start_hook(&mut self) -> Result<(), String> {
        let hook = Hook::install().map_err(|error| error.to_string())?;
        self.shared.set_sink(Some(Arc::new(SendInputSink)));
        self.shared.set_capturing(true, false);
        hook.start_blocking();
        self.session = Session::Hook(hook);
        Ok(())
    }

    fn start_driver(&mut self) -> Result<(), String> {
        let driver: Arc<dyn Driver> =
            Arc::new(Interception::open().map_err(|error| error.describe())?);
        let sink = Arc::new(InterceptionSink::new(driver.clone()));
        self.shared.set_sink(Some(sink.clone()));
        self.shared.set_capturing(true, true);

        let stop = Arc::new(AtomicBool::new(false));
        let thread = spawn("capture", {
            let stop = stop.clone();
            let shared = self.shared.clone();
            move || {
                raise_priority();
                let _ = catch_unwind(AssertUnwindSafe(|| {
                    run_capture(driver.as_ref(), &sink, shared.as_ref(), &stop);
                }));
                if !stop.load(Ordering::Relaxed) {
                    shared.capture_ended();
                }
            }
        })
        .map_err(|error| error.to_string())?;
        self.session = Session::Driver(DriverRun { stop, thread });
        Ok(())
    }

    fn stop_session(&mut self) {
        // Motion stops being swallowed before the way back in is removed.
        let session = std::mem::replace(&mut self.session, Session::None);
        match session {
            Session::None => {}
            Session::Hook(hook) => drop(hook),
            Session::Driver(run) => {
                run.stop.store(true, Ordering::Relaxed);
                let _ = run.thread.join();
            }
        }
        self.shared.release_all();
        self.shared.set_sink(None);
        self.shared.set_capturing(false, false);
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.stop_session();
    }
}

fn spawn(name: &str, body: impl FnOnce() + Send + 'static) -> io::Result<JoinHandle<()>> {
    thread::Builder::new().name(name.to_string()).spawn(body)
}

/// A thread that starts again if it panics. The panic itself is in the log.
fn spawn_guarded(name: &str, body: impl Fn() + Send + 'static) -> io::Result<JoinHandle<()>> {
    let label = name.to_string();
    spawn(name, move || loop {
        if catch_unwind(AssertUnwindSafe(&body)).is_ok() {
            return;
        }
        log::write(&format!("thread {label} failed, starting it again"));
        thread::sleep(THREAD_RESTART_DELAY);
    })
}

fn raise_priority() {
    unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use ilumx_core::spec::parse_spec;

    struct Recorder(Mutex<Vec<(String, i32, i32)>>);

    impl Sink for Recorder {
        fn emit(&self, key: &str, dx: i32, dy: i32) {
            self.0.lock().unwrap().push((key.to_string(), dx, dy));
        }
    }

    fn setting(enabled: bool, spec: &str) -> MouseSetting {
        MouseSetting {
            enabled,
            preset: PresetKey::default_key(),
            config: parse_spec(spec).unwrap(),
        }
    }

    fn with_recorder() -> (Shared, Arc<Recorder>) {
        let shared = Shared::new();
        let recorder = Arc::new(Recorder(Mutex::new(Vec::new())));
        shared.set_sink(Some(recorder.clone()));
        (shared, recorder)
    }

    #[test]
    fn a_mouse_that_is_on_reaches_the_system() {
        let (shared, recorder) = with_recorder();
        shared.set_mouse("A", setting(true, "passthrough"));
        shared.process(&shared.channel("A"), 1_000, 3, -4);
        assert_eq!(*recorder.0.lock().unwrap(), [("A".to_string(), 3, -4)]);
    }

    #[test]
    fn a_mouse_that_is_off_is_only_shown() {
        let (shared, recorder) = with_recorder();
        shared.set_mouse("A", setting(false, "passthrough"));
        let channel = shared.channel("A");
        shared.process(&channel, 1_000, 3, -4);
        assert!(recorder.0.lock().unwrap().is_empty());
        // The test area still gets input and output.
        assert_eq!(channel.processor.tap.drain().len(), 2);
    }

    #[test]
    fn mice_keep_their_own_settings() {
        let (shared, recorder) = with_recorder();
        shared.set_mouse("A", setting(true, "precision:factor=0.5"));
        shared.set_mouse("B", setting(true, "passthrough"));
        shared.process(&shared.channel("A"), 1_000, 10, 0);
        shared.process(&shared.channel("B"), 2_000, 10, 0);
        assert_eq!(
            *recorder.0.lock().unwrap(),
            [("A".to_string(), 5, 0), ("B".to_string(), 10, 0)]
        );
    }

    #[test]
    fn the_same_mouse_gets_the_same_channel() {
        let shared = Shared::new();
        let first = shared.channel("A");
        let second = shared.channel("A");
        assert!(Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn a_channel_opened_later_takes_its_settings() {
        let shared = Shared::new();
        shared
            .settings
            .lock()
            .unwrap()
            .insert("A".to_string(), setting(true, "passthrough"));
        assert!(shared.channel("A").is_enabled());
    }

    #[test]
    fn switching_a_mouse_off_lets_go_of_held_motion() {
        let (shared, recorder) = with_recorder();
        shared.set_mouse("A", setting(true, "ema:tau_ms=1000"));
        shared.process(&shared.channel("A"), 1_000, 50, 0);
        assert!(recorder.0.lock().unwrap().is_empty());
        shared.set_mouse("A", setting(false, "ema:tau_ms=1000"));
        assert_eq!(*recorder.0.lock().unwrap(), [("A".to_string(), 50, 0)]);
    }

    #[test]
    fn only_switched_on_mice_count_for_capturing() {
        let shared = Shared::new();
        shared.set_mouse("A", setting(false, "passthrough"));
        assert!(!shared.any_enabled());
        shared.set_mouse("B", setting(true, "passthrough"));
        assert!(shared.any_enabled());
    }

    fn cycle() -> Vec<(PresetKey, PipelineConfig)> {
        ["off", "balance", "smooth"]
            .iter()
            .map(|id| (PresetKey::Standard(id.to_string()), PipelineConfig::default()))
            .collect()
    }

    #[test]
    fn the_hotkey_walks_the_mouse_in_use_through_the_presets() {
        let shared = Shared::new();
        shared.set_cycle(cycle());
        shared.set_mouse("A", setting(true, "passthrough"));
        shared.set_mouse("B", setting(true, "passthrough"));
        shared.channel("A").note_packet(1_000);
        shared.channel("B").note_packet(2_000);

        shared.next_preset();
        let switch = shared.take_switch().unwrap();
        assert_eq!(switch.mouse, "B");
        assert_eq!(switch.preset, PresetKey::Standard("smooth".to_string()));
        assert_eq!(shared.take_switch(), None);

        shared.next_preset();
        let switch = shared.take_switch().unwrap();
        assert_eq!(switch.preset, PresetKey::Standard("off".to_string()));
    }

    #[test]
    fn the_hotkey_leaves_switched_off_mice_alone() {
        let shared = Shared::new();
        shared.set_cycle(cycle());
        shared.set_mouse("A", setting(false, "passthrough"));
        shared.next_preset();
        assert_eq!(shared.take_switch(), None);
    }

    #[test]
    fn nothing_counts_as_stalled_without_capturing() {
        let shared = Shared::new();
        assert!(!shared.stalled_at(u64::MAX));
    }

    #[test]
    fn a_silent_source_counts_as_stalled_while_capturing() {
        let shared = Shared::new();
        shared.set_capturing(true, false);
        shared.last_beat_us.store(1_000, Ordering::Relaxed);
        assert!(!shared.stalled_at(1_000 + HOOK_STALL_LIMIT_US));
        assert!(shared.stalled_at(1_000 + HOOK_STALL_LIMIT_US + 1));
    }

    #[test]
    fn the_driver_gets_less_patience_than_the_hook() {
        let shared = Shared::new();
        shared.set_capturing(true, true);
        shared.last_beat_us.store(1_000, Ordering::Relaxed);
        assert!(shared.stalled_at(1_000 + DRIVER_STALL_LIMIT_US + 1));
        assert!(!shared.hook_source_quiet(1_000 + DRIVER_STALL_LIMIT_US + 1));
    }

    #[test]
    fn the_hook_steps_aside_long_before_the_program_starts_over() {
        let shared = Shared::new();
        shared.set_capturing(true, false);
        shared.last_beat_us.store(1_000, Ordering::Relaxed);
        let soon = 1_000 + STAND_ASIDE_AFTER_US + 1;
        assert!(shared.hook_source_quiet(soon));
        assert!(!shared.stalled_at(soon));
    }

    #[test]
    fn nothing_counts_as_stuck_after_the_capture_ended() {
        let shared = Shared::new();
        shared.set_capturing(true, true);
        shared.capture_ended();
        assert!(!shared.stalled_at(u64::MAX));
    }

    fn situation() -> Situation {
        Situation {
            capturing: true,
            ..Situation::default()
        }
    }

    #[test]
    fn without_capturing_the_filter_is_off_whatever_is_in_front() {
        let now = Situation {
            capturing: false,
            foreground: Foreground::Elevated,
            ..situation()
        };
        assert_eq!(decide(now), Standing::Off);
    }

    #[test]
    fn the_hook_works_in_ordinary_windows() {
        assert_eq!(decide(situation()), Standing::Filtering);
    }

    #[test]
    fn the_hook_stands_aside_for_elevated_windows_and_games() {
        let with = |foreground| Situation {
            foreground,
            ..situation()
        };
        assert_eq!(decide(with(Foreground::Elevated)), Standing::AdminWindow);
        assert_eq!(decide(with(Foreground::Game)), Standing::Game);
    }

    #[test]
    fn trouble_counts_only_in_ordinary_windows() {
        let broken = Situation {
            trouble: true,
            ..situation()
        };
        assert_eq!(decide(broken), Standing::Repairing);
        let behind_admin = Situation {
            foreground: Foreground::Elevated,
            ..broken
        };
        assert_eq!(decide(behind_admin), Standing::AdminWindow);
    }

    #[test]
    fn the_driver_ignores_what_is_in_front() {
        let now = Situation {
            use_driver: true,
            foreground: Foreground::Elevated,
            trouble: true,
            ..situation()
        };
        assert_eq!(decide(now), Standing::Filtering);
    }

    #[test]
    fn a_pause_wins_over_everything() {
        for use_driver in [false, true] {
            let now = Situation {
                use_driver,
                paused: true,
                foreground: Foreground::Game,
                ..situation()
            };
            assert_eq!(decide(now), Standing::Paused);
        }
    }

    #[test]
    fn with_hands_off_nothing_reaches_the_system() {
        let (shared, recorder) = with_recorder();
        shared.set_mouse("A", setting(true, "passthrough"));
        shared.apply_standing(Standing::AdminWindow);
        let channel = shared.channel("A");
        shared.process(&channel, 1_000, 3, -4);
        assert!(recorder.0.lock().unwrap().is_empty());
        assert_eq!(channel.processor.tap.drain().len(), 2, "the test area still shows it");
    }

    #[test]
    fn stepping_aside_lets_go_of_what_is_held_first() {
        let (shared, recorder) = with_recorder();
        shared.set_mouse("A", setting(true, "ema:tau_ms=1000"));
        shared.process(&shared.channel("A"), 1_000, 9, 0);
        assert!(shared.apply_standing(Standing::Game));
        assert_eq!(*recorder.0.lock().unwrap(), [("A".to_string(), 9, 0)]);
        assert!(!shared.apply_standing(Standing::Game), "no change the second time");
    }

    #[test]
    fn coming_back_opens_the_way_out_again() {
        let (shared, recorder) = with_recorder();
        shared.set_mouse("A", setting(true, "passthrough"));
        shared.apply_standing(Standing::Paused);
        shared.apply_standing(Standing::Filtering);
        shared.process(&shared.channel("A"), 1_000, 3, 0);
        assert_eq!(recorder.0.lock().unwrap().len(), 1);
    }

    #[test]
    fn standings_survive_being_stored() {
        for standing in Standing::ALL {
            assert_eq!(Standing::from_index(standing as u8), standing);
        }
        assert_eq!(Standing::from_index(200), Standing::Off);
    }

    fn channel(key: &str) -> Arc<Channel> {
        Arc::new(Channel::new(key, &MouseSetting::default()))
    }

    #[test]
    fn a_device_that_could_not_be_placed_is_asked_about_again() {
        let mut matcher = DeviceMatcher::default();
        assert!(matcher.place(7, 0, || None).is_none());
        assert!(
            matcher.place(7, 1_000, || Some(channel("A"))).is_none(),
            "not asked again at once"
        );
        let found = matcher.place(7, UNKNOWN_DEVICE_US, || Some(channel("A")));
        assert_eq!(found.unwrap().key, "A");
    }

    #[test]
    fn a_failed_look_up_keeps_what_was_known() {
        let mut matcher = DeviceMatcher::default();
        matcher.place(7, 0, || Some(channel("A")));
        let later = matcher.place(7, KNOWN_DEVICE_US, || None);
        assert_eq!(later.unwrap().key, "A");
    }

    #[test]
    fn a_handle_taken_over_by_another_device_is_noticed() {
        let mut matcher = DeviceMatcher::default();
        matcher.place(7, 0, || Some(channel("A")));
        let soon = matcher.place(7, 1_000, || Some(channel("B")));
        assert_eq!(soon.unwrap().key, "A", "answers are kept for a while");
        let later = matcher.place(7, KNOWN_DEVICE_US, || Some(channel("B")));
        assert_eq!(later.unwrap().key, "B");
    }

    #[test]
    fn stopping_lets_go_of_what_is_held() {
        let (shared, recorder) = with_recorder();
        shared.set_mouse("A", setting(true, "ema:tau_ms=1000"));
        shared.process(&shared.channel("A"), 1_000, 9, 0);
        shared.release_all();
        assert_eq!(*recorder.0.lock().unwrap(), [("A".to_string(), 9, 0)]);
    }
}
