//! The stall line (`VICTORY_STALL_LINE=0` leaves it out; on by default):
//! with the lines of every 5 seconds, what a scene's first seconds wait for.
//!
//! Every scene opens with soft textures and often with a stall: a sample of
//! 2 to 12 fps with 1.4 s of mapping calls in 10 s and 16,000 textures
//! waiting for their memory beside it. The other lines say how much in all;
//! this one says whether the time went in a few long calls or many short
//! ones, how long the oldest texture has waited, and of the longest frame of
//! the window what the thread that submits did with it. Counters only: a
//! clock read per mapping call and per present.

use std::cell::Cell;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::Instant;

/// Whether the line is written and its counters kept (on unless
/// `VICTORY_STALL_LINE=0`).
pub fn on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_STALL_LINE").is_none_or(|v| v != "0"))
}

/// The title's mapping calls since the last line: how many, their
/// nanoseconds, the longest one's.
static MAPPING: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];

/// Nanoseconds of the title's mapping calls since the start (a frame's
/// share is taken from it).
static MAPPING_ALL: AtomicU64 = AtomicU64::new(0);

thread_local! {
    /// Mapping calls under way on this thread: a call made inside another
    /// (one page of a scatter) is the outer one's time.
    static DEPTH: Cell<u32> = const { Cell::new(0) };
}

/// One of the title's mapping calls (`XMemMapPhysicalPages`, its scatter
/// form, the queue's `CopyPageMappingsBatchX`), counted when dropped.
pub struct Mapping(Option<(Instant, bool)>);

impl Mapping {
    pub fn begin() -> Mapping {
        if !on() {
            return Mapping(None);
        }
        let outer = DEPTH.with(|d| {
            let n = d.get();
            d.set(n + 1);
            n == 0
        });
        Mapping(Some((Instant::now(), outer)))
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        let Some((started, outer)) = self.0 else { return };
        DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
        if !outer {
            return;
        }
        let ns = started.elapsed().as_nanos() as u64;
        MAPPING[0].fetch_add(1, Relaxed);
        MAPPING[1].fetch_add(ns, Relaxed);
        MAPPING[2].fetch_max(ns, Relaxed);
        MAPPING_ALL.fetch_add(ns, Relaxed);
    }
}

/// What a frame's time went into, as the counters the present reads say
/// (microseconds): the submitting thread preparing its submissions and
/// waiting for the card in them, of the preparing the texture uploads; new
/// card memory made resident and waited for until it is mapped (on any
/// thread that made some); the title's mapping calls.
pub(crate) const PARTS: usize = 6;

/// The counters a present hands in, in [`PARTS`]' order; the first
/// [`RESET`] of them are set back to 0 by the frame-rate line every second
/// ([`swapped`]), the others only grow.
pub(crate) const RESET: usize = 3;

#[derive(Default)]
struct Frames {
    /// The last present, and the counters then.
    last: Option<(Instant, [u64; PARTS])>,
    frames: u64,
    /// Frames over [`LONG_MS`].
    long: u64,
    /// The longest frame: its milliseconds and what went into it.
    worst: Option<(f64, [u64; PARTS])>,
}

static FRAMES: Mutex<Frames> = Mutex::new(Frames { last: None, frames: 0, long: 0, worst: None });

/// A frame this long is a stall.
const LONG_MS: f64 = 100.0;

/// A frame was presented; `now` are the counters of [`PARTS`] but the last
/// (the mapping calls', kept here).
pub(crate) fn presented(now: [u64; PARTS - 1]) {
    if !on() {
        return;
    }
    let at = Instant::now();
    let mut counts = [0; PARTS];
    counts[..PARTS - 1].copy_from_slice(&now);
    counts[PARTS - 1] = MAPPING_ALL.load(Relaxed) / 1000;
    let mut f = FRAMES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((then, before)) = f.last {
        let ms = (at - then).as_secs_f64() * 1e3;
        let parts: [u64; PARTS] = std::array::from_fn(|i| counts[i].saturating_sub(before[i]));
        frame(&mut f, ms, parts);
    }
    f.last = Some((at, counts));
}

/// Counts a frame of `ms` into the window.
fn frame(f: &mut Frames, ms: f64, parts: [u64; PARTS]) {
    f.frames += 1;
    if ms > LONG_MS {
        f.long += 1;
    }
    if f.worst.is_none_or(|(worst, _)| ms > worst) {
        f.worst = Some((ms, parts));
    }
}

