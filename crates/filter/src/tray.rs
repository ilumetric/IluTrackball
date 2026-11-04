//! The icon next to the clock.
//!
//! It runs on a thread of its own with a hidden window, so it answers whatever
//! the settings window is doing. Windows forgets every tray icon when Explorer
//! restarts, and at logon Explorer may not be there yet, so the icon is put back
//! for as long as it is missing.

use std::ffi::c_void;
use std::io;
use std::mem;
use std::ptr;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};

use windows_sys::core::{PCSTR, PCWSTR};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    CreateBitmap, CreateDIBSection, DeleteObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    DIB_RGB_COLORS,
};
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows_sys::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIconIndirect, CreatePopupMenu, CreateWindowExW, DefWindowProcW,
    DestroyMenu, DispatchMessageW, FindWindowW, GetCursorPos, GetMessageW,
    GetSystemMetrics, PostMessageW, PostQuitMessage, RegisterClassW, RegisterWindowMessageW,
    SetForegroundWindow, SetMenuDefaultItem, SetTimer, TrackPopupMenu, TranslateMessage, HICON,
    ICONINFO, MF_CHECKED, MF_GRAYED, MF_SEPARATOR, MF_STRING, MSG, SM_CXSMICON, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, WM_APP, WM_CLOSE, WM_DESTROY, WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP, WM_TIMER,
    WNDCLASSW,
};

use crate::engine::{emergency_exit, Shared, Standing};
use crate::i18n::Lang;
use crate::log;
use crate::{ICON_RGBA, ICON_SIZE};

const CLASS_NAME: &str = "IluTrackballTray";
const APP_NAME: &str = "IluTrackball";
/// Windows sends this about the icon.
const WM_TRAY: u32 = WM_APP + 1;
/// Another copy of the program asks for the window.
const WM_SHOW_REQUEST: u32 = WM_APP + 2;
const ICON_ID: u32 = 1;
const REFRESH_TIMER: usize = 1;
const REFRESH_MS: u32 = 1_000;
const QUIT_TIMER: usize = 2;
/// A window that does not close by itself after this long gets no more time.
const QUIT_LIMIT_MS: u32 = 3_000;

const MENU_OPEN: u32 = 1;
const MENU_PAUSE: u32 = 2;
const MENU_QUIT: u32 = 3;
const MENU_UPDATE: u32 = 4;

/// `SetPreferredAppMode` and `FlushMenuThemes` of uxtheme.dll. They have no
/// names, only these numbers. Without them a Win32 menu is always light.
const SET_PREFERRED_APP_MODE: usize = 135;
const FLUSH_MENU_THEMES: usize = 136;
const FORCE_DARK: i32 = 2;

/// What the icon asks of the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayCommand {
    Show,
    /// Show the window at the page where updates are.
    ShowUpdates,
    Quit,
}

/// What the icon currently shows.
#[derive(Clone, Copy, Debug, Default)]
struct Shown {
    present: bool,
    standing: Option<Standing>,
    english: bool,
}

struct Context {
    shared: Arc<Shared>,
    wake: Box<dyn Fn() + Send + Sync>,
    commands: Mutex<Vec<TrayCommand>>,
    shown: Mutex<Shown>,
    /// Handles of the two icons, which stay valid for the whole run.
    working_icon: isize,
    resting_icon: isize,
    taskbar_created: u32,
}

static CONTEXT: OnceLock<Context> = OnceLock::new();

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

/// The state of the filter in words, for the icon and the window.
pub fn status_text(standing: Standing, lang: Lang) -> &'static str {
    match standing {
        Standing::Off => lang.tr("Фильтр выключен", "The filter is off"),
        Standing::Filtering => lang.tr("Фильтр работает", "The filter is working"),
        Standing::Paused => lang.tr("Фильтр на паузе", "The filter is paused"),
        Standing::AdminWindow => lang.tr(
            "Окно с правами администратора: фильтр здесь не действует",
            "A window with administrator rights: the filter cannot work here",
        ),
        Standing::Game => lang.tr("Игра: мышь работает напрямую", "Game: the mouse works directly"),
        Standing::Repairing => lang.tr("Восстанавливаю связь с мышью", "Restoring the mouse link"),
    }
}

