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

/// The flags as the settings' text box shows them: one `NAME=value` a line,
/// a switched-off one behind `# `.
pub fn to_text(flags: &[Flag]) -> String {
    flags
        .iter()
        .map(|f| format!("{}{}={}", if f.enabled { "" } else { "# " }, f.name, f.value))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The text box as the player sees it: the build's own switches (`base`) with
/// the player's changes over them, then the player's other switches.
pub fn with_base(flags: &[Flag], base: &[(String, String)]) -> String {
    let mut shown: Vec<Flag> = base
        .iter()
        .map(|(name, value)| {
            flags.iter().find(|f| &f.name == name).cloned().unwrap_or(Flag { name: name.clone(), value: value.clone(), enabled: true })
        })
        .collect();
    shown.extend(flags.iter().filter(|f| !base.iter().any(|(n, _)| n == &f.name)).cloned());
    to_text(&shown)
}

/// What is kept of the box: only what differs from the build's switches. A
/// switch of the build left as it is is not stored (a newer build's value then
/// applies); one changed or switched off is; one deleted is stored empty,
/// which takes it out of the run.
pub fn without_base(typed: Vec<Flag>, base: &[(String, String)]) -> Vec<Flag> {
    let mut out: Vec<Flag> = Vec::new();
    for (name, value) in base {
        match typed.iter().find(|f| &f.name == name) {
            Some(f) if f.enabled && &f.value == value => {}
            Some(f) => out.push(f.clone()),
            None => out.push(Flag { name: name.clone(), value: String::new(), enabled: true }),
        }
    }
    out.extend(typed.into_iter().filter(|f| !base.iter().any(|(n, _)| n == &f.name)));
    out
}

/// The text box read back: the flags it holds (a line behind `#` is kept
/// switched off) and how many non-empty lines were not a flag.
pub fn from_text(text: &str) -> (Vec<Flag>, usize) {
    let mut out: Vec<Flag> = Vec::new();
    let mut skipped = 0;
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let (enabled, body) = match line.strip_prefix('#') {
            Some(rest) => (false, rest.trim()),
            None => (true, line),
        };
        match parse_pasted(body).into_iter().next() {
            Some(mut f) => {
                f.enabled = enabled;
                out.retain(|o| o.name != f.name);
                out.push(f);
            }
            None if enabled => skipped += 1,
            None => {}
        }
    }
    (out, skipped)
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_box_shows_the_build_switches_and_stores_only_changes() {
        let base = vec![("VICTORY_A".to_owned(), "1".to_owned()), ("VICTORY_B".to_owned(), "2".to_owned())];
        let shown = super::with_base(&[], &base);
        assert_eq!(shown, "VICTORY_A=1
VICTORY_B=2");
        let (typed, _) = super::from_text("VICTORY_A=1
# VICTORY_B=2
VICTORY_C=3");
        let kept = super::without_base(typed, &base);
        let names: Vec<_> = kept.iter().map(|f| (f.name.as_str(), f.value.as_str(), f.enabled)).collect();
        assert_eq!(names, vec![("VICTORY_B", "2", false), ("VICTORY_C", "3", true)]);
        let (typed, _) = super::from_text("VICTORY_B=2");
        assert_eq!(super::without_base(typed, &base)[0].value, "");
        assert_eq!(super::with_base(&kept, &base), "VICTORY_A=1
# VICTORY_B=2
VICTORY_C=3");
    }

    #[test]
    fn the_text_box_round_trips() {
        let (flags, skipped) = super::from_text("VICTORY_A=1\n# VICTORY_B=x\n\nnot a flag\n# a comment\nVICTORY_A=2");
        assert_eq!(skipped, 1);
        assert_eq!(super::to_text(&flags), "# VICTORY_B=x\nVICTORY_A=2");
    }

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
