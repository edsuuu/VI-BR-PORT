//! Our passes over copies in the card's memory as one dispatch per list,
//! over a table of pieces (`VICTORY_CARD_HASH_TABLE=1` for the hash passes
//! after ranges are sent up, `VICTORY_CARD_COMPARE_TABLE=1` for the compares
//! ahead of the sends and after the lists; both off by default).
//!
//! Each run of pieces had a dispatch of its own, with the copy's buffers as
//! root descriptors: on the boat some 190 hash passes and 490 compare passes
//! a frame, for 5,700 pieces, and a dispatch between other commands costs the
//! card some 3.4 us whatever it does (measured on this card), about 2.3 ms a
//! frame by count alone. Here every pass of a list goes into one dispatch: a
//! group per piece, as before, which takes its copy, piece, offset into the
//! results and mode from a table written by the CPU (16 bytes a piece), and
//! its copy's buffers from a descriptor table of ours (two views a copy).
//! The groups hash and compare the same bytes as the passes did and write
//! the same results: the shader below is [`super::CARD_COMPARE`] with only
//! where its numbers come from changed, and a WARP test checks both give the
//! same hashes and results.
//!
//! The table and the views are taken from rings that start again when the
//! compare's results do (no plan of ours left on the card,
//! `Host::card_done`); a list that finds no room or names more than
//! [`COPIES`] copies, or a copy too large for a view, has its passes as
//! before.

use super::*;
use std::sync::atomic::AtomicU64;

/// `VICTORY_CARD_HASH_TABLE=1`: the hash passes after ranges are sent up go
/// in one dispatch per list (off by default).
pub(super) fn hash_on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| crate::switch::on("VICTORY_CARD_HASH_TABLE"))
}

/// `VICTORY_CARD_COMPARE_TABLE=1`: the compares, ahead of the sends and
/// after the lists, go in one dispatch per list (off by default).
pub(super) fn compare_on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| crate::switch::on("VICTORY_CARD_COMPARE_TABLE"))
}

