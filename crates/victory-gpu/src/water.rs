//! `VICTORY_WATER_LIGHT`: whether the water's tiled light programs run
//! (`water_shading:CS_TransmissionVolumetrics_Tiled` and `_Tiled_LocalLights`,
//! [`crate::pipeline::skips_rt_skin`]).
//!
//! The title dispatches them in every frame it runs its water at all, and on a
//! host of 32-lane waves they cost 2-4 fps also where no water is on screen
//! (the prison walk 24.6 fps with them, 28.8 without; the opening 24.0 against
//! 26.4). Left out, the sea near the boat is black. `auto` runs them only while
//! the frame draws water ([`seen`]), with a hold of [`HOLD`] frames after the
//! water leaves; otherwise they are left out as `0` leaves them out. Each time
//! they start again the water's temporal pass takes some frames to catch up
//! ([`HOLD`]).
//!
//! What counts as drawing water is a pipeline whose programs carry one of the
//! names in [`signs`] (`VICTORY_WATER_LIGHT_SIGN`, default the water surface's
//! resolve, `PS_ResolveWaterSurface`): bound in both recorded frames of the
//! boat on the sea, in none of the frame recorded in the prison cell.

use std::sync::Mutex;

/// What the switch asks for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Mode {
    /// Unset, `1` or anything else not named below: they run (the default).
    Run,
    /// `0`: left out on a host of 32-lane waves.
    Off,
    /// `auto`: on a host of 32-lane waves, left out while no water was seen.
    Auto,
}

/// The switch's text as a mode. Anything but `0` and `auto` keeps the
/// meaning it had before `auto` was added: they run.
pub(crate) fn mode_of(value: Option<&std::ffi::OsStr>) -> Mode {
    match value {
        Some(v) if v == "0" => Mode::Off,
        Some(v) if v.eq_ignore_ascii_case("auto") => Mode::Auto,
        _ => Mode::Run,
    }
}

/// Frames the programs go on running after the last frame that drew water:
/// a second at the title's 30 fps (`VICTORY_WATER_LIGHT_HOLD=<frames>` sets
/// another). A shore, a bridge or a turn of the camera takes the water off
/// screen for a few frames at a time; without a hold the light would stop and
/// start again with it.
///
/// Each start costs more than one frame. `CS_TransmissionVolumetrics_Temporal_Tiled`
/// (`c7fd991b910dc7fe`) is not left out: in the boat capture it runs right
/// after the two, on a history the frame before copied out of its own output,
/// and it blends their output into that history. While they are left out it
/// blends what they wrote when they last ran, or nothing at all if they never
/// ran in the session (the state of `0`, where the sea is black). When they
/// start again the history has to converge on their new output, so the sea
/// can fade in from dark or from an old light over as many frames as the
/// title's blend takes, not change in one. Leaving the temporal pass out with
/// them would not help: its history would be just as old. Each start is
/// counted ([`Gate::returned`]) so a run can tell how often this happens.
pub(crate) const HOLD: u64 = 30;

/// The hold, `VICTORY_WATER_LIGHT_HOLD` if it is a number, else [`HOLD`].
fn hold_of(value: Option<&str>) -> u64 {
    value.and_then(|v| v.trim().parse().ok()).unwrap_or(HOLD)
}

/// The default names of the programs whose pipelines count as water drawn.
const SIGNS: &str = "PS_ResolveWaterSurface";

/// The names a pipeline's programs are looked for: `VICTORY_WATER_LIGHT_SIGN`
/// (comma-separated parts of program names), else [`SIGNS`].
fn signs_of(value: Option<&str>) -> Vec<Vec<u8>> {
    let text = value.filter(|v| !v.trim().is_empty()).unwrap_or(SIGNS);
    text.split(',').map(str::trim).filter(|s| !s.is_empty()).map(|s| s.as_bytes().to_vec()).collect()
}

/// Whether a program's container carries one of `signs`.
fn carries(container: &[u8], signs: &[Vec<u8>]) -> bool {
    signs.iter().any(|name| container.windows(name.len()).any(|w| w == name.as_slice()))
}

/// The decision for one frame, from the frames the water was seen in.
#[derive(Debug)]
pub(crate) struct Gate {
    /// The latest frame a water sign was recorded in.
    seen: Option<u64>,
    /// The frame decided last and what for: every dispatch of one frame gets
    /// the same answer, whichever thread records it and whatever signs come
    /// after the first of them.
    decided: Option<(u64, bool)>,
    /// Frames the programs ran in, and frames they were left out of.
    pub(crate) ran: u64,
    pub(crate) left: u64,
    /// Frames a sign was recorded in.
    pub(crate) signed: u64,
    /// Frames whose sign was recorded after the frame had been decided
    /// against the light. The title records its lists on several threads at
    /// once, and the resolve (list 236 in the boat capture) and the light (list
    /// 245) need not be recorded in the order they are submitted in; nor need
    /// both get the same `frame::current()`, a counter the frame wait moves on
    /// the title's main thread. The hold hides this while water stays on
    /// screen; it can only happen in the first frame of water after more than
    /// [`HOLD`] frames without it, and this count says whether it does.
    pub(crate) late: u64,
    /// Frames in which the programs ran after a frame they were left out of,
    /// or for the first time: each can fade in through the temporal history
    /// ([`HOLD`]).
    pub(crate) returned: u64,
    /// The hold in frames ([`hold_of`]).
    hold: u64,
}

