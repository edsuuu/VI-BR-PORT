//! The window, as the "Launcher" design draws it: a fixed 960 x 640 sunset
//! with four views. Play: the scene with the game's folder, the Play button
//! and its status on the left, the options panel on the right. Preparing: a
//! card in the middle with the steps of the run. Logs: what the build, the
//! shaders and the game print, live. Settings: one column of every option,
//! the typed `VICTORY_*` switches and the advanced settings.
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, FontFamily, FontId, Layout, Rect, RichText, UiBuilder, pos2, vec2};
use serde_json::Value;

use crate::config::{self, Config};
use crate::flags::{self, Flag};
use crate::game::{self, GameProblem};
use crate::options::{self, Kind, Opt};
use crate::paths;
use crate::run::{self, Event, Plan, PlanProblem, StepId};
use crate::text::{self, LANGUAGES, Language};
use crate::theme::{self, StepMark, colour, num};
use crate::update::{self, Found, Sources};

const PROJECT_URL: &str = "https://github.com/edsuuu/VI-BR-PORT";
/// How much of a log the Logs view shows.
const LOG_TAIL: u64 = 256 * 1024;

pub fn start() -> ExitCode {
    let size = [num("size", "windowWidth"), num("size", "windowHeight")];
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("VI-BR-PORT")
            .with_inner_size(size)
            .with_min_inner_size(size)
            .with_resizable(false)
            .with_maximize_button(false),
        ..Default::default()
    };
    match eframe::run_native("VI-BR-PORT", options, Box::new(|cc| Ok(Box::new(App::new(&cc.egui_ctx))))) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Play,
    Settings,
    Prep,
    Logs,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StepState {
    Waiting,
    Running,
    Done,
    Skipped,
    Failed,
    Cancelled,
}

struct Prep {
    steps: Vec<(StepId, StepState)>,
    events: Receiver<Event>,
    running: run::Running,
    cancelled: Arc<AtomicBool>,
    message: Option<String>,
    finished: bool,
    game_open: bool,
}

enum Update {
    Idle,
    Checking(Receiver<Result<Option<Found>, update::UpdateError>>),
    UpToDate,
    NoRelease,
    Available(Found),
    Downloading { found: Found, done: Arc<AtomicU64>, total: u64, result: Receiver<Result<PathBuf, String>> },
    Ready(PathBuf, Found),
    Installed,
    Failed(String),
}

/// The Logs view: which step's log, its text and when it was last read.
struct Logs {
    step: StepId,
    /// Follow the step that is running.
    follow: bool,
    text: String,
    read_at: Option<Instant>,
    /// Where Close goes back to.
    back: View,
}

pub struct App {
    language: Language,
    config: Config,
    config_path: PathBuf,
    /// The repository `victory.exe` is built in, when the launcher runs from
    /// a checkout with cargo.
    checkout: Option<PathBuf>,
    notice: Option<String>,
    view: View,
    view_since: Instant,
    opened: Instant,
    game: Option<Result<PathBuf, GameProblem>>,
    prep: Option<Prep>,
    update: Update,
    logs: Logs,
    new_flag: (String, String),
    pasted: String,
}

impl App {
    fn new(ctx: &egui::Context) -> Self {
        theme::apply(ctx);
        let config_path = paths::config_path();
        let loaded = config::load(&config_path);
        let language = Language::detect(loaded.config.language.as_deref());
        let notice = loaded.set_aside.map(|p| text::fill(language, "notice.set_aside", &[&p.display().to_string()]));
        let game = loaded.config.game_exe.as_deref().map(|p| game::find_game_exe(std::path::Path::new(p)));
        let mut app = Self {
            language,
            config: loaded.config,
            config_path,
            checkout: run::checkout(),
            notice,
            view: View::Play,
            view_since: Instant::now(),
            opened: Instant::now(),
            game,
            prep: None,
            update: Update::Idle,
            logs: Logs { step: StepId::Open, follow: true, text: String::new(), read_at: None, back: View::Play },
            new_flag: (String::new(), String::new()),
            pasted: String::new(),
        };
        if app.config.updates_on() {
            app.check_updates(ctx);
        }
        // VI_LAUNCHER_VIEW=settings|logs opens on that view (for screenshots
        // of the window without clicking through it).
        match std::env::var("VI_LAUNCHER_VIEW").as_deref() {
            Ok("settings") => app.view = View::Settings,
            Ok("logs") => app.view = View::Logs,
            _ => {}
        }
        app
    }

    fn t(&self, key: &str) -> String {
        text::get(self.language, key)
    }

    fn go(&mut self, view: View) {
        if self.view != view {
            if view == View::Logs {
                self.logs.back = if self.view == View::Logs { View::Play } else { self.view };
                self.logs.read_at = None;
            }
            self.view = view;
            self.view_since = Instant::now();
        }
    }

    fn save(&mut self) {
        if let Err(e) = config::save(&self.config_path, &self.config) {
            self.notice = Some(text::fill(self.language, "notice.not_saved", &[&e.to_string()]));
        }
    }

