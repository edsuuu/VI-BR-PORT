//! Keeping the title on its own loading screen while our side catches up
//! (`VICTORY_LOAD_HOLD=<max seconds>`; off unless set).
//!
//! On the console the data a load streams in is on the card the moment its
//! DirectStorage request completes. Here it is not: the texture views the
//! title writes as each request completes still have to be uploaded, and
//! the card's new memory waited for. The title leaves its loading screen
//! once its requests have completed, which on our side is long before the
//! card holds what the scene needs, so the first seconds of the scene ran
//! at 2-7 fps with low or broken textures (run intro-fim-1: 26,516
//! texture slots waiting, 8,800 waits for new card memory, in the ten
//! seconds after the world's first frame).
//!
//! So, while the title shows few draws a frame (its loading screen: fewer
//! than the world's 500) and our side is behind, DirectStorage holds back
//! the status entries the title polls for completion
//! (`victory_hle::modules::dstorage`), up to the max seconds. The reads
//! themselves run on: only their completion is told later. The title's
//! frames are never held; only what it is told of its loads. While held,
//! the pass that uploads the textures the title picks by index gets a
//! larger budget a frame ([`early_texture_budget`]), so the backlog
//! shrinks behind the loading screen instead of in the scene.
//!
//! Behind is: more texture slots queued for upload (not waiting for their
//! memory) than `VICTORY_LOAD_HOLD_TEXTURES` (256), host pipelines queued
//! and not made, or a wait for new card memory since the last frame. The
//! hold ends when nothing was behind for [`QUIET_FRAMES`] frames, when the
//! scene draws (a frame of 500 draws or more), when the max seconds pass,
//! or when the title's frames stop (it is waiting on the load itself, on
//! the thread that presents: holding on would only freeze its screen).

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

/// Draws a frame from which it is the scene, not a loading screen (as for
/// the world's first frame, `crate::resource`).
pub(crate) const SCENE_DRAWS: u64 = 500;
/// Frames of a loading screen in a row before a hold may begin: a black
/// frame or two between two shots of a cutscene is not a load.
pub(crate) const LOADING_FRAMES: u64 = 15;
/// Frames in a row with nothing behind that end a hold (a second at 30).
pub(crate) const QUIET_FRAMES: u64 = 30;
/// A hold ends when the title has not presented for this long.
pub(crate) const STALL_MS: u64 = 500;
/// The early texture pass's budget while held: milliseconds and bytes a
/// frame (a loading screen's frame prepares in under a millisecond).
const HELD_TEXTURE_MS: u64 = 20;
const HELD_TEXTURE_BYTES: u64 = 256 << 20;

/// The max seconds, in milliseconds (`None`: off).
fn max_ms() -> Option<u64> {
    static MS: OnceLock<Option<u64>> = OnceLock::new();
    *MS.get_or_init(|| {
        let seconds = std::env::var("VICTORY_LOAD_HOLD").ok()?.trim().parse::<f64>().ok()?;
        (seconds > 0.0).then_some((seconds * 1000.0) as u64)
    })
}

/// Whether `VICTORY_LOAD_HOLD` is set.
pub fn on() -> bool {
    max_ms().is_some()
}

fn textures_most() -> u64 {
    static MOST: OnceLock<u64> = OnceLock::new();
    *MOST.get_or_init(|| {
        std::env::var("VICTORY_LOAD_HOLD_TEXTURES").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(256)
    })
}

fn now_ms() -> u64 {
    static EPOCH: OnceLock<std::time::Instant> = OnceLock::new();
    EPOCH.get_or_init(std::time::Instant::now).elapsed().as_millis() as u64
}

/// When the hold began ([`now_ms`] + 1; 0: none), and the last present.
static SINCE: AtomicU64 = AtomicU64::new(0);
static PRESENTED: AtomicU64 = AtomicU64::new(0);
/// Status entries held back during the current hold.
static HELD: AtomicU64 = AtomicU64::new(0);

/// Whether the title's loads are to be told complete later: a hold is on,
/// within its max seconds, and the title presented lately. Cheap: the title
/// polls its status entries in a `Sleep(1)` loop through this.
pub fn holding() -> bool {
    let since = SINCE.load(Ordering::Relaxed);
    if since == 0 {
        return false;
    }
    let Some(max) = max_ms() else { return false };
    let now = now_ms();
    now.saturating_sub(since - 1) < max && now.saturating_sub(PRESENTED.load(Ordering::Relaxed)) < STALL_MS
}

/// DirectStorage held back a status entry.
pub fn note_held() {
    HELD.fetch_add(1, Ordering::Relaxed);
}

/// The early texture pass's budget (milliseconds, bytes) while held, if
/// it is larger than `ms` (`crate::draw`; `VICTORY_EARLY_TEXTURES_MS=0`,
/// no pass, stays so).
pub(crate) fn early_texture_budget(ms: u64) -> Option<(u64, u64)> {
    (ms > 0 && ms < HELD_TEXTURE_MS && holding()).then_some((HELD_TEXTURE_MS, HELD_TEXTURE_BYTES))
}