/// [`super::CARD_COMPARE`] over a table of pieces. A group's entry (16
/// bytes): its copy's index in the descriptor tables (the copy's views are
/// at `2 * index` of both, see [`Table::prepare`]), the compare's mode and
/// the copy's skew packed in one dword; the piece; the byte of its result in
/// `differs`; the copy's size.
pub(crate) const CARD_COMPARE_TABLE: &str = r#"
#define RS "RootConstants(num32BitConstants=1, b0), DescriptorTable(SRV(t0, space=1, numDescriptors=2048, flags=DESCRIPTORS_VOLATILE)), DescriptorTable(UAV(u0, space=1, numDescriptors=2048, flags=DESCRIPTORS_VOLATILE)), SRV(t1), UAV(u1)"
cbuffer Table : register(b0) {
    uint base;     // the first group's entry
};
ByteAddressBuffer nows[2048] : register(t0, space1);
RWByteAddressBuffer hashes[2048] : register(u0, space1);
ByteAddressBuffer entries : register(t1);
RWByteAddressBuffer differs : register(u1);
groupshared uint4 parts[64];
[RootSignature(RS)]
[numthreads(64, 1, 1)]
void main(uint3 id : SV_GroupID, uint thread : SV_GroupIndex) {
    // The same entry for the whole group: the views' index is uniform in
    // every wave.
    uint4 e = entries.Load4((base + id.x) * 16u);
    uint copy = ((e.x >> 16u) & 0x3FFFu) * 2u;
    uint compare = e.x >> 30u;
    uint skew = e.x & 0xFFFFu;
    uint piece = e.y;
    uint out_at = e.z;
    uint size = e.w;
    uint lo = piece * 65536u;
    lo = lo > skew ? lo - skew : 0u;
    uint hi = min((piece + 1u) * 65536u - skew, size);
    lo = lo & ~15u;
    hi = (hi + 15u) & ~15u;
    uint4 h = uint4(0x9E3779B9u, 0x85EBCA6Bu, 0xC2B2AE35u, 0x27D4EB2Fu) + thread;
    uint at = lo + thread * 16u;
    [loop]
    for (; at + 7168u < hi; at += 8192u) {
        uint4 v0 = nows[copy].Load4(at + 0u);
        uint4 v1 = nows[copy].Load4(at + 1024u);
        uint4 v2 = nows[copy].Load4(at + 2048u);
        uint4 v3 = nows[copy].Load4(at + 3072u);
        uint4 v4 = nows[copy].Load4(at + 4096u);
        uint4 v5 = nows[copy].Load4(at + 5120u);
        uint4 v6 = nows[copy].Load4(at + 6144u);
        uint4 v7 = nows[copy].Load4(at + 7168u);
        h = (h ^ v0) * uint4(0x01000193u, 0x9E3779B1u, 0x85EBCA77u, 0xC2B2AE3Du);
        h = h.yzwx ^ (h >> 15);
        h = (h ^ v1) * uint4(0x01000193u, 0x9E3779B1u, 0x85EBCA77u, 0xC2B2AE3Du);
        h = h.yzwx ^ (h >> 15);
        h = (h ^ v2) * uint4(0x01000193u, 0x9E3779B1u, 0x85EBCA77u, 0xC2B2AE3Du);
        h = h.yzwx ^ (h >> 15);
        h = (h ^ v3) * uint4(0x01000193u, 0x9E3779B1u, 0x85EBCA77u, 0xC2B2AE3Du);
        h = h.yzwx ^ (h >> 15);
        h = (h ^ v4) * uint4(0x01000193u, 0x9E3779B1u, 0x85EBCA77u, 0xC2B2AE3Du);
        h = h.yzwx ^ (h >> 15);
        h = (h ^ v5) * uint4(0x01000193u, 0x9E3779B1u, 0x85EBCA77u, 0xC2B2AE3Du);
        h = h.yzwx ^ (h >> 15);
        h = (h ^ v6) * uint4(0x01000193u, 0x9E3779B1u, 0x85EBCA77u, 0xC2B2AE3Du);
        h = h.yzwx ^ (h >> 15);
        h = (h ^ v7) * uint4(0x01000193u, 0x9E3779B1u, 0x85EBCA77u, 0xC2B2AE3Du);
        h = h.yzwx ^ (h >> 15);
    }
    [loop]
    for (; at < hi; at += 1024u) {
        h = (h ^ nows[copy].Load4(at)) * uint4(0x01000193u, 0x9E3779B1u, 0x85EBCA77u, 0xC2B2AE3Du);
        h = h.yzwx ^ (h >> 15);
    }
    parts[thread] = h;
    GroupMemoryBarrierWithGroupSync();
    if (thread != 0u) return;
    h = parts[0];
    [loop]
    for (uint i = 1u; i < 64u; ++i) {
        h = (h ^ parts[i]) * uint4(0x01000193u, 0x9E3779B1u, 0x85EBCA77u, 0xC2B2AE3Du);
        h = h.yzwx ^ (h >> 15);
    }
    if (compare != 0u) {
        uint folded = h.x ^ (h.y * 3u) ^ (h.z * 5u) ^ (h.w * 7u);
        differs.Store(out_at, (folded << 1u) | (any(h != hashes[copy].Load4(piece * 16u)) ? 1u : 0u));
        if (compare == 2u) hashes[copy].Store4(piece * 16u, h);
    } else {
        hashes[copy].Store4(piece * 16u, h);
    }
}
"#;

/// A group's entry in the table ([`CARD_COMPARE_TABLE`]): the copy's index
/// `copy` among the dispatch's (its views at `2 * copy`), the compare's
/// `mode`, the copy's `skew`, the `piece`, its result's dword `at` in
/// `differs`, the copy's hashed `size`.
pub(crate) fn entry(copy: u32, mode: u32, skew: u32, piece: u32, at: u32, size: u32) -> [u32; 4] {
    debug_assert!(copy < COPIES as u32 && mode < 4 && skew < 1 << 16);
    [skew | (copy << 16) | (mode << 30), piece, at * 4, size]
}