    fn check_updates(&mut self, ctx: &egui::Context) {
        if matches!(self.update, Update::Checking(_) | Update::Downloading { .. }) {
            return;
        }
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(update::check(&Sources::fork()));
            ctx.request_repaint();
        });
        self.update = Update::Checking(rx);
    }

    fn current_run(&self) -> options::Run {
        let capture = paths::data_dir().join("captures").display().to_string();
        options::run(&self.config.options, &self.config.flags, &capture)
    }

    fn make_plan(&self) -> Result<Plan, PlanProblem> {
        let run = self.current_run();
        let chosen = self.config.victory_exe.as_deref().map(std::path::Path::new);
        // A build named by hand is used as it is; else a checkout builds its own.
        let checkout = if chosen.is_some() { None } else { self.checkout.clone() };
        run::plan(
            paths::find_victory(chosen, &run.build),
            paths::victory_candidates(chosen, &run.build),
            checkout,
            self.game.as_ref().and_then(|g| g.as_ref().ok().cloned()),
            run,
            self.config.runner.as_deref(),
        )
    }

    fn play(&mut self, ctx: &egui::Context) {
        let Ok(plan) = self.make_plan() else { return };
        let (tx, rx) = channel();
        let running: run::Running = Arc::new(Mutex::new(None));
        let cancelled = Arc::new(AtomicBool::new(false));
        let steps = plan.steps.iter().map(|s| (s.id, StepState::Waiting)).collect();
        {
            let running = running.clone();
            let cancelled = cancelled.clone();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let (inner_tx, inner_rx) = channel();
                let worker = std::thread::spawn(move || run::execute(&plan, &running, &cancelled, &inner_tx));
                for e in inner_rx {
                    let _ = tx.send(e);
                    ctx.request_repaint();
                }
                let _ = worker.join();
                ctx.request_repaint();
            });
        }
        self.prep = Some(Prep { steps, events: rx, running, cancelled, message: None, finished: false, game_open: false });
        self.logs.follow = true;
        self.go(View::Prep);
    }

    fn poll(&mut self) {
        let language = self.language;
        if let Some(prep) = &mut self.prep {
            while let Ok(e) = prep.events.try_recv() {
                let set = |prep: &mut Prep, id: StepId, s: StepState| {
                    if let Some(entry) = prep.steps.iter_mut().find(|(i, _)| *i == id) {
                        entry.1 = s;
                    }
                };
                match e {
                    Event::Started(id) => {
                        set(prep, id, StepState::Running);
                        prep.game_open = id == StepId::Open;
                        if self.logs.follow {
                            self.logs.step = id;
                            self.logs.read_at = None;
                        }
                    }
                    Event::Skipped(id) => set(prep, id, StepState::Skipped),
                    Event::Done(id) => set(prep, id, StepState::Done),
                    Event::Failed { step, code, log } => {
                        set(prep, step, StepState::Failed);
                        let code = code.map_or_else(|| "-".to_owned(), |c| c.to_string());
                        prep.message = Some(text::fill(
                            language,
                            "prep.failed",
                            &[&text::get(language, &format!("prep.step.{}", step.key())), &code, &log.display().to_string()],
                        ));
                        prep.finished = true;
                        prep.game_open = false;
                    }
                    Event::Cancelled(id) => {
                        set(prep, id, StepState::Cancelled);
                        prep.finished = true;
                        prep.game_open = false;
                    }
                    Event::GameEnded(code) => {
                        set(prep, StepId::Open, StepState::Done);
                        prep.finished = true;
                        prep.game_open = false;
                        if let Some(c) = code.filter(|c| *c != 0) {
                            let log = run::log_path(StepId::Open);
                            prep.message = Some(text::fill(language, "prep.game_ended", &[&c.to_string(), &log.display().to_string()]));
                        }
                    }
                }
            }
        }
        let next = match &self.update {
            Update::Checking(rx) => rx.try_recv().ok().map(|r| match r {
                Ok(Some(found)) => Update::Available(found),
                Ok(None) => Update::UpToDate,
                Err(update::UpdateError::NoRelease) => Update::NoRelease,
                Err(e) => Update::Failed(e.to_string()),
            }),
            Update::Downloading { found, result, .. } => result.try_recv().ok().map(|r| match r {
                Ok(dir) => Update::Ready(dir, found.clone()),
                Err(e) => Update::Failed(e),
            }),
            _ => None,
        };
        if let Some(n) = next {
            self.update = n;
        }
    }

    fn busy(&self) -> bool {
        self.prep.as_ref().is_some_and(|p| !p.finished)
    }

    fn game_open(&self) -> bool {
        self.prep.as_ref().is_some_and(|p| p.game_open)
    }

    /// A short line on where the update check stands, if it says anything.
    fn update_state(&self) -> Option<(String, &'static str)> {
        let lang = self.language;
        match &self.update {
            Update::Checking(_) => Some((text::get(lang, "update.checking"), "textMuted")),
            Update::UpToDate => Some((text::get(lang, "update.none"), "textMuted")),
            Update::NoRelease => Some((text::get(lang, "update.no_release"), "textMuted")),
            Update::Failed(e) => Some((text::fill(lang, "update.failed", &[e]), "gold")),
            _ => None,
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll();
        let ctx = ui.ctx().clone();
        let full = ui.max_rect();
        let fade_ms = num("motion", "backgroundFadeMs").max(1.0);
        let fade_in = (self.opened.elapsed().as_secs_f32() * 1000.0 / fade_ms).min(1.0);
        theme::paint_scene(ui.painter(), full, ctx.input(|i| i.time), fade_in);
        let view_ms = num("motion", "viewFadeMs").max(1.0);
        let view_alpha = (self.view_since.elapsed().as_secs_f32() * 1000.0 / view_ms).min(1.0);
        ui.scope_builder(UiBuilder::new().max_rect(full), |ui| {
            ui.multiply_opacity(view_alpha);
            match self.view {
                View::Play => self.play_view(ui, full, &ctx),
                View::Settings => self.settings_view(ui, full, &ctx),
                View::Prep => self.prep_view(ui, full),
                View::Logs => self.logs_view(ui, full),
            }
        });
        // The sun's glow breathes and the logs follow the run; slower while
        // the game is open, unless its log is on screen.
        let wait = if fade_in < 1.0 || view_alpha < 1.0 {
            16
        } else if self.view == View::Logs {
            250
        } else if self.game_open() {
            1000
        } else {
            60
        };
        ctx.request_repaint_after(Duration::from_millis(wait));
    }
}

