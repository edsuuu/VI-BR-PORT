//! Which processors our own threads run on, where a processor has two kinds
//! of cores. `cpu_sets` and `keep_on_fast_cores` are fxceless/victory-fork's
//! (`framelog.rs`, commit 0b0074e0, GPL-3.0-or-later, as this).

/// The system's processor sets of group 0: each one's id and its efficiency
/// class (higher is faster).
#[cfg(test)]
fn cpu_sets() -> Vec<(u32, u8)> {
    system_sets().into_iter().map(|(id, class, _)| (id, class)).collect()
}

/// The system's processor sets of group 0 (id, efficiency class) with the core
/// each one is a logical processor of (two sets
/// share a core where it runs two threads at once).
fn system_sets() -> Vec<(u32, u8, u8)> {
    use windows_sys::Win32::System::SystemInformation::{GetSystemCpuSetInformation, SYSTEM_CPU_SET_INFORMATION};
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    let mut bytes = 0u32;
    // SAFETY: a size query, then the call into a buffer of that size.
    unsafe { GetSystemCpuSetInformation(std::ptr::null_mut(), 0, &mut bytes, GetCurrentProcess(), 0) };
    let mut buffer = vec![0u64; (bytes as usize).div_ceil(8).max(1)];
    // SAFETY: as above; the buffer holds `bytes` bytes, aligned for the records.
    let ok = unsafe { GetSystemCpuSetInformation(buffer.as_mut_ptr().cast(), bytes, &mut bytes, GetCurrentProcess(), 0) };
    let mut sets = Vec::new();
    let mut at = 0usize;
    while ok != 0 && at + size_of::<SYSTEM_CPU_SET_INFORMATION>() <= bytes as usize {
        // SAFETY: a record inside the buffer; each says its own size.
        let info = unsafe { &*(buffer.as_ptr().cast::<u8>().add(at).cast::<SYSTEM_CPU_SET_INFORMATION>()) };
        if info.Size == 0 {
            break;
        }
        // SAFETY: the record's type is CpuSet (the only one there is).
        let set = unsafe { &info.Anonymous.CpuSet };
        if set.Group == 0 {
            sets.push((set.Id, set.EfficiencyClass, set.CoreIndex));
        }
        at += info.Size as usize;
    }
    sets
}

/// The sets of the fastest class, if there are slower ones too.
fn fast_of(sets: &[(u32, u8)]) -> Vec<u32> {
    let best = sets.iter().map(|c| c.1).max().unwrap_or(0);
    let fast: Vec<u32> = sets.iter().filter(|c| c.1 == best).map(|c| c.0).collect();
    if fast.len() == sets.len() { Vec::new() } else { fast }
}

/// `VICTORY_FAST_CORES`: 0: no thread asks; 1: the threads a frame waits for
/// one by one; 2 (the default): the threads they share their work with too;
/// 3: those that work beside the rest as well (lists translated, descriptors
/// made): more threads by then than the fast cores run at a time, while the
/// slow ones stood idle.
fn stage() -> u32 {
    static STAGE: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *STAGE.get_or_init(|| std::env::var("VICTORY_FAST_CORES").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(2))
}

/// Asks the system to run the calling thread, which a frame waits for, on
/// the fast cores only, where the processor has two kinds. That fork's submit
/// thread was found on a slow one in 1,529 looks of 24,021. (A selection of
/// processors is a preference the scheduler keeps unless nothing of it is
/// free.) The title's threads are not asked for.
pub(crate) fn keep_on_fast_cores(what: &str) {
    ask(what, 1);
}

/// [`keep_on_fast_cores`] for one of the threads a submission's preparing or
/// a landing shares its work with.
pub(crate) fn keep_helper_on_fast_cores(what: &str) {
    ask(what, 2);
}

/// [`keep_on_fast_cores`] for one of many threads whose work is ahead of what
/// a frame waits for.
pub(crate) fn keep_many_on_fast_cores(what: &str) {
    ask(what, 3);
}

/// Whether the threads the submitting thread waits for run at its priority,
/// above normal (`VICTORY_HELPER_PRIORITY=1`; off by default: at normal, as
/// before). Read once.
///
/// The submitting thread runs above normal (queue/behind.rs), but the
/// threads it hands work to and then waits for (the wide pool; the pools of
/// landings, of the merges under flights' writes and of the heaps' records;
/// the heaps' record thread; the pass beside the lists; the thread that lands
/// flights) ran at normal, level with the title's threads and the eight list
/// translators. Measured by the fork
/// it comes from (Kemerd/VIctory-NVIDIA-RT 80c70b719, i5-12400F, six cores,
/// RTX 3060, one run off / on): 8.6 / 9.6 fps, refreshes 23.1 / 17.6 ms a
/// frame, waiting for the heaps' records 6.2 / 2.5 ms. The thread that lands
/// flights is added here (the relay, which that fork's submit did not wait
/// for in the same way).
fn helpers_raised() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| crate::switch::on("VICTORY_HELPER_PRIORITY"))
}

