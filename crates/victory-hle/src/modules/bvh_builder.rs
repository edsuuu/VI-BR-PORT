//! `XboxBvhBuilder.dll`: the console's ray-tracing structure builder, a C++
//! class the title makes once and calls through its import table ("this" in
//! the first register). The title imports seven of its twelve exports (its
//! import table, retail build): the constructor and destructor, the batched
//! build, the copy, the prebuild sizes, the batched unpacking of shipped
//! structures and a Morton-code setting. Their inputs are Direct3D's own
//! descriptions of what to build (triangles, boxes, instances by address),
//! which is what lets the host's ray tracing take the builder's place one
//! day (`docs` of the ray-tracing work: the builder taken over).
//!
//! With `VICTORY_DXR_TRACE=1` (or `all`) or `VICTORY_DXR_NATIVE=shadow` (or
//! `1`) the title's imports are bound to the functions here ([`Interpose`]);
//! with neither (the default) they are bound as before. The functions call
//! the title's own and, around that:
//!
//! - with the trace, the listening post: they log what each call asks and
//!   count it for the five-second report (`victory_gpu::rtcalls`);
//! - with the host's own structures (`victory_gpu::dxr`), they hand the
//!   builds, unpackings and copies they decode to the host's side once the
//!   title's builder has recorded its own, at the same place of the same
//!   list ([`host_build`]). `1` stops nothing yet: the title's builder runs
//!   in both modes, until no ray program walks its structures.
//!
//! Nothing the title or its builder sees changes.
//!
//! The descriptions are decoded in [`desc`], the console's own layouts as
//! the builder's decompiled code reads them; the first calls are dumped in
//! hex all the same. Beside the call lines, a line at most every five
//! seconds says what was seen since the start ([`Seen`]): the extension
//! records of the builds, the shipped data of the unpacked structures, and
//! the kinds and opacity of the geometries.
// The exported names are the title's.
#![allow(non_snake_case)]
// Title code calls these through its import table.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

pub mod desc;

use std::collections::HashMap;
use std::ffi::c_void;
use std::fmt::Write as _;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::time::{Duration, Instant};

use victory_gpu::dxr;
use victory_gpu::rtcalls::{self, Count, Trace};
use victory_loader::{ImportResolver, Provenance, Resolution};
use victory_pe::ImportSymbol;

use desc::{
    BATCHED, BUILD_DESC, Batched, BuildDesc, DEHYDRATED, Dehydrated, Geometry, INPUTS, INSTANCE,
    Inputs, Instance, Memory, POSTBUILD, Placement, Postbuild, PrebuildInfo, Record, STREAM,
    Stream,
};

/// The DLL the title imports the builder from.
pub const BUILDER_DLL: &str = "XboxBvhBuilder.dll";

const CONSTRUCT: usize = 0;
const DESTROY: usize = 1;
const BUILD: usize = 2;
const COPY: usize = 3;
const PREBUILD: usize = 4;
const REHYDRATE: usize = 5;
const MORTON: usize = 6;

/// The seven imports, as the title's import table spells them.
const NAMES: [&str; 7] = [
    "??0XboxBvhBuilder@@QEAA@PEAUID3D12Device11@@_N@Z",
    "??1XboxBvhBuilder@@QEAA@XZ",
    "?BuildRaytracingAccelerationStructureBatched2X@XboxBvhBuilder@@QEAAXPEAUID3D12GraphicsCommandList6@@IPEBUD3D12XBOX_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_BATCHED_DESC2@@@Z",
    "?CopyRaytracingAccelerationStructure@XboxBvhBuilder@@QEAAXPEAUID3D12GraphicsCommandList4@@_K1W4D3D12_RAYTRACING_ACCELERATION_STRUCTURE_COPY_MODE@@@Z",
    "?GetRaytracingAccelerationStructurePrebuildInfo@XboxBvhBuilder@@QEAAXPEBUD3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS@@PEBUD3D12XBOX_XDXR_STREAM_DESC@@PEAUD3D12_RAYTRACING_ACCELERATION_STRUCTURE_PREBUILD_INFO@@@Z",
    "?RehydrateRaytracingAccelerationStructureBatched2X@XboxBvhBuilder@@QEAAJPEAUID3D12GraphicsCommandList8@@IPEBUD3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_DESC@@PEB_K@Z",
    "?Set64BitMortonCodes@XboxBvhBuilder@@QEAAX_N@Z",
];

const KINDS: [&str; 7] = [
    "construct",
    "destroy",
    "build",
    "copy",
    "prebuild",
    "rehydrate",
    "morton",
];

/// The title's own functions, by [`NAMES`].
static ORIGINALS: [AtomicUsize; 7] = [const { AtomicUsize::new(0) }; 7];

// Member functions: the object first, then the arguments, in the host's one
// x64 calling convention. A C++ `bool` is passed as a byte.
type Construct = unsafe extern "system" fn(*mut c_void, *mut c_void, u8) -> *mut c_void;
type Destroy = unsafe extern "system" fn(*mut c_void);
type Build = unsafe extern "system" fn(*mut c_void, *mut c_void, u32, *const u8);
type Copy = unsafe extern "system" fn(*mut c_void, *mut c_void, u64, u64, i32);
type Prebuild = unsafe extern "system" fn(*mut c_void, *const u8, *const u8, *mut PrebuildInfo);
type Rehydrate =
    unsafe extern "system" fn(*mut c_void, *mut c_void, u32, *const u8, *const u64) -> i32;
type Morton = unsafe extern "system" fn(*mut c_void, u8);

/// Binds the title's imports of [`BUILDER_DLL`] to the functions here when
/// `VICTORY_DXR_TRACE` or `VICTORY_DXR_NATIVE` is on ([`interposed`]),
/// keeping what `inner` binds them to (the title's own DLL) for them to
/// call; otherwise binds as `inner` does.
pub struct Interpose<'a> {
    pub inner: &'a dyn ImportResolver,
}

