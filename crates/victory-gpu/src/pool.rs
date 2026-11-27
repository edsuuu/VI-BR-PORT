//! Host memory for the textures the title fills, and for their staging
//! buffers, cut from blocks made once.
//!
//! Each such texture had an allocation of its own and its staging buffer
//! another. On an RX 7900 XTX an allocation takes about 310 microseconds
//! whatever its size, on one thread (`texture::allocation_costs`), and a
//! texture cut from a block 6: the pass that makes the textures shaders
//! pick by index (`draw.rs` `heap_textures`) got through 350-500 of the
//! 24,000 queued when the world loads in each of its 250 ms turns, some
//! forty seconds of a world without its textures.
//! `VICTORY_TEXTURE_POOL=0` makes every one an allocation of its own again.
//!
//! A piece given back is used again only two seconds later (nothing the card
//! was told before that is still running: Windows resets a card that takes
//! that long over one submission), and by then no list names it (the owners' rule, as for allocations of their own): a
//! texture's piece comes back when the last reference to the host texture
//! goes (an object of ours the texture holds says so), a staging buffer's
//! when the last list that recorded its copy is reset.

use std::collections::{BTreeMap, VecDeque};
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};

use windows::Win32::Graphics::Direct3D12::*;
use windows::core::{GUID, HRESULT, IUnknown, Interface};

/// Bytes of a block; pieces of more than half of one are not pooled (with
/// blocks made ahead, [`ahead`]: of more than one).
const BLOCK: u64 = 64 << 20;
/// Blocks with nothing in them that are kept.
const SPARE: usize = 2;
/// The same while the card has room ([`room`]): 4 GiB of them.
const SPARE_KEPT: usize = 64;
/// The same of blocks of staging buffers, which are the PC's memory: run
/// crash-1 held 5.2 GB of them.
const STAGING_KEPT: usize = 8;
/// How many are kept now.
static SPARES: AtomicUsize = AtomicUsize::new(SPARE);

/// Sets how many empty blocks stay, by the card's memory (budget and use):
/// [`SPARE_KEPT`] below 85 % ([`crate::host::keep_freed`]), else [`SPARE`].
/// Asked once a second.
pub(crate) fn room(memory: Option<[u64; 4]>) {
    SPARES.store(spares(memory, crate::host::keep_freed()), Ordering::Relaxed);
}

fn spares(memory: Option<[u64; 4]>, keep: bool) -> usize {
    match memory {
        Some([budget, used, ..]) if keep && u128::from(used) * 100 < u128::from(budget) * 85 => SPARE_KEPT,
        _ => SPARE,
    }
}

/// Milliseconds until a piece given back is used again. (It was three
/// frames, a tenth of a second while the title shows a loading screen at 30
/// a second, with up to 18 submissions in the air.)
const REST: u64 = 2500;

fn now() -> u64 {
    crate::host::now_ms()
}

pub(crate) fn on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_TEXTURE_POOL").is_none_or(|v| v != "0"))
}

/// Whether a new block of textures is made only while the card has room for
/// it (`VICTORY_POOL_GATE=1`; off by default for now: a block is made
/// whenever no block has room, as before).
///
/// A block is 64 MiB and goes back to the card only when its last texture
/// does ([`Pool::rested`]). The texture queue and the sweep
/// (`crate::resource::room`, `let_go_idle`) weigh the card's use *less the
/// free part of the blocks*, so whenever a texture fits no free piece (pieces
/// rest for 2.5 s, and a large texture wants one unbroken range) a new block
/// is made beside them and the card's use goes up for good: on the 8 GB card
/// with the Series S profile the prison walk stood at 5,792 MB in use of a
/// budget of 5,666 MB, and the frame fell from 30 a second to 8-17 for
/// seconds. With this a block is made only while the card's whole use, with
/// that block and with what is still to come of render targets and buffer
/// copies, stays under the queue's line (`crate::resource::block_room`).
/// Without that room a texture goes into a free piece of a block there is, or
/// it waits: the texture pass leaves its slot due (`draw.rs` `heap_textures`;
/// the slot shows what it showed), and the queue and the sweep then go by the
/// free part of the blocks (`crate::resource::weighed`). A texture that
/// cannot wait (a draw reads it through a table now, or a bake) still gets
/// its block ([`PAST`]). Nothing is moved, and nothing is given back that was
/// not before.
pub(crate) fn gate() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_POOL_GATE").is_some_and(|v| v == "1"))
}

/// What had no place: `known` if a piece of `len` bytes at `align` would
/// have none where that had none, else this one.
fn smaller(known: Option<(u64, u64)>, len: u64, align: u64) -> (u64, u64) {
    match known {
        Some((l, a)) if l <= len && a <= align => (l, a),
        _ => (len, align),
    }
}

/// Whether a block may be made for a piece that fits none ([`gate`]).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Grow {
    /// The card has room (or the gate is off): as before, and blocks ahead
    /// too.
    Yes,
    /// It has none, and the texture cannot wait: a block for it, none ahead.
    Must,
    /// It has none, and the texture can wait ([`waiting`]): no block.
    No,
}

