//! Diagnostics: a write of ours into title memory that turns a pointer the
//! title keeps there into garbage (`VICTORY_SMASH_WATCH`; off by default).
//!
//! Why: the title fell five times on 27 Nov at `+0x4e9b54a` (runs
//! intro-s-bs5, barco-s-final, barco-d-agua2q, prisao-s-t1, barco-ab-a1), in
//! the code that recycles a block of its ring allocator: the blocks are 128
//! KiB of the 174 MiB heap whose host copy is in the card's memory, and each
//! keeps a list of 24-byte records (next, object, count), one at the block's
//! end and some inside it. In each minidump the object pointer of a record
//! (`0x9_0418_xxxx`) had its high dword replaced (by 1 four times, by 0x49
//! once; once its low dword was 0 too) while the low dword and the record's
//! other fields were intact; in a sixth (barco-ab-f2, `+0x4e9b578`) its low
//! dword was shifted right by four bits. That is a store of a few bytes over
//! memory the title had written, and the title faults on the pointer the next
//! time the block comes round. Beside the records lie the title's descriptor
//! tables (our records of 12 bytes, tag `vVIE`), and in the intro's and
//! barco-ab-f2's minidumps every fifth dword of them was changed the same way
//! (+0x40, or shifted): something works on 20-byte elements laid over memory
//! the title uses for other things.
//!
//! With `1`, the paths of ours that write title memory ([`Path`]) look at
//! what they are about to change, before they write: an 8-byte aligned qword
//! that holds what looks like a pointer into title memory ([`pointer_like`])
//! and that the write turns into no pointer of the same 4 GiB is an event
//! ([`smashes`]). Events
//! are logged (the first [`log_limit`], and every one at the end of a 64 KiB
//! block, where the ring keeps a record), counted by path on the frame-rate
//! line, and the last [`KEPT`] are printed with the crash report, those near
//! a register first. For a landing the line also says what the card and the
//! CPU's side held, the landing's flights, and the programs that declared
//! writes over the address ([`program`], [`declared`]).
//!
//! With `2`, also a thread watches the qword 16 bytes before the end of each
//! 64 KiB of the biggest copy landed through ([`seen_copy`]), where a ring
//! block's last record keeps its object pointer, every [`WATCH_MS`] ms, and
//! logs one changed the way the crashes saw it ([`watch_flags`]), with which
//! path of ours wrote that 64 KiB last and when ([`touched`]). A write that
//! none of our paths made (the title's own code, or something not listed
//! here) is seen there too, as one with no event of ours at its address.
//! But so is the title's own reuse of a block's last bytes (a block handed
//! out again without a record holds data there): some 8,000 lines a run in
//! barco-q-w1..3, none of them a crash. `crate::ringcheck` (`VICTORY_RING_CHECK`)
//! looks only at the records the title still holds.
//!
//! What it costs (`1`): a landed 4 KiB page in which the card's bytes differ
//! from the CPU side's is read from title memory once more (some 25 MB a
//! frame in the world, on the threads that land), and every copy into title
//! memory reads its destination first.
//!
//! `VICTORY_SMASH_GUARD=1` ([`guard`]) also gives a pointer a landing
//! smashed the crashes' way back to the title, right after the merge.

use std::cell::Cell;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

/// `VICTORY_SMASH_WATCH`: 0 (unset) off, 1 the writes, 2 also the watch of
/// block ends. Off without `crate::safecopy` (`VICTORY_SAFE_COPY=0`): its
/// reads of title memory go through that copy.
pub(crate) fn level() -> u8 {
    static LEVEL: OnceLock<u8> = OnceLock::new();
    *LEVEL.get_or_init(|| match std::env::var("VICTORY_SMASH_WATCH").ok().as_deref().map(str::trim) {
        Some("2") if crate::safecopy::on() => 2,
        Some(v) if !v.is_empty() && v != "0" && crate::safecopy::on() => 1,
        _ => 0,
    })
}

/// See the file.
pub(crate) fn on() -> bool {
    level() > 0 || guard()
}

/// Whether the paths other than the landing look before they write (and
/// draws note their programs): with `VICTORY_SMASH_WATCH` only. The guard
/// alone needs the landing's look ([`before_merge`]) and nothing else.
pub(crate) fn checks() -> bool {
    level() > 0
}

/// `VICTORY_SMASH_GUARD=1` (off by default; with it the checks of `1` are
/// on): a landing does not put the card's bytes over a title pointer in the
/// ways the crashes saw ([`watch_flags`]). Title memory and the CPU's side
/// keep the title's qword ([`keep_title`]), the card keeps its own (it is
/// what the card's later work reads), and the line says so.
///
/// Why it is no fix: whatever writes there on the card still does, beside
/// the pointer (the title's descriptors round it stay as it left them). It
/// keeps the ring's records whole while that is found. What it may cost:
/// a qword the card writes that looks the same (a count beside an aligned
/// value, whose count changes in the high dword) stays as it was for the
/// title's CPU; the card keeps it.
pub(crate) fn guard() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| crate::switch::on("VICTORY_SMASH_GUARD") && crate::safecopy::on())
}

/// Qwords [`keep_title`] kept, since the start and since the last line.
static KEPT_ALL: AtomicU64 = AtomicU64::new(0);
static KEPT_SINCE: AtomicU64 = AtomicU64::new(0);

/// After a landing's merge ([`before_merge`] asked for it): title memory at
/// `into` (an address that shows the qword, ours or the title's) gets `was`
/// back if it holds what the merge left (`now`; else the title wrote it
/// meanwhile and keeps that), and so does the CPU's side at `held`.
///
/// # Safety
/// `held` is 8 bytes of the copy's CPU side that only the caller writes.
pub(crate) unsafe fn keep_title(into: usize, held: *mut u8, was: u64, now: u64) {
    let mut there = [0u8; 8];
    // SAFETY: into our own array; memory that is not there fails the copy.
    if !unsafe { crate::safecopy::copy(there.as_mut_ptr(), into as *const u8, 8) } || u64::from_le_bytes(there) != now {
        return;
    }
    let bytes = was.to_le_bytes();
    // SAFETY: title memory the merge just wrote; a page that went away
    // fails the copy.
    if !unsafe { crate::safecopy::copy(into as *mut u8, bytes.as_ptr(), 8) } {
        return;
    }
    // SAFETY: the caller's.
    unsafe { held.cast::<u64>().write_unaligned(was) };
    KEPT_SINCE.fetch_add(1, Ordering::Relaxed);
    if KEPT_ALL.fetch_add(1, Ordering::Relaxed) < 64 {
        tracing::warn!(
            "smash guard (VICTORY_SMASH_GUARD=1): a landing would have left {now:#018x} over the title's {was:#018x} at {into:#x}; the \
             title's is kept (frame {})",
            crate::frame::current()
        );
    }
}