/// Views in the descriptor ring: two per copy a list's dispatch reads (8 MB
/// of heap; more than the compare's results hold dwords, so that a ring
/// half taken, which has flights land to start it again, is not this one).
const SLOTS: u32 = 1 << 18;
/// Entries in the table ring (16 bytes each, 8 MB): more than the
/// compare's results hold (2^18 dwords), so the results run out first.
const ENTRIES: u32 = 1 << 19;
/// The most copies one dispatch can name: the shader's descriptor tables
/// are 2048 views long (a bound the compiler wants), two a copy.
const COPIES: usize = 1024;
/// The views a dispatch's two tables reach from their first slot.
const REACH: u32 = 2 * COPIES as u32 + 1;
/// The largest buffer a raw view takes (2^27 dwords); larger copies keep
/// their passes as before.
const VIEW_MOST: u64 = 1 << 29;

/// Since the last frame-rate line: lists whose passes went into one
/// dispatch (hash, compare), the dispatches they took, lists that had their
/// passes as before for want of room, passes left as before for a copy too
/// large for a view.
pub(super) static STATS: [AtomicU64; 5] = [const { AtomicU64::new(0) }; 5];

pub(super) mod stat {
    pub(crate) const HASH_LISTS: usize = 0;
    pub(crate) const COMPARE_LISTS: usize = 1;
    pub(crate) const DISPATCHES: usize = 2;
    pub(crate) const NO_ROOM: usize = 3;
    pub(crate) const TOO_LARGE: usize = 4;
}

/// A pass as the root-descriptor path records it ([`super::card_pass`]).
pub(super) struct Pass<'a> {
    pub(super) buffer: &'a ID3D12Resource,
    pub(super) before: &'a ID3D12Resource,
    /// The copy's hashed size ([`crate::shadow::Shadow::card_hash_size`]).
    pub(super) size: u32,
    pub(super) span: crate::shadow::CardSpan,
    /// The first piece's dword in `differs`.
    pub(super) at: u32,
    pub(super) mode: u32,
}

/// What [`Table`] keeps: the pipeline, the descriptor ring, the table ring.
pub(super) struct Table {
    root: ID3D12RootSignature,
    pso: ID3D12PipelineState,
    views: ID3D12DescriptorHeap,
    cpu: usize,
    gpu: u64,
    step: u32,
    /// The table ring (upload memory, mapped for good) and its address.
    _entries: ID3D12Resource,
    data: usize,
    entries_at: u64,
    /// Taken by plans whose lists have not run yet.
    slots_taken: u32,
    entries_taken: u32,
}

// SAFETY: D3D12 objects (free-threaded) and a persistent mapping, used only
// under `CARD_WORK`'s lock.
unsafe impl Send for Table {}

/// A dispatch ready to record ([`Table::prepare`]).
pub(super) struct Prepared {
    srvs: u64,
    uavs: u64,
    first: u32,
    groups: u32,
}

