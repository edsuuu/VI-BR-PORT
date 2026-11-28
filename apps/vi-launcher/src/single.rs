//! One launcher at a time. The first window holds a named mutex for as long as
//! it runs; a second start finds it taken, brings the first window back (it may
//! be hidden in the tray while the game is open) and closes.

/// True when this is the only launcher running: the window may open. False
/// when another one already is; that one has been brought to the front.
#[cfg(windows)]
pub fn first(title: &str) -> bool {
    use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
    use windows_sys::Win32::System::Threading::CreateMutexW;

    let name = wide("Local\\VI-BR-Launcher.one");
    // SAFETY: a valid, NUL-ended name; the handle is kept until the process ends.
    let mutex = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    if mutex.is_null() {
        // Without the mutex nothing can be told: open as before.
        return true;
    }
    // SAFETY: read right after the call that set it.
    if unsafe { GetLastError() } != ERROR_ALREADY_EXISTS {
        return true;
    }
    show_other(title);
    false
}

#[cfg(not(windows))]
pub fn first(_title: &str) -> bool {
    true
}

/// Shows the running launcher's window, restored and in front.
#[cfg(windows)]
pub fn show_other(title: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{FindWindowW, SW_RESTORE, SW_SHOW, SetForegroundWindow, ShowWindow};

    let title = wide(title);
    // The first window may still be opening: look for it for a few seconds.
    for _ in 0..20 {
        // SAFETY: a NUL-ended title and no class; the window is only shown.
        unsafe {
            let window = FindWindowW(std::ptr::null(), title.as_ptr());
            if !window.is_null() {
                ShowWindow(window, SW_SHOW);
                ShowWindow(window, SW_RESTORE);
                SetForegroundWindow(window);
                return;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
    }
}

#[cfg(windows)]
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