impl Default for Gate {
    fn default() -> Self {
        Gate { seen: None, decided: None, ran: 0, left: 0, signed: 0, late: 0, returned: 0, hold: HOLD }
    }
}

impl Gate {
    /// A water sign recorded in `frame`.
    pub(crate) fn seen(&mut self, frame: u64) {
        if self.seen.is_none_or(|s| s < frame) {
            self.seen = Some(frame);
            self.signed += 1;
            if self.decided == Some((frame, false)) {
                self.late += 1;
            }
        }
    }

    /// Whether the programs run in `frame`: water was seen in it (before
    /// this, its first water-light dispatch) or in the [`HOLD`] frames before.
    pub(crate) fn runs(&mut self, frame: u64) -> bool {
        if let Some((at, runs)) = self.decided
            && at == frame
        {
            return runs;
        }
        let runs = self.seen.is_some_and(|s| s.saturating_add(self.hold) >= frame);
        if runs && self.decided.is_none_or(|(_, before)| !before) {
            self.returned += 1;
        }
        self.decided = Some((frame, runs));
        if runs {
            self.ran += 1;
        } else {
            self.left += 1;
        }
        runs
    }
}

#[cfg(windows)]
static GATE: Mutex<Gate> = Mutex::new(Gate {
    seen: None,
    decided: None,
    ran: 0,
    left: 0,
    signed: 0,
    late: 0,
    returned: 0,
    hold: HOLD,
});

#[cfg(windows)]
fn gate() -> std::sync::MutexGuard<'static, Gate> {
    GATE.lock().unwrap_or_else(|e| e.into_inner())
}

/// The switch's text, read once.
#[cfg(windows)]
fn asked() -> Mode {
    static ASKED: std::sync::OnceLock<Mode> = std::sync::OnceLock::new();
    *ASKED.get_or_init(|| mode_of(std::env::var_os("VICTORY_WATER_LIGHT").as_deref()))
}

/// The switch, read once.
#[cfg(windows)]
pub(crate) fn mode() -> Mode {
    static MODE: std::sync::OnceLock<Mode> = std::sync::OnceLock::new();
    *MODE.get_or_init(|| {
        if victory_shader::emit::host_settings()[0] != 32 {
            // On hosts of 64-lane waves the programs translate as they are.
            Mode::Run
        } else {
            asked()
        }
    })
}

/// Whether a new pipeline with these programs' containers counts as water
/// drawn (`auto` only: otherwise none is looked at).
#[cfg(windows)]
pub(crate) fn is_sign<'a>(containers: impl IntoIterator<Item = &'a [u8]>) -> bool {
    if asked() != Mode::Auto {
        return false;
    }
    static SIGNS: std::sync::OnceLock<Vec<Vec<u8>>> = std::sync::OnceLock::new();
    let signs = SIGNS.get_or_init(|| {
        let value = std::env::var("VICTORY_WATER_LIGHT_SIGN").ok();
        let signs = signs_of(value.as_deref());
        tracing::info!(
            signs = ?signs.iter().map(|s| String::from_utf8_lossy(s).into_owned()).collect::<Vec<_>>(),
            "VICTORY_WATER_LIGHT=auto: pipelines with programs of these names count as water drawn"
        );
        signs
    });
    containers.into_iter().any(|c| carries(c, signs))
}

/// A pipeline that counts as water drawn was bound (`auto` only).
#[cfg(windows)]
pub(crate) fn seen() {
    if mode() == Mode::Auto {
        gate().seen(crate::frame::current());
    }
}

/// Whether a dispatch of one of the water's tiled light programs is left out.
#[cfg(windows)]
pub(crate) fn leaves_out() -> bool {
    match mode() {
        Mode::Run => false,
        Mode::Off => true,
        Mode::Auto => {
            static HOLD_ASKED: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
            let hold = *HOLD_ASKED.get_or_init(|| hold_of(std::env::var("VICTORY_WATER_LIGHT_HOLD").ok().as_deref()));
            let mut g = gate();
            g.hold = hold;
            !g.runs(crate::frame::current())
        }
    }
}