/// Whether the title's imports go through ours: for the listening post
/// (`VICTORY_DXR_TRACE`) or for the host's own structures
/// (`VICTORY_DXR_NATIVE`). Neither by default.
fn interposed() -> bool {
    traced() || dxr::native_on()
}

/// Whether the listening post logs and counts (`VICTORY_DXR_TRACE`).
fn traced() -> bool {
    rtcalls::trace() != Trace::Off
}

impl ImportResolver for Interpose<'_> {
    fn resolve(&self, dll: &str, symbol: &ImportSymbol) -> Resolution {
        let found = self.inner.resolve(dll, symbol);
        if !dll.eq_ignore_ascii_case(BUILDER_DLL) || !interposed() {
            return found;
        }
        // Only what the title's DLL itself provides is wrapped: a stub for a
        // missing function has nothing behind it to call.
        match (&found, symbol) {
            (
                &Resolution::Found {
                    address,
                    provenance: Provenance::Title,
                },
                ImportSymbol::Name { name, .. },
            ) => match interpose(name, address) {
                Some(ours) => Resolution::Found {
                    address: ours,
                    provenance: Provenance::Hle,
                },
                None => found,
            },
            _ => found,
        }
    }
}

/// Keeps `original` as the title's `name` and gives ours, if we have one.
fn interpose(name: &str, original: usize) -> Option<usize> {
    let i = NAMES.iter().position(|n| *n == name)?;
    if original == 0 {
        return None;
    }
    ORIGINALS[i].store(original, Relaxed);
    let ours = [
        construct as *const () as usize,
        destroy as *const () as usize,
        build_batched as *const () as usize,
        copy_structure as *const () as usize,
        prebuild_info as *const () as usize,
        rehydrate_batched as *const () as usize,
        set_morton as *const () as usize,
    ][i];
    tracing::info!(
        kind = KINDS[i],
        original = format_args!("{original:#x}"),
        trace = traced(),
        native = dxr::native_on(),
        "dxr trace: the title's call of its structure builder goes through ours (VICTORY_DXR_TRACE, VICTORY_DXR_NATIVE)"
    );
    Some(ours)
}

fn original(i: usize) -> *const () {
    ORIGINALS[i].load(Relaxed) as *const ()
}

/// The process's own memory, read only where it is committed and readable
/// (the GPU addresses a description holds are title addresses, which may
/// not be).
struct Process;

impl Memory for Process {
    fn bytes(&self, at: u64, len: usize) -> Option<Vec<u8>> {
        readable(at, len).then(|| {
            // SAFETY: committed, readable memory of the process, all of it.
            unsafe { std::slice::from_raw_parts(at as *const u8, len) }.to_vec()
        })
    }
}

#[cfg(windows)]
fn readable(at: u64, len: usize) -> bool {
    use windows_sys::Win32::System::Memory::{
        MEM_COMMIT, MEMORY_BASIC_INFORMATION, PAGE_GUARD, PAGE_NOACCESS, VirtualQuery,
    };
    if at == 0 || len == 0 {
        return at != 0;
    }
    let end = at.saturating_add(len as u64);
    let mut p = at;
    while p < end {
        // SAFETY: plain query of the process's own address space.
        let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
        let got = unsafe {
            VirtualQuery(
                p as *const c_void,
                &mut info,
                size_of::<MEMORY_BASIC_INFORMATION>(),
            )
        };
        if got == 0
            || info.State != MEM_COMMIT
            || info.Protect & (PAGE_NOACCESS | PAGE_GUARD) != 0
            || info.Protect == 0
        {
            return false;
        }
        p = info.BaseAddress as u64 + info.RegionSize as u64;
    }
    true
}

#[cfg(not(windows))]
fn readable(_: u64, _: usize) -> bool {
    false
}

/// Build entries logged line by line under `VICTORY_DXR_TRACE=1`; after
/// them only the first build of each destination is.
const BUDGET: u64 = 20_000;
/// Calls of each kind dumped in hex.
const DUMPED: u64 = 8;
/// Geometries listed per build, at most (the rest are summed).
const LISTED: usize = 24;
/// Instances of a top level shown, at most (as the CPU sees their memory
/// when the build is recorded: a GPU-written buffer may not hold them yet).
const PEEKED: usize = 4;
/// Entries of one call read, at most.
const ENTRIES: u32 = 4096;

static LOGGED: AtomicU64 = AtomicU64::new(0);
static CALLS: [AtomicU64; 7] = [const { AtomicU64::new(0) }; 7];

/// The call number of a call of kind `i`, from 0.
fn number(i: usize) -> u64 {
    CALLS[i].fetch_add(1, Relaxed)
}

/// The last prebuild sizes the title was given for inputs at a CPU address
/// with a level, flags and count: what its build of those inputs is likely
/// to have reserved.
static PREBUILT: Mutex<Option<HashMap<(u64, u32, u32, u32), PrebuildInfo>>> = Mutex::new(None);

fn prebuilt(key: (u64, u32, u32, u32), put: Option<PrebuildInfo>) -> Option<PrebuildInfo> {
    let mut map = PREBUILT.lock().unwrap_or_else(|e| e.into_inner());
    let map = map.get_or_insert_with(HashMap::new);
    match put {
        Some(info) => {
            if map.len() >= 1 << 16 {
                map.clear();
            }
            map.insert(key, info)
        }
        None => map.get(&key).copied(),
    }
}

fn key(i: &Inputs) -> (u64, u32, u32, u32) {
    (i.descs, i.kind, i.flags & !desc::PERFORM_UPDATE, i.count)
}

