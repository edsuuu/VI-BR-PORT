//! The options, read from the same `scripts/launcher/options.json` the
//! PowerShell launcher shows, and the run they add up to: the console, the
//! build, the jobs of the preparing steps and the switches of the run.
use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::flags::Flag;

const OPTIONS_JSON: &str = include_str!("../../../scripts/launcher/options.json");
const BASE_JSON: &str = include_str!("../base.json");

#[derive(Debug, Deserialize)]
pub struct Catalog {
    pub translation_switches: TranslationSwitches,
    pub options: Vec<Opt>,
}

#[derive(Debug, Deserialize)]
pub struct TranslationSwitches {
    pub names: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Toggle,
    Choice,
}

#[derive(Debug, Deserialize)]
pub struct Opt {
    pub id: String,
    pub kind: Kind,
    pub group: String,
    #[serde(default)]
    pub main: bool,
    #[serde(default = "yes")]
    pub available: bool,
    #[serde(default)]
    pub note: Option<String>,
    pub default: Value,
    #[serde(default)]
    pub values: Vec<ChoiceValue>,
    #[serde(default)]
    pub on: Effect,
    #[serde(default)]
    pub off: Effect,
    #[serde(default)]
    pub requires: Option<Requires>,
}

#[derive(Debug, Deserialize)]
pub struct ChoiceValue {
    pub value: String,
    #[serde(default = "yes")]
    pub available: bool,
    #[serde(default)]
    pub effect: Effect,
}

#[derive(Debug, Deserialize)]
pub struct Requires {
    pub option: String,
    pub value: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct Effect {
    #[serde(default)]
    pub play: Map<String, Value>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Deserialize)]
struct Base {
    base: BTreeMap<String, String>,
    fast: BTreeMap<String, String>,
    safe: BTreeMap<String, String>,
    ray_tracing: BTreeMap<String, String>,
    no_ray_tracing: BTreeMap<String, String>,
}

pub fn catalog() -> &'static Catalog {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    CATALOG.get_or_init(|| serde_json::from_str(OPTIONS_JSON).expect("scripts/launcher/options.json"))
}

fn base() -> &'static Base {
    static BASE: OnceLock<Base> = OnceLock::new();
    BASE.get_or_init(|| serde_json::from_str(BASE_JSON).expect("apps/vi-launcher/base.json"))
}

/// The switches every run starts from (base.json, scripts/play.ps1's list), in
/// name order: what the flags box shows before the player's own changes.
pub fn base_flags() -> Vec<(String, String)> {
    base().base.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
}

pub const GROUPS: &[&str] = &["profile", "image", "light", "performance", "diagnostics"];

/// Options this launcher does not show: the preparing steps' threads and the
/// card memory are set from the PC ([`crate::hardware`]), the detailed log is
/// always on, and frames are never captured from here.
pub const AUTOMATIC: &[&str] = &["jobs", "card_memory", "detailed_log", "capture_frames"];

impl Opt {
    /// The value in the settings, if it is one this option takes and can use
    /// on this branch; the default otherwise.
    pub fn value(&self, saved: &Map<String, Value>) -> Value {
        match saved.get(&self.id) {
            Some(v) if self.usable(v) => v.clone(),
            _ => self.default.clone(),
        }
    }

    pub fn usable(&self, value: &Value) -> bool {
        if !self.available {
            return false;
        }
        match self.kind {
            Kind::Toggle => value.is_boolean(),
            Kind::Choice => value
                .as_str()
                .is_some_and(|s| self.values.iter().any(|c| c.value == s && c.available)),
        }
    }

    fn effect(&self, value: &Value) -> Option<&Effect> {
        match self.kind {
            Kind::Toggle => Some(if value.as_bool() == Some(true) { &self.on } else { &self.off }),
            Kind::Choice => self.values.iter().find(|c| Some(c.value.as_str()) == value.as_str()).map(|c| &c.effect),
        }
    }
}

