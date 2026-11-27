//! Exact lengths of the structures the title's builder is told to build or
//! copy, and of their scratch memory, by their start in title memory.
//!
//! A builder dispatch names its memory by root views, which have a start and
//! no length, so without this a structure is made ready by a guess
//! (`VIEW`/`BUILT` in the module above: 256 KiB and 4 MiB). The guess holds
//! 4 MiB of title memory for a structure of a few KiB and stops short of a
//! longer one: a top level of 32,768 instances is about 12 MB (a claim of the
//! fork this is ported from, run rt-3, where some 800 pages a frame were
//! still found without a copy in one stretch of addresses).
//!
//! The lengths come from the title's own calls of its builder
//! (`victory_hle::modules::bvh_builder` with `VICTORY_RT_NATIVE=1`), which
//! ask the builder's CPU-side size calculation again for every build. Without
//! that switch nothing is ever put in and every lookup answers `None`.

use std::collections::BTreeMap;

/// Extents kept at most; past it all are let go (the builder's calls name
/// the live ones again as they come).
pub(super) const MOST: usize = 1 << 16;

/// Starts and lengths of structures, none overlapping another.
pub(super) struct Extents {
    map: BTreeMap<u64, u64>,
}

/// What [`Extents::put`] did.
#[derive(Debug, PartialEq)]
pub(super) enum Put {
    /// The same extent was there already.
    Known,
    /// Taken (others it overlapped were replaced or cut short).
    Taken,
    /// Taken after all others were let go ([`MOST`]).
    Cleared,
    /// Not a length worth believing, or not memory the directory covers.
    Refused,
}

impl Extents {
    pub(super) const fn new() -> Extents {
        Extents { map: BTreeMap::new() }
    }

    /// The builder was told to use `len` bytes from `start`, inside
    /// `[lowest, end)` and at most `longest` bytes long. An extent that
    /// begins inside the new one is replaced, and one that reaches into it
    /// from below is cut short: memory is reused for structures of other
    /// sizes.
    pub(super) fn put(&mut self, start: u64, len: u64, lowest: u64, end: u64, longest: u64) -> Put {
        if start < lowest || len == 0 || len > longest || start.checked_add(len).is_none_or(|e| e > end) {
            return Put::Refused;
        }
        // (The same structure named again, frame after frame.)
        if self.map.get(&start) == Some(&len) {
            return Put::Known;
        }
        let stop = start + len;
        let inside: Vec<u64> = self.map.range(start..stop).map(|(&at, _)| at).collect();
        for at in inside {
            self.map.remove(&at);
        }
        if let Some((&before, held)) = self.map.range_mut(..start).next_back()
            && before + *held > start
        {
            *held = start - before;
        }
        let cleared = self.map.len() >= MOST;
        if cleared {
            self.map.clear();
        }
        self.map.insert(start, len);
        if cleared { Put::Cleared } else { Put::Taken }
    }

    /// The bytes from `at` to the end of the extent that holds it.
    pub(super) fn from(&self, at: u64) -> Option<u64> {
        let (&start, &len) = self.map.range(..=at).next_back()?;
        (at < start + len).then(|| start + len - at)
    }

    pub(super) fn len(&self) -> usize {
        self.map.len()
    }
}

/// The compacted size a copy of a structure `whole` bytes long was given,
/// if `size` (what the title's size slot holds) reads as one: not zero, a
/// multiple of 64 bytes and no longer than the structure it is a copy of.
/// Otherwise the slot held something else (the title read it before our
/// submission that stores it landed, `sizes_wait` in the module above), and
/// the source's own length would reach over whatever lies behind the copy.
pub(super) fn compacted(size: u64, whole: u64) -> Option<u64> {
    (size != 0 && size <= whole && size.is_multiple_of(64)).then_some(size)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOW: u64 = 1 << 32;
    const END: u64 = 1 << 37;
    const LONG: u64 = 256 << 20;

    #[test]
    fn an_extent_answers_inside_it_and_not_past_it() {
        let mut e = Extents::new();
        assert_eq!(e.put(LOW + 0x1000, 0x300, LOW, END, LONG), Put::Taken);
        assert_eq!(e.from(LOW + 0x1000), Some(0x300));
        assert_eq!(e.from(LOW + 0x1100), Some(0x200));
        assert_eq!(e.from(LOW + 0x12ff), Some(1));
        assert_eq!(e.from(LOW + 0x1300), None);
        assert_eq!(e.from(LOW + 0xfff), None);
        // Named again: nothing to do.
        assert_eq!(e.put(LOW + 0x1000, 0x300, LOW, END, LONG), Put::Known);
        assert_eq!(e.len(), 1);
    }

    #[test]
    fn memory_reused_for_another_structure_replaces_and_cuts() {
        let mut e = Extents::new();
        e.put(LOW, 0x1000, LOW, END, LONG);
        e.put(LOW + 0x2000, 0x100, LOW, END, LONG);
        e.put(LOW + 0x3000, 0x100, LOW, END, LONG);
        // A longer one from 0x800: the first is cut short at its start, the
        // two that begin inside it are gone.
        assert_eq!(e.put(LOW + 0x800, 0x3000, LOW, END, LONG), Put::Taken);
        assert_eq!(e.from(LOW), Some(0x800));
        assert_eq!(e.from(LOW + 0x7ff), Some(1));
        assert_eq!(e.from(LOW + 0x800), Some(0x3000));
        assert_eq!(e.from(LOW + 0x3000), Some(0x800));
        assert_eq!(e.len(), 2);
        // The same start with another length: replaced, not added.
        assert_eq!(e.put(LOW + 0x800, 0x40, LOW, END, LONG), Put::Taken);
        assert_eq!(e.from(LOW + 0x840), None);
        assert_eq!(e.len(), 2);
    }

    #[test]
    fn lengths_not_worth_believing_are_refused() {
        let mut e = Extents::new();
        // Below the title's memory, empty, too long, past the directory.
        assert_eq!(e.put(LOW - 0x100, 0x100, LOW, END, LONG), Put::Refused);
        assert_eq!(e.put(LOW, 0, LOW, END, LONG), Put::Refused);
        assert_eq!(e.put(LOW, LONG + 1, LOW, END, LONG), Put::Refused);
        assert_eq!(e.put(END - 0x100, 0x200, LOW, END, LONG), Put::Refused);
        assert_eq!(e.put(u64::MAX - 4, 0x10, LOW, u64::MAX, LONG), Put::Refused);
        assert_eq!(e.len(), 0);
        assert_eq!(e.put(END - 0x100, 0x100, LOW, END, LONG), Put::Taken);
    }

    #[test]
    fn past_the_most_all_are_let_go() {
        let mut e = Extents::new();
        for n in 0..MOST as u64 {
            e.put(LOW + n * 0x100, 0x100, LOW, END, LONG);
        }
        assert_eq!(e.len(), MOST);
        assert_eq!(e.put(LOW + MOST as u64 * 0x100, 0x100, LOW, END, LONG), Put::Cleared);
        assert_eq!(e.len(), 1);
        assert_eq!(e.from(LOW), None);
    }

    #[test]
    fn a_copy_takes_a_size_only_if_it_reads_as_one() {
        assert_eq!(compacted(0x400, 0x1000), Some(0x400));
        assert_eq!(compacted(0x1000, 0x1000), Some(0x1000));
        // Empty, longer than what it copies, not whole nodes.
        assert_eq!(compacted(0, 0x1000), None);
        assert_eq!(compacted(0x1040, 0x1000), None);
        assert_eq!(compacted(0x410, 0x1000), None);
    }
}
