//! A decoration bake's result kept on the card beside it, and put back over
//! whatever of ours writes it after the bake (`VICTORY_BAKE_KEEP`; off by
//! default).
//!
//! The title bakes a character's skin, make-up, tattoos and prints once: the
//! bake draw, then compute passes that compress it into a scratch texture of
//! 32-bit integers, then a copy of each level of that scratch into a
//! block-compressed texture of the title's, the result
//! (`PS_DecorationBlend_*`, `CS_CompressBC*`; `crate::bake`). The result's
//! texels exist only in its host copy: title memory under it never gets them
//! (`crate::texture::host_written`), and nothing bakes it again.
//!
//! Measured in the runs of 28 Nov 05:30 with the bake probe of
//! `agente/confete` (`barco-g-cf10`, `cf11`, `cf20`, `cf21`): no result read
//! back at its copy looked like noise (some 27,000 read-backs, most of other
//! textures compressed so), and 30 frames later 11 to 20 results of each run held
//! block noise or bands of other textures in the same host copy
//! (`changed_since_its_copy`), e.g. a shirt print whole at its copy and noise
//! 30 frames on. So what the player sees as a face or a tattoo in coloured
//! noise is not a bake drawn from missing sources: it is a good result
//! written over after the bake, for the rest of the run.
//!
//! What of ours can write a texture the host copied into, by the code: an
//! upload of its title memory recorded into a list before the bake's copy was
//! recorded (so not refused by `host_written`) whose list is submitted after
//! the bake's list (written-down lists are translated by threads of ours, in
//! another order than the title submits them); a first upload on the copy
//! queue of its own claimed before the copy (`crate::texture::ahead`), which
//! no queue orders against the copy; a host copy of the result made again
//! (from title memory). Each of these is named by a line here when it happens
//! to a kept result.
//!
//! So each result is kept: every copy of the title's into it is copied again,
//! in the same list, into a texture of ours of the same description, and the
//! regions written are put back from it 1, 4, 16, 64, 256 and 1,024 frames
//! after the last copy ([`AGAIN`]), and in the next pass whenever one of the
//! writers above is seen at it. A block-compressed texture is written by
//! copies alone (it is never a render target or unordered access), so a
//! later copy of the title's into it is copied into the keeper too, and
//! nothing the title wrote is undone. `VICTORY_BAKE_KEEP=2` also reads back
//! level 0 of the result and of its keeper before each putting back, and says
//! when they differed (the proof in a run, and when the writing came).

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use windows::Win32::Graphics::Direct3D12::*;
use windows::core::Interface;

use crate::host::HostList;
use crate::object::This;

/// 0: off; 1: results kept and put back; 2: and compared before each
/// putting back (`VICTORY_BAKE_KEEP`; off by default).
pub(crate) fn mode() -> u8 {
    static MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *MODE.get_or_init(|| match std::env::var("VICTORY_BAKE_KEEP").as_deref() {
        Ok("1") => 1,
        Ok("2") => 2,
        _ => 0,
    })
}

pub(crate) fn on() -> bool {
    mode() != 0
}

/// Frames after a result's last copy at which its regions are put back.
pub(crate) const AGAIN: [u64; 6] = [1, 4, 16, 64, 256, 1024];

/// The most bytes of keepers at once (a run bakes some hundreds of results of
/// 12 KiB to 1.4 MB, and one atlas of some 12 MB); past it new results are
/// not kept.
const MOST_BYTES: u64 = 512 << 20;

/// Whether DXGI format `format` is block-compressed (BC1 to BC5, BC6H, BC7).
pub(crate) fn block_compressed(format: u32) -> bool {
    matches!(format, 70..=84 | 94..=99)
}

/// Whether a copy from a texture of format `from` into one of format `to` is
/// a bake writing its result: blocks the compute passes wrote as 32-bit
/// integers (two or four to a block) into a block-compressed texture. (Taken
/// only in a list that drew a bake, [`bake_drawn`]: the title compresses
/// other textures so too, three 4224x4224 BC1 ones a few regions a frame.)
pub(crate) fn result_copy(to: u32, from: u32) -> bool {
    block_compressed(to) && matches!(from, 1..=4 | 15..=18)
}

/// The key a list notes when it records a bake draw (`HostList::first_use`,
/// forgotten at its reset): no texture is at this address.
const BAKED: usize = usize::MAX - 0xbace;

/// `list` records a draw of the title's decoration bake.
pub(crate) fn bake_drawn(list: &HostList) {
    if on() {
        list.first_use(BAKED);
    }
}

/// The most regions a result keeps apart: past them, the levels it has are
/// put back whole (an atlas many bakes copy into).
const MOST_REGIONS: usize = 64;

