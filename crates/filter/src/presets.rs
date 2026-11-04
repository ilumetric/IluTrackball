//! The preset library: standard presets that ship with the program and cannot
//! change, and the user's own, which can also travel as files.

use ilumx_core::pipeline::PipelineConfig;
use ilumx_core::spec::{format_spec, parse_spec};
use serde::{Deserialize, Serialize};

use crate::i18n::Lang;

/// Extension of an exported preset.
pub const FILE_EXTENSION: &str = "ilutrack";

const FILE_VERSION: u32 = 1;
const STANDARD_PREFIX: &str = "standard:";
const USER_PREFIX: &str = "user:";
const DEFAULT_ID: &str = "balance";

struct Standard {
    id: &'static str,
    ru: &'static str,
    en: &'static str,
    spec: &'static str,
}

const STANDARD: [Standard; 7] = [
    Standard {
        id: "off",
        ru: "Без фильтра",
        en: "No filter",
        spec: "passthrough",
    },
    Standard {
        id: "low-lag",
        ru: "Мин. задержка",
        en: "Lowest lag",
        spec: "debeat:release_ms=40",
    },
    Standard {
        id: DEFAULT_ID,
        ru: "Баланс",
        en: "Balanced",
        spec: "debeat:release_ms=30+ema:tau_ms=4",
    },
    Standard {
        id: "smooth",
        ru: "Плавно",
        en: "Smooth",
        spec: "debeat:release_ms=60+spread:ms=16.7",
    },
    Standard {
        id: "precise",
        ru: "Точная работа",
        en: "Precise work",
        spec: "debeat:release_ms=30+ema:tau_ms=4+precision:factor=0.5",
    },
    Standard {
        id: "modeling",
        ru: "Моделинг",
        en: "Modeling",
        spec: "debeat:release_ms=30+ema:tau_ms=4+accel:type=table,points=0/1;0.7/1.05;1.4/1.8;2.2/3.3;3/6.2;3.4/8",
    },
    Standard {
        id: "one-euro",
        ru: "One Euro",
        en: "One Euro",
        spec: "one_euro:min_cutoff=4,beta=0.05",
    },
];

fn standard_config(item: &Standard) -> PipelineConfig {
    parse_spec(item.spec).expect("standard presets are valid")
}

/// Which preset a mouse uses.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum PresetKey {
    Standard(String),
    User(String),
}

impl PresetKey {
    pub fn default_key() -> Self {
        PresetKey::Standard(DEFAULT_ID.to_string())
    }

    pub fn is_standard(&self) -> bool {
        matches!(self, PresetKey::Standard(_))
    }

