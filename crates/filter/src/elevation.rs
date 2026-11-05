//! Running with administrator rights without asking at every start.
//!
//! An ordinary program cannot act while a window above it is in front: the
//! motion does not reach it, or what it sends back is thrown away. Windows
//! starts a scheduled task with full rights and no prompt, so the switch in the
//! settings asks once, to create that task, and every later start goes through
//! it. The registry key of the user says what the task does: reading the task
//! itself would mean parsing the output of `schtasks`, in the language of the
//! system.

use std::env;
use std::fs;
use std::io;
use std::mem;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{self, Command, Stdio};
use std::ptr;
use std::sync::{Arc, Mutex};
use std::thread;

use windows_sys::core::PCWSTR;
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_CANCELLED};
use windows_sys::Win32::System::Com::{
    CoInitializeEx, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
};
use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;
use windows_sys::Win32::System::Threading::{
    GetExitCodeProcess, WaitForSingleObject, CREATE_NO_WINDOW, INFINITE,
};
use windows_sys::Win32::UI::Shell::{
    ShellExecuteExW, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS,
    SHELLEXECUTEINFOW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

use crate::autostart::{self, delete_value, read_value, write_value, MINIMIZED_FLAG};
use crate::foreground;
use crate::log;

/// Tells a copy that the task started it. It may have to wait for the copy that
/// asked for it to be gone.
pub const TASK_FLAG: &str = "--from-task";
/// Runs one change of the task with full rights and ends. The word after it is
/// what `TaskMode::word` gives.
pub const SET_TASK_FLAG: &str = "--set-admin-task";

const TASK_NAME: &str = "IluTrackball";
const STATE_KEY: &str = "Software\\IluTrackball";
const PROGRAM_VALUE: &str = "AdminTaskProgram";
const AT_LOGON_VALUE: &str = "AdminTaskAtLogon";
const SHOW_WINDOW_VALUE: &str = "ShowWindowOnce";
const PATH_CHARS: usize = 512;
const HELPER_FAILED: i32 = 1;

/// What the task does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskMode {
    /// There is no task.
    Off,
    /// It only serves starts of the program.
    OnDemand,
    /// It also starts the program when the user signs in.
    AtLogon,
}

impl TaskMode {
    fn word(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::OnDemand => "demand",
            Self::AtLogon => "logon",
        }
    }

    fn from_word(word: &str) -> Option<Self> {
        [Self::Off, Self::OnDemand, Self::AtLogon]
            .into_iter()
            .find(|mode| mode.word() == word)
    }
}

/// Why a change of the task did not happen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The person said no to the prompt of Windows.
    Declined,
    Failed(String),
}

/// A task is only this copy's if it starts this very file.
fn same_program(recorded: &str, program: &Path) -> bool {
    recorded.eq_ignore_ascii_case(&program.to_string_lossy())
}

/// What the task does for this copy of the program.
pub fn current() -> TaskMode {
    let Ok(program) = env::current_exe() else {
        return TaskMode::Off;
    };
    match read_value(STATE_KEY, PROGRAM_VALUE) {
        Some(recorded) if same_program(&recorded, &program) => {
            if read_value(STATE_KEY, AT_LOGON_VALUE).is_some() {
                TaskMode::AtLogon
            } else {
                TaskMode::OnDemand
            }
        }
        _ => TaskMode::Off,
    }
}

fn record(mode: TaskMode, program: &Path) -> Result<(), String> {
    if mode == TaskMode::Off {
        delete_value(STATE_KEY, PROGRAM_VALUE)?;
        return delete_value(STATE_KEY, AT_LOGON_VALUE);
    }
    write_value(STATE_KEY, PROGRAM_VALUE, &program.to_string_lossy())?;
    if mode == TaskMode::AtLogon {
        write_value(STATE_KEY, AT_LOGON_VALUE, "1")
    } else {
        delete_value(STATE_KEY, AT_LOGON_VALUE)
    }
}

/// The next copy to start, whatever the way, shows its window once. The task
/// always starts the program hidden.
pub fn ask_for_window() {
    let _ = write_value(STATE_KEY, SHOW_WINDOW_VALUE, "1");
}