/// `regions` with `region` added, as [`MOST_REGIONS`] allows: past them, one
/// whole level for each subresource met, `level(sub)` its box.
fn add_region(regions: &mut Vec<Region>, region: Region, level: impl Fn(u32) -> [u32; 6]) {
    if regions.contains(&region) {
        return;
    }
    regions.push(region);
    if regions.len() > MOST_REGIONS {
        let mut subs: Vec<u32> = regions.iter().map(|r| r.sub).collect();
        subs.sort_unstable();
        subs.dedup();
        *regions = subs.into_iter().map(|sub| Region { sub, b: level(sub) }).collect();
    }
}

/// The box (left, top, front, right, bottom, back) in the destination's
/// texels that a copy to `at` wrote, in a destination level of `level`
/// texels, of `size` source texels; `scale`: destination texels a source
/// texel is along a side (4 for integers into blocks, 1 for like formats).
/// `None`: nothing of it lies in the level.
pub(crate) fn written(at: [u32; 3], level: [u32; 3], size: [u32; 3], scale: u32) -> Option<[u32; 6]> {
    let end = |i: usize, s: u32| at[i].saturating_add(size[i].saturating_mul(s)).min(level[i]);
    let b = [at[0], at[1], at[2], end(0, scale), end(1, scale), end(2, 1)];
    (b[0] < b[3] && b[1] < b[4] && b[2] < b[5]).then_some(b)
}

/// Of `bytes_a` and `bytes_b`, `rows` rows of `row` bytes `pitch` apart: how
/// many of the 8-byte pieces differ, and how many there are.
pub(crate) fn differing(a: &[u8], b: &[u8], rows: usize, row: usize, pitch: usize) -> (u64, u64) {
    let (mut differ, mut all) = (0, 0);
    for r in 0..rows {
        let (Some(x), Some(y)) = (a.get(r * pitch..r * pitch + row), b.get(r * pitch..r * pitch + row)) else {
            break;
        };
        for (p, q) in x.chunks(8).zip(y.chunks(8)) {
            all += 1;
            differ += u64::from(p != q);
        }
    }
    (differ, all)
}

/// One region a copy of the title's wrote: the subresource and the box.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Region {
    sub: u32,
    b: [u32; 6],
}

/// A kept result.
struct Kept {
    /// The host copy the bake's copies wrote (its address).
    host: usize,
    keeper: ID3D12Resource,
    bytes: u64,
    regions: Vec<Region>,
    /// The frame of the last copy into it, and the list that recorded it.
    frame: u64,
    list: usize,
    /// The submission of that list: 0 until it is submitted.
    submitted: std::sync::Arc<AtomicU64>,
    /// [`AGAIN`] done, and the frame of the next putting back.
    again: usize,
    due: u64,
    /// The number of this result, and the texture's description for the lines.
    n: u64,
    what: String,
}

static KEPT: Mutex<Option<HashMap<usize, Kept>>> = Mutex::new(None);
/// Results kept now (so that the checks of every upload cost an atomic read
/// when there are none).
static ALIVE: AtomicU64 = AtomicU64::new(0);
/// Results kept since the start; the bytes of the keepers alive; since the
/// start: regions put back, results found written over by the comparison,
/// writers seen at a result (an upload recorded before the bake, the copy
/// queue), a spare, host copies made again, results not kept for the bytes.
static COUNTS: [AtomicU64; 9] = [const { AtomicU64::new(0) }; 9];

/// A kept result is let go: its keeper with the next list that has run
/// (the lists that name it hold it).
fn let_go(map: &mut HashMap<usize, Kept>, title: usize) {
    if let Some(k) = map.remove(&title) {
        ALIVE.fetch_sub(1, Relaxed);
        COUNTS[1].fetch_sub(k.bytes, Relaxed);
    }
}

fn kept<T>(f: impl FnOnce(&mut HashMap<usize, Kept>) -> T) -> T {
    let mut map = KEPT.lock().unwrap_or_else(|e| e.into_inner());
    f(map.get_or_insert_with(HashMap::new))
}

fn text(d: &D3D12_RESOURCE_DESC) -> String {
    format!("{}x{} f{} levels {}", d.Width, d.Height, d.Format.0, d.MipLevels)
}

/// The texels of subresource `sub` of a texture of description `d`.
fn level_size(d: &D3D12_RESOURCE_DESC, sub: u32) -> [u32; 3] {
    let levels = u32::from(d.MipLevels.max(1));
    let level = sub % levels;
    let volume = d.Dimension == D3D12_RESOURCE_DIMENSION_TEXTURE3D;
    let depth = if volume { (u32::from(d.DepthOrArraySize) >> level).max(1) } else { 1 };
    [((d.Width >> level) as u32).max(1), (d.Height >> level).max(1), depth]
}

fn barrier(list: &HostList, resource: &ID3D12Resource, before: D3D12_RESOURCE_STATES, after: D3D12_RESOURCE_STATES) {
    let b = crate::host::transition(resource, before, after);
    // SAFETY: an open list and a live resource in the `before` state.
    unsafe { list.list.ResourceBarrier(&[b]) };
}

