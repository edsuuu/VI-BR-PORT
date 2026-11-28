//! A line once a minute of what the relay copied back into title memory
//! (`crate::host::relay`), by kind, and of who waited for it
//! (`VICTORY_BACK_LINE=0` leaves it out; on by default, it only counts).
//!
//! The frame-rate line's "copying back" is one number, the relay thread's
//! time from the start of a landing to its ranges done with: the list of
//! pieces to fetch, the wait for the fetch from the card's memory and the
//! merges. Seen once a second it swings with whatever the second held, and it
//! does not say whether anybody stood behind it. What the title is told (its
//! fences, the bottom-of-pipe writes) is told only after a landing, so the
//! landing's length is how late the title hears of work the card has done;
//! the waits counted here are those of threads that asked for a landing to be
//! over (`Host::settle` with the relay: presents, the title's blocking fence
//! waits, the submitting thread's reads of memory flights may write).
//!
//! Everything is a relaxed atomic add, a few per landing: cheap enough for
//! the release build.

use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

/// The places of [`STATS`].
pub(crate) mod k {
    /// Landings, their microseconds in all, the longest one's.
    pub(crate) const LANDINGS: usize = 0;
    pub(crate) const LANDING_US: usize = 1;
    pub(crate) const LANDING_MAX_US: usize = 2;
    /// Copies in the card's memory: fetches, pieces, bytes fetched, copy
    /// regions recorded for them, the wait for the fetches, the merge, the
    /// bytes put into title memory, the list of pieces made.
    pub(crate) const FETCHES: usize = 3;
    pub(crate) const PIECES: usize = 4;
    pub(crate) const FETCHED: usize = 5;
    pub(crate) const REGIONS: usize = 6;
    pub(crate) const FETCH_US: usize = 7;
    pub(crate) const CARD_MERGE_US: usize = 8;
    pub(crate) const CARD_PUT: usize = 9;
    pub(crate) const GATHER_US: usize = 10;
    /// Copies in the PC's memory: looked through and merged, bytes put.
    pub(crate) const PC_US: usize = 11;
    pub(crate) const PC_PUT: usize = 12;
    /// Read-backs and telling the title, after the merges.
    pub(crate) const TELL_US: usize = 13;
    /// Waits for landings: at presents and the sweep, the title's blocking
    /// fence waits, the submitting thread's (memory flights may write, lists
    /// in the air, memory values); count and microseconds.
    pub(crate) const WAIT_PRESENT: usize = 14;
    pub(crate) const WAIT_PRESENT_US: usize = 15;
    pub(crate) const WAIT_FENCE: usize = 16;
    pub(crate) const WAIT_FENCE_US: usize = 17;
    pub(crate) const WAIT_SUBMIT: usize = 18;
    pub(crate) const WAIT_SUBMIT_US: usize = 19;
    /// Fetches whose merge began before the last part had come down
    /// (`VICTORY_FETCH_SPLIT`), and the microseconds of merging done while a
    /// part was still on its way.
    pub(crate) const SPLIT: usize = 20;
    pub(crate) const SPLIT_OVERLAP_US: usize = 21;
    pub(crate) const COUNT: usize = 22;
}

static STATS: [AtomicU64; k::COUNT] = [const { AtomicU64::new(0) }; k::COUNT];

/// Whether the line is made (`VICTORY_BACK_LINE=0`: not).
pub(crate) fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_BACK_LINE").is_none_or(|v| v != "0"))
}

pub(crate) fn add(which: usize, n: u64) {
    STATS[which].fetch_add(n, Ordering::Relaxed);
}

pub(crate) fn most(which: usize, n: u64) {
    STATS[which].fetch_max(n, Ordering::Relaxed);
}

/// Microseconds since `from`.
pub(crate) fn us(from: std::time::Instant) -> u64 {
    from.elapsed().as_micros() as u64
}

/// A wait for landings of `us` microseconds: `kind` is one of the `WAIT_`
/// counts (its microseconds are the next place).
pub(crate) fn waited(kind: usize, us: u64) {
    add(kind, 1);
    add(kind + 1, us);
}

/// The frames and the time since the last line.
static SINCE: Mutex<Option<(std::time::Instant, u64)>> = Mutex::new(None);