/// Averages the picture down to `to` pixels a side. Colour is weighed by its
/// alpha, or the dark surroundings of the transparent pixels would bleed in.
fn shrink(pixels: &[u8], from: usize, to: usize) -> Vec<u8> {
    let mut result = vec![0u8; to * to * 4];
    for y in 0..to {
        for x in 0..to {
            let (mut colour, mut alpha, mut count) = ([0u32; 3], 0u32, 0u32);
            for source_y in (y * from / to)..((y + 1) * from).div_ceil(to) {
                for source_x in (x * from / to)..((x + 1) * from).div_ceil(to) {
                    let at = (source_y * from + source_x) * 4;
                    let weight = u32::from(pixels[at + 3]);
                    for (channel, value) in colour.iter_mut().zip(&pixels[at..at + 3]) {
                        *channel += u32::from(*value) * weight;
                    }
                    alpha += weight;
                    count += 1;
                }
            }
            let at = (y * to + x) * 4;
            if alpha > 0 {
                for (target, channel) in result[at..at + 3].iter_mut().zip(colour) {
                    *target = (channel / alpha) as u8;
                }
            }
            result[at + 3] = (alpha / count.max(1)) as u8;
        }
    }
    result
}

/// The same picture without colour.
fn greyed(pixels: &[u8]) -> Vec<u8> {
    pixels
        .chunks_exact(4)
        .flat_map(|pixel| {
            let [red, green, blue, alpha] = [pixel[0], pixel[1], pixel[2], pixel[3]];
            let light = (u32::from(red) * 3 + u32::from(green) * 6 + u32::from(blue)) / 10;
            let grey = light as u8;
            [grey, grey, grey, alpha]
        })
        .collect()
}

/// Turns RGBA pixels into an icon. Null if Windows refuses.
fn make_icon(pixels: &[u8], size: usize) -> HICON {
    let mut info: BITMAPINFO = unsafe { mem::zeroed() };
    info.bmiHeader = BITMAPINFOHEADER {
        biSize: mem::size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: size as i32,
        biHeight: -(size as i32),
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB,
        ..unsafe { mem::zeroed() }
    };
    let mut bits: *mut c_void = ptr::null_mut();
    let colour = unsafe {
        CreateDIBSection(ptr::null_mut(), &info, DIB_RGB_COLORS, &mut bits, ptr::null_mut(), 0)
    };
    if colour.is_null() || bits.is_null() {
        return ptr::null_mut();
    }
    let target = unsafe { std::slice::from_raw_parts_mut(bits as *mut u8, size * size * 4) };
    for (out, pixel) in target.chunks_exact_mut(4).zip(pixels.chunks_exact(4)) {
        out.copy_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
    }
    let mask = unsafe { CreateBitmap(size as i32, size as i32, 1, 1, ptr::null()) };
    let icon = ICONINFO {
        fIcon: 1,
        xHotspot: 0,
        yHotspot: 0,
        hbmMask: mask,
        hbmColor: colour,
    };
    let handle = unsafe { CreateIconIndirect(&icon) };
    unsafe {
        DeleteObject(colour);
        DeleteObject(mask);
    }
    handle
}

fn build_icons() -> (isize, isize) {
    let size = (unsafe { GetSystemMetrics(SM_CXSMICON) }).max(16) as usize;
    let picture = shrink(ICON_RGBA, ICON_SIZE as usize, size);
    let working = make_icon(&picture, size) as isize;
    let resting = make_icon(&greyed(&picture), size) as isize;
    (working, resting)
}

fn notify_data(window: HWND, icon: HICON, tip: &str) -> NOTIFYICONDATAW {
    let mut data: NOTIFYICONDATAW = unsafe { mem::zeroed() };
    data.cbSize = mem::size_of::<NOTIFYICONDATAW>() as u32;
    data.hWnd = window;
    data.uID = ICON_ID;
    data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    data.uCallbackMessage = WM_TRAY;
    data.hIcon = icon;
    let room = data.szTip.len() - 1;
    for (target, unit) in data.szTip.iter_mut().zip(tip.encode_utf16().take(room)) {
        *target = unit;
    }
    data
}

fn language(shared: &Shared) -> Lang {
    if shared.is_english() {
        Lang::En
    } else {
        Lang::Ru
    }
}

/// Puts the icon up or brings it up to date.
fn refresh_icon(window: HWND, context: &Context) {
    let Ok(mut shown) = context.shown.lock() else {
        return;
    };
    let standing = context.shared.standing();
    let english = context.shared.is_english();
    if shown.present && shown.standing == Some(standing) && shown.english == english {
        return;
    }
    let icon = if standing == Standing::Filtering {
        context.working_icon
    } else {
        context.resting_icon
    };
    let text = status_text(standing, language(&context.shared));
    let data = notify_data(window, icon as HICON, &format!("{APP_NAME}: {text}"));
    let mut done = false;
    if !shown.present {
        done = unsafe { Shell_NotifyIconW(NIM_ADD, &data) } != 0;
    }
    if !done {
        done = unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) } != 0;
    }
    if done {
        *shown = Shown {
            present: true,
            standing: Some(standing),
            english,
        };
    } else {
        // Explorer is not there yet. The timer comes back to this.
        shown.present = false;
    }
}

fn send(context: &Context, command: TrayCommand) {
    if let Ok(mut commands) = context.commands.lock() {
        commands.push(command);
    }
    (context.wake)();
}

