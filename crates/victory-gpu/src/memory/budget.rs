//! Pages from fault notes as a budget that goes round, not a limit for the
//! run (`VICTORY_FAULT_MB`, see [`asked`]).
//!
//! The run-wide limit (2,048 pages, `FAULTS_ALL` in the module above) was
//! used up in the first minute of a ray-tracing run: "faults in all 2048"
//! and 18.3 pages a frame without a copy from then on (run
//! `prisao-rt-1126-153722`). What rays touch follows where the player goes,
//! so here at most so many arena pages are held because a shader found them
//! empty, and at the limit the oldest is given back for the new one. A
//! present page leaves no note, so whether it is still read is not known: if
//! it is, its note comes back the next frame, and it is made again first
//! (one frame of zeros there, counted: given back and wanted again).
//!
//! The notes come in two halves: the programs that walk leave theirs in the
//! first, the builder in the second. What the builder found empty it may
//! have stored in since, and that is on the card alone, so the walkers'
//! pages are given back first ([`goes_first`]).
//!
//! This module keeps only the bookkeeping, by page number and holding
//! serial; the module above holds the pages and asks it.
//!
//! Ported from another fork's commit (3e573d255), with the budget settable
//! and off by default.

use std::collections::{HashMap, VecDeque};

/// A page from a fault is not given back for so many frames after it came.
/// When more new pages are wanted than that lets through (all of the budget
/// in under so many frames: rays that go anywhere), the new ones wait,
/// instead of the pages in use going round at the frame's limit.
pub(super) const YOUNG: u64 = 128;
/// A page given back and wanted again within so many frames was still read
/// (a submission lands up to two frames late).
pub(super) const SOON: u64 = 4;

/// The budget in pages of `page` bytes that `VICTORY_FAULT_MB` asks for
/// (`mb`), unless `VICTORY_FAULT_BUDGET=0` (`budget`) keeps the run-wide
/// limit whatever it says. Unset, empty, 0 or not a number: none, the
/// run-wide limit as before.
pub(super) fn asked(mb: Option<&str>, budget: Option<&str>, page: u64) -> Option<usize> {
    if budget.is_some_and(|b| b.trim() == "0") {
        return None;
    }
    let mb = mb?.trim().parse::<u64>().ok().filter(|&mb| mb > 0)?;
    usize::try_from((mb << 20) / page).ok().filter(|&n| n > 0)
}

/// Which pages are held from faults, and which were given back.
pub(super) struct Budget {
    /// Arena pages held from faults, at most.
    pub(super) pages: usize,
    /// The pages held from faults (the walking programs', the builder's)
    /// with their holding serials, in the order they came; how many of them
    /// are still so (an entry whose page was named by a list since, or given
    /// back, stays in the queue until it comes to the front).
    order: [VecDeque<(u64, u64)>; 2],
    pub(super) held: [usize; 2],
    /// Pages given back for others: the frame, and whether a shader was seen
    /// to want the page again within [`SOON`] frames.
    given: HashMap<u64, (u64, bool)>,
    /// A bit per page a shader found empty, in all and since the last
    /// report; how many are set.
    wanted: [Vec<u64>; 2],
    pub(super) wanted_pages: [u64; 2],
}

impl Budget {
    /// `pages` held at most, of a directory of `directory` pages.
    pub(super) fn new(pages: usize, directory: usize) -> Budget {
        let words = directory.div_ceil(64);
        Budget {
            pages,
            order: Default::default(),
            held: [0; 2],
            given: HashMap::new(),
            wanted: [vec![0; words], vec![0; words]],
            wanted_pages: [0; 2],
        }
    }

    /// Pages held from faults, of both kinds.
    pub(super) fn total(&self) -> usize {
        self.held[0] + self.held[1]
    }

    /// Whether another may be held without one given back.
    pub(super) fn under(&self) -> bool {
        self.total() < self.pages
    }