/// Called with every frame-rate line, `frames` the frames it counted: makes
/// the line once a minute.
pub(crate) fn tick(frames: u64) {
    if !on() {
        return;
    }
    let (elapsed, frames) = {
        let mut since = SINCE.lock().unwrap_or_else(|e| e.into_inner());
        let (from, counted) = since.get_or_insert((std::time::Instant::now(), 0));
        *counted += frames;
        let elapsed = from.elapsed();
        if elapsed < std::time::Duration::from_secs(60) {
            return;
        }
        let counted = *counted;
        *since = Some((std::time::Instant::now(), 0));
        (elapsed, counted)
    };
    let s: Vec<u64> = STATS.iter().map(|s| s.swap(0, Ordering::Relaxed)).collect();
    tracing::info!("{}", line(&s, elapsed.as_secs_f64(), frames));
}

/// The line, of the counts `s` over `secs` seconds and `frames` frames.
fn line(s: &[u64], secs: f64, frames: u64) -> String {
    let per_frame = |v: u64| v as f64 / frames.max(1) as f64;
    let ms = |v: u64| per_frame(v) / 1000.0;
    let mb = |v: u64| v as f64 / (1 << 20) as f64;
    let landings = s[k::LANDINGS].max(1) as f64;
    format!(
        "copied back in the last {secs:.0} s ({frames} frames; VICTORY_BACK_LINE=0 leaves this out), per frame: \
         copies in the card's memory {:.1} fetches, {:.0} pieces {:.1} MB fetched in {:.0} copy regions \
         (VICTORY_FETCH_JOIN), {:.2} ms choosing them, {:.2} ms waiting for the fetches, {:.2} ms merging, {:.1} MB put \
         ({:.1} fetches merged in parts while the rest came down, {:.2} ms of merging beside the fetch: VICTORY_FETCH_SPLIT); \
         copies in the PC's memory {:.2} ms looked through and merged, {:.1} MB put; read-backs and telling the title \
         {:.2} ms; the relay thread {:.2} ms landing in {:.1} landings, {:.1} ms each, the longest {:.1} ms (the title's \
         fences and bottom-of-pipe writes are told only after one); in the minute {:.0} MB fetched, {:.0} MB put. \
         Who waited for landings, per frame: presents and the sweep {:.2} times {:.2} ms, the title's blocking fence waits \
         {:.2} times {:.2} ms, the submitting thread {:.2} times {:.2} ms",
        per_frame(s[k::FETCHES]),
        per_frame(s[k::PIECES]),
        per_frame(s[k::FETCHED]) / (1 << 20) as f64,
        per_frame(s[k::REGIONS]),
        ms(s[k::GATHER_US]),
        ms(s[k::FETCH_US]),
        ms(s[k::CARD_MERGE_US]),
        per_frame(s[k::CARD_PUT]) / (1 << 20) as f64,
        per_frame(s[k::SPLIT]),
        ms(s[k::SPLIT_OVERLAP_US]),
        ms(s[k::PC_US]),
        per_frame(s[k::PC_PUT]) / (1 << 20) as f64,
        ms(s[k::TELL_US]),
        ms(s[k::LANDING_US]),
        per_frame(s[k::LANDINGS]),
        s[k::LANDING_US] as f64 / 1000.0 / landings,
        s[k::LANDING_MAX_US] as f64 / 1000.0,
        mb(s[k::FETCHED]),
        mb(s[k::CARD_PUT] + s[k::PC_PUT]),
        per_frame(s[k::WAIT_PRESENT]),
        ms(s[k::WAIT_PRESENT_US]),
        per_frame(s[k::WAIT_FENCE]),
        ms(s[k::WAIT_FENCE_US]),
        per_frame(s[k::WAIT_SUBMIT]),
        ms(s[k::WAIT_SUBMIT_US]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_line_is_per_frame_and_survives_no_frames() {
        let mut s = [0u64; k::COUNT];
        s[k::FETCHES] = 60;
        s[k::FETCHED] = 30 << 20;
        s[k::LANDINGS] = 4;
        s[k::LANDING_US] = 8_000;
        s[k::LANDING_MAX_US] = 3_500;
        s[k::WAIT_FENCE] = 2;
        s[k::WAIT_FENCE_US] = 1_000;
        let line = line(&s, 60.0, 30);
        assert!(line.contains("2.0 fetches"), "{line}");
        assert!(line.contains("1.0 MB fetched"), "{line}");
        assert!(line.contains("2.0 ms each, the longest 3.5 ms"), "{line}");
        assert!(line.contains("in the minute 30 MB fetched"), "{line}");
        assert!(line.contains("blocking fence waits 0.07 times 0.03 ms"), "{line}");
        // No frames and no landings: no division by zero.
        let empty = super::line(&[0; k::COUNT], 60.0, 0);
        assert!(empty.contains("0.0 fetches"), "{empty}");
    }
}