/// Raises the calling thread, one the submitting thread waits for, to that
/// thread's priority when [`helpers_raised`]; else leaves it as it is.
pub(crate) fn raise_helper() {
    use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_ABOVE_NORMAL};
    if helpers_raised() {
        // SAFETY: the calling thread.
        unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_ABOVE_NORMAL) };
    }
}

/// Whether the thread that prepares and sends submissions has a fast core to
/// itself (`VICTORY_OWN_CORE=1`; off by default: it shares the fast cores
/// with the rest). Read once.
///
/// That thread is what a walking frame waits for: in the prison walk of
/// 28 Nov (prisao-fim-1) it worked 35-45 ms of 37-47 ms frames while the
/// title waited 10-14 ms a frame at its presents for it, and the card stood
/// with nothing of ours 1-4 ms. Beside it, eight list translators worked
/// 140-215 ms a frame, the landing and merging pools, the heaps' records and
/// the title's own threads: a fast core's second logical processor is
/// rarely free, and a thread shares the core's units with whatever runs on
/// the other. With the switch the process's threads are asked off one fast
/// core (the system's default sets for the process, and the sets our own
/// threads ask for, leave out its logical processors) and the submitting
/// thread is asked onto it. Like every selection of processors it is a
/// preference: the scheduler still runs a thread elsewhere when nothing of
/// its selection is free, and other processes are not asked.
fn own_core_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| crate::switch::on("VICTORY_OWN_CORE"))
}

/// The sets (ids) of the core [`own_core_on`] gives the submitting thread:
/// of the fastest class, the core with the highest index (the system's
/// interrupts and the first threads started favour the low ones). None with
/// fewer than four cores, where one of them is too much to take from the rest.
fn own_core_of(sets: &[(u32, u8, u8)]) -> Vec<u32> {
    let cores: std::collections::BTreeSet<u8> = sets.iter().map(|s| s.2).collect();
    if cores.len() < 4 {
        return Vec::new();
    }
    let best = sets.iter().map(|s| s.1).max().unwrap_or(0);
    let Some(core) = sets.iter().filter(|s| s.1 == best).map(|s| s.2).max() else {
        return Vec::new();
    };
    sets.iter().filter(|s| s.2 == core).map(|s| s.0).collect()
}

/// `sets` without those in `out`.
fn without(sets: &[u32], out: &[u32]) -> Vec<u32> {
    sets.iter().copied().filter(|id| !out.contains(id)).collect()
}

/// What the asking is made of, found once: the fast sets (empty when all
/// are alike), how many there are in all, every set's id, and the sets of
/// the submitting thread's own core (empty without [`own_core_on`]).
struct Sets {
    fast: Vec<u32>,
    of: usize,
    all: Vec<u32>,
    own: Vec<u32>,
}

fn sets() -> &'static Sets {
    static SETS: std::sync::OnceLock<Sets> = std::sync::OnceLock::new();
    SETS.get_or_init(|| {
        let full = system_sets();
        let pairs: Vec<(u32, u8)> = full.iter().map(|&(id, class, _)| (id, class)).collect();
        Sets {
            fast: fast_of(&pairs),
            of: full.len(),
            all: full.iter().map(|s| s.0).collect(),
            own: if own_core_on() { own_core_of(&full) } else { Vec::new() },
        }
    })
}

/// For the thread that prepares and sends submissions: with
/// [`own_core_on`] it is asked onto its own core, and the process's other
/// threads (those that ask nothing) off it; else [`keep_on_fast_cores`].
pub(crate) fn keep_submitting_thread(what: &str) {
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetCurrentThread, SetProcessDefaultCpuSets, SetThreadSelectedCpuSets};
    let sets = sets();
    if sets.own.is_empty() {
        keep_on_fast_cores(what);
        return;
    }
    let rest = without(&sets.all, &sets.own);
    // SAFETY: our own process and thread, and lists of the system's own set ids.
    let (process, thread) = unsafe {
        (
            SetProcessDefaultCpuSets(GetCurrentProcess(), rest.as_ptr(), rest.len() as u32),
            SetThreadSelectedCpuSets(GetCurrentThread(), sets.own.as_ptr(), sets.own.len() as u32),
        )
    };
    if process == 0 || thread == 0 {
        tracing::warn!(what, error = %std::io::Error::last_os_error(), "the thread could not be given a core of its own (VICTORY_OWN_CORE=1)");
        keep_on_fast_cores(what);
    } else {
        tracing::info!(
            what,
            own = ?sets.own,
            rest = rest.len(),
            "the thread has a fast core to itself, the process's other threads are asked off it (VICTORY_OWN_CORE=1)"
        );
    }
}

