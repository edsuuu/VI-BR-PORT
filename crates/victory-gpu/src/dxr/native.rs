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
//!    title memory ([`resolve`]) and has them brought up to date at
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
//! the copies holding its range was ([`resolve`]); with more than one such
//! copy it is refused and counted; with none, it reads what a draw would.
//!
//! **Lifetimes.** A structure's memory is held by the table while the title
//! names it and by every list that builds, refits, copies or reads it, until
//! that list is reset; then it goes back to the pool. Nothing is freed under
//! a list that may still run.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};

use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT;
use windows::core::Interface;

use super::pool::{self, Kind, Lease, Pool};
use super::table::{self, Decision, GeometryShape, PERFORM_UPDATE, Shape, Table, Why};
use super::{Build, Geometry, Level, Origin, Triangles};
use crate::host::HostList;
use crate::object::This;

/// `..._BUILD_FLAG_PREFER_FAST_TRACE` and `_FAST_BUILD`, which Direct3D does
/// not take together.
const FAST_TRACE: u32 = 0x4;
const FAST_BUILD: u32 = 0x8;
/// `..._BUILD_FLAG_ALLOW_COMPACTION`.
const ALLOW_COMPACTION: u32 = 0x2;

/// A host structure: a range of the pool and whether the list that builds it
/// was submitted.
pub(super) struct Structure {
    pub(crate) lease: Arc<Lease>,
    submitted: AtomicBool,
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
    pub(super) fn new(lease: Arc<Lease>) -> Arc<Structure> {
        Arc::new(Structure { lease, submitted: AtomicBool::new(false), named: Mutex::default() })
    }

    pub(crate) fn gpu(&self) -> u64 {
        self.lease.gpu
    }
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

/// Why a build was not done on the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Refusal {
    /// The title's list records nowhere (no host device or list).
    NoList,
    /// The host device has no ray tracing (or not the tier a format needs).
    NoDevice,
    /// A copy list (structures are built on direct and compute lists).
    CopyList,
    /// The geometry descriptions could not all be read.
    NotWhole,
    VertexFormat,
    IndexFormat,
    /// An address or stride off what Direct3D asks.
    Unaligned,
    /// No host copy holds the data.
    NoCopy,
    /// More than one host copy holding the data was written by shaders.
    SeveralWritten,
    OtherGeometry,
    /// The host's driver sizes no structure for the inputs.
    Prebuild,
    /// The memory ceiling, or no memory for another chunk.
    Memory,
    /// Instances laid out other than in an array.
    InstanceLayout,
    /// A copy whose source the host has no structure for.
    CopySource,
    /// A copy mode other than a clone or a compaction.
    CopyMode,
}

impl Refusal {
    const ALL: [Refusal; 15] = [
        Refusal::NoList,
        Refusal::NoDevice,
        Refusal::CopyList,
        Refusal::NotWhole,
        Refusal::VertexFormat,
        Refusal::IndexFormat,
        Refusal::Unaligned,
        Refusal::NoCopy,
        Refusal::SeveralWritten,
        Refusal::OtherGeometry,
        Refusal::Prebuild,
        Refusal::Memory,
        Refusal::InstanceLayout,
        Refusal::CopySource,
        Refusal::CopyMode,
    ];

    fn name(self) -> &'static str {
        match self {
            Refusal::NoList => "the list records nowhere",
            Refusal::NoDevice => "no host ray tracing",
            Refusal::CopyList => "a copy list",
            Refusal::NotWhole => "descriptions not all readable",
            Refusal::VertexFormat => "a vertex format the host does not build from",
            Refusal::IndexFormat => "an index format the host does not build from",
            Refusal::Unaligned => "data off its alignment",
            Refusal::NoCopy => "data in no host copy",
            Refusal::SeveralWritten => "data in several host copies shaders write",
            Refusal::OtherGeometry => "a geometry of another kind",
            Refusal::Prebuild => "the host sizes no structure",
            Refusal::Memory => "no card memory (VICTORY_DXR_NATIVE_MB)",
            Refusal::InstanceLayout => "instances as an array of pointers",
            Refusal::CopySource => "a copy of a structure the host has not",
            Refusal::CopyMode => "a copy mode other than clone or compact",
        }
    }
}