/// Sets an option, keeping the ties of `requires`: a toggle turned on sets the
/// value it needs, and another value of that option turns the toggle off.
pub fn set(saved: &mut Map<String, Value>, id: &str, value: Value) {
    let cat = catalog();
    let Some(opt) = cat.options.iter().find(|o| o.id == id) else { return };
    if !opt.usable(&value) {
        return;
    }
    saved.insert(id.to_owned(), value.clone());
    if let (Some(req), Some(true)) = (&opt.requires, value.as_bool()) {
        saved.insert(req.option.clone(), Value::String(req.value.clone()));
    }
    for other in &cat.options {
        if let Some(req) = &other.requires
            && req.option == id
            && value.as_str() != Some(req.value.as_str())
        {
            saved.insert(other.id.clone(), Value::Bool(false));
        }
    }
}

/// Everything a run needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub console: String,
    pub build: String,
    pub ray_tracing: bool,
    pub jobs: u32,
    /// The switches of the run, in name order. Names given an empty value are
    /// left out.
    pub env: BTreeMap<String, String>,
}

/// The run the settings and the typed switches add up to, in the order
/// `scripts/play.ps1` sets them: its base, the ray tracing pair, then the
/// options' own switches, then the typed ones (a later one wins).
pub fn run(saved: &Map<String, Value>, typed: &[Flag], capture_dir: &str) -> Run {
    let cat = catalog();
    let mut console = "series-s".to_owned();
    let mut build = "release".to_owned();
    let mut ray_tracing = false;
    // Set from the PC, not chosen: every CPU thread, and the card memory
    // from the card's size.
    let jobs = crate::hardware::threads();
    let mut more: Vec<(String, String)> = vec![
        ("VICTORY_MEMORY_CARD_MB".to_owned(), crate::hardware::card_memory_mb(crate::hardware::vram()).to_string()),
        // The detailed log, always (its debug lines exist in the diag build).
        ("VICTORY_LOG".to_owned(), "debug".to_owned()),
    ];
    for opt in &cat.options {
        if !opt.available || AUTOMATIC.contains(&opt.id.as_str()) {
            continue;
        }
        let value = opt.value(saved);
        let Some(effect) = opt.effect(&value) else { continue };
        if let Some(c) = effect.play.get("Console").and_then(Value::as_str) {
            console = c.to_owned();
        }
        if let Some(b) = effect.play.get("Build").and_then(Value::as_str) {
            build = b.to_owned();
        }
        if effect.play.get("RayTracing").and_then(Value::as_bool) == Some(true) {
            ray_tracing = true;
        }
        for (k, v) in &effect.env {
            more.push((k.clone(), v.replace("{capture_dir}", capture_dir)));
        }
    }
    for flag in typed.iter().filter(|f| f.enabled && crate::flags::check_name(&f.name).is_ok()) {
        more.push((flag.name.trim().to_owned(), flag.value.clone()));
    }

    let base = base();
    let mut env: BTreeMap<String, String> = base.base.clone();
    let safe = more.iter().any(|(k, v)| k == "VICTORY_SAFE" && v == "1");
    env.extend(if safe { base.safe.clone() } else { base.fast.clone() });
    env.extend(if ray_tracing { base.ray_tracing.clone() } else { base.no_ray_tracing.clone() });
    for (k, v) in more {
        env.insert(k, v);
    }
    env.retain(|_, v| !v.is_empty());
    Run { console, build, ray_tracing, jobs, env }
}

