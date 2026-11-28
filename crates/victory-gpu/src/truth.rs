//! `VICTORY_TEX_TRUTH=1` (off by default; log only): what the title itself
//! says of the pages its streamed textures are read from, set against what
//! our layer shows (docs `api-confete.md`, section 3). Four parts:
//!
//! - P1, at every `CopyPageMappingsBatchX`: for each pool page mapped into
//!   the tiled heap, the physical page the title's own table names for that
//!   pool index (title+0x71bfcc0, `VICTORY_TEX_TRUTH_TABLE` to move it;
//!   21-bit numbers `>> 4`, 0xFFFFFFFF free) against the page our pool view
//!   shows there, and, after the mapping, against what the GPU address shows.
//!   The table's address and form are checked in the same run: the page
//!   numbers `XMemMapPhysicalPagesScatter` got just before the batch must
//!   be those the table names (read both as a table at that address and as
//!   a pointer to one; the reading that agrees more is used).
//! - P2, when a texture is refreshed (every 8th frame at most): each of its
//!   64 KiB pages is resident (the pool index it came from is still the
//!   title's for that page), free (the table says the index is free),
//!   reused (a later batch mapped the same index elsewhere), or a hole
//!   (unmapped by a batch). From the layout: the first level from which
//!   every level is resident. Shader views made of such a texture that
//!   start finer than that are counted, with whether the title's own
//!   `ResourceMinLODClamp` kept them off it.
//! - P3, every 5 s and at a capture: textures of 4 MiB at most read lately,
//!   their memory now against what was last detiled, 64 KiB at a time.
//! - P4: landings (shader writes going into title memory) on a page that a
//!   batch remapped after the submission they came from had begun.
//!
//! `VICTORY_CLAMP_RESIDENT=1` (off by default) is the remedy P2 points to,
//! the idea of upstream pull request 3647 (`VICTORY_PARTIAL_MIP_CLAMP`)
//! with the title's table for "there" in place of "mapped": shader views of
//! such a texture start at its first fully resident level, and are made
//! again when that changes. It turns the P2 bookkeeping on by itself.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};

const PAGE: u64 = 64 << 10;
/// A table entry of a free pool index.
const FREE: u32 = u32::MAX;

/// Whether the probe is on (`VICTORY_TEX_TRUTH=1`).
pub fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| crate::switch::on("VICTORY_TEX_TRUTH"))
}

/// Whether views are kept to resident levels (`VICTORY_CLAMP_RESIDENT=1`).
pub(crate) fn clamp() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| crate::switch::on("VICTORY_CLAMP_RESIDENT"))
}

/// Whether pages are followed at all.
pub(crate) fn wanted() -> bool {
    on() || clamp()
}

/// What the runtime's own pool view showed at a pool page, against the
/// page the title's table names there.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pool {
    /// The same physical page.
    Agrees,
    /// Another physical page.
    Other,
    /// The runtime knows no page by that number.
    Unknown,
    /// The pool view shows no page there.
    Hole,
}

/// The runtime's answers ([`use_runtime`]).
pub struct Runtime {
    /// Where the title's image starts.
    pub title: fn() -> Option<usize>,
    /// For each (pool page address, GPU address, page number): what the
    /// pool view shows against the number, and whether the GPU address
    /// shows the same page as the pool view (after the mapping).
    pub compare: fn(&[(u64, u64, u64)]) -> Vec<(Pool, bool)>,
}

static RUNTIME: OnceLock<Runtime> = OnceLock::new();

/// The runtime names its answers (before the device exists).
pub fn use_runtime(runtime: Runtime) {
    let _ = RUNTIME.set(runtime);
}

/// Where the title's table of pool pages is, from its image's start.
fn table_offset() -> usize {
    static AT: OnceLock<usize> = OnceLock::new();
    *AT.get_or_init(|| {
        std::env::var("VICTORY_TEX_TRUTH_TABLE")
            .ok()
            .and_then(|v| usize::from_str_radix(v.trim().trim_start_matches("0x"), 16).ok())
            .unwrap_or(0x71b_fcc0)
    })
}