/// How many events are logged as they come (`VICTORY_SMASH_WATCH_LOG`, 200);
/// later ones are counted and kept, and those at a 64 KiB block's end are
/// logged whatever the count.
fn log_limit() -> u64 {
    static LIMIT: OnceLock<u64> = OnceLock::new();
    *LIMIT.get_or_init(|| std::env::var("VICTORY_SMASH_WATCH_LOG").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(200))
}

/// Events kept for the crash report.
const KEPT: usize = 1024;

/// The watch of block ends looks this often.
const WATCH_MS: u64 = 2;

/// Where the ring allocator keeps a block's last record's object pointer:
/// 16 bytes before the end of the block (blocks of 128 KiB, on 64 KiB lines).
const BLOCK: usize = 64 << 10;
const NODE_OBJECT: usize = BLOCK - 0x10;

/// Which path of ours writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Path {
    /// `crate::land`: what shaders wrote, from a copy in the card's memory.
    LandCard,
    /// `crate::land`: the same, from a copy in the PC's memory.
    LandBack,
    /// `crate::shadow`'s `put_changed`: what shaders wrote, without
    /// `VICTORY_LAND` (or a copy's own merge).
    PutBack,
    /// A value a list writes when it has run (bottom-of-pipe writes, query
    /// results: `crate::queue::Written`).
    Value,
    /// A texture copied into a buffer, the rows put into title memory once
    /// the list has run (`crate::host`'s read-backs).
    ReadBack,
    /// `crate::host::copy_mapped` into title memory: the title's buffer
    /// copies done on the CPU, textures shaders wrote (`crate::texback`),
    /// copies at their place.
    Copy,
    /// What the runtime writes for the title (DirectStorage's reads, page
    /// copies of `xmem`: `crate::watch::title_memory_written`). Only noted
    /// for the watch of block ends: the title asked for those bytes.
    Runtime,
    /// Seen by the watch of block ends, written by no path above.
    Watch,
    /// A descriptor the title makes or copies into a slot of its memory
    /// (`crate::descriptor`: our record of 12 bytes, or the slots' bytes).
    /// The ring's blocks hold the title's descriptor tables beside its
    /// records: our records (tag `vVIE`) lie next to the smashed ones in the
    /// minidumps.
    Descriptor,
}

const PATHS: usize = 9;

impl Path {
    fn index(self) -> usize {
        self as usize
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Path::LandCard => "landing (a copy in the card's memory)",
            Path::LandBack => "landing (a copy in the PC's memory)",
            Path::PutBack => "what shaders wrote, put back",
            Path::Value => "a value written when a list has run",
            Path::ReadBack => "a texture read back",
            Path::Copy => "a copy into title memory",
            Path::Runtime => "the runtime (DirectStorage, xmem)",
            Path::Watch => "the watch of block ends",
            Path::Descriptor => "a descriptor made or copied",
        }
    }

    fn short(self) -> &'static str {
        match self {
            Path::LandCard => "land-card",
            Path::LandBack => "land-back",
            Path::PutBack => "put-back",
            Path::Value => "value",
            Path::ReadBack => "read-back",
            Path::Copy => "copy",
            Path::Runtime => "runtime",
            Path::Watch => "watch",
            Path::Descriptor => "descriptor",
        }
    }

    fn of(index: u8) -> Option<Path> {
        [
            Path::LandCard,
            Path::LandBack,
            Path::PutBack,
            Path::Value,
            Path::ReadBack,
            Path::Copy,
            Path::Runtime,
            Path::Watch,
            Path::Descriptor,
        ]
        .get(usize::from(index))
        .copied()
    }
}

/// Whether `v` looks like a pointer into title memory: 8-byte aligned, a
/// low dword that is not 0, and a high dword of 1 to 255 (title memory lies
/// between 4 GiB and 1 TiB: the console's GPU addresses have 40 bits).
pub(crate) fn pointer_like(v: u64) -> bool {
    v & 7 == 0 && v as u32 != 0 && (1..=0xff).contains(&(v >> 32))
}

/// Whether a write that leaves `now` where `was` stood smashes a pointer:
/// `was` looks like one and it changes into what is no pointer of the same 4
/// GiB: its high dword changes, or it is left unaligned, or its low dword
/// goes to 0. (A pointer moved to another 8-aligned place in the same 4 GiB,
/// as a structure's links are, is no event.) The crashes' pointers became
/// `1_xxxx`, `49_xxxx`, `1_0000_0000`, `9_0000_0000`, and once its low dword
/// shifted right by four bits (`9_0418_e9f0` -> `9_0041_8e9f`, run
/// barco-ab-f2).
pub(crate) fn smashes(was: u64, now: u64) -> bool {
    pointer_like(was) && now != was && (was >> 32 != now >> 32 || now & 7 != 0 || now as u32 == 0)
}

/// Per byte of `x`: 0xff where it is not 0.
fn byte_mask(x: u64) -> u64 {
    let mut y = x | x >> 4;
    y |= y >> 2;
    y |= y >> 1;
    (y & 0x0101_0101_0101_0101) * 0xff
}

/// What [`crate::safecopy::merge`] leaves in a qword of title memory that
/// held `title`: the card's bytes (`card`) where they differ from what it
/// held (`held`), the title's elsewhere.
pub(crate) fn merged(title: u64, card: u64, held: u64) -> u64 {
    let mask = byte_mask(card ^ held);
    (card & mask) | (title & !mask)
}

/// One event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Event {
    pub(crate) path: Path,
    /// The qword's title address.
    pub(crate) at: usize,
    /// It, before and after the write.
    pub(crate) was: u64,
    pub(crate) now: u64,
    /// For a landing: what the card held there, and the CPU's side.
    pub(crate) card: Option<(u64, u64)>,
    /// The landing it came in ([`landing`]; 0: none).
    pub(crate) landing: u64,
    pub(crate) frame: u64,
    /// Milliseconds since the first event or touch.
    pub(crate) ms: u64,
}

