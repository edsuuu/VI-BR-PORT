//! A diagnostic of the title's decoration bake (`VICTORY_BAKE_PROBE=1`; off by
//! default): one run that says which texture a character's skin, tattoos or
//! clothes came out wrong from, and why.
//!
//! The bake (`crate::bake`) draws a character's decorations into the corner of
//! 2048x2048 targets with a pixel program that picks its sources by index;
//! compute programs then dilate the corner, make its levels and compress them
//! block by block into a 32x32 R32G32_UINT or R32G32B32A32_UINT scratch
//! texture, which is copied into each level of a block-compressed texture
//! (BC1, BC3 or BC7; 128x128 in the prison): the result the character's
//! G-buffer draws sample, by index too. Some characters came out in coloured
//! 4x4 blocks, or in one flat purple or green, for the whole run, and in other
//! launches not (a guard's forearm, an inmate's tattooed arm, the counselor's
//! face). Any step of that chain can do it, so each is looked at:
//!
//! 1. At each bake draw: every slot the dwords of what the draw binds name
//!    that holds a texture view (any size, placed or not: more than the small
//!    placed ones `crate::bake` takes), what its host slot shows (the view's
//!    own host copy, another texture, the neutral grey, nothing), how much of
//!    its title memory is mapped, and which DirectStorage requests wrote that
//!    memory last and how (raw, zlib, BCPack of which BC format, failed).
//!    When the bake's list is submitted: whether the host copy holds what title
//!    memory holds then, and what those bytes look like (all-zero blocks, BC7
//!    blocks without a mode, blocks whose colour jumps from the one before:
//!    noise). [`LATER`] frames on, the memory again: other bytes there under
//!    the same view mean the bake drew from bytes that were not yet the
//!    texture's (still streaming in, or written where nothing told of it),
//!    and what wrote them after the bake, if a DirectStorage request or a
//!    landing of what the card wrote into a title buffer did.
//! 2. After the draw, the rectangle of its first target is read back: what the
//!    bake drew (noise there and every source clean: a source not found).
//! 3. Dispatches of the bake's list that did not run after it: the compression
//!    not run leaves the scratch texture as it was.
//! 4. At the copy into the result's level 0 the level is read back, and again
//!    [`LATER`] frames on, with the slots whose placed views lie in the
//!    result's memory and what they show: whether the result kept what the
//!    bake made, and whether what a character samples there is that texture
//!    or another one filled from title memory, which the copy never reached.
//!
//! `VICTORY_BAKE_PROBE_DUMP=<folder>`: also a table of every source of every
//! bake (`bake-probe.csv`), the targets' rectangles as pictures
//! (`bake-<n>-target.bmp`) and the results' raw blocks
//! (`bake-<n>-result-<when>-<w>x<h>-f<format>-s0.bin`, which
//! `scripts/bc-png.mjs` turns into pictures).
//!
//! What it costs: a lock per DirectStorage request; per bake draw a look at a
//! few dozen slots, a digest of each source's memory when the list is
//! submitted (some MB), and two or three read-backs of 16-256 KB. At most
//! [`MOST`] bakes are looked at. Nothing here writes title memory or changes
//! what is drawn: the read-backs are copies out of the textures, with the
//! state changes copies need.

use std::collections::{BTreeMap, HashMap};
use std::io::Write as _;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, OnceLock};

use windows::Win32::Graphics::Direct3D12::*;
use windows::core::Interface;

use crate::descriptor::View;
use crate::host::HostList;
use crate::object::This;

/// Whether the probe is on (`VICTORY_BAKE_PROBE=1`; off by default).
pub fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_BAKE_PROBE").is_some_and(|v| v == "1"))
}

/// Where pictures and the table go (`VICTORY_BAKE_PROBE_DUMP`), if anywhere.
fn dump_dir() -> Option<&'static std::path::Path> {
    static DIR: OnceLock<Option<std::path::PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = std::path::PathBuf::from(std::env::var_os("VICTORY_BAKE_PROBE_DUMP")?);
        std::fs::create_dir_all(&dir).ok()?;
        Some(dir)
    })
    .as_deref()
}

/// The most bake draws looked at in a run.
const MOST: u64 = 4096;
/// Frames after its copy that a bake's result is looked at again.
const LATER: u64 = 30;
/// The most bytes of one source's memory read for its digest and its look.
const READ_MOST: usize = 8 << 20;
/// A colour step between two neighbours (sum over red, green and blue) that
/// counts as a jump: a texture's neighbouring blocks or pixels rarely differ
/// so much, random blocks mostly do.
const JUMP: u32 = 96;

// What bytes look like.

/// How a block of a block-compressed DXGI format is read here.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Bc1,
    /// BC2 and BC3: the colour in the second half.
    Bc3,
    Bc4,
    Bc5,
    Bc6,
    Bc7,
}

fn kind(format: u32) -> Option<Kind> {
    Some(match format {
        70..=72 => Kind::Bc1,
        73..=78 => Kind::Bc3,
        79..=81 => Kind::Bc4,
        82..=84 => Kind::Bc5,
        94..=96 => Kind::Bc6,
        97..=99 => Kind::Bc7,
        _ => return None,
    })
}

impl Kind {
    fn bytes(self) -> usize {
        match self {
            Kind::Bc1 | Kind::Bc4 => 8,
            _ => 16,
        }
    }
}

fn rgb565(c: u16) -> [u8; 3] {
    let (r, g, b) = ((c >> 11) & 31, (c >> 5) & 63, c & 31);
    [(r << 3 | r >> 2) as u8, (g << 2 | g >> 4) as u8, (b << 3 | b >> 2) as u8]
}

fn mid(a: u8, b: u8) -> u8 {
    ((u16::from(a) + u16::from(b)) / 2) as u8
}

fn average(a: [u8; 3], b: [u8; 3]) -> [u8; 3] {
    std::array::from_fn(|i| mid(a[i], b[i]))
}

fn bits(v: u128, from: u32, n: u32) -> u32 {
    ((v >> from) & ((1u128 << n) - 1)) as u32
}

/// The mean of a BC7 block's two endpoints of one subset: red, green and
/// blue of `n` bits each, endpoint 0 then 1 per channel, from bit `at`; with
/// `p`, the bits of the endpoints' p-bits below them.
fn bc7_ends(v: u128, at: u32, n: u32, p: Option<(u32, u32)>) -> [u8; 3] {
    let widen = |e: u32| match p {
        Some(_) => e,
        None => e << (8 - n) | e >> (2 * n - 8),
    };
    std::array::from_fn(|c| {
        let c = c as u32;
        let (mut e0, mut e1) = (bits(v, at + 2 * c * n, n), bits(v, at + (2 * c + 1) * n, n));
        if let Some((p0, p1)) = p {
            e0 = e0 << 1 | bits(v, p0, 1);
            e1 = e1 << 1 | bits(v, p1, 1);
        }
        ((widen(e0) + widen(e1)) / 2).min(255) as u8
    })
}

