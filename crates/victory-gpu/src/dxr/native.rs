//! The host's own builds of the title's structures, beside the console's
//! (`VICTORY_DXR_NATIVE`, see [`super::native`]).
//!
//! The listening post hands over each build the title asked of its builder,
//! decoded ([`super::Build`]). Here it becomes work at the same place of the
//! same title list ([`crate::list::external`]), so the host's build comes
//! after whatever the title recorded before its builder call (the skinning
//! that wrote the vertices) and before whatever it records after, and is
//! submitted when the title submits the list. That work:
//!
//! 1. finds the vertex, index and transform data in the host copies of
//!    title memory (`super::bottom`) and has them brought up to date at
//!    submission, as a draw's vertex buffers are;
//! 2. decides between a build and a refit ([`super::table::decide`]: the
//!    update rules of Direct3D, which the title follows for the console);
//! 3. takes card memory for the structure and its scratch ([`super::pool`])
//!    and records the build between barriers, with timestamps around it.
//!
//! **Resource states.** The host copies of title memory are buffers made in
//! the common state, and nothing in the layer moves a buffer to another
//! state: `shadow.rs` keeps no state at all, and every list relies on the
//! implicit promotion of buffers out of the common state, with unordered
//! access barriers on every resource where the title's barriers name
//! buffers (`list.rs` `barrier`) and where copies are placed in a list
//! (`host/place.rs`). The builds do the same: a barrier on every resource
//! before them (what the skinning wrote is done) and after them (what reads
//! the structures sees them). The structures and their scratch memory are
//! buffers of ours that never leave their one state ([`super::pool`]).
//!
//! **Which copy** (the critic's item 9). `shadow::find_any` gives a draw any
//! copy that holds a range, and a copy is not told of a submission's shader
//! writes into another copy of the same bytes (`Shadow::gpu_writes`). A
//! build reads the copy shaders were declared to write if exactly one of
//! the copies holding its range was (`super::bottom`); with more than one such
//! copy it is refused and counted; with none, it reads what a draw would.
//!
//! **Lifetimes.** A structure's memory is held by the table while the title
//! names it and by every list that builds, refits, copies or reads it, until
//! that list is reset; then it goes back to the pool. Nothing is freed under
//! a list that may still run.

use std::fmt::Write as _;
use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};

use windows::Win32::Graphics::Direct3D12::*;
use windows::core::Interface;

use super::count::{C, COUNT, COUNTS, FRAMES, REBUILT, REFUSED, Refusal, add, refuse};
use super::pool::{self, Kind, Lease, Pool};
use super::stamps::Gpu;
use super::table::{self, PERFORM_UPDATE, Table, Why};
use super::{Build, Level, Origin};
use crate::host::HostList;
use crate::object::This;

/// `..._BUILD_FLAG_ALLOW_COMPACTION`.
pub(super) const ALLOW_COMPACTION: u32 = 0x2;

/// A host structure: a range of the pool and whether the list that builds it
/// was submitted.
pub(super) struct Structure {
    pub(crate) lease: Arc<Lease>,
    submitted: AtomicBool,
    /// The recording of the title list its build was recorded in
    /// ([`in_recording`]; 0: none known).
    recording: u64,
    /// Of a top level: the bottom levels its instances named when it was
    /// submitted, held as long as it is.
    #[allow(dead_code)]
    pub(crate) named: Mutex<Vec<Arc<Structure>>>,
}

impl table::Version for Structure {
    fn submitted(&self) -> bool {
        self.submitted.load(Relaxed)
    }
}

impl Structure {
    /// A structure built in the recording [`in_recording`] runs in.
    pub(super) fn new(lease: Arc<Lease>) -> Arc<Structure> {
        Arc::new(Structure {
            lease,
            submitted: AtomicBool::new(false),
            recording: RECORDING.get(),
            named: Mutex::default(),
        })
    }

    pub(crate) fn gpu(&self) -> u64 {
        self.lease.gpu
    }

    /// Whether its build was recorded in the title list recording `mark`
    /// (not 0): what is recorded after it there runs after it.
    pub(super) fn recorded_in(&self, mark: u64) -> bool {
        mark != 0 && self.recording == mark
    }
}