/// Events since the start, and since the last frame-rate line, by path.
static COUNTS: [AtomicU64; PATHS] = [const { AtomicU64::new(0) }; PATHS];
static SINCE: [AtomicU64; PATHS] = [const { AtomicU64::new(0) }; PATHS];
/// Events logged as they came.
static LOGGED: AtomicU64 = AtomicU64::new(0);
/// The last [`KEPT`] events.
static RING: Mutex<VecDeque<Event>> = Mutex::new(VecDeque::new());
/// The last event at each address, [`BY_ADDRESS_MOST`] addresses at most
/// (then it starts again): a record of the ring is smashed while its block
/// is in use, and read seconds later, when the block comes round; the
/// GPU's own counters next to pointers (indirect arguments) may be events
/// every frame and push it out of [`RING`].
static BY_ADDRESS: Mutex<Option<std::collections::HashMap<usize, Event>>> = Mutex::new(None);
const BY_ADDRESS_MOST: usize = 1 << 16;

/// The last event at `at`, if one is kept.
pub(crate) fn event_at(at: usize) -> Option<Event> {
    BY_ADDRESS.try_lock().ok()?.as_ref()?.get(&at).copied()
}

pub(crate) fn ms() -> u64 {
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed().as_millis() as u64
}

/// What is being landed: its number, the flights' numbers, and the ranges
/// their lists' programs declared written (from, to, program).
struct Landing {
    number: u64,
    flights: Vec<u64>,
    declared: Vec<(usize, usize, u64)>,
}

static LANDING: Mutex<Landing> = Mutex::new(Landing { number: 0, flights: Vec::new(), declared: Vec::new() });

/// A landing begins (`crate::host`'s `relay_land`; one at a time): the
/// numbers of its flights and the ranges their programs declared written.
pub(crate) fn landing(flights: Vec<u64>, mut declared: Vec<(usize, usize, u64)>) {
    declared.sort_unstable();
    declared.dedup();
    let mut landing = LANDING.lock().unwrap_or_else(|e| e.into_inner());
    landing.number += 1;
    landing.flights = flights;
    landing.declared = declared;
}

fn landing_number() -> u64 {
    LANDING.try_lock().map_or(0, |l| l.number)
}

thread_local! {
    /// While a draw's or dispatch's stage is bound (`crate::draw`): its
    /// program's hash (0: none).
    static PROGRAM: Cell<u64> = const { Cell::new(0) };
}

/// The stage whose program is `hash` (hex, as the pipeline log lines print
/// it) is being bound on this thread; `None`: none is.
pub(crate) fn program(hash: Option<&str>) {
    let value = hash.map_or(0, |h| u64::from_str_radix(h, 16).unwrap_or_else(|_| fnv(h.as_bytes())));
    PROGRAM.set(value);
}

/// The program of the stage being bound on this thread (0: none).
pub(crate) fn current_program() -> u64 {
    PROGRAM.get()
}

/// While a draw or dispatch is bound: when it is dropped, no program is.
pub(crate) struct Binding;

impl Drop for Binding {
    fn drop(&mut self) {
        PROGRAM.set(0);
    }
}

fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3))
}

/// The programs whose declared writes cover `at`, and those ranges (at most
/// a few of each).
fn declared_over(at: usize) -> (Vec<u64>, Vec<(usize, usize)>) {
    let Ok(landing) = LANDING.try_lock() else {
        return (Vec::new(), Vec::new());
    };
    let mut programs = Vec::new();
    let mut ranges = Vec::new();
    for &(from, to, program) in landing.declared.iter().filter(|d| d.0 <= at && at < d.1) {
        if !programs.contains(&program) && programs.len() < 12 {
            programs.push(program);
        }
        if !ranges.contains(&(from, to)) && ranges.len() < 4 {
            ranges.push((from, to));
        }
    }
    (programs, ranges)
}

/// The copy watched for block ends: start and bytes (the biggest landed
/// through so far).
static WATCHED: Mutex<(usize, usize)> = Mutex::new((0, 0));
/// The most 64 KiB pieces stamped (2 GiB).
const WATCH_MOST: usize = 32768;

/// `VICTORY_WRITE_STAMPS=1` (off by default): who of ours wrote each 64 KiB
/// of the biggest copy landed through (the title's 174 MiB heap, where the
/// ring lies) is kept, the last [`WRITERS`] of each: the path, when, and
/// the landing under way then ([`landing_begins`]). Nothing is checked at
/// the writes and no thread runs: a store or two per write. The crash
/// report's note prints them for the 64 KiB of each register, and for the
/// block a register's ring descriptor names ([`writers_note`]). The watch
/// of block ends and `crate::ringcheck` use the same stamps.
pub(crate) fn stamps() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| crate::switch::on("VICTORY_WRITE_STAMPS") && crate::safecopy::on())
}

/// Writers kept per 64 KiB.
const WRITERS: usize = 4;

/// Landings begun since the start (`crate::host`'s `relay_land`).
static LANDINGS: AtomicU64 = AtomicU64::new(0);

/// A landing begins: writes from here on carry its number.
pub(crate) fn landing_begins() {
    LANDINGS.fetch_add(1, Ordering::Relaxed);
}

/// The last writers of each 64 KiB of `[base, base + pieces * BLOCK)`: per
/// piece [`WRITERS`] stamps (milliseconds << 32 | landing << 8 | path + 1,
/// the landing's low 24 bits) and the count of stamps put, whose low bits
/// say which slot is next.
pub(crate) struct Stamps {
    base: usize,
    pieces: Vec<[AtomicU64; WRITERS]>,
    next: Vec<AtomicU32>,
}

/// One writer: its path, when (milliseconds of [`ms`], low 32 bits) and the
/// landing under way then (low 24 bits).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Writer {
    pub(crate) path: Path,
    pub(crate) ms: u64,
    pub(crate) landing: u64,
}

impl Stamps {
    pub(crate) fn new(start: usize, len: usize) -> Stamps {
        let n = (len / BLOCK + 2).min(WATCH_MOST);
        Stamps {
            base: start / BLOCK * BLOCK,
            pieces: (0..n).map(|_| [const { AtomicU64::new(0) }; WRITERS]).collect(),
            next: (0..n).map(|_| AtomicU32::new(0)).collect(),
        }
    }

    fn piece(&self, at: usize) -> Option<usize> {
        let piece = at.checked_sub(self.base)? / BLOCK;
        (piece < self.pieces.len()).then_some(piece)
    }

