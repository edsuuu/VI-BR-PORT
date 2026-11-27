//! The textures a draw of the title's decoration bake samples, given to it
//! whatever the texture queue's room line says (`VICTORY_BAKE_ADMIT=1`).
//!
//! The bake (`PS_BakeDecorations_*`, `PS_DecorationBlend_*`) composes a
//! character's skin, tattoos and clothes into a texture of the title's once,
//! when the character appears, and samples its sources from the view heap by
//! index. A source the pass ahead of the bake did not bring onto the card is
//! baked as whatever its host slot showed (the texture that held the slot
//! before, the neutral grey, or nothing), for the rest of the run: the
//! counselor's face in purple and pink noise and a guard's forearm in
//! coloured confetti in the owner's recording of 27 Nov, an inmate's head in
//! one flat colour. The pass refused them for room: in a world's first frames
//! the queue's line counts as used the room kept for render targets not
//! measured yet (`resource::targets_room_kept`), and minutes in the card is
//! near its budget.
//!
//! The indices are data, not code: dwords of the constant buffer the draw
//! binds (three base textures at 0x20, 0x40 and 0x60 in
//! `PS_DecorationBlend_1010`) and of the 304-byte decoration records of the
//! structured buffer it binds (+0x0 and +0x4 in `PS_BakeDecorations_0000`).
//! So every dword of those buffers, of the stage's root constants and of its
//! root views that names a slot holding a small placed texture is taken as a
//! source ([`is_source`]): a dword that only looks like one costs the look at
//! a small texture, never a wrong picture. Unlike the attempt of 27 Nov
//! (`VICTORY_BAKE_SOURCES`, off since 2cdaeb1f3), texture objects (the small
//! flags and counts that named the heap's first slots), anything larger than a
//! decoration (the bake's own 22 MB targets), and memory that is not the
//! title's placed textures are never sources; the sources are not watched
//! after the bake; and the bake waits only for its own sources' uploads on the
//! copy queue, not for every upload on its way. That wait is the list's: a
//! source is never taken as read through a table (the attempt's second
//! fault: the pass marked such a slot done with no upload), so its upload is
//! recorded into the bake's list, and recording it waits for the upload of it
//! on its way, a second at most (`crate::texture::ahead::listed`), then
//! records one of its own.
//!
//! Nothing here writes title memory: the buffers are read through the kernel
//! (`host::copy_mapped`, which fails instead of faulting on memory that is
//! not there), at most [`BUFFER_BYTES`] of each.

use std::sync::atomic::{AtomicU64, Ordering};

/// Whether a bake draw's sources are found and given to it past the queue's
/// room line (`VICTORY_BAKE_ADMIT=1`; off by default: the pass ahead of a bake
/// refuses them as it refuses any texture when the card has no room).
pub(crate) fn admit_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_BAKE_ADMIT").is_some_and(|v| v == "1"))
}

/// The largest texture taken as a source: the decorations met as sources in
/// the runs of 27 Nov were 4 KiB to 384 KiB in the prison and up to 1.4 MiB on
/// the boat (512x512 BC3 with its levels: 352 KiB); the 22 MB textures met
/// were the bake's own targets.
pub(crate) const SOURCE_BYTES: u64 = 2 << 20;

/// The most sources one draw takes: a bake named 16 to 19 (runs barco-s-final,
/// prisao-s-dlss1); past this the dwords are not indices.
pub(crate) const MOST: usize = 64;

/// The most bytes of one buffer looked through for indices.
pub(crate) const BUFFER_BYTES: usize = 64 << 10;

/// The bytes looked through behind a root view no title buffer is known to
/// hold (a root view names an address alone): a constant buffer's worth.
pub(crate) const ROOT_VIEW_BYTES: u64 = 4 << 10;

/// The most bytes of sources one bake draw makes past the queue's room line:
/// sixteen sources of the largest a decoration is. On a card with no room, the
/// rest are refused as any texture is.
pub(crate) const PAST_LINE_BYTES: u64 = 32 << 20;

/// Whether a slot's view, as [`is_source`] needs it: a placed texture (not a
/// buffer, not a texture object), the bytes of its memory from its first
/// level on.
pub(crate) fn is_source(placed_texture: bool, bytes: Option<u64>) -> bool {
    placed_texture && bytes.is_some_and(|b| b > 0 && b <= SOURCE_BYTES)
}

/// The dwords of `words` that are below `count` and that `is_source` takes,
/// each once, in the order met, at most [`MOST`] in `out` (which may already
/// hold some).
pub(crate) fn candidates(words: &[u32], count: u32, mut is_source: impl FnMut(u32) -> bool, out: &mut Vec<u32>) {
    for &word in words {
        if out.len() >= MOST {
            return;
        }
        if word < count && !out.contains(&word) && is_source(word) {
            out.push(word);
        }
    }
}

/// What one bake draw's pass made of its sources past the queue's line.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PastLine {
    /// Sources made although the card had no room, and their bytes.
    pub(crate) slots: u32,
    pub(crate) bytes: u64,
    /// Sources refused all the same: past [`PAST_LINE_BYTES`].
    pub(crate) refused: u32,
}

