//! Scratch memory of the host's builds (`VICTORY_DXR_SCRATCH_MB`).
//!
//! Unset (the default), as before: every build takes scratch memory of its
//! own from the pool, which the list holds until it is reset. Builds of one
//! recording may overlap on the card, so they cannot share it; but a list
//! that unpacks hundreds of structures while the title streams holds
//! hundreds of ranges at once, and the pool's chunks, once made, stay.
//!
//! With `VICTORY_DXR_SCRATCH_MB=N`, the builds of one host list share at
//! most N MiB (a structure that needs more still gets its own): ranges are
//! cut from pieces the list holds until it is reset, and when the pieces are
//! used up a barrier on every resource ends the builds recorded before it,
//! whose scratch the next ones then take again. Every recording of ours
//! starts and ends with such a barrier (`super::native::record_on`), so each
//! starts at the beginning of the list's pieces.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use windows::Win32::Graphics::Direct3D12::ID3D12GraphicsCommandList4;

use super::count::{C, add};
use super::native::{Held, Refused, lease_scratch, wait_all};
use super::pool::{ALIGN, Lease};
use crate::host::HostList;

/// `VICTORY_DXR_SCRATCH_MB` in bytes (0: unset, each build its own).
fn budget() -> u64 {
    static BUDGET: OnceLock<u64> = OnceLock::new();
    *BUDGET.get_or_init(|| {
        let mb = std::env::var("VICTORY_DXR_SCRATCH_MB").ok().and_then(|v| v.trim().parse::<u64>().ok()).unwrap_or(0);
        if mb > 0 && super::native_on() {
            tracing::info!(mb, "VICTORY_DXR_SCRATCH_MB: the host's builds of one list share their scratch memory");
        }
        mb << 20
    })
}

/// The smallest piece taken.
const PIECE: u64 = 1 << 20;

/// The pieces one host list's builds share.
#[derive(Default)]
pub(super) struct Pieces {
    pieces: Vec<Arc<Lease>>,
}

/// Each host list's pieces, by the list's address; a list holds its own
/// (`Held`) until it is reset, so a dead entry is a list reset since.
static LISTS: Mutex<Option<HashMap<usize, Weak<Mutex<Pieces>>>>> = Mutex::new(None);

/// Where the builds of one recording take their scratch memory.
pub(super) struct Scratch {
    shared: Option<Arc<Mutex<Pieces>>>,
    /// The next range: a piece and an offset in it.
    piece: usize,
    offset: u64,
}

impl Scratch {
    /// For a recording into `host`'s list, which holds the pieces it uses
    /// (through `held`) until it is reset.
    pub(super) fn new(host: &HostList, held: &mut Held) -> Scratch {
        if budget() == 0 {
            return Scratch { shared: None, piece: 0, offset: 0 };
        }
        let key = std::ptr::from_ref(host) as usize;
        let mut lists = LISTS.lock().unwrap_or_else(|e| e.into_inner());
        let lists = lists.get_or_insert_with(HashMap::new);
        let shared = match lists.get(&key).and_then(Weak::upgrade) {
            Some(s) => s,
            None => {
                lists.retain(|_, w| w.strong_count() > 0);
                let s = Arc::new(Mutex::new(Pieces::default()));
                lists.insert(key, Arc::downgrade(&s));
                held.pieces.push(s.clone());
                s
            }
        };
        Scratch { shared: Some(shared), piece: 0, offset: 0 }
    }

    /// Everything recorded before is done (a barrier on every resource was
    /// just recorded): the pieces can be taken again from the start.
    pub(super) fn waited(&mut self) {
        (self.piece, self.offset) = (0, 0);
    }

    /// Scratch memory of `len` bytes for a build recorded now in `list`:
    /// its address on the card.
    pub(super) fn take(&mut self, list: &ID3D12GraphicsCommandList4, len: u64, held: &mut Held) -> Result<u64, Refused> {
        let Some(shared) = self.shared.clone() else {
            let lease = lease_scratch(len)?;
            let gpu = lease.gpu;
            held.leases.push(lease);
            return Ok(gpu);
        };
        let len = len.max(1).next_multiple_of(ALIGN);
        let mut p = shared.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(at) = self.fit(&p.pieces, len) {
            return Ok(at);
        }
        let total: u64 = p.pieces.iter().map(|l| l.len()).sum();
        let room = budget().saturating_sub(total);
        if room >= len {
            let size = len.max(PIECE.max(total)).min(room);
            return self.add(&mut p.pieces, size, len);
        }
        // Spent: the builds recorded before end here, and their scratch is
        // taken again.
        if (self.piece, self.offset) != (0, 0) {
            wait_all(list);
            add(C::ScratchWaits, 1);
            self.waited();
            if let Some(at) = self.fit(&p.pieces, len) {
                return Ok(at);
            }
        }
        // Bigger than any piece: one of its own, past the budget.
        self.add(&mut p.pieces, len, len)
    }

    /// `len` bytes from the next range on, if a piece from there has them.
    fn fit(&mut self, pieces: &[Arc<Lease>], len: u64) -> Option<u64> {
        let spans: Vec<(u64, u64)> = pieces.iter().map(|l| (l.gpu, l.len())).collect();
        let (at, piece, offset) = fit(&spans, self.piece, self.offset, len);
        (self.piece, self.offset) = (piece, offset);
        at
    }

    /// A new piece of `size` bytes, its first `len` taken.
    fn add(&mut self, pieces: &mut Vec<Arc<Lease>>, size: u64, len: u64) -> Result<u64, Refused> {
        let lease = lease_scratch(size)?;
        let at = lease.gpu;
        pieces.push(lease);
        (self.piece, self.offset) = (pieces.len() - 1, len);
        Ok(at)
    }
}

/// Of pieces at (address, length), `len` bytes from piece `piece` at
/// `offset` on: the address if one from there has room, and the next range
/// (past the last piece if none had room).
fn fit(pieces: &[(u64, u64)], mut piece: usize, mut offset: u64, len: u64) -> (Option<u64>, usize, u64) {
    while let Some(&(gpu, size)) = pieces.get(piece) {
        if offset + len <= size {
            return (Some(gpu + offset), piece, offset + len);
        }
        (piece, offset) = (piece + 1, 0);
    }
    (None, piece, offset)
}

#[cfg(test)]
mod tests {
    use super::fit;

    #[test]
    fn ranges_are_cut_from_the_pieces_in_order() {
        let pieces = [(0x1000_0000, 0x1000), (0x2000_0000, 0x4000)];
        // From the start: the first piece, then on in it.
        assert_eq!(fit(&pieces, 0, 0, 0x800), (Some(0x1000_0000), 0, 0x800));
        assert_eq!(fit(&pieces, 0, 0x800, 0x800), (Some(0x1000_0800), 0, 0x1000));
        // What is left of a piece too small: the next one.
        assert_eq!(fit(&pieces, 0, 0x800, 0x1000), (Some(0x2000_0000), 1, 0x1000));
        // Room in none from here: past the last piece.
        assert_eq!(fit(&pieces, 1, 0x3000, 0x2000), (None, 2, 0));
        assert_eq!(fit(&pieces, 0, 0, 0x8000), (None, 2, 0));
        assert_eq!(fit(&[], 0, 0, 1), (None, 0, 0));
    }
}