fn copy_box(list: &HostList, to: &ID3D12Resource, from: &ID3D12Resource, r: Region) {
    let location = |resource: &ID3D12Resource| D3D12_TEXTURE_COPY_LOCATION {
        // SAFETY: a borrowed pointer the location never releases.
        pResource: unsafe { std::mem::transmute_copy(resource) },
        Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
        Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { SubresourceIndex: r.sub },
    };
    let b = D3D12_BOX { left: r.b[0], top: r.b[1], front: r.b[2], right: r.b[3], bottom: r.b[4], back: r.b[5] };
    // SAFETY: an open list; both textures live, of one description, in the
    // copy states; the box lies in the subresource.
    unsafe { list.list.CopyTextureRegion(&location(to), r.b[0], r.b[1], r.b[2], &location(from), Some(&b)) };
}

fn keeper(d: &D3D12_RESOURCE_DESC) -> Option<ID3D12Resource> {
    let host = crate::host::host()?;
    let heap = D3D12_HEAP_PROPERTIES { Type: D3D12_HEAP_TYPE_DEFAULT, ..Default::default() };
    let desc = D3D12_RESOURCE_DESC { Alignment: 0, Flags: D3D12_RESOURCE_FLAG_NONE, ..*d };
    let mut out: Option<ID3D12Resource> = None;
    // SAFETY: plain creation of a texture of ours.
    unsafe {
        host.device.CreateCommittedResource(
            &heap,
            D3D12_HEAP_FLAG_NONE,
            &desc,
            D3D12_RESOURCE_STATE_COMMON,
            None,
            &mut out,
        )
    }
    .ok()?;
    out
}

fn bytes_of(d: &D3D12_RESOURCE_DESC) -> u64 {
    let Some(host) = crate::host::host() else {
        return 0;
    };
    let layers = if d.Dimension == D3D12_RESOURCE_DIMENSION_TEXTURE3D { 1 } else { u32::from(d.DepthOrArraySize) };
    let mut total = 0u64;
    // SAFETY: a plain query.
    unsafe {
        host.device.GetCopyableFootprints(d, 0, u32::from(d.MipLevels) * layers, 0, None, None, None, Some(&mut total))
    };
    if total == u64::MAX { 0 } else { total }
}

/// A copy of the title's was recorded into `list`: into subresource `sub` of
/// title texture `title`, whose host copy is `to`, at `at`; from a texture of
/// description `from` (its subresource `from_sub`), or from a buffer (`None`),
/// with the box `given` in the source's texels or all of it. A bake's result
/// is kept from its first copy on; a kept one gets every later copy too.
#[allow(clippy::too_many_arguments)] // the copy's own arguments
pub(crate) fn copied(
    list: &HostList,
    title: This,
    to: &ID3D12Resource,
    sub: u32,
    at: [u32; 3],
    from: Option<&D3D12_RESOURCE_DESC>,
    from_sub: u32,
    given: Option<[u32; 6]>,
) {
    if !on() {
        return;
    }
    let baked = list.used(BAKED);
    keep_copy(list, title, to, sub, at, from, from_sub, given, baked);
}