    /// `path` writes `[at, at + len)` at `ms`, in landing `landing`.
    pub(crate) fn stamp(&self, path: Path, at: usize, len: usize, ms: u64, landing: u64) {
        let (Some(first), true) = (self.piece(at), len > 0) else {
            return;
        };
        let last = self.piece(at + len - 1).unwrap_or(self.pieces.len() - 1);
        let stamp = (ms & 0xffff_ffff) << 32 | (landing & 0xff_ffff) << 8 | (path.index() as u64 + 1);
        for piece in first..=last.min(first + 64) {
            let slot = self.next[piece].fetch_add(1, Ordering::Relaxed) as usize % WRITERS;
            self.pieces[piece][slot].store(stamp, Ordering::Relaxed);
        }
    }

    /// The writers kept for the 64 KiB at `at`, the last first.
    pub(crate) fn writers(&self, at: usize) -> Vec<Writer> {
        let Some(piece) = self.piece(at) else {
            return Vec::new();
        };
        let mut all: Vec<Writer> = self.pieces[piece]
            .iter()
            .map(|s| s.load(Ordering::Relaxed))
            .filter_map(|s| {
                let path = Path::of(((s & 0xff) as u8).checked_sub(1)?)?;
                Some(Writer { path, ms: s >> 32, landing: s >> 8 & 0xff_ffff })
            })
            .collect();
        all.sort_by_key(|w| std::cmp::Reverse(w.ms));
        all
    }

    fn holds(&self, at: usize) -> bool {
        self.piece(at).is_some()
    }
}

static STAMPS: OnceLock<Stamps> = OnceLock::new();

/// A copy is landed through (`crate::shadow`): the biggest of them from 64
/// MiB on (the title's 174 MiB heap, where the ring lies) is stamped with
/// [`stamps`], and watched with `2` (the first one keeps both).
pub(crate) fn seen_copy(start: usize, len: usize) {
    if level() < 2 && !stamps() || len < 64 << 20 {
        return;
    }
    {
        let mut watched = WATCHED.lock().unwrap_or_else(|e| e.into_inner());
        if watched.1 >= len {
            return;
        }
        if watched.1 != 0 {
            // (One watch: a bigger copy found later is only said.)
            tracing::info!(
                start = format_args!("{start:#x}"),
                len,
                watched = format_args!("{:#x}+{:#x}", watched.0, watched.1),
                "smash watch: a bigger copy than the one watched is landed through; the watch stays where it is"
            );
            return;
        }
        *watched = (start, len);
    }
    touch_range(start, len);
    if stamps() {
        tracing::info!(
            start = format_args!("{start:#x}"),
            len,
            "write stamps (VICTORY_WRITE_STAMPS=1): the last 4 writers of ours of each 64 KiB of this copy are kept"
        );
    }
    if level() < 2 {
        return;
    }
    tracing::info!(
        start = format_args!("{start:#x}"),
        len,
        every_ms = WATCH_MS,
        "smash watch (VICTORY_SMASH_WATCH=2): the qword 16 bytes before the end of each 64 KiB of this copy is watched"
    );
    let _ = std::thread::Builder::new().name("victory-smash-watch".into()).spawn(move || watch(start, len));
}

/// From now on [`touched`] notes who of ours writes each 64 KiB of `[start,
/// start + len)`, for the watch of block ends, `crate::ringcheck` and the
/// crash report (the first range asked for is kept).
pub(crate) fn touch_range(start: usize, len: usize) {
    let _ = STAMPS.set(Stamps::new(start, len));
}

/// A path of ours writes `[at, at + len)` of title memory: noted, inside the
/// stamped copy, with when and the landing under way.
pub(crate) fn touched(path: Path, at: usize, len: usize) {
    let Some(stamps) = STAMPS.get() else {
        return;
    };
    if stamps.holds(at) {
        stamps.stamp(path, at, len, ms(), LANDINGS.load(Ordering::Relaxed));
    }
}

/// Who of ours wrote the 64 KiB at `at` last, and how long ago.
pub(crate) fn last_touch(at: usize) -> Option<(Path, u64)> {
    let w = *STAMPS.get()?.writers(at).first()?;
    Some((w.path, (ms() & 0xffff_ffff).saturating_sub(w.ms)))
}

/// For the crash report: the last writers of the 64 KiB each register points
/// into, and of the two 64 KiB of a 128 KiB block that a register's ring
/// descriptor names (its qword at +0x10, `crate::ringcheck`); empty without
/// stamps.
fn writers_note(registers: &[u64; 16]) -> String {
    let Some(stamps) = STAMPS.get() else {
        return String::new();
    };
    writers_text(stamps, registers, ms() & 0xffff_ffff, LANDINGS.load(Ordering::Relaxed))
}

/// [`writers_note`] for `stamps`, at `now`, `landings` begun.
fn writers_text(stamps: &Stamps, registers: &[u64; 16], now: u64, landings: u64) -> String {
    const NAMES: [&str; 16] = ["rax", "rcx", "rdx", "rbx", "rsp", "rbp", "rsi", "rdi", "r8", "r9", "r10", "r11", "r12", "r13", "r14", "r15"];
    let mut text = format!(
        "write stamps (VICTORY_WRITE_STAMPS): {landings} landings begun; the last writers of ours of each 64 KiB, the newest first:\n"
    );
    let line = |what: String, at: usize| -> String {
        let writers: Vec<String> = stamps
            .writers(at)
            .iter()
            .map(|w| format!("{} {} ms before the end (landing {})", w.path.short(), now.saturating_sub(w.ms), w.landing))
            .collect();
        let shown = if writers.is_empty() { "none".to_string() } else { writers.join(", ") };
        format!("  {what} {at:#x}: {shown}\n")
    };
    for (name, &r) in NAMES.iter().zip(registers) {
        let r = r as usize;
        if stamps.holds(r) {
            text.push_str(&line(format!("{name} in"), r));
            continue;
        }
        // A ring descriptor: the block it names, both halves.
        let mut block = [0u8; 8];
        // SAFETY: into our own array; memory that is not there fails the
        // copy.
        if r != 0 && r.is_multiple_of(8) && unsafe { crate::safecopy::copy(block.as_mut_ptr(), r.wrapping_add(0x10) as *const u8, 8) } {
            let block = u64::from_le_bytes(block) as usize;
            if stamps.holds(block) && (block - stamps.base).is_multiple_of(2 * BLOCK) {
                text.push_str(&line(format!("{name}'s ring block, first half"), block));
                text.push_str(&line(format!("{name}'s ring block, second half (its last record)"), block + BLOCK));
            }
        }
    }
    text
}

