//! What the PC has, so the launcher sets it by itself: the card's memory
//! (for `VICTORY_MEMORY_CARD_MB`) and the CPU's threads (the `--jobs` of the
//! preparing steps).
use std::sync::OnceLock;

/// Every logical CPU: the preparing steps (translating the shaders, warming
/// the pipelines) use the whole CPU.
pub fn threads() -> u32 {
    std::thread::available_parallelism().map_or(8, |n| n.get() as u32)
}

/// The largest card's own memory in bytes, if it can be read.
pub fn vram() -> Option<u64> {
    static VRAM: OnceLock<Option<u64>> = OnceLock::new();
    *VRAM.get_or_init(detect)
}

/// The card memory the run keeps for the title, from the card's size:
/// 512 MB up to 6 GB cards, 1,024 MB up to 10 GB, 2,048 MB above (an 8 GB
/// card measured the same with 1,024 and 2,048). Unknown: 1,024, the
/// README's setting.
pub fn card_memory_mb(vram: Option<u64>) -> u32 {
    const GIB: u64 = 1 << 30;
    match vram {
        Some(b) if b <= 6 * GIB + GIB / 2 => 512,
        Some(b) if b <= 10 * GIB + GIB / 2 => 1024,
        Some(_) => 2048,
        None => 1024,
    }
}

/// The display adapters' `HardwareInformation.qwMemorySize` in the
/// registry: the size the driver reports, past 4 GB too (WMI's AdapterRAM
/// stops at 4 GB).
#[cfg(windows)]
fn detect() -> Option<u64> {
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_LOCAL_MACHINE, KEY_READ, RRF_RT_ANY, RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW,
    };
    let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let class = wide(r"SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}");
    let mut key: HKEY = std::ptr::null_mut();
    // SAFETY: a NUL-terminated name and an out handle.
    if unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, class.as_ptr(), 0, KEY_READ, &mut key) } != 0 {
        return None;
    }
    let mut best: Option<u64> = None;
    for name in ["HardwareInformation.qwMemorySize", "HardwareInformation.MemorySize"] {
        let value = wide(name);
        for i in 0.. {
            let mut sub = [0u16; 64];
            let mut len = sub.len() as u32;
            // SAFETY: the buffer and its length in characters.
            let r = unsafe {
                RegEnumKeyExW(key, i, sub.as_mut_ptr(), &mut len, std::ptr::null(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut())
            };
            if r != 0 {
                break;
            }
            let mut bytes = [0u8; 8];
            let mut size = bytes.len() as u32;
            // SAFETY: the subkey name is NUL-terminated by the call; the buffer and its size.
            let r = unsafe {
                RegGetValueW(key, sub.as_ptr(), value.as_ptr(), RRF_RT_ANY, std::ptr::null_mut(), bytes.as_mut_ptr().cast(), &mut size)
            };
            if r == 0 && size >= 4 {
                let v = u64::from_le_bytes(bytes);
                if v > 0 {
                    best = Some(best.map_or(v, |b| b.max(v)));
                }
            }
        }
        if best.is_some() {
            break;
        }
    }
    // SAFETY: the handle opened above.
    unsafe { RegCloseKey(key) };
    best
}

/// AMD's sysfs file, else `nvidia-smi`.
#[cfg(not(windows))]
fn detect() -> Option<u64> {
    let mut best: Option<u64> = None;
    if let Ok(cards) = std::fs::read_dir("/sys/class/drm") {
        for c in cards.filter_map(Result::ok) {
            let p = c.path().join("device").join("mem_info_vram_total");
            if let Some(v) = std::fs::read_to_string(p).ok().and_then(|t| t.trim().parse::<u64>().ok()) {
                best = Some(best.map_or(v, |b| b.max(v)));
            }
        }
    }
    if best.is_none()
        && let Ok(out) = std::process::Command::new("nvidia-smi")
            .args(["--query-gpu=memory.total", "--format=csv,noheader,nounits"])
            .output()
    {
        best = String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.trim().parse::<u64>().ok())
            .max()
            .map(|mib| mib << 20);
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_memory_from_the_card() {
        const GIB: u64 = 1 << 30;
        assert_eq!(card_memory_mb(Some(6 * GIB)), 512);
        assert_eq!(card_memory_mb(Some(8 * GIB)), 1024);
        assert_eq!(card_memory_mb(Some(12 * GIB)), 2048);
        assert_eq!(card_memory_mb(None), 1024);
        assert!(threads() >= 1);
    }
}
