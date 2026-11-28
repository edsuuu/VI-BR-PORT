//! Compaction of the host's static bottom levels (`VICTORY_DXR_COMPACT=1`;
//! off by default, and then nothing here runs).
//!
//! The title unpacks its world's structures (`RehydrateRaytracing...`) with
//! no `ALLOW_COMPACTION` and builds many of its own without updates; the
//! host built each into as much memory as the driver's worst case says.
//! The prison's unpacked world alone is about three times the 512 MiB
//! ceiling of the M2a runs (some 25 M triangles at ~63 bytes each on this
//! card).
//!
//! With the switch, every bottom level built from scratch without
//! `ALLOW_UPDATE` is built with `ALLOW_COMPACTION` and its build writes its
//! compacted size into a buffer of ours the CPU reads ([`ask`]). Some frames
//! after its list was submitted (every submission has run by then: the
//! host's frame waits for them), a recording of top levels of ours copies it
//! compacted into memory of its size and makes the copy the destination's
//! current version ([`run`]); the one built goes back to the pool when the
//! top levels and lists that hold it let go. Only in a recording of top
//! levels, so that the copy runs before the top levels of the title's own
//! call in its own list, on its own queue.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use windows::Win32::Graphics::Direct3D12::*;

use super::count::{C, add};
use super::native::{ALLOW_COMPACTION, Held, Structure, lease_for, wait_all};
use super::table::{ALLOW_UPDATE, Table};
use crate::host::HostList;

/// `VICTORY_DXR_COMPACT=1`.
pub(super) fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        let on = std::env::var("VICTORY_DXR_COMPACT").is_ok_and(|v| v.trim() == "1");
        if on && super::native_on() {
            tracing::info!("VICTORY_DXR_COMPACT: the host compacts its bottom levels built without ALLOW_UPDATE");
        }
        on
    })
}

/// The flags a bottom level built from scratch with `flags` is built with:
/// those, and `ALLOW_COMPACTION` if it is never updated.
pub(super) fn flags(flags: u32) -> u32 {
    if on() && flags & ALLOW_UPDATE == 0 { flags | ALLOW_COMPACTION } else { flags }
}

/// Slots of compacted sizes (8 bytes each) in the ring.
const SLOTS: u64 = 1 << 18;
/// Frames after a submission its sizes are read in.
const SETTLE: u64 = 8;
/// Frames after which a size never written is given up.
const GIVE_UP: u64 = 120;
/// Compactions at most a recording.
const MOST: usize = 128;

/// Where the builds write their compacted sizes: a buffer in memory the CPU
/// reads and shaders may write (`Host::shared_buffer`), mapped for good.
struct Sizes {
    _buffer: ID3D12Resource,
    gpu: u64,
    data: *mut u64,
}

// SAFETY: a D3D12 buffer (free-threaded) and its mapping, read and zeroed
// only under `PENDING`'s lock.
unsafe impl Send for Sizes {}
// SAFETY: as above.
unsafe impl Sync for Sizes {}

fn sizes() -> Option<&'static Sizes> {
    static SIZES: OnceLock<Option<Sizes>> = OnceLock::new();
    SIZES
        .get_or_init(|| {
            let (buffer, data) = crate::host::host()?.shared_buffer(SLOTS * 8)?;
            crate::host::name(&buffer, || "host ray tracing: compacted sizes".into());
            // SAFETY: a live buffer.
            let gpu = unsafe { buffer.GetGPUVirtualAddress() };
            Some(Sizes { _buffer: buffer, gpu, data: data.cast() })
        })
        .as_ref()
}

/// A build whose compacted size is coming.
struct Pending {
    dest: u64,
    /// (Not held: one let go meanwhile is not compacted.)
    built: Weak<Structure>,
    slot: u64,
    /// The frame its list was submitted in (0: not yet).
    sent: Arc<AtomicU64>,
    asked: u64,
}

static PENDING: Mutex<VecDeque<Pending>> = Mutex::new(VecDeque::new());
static NEXT: AtomicU64 = AtomicU64::new(0);

/// For the build of `built` into `dest` about to be recorded in `host`'s
/// list with `flags`: what it writes its compacted size by, if it is to be
/// compacted.
pub(super) fn ask(
    host: &HostList,
    dest: u64,
    built: &Arc<Structure>,
    flags: u32,
    frame: u64,
) -> Option<D3D12_RAYTRACING_ACCELERATION_STRUCTURE_POSTBUILD_INFO_DESC> {
    if !on() || flags & ALLOW_COMPACTION == 0 || flags & ALLOW_UPDATE != 0 {
        return None;
    }
    let sizes = sizes()?;
    let slot = NEXT.fetch_add(1, Relaxed) % SLOTS;
    let sent = Arc::new(AtomicU64::new(0));
    {
        let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: inside the mapping; the slot's last build was given up or
        // taken long before (the ring is far longer than builds in flight).
        unsafe { sizes.data.add(slot as usize).write_volatile(0) };
        pending.push_back(Pending { dest, built: Arc::downgrade(built), slot, sent: sent.clone(), asked: frame });
    }
    host.defer_host(move || {
        let _ = sent.compare_exchange(0, crate::frame::current().max(1), Relaxed, Relaxed);
    });
    Some(D3D12_RAYTRACING_ACCELERATION_STRUCTURE_POSTBUILD_INFO_DESC {
        DestBuffer: sizes.gpu + 8 * slot,
        InfoType: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_POSTBUILD_INFO_COMPACTED_SIZE,
    })
}

