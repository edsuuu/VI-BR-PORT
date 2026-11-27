//! The tail of a submission sent ahead on a thread of its own
//! (`VICTORY_SEND_THREAD=1`; off by default).
//!
//! A submission sent ahead (`Host::submit` with `late`) ended, on the
//! submitting thread, with ExecuteCommandLists of its lists, the Signal of its
//! fence value, `awake::sent`, the timeline's note and `Host::card_sent`. With
//! the switch that tail is handed to one thread of ours, `victory-send`, and
//! the submitting thread goes on to the flight's bookkeeping and the next
//! submission. Without it the same [`Job::run`] is called where the calls
//! were, so the old behaviour is this code.
//!
//! No bytes move here: by the time a job exists the submission's sends are
//! taken (`send::take`), its refreshes and fills are done and its lists are
//! closed. The thread only puts them on the queue.
//!
//! What keeps the card seeing what it saw before:
//!
//! 1. The fence value is reserved by the submitting thread (`*submitted` is
//!    counted up under the submission lock) and the flight goes into the
//!    line from the submitting thread, as before, before the next submission
//!    looks at it. The value is not completed until this thread has signalled
//!    it, and every wait and every landing goes by completion.
//! 2. Whatever else puts work on the host's queue, or signals the host's
//!    fence, waits first for the jobs handed over so far ([`wait_idle`]):
//!    the sync and compare lists of copies in the card's memory
//!    (`card_before`, `card_cover`), the fetch of `card_back` and the
//!    relay's fetch on the drawing queue, a synchronous submission, the
//!    calls that must go to the queue in order with the lists
//!    (`Host::queue`, the tiles' mappings), and the presents. So the order of
//!    the queue is the order the calls were made in, and the fence is never
//!    signalled with a value below one signalled before. The relay's fetch
//!    reads the line's target first and waits after: every flight at or
//!    below it was handed over before, and is on the queue by then (read the
//!    other way round, it could claim to run behind a flight it overtook).
//!    The present waits, so the frame's copy into the back buffer is on the
//!    queue before Present (without that the copy writes a buffer the
//!    compositor holds, and the device is removed: 0x887A002B, Kemerd's
//!    421dfacb1).
//! 3. The thread always signals: a job's fence value is signalled whatever
//!    happened before (an error, a panic), so nothing that waits for a flight
//!    waits for good.
//!
//! The thread takes no lock of the host's but the card clock's (in
//! `card_sent`), which nobody holds while waiting; [`wait_idle`] may be called
//! with the submission lock, `QUEUE_ORDER`, `CARD_WORK` or the heat book
//! held. Ported from Kemerd/VIctory-NVIDIA-RT e994b9005 (step 5a), cdef33923
//! and 421dfacb1.

use std::collections::VecDeque;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::Instant;

use super::*;

/// Whether submissions sent ahead are sent by [`hand_over`]'s thread
/// (`VICTORY_SEND_THREAD=1`, only with `late()`; off by default). Read once.
pub(super) fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| late() && crate::switch::on("VICTORY_SEND_THREAD"))
}

/// The tail of a submission sent ahead: its closed lists and the fence value
/// to signal behind them.
pub(super) struct Job {
    /// The lists in the order they go to the queue.
    lists: Vec<Option<ID3D12CommandList>>,
    /// The places in `lists` where another ExecuteCommandLists call begins,
    /// after the first (`cuts` in `Host::submit`).
    cuts: Vec<usize>,
    /// The value signalled behind the lists.
    fence: u64,
    /// Set once the value was signalled, so that a panic after that does not
    /// signal it again.
    signalled: bool,
    /// Ticks before sending and the stamp place (the arguments of
    /// `crate::timeline::sent`).
    #[cfg(feature = "diagnostics")]
    timeline: Option<(u64, Option<usize>)>,
}

// SAFETY: closed lists of free-threaded D3D12 objects, which nothing records
// into again before they have run.
unsafe impl Send for Job {}

impl Job {
    /// A job for `lists` (cut where `cuts` say), followed by `fence`.
    pub(super) fn new(lists: Vec<Option<ID3D12CommandList>>, cuts: Vec<usize>, fence: u64) -> Job {
        Job {
            lists,
            cuts,
            fence,
            signalled: false,
            #[cfg(feature = "diagnostics")]
            timeline: None,
        }
    }

    /// Notes what the timeline records when the lists are sent.
    #[cfg(feature = "diagnostics")]
    pub(super) fn timed(mut self, ticks: u64, stamp: Option<usize>) -> Job {
        self.timeline = Some((ticks, stamp));
        self
    }

