//! Starting together with Windows, through the Run key of the current user.

use std::env;
use std::ffi::c_void;
use std::mem;
use std::path::Path;
use std::ptr;

use windows_sys::core::PCWSTR;
use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows_sys::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
};

/// Passed by Windows at startup: the program opens minimized.
pub const MINIMIZED_FLAG: &str = "--minimized";

const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const VALUE_NAME: &str = "IluTrackball";
const COMMAND_CHARS: usize = 1024;

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn command_line(program: &Path) -> String {
    format!("\"{}\" {MINIMIZED_FLAG}", program.display())
}

pub(crate) fn read_value(key: &str, name: &str) -> Option<String> {
    let (key, name) = (wide(key), wide(name));
    let mut buffer = [0u16; COMMAND_CHARS];
    let mut size = mem::size_of_val(&buffer) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr() as PCWSTR,
            name.as_ptr() as PCWSTR,
            RRF_RT_REG_SZ,
            ptr::null_mut(),
            buffer.as_mut_ptr() as *mut c_void,
            &mut size,
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    let length = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..length]))
}

pub(crate) fn write_value(key: &str, name: &str, text: &str) -> Result<(), String> {
    let (key, name, text) = (wide(key), wide(name), wide(text));
    let status = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            key.as_ptr() as PCWSTR,
            name.as_ptr() as PCWSTR,
            REG_SZ,
            text.as_ptr() as *const c_void,
            (text.len() * mem::size_of::<u16>()) as u32,
        )
    };
    if status == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(format!("registry error {status}"))
    }
}

pub(crate) fn delete_value(key: &str, name: &str) -> Result<(), String> {
    let (key, name) = (wide(key), wide(name));
    let status = unsafe {
        RegDeleteKeyValueW(
            HKEY_CURRENT_USER,
            key.as_ptr() as PCWSTR,
            name.as_ptr() as PCWSTR,
        )
    };
    if status == ERROR_SUCCESS || status == ERROR_FILE_NOT_FOUND {
        Ok(())
    } else {
        Err(format!("registry error {status}"))
    }
}

pub fn is_enabled() -> bool {
    read_value(RUN_KEY, VALUE_NAME).is_some()
}

pub fn set_enabled(enabled: bool) -> Result<(), String> {
    if !enabled {
        return delete_value(RUN_KEY, VALUE_NAME);
    }
    let program = env::current_exe().map_err(|error| error.to_string())?;
    write_value(RUN_KEY, VALUE_NAME, &command_line(&program))
}

/// Keeps the entry pointing here after the program was moved. An entry that is
/// not there stays away: only the switch in the window may create one.
pub fn follow_program() {
    let Some(current) = read_value(RUN_KEY, VALUE_NAME) else {
        return;
    };
    if let Ok(program) = env::current_exe() {
        let wanted = command_line(&program);
        if current != wanted {
            let _ = write_value(RUN_KEY, VALUE_NAME, &wanted);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::Registry::RegDeleteKeyW;

    /// A private key, so tests never touch the real startup list.
    const TEST_KEY: &str = "Software\\IluTrackballTest";

    #[test]
    fn the_command_quotes_the_path_and_starts_minimized() {
        let command = command_line(Path::new("C:\\Program Files\\Ilu\\IluTrackball.exe"));
        assert_eq!(
            command,
            "\"C:\\Program Files\\Ilu\\IluTrackball.exe\" --minimized"
        );
    }

    #[test]
    fn a_value_can_be_written_read_and_removed() {
        write_value(TEST_KEY, "entry", "\"C:\\Путь\\a.exe\" --minimized").unwrap();
        assert_eq!(
            read_value(TEST_KEY, "entry").as_deref(),
            Some("\"C:\\Путь\\a.exe\" --minimized")
        );
        delete_value(TEST_KEY, "entry").unwrap();
        assert_eq!(read_value(TEST_KEY, "entry"), None);

        // Removing what is not there is no error: the switch may simply be off.
        delete_value(TEST_KEY, "entry").unwrap();

        let key = wide(TEST_KEY);
        unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, key.as_ptr() as PCWSTR) };
    }
}
