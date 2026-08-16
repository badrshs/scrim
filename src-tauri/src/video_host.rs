//! The window mpv renders into.
//!
//! ## Why this is a separate top-level window
//!
//! Scrim's design floats controls, drawers, and dialogs over the picture. The
//! obvious arrangement is one window with mpv in a child HWND underneath a
//! transparent WebView2. That was tried first and does not work: WebView2 in
//! windowed mode composites through DWM against what is behind the *top-level
//! window*, not against sibling child HWNDs, so a child placed at HWND_BOTTOM
//! is simply painted over. Measured, not assumed: with the child forced to
//! HWND_TOP the picture appeared correctly, and with it at HWND_BOTTOM the
//! stage was solid black.
//!
//! So the picture gets its own borderless top-level window, and the Tauri
//! window is made an *owned* window of it. Windows guarantees an owned window
//! stays above its owner, which is exactly the z-order needed, without
//! resorting to a global always-on-top that would sit over unrelated apps.
//!
//! ```text
//!   video window   WS_POPUP, tool window, never activated   <- mpv --wid
//!        ^ owns
//!   Tauri window   transparent WebView2                     <- all the UI
//! ```
//!
//! The two are kept in lockstep: the UI reports where the stage rectangle is,
//! and the video window is moved to match it in screen coordinates.
//!
//! See docs/compositing.md.

#![cfg(windows)]

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{ClientToScreen, GetStockObject, BLACK_BRUSH, HBRUSH};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, RegisterClassExW,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, GWLP_HWNDPARENT, SWP_NOACTIVATE, SWP_NOZORDER,
    SW_HIDE, SW_SHOWNOACTIVATE, WNDCLASSEXW, WS_CLIPCHILDREN, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_POPUP,
};

const CLASS_NAME: PCWSTR = w!("ScrimVideoStage");

/// A borderless top-level window that mpv can be pointed at with `--wid`.
pub struct VideoHost {
    hwnd: HWND,
    ui: HWND,
}

// Only touched from the main thread, through Tauri's run loop.
unsafe impl Send for VideoHost {}
unsafe impl Sync for VideoHost {}

unsafe extern "system" fn wndproc(h: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    DefWindowProcW(h, msg, w, l)
}

impl VideoHost {
    /// Create the video window and make the Tauri window its owned window.
    pub fn new(ui: HWND) -> Result<Self, String> {
        unsafe {
            let instance = GetModuleHandleW(None).map_err(|e| format!("GetModuleHandleW: {e}"))?;

            // Registering twice is harmless; the second call fails and we
            // carry on, which keeps this safe if a window is ever recreated.
            let class = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(wndproc),
                hInstance: instance.into(),
                lpszClassName: CLASS_NAME,
                // Black, so letterbox bars and the moment before the first
                // frame match the stage rather than flashing white.
                hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
                ..Default::default()
            };
            RegisterClassExW(&class);

            let hwnd = CreateWindowExW(
                // TOOLWINDOW keeps it out of the taskbar and alt-tab; it is not
                // a window the user should ever think about. NOACTIVATE stops
                // it stealing focus from the interface in front of it.
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                CLASS_NAME,
                w!("Scrim video"),
                WS_POPUP | WS_CLIPCHILDREN,
                0,
                0,
                16,
                16,
                None,
                None,
                // The class already carries the module handle from
                // RegisterClassExW above, and the HINSTANCE types differ
                // between the windows crate versions in this tree.
                None,
                None,
            )
            .map_err(|e| format!("CreateWindowExW for the video window: {e}"))?;

            // The important line. An owned window is always above its owner,
            // so making the video window the OWNER of the Tauri window pins
            // the interface above the picture for free.
            SetWindowLongPtrW(ui, GWLP_HWNDPARENT, hwnd.0 as isize);

            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);

            Ok(Self { hwnd, ui })
        }
    }

    /// The value to pass to mpv as `--wid`.
    pub fn wid(&self) -> isize {
        self.hwnd.0 as isize
    }

    /// Move the video window to a rectangle given in the UI window's client
    /// coordinates, already scaled to physical pixels.
    pub fn set_bounds(&self, x: i32, y: i32, width: i32, height: i32) {
        unsafe {
            let mut origin = POINT { x, y };
            if ClientToScreen(self.ui, &mut origin).as_bool() {
                let _ = SetWindowPos(
                    self.hwnd,
                    None,
                    origin.x,
                    origin.y,
                    width.max(1),
                    height.max(1),
                    SWP_NOACTIVATE | SWP_NOZORDER,
                );
            }
        }
    }

    /// Cover the UI window's whole client area. Used before the interface has
    /// had a chance to measure its own stage.
    pub fn fill_parent(&self) {
        unsafe {
            let mut rect = RECT::default();
            if GetClientRect(self.ui, &mut rect).is_ok() {
                self.set_bounds(0, 0, rect.right - rect.left, rect.bottom - rect.top);
            }
        }
    }

    // There is deliberately no z-order call here. Making the video window the
    // owner of the interface window means Windows maintains the ordering
    // itself, so there is nothing to re-assert on resize. The single-window
    // arrangement needed one; see docs/compositing.md for why it was dropped.

    /// Hide the picture without destroying the window, so stopping playback
    /// does not leave the last decoded frame frozen on screen.
    pub fn hide(&self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }

    pub fn show(&self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        }
    }
}

impl Drop for VideoHost {
    fn drop(&mut self) {
        unsafe {
            // Release the ownership link first, or destroying the owner can
            // take the interface window down with it.
            SetWindowLongPtrW(self.ui, GWLP_HWNDPARENT, 0);
            let _ = DestroyWindow(self.hwnd);
        }
    }
}
