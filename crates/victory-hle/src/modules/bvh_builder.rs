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
//! This first step only listens. With `VICTORY_DXR_TRACE=1` (or `all`;
//! unset: off, the default, and the imports are bound as before) the
//! title's imports are bound to the functions here ([`Interpose`]), which
//! log what each call asks, call the title's own function, and count it for
//! the five-second report (`victory_gpu::rtcalls`). Nothing the title or
//! its builder sees changes.
//!
//! The descriptions are decoded in [`desc`]; the console's own batched
//! layout is inferred and its first calls are dumped in hex to confirm it.
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
use std::time::Instant;

use victory_gpu::rtcalls::{self, Count, Trace};
use victory_loader::{ImportResolver, Provenance, Resolution};
use victory_pe::ImportSymbol;

use desc::{
    BATCHED, BUILD_DESC, Batched, BuildDesc, INPUTS, INSTANCE, Inputs, Instance, Memory, POSTBUILD,
    Postbuild, PrebuildInfo,
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
/// `VICTORY_DXR_TRACE` is on, keeping what `inner` binds them to (the
/// title's own DLL) for them to call; otherwise binds as `inner` does.
pub struct Interpose<'a> {
    pub inner: &'a dyn ImportResolver,
}

impl ImportResolver for Interpose<'_> {
    fn resolve(&self, dll: &str, symbol: &ImportSymbol) -> Resolution {
        let found = self.inner.resolve(dll, symbol);
        if !dll.eq_ignore_ascii_case(BUILDER_DLL) || rtcalls::trace() == Trace::Off {
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
        "dxr trace: the title's call of its structure builder goes through ours (VICTORY_DXR_TRACE)"
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

/// Counts one build and, when it is to be logged, says it: the level and
/// flags, its geometries or instances, where it goes, and its scratch.
fn note_build(call: u64, entry: u32, of: u32, d: &BuildDesc, tail: &str) {
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
    let (geometries, whole) = desc::geometries(&Process, i);
    if top {
        rtcalls::add(Count::Instances, u64::from(i.count));
    } else {
        rtcalls::add(Count::Geometries, u64::from(i.count));
        rtcalls::add(
            Count::Triangles,
            geometries.iter().map(|g| g.triangles()).sum(),
        );
        rtcalls::add(Count::Aabbs, geometries.iter().map(|g| g.aabbs()).sum());
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
    if call < DUMPED {
        tracing::info!(
            "dxr trace: batched build call {call}, {count} entries, list {list:p}; {}",
            dump(
                "the entries (80 bytes each, inferred)",
                descs as u64,
                n.min(4) as usize * BATCHED
            )
        );
    }
    if let Some(bytes) = Process.bytes(descs as u64, n as usize * BATCHED) {
        for (k, one) in bytes.chunks_exact(BATCHED).enumerate() {
            let e = Batched::parse(one);
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
            if e.word != 0 || e.rest != [0, 0] {
                let _ = write!(
                    tail,
                    "; unknown fields {:#x} {:#x} {:#x}",
                    e.word, e.rest[0], e.rest[1]
                );
            }
            note_build(call, k as u32, count, &e.build, &tail);
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
    let logged = LOGGED.fetch_add(1, Relaxed);
    if rtcalls::trace() == Trace::All || logged < BUDGET {
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
    let Some(b) = Process.bytes(inputs as u64, INPUTS) else {
        return;
    };
    let i = Inputs::parse(&b);
    // SAFETY: the title's answer, just written.
    let answer = (!info.is_null()).then(|| unsafe { info.read() });
    if let Some(a) = answer {
        prebuilt(key(&i), Some(a));
    }
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
    if call < DUMPED {
        if stream.is_null() {
            text.push_str("; no stream description");
        } else {
            let _ = write!(
                text,
                "; {}",
                dump("the stream description", stream as u64, 64)
            );
        }
    }
    tracing::info!("{text}");
}

unsafe extern "system" fn rehydrate_batched(
    this: *mut c_void,
    list: *mut c_void,
    count: u32,
    descs: *const u8,
    extra: *const u64,
) -> i32 {
    let call = number(REHYDRATE);
    rtcalls::add(Count::Rehydrated, u64::from(count));
    let n = count.min(ENTRIES) as usize;
    let logged = LOGGED.fetch_add(1, Relaxed);
    let say = rtcalls::trace() == Trace::All || logged < BUDGET;
    let mut text = format!(
        "dxr trace: rehydrate call {call}, {count} structures, list {list:p}, the 64-bit values at {extra:p}"
    );
    if call < DUMPED {
        let _ = write!(
            text,
            "; {}; {}",
            dump("the descriptions", descs as u64, n.min(4) * BUILD_DESC),
            dump("the 64-bit values", extra as u64, n.min(8) * 8)
        );
    }
    let values = Process.bytes(extra as u64, n * 8);
    if let Some(b) = Process.bytes(descs as u64, n * BUILD_DESC) {
        for (k, one) in b.chunks_exact(BUILD_DESC).enumerate() {
            let d = BuildDesc::parse(one);
            rtcalls::named(d.inputs.top(), d.dest);
            if say && k < LISTED {
                let value = values.as_ref().map(|v| {
                    u64::from_le_bytes(v[k * 8..k * 8 + 8].try_into().unwrap_or_default())
                });
                let _ = write!(
                    text,
                    "\n  {k}: {} level {} count {} dest {:#x} source {:#x} scratch {:#x} value {}",
                    d.inputs.level(),
                    desc::build_flags(d.inputs.flags),
                    d.inputs.count,
                    d.dest,
                    d.source,
                    d.scratch,
                    value.map_or("?".to_string(), |v| format!("{v:#x}")),
                );
                // Whether an unpacking still says what the structure is made
                // of (what a host build of it would need), and how much.
                let (g, whole) = desc::geometries(&Process, &d.inputs);
                let triangles: u64 = g.iter().map(|g| g.triangles()).sum();
                let _ = write!(
                    text,
                    "; {} geometries read{}, {triangles} triangles",
                    g.len(),
                    if whole { "" } else { " (not all)" }
                );
                if let Some(first) = g.first() {
                    let _ = write!(text, "; the first: {}", first.describe());
                }
            }
        }
    }
    let since = Instant::now();
    // SAFETY: as above.
    let code = unsafe {
        std::mem::transmute::<*const (), Rehydrate>(original(REHYDRATE))(
            this, list, count, descs, extra,
        )
    };
    took(since);
    if say {
        let _ = write!(text, "\n  -> {code:#x}");
        tracing::info!("{text}");
    }
    code
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
        // The tests run without VICTORY_DXR_TRACE.
        if rtcalls::trace() != Trace::Off {
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