/// What the watch of block ends makes of a qword that was `was` and is
/// `now`, narrower than [`smashes`] (the slot holds the title's data when a
/// block has no record at its end): a pointer whose low dword stayed and
/// whose high dword changed (four crashes, the intro's +0x40), one whose low
/// dword went to 0 with a small high dword (barco-s-final: `1_0000_0000`,
/// then `9_0000_0000` in other blocks), or one whose high dword stayed and
/// whose low dword was shifted right by four bits (barco-ab-f2).
pub(crate) fn watch_flags(was: u64, now: u64) -> bool {
    let (lo, hi) = (was as u32, was >> 32);
    let kept_low = now as u32 == lo && now >> 32 != hi;
    let small_high_alone = now as u32 == 0 && (1..=0xff).contains(&(now >> 32));
    let shifted_low = now >> 32 == hi && now as u32 == lo >> 4;
    pointer_like(was) && now != was && (kept_low || small_high_alone || shifted_low)
}

/// The watch of block ends: see the file.
fn watch(start: usize, len: usize) {
    let ends: Vec<usize> =
        (start / BLOCK..(start + len) / BLOCK).map(|b| b * BLOCK + NODE_OBJECT).filter(|&a| a >= start && a + 8 <= start + len).take(WATCH_MOST).collect();
    let mut last: Vec<Option<u64>> = vec![None; ends.len()];
    let mut seen = 0u64;
    loop {
        std::thread::sleep(std::time::Duration::from_millis(WATCH_MS));
        for (i, &at) in ends.iter().enumerate() {
            let mut bytes = [0u8; 8];
            // SAFETY: our own 8 bytes; title memory that is not there fails
            // the copy.
            if !unsafe { crate::safecopy::copy(bytes.as_mut_ptr(), at as *const u8, 8) } {
                last[i] = None;
                continue;
            }
            let now = u64::from_le_bytes(bytes);
            if let Some(was) = last[i].replace(now)
                && was != now
                && watch_flags(was, now)
            {
                seen += 1;
                let ours = last_touch(at);
                let near = event_at(at);
                tracing::warn!(
                    "smash watch (VICTORY_SMASH_WATCH=2): the qword at {at:#x}, 16 bytes before the end of its 64 KiB, went from \
                     {was:#018x} to {now:#018x} within {WATCH_MS} ms (frame {}); our last write in that 64 KiB: {}; an event of ours \
                     there: {} ({seen} such changes since the start)",
                    crate::frame::current(),
                    ours.map_or_else(|| "none since the watch began".to_string(), |(p, ago)| format!("{} {ago} ms ago", p.name())),
                    near.map_or_else(|| "none".to_string(), |e| format!("{} {} ms ago", e.path.name(), ms().saturating_sub(e.ms))),
                );
                if near.is_none() {
                    note(Event { path: Path::Watch, at, was, now, card: None, landing: landing_number(), frame: crate::frame::current(), ms: ms() });
                }
            }
        }
    }
}

/// An event: counted, kept, and logged if it may be.
fn note(event: Event) {
    COUNTS[event.path.index()].fetch_add(1, Ordering::Relaxed);
    SINCE[event.path.index()].fetch_add(1, Ordering::Relaxed);
    if let Ok(mut ring) = RING.lock() {
        if ring.len() == KEPT {
            ring.pop_front();
        }
        ring.push_back(event);
    }
    if let Ok(mut by) = BY_ADDRESS.lock() {
        let by = by.get_or_insert_with(Default::default);
        if by.len() >= BY_ADDRESS_MOST && !by.contains_key(&event.at) {
            by.clear();
        }
        by.insert(event.at, event);
    }
    let end = event.at % BLOCK == NODE_OBJECT;
    if event.path == Path::Watch || (LOGGED.fetch_add(1, Ordering::Relaxed) >= log_limit() && !end) {
        return;
    }
    tracing::warn!("{}", describe(&event, true));
}

/// An event as a line; with `context`, the landing's flights and the
/// programs that declared writes there.
fn describe(e: &Event, context: bool) -> String {
    let mut text = format!(
        "smash watch (VICTORY_SMASH_WATCH): {} turns a title pointer into no pointer at {:#x} ({:#x} into its 64 KiB): {:#018x} \
         -> {:#018x}, frame {}",
        e.path.name(),
        e.at,
        e.at % BLOCK,
        e.was,
        e.now,
        e.frame
    );
    if let Some((card, held)) = e.card {
        text.push_str(&format!("; the card held {card:#018x}, the CPU's side {held:#018x}"));
    }
    if e.landing != 0 {
        text.push_str(&format!("; landing {}", e.landing));
    }
    if context && matches!(e.path, Path::LandCard | Path::LandBack) {
        if let Ok(landing) = LANDING.try_lock()
            && landing.number == e.landing
        {
            let (first, last) = (landing.flights.first().copied().unwrap_or(0), landing.flights.last().copied().unwrap_or(0));
            text.push_str(&format!(" of {} flights ({first}..={last})", landing.flights.len()));
        }
        let (programs, ranges) = declared_over(e.at);
        let programs: Vec<String> = programs.iter().map(|p| if *p == 0 { "unknown".to_string() } else { format!("{p:016x}") }).collect();
        let ranges: Vec<String> = ranges.iter().map(|(a, b)| format!("{a:#x}..{b:#x}")).collect();
        text.push_str(&format!("; declared written there by programs [{}] in ranges [{}]", programs.join(", "), ranges.join(", ")));
    }
    text
}

/// Before a landing merges `len` bytes (within one 4 KiB page) into title
/// memory at `title`: `now` what the card holds, `held` what it held
/// ([`crate::safecopy::merge`]). `card`: the copy is in the card's memory.
/// With [`guard`]: the qwords to give back after the merge ([`keep_title`]),
/// each its offset from the stretch's start, what it was and what the merge
/// leaves.
///
/// # Safety
/// `now` and `held` are `len` readable bytes of ours.
pub(crate) unsafe fn before_merge(card: bool, title: usize, now: *const u8, held: *const u8, len: usize) -> Vec<(usize, u64, u64)> {
    // SAFETY: the caller's.
    unsafe { look_before_merge(card, title, now, held, len, level() > 0, guard()) }
}