/// A block's colour, roughly: the mean of its endpoints. `None` where it is
/// not read here (BC6H; BC7 modes of more than one subset).
fn colour(kind: Kind, block: &[u8]) -> Option<[u8; 3]> {
    let half = |b: &[u8]| average(rgb565(u16::from_le_bytes([b[0], b[1]])), rgb565(u16::from_le_bytes([b[2], b[3]])));
    match kind {
        Kind::Bc1 => Some(half(block)),
        Kind::Bc3 => Some(half(&block[8..])),
        Kind::Bc4 => Some([mid(block[0], block[1]); 3]),
        Kind::Bc5 => Some([mid(block[0], block[1]), mid(block[8], block[9]), 0]),
        Kind::Bc6 => None,
        Kind::Bc7 => {
            let v = u128::from_le_bytes(block.try_into().ok()?);
            match block[0].trailing_zeros() {
                4 => Some(bc7_ends(v, 8, 5, None)),
                5 => Some(bc7_ends(v, 8, 7, None)),
                6 => Some(bc7_ends(v, 7, 7, Some((63, 64)))),
                _ => None,
            }
        }
    }
}

fn step(a: [u8; 3], b: [u8; 3]) -> u32 {
    (0..3).map(|i| u32::from(a[i].abs_diff(b[i]))).sum()
}

/// What the blocks of a block-compressed texture's bytes look like, taken in
/// the order they lie (the console's tiles keep most neighbours close).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Blocks {
    pub(crate) blocks: u64,
    /// All bytes zero (memory nothing wrote reads so).
    pub(crate) zero: u64,
    /// The same as the block before.
    pub(crate) same: u64,
    /// BC7 blocks without a mode bit: the card decodes them as zeros.
    pub(crate) invalid: u64,
    /// Blocks whose colour is read here ([`colour`]), and the sum of those.
    pub(crate) coloured: u64,
    pub(crate) rgb: [u64; 3],
    /// Of those, the ones whose colour jumps ([`JUMP`]) from the last one
    /// before them whose colour is read (BC7 blocks of other modes between
    /// are passed over).
    pub(crate) jumps: u64,
}

impl Blocks {
    /// A word for the look, by thresholds that are guesses: random bytes
    /// jump in about four blocks of five, a picture in few.
    pub(crate) fn looks(&self) -> &'static str {
        if self.blocks == 0 {
            "nothing"
        } else if self.zero * 2 > self.blocks {
            "zeros"
        } else if self.invalid * 8 > self.blocks {
            "bc7 without modes"
        } else if self.coloured >= 16 && self.jumps * 3 > self.coloured {
            "noise"
        } else if self.same * 10 >= self.blocks * 9 {
            "one block"
        } else {
            "texture"
        }
    }

    fn mean(&self) -> [u64; 3] {
        self.rgb.map(|s| s / self.coloured.max(1))
    }

    fn percent(&self, n: u64) -> u64 {
        n * 100 / self.blocks.max(1)
    }
}

/// [`Blocks`] of `bytes` of DXGI format `format`; `None` if the format is not
/// block-compressed.
pub(crate) fn blocks(format: u32, bytes: &[u8]) -> Option<Blocks> {
    let kind = kind(format)?;
    let mut s = Blocks::default();
    let mut before: Option<&[u8]> = None;
    let mut was: Option<[u8; 3]> = None;
    for block in bytes.chunks_exact(kind.bytes()) {
        s.blocks += 1;
        if block.iter().all(|&b| b == 0) {
            s.zero += 1;
        }
        if kind == Kind::Bc7 && block[0] == 0 {
            s.invalid += 1;
        }
        if before == Some(block) {
            s.same += 1;
        }
        before = Some(block);
        let Some(c) = colour(kind, block) else {
            continue;
        };
        s.coloured += 1;
        for (sum, v) in s.rgb.iter_mut().zip(c) {
            *sum += u64::from(v);
        }
        if was.is_some_and(|w| step(w, c) > JUMP) {
            s.jumps += 1;
        }
        was = Some(c);
    }
    Some(s)
}

/// What 8-bit RGBA pixels look like.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Pixels {
    pub(crate) pixels: u64,
    pub(crate) rgba: [u64; 4],
    /// All four channels zero.
    pub(crate) zero: u64,
    /// Whose colour jumps ([`JUMP`]) from the pixel to its left.
    pub(crate) jumps: u64,
    /// Of the coloured ones (largest minus smallest channel over 64), how
    /// many in each sixth of the hue circle.
    pub(crate) hues: [u64; 6],
}

impl Pixels {
    /// Sixths of the hue circle with a twentieth or more of the coloured
    /// pixels: a skin, a print, a suit take one to three; noise all six.
    pub(crate) fn hues_held(&self) -> usize {
        let coloured: u64 = self.hues.iter().sum();
        self.hues.iter().filter(|&&n| coloured > 0 && n * 20 >= coloured).count()
    }

    /// A word for the look (thresholds are guesses, as [`Blocks::looks`]).
    pub(crate) fn looks(&self) -> &'static str {
        if self.pixels == 0 {
            "nothing"
        } else if self.zero * 2 > self.pixels {
            "zeros"
        } else if self.jumps * 8 > self.pixels && self.hues_held() >= 4 {
            "noise"
        } else {
            "picture"
        }
    }

    fn mean(&self) -> [u64; 4] {
        self.rgba.map(|s| s / self.pixels.max(1))
    }
}

fn hue(c: [u8; 3]) -> Option<usize> {
    let (max, min) = (*c.iter().max()?, *c.iter().min()?);
    if max - min <= 64 {
        return None;
    }
    let [r, g, b] = c.map(i32::from);
    let d = i32::from(max - min);
    let h = if i32::from(max) == r {
        (g - b) * 60 / d
    } else if i32::from(max) == g {
        120 + (b - r) * 60 / d
    } else {
        240 + (r - g) * 60 / d
    };
    Some((h.rem_euclid(360) / 60) as usize)
}

/// [`Pixels`] of `height` rows of `width` RGBA (or BGRA) pixels, `pitch`
/// bytes apart.
pub(crate) fn pixels(data: &[u8], width: usize, height: usize, pitch: usize, bgra: bool) -> Pixels {
    let mut s = Pixels::default();
    for y in 0..height {
        let Some(row) = data.get(y * pitch..y * pitch + width * 4) else {
            break;
        };
        let mut left: Option<[u8; 3]> = None;
        for p in row.chunks_exact(4) {
            let c = if bgra { [p[2], p[1], p[0]] } else { [p[0], p[1], p[2]] };
            s.pixels += 1;
            for (sum, v) in s.rgba.iter_mut().zip([c[0], c[1], c[2], p[3]]) {
                *sum += u64::from(v);
            }
            if p.iter().all(|&b| b == 0) {
                s.zero += 1;
            }
            if left.is_some_and(|l| step(l, c) > JUMP) {
                s.jumps += 1;
            }
            if let Some(h) = hue(c) {
                s.hues[h] += 1;
            }
            left = Some(c);
        }
    }
    s
}

// Where title memory got its bytes.

/// How a DirectStorage request wrote title memory: bits 0-3 the BC format
/// BCPack unpacked (0: none), then zlib, a destination of physical pages, a
/// source in memory, and a request that failed (its destination holds what
/// it held before).
const ZLIB: u32 = 1 << 4;
const PAGES: u32 = 1 << 5;
const MEMORY: u32 = 1 << 6;
const FAILED: u32 = 1 << 7;
/// Not DirectStorage: what the card wrote into a title buffer, landed into
/// title memory by us after the card ran it ([`landed`]).
const LANDED: u32 = 1 << 8;
/// Not DirectStorage: a buffer copy of the title's into this memory,
/// recorded in a list (done at its submission, or on the card and landed
/// after it ran: either way after the texture pass of a bake recorded in the
/// meantime judged the memory) ([`title_copy`]).
const COPIED: u32 = 1 << 9;

