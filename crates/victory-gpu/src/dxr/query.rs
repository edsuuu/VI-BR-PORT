//! The host's structure where a translated program asks the host's hardware
//! (milestone M3a, `VICTORY_RT_QUERY`): the root SRV `t252` the translation
//! of a walk made a ray query of reads (`victory_shader::emit::walk`).
//!
//! Such a program names the title's structure as the view whose descriptor
//! is at a fixed dword of one of its descriptor tables: its user data has a
//! word of kind `USER_STRUCTURE` with that table's root slot and dword
//! ([`asked`]). For it the host's root signature declares a root SRV at
//! `t<SLOT_STRUCTURE>` in `ROOT_CBV_SPACE` (`crate::pipeline::host`), and at
//! each of its draws or dispatches the host binds there ([`bind`]):
//!
//! - the host's top level built for the title's structure at that view
//!   (`VICTORY_DXR_NATIVE`, `super::native`), in the version the program
//!   reads when it runs ([`super::table::Table::read_now`]): the current one
//!   if its build was submitted or recorded earlier in the same recording of
//!   the same title list, else the one before it;
//! - otherwise an empty top level (no instances), built once on a queue of
//!   its own and kept for good ([`empty`]): every ray misses, as it would in a
//!   structure with nothing in it.
//!
//! The user-data word is 1 when a host top level was bound, 0 for the empty
//! one; the program does not read it. A five-second line ([`line`]) counts
//! what was bound and why.
//!
//! Nothing here runs unless a translation asks: only `VICTORY_RT_QUERY`
//! makes one ([`asked`] is the only way in). Without `VICTORY_DXR_NATIVE`
//! every such program gets the empty structure.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, OnceLock};

use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::core::Interface;

use super::native::{self, Structure};
use super::table::{Reading, Shape};
use victory_shader::RootType;
use victory_shader::emit::{HostShader, USER_STRUCTURE};

/// The title's structure a shader asks the host for: the root slot of the
/// descriptor table and the dword in it of the structure's view (`None`: it
/// asks for none).
pub(crate) fn asked(host: &HostShader) -> Option<(u8, u32)> {
    host.user_data
        .iter()
        .find(|u| u.root == RootType::Other(USER_STRUCTURE))
        .map(|u| (u.slot, u.dword))
}