/// [`before_merge`], with whether events are noted and whether the guard
/// asks for qwords back.
///
/// # Safety
/// [`before_merge`]'s.
unsafe fn look_before_merge(
    card: bool,
    title: usize,
    now: *const u8,
    held: *const u8,
    len: usize,
    notes: bool,
    guarding: bool,
) -> Vec<(usize, u64, u64)> {
    let path = if card { Path::LandCard } else { Path::LandBack };
    let mut keep = Vec::new();
    // SAFETY: the caller's.
    if len == 0 || len > 4096 || !unsafe { crate::safecopy::differ(now, held, len) } {
        return keep;
    }
    let mut page = [0u8; 4096];
    // SAFETY: into our own array; title memory that is not there fails the
    // copy, and the merge puts nothing there either.
    if !unsafe { crate::safecopy::copy(page.as_mut_ptr(), title as *const u8, len) } {
        return keep;
    }
    let mut i = title.wrapping_neg() & 7;
    while i + 8 <= len {
        // SAFETY: inside the caller's `len` bytes.
        let (c, h) = unsafe { (now.add(i).cast::<u64>().read_unaligned(), held.add(i).cast::<u64>().read_unaligned()) };
        if c != h {
            let t = u64::from_le_bytes(page[i..i + 8].try_into().unwrap_or_default());
            let m = merged(t, c, h);
            if smashes(t, m) {
                // (The guard alone notes nothing: the GPU's own data looks
                // like this some 90,000 times a second in the world, runs
                // barco-q-w1..3, and noting them all took about 1.8 ms a
                // frame, prisao-q-w1.)
                if notes {
                    note(Event { path, at: title + i, was: t, now: m, card: Some((c, h)), landing: landing_number(), frame: crate::frame::current(), ms: ms() });
                }
                if guarding && watch_flags(t, m) {
                    keep.push((i, t, m));
                }
            }
        }
        i += 8;
    }
    keep
}

/// Before `now` is put at title address `at` where it differs from `was`
/// (what title memory held when read: `crate::shadow`'s `put_changed`).
pub(crate) fn before_put(path: Path, at: usize, was: &[u8], now: &[u8]) {
    let n = was.len().min(now.len());
    let mut i = at.wrapping_neg() & 7;
    while i + 8 <= n {
        let w = u64::from_le_bytes(was[i..i + 8].try_into().unwrap_or_default());
        let v = u64::from_le_bytes(now[i..i + 8].try_into().unwrap_or_default());
        if w != v && smashes(w, v) {
            note(Event { path, at: at + i, was: w, now: v, card: None, landing: landing_number(), frame: crate::frame::current(), ms: ms() });
        }
        i += 8;
    }
}

/// Before `len` bytes from `src` (ours or title memory) are copied to title
/// memory at `dst`: both are read here (what cannot be read is taken as
/// zeros, which the copy leaves there).
pub(crate) fn before_copy(path: Path, dst: usize, src: *const u8, len: usize) {
    let (mut was, mut now) = ([0u8; 4096], [0u8; 4096]);
    let mut from = 0;
    while from < len {
        // Pieces that end on a 4 KiB line of the destination, so that its
        // qwords are whole in one.
        let n = (4096 - (dst + from) % 4096).min(len - from);
        // SAFETY: into our own arrays; memory that is not there fails the
        // copy.
        let there = unsafe { crate::safecopy::copy(was.as_mut_ptr(), (dst + from) as *const u8, n) };
        if there {
            // SAFETY: as above.
            if !unsafe { crate::safecopy::copy(now.as_mut_ptr(), src.wrapping_add(from), n) } {
                now[..n].fill(0);
            }
            before_put(path, dst + from, &was[..n], &now[..n]);
        }
        from += n;
    }
}

/// Before the low `bytes` (4 or 8) of `value` are stored at `at`
/// (`crate::queue::Written::write`).
pub(crate) fn before_value(at: usize, value: u64, bytes: usize) {
    let aligned = at & !7;
    let mut was = [0u8; 16];
    // SAFETY: into our own array; memory that is not there fails the copy.
    if !unsafe { crate::safecopy::copy(was.as_mut_ptr(), aligned as *const u8, 16) } {
        return;
    }
    let mut now = was;
    let off = at - aligned;
    now[off..off + bytes].copy_from_slice(&value.to_le_bytes()[..bytes]);
    for q in [0, 8] {
        let w = u64::from_le_bytes(was[q..q + 8].try_into().unwrap_or_default());
        let v = u64::from_le_bytes(now[q..q + 8].try_into().unwrap_or_default());
        if w != v && smashes(w, v) {
            note(Event {
                path: Path::Value,
                at: aligned + q,
                was: w,
                now: v,
                card: None,
                landing: landing_number(),
                frame: crate::frame::current(),
                ms: ms(),
            });
        }
    }
}

/// For the frame-rate line.
pub(crate) fn log(frames: f64) {
    if !on() {
        return;
    }
    let since: Vec<u64> = SINCE.iter().map(|s| s.swap(0, Ordering::Relaxed)).collect();
    let all: Vec<u64> = COUNTS.iter().map(|c| c.load(Ordering::Relaxed)).collect();
    let by = |v: &[u64]| -> String {
        let parts: Vec<String> = (0..PATHS).filter(|&i| v[i] > 0).filter_map(|i| Some(format!("{} {}", Path::of(i as u8)?.short(), v[i]))).collect();
        if parts.is_empty() { "none".to_string() } else { parts.join(", ") }
    };
    tracing::info!(
        "smash watch (VICTORY_SMASH_WATCH={}): writes of ours that turn a title pointer into no pointer, since the last line \
         ({frames:.0} frames): {}; since the start: {}",
        level(),
        by(&since),
        by(&all),
    );
    if guard() {
        tracing::info!(
            "smash guard (VICTORY_SMASH_GUARD=1): title pointers a landing would have smashed, kept as the title had them: {} since the \
             last line, {} since the start",
            KEPT_SINCE.swap(0, Ordering::Relaxed),
            KEPT_ALL.load(Ordering::Relaxed),
        );
    }
}

/// For the crash report: the events kept, those within 64 bytes of a
/// register's value first (the title faults on the pointer it read, and a
/// register holds the record's address or the pointer).
pub(crate) fn crash_note(registers: &[u64; 16]) -> String {
    let mut text = if on() { note_text(registers) } else { String::new() };
    text.push_str(&writers_note(registers));
    text
}