/// A page number of the title's table entry (`None`: free).
fn number(entry: u32) -> Option<u64> {
    (entry != FREE).then_some(u64::from(entry & 0x1f_ffff) << 4)
}

/// Both readings of the title's table: the entries at the address, and the
/// entries behind a pointer kept there. Read whole, through the kernel.
#[derive(Default)]
struct Table {
    frame: u64,
    read: [Vec<u32>; 2],
    /// Where each reading starts (0: none).
    at: [usize; 2],
}

impl Table {
    /// Where both readings start now.
    fn find(&mut self) {
        let Some(at) = RUNTIME.get().and_then(|r| (r.title)()).map(|base| base + table_offset()) else {
            self.at = [0; 2];
            return;
        };
        let mut pointer = [0u8; 8];
        let behind = if crate::host::read_into(at as *const u8, &mut pointer) { u64::from_le_bytes(pointer) as usize } else { 0 };
        self.at = [at, behind];
    }

    /// One entry as the title holds it now (a batch's own entries are
    /// read so, not from the copy of the whole table [`Table::take`]
    /// makes once a frame).
    fn now(&self, reading: usize, index: u32) -> Option<u32> {
        let at = self.at[reading];
        let mut entry = [0u8; 4];
        (at != 0 && crate::host::read_into((at + index as usize * 4) as *const u8, &mut entry))
            .then(|| u32::from_le_bytes(entry))
    }

    fn take(&mut self, entries: usize) {
        self.frame = crate::frame::current();
        let entries = entries.min(4 << 20);
        self.find();
        for (k, from) in self.at.into_iter().map(|at| (at != 0).then_some(at)).enumerate() {
            let out = &mut self.read[k];
            out.resize(entries, FREE);
            let ok = from.filter(|&f| f != 0).is_some_and(|from| {
                // SAFETY: `out` is `entries` u32s; viewed as their bytes.
                let bytes = unsafe { std::slice::from_raw_parts_mut(out.as_mut_ptr().cast::<u8>(), entries * 4) };
                crate::host::read_into(from as *const u8, bytes)
            });
            if !ok {
                out.clear();
            }
        }
    }

    fn entry(&self, reading: usize, index: u32) -> Option<u32> {
        self.read[reading].get(index as usize).copied()
    }
}

/// A heap page's pool index, as the last batch to name the page left it.
#[derive(Clone, Copy)]
struct Mapped {
    pool: u64,
    /// `None`: unmapped (a null range).
    index: Option<u32>,
    /// One index shown at many pages (a constant range): not reused.
    shared: bool,
    /// The submissions begun when it was mapped (`crate::host::submissions`).
    begun: u64,
}

#[derive(Default)]
struct Pages {
    /// By heap page (address / 64 KiB).
    mapped: HashMap<u64, Mapped>,
    /// The heap page each (pool, index) was last mapped at.
    owner: HashMap<(u64, u32), u64>,
    /// Page numbers `XMemMapPhysicalPagesScatter` got lately.
    scattered: HashSet<u64>,
    pool: (u64, u32),
    table: Table,
}

static PAGES: Mutex<Option<Pages>> = Mutex::new(None);

fn pages<R>(f: impl FnOnce(&mut Pages) -> R) -> R {
    let mut pages = PAGES.lock().unwrap_or_else(|e| e.into_inner());
    f(pages.get_or_insert_with(Pages::default))
}

/// Counters, all since the start.
#[derive(Default)]
struct Counts {
    /// Scatter against table: [reading][same, not].
    checked: [[AtomicU64; 2]; 2],
    /// P1: agrees, other, free, hole, unknown; the GPU address after the
    /// mapping not showing the pool page.
    pool: [AtomicU64; 5],
    after: AtomicU64,
    /// P2: textures looked at, with a level let go; their pages resident,
    /// free, reused, hole, not mapped by a batch.
    textures: [AtomicU64; 2],
    page: [AtomicU64; 5],
    /// Views made of a looked-at texture, reaching a level let go, of those
    /// kept off it by the title's clamp, moved by the clamp.
    views: [AtomicU64; 4],
    /// P3: same and let go, changed, same and resident.
    audit: [AtomicU64; 3],
    /// P4: landings on pages likely remapped after their submission began,
    /// the pages, and the landings for which that is certain.
    late: [AtomicU64; 3],
}

