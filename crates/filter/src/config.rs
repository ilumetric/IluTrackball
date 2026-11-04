//! The settings file next to the program.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use ilumx_core::spec::{format_spec, parse_spec};
use serde::{Deserialize, Serialize};

use crate::i18n::Lang;
use crate::presets::{Library, PresetKey, UserPreset};

pub const FILE_NAME: &str = "ilutrackball.toml";
/// The name from before the program was renamed. Read once, then left alone.
const OLD_FILE_NAME: &str = "ilumx.toml";

const HEADER: &str = "\
# IluTrackball settings. The program picks up changes to this file while running.
# The window rewrites this file, so comments added here will not survive.
#
# hotkey  switches the mouse in use to the next preset
# device  a mouse with its own switch and preset
# preset  your own presets; the standard ones live inside the program
# spec    the processing chain, see README.md

";

/// How the filtered motion reaches the system.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    /// A mouse hook. Needs nothing installed.
    #[default]
    Hook,
    /// The Interception driver.
    Interception,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PresetEntry {
    pub name: String,
    pub spec: String,
}

/// Settings of one mouse.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeviceEntry {
    pub key: String,
    pub enabled: bool,
    pub preset: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub language: Lang,
    pub backend: Backend,
    pub hotkey: String,
    /// Leaves the mouse alone while a game has it.
    pub games_direct: bool,
    pub device: Vec<DeviceEntry>,
    pub preset: Vec<PresetEntry>,
    /// From the old format, in which one preset was active for the one mouse.
    #[serde(skip_serializing)]
    pub active_preset: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            language: Lang::default(),
            backend: Backend::default(),
            hotkey: "Ctrl+Alt+F11".to_string(),
            games_direct: true,
            device: Vec::new(),
            preset: Vec::new(),
            active_preset: None,
        }
    }
}

/// Settings read from the file, ready to use.
#[derive(Clone, Debug, PartialEq)]
pub struct Loaded {
    pub config: Config,
    pub library: Library,
    /// The old active preset, for the mouse that had it. `Some` once only.
    pub migrated_preset: Option<PresetKey>,
    /// User presets that could not be read.
    pub problems: Vec<String>,
}

/// Turns the file contents into settings. In the old format the list held the
/// standard presets as well: those unchanged are dropped, and those the user
/// changed stay as user presets under a name of their own.
pub fn resolve(config: Config) -> Loaded {
    let migrating = config.active_preset.is_some();
    let mut library = Library::default();
    let mut problems = Vec::new();
    let mut migrated_preset = None;

    for entry in &config.preset {
        let parsed = match parse_spec(&entry.spec) {
            Ok(parsed) => parsed,
            Err(error) => {
                problems.push(format!("«{}»: {error}", entry.name));
                continue;
            }
        };
        let standard = if migrating {
            Library::matching_standard(&entry.name, &parsed)
        } else {
            None
        };
        let key = match standard {
            Some(key) => key,
            None if migrating => library.add(&format!("{} (мой)", entry.name), parsed),
            None => library.add(&entry.name, parsed),
        };
        if config.active_preset.as_deref() == Some(entry.name.as_str()) {
            migrated_preset = Some(key);
        }
    }

    Loaded {
        config: Config {
            active_preset: None,
            ..config
        },
        library,
        migrated_preset,
        problems,
    }
}

/// Puts the current user presets back into the settings.
pub fn with_library(config: &Config, library: &Library) -> Config {
    Config {
        preset: library
            .user
            .iter()
            .map(|UserPreset { name, config }| PresetEntry {
                name: name.clone(),
                spec: format_spec(config),
            })
            .collect(),
        ..config.clone()
    }
}

/// The settings file sits next to the program.
pub fn config_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .unwrap_or_default()
        .join(FILE_NAME)
}

pub fn parse(text: &str) -> Result<Config, String> {
    toml::from_str(text).map_err(|error| error.to_string())
}

pub fn render(config: &Config) -> Result<String, String> {
    let body = toml::to_string_pretty(config).map_err(|error| error.to_string())?;
    Ok(format!("{HEADER}{body}"))
}

/// Reads the settings. A missing file gives the defaults, a broken one is an
/// error and stays as it is.
pub fn load(path: &Path) -> Result<Config, String> {
    let old = path.with_file_name(OLD_FILE_NAME);
    let source = if path.exists() {
        path
    } else if old.exists() {
        old.as_path()
    } else {
        return Ok(Config::default());
    };
    let text = fs::read_to_string(source).map_err(|error| error.to_string())?;
    parse(&text)
}

/// Writes through a temporary file, so a crash or a full disk cannot leave a
/// half-written settings file behind.
pub fn save(path: &Path, config: &Config) -> Result<(), String> {
    let text = render(config)?;
    let temporary = path.with_extension("toml.tmp");
    fs::write(&temporary, text).map_err(|error| error.to_string())?;
    fs::rename(&temporary, path).map_err(|error| error.to_string())
}

