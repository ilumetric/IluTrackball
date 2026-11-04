//! Updates from the releases on GitHub.
//!
//! All of it runs on threads of its own, so a slow network cannot reach the
//! cursor. Nothing is downloaded until the user asks.
//!
//! A running program cannot be overwritten, but it can be renamed. So the new
//! file is fetched and checked next to the old one, the two change places, and
//! the new one starts once the old one has let go of the mouse. The old file
//! stays until the new one has lived for a while, and a new version that fails
//! at once puts it back.
//!
//! The checksum proves that the file arrived whole. It comes from the same
//! release, so it says nothing about who made the release: for that the program
//! relies on HTTPS and on the GitHub account.

use std::env;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use ilumx_win::hash::sha256_hex;
use ilumx_win::http::{self, Request};
use serde::Deserialize;

use crate::autostart::MINIMIZED_FLAG;
use crate::log;
use crate::recovery::{RESTARTED_FLAG, TRIAL_LIFE};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Tells a new copy that it has just replaced the old version.
pub const UPDATED_FLAG: &str = "--updated";
/// Asks a file to prove it can start: it answers with `SELF_TEST_CODE`.
pub const SELF_TEST_FLAG: &str = "--self-test";
pub const SELF_TEST_CODE: i32 = 77;

const REPOSITORY: &str = "ilumetric/IluTrackball";
const API_ACCEPT: &str = "application/vnd.github+json";
const USER_AGENT: &str = concat!("IluTrackball/", env!("CARGO_PKG_VERSION"));
const PROGRAM_ASSET: &str = "IluTrackball.exe";
const CHECKSUM_ASSET: &str = "IluTrackball.exe.sha256";
const SELF_TEST_LIMIT: Duration = Duration::from_secs(15);
const SELF_TEST_POLL: Duration = Duration::from_millis(50);

/// A debug build can be pointed at a local server, to go through a whole update
/// without publishing anything. A release build has no such thing.
#[cfg(debug_assertions)]
fn test_server() -> Option<String> {
    env::var("ILUTRACKBALL_TEST_SERVER").ok()
}

#[cfg(not(debug_assertions))]
fn test_server() -> Option<String> {
    None
}

fn latest_release_url() -> String {
    match test_server() {
        Some(server) => format!("{server}/latest"),
        None => format!("https://api.github.com/repos/{REPOSITORY}/releases/latest"),
    }
}

/// Files are taken from this place only, whatever an answer says.
fn download_prefix() -> String {
    match test_server() {
        Some(server) => format!("{server}/"),
        None => format!("https://github.com/{REPOSITORY}/releases/download/"),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    /// The page of the release, with its notes.
    pub page: String,
    program_url: String,
    checksum_url: String,
    size: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// GitHub could not be reached.
    Network,
    /// The answer made no sense.
    BadAnswer,
    /// The file did not arrive the way it was published.
    Damaged,
    /// The new file does not start.
    WontStart,
    /// The program cannot write to its own folder.
    CannotWrite,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum UpdateState {
    #[default]
    Unknown,
    Checking,
    UpToDate,
    Available(Release),
    Downloading {
        release: Release,
        share: f32,
    },
    /// The new file is in place and only needs starting.
    Ready,
    Failed(Failure),
}

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    assets: Vec<ApiAsset>,
}

#[derive(Deserialize)]
struct ApiAsset {
    name: String,
    browser_download_url: String,
    size: u64,
}

fn parse_version(text: &str) -> Option<(u32, u32, u32)> {
    let mut parts = text.trim().trim_start_matches('v').split('.');
    let mut next = || parts.next()?.parse().ok();
    let version = (next()?, next()?, next()?);
    parts.next().is_none().then_some(version)
}

fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(candidate), Some(current)) => candidate > current,
        _ => false,
    }
}

/// Reads the answer about the latest release. `None` means there is nothing to
/// install, which includes a release that has no program in it yet.
fn read_release(answer: &str, current: &str, prefix: &str) -> Result<Option<Release>, Failure> {
    let release: ApiRelease = serde_json::from_str(answer).map_err(|_| Failure::BadAnswer)?;
    if !is_newer(&release.tag_name, current) {
        return Ok(None);
    }
    let asset = |name: &str| release.assets.iter().find(|asset| asset.name == name);
    let (Some(program), Some(checksum)) = (asset(PROGRAM_ASSET), asset(CHECKSUM_ASSET)) else {
        return Ok(None);
    };
    let trusted = |asset: &ApiAsset| asset.browser_download_url.starts_with(prefix);
    if !trusted(program) || !trusted(checksum) {
        return Err(Failure::BadAnswer);
    }
    Ok(Some(Release {
        version: release.tag_name.trim_start_matches('v').to_string(),
        page: release.html_url.clone(),
        program_url: program.browser_download_url.clone(),
        checksum_url: checksum.browser_download_url.clone(),
        size: program.size,
    }))
}