fn how_text(how: u32) -> String {
    if how & LANDED != 0 {
        return "landed from the card (a title buffer the card wrote)".to_owned();
    }
    if how & COPIED != 0 {
        return "a copy of the title's on the card (CopyBufferRegion, CopyMemoryX), recorded".to_owned();
    }
    let mut text = match how & 15 {
        0 if how & ZLIB != 0 => "zlib".to_owned(),
        0 => "raw".to_owned(),
        bc => format!("bcpack bc{bc}{}", if how & ZLIB != 0 { "+zlib" } else { "" }),
    };
    if how & PAGES != 0 {
        text.push_str(" into pages");
    }
    if how & MEMORY != 0 {
        text.push_str(" from memory");
    }
    if how & FAILED != 0 {
        text.push_str(" FAILED");
    }
    text
}

/// The memory DirectStorage requests wrote, by address: each range with how
/// and the frame of the last request that wrote it.
#[derive(Default)]
pub(crate) struct Streams {
    by_start: BTreeMap<u64, (u64, u32, u64)>,
}

/// What [`Streams::covering`] says of a range.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Covered {
    /// Bytes of it some request wrote.
    pub(crate) bytes: u64,
    /// Of them, by requests that failed.
    pub(crate) failed: u64,
    /// The last of those requests: how, and its frame.
    pub(crate) newest: Option<(u32, u64)>,
}

impl Streams {
    /// A request wrote `[start, end)` in `frame`: what any other wrote there
    /// is forgotten.
    pub(crate) fn note(&mut self, start: u64, end: u64, how: u32, frame: u64) {
        if end <= start {
            return;
        }
        if let Some((&at, &(old_end, h, f))) = self.by_start.range(..start).next_back()
            && old_end > start
        {
            self.by_start.insert(at, (start, h, f));
            if old_end > end {
                self.by_start.insert(end, (old_end, h, f));
            }
        }
        let inside: Vec<u64> = self.by_start.range(start..end).map(|(&at, _)| at).collect();
        for at in inside {
            if let Some((old_end, h, f)) = self.by_start.remove(&at)
                && old_end > end
            {
                self.by_start.insert(end, (old_end, h, f));
            }
        }
        self.by_start.insert(start, (end, how, frame));
    }

    pub(crate) fn covering(&self, start: u64, end: u64) -> Covered {
        let mut c = Covered::default();
        let before = self.by_start.range(..start).next_back().filter(|(_, s)| s.0 > start);
        for (&at, &(stop, how, frame)) in before.into_iter().chain(self.by_start.range(start..end)) {
            let bytes = stop.min(end) - at.max(start);
            c.bytes += bytes;
            if how & FAILED != 0 {
                c.failed += bytes;
            }
            if c.newest.is_none_or(|(_, f)| frame >= f) {
                c.newest = Some((how, frame));
            }
        }
        c
    }

    fn len(&self) -> usize {
        self.by_start.len()
    }
}

static STREAMS: Mutex<Option<Streams>> = Mutex::new(None);

/// A landing of ours put what the card wrote into `len` bytes of title memory
/// at `at` (`crate::land`). For the probe only.
pub(crate) fn landed(at: usize, len: usize) {
    streamed(at, len, LANDED);
}

/// A buffer copy of the title's into `len` bytes at `at` is recorded
/// (`crate::list`). For the probe only.
pub(crate) fn title_copy(at: usize, len: usize) {
    streamed(at, len, COPIED);
}

/// A DirectStorage request wrote `len` bytes of title memory at `at` (or
/// failed to: `how` says, [`FAILED`]). For the probe only: nothing when it
/// is off.
pub fn streamed(at: usize, len: usize, how: u32) {
    if !on() || at == 0 || len == 0 {
        return;
    }
    let frame = crate::frame::current();
    let mut streams = STREAMS.lock().unwrap_or_else(|e| e.into_inner());
    let streams = streams.get_or_insert_with(Streams::default);
    // (Bounded: a range per request at most, and one forgotten when another
    // writes over it; past some millions all are forgotten once.)
    if streams.len() > 1 << 21 {
        *streams = Streams::default();
        static FULL: std::sync::Once = std::sync::Once::new();
        FULL.call_once(|| tracing::warn!("bake probe: the record of DirectStorage writes was full and starts again"));
    }
    streams.note(at as u64, (at + len) as u64, how, frame);
}

fn stream_text(c: Covered, len: u64, frame: u64) -> String {
    match c.newest {
        None => "none".to_owned(),
        Some((how, f)) => format!(
            "{} of {} bytes, last {} {} frames ago{}",
            c.bytes,
            len,
            how_text(how),
            frame.saturating_sub(f),
            if c.failed > 0 { format!(", {} bytes by failed requests", c.failed) } else { String::new() }
        ),
    }
}

// Bakes.

static BAKES: AtomicU64 = AtomicU64::new(0);

/// A list's last bake draw: its number and frame, whether it was drawn, the
/// dispatches after it that did not run (by pipeline), and those that did.
/// (A host list is recorded again and again: what comes in a later frame is
/// not the bake's, [`Open::now`].)
struct Open {
    seq: u64,
    frame: u64,
    drawn: bool,
    not_run: Vec<String>,
    ran: u32,
}

impl Open {
    fn now(&self) -> bool {
        crate::frame::current() <= self.frame + 1
    }
}

/// By host list.
static OPEN: Mutex<Option<HashMap<usize, Open>>> = Mutex::new(None);

fn with_open<T>(f: impl FnOnce(&mut HashMap<usize, Open>) -> T) -> T {
    f(OPEN.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new))
}

fn list_key(list: &HostList) -> usize {
    std::ptr::from_ref(list) as usize
}

/// A texture a bake draw samples, as it was when the draw was recorded.
struct Source {
    slot: u32,
    placed: bool,
    address: u64,
    /// Width, height, format, levels of the texture; the view's format and
    /// first level.
    shape: [u32; 4],
    view_format: u32,
    first: u32,
    /// The bytes of the levels the view shows, from the start of its memory,
    /// and how many of them are mapped.
    bytes: u64,
    mapped: u64,
    layout: Option<victory_formats::texture::Layout>,
    copy: Option<Arc<crate::resource::HostTexture>>,
    /// What the host slot shows: "its copy", "another texture WxH fF", "the
    /// neutral grey", "nothing".
    shows: String,
    made_from: bool,
    /// The title's view in the slot then, to tell at the later look whether
    /// the slot still holds it ([`Watched`]).
    view: View,
    /// A texture the host writes itself: its title memory says nothing.
    host_writes: bool,
}

/// A texture a bake draw sampled, looked at again [`LATER`] frames after the
/// bake: memory that holds other bytes then, under the same view, got its
/// real bytes after the bake (still streaming in, written where nothing told
/// of it), and the bake drew from what was there before.
struct Watched {
    seq: u64,
    frame: u64,
    slot: u32,
    view: View,
    address: u64,
    len: usize,
    format: u32,
    digest: crate::texture::Digest,
    looks: &'static str,
}

static WATCHED: Mutex<Vec<Watched>> = Mutex::new(Vec::new());

impl Source {
    fn is_its_copy(&self) -> bool {
        self.shows == "its copy"
    }
}

fn words_of(desc: &crate::resource::ResourceDesc) -> [u32; 14] {
    // SAFETY: the description is 56 bytes in this layout (`placed_bytes`).
    unsafe { std::ptr::from_ref(desc).cast::<[u32; 14]>().read_unaligned() }
}

fn desc_of(words: &[u32; 14]) -> crate::resource::ResourceDesc {
    // SAFETY: as above.
    unsafe { words.as_ptr().cast::<crate::resource::ResourceDesc>().read_unaligned() }
}