/// True if a copy before this one asked for the window.
pub fn take_window_request() -> bool {
    let asked = read_value(STATE_KEY, SHOW_WINDOW_VALUE).is_some();
    if asked {
        let _ = delete_value(STATE_KEY, SHOW_WINDOW_VALUE);
    }
    asked
}

fn escaped(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The task as Windows wants it. Without these settings it would stop after
/// three days and on battery power, and the filter would run with low priority.
fn task_xml(program: &Path, user: &str, at_logon: bool) -> String {
    let folder = program.parent().unwrap_or(program);
    let user = escaped(user);
    let trigger = if at_logon {
        format!("<LogonTrigger><Enabled>true</Enabled><UserId>{user}</UserId></LogonTrigger>")
    } else {
        String::new()
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Description>Starts IluTrackball with administrator rights</Description></RegistrationInfo>
  <Triggers>{trigger}</Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>{user}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>HighestAvailable</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>false</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle>
    <WakeToRun>false</WakeToRun>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>4</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{command}</Command>
      <Arguments>{MINIMIZED_FLAG} {TASK_FLAG}</Arguments>
      <WorkingDirectory>{folder}</WorkingDirectory>
    </Exec>
  </Actions>
</Task>
"#,
        command = escaped(&program.to_string_lossy()),
        folder = escaped(&folder.to_string_lossy()),
    )
}

fn utf16_file(text: &str) -> Vec<u8> {
    let mut bytes = vec![0xFF, 0xFE];
    bytes.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
    bytes
}

/// A full path: a bare name would also be looked for next to the program, in a
/// folder the user can write to, and this runs with full rights.
fn schtasks_path() -> PathBuf {
    let mut buffer = [0u16; PATH_CHARS];
    let length = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), PATH_CHARS as u32) } as usize;
    let folder = String::from_utf16_lossy(&buffer[..length.min(PATH_CHARS)]);
    PathBuf::from(folder).join("schtasks.exe")
}

fn schtasks(arguments: &[&str]) -> Result<(), String> {
    let output = Command::new(schtasks_path())
        .args(arguments)
        .stdin(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        return Ok(());
    }
    log::write(&format!(
        "schtasks {}: {}",
        arguments[0],
        String::from_utf8_lossy(&output.stderr).trim()
    ));
    Err(format!("schtasks: {:?}", output.status.code()))
}

fn account() -> Result<String, String> {
    let name = env::var("USERNAME").map_err(|error| error.to_string())?;
    Ok(match env::var("USERDOMAIN") {
        Ok(domain) => format!("{domain}\\{name}"),
        Err(_) => name,
    })
}

fn create_task(program: &Path, at_logon: bool) -> Result<(), String> {
    let text = task_xml(program, &account()?, at_logon);
    let file = env::temp_dir().join(format!("IluTrackball-task-{}.xml", process::id()));
    fs::write(&file, utf16_file(&text)).map_err(|error| error.to_string())?;
    let result = schtasks(&[
        "/Create",
        "/TN",
        TASK_NAME,
        "/XML",
        &file.to_string_lossy(),
        "/F",
    ]);
    let _ = fs::remove_file(&file);
    result
}

/// Makes the change. This needs full rights, and the same account as the
/// person's, because the task and the record of it are both theirs.
fn apply(mode: TaskMode) -> Result<(), String> {
    let program = env::current_exe().map_err(|error| error.to_string())?;
    if mode == TaskMode::Off {
        let started_at_logon = current() == TaskMode::AtLogon;
        if schtasks(&["/Query", "/TN", TASK_NAME]).is_ok() {
            schtasks(&["/Delete", "/TN", TASK_NAME, "/F"])?;
        }
        autostart::set_enabled(started_at_logon)?;
    } else {
        create_task(&program, mode == TaskMode::AtLogon)?;
        // The task takes over from the Run key: both would start the program.
        autostart::set_enabled(false)?;
    }
    record(mode, &program)
}