fn find_release() -> Result<Option<Release>, Failure> {
    let url = latest_release_url();
    let request = Request {
        url: &url,
        user_agent: USER_AGENT,
        accept: Some(API_ACCEPT),
    };
    let mut answer = Vec::new();
    let status = http::get(&request, &mut |data, _| {
        answer.extend_from_slice(data);
        Ok(())
    })
    .map_err(|_| Failure::Network)?;
    match status {
        200 => {
            let text = String::from_utf8_lossy(&answer);
            read_release(&text, VERSION, &download_prefix())
        }
        // Nothing has been published yet.
        404 => Ok(None),
        _ => Err(Failure::Network),
    }
}

/// The first word of a checksum file, the hash itself.
fn read_checksum(text: &str) -> Option<String> {
    let hash = text.split_whitespace().next()?.to_ascii_lowercase();
    let is_hash = hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit());
    is_hash.then_some(hash)
}

fn fetch_checksum(release: &Release) -> Result<String, Failure> {
    let request = Request {
        url: &release.checksum_url,
        user_agent: USER_AGENT,
        accept: None,
    };
    let mut answer = Vec::new();
    let status = http::get(&request, &mut |data, _| {
        answer.extend_from_slice(data);
        Ok(())
    })
    .map_err(|_| Failure::Network)?;
    if status != 200 {
        return Err(Failure::Network);
    }
    read_checksum(&String::from_utf8_lossy(&answer)).ok_or(Failure::BadAnswer)
}

fn download(release: &Release, target: &Path, on_share: &dyn Fn(f32)) -> Result<(), Failure> {
    let mut file = File::create(target).map_err(|_| Failure::CannotWrite)?;
    let request = Request {
        url: &release.program_url,
        user_agent: USER_AGENT,
        accept: None,
    };
    let mut received = 0u64;
    let mut disk_failed = false;
    let status = http::get(&request, &mut |data, _| {
        if let Err(error) = file.write_all(data) {
            disk_failed = true;
            return Err(error);
        }
        received += data.len() as u64;
        on_share(received as f32 / release.size.max(1) as f32);
        Ok(())
    });
    if disk_failed {
        return Err(Failure::CannotWrite);
    }
    match status {
        Ok(200) => file.flush().map_err(|_| Failure::CannotWrite),
        _ => Err(Failure::Network),
    }
}

fn verify(file: &Path, size: u64, expected: &str) -> Result<(), Failure> {
    let bytes = fs::read(file).map_err(|_| Failure::CannotWrite)?;
    let whole = bytes.len() as u64 == size;
    let same = sha256_hex(&bytes).is_some_and(|hash| hash == expected);
    if whole && same {
        Ok(())
    } else {
        Err(Failure::Damaged)
    }
}

/// Runs the new file and waits for its answer. This catches a file that Windows
/// cannot load before anything has been touched.
fn self_test(file: &Path) -> Result<(), Failure> {
    let mut child = Command::new(file)
        .arg(SELF_TEST_FLAG)
        .spawn()
        .map_err(|_| Failure::WontStart)?;
    let give_up_at = Instant::now() + SELF_TEST_LIMIT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.code() == Some(SELF_TEST_CODE) => return Ok(()),
            Ok(Some(_)) | Err(_) => return Err(Failure::WontStart),
            Ok(None) if Instant::now() >= give_up_at => {
                let _ = child.kill();
                return Err(Failure::WontStart);
            }
            Ok(None) => thread::sleep(SELF_TEST_POLL),
        }
    }
}

/// `IluTrackball.exe` becomes `IluTrackball.old.exe` and so on.
fn sibling(program: &Path, kind: &str) -> PathBuf {
    program.with_extension(format!("{kind}.exe"))
}

/// Puts the new file in the place of the program and keeps the old one beside
/// it. If the second step fails, the first one is undone.
fn swap_in(program: &Path, new: &Path, old: &Path) -> io::Result<()> {
    if old.exists() {
        fs::remove_file(old)?;
    }
    fs::rename(program, old)?;
    if let Err(error) = fs::rename(new, program) {
        let _ = fs::rename(old, program);
        return Err(error);
    }
    Ok(())
}