fn show_menu(window: HWND, context: &Context) {
    let lang = language(&context.shared);
    let paused = context.shared.is_paused();
    let status = wide(status_text(context.shared.standing(), lang));
    let open = wide(lang.tr("Открыть IluTrackball", "Open IluTrackball"));
    let pause = wide(lang.tr("Пауза", "Pause"));
    let quit = wide(lang.tr("Выход", "Quit"));
    let update = context.shared.new_version().map(|version| {
        wide(&format!("{} {version}", lang.tr("Обновить до", "Update to")))
    });

    let menu = unsafe { CreatePopupMenu() };
    if menu.is_null() {
        return;
    }
    let pause_flags = if paused { MF_STRING | MF_CHECKED } else { MF_STRING };
    unsafe {
        AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, status.as_ptr());
        AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
        AppendMenuW(menu, MF_STRING, MENU_OPEN as usize, open.as_ptr());
        if let Some(update) = &update {
            AppendMenuW(menu, MF_STRING, MENU_UPDATE as usize, update.as_ptr());
        }
        AppendMenuW(menu, pause_flags, MENU_PAUSE as usize, pause.as_ptr());
        AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
        AppendMenuW(menu, MF_STRING, MENU_QUIT as usize, quit.as_ptr());
        SetMenuDefaultItem(menu, MENU_OPEN, 0);
    }

    let mut point = POINT { x: 0, y: 0 };
    unsafe { GetCursorPos(&mut point) };
    // Without this the menu stays open after a click elsewhere.
    unsafe { SetForegroundWindow(window) };
    let chosen = unsafe {
        TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            point.x,
            point.y,
            0,
            window,
            ptr::null(),
        )
    };
    unsafe {
        PostMessageW(window, WM_NULL, 0, 0);
        DestroyMenu(menu);
    }

    match chosen as u32 {
        MENU_OPEN => send(context, TrayCommand::Show),
        MENU_UPDATE => send(context, TrayCommand::ShowUpdates),
        MENU_PAUSE => {
            context.shared.set_paused(!paused);
            (context.wake)();
        }
        MENU_QUIT => {
            unsafe { SetTimer(window, QUIT_TIMER, QUIT_LIMIT_MS, None) };
            send(context, TrayCommand::Quit);
        }
        _ => {}
    }
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let Some(context) = CONTEXT.get() else {
        return unsafe { DefWindowProcW(window, message, wparam, lparam) };
    };
    if message == context.taskbar_created {
        if let Ok(mut shown) = context.shown.lock() {
            shown.present = false;
        }
        refresh_icon(window, context);
        return 0;
    }
    match message {
        WM_TRAY => {
            match (lparam & 0xFFFF) as u32 {
                WM_LBUTTONUP => send(context, TrayCommand::Show),
                WM_RBUTTONUP => show_menu(window, context),
                _ => {}
            }
            0
        }
        WM_SHOW_REQUEST => {
            send(context, TrayCommand::Show);
            0
        }
        WM_TIMER if wparam == QUIT_TIMER => {
            log::write("the window did not close in time");
            emergency_exit();
        }
        WM_TIMER => {
            refresh_icon(window, context);
            0
        }
        WM_DESTROY => {
            let data = notify_data(window, ptr::null_mut(), "");
            unsafe {
                Shell_NotifyIconW(NIM_DELETE, &data);
                PostQuitMessage(0);
            }
            0
        }
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}

fn ask_for_dark_menus() {
    type SetMode = unsafe extern "system" fn(i32) -> i32;
    type Flush = unsafe extern "system" fn();

    let name = wide("uxtheme.dll");
    let library = unsafe { LoadLibraryW(name.as_ptr() as PCWSTR) };
    if library.is_null() {
        return;
    }
    unsafe {
        if let Some(function) = GetProcAddress(library, SET_PREFERRED_APP_MODE as PCSTR) {
            let set_mode: SetMode = mem::transmute(function);
            set_mode(FORCE_DARK);
        }
        if let Some(function) = GetProcAddress(library, FLUSH_MENU_THEMES as PCSTR) {
            let flush: Flush = mem::transmute(function);
            flush();
        }
    }
}