/// [`copied`], `baked` if the list drew a bake.
#[allow(clippy::too_many_arguments)] // the copy's own arguments
fn keep_copy(
    list: &HostList,
    title: This,
    to: &ID3D12Resource,
    sub: u32,
    at: [u32; 3],
    from: Option<&D3D12_RESOURCE_DESC>,
    from_sub: u32,
    given: Option<[u32; 6]>,
    baked: bool,
) {
    // SAFETY: a live resource.
    let d = unsafe { to.GetDesc() };
    let level = level_size(&d, sub);
    let size = match (given, from) {
        (Some(g), _) => [g[3].saturating_sub(g[0]), g[4].saturating_sub(g[1]), g[5].saturating_sub(g[2])],
        (None, Some(f)) => level_size(f, from_sub),
        // From a buffer: its footprint is the level's (copies of a whole level).
        (None, None) => level,
    };
    let scale = match from {
        Some(f) if block_compressed(d.Format.0 as u32) && !block_compressed(f.Format.0 as u32) => 4,
        _ => 1,
    };
    let Some(b) = written(at, level, size, scale) else {
        return;
    };
    let region = Region { sub, b };
    let result = baked && from.is_some_and(|f| result_copy(d.Format.0 as u32, f.Format.0 as u32));
    let frame = crate::frame::current();
    let raw = to.as_raw() as usize;
    let keeper = kept(|map| {
        // A host copy made again since: kept from this copy on.
        if map.get(&(title as usize)).is_some_and(|k| k.host != raw) {
            let_go(map, title as usize);
        }
        let k = match map.entry(title as usize) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(e) => {
                if !result {
                    return None;
                }
                let bytes = bytes_of(&d);
                if COUNTS[1].load(Relaxed).saturating_add(bytes) > MOST_BYTES {
                    if COUNTS[8].fetch_add(1, Relaxed) == 0 {
                        tracing::warn!(
                            most_mb = MOST_BYTES >> 20,
                            "a decoration bake's result is not kept: the keepers have the most bytes they may (VICTORY_BAKE_KEEP)"
                        );
                    }
                    return None;
                }
                let keeper = keeper(&d)?;
                let n = COUNTS[0].fetch_add(1, Relaxed) + 1;
                COUNTS[1].fetch_add(bytes, Relaxed);
                ALIVE.fetch_add(1, Relaxed);
                if n <= 16 || n.is_power_of_two() {
                    tracing::info!(
                        result = format_args!("{:#x}", title as usize),
                        texture = text(&d),
                        bytes,
                        kept_so = n,
                        keepers_mb = COUNTS[1].load(Relaxed) >> 20,
                        "a decoration bake's result is kept beside it, to be put back over what of ours writes it later (VICTORY_BAKE_KEEP)"
                    );
                }
                e.insert(Kept {
                    host: raw,
                    keeper,
                    bytes,
                    regions: Vec::new(),
                    frame,
                    list: 0,
                    submitted: std::sync::Arc::new(AtomicU64::new(0)),
                    again: 0,
                    due: 0,
                    n,
                    what: text(&d),
                })
            }
        };
        add_region(&mut k.regions, region, |sub| {
            let [w, h, depth] = level_size(&d, sub);
            [0, 0, 0, w, h, depth]
        });
        k.frame = frame;
        k.list = std::ptr::from_ref(list) as usize;
        k.again = 0;
        k.due = frame + AGAIN[0];
        // The putting back waits for this list's submission.
        k.submitted = std::sync::Arc::new(AtomicU64::new(0));
        let submitted = k.submitted.clone();
        list.defer_host(move || submitted.store(crate::host::submissions().0.max(1), Relaxed));
        Some(k.keeper.clone())
    });
    let Some(keeper) = keeper else {
        return;
    };
    // The title's copy is in the list: what it wrote goes into the keeper.
    crate::list::ensure_state(list, title, 0x800);
    barrier(list, &keeper, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_COPY_DEST);
    copy_box(list, &keeper, to, region);
    barrier(list, &keeper, D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_COMMON);
    list.keep(&keeper);
}

/// A copy of the title's of a whole texture (`CopyResource`) into `title`,
/// whose host copy is `to`: every level of a kept result goes into its keeper.
pub(crate) fn copied_whole(list: &HostList, title: This, to: &ID3D12Resource) {
    if !on() || ALIVE.load(Relaxed) == 0 || !kept(|map| map.contains_key(&(title as usize))) {
        return;
    }
    // SAFETY: a live resource.
    let d = unsafe { to.GetDesc() };
    let layers = if d.Dimension == D3D12_RESOURCE_DIMENSION_TEXTURE3D { 1 } else { u32::from(d.DepthOrArraySize) };
    for sub in 0..u32::from(d.MipLevels) * layers {
        copied(list, title, to, sub, [0; 3], Some(&d), sub, None);
    }
}

/// Says that `writer` wrote over kept result `title` (or the one whose host
/// copy is `host`) after its bake, and puts it back at the next pass.
fn written_over(title: Option<usize>, host: Option<usize>, list: Option<usize>, writer: &'static str, count: usize) {
    let said = kept(|map| {
        let key = match (title, host) {
            (Some(t), _) => t,
            (None, Some(h)) => *map.iter().find(|(_, k)| k.host == h)?.0,
            _ => return None,
        };
        let k = map.get_mut(&key)?;
        let submitted = k.submitted.load(Relaxed);
        // Before the bake's list was submitted, or by that very list (an
        // upload it recorded before the copy runs before it): no harm.
        if submitted == 0 || list == Some(k.list) {
            return None;
        }
        k.due = 0;
        Some((key, k.n, k.what.clone(), crate::frame::current().saturating_sub(k.frame)))
    });
    let Some((title, n, what, frames)) = said else {
        return;
    };
    let seen = COUNTS[count].fetch_add(1, Relaxed) + 1;
    if seen <= 64 || seen.is_power_of_two() {
        tracing::warn!(
            result = format_args!("{title:#x}"),
            kept = n,
            texture = what,
            frames_after_its_bake = frames,
            writer,
            so_far = seen,
            "a decoration bake's result is written over after its bake by something of ours; put back at the next pass (VICTORY_BAKE_KEEP)"
        );
    }
}

/// An upload of title texture `title`'s memory recorded into list `list`
/// runs now (its submission): over the result of a bake whose copy was in a
/// list submitted before it, the bake is lost.
pub(crate) fn upload_runs(title: usize, list: usize) {
    if ALIVE.load(Relaxed) != 0 {
        written_over(
            Some(title),
            None,
            Some(list),
            "an upload of its title memory, recorded before the bake's copy, submitted after it",
            4,
        );
    }
}