/// A host structure held by what reads it (a title list's recording, until
/// the list has run or is reset).
pub(crate) struct Hold(#[allow(dead_code)] Arc<Structure>);

/// What [`bind`] binds.
pub(crate) struct Bound {
    /// The structure's GPU address: the root SRV.
    pub(crate) gpu: u64,
    /// A top level of the host's (else the empty one): the user-data word.
    pub(crate) own: bool,
    /// What the recording holds while it may run.
    pub(crate) hold: Option<Hold>,
}

/// What [`line`] counts.
#[derive(Clone, Copy)]
enum Q {
    /// Draws or dispatches of a program that asks.
    Asked,
    /// Bound the host's current version, recorded earlier in the same list.
    Here,
    /// Bound the host's current version, already submitted.
    Submitted,
    /// Bound the version before a newer one not submitted yet.
    Previous,
    /// Bound the empty one: no view at the word.
    NoView,
    /// No host structure at the view.
    Unknown,
    /// A bottom level at the view.
    Bottom,
    /// Only a version recorded in another list not submitted yet.
    Pending,
    /// The host builds none (`VICTORY_DXR_NATIVE` off).
    NativeOff,
    /// Nothing could be bound (no empty structure): dropped.
    Dropped,
}

const QS: usize = Q::Dropped as usize + 1;
static COUNT: [AtomicU64; QS] = [const { AtomicU64::new(0) }; QS];
static FRAMES: AtomicU64 = AtomicU64::new(0);

fn add(q: Q) {
    COUNT[q as usize].fetch_add(1, Relaxed);
}

/// Views said in full the first time a program reads them.
const SAID: usize = 16;
static SEEN: Mutex<Vec<u64>> = Mutex::new(Vec::new());

/// Says what the first [`SAID`] views a program reads were bound to.
fn say(view: u64, what: impl FnOnce() -> String) {
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    if seen.len() < SAID && !seen.contains(&view) {
        seen.push(view);
        drop(seen);
        tracing::info!(
            "host ray queries: a program reads the title's structure at {view:#x}: {}",
            what()
        );
    }
}

/// The host structure a draw or dispatch recorded now in the recording
/// `mark` of a title list (`crate::draw::State::structures_mark`) reads for
/// the title's structure at `view` (0: no view at the word), as the module
/// says. `None` when nothing can be bound (the empty structure could not be
/// made): the draw or dispatch is to be dropped.
pub(crate) fn bind(view: u64, mark: u64) -> Option<Bound> {
    add(Q::Asked);
    let own = if view == 0 {
        Err(Q::NoView)
    } else if !super::native_on() {
        Err(Q::NativeOff)
    } else {
        let frame = crate::frame::current();
        let mut table = native::table();
        match table.read_now(view, frame, |s| s.recorded_in(mark)) {
            None => Err(Q::Unknown),
            Some((_, Shape::Bottom(_))) => Err(Q::Bottom),
            Some((Reading::Pending, _)) => Err(Q::Pending),
            Some((Reading::Current(s), &Shape::Top(n))) => Ok((
                if s.recorded_in(mark) {
                    Q::Here
                } else {
                    Q::Submitted
                },
                s,
                n,
            )),
            Some((Reading::Previous(s), &Shape::Top(n))) => Ok((Q::Previous, s, n)),
        }
    };
    match own {
        Ok((q, s, instances)) => {
            add(q);
            let gpu = s.gpu();
            say(view, || {
                format!(
                    "the host's top level of {instances} instances at {gpu:#x} ({})",
                    name(q)
                )
            });
            Some(Bound {
                gpu,
                own: true,
                hold: Some(Hold(s)),
            })
        }
        Err(q) => {
            let Some(gpu) = empty() else {
                add(Q::Dropped);
                return None;
            };
            add(q);
            say(view, || format!("the empty top level ({})", name(q)));
            Some(Bound {
                gpu,
                own: false,
                hold: None,
            })
        }
    }
}

fn name(q: Q) -> &'static str {
    match q {
        Q::Asked => "asked",
        Q::Here => "built before it in the same list",
        Q::Submitted => "submitted before",
        Q::Previous => "the one before a newer build not submitted yet",
        Q::NoView => "no view at the word",
        Q::Unknown => "no host structure at the view",
        Q::Bottom => "a bottom level at the view",
        Q::Pending => "only a build not submitted yet",
        Q::NativeOff => "VICTORY_DXR_NATIVE is off",
        Q::Dropped => "nothing to bind",
    }
}

/// The empty top level's GPU address, made on first use ([`empty_on`]).
fn empty() -> Option<u64> {
    static EMPTY: OnceLock<Option<(ID3D12Resource, u64)>> = OnceLock::new();
    EMPTY
        .get_or_init(|| {
            let host = crate::host::host()?;
            match empty_on(&host.device) {
                Ok(made) => {
                    tracing::info!(at = format_args!("{:#x}", made.1), "host ray queries: the empty top level is built");
                    Some(made)
                }
                Err(e) => {
                    tracing::warn!(error = %e, "host ray queries: no empty top level; programs that would read it are dropped");
                    None
                }
            }
        })
        .as_ref()
        .map(|e| e.1)
}

