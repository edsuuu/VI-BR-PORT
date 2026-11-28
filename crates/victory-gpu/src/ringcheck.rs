//! Diagnostics: the records the title's ring allocator still holds, looked at
//! from a thread of ours (`VICTORY_RING_CHECK=<rva>`, hex; off when unset).
//!
//! Why: the title falls at `+0x4e9b54a`/`+0x4e9b578` reading the object
//! pointer of such a record, which something had smashed while the record was
//! in use. `crate::smash`'s watch of block ends saw the qword where a block's
//! last record keeps that pointer change some 8,000 times a run (barco-q-w1
//! to w3) with no write of ours there and no crash: the title hands a block
//! out again without a record, and the block's last bytes then hold its data.
//! A record is only wrong while a block's list still holds it, so this walks
//! the lists, by the allocator's own globals, and is silent otherwise.
//!
//! `<rva>`: where in the title's image the allocator keeps, one after the
//! other, the first block's address (8 bytes), its blocks' descriptors (8:
//! an array of 0x58 bytes each, the list of records at +0x48), how many
//! blocks (4) and their size (4). In the build of 27 Nov (930.048.020) that is
//! `0x75df1b8` (`DAT_1475df1b8`, written by `FUN_14247ac20`). A record is
//! next (8), object (8), count (4).
//!
//! Every [`EVERY_MS`] ms each list is walked. A record whose object is no
//! pointer into the 4 GiB the descriptors lie in, two looks in a row (the
//! title links a record inside a block before it writes its object), is
//! logged once per value, with what it held when last right and which path of
//! ours wrote its 64 KiB last (`crate::smash::touched`, kept from here on for
//! the whole ring), and so is a link out of the ring's blocks. Reads go
//! through `crate::safecopy`: memory that is not there is skipped.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

/// How often the lists are walked.
const EVERY_MS: u64 = 5;
/// A block's descriptor, and where in it its list of records begins.
const DESCRIPTOR: usize = 0x58;
const LIST: usize = 0x48;
/// A record's bytes.
const RECORD: usize = 0x18;
/// Bounds against garbage: blocks, and records in one list.
const MOST_BLOCKS: usize = 1 << 16;
const MOST_RECORDS: usize = 64;

/// The switch's value: the allocator's globals' place in the title's image.
fn rva() -> Option<usize> {
    static RVA: OnceLock<Option<usize>> = OnceLock::new();
    *RVA.get_or_init(|| {
        let v = std::env::var("VICTORY_RING_CHECK").ok()?;
        let v = v.trim();
        let v = v.strip_prefix("0x").or_else(|| v.strip_prefix("0X")).unwrap_or(v);
        usize::from_str_radix(v, 16).ok().filter(|&rva| rva != 0)
    })
}

static IMAGE: AtomicUsize = AtomicUsize::new(0);

/// The title's main image is mapped at `base` (`victory-hle`'s runtime): with
/// the switch, the walks begin (once the allocator has set its globals).
pub(crate) fn title_image(base: usize) {
    let Some(rva) = rva() else {
        return;
    };
    if IMAGE.swap(base, Ordering::AcqRel) != 0 {
        return;
    }
    let _ = std::thread::Builder::new().name("victory-ring-check".into()).spawn(move || run(base + rva));
}

fn read_u64(at: usize) -> Option<u64> {
    let mut b = [0u8; 8];
    // SAFETY: into our own array; memory that is not there fails the copy.
    unsafe { crate::safecopy::copy(b.as_mut_ptr(), at as *const u8, 8) }.then(|| u64::from_le_bytes(b))
}

fn read_u32(at: usize) -> Option<u32> {
    let mut b = [0u8; 4];
    // SAFETY: as in `read_u64`.
    unsafe { crate::safecopy::copy(b.as_mut_ptr(), at as *const u8, 4) }.then(|| u32::from_le_bytes(b))
}

/// The allocator's globals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Ring {
    pub(crate) base: usize,
    pub(crate) descriptors: usize,
    pub(crate) blocks: usize,
    pub(crate) size: usize,
}