fn took(since: Instant) {
    rtcalls::add(
        Count::Nanos,
        u64::try_from(since.elapsed().as_nanos()).unwrap_or(u64::MAX),
    );
}

fn dump(what: &str, at: u64, len: usize) -> String {
    match Process.bytes(at, len) {
        Some(b) => format!("{what} at {at:#x}, {len} bytes:{}", desc::dwords(&b)),
        None => format!("{what} at {at:#x}: not readable"),
    }
}

/// What the trace saw since the start, said at most every [`SAY_EVERY`]
/// (on the first call of the builder after that): it counts every call,
/// past the budget of call lines too.
#[derive(Default)]
struct Seen {
    /// Batched entries, and those with a list of extension records.
    entries: u64,
    listed: u64,
    /// Prebuild asks with a list of extension records.
    prebuilds_listed: u64,
    /// Records by kind (0, 1, 2, any other), in builds and prebuilds.
    kinds: [u64; 4],
    /// Lists whose size is not a multiple of 16, or not readable.
    odd: u64,
    unread: u64,
    /// Builds for which the builder takes kind 1's instance count (top
    /// levels), a level (bottom levels), the mode bit of kind 0.
    instances: u64,
    levels: u64,
    modes: u64,
    /// Structures unpacked, those whose shipped data the CPU cannot read,
    /// those without its bit 24 (the builder refuses the whole call), and
    /// those whose description names no geometry.
    unpacked: u64,
    shipped_unread: u64,
    shipped_invalid: u64,
    bare: u64,
    /// Geometries by kind and opacity, of the builds and of the unpackings:
    /// opaque triangle lists, other triangle lists, box lists, other kinds.
    built: [u64; 4],
    unpacked_geometries: [u64; 4],
    /// The +60 word of the unpacked structures' shipped data
    /// ([`Placement`]): none, rotated, centred, scaled, and another word
    /// that changes nothing (one word can be rotated, centred and scaled).
    /// That of built structures is written on the card and not seen here.
    placements: [u64; 5],
    /// How often each +60 word was seen (at most [`WORDS`] words).
    words: HashMap<u32, u64>,
}

/// The +60 words told apart at most, and those the summary names.
const WORDS: usize = 4096;
const COMMON: usize = 8;

/// The summary's period.
const SAY_EVERY: Duration = Duration::from_secs(5);

static SEEN: Mutex<Option<Seen>> = Mutex::new(None);
static SAID: Mutex<Option<Instant>> = Mutex::new(None);

fn seen(f: impl FnOnce(&mut Seen)) {
    let mut s = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    f(s.get_or_insert_with(Seen::default));
}

impl Seen {
    fn records(&mut self, stream: &Stream, records: Option<&[Record]>) {
        if stream.empty() {
            return;
        }
        if !stream.bytes.is_multiple_of(desc::RECORD as u64) {
            self.odd += 1;
        }
        match records {
            Some(r) => {
                for one in r {
                    self.kinds[(one.kind() as usize).min(3)] += 1;
                }
            }
            None => self.unread += 1,
        }
    }

    fn entry(&mut self, e: &Batched, records: Option<&[Record]>) {
        self.entries += 1;
        if e.extensions.empty() {
            return;
        }
        self.listed += 1;
        self.records(&e.extensions, records);
        let Some(r) = records else { return };
        let i = &e.build.inputs;
        let t = desc::taken(i, r);
        self.instances += u64::from(t.instances.is_some_and(|n| n != i.count));
        self.levels += u64::from(t.level.is_some());
        self.modes += u64::from(t.mode);
    }

    /// Counts geometries by kind and opacity into `into`.
    fn kinds(into: &mut [u64; 4], geometries: &[Geometry]) {
        for g in geometries {
            into[match g {
                _ if g.opaque_triangles() => 0,
                Geometry::Triangles { .. } => 1,
                Geometry::Aabbs { .. } => 2,
                Geometry::Other { .. } => 3,
            }] += 1;
        }
    }

    /// One structure unpacked: its shipped data (`None`: not readable) and
    /// the geometries its description names.
    fn shipped(&mut self, data: Option<&Dehydrated>, geometries: &[Geometry]) {
        self.unpacked += 1;
        self.bare += u64::from(geometries.is_empty());
        Self::kinds(&mut self.unpacked_geometries, geometries);
        match data {
            Some(d) => {
                self.shipped_invalid += u64::from(!d.valid());
                self.placed(d.placement);
            }
            None => self.shipped_unread += 1,
        }
    }

    /// The histogram of the +60 word.
    fn placed(&mut self, p: Placement) {
        if p.none() {
            self.placements[0] += 1;
        } else {
            self.placements[1] += u64::from(p.rotation() != 0);
            self.placements[2] += u64::from(p.centred());
            self.placements[3] += u64::from(p.scaled());
            self.placements[4] += u64::from(p.identity());
        }
        if self.words.len() < WORDS || self.words.contains_key(&p.0) {
            *self.words.entry(p.0).or_default() += 1;
        }
    }

    /// The most common +60 words, most first.
    fn common(&self) -> String {
        let mut words: Vec<(u32, u64)> = self.words.iter().map(|(&w, &n)| (w, n)).collect();
        words.sort_by_key(|&(w, n)| (std::cmp::Reverse(n), w));
        let mut text = format!("{} distinct", words.len());
        for (w, n) in words.into_iter().take(COMMON) {
            let _ = write!(text, ", {w:#x} x{n}");
        }
        text
    }