fn texture_text(texture: &ID3D12Resource) -> String {
    // SAFETY: a live resource.
    let d = unsafe { texture.GetDesc() };
    format!("{}x{} f{}", d.Width, d.Height, d.Format.0)
}

/// What a bake draw samples, at its recording: `slots` of `heap` are the
/// texture views the dwords of what it binds name (`cut`: places read only in
/// part); `pixel` its program; `target` and `viewport` its first target and
/// rectangle. The sources' memory is looked at again when `list` is
/// submitted.
#[allow(clippy::too_many_arguments)] // what one draw is
pub(crate) fn bake_draw(
    list: &HostList,
    heap: &crate::descriptor::Heap,
    host: &crate::descriptor::host::HostHeap,
    slots: &[u32],
    cut: u32,
    pixel: &str,
    target: Option<usize>,
    viewport: Option<D3D12_VIEWPORT>,
) {
    let seq = BAKES.fetch_add(1, Relaxed) + 1;
    if seq > MOST {
        if seq == MOST + 1 {
            tracing::info!(most = MOST, "bake probe: enough bakes looked at; the rest are not");
        }
        return;
    }
    let frame = crate::frame::current();
    let sources: Vec<Source> = slots.iter().filter_map(|&slot| source(heap, host, slot)).collect();
    let target_text = target
        .and_then(|t| crate::resource::host_resource(t as This))
        .map_or_else(|| "none".to_owned(), texture_text);
    let rect = viewport.map(|v| [v.TopLeftX as i32, v.TopLeftY as i32, v.Width as i32, v.Height as i32]);
    tracing::info!(
        bake = seq,
        frame,
        pixel,
        slots_named = slots.len(),
        read_in_part = cut,
        target = target_text,
        rect = ?rect,
        "bake probe: a decoration bake draw is recorded; what it samples is looked at when its list is submitted"
    );
    with_open(|open| {
        open.insert(list_key(list), Open { seq, frame, drawn: false, not_run: Vec::new(), ran: 0 });
    });
    let pixel = pixel.to_owned();
    let sources = Mutex::new(Some(sources));
    list.defer_host(move || {
        let Some(sources) = sources.lock().unwrap_or_else(|e| e.into_inner()).take() else {
            return;
        };
        submitted(seq, frame, &pixel, &sources);
    });
}

/// [`Source`] of slot `slot`, if it holds a texture view.
fn source(heap: &crate::descriptor::Heap, host: &crate::descriptor::host::HostHeap, slot: u32) -> Option<Source> {
    let handle = heap.handle(slot)?;
    let view = crate::descriptor::view(handle)?;
    let (placed, address, words, view_format, first, title) = match &view {
        View::Placed(false, resource, desc) if resource[0] != 1 => {
            let address = u64::from(desc[10]) | u64::from(desc[11]) << 32;
            (true, address, *resource, desc[0], crate::resource::first_mip(desc), crate::resource::texture_at_made(address, resource))
        }
        View::Shader(object, desc) => {
            let (memory, d) = crate::resource::texture_memory(*object as This)?;
            let words = words_of(&d);
            let first = desc.map_or(0, |d| crate::resource::first_mip(&d));
            (false, memory as u64, words, desc.map_or(words[8], |d| d[0]), first, Some(*object as This))
        }
        _ => return None,
    };
    let layout = crate::resource::texture_layout(&desc_of(&words))
        .filter(|t| t.planes.len() == 1)
        .map(|t| t.planes[0].layout.clone());
    let bytes = layout.as_ref().map_or(0, |l| l.bytes_from(crate::resource::read_from(l, first)));
    let mapped = if address == 0 { 0 } else { crate::texture::mapped_bytes(address as *const u8, bytes as usize) };
    let copy = title.and_then(crate::resource::made_copy);
    // A render target, depth or unordered access texture, or one the card
    // copied into (an earlier bake's result): its contents are the host's,
    // and its title memory holds nothing of them.
    let host_writes = copy.as_ref().is_some_and(|c| c.flags() & 0x7 != 0)
        || title.is_some_and(|t| crate::texture::host_wrote(t as usize));
    let shown = host.shown_texture(slot);
    let shows = match (&shown, &copy) {
        (None, _) => "nothing".to_owned(),
        (Some(s), Some(c)) if s.as_raw() == c.texture.as_raw() => "its copy".to_owned(),
        (Some(s), _) if crate::descriptor::host::is_neutral(s) => "the neutral grey".to_owned(),
        (Some(s), _) => format!("another texture {}", texture_text(s)),
    };
    Some(Source {
        slot,
        placed,
        address,
        shape: [words[4], words[6], words[8], words[7] >> 16],
        view_format,
        first,
        bytes,
        mapped,
        layout,
        copy,
        shows,
        made_from: host.texture_slot_made_from(slot, handle),
        view,
        host_writes,
    })
}

/// What DirectStorage requests wrote of `[address, address + bytes)`.
fn covering(address: u64, bytes: u64) -> Covered {
    STREAMS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(|s| s.covering(address, address + bytes))
        .unwrap_or_default()
}

/// The bake's list is submitted: what each source's host copy and memory
/// hold now.
fn submitted(seq: u64, frame: u64, pixel: &str, sources: &[Source]) {
    let mut suspects = 0;
    let mut read = Vec::new();
    for s in sources {
        // What title memory holds now, and what that looks like.
        let len = (s.bytes as usize).min(READ_MOST);
        read.clear();
        read.resize(len, 0);
        if s.address != 0 {
            crate::host::read_there(s.address as *const u8, &mut read);
        }
        let look = blocks(s.shape[2], &read);
        let stream = stream_text(covering(s.address, s.bytes), s.bytes, frame);
        // Whether the host copy holds what title memory holds (the digest of
        // its last upload, as `crate::texture::unchanged` compares).
        let holds = match (&s.copy, &s.layout) {
            (Some(copy), Some(layout)) => {
                let held = *copy.holds.lock().unwrap_or_else(|e| e.into_inner());
                held.map(|(from, digest)| {
                    let size = layout.bytes_from(from) as usize;
                    let mut whole = vec![0u8; size];
                    crate::host::read_there(s.address as *const u8, &mut whole);
                    crate::texture::digest(&whole) == digest
                })
            }
            _ => None,
        };
        let ahead = s.copy.as_ref().map_or(u8::MAX, |c| c.ahead.load(Relaxed));
        let ahead_text = match ahead {
            0 => "none",
            1 => "PENDING",
            2 => "ran",
            3 => "in a list",
            _ => "no copy",
        };
        let looks = if s.host_writes { "the host's own" } else { look.map_or("not block-compressed", |l| l.looks()) };
        let suspect = !s.is_its_copy()
            || (!s.host_writes
                && (holds != Some(true)
                    || ahead == 1
                    || s.mapped < s.bytes
                    || matches!(looks, "zeros" | "noise" | "bc7 without modes")
                    || stream.contains("FAILED")));
        suspects += u32::from(suspect);
        if s.address != 0 && len > 0 && !s.host_writes {
            WATCHED.lock().unwrap_or_else(|e| e.into_inner()).push(Watched {
                seq,
                frame,
                slot: s.slot,
                view: s.view.clone(),
                address: s.address,
                len,
                format: s.shape[2],
                digest: crate::texture::digest(&read),
                looks,
            });
        }
        let l = look.unwrap_or_default();
        let kind = if s.placed { "placed" } else { "object" };
        macro_rules! line {
            ($level:ident) => {
                tracing::$level!(
                    bake = seq,
                    frame,
                    slot = s.slot,
                    kind,
                    address = format_args!("{:#x}", s.address),
                    texture = format_args!("{}x{} f{} levels {}", s.shape[0], s.shape[1], s.shape[2], s.shape[3]),
                    view_format = s.view_format,
                    first_level = s.first,
                    bytes = s.bytes,
                    mapped = s.mapped,
                    slot_shows = s.shows.as_str(),
                    slot_made_from_its_view = s.made_from,
                    host_copy_holds_title_memory = ?holds,
                    upload_ahead = ahead_text,
                    streamed = stream.as_str(),
                    title_memory_looks = looks,
                    zero_blocks_pct = l.percent(l.zero),
                    bc7_without_mode_pct = l.percent(l.invalid),
                    jumps_pct = l.jumps * 100 / l.coloured.max(1),
                    mean_rgb = ?l.mean(),
                    "bake probe: a texture the bake draw samples"
                )
            };
        }
        if suspect {
            line!(warn);
        } else if seq <= 16 {
            line!(info);
        }
        let row: Vec<String> = vec![
            seq.to_string(),
            frame.to_string(),
            pixel.to_owned(),
            s.slot.to_string(),
            kind.to_owned(),
            format!("{:#x}", s.address),
            s.shape[0].to_string(),
            s.shape[1].to_string(),
            s.shape[2].to_string(),
            s.view_format.to_string(),
            s.shape[3].to_string(),
            s.first.to_string(),
            s.bytes.to_string(),
            s.mapped.to_string(),
            format!("\"{}\"", s.shows),
            s.made_from.to_string(),
            format!("{holds:?}"),
            ahead_text.to_owned(),
            format!("\"{stream}\""),
            looks.to_owned(),
            l.blocks.to_string(),
            l.zero.to_string(),
            l.invalid.to_string(),
            l.coloured.to_string(),
            l.jumps.to_string(),
            suspect.to_string(),
        ];
        table(&row.join(","));
    }
    tracing::info!(
        bake = seq,
        frame,
        pixel,
        sources = sources.len(),
        suspects,
        "bake probe: the bake's list is submitted; sources looked at"
    );
}