fn fetch_and_swap(
    release: &Release,
    program: &Path,
    on_share: &dyn Fn(f32),
) -> Result<(), Failure> {
    let expected = fetch_checksum(release)?;
    let new = sibling(program, "new");
    let prepared = download(release, &new, on_share)
        .and_then(|()| verify(&new, release.size, &expected))
        .and_then(|()| self_test(&new))
        .and_then(|()| {
            swap_in(program, &new, &sibling(program, "old")).map_err(|_| Failure::CannotWrite)
        });
    if prepared.is_err() {
        let _ = fs::remove_file(&new);
    }
    prepared
}

/// Asks about releases and installs them, on threads of its own.
#[derive(Clone)]
pub struct Updater {
    state: Arc<Mutex<UpdateState>>,
    program: Option<PathBuf>,
}

impl Updater {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(UpdateState::Unknown)),
            // Taken now: after the swap the system may no longer know this name.
            program: env::current_exe().ok(),
        }
    }

    pub fn state(&self) -> UpdateState {
        self.state
            .lock()
            .map(|state| state.clone())
            .unwrap_or_default()
    }

    fn set(&self, state: UpdateState) {
        if let Ok(mut current) = self.state.lock() {
            *current = state;
        }
    }

    fn is_busy(&self) -> bool {
        matches!(
            self.state(),
            UpdateState::Checking | UpdateState::Downloading { .. } | UpdateState::Ready
        )
    }

    /// Asks GitHub about the latest release. A check nobody asked for keeps
    /// quiet about a failure: the computer may simply be offline.
    pub fn check(&self, by_hand: bool) {
        if self.is_busy() {
            return;
        }
        let before = self.state();
        self.set(UpdateState::Checking);
        let updater = self.clone();
        let spawned = thread::Builder::new()
            .name("update-check".to_string())
            .spawn(move || {
                let next = match find_release() {
                    Ok(Some(release)) => UpdateState::Available(release),
                    Ok(None) => UpdateState::UpToDate,
                    Err(failure) if by_hand => UpdateState::Failed(failure),
                    Err(_) => before,
                };
                updater.set(next);
            });
        if spawned.is_err() {
            self.set(UpdateState::Failed(Failure::Network));
        }
    }

    /// Fetches the release that was found and puts it in place.
    pub fn install(&self) {
        let UpdateState::Available(release) = self.state() else {
            return;
        };
        let Some(program) = self.program.clone() else {
            self.set(UpdateState::Failed(Failure::CannotWrite));
            return;
        };
        self.set(UpdateState::Downloading {
            release: release.clone(),
            share: 0.0,
        });
        let updater = self.clone();
        let spawned = thread::Builder::new()
            .name("update-install".to_string())
            .spawn(move || {
                let on_share = |share: f32| {
                    updater.set(UpdateState::Downloading {
                        release: release.clone(),
                        share: share.min(1.0),
                    });
                };
                match fetch_and_swap(&release, &program, &on_share) {
                    Ok(()) => {
                        log::write(&format!("version {} is in place", release.version));
                        updater.set(UpdateState::Ready);
                    }
                    Err(failure) => {
                        log::write(&format!("update failed: {failure:?}"));
                        updater.set(UpdateState::Failed(failure));
                    }
                }
            });
        if spawned.is_err() {
            self.set(UpdateState::Failed(Failure::CannotWrite));
        }
    }

    /// Starts the new version. It waits until this copy has let go of the lock.
    pub fn start_installed(&self, hidden: bool) {
        let Some(program) = &self.program else {
            return;
        };
        let mut command = Command::new(program);
        command.arg(UPDATED_FLAG);
        if hidden {
            command.arg(MINIMIZED_FLAG);
        }
        if let Some(folder) = program.parent() {
            command.current_dir(folder);
        }
        if let Err(error) = command.spawn() {
            log::write(&format!("could not start the new version: {error}"));
        }
    }
}

/// Clears away what an update left behind. A version that has just been
/// installed keeps the old file until it has proved itself.
pub fn tidy_up(just_updated: bool) {
    let Ok(program) = env::current_exe() else {
        return;
    };
    let _ = fs::remove_file(sibling(&program, "new"));
    let _ = fs::remove_file(sibling(&program, "bad"));
    let old = sibling(&program, "old");
    if !just_updated {
        let _ = fs::remove_file(old);
        return;
    }
    let _ = thread::Builder::new()
        .name("update-tidy".to_string())
        .spawn(move || {
            thread::sleep(TRIAL_LIFE);
            let _ = fs::remove_file(old);
        });
}

