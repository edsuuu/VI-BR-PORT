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
//! With the same switch a second line says which of the title's ray
//! programs (those with the console's node test, `image_bvh*_intersect_ray`)
//! were dispatched in those five seconds, by container hash and entry point,
//! how often and over how many groups ([`dispatched`]): which families run
//! in a scene is what decides which walks to replace first.
//!
//! Without the switch nothing is counted and the report says nothing.

use std::collections::{HashMap, HashSet};
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

/// One ray program in the census.
struct Program {
    hash: String,
    /// Its entry point, from the container (`?` if none is found).
    name: String,
    /// Its node tests, and whether it runs 64 lanes a wave.
    sites: usize,
    wave64: bool,
    dispatches: u64,
    indirect: u64,
    groups: u64,
}

/// The census of ray programs ([`dispatched`]).
#[derive(Default)]
struct Census {
    /// Each container seen at a dispatch, by the address of its shared
    /// shader (`pipeline::SHADERS` keeps every one for the run, so an
    /// address is never another's): its program, if it is one.
    known: HashMap<usize, Option<usize>>,
    programs: Vec<Program>,
}

static CENSUS: Mutex<Option<Census>> = Mutex::new(None);

/// The entry point a container names (`CS_..._Wrapped`), if it holds one.
fn entry_name(container: &[u8]) -> Option<String> {
    const END: &[u8] = b"_Wrapped";
    let end = container.windows(END.len()).position(|w| w == END)? + END.len();
    let start = container[..end]
        .iter()
        .rposition(|&b| !(b.is_ascii_alphanumeric() || b == b'_'))
        .map_or(0, |i| i + 1);
    Some(String::from_utf8_lossy(&container[start..end]).into_owned())
}

/// The node tests of a shader's console programs, and whether one runs 64
/// lanes a wave.
fn node_tests(shader: &crate::pipeline::Shader) -> (usize, bool) {
    let Some(programs) = shader.programs() else {
        return (0, false);
    };
    let mut sites = 0;
    let mut wave64 = false;
    for p in &programs {
        let Ok(code) = victory_shader::gfx10::decode(&p.code) else {
            continue;
        };
        let n = code
            .iter()
            .filter(|i| {
                matches!(
                    i.name(),
                    Some("image_bvh_intersect_ray" | "image_bvh64_intersect_ray")
                )
            })
            .count();
        sites += n;
        wave64 |= n > 0 && p.wave64;
    }
    (sites, wave64)
}

/// A dispatch of `p` was recorded (`groups`: its group counts; `None` for an
/// indirect one). With the trace on, a ray program's is counted for the
/// census line; the first time a container is seen its code is looked at
/// once, outside the lock.
pub(crate) fn dispatched(p: &crate::pipeline::host::HostPipeline, groups: Option<[u32; 3]>) {
    if trace() == Trace::Off || !p.compute {
        return;
    }
    let Some(stage) = p.stages.first() else {
        return;
    };
    let key = std::sync::Arc::as_ptr(&stage.shader) as usize;
    let count = |c: &mut Census, i: Option<usize>| {
        if let Some(program) = i.map(|i| &mut c.programs[i]) {
            match groups {
                Some([x, y, z]) => {
                    program.dispatches += 1;
                    program.groups += u64::from(x) * u64::from(y) * u64::from(z);
                }
                None => program.indirect += 1,
            }
        }
    };
    {
        let mut census = CENSUS.lock().unwrap_or_else(|e| e.into_inner());
        let c = census.get_or_insert_with(Census::default);
        if let Some(&i) = c.known.get(&key) {
            count(c, i);
            return;
        }
    }
    let (sites, wave64) = node_tests(&stage.shader);
    let program = (sites > 0).then(|| Program {
        hash: stage.shader.hash.clone(),
        name: entry_name(stage.shader.container()).unwrap_or_else(|| "?".into()),
        sites,
        wave64,
        dispatches: 0,
        indirect: 0,
        groups: 0,
    });
    if let Some(p) = &program {
        tracing::info!(
            hash = %p.hash,
            name = %p.name,
            sites = p.sites,
            wave64 = p.wave64,
            frame = crate::frame::current(),
            "dxr trace: a ray program is dispatched for the first time (VICTORY_DXR_TRACE)"
        );
    }
    let mut census = CENSUS.lock().unwrap_or_else(|e| e.into_inner());
    let c = census.get_or_insert_with(Census::default);
    let i = match c.known.get(&key) {
        // Another thread looked at it meanwhile.
        Some(&i) => i,
        None => {
            let i = program.map(|p| {
                c.programs.push(p);
                c.programs.len() - 1
            });
            c.known.insert(key, i);
            i
        }
    };
    count(c, i);
}

/// The census line: the ray programs dispatched since the last, most
/// dispatched first, a frame each; their counts start again.
fn census_line(frames: f64) {
    let mut census = CENSUS.lock().unwrap_or_else(|e| e.into_inner());
    let Some(c) = census.as_mut() else {
        tracing::info!("ray programs dispatched per frame (VICTORY_DXR_TRACE): none seen yet");
        return;
    };
    let seen = c.programs.len();
    let mut ran: Vec<&mut Program> = c
        .programs
        .iter_mut()
        .filter(|p| p.dispatches + p.indirect > 0)
        .collect();
    ran.sort_by_key(|p| std::cmp::Reverse(p.dispatches + p.indirect));
    let total: u64 = ran.iter().map(|p| p.dispatches + p.indirect).sum();
    let mut text = format!(
        "ray programs dispatched per frame (VICTORY_DXR_TRACE): {:.1} dispatches of {} programs ({seen} ray programs seen since the start)",
        total as f64 / frames,
        ran.len(),
    );
    use std::fmt::Write as _;
    for p in ran {
        let _ = write!(
            text,
            "; {} {} ({} sites{}) {:.2}",
            p.hash,
            p.name,
            p.sites,
            if p.wave64 { ", wave64" } else { "" },
            p.dispatches as f64 / frames
        );
        if p.indirect > 0 {
            let _ = write!(text, " + {:.2} indirect", p.indirect as f64 / frames);
        }
        if p.dispatches > 0 {
            let _ = write!(text, ", {:.0} groups", p.groups as f64 / frames);
        }
        p.dispatches = 0;
        p.indirect = 0;
        p.groups = 0;
    }
    tracing::info!("{text}");
}

/// The five-second lines.
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
    census_line(frames);
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

    #[test]
    fn a_container_names_its_entry_point() {
        assert_eq!(
            super::entry_name(b"DXBC..\0\x07CS_RTShadowTrace32_Tiled_1a2b3c4d_Wrapped\0NOBC")
                .as_deref(),
            Some("CS_RTShadowTrace32_Tiled_1a2b3c4d_Wrapped")
        );
        assert_eq!(
            super::entry_name(b"CS_x_Wrapped").as_deref(),
            Some("CS_x_Wrapped")
        );
        assert_eq!(super::entry_name(b"DXBC CS_Unnamed\0"), None);
    }
}