/// A row of the table of sources (`VICTORY_BAKE_PROBE_DUMP`).
fn table(row: &str) {
    static FILE: Mutex<Option<std::io::BufWriter<std::fs::File>>> = Mutex::new(None);
    let Some(dir) = dump_dir() else {
        return;
    };
    let mut file = FILE.lock().unwrap_or_else(|e| e.into_inner());
    if file.is_none() {
        let Ok(f) = std::fs::File::create(dir.join("bake-probe.csv")) else {
            return;
        };
        let mut f = std::io::BufWriter::new(f);
        let _ = writeln!(
            f,
            "bake,frame,pixel,slot,kind,address,width,height,format,view_format,levels,first_level,bytes,mapped,slot_shows,made_from_its_view,holds_title_memory,upload_ahead,streamed,looks,blocks,zero,bc7_without_mode,coloured,jumps,suspect"
        );
        *file = Some(f);
    }
    if let Some(f) = file.as_mut() {
        let _ = writeln!(f, "{row}");
        let _ = f.flush();
    }
}

// Read-backs.

/// What a read-back is of.
enum What {
    /// A bake draw's first target, the rectangle it drew in.
    Target { seq: u64 },
    /// A bake's result, level 0, at its copy or [`LATER`] frames on.
    Result { seq: u64, title: usize, later: bool },
}

struct Pending {
    buffer: ID3D12Resource,
    /// The rows: bytes of each, how many, how far apart; the DXGI format and
    /// the size in texels.
    rows: (usize, usize, usize),
    format: u32,
    size: (u32, u32),
    /// The submission of the list that holds the copy (0: not submitted yet).
    submission: Arc<AtomicU64>,
    asked_ms: u64,
    what: What,
}

static PENDING: Mutex<Vec<Pending>> = Mutex::new(Vec::new());

/// Records into `list` a copy of subresource `sub` of `title`'s host copy
/// `texture` (in `region`: x, y, width, height, if given) into a read-back
/// buffer, looked at once the list has run ([`tick`]).
fn read_back(list: &HostList, title: This, texture: &ID3D12Resource, sub: u32, region: Option<[u32; 4]>, what: What) {
    let Some(host) = crate::host::host() else {
        return;
    };
    // SAFETY: a live resource.
    let desc = unsafe { texture.GetDesc() };
    let mut footprint = D3D12_PLACED_SUBRESOURCE_FOOTPRINT::default();
    let (mut rows, mut row, mut total) = (0u32, 0u64, 0u64);
    // SAFETY: outputs for one subresource.
    unsafe {
        host.device.GetCopyableFootprints(
            &desc,
            sub,
            1,
            0,
            Some(&mut footprint),
            Some(&mut rows),
            Some(&mut row),
            Some(&mut total),
        );
    }
    if total == 0 || total == u64::MAX {
        return;
    }
    let mut source_box = None;
    if let Some([x, y, w, h]) = region {
        let f = &mut footprint.Footprint;
        let (w, h) = (w.min(f.Width.saturating_sub(x)), h.min(f.Height.saturating_sub(y)));
        if w == 0 || h == 0 {
            return;
        }
        let texel = row / u64::from(f.Width.max(1));
        f.Width = w;
        f.Height = h;
        f.RowPitch = ((u64::from(w) * texel).next_multiple_of(256)) as u32;
        rows = h;
        row = u64::from(w) * texel;
        total = u64::from(f.RowPitch) * u64::from(h - 1) + row;
        source_box = Some(D3D12_BOX { left: x, top: y, front: 0, right: x + w, bottom: y + h, back: 1 });
    }
    let Some(buffer) = host.read_back_buffer(total) else {
        return;
    };
    const COPY_SOURCE: u32 = 0x800;
    crate::list::ensure_state(list, title, COPY_SOURCE);
    let to = D3D12_TEXTURE_COPY_LOCATION {
        // SAFETY: a borrowed pointer the location never releases.
        pResource: unsafe { std::mem::transmute_copy(&buffer) },
        Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
        Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { PlacedFootprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT { Offset: 0, ..footprint } },
    };
    let from = D3D12_TEXTURE_COPY_LOCATION {
        // SAFETY: as above.
        pResource: unsafe { std::mem::transmute_copy(texture) },
        Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
        Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { SubresourceIndex: sub },
    };
    // SAFETY: an open list; both locations name live resources (the list
    // keeps the buffer until it is reset).
    unsafe { list.list.CopyTextureRegion(&to, 0, 0, 0, &from, source_box.as_ref().map(std::ptr::from_ref)) };
    list.keep(&buffer);
    let submission = Arc::new(AtomicU64::new(0));
    {
        let submission = submission.clone();
        list.defer_host(move || submission.store(crate::host::submissions().0, Relaxed));
    }
    PENDING.lock().unwrap_or_else(|e| e.into_inner()).push(Pending {
        buffer,
        rows: (row as usize, rows as usize, footprint.Footprint.RowPitch as usize),
        format: footprint.Footprint.Format.0 as u32,
        size: (footprint.Footprint.Width, footprint.Footprint.Height),
        submission,
        asked_ms: crate::host::now_ms(),
        what,
    });
}

