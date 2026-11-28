//! The window's words, from the language files: this launcher's own
//! (`apps/vi-launcher/lang`) and, for the options, the PowerShell launcher's
//! (`scripts/launcher/lang`), so both name an option the same way. A key
//! missing in a language falls back to English, then to the key itself.
use std::collections::HashMap;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    English,
    PortugueseBrazil,
}

pub const LANGUAGES: &[Language] = &[Language::PortugueseBrazil, Language::English];

impl Language {
    pub fn code(self) -> &'static str {
        match self {
            Self::English => "en",
            Self::PortugueseBrazil => "pt-BR",
        }
    }

    pub fn from_code(code: &str) -> Option<Self> {
        LANGUAGES.iter().copied().find(|l| l.code().eq_ignore_ascii_case(code))
    }

    /// The saved language, else the system's (Portuguese for any `pt`), else
    /// English.
    pub fn detect(saved: Option<&str>) -> Self {
        if let Some(l) = saved.and_then(Self::from_code) {
            return l;
        }
        if system_locale().to_ascii_lowercase().starts_with("pt") { Self::PortugueseBrazil } else { Self::English }
    }
}

#[cfg(windows)]
fn system_locale() -> String {
    use windows_sys::Win32::Globalization::GetUserDefaultLocaleName;
    let mut buf = [0u16; 85];
    // SAFETY: the buffer and its length in characters.
    let n = unsafe { GetUserDefaultLocaleName(buf.as_mut_ptr(), buf.len() as i32) };
    if n <= 1 { String::new() } else { String::from_utf16_lossy(&buf[..n as usize - 1]) }
}

#[cfg(not(windows))]
fn system_locale() -> String {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|v| std::env::var(v).ok().filter(|s| !s.is_empty()))
        .unwrap_or_default()
}

type Table = HashMap<String, String>;

fn parse(text: &str) -> Table {
    serde_json::from_str::<HashMap<String, serde_json::Value>>(text.trim_start_matches('\u{feff}'))
        .expect("a language file")
        .into_iter()
        .filter_map(|(k, v)| v.as_str().map(|s| (k, s.to_owned())))
        .collect()
}

fn tables(language: Language) -> &'static [Table; 2] {
    static EN: OnceLock<[Table; 2]> = OnceLock::new();
    static PT: OnceLock<[Table; 2]> = OnceLock::new();
    match language {
        Language::English => EN.get_or_init(|| {
            [parse(include_str!("../lang/en.json")), parse(include_str!("../../../scripts/launcher/lang/en.json"))]
        }),
        Language::PortugueseBrazil => PT.get_or_init(|| {
            [parse(include_str!("../lang/pt-BR.json")), parse(include_str!("../../../scripts/launcher/lang/pt-BR.json"))]
        }),
    }
}

fn lookup(language: Language, key: &str) -> Option<&'static str> {
    tables(language).iter().find_map(|t| t.get(key)).map(String::as_str)
}

pub fn get(language: Language, key: &str) -> String {
    lookup(language, key)
        .or_else(|| lookup(Language::English, key))
        .map_or_else(|| key.to_owned(), str::to_owned)
}

/// A key's text with `{0}`, `{1}`... filled in.
pub fn fill(language: Language, key: &str, args: &[&str]) -> String {
    let mut out = get(language, key);
    for (i, a) in args.iter().enumerate() {
        out = out.replace(&format!("{{{i}}}"), a);
    }
    out
}

/// Whether a key has a text (an option's hint is optional).
pub fn has(language: Language, key: &str) -> bool {
    lookup(language, key).is_some() || lookup(Language::English, key).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every key of this launcher's own files is in both languages.
    #[test]
    fn both_languages_have_every_key() {
        let en = parse(include_str!("../lang/en.json"));
        let pt = parse(include_str!("../lang/pt-BR.json"));
        let mut missing: Vec<_> = en.keys().filter(|k| !pt.contains_key(*k)).collect();
        missing.extend(pt.keys().filter(|k| !en.contains_key(*k)));
        assert!(missing.is_empty(), "{missing:?}");
    }

    #[test]
    fn options_come_from_the_shared_files() {
        assert_eq!(get(Language::PortugueseBrazil, "group.image"), "Imagem");
        assert_eq!(fill(Language::English, "missing.key", &[]), "missing.key");
    }
}
