//! `VICTORY_RT_NATIVE=1` (off by default): the title's builder calls tell
//! the by-address memory how long each structure and each scratch memory is
//! (`victory_gpu::structure_extent`), before the builder's dispatches for
//! them are recorded. Those dispatches name both by their start alone, and
//! without this the memory sizes them by a guess (4 MiB for a structure
//! being built, 256 KiB for one read).
//!
//! The lengths are the builder's own: for each build of a batch the title's
//! own `GetRaytracingAccelerationStructurePrebuildInfo` is asked again on
//! that build's inputs (it runs on the CPU and records nothing). A copy
//! (compaction) gets the size the title read back from the slot its build
//! named for the compacted size, if that reads as one
//! (`victory_gpu::structure_copied`).
//!
//! The calls still go to the title's builder: the same compute programs
//! build the structures. With `VICTORY_DXR_TRACE` as well, the listening
//! post logs as before and this runs beside it.
//!
//! Ported from the ray-tracing part of another fork's interposer
//! (685f4c08b), over this module's own descriptions ([`super::desc`]).

use std::collections::BTreeMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};

use super::desc::{BATCHED, Batched, BuildDesc, INPUTS, POSTBUILD, Postbuild, PrebuildInfo};
use super::{Memory, PREBUILD, Prebuild, Process, original};

/// Whether `VICTORY_RT_NATIVE=1`. Read once; the imports are bound before
/// the title runs.
pub fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_RT_NATIVE").is_some_and(|v| v == "1"))
}

/// Builds of one batch looked at, at most (the title's batches are of 16,
/// by the fork this comes from).
const BATCH_MOST: u32 = 4096;
/// Postbuild requests of one build read, at most.
const ASKED_MOST: u32 = 4;
/// Slots kept at most; past it all are let go (a copy whose slot is not
/// known is sized by the guess, as without this switch).
const SLOTS_MOST: usize = 1 << 16;

/// By a built structure's address: the title address its build was told to
/// store the compacted size at. Read again when the structure is copied.
static SIZE_SLOTS: Mutex<BTreeMap<u64, u64>> = Mutex::new(BTreeMap::new());

/// Since the start: extents told, prebuild asks that gave none, copies sized.
static TOLD: AtomicU64 = AtomicU64::new(0);
static UNSIZED: AtomicU64 = AtomicU64::new(0);
static COPIES: AtomicU64 = AtomicU64::new(0);

/// The slot for the compacted size among a build's postbuild requests: the
/// first of kind 0 with a place to go.
pub(super) fn size_slot(asked: &[Postbuild]) -> Option<u64> {
    asked.iter().find(|p| p.kind == 0 && p.dest != 0).map(|p| p.dest)
}

/// What a build of `build` with the sizes `needs` uses: its result, and its
/// scratch memory (an update has a scratch size of its own; the larger
/// covers both). Empty spans and absent memory are left out.
pub(super) fn spans(build: &BuildDesc, needs: &PrebuildInfo) -> Vec<(u64, u64)> {
    let mut spans = Vec::with_capacity(2);
    if build.dest != 0 && needs.result != 0 {
        spans.push((build.dest, needs.result));
    }
    let scratch = needs.scratch.max(needs.update_scratch);
    if build.scratch != 0 && scratch != 0 {
        spans.push((build.scratch, scratch));
    }
    spans
}