/// Since the start: blocks of textures made; times a block was not made for
/// want of room on the card (a texture then waited); blocks made without
/// that room for a texture that could not wait.
pub(crate) static BLOCKS: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];
const MADE: usize = 0;
const NOT_MADE: usize = 1;
const PAST: usize = 2;

/// Whether the card had no room for a new block when last asked
/// (`crate::resource::block_room`): the free pieces of the blocks are then
/// all the room there is for textures.
static FULL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// See [`FULL`]; always false with [`gate`] off.
pub(crate) fn full() -> bool {
    gate() && FULL.load(Ordering::Relaxed)
}

/// `crate::resource::block_room`'s answer, for [`full`].
pub(crate) fn note_full(full: bool) {
    FULL.store(full, Ordering::Relaxed);
}

/// Whether the card has room for a new block of textures
/// (`crate::resource::block_room`), as it was at most a tenth of a second
/// ago. Always with [`gate`] off.
fn may_grow() -> bool {
    if !gate() {
        return true;
    }
    // The tenth of a second of the last asking, counted from 1, and its
    // answer in the lowest bit (as `crate::resource::room_lately`).
    static LAST: AtomicU64 = AtomicU64::new(0);
    let tenth = now() / 100 + 1;
    let last = LAST.load(Ordering::Relaxed);
    if last >> 1 == tenth {
        return last & 1 != 0;
    }
    let room = crate::resource::block_room(crate::host::host().and_then(|h| h.video_memory()));
    LAST.store((tenth << 1) | u64::from(room), Ordering::Relaxed);
    room
}

thread_local! {
    /// The texture being made on this thread can wait for a place
    /// ([`waiting`]), and the last one asked for did ([`waits`]).
    static CAN_WAIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static WAITED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Runs `make`, which may make a host texture that can wait: if no block
/// has a place for it and the card has no room for a new block ([`gate`]),
/// it is not made, and the second value returned says so. Only the pass
/// over textures picked by index asks this way: it leaves the slot due.
pub(crate) fn waiting<T>(make: impl FnOnce() -> T) -> (T, bool) {
    let was = CAN_WAIT.replace(true);
    WAITED.set(false);
    let made = make();
    CAN_WAIT.set(was);
    (made, WAITED.replace(false))
}

/// Within [`waiting`]: whether the texture this thread just asked a block
/// for ([`texture`]) was not made for want of a place. (False outside it.)
pub(crate) fn waits() -> bool {
    WAITED.get()
}

/// How many blocks of textures are made ahead, so that one has been there
/// for a while when its first piece is taken (`VICTORY_RIPE_BLOCKS`, 12;
/// 0: none, and new blocks are used at once, as before).
///
/// A page fault of lists already on the card, in a texture made the same
/// tenth of a second as the first piece of a block made for it, lost the
/// device in runs n08 and n11 (2026-11-25): the title gives a slot that
/// those lists read by index a new texture. Why the card does not have the
/// new block's addresses yet is not known (n11 waited for `MakeResident`;
/// `indirect::tests::new_blocks_under_readers` never loses it). A block that
/// every submission begun before its making has left behind ([`Ripe`]) has
/// no such lists: pieces are cut from those first.
fn ahead() -> usize {
    static AHEAD: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *AHEAD.get_or_init(|| std::env::var("VICTORY_RIPE_BLOCKS").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(12))
}

/// Submissions begun and finished ([`crate::host::submissions`]), and how
/// many blocks to make ahead (0: every block is ripe at once).
#[derive(Clone, Copy)]
struct Ripe {
    started: u64,
    finished: u64,
    ahead: usize,
    /// Whether a block may be made ([`gate`]).
    grow: Grow,
}

impl Ripe {
    const ALL: Ripe = Ripe { started: 0, finished: 0, ahead: 0, grow: Grow::Yes };

    fn now() -> Ripe {
        let (started, finished) = crate::host::submissions();
        // Not more than are kept ([`room`]): they would go and be made again.
        Ripe { started, finished, ahead: ahead().min(SPARES.load(Ordering::Relaxed)), grow: Grow::Yes }
    }

    /// Whether a piece of `len` bytes is cut from a block at all.
    fn pooled(self, len: u64) -> bool {
        // With blocks made ahead, what would be an allocation of its own (and
        // new when the card first reads it) lies in one of those too.
        len != 0 && len <= if self.ahead > 0 { BLOCK } else { BLOCK / 2 }
    }

    fn is<B>(self, block: &Block<B>) -> bool {
        self.ahead == 0 || block.made <= self.finished
    }
}

/// Pieces cut from blocks that were not ripe, for want of a ripe one.
static UNRIPE: AtomicU64 = AtomicU64::new(0);

/// The free ranges of a block: start and length, never two that touch.
struct Ranges(BTreeMap<u64, u64>);

impl Ranges {
    fn new(size: u64) -> Ranges {
        Ranges(BTreeMap::from([(0, size)]))
    }

    /// The lowest place for `len` bytes at a multiple of `align`.
    fn take(&mut self, len: u64, align: u64) -> Option<u64> {
        let (start, size, at) = self.0.iter().find_map(|(&start, &size)| {
            let at = start.next_multiple_of(align);
            (at + len <= start + size).then_some((start, size, at))
        })?;
        self.0.remove(&start);
        if at > start {
            self.0.insert(start, at - start);
        }
        if at + len < start + size {
            self.0.insert(at + len, start + size - at - len);
        }
        Some(at)
    }

    fn give(&mut self, mut start: u64, mut len: u64) {
        if let Some((&before, &size)) = self.0.range(..start).next_back()
            && before + size == start
        {
            self.0.remove(&before);
            (start, len) = (before, len + size);
        }
        if let Some(size) = self.0.remove(&(start + len)) {
            len += size;
        }
        self.0.insert(start, len);
    }

    fn whole(&self, size: u64) -> bool {
        self.0.get(&0) == Some(&size)
    }
}

/// A part of a block.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Piece {
    block: usize,
    start: u64,
    len: u64,
}