static COUNTS: OnceLock<Counts> = OnceLock::new();

fn counts() -> &'static Counts {
    COUNTS.get_or_init(Counts::default)
}

/// Warned of: the first 16 and then at powers of 2.
fn worth(n: u64) -> bool {
    n <= 16 || n.is_power_of_two()
}

/// The reading of the table taken: the one more scatters agreed with.
fn reading() -> usize {
    let c = &counts().checked;
    usize::from(c[1][0].load(Relaxed) > c[0][0].load(Relaxed))
}

/// The page numbers of a `XMemMapPhysicalPagesScatter` call (0: unmapped).
pub fn scattered(numbers: impl Iterator<Item = usize>) {
    if !on() {
        return;
    }
    pages(|p| {
        if p.scattered.len() > 1 << 16 {
            p.scattered.clear();
        }
        p.scattered.extend(numbers.filter(|&n| n != 0).map(|n| n as u64));
    });
}

/// One page of a batch: the heap address, the pool index it shows (`None`:
/// unmapped), and whether that index is shown at many pages.
pub(crate) type Mapping = (u64, Option<u32>, bool);

/// After a `CopyPageMappingsBatchX` (its mappings made): what it mapped.
pub(crate) fn batch(pool: u64, pool_pages: u32, mapped: &[Mapping]) {
    if mapped.is_empty() {
        return;
    }
    let begun = crate::host::submissions().0;
    let c = counts();
    let mut compare: Vec<(u64, u64, u64)> = Vec::new();
    pages(|p| {
        p.pool = (pool, pool_pages);
        if on() {
            p.table.find();
        }
        for &(at, index, shared) in mapped {
            let page = at / PAGE;
            p.mapped.insert(page, Mapped { pool, index, shared, begun });
            let Some(index) = index else { continue };
            if !shared {
                p.owner.insert((pool, index), page);
            }
            if !on() {
                continue;
            }
            let entries = [p.table.now(0, index), p.table.now(1, index)];
            for (k, entry) in entries.iter().enumerate() {
                if let Some(n) = entry.and_then(number) {
                    c.checked[k][usize::from(!p.scattered.contains(&n))].fetch_add(1, Relaxed);
                }
            }
            match entries[reading()] {
                Some(FREE) => {
                    c.pool[2].fetch_add(1, Relaxed);
                }
                Some(entry) => compare.push((pool + u64::from(index) * PAGE, at, number(entry).unwrap_or(0))),
                None => {}
            }
        }
    });
    let Some(runtime) = RUNTIME.get().filter(|_| !compare.is_empty()) else {
        return;
    };
    for ((pool_at, at, n), (said, same)) in compare.iter().zip((runtime.compare)(&compare)) {
        let slot = match said {
            Pool::Agrees => 0,
            Pool::Other => 1,
            Pool::Hole => 3,
            Pool::Unknown => 4,
        };
        let k = c.pool[slot].fetch_add(1, Relaxed) + 1;
        if said == Pool::Other && worth(k) {
            tracing::warn!(
                pool_page = format_args!("{pool_at:#x}"),
                gpu = format_args!("{at:#x}"),
                page = format_args!("{n:#x}"),
                times = k,
                "texture truth: the pool view shows another page than the title's table names"
            );
        }
        if !same {
            let k = c.after.fetch_add(1, Relaxed) + 1;
            if worth(k) {
                tracing::warn!(
                    pool_page = format_args!("{pool_at:#x}"),
                    gpu = format_args!("{at:#x}"),
                    times = k,
                    "texture truth: after the mapping the GPU address shows another page than the pool"
                );
            }
        }
    }
}

/// What a texture's 64 KiB page is for the title.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Held {
    Resident,
    /// The table says its pool index is free.
    Free,
    /// A later batch mapped its pool index at another page.
    Reused,
    /// Unmapped by a batch.
    Hole,
    /// No batch mapped it: not followed.
    Untracked,
}