/// A batch of `count` builds at `descs` is about to be recorded: each one's
/// result and scratch lengths go to the by-address memory, and the slot of
/// its compacted size is kept for the copy to come.
pub(super) fn batch(this: *mut c_void, count: u32, descs: *const u8) {
    let sizes = original(PREBUILD);
    if sizes.is_null() || descs.is_null() {
        return;
    }
    let n = count.min(BATCH_MOST) as usize;
    let Some(bytes) = Process.bytes(descs as u64, n * BATCHED) else {
        return;
    };
    for (k, one) in bytes.chunks_exact(BATCHED).enumerate() {
        let e = Batched::parse(one);
        if e.build.dest == 0 {
            continue;
        }
        if e.postbuild_count > 0
            && let Some(asked) = Process.bytes(e.postbuild, e.postbuild_count.min(ASKED_MOST) as usize * POSTBUILD)
            && let Some(slot) = size_slot(&asked.chunks_exact(POSTBUILD).map(Postbuild::parse).collect::<Vec<_>>())
        {
            let mut slots = SIZE_SLOTS.lock().unwrap_or_else(|e| e.into_inner());
            if slots.len() >= SLOTS_MOST {
                slots.clear();
            }
            slots.insert(e.build.dest, slot);
        }
        // The title's own inputs, as its build was given them (a copy, so
        // that the title's memory is only read).
        let mut inputs = [0u64; 3];
        // SAFETY: 24 bytes into 24 bytes of a local, apart.
        unsafe {
            std::ptr::copy_nonoverlapping(
                one[8..8 + INPUTS].as_ptr(),
                inputs.as_mut_ptr().cast::<u8>(),
                INPUTS,
            )
        };
        let mut needs = PrebuildInfo::default();
        // SAFETY: the title's own function, as the title calls it but with no
        // stream description, on a copy of the title's inputs.
        unsafe {
            std::mem::transmute::<*const (), Prebuild>(sizes)(
                this,
                inputs.as_ptr().cast::<u8>(),
                std::ptr::null(),
                &raw mut needs,
            )
        };
        let spans = spans(&e.build, &needs);
        if spans.is_empty() {
            UNSIZED.fetch_add(1, Relaxed);
        }
        for (start, len) in spans {
            victory_gpu::structure_extent(start, len);
            let told = TOLD.fetch_add(1, Relaxed);
            if told < 8 || told.is_power_of_two() {
                tracing::info!(
                    told = told + 1,
                    entry = k,
                    start = format_args!("{start:#x}"),
                    len,
                    level = e.build.inputs.level(),
                    count = e.build.inputs.count,
                    "ray tracing (VICTORY_RT_NATIVE): a structure's length from the builder's own sizes"
                );
            }
        }
    }
}

/// The title copies the structure at `source` to `dest`: the copy's length
/// is the compacted size the title read back from the slot the build of
/// `source` named, if it reads as one.
pub(super) fn copy(dest: u64, source: u64) {
    let slot = SIZE_SLOTS.lock().unwrap_or_else(|e| e.into_inner()).get(&source).copied().unwrap_or(0);
    let sized = victory_gpu::structure_copied(dest, source, slot);
    let n = COPIES.fetch_add(1, Relaxed);
    if n < 8 || n.is_power_of_two() {
        tracing::info!(
            copies = n + 1,
            dest = format_args!("{dest:#x}"),
            source = format_args!("{source:#x}"),
            slot = format_args!("{slot:#x}"),
            whole = ?victory_gpu::structure_extent_from(source),
            compacted = ?sized,
            unsized_builds = UNSIZED.load(Relaxed),
            "ray tracing (VICTORY_RT_NATIVE): a copy's length, from the size the title read (none: its slot held no size of this structure)"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::bvh_builder::desc::Inputs;

    #[test]
    fn the_compacted_size_slot_is_the_first_of_its_kind_with_a_place() {
        let p = |dest, kind| Postbuild { dest, kind };
        assert_eq!(size_slot(&[]), None);
        assert_eq!(size_slot(&[p(0x10, 3), p(0, 0), p(0x20, 0), p(0x30, 0)]), Some(0x20));
        assert_eq!(size_slot(&[p(0x10, 2)]), None);
    }

    #[test]
    fn a_build_uses_its_result_and_the_larger_scratch() {
        let build = BuildDesc { dest: 0x1_0000_0000, inputs: Inputs::default(), source: 0, scratch: 0x2_0000_0000 };
        let needs = PrebuildInfo { result: 0x4000, scratch: 0x800, update_scratch: 0x1000 };
        assert_eq!(spans(&build, &needs), [(0x1_0000_0000, 0x4000), (0x2_0000_0000, 0x1000)]);
        // No scratch given, or no size answered: left out.
        let alone = BuildDesc { scratch: 0, ..build };
        assert_eq!(spans(&alone, &needs), [(0x1_0000_0000, 0x4000)]);
        assert_eq!(spans(&build, &PrebuildInfo::default()), []);
    }

    #[test]
    fn without_the_switch_it_is_off() {
        // (The tests run without VICTORY_RT_NATIVE.)
        if std::env::var_os("VICTORY_RT_NATIVE").is_none() {
            assert!(!on());
        }
    }
}