/// The line of the 5-second report, with `auto` only.
#[cfg(windows)]
pub(crate) fn report() {
    if mode() != Mode::Auto {
        return;
    }
    let (ran, left, signed, late, returned, hold) = {
        let mut g = gate();
        (
            std::mem::take(&mut g.ran),
            std::mem::take(&mut g.left),
            std::mem::take(&mut g.signed),
            std::mem::take(&mut g.late),
            std::mem::take(&mut g.returned),
            g.hold,
        )
    };
    tracing::info!(
        "the water's tiled light (VICTORY_WATER_LIGHT=auto), since the last line: run in {ran} frames, left out of {left}; \
         water drawn in {signed} frames (it runs while water was drawn in that frame or the {hold} before), \
         {late} of them drawn after the frame was decided without the light; started again {returned} times"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn the_switch_keeps_its_old_values() {
        assert_eq!(mode_of(None), Mode::Run);
        assert_eq!(mode_of(Some(OsStr::new("1"))), Mode::Run);
        assert_eq!(mode_of(Some(OsStr::new("0"))), Mode::Off);
        assert_eq!(mode_of(Some(OsStr::new("auto"))), Mode::Auto);
        assert_eq!(mode_of(Some(OsStr::new("AUTO"))), Mode::Auto);
        // As before: anything but 0 runs them.
        assert_eq!(mode_of(Some(OsStr::new("yes"))), Mode::Run);
        assert_eq!(mode_of(Some(OsStr::new(""))), Mode::Run);
    }

    #[test]
    fn no_water_seen_leaves_them_out() {
        let mut g = Gate::default();
        assert!(!g.runs(1));
        assert!(!g.runs(2));
        assert_eq!((g.ran, g.left, g.signed), (0, 2, 0));
    }

    #[test]
    fn water_seen_runs_them_in_that_frame_and_holds() {
        let mut g = Gate::default();
        assert!(!g.runs(99));
        g.seen(100);
        assert!(g.runs(100));
        // The water leaves: they go on for the hold, then stop.
        for f in 101..=100 + HOLD {
            assert!(g.runs(f), "frame {f}");
        }
        assert!(!g.runs(101 + HOLD));
        assert!(!g.runs(500));
        // It comes back.
        g.seen(600);
        assert!(g.runs(600));
        assert_eq!(g.signed, 2);
        // Started at 100 and again at 600, not in the frames of the hold.
        assert_eq!(g.returned, 2);
    }

    #[test]
    fn steady_water_starts_once() {
        let mut g = Gate::default();
        for f in 1..=100 {
            g.seen(f);
            assert!(g.runs(f));
        }
        assert_eq!(g.returned, 1);
    }

    #[test]
    fn the_hold_can_be_set() {
        assert_eq!(hold_of(None), HOLD);
        assert_eq!(hold_of(Some("x")), HOLD);
        assert_eq!(hold_of(Some(" 90 ")), 90);
        let mut g = Gate { hold: 0, ..Gate::default() };
        g.seen(10);
        assert!(g.runs(10));
        assert!(!g.runs(11));
    }

    #[test]
    fn water_seen_in_every_frame_keeps_them_running() {
        let mut g = Gate::default();
        for f in 1..=200 {
            g.seen(f);
            // A second sign of the same frame is not another frame.
            g.seen(f);
            assert!(g.runs(f));
        }
        assert_eq!((g.ran, g.left, g.signed), (200, 0, 200));
    }

    #[test]
    fn a_frame_keeps_its_first_answer() {
        let mut g = Gate::default();
        // A dispatch of frame 5 was recorded before the frame's water draw.
        assert!(!g.runs(5));
        g.seen(5);
        // The frame's second program is left out as its first was.
        assert!(!g.runs(5));
        assert!(g.runs(6));
        // Counted once a frame, not once a dispatch.
        assert_eq!((g.ran, g.left), (1, 1));
        // The sign that came after the frame was decided is counted as late.
        assert_eq!(g.late, 1);
    }

    #[test]
    fn a_sign_before_the_decision_is_not_late() {
        let mut g = Gate::default();
        g.seen(5);
        assert!(g.runs(5));
        // A frame decided with the light: a later sign of it is not late either.
        g.seen(6);
        assert!(g.runs(6));
        g.seen(6);
        // Within the hold a frame runs anyway, so its sign is never late.
        assert!(g.runs(7));
        g.seen(7);
        assert_eq!(g.late, 0);
    }

    #[test]
    fn a_sign_of_an_older_frame_does_not_move_it_back() {
        let mut g = Gate::default();
        g.seen(50);
        g.seen(10);
        assert!(g.runs(50 + HOLD));
        assert!(!g.runs(51 + HOLD));
    }

    #[test]
    fn signs_are_parts_of_program_names() {
        let default = signs_of(None);
        assert_eq!(default, vec![b"PS_ResolveWaterSurface".to_vec()]);
        assert_eq!(signs_of(Some("  ")), default);
        assert_eq!(signs_of(Some("A_x, B_y,")), vec![b"A_x".to_vec(), b"B_y".to_vec()]);
        let surface = b"..\0PS_ResolveWaterSurface_a0e5e558_Wrapped\0..";
        let deferred = b"..\0PS_ResolveDeferred_0_1cb4ed87_Wrapped\0..";
        assert!(carries(surface, &default));
        assert!(!carries(deferred, &default));
        assert!(!carries(b"", &default));
    }
}