impl Ring {
    /// The globals at `at`, once the allocator has set them.
    pub(crate) fn at(at: usize) -> Option<Ring> {
        let ring = Ring {
            base: read_u64(at)? as usize,
            descriptors: read_u64(at + 8)? as usize,
            blocks: read_u32(at + 0x10)? as usize,
            size: read_u32(at + 0x14)? as usize,
        };
        let sane = ring.base != 0
            && ring.descriptors != 0
            && (1..=MOST_BLOCKS).contains(&ring.blocks)
            && ring.size >= 4096
            && ring.size.is_power_of_two();
        sane.then_some(ring)
    }

    fn end(&self) -> usize {
        self.base + self.blocks * self.size
    }
}

/// Whether `object` can be a record's object: what looks like a pointer
/// into title memory, in the 4 GiB the descriptors lie in (the objects are
/// a pool beside them: `0x9_0418_xxxx` in every minidump).
pub(crate) fn object_ok(object: u64, descriptors: usize) -> bool {
    crate::smash::pointer_like(object) && object >> 32 == descriptors as u64 >> 32
}

/// What a walk found wrong.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Wrong {
    /// A record in use whose object is no pointer.
    Object { record: usize, block: usize, object: u64 },
    /// A link (from a descriptor or a record) out of the ring's blocks.
    Link { from: usize, block: usize, to: usize },
}

/// One walk of every list. `good`: per record, the last object seen right
/// and when (milliseconds); `ok` says what is right.
pub(crate) fn walk(ring: &Ring, good: &mut HashMap<usize, (u64, u64)>, now: u64, ok: impl Fn(u64) -> bool) -> Vec<Wrong> {
    let end = ring.end();
    let mut wrong = Vec::new();
    for block in 0..ring.blocks {
        let head = ring.descriptors + block * DESCRIPTOR + LIST;
        let Some(first) = read_u64(head) else {
            continue;
        };
        let (mut from, mut record) = (head, first as usize);
        for _ in 0..MOST_RECORDS {
            if record == 0 {
                break;
            }
            if record < ring.base || record + RECORD > end || !record.is_multiple_of(8) {
                wrong.push(Wrong::Link { from, block, to: record });
                break;
            }
            let (Some(next), Some(object)) = (read_u64(record), read_u64(record + 8)) else {
                break;
            };
            if ok(object) {
                good.insert(record, (object, now));
            } else {
                wrong.push(Wrong::Object { record, block, object });
            }
            (from, record) = (record, next as usize);
        }
    }
    wrong
}