    fn describe(&self) -> String {
        let [bo, bt, bb, bx] = self.built;
        let [uo, ut, ub, ux] = self.unpacked_geometries;
        let [pn, pr, pc, ps, pi] = self.placements;
        format!(
            "dxr trace: since the start (VICTORY_DXR_TRACE): {} batched entries, {} with extension records, and {} prebuild \
             asks with them; records of kind 0 {}, kind 1 {}, kind 2 {}, other kinds {}; lists not a multiple of 16 {}, not \
             readable {}; the builder takes kind 1's instance count on {} top levels, a level on {} bottom levels, the mode \
             bit on {}; {} structures unpacked (shipped data not readable {}, BIT 24 CLEAR {}, no geometries named {}); \
             geometries built: {bo} opaque triangle lists, {bt} other triangle lists, {bb} box lists, {bx} of other kinds; \
             unpacked: {uo} opaque triangle lists, {ut} other triangle lists, {ub} box lists, {ux} of other kinds; the +60 \
             word of the unpacked: none (0x7f7f7f) {pn}, rotated {pr}, centred {pc}, scaled {ps}, another word that changes \
             nothing {pi}; {}",
            self.entries,
            self.listed,
            self.prebuilds_listed,
            self.kinds[0],
            self.kinds[1],
            self.kinds[2],
            self.kinds[3],
            self.odd,
            self.unread,
            self.instances,
            self.levels,
            self.modes,
            self.unpacked,
            self.shipped_unread,
            self.shipped_invalid,
            self.bare,
            self.common(),
        )
    }
}

/// Says [`Seen`] if the last time was [`SAY_EVERY`] ago (or never).
fn say_seen() {
    let now = Instant::now();
    {
        let Ok(mut said) = SAID.try_lock() else {
            return;
        };
        if said.is_some_and(|t| now.duration_since(t) < SAY_EVERY) {
            return;
        }
        *said = Some(now);
    }
    let text = {
        let s = SEEN.lock().unwrap_or_else(|e| e.into_inner());
        match s.as_ref() {
            Some(s) => s.describe(),
            None => return,
        }
    };
    tracing::info!("{text}");
}

/// Reads one build's geometries (as the host's side is handed them too)
/// and, with the trace, counts the build and says it ([`say_build`]).
fn note_build(call: u64, entry: u32, of: u32, d: &BuildDesc, tail: &str) -> (Vec<Geometry>, bool) {
    let (geometries, whole) = desc::geometries(&Process, &d.inputs);
    if traced() {
        say_build(call, entry, of, d, tail, &geometries, whole);
    }
    (geometries, whole)
}

/// Counts one build and, when it is to be logged, says it: the level and
/// flags, its geometries or instances, where it goes, and its scratch.
fn say_build(
    call: u64,
    entry: u32,
    of: u32,
    d: &BuildDesc,
    tail: &str,
    geometries: &[Geometry],
    whole: bool,
) {
    let i = &d.inputs;
    let top = i.top();
    rtcalls::add(
        match (top, i.update()) {
            (false, false) => Count::BottomBuilds,
            (false, true) => Count::BottomUpdates,
            (true, false) => Count::TopBuilds,
            (true, true) => Count::TopUpdates,
        },
        1,
    );
    let first = rtcalls::named(top, d.dest);
    let logged = LOGGED.fetch_add(1, Relaxed);
    let say = match rtcalls::trace() {
        Trace::All => true,
        Trace::Budget => logged < BUDGET || first == Some(true),
        Trace::Off => false,
    };
    let mut text = String::new();
    if top {
        rtcalls::add(Count::Instances, u64::from(i.count));
    } else {
        rtcalls::add(Count::Geometries, u64::from(i.count));
        rtcalls::add(
            Count::Triangles,
            geometries.iter().map(|g| g.triangles()).sum(),
        );
        rtcalls::add(Count::Aabbs, geometries.iter().map(|g| g.aabbs()).sum());
        seen(|s| Seen::kinds(&mut s.built, geometries));
    }
    if !say {
        return;
    }
    let sizes =
        prebuilt(key(i), None).map_or("no prebuild seen for these inputs".to_string(), |p| {
            format!(
                "prebuild of these inputs said result {} scratch {} update scratch {}",
                p.result, p.scratch, p.update_scratch
            )
        });
    let _ = write!(
        text,
        "dxr trace: build call {call} entry {}/{of}: {} level {}{} dest {:#x} source {:#x} scratch {:#x}; {sizes}{tail}",
        entry + 1,
        i.level(),
        desc::build_flags(i.flags),
        match first {
            Some(true) => ", first build of this destination",
            Some(false) => "",
            None => ", destination table full",
        },
        d.dest,
        d.source,
        d.scratch,
    );
    if top {
        let _ = write!(
            text,
            "\n  {} instances at {:#x} ({})",
            i.count,
            i.descs,
            if i.layout == desc::ARRAY {
                "array"
            } else {
                "array of pointers"
            }
        );
        match rtcalls::in_title_buffer(i.descs, u64::from(i.count) * INSTANCE as u64) {
            Some((start, bytes)) => {
                let _ = write!(text, "; in a title buffer at {start:#x} ({bytes} bytes)");
            }
            None => text.push_str("; outside title buffers"),
        }
        if i.layout == desc::ARRAY {
            let n = (i.count as usize).min(PEEKED);
            match Process.bytes(i.descs, n * INSTANCE) {
                Some(b) => {
                    for (k, one) in b.chunks_exact(INSTANCE).enumerate() {
                        let _ = write!(
                            text,
                            "\n  instance {k}: {}",
                            Instance::parse(one).describe()
                        );
                    }
                }
                None => text.push_str("\n  (the instances' memory is not readable from the CPU)"),
            }
        }
    } else {
        let triangles: u64 = geometries.iter().map(|g| g.triangles()).sum();
        let _ = write!(
            text,
            "\n  {} geometries ({}), {triangles} triangles{}",
            i.count,
            if i.layout == desc::ARRAY {
                "array"
            } else {
                "array of pointers"
            },
            if whole { "" } else { ", NOT ALL READABLE" }
        );
        for (k, g) in geometries.iter().take(LISTED).enumerate() {
            let _ = write!(text, "\n  geometry {k}: {}", g.describe());
            // Where a host build would read it from: a title buffer's copy,
            // or pages of the by-address memory.
            for (what, at, len) in g.spans() {
                match rtcalls::in_title_buffer(at, len) {
                    Some((start, bytes)) => {
                        let _ = write!(
                            text,
                            "; {what} in a title buffer at {start:#x} ({bytes} bytes)"
                        );
                    }
                    None => {
                        let _ = write!(text, "; {what} outside title buffers");
                    }
                }
            }
        }
        if geometries.len() > LISTED {
            let _ = write!(text, "\n  ... {} more", geometries.len() - LISTED);
        }
    }
    tracing::info!("{text}");
}