impl PastLine {
    /// A source of `bytes` meets a card without room: whether it is made all
    /// the same (counted), within [`PAST_LINE_BYTES`] for the draw.
    pub(crate) fn take(&mut self, bytes: u64) -> bool {
        if self.bytes.saturating_add(bytes) > PAST_LINE_BYTES {
            self.refused += 1;
            return false;
        }
        self.slots += 1;
        self.bytes += bytes;
        true
    }
}

/// What became of one bake draw's sources in its pass.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Sources {
    pub(crate) found: u32,
    /// The pass ended before it reached them (it never does: sources are
    /// served first and past the pass's time and bytes; counted to show it).
    pub(crate) not_reached: u32,
    /// Refused for room past [`PAST_LINE_BYTES`].
    pub(crate) refused: u32,
    /// Their memory is not mapped: the title has not streamed them in, which
    /// nothing of ours can help (on the console the bake reads that memory as
    /// it is).
    pub(crate) not_mapped: u32,
    pub(crate) uploaded: u32,
    /// Places looked in for sources that were read only in part: buffers
    /// past [`BUFFER_BYTES`], and the dwords left once [`MOST`] were found.
    /// A source there may be among the slots refused for room.
    pub(crate) cut: u32,
    pub(crate) same: u32,
    /// Sources whose first upload was on its way on the copy queue of its
    /// own (`VICTORY_UPLOAD_AHEAD=1`) when the bake's pass met them, and the
    /// microseconds recording their uploads took: the list waits for that
    /// upload there, a second at most for each.
    pub(crate) on_their_way: u32,
    pub(crate) waited_us: u64,
}

impl Sources {
    /// The sources this bake samples without their texture on the card for
    /// it because of us: refused or not reached.
    pub(crate) fn missing(&self) -> u32 {
        self.refused + self.not_reached
    }
}

/// Since the start: bake draws whose sources were looked for, sources found,
/// made past the room line, their bytes, refused past [`PAST_LINE_BYTES`],
/// not mapped, on their way on the copy queue, microseconds their uploads
/// took to record.
pub(crate) static COUNTS: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];

/// Adds one bake draw's numbers to [`COUNTS`]; returns the draws so far.
pub(crate) fn count(sources: &Sources, past: &PastLine) -> u64 {
    let add = |i: usize, n: u64| COUNTS[i].fetch_add(n, Ordering::Relaxed) + n;
    let n = add(0, 1);
    add(1, u64::from(sources.found));
    add(2, u64::from(past.slots));
    add(3, past.bytes);
    add(4, u64::from(past.refused));
    add(5, u64::from(sources.not_mapped));
    add(6, u64::from(sources.on_their_way));
    add(7, sources.waited_us);
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources_are_small_placed_textures() {
        assert!(is_source(true, Some(4096)));
        assert!(is_source(true, Some(SOURCE_BYTES)));
        // The bake's own 22 MB targets, a texture object, nothing known.
        assert!(!is_source(true, Some(22_413_312)));
        assert!(!is_source(false, Some(4096)));
        assert!(!is_source(true, None));
        assert!(!is_source(true, Some(0)));
    }

    #[test]
    fn indices_are_the_dwords_that_name_sources() {
        // Floats, a count past the heap, flags and two indices, one twice.
        let words = [0x3f00_0000, 1_000_000, 2, 7_301, 0x400, 7_301, 12];
        let sources = [7_301u32, 12];
        let mut out = vec![12];
        candidates(&words, 10_000, |i| sources.contains(&i), &mut out);
        assert_eq!(out, [12, 7_301]);
        // At most `MOST`.
        let many: Vec<u32> = (0..1_000).collect();
        let mut out = Vec::new();
        candidates(&many, 10_000, |_| true, &mut out);
        assert_eq!(out.len(), MOST);
    }

    #[test]
    fn sources_past_the_line_are_made_up_to_the_cap() {
        let mut past = PastLine::default();
        // Sixteen of the largest a source is fill the cap; the next is refused,
        // and a small one after it is still refused only if it does not fit.
        for _ in 0..16 {
            assert!(past.take(SOURCE_BYTES));
        }
        assert!(!past.take(4096));
        assert_eq!(past, PastLine { slots: 16, bytes: PAST_LINE_BYTES, refused: 1 });
        let mut past = PastLine::default();
        assert!(past.take(PAST_LINE_BYTES - 4096));
        assert!(!past.take(8192));
        assert!(past.take(4096));
        assert_eq!((past.slots, past.refused, past.bytes), (2, 1, PAST_LINE_BYTES));
    }

    #[test]
    fn missing_sources_are_the_ones_we_left() {
        let sources = Sources {
            found: 12,
            uploaded: 3,
            same: 8,
            not_mapped: 1,
            on_their_way: 2,
            waited_us: 40_000,
            ..Sources::default()
        };
        // Not mapped is the title's, and one on its way is waited for by the
        // list: neither is missing on our account.
        assert_eq!(sources.missing(), 0);
        let sources = Sources { refused: 1, not_reached: 2, ..sources };
        assert_eq!(sources.missing(), 3);
    }
}