    /// `page`, holding `serial`, is now held from a fault the builder
    /// (`builds`) or a walking program left.
    pub(super) fn took(&mut self, page: u64, serial: u64, builds: bool) {
        let kind = usize::from(builds);
        self.order[kind].push_back((page, serial));
        self.held[kind] += 1;
        self.given.remove(&page);
    }

    /// A page held from a fault of `kind` (0: a walking program's, 1: the
    /// builder's) is not so any more: named by a list, or let go.
    pub(super) fn left(&mut self, kind: usize) {
        self.held[kind] = self.held[kind].saturating_sub(1);
    }

    /// The oldest page held from a fault, to be given back, unless it is
    /// younger than [`YOUNG`] frames at `frame`. `live` answers for a page
    /// and serial whether that holding is still one from a fault, and the
    /// frame it came in. The caller lets the page go (and calls
    /// [`Budget::left`]).
    pub(super) fn give_back(&mut self, frame: u64, live: impl Fn(u64, u64) -> Option<u64>) -> Option<u64> {
        let kind = goes_first(self.held, self.pages);
        while let Some(&(page, serial)) = self.order[kind].front() {
            let Some(born) = live(page, serial) else {
                // Named by a list since, or gone.
                self.order[kind].pop_front();
                continue;
            };
            if frame.wrapping_sub(born) < YOUNG {
                return None;
            }
            self.order[kind].pop_front();
            self.given.insert(page, (frame, false));
            return Some(page);
        }
        None
    }

    /// A shader found `page` empty at `frame`: counted once in the bits;
    /// whether it was given back within [`SOON`] frames and is wanted again
    /// for the first time since.
    pub(super) fn wanted(&mut self, page: u64, frame: u64) -> bool {
        for i in 0..2 {
            self.wanted_pages[i] += u64::from(mark(&mut self.wanted[i], page));
        }
        match self.given.get_mut(&page) {
            Some(given) if !given.1 && frame.wrapping_sub(given.0) <= SOON => {
                given.1 = true;
                true
            }
            _ => false,
        }
    }

    /// The pages of a frame's notes in the order they are given places:
    /// those given back and wanted again first, since they are read; the
    /// rest as they were.
    pub(super) fn first(&self, pages: &mut [(u64, bool)]) {
        if !self.given.is_empty() {
            pages.sort_by_cached_key(|p| !self.given.contains_key(&p.0));
        }
    }

    /// The age in frames of the oldest page held from a fault.
    pub(super) fn oldest(&mut self, frame: u64, live: impl Fn(u64, u64) -> Option<u64>) -> u64 {
        let mut age = 0;
        for order in &mut self.order {
            while let Some(&(page, serial)) = order.front() {
                match live(page, serial) {
                    Some(born) => {
                        age = age.max(frame.wrapping_sub(born));
                        break;
                    }
                    None => {
                        order.pop_front();
                    }
                }
            }
        }
        age
    }

    /// At a report: the bits since the last one start again, what was given
    /// back long ago is not asked about any more, and queues with many stale
    /// entries are made short. The counts of pages wanted (since the last
    /// report, in all) before that.
    pub(super) fn tidy(&mut self, frame: u64, live: impl Fn(u64, u64) -> Option<u64>) -> [u64; 2] {
        let wanted = self.wanted_pages;
        self.wanted[1].fill(0);
        self.wanted_pages[1] = 0;
        self.given.retain(|_, given| frame.wrapping_sub(given.0) < 2 * YOUNG);
        for kind in 0..2 {
            if self.order[kind].len() > 2 * self.held[kind] + 4096 {
                self.order[kind].retain(|&(p, serial)| live(p, serial).is_some());
            }
        }
        wanted
    }
}

/// The notes of a frame (the page, whether the builder left it) in page
/// order, a page both left once, as the builder's.
pub(super) fn noted(mut raw: Vec<(u64, bool)>) -> Vec<(u64, bool)> {
    raw.sort_unstable();
    raw.dedup_by(|later, first| {
        let same = first.0 == later.0;
        first.1 |= same && later.1;
        same
    });
    raw
}