pub fn modified_time(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|meta| meta.modified()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const OLD_FORMAT: &str = r#"
hotkey = "Ctrl+Alt+F11"
active_preset = "Плавно"

[[preset]]
name = "Без фильтра"
spec = "passthrough"

[[preset]]
name = "Баланс"
spec = "debeat:release_ms=30+ema:tau_ms=4"

[[preset]]
name = "Плавно"
spec = "debeat:release_ms=60+one_euro:min_cutoff=4,beta=0.05,d_cutoff=1"
"#;

    #[test]
    fn old_settings_move_over_without_losing_changes() {
        let loaded = resolve(parse(OLD_FORMAT).unwrap());
        // Unchanged standard presets are gone from the user's list.
        assert_eq!(loaded.library.user.len(), 1);
        assert_eq!(loaded.library.user[0].name, "Плавно (мой)");
        assert_eq!(
            loaded.migrated_preset,
            Some(PresetKey::User("Плавно (мой)".to_string()))
        );
        assert!(loaded.config.active_preset.is_none());
    }

    #[test]
    fn an_old_active_standard_preset_stays_standard() {
        let text = OLD_FORMAT.replace("active_preset = \"Плавно\"", "active_preset = \"Баланс\"");
        let loaded = resolve(parse(&text).unwrap());
        assert_eq!(loaded.migrated_preset, Some(PresetKey::default_key()));
    }

    #[test]
    fn migration_happens_once() {
        let first = resolve(parse(OLD_FORMAT).unwrap());
        let text = render(&with_library(&first.config, &first.library)).unwrap();
        let second = resolve(parse(&text).unwrap());
        assert_eq!(second.migrated_preset, None);
        assert_eq!(second.library, first.library);
    }

    #[test]
    fn a_broken_user_preset_is_reported_and_the_rest_load() {
        let config = Config {
            preset: vec![
                PresetEntry {
                    name: "Сломанный".to_string(),
                    spec: "smooth".to_string(),
                },
                PresetEntry {
                    name: "Хороший".to_string(),
                    spec: "ema".to_string(),
                },
            ],
            ..Config::default()
        };
        let loaded = resolve(config);
        assert_eq!(loaded.library.user.len(), 1);
        assert!(loaded.problems[0].contains("Сломанный"));
    }

    #[test]
    fn settings_survive_a_round_trip() {
        let config = Config {
            language: Lang::En,
            backend: Backend::Interception,
            device: vec![DeviceEntry {
                key: "VID_046D&PID_C548".to_string(),
                enabled: true,
                preset: PresetKey::default_key().to_text(),
            }],
            ..Config::default()
        };
        assert_eq!(parse(&render(&config).unwrap()).unwrap(), config);
    }

    #[test]
    fn russian_is_the_language_of_a_fresh_start() {
        assert_eq!(parse("").unwrap().language, Lang::Ru);
    }

    #[test]
    fn broken_text_is_an_error() {
        assert!(parse("hotkey = ").is_err());
    }

    #[test]
    fn saving_leaves_no_temporary_file() {
        let folder = std::env::temp_dir().join(format!("ilumx-config-{}", std::process::id()));
        fs::create_dir_all(&folder).unwrap();
        let path = folder.join(FILE_NAME);

        save(&path, &Config::default()).unwrap();
        assert!(fs::read_to_string(&path).unwrap().starts_with("# IluTrackball"));
        assert_eq!(fs::read_dir(&folder).unwrap().count(), 1);

        fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn settings_under_the_old_name_are_picked_up() {
        let folder = std::env::temp_dir().join(format!("ilumx-rename-{}", std::process::id()));
        fs::create_dir_all(&folder).unwrap();
        let path = folder.join(FILE_NAME);

        fs::write(folder.join(OLD_FILE_NAME), "hotkey = \"Ctrl+Alt+F9\"\n").unwrap();
        assert_eq!(load(&path).unwrap().hotkey, "Ctrl+Alt+F9");

        // Once the new file exists, it is the one that counts.
        fs::write(&path, "hotkey = \"Ctrl+Alt+F8\"\n").unwrap();
        assert_eq!(load(&path).unwrap().hotkey, "Ctrl+Alt+F8");

        fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn a_missing_file_gives_the_defaults_and_a_broken_one_stays() {
        let folder = std::env::temp_dir().join(format!("ilumx-broken-{}", std::process::id()));
        fs::create_dir_all(&folder).unwrap();
        let path = folder.join(FILE_NAME);

        assert_eq!(load(&path).unwrap(), Config::default());
        fs::write(&path, "hotkey = ").unwrap();
        assert!(load(&path).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "hotkey = ");

        fs::remove_dir_all(&folder).unwrap();
    }
}