/// Builds a top level of no instances on `device`, on a queue of its own,
/// and waits for it: from then on it is a built structure that any list on
/// any queue may read. Its buffer and address.
pub(super) fn empty_on(device: &ID3D12Device) -> Result<(ID3D12Resource, u64), String> {
    fn err(what: &str) -> impl Fn(windows::core::Error) -> String + '_ {
        move |e| format!("{what}: {e}")
    }
    let rt: ID3D12Device5 = device.cast().map_err(err("no ID3D12Device5"))?;
    let inputs = D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS {
        Type: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_TYPE_TOP_LEVEL,
        Flags: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAG_PREFER_FAST_TRACE,
        NumDescs: 0,
        DescsLayout: D3D12_ELEMENTS_LAYOUT_ARRAY,
        Anonymous: D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS_0 { InstanceDescs: 0 },
    };
    let mut sizes = D3D12_RAYTRACING_ACCELERATION_STRUCTURE_PREBUILD_INFO::default();
    // SAFETY: the inputs name no memory.
    unsafe { rt.GetRaytracingAccelerationStructurePrebuildInfo(&inputs, &mut sizes) };
    if sizes.ResultDataMaxSizeInBytes == 0 {
        return Err("the driver sizes no empty top level".into());
    }
    let size = |n: u64| n.max(1).next_multiple_of(super::pool::ALIGN);
    let result = super::pool::chunk_buffer(
        device,
        super::pool::Kind::Structures,
        size(sizes.ResultDataMaxSizeInBytes),
    )
    .ok_or("no card memory for the empty top level")?;
    let scratch = super::pool::chunk_buffer(
        device,
        super::pool::Kind::Scratch,
        size(sizes.ScratchDataSizeInBytes),
    )
    .ok_or("no card memory for its scratch")?;
    // SAFETY: plain object creation and one build into buffers of ours, run
    // on a queue of ours and waited for before anything else may use them.
    unsafe {
        let queue: ID3D12CommandQueue = device
            .CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC {
                Type: D3D12_COMMAND_LIST_TYPE_COMPUTE,
                ..Default::default()
            })
            .map_err(err("queue"))?;
        let allocator: ID3D12CommandAllocator = device
            .CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_COMPUTE)
            .map_err(err("allocator"))?;
        let list: ID3D12GraphicsCommandList4 = device
            .CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_COMPUTE, &allocator, None)
            .map_err(err("list"))?;
        let gpu = result.GetGPUVirtualAddress();
        list.BuildRaytracingAccelerationStructure(
            &D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_DESC {
                DestAccelerationStructureData: gpu,
                Inputs: inputs,
                SourceAccelerationStructureData: 0,
                ScratchAccelerationStructureData: scratch.GetGPUVirtualAddress(),
            },
            None,
        );
        list.Close().map_err(err("close"))?;
        let fence: ID3D12Fence = device
            .CreateFence(0, D3D12_FENCE_FLAG_NONE)
            .map_err(err("fence"))?;
        let event = CreateEventW(None, false, false, None).map_err(err("event"))?;
        queue.ExecuteCommandLists(&[Some(list.cast::<ID3D12CommandList>().map_err(err("list"))?)]);
        let waited = match queue
            .Signal(&fence, 1)
            .and_then(|()| fence.SetEventOnCompletion(1, event))
        {
            Ok(()) => WaitForSingleObject(event, 10_000) == WAIT_OBJECT_0,
            Err(_) => false,
        };
        let _ = CloseHandle(event);
        if !waited || fence.GetCompletedValue() < 1 {
            // The card may still use them: never released.
            std::mem::forget((queue, allocator, list, result, scratch));
            return Err("the build did not finish in 10 s".into());
        }
        Ok((result, gpu))
    }
}

/// A presented frame.
pub(crate) fn frame() {
    FRAMES.fetch_add(1, Relaxed);
}

/// The five-second line, once a program asked: what the programs that ask
/// for the host's structure were given.
pub(crate) fn line() {
    let c: [u64; QS] = std::array::from_fn(|i| COUNT[i].swap(0, Relaxed));
    let frames = FRAMES.swap(0, Relaxed).max(1) as f64;
    static ASKED_EVER: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if c[Q::Asked as usize] > 0 {
        ASKED_EVER.store(true, Relaxed);
    } else if !ASKED_EVER.load(Relaxed) {
        return;
    }
    let per = |q: Q| c[q as usize] as f64 / frames;
    let own = per(Q::Here) + per(Q::Submitted) + per(Q::Previous);
    let empty =
        per(Q::NoView) + per(Q::Unknown) + per(Q::Bottom) + per(Q::Pending) + per(Q::NativeOff);
    tracing::info!(
        "host ray queries a frame (VICTORY_RT_QUERY): {:.1} draws and dispatches of programs that ask for the host's structure; \
         {own:.1} got the host's top level ({:.1} built before them in the same list, {:.1} submitted before, {:.1} the one before \
         a newer build not submitted yet); {empty:.1} got the empty one ({:.1} no view at the word, {:.1} no host structure at the \
         view, {:.1} a bottom level at the view, {:.1} only a build not submitted yet, {:.1} VICTORY_DXR_NATIVE off); {:.1} dropped \
         (nothing to bind)",
        per(Q::Asked),
        per(Q::Here),
        per(Q::Submitted),
        per(Q::Previous),
        per(Q::NoView),
        per(Q::Unknown),
        per(Q::Bottom),
        per(Q::Pending),
        per(Q::NativeOff),
        per(Q::Dropped),
    );
}