/// The thread: see the file.
fn run(globals: usize) {
    if !crate::safecopy::on() {
        tracing::warn!("ring check (VICTORY_RING_CHECK): off, its reads need VICTORY_SAFE_COPY");
        return;
    }
    let ring = loop {
        if let Some(ring) = Ring::at(globals) {
            break ring;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    crate::smash::touch_range(ring.base, ring.blocks * ring.size);
    tracing::info!(
        base = format_args!("{:#x}", ring.base),
        blocks = ring.blocks,
        size = ring.size,
        descriptors = format_args!("{:#x}", ring.descriptors),
        every_ms = EVERY_MS,
        "ring check (VICTORY_RING_CHECK): the title's ring allocator's records in use are looked at"
    );
    let mut good: HashMap<usize, (u64, u64)> = HashMap::new();
    let mut before: HashSet<Wrong> = HashSet::new();
    let mut told: HashSet<Wrong> = HashSet::new();
    let (mut found, mut walks) = (0u64, 0u64);
    loop {
        std::thread::sleep(std::time::Duration::from_millis(EVERY_MS));
        walks += 1;
        let now = crate::smash::ms();
        let wrong = walk(&ring, &mut good, now, |object| object_ok(object, ring.descriptors));
        for w in &wrong {
            // (Twice in a row: a record linked and not yet written is not.)
            if !before.contains(w) || !told.insert(*w) {
                continue;
            }
            found += 1;
            let at = match *w {
                Wrong::Object { record, .. } => record + 8,
                Wrong::Link { from, .. } => from,
            };
            let ours = crate::smash::last_touch(at)
                .map_or_else(|| "none since the check began".to_string(), |(p, ago)| format!("{} {ago} ms ago", p.name()));
            let event = crate::smash::event_at(at)
                .map_or_else(|| "none".to_string(), |e| format!("{} {} ms ago", e.path.name(), now.saturating_sub(e.ms)));
            match *w {
                Wrong::Object { record, block, object } => {
                    let was = good.get(&record).map_or_else(
                        || "never seen right".to_string(),
                        |&(o, when)| format!("{o:#018x} {} ms before", now.saturating_sub(when)),
                    );
                    tracing::warn!(
                        "ring check (VICTORY_RING_CHECK): a record in use at {record:#x} (block {block}, {:#x} into it) holds the \
                         object {object:#018x}, no pointer of the descriptors' 4 GiB; when last right: {was}; our last write in its \
                         64 KiB: {ours}; an event of ours there (VICTORY_SMASH_WATCH): {event}; frame {} ({found} found in {walks} walks)",
                        record - ring.base - block * ring.size,
                        crate::frame::current(),
                    );
                }
                Wrong::Link { from, block, to } => {
                    tracing::warn!(
                        "ring check (VICTORY_RING_CHECK): the list of block {block} goes from {from:#x} to {to:#x}, outside the ring's \
                         blocks; our last write in that 64 KiB: {ours}; frame {} ({found} found in {walks} walks)",
                        crate::frame::current(),
                    );
                }
            }
        }
        before = wrong.into_iter().collect();
        if told.len() > 1 << 16 {
            told.clear();
        }
        if good.len() > 1 << 20 {
            good.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_object_is_right_only_as_a_pointer_beside_the_descriptors() {
        let descriptors = 0x9_0418_7e60;
        assert!(object_ok(0x9_0418_cf18, descriptors));
        // The six crashes' values.
        for bad in [0x1_0418_cf18u64, 0x49_0418_d5a8, 0x1_0000_0000, 0x9_0000_0000, 0x9_0041_8e9f] {
            assert!(!object_ok(bad, descriptors), "{bad:#x}");
        }
    }

    #[test]
    fn a_walk_finds_the_smashed_record_in_use_and_a_link_out_of_the_ring() {
        if !crate::safecopy::on() {
            return;
        }
        const SIZE: usize = 4096;
        let mut heap = vec![0u8; 2 * SIZE + 4096];
        let base = (heap.as_mut_ptr() as usize).next_multiple_of(4096);
        let mut descriptors = vec![0u8; 2 * DESCRIPTOR];
        let d = descriptors.as_mut_ptr() as usize;
        let ring = Ring { base, descriptors: d, blocks: 2, size: SIZE };
        let put = |at: usize, v: u64| {
            // SAFETY: inside `heap` or `descriptors`, both alive below.
            unsafe { (at as *mut u64).write_unaligned(v) }
        };
        let (good_object, bad_object) = (0x1000u64, 0x2000u64);
        // Block 0: its end record. Block 1: a record inside, then the end one.
        let (end0, mid1, end1) = (base + SIZE - RECORD, base + SIZE + 0x100, base + 2 * SIZE - RECORD);
        put(end0, 0);
        put(end0 + 8, good_object);
        put(mid1, end1 as u64);
        put(mid1 + 8, good_object);
        put(end1, 0);
        put(end1 + 8, bad_object);
        put(d + LIST, end0 as u64);
        put(d + DESCRIPTOR + LIST, mid1 as u64);
        let ok = |o: u64| o == good_object;
        let mut good = HashMap::new();
        let wrong = walk(&ring, &mut good, 7, ok);
        assert_eq!(wrong, [Wrong::Object { record: end1, block: 1, object: bad_object }]);
        assert_eq!(good.get(&mid1), Some(&(good_object, 7)));
        // A link out of the blocks.
        put(d + LIST, (base + 4 * SIZE) as u64);
        let wrong = walk(&ring, &mut good, 8, ok);
        assert!(wrong.contains(&Wrong::Link { from: d + LIST, block: 0, to: base + 4 * SIZE }));
        drop((heap, descriptors));
    }

    #[test]
    fn the_globals_are_taken_once_they_are_set() {
        if !crate::safecopy::on() {
            return;
        }
        let mut globals = [0u64; 3];
        let at = globals.as_mut_ptr() as usize;
        assert_eq!(Ring::at(at), None);
        // SAFETY: inside `globals`.
        unsafe {
            (at as *mut u64).write_unaligned(0x12b0000);
            ((at + 8) as *mut u64).write_unaligned(0x9_0418_7e60);
            ((at + 0x10) as *mut u32).write_unaligned(1460);
            ((at + 0x14) as *mut u32).write_unaligned(0x20000);
        }
        assert_eq!(Ring::at(at), Some(Ring { base: 0x12b0000, descriptors: 0x9_0418_7e60, blocks: 1460, size: 0x20000 }));
    }
}