    /// Sends the lists, signals the fence and notes it, in the order
    /// `Host::submit` always did. The signal's result is returned; the caller
    /// fails the run on an error.
    pub(super) fn run(&mut self, host: &Host) -> windows::core::Result<()> {
        let began = Instant::now();
        // SAFETY: closed lists; the fence is ours.
        let signal = unsafe {
            let mut from = 0;
            for to in self.cuts.iter().copied().chain([self.lists.len()]) {
                host.queue.ExecuteCommandLists(&self.lists[from..to]);
                from = to;
            }
            host.queue.Signal(&host.fence, self.fence)
        };
        self.signalled = true;
        awake::sent(&host.device);
        #[cfg(feature = "diagnostics")]
        if let Some((ticks, stamp)) = self.timeline {
            crate::timeline::sent(ticks, self.fence, stamp);
        }
        host.card_sent();
        STATS[0].fetch_add(1, Ordering::Relaxed);
        STATS[1].fetch_add(began.elapsed().as_nanos() as u64, Ordering::Relaxed);
        signal
    }
}

/// Sends `job` on the thread with the switch on, else here. The result is the
/// signal's when sent here, and Ok when handed over (the thread fails the run
/// itself on an error).
pub(super) fn send(host: &Host, mut job: Job) -> windows::core::Result<()> {
    if !on() {
        return job.run(host);
    }
    match hand_over(job) {
        Ok(()) => Ok(()),
        // No thread: sent here, as without the switch.
        Err(mut job) => job.run(host),
    }
}

/// What a sending thread and its waiters share: jobs in the order they were
/// handed over, and how many of them are done.
struct Sends<T> {
    state: Mutex<State<T>>,
    /// The thread waits here for a job.
    work: Condvar,
    /// Threads in [`Sends::wait_idle`] wait here for `done` to reach `handed`.
    idle: Condvar,
}

struct State<T> {
    /// Jobs not taken by the thread yet, in the order they were handed over.
    jobs: VecDeque<T>,
    /// Jobs handed over so far.
    handed: u64,
    /// Jobs the thread is done with (sent, or failed and signalled).
    done: u64,
}

