//! The player's own copy of the game: the folder they extracted, checked for
//! the title's executable.
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::text::{self, Language};

/// The title's executable, as the console's package names it.
pub const GAME_EXE: &str = "game_scarlett_master.exe";

/// Why a folder is not taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GameProblem {
    NoFolder(PathBuf),
    NoExe(PathBuf),
    /// The file is there but is not a Windows executable (an unfinished copy,
    /// a placeholder).
    NotAnExe(PathBuf),
}

impl GameProblem {
    pub fn describe(&self, language: Language) -> String {
        let (key, path) = match self {
            Self::NoFolder(p) => ("game.problem.no_folder", p),
            Self::NoExe(p) => ("game.problem.no_exe", p),
            Self::NotAnExe(p) => ("game.problem.not_exe", p),
        };
        text::get(language, key).replace("{0}", &path.display().to_string()).replace("{1}", GAME_EXE)
    }
}

/// The executable of a game folder. The folder itself, or the executable
/// picked by mistake in place of its folder, or the folder one level up from
/// it (the copy's own folder holding a single inner one), are all taken.
pub fn find_game_exe(chosen: &Path) -> Result<PathBuf, GameProblem> {
    if chosen.is_file() {
        let is_exe = chosen.file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(GAME_EXE));
        return match (is_exe, chosen.parent()) {
            (true, _) => checked(chosen.to_path_buf()),
            (false, Some(dir)) => find_game_exe(dir),
            (false, None) => Err(GameProblem::NoExe(chosen.to_path_buf())),
        };
    }
    if !chosen.is_dir() {
        return Err(GameProblem::NoFolder(chosen.to_path_buf()));
    }
    if let Some(exe) = exe_in(chosen) {
        return checked(exe);
    }
    let inner: Vec<PathBuf> = std::fs::read_dir(chosen)
        .map(|rd| rd.filter_map(Result::ok).map(|e| e.path()).filter(|p| p.is_dir()).collect())
        .unwrap_or_default();
    let found: Vec<PathBuf> = inner.iter().filter_map(|d| exe_in(d)).collect();
    match found.as_slice() {
        [one] => checked(one.clone()),
        _ => Err(GameProblem::NoExe(chosen.to_path_buf())),
    }
}

/// The executable in this folder, whatever the case of its name.
fn exe_in(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(GAME_EXE) && e.path().is_file())
        .map(|e| e.path())
}

/// A Windows executable starts with `MZ` and names its PE header in the first
/// 64 bytes.
fn checked(exe: PathBuf) -> Result<PathBuf, GameProblem> {
    let mut head = [0u8; 64];
    let ok = std::fs::File::open(&exe).and_then(|mut f| f.read_exact(&mut head)).is_ok() && head.starts_with(b"MZ");
    if !ok {
        return Err(GameProblem::NotAnExe(exe));
    }
    let pe = u32::from_le_bytes([head[60], head[61], head[62], head[63]]) as u64;
    let mut sig = [0u8; 4];
    let has_pe = std::fs::File::open(&exe)
        .and_then(|mut f| {
            use std::io::{Seek, SeekFrom};
            f.seek(SeekFrom::Start(pe))?;
            f.read_exact(&mut sig)
        })
        .is_ok()
        && &sig == b"PE\0\0";
    if has_pe { Ok(exe) } else { Err(GameProblem::NotAnExe(exe)) }
}

/// The folder a game executable is in.
pub fn game_dir(exe: &Path) -> PathBuf {
    exe.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(test)]
pub(crate) fn fake_exe(path: &Path) {
    let mut bytes = vec![0u8; 0x90];
    bytes[..2].copy_from_slice(b"MZ");
    bytes[60..64].copy_from_slice(&0x80u32.to_le_bytes());
    bytes[0x80..0x84].copy_from_slice(b"PE\0\0");
    std::fs::write(path, bytes).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_folder_the_exe_or_one_inner_folder() {
        let dir = tempfile::tempdir().unwrap();
        let inner = dir.path().join("GTA VI");
        std::fs::create_dir(&inner).unwrap();
        let exe = inner.join("Game_Scarlett_Master.exe");
        fake_exe(&exe);
        assert_eq!(find_game_exe(&inner).unwrap(), exe);
        assert_eq!(find_game_exe(&exe).unwrap(), exe);
        assert_eq!(find_game_exe(dir.path()).unwrap(), exe);
    }

    #[test]
    fn problems() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(find_game_exe(&dir.path().join("none")), Err(GameProblem::NoFolder(_))));
        assert!(matches!(find_game_exe(dir.path()), Err(GameProblem::NoExe(_))));
        std::fs::write(dir.path().join(GAME_EXE), b"not a program").unwrap();
        assert!(matches!(find_game_exe(dir.path()), Err(GameProblem::NotAnExe(_))));
    }
}