/// A bake draw was recorded into `list`: what it drew in `target` (a title
/// texture) inside `viewport` is read back.
pub(crate) fn drawn(list: &HostList, target: Option<usize>, viewport: Option<D3D12_VIEWPORT>) {
    let Some(seq) = with_open(|open| {
        let o = open.get_mut(&list_key(list)).filter(|o| !o.drawn && o.now())?;
        o.drawn = true;
        Some(o.seq)
    }) else {
        return;
    };
    let (Some(target), Some(v)) = (target, viewport) else {
        return;
    };
    let Some(texture) = crate::resource::host_resource(target as This) else {
        return;
    };
    // SAFETY: a live resource.
    let format = unsafe { texture.GetDesc() }.Format.0;
    // 8-bit RGBA and BGRA.
    if !matches!(format, 27..=32 | 87..=91) {
        tracing::info!(bake = seq, format, "bake probe: the bake's first target is not 8-bit colour: not read back");
        return;
    }
    let region = [v.TopLeftX.max(0.0) as u32, v.TopLeftY.max(0.0) as u32, (v.Width as u32).min(512), (v.Height as u32).min(512)];
    read_back(list, target as This, texture, 0, Some(region), What::Target { seq });
}

/// A bake draw recorded into `list` (its texture pass ran) was not drawn.
pub(crate) fn not_drawn(list: &HostList) {
    let seq = with_open(|open| {
        let o = open.get_mut(&list_key(list)).filter(|o| !o.drawn && o.now())?;
        o.drawn = true;
        Some(o.seq)
    });
    if let Some(seq) = seq {
        tracing::warn!(bake = seq, "bake probe: a bake draw was not drawn (its preparation failed after the texture pass)");
    }
}

/// A dispatch was recorded into `list` (`ran`: or dropped) with title
/// pipeline `pipeline`: one after a bake draw of the list is counted.
pub(crate) fn dispatched(list: &HostList, pipeline: usize, ran: bool) {
    with_open(|open| {
        let Some(o) = open.get_mut(&list_key(list)).filter(|o| o.now()) else {
            return;
        };
        if ran {
            o.ran += 1;
        } else if o.not_run.len() < 16 {
            let name = crate::pipeline::host_pipeline(pipeline as This).map_or_else(
                || {
                    // No host pipeline: one of its programs does not
                    // translate, the host refused it, or it is being made.
                    let hash = std::cell::RefCell::new(String::new());
                    crate::pipeline::shaders_any(pipeline as This, |s| {
                        hash.borrow_mut().clone_from(&s.hash);
                        true
                    });
                    format!("{} (no host pipeline yet)", hash.into_inner())
                },
                |p| p.name.clone(),
            );
            tracing::warn!(bake = o.seq, pipeline = name, "bake probe: a dispatch after a bake draw did not run");
            o.not_run.push(name);
        }
    });
}

/// A bake's result: the title texture a compressed level was copied into.
struct Made {
    seq: u64,
    title: usize,
    /// Its host copy then (its address, to tell it from a later one).
    host: usize,
    memory: u64,
    bytes: u64,
    frame: u64,
    at_copy: Option<crate::texture::Digest>,
    looked_later: bool,
}

static RESULTS: Mutex<Vec<Made>> = Mutex::new(Vec::new());

/// A texture copy was recorded into `list`: from subresource 0 of `source` (a
/// title texture) into subresource `sub` of `title`, whose host copy is `to`.
/// One from a 32-bit integer scratch texture into level 0 of a
/// block-compressed one is a bake's result.
pub(crate) fn copied(list: &HostList, title: This, to: &ID3D12Resource, sub: u32, source: This) {
    if sub != 0 {
        return;
    }
    // SAFETY: a live resource.
    let format = unsafe { to.GetDesc() }.Format.0 as u32;
    let scratch = crate::resource::host_resource(source)
        // SAFETY: a live resource.
        .map(|s| unsafe { s.GetDesc() }.Format.0 as u32)
        .is_some_and(|f| matches!(f, 1..=4 | 15..=18));
    if kind(format).is_none() || !scratch {
        return;
    }
    let (seq, not_run, ran) = with_open(|open| {
        open.get(&list_key(list)).filter(|o| o.now()).map_or((0, Vec::new(), 0), |o| (o.seq, o.not_run.clone(), o.ran))
    });
    if seq == 0 && BAKES.load(Relaxed) > MOST {
        return;
    }
    let (memory, bytes) = crate::resource::texture_memory(title)
        .map(|(m, d)| (m as u64, crate::resource::placed_bytes(&words_of(&d)).unwrap_or(0)))
        .unwrap_or_default();
    tracing::info!(
        bake = seq,
        result = format_args!("{:#x}", title as usize),
        texture = texture_text(to),
        memory = format_args!("{memory:#x}"),
        bytes,
        dispatches_after_the_bake = ran,
        dispatches_not_run = ?not_run,
        "bake probe: a bake's result: its level 0 is copied from the compressed scratch texture; read back now and {LATER} frames on"
    );
    {
        let mut results = RESULTS.lock().unwrap_or_else(|e| e.into_inner());
        results.retain(|r| r.title != title as usize);
        results.push(Made {
            seq,
            title: title as usize,
            host: to.as_raw() as usize,
            memory,
            bytes,
            frame: crate::frame::current(),
            at_copy: None,
            looked_later: false,
        });
    }
    read_back(list, title, to, 0, None, What::Result { seq, title: title as usize, later: false });
}

/// The title releases texture `resource`: it is no bake's result any more.
/// Called before anything of it is let go.
pub(crate) fn released(resource: usize) {
    if !on() {
        return;
    }
    RESULTS.lock().unwrap_or_else(|e| e.into_inner()).retain(|r| r.title != resource);
}

/// At a texture pass of `list` (`heap`'s, `host` its host side): the
/// read-backs that have run are looked at, and the bakes' results due for
/// their later look are looked at and read back again.
pub(crate) fn tick(list: &HostList, heap: &crate::descriptor::Heap, host: &crate::descriptor::host::HostHeap) {
    let finished = crate::host::submissions().1;
    let now = crate::host::now_ms();
    let done: Vec<Pending> = {
        let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
        let (done, waiting): (Vec<Pending>, Vec<Pending>) = pending.drain(..).partition(|p| {
            let s = p.submission.load(Relaxed);
            (s != 0 && s <= finished) || (s == 0 && now > p.asked_ms + 60_000)
        });
        *pending = waiting;
        done
    };
    for p in done {
        read_back_ran(p);
    }
    let frame = crate::frame::current();
    sources_later(heap, frame);
    // Under the lock: a title texture leaves the table ([`released`]) before
    // anything of it is let go.
    let mut results = RESULTS.lock().unwrap_or_else(|e| e.into_inner());
    for r in results.iter_mut().filter(|r| !r.looked_later && frame >= r.frame + LATER) {
        r.looked_later = true;
        let copy = crate::resource::made_copy(r.title as This);
        let state = match &copy {
            None => "LET GO",
            Some(c) if c.texture.as_raw() as usize != r.host => "REPLACED by another host copy",
            Some(_) => "the same host copy",
        };
        let references = copy.as_ref().map_or(0, |c| crate::host::references(&c.texture));
        // The slots whose placed views lie in the result's memory, and what
        // each shows: a placed view is a texture of its own on the host,
        // filled from title memory, which the copy never reached.
        let slots = if r.memory != 0 { host.placed_slots_in(r.memory, r.memory + r.bytes.max(1)) } else { Vec::new() };
        let mut showing = 0;
        let mut others = Vec::new();
        for &slot in &slots {
            let shown = host.shown_texture(slot);
            if shown.as_ref().is_some_and(|s| s.as_raw() as usize == r.host) {
                showing += 1;
            } else if others.len() < 4 {
                let view = heap.handle(slot).and_then(crate::descriptor::view).map_or_else(String::new, |v| match v {
                    View::Placed(_, resource, desc) => format!(
                        "placed {}x{} f{} at {:#x}",
                        resource[4],
                        resource[6],
                        resource[8],
                        u64::from(desc[10]) | u64::from(desc[11]) << 32
                    ),
                    other => format!("{other:?}").chars().take(80).collect(),
                });
                others.push(format!("slot {slot} ({view}) shows {}", shown.as_ref().map_or_else(|| "nothing".to_owned(), texture_text)));
            }
        }
        let wrong = state != "the same host copy" || slots.len() > showing;
        macro_rules! line {
            ($level:ident) => {
                tracing::$level!(
                    bake = r.seq,
                    result = format_args!("{:#x}", r.title),
                    frames_on = frame - r.frame,
                    host_copy = state,
                    references_to_it = references,
                    placed_slots_at_its_memory = slots.len(),
                    of_them_showing_the_bake = showing,
                    others = ?others,
                    "bake probe: a bake's result some frames on: is it what is sampled? (its texels are compared when the read-back has run)"
                )
            };
        }
        if wrong { line!(warn) } else { line!(info) }
        if let Some(copy) = copy.filter(|c| c.texture.as_raw() as usize == r.host) {
            read_back(list, r.title as This, &copy.texture, 0, None, What::Result { seq: r.seq, title: r.title, later: true });
        }
    }
    results.retain(|r| !r.looked_later || frame < r.frame + 10 * LATER);
}