impl Held {
    fn let_go(self) -> bool {
        matches!(self, Held::Free | Held::Reused | Held::Hole)
    }
}

/// The pages under `[at, at + len)`, from the page `at` lies in.
fn held(at: u64, len: u64) -> Vec<Held> {
    let (first, end) = (at / PAGE, (at + len.max(1)).div_ceil(PAGE));
    pages(|p| {
        if p.table.frame != crate::frame::current() && p.pool.1 > 0 {
            p.table.take(p.pool.1 as usize);
        }
        let reading = reading();
        (first..end)
            .map(|page| match p.mapped.get(&page) {
                None => Held::Untracked,
                Some(Mapped { index: None, .. }) => Held::Hole,
                Some(&Mapped { pool, index: Some(index), shared, .. }) => {
                    if !shared && p.owner.get(&(pool, index)).is_some_and(|&o| o != page) {
                        Held::Reused
                    } else if p.table.entry(reading, index) == Some(FREE) {
                        Held::Free
                    } else {
                        Held::Resident
                    }
                }
            })
            .collect()
    })
}

/// The byte span of each level of slice 0 (the smallest levels share the
/// block at the slice's start), as `victory_formats::texture::Layout` lays
/// them out: that block, then the levels from the smallest up, level 0
/// last. `None` for more than one slice, which is not judged.
fn spans(layout: &victory_formats::texture::Layout) -> Option<Vec<(u64, u64)>> {
    let mips = &layout.mips;
    if layout.slices != 1 || mips.is_empty() || mips.len() > 32 {
        return None;
    }
    let tail = (layout.first_tail_mip as usize).min(mips.len());
    Some(
        (0..mips.len())
            .map(|m| {
                if m >= tail {
                    (0, layout.alignment.min(layout.slice_size))
                } else {
                    (mips[m].offset, if m == 0 { layout.slice_size } else { mips[m - 1].offset })
                }
            })
            .collect(),
    )
}

/// The first level from which every level is kept, by `kept` of each
/// level's span; 0 if all are, and if not even the last level is (nothing
/// better to show).
fn first_kept(spans: &[(u64, u64)], kept: impl Fn(u64, u64) -> bool) -> u32 {
    let mut first = spans.len();
    while first > 0 && kept(spans[first - 1].0, spans[first - 1].1) {
        first -= 1;
    }
    if first == spans.len() { 0 } else { first as u32 }
}

/// What was learnt of a texture by its memory's start.
struct Texture {
    len: u64,
    frame: u64,
    /// Its first fully resident level.
    first: u32,
    /// Per 64 KiB of its memory: the digest of what was last detiled.
    detiled: Vec<u64>,
}

static TEXTURES: Mutex<Option<HashMap<u64, Texture>>> = Mutex::new(None);

fn textures<R>(f: impl FnOnce(&mut HashMap<u64, Texture>) -> R) -> R {
    let mut t = TEXTURES.lock().unwrap_or_else(|e| e.into_inner());
    f(t.get_or_insert_with(HashMap::new))
}

/// Looked at again after this many frames.
const EVERY: u64 = 8;