fn ask(what: &str, from: u32) {
    use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadSelectedCpuSets};
    if stage() < from {
        return;
    }
    let Sets { fast, of, own, .. } = sets();
    // (Off the submitting thread's own core, if it has one.)
    let fast = without(fast, own);
    // All alike (or nothing known): nothing to ask for.
    if fast.is_empty() {
        return;
    }
    // SAFETY: our own thread and a list of the system's own set ids.
    let ok = unsafe { SetThreadSelectedCpuSets(GetCurrentThread(), fast.as_ptr(), fast.len() as u32) };
    if ok == 0 {
        tracing::warn!(what, error = %std::io::Error::last_os_error(), "the thread could not be kept on the fast cores");
    } else {
        tracing::info!(what, fast = fast.len(), of, "the thread is kept on the fast cores (VICTORY_FAST_CORES=0: not)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With the switch unset (as the tests run) a helper keeps the priority
    /// it was started with.
    #[test]
    fn a_helper_is_left_at_its_priority_unless_asked() {
        use windows_sys::Win32::System::Threading::{GetCurrentThread, GetThreadPriority};
        let (before, after) = std::thread::spawn(|| {
            // SAFETY: the calling thread.
            let before = unsafe { GetThreadPriority(GetCurrentThread()) };
            raise_helper();
            // SAFETY: as above.
            (before, unsafe { GetThreadPriority(GetCurrentThread()) })
        })
        .join()
        .unwrap();
        if !helpers_raised() {
            assert_eq!(before, after);
        } else {
            assert!(after > before);
        }
    }

    #[test]
    fn the_fast_cores_are_those_of_the_best_class_if_there_is_a_worse_one() {
        assert_eq!(fast_of(&[(256, 1), (257, 1), (258, 0)]), [256, 257]);
        assert!(fast_of(&[(256, 0), (257, 0)]).is_empty());
        assert!(fast_of(&[]).is_empty());
        assert!(!cpu_sets().is_empty());
        std::thread::spawn(|| keep_on_fast_cores("test")).join().unwrap();
    }

    /// The submitting thread's own core is the fast one of the highest
    /// index, both its logical processors; none with fewer than four cores.
    #[test]
    fn the_own_core_is_the_last_fast_one_with_both_its_threads() {
        // (id, class, core): four fast cores of two threads, two slow ones.
        let hybrid: Vec<(u32, u8, u8)> = (0..8)
            .map(|i| (256 + i, 1, (i / 2) as u8))
            .chain([(264, 0, 4), (265, 0, 5)])
            .collect();
        assert_eq!(own_core_of(&hybrid), [262, 263]);
        // All alike, one thread a core: the last.
        let alike: Vec<(u32, u8, u8)> = (0..6).map(|i| (256 + i, 0, i as u8)).collect();
        assert_eq!(own_core_of(&alike), [261]);
        // Three cores: nothing taken from the rest.
        assert!(own_core_of(&[(256, 0, 0), (257, 0, 0), (258, 0, 1), (259, 0, 2)]).is_empty());
        assert!(own_core_of(&[]).is_empty());
        // The others' fast sets leave it out.
        assert_eq!(without(&[256, 257, 262, 263], &[262, 263]), [256, 257]);
        assert_eq!(without(&[256, 257], &[]), [256, 257]);
    }

    /// With the switch unset (as the tests run) the submitting thread is
    /// asked onto the fast cores as before, and nothing is taken from the
    /// process. With it (`VICTORY_OWN_CORE=1 cargo test ... cores::`) the
    /// thread holds its core's sets and the process's default the others.
    #[test]
    fn without_the_switch_no_core_is_kept() {
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetCurrentThread, GetProcessDefaultCpuSets, GetThreadSelectedCpuSets};
        if !own_core_on() {
            assert!(sets().own.is_empty());
        }
        let selected = std::thread::spawn(|| {
            keep_submitting_thread("test");
            let (mut ids, mut count) = ([0u32; 64], 0u32);
            // SAFETY: the calling thread, into a buffer of 64.
            let ok = unsafe { GetThreadSelectedCpuSets(GetCurrentThread(), ids.as_mut_ptr(), 64, &mut count) };
            assert_ne!(ok, 0);
            ids[..count as usize].to_vec()
        })
        .join()
        .unwrap();
        let own = &sets().own;
        if !own.is_empty() {
            assert_eq!(&selected, own);
            let (mut ids, mut count) = ([0u32; 256], 0u32);
            // SAFETY: our process, into a buffer of 256.
            let ok = unsafe { GetProcessDefaultCpuSets(GetCurrentProcess(), ids.as_mut_ptr(), 256, &mut count) };
            assert_ne!(ok, 0);
            assert_eq!(ids[..count as usize], without(&sets().all, own)[..]);
        }
    }
}