/// The part of the program that runs behind the prompt of Windows. Returns what
/// to exit with, or nothing if this start is not such a run.
pub fn run_as_helper(arguments: &[String]) -> Option<i32> {
    let position = arguments.iter().position(|arg| arg == SET_TASK_FLAG)?;
    let mode = arguments
        .get(position + 1)
        .and_then(|word| TaskMode::from_word(word));
    let result = match mode {
        Some(mode) => apply(mode),
        None => Err(format!("{SET_TASK_FLAG}: unknown mode")),
    };
    Some(match result {
        Ok(()) => 0,
        Err(error) => {
            log::write(&format!("the administrator task was not changed: {error}"));
            HELPER_FAILED
        }
    })
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

/// Runs this program again behind the prompt of Windows and waits for it.
fn ask_windows(mode: TaskMode) -> Result<(), Refusal> {
    let program = env::current_exe().map_err(|error| Refusal::Failed(error.to_string()))?;
    let verb = wide("runas");
    let file = wide(&program.to_string_lossy());
    let parameters = wide(&format!("{SET_TASK_FLAG} {}", mode.word()));

    unsafe { CoInitializeEx(ptr::null(), (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32) };
    let mut info: SHELLEXECUTEINFOW = unsafe { mem::zeroed() };
    info.cbSize = mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI;
    info.lpVerb = verb.as_ptr() as PCWSTR;
    info.lpFile = file.as_ptr() as PCWSTR;
    info.lpParameters = parameters.as_ptr() as PCWSTR;
    info.nShow = SW_HIDE;

    if unsafe { ShellExecuteExW(&mut info) } == 0 {
        let error = unsafe { GetLastError() };
        return Err(if error == ERROR_CANCELLED {
            Refusal::Declined
        } else {
            Refusal::Failed(format!("system error {error}"))
        });
    }
    if info.hProcess.is_null() {
        return Err(Refusal::Failed("no process".to_string()));
    }
    let mut code = HELPER_FAILED as u32;
    let read = unsafe {
        WaitForSingleObject(info.hProcess, INFINITE);
        let read = GetExitCodeProcess(info.hProcess, &mut code);
        CloseHandle(info.hProcess);
        read
    };
    if read != 0 && code == 0 {
        Ok(())
    } else {
        Err(Refusal::Failed(
            "see ilutrackball.log next to the program".to_string(),
        ))
    }
}

fn change(mode: TaskMode) -> Result<(), Refusal> {
    if foreground::own_process_elevated() {
        apply(mode).map_err(Refusal::Failed)
    } else {
        ask_windows(mode)
    }
}

/// Starts the program through the task. The copy it starts waits for the lock.
pub fn start_through_task() -> bool {
    schtasks(&["/Run", "/TN", TASK_NAME]).is_ok()
}

/// Changes the task on a thread of its own: the prompt of Windows waits for the
/// person, and the window has to stay alive meanwhile.
#[derive(Clone, Default)]
pub struct TaskSwitch {
    state: Arc<Mutex<SwitchState>>,
}

#[derive(Default)]
enum SwitchState {
    #[default]
    Idle,
    Working,
    Done(TaskMode, Result<(), Refusal>),
}

impl TaskSwitch {
    pub fn is_busy(&self) -> bool {
        self.state
            .lock()
            .is_ok_and(|state| matches!(*state, SwitchState::Working))
    }

    pub fn set(&self, mode: TaskMode) -> io::Result<()> {
        if let Ok(mut state) = self.state.lock() {
            *state = SwitchState::Working;
        }
        let switch = self.clone();
        let spawned = thread::Builder::new()
            .name("admin-task".to_string())
            .spawn(move || {
                let result = change(mode);
                if let Ok(mut state) = switch.state.lock() {
                    *state = SwitchState::Done(mode, result);
                }
            });
        if let Err(error) = spawned {
            if let Ok(mut state) = self.state.lock() {
                *state = SwitchState::Idle;
            }
            return Err(error);
        }
        Ok(())
    }

    /// The outcome of the last change, once.
    pub fn take_outcome(&self) -> Option<(TaskMode, Result<(), Refusal>)> {
        let mut state = self.state.lock().ok()?;
        if !matches!(*state, SwitchState::Done(..)) {
            return None;
        }
        match mem::take(&mut *state) {
            SwitchState::Done(mode, result) => Some((mode, result)),
            SwitchState::Idle | SwitchState::Working => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROGRAM: &str = "C:\\Tools & Co\\IluTrackball.exe";

    fn xml(at_logon: bool) -> String {
        task_xml(Path::new(PROGRAM), "PC\\Макс", at_logon)
    }

    #[test]
    fn every_mode_survives_the_trip_through_the_command_line() {
        for mode in [TaskMode::Off, TaskMode::OnDemand, TaskMode::AtLogon] {
            assert_eq!(TaskMode::from_word(mode.word()), Some(mode));
        }
        assert_eq!(TaskMode::from_word("everything"), None);
    }

    #[test]
    fn the_task_asks_for_full_rights_and_starts_the_program_hidden() {
        let text = xml(false);
        assert!(text.contains("<RunLevel>HighestAvailable</RunLevel>"));
        assert!(text.contains("<Arguments>--minimized --from-task</Arguments>"));
    }

    #[test]
    fn the_task_must_not_stop_the_filter_or_slow_it_down() {
        let text = xml(false);
        assert!(text.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"));
        assert!(text.contains("<StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>"));
        assert!(text.contains("<DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>"));
        assert!(text.contains("<Priority>4</Priority>"));
    }

    #[test]
    fn only_the_autostart_setting_adds_a_trigger() {
        assert!(!xml(false).contains("LogonTrigger"));
        assert!(xml(true).contains("<UserId>PC\\Макс</UserId></LogonTrigger>"));
    }

    #[test]
    fn paths_are_written_so_that_xml_stays_valid() {
        let text = xml(false);
        assert!(text.contains("<Command>C:\\Tools &amp; Co\\IluTrackball.exe</Command>"));
        assert!(text.contains("<WorkingDirectory>C:\\Tools &amp; Co</WorkingDirectory>"));
    }

    #[test]
    fn the_file_is_the_utf16_the_xml_promises() {
        let bytes = utf16_file("<a>");
        assert_eq!(&bytes[..4], &[0xFF, 0xFE, b'<', 0]);
        assert_eq!(bytes.len(), 2 + 3 * 2);
    }

    /// Asks the Task Scheduler itself. Flag 1 only validates: nothing is created.
    #[test]
    #[ignore = "asks the Task Scheduler service of this computer"]
    fn windows_accepts_the_task() {
        for at_logon in [false, true] {
            let text = task_xml(Path::new(PROGRAM), &account().unwrap(), at_logon);
            let file = env::temp_dir().join(format!("ilutrackball-check-{at_logon}.xml"));
            fs::write(&file, utf16_file(&text)).unwrap();
            let script = format!(
                r"$s = New-Object -ComObject Schedule.Service; $s.Connect();
                  $x = [IO.File]::ReadAllText('{}');
                  [void]$s.GetFolder('\').RegisterTask('IluTrackballCheck', $x, 1, $null, $null, 3)",
                file.display()
            );
            let output = Command::new("powershell")
                .args(["-NoProfile", "-Command", &script])
                .output()
                .unwrap();
            let _ = fs::remove_file(&file);
            let said = String::from_utf8_lossy(&output.stderr);
            assert!(output.status.success() && said.trim().is_empty(), "{said}");
        }
    }

    #[test]
    fn a_task_of_a_moved_program_is_not_this_copys() {
        let here = Path::new("E:\\Software\\IluTrackball\\IluTrackball.exe");
        assert!(same_program("e:\\software\\iluTrackball\\IluTrackball.EXE", here));
        assert!(!same_program("D:\\Old\\IluTrackball.exe", here));
    }

    #[test]
    fn the_scheduler_is_looked_for_in_the_system_folder_only() {
        let path = schtasks_path();
        assert!(path.is_absolute(), "{path:?}");
        assert!(path.is_file(), "{path:?}");
    }

    #[test]
    fn an_outcome_is_given_once() {
        let switch = TaskSwitch::default();
        assert!(switch.take_outcome().is_none());
        *switch.state.lock().unwrap() = SwitchState::Done(TaskMode::Off, Ok(()));
        assert_eq!(switch.take_outcome(), Some((TaskMode::Off, Ok(()))));
        assert!(switch.take_outcome().is_none());
        assert!(!switch.is_busy());
    }
}