/// A texture at `[at, at + len)` is refreshed: its pages are looked at
/// (every [`EVERY`] frames at most). If its first resident level moved and
/// views are clamped, its views are made again.
pub(crate) fn refreshed(at: u64, len: u64, layout: &victory_formats::texture::Layout) {
    let frame = crate::frame::current();
    let due = textures(|t| {
        let tex = t.entry(at).or_insert(Texture { len, frame: 0, first: 0, detiled: Vec::new() });
        let due = tex.frame == 0 || frame >= tex.frame + EVERY;
        if due {
            tex.frame = frame.max(1);
        }
        due
    });
    if !due {
        return;
    }
    let held = held(at, len);
    if held.iter().all(|&h| h == Held::Untracked) {
        return;
    }
    let c = counts();
    for &h in &held {
        let slot = match h {
            Held::Resident => 0,
            Held::Free => 1,
            Held::Reused => 2,
            Held::Hole => 3,
            Held::Untracked => 4,
        };
        c.page[slot].fetch_add(1, Relaxed);
    }
    let base = at / PAGE * PAGE;
    let kept = |from: u64, to: u64| {
        let (a, b) = ((at + from - base) / PAGE, (at + to.max(from + 1) - base).div_ceil(PAGE));
        (a..b).all(|k| held.get(k as usize).is_none_or(|h| !h.let_go()))
    };
    let first = spans(layout).map_or(0, |spans| first_kept(&spans, kept));
    c.textures[0].fetch_add(1, Relaxed);
    if first > 0 {
        let n = c.textures[1].fetch_add(1, Relaxed) + 1;
        if on() && worth(n) {
            tracing::warn!(
                memory = format_args!("{at:#x}"),
                bytes = len,
                first_resident = first,
                levels = layout.mips.len(),
                pages = ?held,
                times = n,
                "texture truth: a texture read from pages the title let go"
            );
        }
    }
    let moved = textures(|t| t.get_mut(&at).is_some_and(|tex| std::mem::replace(&mut tex.first, first) != first));
    if moved && clamp() {
        crate::descriptor::host::levels_changed(at);
    }
}

/// A shader view of title texture `resource` is made, starting at level
/// `most` with the title's clamp `min_lod`: counted against its first
/// resident level. Returns the level the view is to start at (clamp on,
/// and the view started finer and the title's clamp did not keep it off).
pub(crate) fn view_made(resource: crate::object::This, most: u32, min_lod: f32) -> Option<u32> {
    let (at, _) = crate::texture::staged_memory(resource)?;
    let first = textures(|t| t.get(&at).map(|tex| tex.first))?;
    let c = counts();
    c.views[0].fetch_add(1, Relaxed);
    if first == 0 || most >= first {
        return None;
    }
    let n = c.views[1].fetch_add(1, Relaxed) + 1;
    if min_lod.floor() >= first as f32 {
        c.views[2].fetch_add(1, Relaxed);
        return None;
    }
    if on() && worth(n) {
        tracing::warn!(
            memory = format_args!("{at:#x}"),
            most,
            min_lod,
            first_resident = first,
            times = n,
            "texture truth: a view reaches levels the title let go"
        );
    }
    c.views[3].fetch_add(1, Relaxed);
    clamp().then_some(first)
}

/// A fast digest of 64 KiB of memory (P3 compares them, nothing more).
fn digest(bytes: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64 ^ bytes.len() as u64;
    for word in bytes.chunks(8) {
        let mut w = [0u8; 8];
        w[..word.len()].copy_from_slice(word);
        h = (h ^ u64::from_le_bytes(w)).wrapping_mul(0x9e37_79b9_7f4a_7c15).rotate_left(29);
    }
    h
}

/// Textures of this many bytes at most are audited (P3).
const AUDITED: u64 = 4 << 20;

/// A texture at `at` was detiled from `tiled` (P3 keeps what it was).
pub(crate) fn detiled(at: u64, tiled: &[u8]) {
    if !on() || tiled.len() as u64 > AUDITED {
        return;
    }
    let pieces = pieces(at, tiled);
    textures(|t| {
        if let Some(tex) = t.get_mut(&at) {
            tex.detiled = pieces;
        }
    });
}

/// The digests of `bytes` at `at`, by the 64 KiB pages they lie in.
fn pieces(at: u64, bytes: &[u8]) -> Vec<u64> {
    let mut out = Vec::new();
    let mut from = 0usize;
    while from < bytes.len() {
        let to = (((at + from as u64) / PAGE + 1) * PAGE - at) as usize;
        let to = to.min(bytes.len());
        out.push(digest(&bytes[from..to]));
        from = to;
    }
    out
}