/// What is counted between two lines.
#[derive(Clone, Copy)]
pub(super) enum C {
    AskedBottom,
    AskedBottomUpdates,
    AskedTop,
    AskedTopUpdates,
    AskedUnpacked,
    AskedCopies,
    Built,
    Refit,
    RefitInPlace,
    Rebuilt,
    Triangles,
    RefitTriangles,
    Boxes,
    Copies,
    TopBuilt,
    Instances,
    /// Instances naming a bottom level the host has not got (made inactive).
    InstancesUnknown,
    /// Of them, in memory the CPU could not read at submission.
    InstancesUnreadable,
    /// Data read from the copy shaders wrote where a draw would read another.
    ShadersCopy,
    /// Data outside title buffers with a page in an arena of by-address
    /// memory (what shaders stored there by address the copy read lacks).
    ByAddress,
    CardNanos,
    Timed,
    Swept,
}

const COUNTS: usize = C::Swept as usize + 1;
static COUNT: [AtomicU64; COUNTS] = [const { AtomicU64::new(0) }; COUNTS];
static REFUSED: [AtomicU64; Refusal::ALL.len()] = [const { AtomicU64::new(0) }; Refusal::ALL.len()];
static REBUILT: [AtomicU64; Why::ALL.len()] = [const { AtomicU64::new(0) }; Why::ALL.len()];
static FRAMES: AtomicU64 = AtomicU64::new(0);

pub(super) fn add(c: C, n: u64) {
    COUNT[c as usize].fetch_add(n, Relaxed);
}

/// Refusals said in full, of each reason.
const SAID: u64 = 4;

fn refuse(r: Refusal, what: impl FnOnce() -> String) {
    let i = Refusal::ALL.iter().position(|&x| x == r).unwrap_or(0);
    REFUSED[i].fetch_add(1, Relaxed);
    if SAID_ALL[i].fetch_add(1, Relaxed) < SAID {
        tracing::info!("host ray tracing: refused ({}): {}", r.name(), what());
    }
}

/// Refusals said in full so far, by reason (the counts above are taken by
/// every line).
static SAID_ALL: [AtomicU64; Refusal::ALL.len()] = [const { AtomicU64::new(0) }; Refusal::ALL.len()];

/// What the host's builds need of the device: its ray-tracing interface,
/// and timestamps.
pub(super) struct Gpu {
    device: ID3D12Device5,
    stamps: ID3D12QueryHeap,
    /// Where the timestamps are resolved to, mapped.
    back: ID3D12Resource,
    data: *const u64,
    next: AtomicU64,
    /// Pairs recorded and not read yet: the slot and the frame.
    pending: Mutex<VecDeque<(u32, u64)>>,
    frequency: u64,
}

// SAFETY: D3D12 objects are free-threaded; `data` is a mapping read only.
unsafe impl Send for Gpu {}
// SAFETY: as above.
unsafe impl Sync for Gpu {}

/// Timestamp pairs in the ring.
const PAIRS: u32 = 4096;

