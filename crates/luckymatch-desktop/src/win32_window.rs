#[cfg(windows)]
mod imp {
    use core::ffi::c_void;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture;
    use windows::Win32::UI::WindowsAndMessaging::{
        PostMessageW, SetWindowPos, HWND_NOTOPMOST, HWND_TOPMOST, SC_MOVE, SWP_NOACTIVATE,
        SWP_NOMOVE, SWP_NOSIZE, WM_SYSCOMMAND,
    };

    fn extract_hwnd(window: &gpui::Window) -> Option<HWND> {
        let handle = HasWindowHandle::window_handle(window).ok()?;
        match handle.as_raw() {
            RawWindowHandle::Win32(win32) => Some(HWND(win32.hwnd.get() as *mut c_void)),
            _ => None,
        }
    }

    pub fn set_always_on_top(window: &gpui::Window, topmost: bool) {
        let Some(hwnd) = extract_hwnd(window) else {
            return;
        };
        let insert_after = if topmost { HWND_TOPMOST } else { HWND_NOTOPMOST };
        unsafe {
            let _ = SetWindowPos(
                hwnd,
                Some(insert_after),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
    }

    pub fn start_window_drag(window: &gpui::Window) {
        let Some(hwnd) = extract_hwnd(window) else {
            return;
        };
        unsafe {
            let _ = ReleaseCapture();
            // PostMessageW lets GPUI finish its mouse handler and release RefCell borrows
            // before Windows enters the modal move loop.
            let _ = PostMessageW(
                Some(hwnd),
                WM_SYSCOMMAND,
                WPARAM((SC_MOVE | 0x0002) as usize),
                LPARAM(0),
            );
        }
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn set_always_on_top(_window: &gpui::Window, _topmost: bool) {}

    pub fn start_window_drag(window: &gpui::Window) {
        window.start_window_move();
    }
}

pub use imp::{set_always_on_top, start_window_drag};