/// [`crash_note`]'s text, whether the switch is on or not.
fn note_text(registers: &[u64; 16]) -> String {
    let Ok(ring) = RING.try_lock() else {
        return "smash watch: the events are locked (a thread holds them), none printed".to_string();
    };
    let all: Vec<u64> = COUNTS.iter().map(|c| c.load(Ordering::Relaxed)).collect();
    let mut text = format!("smash watch (VICTORY_SMASH_WATCH={}): {} events since the start, {} kept\n", level(), all.iter().sum::<u64>(), ring.len());
    // Within 64 bytes of a register: the record's address (the title walks
    // the list with it in a register), or the pointer it read.
    let mut matched: Vec<Event> = Vec::new();
    if let Ok(by) = BY_ADDRESS.try_lock()
        && let Some(by) = by.as_ref()
    {
        for &r in registers {
            let r = r as usize & !7;
            for at in (r.saturating_sub(64)..r.saturating_add(64)).step_by(8) {
                if let Some(e) = by.get(&at)
                    && !matched.contains(e)
                {
                    matched.push(*e);
                }
            }
        }
        let pointers: Vec<Event> =
            by.values().filter(|e| registers.iter().any(|&r| e.now == r || e.was == r) && !matched.contains(e)).copied().collect();
        matched.extend(pointers);
    }
    matched.sort_by_key(|e| e.ms);
    text.push_str(&format!("  near a register: {}\n", matched.len()));
    for e in matched.iter().rev().take(32) {
        text.push_str(&format!("  * {} ({} ms before the end)\n", describe(e, false), ms().saturating_sub(e.ms)));
    }
    text.push_str("  the last ones:\n");
    for e in ring.iter().rev().take(16) {
        text.push_str(&format!("  - {} ({} ms before the end)\n", describe(e, false), ms().saturating_sub(e.ms)));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_crash_pointers_look_like_pointers_and_their_smashes_are_seen() {
        // The five crashes: the right pointer, what the title read.
        for (was, now) in [
            (0x9_0418_cf18u64, 0x1_0418_cf18u64),
            (0x9_0418_d7e8, 0x1_0418_d7e8),
            (0x9_0418_d7b8, 0x1_0418_d7b8),
            (0x9_0418_d5a8, 0x49_0418_d5a8),
            (0x9_0418_d000, 0x1_0000_0000),
            // barco-s-final's later faults, and barco-ab-f2.
            (0x9_0418_d000, 0x9_0000_0000),
            (0x9_0418_e9f0, 0x9_0041_8e9f),
        ] {
            assert!(pointer_like(was));
            assert!(smashes(was, now), "{was:#x} -> {now:#x}");
            assert!(watch_flags(was, now), "{was:#x} -> {now:#x}");
        }
    }

    #[test]
    fn data_that_is_no_pointer_and_pointers_moved_within_their_window_are_not_smashes() {
        // Floats, small integers, a pointer to another object in the same
        // 4 GiB, a pointer not 8-aligned, zero.
        assert!(!smashes(0x3f80_0000_3f00_0000, 0));
        assert!(!smashes(0x0000_0001_0000_0000, 0x0000_0002_0000_0000));
        assert!(!smashes(0x9_0418_cf18, 0x9_0418_d7e8));
        assert!(!smashes(0x9_0418_cf18, 0x9_0418_cf18));
        assert!(!smashes(0x9_0418_cf1c, 0x1_0418_cf1c));
        assert!(!smashes(0, 0x1_0000_0000));
        assert!(!pointer_like(0x7ffe_0000_1000));
    }

    #[test]
    fn a_merge_takes_the_cards_bytes_where_they_differ_from_what_it_held() {
        assert_eq!(byte_mask(0), 0);
        assert_eq!(byte_mask(0x0000_0100_0080_0001), 0x0000_ff00_00ff_00ff);
        assert_eq!(byte_mask(u64::MAX), u64::MAX);
        // The title wrote a record's pointer; the card's copy still had the
        // old high dword (1) where the CPU's side has the new one (9): the
        // merge would put the 1 back.
        let (title, held, card) = (0x9_0418_cf18u64, 0x9_0418_cf18u64, 0x1_0418_cf18u64);
        let m = merged(title, card, held);
        assert_eq!(m, 0x1_0418_cf18);
        assert!(smashes(title, m));
        // A shader that wrote beside the pointer leaves it as it is.
        assert_eq!(merged(title, 0x9_0418_cf18, 0x9_0418_cf18), title);
    }

    #[test]
    fn the_watch_flags_the_crashes_and_not_a_new_record() {
        assert!(watch_flags(0x9_0418_cf18, 0x1_0418_cf18));
        assert!(watch_flags(0x9_0418_d5a8, 0x49_0418_d5a8));
        assert!(watch_flags(0x9_0418_d5a8, 0x1_0000_0000));
        // The title writes a new record: another object, or data.
        assert!(!watch_flags(0x9_0418_cf18, 0x9_0418_d7e8));
        assert!(!watch_flags(0x9_0418_cf18, 0x3f80_0000_0000_0030));
        assert!(!watch_flags(0x3f80_0000_0000_0030, 0x1_0418_cf18));
        assert!(!watch_flags(0x9_0418_cf18, 0x9_0418_cf18));
    }

    #[test]
    fn a_merge_over_a_pointer_is_noted_with_what_the_card_and_the_cpu_held() {
        if !crate::safecopy::on() {
            return;
        }
        // Title memory of our own: a record at the end of a 64 KiB line,
        // as the ring keeps one.
        let mut title = vec![0u8; 2 * BLOCK];
        let start = title.as_mut_ptr() as usize;
        // A 64 KiB line inside `title`, with 4 KiB of it below.
        let base = (start + BLOCK).next_multiple_of(BLOCK);
        let at = base - 0x1000;
        let node = base - 0x18;
        let len = 0x1000;
        title[node + 8 - start..node + 16 - start].copy_from_slice(&0x9_0418_cf18u64.to_le_bytes());
        let held: Vec<u8> = title[at - start..at - start + len].to_vec();
        let mut card = held.clone();
        card[node + 8 - at + 4] = 1;
        let before = COUNTS[Path::LandCard.index()].load(Ordering::Relaxed);
        // SAFETY: `card` and `held` are ours, `len` bytes.
        let keep = unsafe { look_before_merge(true, at, card.as_ptr(), held.as_ptr(), len, true, true) };
        assert_eq!(keep, [(node + 8 - at, 0x9_0418_cf18, 0x1_0418_cf18)]);
        assert!(COUNTS[Path::LandCard.index()].load(Ordering::Relaxed) > before);
        let ring = RING.lock().unwrap();
        let e = ring.iter().rev().find(|e| e.at == node + 8).copied().unwrap();
        assert_eq!((e.was, e.now), (0x9_0418_cf18, 0x1_0418_cf18));
        assert_eq!(e.card, Some((0x1_0418_cf18, 0x9_0418_cf18)));
        drop(ring);
        // The title's crash: r15 holds the record's address.
        let mut registers = [0u64; 16];
        registers[15] = node as u64;
        let note = note_text(&registers);
        assert!(note.contains(&format!("at {:#x}", node + 8)), "{note}");
        assert!(!note.contains("near a register: 0"), "{note}");
    }

    #[test]
    fn a_value_written_over_a_pointers_high_half_is_noted() {
        if !crate::safecopy::on() {
            return;
        }
        let mut memory = [0u64; 4];
        memory[1] = 0x9_0418_d7e8;
        let at = std::ptr::from_mut(&mut memory[1]) as usize;
        let before = COUNTS[Path::Value.index()].load(Ordering::Relaxed);
        before_value(at + 4, 1, 4);
        assert_eq!(COUNTS[Path::Value.index()].load(Ordering::Relaxed), before + 1);
        // A value beside it is no event.
        before_value(at - 8, 7, 8);
        assert_eq!(COUNTS[Path::Value.index()].load(Ordering::Relaxed), before + 1);
    }

    #[test]
    fn a_copy_over_a_pointer_is_noted_and_a_copy_of_the_same_bytes_is_not() {
        if !crate::safecopy::on() {
            return;
        }
        let mut dst = [0u64; 8];
        dst[3] = 0x9_0418_d000;
        let mut src = dst;
        let at = dst.as_mut_ptr() as usize;
        let before = COUNTS[Path::Copy.index()].load(Ordering::Relaxed);
        before_copy(Path::Copy, at, src.as_ptr().cast(), 64);
        assert_eq!(COUNTS[Path::Copy.index()].load(Ordering::Relaxed), before);
        src[3] = 0x1_0000_0000;
        before_copy(Path::Copy, at, src.as_ptr().cast(), 64);
        assert_eq!(COUNTS[Path::Copy.index()].load(Ordering::Relaxed), before + 1);
    }

    #[test]
    fn the_guard_gives_the_title_its_pointer_back_unless_the_title_wrote_since() {
        if !crate::safecopy::on() {
            return;
        }
        // The merge left the card's smashed pointer in both.
        let mut title = [0x1_0418_cf18u64, 0];
        let mut held = [0x1_0418_cf18u64, 0];
        let (at, held_at) = (title.as_mut_ptr() as usize, held.as_mut_ptr() as usize);
        // SAFETY: `held` is ours.
        unsafe { keep_title(at, held_at as *mut u8, 0x9_0418_cf18, 0x1_0418_cf18) };
        // SAFETY: our own memory, written through its address above.
        let (t, h) = unsafe { ((at as *const u64).read_volatile(), (held_at as *const u64).read_volatile()) };
        assert_eq!((t, h), (0x9_0418_cf18, 0x9_0418_cf18));
        // The title wrote a new record meanwhile: it stays.
        // SAFETY: as above.
        unsafe {
            ((at + 8) as *mut u64).write_volatile(0x9_0418_d7e8);
            keep_title(at + 8, (held_at + 8) as *mut u8, 0x9_0418_cf18, 0x1_0418_cf18);
            assert_eq!(((at + 8) as *const u64).read_volatile(), 0x9_0418_d7e8);
        }
    }

    #[test]
    fn the_stamps_keep_the_last_four_writers_of_each_64_kib_the_newest_first() {
        // (No memory behind the range: stamps are only numbers.)
        let base = 0x3744_0000;
        let stamps = Stamps::new(base, 8 * BLOCK);
        assert!(stamps.writers(base).is_empty());
        for (i, path) in [Path::LandCard, Path::Descriptor, Path::Copy, Path::Value, Path::LandCard].into_iter().enumerate() {
            stamps.stamp(path, base + BLOCK - 0x18, 0x18, 100 + i as u64, 7 + i as u64);
        }
        let w = stamps.writers(base);
        assert_eq!(w.len(), 4);
        assert_eq!(w[0], Writer { path: Path::LandCard, ms: 104, landing: 11 });
        assert_eq!((w[1].path, w[2].path, w[3].path), (Path::Value, Path::Copy, Path::Descriptor));
        // A write over two pieces stamps both; one outside stamps nothing.
        stamps.stamp(Path::ReadBack, base + 2 * BLOCK - 8, 16, 200, 9);
        assert_eq!(stamps.writers(base + 2 * BLOCK)[0].path, Path::ReadBack);
        assert_eq!(stamps.writers(base + BLOCK)[0].path, Path::ReadBack);
        stamps.stamp(Path::Copy, base - 16, 8, 300, 9);
        assert!(stamps.writers(base - 16).is_empty());
    }

    #[test]
    fn the_crash_note_names_the_writers_of_a_registers_64_kib_and_of_a_descriptors_block() {
        if !crate::safecopy::on() {
            return;
        }
        let base = 0x3744_0000usize;
        let stamps = Stamps::new(base, 8 * BLOCK);
        // The last record of block 1 (its second half) and a descriptor
        // naming block 1, as `rdi` held it at barco-pg-3's crash.
        stamps.stamp(Path::LandCard, base + 4 * BLOCK - 0x18, 8, 90, 321);
        let descriptor = [0u64, 0, (base + 2 * BLOCK) as u64, 0];
        let mut registers = [0u64; 16];
        registers[7] = descriptor.as_ptr() as u64;
        registers[15] = (base + 4 * BLOCK - 0x18) as u64;
        let text = writers_text(&stamps, &registers, 100, 400);
        assert!(text.contains("r15 in"), "{text}");
        assert!(text.contains("rdi's ring block, second half"), "{text}");
        assert!(text.contains("land-card 10 ms before the end (landing 321)"), "{text}");
    }

    #[test]
    fn every_path_has_its_own_number() {
        for i in 0..PATHS as u8 {
            let path = Path::of(i).unwrap();
            assert_eq!(path.index(), usize::from(i));
            assert!(!path.name().is_empty() && !path.short().is_empty());
        }
        assert_eq!(Path::of(PATHS as u8), None);
    }

    #[test]
    fn a_programs_hash_is_kept_while_its_stage_is_bound() {
        program(Some("dd217cde3d9b34e3"));
        assert_eq!(current_program(), 0xdd21_7cde_3d9b_34e3);
        program(None);
        assert_eq!(current_program(), 0);
    }
}