struct Block<B> {
    memory: B,
    free: Ranges,
    /// The end of the highest piece ever taken: above it the memory is as
    /// it was made (zeros).
    used_to: u64,
    /// The newest submission begun when it was made ([`Ripe`]).
    made: u64,
    /// When that was (ms).
    #[cfg_attr(not(feature = "diagnostics"), allow(dead_code))]
    since: u64,
}

struct Pool<B> {
    /// `None`: a block let go, its place to be used again.
    blocks: Vec<Option<Block<B>>>,
    /// Pieces given back, and when each may be used again.
    resting: VecDeque<(u64, Piece)>,
    /// The empty blocks it was last told to keep, and the most it keeps.
    spare: usize,
    most: usize,
    /// The smallest piece (bytes, alignment) that fitted no block since a
    /// piece last came free or a block was made: one as long and as aligned
    /// fits none either, and one that may wait ([`Grow::No`]) is not looked
    /// for again (a texture that waits asks again and again).
    no_fit: Option<(u64, u64)>,
}

impl<B: Clone> Pool<B> {
    const fn new(most: usize) -> Pool<B> {
        Pool { blocks: Vec::new(), resting: VecDeque::new(), spare: 0, most, no_fit: None }
    }

    /// A piece of `len` bytes and the memory it lies in, and whether that
    /// part was ever used before; `make` makes a block when none has room.
    fn take(&mut self, len: u64, align: u64, now: u64, make: impl FnMut() -> Option<B>) -> Option<(Piece, B, bool)> {
        self.take_ripe(len, align, now, Ripe::ALL, make)
    }

    /// The same from a block that is ripe, if one has room; `make` makes one
    /// ahead too, when few are empty.
    fn take_ripe(
        &mut self,
        len: u64,
        align: u64,
        now: u64,
        ripe: Ripe,
        mut make: impl FnMut() -> Option<B>,
    ) -> Option<(Piece, B, bool)> {
        if !ripe.pooled(len) {
            return None;
        }
        self.rested(now);
        // Known to fit no block there is ([`Pool::no_fit`]). Only a piece
        // that may wait skips the look: any other makes a block then, after
        // the same look as before.
        let known = ripe.grow == Grow::No && self.no_fit.is_some_and(|(l, a)| len >= l && align >= a);
        let mut found = None;
        if !known {
            for (i, block) in self.blocks.iter_mut().enumerate() {
                if let Some(block) = block
                    && ripe.is(block)
                    && let Some(start) = block.free.take(len, align)
                {
                    found = Some((i, start));
                    break;
                }
            }
        }
        if found.is_none() && !known && ripe.ahead > 0 {
            // The one that has been there the longest.
            let mut unripe: Vec<(u64, usize)> =
                self.blocks.iter().enumerate().filter_map(|(i, b)| Some((b.as_ref()?.made, i))).collect();
            unripe.sort_unstable();
            found = unripe.into_iter().find_map(|(_, i)| Some((i, self.blocks[i].as_mut()?.free.take(len, align)?)));
        }
        let (i, start) = match found {
            Some(found) => found,
            None => {
                if !known {
                    // (Every block was looked at: all are ripe without
                    // blocks ahead, and with them the oldest were asked.)
                    self.no_fit = Some(smaller(self.no_fit, len, align));
                }
                // No block without room on the card for a texture that can
                // wait ([`gate`]).
                if ripe.grow == Grow::No {
                    return None;
                }
                let i = self.add(make()?, ripe.started);
                (i, self.blocks[i].as_mut()?.free.take(len, align)?)
            }
        };
        if ripe.ahead > 0 {
            if !ripe.is(self.blocks[i].as_ref()?) {
                let n = UNRIPE.fetch_add(1, Ordering::Relaxed) + 1;
                if n.is_power_of_two() {
                    tracing::info!("textures cut from blocks made while lists ran that still run: {n}");
                }
            }
            let empty = self.blocks.iter().flatten().filter(|b| b.free.whole(BLOCK)).count();
            // (None ahead without room on the card: [`gate`].)
            if empty < ripe.ahead
                && ripe.grow == Grow::Yes
                && let Some(memory) = make()
            {
                self.add(memory, ripe.started);
            }
        }
        let block = self.blocks[i].as_mut()?;
        let again = start < block.used_to;
        block.used_to = block.used_to.max(start + len);
        Some((Piece { block: i, start, len }, block.memory.clone(), again))
    }