unsafe extern "system" fn construct(
    this: *mut c_void,
    device: *mut c_void,
    flag: u8,
) -> *mut c_void {
    number(CONSTRUCT);
    tracing::info!(
        this = format_args!("{this:p}"),
        device = format_args!("{device:p}"),
        flag,
        "dxr trace: the title makes its structure builder"
    );
    // SAFETY: the title's own function, with its arguments.
    unsafe { std::mem::transmute::<*const (), Construct>(original(CONSTRUCT))(this, device, flag) }
}

unsafe extern "system" fn destroy(this: *mut c_void) {
    number(DESTROY);
    tracing::info!(
        this = format_args!("{this:p}"),
        "dxr trace: the title ends its structure builder"
    );
    // SAFETY: as above.
    unsafe { std::mem::transmute::<*const (), Destroy>(original(DESTROY))(this) }
}

unsafe extern "system" fn set_morton(this: *mut c_void, on: u8) {
    number(MORTON);
    tracing::info!(on, "dxr trace: 64-bit Morton codes");
    // SAFETY: as above.
    unsafe { std::mem::transmute::<*const (), Morton>(original(MORTON))(this, on) }
}

unsafe extern "system" fn build_batched(
    this: *mut c_void,
    list: *mut c_void,
    count: u32,
    descs: *const u8,
) {
    let call = number(BUILD);
    rtcalls::add(Count::Batches, 1);
    let n = count.min(ENTRIES);
    let traced = traced();
    if traced && call < DUMPED {
        tracing::info!(
            "dxr trace: batched build call {call}, {count} entries, list {list:p}; {}",
            dump(
                "the entries (80 bytes each)",
                descs as u64,
                n.min(4) as usize * BATCHED
            )
        );
    }
    let native = dxr::native_on();
    let mut host = Vec::new();
    if let Some(bytes) = Process.bytes(descs as u64, n as usize * BATCHED) {
        for (k, one) in bytes.chunks_exact(BATCHED).enumerate() {
            let e = Batched::parse(one);
            if !traced {
                if native {
                    let (g, whole) = desc::geometries(&Process, &e.build.inputs);
                    host.push(host_build(&e.build, e.build.source, &g, whole));
                }
                continue;
            }
            let mut tail = String::new();
            let _ = write!(
                tail,
                "; {} postbuild requests at {:#x}",
                e.postbuild_count, e.postbuild
            );
            if e.postbuild_count > 0 {
                let m = e.postbuild_count.min(4) as usize;
                if let Some(p) = Process.bytes(e.postbuild, m * POSTBUILD) {
                    for r in p.chunks_exact(POSTBUILD).map(Postbuild::parse) {
                        let _ = write!(tail, " ({} to {:#x})", r.what(), r.dest);
                    }
                }
            }
            // What fits the layout the builder reads: the padding after the
            // count at 0, the extension list empty or of 16-byte records of
            // kinds 0, 1 and 2 (the line says otherwise in capitals).
            if e.word != 0 {
                let _ = write!(tail, "; PADDING WORD {:#x} NOT 0", e.word);
            }
            let records = desc::records(&Process, &e.extensions);
            seen(|s| s.entry(&e, records.as_deref()));
            if e.extensions != Stream::default() {
                let _ = write!(
                    tail,
                    "; {}",
                    desc::describe_extensions(&e.extensions, records.as_deref(), &e.build.inputs)
                );
            }
            let (g, whole) = note_build(call, k as u32, count, &e.build, &tail);
            if native {
                host.push(host_build(&e.build, e.build.source, &g, whole));
            }
        }
    } else {
        tracing::warn!(
            "dxr trace: batched build call {call}: its {count} entries at {descs:p} are not readable"
        );
    }
    let since = Instant::now();
    // SAFETY: as above.
    unsafe { std::mem::transmute::<*const (), Build>(original(BUILD))(this, list, count, descs) };
    took(since);
    // The host's builds of the same come after the console's in the list.
    if native {
        dxr::record(list, dxr::Origin::Batched, host);
    }
    if traced {
        say_seen();
    }
}

unsafe extern "system" fn copy_structure(
    this: *mut c_void,
    list: *mut c_void,
    dest: u64,
    source: u64,
    mode: i32,
) {
    let call = number(COPY);
    rtcalls::add(
        match mode {
            0 => Count::Clones,
            1 => Count::Compactions,
            _ => Count::OtherCopies,
        },
        1,
    );
    let traced = traced();
    let logged = if traced {
        LOGGED.fetch_add(1, Relaxed)
    } else {
        0
    };
    if traced && (rtcalls::trace() == Trace::All || logged < BUDGET) {
        let what = match mode {
            0 => "clone",
            1 => "compact",
            2 => "visualization decode",
            3 => "serialize",
            4 => "deserialize",
            _ => "mode?",
        };
        tracing::info!("dxr trace: copy call {call}: {what} {source:#x} -> {dest:#x}");
    }
    let since = Instant::now();
    // SAFETY: as above.
    unsafe {
        std::mem::transmute::<*const (), Copy>(original(COPY))(this, list, dest, source, mode)
    };
    took(since);
    // The host copies its own structure the same way, after the console's.
    if dxr::native_on() {
        dxr::copy(list, dest, source, mode);
    }
    if traced {
        say_seen();
    }
}