/// The audit (P3) of the textures read in the last `frames` frames, of
/// `budget` bytes at most; with `keep`, their memory is written there.
fn audit(frames: u64, budget: u64, keep: Option<&std::path::Path>) -> [u64; 3] {
    let now = crate::frame::current();
    let mut due: Vec<(u64, u64, Vec<u64>, u32)> = textures(|t| {
        t.iter()
            .filter(|(_, tex)| !tex.detiled.is_empty() && tex.frame + frames >= now)
            .map(|(&at, tex)| (at, tex.len, tex.detiled.clone(), tex.first))
            .collect()
    });
    due.sort_unstable_by_key(|d| d.0);
    let mut spent = 0;
    let mut found = [0u64; 3];
    let mut index = String::new();
    for (at, len, before, first) in due {
        if spent + len > budget {
            break;
        }
        spent += len;
        let mut now = vec![0u8; len as usize];
        crate::host::copy_mapped(now.as_mut_ptr(), at as *const u8, len as usize, false);
        let held = held(at, len);
        let mut classes = String::new();
        for (k, (old, new)) in before.iter().zip(pieces(at, &now)).enumerate() {
            let h = held.get(k).copied().unwrap_or(Held::Untracked);
            let class = if *old != new {
                1
            } else if h.let_go() {
                0
            } else {
                2
            };
            found[class] += 1;
            classes.push(['a', 'b', 'c'][class]);
            if class == 1 {
                let n = counts().audit[1].load(Relaxed) + found[1];
                if worth(n) {
                    tracing::warn!(
                        memory = format_args!("{at:#x}"),
                        piece = k,
                        held = ?h,
                        "texture truth: title memory of a texture changed since it was detiled"
                    );
                }
            }
        }
        if let Some(dir) = keep {
            let _ = std::fs::write(dir.join(format!("{at:x}-{len}.bin")), &now);
            index.push_str(&format!("{at:#x} {len} first_resident={first} pages={held:?} audit={classes}\n"));
        }
    }
    if let Some(dir) = keep {
        let _ = std::fs::write(dir.join("index.txt"), index);
    }
    let c = counts();
    for (k, n) in found.iter().enumerate() {
        c.audit[k].fetch_add(*n, Relaxed);
    }
    found
}

/// A capture began in `folder`: its textures' title memory is audited and
/// kept in `title-mem` (200 MB at most). Captures are made only in builds
/// with diagnostics.
#[cfg(feature = "diagnostics")]
pub(crate) fn captured(folder: &std::path::Path) {
    if !on() {
        return;
    }
    let dir = folder.join("title-mem");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let [a, b, c] = audit(30, 200 << 20, Some(&dir));
    tracing::info!(
        folder = %dir.display(),
        "texture truth at the capture: pieces as detiled and let go (a) {a}, changed since detiled (b) {b}, as detiled and resident (c) {c}"
    );
}

/// What shaders wrote is about to go into title memory at `[at, at + len)`
/// (P4).
pub(crate) fn landing(at: usize, len: usize) {
    if !on() {
        return;
    }
    let (started, finished) = crate::host::submissions();
    let (first, end) = (at as u64 / PAGE, (at + len.max(1) - 1) as u64 / PAGE + 1);
    // Remapped by a batch called when the submissions begun were no fewer
    // than all begun now: after whatever lands now had begun (certain).
    // No fewer than those all finished now: after it had begun, unless
    // submissions finish out of order (likely).
    let [certain, likely] = pages(|p| {
        let at = |page: &u64| p.mapped.get(page).map_or(0, |m| m.begun);
        [(first..end).filter(|page| at(page) >= started.max(1)).count(), (first..end).filter(|page| at(page) >= finished.max(1)).count()]
    });
    let c = counts();
    if certain > 0 {
        c.late[2].fetch_add(1, Relaxed);
    }
    if likely > 0 {
        c.late[1].fetch_add(likely as u64, Relaxed);
        let n = c.late[0].fetch_add(1, Relaxed) + 1;
        if worth(n) {
            tracing::warn!(
                at = format_args!("{at:#x}"),
                len,
                pages = likely,
                certain = certain > 0,
                times = n,
                "texture truth: a landing on pages remapped after its submission began"
            );
        }
    }
}

