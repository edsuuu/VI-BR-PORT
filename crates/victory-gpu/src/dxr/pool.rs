//! The card memory the host's own structures and their scratch memory are
//! cut from: big buffers of ours (chunks), each cut into aligned ranges.
//! A range is held by a [`Lease`]; when the last holder lets go (the table
//! that names a structure, every list that builds, refits, copies or reads
//! it: a list holds what it recorded until it is reset) the range goes
//! back to its chunk.
//!
//! Structures live in buffers in the ray-tracing structure state for good
//! (Direct3D allows no other); scratch memory in buffers in the unordered
//! access state for good. Neither is ever moved to another state, so no
//! list has to know where they are.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;

/// Structures, their scratch memory and the buffers handed out are aligned
/// so (`D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BYTE_ALIGNMENT`).
pub(crate) const ALIGN: u64 = 256;

/// The free ranges of one buffer, by offset, joined where they touch.
pub(crate) struct Ranges {
    free: BTreeMap<u64, u64>,
}

impl Ranges {
    pub(crate) fn new(size: u64) -> Ranges {
        let mut free = BTreeMap::new();
        if size > 0 {
            free.insert(0, size);
        }
        Ranges { free }
    }

    /// The offset of `len` bytes on a multiple of `align` (a power of two),
    /// the lowest that fits; `None` if none does.
    pub(crate) fn take(&mut self, len: u64, align: u64) -> Option<u64> {
        let len = len.max(1);
        let (&start, &size, at) = self.free.iter().find_map(|(start, size)| {
            let at = start.next_multiple_of(align);
            (at + len <= start + size).then_some((start, size, at))
        })?;
        self.free.remove(&start);
        if at > start {
            self.free.insert(start, at - start);
        }
        if at + len < start + size {
            self.free.insert(at + len, start + size - at - len);
        }
        Some(at)
    }

    /// `[at, at + len)` is free again.
    pub(crate) fn give(&mut self, at: u64, len: u64) {
        let len = len.max(1);
        let (mut start, mut end) = (at, at + len);
        if let Some((&before, &size)) = self.free.range(..at).next_back()
            && before + size == at
        {
            self.free.remove(&before);
            start = before;
        }
        if let Some(&size) = self.free.get(&end) {
            self.free.remove(&end);
            end += size;
        }
        self.free.insert(start, end - start);
    }

    /// Free bytes.
    pub(crate) fn free(&self) -> u64 {
        self.free.values().sum()
    }
}

/// What a pool's buffers are for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Kind {
    Structures,
    Scratch,
}

struct Chunk {
    buffer: ID3D12Resource,
    gpu: u64,
    size: u64,
    ranges: Ranges,
}

/// A pool of chunks of one kind.
pub(crate) struct Pool {
    kind: Kind,
    chunks: Vec<Option<Chunk>>,
    /// Bytes held by leases, and the chunks' bytes.
    used: u64,
    made: u64,
}

// SAFETY: D3D12 resources are free-threaded.
unsafe impl Send for Pool {}

/// A chunk's size unless one range needs more.
const CHUNK: u64 = 64 << 20;

impl Pool {
    pub(crate) const fn new(kind: Kind) -> Pool {
        Pool { kind, chunks: Vec::new(), used: 0, made: 0 }
    }

    /// Bytes held, and bytes of card memory the pool has.
    pub(crate) fn bytes(&self) -> (u64, u64) {
        (self.used, self.made)
    }
}

/// A range of a pool's buffer, given back when the last holder lets go.
pub(crate) struct Lease {
    pool: &'static Mutex<Pool>,
    chunk: usize,
    at: u64,
    len: u64,
    /// The buffer (held: a list recording into it holds the lease, not the
    /// pool) and the range's address on the card.
    _buffer: ID3D12Resource,
    pub(crate) gpu: u64,
}

// SAFETY: as `Pool`.
unsafe impl Send for Lease {}
// SAFETY: as above; nothing in it changes after it is made.
unsafe impl Sync for Lease {}

impl Lease {
    pub(crate) fn len(&self) -> u64 {
        self.len
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        let mut pool = self.pool.lock().unwrap_or_else(|e| e.into_inner());
        pool.used = pool.used.saturating_sub(self.len);
        let Some(chunk) = pool.chunks.get_mut(self.chunk).and_then(Option::as_mut) else { return };
        chunk.ranges.give(self.at, self.len);
        // A chunk of its own for one big range goes with it.
        if chunk.size > CHUNK && chunk.ranges.free() == chunk.size {
            let size = chunk.size;
            pool.chunks[self.chunk] = None;
            pool.made -= size;
        }
    }
}

