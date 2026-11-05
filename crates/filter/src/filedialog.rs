//! The standard Windows dialogs for choosing a file.

use std::mem;
use std::path::PathBuf;
use std::process::Command;
use std::ptr;

use windows_sys::core::PCWSTR;
use windows_sys::Win32::UI::Controls::Dialogs::{
    GetOpenFileNameW, GetSaveFileNameW, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR, OFN_OVERWRITEPROMPT,
    OFN_PATHMUSTEXIST, OPENFILENAMEW,
};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use crate::foreground;

const PATH_CHARS: usize = 1024;

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

/// `description\0*.ext\0\0`, the form the dialogs want for their filter.
fn filter_text(description: &str, extension: &str) -> Vec<u16> {
    format!("{description} (*.{extension})\0*.{extension}\0\0")
        .encode_utf16()
        .collect()
}

fn to_path(buffer: &[u16]) -> PathBuf {
    let length = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    PathBuf::from(String::from_utf16_lossy(&buffer[..length]))
}

fn dialog_fields(filter: &[u16], extension: &[u16], buffer: &mut [u16]) -> OPENFILENAMEW {
    let mut fields: OPENFILENAMEW = unsafe { mem::zeroed() };
    fields.lStructSize = mem::size_of::<OPENFILENAMEW>() as u32;
    fields.lpstrFilter = filter.as_ptr();
    fields.lpstrFile = buffer.as_mut_ptr();
    fields.nMaxFile = buffer.len() as u32;
    fields.lpstrDefExt = extension.as_ptr();
    fields
}

/// Asks where to save a file, with `name` offered.
pub fn save_path(description: &str, extension: &str, name: &str) -> Option<PathBuf> {
    let filter = filter_text(description, extension);
    let extension = wide(extension);
    let mut buffer = [0u16; PATH_CHARS];
    for (slot, unit) in buffer
        .iter_mut()
        .take(PATH_CHARS - 1)
        .zip(safe_file_name(name).encode_utf16())
    {
        *slot = unit;
    }
    let mut fields = dialog_fields(&filter, &extension, &mut buffer);
    fields.Flags = OFN_OVERWRITEPROMPT | OFN_NOCHANGEDIR | OFN_PATHMUSTEXIST;
    let chosen = unsafe { GetSaveFileNameW(&mut fields) } != 0;
    chosen.then(|| to_path(&buffer))
}

/// Asks which file to open.
pub fn open_path(description: &str, extension: &str) -> Option<PathBuf> {
    let filter = filter_text(description, extension);
    let extension = wide(extension);
    let mut buffer = [0u16; PATH_CHARS];
    let mut fields = dialog_fields(&filter, &extension, &mut buffer);
    fields.Flags = OFN_FILEMUSTEXIST | OFN_NOCHANGEDIR | OFN_PATHMUSTEXIST;
    let chosen = unsafe { GetOpenFileNameW(&mut fields) } != 0;
    chosen.then(|| to_path(&buffer))
}

/// A preset name as a file name: characters Windows refuses become spaces.
fn safe_file_name(name: &str) -> String {
    name.chars()
        .map(|c| if "<>:\"/\\|?*".contains(c) || c.is_control() { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_string()
}

/// Opens a web page or a file with the program Windows has for it.
pub fn open_external(target: &str) {
    if foreground::own_process_elevated() {
        // Started from here, a browser would get full rights too. Explorer
        // hands the request over to the desktop, which has none.
        if let Some(folder) = std::env::var_os("SystemRoot") {
            let explorer = PathBuf::from(folder).join("explorer.exe");
            if Command::new(explorer).arg(target).spawn().is_ok() {
                return;
            }
        }
    }
    let target = wide(target);
    let verb = wide("open");
    unsafe {
        ShellExecuteW(
            ptr::null_mut(),
            verb.as_ptr() as PCWSTR,
            target.as_ptr() as PCWSTR,
            ptr::null(),
            ptr::null(),
            SW_SHOWNORMAL,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_lose_forbidden_characters() {
        assert_eq!(safe_file_name("Моделинг"), "Моделинг");
        assert_eq!(safe_file_name("a/b:c"), "a b c");
        assert_eq!(safe_file_name("  x  "), "x");
    }

    #[test]
    fn the_filter_has_the_double_terminator_of_the_dialogs() {
        let filter = filter_text("Preset", "ilumx");
        assert_eq!(&filter[filter.len() - 2..], &[0, 0]);
        assert_eq!(filter.iter().filter(|&&c| c == 0).count(), 3);
    }

    #[test]
    fn paths_stop_at_the_terminator() {
        let buffer: Vec<u16> = "C:\\a\0junk".encode_utf16().collect();
        assert_eq!(to_path(&buffer), PathBuf::from("C:\\a"));
    }
}