impl App {
    // ---- Play -------------------------------------------------------------

    fn play_view(&mut self, ui: &mut egui::Ui, full: Rect, ctx: &egui::Context) {
        let pad = num("layout", "viewPadding");
        let inner = full.shrink(pad);
        let panel_w = num("size", "panelWidth");
        let aside = Rect::from_min_max(pos2(inner.right() - panel_w, inner.top()), inner.max);
        let scene = Rect::from_min_max(inner.min, pos2(aside.left() - pad, inner.bottom()));

        ui.scope_builder(UiBuilder::new().max_rect(aside), |ui| {
            theme::panel().show(ui, |ui| {
                let pp = num("layout", "panelPadding");
                ui.set_min_size(aside.size() - vec2(pp * 2.0, pp * 2.0));
                self.options_panel(ui, ctx);
            });
        });

        // Above the scene: the game's folder and any notice, on dark panels.
        ui.scope_builder(UiBuilder::new().max_rect(scene).layout(Layout::top_down(egui::Align::Min)), |ui| {
            self.game_card(ui);
            self.notice_card(ui);
            self.update_card(ui, ctx);
        });

        // At the foot of the scene, over the sea: Play and its status.
        let plan = self.make_plan();
        let needs = |p: &Plan, id: StepId| p.steps.iter().any(|s| s.needed && s.id == id);
        let (status, dot) = match &plan {
            Ok(p) if needs(p, StepId::Build) && needs(p, StepId::Shaders) => (self.t("play.status.build_prepare"), "gold"),
            Ok(p) if needs(p, StepId::Build) => (self.t("play.status.build"), "gold"),
            Ok(p) if needs(p, StepId::Shaders) => (self.t("play.status.prepare"), "gold"),
            Ok(_) => (self.t("play.status.ready"), "gold"),
            Err(PlanProblem::NoGame) => (self.t("play.status.no_game"), "rose"),
            Err(PlanProblem::NoVictory(_)) => (self.t("build.missing_short"), "rose"),
        };
        let missing = match &plan {
            Err(PlanProblem::NoVictory(c)) => Some(text::fill(
                self.language,
                "build.missing",
                &[&c.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\n")],
            )),
            _ => None,
        };
        ui.scope_builder(UiBuilder::new().max_rect(scene).layout(Layout::bottom_up(egui::Align::Min)), |ui| {
            ui.spacing_mut().item_spacing.y = 14.0;
            let status_row = ui.horizontal(|ui| {
                ui.set_max_width(num("layout", "statusWidth"));
                theme::dot(ui, dot);
                ui.label(theme::small(&status, "textSoft"));
            });
            if let Some(m) = missing {
                status_row.response.on_hover_text(m);
            }
            let size = vec2(num("size", "playWidth"), num("size", "playHeight"));
            let font = FontId::new(theme::type_size("play"), FontFamily::Name("display".into()));
            let text = self.t("play.button").to_uppercase();
            if theme::gradient_button(ui, &text, size, font, true, true, plan.is_ok()).clicked() {
                self.play(ctx);
            }
        });
    }

    fn options_panel(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.spacing_mut().item_spacing.y = 18.0;
        ui.label(theme::title(&self.t("panel.title"), theme::type_size("titleSmall")));
        let mut toggles = Vec::new();
        for opt in options::catalog().options.iter().filter(|o| o.main) {
            match opt.kind {
                Kind::Choice => self.segmented(ui, opt),
                Kind::Toggle => toggles.push(opt),
            }
        }
        if !toggles.is_empty() {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                for opt in toggles {
                    self.switch(ui, opt, num("layout", "switchRowHeight"));
                }
            });
        }
        ui.with_layout(Layout::bottom_up(egui::Align::Min), |ui| {
            ui.spacing_mut().item_spacing.y = 10.0;
            ui.horizontal(|ui| {
                ui.set_min_height(num("layout", "linkRowHeight"));
                ui.hyperlink_to(RichText::new(self.t("panel.github")).size(theme::type_size("caption")).strong(), PROJECT_URL);
                ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(theme::small(&format!("v{}", update::current_version()), "textMuted").size(theme::type_size("caption")));
                });
            });
            if let Some((state, ink)) = self.update_state() {
                ui.add(egui::Label::new(theme::small(&state, ink).size(theme::type_size("caption"))).truncate());
            }
            let half = (ui.available_width() - 8.0) / 2.0;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                if theme::outline_button(ui, &self.t("panel.logs"), Some(half), false).clicked() {
                    self.go(View::Logs);
                }
                let checking = matches!(self.update, Update::Checking(_) | Update::Downloading { .. });
                let r = ui.add_enabled_ui(!checking, |ui| theme::outline_button(ui, &self.t("panel.check_updates"), Some(half), false)).inner;
                if r.clicked() {
                    self.check_updates(ctx);
                }
            });
            if theme::outline_button(ui, &self.t("panel.more"), Some(ui.available_width()), true).clicked() {
                self.go(View::Settings);
            }
        });
    }

    /// A choice as segments, two to a row, under its caption.
    fn segmented(&mut self, ui: &mut egui::Ui, opt: &'static Opt) {
        let lang = self.language;
        let value = opt.value(&self.config.options);
        let mut changed = None;
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
            ui.label(theme::caption(&text::get(lang, &format!("option.{}.label", opt.id))));
            let w = (ui.available_width() - 8.0) / 2.0;
            for pair in opt.values.chunks(2) {
                ui.horizontal(|ui| {
                    for c in pair {
                        let shown = value_text(lang, &opt.id, &c.value);
                        let chosen = value.as_str() == Some(c.value.as_str());
                        let r = ui.add_enabled_ui(opt.available && c.available, |ui| theme::segment(ui, &shown, chosen, w)).inner;
                        let r = if !c.available { r.on_disabled_hover_text(text::get(lang, "common.unavailable")) } else { r };
                        if r.clicked() {
                            changed = Some(Value::String(c.value.clone()));
                        }
                    }
                });
            }
        });
        if let Some(v) = changed {
            options::set(&mut self.config.options, &opt.id, v);
            self.save();
        }
    }

    /// A toggle as a whole clickable row with its switch.
    fn switch(&mut self, ui: &mut egui::Ui, opt: &'static Opt, height: f32) {
        let lang = self.language;
        let label = text::get(lang, &format!("option.{}.label", opt.id));
        let hint = if !opt.available {
            text::get(lang, "common.unavailable")
        } else {
            let k = format!("option.{}.hint", opt.id);
            if text::has(lang, &k) { text::get(lang, &k) } else { String::new() }
        };
        let mut on = opt.value(&self.config.options).as_bool().unwrap_or(false);
        let r = ui.add_enabled_ui(opt.available, |ui| theme::switch_row(ui, &label, &hint, &mut on, height)).inner;
        if let Some(note) = &opt.note {
            r.on_disabled_hover_text(note.as_str());
        }
        if on != opt.value(&self.config.options).as_bool().unwrap_or(false) {
            options::set(&mut self.config.options, &opt.id, Value::Bool(on));
            self.save();
        }
    }

    fn game_card(&mut self, ui: &mut egui::Ui) {
        let lang = self.language;
        theme::panel().inner_margin(16.0).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.label(theme::caption(&self.t("game.title")));
            let (line, colour_name) = match &self.game {
                Some(Ok(exe)) => (text::fill(lang, "game.ok", &[&exe.display().to_string()]), "textSoft"),
                Some(Err(p)) => (p.describe(lang), "gold"),
                None => (self.t("game.none"), "textSoft"),
            };
            ui.add(egui::Label::new(theme::small(&line, colour_name)).truncate());
            ui.horizontal(|ui| {
                if theme::outline_button(ui, &self.t("game.choose"), None, false).clicked()
                    && let Some(dir) = rfd::FileDialog::new().set_title(self.t("game.choose")).pick_folder()
                {
                    self.pick_game(dir);
                }
                if theme::outline_button(ui, &self.t("game.choose_exe"), None, false).clicked()
                    && let Some(file) = rfd::FileDialog::new().add_filter("exe", &["exe"]).pick_file()
                {
                    self.pick_game(file);
                }
            });
        });
    }

    fn notice_card(&mut self, ui: &mut egui::Ui) {
        let Some(n) = self.notice.clone() else { return };
        ui.add_space(8.0);
        theme::panel().inner_margin(14.0).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.add(egui::Label::new(theme::small(&n, "textSoft")).wrap());
                if theme::close_button(ui, &text::get(self.language, "common.close")).clicked() {
                    self.notice = None;
                }
            });
        });
    }

    fn update_card(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let lang = self.language;
        let (message, button) = match &self.update {
            Update::Available(f) => (text::fill(lang, "update.available", &[&f.manifest.version, f.source]), Some("update.download")),
            Update::Downloading { done, total, .. } => {
                let pct = if *total == 0 { 0 } else { done.load(Ordering::Relaxed) * 100 / total };
                (text::fill(lang, "update.downloading", &[&pct.to_string()]), None)
            }
            Update::Ready(_, f) => (text::fill(lang, "update.ready", &[&f.manifest.version]), Some("update.install")),
            Update::Installed => (self.t("update.installed"), Some("update.restart")),
            _ => return,
        };
        ui.add_space(8.0);
        let mut clicked = false;
        theme::panel().inner_margin(14.0).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                theme::dot(ui, "accent");
                ui.label(theme::small(&message, "text"));
                if let Some(key) = button {
                    ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                        clicked = ui.add_enabled_ui(!self.busy(), |ui| theme::outline_button(ui, &text::get(lang, key), None, false)).inner.clicked();
                    });
                }
            });
        });
        if !clicked {
            return;
        }
        match &self.update {
            Update::Available(found) => {
                let found = found.clone();
                let done = Arc::new(AtomicU64::new(0));
                let total = found.manifest.assets.get(update::platform()).map_or(0, |a| a.size);
                let (tx, rx) = channel();
                let (d, m, c) = (done.clone(), found.manifest.clone(), ctx.clone());
                std::thread::spawn(move || {
                    let progress = |n: u64, _| {
                        d.store(n, Ordering::Relaxed);
                        c.request_repaint();
                    };
                    let _ = tx.send(update::download(&Sources::fork(), &m, &progress).map_err(|e| e.to_string()));
                    c.request_repaint();
                });
                self.update = Update::Downloading { found, done, total, result: rx };
            }
            Update::Ready(dir, _) => {
                self.update = match update::install(dir, &paths::install_dir()) {
                    Ok(()) => Update::Installed,
                    Err(e) => Update::Failed(e.to_string()),
                };
            }
            Update::Installed => {
                if let Ok(exe) = std::env::current_exe()
                    && std::process::Command::new(exe).spawn().is_ok()
                {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            _ => {}
        }
    }

    fn pick_game(&mut self, chosen: PathBuf) {
        let found = game::find_game_exe(&chosen);
        if let Ok(exe) = &found {
            self.config.game_exe = Some(exe.display().to_string());
            self.save();
        }
        self.game = Some(found);
    }

    // ---- Preparing --------------------------------------------------------

    fn prep_view(&mut self, ui: &mut egui::Ui, full: Rect) {
        let lang = self.language;
        let Some(prep) = &self.prep else {
            self.go(View::Play);
            return;
        };
        let w = num("layout", "cardWidth");
        let card = Rect::from_center_size(full.center(), vec2(w, full.height() - 2.0 * num("layout", "viewPadding")));
        let mut back = false;
        let mut cancel = false;
        let mut logs = false;
        ui.scope_builder(UiBuilder::new().max_rect(card).layout(Layout::top_down(egui::Align::Min)), |ui| {
            ui.with_layout(Layout::centered_and_justified(egui::Direction::TopDown), |ui| {
                theme::card().inner_margin(num("layout", "cardPadding")).show(ui, |ui| {
                    ui.set_width(w - 2.0 * num("layout", "cardPadding"));
                    ui.spacing_mut().item_spacing.y = 20.0;
                    ui.with_layout(Layout::top_down(egui::Align::Min), |ui| {
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 8.0;
                            let title = if prep.game_open { "prep.game_open" } else { "prep.title" };
                            ui.label(theme::title(&text::get(lang, title), theme::type_size("titleLarge")));
                            ui.add(egui::Label::new(theme::small(&text::get(lang, "prep.subtitle"), "textSoft").size(theme::type_size("body"))).wrap());
                        });
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 4.0;
                            for (id, state) in &prep.steps {
                                step_row(ui, lang, *id, *state);
                            }
                        });
                        let done = prep.steps.iter().filter(|(_, s)| matches!(s, StepState::Done | StepState::Skipped)).count();
                        let running = prep.steps.iter().any(|(_, s)| *s == StepState::Running);
                        let fraction = (done as f32 + if running { 0.5 } else { 0.0 }) / prep.steps.len().max(1) as f32;
                        theme::progress(ui, fraction);
                        if let Some(m) = &prep.message {
                            ui.horizontal(|ui| {
                                theme::dot(ui, "rose");
                                ui.add(egui::Label::new(theme::small(m, "text")).wrap());
                            });
                        }
                        ui.horizontal(|ui| {
                            logs = theme::outline_button(ui, &text::get(lang, "prep.see_logs"), None, false).clicked();
                            ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                                if prep.finished {
                                    back = theme::outline_button(ui, &text::get(lang, "prep.back"), None, false).clicked();
                                } else {
                                    let key = if prep.game_open { "prep.close_game" } else { "prep.cancel" };
                                    cancel = theme::outline_button(ui, &text::get(lang, key), None, false).clicked();
                                }
                            });
                        });
                        if !prep.finished {
                            ui.label(theme::small(&text::get(lang, "prep.opens_by_itself"), "textSoft"));
                        }
                    });
                });
            });
        });
        if cancel && let Some(prep) = &self.prep {
            prep.cancelled.store(true, Ordering::SeqCst);
            run::cancel(&prep.running);
        }
        if logs {
            self.go(View::Logs);
        }
        if back {
            self.prep = None;
            self.go(View::Play);
        }
    }

    // ---- Logs -------------------------------------------------------------

    fn logs_view(&mut self, ui: &mut egui::Ui, full: Rect) {
        let lang = self.language;
        if self.logs.read_at.is_none_or(|t| t.elapsed() >= Duration::from_millis(250)) {
            self.logs.text = run::tail(&run::log_path(self.logs.step), LOG_TAIL);
            self.logs.read_at = Some(Instant::now());
        }
        let mut close = false;
        self.big_card(ui, full, &text::get(lang, "logs.title"), &mut close, |app, ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                for id in run::LOG_STEPS {
                    let exists = run::log_path(*id).is_file();
                    let name = text::get(lang, &format!("prep.step.{}", id.key()));
                    let r = ui.add_enabled_ui(exists, |ui| theme::segment(ui, &name, app.logs.step == *id, 150.0)).inner;
                    if r.clicked() {
                        app.logs.step = *id;
                        app.logs.follow = false;
                        app.logs.read_at = None;
                    }
                }
            });
            let h = ui.available_height() - num("size", "controlHeight") - 20.0;
            egui::Frame::new().fill(colour("night")).stroke(egui::Stroke::new(1.0, colour("stroke"))).corner_radius(theme::radius("control")).inner_margin(12.0).show(ui, |ui| {
                ui.set_min_size(vec2(ui.available_width(), h - 24.0));
                egui::ScrollArea::both().max_height(h - 24.0).stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
                    let shown = if app.logs.text.is_empty() { text::get(lang, "logs.empty") } else { app.logs.text.clone() };
                    ui.add(egui::Label::new(RichText::new(shown).monospace().color(colour("textSoft"))).extend());
                });
            });
        }, |app, ui| {
            let path = run::log_path(app.logs.step);
            if theme::outline_button(ui, &text::get(lang, "logs.open_folder"), None, false).clicked() {
                open_folder(&paths::logs_dir());
            }
            if theme::outline_button(ui, &text::get(lang, "logs.copy"), None, false).clicked() {
                ui.ctx().copy_text(app.logs.text.clone());
            }
            ui.add(egui::Label::new(theme::small(&path.display().to_string(), "textMuted").size(theme::type_size("caption"))).truncate());
        });
        if close {
            let back = if self.logs.back == View::Prep && self.prep.is_none() { View::Play } else { self.logs.back };
            self.go(back);
        }
    }

    /// A large card over the whole window: its title with a close button,
    /// a body, and a row of buttons at its foot.
    fn big_card(
        &mut self,
        ui: &mut egui::Ui,
        full: Rect,
        title: &str,
        close: &mut bool,
        body: impl FnOnce(&mut Self, &mut egui::Ui),
        foot: impl FnOnce(&mut Self, &mut egui::Ui),
    ) {
        let card = full.shrink(num("layout", "moreMargin"));
        let (px, py) = (num("layout", "morePaddingX"), num("layout", "morePaddingY"));
        ui.scope_builder(UiBuilder::new().max_rect(card), |ui| {
            theme::card().inner_margin(egui::Margin::symmetric(px as i8, py as i8)).show(ui, |ui| {
                ui.set_min_size(card.size() - vec2(2.0 * px, 2.0 * py));
                ui.spacing_mut().item_spacing.y = 20.0;
                ui.horizontal(|ui| {
                    ui.label(theme::title(title, theme::type_size("title")));
                    ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                        let r = theme::close_button(ui, &text::get(self.language, "common.close"));
                        if r.clicked() {
                            *close = true;
                        }
                    });
                });
                let foot_h = num("size", "controlHeight");
                let body_h = ui.available_height() - foot_h - 20.0;
                ui.allocate_ui_with_layout(vec2(ui.available_width(), body_h), Layout::top_down(egui::Align::Min), |ui| {
                    ui.set_min_height(body_h);
                    body(self, ui);
                });
                ui.horizontal(|ui| foot(self, ui));
            });
        });
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            *close = true;
        }
    }

    // ---- Settings ---------------------------------------------------------

    fn settings_view(&mut self, ui: &mut egui::Ui, full: Rect, ctx: &egui::Context) {
        let lang = self.language;
        let mut close = false;
        let mut save_back = false;
        let mut reset = false;
        self.big_card(ui, full, &text::get(lang, "more.title"), &mut close, |app, ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = num("layout", "sectionGap");
                // One section under the other.
                for g in options::GROUPS {
                    app.group_section(ui, g);
                }
                app.flags_section(ui);
                app.advanced_section(ui, ctx);
            });
        }, |app, ui| {
            reset = theme::outline_button(ui, &text::get(app.language, "more.reset"), None, false).clicked();
            ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                let text = text::get(app.language, "more.back");
                let font = FontId::new(theme::type_size("body"), FontFamily::Name("semibold".into()));
                let w = ui.painter().layout_no_wrap(text.clone(), font.clone(), colour("onAccent")).size().x + 44.0;
                if theme::gradient_button(ui, &text, vec2(w, num("size", "controlHeight")), font, false, false, true).clicked() {
                    save_back = true;
                }
            });
        });
        if reset {
            self.config.options.clear();
            self.save();
        }
        if close || save_back {
            self.go(View::Play);
        }
    }

    fn group_section(&mut self, ui: &mut egui::Ui, group: &str) {
        let opts: Vec<&'static Opt> = options::catalog().options.iter().filter(|o| o.group == group).collect();
        if opts.is_empty() {
            return;
        }
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.label(theme::caption(&self.t(&format!("group.{group}"))));
            ui.add_space(4.0);
            for opt in opts {
                let h = num("layout", "rowHeight");
                let rect = match opt.kind {
                    Kind::Toggle => {
                        let before = ui.cursor().min;
                        self.switch(ui, opt, h);
                        Rect::from_min_max(before, pos2(before.x + ui.available_width(), before.y + h))
                    }
                    Kind::Choice => self.choice_row(ui, opt, h),
                };
                theme::row_line(ui, rect);
            }
        });
    }

    /// A choice in the settings: label (and hint) left, a list on the right.
    fn choice_row(&mut self, ui: &mut egui::Ui, opt: &'static Opt, h: f32) -> Rect {
        let lang = self.language;
        let label = text::get(lang, &format!("option.{}.label", opt.id));
        let hint_key = format!("option.{}.hint", opt.id);
        let hint = if !opt.available {
            text::get(lang, "common.unavailable")
        } else if text::has(lang, &hint_key) {
            text::get(lang, &hint_key)
        } else {
            String::new()
        };
        let value = opt.value(&self.config.options);
        let current = value.as_str().unwrap_or("").to_owned();
        let mut changed = None;
        let r = ui.allocate_ui_with_layout(vec2(ui.available_width(), h), Layout::left_to_right(egui::Align::Center), |ui| {
            ui.set_min_height(h);
            ui.add_enabled_ui(opt.available, |ui| {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.set_max_width(ui.available_width() - num("layout", "selectWidth") - 12.0);
                    ui.label(theme::label(&label));
                    if !hint.is_empty() {
                        ui.add(egui::Label::new(theme::small(&hint, "textMuted").size(theme::type_size("caption"))).truncate());
                    }
                });
                ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                    egui::ComboBox::from_id_salt(format!("option-{}", opt.id))
                        .width(num("layout", "selectWidth"))
                        .height(400.0)
                        .selected_text(value_text(lang, &opt.id, &current))
                        .show_ui(ui, |ui| {
                            for c in &opt.values {
                                let shown = value_text(lang, &opt.id, &c.value);
                                let r = ui.add_enabled(c.available, egui::Button::selectable(current == c.value, shown));
                                if r.clicked() {
                                    changed = Some(Value::String(c.value.clone()));
                                }
                            }
                        });
                });
            });
        });
        if let Some(v) = changed {
            options::set(&mut self.config.options, &opt.id, v);
            self.save();
        }
        r.response.rect
    }

    fn flags_section(&mut self, ui: &mut egui::Ui) {
        let lang = self.language;
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.label(theme::caption(&self.t("flags.title")));
            ui.add(egui::Label::new(theme::small(&self.t("flags.hint"), "textMuted")).wrap());
            let mut remove = None;
            let mut dirty = false;
            let field_w = (ui.available_width() - 150.0) / 2.0;
            for (i, f) in self.config.flags.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    let sw = vec2(num("size", "switchWidth"), num("size", "switchHeight"));
                    ui.allocate_ui(vec2(sw.x + 8.0, num("size", "controlHeight")), |ui| {
                        dirty |= theme::switch_row(ui, "", "", &mut f.enabled, num("size", "controlHeight")).changed();
                    });
                    let r = ui.add(egui::TextEdit::singleline(&mut f.name).desired_width(field_w).font(egui::TextStyle::Monospace).margin(vec2(10.0, 12.0)));
                    dirty |= r.changed();
                    if let Err(p) = flags::check_name(&f.name) {
                        r.on_hover_text(text::get(lang, p.key()));
                    }
                    dirty |= ui
                        .add(egui::TextEdit::singleline(&mut f.value).desired_width(field_w).font(egui::TextStyle::Monospace).margin(vec2(10.0, 12.0)))
                        .changed();
                    if theme::close_button(ui, &text::get(lang, "flags.remove")).clicked() {
                        remove = Some(i);
                    }
                });
            }
            if let Some(i) = remove {
                self.config.flags.remove(i);
                dirty = true;
            }
            ui.horizontal(|ui| {
                ui.add_space(num("size", "switchWidth") + 16.0);
                ui.add(egui::TextEdit::singleline(&mut self.new_flag.0).hint_text("VICTORY_...").desired_width(field_w).font(egui::TextStyle::Monospace).margin(vec2(10.0, 12.0)));
                ui.add(
                    egui::TextEdit::singleline(&mut self.new_flag.1)
                        .hint_text(text::get(lang, "flags.value"))
                        .desired_width(field_w)
                        .font(egui::TextStyle::Monospace)
                        .margin(vec2(10.0, 12.0)),
                );
                let name = self.new_flag.0.trim().to_ascii_uppercase();
                let check = flags::check_name(&name);
                let add = ui.add_enabled_ui(check.is_ok(), |ui| theme::outline_button(ui, &text::get(lang, "flags.add"), None, false)).inner;
                if add.clicked() {
                    self.config.flags.retain(|f| f.name != name);
                    self.config.flags.push(Flag { name, value: self.new_flag.1.trim().to_owned(), enabled: true });
                    self.new_flag = (String::new(), String::new());
                    dirty = true;
                }
            });
            if let Err(p) = flags::check_name(self.new_flag.0.trim())
                && !self.new_flag.0.trim().is_empty()
            {
                ui.label(theme::small(&text::get(lang, p.key()), "gold"));
            }
            egui::CollapsingHeader::new(theme::small(&text::get(lang, "flags.paste"), "textSoft")).id_salt("paste").show(ui, |ui| {
                ui.add(egui::TextEdit::multiline(&mut self.pasted).desired_rows(3).desired_width(f32::INFINITY).font(egui::TextStyle::Monospace));
                if theme::outline_button(ui, &text::get(lang, "flags.paste_add"), None, false).clicked() {
                    for f in flags::parse_pasted(&self.pasted) {
                        self.config.flags.retain(|o| o.name != f.name);
                        self.config.flags.push(f);
                    }
                    self.pasted.clear();
                    dirty = true;
                }
            });
            egui::CollapsingHeader::new(theme::small(&text::get(lang, "flags.preview"), "textSoft")).id_salt("preview").show(ui, |ui| {
                let mut shown = self.current_run().env.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("\n");
                ui.add(egui::TextEdit::multiline(&mut shown).desired_rows(8).desired_width(f32::INFINITY).font(egui::TextStyle::Monospace));
            });
            if dirty {
                self.save();
            }
        });
    }

    fn advanced_section(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let lang = self.language;
        let h = num("layout", "rowHeight");
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.label(theme::caption(&self.t("advanced.title")));
            ui.add_space(4.0);

            let r = ui.allocate_ui_with_layout(vec2(ui.available_width(), h), Layout::left_to_right(egui::Align::Center), |ui| {
                ui.set_min_height(h);
                ui.label(theme::label(&text::get(lang, "advanced.language")));
                ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                    for l in LANGUAGES.iter().rev() {
                        if theme::segment(ui, &text::get(*l, "language.name"), *l == self.language, 170.0).clicked() {
                            self.language = *l;
                            self.config.language = Some(l.code().to_owned());
                            self.save();
                        }
                    }
                });
            });
            theme::row_line(ui, r.response.rect);

            let r = ui.allocate_ui_with_layout(vec2(ui.available_width(), h), Layout::left_to_right(egui::Align::Center), |ui| {
                ui.set_min_height(h);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(theme::label(&text::get(lang, "advanced.victory")));
                    let how = match &self.checkout {
                        Some(root) if self.config.victory_exe.is_none() => text::fill(lang, "advanced.victory.checkout", &[&root.display().to_string()]),
                        _ => text::get(lang, "advanced.victory.hint"),
                    };
                    ui.add(egui::Label::new(theme::small(&how, "textMuted").size(theme::type_size("caption"))).truncate());
                });
                ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                    if theme::outline_button(ui, "…", Some(44.0), false).clicked()
                        && let Some(file) = rfd::FileDialog::new().add_filter("victory", &["exe"]).pick_file()
                    {
                        self.config.victory_exe = Some(file.display().to_string());
                        self.save();
                    }
                    let mut path = self.config.victory_exe.clone().unwrap_or_default();
                    let edit = egui::TextEdit::singleline(&mut path)
                        .hint_text(text::get(lang, "advanced.victory.auto"))
                        .desired_width(300.0)
                        .margin(vec2(10.0, 12.0));
                    if ui.add(edit).changed() {
                        self.config.victory_exe = Some(path.trim().to_owned()).filter(|p| !p.is_empty());
                        self.save();
                    }
                });
            });
            theme::row_line(ui, r.response.rect);

            let r = ui.allocate_ui_with_layout(vec2(ui.available_width(), h), Layout::left_to_right(egui::Align::Center), |ui| {
                ui.set_min_height(h);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(theme::label(&text::get(lang, "advanced.runner")));
                    ui.label(theme::small(&text::get(lang, "advanced.runner.hint"), "textMuted").size(theme::type_size("caption")));
                });
                ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                    let mut runner = self.config.runner.clone().unwrap_or_default();
                    let hint = if cfg!(windows) { "" } else { "wine" };
                    let edit = egui::TextEdit::singleline(&mut runner).hint_text(hint).desired_width(num("layout", "selectWidth")).margin(vec2(10.0, 12.0));
                    if ui.add(edit).changed() {
                        self.config.runner = Some(runner.trim().to_owned()).filter(|p| !p.is_empty());
                        self.save();
                    }
                });
            });
            theme::row_line(ui, r.response.rect);

            let r = ui.allocate_ui_with_layout(vec2(ui.available_width(), h), Layout::left_to_right(egui::Align::Center), |ui| {
                ui.set_min_height(h);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(theme::label(&text::get(lang, "advanced.updates_now")));
                    if let Some((state, ink)) = self.update_state() {
                        ui.add(egui::Label::new(theme::small(&state, ink).size(theme::type_size("caption"))).truncate());
                    }
                });
                ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                    let checking = matches!(self.update, Update::Checking(_) | Update::Downloading { .. });
                    let r = ui.add_enabled_ui(!checking, |ui| theme::outline_button(ui, &text::get(lang, "advanced.check_now"), None, false)).inner;
                    if r.clicked() {
                        self.check_updates(ctx);
                    }
                });
            });
            theme::row_line(ui, r.response.rect);

            let before = ui.cursor().min;
            let mut on = self.config.updates_on();
            if theme::switch_row(ui, &text::get(lang, "advanced.updates"), "", &mut on, h).changed() {
                self.config.check_updates = Some(on);
                self.save();
            }
            theme::row_line(ui, Rect::from_min_max(before, pos2(before.x + ui.available_width(), before.y + h)));
            ui.add_space(10.0);
            ui.label(theme::small(&text::fill(lang, "advanced.data", &[&paths::data_dir().display().to_string()]), "textMuted"));
        });
    }
}