unsafe extern "system" fn prebuild_info(
    this: *mut c_void,
    inputs: *const u8,
    stream: *const u8,
    info: *mut PrebuildInfo,
) {
    let call = number(PREBUILD);
    rtcalls::add(Count::PrebuildAsks, 1);
    let since = Instant::now();
    // SAFETY: as above.
    unsafe {
        std::mem::transmute::<*const (), Prebuild>(original(PREBUILD))(this, inputs, stream, info)
    };
    took(since);
    // The host sizes its own structures itself: only the trace reads these.
    if !traced() {
        return;
    }
    let Some(b) = Process.bytes(inputs as u64, INPUTS) else {
        return;
    };
    let i = Inputs::parse(&b);
    // SAFETY: the title's answer, just written.
    let answer = (!info.is_null()).then(|| unsafe { info.read() });
    if let Some(a) = answer {
        prebuilt(key(&i), Some(a));
    }
    // The stream description's first 16 bytes: the same list of extension
    // records a batched entry has at +0x40 (0x180021880 copies them there).
    let list = (!stream.is_null())
        .then(|| Process.bytes(stream as u64, STREAM))
        .flatten()
        .map(|b| Stream::parse(&b));
    let records = list.as_ref().map(|s| (s, desc::records(&Process, s)));
    if let Some((s, r)) = &records
        && !s.empty()
    {
        seen(|seen| {
            seen.prebuilds_listed += 1;
            seen.records(s, r.as_deref());
        });
    }
    say_seen();
    let logged = LOGGED.fetch_add(1, Relaxed);
    if rtcalls::trace() != Trace::All && logged >= BUDGET {
        return;
    }
    let mut text = format!(
        "dxr trace: prebuild call {call}: {} level {} count {} descs {:#x} -> {}",
        i.level(),
        desc::build_flags(i.flags),
        i.count,
        i.descs,
        answer.map_or("no answer".to_string(), |a| format!(
            "result {} scratch {} update scratch {}",
            a.result, a.scratch, a.update_scratch
        ))
    );
    if !i.top() {
        let (g, _) = desc::geometries(&Process, &i);
        let triangles: u64 = g.iter().map(|g| g.triangles()).sum();
        let _ = write!(text, "; {triangles} triangles");
        if let Some(a) = answer.filter(|_| triangles > 0) {
            let _ = write!(
                text,
                " ({:.1} result bytes a triangle)",
                a.result as f64 / triangles as f64
            );
        }
    }
    match &records {
        None if stream.is_null() => text.push_str("; no stream description"),
        None => text.push_str("; the stream description is not readable"),
        Some((s, r)) => {
            let _ = write!(
                text,
                "; stream description: {}",
                desc::describe_extensions(s, r.as_deref(), &i)
            );
        }
    }
    if call < DUMPED && !stream.is_null() {
        let _ = write!(
            text,
            "; {}",
            dump("the stream description", stream as u64, STREAM)
        );
    }
    tracing::info!("{text}");
}

unsafe extern "system" fn rehydrate_batched(
    this: *mut c_void,
    list: *mut c_void,
    count: u32,
    descs: *const u8,
    shipped: *const u64,
) -> i32 {
    let call = number(REHYDRATE);
    rtcalls::add(Count::Rehydrated, u64::from(count));
    let n = count.min(ENTRIES) as usize;
    let traced = traced();
    let native = dxr::native_on();
    let logged = if traced {
        LOGGED.fetch_add(1, Relaxed)
    } else {
        0
    };
    let say = traced && (rtcalls::trace() == Trace::All || logged < BUDGET);
    let mut text = format!(
        "dxr trace: rehydrate call {call}, {count} structures, list {list:p}, the addresses of their shipped data at {shipped:p}"
    );
    if say && call < DUMPED {
        let _ = write!(
            text,
            "; {}; {}",
            dump("the descriptions", descs as u64, n.min(4) * BUILD_DESC),
            dump(
                "the addresses of the shipped data",
                shipped as u64,
                n.min(8) * 8
            )
        );
    }
    // `p[i]` is the address of structure i's shipped data, which the
    // builder reads on the CPU; with `p` null it reads the description's
    // source instead (0x180014700).
    let addresses = (traced && !shipped.is_null())
        .then(|| Process.bytes(shipped as u64, n * 8))
        .flatten();
    let mut host = Vec::new();
    if let Some(b) = Process.bytes(descs as u64, n * BUILD_DESC) {
        for (k, one) in b.chunks_exact(BUILD_DESC).enumerate() {
            let d = BuildDesc::parse(one);
            if !traced {
                if native {
                    let (g, whole) = desc::geometries(&Process, &d.inputs);
                    host.push(host_build(&d, 0, &g, whole));
                }
                continue;
            }
            rtcalls::named(d.inputs.top(), d.dest);
            let at = if shipped.is_null() {
                Some(d.source)
            } else {
                addresses
                    .as_ref()
                    .map(|v| u64::from_le_bytes(v[k * 8..k * 8 + 8].try_into().unwrap_or_default()))
            };
            let data = at
                .and_then(|a| Process.bytes(a, DEHYDRATED))
                .map(|b| Dehydrated::parse(&b));
            // Whether an unpacking still says what the structure is made
            // of (what a host build of it would need), and how much.
            let (g, whole) = desc::geometries(&Process, &d.inputs);
            seen(|s| s.shipped(data.as_ref(), &g));
            if say && k < LISTED {
                describe_unpacking(&mut text, k, &d, at, data.as_ref(), &g, whole);
            }
            if native {
                host.push(host_build(&d, 0, &g, whole));
            }
        }
    }
    let since = Instant::now();
    // SAFETY: as above.
    let code = unsafe {
        std::mem::transmute::<*const (), Rehydrate>(original(REHYDRATE))(
            this, list, count, descs, shipped,
        )
    };
    took(since);
    // The host builds what the title's builder unpacked, from the geometries
    // the descriptions name, after it in the list; nothing if the builder
    // refused the call (an `HRESULT` below 0: it recorded nothing).
    if native && code >= 0 {
        dxr::record(list, dxr::Origin::Rehydrated, host);
    }
    if say {
        let _ = write!(text, "\n  -> {code:#x}");
        tracing::info!("{text}");
    }
    if traced {
        say_seen();
    }
    code
}

