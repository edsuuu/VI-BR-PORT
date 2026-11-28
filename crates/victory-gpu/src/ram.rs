//! The PC's memory our layer holds, by kind, in one log line a minute, in
//! release builds too (`VICTORY_RAM_LOG=0` leaves it out; on by default).
//!
//! The memory lines of `crate::copystat` are in diagnostics builds only,
//! which run at 5 to 20 frames a second and keep other things than a
//! release build does: the PC's memory a release build holds while it plays
//! at 30 was known from one number a run (the system's count of the memory
//! shared with the card, said when the swap chain is made). This line sums
//! counters that are kept anyway, so it costs a few locks a minute: the
//! table of windows is walked once (some 1,500 of them) under its read lock.

use std::sync::atomic::{AtomicU64, Ordering};

/// Whether the line is written (on unless `VICTORY_RAM_LOG=0`).
fn on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_RAM_LOG").is_none_or(|v| v != "0"))
}

/// Seconds between two lines.
const EVERY_S: u64 = 60;

/// When the last line was written (ms, [`crate::host::now_ms`]); 0: never.
static LAST: AtomicU64 = AtomicU64::new(0);

/// The process's working set and private bytes (what it has committed).
fn process_memory() -> (u64, u64) {
    use windows_sys::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    let mut c = PROCESS_MEMORY_COUNTERS {
        cb: size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        ..Default::default()
    };
    // SAFETY: our own process and a sized structure.
    unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) };
    (c.WorkingSetSize as u64, c.PagefileUsage as u64)
}

/// Writes the line if a minute has gone by since the last one. Called by
/// the frame-rate line (every five seconds).
pub(crate) fn tick() {
    if !on() {
        return;
    }
    let now = crate::host::now_ms();
    let last = LAST.load(Ordering::Relaxed);
    // The first line a minute in: the world has begun to load by then.
    if last == 0 {
        LAST.store(now.max(1), Ordering::Relaxed);
        return;
    }
    if now.saturating_sub(last) < EVERY_S * 1000 || LAST.compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed).is_err() {
        return;
    }
    line();
}

fn line() {
    let mb = |v: u64| v >> 20;
    let [windows, window_bytes, cover, slot_held, slot_bytes] = crate::shadow::ram_windows();
    let [copies, copy_bytes] = crate::shadow::alive();
    // What is not a window is a title buffer's copy (with its card side in
    // the card's memory, the CPU's side here).
    let mirror = copy_bytes.saturating_sub(window_bytes);
    let alive = |i: usize| crate::host::ALIVE[i].load(Ordering::Relaxed);
    let [_, staging_blocks] = crate::pool::memory();
    let [stagings, staging_bytes, _] = crate::texture::memory();
    let send = crate::host::send_kept();
    let ours = window_bytes + mirror + alive(2) + alive(7) + (staging_blocks << 20) + staging_bytes + send;
    let shared = crate::host::host().and_then(|h| h.video_memory()).map(|[_, _, _, shared]| shared);
    let (working, private) = process_memory();
    let free = crate::host::available_physical_ram().unwrap_or(0);
    tracing::info!(
        "PC memory we hold (VICTORY_RAM_LOG=0: no line), MB: windows of title memory {} ({windows}, over {} of title memory, \
         {slot_held} of them {} held by descriptor slots); title buffers' copies {} ({} copies in all); lists' scratch {} \
         ({} of it kept spare), lists' other upload buffers {} ({}); blocks of texture staging {}; textures' own staging {} \
         ({stagings}); kept for submissions' sends {}; host lists {}; sum {}; shared with the card as the system counts it {}; \
         the process: private {}, working set {}; the PC's memory free as our caches see it (VICTORY_RAM_ASSUME_FREE_MB) {}",
        mb(window_bytes),
        mb(cover),
        mb(slot_bytes),
        mb(mirror),
        copies,
        mb(alive(2)),
        mb(alive(4)),
        mb(alive(7)),
        alive(6),
        staging_blocks,
        mb(staging_bytes),
        mb(send),
        alive(0),
        mb(ours),
        shared.map_or_else(|| "?".to_string(), |s| mb(s).to_string()),
        mb(private),
        mb(working),
        mb(free),
    );
}
