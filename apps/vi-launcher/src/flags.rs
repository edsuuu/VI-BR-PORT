//! The switches the player adds by hand: any `VICTORY_*` name with a value,
//! each with its own on/off, applied after the options' own switches.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Flag {
    pub name: String,
    #[serde(default)]
    pub value: String,
    #[serde(default = "on")]
    pub enabled: bool,
}

fn on() -> bool {
    true
}

/// Why a typed name is not taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameProblem {
    Empty,
    NotVictory,
    BadCharacter,
    /// The launcher sets this one itself and keeps where the files are.
    Kept,
}

impl NameProblem {
    pub fn key(self) -> &'static str {
        match self {
            Self::Empty => "flags.problem.empty",
            Self::NotVictory => "flags.problem.prefix",
            Self::BadCharacter => "flags.problem.characters",
            Self::Kept => "flags.problem.kept",
        }
    }
}

/// The switches that say where the files are: never cleared, never typed.
pub const KEPT_PATHS: &[&str] = &["VICTORY_CONFIG", "VICTORY_GAME_DIR", "VICTORY_CACHE_DIR", "VICTORY_DATA_DIR"];

pub fn check_name(name: &str) -> Result<(), NameProblem> {
    let name = name.trim();
    if name.is_empty() {
        return Err(NameProblem::Empty);
    }
    if !name.starts_with("VICTORY_") || name.len() == "VICTORY_".len() {
        return Err(NameProblem::NotVictory);
    }
    if !name.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_') {
        return Err(NameProblem::BadCharacter);
    }
    if KEPT_PATHS.contains(&name) {
        return Err(NameProblem::Kept);
    }
    Ok(())
}

/// A pasted line of switches (`A=1;B=2`, one per line, or the
/// `$env:A='1'` form) made into flags. Lines that are not a switch are left
/// out.
pub fn parse_pasted(text: &str) -> Vec<Flag> {
    let mut out = Vec::new();
    for part in text.split(['\n', ';']) {
        let part = part.trim().trim_start_matches("$env:").trim_start_matches("set ").trim();
        let Some((name, value)) = part.split_once('=') else { continue };
        let name = name.trim().to_owned();
        let value = value.trim().trim_matches(['\'', '"']).to_owned();
        if check_name(&name).is_ok() {
            out.push(Flag { name, value, enabled: true });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(check_name("VICTORY_RT_QUERY"), Ok(()));
        assert_eq!(check_name(""), Err(NameProblem::Empty));
        assert_eq!(check_name("RT_QUERY"), Err(NameProblem::NotVictory));
        assert_eq!(check_name("VICTORY_"), Err(NameProblem::NotVictory));
        assert_eq!(check_name("VICTORY_rt"), Err(NameProblem::BadCharacter));
        assert_eq!(check_name("VICTORY_DATA_DIR"), Err(NameProblem::Kept));
    }

    #[test]
    fn pasted() {
        let flags = parse_pasted("VICTORY_A=1;VICTORY_B=x,y\n$env:VICTORY_C='2'\nnot a flag\nPATH=1");
        let names: Vec<_> = flags.iter().map(|f| (f.name.as_str(), f.value.as_str())).collect();
        assert_eq!(names, [("VICTORY_A", "1"), ("VICTORY_B", "x,y"), ("VICTORY_C", "2")]);
    }
}