impl Gpu {
    fn get() -> Option<&'static Gpu> {
        static GPU: OnceLock<Option<Gpu>> = OnceLock::new();
        GPU.get_or_init(|| {
            let host = crate::host::host()?;
            if super::tier() < 10 {
                tracing::warn!("host ray tracing: the host has none; VICTORY_DXR_NATIVE builds nothing");
                return None;
            }
            let device: ID3D12Device5 = host.device.cast().ok()?;
            let mut stamps: Option<ID3D12QueryHeap> = None;
            // SAFETY: plain object creation.
            unsafe {
                device
                    .CreateQueryHeap(
                        &D3D12_QUERY_HEAP_DESC { Type: D3D12_QUERY_HEAP_TYPE_TIMESTAMP, Count: 2 * PAIRS, NodeMask: 0 },
                        &mut stamps,
                    )
                    .ok()?;
            }
            let back = readback(&host.device, u64::from(2 * PAIRS) * 8)?;
            let mut data = std::ptr::null_mut();
            // SAFETY: a readback buffer of ours, mapped for good.
            unsafe { back.Map(0, None, Some(&mut data)).ok()? };
            // SAFETY: the mapping is that long; zero means "not written".
            unsafe { std::ptr::write_bytes(data.cast::<u64>(), 0, 2 * PAIRS as usize) };
            Some(Gpu {
                device,
                stamps: stamps?,
                back,
                data: data.cast_const().cast(),
                next: AtomicU64::new(0),
                pending: Mutex::default(),
                frequency: host.timestamp_frequency(),
            })
        })
        .as_ref()
    }

    /// A timestamp before the builds: the pair's slot.
    fn begin(&self, list: &ID3D12GraphicsCommandList4) -> u32 {
        let slot = (self.next.fetch_add(1, Relaxed) % u64::from(PAIRS)) as u32;
        // SAFETY: an open list, our query heap.
        unsafe { list.EndQuery(&self.stamps, D3D12_QUERY_TYPE_TIMESTAMP, 2 * slot) };
        slot
    }

    /// The one after, resolved where the line reads it a few frames later.
    fn end(&self, list: &ID3D12GraphicsCommandList4, slot: u32) {
        // SAFETY: as above; the readback buffer is ours and always a copy
        // destination.
        unsafe {
            list.EndQuery(&self.stamps, D3D12_QUERY_TYPE_TIMESTAMP, 2 * slot + 1);
            list.ResolveQueryData(&self.stamps, D3D12_QUERY_TYPE_TIMESTAMP, 2 * slot, 2, &self.back, u64::from(slot) * 16);
        }
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        if pending.len() < PAIRS as usize / 2 {
            pending.push_back((slot, crate::frame::current()));
        }
    }

    /// Adds what the pairs of frames long gone measured.
    fn take_times(&self) {
        let now = crate::frame::current();
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        while let Some(&(slot, frame)) = pending.front() {
            if frame + 8 > now {
                break;
            }
            pending.pop_front();
            // SAFETY: inside the mapping; the card wrote these frames ago.
            let (a, b) = unsafe {
                let p = self.data.add(2 * slot as usize).cast_mut();
                let pair = (p.read_volatile(), p.add(1).read_volatile());
                p.write_volatile(0);
                p.add(1).write_volatile(0);
                pair
            };
            // (A list never submitted wrote nothing.)
            if a != 0 && b > a && self.frequency > 0 {
                add(C::CardNanos, ((b - a) as u128 * 1_000_000_000 / u128::from(self.frequency)) as u64);
                add(C::Timed, 1);
            }
        }
    }
}