/// The line of every 5 s.
pub(crate) fn status() {
    if !on() {
        return;
    }
    let [a, b, cc] = audit(150, 16 << 20, None);
    let c = counts();
    let l = |a: &AtomicU64| a.load(Relaxed);
    tracing::info!(
        "texture truth: table read {} (scatter vs table: at the address {} same {} not, behind its pointer {} same {} not); \
         pool pages: agree {}, another page {}, the title says free {}, hole {}, a number not ours {}; GPU address after the mapping not the pool's page {}; \
         textures looked at {}, with levels the title let go {} (pages resident {}, free {}, reused {}, hole {}, untracked {}); \
         views made {}, reaching levels the title let go {}, the title's own clamp covered {}, the resident clamp would move {} (clamp on: {}); \
         landings on pages remapped after their submission began: likely {} ({} pages), certain {}; audit now: as detiled and let go {a}, changed {b}, as detiled and resident {cc} (since the start {}, {}, {})",
        if reading() == 1 { "behind its pointer" } else { "at the address" },
        l(&c.checked[0][0]),
        l(&c.checked[0][1]),
        l(&c.checked[1][0]),
        l(&c.checked[1][1]),
        l(&c.pool[0]),
        l(&c.pool[1]),
        l(&c.pool[2]),
        l(&c.pool[3]),
        l(&c.pool[4]),
        l(&c.after),
        l(&c.textures[0]),
        l(&c.textures[1]),
        l(&c.page[0]),
        l(&c.page[1]),
        l(&c.page[2]),
        l(&c.page[3]),
        l(&c.page[4]),
        l(&c.views[0]),
        l(&c.views[1]),
        l(&c.views[2]),
        l(&c.views[3]),
        clamp(),
        l(&c.late[0]),
        l(&c.late[1]),
        l(&c.late[2]),
        l(&c.audit[0]),
        l(&c.audit[1]),
        l(&c.audit[2]),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_kept_level_is_where_every_coarser_one_is_kept() {
        // Levels 0..4 at their own spans, 4 and 5 in the tail block at 0.
        let spans = [(0x40000, 0x80000), (0x20000, 0x40000), (0x10000, 0x20000), (0x10000, 0x10000), (0, 0x10000), (0, 0x10000)];
        assert_eq!(first_kept(&spans, |_, _| true), 0, "all kept");
        assert_eq!(first_kept(&spans, |_, _| false), 0, "none: nothing better");
        assert_eq!(first_kept(&spans, |from, _| from < 0x40000), 1, "level 0 let go");
        assert_eq!(first_kept(&spans, |from, _| from != 0x20000), 2, "a hole at level 1");
        assert_eq!(first_kept(&spans, |from, _| from == 0), 4, "the tail alone");
    }

    #[test]
    fn pieces_follow_the_64k_pages() {
        let bytes = vec![7u8; 0x30000];
        let at = 0x1_0000_8000;
        let p = pieces(at, &bytes);
        assert_eq!(p.len(), 4, "a half page, two whole, a half");
        assert_eq!(p[1], p[2]);
        assert_ne!(p[0], p[1], "lengths differ");
    }

    #[test]
    fn table_entries_name_pages_by_their_frames() {
        assert_eq!(number(FREE), None);
        assert_eq!(number(0x12), Some(0x120));
        assert_eq!(number(0xffe0_0001), Some(0x10), "21 bits");
    }

    #[test]
    fn heap_pages_are_resident_reused_or_unmapped() {
        let (pool, heap) = (0x7_0000_0000u64, 0x9_0000_0000u64);
        // Not followed until a batch maps it.
        assert_eq!(held(heap, PAGE), [Held::Untracked]);
        batch(pool, 0, &[(heap, Some(3), false), (heap + PAGE, Some(4), false), (heap + 2 * PAGE, None, false)]);
        assert_eq!(held(heap, 3 * PAGE), [Held::Resident, Held::Resident, Held::Hole]);
        // Index 3 given to another page: the first shows what is not its own.
        batch(pool, 0, &[(heap + 9 * PAGE, Some(3), false)]);
        assert_eq!(held(heap, 2 * PAGE), [Held::Reused, Held::Resident]);
        // One index at many pages is not reuse.
        batch(pool, 0, &[(heap + 20 * PAGE, Some(7), true), (heap + 21 * PAGE, Some(7), true)]);
        assert_eq!(held(heap + 20 * PAGE, 2 * PAGE), [Held::Resident, Held::Resident]);
    }
}