    /// The form kept in the settings file.
    pub fn to_text(&self) -> String {
        match self {
            PresetKey::Standard(id) => format!("{STANDARD_PREFIX}{id}"),
            PresetKey::User(name) => format!("{USER_PREFIX}{name}"),
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        if let Some(id) = text.strip_prefix(STANDARD_PREFIX) {
            return Some(PresetKey::Standard(id.to_string()));
        }
        text.strip_prefix(USER_PREFIX)
            .map(|name| PresetKey::User(name.to_string()))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct UserPreset {
    pub name: String,
    pub config: PipelineConfig,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Library {
    pub user: Vec<UserPreset>,
}

impl Library {
    pub fn standard_keys() -> Vec<PresetKey> {
        STANDARD
            .iter()
            .map(|item| PresetKey::Standard(item.id.to_string()))
            .collect()
    }

    pub fn user_keys(&self) -> Vec<PresetKey> {
        self.user
            .iter()
            .map(|preset| PresetKey::User(preset.name.clone()))
            .collect()
    }

    /// Every preset in the order of the list.
    pub fn keys(&self) -> Vec<PresetKey> {
        let mut keys = Self::standard_keys();
        keys.extend(self.user_keys());
        keys
    }

    pub fn config(&self, key: &PresetKey) -> Option<PipelineConfig> {
        match key {
            PresetKey::Standard(id) => STANDARD
                .iter()
                .find(|item| item.id == id)
                .map(standard_config),
            PresetKey::User(name) => self
                .user
                .iter()
                .find(|preset| preset.name == *name)
                .map(|preset| preset.config.clone()),
        }
    }

    pub fn name(&self, key: &PresetKey, lang: Lang) -> String {
        match key {
            PresetKey::Standard(id) => STANDARD
                .iter()
                .find(|item| item.id == id)
                .map_or_else(|| id.clone(), |item| lang.tr(item.ru, item.en).to_string()),
            PresetKey::User(name) => name.clone(),
        }
    }

    /// The wanted name, with a number added if it is taken.
    pub fn unique_name(&self, wanted: &str) -> String {
        let taken = |name: &str| {
            self.user.iter().any(|preset| preset.name == name)
                || STANDARD.iter().any(|item| item.ru == name || item.en == name)
        };
        let wanted = wanted.trim();
        let wanted = if wanted.is_empty() { "Preset" } else { wanted };
        if !taken(wanted) {
            return wanted.to_string();
        }
        (2..)
            .map(|number| format!("{wanted} {number}"))
            .find(|name| !taken(name))
            .expect("some number is free")
    }

    pub fn add(&mut self, name: &str, config: PipelineConfig) -> PresetKey {
        let name = self.unique_name(name);
        self.user.push(UserPreset {
            name: name.clone(),
            config,
        });
        PresetKey::User(name)
    }

    /// Changes a user preset. Standard presets refuse, they never change.
    pub fn update(&mut self, key: &PresetKey, config: PipelineConfig) -> bool {
        let PresetKey::User(name) = key else {
            return false;
        };
        match self.user.iter_mut().find(|preset| preset.name == *name) {
            Some(preset) => {
                preset.config = config;
                true
            }
            None => false,
        }
    }

    /// Renames a user preset and returns its new key.
    pub fn rename(&mut self, key: &PresetKey, name: &str) -> Option<PresetKey> {
        let PresetKey::User(old) = key else {
            return None;
        };
        if old == name.trim() {
            return Some(key.clone());
        }
        let name = self.unique_name(name);
        let preset = self.user.iter_mut().find(|preset| preset.name == *old)?;
        preset.name = name.clone();
        Some(PresetKey::User(name))
    }

    pub fn remove(&mut self, key: &PresetKey) -> bool {
        let PresetKey::User(name) = key else {
            return false;
        };
        let before = self.user.len();
        self.user.retain(|preset| preset.name != *name);
        self.user.len() != before
    }

    /// The standard preset with this name and these settings, if there is one.
    pub fn matching_standard(name: &str, config: &PipelineConfig) -> Option<PresetKey> {
        STANDARD
            .iter()
            .find(|item| item.ru == name && standard_config(item) == *config)
            .map(|item| PresetKey::Standard(item.id.to_string()))
    }
}

/// A preset as a file to pass around.
#[derive(Debug, Serialize, Deserialize)]
struct PresetFile {
    version: u32,
    name: String,
    spec: String,
}

pub fn export_text(name: &str, config: &PipelineConfig) -> Result<String, String> {
    let file = PresetFile {
        version: FILE_VERSION,
        name: name.to_string(),
        spec: format_spec(config),
    };
    toml::to_string_pretty(&file).map_err(|error| error.to_string())
}

pub fn import_text(text: &str) -> Result<UserPreset, String> {
    let file: PresetFile = toml::from_str(text).map_err(|error| error.to_string())?;
    if file.version > FILE_VERSION {
        return Err(format!("version {} is newer than this program", file.version));
    }
    let config = parse_spec(&file.spec)?;
    Ok(UserPreset {
        name: file.name,
        config,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(spec: &str) -> PipelineConfig {
        parse_spec(spec).unwrap()
    }

    #[test]
    fn standard_presets_all_load() {
        let library = Library::default();
        for key in Library::standard_keys() {
            assert!(library.config(&key).is_some(), "{key:?}");
            assert!(!library.name(&key, Lang::Ru).is_empty());
            assert!(!library.name(&key, Lang::En).is_empty());
        }
    }

    #[test]
    fn standard_presets_have_both_languages() {
        let library = Library::default();
        let key = PresetKey::default_key();
        assert_eq!(library.name(&key, Lang::Ru), "Баланс");
        assert_eq!(library.name(&key, Lang::En), "Balanced");
    }

    #[test]
    fn standard_presets_cannot_change() {
        let mut library = Library::default();
        let key = PresetKey::default_key();
        let before = library.config(&key);
        assert!(!library.update(&key, config("ema:tau_ms=99")));
        assert!(library.rename(&key, "мой").is_none());
        assert!(!library.remove(&key));
        assert_eq!(library.config(&key), before);
    }

    #[test]
    fn user_presets_add_update_and_go() {
        let mut library = Library::default();
        let key = library.add("Мой", config("ema:tau_ms=5"));
        assert_eq!(key, PresetKey::User("Мой".to_string()));

        assert!(library.update(&key, config("ema:tau_ms=9")));
        assert_eq!(library.config(&key), Some(config("ema:tau_ms=9")));

        let renamed = library.rename(&key, "Новый").unwrap();
        assert_eq!(library.config(&key), None);
        assert!(library.config(&renamed).is_some());

        assert!(library.remove(&renamed));
        assert!(library.user.is_empty());
    }

    #[test]
    fn names_never_collide() {
        let mut library = Library::default();
        let first = library.add("Мой", config("passthrough"));
        let second = library.add("Мой", config("passthrough"));
        assert_ne!(first, second);
        assert_eq!(second, PresetKey::User("Мой 2".to_string()));
        assert_eq!(
            library.add("Баланс", config("passthrough")),
            PresetKey::User("Баланс 2".to_string())
        );
    }

    #[test]
    fn renaming_to_the_same_name_changes_nothing() {
        let mut library = Library::default();
        let key = library.add("Мой", config("passthrough"));
        assert_eq!(library.rename(&key, "Мой"), Some(key));
    }

    #[test]
    fn keys_survive_the_settings_file() {
        for key in [PresetKey::default_key(), PresetKey::User("Мой: 1".to_string())] {
            assert_eq!(PresetKey::parse(&key.to_text()), Some(key));
        }
        assert_eq!(PresetKey::parse("nonsense"), None);
    }

    #[test]
    fn a_preset_survives_a_file() {
        let original = config("debeat:release_ms=30+accel:type=table,points=0/1;2/3");
        let text = export_text("Для работы", &original).unwrap();
        let imported = import_text(&text).unwrap();
        assert_eq!(imported.name, "Для работы");
        assert_eq!(imported.config, original);
    }

    #[test]
    fn a_bad_file_is_an_error() {
        assert!(import_text("not a preset").is_err());
        assert!(import_text("version = 1\nname = \"x\"\nspec = \"smooth\"\n").is_err());
        assert!(import_text("version = 99\nname = \"x\"\nspec = \"ema\"\n")
            .unwrap_err()
            .contains("newer"));
    }

    #[test]
    fn finds_an_unchanged_standard_preset_by_name() {
        let same = config("debeat:release_ms=30+ema:tau_ms=4");
        assert_eq!(
            Library::matching_standard("Баланс", &same),
            Some(PresetKey::default_key())
        );
        assert_eq!(Library::matching_standard("Баланс", &config("ema")), None);
    }
}