/// What a frame showed of our side, at its present.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Frame {
    pub at_ms: u64,
    pub draws: u64,
    /// Texture slots queued for upload, less those waiting for memory.
    pub textures: u64,
    /// Host pipelines queued and not made.
    pub making: u64,
    /// Waits for new card memory since the start.
    pub memory_waits: u64,
}

/// Why a hold ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Why {
    CaughtUp,
    SceneDrawn,
    MaxSeconds,
    FramesStopped,
}

impl Why {
    fn text(self) -> &'static str {
        match self {
            Why::CaughtUp => "our side caught up",
            Why::SceneDrawn => "the title drew its scene (500 draws or more)",
            Why::MaxSeconds => "the max seconds passed",
            Why::FramesStopped => "the title's frames stopped (it waits on the load where it presents)",
        }
    }
}

/// A hold that ended: how long, why, and how far behind our side was at
/// most and at the end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Rise {
    pub held_ms: u64,
    pub why: Why,
    pub peak_textures: u64,
    pub textures: u64,
    pub making: u64,
}

/// The decision, frame by frame (no clock, no counters of its own: tested
/// alone).
#[derive(Debug)]
pub(crate) struct Gate {
    max_ms: u64,
    textures_most: u64,
    loading: u64,
    quiet: u64,
    waits: u64,
    last_at: Option<u64>,
    since: Option<u64>,
    /// False after a hold ran out of time or the frames stopped, until a
    /// frame with nothing behind: the same load is not held twice.
    armed: bool,
    peak: u64,
}

impl Gate {
    pub(crate) fn new(max_ms: u64, textures_most: u64) -> Gate {
        Gate {
            max_ms,
            textures_most,
            loading: 0,
            quiet: 0,
            waits: 0,
            last_at: None,
            since: None,
            armed: true,
            peak: 0,
        }
    }

    /// When the current hold began, if one is on.
    pub(crate) fn since(&self) -> Option<u64> {
        self.since
    }

    /// Takes in a frame; returns the hold that ended with it, if any.
    pub(crate) fn frame(&mut self, f: Frame) -> Option<Rise> {
        let waited = f.memory_waits > self.waits;
        self.waits = f.memory_waits;
        let behind = f.textures > self.textures_most || f.making > 0 || waited;
        self.quiet = if behind { 0 } else { self.quiet + 1 };
        self.loading = if f.draws < SCENE_DRAWS { self.loading + 1 } else { 0 };
        let stalled = self.last_at.is_some_and(|at| f.at_ms.saturating_sub(at) >= STALL_MS);
        self.last_at = Some(f.at_ms);
        if let Some(start) = self.since {
            self.peak = self.peak.max(f.textures);
            let why = if stalled {
                Some(Why::FramesStopped)
            } else if self.loading == 0 {
                Some(Why::SceneDrawn)
            } else if self.quiet >= QUIET_FRAMES {
                Some(Why::CaughtUp)
            } else if f.at_ms.saturating_sub(start) >= self.max_ms {
                Some(Why::MaxSeconds)
            } else {
                None
            };
            let why = why?;
            self.since = None;
            if matches!(why, Why::MaxSeconds | Why::FramesStopped) {
                self.armed = false;
            }
            return Some(Rise {
                held_ms: f.at_ms.saturating_sub(start),
                why,
                peak_textures: self.peak,
                textures: f.textures,
                making: f.making,
            });
        }
        if !behind {
            self.armed = true;
        }
        if self.armed && behind && self.loading >= LOADING_FRAMES {
            self.since = Some(f.at_ms);
            self.peak = f.textures;
        }
        None
    }
}