/// The textures bakes sampled [`LATER`] frames before `frame` are looked at
/// again ([`Watched`]): memory that holds other bytes now under the view the
/// slot still holds got them after the bake, and the bake drew from what was
/// there before (said with what wrote them, and when).
fn sources_later(heap: &crate::descriptor::Heap, frame: u64) {
    let due: Vec<Watched> = {
        let mut watched = WATCHED.lock().unwrap_or_else(|e| e.into_inner());
        let (due, waiting): (Vec<Watched>, Vec<Watched>) = watched.drain(..).partition(|w| frame >= w.frame + LATER);
        *watched = waiting;
        due
    };
    let mut read = Vec::new();
    for w in due {
        read.clear();
        read.resize(w.len, 0);
        crate::host::read_there(w.address as *const u8, &mut read);
        if crate::texture::digest(&read) == w.digest {
            continue;
        }
        let same_view = heap.handle(w.slot).and_then(crate::descriptor::view).is_some_and(|v| v == w.view);
        let covered = covering(w.address, w.len as u64);
        let written_after = match covered.newest {
            Some((how, f)) if f > w.frame => how_text(how),
            _ => "nothing recorded (the title's processor, or a way not recorded)".to_owned(),
        };
        let looks_now = blocks(w.format, &read).map_or("not block-compressed", |b| b.looks());
        let mapped_now = crate::texture::mapped_bytes(w.address as *const u8, w.len);
        macro_rules! line {
            ($level:ident, $text:literal) => {
                tracing::$level!(
                    bake = w.seq,
                    slot = w.slot,
                    address = format_args!("{:#x}", w.address),
                    bytes = w.len,
                    frames_after_the_bake = frame - w.frame,
                    slot_holds_the_same_view = same_view,
                    mapped_now,
                    looked_at_the_bake = w.looks,
                    looks_now,
                    written_after_the_bake_by = written_after.as_str(),
                    streamed = stream_text(covered, w.len as u64, frame).as_str(),
                    $text
                )
            };
        }
        if same_view && mapped_now < w.len as u64 {
            line!(
                info,
                "bake probe: the memory of a texture a bake sampled is not all mapped any more (streamed out): not compared"
            );
        } else if same_view {
            line!(
                warn,
                "bake probe: a texture a bake sampled holds other bytes now, under the same view: the bake drew from bytes that were not yet the texture's"
            );
        } else {
            line!(
                info,
                "bake probe: the memory of a texture a bake sampled holds other bytes now; its slot holds another view (the memory was given to another texture)"
            );
        }
    }
}

/// A read-back has run (or its list was never submitted): what it holds.
fn read_back_ran(p: Pending) {
    if p.submission.load(Relaxed) == 0 {
        return;
    }
    let (row, rows, pitch) = p.rows;
    let len = pitch * rows.saturating_sub(1) + row;
    let mut data: *mut std::ffi::c_void = std::ptr::null_mut();
    let range = D3D12_RANGE { Begin: 0, End: len };
    // SAFETY: a read-back buffer of ours of at least `len` bytes, written by
    // a list that has run.
    if unsafe { p.buffer.Map(0, Some(&range), Some(&mut data)) }.is_err() || data.is_null() {
        return;
    }
    // SAFETY: as above.
    let bytes: Vec<u8> = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), len) }.to_vec();
    // SAFETY: mapped above; nothing written.
    unsafe { p.buffer.Unmap(0, Some(&D3D12_RANGE { Begin: 0, End: 0 })) };
    match p.what {
        What::Target { seq } => {
            let bgra = matches!(p.format, 87..=91);
            let s = pixels(&bytes, p.size.0 as usize, rows, pitch, bgra);
            let looks = s.looks();
            macro_rules! line {
                ($level:ident) => {
                    tracing::$level!(
                        bake = seq,
                        size = format_args!("{}x{}", p.size.0, p.size.1),
                        looks,
                        mean_rgba = ?s.mean(),
                        jumps_pct = s.jumps * 100 / s.pixels.max(1),
                        zero_pct = s.zero * 100 / s.pixels.max(1),
                        hue_sixths_held = s.hues_held(),
                        "bake probe: what a bake draw left in its first target"
                    )
                };
            }
            if looks == "picture" { line!(info) } else { line!(warn) }
            if let Some(dir) = dump_dir() {
                let _ = bmp(&dir.join(format!("bake-{seq}-target.bmp")), &bytes, p.size.0 as usize, rows, pitch, bgra);
            }
        }
        What::Result { seq, title, later } => {
            // The blocks, row after row.
            let mut blocks_bytes = Vec::with_capacity(row * rows);
            for r in 0..rows {
                blocks_bytes.extend_from_slice(&bytes[r * pitch..r * pitch + row]);
            }
            let digest = crate::texture::digest(&blocks_bytes);
            let look = blocks(p.format, &blocks_bytes).unwrap_or_default();
            let changed = {
                let mut results = RESULTS.lock().unwrap_or_else(|e| e.into_inner());
                let r = results.iter_mut().find(|r| r.title == title && r.seq == seq);
                match (r, later) {
                    (Some(r), false) => {
                        r.at_copy = Some(digest);
                        None
                    }
                    (Some(r), true) => r.at_copy.map(|d| d != digest),
                    (None, _) => None,
                }
            };
            let looks = look.looks();
            macro_rules! line {
                ($level:ident) => {
                    tracing::$level!(
                        bake = seq,
                        result = format_args!("{title:#x}"),
                        when = if later { "later" } else { "at its copy" },
                        texture = format_args!("{}x{} f{}", p.size.0, p.size.1, p.format),
                        looks,
                        jumps_pct = look.jumps * 100 / look.coloured.max(1),
                        zero_blocks_pct = look.percent(look.zero),
                        mean_rgb = ?look.mean(),
                        changed_since_its_copy = ?changed,
                        "bake probe: a bake's result, level 0, read back"
                    )
                };
            }
            if changed == Some(true) || matches!(looks, "zeros" | "noise" | "bc7 without modes") {
                line!(warn);
            } else {
                line!(info);
            }
            if let Some(dir) = dump_dir() {
                let when = if later { "later" } else { "copy" };
                let name = format!("bake-{seq}-result-{when}-{}x{}-f{}-s0.bin", p.size.0, p.size.1, p.format);
                let _ = std::fs::write(dir.join(name), &blocks_bytes);
            }
        }
    }
}