    fn add(&mut self, memory: B, made: u64) -> usize {
        let i = match self.blocks.iter().position(Option::is_none) {
            Some(i) => i,
            None => {
                self.blocks.push(None);
                self.blocks.len() - 1
            }
        };
        self.blocks[i] = Some(Block { memory, free: Ranges::new(BLOCK), used_to: 0, made, since: crate::host::now_ms() });
        self.no_fit = None;
        i
    }

    fn give(&mut self, piece: Piece, now: u64) {
        self.resting.push_back((now + REST, piece));
    }

    /// Makes the pieces that rested long enough free, and lets go of empty
    /// blocks past [`SPARE`].
    fn rested(&mut self, now: u64) {
        let mut any = false;
        while let Some(&(ready, piece)) = self.resting.front()
            && ready <= now
        {
            self.resting.pop_front();
            if let Some(block) = self.blocks[piece.block].as_mut() {
                block.free.give(piece.start, piece.len);
                any |= block.free.whole(BLOCK);
            }
            self.no_fit = None;
        }
        // (Without a piece given back too, once there is less room.)
        let most = SPARES.load(Ordering::Relaxed).min(self.most);
        if any || most < self.spare {
            self.spare = most;
            let mut spare = 0;
            for block in &mut self.blocks {
                if block.as_ref().is_some_and(|b| b.free.whole(BLOCK)) {
                    spare += 1;
                    if spare > most {
                        *block = None;
                    }
                }
            }
        }
    }

    fn blocks(&self) -> usize {
        self.blocks.iter().flatten().count()
    }
}

/// A mapped upload buffer's memory.
#[derive(Clone)]
struct Upload(ID3D12Resource, *mut u8);
// SAFETY: the mapping is written by the owners of its pieces, each its own.
unsafe impl Send for Upload {}

static TEXTURES: Mutex<Pool<ID3D12Heap>> = Mutex::new(Pool::new(SPARE_KEPT));
static STAGING: Mutex<Pool<Upload>> = Mutex::new(Pool::new(STAGING_KEPT));