/// The copy queue of its own (`crate::texture::ahead`) ran an upload into
/// host texture `host`.
pub(crate) fn ahead_ran(host: usize) {
    if ALIVE.load(Relaxed) != 0 {
        written_over(
            None,
            Some(host),
            None,
            "a first upload on the copy queue of its own (VICTORY_UPLOAD_AHEAD), claimed before the bake's copy",
            5,
        );
    }
}

/// The title released texture `title`: it is no result any more.
pub(crate) fn forget(title: usize) {
    if ALIVE.load(Relaxed) == 0 {
        return;
    }
    kept(|map| let_go(map, title));
}

/// A comparison of a result with its keeper on its way (`VICTORY_BAKE_KEEP=2`).
struct Compare {
    n: u64,
    what: String,
    frames: u64,
    again: usize,
    result: ID3D12Resource,
    keeper: ID3D12Resource,
    rows: (usize, usize, usize),
    submission: std::sync::Arc<AtomicU64>,
    asked: u64,
}

static COMPARES: Mutex<Vec<Compare>> = Mutex::new(Vec::new());

/// Records the read-back of level 0 of `result` and of `keeper` (one
/// description) into `list`: both in the copy source state.
fn read_both(
    list: &HostList,
    result: &ID3D12Resource,
    keeper: &ID3D12Resource,
) -> Option<(ID3D12Resource, ID3D12Resource, (usize, usize, usize))> {
    let host = crate::host::host()?;
    // SAFETY: a live resource.
    let d = unsafe { result.GetDesc() };
    let mut footprint = D3D12_PLACED_SUBRESOURCE_FOOTPRINT::default();
    let (mut rows, mut row, mut total) = (0u32, 0u64, 0u64);
    // SAFETY: outputs for one subresource.
    unsafe {
        host.device.GetCopyableFootprints(
            &d,
            0,
            1,
            0,
            Some(&mut footprint),
            Some(&mut rows),
            Some(&mut row),
            Some(&mut total),
        )
    };
    if total == 0 || total == u64::MAX {
        return None;
    }
    let a = host.read_back_buffer(total)?;
    let b = host.read_back_buffer(total)?;
    for (buffer, texture) in [(&a, result), (&b, keeper)] {
        let to = D3D12_TEXTURE_COPY_LOCATION {
            // SAFETY: a borrowed pointer the location never releases.
            pResource: unsafe { std::mem::transmute_copy(buffer) },
            Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
            Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                PlacedFootprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT { Offset: 0, ..footprint },
            },
        };
        let from = D3D12_TEXTURE_COPY_LOCATION {
            // SAFETY: as above.
            pResource: unsafe { std::mem::transmute_copy(texture) },
            Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
            Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { SubresourceIndex: 0 },
        };
        // SAFETY: an open list; both live; the buffer holds the subresource.
        unsafe { list.list.CopyTextureRegion(&to, 0, 0, 0, &from, None) };
        list.keep(buffer);
    }
    Some((a, b, (row as usize, rows as usize, footprint.Footprint.RowPitch as usize)))
}

/// The comparisons whose lists have run.
fn compared() {
    let finished = crate::host::submissions().1;
    let now = crate::host::now_ms();
    let done: Vec<Compare> = {
        let mut all = COMPARES.lock().unwrap_or_else(|e| e.into_inner());
        let (done, wait): (Vec<_>, Vec<_>) = all.drain(..).partition(|c| {
            let s = c.submission.load(Relaxed);
            (s != 0 && s <= finished) || now > c.asked + 60_000
        });
        *all = wait;
        done
    };
    for c in done {
        if c.submission.load(Relaxed) == 0 {
            continue;
        }
        let (row, rows, pitch) = c.rows;
        let len = pitch * rows.saturating_sub(1) + row;
        let read = |buffer: &ID3D12Resource| -> Option<Vec<u8>> {
            let mut data = std::ptr::null_mut();
            let range = D3D12_RANGE { Begin: 0, End: len };
            // SAFETY: a read-back buffer of at least `len` bytes whose copy has run.
            unsafe {
                buffer.Map(0, Some(&range), Some(&mut data)).ok()?;
                let bytes = std::slice::from_raw_parts(data.cast::<u8>(), len).to_vec();
                buffer.Unmap(0, None);
                Some(bytes)
            }
        };
        let (Some(a), Some(b)) = (read(&c.result), read(&c.keeper)) else {
            continue;
        };
        let (differ, all) = differing(&a, &b, rows, row, pitch);
        if differ == 0 {
            continue;
        }
        let n = COUNTS[3].fetch_add(1, Relaxed) + 1;
        if n <= 64 || n.is_power_of_two() {
            tracing::warn!(
                kept = c.n,
                texture = c.what,
                frames_after_its_bake = c.frames,
                putting_back = c.again + 1,
                differing_pct = differ * 100 / all.max(1),
                found_so = n,
                "a decoration bake's result held other texels than its bake made: put back (VICTORY_BAKE_KEEP=2)"
            );
        }
    }
}