/// At each present: takes in what the frame showed and starts or ends a
/// hold; one line when a hold ends.
#[cfg(windows)]
pub(crate) fn presented(frame: u64) {
    let Some(max) = max_ms() else { return };
    static GATE: std::sync::Mutex<Option<Gate>> = std::sync::Mutex::new(None);
    static LAST_DRAWN: AtomicU64 = AtomicU64::new(0);
    let drawn = crate::draw::drawn();
    let draws = drawn.saturating_sub(LAST_DRAWN.swap(drawn, Ordering::Relaxed));
    let [queued, _, away, ..] = [0, 1, 2].map(|i| crate::draw::HEAP_TEXTURES_QUEUE[i].load(Ordering::Relaxed));
    let at_ms = now_ms();
    let f = Frame {
        at_ms,
        draws,
        textures: queued.saturating_sub(away),
        making: crate::pipeline::making(),
        memory_waits: crate::host::memory_waits(),
    };
    let mut gate = GATE.lock().unwrap_or_else(|e| e.into_inner());
    let gate = gate.get_or_insert_with(|| Gate::new(max, textures_most()));
    let was = gate.since();
    let rise = gate.frame(f);
    PRESENTED.store(at_ms, Ordering::Relaxed);
    match (was, gate.since()) {
        (None, Some(start)) => {
            HELD.store(0, Ordering::Relaxed);
            SINCE.store(start + 1, Ordering::Relaxed);
            tracing::info!(
                frame,
                textures = f.textures,
                making = f.making,
                "load hold: the title's loads are told complete later while our side catches up (VICTORY_LOAD_HOLD)"
            );
        }
        (Some(_), None) => SINCE.store(0, Ordering::Relaxed),
        _ => {}
    }
    if let Some(r) = rise {
        tracing::info!(
            frame,
            held_s = format_args!("{:.1}", r.held_ms as f64 / 1000.0),
            status_entries_held = HELD.load(Ordering::Relaxed),
            textures_at_most = r.peak_textures,
            textures_left = r.textures,
            pipelines_left = r.making,
            "load hold rose: {}",
            r.why.text()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loading(at_ms: u64, textures: u64) -> Frame {
        Frame { at_ms, draws: 20, textures, ..Frame::default() }
    }

    /// Frames 33 ms apart from `from`, as `f` makes them.
    fn run(gate: &mut Gate, from: u64, n: u64, f: impl Fn(u64) -> Frame) -> Option<Rise> {
        (0..n).find_map(|i| gate.frame(f(from + i * 33)))
    }

    #[test]
    fn a_backlog_on_a_loading_screen_holds_until_it_is_gone() {
        let mut gate = Gate::new(30_000, 256);
        assert!(run(&mut gate, 0, LOADING_FRAMES - 1, |at| loading(at, 5000)).is_none());
        assert!(gate.since().is_none(), "not before the loading screen has lasted");
        assert!(run(&mut gate, 500, 1, |at| loading(at, 5000)).is_none());
        assert!(gate.since().is_some());
        // Still behind: held on.
        assert!(run(&mut gate, 600, 100, |at| loading(at, 1000)).is_none());
        // Caught up: ends after the quiet frames.
        let rise = run(&mut gate, 4000, QUIET_FRAMES + 1, |at| loading(at, 10)).expect("a rise");
        assert_eq!(rise.why, Why::CaughtUp);
        assert_eq!(rise.peak_textures, 5000);
        assert!(gate.since().is_none());
    }

    #[test]
    fn the_scene_drawing_ends_a_hold() {
        let mut gate = Gate::new(30_000, 256);
        run(&mut gate, 0, LOADING_FRAMES, |at| loading(at, 5000));
        assert!(gate.since().is_some());
        let rise = gate.frame(Frame { at_ms: 480, draws: 1500, textures: 5000, ..Frame::default() });
        assert_eq!(rise.map(|r| r.why), Some(Why::SceneDrawn));
    }

    #[test]
    fn the_max_seconds_end_a_hold_and_the_same_load_is_not_held_again() {
        let mut gate = Gate::new(2_000, 256);
        let rise = run(&mut gate, 0, 200, |at| loading(at, 5000)).expect("a rise");
        assert_eq!(rise.why, Why::MaxSeconds);
        assert!(rise.held_ms >= 2_000 && rise.held_ms < 2_100);
        assert!(run(&mut gate, 10_000, 100, |at| loading(at, 5000)).is_none());
        assert!(gate.since().is_none(), "not again while still behind");
        // Once caught up, the next load may be held.
        gate.frame(loading(20_000, 0));
        run(&mut gate, 20_033, LOADING_FRAMES, |at| loading(at, 5000));
        assert!(gate.since().is_some());
    }

    #[test]
    fn frames_that_stop_end_a_hold() {
        let mut gate = Gate::new(30_000, 256);
        run(&mut gate, 0, LOADING_FRAMES, |at| loading(at, 5000));
        assert!(gate.since().is_some());
        let rise = gate.frame(loading(500 + STALL_MS + 400, 5000));
        assert_eq!(rise.map(|r| r.why), Some(Why::FramesStopped));
    }

    #[test]
    fn pipelines_and_memory_waits_are_behind_too() {
        let mut gate = Gate::new(30_000, 256);
        run(&mut gate, 0, LOADING_FRAMES, |at| Frame { making: 3, ..loading(at, 0) });
        assert!(gate.since().is_some());
        // Memory waits every frame keep it held.
        assert!(run(&mut gate, 495, 60, |at| Frame { memory_waits: at, ..loading(at, 0) }).is_none());
        let rise = run(&mut gate, 2475, QUIET_FRAMES + 1, |at| Frame { memory_waits: 3000, ..loading(at, 0) });
        assert_eq!(rise.map(|r| r.why), Some(Why::CaughtUp));
    }

    #[test]
    fn the_scene_or_a_quiet_load_never_holds() {
        let mut gate = Gate::new(30_000, 256);
        assert!(run(&mut gate, 0, 100, |at| Frame { at_ms: at, draws: 3000, textures: 30_000, ..Frame::default() }).is_none());
        assert!(gate.since().is_none());
        assert!(run(&mut gate, 5000, 100, |at| loading(at, 100)).is_none());
        assert!(gate.since().is_none());
    }
}