/// Structure `k` of an unpacking in its line: its description, its shipped
/// data at `at`, and the geometries the description names.
fn describe_unpacking(
    text: &mut String,
    k: usize,
    d: &BuildDesc,
    at: Option<u64>,
    data: Option<&Dehydrated>,
    g: &[Geometry],
    whole: bool,
) {
    let _ = write!(
        text,
        "\n  {k}: {} level {} count {} dest {:#x} source {:#x} scratch {:#x}",
        d.inputs.level(),
        desc::build_flags(d.inputs.flags),
        d.inputs.count,
        d.dest,
        d.source,
        d.scratch,
    );
    match (at, data) {
        (None, _) => text.push_str("; the shipped data's address is not readable"),
        (Some(a), None) => {
            let _ = write!(text, "; the shipped data at {a:#x} is not readable");
        }
        (Some(a), Some(data)) => {
            let _ = write!(text, "; the shipped data at {a:#x}: {}", data.describe());
        }
    }
    let triangles: u64 = g.iter().map(|g| g.triangles()).sum();
    let _ = write!(
        text,
        "; {} geometries read{}, {triangles} triangles, flags: {}",
        g.len(),
        if whole { "" } else { " (not all)" },
        desc::flags_tally(g)
    );
    if let Some(first) = g.first() {
        let _ = write!(text, "; the first: {}", first.describe());
    }
}

/// A build as the host's side takes it (`victory_gpu::dxr::record`): the
/// description decoded, with the geometries read from it. `source` is the
/// structure an update starts from; an unpacking has none (its source is
/// the shipped data, which the host does not read).
fn host_build(d: &BuildDesc, source: u64, geometries: &[Geometry], whole: bool) -> dxr::Build {
    let i = &d.inputs;
    dxr::Build {
        dest: d.dest,
        source,
        flags: i.flags,
        level: if i.top() {
            dxr::Level::Top {
                instances: i.descs,
                count: i.count,
                layout: i.layout,
            }
        } else {
            dxr::Level::Bottom {
                geometries: geometries.iter().map(host_geometry).collect(),
                whole,
            }
        },
    }
}

