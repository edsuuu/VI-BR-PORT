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

/// Whether the caps of what is kept for reuse are those for a PC with little
/// memory (`VICTORY_LOW_RAM=1`; `=auto`: when the PC has 20 GiB or less;
/// unset or `0`: off, the caps as before). Each cap can be set on its own,
/// which wins over this: [`kept_scratch`] (`VICTORY_SCRATCH_KEPT_MB`, 1024
/// with `VICTORY_KEEP_FREED` on, 128 without; 256 here), the send memory's
/// (`VICTORY_SEND_KEPT_MB`, 768; 128 here), the blocks of texture staging's
/// (`VICTORY_STAGING_BLOCKS_KEPT`, 8; 2 here) and the idle host lists'
/// (`VICTORY_IDLE_LISTS_MAX`, 512; 256 here). What is kept is what a run
/// on the 64 GB machine never gave back: 0.8 to 0.9 GB of scratch chunks
/// spare, 0.7 to 0.9 GB of staging blocks (diagnostics runs of 2026-11-27
/// and 28). Given back past the caps, it is made again when a burst needs
/// it: what the caps cost is that making, and the risk `VICTORY_KEEP_FREED`
/// names (memory given back while something still read it).
pub(crate) fn low() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        let setting = std::env::var("VICTORY_LOW_RAM").ok();
        low_for(setting.as_deref(), total_physical_ram())
    })
}

fn low_for(setting: Option<&str>, total: Option<u64>) -> bool {
    match setting.map(str::trim) {
        Some("auto") => total.is_some_and(|t| t <= 20 << 30),
        Some(v) => v != "0",
        None => false,
    }
}

/// The PC's physical memory, in bytes.
fn total_physical_ram() -> Option<u64> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    // SAFETY: plain data the call fills in.
    let mut m: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    m.dwLength = size_of::<MEMORYSTATUSEX>() as u32;
    // SAFETY: a sized structure.
    let ok = unsafe { GlobalMemoryStatusEx(&mut m) } != 0;
    ok.then_some(m.ullTotalPhys)
}

/// A whole number a switch is set to.
fn number(name: &str) -> Option<u64> {
    std::env::var(name).ok().and_then(|v| v.trim().parse().ok())
}

/// A cap: what the switch `name` says, else `low` with [`low`], else
/// `normal`.
fn cap(name: &str, low: u64, normal: u64) -> u64 {
    pick(number(name), self::low(), low, normal)
}

fn pick(set: Option<u64>, is_low: bool, low: u64, normal: u64) -> u64 {
    set.unwrap_or(if is_low { low } else { normal })
}

/// The most bytes of scratch chunks kept while no list holds them, given
/// what was kept before (`normal`).
pub(crate) fn kept_scratch(normal: u64) -> u64 {
    static MB: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
    let set = *MB.get_or_init(|| number("VICTORY_SCRATCH_KEPT_MB"));
    pick(set.map(|mb| mb << 20), low(), 256 << 20, normal)
}

/// The most bytes of send memory kept for the next submissions, given what
/// was kept before (`normal`).
pub(crate) fn kept_send(normal: u64) -> u64 {
    static MB: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
    let set = *MB.get_or_init(|| number("VICTORY_SEND_KEPT_MB"));
    pick(set.map(|mb| mb << 20), low(), (128 << 20).min(normal), normal)
}

/// The most empty blocks of texture staging kept.
pub(crate) fn kept_staging_blocks(normal: usize) -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| cap("VICTORY_STAGING_BLOCKS_KEPT", 2, normal as u64) as usize)
}

/// The most host lists kept idle.
pub(crate) fn idle_lists(normal: usize) -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| cap("VICTORY_IDLE_LISTS_MAX", 256, normal as u64) as usize)
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
        "PC memory we hold (VICTORY_RAM_LOG=0: no line; the caps of what is kept for reuse {}), MB: windows of title memory {} ({windows}, over {} of title memory, \
         {slot_held} of them {} held by descriptor slots); title buffers' copies {} ({} copies in all); lists' scratch {} \
         ({} of it kept spare), lists' other upload buffers {} ({}); blocks of texture staging {}; textures' own staging {} \
         ({stagings}); kept for submissions' sends {}; host lists {}; sum {}; shared with the card as the system counts it {}; \
         the process: private {}, working set {}; the PC's memory free as our caches see it (VICTORY_RAM_ASSUME_FREE_MB) {}",
        if low() { "for little memory (VICTORY_LOW_RAM)" } else { "as before, unless set one by one" },
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn low_memory_caps_are_asked_for_or_found() {
        let gib = 1u64 << 30;
        assert!(!low_for(None, Some(8 * gib)));
        assert!(!low_for(Some("0"), Some(8 * gib)));
        assert!(low_for(Some("1"), Some(64 * gib)));
        assert!(low_for(Some("auto"), Some(16 * gib)));
        assert!(!low_for(Some("auto"), Some(64 * gib)));
        assert!(!low_for(Some("auto"), None));
        // A cap set on its own wins; else the low one with the profile.
        assert_eq!(pick(Some(300), true, 2, 8), 300);
        assert_eq!(pick(None, true, 2, 8), 2);
        assert_eq!(pick(None, false, 2, 8), 8);
    }
}
