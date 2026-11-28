//! The title's colour grading: the tone map pixel program (`PS_ToneMap_*`)
//! reads a 3D lookup table that compute programs bake once, at loading or
//! when the options change (`CS_BakeACES*`, `CS_BakeCES*`, `CS_LUT3D*`).
//! Some sessions show the whole picture in bands of few colours from the
//! world's first frame to the end (docs `api-poster.md`): either the table
//! is read without filtering (its sampler slot was never written and holds
//! the plain point sampler), or it was baked wrong because a bake dispatch
//! found no host pipeline yet and was dropped, never to run again.
//!
//! One line at the world's first frame says which: the tone map's views
//! and samplers as its last draw bound them, and how many bake dispatches
//! ran and were dropped (that frame and since the start). Two switches,
//! both off by default, try the remedies: `VICTORY_LUT_BAKE_WAIT=1` (a bake
//! dispatch waits for its pipeline however long it takes) and
//! `VICTORY_NULL_SAMPLER_3D_LINEAR=1` (a draw or dispatch that reads a 3D
//! texture and a sampler slot without a sampler gets a linear one there).

use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

/// A program's part in the colour grading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Role {
    /// The final tone map pixel program, which reads the table.
    ToneMap,
    /// A compute program that bakes or blends the table.
    Bake,
}

/// The part of the program in container `bytes` of stage `stage`, by the
/// program names the title's containers carry.
pub(crate) fn role(stage: &str, bytes: &[u8]) -> Option<Role> {
    let has = |name: &[u8]| bytes.windows(name.len()).any(|w| w == name);
    match stage {
        "pixel" if has(b"PS_ToneMap_") => Some(Role::ToneMap),
        "compute" if [&b"CS_BakeACES"[..], b"CS_BakeCES", b"CS_LUT3D"].iter().any(|n| has(n)) => Some(Role::Bake),
        _ => None,
    }
}

/// Bake dispatches that ran and that were dropped since the start, and the
/// totals at the last present (what the frame since took is the difference).
static RAN: AtomicU64 = AtomicU64::new(0);
static DROPPED: AtomicU64 = AtomicU64::new(0);
static RAN_BEFORE: AtomicU64 = AtomicU64::new(0);
static DROPPED_BEFORE: AtomicU64 = AtomicU64::new(0);
/// The first few bake programs dropped (their hashes), for the line.
static DROPPED_HASHES: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// What the tone map's last draw bound, until the line is written.
static TONE_MAP: Mutex<Option<String>> = Mutex::new(None);
/// Whether the line has been written (the probes stop then).
static LOGGED: AtomicBool = AtomicBool::new(false);

/// Whether a bake dispatch waits for its host pipeline without limit
/// (`VICTORY_LUT_BAKE_WAIT=1`; off by default: it is dropped past the
/// frame's waiting budget, as every other dispatch).
pub(crate) fn bake_wait() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        let on = std::env::var_os("VICTORY_LUT_BAKE_WAIT").is_some_and(|v| v == "1");
        if on {
            tracing::info!("LUT bake dispatches wait for their host pipeline (VICTORY_LUT_BAKE_WAIT=1)");
        }
        on
    })
}

/// Whether a sampler slot without a sampler, read by a program that reads
/// a 3D texture, gets a linear sampler instead of the plain point one
/// (`VICTORY_NULL_SAMPLER_3D_LINEAR=1`; off by default).
pub(crate) fn null_3d_linear() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        let on = std::env::var_os("VICTORY_NULL_SAMPLER_3D_LINEAR").is_some_and(|v| v == "1");
        if on {
            tracing::info!("sampler slots without a sampler read with a 3D texture are made linear (VICTORY_NULL_SAMPLER_3D_LINEAR=1)");
        }
        on
    })
}

