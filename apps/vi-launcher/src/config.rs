//! The settings file (`launcher.json`): the values of the options, the game,
//! the language, the switches added by hand and where the build is. It is the
//! PowerShell launcher's file too: keys this launcher does not know are kept.
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::flags::Flag;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Config {
    #[serde(default = "format_one")]
    pub format: u32,
    /// Option id to its value: a boolean for a toggle, a string for a choice.
    #[serde(default)]
    pub options: Map<String, Value>,
    /// The title's executable inside the chosen folder.
    #[serde(default)]
    pub game_exe: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    /// Where the title's saves (and its other data) go: `VICTORY_DATA_DIR`
    /// of the run. None: the build's own place.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saves_dir: Option<String>,
    /// `VICTORY_*` switches typed by the player, after the options' own.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flags: Vec<Flag>,
    /// Another `victory.exe` than the one found next to the launcher.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub victory_exe: Option<String>,
    /// What starts `victory.exe` where it is not native (Linux): `wine`, or a
    /// command such as `proton run`. Empty means `wine` there and nothing on
    /// Windows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner: Option<String>,
    /// Ask for updates when the window opens (the default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check_updates: Option<bool>,
    /// The window full screen (F11), as it was left.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fullscreen: Option<bool>,
    #[serde(flatten)]
    pub rest: Map<String, Value>,
}

fn format_one() -> u32 {
    1
}

impl Default for Config {
    fn default() -> Self {
        Self {
            format: 1,
            options: Map::new(),
            game_exe: None,
            language: None,
            saves_dir: None,
            flags: Vec::new(),
            victory_exe: None,
            runner: None,
            check_updates: None,
            fullscreen: None,
            rest: Map::new(),
        }
    }
}

/// What reading the file found.
pub struct Loaded {
    pub config: Config,
    /// A file that could not be read as settings was moved here.
    pub set_aside: Option<PathBuf>,
}

pub fn load(path: &Path) -> Loaded {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return Loaded { config: Config::default(), set_aside: None },
    };
    // (Files written by Windows PowerShell may start with a byte order mark.)
    match serde_json::from_str::<Config>(text.trim_start_matches('\u{feff}')) {
        Ok(config) => Loaded { config, set_aside: None },
        Err(_) => {
            let bad = path.with_extension("json.bad");
            let set_aside = std::fs::rename(path, &bad).ok().map(|()| bad);
            Loaded { config: Config::default(), set_aside }
        }
    }
}

/// Written next to the file and moved over it, so a crash never leaves half a
/// file.
pub fn save(path: &Path, config: &Config) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(config).map_err(std::io::Error::other)?;
    let partial = path.with_extension("json.partial");
    std::fs::write(&partial, text + "\n")?;
    std::fs::rename(&partial, path)
}

impl Config {
    pub fn updates_on(&self) -> bool {
        self.check_updates.unwrap_or(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_of_the_other_launcher_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("launcher.json");
        std::fs::write(&path, r#"{"format":1,"options":{"ray_tracing":true},"game_exe":null,"language":"pt-BR","mcp":{"x":1}}"#).unwrap();
        let loaded = load(&path);
        assert_eq!(loaded.config.options["ray_tracing"], Value::Bool(true));
        save(&path, &loaded.config).unwrap();
        let again: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(again["mcp"]["x"], 1);
    }

    #[test]
    fn a_broken_file_is_set_aside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("launcher.json");
        std::fs::write(&path, "{ not json").unwrap();
        let loaded = load(&path);
        assert_eq!(loaded.config, Config::default());
        assert!(loaded.set_aside.unwrap().is_file());
    }
}