/// What the shader cache is keyed on: each translation switch with its value,
/// hashed as the PowerShell launcher does, so the two agree on when the
/// shaders are translated again.
pub fn translation_key(env: &BTreeMap<String, String>) -> String {
    use sha2::{Digest, Sha256};
    let mut text = String::new();
    for name in &catalog().translation_switches.names {
        let value = env.get(name).map_or("", String::as_str);
        text.push_str(&format!("{name}={value}\n"));
    }
    hex(&Sha256::digest(text.as_bytes()))
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_the_readme_run() {
        let r = run(&Map::new(), &[], "C:\\cap");
        assert_eq!(r.console, "series-s");
        assert_eq!(r.build, "release");
        assert!(!r.ray_tracing);
        assert_eq!(r.env["VICTORY_RT_MISS"], "1");
        assert_eq!(r.env["VICTORY_THIN"], "1");
        assert!(!r.env.contains_key("VICTORY_BVH_BUILDER"));
    }

    #[test]
    fn options_and_typed_switches() {
        let mut saved = Map::new();
        set(&mut saved, "ray_tracing", Value::Bool(true));
        // Chosen card memory is not used: it comes from the card.
        set(&mut saved, "card_memory", Value::String("2048".into()));
        let typed = [
            Flag { name: "VICTORY_RT_QUERY".into(), value: "a0".into(), enabled: true },
            Flag { name: "VICTORY_THIN".into(), value: String::new(), enabled: true },
            Flag { name: "VICTORY_OFF".into(), value: "1".into(), enabled: false },
        ];
        let r = run(&saved, &typed, "");
        assert!(r.ray_tracing);
        assert_eq!(r.env["VICTORY_BVH_BUILDER"], "1");
        assert!(!r.env.contains_key("VICTORY_RT_MISS"));
        let auto = crate::hardware::card_memory_mb(crate::hardware::vram()).to_string();
        assert_eq!(r.env["VICTORY_MEMORY_CARD_MB"], auto);
        assert_eq!(r.jobs, crate::hardware::threads());
        assert_eq!(r.env["VICTORY_RT_QUERY"], "a0");
        assert!(!r.env.contains_key("VICTORY_THIN"), "an empty value removes the switch");
        assert!(!r.env.contains_key("VICTORY_OFF"));
    }

    #[test]
    fn requires_ties_the_two_options() {
        let mut saved = Map::new();
        set(&mut saved, "capture_frames", Value::Bool(true));
        assert_eq!(saved["build"], "diag");
        set(&mut saved, "build", Value::String("release".into()));
        assert_eq!(saved["capture_frames"], false);
        assert_eq!(run(&saved, &[], "").build, "release");
    }

    #[test]
    fn unavailable_values_are_never_used() {
        let mut saved = Map::new();
        saved.insert("image".into(), Value::String("dlss".into()));
        assert!(!run(&saved, &[], "").env.contains_key("VICTORY_DLSS"));
    }

    #[test]
    fn the_key_changes_with_a_translation_switch_only() {
        let a = run(&Map::new(), &[], "");
        let mut b = a.env.clone();
        b.insert("VICTORY_THIN".into(), "0".into());
        assert_eq!(translation_key(&a.env), translation_key(&b));
        b.remove("VICTORY_RT_MISS");
        assert_ne!(translation_key(&a.env), translation_key(&b));
    }

    /// base.json is scripts/play.ps1's own list: a switch added there has to
    /// be added here.
    #[test]
    fn base_matches_play_ps1() {
        let play = include_str!("../../../scripts/play.ps1");
        let mut found = BTreeMap::new();
        // Every `VICTORY_NAME = 'value'` outside comments.
        for line in play.lines().filter(|l| !l.trim_start().starts_with('#')) {
            let mut rest = line;
            while let Some(at) = rest.find("VICTORY_") {
                rest = &rest[at..];
                let name_len = rest.bytes().take_while(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || *b == b'_').count();
                let (name, after) = rest.split_at(name_len);
                rest = after;
                if let Some(value) = after.strip_prefix(" = '")
                    && let Some(end) = value.find('\'')
                {
                    found.insert(name.to_owned(), value[..end].to_owned());
                }
            }
        }
        let b = base();
        let mut ours = b.base.clone();
        for m in [&b.fast, &b.safe, &b.ray_tracing, &b.no_ray_tracing] {
            ours.extend(m.clone());
        }
        assert_eq!(ours, found);
    }
}
