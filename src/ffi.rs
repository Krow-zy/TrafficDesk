//! Raw Win32 glue: single instance, wall-clock time and the taskbar/window
//! styles that eframe leaves to us.
#![allow(non_snake_case, non_camel_case_types, dead_code)]

use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError, SYSTEMTIME};
use windows_sys::Win32::System::SystemInformation::GetLocalTime;
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    FindWindowW, GWL_EXSTYLE, GetWindowLongPtrW, IsIconic, SW_MINIMIZE, SW_RESTORE,
    SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SetForegroundWindow,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
};

/// NUL-terminated UTF-16 buffer for `*W` APIs.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Local wall-clock time, formatted for the log file.
pub fn local_now_string() -> String {
    let mut st = SYSTEMTIME {
        wYear: 0,
        wMonth: 0,
        wDayOfWeek: 0,
        wDay: 0,
        wHour: 0,
        wMinute: 0,
        wSecond: 0,
        wMilliseconds: 0,
    };
    unsafe { GetLocalTime(&mut st) };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond, st.wMilliseconds
    )
}

// ---------------------------------------------------------------------------
// Single instance guard: one named mutex per user session.
// ---------------------------------------------------------------------------

pub fn single_instance_guard() -> bool {
    let name = wide("Local\\TrafficDesk.SingleInstance.v1");
    let h = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    if h.is_null() {
        // Could not create the guard at all; do not block startup on it.
        return true;
    }
    unsafe { GetLastError() != ERROR_ALREADY_EXISTS }
}

// ---------------------------------------------------------------------------
// Taskbar / Alt-Tab presence
//
// eframe's `with_taskbar(false)` does not reach winit on this version, so the
// extended styles are adjusted directly: drop WS_EX_APPWINDOW and add
// WS_EX_TOOLWINDOW, which is what "floating panel with a tray icon" wants.
// ---------------------------------------------------------------------------

/// Our own top-level window, by winit's class name and our title.
pub fn find_own_window(title: &str) -> Option<isize> {
    let class = wide("Window Class");
    let text = wide(title);
    let hwnd = unsafe { FindWindowW(class.as_ptr(), text.as_ptr()) };
    if hwnd.is_null() {
        None
    } else {
        Some(hwnd as isize)
    }
}

/// eframe's `with_taskbar(false)` never reaches winit on this version, and
/// winit re-applies its styles during startup, so this is re-asserted
/// periodically: drop WS_EX_APPWINDOW and add WS_EX_TOOLWINDOW, which is what
/// a floating panel with a tray icon wants.
pub fn assert_tool_window(hwnd: isize) -> bool {
    let hwnd = hwnd as *mut core::ffi::c_void;
    unsafe {
        let current = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        let wanted = (current | WS_EX_TOOLWINDOW) & !WS_EX_APPWINDOW;
        if wanted == current {
            return true;
        }
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, wanted as isize);
        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
        true
    }
}

/// True when our window is on screen rather than tucked away.
pub fn own_window_visible() -> Option<bool> {
    let hwnd = find_own_window("TrafficDesk")? as *mut core::ffi::c_void;
    Some(unsafe { IsIconic(hwnd) } == 0)
}

/// Show or hide our own window, returning the state afterwards.
///
/// Hiding is done by minimizing rather than by `SW_HIDE` or by
/// `ViewportCommand::Visible(false)`: a hidden window never receives the
/// `WM_PAINT` that winit waits for, so the event loop spins at full CPU and
/// stops running `App::update`, which leaves queued commands (including the one
/// that would restore the window) unprocessed. Our window is a tool window, so
/// there is no taskbar button: the tray icon and the hotkey are the way back.
pub fn set_own_window_visible(visible: bool) -> Option<bool> {
    let hwnd = find_own_window("TrafficDesk")? as *mut core::ffi::c_void;
    unsafe {
        if visible {
            ShowWindow(hwnd, SW_RESTORE);
            SetForegroundWindow(hwnd);
        } else {
            ShowWindow(hwnd, SW_MINIMIZE);
        }
        Some(IsIconic(hwnd) == 0)
    }
}