impl Table {
    pub(super) fn make(host: &Host) -> windows::core::Result<Table> {
        let bytes = compile(CARD_COMPARE_TABLE, windows::core::s!("main"), windows::core::s!("cs_5_1"))?;
        let kind = D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV;
        let (entries, data) =
            host.upload_buffer(u64::from(ENTRIES) * 16).ok_or_else(windows::core::Error::empty)?;
        name(&entries, || "card compare table".into());
        // SAFETY: the root signature is inside the shader; the bytecode and
        // root signature remain live through pipeline creation; plain heap
        // creation and queries.
        unsafe {
            let root: ID3D12RootSignature = host.device.CreateRootSignature(0, &bytes)?;
            let pso: ID3D12PipelineState = host.device.CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                pRootSignature: std::mem::transmute_copy(&root),
                CS: D3D12_SHADER_BYTECODE {
                    pShaderBytecode: bytes.as_ptr().cast(),
                    BytecodeLength: bytes.len(),
                },
                ..Default::default()
            })?;
            let views: ID3D12DescriptorHeap = host.device.CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                Type: kind,
                NumDescriptors: SLOTS,
                Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
                NodeMask: 0,
            })?;
            Ok(Table {
                cpu: views.GetCPUDescriptorHandleForHeapStart().ptr,
                gpu: views.GetGPUDescriptorHandleForHeapStart().ptr,
                step: host.device.GetDescriptorHandleIncrementSize(kind),
                views,
                root,
                pso,
                entries_at: entries.GetGPUVirtualAddress(),
                _entries: entries,
                data: data as usize,
                slots_taken: 0,
                entries_taken: 0,
            })
        }
    }

    /// The passes `passes` as one table: their copies' views and their
    /// pieces' entries written, room taken. `None` (nothing taken) if the
    /// rings have no room; the caller records the passes as before. Passes
    /// of a copy too large for a view are left out of the table: their
    /// indices are given back for the caller to record as before.
    pub(super) fn prepare(&mut self, device: &ID3D12Device, passes: &[Pass]) -> Option<(Prepared, Vec<usize>)> {
        let mut copies: Vec<(*const std::ffi::c_void, *const std::ffi::c_void)> = Vec::new();
        let mut of: Vec<Option<u32>> = Vec::with_capacity(passes.len());
        let mut left = Vec::new();
        let mut groups = 0u32;
        for (i, p) in passes.iter().enumerate() {
            // SAFETY: plain queries of live buffers.
            let (now, before) = unsafe { (p.buffer.GetDesc().Width, p.before.GetDesc().Width) };
            if now > VIEW_MOST || before > VIEW_MOST {
                left.push(i);
                of.push(None);
                continue;
            }
            let key = (p.buffer.as_raw().cast_const(), p.before.as_raw().cast_const());
            let k = match copies.iter().position(|c| *c == key) {
                Some(k) => k,
                None => {
                    copies.push(key);
                    copies.len() - 1
                }
            };
            of.push(Some(k as u32));
            groups += p.span.pieces;
        }
        if !left.is_empty() {
            STATS[stat::TOO_LARGE].fetch_add(left.len() as u64, Ordering::Relaxed);
        }
        let slots = 2 * copies.len() as u32;
        if copies.len() > COPIES
            || self.slots_taken + slots.max(REACH) > SLOTS
            || u64::from(self.entries_taken) + u64::from(groups) > u64::from(ENTRIES)
        {
            STATS[stat::NO_ROOM].fetch_add(1, Ordering::Relaxed);
            return None;
        }
        let slot0 = self.slots_taken;
        let first = self.entries_taken;
        self.slots_taken += slots;
        self.entries_taken += groups;
        // The views: an SRV of the copy and a UAV of its hashes, side by side.
        let mut written = vec![false; copies.len()];
        let mut next = first as usize;
        for (p, k) in passes.iter().zip(&of) {
            let Some(k) = *k else { continue };
            if !std::mem::replace(&mut written[k as usize], true) {
                let at = |slot: u32| D3D12_CPU_DESCRIPTOR_HANDLE { ptr: self.cpu + (slot * self.step) as usize };
                // SAFETY: plain queries of live buffers.
                let (now, before) = unsafe { (p.buffer.GetDesc().Width, p.before.GetDesc().Width) };
                let srv = D3D12_SHADER_RESOURCE_VIEW_DESC {
                    Format: DXGI_FORMAT_R32_TYPELESS,
                    ViewDimension: D3D12_SRV_DIMENSION_BUFFER,
                    Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
                    Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
                        Buffer: D3D12_BUFFER_SRV {
                            FirstElement: 0,
                            NumElements: (now / 4) as u32,
                            StructureByteStride: 0,
                            Flags: D3D12_BUFFER_SRV_FLAG_RAW,
                        },
                    },
                };
                let uav = D3D12_UNORDERED_ACCESS_VIEW_DESC {
                    Format: DXGI_FORMAT_R32_TYPELESS,
                    ViewDimension: D3D12_UAV_DIMENSION_BUFFER,
                    Anonymous: D3D12_UNORDERED_ACCESS_VIEW_DESC_0 {
                        Buffer: D3D12_BUFFER_UAV {
                            FirstElement: 0,
                            NumElements: (before / 4) as u32,
                            StructureByteStride: 0,
                            CounterOffsetInBytes: 0,
                            Flags: D3D12_BUFFER_UAV_FLAG_RAW,
                        },
                    },
                };
                // SAFETY: slots of our heap taken above (no list of ours on
                // the card reads them: the ring starts again only when none
                // is); live buffers.
                unsafe {
                    device.CreateShaderResourceView(p.buffer, Some(&srv), at(slot0 + 2 * k));
                    device.CreateUnorderedAccessView(p.before, None::<&ID3D12Resource>, Some(&uav), at(slot0 + 2 * k + 1));
                }
            }
            for i in 0..p.span.pieces {
                let e = entry(k, p.mode, p.span.skew, p.span.first + i, p.at + i, p.size);
                // SAFETY: inside the mapping (room taken above); no list of
                // ours on the card reads these entries.
                unsafe { (self.data as *mut [u32; 4]).add(next).write_unaligned(e) };
                next += 1;
            }
        }
        let handle = |slot: u32| self.gpu + u64::from(slot * self.step);
        Some((Prepared { srvs: handle(slot0), uavs: handle(slot0 + 1), first, groups }, left))
    }

    /// Records `prepared` into `list`, results into `differs` (its address).
    /// Leaves the list with our descriptor heap and this pipeline set; gives
    /// the dispatches recorded.
    ///
    /// # Safety
    /// An open list of ours; the buffers of the prepared passes are alive
    /// until it has run.
    pub(super) unsafe fn record(&self, list: &ID3D12GraphicsCommandList, prepared: &Prepared, differs: u64) -> u64 {
        const MOST: u32 = D3D12_CS_DISPATCH_MAX_THREAD_GROUPS_PER_DIMENSION;
        // SAFETY: the caller's.
        unsafe {
            list.SetDescriptorHeaps(&[Some(self.views.clone())]);
            list.SetComputeRootSignature(&self.root);
            list.SetPipelineState(&self.pso);
            list.SetComputeRootDescriptorTable(1, D3D12_GPU_DESCRIPTOR_HANDLE { ptr: prepared.srvs });
            list.SetComputeRootDescriptorTable(2, D3D12_GPU_DESCRIPTOR_HANDLE { ptr: prepared.uavs });
            list.SetComputeRootShaderResourceView(3, self.entries_at);
            list.SetComputeRootUnorderedAccessView(4, differs);
            let mut done = 0;
            let mut dispatches = 0;
            while done < prepared.groups {
                let n = (prepared.groups - done).min(MOST);
                list.SetComputeRoot32BitConstant(0, prepared.first + done, 0);
                list.Dispatch(n, 1, 1);
                STATS[stat::DISPATCHES].fetch_add(1, Ordering::Relaxed);
                dispatches += 1;
                done += n;
            }
            dispatches
        }
    }

    /// No plan of ours is on the card: both rings start again.
    pub(super) fn restart(&mut self) {
        self.slots_taken = 0;
        self.entries_taken = 0;
    }

    /// Whether either ring is half taken ([`Host::card_results_low`]).
    pub(super) fn low(&self) -> bool {
        self.slots_taken * 2 > SLOTS || self.entries_taken * 2 > ENTRIES
    }
}

/// The frame-rate line's part, if a switch is on.
pub(super) fn line(frames: f64) {
    if !hash_on() && !compare_on() {
        return;
    }
    let s: Vec<f64> = STATS.iter().map(|n| n.swap(0, Ordering::Relaxed) as f64 / frames).collect();
    tracing::info!(
        "our passes over copies in the card's memory as one dispatch per list over a table of pieces per frame \
         (VICTORY_CARD_HASH_TABLE={}, VICTORY_CARD_COMPARE_TABLE={}): {:.1} lists of hash passes and {:.1} of compares, \
         in {:.1} dispatches; {:.2} lists left as before for want of room, {:.1} passes for a copy too large for a view",
        u8::from(hash_on()),
        u8::from(compare_on()),
        s[stat::HASH_LISTS],
        s[stat::COMPARE_LISTS],
        s[stat::DISPATCHES],
        s[stat::NO_ROOM],
        s[stat::TOO_LARGE],
    );
}