/// Whose page from a fault is given back first, of `held` (the walking
/// programs', the builder's) under a budget of `pages`: the walking
/// programs', which read the title's own bytes and get them again whole.
/// The builder's go only once they are more than half of the budget, or all
/// there are.
pub(super) fn goes_first(held: [usize; 2], pages: usize) -> usize {
    usize::from(held[0] == 0 || held[1] > pages / 2)
}

/// Sets `page`'s bit; whether it was not set.
fn mark(bits: &mut [u64], page: u64) -> bool {
    let Some(word) = bits.get_mut((page / 64) as usize) else {
        return false;
    };
    let bit = 1 << (page % 64);
    let new = *word & bit == 0;
    *word |= bit;
    new
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: u64 = 1 << 16;

    #[test]
    fn the_budget_is_off_unless_asked_for() {
        assert_eq!(asked(None, None, PAGE), None);
        assert_eq!(asked(Some(""), None, PAGE), None);
        assert_eq!(asked(Some("0"), None, PAGE), None);
        assert_eq!(asked(Some("lots"), None, PAGE), None);
        assert_eq!(asked(Some("256"), None, PAGE), Some(4096));
        assert_eq!(asked(Some(" 512 "), None, PAGE), Some(8192));
        // The run-wide limit whatever the budget says.
        assert_eq!(asked(Some("256"), Some("0"), PAGE), None);
        assert_eq!(asked(Some("256"), Some("1"), PAGE), Some(4096));
    }

    /// The holdings the module above would have: page to (serial, born,
    /// still from a fault).
    struct Held(HashMap<u64, (u64, u64, bool)>);

    impl Held {
        fn live(&self) -> impl Fn(u64, u64) -> Option<u64> + '_ {
            |page, serial| self.0.get(&page).filter(|h| h.0 == serial && h.2).map(|h| h.1)
        }
    }

    #[test]
    fn the_oldest_page_goes_back_and_not_before_it_is_old_enough() {
        let mut b = Budget::new(3, 1 << 10);
        let mut held = Held(HashMap::new());
        for (serial, page) in [(1, 0x1_0001), (2, 0x1_0002), (3, 0x1_0003)] {
            held.0.insert(page, (serial, serial, true));
            b.took(page, serial, false);
        }
        assert!(!b.under());
        // Still young: the new page waits.
        assert_eq!(b.give_back(100, held.live()), None);
        assert_eq!(b.give_back(YOUNG + 1, held.live()), Some(0x1_0001));
        held.0.remove(&0x1_0001);
        b.left(0);
        assert!(b.under());
        // The next is younger by a frame.
        assert_eq!(b.give_back(YOUNG + 1, held.live()), None);
        assert_eq!(b.give_back(YOUNG + 2, held.live()), Some(0x1_0002));
    }

    #[test]
    fn a_page_named_by_a_list_since_is_never_given_back() {
        let mut b = Budget::new(2, 1 << 10);
        let mut held = Held(HashMap::new());
        held.0.insert(0x1_0001, (1, 0, true));
        held.0.insert(0x1_0002, (2, 0, true));
        b.took(0x1_0001, 1, false);
        b.took(0x1_0002, 2, false);
        // Named by a list: no longer from a fault.
        held.0.get_mut(&0x1_0001).unwrap().2 = false;
        b.left(0);
        assert_eq!(b.give_back(1000, held.live()), Some(0x1_0002));
        // A page held again under another serial is not the one queued.
        held.0.insert(0x1_0003, (3, 0, true));
        b.took(0x1_0003, 3, false);
        held.0.insert(0x1_0003, (4, 0, true));
        assert_eq!(b.give_back(1000, held.live()), None);
    }

    #[test]
    fn a_page_given_back_and_still_read_is_made_again_first() {
        let mut b = Budget::new(1, 1 << 20);
        let held = Held(HashMap::from([(0x1_0001, (1, 0, true))]));
        b.took(0x1_0001, 1, false);
        assert_eq!(b.give_back(200, held.live()), Some(0x1_0001));
        // Its note comes back two frames later: wanted again, once.
        assert!(b.wanted(0x1_0001, 202));
        assert!(!b.wanted(0x1_0001, 203));
        // A page never given back is not.
        assert!(!b.wanted(0x1_0009, 202));
        // It goes before the others of the frame's notes.
        let mut pages = vec![(0x1_0000, false), (0x1_0001, false), (0x1_0002, true)];
        b.first(&mut pages);
        assert_eq!(pages, [(0x1_0001, false), (0x1_0000, false), (0x1_0002, true)]);
        // Made again: not one given back any more.
        b.took(0x1_0001, 2, false);
        let mut pages = vec![(0x1_0000, false), (0x1_0001, false)];
        b.first(&mut pages);
        assert_eq!(pages, [(0x1_0000, false), (0x1_0001, false)]);
    }

    #[test]
    fn wanted_long_after_it_was_given_back_is_not_counted_as_still_read() {
        let mut b = Budget::new(1, 1 << 20);
        let held = Held(HashMap::from([(0x1_0001, (1, 0, true))]));
        b.took(0x1_0001, 1, false);
        assert_eq!(b.give_back(200, held.live()), Some(0x1_0001));
        assert!(!b.wanted(0x1_0001, 200 + SOON + 1));
        // And forgotten at a report long after.
        let wanted = b.tidy(200 + 2 * YOUNG, held.live());
        assert_eq!(wanted, [1, 1]);
        assert_eq!(b.wanted_pages, [1, 0]);
        let mut pages = vec![(0x1_0000, false), (0x1_0001, false)];
        b.first(&mut pages);
        assert_eq!(pages[0].0, 0x1_0000);
    }

    #[test]
    fn the_walking_programs_pages_go_first() {
        assert_eq!(goes_first([100, 100], 8192), 0);
        assert_eq!(goes_first([1, 4096], 8192), 0);
        // The builder's once they are more than half, or all there are.
        assert_eq!(goes_first([100, 4097], 8192), 1);
        assert_eq!(goes_first([0, 3], 8192), 1);
        let mut b = Budget::new(2, 1 << 10);
        let held = Held(HashMap::from([(0x1_0001, (1, 0, true)), (0x1_0002, (2, 0, true))]));
        b.took(0x1_0001, 1, true);
        b.took(0x1_0002, 2, false);
        assert_eq!(b.give_back(1000, held.live()), Some(0x1_0002));
    }

    #[test]
    fn the_oldest_age_skips_what_is_not_held_from_a_fault() {
        let mut b = Budget::new(4, 1 << 10);
        let held = Held(HashMap::from([(0x1_0002, (2, 30, true))]));
        b.took(0x1_0001, 1, false);
        b.took(0x1_0002, 2, false);
        assert_eq!(b.oldest(100, held.live()), 70);
    }

    #[test]
    fn a_frames_notes_come_once_a_page_and_the_builders_stay_the_builders() {
        let notes = noted(vec![(0x2_0000, false), (0x10, true), (0x1_0005, true), (0x1_0005, false), (0x1_0004, false)]);
        assert_eq!(notes, [(0x10, true), (0x1_0004, false), (0x1_0005, true), (0x2_0000, false)]);
        assert_eq!(notes.partition_point(|p| p.0 < 0x1_0000), 1);
    }

    #[test]
    fn a_page_wanted_is_counted_once() {
        let mut bits = vec![0u64; 4];
        assert!(mark(&mut bits, 65));
        assert!(!mark(&mut bits, 65));
        assert!(mark(&mut bits, 64));
        assert_eq!(bits, [0, 3, 0, 0]);
        // Past the directory: not counted.
        assert!(!mark(&mut bits, 4 * 64));
    }
}