/// What to do with one pending build at `frame`, its size read `size`.
#[derive(Debug, PartialEq, Eq)]
enum Due {
    /// Not yet (not submitted, or not settled, or the size not written).
    Wait,
    /// Dropped: given up, or the destination is another structure now, or
    /// the size is no smaller than the structure.
    Drop,
    Compact,
}

fn due(sent: u64, asked: u64, frame: u64, size: u64, len: u64, current: bool) -> Due {
    if sent == 0 || frame < sent + SETTLE || size == 0 {
        return if frame > asked + GIVE_UP { Due::Drop } else { Due::Wait };
    }
    if !current || size >= len {
        return Due::Drop;
    }
    Due::Compact
}

/// In a recording of top levels into `list` (after a barrier on every
/// resource): the compactions due, recorded, the copies made the current
/// versions of their destinations. A barrier follows them.
pub(super) fn run(list: &ID3D12GraphicsCommandList4, table: &mut Table<Structure>, frame: u64, held: &mut Held) {
    if !on() {
        return;
    }
    let Some(sizes) = sizes() else { return };
    let mut due_now = Vec::new();
    {
        let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
        let mut kept = VecDeque::with_capacity(pending.len());
        while let Some(p) = pending.pop_front() {
            if due_now.len() >= MOST {
                kept.push_back(p);
                continue;
            }
            // SAFETY: inside the mapping, under the lock.
            let size = unsafe { sizes.data.add(p.slot as usize).read_volatile() };
            // One let go meanwhile is not compacted.
            let Some(built) = p.built.upgrade() else {
                add(C::CompactDropped, 1);
                continue;
            };
            let current = table.get(p.dest).is_some_and(|e| Arc::ptr_eq(&e.current, &built));
            match due(p.sent.load(Relaxed), p.asked, frame, size, built.lease.len(), current) {
                Due::Wait => kept.push_back(p),
                Due::Drop => add(C::CompactDropped, 1),
                Due::Compact => due_now.push((p.dest, built, size)),
            }
        }
        *pending = kept;
    }
    if due_now.is_empty() {
        return;
    }
    for (dest, built, size) in due_now {
        let Some((flags, shape, last_used)) = table.get(dest).map(|e| (e.flags, e.shape.clone(), e.last_used)) else { continue };
        let lease = match lease_for(table, size, frame) {
            Ok(l) => l,
            Err(_) => {
                add(C::CompactDropped, 1);
                continue;
            }
        };
        let made = Structure::new(lease);
        // SAFETY: an open list; both structures are held by the list (below).
        unsafe {
            list.CopyRaytracingAccelerationStructure(made.gpu(), built.gpu(), D3D12_RAYTRACING_ACCELERATION_STRUCTURE_COPY_MODE_COMPACT)
        };
        add(C::Compacted, 1);
        add(C::CompactedFrom, built.lease.len());
        add(C::CompactedTo, made.lease.len());
        table.built(dest, flags, shape, made.clone(), last_used);
        held.leases.push(built.lease.clone());
        held.made.push(made);
    }
    wait_all(list);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_size_is_taken_once_settled_and_only_for_the_current_version() {
        // Not submitted, or submitted lately: wait; past the give-up: drop.
        assert_eq!(due(0, 10, 20, 0, 1000, true), Due::Wait);
        assert_eq!(due(15, 10, 20, 500, 1000, true), Due::Wait);
        assert_eq!(due(0, 10, 10 + GIVE_UP + 1, 0, 1000, true), Due::Drop);
        // Settled but the size not written yet: wait.
        assert_eq!(due(15, 10, 15 + SETTLE, 0, 1000, true), Due::Wait);
        // Written: compacted, unless the destination holds another version
        // now or nothing is gained.
        assert_eq!(due(15, 10, 15 + SETTLE, 500, 1000, true), Due::Compact);
        assert_eq!(due(15, 10, 15 + SETTLE, 500, 1000, false), Due::Drop);
        assert_eq!(due(15, 10, 15 + SETTLE, 1000, 1000, true), Due::Drop);
    }
}