thread_local! {
    /// The title list recording work at a place of the list runs for
    /// ([`in_recording`]).
    static RECORDING: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Runs `f` (work of ours at a place of a title list, `crate::list::external`)
/// with the structures it makes marked as built in the title list recording
/// `mark` (`crate::draw::State::structures_mark`): a dispatch recorded later
/// in the same recording reads them, whenever the list is submitted
/// (`super::query`).
pub(crate) fn in_recording<R>(mark: u64, f: impl FnOnce() -> R) -> R {
    let was = RECORDING.replace(mark);
    let r = f();
    RECORDING.set(was);
    r
}

static TABLE: LazyLock<Mutex<Table<Structure>>> = LazyLock::new(Default::default);
static STRUCTURES: Mutex<Pool> = Mutex::new(Pool::new(Kind::Structures));
static SCRATCH: Mutex<Pool> = Mutex::new(Pool::new(Kind::Scratch));

/// Card memory the host's structures may hold, in bytes:
/// `VICTORY_DXR_NATIVE_MB` (MiB), 1024 MiB by default. A build past it is
/// refused and counted. (The 8 GB card holds the console's structures too
/// while both are built.)
fn most() -> u64 {
    static MOST: OnceLock<u64> = OnceLock::new();
    *MOST.get_or_init(|| {
        let mb = std::env::var("VICTORY_DXR_NATIVE_MB").ok().and_then(|v| v.trim().parse::<u64>().ok());
        mb.unwrap_or(1024) << 20
    })
}

/// Scratch memory held at most (lists hold theirs until they are reset).
const SCRATCH_MOST: u64 = 1 << 30;

/// Frames a destination nothing builds, refits or names stays for.
const IDLE: u64 = 256;

/// A barrier on every resource: shader writes and builds recorded before it
/// are done before what is recorded after it reads or writes.
pub(crate) fn wait_all(list: &ID3D12GraphicsCommandList4) {
    let barrier = D3D12_RESOURCE_BARRIER {
        Type: D3D12_RESOURCE_BARRIER_TYPE_UAV,
        Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
        Anonymous: D3D12_RESOURCE_BARRIER_0 {
            UAV: ManuallyDrop::new(D3D12_RESOURCE_UAV_BARRIER { pResource: ManuallyDrop::new(None) }),
        },
    };
    // SAFETY: an open list.
    unsafe { list.ResourceBarrier(&[barrier]) };
}

/// A presented frame.
pub(crate) fn frame() {
    if super::native_on() {
        FRAMES.fetch_add(1, Relaxed);
        super::probe::tick();
    }
}

pub(crate) fn record(list: This, origin: Origin, builds: Vec<Build>) {
    if !super::native_on() || builds.is_empty() {
        return;
    }
    for b in &builds {
        let update = b.flags & PERFORM_UPDATE != 0;
        let c = match (&b.level, origin, update) {
            (_, Origin::Rehydrated, _) => C::AskedUnpacked,
            (Level::Bottom { .. }, _, false) => C::AskedBottom,
            (Level::Bottom { .. }, _, true) => C::AskedBottomUpdates,
            (Level::Top { .. }, _, false) => C::AskedTop,
            (Level::Top { .. }, _, true) => C::AskedTopUpdates,
        };
        add(c, 1);
    }
    let n = builds.len();
    let work: crate::list::External = Box::new(move |host| record_on(host, origin, &builds));
    if !crate::list::external(list, work) {
        for _ in 0..n {
            refuse(Refusal::NoList, || format!("list {list:p}"));
        }
    }
}

/// What a recording holds for its list, and says at the list's submission.
#[derive(Default)]
pub(super) struct Held {
    pub(super) leases: Vec<Arc<Lease>>,
    pub(super) made: Vec<Arc<Structure>>,
}

impl Held {
    /// The list holds what it recorded until it is reset, and its builds are
    /// there for what is submitted after it once it is submitted.
    fn give(self, host: &HostList) {
        if self.leases.is_empty() && self.made.is_empty() {
            return;
        }
        host.defer_host(move || {
            for s in &self.made {
                s.submitted.store(true, Relaxed);
            }
            let _ = &self.leases;
        });
    }
}

/// The host's builds of `builds` into the host list `host` is at.
fn record_on(host: &HostList, origin: Origin, builds: &[Build]) -> bool {
    let Some(gpu) = Gpu::get() else {
        for _ in builds {
            refuse(Refusal::NoDevice, String::new);
        }
        return false;
    };
    // SAFETY: a plain query of an open list.
    if unsafe { host.list.GetType() } == D3D12_COMMAND_LIST_TYPE_COPY {
        for _ in builds {
            refuse(Refusal::CopyList, String::new);
        }
        return false;
    }
    let Ok(list) = host.list.cast::<ID3D12GraphicsCommandList4>() else {
        for _ in builds {
            refuse(Refusal::NoDevice, || "no ID3D12GraphicsCommandList4".into());
        }
        return false;
    };
    let frame = crate::frame::current();
    let mut held = Held::default();
    let slot = gpu.begin(&list);
    // What wrote the inputs before (the skinning, the title's copies) is done.
    wait_all(&list);
    // Pages of the console's structures the probe asked for (they are as the
    // title's builds recorded before this left them).
    super::probe::read_pages(host, &list);
    let mut tops = Vec::new();
    {
        let mut table = TABLE.lock().unwrap_or_else(|e| e.into_inner());
        for b in builds.iter().filter(|b| matches!(b.level, Level::Bottom { .. })) {
            if let Err((r, why)) = super::bottom::bottom(host, &list, gpu, &mut table, b, origin, frame, &mut held) {
                refuse(r, || format!("dest {:#x}: {why}", b.dest));
                // What the host had there is not what the title has now.
                table.forget(b.dest);
            }
        }
        if builds.iter().any(|b| matches!(b.level, Level::Top { .. })) {
            // The top levels read the bottom levels' boxes.
            wait_all(&list);
            // Their instances are rewritten at submission to name the newest
            // bottom levels already submitted (`super::top`): the ones built
            // just above are, for a top level after them in the same list, so
            // they are said submitted at a step before the rewrite's (steps
            // run in the order they are put; else the first build of a bottom
            // level was not named by the top level of its own batch, and a
            // rebuilt one was named in its stale version).
            std::mem::take(&mut held).give(host);
        }
        for b in builds.iter().filter(|b| matches!(b.level, Level::Top { .. })) {
            match super::top::top(host, &list, gpu, &mut table, b, frame, &mut held) {
                Ok((s, descs)) => tops.push((b, s, descs)),
                Err((r, why)) => {
                    refuse(r, || format!("dest {:#x}: {why}", b.dest));
                    table.forget(b.dest);
                }
            }
        }
    }
    // What reads the structures from here on sees them built.
    wait_all(&list);
    gpu.end(&list, slot);
    for (b, s, descs) in tops {
        if let Level::Top { count, .. } = b.level {
            super::probe::after_top(host, &list, b.dest, count, &s, descs);
        }
    }
    held.give(host);
    true
}

/// The table, locked.
pub(super) fn table() -> std::sync::MutexGuard<'static, Table<Structure>> {
    TABLE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Card memory for a structure of `len` bytes.
pub(super) fn lease_structure(len: u64) -> Result<Arc<Lease>, Refused> {
    lease(&STRUCTURES, len, most())
}

/// Scratch memory of `len` bytes.
pub(super) fn lease_scratch(len: u64) -> Result<Arc<Lease>, Refused> {
    lease(&SCRATCH, len, SCRATCH_MOST)
}

pub(super) type Refused = (Refusal, String);

/// The host driver's sizes for `inputs`.
pub(super) fn prebuild(
    gpu: &Gpu,
    inputs: &D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS,
) -> Result<D3D12_RAYTRACING_ACCELERATION_STRUCTURE_PREBUILD_INFO, Refused> {
    let mut sizes = D3D12_RAYTRACING_ACCELERATION_STRUCTURE_PREBUILD_INFO::default();
    // SAFETY: the inputs point at descriptions that outlive the call.
    unsafe { gpu.device.GetRaytracingAccelerationStructurePrebuildInfo(inputs, &mut sizes) };
    if sizes.ResultDataMaxSizeInBytes == 0 {
        return Err((Refusal::Prebuild, format!("{} descriptions", inputs.NumDescs)));
    }
    Ok(sizes)
}

fn lease(pool: &'static Mutex<Pool>, len: u64, most: u64) -> Result<Arc<Lease>, Refused> {
    pool::lease(pool, len, most).map_err(|why| (Refusal::Memory, format!("{len} bytes: {why}")))
}

pub(crate) fn copy(list: This, dest: u64, source: u64, mode: i32) {
    if !super::native_on() {
        return;
    }
    add(C::AskedCopies, 1);
    let work: crate::list::External = Box::new(move |host| copy_on(host, dest, source, mode));
    if !crate::list::external(list, work) {
        refuse(Refusal::NoList, || format!("list {list:p}"));
    }
}

/// `D3D12_RAYTRACING_ACCELERATION_STRUCTURE_COPY_MODE_CLONE` and `_COMPACT`.
const CLONE: i32 = 0;
const COMPACT: i32 = 1;

fn copy_on(host: &HostList, dest: u64, source: u64, mode: i32) -> bool {
    let refused = |r: Refusal| {
        refuse(r, || format!("copy {source:#x} -> {dest:#x} mode {mode}"));
        false
    };
    if mode != CLONE && mode != COMPACT {
        return refused(Refusal::CopyMode);
    }
    if Gpu::get().is_none() {
        return refused(Refusal::NoDevice);
    }
    let Ok(list) = host.list.cast::<ID3D12GraphicsCommandList4>() else {
        return refused(Refusal::NoDevice);
    };
    let mut table = TABLE.lock().unwrap_or_else(|e| e.into_inner());
    let Some((src, flags, shape)) = table.get(source).map(|e| (e.current.clone(), e.flags, e.shape.clone())) else {
        table.forget(dest);
        return refused(Refusal::CopySource);
    };
    // A compaction of a structure built without ALLOW_COMPACTION would be
    // refused by the host: a clone holds the same. The copy gets as much
    // memory as the source, which a compaction never needs more than.
    let mode = if mode == COMPACT && flags & ALLOW_COMPACTION != 0 {
        D3D12_RAYTRACING_ACCELERATION_STRUCTURE_COPY_MODE_COMPACT
    } else {
        D3D12_RAYTRACING_ACCELERATION_STRUCTURE_COPY_MODE_CLONE
    };
    let made = match lease(&STRUCTURES, src.lease.len(), most()) {
        Ok(lease) => Structure::new(lease),
        Err((r, why)) => {
            refuse(r, || why);
            table.forget(dest);
            return false;
        }
    };
    wait_all(&list);
    // SAFETY: an open list; both structures are held by the list (below).
    unsafe { list.CopyRaytracingAccelerationStructure(made.gpu(), src.gpu(), mode) };
    wait_all(&list);
    table.built(dest, flags, shape, made.clone(), crate::frame::current());
    drop(table);
    add(C::Copies, 1);
    Held { leases: vec![src.lease.clone()], made: vec![made] }.give(host);
    true
}

/// The five-second line (with [`super::native_on`]): what the title asked,
/// what the host built and refused, and what its structures hold.
pub(crate) fn line() {
    if !super::native_on() {
        return;
    }
    if let Some(gpu) = Gpu::get() {
        gpu.take_times();
    }
    let swept = {
        let mut table = TABLE.lock().unwrap_or_else(|e| e.into_inner());
        table.sweep(crate::frame::current(), IDLE).len() as u64
    };
    add(C::Swept, swept);
    let frames = FRAMES.swap(0, Relaxed).max(1) as f64;
    let c: [f64; COUNTS] = std::array::from_fn(|i| COUNT[i].swap(0, Relaxed) as f64);
    let per = |x: C| c[x as usize] / frames;
    let refused: Vec<(Refusal, u64)> =
        Refusal::ALL.iter().zip(&REFUSED).map(|(&r, n)| (r, n.swap(0, Relaxed))).filter(|&(_, n)| n > 0).collect();
    let rebuilt: Vec<(Why, u64)> =
        Why::ALL.iter().zip(&REBUILT).map(|(&w, n)| (w, n.swap(0, Relaxed))).filter(|&(_, n)| n > 0).collect();
    let (entries, bottom, top) = {
        let table = TABLE.lock().unwrap_or_else(|e| e.into_inner());
        let top = table.entries().filter(|(_, e)| e.shape.top()).count();
        (table.len(), table.len() - top, top)
    };
    let (held, made) = STRUCTURES.lock().unwrap_or_else(|e| e.into_inner()).bytes();
    let (scratch_held, scratch_made) = SCRATCH.lock().unwrap_or_else(|e| e.into_inner()).bytes();
    let mb = |b: u64| b as f64 / f64::from(1 << 20);
    let mut text = format!(
        "host ray-tracing structures a frame (VICTORY_DXR_NATIVE): the title asked {:.1} bottom builds and {:.1} updates, \
         {:.1} top builds and {:.1} updates, {:.2} unpackings, {:.2} copies; the host built {:.1} bottom levels \
         ({:.0} triangles, {:.0} boxes), refit {:.1} ({:.1} in place, {:.0} triangles) and {:.2} copies; top levels {:.1} ({:.0} instances, {:.0} of them naming no host structure, {:.0} unreadable at submission)",
        per(C::AskedBottom),
        per(C::AskedBottomUpdates),
        per(C::AskedTop),
        per(C::AskedTopUpdates),
        per(C::AskedUnpacked),
        per(C::AskedCopies),
        per(C::Built),
        per(C::Triangles),
        per(C::Boxes),
        per(C::Refit),
        per(C::RefitInPlace),
        per(C::RefitTriangles),
        per(C::Copies),
        per(C::TopBuilt),
        per(C::Instances),
        per(C::InstancesUnknown),
        per(C::InstancesUnreadable),
    );
    if per(C::Rebuilt) > 0.0 {
        let _ = write!(text, "; of the builds {:.1} were updates built from scratch (", per(C::Rebuilt));
        for (k, (w, n)) in rebuilt.iter().enumerate() {
            let _ = write!(text, "{}{}: {:.1}", if k > 0 { ", " } else { "" }, w.name(), *n as f64 / frames);
        }
        text.push(')');
    }
    let timed = c[C::Timed as usize];
    let _ = write!(
        text,
        "; card time of the host's builds {:.3} ms a frame ({:.0} calls timed, {:.3} ms each)",
        c[C::CardNanos as usize] / 1e6 / frames,
        timed,
        if timed > 0.0 { c[C::CardNanos as usize] / 1e6 / timed } else { 0.0 },
    );
    if super::expand::on() {
        let _ = write!(
            text,
            "; expanded (VICTORY_DXR_EXPAND) {:.1} index-53 lists ({:.0} triangles) and {:.1} quantized position lists \
             ({:.0} vertices), {:.2} MB of upload memory, {:.1} in pages of by-address memory the card holds; at submission \
             {:.2} not readable, {:.0} triangles made degenerate, {:.0} positions not finite",
            per(C::ExpandedIndexLists),
            per(C::ExpandedTriangles),
            per(C::ExpandedPositionLists),
            per(C::ExpandedVertices),
            per(C::ExpandedBytes) / f64::from(1 << 20),
            per(C::ExpandedByAddress),
            per(C::ExpandedUnreadable),
            per(C::ExpandedDegenerate),
            per(C::ExpandedNotFinite),
        );
    }
    if c[C::ShadersCopy as usize] > 0.0 {
        let _ = write!(text, "; {:.1} reads from the copy shaders wrote, not the one a draw takes", per(C::ShadersCopy));
    }
    if c[C::ByAddress as usize] > 0.0 {
        let _ = write!(
            text,
            "; {:.1} reads of memory outside buffers that by-address shaders hold in pages of the card (what they stored there is not in the copy read)",
            per(C::ByAddress)
        );
    }
    if refused.is_empty() {
        text.push_str("; nothing refused");
    } else {
        let all: u64 = refused.iter().map(|r| r.1).sum();
        let _ = write!(text, "; refused {:.1} (", all as f64 / frames);
        for (k, (r, n)) in refused.iter().enumerate() {
            let _ = write!(text, "{}{}: {:.1}", if k > 0 { ", " } else { "" }, r.name(), *n as f64 / frames);
        }
        text.push(')');
    }
    let _ = write!(
        text,
        "; host structures now {entries} ({bottom} bottom, {top} top), {:.1} MB held of {:.1} MB of card memory, \
         scratch {:.1} MB held of {:.1} MB; {swept} let go as unused for {IDLE} frames since the last line",
        mb(held),
        mb(made),
        mb(scratch_held),
        mb(scratch_made),
    );
    tracing::info!("{text}");
}