fn lock<T>(m: &'static Mutex<T>) -> std::sync::MutexGuard<'static, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Bytes of the pieces textures hold now.
static HELD: AtomicU64 = AtomicU64::new(0);

/// Bytes of the texture blocks no texture holds: the card's memory in use
/// by us that new textures take first.
pub(crate) fn texture_room() -> u64 {
    (lock(&TEXTURES).blocks() as u64 * BLOCK).saturating_sub(HELD.load(Ordering::Relaxed))
}

/// For the memory line: blocks of textures and of staging buffers, in MiB.
#[cfg(feature = "diagnostics")]
pub(crate) fn memory() -> [u64; 2] {
    [lock(&TEXTURES).blocks() as u64 * (BLOCK >> 20), lock(&STAGING).blocks() as u64 * (BLOCK >> 20)]
}

/// Part of a block of mapped upload memory, back in the pool when dropped.
pub(crate) struct StagingPiece(Piece);

impl Drop for StagingPiece {
    fn drop(&mut self) {
        lock(&STAGING).give(self.0, now());
    }
}

/// `bytes` of upload memory, all zeros: the buffer it lies in, where in
/// that buffer, and its mapping. `None`: not pooled.
pub(crate) fn staging(host: &crate::host::Host, bytes: u64) -> Option<(StagingPiece, ID3D12Resource, u64, *mut u8)> {
    if !on() {
        return None;
    }
    // Copies from a buffer begin at multiples of 512.
    let bytes = bytes.next_multiple_of(512);
    let (piece, memory, again) = lock(&STAGING).take(bytes, 512, now(), || {
        let (buffer, data) = host.upload_buffer(BLOCK)?;
        crate::host::name(&buffer, || "staging of textures (a block)".into());
        Some(Upload(buffer, data))
    })?;
    let owned = StagingPiece(piece);
    // SAFETY: the piece lies in the block's mapping.
    let data = unsafe { memory.1.add(piece.start as usize) };
    if again {
        // What is not detiled into it (an unmapped texture) uploads zeros,
        // as from a buffer of its own.
        // SAFETY: the piece is ours alone.
        unsafe { std::ptr::write_bytes(data, 0, bytes as usize) };
    }
    Some((owned, memory.0, piece.start, data))
}

/// An object the host texture holds and releases when it goes: its piece
/// is free then.
#[repr(C)]
struct Reclaim {
    methods: &'static [*const (); 3],
    references: AtomicU32,
    piece: Piece,
}

struct Methods([*const (); 3]);
// SAFETY: function addresses only.
unsafe impl Sync for Methods {}

static RECLAIM: Methods = Methods([query as *const (), add_ref as *const (), release as *const ()]);

unsafe extern "system" fn query(this: *mut c_void, iid: *const GUID, out: *mut *mut c_void) -> HRESULT {
    const E_NOINTERFACE: HRESULT = HRESULT(0x8000_4002_u32 as i32);
    // SAFETY: COM's arguments.
    unsafe {
        if out.is_null() {
            return HRESULT(0x8000_4003_u32 as i32);
        }
        if iid.as_ref().is_some_and(|iid| *iid == IUnknown::IID) {
            add_ref(this);
            *out = this;
            return HRESULT(0);
        }
        *out = std::ptr::null_mut();
    }
    E_NOINTERFACE
}

unsafe extern "system" fn add_ref(this: *mut c_void) -> u32 {
    // SAFETY: one of ours, alive while a reference to it is.
    unsafe { (*this.cast::<Reclaim>()).references.fetch_add(1, Ordering::Relaxed) + 1 }
}

unsafe extern "system" fn release(this: *mut c_void) -> u32 {
    // SAFETY: as above; the last reference frees the box it was made as.
    unsafe {
        let left = (*this.cast::<Reclaim>()).references.fetch_sub(1, Ordering::AcqRel) - 1;
        if left == 0 {
            let gone = Box::from_raw(this.cast::<Reclaim>());
            HELD.fetch_sub(gone.piece.len, Ordering::Relaxed);
            lock(&TEXTURES).give(gone.piece, now());
        }
        left
    }
}

/// Textures made in the last second ([`fresh`]), and when.
static RECENT: Mutex<VecDeque<(usize, u64)>> = Mutex::new(VecDeque::new());

/// Whether `texture` was made in a block in the last second.
pub(crate) fn fresh(texture: &ID3D12Resource) -> bool {
    let raw = texture.as_raw() as usize;
    let since = now().saturating_sub(1000);
    RECENT.lock().unwrap_or_else(|e| e.into_inner()).iter().rev().take_while(|r| r.1 >= since).any(|r| r.0 == raw)
}

fn made_now(texture: &ID3D12Resource) {
    let now = now();
    let mut recent = RECENT.lock().unwrap_or_else(|e| e.into_inner());
    while recent.front().is_some_and(|r| r.1 + 1000 < now) {
        recent.pop_front();
    }
    recent.push_back((texture.as_raw() as usize, now));
}

/// Makes `texture` give `piece` back when the last reference to it goes.
fn give_back_with(texture: &ID3D12Resource, piece: Piece) {
    const KEY: GUID = GUID::from_u128(0x6f0e2f0c_5a59_4b1e_9a3f_7f4d2c9be713);
    let made = Box::into_raw(Box::new(Reclaim { methods: &RECLAIM.0, references: AtomicU32::new(1), piece }));
    // SAFETY: a COM object of ours with one reference, which this takes and
    // drops at the end; the texture takes its own.
    unsafe {
        let object = IUnknown::from_raw(made.cast());
        if texture.SetPrivateDataInterface(&KEY, &object).is_err() {
            // Never given back: the piece stays taken, nothing is reused
            // under a texture that may be alive.
            std::mem::forget(object);
        }
    }
}

/// A texture of `desc` (no target, depth or unordered access) in a block;
/// `None`: not pooled, or the host refused.
///
/// # Safety
/// `desc` is a valid description.
pub(crate) unsafe fn texture(
    device: &ID3D12Device,
    desc: *const D3D12_RESOURCE_DESC,
    state: D3D12_RESOURCE_STATES,
) -> Option<ID3D12Resource> {
    if !on() {
        return None;
    }
    // Small textures lie at multiples of 4 KiB where the host allows it
    // (the top mip is 64 KiB at most): 64 KiB each otherwise.
    // SAFETY: the caller's description.
    let mut d = unsafe { *desc };
    d.Alignment = u64::from(D3D12_SMALL_RESOURCE_PLACEMENT_ALIGNMENT);
    // SAFETY: plain queries.
    let mut info = unsafe { device.GetResourceAllocationInfo(0, &[d]) };
    if info.SizeInBytes == u64::MAX || info.Alignment != d.Alignment {
        d.Alignment = 0;
        info = unsafe { device.GetResourceAllocationInfo(0, &[d]) };
    }
    if info.SizeInBytes == u64::MAX {
        return None;
    }
    // Whether a block may be made for it if it fits none ([`gate`]).
    WAITED.set(false);
    let mut ripe = Ripe::now();
    ripe.grow = match (may_grow(), CAN_WAIT.get()) {
        (true, _) => Grow::Yes,
        (false, false) => Grow::Must,
        (false, true) => Grow::No,
    };
    let taken = lock(&TEXTURES).take_ripe(info.SizeInBytes, info.Alignment.max(1), now(), ripe, || {
        let desc = D3D12_HEAP_DESC {
            SizeInBytes: BLOCK,
            Properties: D3D12_HEAP_PROPERTIES { Type: D3D12_HEAP_TYPE_DEFAULT, ..Default::default() },
            Alignment: 0,
            Flags: D3D12_HEAP_FLAG_ALLOW_ONLY_NON_RT_DS_TEXTURES | crate::host::not_resident(),
        };
        let mut heap: Option<ID3D12Heap> = None;
        // SAFETY: plain creation.
        if let Err(e) = unsafe { device.CreateHeap(&desc, &mut heap) } {
            tracing::warn!(error = %e, "no block of texture memory");
        }
        if let Some(h) = &heap
            && let Err(e) = crate::host::resident(device, h).and_then(|()| crate::host::mapped(device))
        {
            tracing::warn!(error = %e, "a block of texture memory is not resident");
            return None;
        }
        if heap.is_some() {
            BLOCKS[MADE].fetch_add(1, Ordering::Relaxed);
            if ripe.grow == Grow::Must {
                let n = BLOCKS[PAST].fetch_add(1, Ordering::Relaxed) + 1;
                if n.is_power_of_two() {
                    tracing::info!(
                        "blocks of textures made without room on the card, for textures that could not wait (read through a \
                         table now, or baked): {n}"
                    );
                }
            }
        }
        heap
    });
    #[cfg_attr(not(feature = "diagnostics"), allow(unused_variables))]
    let Some((piece, heap, again)) = taken else {
        // Not made for want of a place (and not for its size): it waits.
        if ripe.grow == Grow::No && ripe.pooled(info.SizeInBytes) {
            BLOCKS[NOT_MADE].fetch_add(1, Ordering::Relaxed);
            WAITED.set(true);
        }
        return None;
    };
    let mut made: Option<ID3D12Resource> = None;
    // SAFETY: the piece is no one else's; the heap lives while a piece of
    // it is taken.
    let result = unsafe { device.CreatePlacedResource(&heap, piece.start, &d, state, None, &mut made) };
    match (result, made) {
        (Ok(()), Some(made)) => {
            HELD.fetch_add(piece.len, Ordering::Relaxed);
            give_back_with(&made, piece);
            made_now(&made);
            #[cfg(feature = "diagnostics")]
            {
                crate::book::made(&made, || "a texture in a block".into());
                crate::book::block(&heap, &made, piece.start, BLOCK);
                crate::book::note(made.as_raw() as usize, || {
                    let block = lock(&TEXTURES).blocks.get(piece.block).and_then(Option::as_ref).map(|b| (b.made, b.since));
                    let (submission, since) = block.unwrap_or_default();
                    format!(
                        "; piece {:#x}+{:#x} of block {} {:#x} made {:.1} s before it at submission {submission}, used before: {again}, made at submissions {:?}",
                        piece.start,
                        piece.len,
                        piece.block,
                        heap.as_raw() as usize,
                        now().saturating_sub(since) as f64 / 1e3,
                        crate::host::submissions()
                    )
                });
            }
            Some(made)
        }
        _ => {
            lock(&TEXTURES).give(piece, now());
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_are_cut_aligned_and_join_again() {
        let mut r = Ranges::new(1024);
        assert_eq!(r.take(100, 64), Some(0));
        assert_eq!(r.take(100, 64), Some(128));
        assert_eq!(r.take(28, 4), Some(100));
        assert_eq!(r.take(2000, 4), None);
        r.give(128, 100);
        r.give(0, 100);
        assert!(!r.whole(1024));
        r.give(100, 28);
        assert!(r.whole(1024));
    }

    #[test]
    fn empty_blocks_stay_while_the_card_has_room() {
        assert_eq!(spares(Some([100, 84, 0, 0]), true), SPARE_KEPT);
        assert_eq!(spares(Some([100, 85, 0, 0]), true), SPARE);
        assert_eq!(spares(Some([100, 10, 0, 0]), false), SPARE);
        assert_eq!(spares(None, true), SPARE);
    }

    #[test]
    fn pieces_never_overlap_and_rest_before_they_are_used_again() {
        let mut pool: Pool<u32> = Pool::new(SPARE);
        let made = std::cell::Cell::new(0);
        let make = || {
            made.set(made.get() + 1);
            Some(made.get())
        };
        let mut taken: Vec<Piece> = Vec::new();
        for i in 0..500u64 {
            let (p, _, again) = pool.take(65536 * (1 + i % 7), 65536, 0, make).unwrap();
            assert!(!again);
            assert!(taken.iter().all(|q| q.block != p.block || p.start + p.len <= q.start || q.start + q.len <= p.start));
            taken.push(p);
        }
        let blocks = pool.blocks();
        assert!(blocks >= 2);
        // Given back at 10: not used again before 10 + REST.
        let first = taken[0];
        pool.give(first, 10);
        let (p, _, _) = pool.take(first.len, 65536, 10 + REST - 1, make).unwrap();
        assert_ne!((p.block, p.start), (first.block, first.start));
        let (q, _, again) = pool.take(first.len, 65536, 10 + REST, make).unwrap();
        assert_eq!((q.block, q.start, again), (first.block, first.start, true));
        // Everything back: the empty blocks past the spare ones go.
        for p in taken.drain(1..).chain([p, q]) {
            pool.give(p, 20);
        }
        pool.rested(20 + REST);
        assert_eq!(pool.blocks(), blocks.min(SPARE));
        // Too big for a block, and a block that cannot be made.
        assert!(pool.take(BLOCK, 65536, 30, make).is_none());
        let mut none: Pool<u32> = Pool::new(SPARE);
        assert!(none.take(65536, 65536, 0, || None).is_none());
    }

    #[test]
    fn pieces_come_from_blocks_older_than_the_lists_that_run() {
        let mut pool: Pool<u64> = Pool::new(SPARE_KEPT);
        let made = std::cell::Cell::new(0);
        let make = || {
            made.set(made.get() + 1);
            Some(made.get())
        };
        let at = |started, finished| Ripe { started, finished, ahead: 2, ..Ripe::ALL };
        let take = |pool: &mut Pool<u64>, ripe| pool.take_ripe(BLOCK / 2, 65536, 0, ripe, make).unwrap().1;
        // Nothing runs: at once, and two are made ahead, one a time.
        assert_eq!(take(&mut pool, at(0, 0)), 1);
        assert_eq!(made.get(), 2);
        assert_eq!(take(&mut pool, at(0, 0)), 1);
        assert_eq!(made.get(), 3);
        // 4 is made while submission 5 runs.
        assert_eq!(take(&mut pool, at(5, 4)), 2);
        assert_eq!(made.get(), 4);
        assert_eq!(take(&mut pool, at(5, 4)), 2);
        assert_eq!(take(&mut pool, at(6, 4)), 3);
        assert_eq!(take(&mut pool, at(6, 4)), 3);
        // None is ripe: the oldest; it is ripe once 5 has finished.
        assert_eq!(take(&mut pool, at(7, 4)), 4);
        assert_eq!(take(&mut pool, at(7, 5)), 4);
        // A whole block's worth too.
        assert_eq!(pool.take_ripe(BLOCK, 65536, 0, at(8, 7), make).unwrap().1, 5);
        // Off: the first with room, whatever its age, none ahead.
        let n = made.get();
        assert_eq!(pool.take(BLOCK / 2, 65536, 0, make).unwrap().1, 6);
        assert_eq!(made.get(), n);
        assert!(pool.take(BLOCK, 65536, 0, make).is_none());
    }

    /// Without room on the card no block is made for a texture that can
    /// wait: it gets a free piece of a block there is, or none; one that
    /// cannot wait gets its block, and none is made ahead.
    #[test]
    fn no_block_is_made_without_room_for_a_texture_that_can_wait() {
        let mut pool: Pool<u64> = Pool::new(SPARE_KEPT);
        let made = std::cell::Cell::new(0);
        let make = || {
            made.set(made.get() + 1);
            Some(made.get())
        };
        let ripe = |grow| Ripe { ahead: 2, grow, ..Ripe::ALL };
        let quarter = BLOCK / 4;
        // With room: a block for it and one ahead, as before.
        let (first, _, _) = pool.take_ripe(quarter, 65536, 0, ripe(Grow::Yes), make).unwrap();
        assert_eq!((pool.blocks(), made.get()), (2, 2));
        // Without: the free pieces of the blocks there are, and no block
        // ahead (the second block is the only empty one).
        for _ in 0..7 {
            assert!(pool.take_ripe(quarter, 65536, 0, ripe(Grow::No), make).is_some());
        }
        assert_eq!((pool.blocks(), made.get()), (2, 2));
        // Nothing fits: it waits, and so does every piece as long, without
        // a look at the blocks ([`Pool::no_fit`]); a smaller one is looked
        // for.
        assert!(pool.take_ripe(quarter, 65536, 0, ripe(Grow::No), make).is_none());
        assert_eq!(pool.no_fit, Some((quarter, 65536)));
        assert!(pool.take_ripe(2 * quarter, 65536, 0, ripe(Grow::No), make).is_none());
        assert!(pool.take_ripe(65536, 65536, 0, ripe(Grow::No), make).is_none());
        assert_eq!(pool.no_fit, Some((65536, 65536)));
        assert_eq!((pool.blocks(), made.get()), (2, 2));
        // One that cannot wait gets a block, and none ahead.
        assert!(pool.take_ripe(quarter, 65536, 0, ripe(Grow::Must), make).is_some());
        assert_eq!((pool.blocks(), made.get()), (3, 3));
        assert_eq!(pool.no_fit, None);
        // A piece that came free is found again.
        for _ in 0..3 {
            assert!(pool.take_ripe(quarter, 65536, 0, ripe(Grow::No), make).is_some());
        }
        assert!(pool.take_ripe(quarter, 65536, 0, ripe(Grow::No), make).is_none());
        pool.give(first, 0);
        assert!(pool.take_ripe(quarter, 65536, REST - 1, ripe(Grow::No), make).is_none());
        let (again, _, _) = pool.take_ripe(quarter, 65536, REST, ripe(Grow::No), make).unwrap();
        assert_eq!((again.block, again.start), (first.block, first.start));
        assert_eq!((pool.blocks(), made.get()), (3, 3));
        // Too big for a block is not a wait.
        assert!(!ripe(Grow::No).pooled(BLOCK + 1));
        assert!(ripe(Grow::No).pooled(BLOCK));
        assert!(!Ripe::ALL.pooled(BLOCK));
    }

    /// With the gate off ([`Grow::Yes`]) a piece that is known to fit
    /// nowhere still gets the look it had and the block it had: the same
    /// blocks are made, in the same order, as without the bookkeeping.
    #[test]
    fn with_room_every_piece_is_looked_for_and_gets_a_block_as_before() {
        let mut pool: Pool<u64> = Pool::new(SPARE_KEPT);
        let made = std::cell::Cell::new(0);
        let make = || {
            made.set(made.get() + 1);
            Some(made.get())
        };
        let half = BLOCK / 2;
        let blocks: Vec<u64> = (0..6).map(|_| pool.take_ripe(half, 65536, 0, Ripe::ALL, make).unwrap().1).collect();
        assert_eq!(blocks, [1, 1, 2, 2, 3, 3]);
        assert_eq!(pool.no_fit, None);
        // A half block fits none now; the next one makes a block anyway.
        let p = pool.take_ripe(half, 65536, 0, Ripe::ALL, make).unwrap();
        assert_eq!((p.1, made.get()), (4, 4));
    }

    /// The pool's side of the gate's way out of a wait: a texture that
    /// waits is not made, and takes the place of one the sweep let go once
    /// that has rested, with no block made for it. (The sweep's side, that
    /// it lets textures go while the blocks have less than its reserve free:
    /// `crate::resource` `the_sweep_keeps_room_in_the_blocks`.)
    #[test]
    fn a_waiting_texture_takes_a_piece_that_comes_free() {
        let mut pool: Pool<u64> = Pool::new(0);
        let ripe = Ripe { grow: Grow::No, ..Ripe::ALL };
        // Two full blocks, made while the card had room.
        let pieces: Vec<Piece> = (0..8).map(|_| pool.take(BLOCK / 4, 65536, 0, || Some(1)).unwrap().0).collect();
        assert_eq!(pool.blocks(), 2);
        // A texture waits: nothing is made for it.
        assert!(pool.take_ripe(BLOCK / 4, 65536, 0, ripe, || panic!("no block without room")).is_none());
        // The sweep lets one go; once it rested, the wait ends in its place.
        pool.give(pieces[5], 0);
        assert!(pool.take_ripe(BLOCK / 4, 65536, REST - 1, ripe, || panic!("no block without room")).is_none());
        let (p, _, again) = pool.take_ripe(BLOCK / 4, 65536, REST, ripe, || panic!("no block without room")).unwrap();
        assert_eq!((p.block, p.start, again), (pieces[5].block, pieces[5].start, true));
    }

    /// A pooled texture's piece comes back when its last reference goes, not
    /// before; its staging piece begins as zeros.
    #[test]
    fn a_texture_gives_its_piece_back_when_it_goes() {
        let Some(host) = crate::host::host() else { return };
        let desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
            Width: 256,
            Height: 256,
            DepthOrArraySize: 1,
            MipLevels: 1,
            Format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_R8G8B8A8_UNORM,
            SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            ..Default::default()
        };
        // SAFETY: a valid description.
        let Some(t) = (unsafe { texture(&host.device, &desc, D3D12_RESOURCE_STATE_COMMON) }) else { return };
        assert_eq!(crate::host::references(&t), 1, "what the texture holds is no reference to it");
        let resting = || lock(&TEXTURES).resting.len();
        let before = resting();
        let other = t.clone();
        drop(t);
        assert_eq!(resting(), before);
        drop(other);
        assert_eq!(resting(), before + 1);
        // SAFETY: the piece's mapping, 4096 bytes of it ours.
        unsafe {
            let (piece, _, at, data) = staging(host, 4096).unwrap();
            assert!(at.is_multiple_of(512));
            std::ptr::write_bytes(data, 0xAB, 4096);
            drop(piece);
            lock(&STAGING).rested(u64::MAX);
            let (_piece, _, _, data) = staging(host, 4096).unwrap();
            assert!((0..4096).all(|i| *data.add(i) == 0));
            // SAFETY: plain query.
            assert!(host.device.GetDeviceRemovedReason().is_ok());
        }
    }
}
