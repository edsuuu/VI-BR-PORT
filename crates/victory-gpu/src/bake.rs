//! The textures a draw of the title's decoration bake samples.
//!
//! The bake's pixel programs (`PS_BakeDecorations_*`, `PS_DecorationBlend_*`)
//! pick their source textures from the view heap by index, and the indices
//! are data, not code: dwords of the constant buffer the draw binds (three
//! base textures at 0x20, 0x40 and 0x60 in `PS_DecorationBlend_1010`), and
//! dwords of the 304-byte decoration records of the structured buffer it
//! binds (+0x0 and +0x4 of each record in `PS_BakeDecorations_0000`). The
//! offsets differ from program to program, so every dword of those buffers
//! (and of the stage's root constants) that names a texture view of the
//! heap is taken as a source: a dword that is not an index costs one look
//! at a texture the bake does not read, never a wrong result.
//!
//! The pass ahead of a bake looked only at the slots the queue of textures
//! picked by index held as due within a few frames. A title that streams a
//! decoration in writes its view first and fills its memory afterwards
//! through another view of the same pages, which nothing tells of: the
//! queue holds the slot as uploaded, to be looked at again in 4, 12, 40
//! frames, and a bake between the fill and that look samples what the
//! memory held before (another texture's blocks: coloured noise on the
//! man's shorts in the opening, one launch in five). Each source the bake
//! names is now brought up to date at the bake itself, compared whole
//! (`crate::texture::unchanged`), whatever the queue knew of it.
//!
//! The sources are also watched for some frames after the bake: one that
//! changes then was filled after the title asked for the bake, which on the
//! console reads it later, when the card runs the draw. That is said in the
//! log; it is what would ask for the bake to be drawn again.

use std::sync::Mutex;

/// Whether a bake draw's sources are found and brought up to date at the
/// draw (`VICTORY_BAKE_SOURCES=0`: not, as before; on by default).
pub(crate) fn on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_BAKE_SOURCES").is_none_or(|v| v != "0"))
}

/// The most sources one draw takes: the bake's records are a few dozen;
/// past this the dwords are not indices.
pub(crate) const MOST: usize = 256;

/// The most bytes of one buffer looked through for indices.
pub(crate) const BUFFER_BYTES: usize = 64 << 10;

/// The dwords of `words` that are below `count` and that `is_texture` says
/// name a texture view, each once, in the order met, at most [`MOST`] in
/// `out` (which may already hold some).
pub(crate) fn candidates(words: &[u32], count: u32, mut is_texture: impl FnMut(u32) -> bool, out: &mut Vec<u32>) {
    for &word in words {
        if out.len() >= MOST {
            return;
        }
        if word < count && !out.contains(&word) && is_texture(word) {
            out.push(word);
        }
    }
}

/// The frames a source is watched after its bake.
pub(crate) const WATCH_FRAMES: u64 = 64;

/// The most sources watched at once.
const WATCHED_MOST: usize = 512;

/// A source of a bake, watched after it: its slot, its memory (address,
/// bytes), the fingerprint it had at the bake, and the bake's frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Watched {
    pub(crate) slot: u32,
    pub(crate) at: u64,
    pub(crate) len: u64,
    pub(crate) print: u64,
    pub(crate) frame: u64,
}

/// The sources of the bakes of the last [`WATCH_FRAMES`] frames.
#[derive(Default)]
pub(crate) struct Watch {
    sources: Vec<Watched>,
}

impl Watch {
    /// Watches `source` from its bake on (in place of what was watched of
    /// the same slot and memory).
    pub(crate) fn add(&mut self, source: Watched) {
        self.sources.retain(|w| !(w.slot == source.slot && w.at == source.at));
        if self.sources.len() >= WATCHED_MOST {
            self.sources.remove(0);
        }
        self.sources.push(source);
    }

    /// At `frame`: lets go of the sources baked longer ago than
    /// [`WATCH_FRAMES`], and returns those whose fingerprint (`print` of
    /// address and bytes) is no longer the one they had at the bake, each
    /// once (it then watches the new fingerprint).
    pub(crate) fn changed(&mut self, frame: u64, mut print: impl FnMut(u64, u64) -> u64) -> Vec<Watched> {
        self.sources.retain(|w| frame.saturating_sub(w.frame) <= WATCH_FRAMES);
        let mut out = Vec::new();
        for w in &mut self.sources {
            let now = print(w.at, w.len);
            if now != w.print {
                out.push(*w);
                w.print = now;
            }
        }
        out
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }
}

/// The sources being watched after their bakes.
pub(crate) static WATCH: Mutex<Watch> = Mutex::new(Watch { sources: Vec::new() });

/// Since the start: bake draws whose sources were looked at, sources
/// looked at, of them uploaded because they had changed, of those the
/// ones the queue held as up to date (out of it, or not due before the
/// draw), and sources found changed after their bake.
pub(crate) static COUNTS: [std::sync::atomic::AtomicU64; 5] = [const { std::sync::atomic::AtomicU64::new(0) }; 5];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indices_are_the_dwords_that_name_texture_views() {
        // Floats, a count past the heap, flags and two indices, one twice.
        let words = [0x3f00_0000, 1_000_000, 2, 7_301, 0x400, 7_301, 12];
        let textures = [7_301u32, 12, 2];
        let mut out = vec![12];
        candidates(&words, 10_000, |i| textures.contains(&i) && i != 2, &mut out);
        assert_eq!(out, [12, 7_301]);
        // At most `MOST`.
        let many: Vec<u32> = (0..1_000).collect();
        let mut out = Vec::new();
        candidates(&many, 10_000, |_| true, &mut out);
        assert_eq!(out.len(), MOST);
    }

    #[test]
    fn a_source_that_changes_after_its_bake_is_said_once_while_it_is_watched() {
        let mut watch = Watch::default();
        let source = Watched { slot: 5, at: 0x1000, len: 64, print: 1, frame: 100 };
        watch.add(source);
        watch.add(Watched { slot: 6, at: 0x2000, len: 64, print: 1, frame: 100 });
        let mut memory = std::collections::HashMap::from([(0x1000u64, 1u64), (0x2000, 1)]);
        assert!(watch.changed(101, |at, _| memory[&at]).is_empty());
        memory.insert(0x1000, 2);
        assert_eq!(watch.changed(102, |at, _| memory[&at]), [source]);
        assert!(watch.changed(103, |at, _| memory[&at]).is_empty());
        // Baked again: watched from the new bake, in place of the old.
        watch.add(Watched { frame: 150, print: 2, ..source });
        assert_eq!(watch.sources.len(), 2);
        // Past the frames watched, let go.
        assert!(watch.changed(100 + WATCH_FRAMES + 1, |at, _| memory[&at]).is_empty());
        assert_eq!(watch.sources.len(), 1);
        assert!(!watch.is_empty());
        assert!(watch.changed(150 + WATCH_FRAMES + 1, |at, _| memory[&at]).is_empty());
        assert!(watch.is_empty());
    }
}
