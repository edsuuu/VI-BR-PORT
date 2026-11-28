//! The device's ray-tracing interface for the host's builds, and the card's
//! time of each recorded batch of them: a timestamp before and after,
//! resolved into a readback buffer that is read a few frames later.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};

use windows::Win32::Graphics::Direct3D12::*;
use windows::core::Interface;

use super::count::{C, add};

/// What the host's builds need of the device: its ray-tracing interface,
/// and timestamps.
pub(super) struct Gpu {
    pub(super) device: ID3D12Device5,
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
    pub(super) fn get() -> Option<&'static Gpu> {
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
    pub(super) fn begin(&self, list: &ID3D12GraphicsCommandList4) -> u32 {
        let slot = (self.next.fetch_add(1, Relaxed) % u64::from(PAIRS)) as u32;
        // SAFETY: an open list, our query heap.
        unsafe { list.EndQuery(&self.stamps, D3D12_QUERY_TYPE_TIMESTAMP, 2 * slot) };
        slot
    }

    /// The one after, resolved where the line reads it a few frames later.
    pub(super) fn end(&self, list: &ID3D12GraphicsCommandList4, slot: u32) {
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
    pub(super) fn take_times(&self) {
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