/// A buffer of ours on the card for the pool's kind.
fn chunk_buffer(device: &ID3D12Device, kind: Kind, size: u64) -> Option<ID3D12Resource> {
    let desc = D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
        Width: size,
        Height: 1,
        DepthOrArraySize: 1,
        MipLevels: 1,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
        Flags: D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
        ..Default::default()
    };
    let state = match kind {
        Kind::Structures => D3D12_RESOURCE_STATE_RAYTRACING_ACCELERATION_STRUCTURE,
        Kind::Scratch => D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
    };
    let mut buffer: Option<ID3D12Resource> = None;
    // SAFETY: a plain committed buffer of ours.
    let made = unsafe {
        device.CreateCommittedResource(
            &D3D12_HEAP_PROPERTIES { Type: D3D12_HEAP_TYPE_DEFAULT, ..Default::default() },
            D3D12_HEAP_FLAG_NONE,
            &desc,
            state,
            None,
            &mut buffer,
        )
    };
    match made {
        Ok(()) => {
            let buffer = buffer?;
            crate::host::name(&buffer, || format!("host ray-tracing {kind:?} {size:#x}"));
            Some(buffer)
        }
        Err(e) => {
            tracing::warn!(error = %e, size, ?kind, "host ray tracing: no card buffer for structures");
            None
        }
    }
}

/// `len` bytes of `pool` (a static of ours), at most `most` held in all.
pub(crate) fn lease(pool: &'static Mutex<Pool>, len: u64, most: u64) -> Result<Arc<Lease>, &'static str> {
    let host = crate::host::host().ok_or("no host device")?;
    let len = len.max(1).next_multiple_of(ALIGN);
    let mut p = pool.lock().unwrap_or_else(|e| e.into_inner());
    if p.used + len > most {
        return Err("the memory ceiling of host structures (VICTORY_DXR_NATIVE_MB)");
    }
    let mut found = None;
    for (i, chunk) in p.chunks.iter_mut().enumerate() {
        if let Some(c) = chunk
            && let Some(at) = c.ranges.take(len, ALIGN)
        {
            found = Some((i, at));
            break;
        }
    }
    let (i, at) = match found {
        Some(f) => f,
        None => {
            let size = len.max(CHUNK);
            let buffer = chunk_buffer(&host.device, p.kind, size).ok_or("no card memory for another chunk")?;
            // SAFETY: a live buffer.
            let gpu = unsafe { buffer.GetGPUVirtualAddress() };
            let mut ranges = Ranges::new(size);
            let at = ranges.take(len, ALIGN).ok_or("a chunk too small")?;
            let chunk = Chunk { buffer, gpu, size, ranges };
            p.made += size;
            let i = match p.chunks.iter().position(Option::is_none) {
                Some(i) => {
                    p.chunks[i] = Some(chunk);
                    i
                }
                None => {
                    p.chunks.push(Some(chunk));
                    p.chunks.len() - 1
                }
            };
            (i, at)
        }
    };
    p.used += len;
    let chunk = p.chunks[i].as_ref().ok_or("a chunk gone")?;
    Ok(Arc::new(Lease { pool, chunk: i, at, len, _buffer: chunk.buffer.clone(), gpu: chunk.gpu + at }))
}

#[cfg(test)]
mod tests {
    use super::Ranges;

    #[test]
    fn ranges_are_taken_aligned_and_joined_when_given_back() {
        let mut r = Ranges::new(4096);
        assert_eq!(r.take(100, 256), Some(0));
        assert_eq!(r.take(100, 256), Some(256));
        assert_eq!(r.take(1000, 256), Some(512));
        // What is left: 100..256, 356..512, 1512..4096.
        assert_eq!(r.free(), 4096 - 1200);
        assert_eq!(r.take(4000, 256), None);
        r.give(256, 100);
        // 100..512 is one range again: 412 bytes, 256 of them from 256 on.
        assert_eq!(r.take(256, 256), Some(256));
        r.give(256, 256);
        r.give(0, 100);
        r.give(512, 1000);
        assert_eq!(r.free(), 4096);
        // Joined into one: the whole buffer fits again.
        assert_eq!(r.take(4096, 256), Some(0));
        assert_eq!(r.take(1, 1), None);
    }

    #[test]
    fn an_empty_buffer_has_no_room() {
        let mut r = Ranges::new(0);
        assert_eq!(r.take(1, 1), None);
        assert_eq!(r.free(), 0);
    }
}