/// A bake dispatch ran (`ran`) or was dropped without a host pipeline.
pub(crate) fn note_bake(ran: bool, hashes: impl FnOnce() -> String) {
    if ran {
        RAN.fetch_add(1, Relaxed);
        return;
    }
    if DROPPED.fetch_add(1, Relaxed) < 64 {
        let hashes = hashes();
        let mut dropped = DROPPED_HASHES.lock().unwrap_or_else(|e| e.into_inner());
        if !dropped.contains(&hashes) {
            tracing::warn!(shaders = %hashes, frame = crate::frame::current(), "a LUT bake dispatch dropped without a host pipeline: the colour table may stay wrong for the session");
            if dropped.len() < 8 {
                dropped.push(hashes);
            }
        }
    }
}

/// Whether the tone map's bindings are still wanted for the line.
pub(crate) fn probing() -> bool {
    !LOGGED.load(Relaxed)
}

/// The tone map's last draw bound `text`.
pub(crate) fn note_tone_map(text: String) {
    *TONE_MAP.lock().unwrap_or_else(|e| e.into_inner()) = Some(text);
}

/// At each present: at the world's first frame (`world`), the line.
pub(crate) fn presented(frame: u64, world: bool) {
    let ran = RAN.load(Relaxed);
    let dropped = DROPPED.load(Relaxed);
    let ran_frame = ran - RAN_BEFORE.swap(ran, Relaxed);
    let dropped_frame = dropped - DROPPED_BEFORE.swap(dropped, Relaxed);
    if !world || LOGGED.swap(true, Relaxed) {
        return;
    }
    let tone_map = TONE_MAP.lock().unwrap_or_else(|e| e.into_inner()).take();
    let hashes = DROPPED_HASHES.lock().unwrap_or_else(|e| e.into_inner()).join(",");
    tracing::info!(
        frame,
        tone_map = tone_map.as_deref().unwrap_or("not drawn yet"),
        bakes_ran_frame = ran_frame,
        bakes_dropped_frame = dropped_frame,
        bakes_ran = ran,
        bakes_dropped = dropped,
        dropped_bakes = %hashes,
        bake_wait = bake_wait(),
        null_3d_linear = null_3d_linear(),
        "colour grading at the world's first frame: the tone map's LUT view and sampler, and the LUT bakes"
    );
}

/// A host sampler filter in words: "point", "linear", "aniso" or the value.
pub(crate) fn filter_name(filter: i32) -> String {
    // D3D12_FILTER: bits 0, 2 and 4 are mip, mag and min linear; 0x40 is
    // anisotropic; 0x80 comparison and 0x100/0x180 minimum/maximum.
    let base = filter & 0x7f;
    let word = match base {
        0x00 => "point",
        0x15 => "linear",
        0x14 => "linear-mip-point",
        0x55 | 0x54 => "aniso",
        _ => "mixed",
    };
    if filter & !0x7f == 0 { word.to_owned() } else { format!("{word}+{:#x}", filter & !0x7f) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colour_grading_programs_are_known_by_name() {
        assert_eq!(role("pixel", b"..\0PS_ToneMap_c4fa8d83\0.."), Some(Role::ToneMap));
        assert_eq!(role("compute", b"CS_BakeACESWithGamutCompressLUT_1Shaper"), Some(Role::Bake));
        assert_eq!(role("compute", b"CS_BakeCESLUT_0"), Some(Role::Bake));
        assert_eq!(role("compute", b"CS_LUT3DBlend16WithBaseLayer"), Some(Role::Bake));
        assert_eq!(role("compute", b"PS_ToneMap_c4fa8d83"), None);
        assert_eq!(role("pixel", b"CS_LUT3DBlend4"), None);
        assert_eq!(role("pixel", b"PS_PedSkin_0001"), None);
    }

    #[test]
    fn filters_are_named() {
        assert_eq!(filter_name(0), "point");
        assert_eq!(filter_name(0x15), "linear");
        assert_eq!(filter_name(0x14), "linear-mip-point");
        assert_eq!(filter_name(0x55), "aniso");
        assert_eq!(filter_name(0x95), "linear+0x80");
    }
}
