//! Diagnostics: a timeline of a few frames. `VICTORY_TIMELINE=<file>` writes
//! one, from the first present after `VICTORY_TIMELINE_AT` seconds (60) on
//! and for `VICTORY_TIMELINE_FRAMES` frames (3);
//! `VICTORY_TIMELINE=<from s>[+<from s>..],<frames>,<file>` one for each of
//! the times (each in a file of its own, the second in its name): a Chrome trace (JSON, for
//! `chrome://tracing` or Perfetto) and beside it a text summary (`.txt`)
//! with the chain of work and waits each frame's present came out of.
//!
//! What is recorded while it runs, by every thread:
//! - spans of ours ([`span`]): the title's calls, the submitting thread's
//!   stages, the relay's, the pools' jobs, texture and streaming work;
//! - waits and the wake-ups that end them ([`hooks`]): the functions the
//!   title's threads wait with (its imports are bound to wrappers) and those
//!   our own code does (the entries of our own import table: every contended
//!   lock, condition and park of the standard library is a `WaitOnAddress`),
//!   each with the thread that woke it and where it waited;
//! - kernel calls on the address space that took long;
//! - the card: when each submission began and ended on it (timestamps).
//!
//! Without the variable nothing is wrapped and a span is one load of a
//! flag; a build without `diagnostics` has nothing of it.

#[cfg(all(windows, feature = "diagnostics"))]
mod hooks;
#[cfg(all(windows, feature = "diagnostics"))]
mod record;
#[cfg(all(windows, feature = "diagnostics"))]
mod report;

#[cfg(all(windows, feature = "diagnostics"))]
pub use hooks::import;
#[cfg(all(windows, feature = "diagnostics"))]
pub use record::{Span, span};
#[cfg(all(windows, feature = "diagnostics"))]
pub(crate) use record::{called, card_wait, on, polled, presented, sent, ticks, title_thread, told};

/// A span of ours: from where it is made until it is dropped. In a build
/// without `diagnostics` it is nothing, unless the thread is timed
/// ([`time_this_thread`]): then its time goes to its name's sum.
#[cfg(not(all(windows, feature = "diagnostics")))]
pub struct Span(Option<(&'static str, std::time::Instant)>);

#[cfg(not(all(windows, feature = "diagnostics")))]
impl Span {
    /// The span ends here and another begins.
    #[inline(always)]
    pub fn next(&mut self, name: &'static str) {
        if let Some((was, at)) = self.0.take() {
            let now = std::time::Instant::now();
            clock::add(was, now - at);
            self.0 = Some((name, now));
        }
    }

    /// The span ends here.
    #[inline(always)]
    pub fn end(self) {}
}

#[cfg(not(all(windows, feature = "diagnostics")))]
impl Drop for Span {
    #[inline(always)]
    fn drop(&mut self) {
        if let Some((name, at)) = self.0.take() {
            clock::add(name, at.elapsed());
        }
    }
}

/// A span named `name`.
#[cfg(not(all(windows, feature = "diagnostics")))]
#[inline(always)]
pub fn span(name: &'static str) -> Span {
    Span(clock::timed().then(|| (name, std::time::Instant::now())))
}

#[cfg(not(all(windows, feature = "diagnostics")))]
pub(crate) use clock::{split_line, time_this_thread};

/// Without `diagnostics`: the spans of the thread that prepares and sends
/// submissions, timed by name (`VICTORY_PREP_SPLIT=1`; off by default), for
/// a line beside the frame-rate line.
///
/// Why: that thread is what holds the frame in the prison (28 Nov, run
/// prisao-pfC1: 23 ms a frame at 2,800 draws, 40 at 5,300), and a release
/// build says only its "preparing" and the time ahead of it; the split of
/// the diagnostics build is of a slower build. Two clock reads a span, some
/// 30 spans a submission and 25 submissions a frame.
#[cfg(not(all(windows, feature = "diagnostics")))]
mod clock {
    use std::cell::Cell;
    use std::sync::Mutex;

    thread_local! {
        static TIMED: Cell<bool> = const { Cell::new(false) };
    }

    fn on() -> bool {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ON.get_or_init(|| crate::switch::on("VICTORY_PREP_SPLIT"))
    }

    /// The calling thread's spans are timed, if the switch is on.
    pub(crate) fn time_this_thread() {
        if on() {
            TIMED.with(|t| t.set(true));
        }
    }

    #[inline(always)]
    pub(super) fn timed() -> bool {
        TIMED.with(Cell::get)
    }

    /// By name: nanoseconds and spans, since the last line.
    static SUMS: Mutex<Vec<(&'static str, u64, u64)>> = Mutex::new(Vec::new());

    pub(super) fn add(name: &'static str, took: std::time::Duration) {
        let ns = took.as_nanos() as u64;
        let mut sums = SUMS.lock().unwrap_or_else(|e| e.into_inner());
        match sums.iter_mut().find(|s| std::ptr::eq(s.0, name) || s.0 == name) {
            Some(s) => {
                s.1 += ns;
                s.2 += 1;
            }
            None => sums.push((name, ns, 1)),
        }
    }

    /// The sums per frame, the longest first; a span inside another is in
    /// both.
    pub(crate) fn split_line(frames: f64) {
        if !on() || frames <= 0.0 {
            return;
        }
        let mut sums = std::mem::take(&mut *SUMS.lock().unwrap_or_else(|e| e.into_inner()));
        if sums.is_empty() {
            return;
        }
        sums.sort_by_key(|s| std::cmp::Reverse(s.1));
        let parts: Vec<String> = sums
            .iter()
            .map(|(name, ns, n)| format!("{name} {:.2} ({:.0})", *ns as f64 / 1e6 / frames, *n as f64 / frames))
            .collect();
        tracing::info!(
            "the submitting thread's spans per frame, ms (how many), a span inside another counted in both (VICTORY_PREP_SPLIT): {}",
            parts.join(", ")
        );
    }
}

/// With `diagnostics` the timeline has the spans ([`record`]).
#[cfg(all(windows, feature = "diagnostics"))]
pub(crate) fn time_this_thread() {}

/// With `diagnostics` the timeline has the spans ([`record`]).
#[cfg(all(windows, feature = "diagnostics"))]
pub(crate) fn split_line(_frames: f64) {}