impl<T> Sends<T> {
    const fn new() -> Self {
        Sends { state: Mutex::new(State { jobs: VecDeque::new(), handed: 0, done: 0 }), work: Condvar::new(), idle: Condvar::new() }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State<T>> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Queues `job` and wakes the thread.
    fn hand(&self, job: T) {
        let mut state = self.state();
        state.jobs.push_back(job);
        state.handed += 1;
        drop(state);
        self.work.notify_one();
    }

    /// The next job, waiting for one (the thread).
    fn next(&self) -> T {
        let mut state = self.state();
        loop {
            if let Some(job) = state.jobs.pop_front() {
                return job;
            }
            state = self.work.wait(state).unwrap_or_else(|e| e.into_inner());
        }
    }

    /// The thread is done with the job it took last.
    fn finished(&self) {
        self.state().done += 1;
        self.idle.notify_all();
    }

    /// Returns once every job handed over so far is done; how long it waited,
    /// None if nothing was outstanding.
    fn wait_idle(&self) -> Option<std::time::Duration> {
        let mut state = self.state();
        if state.handed == state.done {
            return None;
        }
        let began = Instant::now();
        while state.handed != state.done {
            state = self.idle.wait(state).unwrap_or_else(|e| e.into_inner());
        }
        Some(began.elapsed())
    }
}

static SENDS: Sends<Job> = Sends::new();

/// Since the last frame-rate line: [0] jobs run (here or on the thread), [1]
/// their nanoseconds in [`Job::run`], [2] waits in [`wait_idle`] that found
/// the thread busy, [3] their nanoseconds, [4] jobs that came out of order.
static STATS: [AtomicU64; 5] = [const { AtomicU64::new(0) }; 5];

/// Queues `job` for the thread, starting it on the first call. `Err(job)`
/// gives the job back when the thread could not be made (said once, never
/// tried again): the caller sends it itself.
fn hand_over(job: Job) -> Result<(), Job> {
    static STARTED: OnceLock<bool> = OnceLock::new();
    let started = *STARTED.get_or_init(|| match std::thread::Builder::new().name("victory-send".into()).spawn(worker) {
        Ok(_) => {
            tracing::info!("submissions sent ahead are sent by a thread of their own (VICTORY_SEND_THREAD=1)");
            true
        }
        Err(e) => {
            tracing::warn!(error = %e, "no thread to send submissions with: the submitting thread sends them (VICTORY_SEND_THREAD=1)");
            false
        }
    });
    if !started {
        return Err(job);
    }
    SENDS.hand(job);
    Ok(())
}

/// Returns once the thread has sent everything handed over so far; at once
/// when nothing is outstanding (always, with the switch off). Called before
/// anything else of ours goes to the host's queue or signals its fence, so it
/// comes after the jobs.
pub(super) fn wait_idle() {
    if !on() {
        return;
    }
    if let Some(waited) = SENDS.wait_idle() {
        STATS[2].fetch_add(1, Ordering::Relaxed);
        STATS[3].fetch_add(waited.as_nanos() as u64, Ordering::Relaxed);
    }
}

/// Whether a job comes in the order its value was reserved in: above the last
/// one's. `last` moves on to this job whatever the answer.
fn in_order(last: &mut u64, fence: u64) -> bool {
    let ok = fence > *last;
    *last = fence;
    ok
}

/// The thread: takes jobs in the order they were handed over and runs them.
fn worker() {
    crate::cores::keep_on_fast_cores("victory-send");
    // As the submitting thread (queue/behind.rs), which waits for it.
    {
        use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_ABOVE_NORMAL};
        // SAFETY: this thread.
        unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_ABOVE_NORMAL) };
    }
    let Some(host) = host() else {
        fail(format_args!("no host device for the thread that sends submissions (VICTORY_SEND_THREAD=1)"));
    };
    let mut last = 0u64;
    loop {
        let mut job = SENDS.next();
        // Out of order is counted and said a few times, then run anyway:
        // holding it back would leave its value unsignalled.
        if !in_order(&mut last, job.fence) && STATS[4].fetch_add(1, Ordering::Relaxed) < 4 {
            tracing::warn!(fence = job.fence, "a submission reached the thread that sends out of order (VICTORY_SEND_THREAD=1)");
        }
        let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| job.run(host)));
        if outcome.is_err() && !job.signalled {
            // SAFETY: our queue and fence; the value was reserved for this job.
            let _ = unsafe { host.queue.Signal(&host.fence, job.fence) };
        }
        // Done, and the waiters woken, before anything that can end the run.
        SENDS.finished();
        match outcome {
            Ok(Ok(())) => {}
            Ok(Err(e)) => fail(format_args!("a submission sent ahead could not be followed by a signal ({e})")),
            Err(_) => fail(format_args!("the thread that sends submissions panicked")),
        }
    }
}

