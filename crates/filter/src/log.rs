//! A small log next to the program, for what happens with no window to show it.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use ilumx_win::windows_settings::local_timestamp;

const FILE_NAME: &str = "ilutrackball.log";
/// The log starts over past this size, so it can never fill a disk.
const MAX_BYTES: u64 = 256 * 1024;

pub fn path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.join(FILE_NAME))
}

/// Adds a line. Failing to log must never disturb anything else.
pub fn write(line: &str) {
    let Some(path) = path() else {
        return;
    };
    if fs::metadata(&path).is_ok_and(|meta| meta.len() > MAX_BYTES) {
        let _ = fs::remove_file(&path);
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(file, "{} {line}", local_timestamp());
    }
}
