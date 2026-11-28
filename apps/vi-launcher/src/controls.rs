//! Mouse and keyboard: the player's copy of the key bindings, made from the
//! repository's `bindings.ini` the first time. A run with mouse and keyboard
//! on hands its path to the build as `VICTORY_MNK_BINDINGS` (the same file
//! `scripts/play.ps1 -MouseKeyboard` uses).
use std::path::PathBuf;

/// The bindings the build ships with.
pub const DEFAULT: &str = include_str!("../../../bindings.ini");

/// `%LOCALAPPDATA%\victory\bindings.ini` (beside the build's own data), or
/// the launcher's folder where there is no such place.
pub fn path() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(|d| PathBuf::from(d).join("victory"))
        .unwrap_or_else(crate::paths::data_dir)
        .join("bindings.ini")
}

/// The bindings file, written with the defaults when it is not there.
pub fn ensure() -> std::io::Result<PathBuf> {
    let p = path();
    if !p.is_file() {
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&p, DEFAULT)?;
    }
    Ok(p)
}

/// The bindings as they are now (the defaults when there is no file).
pub fn read() -> String {
    std::fs::read_to_string(path()).unwrap_or_else(|_| DEFAULT.to_owned())
}

/// The bindings as the settings show them: without the comment lines at the
/// top of the file (how to edit it by hand, which the launcher does for the player).
pub fn without_header(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let body = lines.iter().position(|l| !(l.trim_start().starts_with('#') || l.trim().is_empty())).unwrap_or(lines.len());
    let mut out = lines[body..].join("\n");
    if text.ends_with('\n') && !out.is_empty() {
        out.push('\n');
    }
    out
}

pub fn write(text: &str) -> std::io::Result<()> {
    let p = ensure()?;
    std::fs::write(p, text)
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_settings_show_the_bindings_without_the_header() {
        let shown = super::without_header(super::DEFAULT);
        assert!(!shown.starts_with('#'));
        assert!(shown.contains("MoveUp = W"));
    }

    #[test]
    fn the_defaults_move_with_wasd() {
        for line in ["MoveUp = W", "MoveLeft = A", "MoveDown = S", "MoveRight = D"] {
            assert!(super::DEFAULT.contains(line), "{line}");
        }
    }
}