fn put_old_back(program: &Path, old: &Path, bad: &Path) -> io::Result<()> {
    if bad.exists() {
        fs::remove_file(bad)?;
    }
    fs::rename(program, bad)?;
    if let Err(error) = fs::rename(old, program) {
        let _ = fs::rename(bad, program);
        return Err(error);
    }
    Ok(())
}

/// Called when a new version fails soon after starting: goes back to the old
/// one and starts it. False if there is nothing to go back to.
pub fn roll_back() -> bool {
    let Ok(program) = env::current_exe() else {
        return false;
    };
    let old = sibling(&program, "old");
    if !old.is_file() || put_old_back(&program, &old, &sibling(&program, "bad")).is_err() {
        return false;
    }
    let mut command = Command::new(&program);
    command.args([MINIMIZED_FLAG, RESTARTED_FLAG]);
    if let Some(folder) = program.parent() {
        command.current_dir(folder);
    }
    command.spawn().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process;

    const PREFIX: &str = "https://github.com/ilumetric/IluTrackball/releases/download/";
    const HASH: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    fn answer(tag: &str, program_url: &str) -> String {
        format!(
            r#"{{"tag_name":"{tag}","html_url":"https://github.com/ilumetric/IluTrackball/releases/tag/{tag}","assets":[
              {{"name":"IluTrackball.exe.sha256","browser_download_url":"{PREFIX}{tag}/IluTrackball.exe.sha256","size":85}},
              {{"name":"IluTrackball.exe","browser_download_url":"{program_url}","size":8400000}}
            ]}}"#
        )
    }

    fn program_url(tag: &str) -> String {
        format!("{PREFIX}{tag}/IluTrackball.exe")
    }

    /// A folder of its own, gone at the end of the test.
    struct Folder(PathBuf);

    impl Folder {
        fn new(name: &str) -> Self {
            let path = env::temp_dir().join(format!("ilutrackball-{}-{name}", process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn file(&self, name: &str, text: &str) -> PathBuf {
            let path = self.0.join(name);
            fs::write(&path, text).unwrap();
            path
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn versions_compare_as_numbers() {
        assert!(is_newer("v0.10.0", "0.9.9"));
        assert!(is_newer("1.0.0", "0.99.99"));
        assert!(is_newer("v0.1.1", "0.1.0"));
        assert!(!is_newer("v0.1.0", "0.1.0"));
        assert!(!is_newer("v0.0.9", "0.1.0"));
    }

    #[test]
    fn a_tag_that_is_no_version_is_never_newer() {
        assert!(!is_newer("nightly", "0.1.0"));
        assert!(!is_newer("v1.2", "0.1.0"));
        assert!(!is_newer("v1.2.3-beta", "0.1.0"));
        assert!(!is_newer("v1.2.3.4", "0.1.0"));
    }

    #[test]
    fn this_build_has_a_version_the_comparison_understands() {
        assert!(parse_version(VERSION).is_some());
    }

    #[test]
    fn a_newer_release_is_read_from_the_answer() {
        let text = answer("v0.2.0", &program_url("v0.2.0"));
        let release = read_release(&text, "0.1.0", PREFIX).unwrap().unwrap();
        assert_eq!(release.version, "0.2.0");
        assert_eq!(release.size, 8_400_000);
        assert_eq!(release.program_url, program_url("v0.2.0"));
        assert!(release.checksum_url.ends_with("/IluTrackball.exe.sha256"));
        assert!(release.page.ends_with("/releases/tag/v0.2.0"));
    }

    #[test]
    fn the_same_or_an_older_release_is_nothing_to_install() {
        let same = answer("v0.1.0", &program_url("v0.1.0"));
        assert_eq!(read_release(&same, "0.1.0", PREFIX), Ok(None));
    }

    #[test]
    fn a_release_without_its_files_is_nothing_to_install() {
        let text = r#"{"tag_name":"v0.2.0","html_url":"x","assets":[]}"#;
        assert_eq!(read_release(text, "0.1.0", PREFIX), Ok(None));
        let no_assets = r#"{"tag_name":"v0.2.0","html_url":"x"}"#;
        assert_eq!(read_release(no_assets, "0.1.0", PREFIX), Ok(None));
    }

    #[test]
    fn a_file_from_anywhere_else_is_refused() {
        for url in [
            "https://evil.example/IluTrackball.exe",
            "http://github.com/ilumetric/IluTrackball/releases/download/v0.2.0/IluTrackball.exe",
            "https://github.com/someone/else/releases/download/v0.2.0/IluTrackball.exe",
        ] {
            let text = answer("v0.2.0", url);
            assert_eq!(read_release(&text, "0.1.0", PREFIX), Err(Failure::BadAnswer), "{url}");
        }
    }

    #[test]
    fn nonsense_is_a_bad_answer() {
        assert_eq!(read_release("<html>", "0.1.0", PREFIX), Err(Failure::BadAnswer));
    }

    #[test]
    fn checksum_files_are_read_in_the_usual_form() {
        let line = format!("{HASH}  IluTrackball.exe\n");
        assert_eq!(read_checksum(&line).as_deref(), Some(HASH));
        let upper = HASH.to_ascii_uppercase();
        assert_eq!(read_checksum(&upper).as_deref(), Some(HASH));
        assert_eq!(read_checksum("not a hash"), None);
        assert_eq!(read_checksum(""), None);
    }

    #[test]
    fn only_the_exact_file_passes_the_check() {
        let folder = Folder::new("verify");
        let file = folder.file("new.exe", "abc");
        assert_eq!(verify(&file, 3, HASH), Ok(()));
        assert_eq!(verify(&file, 4, HASH), Err(Failure::Damaged), "a cut off file");
        let other = folder.file("other.exe", "abd");
        assert_eq!(verify(&other, 3, HASH), Err(Failure::Damaged));
    }

    #[test]
    fn the_program_and_its_neighbours_have_clear_names() {
        let program = Path::new(r"C:\Tools\IluTrackball.exe");
        assert_eq!(sibling(program, "old"), Path::new(r"C:\Tools\IluTrackball.old.exe"));
        assert_eq!(sibling(program, "new"), Path::new(r"C:\Tools\IluTrackball.new.exe"));
    }

    #[test]
    fn the_new_file_takes_the_place_of_the_program() {
        let folder = Folder::new("swap");
        let program = folder.file("IluTrackball.exe", "old version");
        let new = folder.file("IluTrackball.new.exe", "new version");
        let old = sibling(&program, "old");
        swap_in(&program, &new, &old).unwrap();
        assert_eq!(fs::read_to_string(&program).unwrap(), "new version");
        assert_eq!(fs::read_to_string(&old).unwrap(), "old version");
        assert!(!new.exists());
    }

    #[test]
    fn a_failed_swap_leaves_the_program_where_it_was() {
        let folder = Folder::new("swap-fail");
        let program = folder.file("IluTrackball.exe", "old version");
        let missing = folder.0.join("IluTrackball.new.exe");
        let old = sibling(&program, "old");
        assert!(swap_in(&program, &missing, &old).is_err());
        assert_eq!(fs::read_to_string(&program).unwrap(), "old version");
        assert!(!old.exists());
    }

    #[test]
    fn a_leftover_from_the_last_update_does_not_stop_the_next_one() {
        let folder = Folder::new("swap-leftover");
        let program = folder.file("IluTrackball.exe", "version 2");
        let new = folder.file("IluTrackball.new.exe", "version 3");
        let old = folder.file("IluTrackball.old.exe", "version 1");
        swap_in(&program, &new, &old).unwrap();
        assert_eq!(fs::read_to_string(&old).unwrap(), "version 2");
    }

    #[test]
    fn going_back_restores_the_old_program() {
        let folder = Folder::new("back");
        let program = folder.file("IluTrackball.exe", "failed version");
        let old = folder.file("IluTrackball.old.exe", "good version");
        let bad = sibling(&program, "bad");
        put_old_back(&program, &old, &bad).unwrap();
        assert_eq!(fs::read_to_string(&program).unwrap(), "good version");
        assert_eq!(fs::read_to_string(&bad).unwrap(), "failed version");
        assert!(!old.exists());
    }

    #[test]
    fn a_broken_file_fails_the_self_test() {
        let folder = Folder::new("selftest");
        let file = folder.file("broken.exe", "this is no program");
        assert_eq!(self_test(&file), Err(Failure::WontStart));
    }

    /// Needs the internet, so it only runs when asked for by name.
    #[test]
    #[ignore]
    fn the_real_github_answers() {
        // Either nothing is published yet or this build is the latest one.
        assert_eq!(find_release(), Ok(None));
    }

    #[test]
    fn a_busy_updater_does_not_start_a_second_job() {
        let updater = Updater::new();
        updater.set(UpdateState::Ready);
        updater.check(true);
        assert_eq!(updater.state(), UpdateState::Ready);
    }

    #[test]
    fn nothing_is_installed_before_a_release_was_found() {
        let updater = Updater::new();
        updater.install();
        assert_eq!(updater.state(), UpdateState::Unknown);
    }
}
