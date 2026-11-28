//! Preparing and starting a run. In a checkout of the repository the first
//! step builds `victory.exe` (cargo, the `victory` crate: the launcher and the
//! build stay one project); a bundle has it built already. Then the shaders
//! are translated when the build or a translation switch changed, the
//! pipelines a run wrote down are warmed, and the game is opened. What was
//! prepared is kept in `prepared.json`, in the PowerShell launcher's form, so
//! the two agree on what is left to do.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value};

use crate::flags::KEPT_PATHS;
use crate::options::{self, Run};
use crate::paths;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepId {
    Build,
    Shaders,
    Warm,
    Open,
}

impl StepId {
    pub fn key(self) -> &'static str {
        match self {
            Self::Build => "build",
            Self::Shaders => "shaders",
            Self::Warm => "warm",
            Self::Open => "open",
        }
    }
}

pub const LOG_STEPS: &[StepId] = &[StepId::Build, StepId::Shaders, StepId::Warm, StepId::Open];

#[derive(Debug, Clone)]
pub struct Step {
    pub id: StepId,
    pub needed: bool,
    /// Why it is needed or not (English: it goes to the log).
    pub reason: &'static str,
    pub args: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Plan {
    pub victory: PathBuf,
    /// Handed to the build as `VICTORY_GAME_DIR` unless that is already set
    /// (a bundle has no `victory.toml` naming it).
    pub game_dir: PathBuf,
    /// Where the saves go (`VICTORY_DATA_DIR`), when chosen in the settings.
    pub data_dir: Option<PathBuf>,
    /// The repository `victory.exe` is built in, when the launcher runs from
    /// a checkout of it.
    pub checkout: Option<PathBuf>,
    pub run: Run,
    pub runner: Vec<String>,
    pub steps: Vec<Step>,
    victory_hash: String,
    translation_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanProblem {
    NoVictory(Vec<PathBuf>),
    NoGame,
}

/// The checkout the launcher can build `victory.exe` in: the repository it
/// runs from, when `cargo` is there to build with.
pub fn checkout() -> Option<PathBuf> {
    let root = paths::repository_root(&paths::install_dir())?;
    let has_cargo = Command::new("cargo")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    has_cargo.then_some(root)
}

/// The cargo arguments that build `victory.exe` for a build name.
pub fn build_args(build: &str) -> Vec<String> {
    let args: &[&str] = if build == "diag" {
        &["build", "--locked", "--profile", "diag", "-p", "victory", "--features", "diagnostics"]
    } else {
        &["build", "--locked", "--release", "-p", "victory"]
    };
    args.iter().map(|s| (*s).to_owned()).collect()
}

/// What the stamp says about the shaders and the warm-up, for a build whose
/// `victory.exe` has this hash.
fn decide(build: &str, victory_hash: &str, translation_key: &str) -> [(bool, &'static str); 2] {
    let stamp = read_stamp();
    let entry = stamp.get(build).and_then(Value::as_object).cloned().unwrap_or_default();
    let text = |k: &str| entry.get(k).and_then(Value::as_str).map(str::to_owned);
    let number = |k: &str| entry.get(k).and_then(Value::as_i64);
    let shaders_reason = match text("victory_hash") {
        None => "never translated with this build",
        Some(h) if h != victory_hash => "victory.exe changed",
        _ if text("translation_key").as_deref() != Some(translation_key) => "a translation switch changed",
        _ => "up to date",
    };
    let need_shaders = shaders_reason != "up to date";
    let warm = match (number("last_run_at"), number("warm_at")) {
        (None, _) => (false, "no run has written its pipelines down yet"),
        (Some(_), _) if need_shaders => (true, "the shaders are translated again"),
        (Some(run_at), Some(warm_at)) if run_at <= warm_at => (false, "up to date"),
        _ => (true, "a run drew with pipelines since the last warm-up"),
    };
    [(need_shaders, shaders_reason), warm]
}

/// The steps a run takes now. With a checkout the build is made there first,
/// so a missing `victory.exe` is not a problem.
pub fn plan(
    victory: Option<PathBuf>,
    candidates: Vec<PathBuf>,
    checkout: Option<PathBuf>,
    game_exe: Option<PathBuf>,
    run: Run,
    runner: Option<&str>,
) -> Result<Plan, PlanProblem> {
    let built_here = checkout.as_ref().map(|root| root.join("target").join(&run.build).join(paths::VICTORY_EXE));
    let victory = victory.or(built_here).ok_or(PlanProblem::NoVictory(candidates))?;
    let game_exe = game_exe.ok_or(PlanProblem::NoGame)?;
    let victory_hash = cached_sha256(&victory).unwrap_or_default();
    let translation_key = options::translation_key(&run.env);
    let [(need_shaders, shaders_reason), (need_warm, warm_reason)] = decide(&run.build, &victory_hash, &translation_key);
    let game_path = crate::game::game_dir(&game_exe);
    let jobs = run.jobs.to_string();
    let steps = vec![
        Step {
            id: StepId::Build,
            needed: checkout.is_some(),
            reason: if checkout.is_some() { "built from the checkout the launcher runs in" } else { "a bundle's build" },
            args: build_args(&run.build),
        },
        Step {
            id: StepId::Shaders,
            needed: need_shaders,
            reason: shaders_reason,
            args: vec!["shaders".into(), game_path.display().to_string(), "--jobs".into(), jobs.clone()],
        },
        Step {
            id: StepId::Warm,
            needed: need_warm,
            reason: warm_reason,
            args: vec!["warm".into(), "--used".into(), "--jobs".into(), jobs],
        },
        Step {
            id: StepId::Open,
            needed: true,
            reason: "the run",
            args: vec!["run".into(), game_exe.display().to_string(), "--console".into(), run.console.clone(), "--execute".into()],
        },
    ];
    Ok(Plan {
        victory,
        game_dir: game_path,
        data_dir: None,
        checkout,
        runner: runner_command(runner),
        run,
        steps,
        victory_hash,
        translation_key,
    })
}

/// What starts `victory.exe`: nothing on Windows, `wine` elsewhere unless the
/// settings name another command.
pub fn runner_command(runner: Option<&str>) -> Vec<String> {
    let typed: Vec<String> = runner.unwrap_or("").split_whitespace().map(str::to_owned).collect();
    if !typed.is_empty() || cfg!(windows) {
        return typed;
    }
    vec!["wine".to_owned()]
}

/// The window asks for the plan every frame: a build's hash is kept while
/// its size and time stay the same.
fn cached_sha256(path: &Path) -> std::io::Result<String> {
    type Key = (PathBuf, u64, Option<std::time::SystemTime>);
    static CACHE: Mutex<Option<(Key, String)>> = Mutex::new(None);
    let meta = std::fs::metadata(path)?;
    let key: Key = (path.to_path_buf(), meta.len(), meta.modified().ok());
    if let Some((k, hash)) = CACHE.lock().unwrap().as_ref()
        && *k == key
    {
        return Ok(hash.clone());
    }
    let hash = file_sha256(path)?;
    *CACHE.lock().unwrap() = Some((key, hash.clone()));
    Ok(hash)
}

pub fn file_sha256(path: &Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)?;
    Ok(options::hex(&hasher.finalize()))
}

fn read_stamp() -> Map<String, Value> {
    std::fs::read_to_string(paths::stamp_path())
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(t.trim_start_matches('\u{feff}')).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_else(|| {
            let mut m = Map::new();
            m.insert("format".into(), Value::from(1));
            m
        })
}

fn write_stamp(build: &str, values: &[(&str, Value)]) {
    let mut stamp = read_stamp();
    let entry = stamp.entry(build.to_owned()).or_insert_with(|| Value::Object(Map::new()));
    if !entry.is_object() {
        *entry = Value::Object(Map::new());
    }
    if let Some(e) = entry.as_object_mut() {
        for (k, v) in values {
            e.insert((*k).to_owned(), v.clone());
        }
    }
    let path = paths::stamp_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = serde_json::to_string_pretty(&Value::Object(stamp)) {
        let partial = path.with_extension("json.partial");
        if std::fs::write(&partial, text).is_ok() {
            let _ = std::fs::rename(partial, path);
        }
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// The log file of a step.
pub fn log_path(id: StepId) -> PathBuf {
    paths::logs_dir().join(format!("{}.log", id.key()))
}

/// What the worker tells the window.
#[derive(Debug, Clone)]
pub enum Event {
    Started(StepId),
    Skipped(StepId),
    Done(StepId),
    Failed { step: StepId, code: Option<i32>, log: PathBuf },
    Cancelled(StepId),
    /// The game was opened; its process ended with this code.
    GameEnded(Option<i32>),
}

/// The process a step runs, shared with the window so Cancel can end it.
pub type Running = Arc<Mutex<Option<Child>>>;

/// Runs the plan's steps one after another on this thread. After a build the
/// shaders and the warm-up are decided again, from the new `victory.exe`.
pub fn execute(plan: &Plan, running: &Running, cancelled: &Arc<std::sync::atomic::AtomicBool>, events: &Sender<Event>) {
    use std::sync::atomic::Ordering;
    let _ = std::fs::create_dir_all(paths::logs_dir());
    let mut plan = plan.clone();
    for i in 0..plan.steps.len() {
        let step = plan.steps[i].clone();
        if cancelled.load(Ordering::SeqCst) {
            let _ = events.send(Event::Cancelled(step.id));
            return;
        }
        if !step.needed {
            let _ = events.send(Event::Skipped(step.id));
            continue;
        }
        let _ = events.send(Event::Started(step.id));
        let log = log_path(step.id);
        let child = match spawn(&plan, &step, &log) {
            Ok(c) => c,
            Err(e) => {
                let _ = std::fs::write(&log, format!("could not start the {} step: {e}\n", step.id.key()));
                let _ = events.send(Event::Failed { step: step.id, code: None, log });
                return;
            }
        };
        if step.id == StepId::Open {
            write_stamp(&plan.run.build, &[("last_run_at", Value::from(now()))]);
        }
        *running.lock().unwrap() = Some(child);
        let status = loop {
            let mut guard = running.lock().unwrap();
            let Some(child) = guard.as_mut() else { break None };
            match child.try_wait() {
                Ok(Some(status)) => {
                    *guard = None;
                    break Some(status);
                }
                Ok(None) => {}
                Err(_) => {
                    *guard = None;
                    break None;
                }
            }
            drop(guard);
            std::thread::sleep(std::time::Duration::from_millis(200));
        };
        if cancelled.load(Ordering::SeqCst) {
            let _ = events.send(Event::Cancelled(step.id));
            return;
        }
        let code = status.and_then(|s| s.code());
        if step.id == StepId::Open {
            let _ = events.send(Event::GameEnded(code));
            return;
        }
        if !status.is_some_and(|s| s.success()) {
            let _ = events.send(Event::Failed { step: step.id, code, log });
            return;
        }
        match step.id {
            StepId::Build => {
                plan.victory_hash = file_sha256(&plan.victory).unwrap_or_default();
                let decided = decide(&plan.run.build, &plan.victory_hash, &plan.translation_key);
                for (s, (needed, reason)) in plan.steps.iter_mut().filter(|s| matches!(s.id, StepId::Shaders | StepId::Warm)).zip(decided) {
                    s.needed = needed;
                    s.reason = reason;
                }
            }
            StepId::Shaders => write_stamp(
                &plan.run.build,
                &[
                    ("victory_hash", Value::from(plan.victory_hash.clone())),
                    ("translation_key", Value::from(plan.translation_key.clone())),
                    ("shaders_at", Value::from(now())),
                ],
            ),
            StepId::Warm => write_stamp(&plan.run.build, &[("warm_at", Value::from(now()))]),
            StepId::Open => {}
        }
        let _ = events.send(Event::Done(step.id));
    }
}

fn spawn(plan: &Plan, step: &Step, log: &Path) -> std::io::Result<Child> {
    let mut out = std::fs::File::create(log)?;
    let mut cmd = if step.id == StepId::Build {
        let root = plan.checkout.clone().unwrap_or_else(|| PathBuf::from("."));
        {
            use std::io::Write;
            writeln!(out, "# build: {}", step.reason)?;
            writeln!(out, "# cargo {} (in {})", step.args.join(" "), root.display())?;
        }
        let mut cmd = Command::new("cargo");
        cmd.args(&step.args).current_dir(root);
        cmd
    } else {
        {
            use std::io::Write;
            writeln!(out, "# {}: {}", step.id.key(), step.reason)?;
            writeln!(out, "# {} {}", plan.victory.display(), step.args.join(" "))?;
        }
        let (program, mut args): (PathBuf, Vec<String>) = match plan.runner.split_first() {
            Some((first, rest)) => {
                let mut a = rest.to_vec();
                a.push(plan.victory.display().to_string());
                (PathBuf::from(first), a)
            }
            None => (plan.victory.clone(), Vec::new()),
        };
        args.extend(step.args.iter().cloned());
        let mut cmd = Command::new(program);
        cmd.args(&args).current_dir(paths::run_dir(&plan.victory));
        apply_env(&mut cmd, &plan.run.env);
        if std::env::var_os("VICTORY_GAME_DIR").is_none() {
            cmd.env("VICTORY_GAME_DIR", &plan.game_dir);
        }
        // The saves folder chosen in the settings wins over one inherited.
        if let Some(dir) = &plan.data_dir {
            cmd.env("VICTORY_DATA_DIR", dir);
        }
        cmd
    };
    let err = out.try_clone()?;
    cmd.stdin(Stdio::null()).stdout(out).stderr(err);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: no console of its own; its lines go to the log.
        cmd.creation_flags(0x0800_0000);
    }
    cmd.spawn()
}

/// The run's switches, and no other `VICTORY_*` left over from where the
/// launcher was started, except the ones that say where the files are.
fn apply_env(cmd: &mut Command, env: &BTreeMap<String, String>) {
    for (name, _) in std::env::vars_os() {
        let name = name.to_string_lossy();
        if name.starts_with("VICTORY_") && !KEPT_PATHS.contains(&name.as_ref()) {
            cmd.env_remove(name.as_ref());
        }
    }
    cmd.envs(env);
}

/// Ends the step that is running, if any.
pub fn cancel(running: &Running) {
    if let Some(child) = running.lock().unwrap().as_mut() {
        let _ = child.kill();
    }
}

/// The end of a log, at most `limit` bytes, from a whole line on.
pub fn tail(path: &Path, limit: u64) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut file) = std::fs::File::open(path) else { return String::new() };
    let len = file.metadata().map_or(0, |m| m.len());
    let start = len.saturating_sub(limit);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut bytes = Vec::new();
    let _ = file.read_to_end(&mut bytes);
    let text = String::from_utf8_lossy(&bytes).into_owned();
    if start > 0 {
        text.split_once('\n').map_or(text.clone(), |(_, rest)| rest.to_owned())
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_runner_is_wine_off_windows_only() {
        if cfg!(windows) {
            assert!(runner_command(None).is_empty());
        } else {
            assert_eq!(runner_command(None), ["wine"]);
        }
        assert_eq!(runner_command(Some("proton run")), ["proton", "run"]);
    }

    #[test]
    fn a_checkout_builds_the_victory_crate_first() {
        assert_eq!(build_args("release"), ["build", "--locked", "--release", "-p", "victory"]);
        assert!(build_args("diag").contains(&"diagnostics".to_owned()));
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("game_scarlett_master.exe");
        crate::game::fake_exe(&exe);
        let run = options::run(&Map::new(), &[], "");
        let plan = plan(None, Vec::new(), Some(dir.path().to_path_buf()), Some(exe.clone()), run.clone(), None).unwrap();
        assert_eq!(plan.steps[0].id, StepId::Build);
        assert!(plan.steps[0].needed);
        assert_eq!(plan.victory, dir.path().join("target").join("release").join("victory.exe"));
        let bundle = plan_without_checkout(&exe, run);
        assert!(!bundle.steps[0].needed);
    }

    fn plan_without_checkout(exe: &Path, run: Run) -> Plan {
        plan(Some(exe.to_path_buf()), Vec::new(), None, Some(exe.to_path_buf()), run, None).unwrap()
    }

    #[test]
    fn tails_start_on_a_whole_line() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.log");
        std::fs::write(&p, "first line\nsecond line\nthird\n").unwrap();
        assert_eq!(tail(&p, 1000), "first line\nsecond line\nthird\n");
        assert_eq!(tail(&p, 14), "third\n");
    }
}