/// The counters per frame for the frame-rate line, and back to zero; nothing
/// is said when no submission was sent ahead.
pub(super) fn line(frames: f64) {
    let frames = frames.max(1.0);
    let s: [f64; 5] = std::array::from_fn(|i| STATS[i].swap(0, Ordering::Relaxed) as f64);
    if s[0] <= 0.0 && s[2] <= 0.0 {
        return;
    }
    tracing::info!(
        "submissions sent ahead, their ExecuteCommandLists and signal, per frame: {:.1} in {:.2} ms {} (VICTORY_SEND_THREAD); \
         others waited {:.2} ms for that thread in {:.1} waits before putting work of their own on the queue; {:.0} out of order",
        s[0] / frames,
        s[1] / 1e6 / frames,
        if on() { "on a thread of their own" } else { "on the submitting thread" },
        s[3] / 1e6 / frames,
        s[2] / frames,
        s[4] / frames
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rising_values_are_in_order() {
        let mut last = 0;
        assert!(in_order(&mut last, 1));
        assert!(in_order(&mut last, 2));
        assert!(in_order(&mut last, 5));
        assert_eq!(last, 5);
    }

    #[test]
    fn a_repeated_or_lower_value_is_flagged() {
        let mut last = 0;
        assert!(in_order(&mut last, 4));
        assert!(!in_order(&mut last, 4));
        assert!(!in_order(&mut last, 3));
        assert!(in_order(&mut last, 5));
    }

    /// A thread takes jobs in the order they were handed over, and a wait
    /// returns only once every job handed over before it is done, however
    /// slow they are; with nothing outstanding it does not wait.
    #[test]
    fn a_wait_comes_after_every_job_handed_over_before_it() {
        static SENT: Sends<(u64, u64)> = Sends::new();
        let ran = std::sync::Arc::new(Mutex::new(Vec::new()));
        let seen = ran.clone();
        std::thread::spawn(move || {
            loop {
                let (n, sleep) = SENT.next();
                std::thread::sleep(std::time::Duration::from_micros(sleep));
                seen.lock().unwrap().push(n);
                SENT.finished();
            }
        });
        assert!(SENT.wait_idle().is_none());
        let mut handed = 0;
        for round in 0..50u64 {
            for k in 0..round % 4 + 1 {
                handed += 1;
                SENT.hand((handed, (round * 7 + k * 13) % 300));
            }
            // What a caller that puts work on the queue now sees.
            SENT.wait_idle();
            let ran = ran.lock().unwrap();
            assert_eq!(ran.len() as u64, handed);
            assert!(ran.windows(2).all(|w| w[0] < w[1]), "in the order handed over");
        }
        assert!(SENT.wait_idle().is_none());
    }

    /// What sending costs the submitting thread, here and handed to a
    /// thread: 18 lists of 32 copies each in two ExecuteCommandLists and a
    /// signal, as a submission of the prison walk has (some 290 lists in 16
    /// submissions a frame). The lists of the game hold thousands of
    /// commands over many resources, so this is the floor of what the driver
    /// takes for them.
    #[test]
    #[ignore = "a measurement on the host's card: cargo test -p victory-gpu --release --lib send_costs -- --ignored --nocapture"]
    fn send_costs() {
        let Some(host) = host() else {
            return;
        };
        let (a, _) = host.shared_buffer(1 << 16).unwrap();
        let (b, _) = host.shared_buffer(1 << 16).unwrap();
        const LISTS: usize = 18;
        const ROUNDS: u64 = 300;
        let lists: Vec<HostList> = (0..LISTS).map(|_| host.new_list().unwrap()).collect();
        for list in &lists {
            // SAFETY: an open list; live buffers.
            unsafe {
                for _ in 0..32 {
                    list.list.CopyBufferRegion(&b, 0, &a, 0, 4096);
                }
            }
            list.close();
        }
        let closed: Vec<Option<ID3D12CommandList>> = lists.iter().map(|l| l.list.cast().ok()).collect();
        let fence: ID3D12Fence = unsafe { host.device.CreateFence(0, D3D12_FENCE_FLAG_NONE) }.unwrap();
        let send = |value: u64| {
            // SAFETY: closed lists that are not running (each round waits).
            unsafe {
                host.queue.ExecuteCommandLists(&closed[..LISTS / 2]);
                host.queue.ExecuteCommandLists(&closed[LISTS / 2..]);
                host.queue.Signal(&fence, value).unwrap();
            }
        };
        let wait = |value: u64| {
            while unsafe { fence.GetCompletedValue() } < value {
                std::thread::yield_now();
            }
        };
        // Here: the submitting thread's time is the calls'.
        let mut here = std::time::Duration::ZERO;
        for value in 1..=ROUNDS {
            let began = Instant::now();
            send(value);
            here += began.elapsed();
            wait(value);
        }
        // Handed over: its time is the hand-over's.
        struct Call<'a>(&'a dyn Fn(u64));
        // SAFETY: free-threaded D3D12 objects, used by one thread at a time.
        unsafe impl Send for Call<'_> {}
        unsafe impl Sync for Call<'_> {}
        static SENT: Sends<u64> = Sends::new();
        let call = Call(&send);
        let mut handed = std::time::Duration::ZERO;
        std::thread::scope(|scope| {
            let call = &call;
            scope.spawn(move || {
                loop {
                    let value = SENT.next();
                    if value == 0 {
                        SENT.finished();
                        return;
                    }
                    (call.0)(value);
                    SENT.finished();
                }
            });
            for value in ROUNDS + 1..=2 * ROUNDS {
                let began = Instant::now();
                SENT.hand(value);
                handed += began.elapsed();
                SENT.wait_idle();
                wait(value);
            }
            SENT.hand(0);
        });
        let per = |d: std::time::Duration| d.as_secs_f64() * 1e6 / ROUNDS as f64;
        eprintln!(
            "a submission of {LISTS} lists sent, us on the submitting thread: {:.1} sending it, {:.1} handing it over",
            per(here),
            per(handed)
        );
    }

    #[test]
    fn with_the_switch_off_nothing_waits() {
        // (The switch is read once; the tests run without it.)
        if !on() {
            let began = Instant::now();
            wait_idle();
            assert!(began.elapsed() < std::time::Duration::from_secs(1));
        }
    }
}
