//! `VICTORY_WATER_LIGHT`: whether the water's tiled light programs run
//! (`water_shading:CS_TransmissionVolumetrics_Tiled` and `_Tiled_LocalLights`,
//! [`crate::pipeline::skips_rt_skin`]).
//!
//! The title dispatches them in every frame it runs its water at all, and on a
//! host of 32-lane waves they cost 2-4 fps also where no water is on screen
//! (the prison walk 24.6 fps with them, 28.8 without; the opening 24.0 against
//! 26.4). Left out, the sea near the boat is black. `auto` runs them only while
//! the frame draws water ([`seen`]), with a hold of [`HOLD`] frames after the
//! water leaves; otherwise they are left out as `0` leaves them out.
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
/// a second at the title's 30 fps. A shore, a bridge or a turn of the camera
/// takes the water off screen for a few frames at a time; without a hold the
/// light would stop and start again with it, and the first frame of each
/// start shows what the programs wrote when they last ran (the decision of
/// a frame can be taken before its water is recorded, [`Gate::runs`]).
pub(crate) const HOLD: u64 = 30;

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
#[derive(Default, Debug)]
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
}

impl Gate {
    /// A water sign recorded in `frame`.
    pub(crate) fn seen(&mut self, frame: u64) {
        if self.seen.is_none_or(|s| s < frame) {
            self.seen = Some(frame);
            self.signed += 1;
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
        let runs = self.seen.is_some_and(|s| s.saturating_add(HOLD) >= frame);
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
        Mode::Auto => !gate().runs(crate::frame::current()),
    }
}

/// The line of the 5-second report, with `auto` only.
#[cfg(windows)]
pub(crate) fn report() {
    if mode() != Mode::Auto {
        return;
    }
    let (ran, left, signed) = {
        let mut g = gate();
        (std::mem::take(&mut g.ran), std::mem::take(&mut g.left), std::mem::take(&mut g.signed))
    };
    tracing::info!(
        "the water's tiled light (VICTORY_WATER_LIGHT=auto), since the last line: run in {ran} frames, left out of {left}; \
         water drawn in {signed} frames (it runs while water was drawn in that frame or the {HOLD} before)"
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