fn host_geometry(g: &Geometry) -> dxr::Geometry {
    match *g {
        Geometry::Triangles {
            flags,
            transform,
            index_format,
            vertex_format,
            index_count,
            vertex_count,
            index_buffer,
            vertex_buffer,
            vertex_stride,
        } => dxr::Geometry::Triangles(dxr::Triangles {
            flags,
            transform,
            index_format,
            vertex_format,
            index_count,
            vertex_count,
            index_buffer,
            vertex_buffer,
            vertex_stride,
        }),
        Geometry::Aabbs {
            flags,
            count,
            start,
            stride,
        } => dxr::Geometry::Boxes {
            flags,
            count,
            start,
            stride,
        },
        Geometry::Other { kind, flags } => dxr::Geometry::Other { kind, flags },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed;

    impl ImportResolver for Fixed {
        fn resolve(&self, dll: &str, _: &ImportSymbol) -> Resolution {
            if dll.eq_ignore_ascii_case(BUILDER_DLL) {
                Resolution::Found {
                    address: 0x1234,
                    provenance: Provenance::Title,
                }
            } else {
                Resolution::Missing
            }
        }
    }

    #[test]
    fn without_the_switch_the_imports_are_bound_as_before() {
        // The tests run without VICTORY_DXR_TRACE and VICTORY_DXR_NATIVE.
        if interposed() {
            return;
        }
        let symbol = ImportSymbol::Name {
            hint: 0,
            name: NAMES[BUILD].to_string(),
        };
        let found = Interpose { inner: &Fixed }.resolve(BUILDER_DLL, &symbol);
        assert_eq!(
            found,
            Resolution::Found {
                address: 0x1234,
                provenance: Provenance::Title
            }
        );
    }

    #[test]
    fn the_host_is_handed_the_build_as_decoded() {
        let d = BuildDesc {
            dest: 0x1_0000,
            inputs: Inputs {
                kind: desc::BOTTOM_LEVEL,
                flags: desc::ALLOW_UPDATE | desc::PERFORM_UPDATE,
                count: 2,
                layout: desc::ARRAY,
                descs: 0x5000,
            },
            source: 0x1_0000,
            scratch: 0x9000,
        };
        let triangles = Geometry::Triangles {
            flags: 1,
            transform: 0x6000,
            index_format: 57,
            vertex_format: 6,
            index_count: 300,
            vertex_count: 120,
            index_buffer: 0xb000,
            vertex_buffer: 0xa000,
            vertex_stride: 12,
        };
        let boxes = Geometry::Aabbs {
            flags: 2,
            count: 7,
            start: 0xd000,
            stride: 32,
        };
        let b = host_build(&d, d.source, &[triangles, boxes], false);
        assert_eq!(
            b,
            dxr::Build {
                dest: 0x1_0000,
                source: 0x1_0000,
                flags: desc::ALLOW_UPDATE | desc::PERFORM_UPDATE,
                level: dxr::Level::Bottom {
                    geometries: vec![
                        dxr::Geometry::Triangles(dxr::Triangles {
                            flags: 1,
                            transform: 0x6000,
                            index_format: 57,
                            vertex_format: 6,
                            index_count: 300,
                            vertex_count: 120,
                            index_buffer: 0xb000,
                            vertex_buffer: 0xa000,
                            vertex_stride: 12,
                        }),
                        dxr::Geometry::Boxes {
                            flags: 2,
                            count: 7,
                            start: 0xd000,
                            stride: 32,
                        },
                    ],
                    whole: false,
                },
            }
        );
        assert_eq!(
            host_geometry(&Geometry::Other { kind: 5, flags: 1 }),
            dxr::Geometry::Other { kind: 5, flags: 1 }
        );
        // A top level names its instances; an unpacking is handed no source.
        let top = BuildDesc {
            inputs: Inputs {
                kind: desc::TOP_LEVEL,
                count: 7,
                layout: desc::ARRAY_OF_POINTERS,
                descs: 0x7000,
                ..Inputs::default()
            },
            source: 0x2_0000,
            ..d
        };
        let t = host_build(&top, 0, &[], true);
        assert_eq!(
            t.level,
            dxr::Level::Top {
                instances: 0x7000,
                count: 7,
                layout: desc::ARRAY_OF_POINTERS
            }
        );
        assert_eq!(t.source, 0);
    }

    #[test]
    fn the_process_memory_reader_refuses_what_is_not_there() {
        let here = [7u8; 16];
        assert_eq!(
            Process.bytes(here.as_ptr() as u64, 16).as_deref(),
            Some(&here[..])
        );
        assert_eq!(Process.bytes(0, 16), None);
        assert_eq!(Process.bytes(8, 16), None);
    }

    #[test]
    fn the_summary_counts_extension_records() {
        let mut s = Seen::default();
        // A top level whose kind-1 record changes its instance count.
        let e = Batched {
            build: BuildDesc {
                inputs: Inputs {
                    kind: desc::TOP_LEVEL,
                    count: 10,
                    ..Inputs::default()
                },
                ..BuildDesc::default()
            },
            extensions: Stream { bytes: 16, at: 1 },
            ..Batched::default()
        };
        let mut r = vec![1u8, 0, 0, 0, 0, 0, 0, 0];
        r.extend_from_slice(&12u32.to_le_bytes());
        r.extend_from_slice(&[0; 4]);
        s.entry(&e, Some(&[Record::parse(&r)]));
        s.entry(&Batched::default(), None);
        assert_eq!((s.entries, s.listed, s.instances, s.kinds[1]), (2, 1, 1, 1));
    }

    #[test]
    fn the_summary_counts_unpacked_structures_and_geometry_kinds() {
        let triangles = |flags| Geometry::Triangles {
            flags,
            transform: 0,
            index_format: 0,
            vertex_format: 6,
            index_count: 0,
            vertex_count: 3,
            index_buffer: 0,
            vertex_buffer: 0x1000,
            vertex_stride: 12,
        };
        let boxes = Geometry::Aabbs {
            flags: 0,
            count: 2,
            start: 0x2000,
            stride: 24,
        };
        let good = Dehydrated {
            first: desc::DEHYDRATED_BIT,
            ..Dehydrated::default()
        };
        let mut s = Seen::default();
        s.shipped(Some(&good), &[triangles(1), triangles(0), boxes]);
        s.shipped(Some(&Dehydrated::default()), &[]);
        s.shipped(None, &[triangles(1)]);
        assert_eq!(
            (s.unpacked, s.shipped_unread, s.shipped_invalid, s.bare),
            (3, 1, 1, 1)
        );
        assert_eq!(s.unpacked_geometries, [2, 1, 1, 0]);
        Seen::kinds(
            &mut s.built,
            &[boxes, Geometry::Other { kind: 3, flags: 0 }],
        );
        assert_eq!(s.built, [0, 0, 1, 1]);
        let text = s.describe();
        assert!(
            text.contains("3 structures unpacked (shipped data not readable 1, BIT 24 CLEAR 1, no geometries named 1)"),
            "{text}"
        );
    }

    #[test]
    fn the_summary_holds_a_histogram_of_the_placement_word() {
        let mut s = Seen::default();
        let words = [
            desc::NO_PLACEMENT,
            desc::NO_PLACEMENT,
            5 << 24,
            1 << 31 | 0x7f_7f80,
            1 << 30,
            1 << 31 | 0x7f_7f7f,
        ];
        for word in words {
            let d = Dehydrated {
                first: desc::DEHYDRATED_BIT,
                placement: Placement(word),
                ..Dehydrated::default()
            };
            s.shipped(Some(&d), &[]);
        }
        // None, rotated, centred, scaled, placed but in effect none.
        assert_eq!(s.placements, [2, 1, 1, 1, 1]);
        let text = s.describe();
        assert!(
            text.contains(
                "none (0x7f7f7f) 2, rotated 1, centred 1, scaled 1, another word that changes nothing 1; \
                 5 distinct, 0x7f7f7f x2, 0x5000000 x1, 0x40000000 x1"
            ),
            "{text}"
        );
    }

    #[test]
    fn the_inputs_key_ignores_the_update_bit() {
        let a = Inputs {
            kind: 1,
            flags: desc::ALLOW_UPDATE,
            count: 3,
            layout: 0,
            descs: 0x10,
        };
        let b = Inputs {
            flags: desc::ALLOW_UPDATE | desc::PERFORM_UPDATE,
            ..a
        };
        assert_eq!(key(&a), key(&b));
    }
}
