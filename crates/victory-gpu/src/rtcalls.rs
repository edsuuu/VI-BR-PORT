//! What the title asks of its ray-tracing structure builder
//! (`XboxBvhBuilder.dll`), counted for the frame report: the builder's calls
//! are seen in `victory_hle::modules::bvh_builder` (with
//! `VICTORY_DXR_TRACE`), the frames here, and the five-second report says
//! how much a frame asked for: structures built and updated at each level,
//! triangles, boxes and instances that went into them, copies, compactions
//! and unpackings, and how many distinct structures were named since the
//! start. These are what decide whether building on the host's own ray
//! tracing pays: how much there is to build each frame, and how much of it
//! is updates.
//!
//! Without the switch nothing is counted and the report says nothing.

use std::collections::HashSet;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// `VICTORY_DXR_TRACE`: `1` traces the builder's calls (each call while a
/// budget lasts, then the first of each structure), `all` every call; unset
/// or anything else: off (the default).
pub fn trace() -> Trace {
    static ON: OnceLock<Trace> = OnceLock::new();
    *ON.get_or_init(|| match std::env::var("VICTORY_DXR_TRACE").as_deref() {
        Ok("1") => Trace::Budget,
        Ok("all") => Trace::All,
        _ => Trace::Off,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trace {
    Off,
    Budget,
    All,
}

/// What is counted.
#[derive(Clone, Copy, Debug)]
pub enum Count {
    BottomBuilds,
    BottomUpdates,
    TopBuilds,
    TopUpdates,
    Geometries,
    Triangles,
    Aabbs,
    Instances,
    /// Calls of the batched build (each with one or more builds).
    Batches,
    Clones,
    Compactions,
    /// Other copy modes (serialize, decode for tools).
    OtherCopies,
    /// Structures unpacked from their shipped ("dehydrated") form.
    Rehydrated,
    PrebuildAsks,
    /// Nanoseconds the title's DLL took for the calls, on the title's threads.
    Nanos,
}

const COUNTS: usize = Count::Nanos as usize + 1;

static SINCE: [AtomicU64; COUNTS] = [const { AtomicU64::new(0) }; COUNTS];
static FRAMES: AtomicU64 = AtomicU64::new(0);
/// Distinct destination addresses named since the start, at each level.
static NAMED: Mutex<Option<[HashSet<u64>; 2]>> = Mutex::new(None);
/// Destinations kept at most per level (the count stops growing there).
const NAMED_MOST: usize = 1 << 20;

pub fn add(what: Count, n: u64) {
    SINCE[what as usize].fetch_add(n, Relaxed);
}

/// A build named `dest` at the top level or not; whether it is the first
/// time that address is (`None` once the table is full).
pub fn named(top: bool, dest: u64) -> Option<bool> {
    let mut named = NAMED.lock().unwrap_or_else(|e| e.into_inner());
    let set = &mut named.get_or_insert_with(Default::default)[usize::from(top)];
    if set.len() >= NAMED_MOST && !set.contains(&dest) {
        return None;
    }
    Some(set.insert(dest))
}

/// The title buffer (start, bytes) that holds all of `[address, address +
/// len)`, if one does: data a host build could read from that buffer's
/// copy where it is ([`crate::shadow`]); outside buffers it lies in pages
/// of the by-address memory, which are not one run on the card.
pub fn in_title_buffer(address: u64, len: u64) -> Option<(u64, u64)> {
    crate::shadow::in_buffer(address, len.max(1))
}

/// A frame was presented.
pub(crate) fn frame() {
    if trace() != Trace::Off {
        FRAMES.fetch_add(1, Relaxed);
    }
}

/// The five-second line.
pub(crate) fn line() {
    if trace() == Trace::Off {
        return;
    }
    let frames = FRAMES.swap(0, Relaxed).max(1) as f64;
    let c: [f64; COUNTS] = std::array::from_fn(|i| SINCE[i].swap(0, Relaxed) as f64 / frames);
    let [bottom, top] = {
        let named = NAMED.lock().unwrap_or_else(|e| e.into_inner());
        named.as_ref().map_or([0, 0], |n| [n[0].len(), n[1].len()])
    };
    use Count::*;
    tracing::info!(
        "ray-tracing structures asked per frame (VICTORY_DXR_TRACE): bottom level {:.1} built and {:.1} updated, \
         top level {:.1} built and {:.1} updated, in {:.1} batched calls; {:.0} geometries, {:.0} triangles, {:.0} boxes, \
         {:.0} instances; copies {:.1} clones, {:.1} compactions, {:.1} other; {:.2} unpacked; {:.1} prebuild asks; \
         {:.3} ms of the builder's own on the title's threads; since the start {bottom} bottom-level and {top} top-level \
         destinations named",
        c[BottomBuilds as usize],
        c[BottomUpdates as usize],
        c[TopBuilds as usize],
        c[TopUpdates as usize],
        c[Batches as usize],
        c[Geometries as usize],
        c[Triangles as usize],
        c[Aabbs as usize],
        c[Instances as usize],
        c[Clones as usize],
        c[Compactions as usize],
        c[OtherCopies as usize],
        c[Rehydrated as usize],
        c[PrebuildAsks as usize],
        c[Nanos as usize] / 1e6,
    );
}

#[cfg(test)]
mod tests {
    #[test]
    fn destinations_are_new_once() {
        assert_eq!(super::named(false, 0x1234_5000), Some(true));
        assert_eq!(super::named(false, 0x1234_5000), Some(false));
        // The levels are apart.
        assert_eq!(super::named(true, 0x1234_5000), Some(true));
    }
}