/// At a texture pass of `list`: the kept results due are put back (and with
/// `VICTORY_BAKE_KEEP=2` compared first), in this list, ahead of its draws.
pub(crate) fn tick(list: &HostList) {
    if on() {
        put_back(list, mode());
    }
}

/// [`tick`] in `mode` (1 or 2).
fn put_back(list: &HostList, mode: u8) {
    if mode == 2 {
        compared();
    }
    if ALIVE.load(Relaxed) == 0 {
        return;
    }
    let frame = crate::frame::current();
    let mut put = 0u64;
    let mut gone = Vec::new();
    kept(|map| {
        for (&title, k) in map.iter_mut() {
            if k.due > frame || k.submitted.load(Relaxed) == 0 {
                continue;
            }
            let Some(copy) = crate::resource::made_copy(title as This) else {
                gone.push(title);
                continue;
            };
            let to = &copy.texture;
            if to.as_raw() as usize != k.host {
                // Its host copy was made again (from title memory, which the
                // bake never reached): put back into it if it is alike.
                let made = COUNTS[7].fetch_add(1, Relaxed) + 1;
                // SAFETY: live resources.
                let (now, was) = unsafe { (to.GetDesc(), k.keeper.GetDesc()) };
                let alike = (now.Width, now.Height, now.MipLevels, now.Format, now.DepthOrArraySize)
                    == (was.Width, was.Height, was.MipLevels, was.Format, was.DepthOrArraySize);
                if made <= 64 || made.is_power_of_two() {
                    tracing::warn!(
                        result = format_args!("{title:#x}"),
                        kept = k.n,
                        texture = k.what,
                        frames_after_its_bake = frame.saturating_sub(k.frame),
                        alike,
                        so_far = made,
                        "a decoration bake's result got another host copy after its bake (VICTORY_BAKE_KEEP): {}",
                        if alike { "put back into it" } else { "not alike: let go" }
                    );
                }
                if !alike {
                    gone.push(title);
                    continue;
                }
                k.host = to.as_raw() as usize;
            }
            if mode == 2 {
                crate::list::ensure_state(list, title as This, 0x800);
                barrier(list, &k.keeper, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_COPY_SOURCE);
                if let Some((a, b, rows)) = read_both(list, to, &k.keeper) {
                    let submission = std::sync::Arc::new(AtomicU64::new(0));
                    let s = submission.clone();
                    list.defer_host(move || s.store(crate::host::submissions().0.max(1), Relaxed));
                    COMPARES.lock().unwrap_or_else(|e| e.into_inner()).push(Compare {
                        n: k.n,
                        what: k.what.clone(),
                        frames: frame.saturating_sub(k.frame),
                        again: k.again,
                        result: a,
                        keeper: b,
                        rows,
                        submission,
                        asked: crate::host::now_ms(),
                    });
                }
                barrier(list, &k.keeper, D3D12_RESOURCE_STATE_COPY_SOURCE, D3D12_RESOURCE_STATE_COMMON);
            }
            crate::list::ensure_state(list, title as This, 0x400);
            barrier(list, &k.keeper, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_COPY_SOURCE);
            for &r in &k.regions {
                copy_box(list, to, &k.keeper, r);
                put += 1;
            }
            barrier(list, &k.keeper, D3D12_RESOURCE_STATE_COPY_SOURCE, D3D12_RESOURCE_STATE_COMMON);
            list.keep(&k.keeper);
            list.keep(to);
            // The next one: the next of `AGAIN` after the last copy, or, after
            // a writer was seen (`due` 0), the one it had come to.
            if k.due != 0 || k.again == 0 {
                k.again += 1;
            }
            k.due = AGAIN.get(k.again).map_or(u64::MAX, |&f| k.frame + f).max(frame + 1);
        }
        for title in &gone {
            let_go(map, *title);
        }
    });
    if put > 0 {
        let all = COUNTS[2].fetch_add(put, Relaxed) + put;
        static SAID: AtomicU64 = AtomicU64::new(0);
        let now = crate::host::now_ms();
        let last = SAID.load(Relaxed);
        if now >= last + 5000 && SAID.compare_exchange(last, now, Relaxed, Relaxed).is_ok() {
            tracing::info!(
                kept_so = COUNTS[0].load(Relaxed),
                alive = ALIVE.load(Relaxed),
                keepers_mb = COUNTS[1].load(Relaxed) >> 20,
                regions_put_back_so = all,
                found_written_over_so = COUNTS[3].load(Relaxed),
                by_an_upload_so = COUNTS[4].load(Relaxed),
                by_the_copy_queue_so = COUNTS[5].load(Relaxed),
                host_copies_made_again_so = COUNTS[7].load(Relaxed),
                "decoration bakes' results kept and put back (VICTORY_BAKE_KEEP)"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bake_copy_is_integers_into_blocks() {
        // R32G32B32A32_UINT into BC3_UNORM_SRGB, R32G32_UINT into BC1, BC7.
        assert!(result_copy(78, 3));
        assert!(result_copy(71, 17));
        assert!(result_copy(99, 3));
        // A texture copied into another of its kind, or integers into
        // integers, or blocks into an uncompressed texture: not a bake.
        assert!(!result_copy(78, 78));
        assert!(!result_copy(3, 3));
        assert!(!result_copy(28, 3));
    }

    #[test]
    fn what_a_copy_wrote_is_in_destination_texels() {
        // A whole 32x32 scratch of blocks into level 0 of a 128x128 result.
        assert_eq!(written([0; 3], [128, 128, 1], [32, 32, 1], 4), Some([0, 0, 0, 128, 128, 1]));
        // Into a corner of a 2048x2048 atlas.
        assert_eq!(written([256, 512, 0], [2048, 2048, 1], [64, 64, 1], 4), Some([256, 512, 0, 512, 768, 1]));
        // The last levels of a chain: a block of 4x4 into a 2x2 or 1x1 level.
        assert_eq!(written([0; 3], [2, 2, 1], [1, 1, 1], 4), Some([0, 0, 0, 2, 2, 1]));
        assert_eq!(written([0; 3], [1, 1, 1], [1, 1, 1], 4), Some([0, 0, 0, 1, 1, 1]));
        // Like formats, cut to the level.
        assert_eq!(written([96, 0, 0], [128, 128, 1], [64, 16, 1], 1), Some([96, 0, 0, 128, 16, 1]));
        // Nothing of it in the level.
        assert_eq!(written([128, 0, 0], [128, 128, 1], [4, 4, 1], 4), None);
        assert_eq!(written([0; 3], [128, 128, 1], [0, 4, 1], 4), None);
    }

    #[test]
    fn differing_pieces_are_counted_by_rows() {
        // Two rows of 16 bytes, 32 apart; the padding between differs and is
        // not counted.
        let mut a = vec![0u8; 48];
        let mut b = vec![0u8; 48];
        a[20] = 1;
        assert_eq!(differing(&a, &b, 2, 16, 32), (0, 4));
        b[40] = 7;
        assert_eq!(differing(&a, &b, 2, 16, 32), (1, 4));
        // A short buffer stops the count.
        assert_eq!(differing(&a[..40], &b, 2, 16, 32), (0, 2));
    }

    #[test]
    fn the_puttings_back_come_later_and_later() {
        assert!(AGAIN.windows(2).all(|w| w[0] < w[1]));
        // The probe's look at 30 frames falls between two of them.
        assert!(AGAIN.iter().any(|&f| f < 30) && AGAIN.iter().any(|&f| f > 30));
    }

    #[test]
    fn regions_past_the_most_become_whole_levels() {
        let level = |sub: u32| [0, 0, 0, 2048 >> sub, 2048 >> sub, 1];
        let mut regions = Vec::new();
        let corner = |i: u32, sub: u32| Region { sub, b: [i * 4, 0, 0, i * 4 + 4, 4, 1] };
        add_region(&mut regions, corner(0, 0), level);
        // The same region twice is one.
        add_region(&mut regions, corner(0, 0), level);
        assert_eq!(regions.len(), 1);
        for i in 1..MOST_REGIONS as u32 {
            add_region(&mut regions, corner(i, i % 2), level);
        }
        assert_eq!(regions.len(), MOST_REGIONS);
        // One more: levels 0 and 1 whole, and the new one's level 2 whole.
        add_region(&mut regions, corner(0, 2), level);
        assert_eq!(
            regions,
            [
                Region { sub: 0, b: [0, 0, 0, 2048, 2048, 1] },
                Region { sub: 1, b: [0, 0, 0, 1024, 1024, 1] },
                Region { sub: 2, b: [0, 0, 0, 512, 512, 1] },
            ]
        );
    }

    /// Writes `fill` into every byte of level 0 of `texture` in `list` (as a
    /// bake's copy, or an upload over it, would).
    fn write_level0(list: &HostList, title: This, texture: &ID3D12Resource, fill: u8) {
        let host = crate::host::host().unwrap();
        // SAFETY: a live resource.
        let d = unsafe { texture.GetDesc() };
        let mut footprint = D3D12_PLACED_SUBRESOURCE_FOOTPRINT::default();
        let mut total = 0u64;
        // SAFETY: outputs for one subresource.
        unsafe { host.device.GetCopyableFootprints(&d, 0, 1, 0, Some(&mut footprint), None, None, Some(&mut total)) };
        let (buffer, data) = host.upload_buffer(total).unwrap();
        // SAFETY: a mapped buffer of `total` bytes.
        unsafe { std::ptr::write_bytes(data, fill, total as usize) };
        crate::list::ensure_state(list, title, 0x400);
        let to = D3D12_TEXTURE_COPY_LOCATION {
            // SAFETY: a borrowed pointer the location never releases.
            pResource: unsafe { std::mem::transmute_copy(texture) },
            Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
            Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { SubresourceIndex: 0 },
        };
        let from = D3D12_TEXTURE_COPY_LOCATION {
            // SAFETY: as above.
            pResource: unsafe { std::mem::transmute_copy(&buffer) },
            Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
            Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { PlacedFootprint: footprint },
        };
        // SAFETY: an open list; both live; the buffer holds the level.
        unsafe { list.list.CopyTextureRegion(&to, 0, 0, 0, &from, None) };
        list.keep(&buffer);
    }

    /// Level 0 of `texture` as the card has it (its rows, as the footprint lays them).
    fn read_level0(title: This, texture: &ID3D12Resource) -> Vec<u8> {
        let host = crate::host::host().unwrap();
        let list = host.new_list().unwrap();
        list.close();
        list.reset();
        crate::list::ensure_state(&list, title, 0x800);
        let keeper = texture.clone();
        let (a, _, (row, rows, pitch)) = read_both(&list, texture, &keeper).unwrap();
        list.close();
        host.execute(&[&list]);
        let mut data = std::ptr::null_mut();
        // SAFETY: a read-back buffer whose copy has run.
        unsafe {
            a.Map(0, None, Some(&mut data)).unwrap();
            let all = std::slice::from_raw_parts(data.cast::<u8>(), pitch * (rows - 1) + row);
            let bytes = (0..rows).flat_map(|r| all[r * pitch..r * pitch + row].iter().copied()).collect();
            a.Unmap(0, None);
            bytes
        }
    }

    /// A bake's result copied in a list that drew a bake, written over by a
    /// later list, is put back at the next pass after it is due, and the
    /// comparison before it says so; a copy of the title's into it later is
    /// kept too, and nothing is put back over it.
    #[test]
    fn a_result_written_over_after_its_bake_is_put_back() {
        use windows_sys::Win32::System::Memory::{MEM_COMMIT, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc};
        let Some(host) = crate::host::host() else { return };
        // 64x64 BC1, one level, placed in memory of ours.
        let desc = [3u32, 0, 0, 0, 64, 0, 64, 0x1_0001, 71, 1, 0, 0x105, 0, 0];
        let whole = crate::resource::placed_bytes(&desc).unwrap() as usize;
        // SAFETY: memory of our own that stays.
        let at = unsafe { VirtualAlloc(std::ptr::null(), whole, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE) }.cast::<u8>();
        let title = crate::resource::texture_at(at as u64, &desc).unwrap();
        let copy = crate::resource::host_copy(title).unwrap();
        let texture = copy.texture.clone();
        // The scratch the bake's blocks come from: 16x16 R32G32_UINT.
        let scratch = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
            Width: 16,
            Height: 16,
            DepthOrArraySize: 1,
            MipLevels: 1,
            Format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT(17),
            SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            ..Default::default()
        };
        let run = |record: &dyn Fn(&HostList)| {
            let list = host.new_list().unwrap();
            list.close();
            list.reset();
            record(&list);
            list.close();
            host.execute(&[&list]);
        };
        let kept_before = COUNTS[0].load(Relaxed);
        // The bake: its blocks (0x5a) copied in, in a list that drew a bake.
        run(&|list| {
            write_level0(list, title, &texture, 0x5a);
            keep_copy(list, title, &texture, 0, [0; 3], Some(&scratch), 0, None, true);
        });
        assert_eq!(COUNTS[0].load(Relaxed), kept_before + 1, "kept");
        // Something of ours writes over it.
        run(&|list| write_level0(list, title, &texture, 0xc3));
        assert!(read_level0(title, &texture).iter().all(|&b| b == 0xc3));
        // Not due yet in this frame: nothing put back.
        run(&|list| put_back(list, 1));
        assert!(read_level0(title, &texture).iter().all(|&b| b == 0xc3));
        // Due a frame on: put back, and compared first.
        crate::frame::test_advance();
        let found = COUNTS[3].load(Relaxed);
        run(&|list| put_back(list, 2));
        assert!(read_level0(title, &texture).iter().all(|&b| b == 0x5a), "put back");
        compared();
        assert!(COUNTS[3].load(Relaxed) > found, "the comparison saw it");
        // A later copy of the title's into it is the title's to keep.
        run(&|list| {
            write_level0(list, title, &texture, 0x77);
            keep_copy(list, title, &texture, 0, [0; 3], Some(&scratch), 0, None, false);
        });
        for _ in 0..4 {
            crate::frame::test_advance();
        }
        run(&|list| put_back(list, 1));
        assert!(read_level0(title, &texture).iter().all(|&b| b == 0x77), "the title's later copy stays");
        forget(title as usize);
        assert!(!kept(|map| map.contains_key(&(title as usize))));
    }
}