/// A readback buffer of `size` bytes.
fn readback(device: &ID3D12Device, size: u64) -> Option<ID3D12Resource> {
    let desc = D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
        Width: size,
        Height: 1,
        DepthOrArraySize: 1,
        MipLevels: 1,
        SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
        ..Default::default()
    };
    let mut buffer: Option<ID3D12Resource> = None;
    // SAFETY: a plain committed buffer of ours.
    unsafe {
        device
            .CreateCommittedResource(
                &D3D12_HEAP_PROPERTIES { Type: D3D12_HEAP_TYPE_READBACK, ..Default::default() },
                D3D12_HEAP_FLAG_NONE,
                &desc,
                D3D12_RESOURCE_STATE_COPY_DEST,
                None,
                &mut buffer,
            )
            .ok()?;
    }
    buffer
}

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
            if let Err((r, why)) = bottom(host, &list, gpu, &mut table, b, origin, frame, &mut held) {
                refuse(r, || format!("dest {:#x}: {why}", b.dest));
                // What the host had there is not what the title has now.
                table.forget(b.dest);
            }
        }
        if builds.iter().any(|b| matches!(b.level, Level::Top { .. })) {
            // The top levels read the bottom levels' boxes.
            wait_all(&list);
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

/// Bytes of a vertex's position in a vertex format the host builds from, and
/// the alignment of its components; formats of ray-tracing tier 1.1 only on
/// a host that has it.
fn vertex_format(format: u32) -> Option<(u64, u64)> {
    let tier_1_1 = super::tier() >= super::TIER_1_1;
    match format {
        // R32G32B32_FLOAT, R32G32_FLOAT
        6 => Some((12, 4)),
        16 => Some((8, 4)),
        // R16G16B16A16_FLOAT, R16G16_FLOAT, R16G16B16A16_SNORM, R16G16_SNORM
        10 | 13 => Some((8, 2)),
        34 | 37 => Some((4, 2)),
        // Tier 1.1: R16G16B16A16_UNORM, R16G16_UNORM, R10G10B10A2_UNORM,
        // R8G8B8A8_UNORM/SNORM, R8G8_UNORM/SNORM.
        11 if tier_1_1 => Some((8, 2)),
        35 if tier_1_1 => Some((4, 2)),
        24 if tier_1_1 => Some((4, 4)),
        28 | 31 if tier_1_1 => Some((4, 1)),
        49 | 51 if tier_1_1 => Some((2, 1)),
        _ => None,
    }
}

/// The host GPU address of `len` bytes of title memory at `address`, in the
/// host copy a build is to read (see the module), brought up to date when
/// `host`'s list is submitted.
fn resolve(host: &HostList, address: u64, len: u64) -> Result<u64, Refused> {
    let mut written: Vec<Arc<crate::shadow::Shadow>> = Vec::new();
    for copy in crate::shadow::copies_over(address, len) {
        if copy.written() && !written.iter().any(|w| Arc::ptr_eq(w, &copy)) {
            written.push(copy);
        }
    }
    if written.len() > 1 {
        return Err((Refusal::SeveralWritten, format!("{address:#x}+{len:#x} in {} copies", written.len())));
    }
    let drawn = crate::shadow::find_any(address, len);
    let copy = match (written.pop(), drawn) {
        (Some(w), drawn) => {
            if !drawn.as_ref().is_some_and(|d| Arc::ptr_eq(d, &w)) {
                add(C::ShadersCopy, 1);
            }
            w
        }
        (None, Some(d)) => d,
        (None, None) => return Err((Refusal::NoCopy, format!("{address:#x}+{len:#x}"))),
    };
    if copy.offset_in(address).is_none() || copy.left(address) < len {
        return Err((Refusal::NoCopy, format!("{address:#x}+{len:#x} past its copy")));
    }
    // Outside title buffers, a page the shaders that find memory by address
    // hold in an arena of ours may have bytes they stored that only the card
    // has (`crate::memory`): counted, read as a draw would.
    const PAGE: u64 = 1 << 16;
    if crate::shadow::in_buffer(address, len).is_none()
        && (address / PAGE..=(address + len - 1) / PAGE).take(64).any(|p| crate::memory::home(p * PAGE).is_some_and(|h| h.2))
    {
        add(C::ByAddress, 1);
    }
    copy.refresh(host, address, len);
    Ok(copy.gpu(address))
}

/// The host's description of a triangle geometry, and its shape.
fn triangles(host: &HostList, t: &Triangles) -> Result<(D3D12_RAYTRACING_GEOMETRY_DESC, GeometryShape), Refused> {
    let (size, align) = vertex_format(t.vertex_format)
        .ok_or_else(|| (Refusal::VertexFormat, crate::dxr::format_name(t.vertex_format)))?;
    let index = match t.index_format {
        0 => 0,
        57 => 2,
        42 => 4,
        f => return Err((Refusal::IndexFormat, crate::dxr::format_name(f))),
    };
    let unaligned = !t.vertex_stride.is_multiple_of(align)
        || !t.vertex_buffer.is_multiple_of(align)
        || (index > 0 && !t.index_buffer.is_multiple_of(index))
        || !t.transform.is_multiple_of(16);
    if unaligned {
        return Err((
            Refusal::Unaligned,
            format!("vertices {:#x} stride {}, indices {:#x}, transform {:#x}", t.vertex_buffer, t.vertex_stride, t.index_buffer, t.transform),
        ));
    }
    let vertices = match t.vertex_count {
        0 => 0,
        n => resolve(host, t.vertex_buffer, t.vertex_stride * u64::from(n - 1) + size)?,
    };
    let indices = if index > 0 && t.index_count > 0 { resolve(host, t.index_buffer, index * u64::from(t.index_count))? } else { 0 };
    let transform = if t.transform != 0 { resolve(host, t.transform, 48)? } else { 0 };
    let desc = D3D12_RAYTRACING_GEOMETRY_DESC {
        Type: D3D12_RAYTRACING_GEOMETRY_TYPE_TRIANGLES,
        Flags: D3D12_RAYTRACING_GEOMETRY_FLAGS(t.flags as i32),
        Anonymous: D3D12_RAYTRACING_GEOMETRY_DESC_0 {
            Triangles: D3D12_RAYTRACING_GEOMETRY_TRIANGLES_DESC {
                Transform3x4: transform,
                IndexFormat: DXGI_FORMAT(t.index_format as i32),
                VertexFormat: DXGI_FORMAT(t.vertex_format as i32),
                IndexCount: if index > 0 { t.index_count } else { 0 },
                VertexCount: t.vertex_count,
                IndexBuffer: indices,
                VertexBuffer: D3D12_GPU_VIRTUAL_ADDRESS_AND_STRIDE { StartAddress: vertices, StrideInBytes: t.vertex_stride },
            },
        },
    };
    let shape = GeometryShape {
        kind: 0,
        flags: t.flags,
        index_format: t.index_format,
        vertex_format: t.vertex_format,
        index_count: t.index_count,
        vertex_count: t.vertex_count,
        transform: t.transform != 0,
    };
    Ok((desc, shape))
}

/// The triangles a geometry holds.
fn triangle_count(t: &Triangles) -> u64 {
    u64::from(if t.index_format == 0 { t.vertex_count } else { t.index_count }) / 3
}

/// The flags the host builds with: the title's, but never both preferences
/// (Direct3D refuses that; the walks matter more), and for an unpacked
/// structure (built once, walked for as long as it is streamed in) a
/// preference for fast walks if it has none.
pub(super) fn host_flags(flags: u32, origin: Origin) -> u32 {
    let mut flags = flags & !PERFORM_UPDATE;
    if flags & FAST_TRACE != 0 {
        flags &= !FAST_BUILD;
    }
    if origin == Origin::Rehydrated && flags & (FAST_TRACE | FAST_BUILD) == 0 {
        flags |= FAST_TRACE;
    }
    flags
}

/// Bottom levels said in full (the host's sizes beside the title's ask).
const DESCRIBED: u64 = 64;
static DESCRIBED_SO_FAR: AtomicU64 = AtomicU64::new(0);

#[allow(clippy::too_many_arguments)]
fn bottom(
    host: &HostList,
    list: &ID3D12GraphicsCommandList4,
    gpu: &Gpu,
    table: &mut Table<Structure>,
    b: &Build,
    origin: Origin,
    frame: u64,
    held: &mut Held,
) -> Result<(), Refused> {
    let Level::Bottom { geometries, whole } = &b.level else {
        return Ok(());
    };
    if !whole {
        return Err((Refusal::NotWhole, format!("{} geometries read", geometries.len())));
    }
    let mut descs = Vec::with_capacity(geometries.len());
    let mut shapes = Vec::with_capacity(geometries.len());
    let (mut tris, mut boxes) = (0u64, 0u64);
    for g in geometries {
        match *g {
            Geometry::Triangles(t) => {
                let (desc, shape) = triangles(host, &t)?;
                tris += triangle_count(&t);
                descs.push(desc);
                shapes.push(shape);
            }
            Geometry::Boxes { flags, count, start, stride } => {
                if !start.is_multiple_of(8) || !stride.is_multiple_of(8) || stride < 24 {
                    return Err((Refusal::Unaligned, format!("boxes at {start:#x} stride {stride}")));
                }
                let at = if count > 0 { resolve(host, start, stride * (count - 1) + 24)? } else { 0 };
                boxes += count;
                descs.push(D3D12_RAYTRACING_GEOMETRY_DESC {
                    Type: D3D12_RAYTRACING_GEOMETRY_TYPE_PROCEDURAL_PRIMITIVE_AABBS,
                    Flags: D3D12_RAYTRACING_GEOMETRY_FLAGS(flags as i32),
                    Anonymous: D3D12_RAYTRACING_GEOMETRY_DESC_0 {
                        AABBs: D3D12_RAYTRACING_GEOMETRY_AABBS_DESC {
                            AABBCount: count,
                            AABBs: D3D12_GPU_VIRTUAL_ADDRESS_AND_STRIDE { StartAddress: at, StrideInBytes: stride },
                        },
                    },
                });
                shapes.push(GeometryShape {
                    kind: 1,
                    flags,
                    index_format: 0,
                    vertex_format: 0,
                    index_count: 0,
                    vertex_count: count as u32,
                    transform: false,
                });
            }
            Geometry::Other { kind, .. } => return Err((Refusal::OtherGeometry, format!("type {kind}"))),
        }
    }
    let update = origin == Origin::Batched && b.flags & PERFORM_UPDATE != 0;
    let flags = host_flags(b.flags, origin);
    let shape = Shape::Bottom(shapes);
    let decision = table::decide(update, b.dest, b.source, flags, &shape, table);
    let refit = matches!(decision, Decision::Refit { .. });
    let inputs = D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS {
        Type: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_TYPE_BOTTOM_LEVEL,
        Flags: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAGS((flags | if refit { PERFORM_UPDATE } else { 0 }) as i32),
        NumDescs: descs.len() as u32,
        DescsLayout: D3D12_ELEMENTS_LAYOUT_ARRAY,
        Anonymous: D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS_0 { pGeometryDescs: descs.as_ptr() },
    };
    let sizes = prebuild(gpu, &inputs)?;
    let (dest, source, made) = match decision {
        Decision::Refit { in_place } => {
            let from = if b.source == 0 { b.dest } else { b.source };
            let src = table.get(from).map(|e| e.current.clone()).ok_or((Refusal::Prebuild, "a source gone".to_string()))?;
            held.leases.push(src.lease.clone());
            if in_place {
                (src.gpu(), src.gpu(), None)
            } else {
                let lease = lease(&STRUCTURES, sizes.ResultDataMaxSizeInBytes.max(src.lease.len()), most())?;
                let s = Structure::new(lease);
                (s.gpu(), src.gpu(), Some(s))
            }
        }
        Decision::Build | Decision::Rebuild(_) => {
            let s = Structure::new(lease(&STRUCTURES, sizes.ResultDataMaxSizeInBytes, most())?);
            (s.gpu(), 0, Some(s))
        }
    };
    let scratch_len = if refit { sizes.UpdateScratchDataSizeInBytes } else { sizes.ScratchDataSizeInBytes };
    let scratch = lease(&SCRATCH, scratch_len, SCRATCH_MOST)?;
    let desc = D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_DESC {
        DestAccelerationStructureData: dest,
        Inputs: inputs,
        SourceAccelerationStructureData: source,
        ScratchAccelerationStructureData: scratch.gpu,
    };
    // SAFETY: an open list; the description and the geometry descriptions it
    // points at outlive the call; every address is of a buffer the list holds
    // until it is reset (the copies by their refresh, ours by `held`), each in
    // the state a build needs.
    unsafe { list.BuildRaytracingAccelerationStructure(&desc, None) };
    held.leases.push(scratch);
    match (decision, made) {
        (Decision::Refit { in_place: true }, _) => {
            table.refit_in_place(b.dest, frame);
            add(C::Refit, 1);
            add(C::RefitInPlace, 1);
            add(C::RefitTriangles, tris);
        }
        (_, Some(s)) => {
            table.built(b.dest, flags, shape, s.clone(), frame);
            held.made.push(s);
            match decision {
                Decision::Refit { .. } => {
                    add(C::Refit, 1);
                    add(C::RefitTriangles, tris);
                }
                Decision::Rebuild(why) => {
                    add(C::Built, 1);
                    add(C::Rebuilt, 1);
                    add(C::Triangles, tris);
                    REBUILT[Why::ALL.iter().position(|&w| w == why).unwrap_or(0)].fetch_add(1, Relaxed);
                }
                Decision::Build => {
                    add(C::Built, 1);
                    add(C::Triangles, tris);
                }
            }
        }
        (_, None) => {}
    }
    add(C::Boxes, boxes);
    if DESCRIBED_SO_FAR.fetch_add(1, Relaxed) < DESCRIBED {
        tracing::info!(
            "host ray tracing: bottom level dest {:#x} ({origin:?}, {} geometries, {tris} triangles, {boxes} boxes, flags {:#x}): {decision:?}; host sizes result {} scratch {} update scratch {}",
            b.dest,
            descs.len(),
            flags,
            sizes.ResultDataMaxSizeInBytes,
            sizes.ScratchDataSizeInBytes,
            sizes.UpdateScratchDataSizeInBytes,
        );
    }
    Ok(())
}

/// The host driver's sizes for `inputs`.
pub(super) fn prebuild(gpu: &Gpu, inputs: &D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS) -> Result<D3D12_RAYTRACING_ACCELERATION_STRUCTURE_PREBUILD_INFO, Refused> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_host_builds_with_the_titles_flags_but_never_both_preferences() {
        // The update bit is the decision's, not the build's.
        assert_eq!(host_flags(0x1 | PERFORM_UPDATE, Origin::Batched), 0x1);
        // Both preferences: the fast walk wins.
        assert_eq!(host_flags(FAST_TRACE | FAST_BUILD | ALLOW_COMPACTION, Origin::Batched), FAST_TRACE | ALLOW_COMPACTION);
        // Unpacked structures are walked for long: fast walks unless asked otherwise.
        assert_eq!(host_flags(ALLOW_COMPACTION, Origin::Rehydrated), FAST_TRACE | ALLOW_COMPACTION);
        assert_eq!(host_flags(FAST_BUILD, Origin::Rehydrated), FAST_BUILD);
        assert_eq!(host_flags(0, Origin::Batched), 0);
    }

    #[test]
    fn vertex_formats_of_tier_one_are_taken_and_others_named() {
        assert_eq!(vertex_format(6), Some((12, 4)));
        assert_eq!(vertex_format(10), Some((8, 2)));
        assert_eq!(vertex_format(2), None);
        assert_eq!(crate::dxr::format_name(2), "R32G32B32A32_FLOAT");
        assert_eq!(triangle_count(&Triangles { index_format: 57, index_count: 300, vertex_count: 7, ..Default::default() }), 100);
        assert_eq!(triangle_count(&Triangles { vertex_count: 9, ..Default::default() }), 3);
    }
}