/// A 24-bit BMP of `rows` rows of `width` RGBA (`bgra`: BGRA) pixels,
/// `pitch` bytes apart.
fn bmp(path: &std::path::Path, data: &[u8], width: usize, rows: usize, pitch: usize, bgra: bool) -> std::io::Result<()> {
    let line = (width * 3).next_multiple_of(4);
    let image = line * rows;
    let mut out = Vec::with_capacity(54 + image);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&((54 + image) as u32).to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(width as i32).to_le_bytes());
    out.extend_from_slice(&(rows as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&(image as u32).to_le_bytes());
    out.extend_from_slice(&[0; 16]);
    for y in (0..rows).rev() {
        let row = &data[y * pitch..y * pitch + width * 4];
        for p in row.chunks_exact(4) {
            out.extend_from_slice(&if bgra { [p[0], p[1], p[2]] } else { [p[2], p[1], p[0]] });
        }
        out.resize(out.len() + line - width * 3, 0);
    }
    std::fs::write(path, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A generator of bytes for the tests.
    fn noise(n: usize, mut seed: u64) -> Vec<u8> {
        (0..n)
            .map(|_| {
                seed = seed.wrapping_mul(0x5851_F42D_4C95_7F2D).wrapping_add(0x1405_7B7E_F767_814F);
                (seed >> 33) as u8
            })
            .collect()
    }

    /// A BC1 block of one 565 colour.
    fn bc1(c: u16) -> [u8; 8] {
        let [a, b] = c.to_le_bytes();
        [a, b, a, b, 0, 0, 0, 0]
    }

    #[test]
    fn random_blocks_look_like_noise_and_a_gradient_does_not() {
        for format in [71, 77, 98] {
            let random = blocks(format, &noise(16 * 4096, 7)).unwrap();
            assert_eq!(random.looks(), "noise", "format {format}: {random:?}");
        }
        // A smooth BC1 ramp of red (up and down a step a block) and green.
        let ramp: Vec<u8> = (0..4096u16)
            .flat_map(|i| {
                let red = if i % 64 < 32 { i % 64 } else { 63 - i % 64 };
                bc1(red << 11 | (i / 64 % 64) << 5)
            })
            .collect();
        let s = blocks(71, &ramp).unwrap();
        assert_eq!(s.looks(), "texture", "{s:?}");
        assert_eq!(s.jumps, 0);
        // Zeros, one block, and BC7 without modes.
        assert_eq!(blocks(98, &vec![0; 16 * 64]).unwrap().looks(), "zeros");
        let flat: Vec<u8> = (0..64).flat_map(|_| bc1(0xf81f)).collect();
        assert_eq!(blocks(71, &flat).unwrap().looks(), "one block");
        let modeless: Vec<u8> = (0..64).flat_map(|i| [[0u8; 15].as_slice(), &[i as u8 | 1]].concat()).collect();
        assert_eq!(blocks(98, &modeless).unwrap().looks(), "bc7 without modes");
        // Not block-compressed.
        assert!(blocks(28, &[0; 64]).is_none());
    }

    #[test]
    fn a_bc7_block_of_one_subset_gives_its_endpoints_colour() {
        // Mode 6: red 7 bits 100 at both ends, green 0, blue 127; p-bits 1 (of
        // every channel).
        let mut v: u128 = 1 << 6;
        let put = |v: &mut u128, at: u32, n: u32, x: u128| *v |= (x & ((1 << n) - 1)) << at;
        put(&mut v, 7, 7, 100);
        put(&mut v, 14, 7, 100);
        put(&mut v, 35, 7, 127);
        put(&mut v, 42, 7, 127);
        put(&mut v, 63, 1, 1);
        put(&mut v, 64, 1, 1);
        assert_eq!(colour(Kind::Bc7, &v.to_le_bytes()), Some([201, 1, 255]));
        // Mode 5: 7-bit colours widened.
        let mut v: u128 = 1 << 5;
        put(&mut v, 8, 7, 127);
        put(&mut v, 15, 7, 127);
        assert_eq!(colour(Kind::Bc7, &v.to_le_bytes()), Some([255, 0, 0]));
        // Mode 1 (two subsets): not read.
        assert_eq!(colour(Kind::Bc7, &(2u128).to_le_bytes()), None);
    }

    #[test]
    fn pixels_of_noise_and_of_a_skin() {
        let (w, h) = (64usize, 64usize);
        let skin: Vec<u8> = (0..w * h).flat_map(|i| [200, 150 + (i % 7) as u8, 120, 255]).collect();
        let s = pixels(&skin, w, h, w * 4, false);
        assert_eq!(s.looks(), "picture", "{s:?}");
        assert_eq!(s.mean()[3], 255);
        let random = noise(w * h * 4, 3);
        let s = pixels(&random, w, h, w * 4, false);
        assert_eq!(s.looks(), "noise", "{s:?}");
        assert_eq!(s.hues_held(), 6);
        assert_eq!(pixels(&vec![0; w * h * 4], w, h, w * 4, true).looks(), "zeros");
    }

    #[test]
    fn streams_keep_the_last_writer_of_each_byte() {
        let mut s = Streams::default();
        s.note(0x1000, 0x5000, 7, 10);
        // A later request over the middle splits the first.
        s.note(0x2000, 0x3000, ZLIB, 20);
        assert_eq!(s.by_start.len(), 3);
        assert_eq!(s.covering(0x1000, 0x5000), Covered { bytes: 0x4000, failed: 0, newest: Some((ZLIB, 20)) });
        assert_eq!(s.covering(0x3000, 0x4000), Covered { bytes: 0x1000, failed: 0, newest: Some((7, 10)) });
        // One over the start and past the end of others replaces them.
        s.note(0x800, 0x6000, 1 | FAILED, 30);
        assert_eq!(s.by_start.len(), 1);
        let c = s.covering(0x0, 0x8000);
        assert_eq!(c, Covered { bytes: 0x5800, failed: 0x5800, newest: Some((1 | FAILED, 30)) });
        // Nothing written there.
        assert_eq!(s.covering(0x9000, 0xa000), Covered::default());
        assert_eq!(how_text(7 | ZLIB | PAGES), "bcpack bc7+zlib into pages");
        assert_eq!(how_text(FAILED), "raw FAILED");
    }

    #[test]
    fn a_bmp_has_its_header_and_rows_bottom_up() {
        let dir = std::env::temp_dir().join(format!("bake-probe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.bmp");
        // 2x2: top row red, green; bottom row blue, white (RGBA, pitch 8).
        let data = [255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255];
        bmp(&path, &data, 2, 2, 8, false).unwrap();
        let out = std::fs::read(&path).unwrap();
        assert_eq!(&out[..2], b"BM");
        assert_eq!(out.len(), 54 + 2 * 8);
        // Bottom row first, BGR: blue then white.
        assert_eq!(&out[54..60], &[255, 0, 0, 255, 255, 255]);
        assert_eq!(&out[62..68], &[0, 0, 255, 0, 255, 0]);
        let _ = std::fs::remove_dir_all(dir);
    }
}
