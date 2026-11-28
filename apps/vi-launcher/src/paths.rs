//! Where the launcher keeps its files and where it looks for the build it
//! starts.
use std::path::{Path, PathBuf};

/// The folder of the launcher's own files: the settings, the stamp of what
/// was prepared, the logs and the downloads. The same folder the PowerShell
/// launcher uses (`%APPDATA%\VI-BR-PORT`, `~/.config/VI-BR-PORT`), so both
/// read one settings file. `VICTORY_LAUNCHER_CONFIG` names another settings
/// file (the tests use it); the rest then goes next to it.
pub fn data_dir() -> PathBuf {
    if let Some(parent) = config_override().as_deref().and_then(Path::parent) {
        return parent.to_path_buf();
    }
    directories::BaseDirs::new()
        .map(|b| b.config_dir().to_path_buf())
        .unwrap_or_else(std::env::temp_dir)
        .join("VI-BR-PORT")
}

fn config_override() -> Option<PathBuf> {
    std::env::var_os("VICTORY_LAUNCHER_CONFIG").filter(|v| !v.is_empty()).map(PathBuf::from)
}

pub fn config_path() -> PathBuf {
    config_override().unwrap_or_else(|| data_dir().join("launcher.json"))
}

pub fn stamp_path() -> PathBuf {
    data_dir().join("prepared.json")
}

pub fn logs_dir() -> PathBuf {
    data_dir().join("logs")
}

pub fn downloads_dir() -> PathBuf {
    data_dir().join("updates")
}

/// The folder the launcher's executable is in.
pub fn install_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// The file name of the build the launcher starts.
pub const VICTORY_EXE: &str = "victory.exe";

/// The repository the launcher was built in, when it runs from its
/// `target` folder: the first folder up from it with a `Cargo.toml` and
/// `apps/vi-launcher`.
pub fn repository_root(from: &Path) -> Option<PathBuf> {
    from.ancestors()
        .find(|d| d.join("Cargo.toml").is_file() && d.join("apps").join("vi-launcher").is_dir())
        .map(Path::to_path_buf)
}

/// Where the build is looked for, in order: the path set in the options, the
/// `victory` folder of a release bundle, next to the launcher, and in a
/// repository checkout `target/<build>/victory.exe`.
pub fn victory_candidates(chosen: Option<&Path>, build: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(path) = chosen {
        out.push(path.to_path_buf());
    }
    let here = install_dir();
    out.push(here.join("victory").join(VICTORY_EXE));
    out.push(here.join(VICTORY_EXE));
    if let Some(root) = repository_root(&here) {
        out.push(root.join("target").join(build).join(VICTORY_EXE));
    }
    out
}

pub fn find_victory(chosen: Option<&Path>, build: &str) -> Option<PathBuf> {
    victory_candidates(chosen, build).into_iter().find(|p| p.is_file())
}

/// The folder a run is started in: the one holding `victory.toml` next to the
/// build or above it (a checkout's root), else the build's own folder.
pub fn run_dir(victory: &Path) -> PathBuf {
    let own = victory.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
    own.ancestors()
        .take(4)
        .find(|d| d.join("victory.toml").is_file() || d.join("Cargo.toml").is_file())
        .map(Path::to_path_buf)
        .unwrap_or(own)
}