/// The frame-rate line set the first [`RESET`] counters back to 0.
pub(crate) fn swapped() {
    let mut f = FRAMES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, before)) = &mut f.last {
        before[..RESET].fill(0);
    }
}

/// The line, every 5 s: `waiting` textures picked by index waiting for their
/// memory now, the oldest of them `oldest` frames since its write.
pub(crate) fn line(seconds: f64, waiting: u64, oldest: u64) {
    if !on() {
        return;
    }
    let [calls, ns, longest] = [0, 1, 2].map(|i| MAPPING[i].swap(0, Relaxed));
    let (frames, long, worst) = {
        let mut f = FRAMES.lock().unwrap_or_else(|e| e.into_inner());
        (std::mem::take(&mut f.frames), std::mem::take(&mut f.long), f.worst.take())
    };
    tracing::info!("{}", text(seconds, [calls, ns, longest], waiting, oldest, frames, long, worst));
}

/// [`line`]'s words.
fn text(
    seconds: f64,
    [calls, ns, longest]: [u64; 3],
    waiting: u64,
    oldest: u64,
    frames: u64,
    long: u64,
    worst: Option<(f64, [u64; PARTS])>,
) -> String {
    let ms = |us: u64| us as f64 / 1e3;
    let worst = match worst {
        Some((frame_ms, [preparing, card, textures, resident, mapped, mapping])) => format!(
            "the longest {frame_ms:.0} ms: the submitting thread preparing {:.1} ms (textures {:.1}), waiting for the card \
             {:.1} ms, the rest {:.1} ms (the title's own work, or waits not timed here); new card memory made resident \
             {:.1} ms and waited for until mapped {:.1} ms; the title's mapping calls {:.1} ms",
            ms(preparing),
            ms(textures),
            ms(card),
            (frame_ms - ms(preparing) - ms(card)).max(0.0),
            ms(resident),
            ms(mapped),
            ms(mapping)
        ),
        None => "none presented".into(),
    };
    format!(
        "stalls in the last {seconds:.1} s (VICTORY_STALL_LINE=0 leaves this out): the title's mapping calls {calls}, \
         {:.0} ms in all, the longest {:.1} ms; textures picked by index waiting for their memory {waiting}, the oldest \
         for {oldest} frames; frames {frames}, {long} of them over {LONG_MS:.0} ms; {worst}",
        ns as f64 / 1e6,
        longest as f64 / 1e6
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A window of frames: those over 100 ms counted, the longest kept with
    /// what went into it, and the words the lead reads.
    #[test]
    fn the_longest_frame_and_the_stalls_are_counted() {
        let mut f = Frames::default();
        frame(&mut f, 33.0, [1000; PARTS]);
        frame(&mut f, 140.0, [20_000, 5_000, 3_000, 800, 400, 60_000]);
        frame(&mut f, 101.0, [0; PARTS]);
        frame(&mut f, 40.0, [0; PARTS]);
        assert_eq!((f.frames, f.long), (4, 2));
        let worst = f.worst.expect("a frame");
        assert_eq!(worst.0, 140.0);
        let said = text(5.0, [10, 2_500_000_000, 900_000_000], 16_000, 450, f.frames, f.long, Some(worst));
        assert!(said.contains("the title's mapping calls 10, 2500 ms in all, the longest 900.0 ms"), "{said}");
        assert!(said.contains("waiting for their memory 16000, the oldest for 450 frames"), "{said}");
        assert!(said.contains("frames 4, 2 of them over 100 ms"), "{said}");
        assert!(said.contains("the longest 140 ms: the submitting thread preparing 20.0 ms (textures 3.0), waiting for the card 5.0 ms, the rest 115.0 ms"), "{said}");
        assert!(said.contains("the title's mapping calls 60.0 ms"), "{said}");
    }

    /// Calls inside a call count once, as the outer one's time.
    #[test]
    fn a_call_inside_a_call_is_the_outer_ones_time() {
        let before = MAPPING_ALL.load(Relaxed);
        let calls = MAPPING[0].load(Relaxed);
        {
            let _outer = Mapping::begin();
            let _inner = Mapping::begin();
        }
        if on() {
            assert!(MAPPING[0].load(Relaxed) > calls);
            assert!(MAPPING_ALL.load(Relaxed) >= before);
            assert_eq!(DEPTH.with(Cell::get), 0);
        }
    }
}