/// The value of a choice as the language files name it, else as it is.
fn value_text(lang: Language, id: &str, value: &str) -> String {
    let k = format!("option.{id}.value.{value}");
    if text::has(lang, &k) { text::get(lang, &k) } else { value.to_owned() }
}

/// Opens a folder in the system's file manager.
fn open_folder(dir: &std::path::Path) {
    let _ = std::fs::create_dir_all(dir);
    let program = if cfg!(windows) { "explorer" } else { "xdg-open" };
    let _ = std::process::Command::new(program).arg(dir).spawn();
}

fn step_row(ui: &mut egui::Ui, lang: Language, id: StepId, state: StepState) {
    let (mark, state_key) = match state {
        StepState::Waiting => (StepMark::Waiting, "prep.state.waiting"),
        StepState::Running => (StepMark::Current, "prep.state.running"),
        StepState::Done => (StepMark::Done, "prep.state.done"),
        StepState::Skipped => (StepMark::Skipped, "prep.state.skipped"),
        StepState::Failed => (StepMark::Failed, "prep.state.failed"),
        StepState::Cancelled => (StepMark::Skipped, "prep.state.cancelled"),
    };
    let current = state == StepState::Running;
    let ink = match state {
        StepState::Waiting | StepState::Skipped | StepState::Cancelled => "textMuted",
        StepState::Running => "text",
        _ => "textSoft",
    };
    ui.allocate_ui_with_layout(vec2(ui.available_width(), num("layout", "stepRowHeight")), Layout::left_to_right(egui::Align::Center), |ui| {
        ui.set_min_height(num("layout", "stepRowHeight"));
        ui.spacing_mut().item_spacing.x = 12.0;
        theme::step_mark(ui, mark);
        let name = text::get(lang, &format!("prep.step.{}", id.key()));
        let name = if current { theme::label(&name) } else { RichText::new(name).color(colour(ink)) };
        ui.label(name);
        ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
            let s = theme::small(&text::get(lang, state_key), ink);
            ui.label(if current { s.strong() } else { s });
            if current {
                ui.spinner();
            }
        });
    });
}