fn create_window() -> io::Result<HWND> {
    let class_name = wide(CLASS_NAME);
    let instance = unsafe { GetModuleHandleW(ptr::null()) };
    let mut class: WNDCLASSW = unsafe { mem::zeroed() };
    class.lpfnWndProc = Some(window_proc);
    class.hInstance = instance;
    class.lpszClassName = class_name.as_ptr();
    if unsafe { RegisterClassW(&class) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // A real window, never shown: a message-only one would not hear about
    // Explorer starting again.
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
    Ok(window)
}

fn run_tray_thread(ready: mpsc::Sender<io::Result<isize>>) {
    ask_for_dark_menus();
    let window = match create_window() {
        Ok(window) => window,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    let _ = ready.send(Ok(window as isize));
    if let Some(context) = CONTEXT.get() {
        refresh_icon(window, context);
    }
    unsafe { SetTimer(window, REFRESH_TIMER, REFRESH_MS, None) };

    let mut message: MSG = unsafe { mem::zeroed() };
    while unsafe { GetMessageW(&mut message, ptr::null_mut(), 0, 0) } > 0 {
        unsafe {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

/// A running icon. Dropping it takes the icon away.
pub struct Tray {
    window: isize,
    thread: Option<JoinHandle<()>>,
}

impl Tray {
    /// `wake` gets the window to look at the commands, whatever it is doing.
    pub fn start(shared: Arc<Shared>, wake: impl Fn() + Send + Sync + 'static) -> io::Result<Self> {
        let (working_icon, resting_icon) = build_icons();
        let taskbar_created = unsafe {
            let name = wide("TaskbarCreated");
            RegisterWindowMessageW(name.as_ptr() as PCWSTR)
        };
        let context = Context {
            shared,
            wake: Box::new(wake),
            commands: Mutex::new(Vec::new()),
            shown: Mutex::new(Shown::default()),
            working_icon,
            resting_icon,
            taskbar_created,
        };
        if CONTEXT.set(context).is_err() {
            return Err(io::Error::other("the tray was started twice"));
        }

        let (ready_tx, ready_rx) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("tray".to_string())
            .spawn(move || run_tray_thread(ready_tx))?;
        let window = ready_rx
            .recv()
            .map_err(|_| io::Error::other("the tray thread stopped during startup"))??;
        Ok(Self {
            window,
            thread: Some(thread),
        })
    }

    /// What the icon has asked for since the last time.
    pub fn take_commands() -> Vec<TrayCommand> {
        CONTEXT
            .get()
            .and_then(|context| context.commands.lock().ok())
            .map(|mut commands| mem::take(&mut *commands))
            .unwrap_or_default()
    }

    /// Asks a copy of the program that is already running to show its window.
    /// False if there is no such copy to ask.
    pub fn show_running_copy() -> bool {
        let class_name = wide(CLASS_NAME);
        let window = unsafe { FindWindowW(class_name.as_ptr() as PCWSTR, ptr::null()) };
        !window.is_null() && unsafe { PostMessageW(window, WM_SHOW_REQUEST, 0, 0) } != 0
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        unsafe { PostMessageW(self.window as HWND, WM_CLOSE, 0, 0) };
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(size: usize, pixel: [u8; 4]) -> Vec<u8> {
        pixel.repeat(size * size)
    }

    #[test]
    fn shrinking_keeps_a_solid_colour() {
        let small = shrink(&solid(64, [200, 40, 30, 255]), 64, 24);
        assert_eq!(small.len(), 24 * 24 * 4);
        assert!(small.chunks_exact(4).all(|pixel| pixel == [200, 40, 30, 255]));
    }

    #[test]
    fn shrinking_does_not_darken_a_soft_edge() {
        // Half transparent black next to solid red must stay red, only fainter.
        let mut pixels = solid(2, [200, 0, 0, 255]);
        pixels[4..8].copy_from_slice(&[0, 0, 0, 0]);
        pixels[12..16].copy_from_slice(&[0, 0, 0, 0]);
        let small = shrink(&pixels, 2, 1);
        assert_eq!(&small[..3], &[200, 0, 0]);
        assert!((120..=135).contains(&small[3]), "{}", small[3]);
    }

    #[test]
    fn the_real_icon_shrinks_to_something_round() {
        let size = 24;
        let small = shrink(ICON_RGBA, ICON_SIZE as usize, size);
        let alpha = |x: usize, y: usize| small[(y * size + x) * 4 + 3];
        assert_eq!(alpha(0, 0), 0, "the corners stay transparent");
        assert_eq!(alpha(size / 2, size / 2), 255);
    }

    #[test]
    fn grey_loses_the_colour_and_keeps_the_shape() {
        let grey = greyed(&[200, 40, 30, 128]);
        assert_eq!(grey[0], grey[1]);
        assert_eq!(grey[1], grey[2]);
        assert_eq!(grey[3], 128);
    }

    #[test]
    fn every_standing_has_words_in_both_languages() {
        let all = [
            Standing::Off,
            Standing::Filtering,
            Standing::Paused,
            Standing::AdminWindow,
            Standing::Game,
            Standing::Repairing,
        ];
        for standing in all {
            let (ru, en) = (status_text(standing, Lang::Ru), status_text(standing, Lang::En));
            assert_ne!(ru, en);
            // The tip of a tray icon has room for 127 characters.
            let tip = format!("{APP_NAME}: {ru}");
            assert!(tip.encode_utf16().count() <= 127, "{tip}");
        }
    }
}
