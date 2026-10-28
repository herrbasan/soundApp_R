// Multi-Window Manager (Phase 1.5): several native `winit` windows in one process,
// each owning a `Renderer` + `Scene` + `UiState` while sharing the caller's single
// `TextSystem` glyph atlas. Fast `hide()` / `show()` toggling keeps windows alive —
// hide only hides the native surface, so re-showing costs no GL re-init.

use std::sync::Arc;

use winit::dpi::LogicalSize;
use winit::event_loop::ActiveEventLoop;
use winit::window::{ResizeDirection, Window, WindowAttributes, WindowId};

use crate::render::{BackendKind, Renderer};
use crate::text::FontFace;
use crate::widgets::{BadgeVariant, ButtonVariant, Rect, Ui, UiState, ICON_CLOSE};
use crate::Scene;

#[cfg(target_os = "windows")]
mod live_drag {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use winit::window::{ResizeDirection, Window};

    type TimerProc = unsafe extern "system" fn(isize, u32, usize, u32);
    type WndEnumProc = unsafe extern "system" fn(isize, isize) -> i32;

    #[link(name = "user32")]
    extern "system" {
        fn SetTimer(hwnd: isize, id: usize, elapse: u32, func: Option<TimerProc>) -> usize;
        fn KillTimer(hwnd: isize, id: usize) -> i32;
        fn InvalidateRect(hwnd: isize, rect: *const [i32; 4], erase: i32) -> i32;
        fn IsWindowVisible(hwnd: isize) -> i32;
        fn EnumThreadWindows(thread_id: u32, func: WndEnumProc, lparam: isize) -> i32;
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentThreadId() -> u32;
    }

    const DRAG_TIMER_ID: usize = 0x4E55_4901;

    unsafe extern "system" fn enum_invalidate(hwnd: isize, _lparam: isize) -> i32 {
        if IsWindowVisible(hwnd) != 0 {
            InvalidateRect(hwnd, std::ptr::null(), 0);
        }
        1
    }

    unsafe extern "system" fn drag_timer_proc(_hwnd: isize, _msg: u32, _id: usize, _time: u32) {
        EnumThreadWindows(GetCurrentThreadId(), enum_invalidate, 0);
    }

    fn hwnd_of(window: &Window) -> Option<isize> {
        let wh = window.window_handle().ok()?;
        match wh.as_raw() {
            RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
            _ => None,
        }
    }

    pub fn drag_window(window: &Window) {
        if let Some(hwnd) = hwnd_of(window) {
            unsafe {
                SetTimer(hwnd, DRAG_TIMER_ID, 16, Some(drag_timer_proc));
            }
            let _ = window.drag_window();
            unsafe {
                KillTimer(hwnd, DRAG_TIMER_ID);
            }
        } else {
            let _ = window.drag_window();
        }
    }

    pub fn drag_resize_window(window: &Window, dir: ResizeDirection) {
        if let Some(hwnd) = hwnd_of(window) {
            unsafe {
                SetTimer(hwnd, DRAG_TIMER_ID, 16, Some(drag_timer_proc));
            }
            let _ = window.drag_resize_window(dir);
            unsafe {
                KillTimer(hwnd, DRAG_TIMER_ID);
            }
        } else {
            let _ = window.drag_resize_window(dir);
        }
    }
}

#[cfg(target_os = "windows")]
mod cursor_probe {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use winit::window::Window;

    #[repr(C)]
    struct Point {
        x: i32,
        y: i32,
    }

    #[link(name = "user32")]
    extern "system" {
        fn GetCursorPos(point: *mut Point) -> i32;
        fn ScreenToClient(hwnd: isize, point: *mut Point) -> i32;
        fn GetAsyncKeyState(key: i32) -> i16;
    }

    pub fn cursor_in_window(window: &Window) -> Option<(f32, f32)> {
        let handle = window.window_handle().ok()?;
        let hwnd = match handle.as_raw() {
            RawWindowHandle::Win32(h) => h.hwnd.get(),
            _ => return None,
        };
        let mut point = Point { x: 0, y: 0 };
        let ok = unsafe {
            GetCursorPos(&mut point) != 0 && ScreenToClient(hwnd, &mut point) != 0
        };
        ok.then_some((point.x as f32, point.y as f32))
    }

    pub fn primary_button_down() -> bool {
        unsafe { (GetAsyncKeyState(0x01) as u16 & 0x8000) != 0 }
    }
}

/// Current pointer position in physical client pixels. OLE drag-over and drop
/// notifications carry a point that winit discards, so drag UI must read the OS
/// cursor rather than waiting for `CursorMoved`.
#[cfg(target_os = "windows")]
pub fn cursor_position_in_window(window: &Window) -> Option<(f32, f32)> {
    cursor_probe::cursor_in_window(window)
}

#[cfg(not(target_os = "windows"))]
pub fn cursor_position_in_window(_window: &Window) -> Option<(f32, f32)> {
    None
}

/// Whether the primary mouse button is currently held. OLE drag notifications
/// carry no button state, so this closes the gap when `HoveredFileCancelled`
/// is lost.
#[cfg(target_os = "windows")]
pub fn primary_mouse_button_down() -> bool {
    cursor_probe::primary_button_down()
}

#[cfg(not(target_os = "windows"))]
pub fn primary_mouse_button_down() -> bool {
    false
}

#[cfg(target_os = "windows")]
mod taskbar_group {
    use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU8, Ordering};
    use std::sync::Mutex;

    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use winit::window::Window;

    #[link(name = "user32")]
    extern "system" {
        fn SetWindowLongPtrW(hwnd: isize, index: i32, value: isize) -> isize;
        fn CallWindowProcW(prev: isize, hwnd: isize, msg: u32, w: usize, l: isize) -> isize;
        fn DefWindowProcW(hwnd: isize, msg: u32, w: usize, l: isize) -> isize;
        fn ShowWindow(hwnd: isize, cmd: i32) -> i32;
        fn IsWindow(hwnd: isize) -> i32;
        fn IsWindowVisible(hwnd: isize) -> i32;
        fn IsIconic(hwnd: isize) -> i32;
        fn LoadImageW(
            instance: isize,
            name: *const u16,
            kind: u32,
            cx: i32,
            cy: i32,
            flags: u32,
        ) -> isize;
        fn GetModuleHandleW(name: *const u16) -> isize;
        fn CreatePopupMenu() -> isize;
        fn AppendMenuW(menu: isize, flags: u32, id: usize, item: *const u16) -> i32;
        fn TrackPopupMenu(
            menu: isize,
            flags: u32,
            x: i32,
            y: i32,
            reserved: i32,
            hwnd: isize,
            rect: *const c_void,
        ) -> i32;
        fn DestroyMenu(menu: isize) -> i32;
        fn GetCursorPos(point: *mut Point) -> i32;
        fn SetForegroundWindow(hwnd: isize) -> i32;
        fn PostMessageW(hwnd: isize, msg: u32, w: usize, l: isize) -> i32;
    }

    #[link(name = "shell32")]
    extern "system" {
        fn Shell_NotifyIconW(message: u32, data: *mut NotifyIconDataW) -> i32;
    }

    use std::ffi::OsStr;
    use std::os::raw::c_void;
    use std::os::windows::ffi::OsStrExt;

    #[repr(C)]
    struct Point {
        x: i32,
        y: i32,
    }

    #[repr(C)]
    struct NotifyIconDataW {
        cb_size: u32,
        hwnd: isize,
        uid: u32,
        flags: u32,
        callback_message: u32,
        icon: isize,
        tip: [u16; 128],
        state: u32,
        state_mask: u32,
        info: [u16; 256],
        timeout_or_version: u32,
        info_title: [u16; 64],
        info_flags: u32,
        guid_item: [u8; 16],
        balloon_icon: isize,
    }

    const GWLP_WNDPROC: i32 = -4;
    const WM_ACTIVATE: u32 = 0x0006;
    const WM_SHOWWINDOW: u32 = 0x0018;
    const WM_SYSCOMMAND: u32 = 0x0112;
    const SC_MINIMIZE: u32 = 0xF020;
    const SC_RESTORE: u32 = 0xF120;
    const SW_MINIMIZE: i32 = 6;
    const SW_SHOWNOACTIVATE: i32 = 4;
    const WM_APP: u32 = 0x8000;
    const WM_TRAY: u32 = WM_APP + 1;
    const WM_LBUTTONUP: u32 = 0x0202;
    const WM_RBUTTONUP: u32 = 0x0205;
    const IMAGE_ICON: u32 = 1;
    const LR_DEFAULTSIZE: u32 = 0x0000_0040;
    const LR_SHARED: u32 = 0x0000_8000;
    const NIM_ADD: u32 = 0;
    const NIM_DELETE: u32 = 2;
    const NIF_MESSAGE: u32 = 1;
    const NIF_ICON: u32 = 2;
    const NIF_TIP: u32 = 4;
    const MF_STRING: u32 = 0;
    const TPM_RETURNCMD: u32 = 0x0100;
    const TPM_NONOTIFY: u32 = 0x0080;
    const TPM_RIGHTBUTTON: u32 = 0x0002;
    const TRAY_SHOW: u8 = 1;
    const TRAY_QUIT: u8 = 2;

    static WINDOWS: Mutex<Vec<(isize, isize)>> = Mutex::new(Vec::new());
    static GROUP_MINIMIZED: AtomicBool = AtomicBool::new(false);
    static TRAY_ADDED: AtomicBool = AtomicBool::new(false);
    static TRAY_HWND: AtomicIsize = AtomicIsize::new(0);
    static TRAY_ACTION: AtomicU8 = AtomicU8::new(0);

    fn wide(value: &str) -> Vec<u16> {
        OsStr::new(value).encode_wide().chain(Some(0)).collect()
    }

    fn set_tip(dst: &mut [u16; 128], tooltip: &str) {
        for (slot, ch) in dst
            .iter_mut()
            .zip(OsStr::new(tooltip).encode_wide())
        {
            *slot = ch;
        }
    }

    fn original_proc(hwnd: isize) -> Option<isize> {
        let windows = WINDOWS.lock().unwrap_or_else(|e| e.into_inner());
        windows
            .iter()
            .find(|(h, _)| *h == hwnd)
            .map(|(_, proc)| *proc)
    }

    fn snapshot() -> Vec<isize> {
        let windows = WINDOWS.lock().unwrap_or_else(|e| e.into_inner());
        windows.iter().map(|(h, _)| *h).collect()
    }

    fn show_tray_menu(hwnd: isize) {
        let menu = unsafe { CreatePopupMenu() };
        if menu == 0 {
            return;
        }
        let show = wide("Show SoundApp");
        let quit = wide("Quit");
        unsafe {
            AppendMenuW(menu, MF_STRING, TRAY_SHOW as usize, show.as_ptr());
            AppendMenuW(menu, MF_STRING, TRAY_QUIT as usize, quit.as_ptr());
            let mut point = Point { x: 0, y: 0 };
            GetCursorPos(&mut point);
            SetForegroundWindow(hwnd);
            let command = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
                point.x,
                point.y,
                0,
                hwnd,
                std::ptr::null(),
            );
            DestroyMenu(menu);
            PostMessageW(hwnd, 0, 0, 0);
            match command {
                value if value == TRAY_SHOW as i32 => {
                    TRAY_ACTION.store(TRAY_SHOW, Ordering::SeqCst)
                }
                value if value == TRAY_QUIT as i32 => {
                    TRAY_ACTION.store(TRAY_QUIT, Ordering::SeqCst)
                }
                _ => {}
            }
        }
    }

    unsafe extern "system" fn proc(hwnd: isize, msg: u32, w: usize, l: isize) -> isize {
        let Some(prev) = original_proc(hwnd) else {
            return DefWindowProcW(hwnd, msg, w, l);
        };
        let call_prev = || unsafe { CallWindowProcW(prev, hwnd, msg, w, l) };
        if msg == WM_TRAY {
            match l as u32 & 0xFFFF {
                WM_LBUTTONUP => TRAY_ACTION.store(TRAY_SHOW, Ordering::SeqCst),
                WM_RBUTTONUP => show_tray_menu(hwnd),
                _ => {}
            }
            return 0;
        }
        let activated = msg == WM_ACTIVATE && (w as u32 & 0xFFFF) != 0;
        let shown = msg == WM_SHOWWINDOW && w != 0;
        if (activated || shown) && GROUP_MINIMIZED.swap(false, Ordering::SeqCst) {
            let result = call_prev();
            restore_others();
            return result;
        }
        if msg == WM_SYSCOMMAND {
            let cmd = w as u32 & 0xFFF0;
            if cmd == SC_MINIMIZE {
                let result = call_prev();
                for other in snapshot() {
                    if other != hwnd
                        && IsWindow(other) != 0
                        && IsWindowVisible(other) != 0
                        && IsIconic(other) == 0
                    {
                        ShowWindow(other, SW_MINIMIZE);
                    }
                }
                GROUP_MINIMIZED.store(true, Ordering::SeqCst);
                return result;
            }
            if cmd == SC_RESTORE && GROUP_MINIMIZED.swap(false, Ordering::SeqCst) {
                let result = call_prev();
                restore_others();
                return result;
            }
        }
        call_prev()
    }

    fn restore_others() {
        for hwnd in snapshot() {
            unsafe {
                if IsWindow(hwnd) != 0 && IsIconic(hwnd) != 0 {
                    ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                    crate::diag::diag(&format!(
                        "nui taskbar-group RESTORE hwnd=0x{hwnd:x} (was iconic) -> mapped={}",
                        IsWindowVisible(hwnd) != 0
                    ));
                }
            }
        }
    }

    pub fn register(window: &Window) {
        let Ok(handle) = window.window_handle() else {
            return;
        };
        let RawWindowHandle::Win32(win32) = handle.as_raw() else {
            return;
        };
        let hwnd = win32.hwnd.get() as isize;
        {
            let windows = WINDOWS.lock().unwrap_or_else(|e| e.into_inner());
            if windows.iter().any(|(h, _)| *h == hwnd) {
                return;
            }
        }
        let prev = unsafe {
            SetWindowLongPtrW(hwnd, GWLP_WNDPROC, proc as usize as isize)
        };
        if prev == 0 {
            eprintln!("[nui] taskbar group: could not subclass window {hwnd}");
            return;
        }
        let mut windows = WINDOWS.lock().unwrap_or_else(|e| e.into_inner());
        windows.push((hwnd, prev));
    }

    pub fn restore_if_needed() {
        if GROUP_MINIMIZED.swap(false, Ordering::SeqCst) {
            restore_others();
        }
    }

    pub fn tray_install(window: &Window, icon_resource: u16, tooltip: &str) {
        if TRAY_ADDED.load(Ordering::SeqCst) {
            return;
        }
        let Ok(handle) = window.window_handle() else {
            return;
        };
        let RawWindowHandle::Win32(win32) = handle.as_raw() else {
            return;
        };
        let hwnd = win32.hwnd.get() as isize;
        let icon = unsafe {
            let module = GetModuleHandleW(std::ptr::null());
            LoadImageW(
                module,
                icon_resource as usize as *const u16,
                IMAGE_ICON,
                0,
                0,
                LR_DEFAULTSIZE | LR_SHARED,
            )
        };
        if icon == 0 {
            eprintln!("[nui] tray icon resource {icon_resource} unavailable");
            return;
        }
        let mut data: NotifyIconDataW = unsafe { std::mem::zeroed() };
        data.cb_size = std::mem::size_of::<NotifyIconDataW>() as u32;
        data.hwnd = hwnd;
        data.uid = 1;
        data.flags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        data.callback_message = WM_TRAY;
        data.icon = icon;
        set_tip(&mut data.tip, tooltip);
        if unsafe { Shell_NotifyIconW(NIM_ADD, &mut data) } != 0 {
            TRAY_HWND.store(hwnd, Ordering::SeqCst);
            TRAY_ADDED.store(true, Ordering::SeqCst);
        } else {
            eprintln!("[nui] Shell_NotifyIconW(NIM_ADD) failed");
        }
    }

    pub fn tray_remove() {
        if !TRAY_ADDED.swap(false, Ordering::SeqCst) {
            return;
        }
        let hwnd = TRAY_HWND.swap(0, Ordering::SeqCst);
        let mut data: NotifyIconDataW = unsafe { std::mem::zeroed() };
        data.cb_size = std::mem::size_of::<NotifyIconDataW>() as u32;
        data.hwnd = hwnd;
        data.uid = 1;
        unsafe {
            Shell_NotifyIconW(NIM_DELETE, &mut data);
        }
    }

    pub fn tray_take_action() -> u8 {
        TRAY_ACTION.swap(0, Ordering::SeqCst)
    }
}

/// Bring a window to the foreground without winit's ~800 ms synthetic Alt tap.
///
/// NOTE the `SWP_SHOWWINDOW`: this maps the window as a side effect. Calling it
/// on a window that was just cloaked shows the HWND again while the cloak keeps
/// it off-screen — the "it comes back on its own" signature. The diagnostics
/// line records what was mapped before and after, and whether the OS actually
/// granted foreground (it refuses unless the caller already owns it).
pub fn focus_window_now(window: &Window) {
    #[cfg(target_os = "windows")]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        #[link(name = "user32")]
        extern "system" {
            fn GetForegroundWindow() -> isize;
            fn SetForegroundWindow(hwnd: isize) -> i32;
            fn IsWindowVisible(hwnd: isize) -> i32;
            fn SetWindowPos(
                hwnd: isize,
                after: isize,
                x: i32,
                y: i32,
                cx: i32,
                cy: i32,
                flags: u32,
            ) -> i32;
        }
        const HWND_TOP: isize = 0;
        const SWP_NOSIZE: u32 = 0x0001;
        const SWP_NOMOVE: u32 = 0x0002;
        const SWP_NOACTIVATE: u32 = 0x0010;
        const SWP_SHOWWINDOW: u32 = 0x0040;
        if let Ok(wh) = window.window_handle() {
            if let RawWindowHandle::Win32(h) = wh.as_raw() {
                let hwnd = h.hwnd.get() as isize;
                unsafe {
                    let fg_before = GetForegroundWindow();
                    let vis_before = IsWindowVisible(hwnd) != 0;
                    SetWindowPos(
                        hwnd,
                        HWND_TOP,
                        0,
                        0,
                        0,
                        0,
                        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
                    );
                    if GetForegroundWindow() != hwnd {
                        SetForegroundWindow(hwnd);
                    }
                    crate::diag::diag(&format!(
                        "nui focus_now hwnd=0x{hwnd:x} mapped={vis_before}->{} fg=0x{fg_before:x}->0x{:x} granted={}",
                        IsWindowVisible(hwnd) != 0,
                        GetForegroundWindow(),
                        GetForegroundWindow() == hwnd
                    ));
                }
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    window.focus_window();
}

/// Make taskbar-button clicks minimize the whole application window group and
/// let a restore click bring the previously visible windows back together.
/// Call once per top-level window after creation.
pub fn install_taskbar_group(window: &Window) {
    #[cfg(target_os = "windows")]
    taskbar_group::register(window);
    #[cfg(not(target_os = "windows"))]
    let _ = window;
}

/// Restore sibling windows when the OS restored one of the group from the
/// taskbar. Call from each window's `Focused(true)` event.
pub fn restore_taskbar_group_if_needed() {
    #[cfg(target_os = "windows")]
    taskbar_group::restore_if_needed();
}

/// Tray action requested by the shell icon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayAction {
    Show,
    Quit,
}

/// Install the shell tray icon on `window` (the app's main HWND), using an
/// embedded icon resource and a tooltip.
pub fn install_tray_icon(window: &Window, icon_resource: u16, tooltip: &str) {
    #[cfg(target_os = "windows")]
    taskbar_group::tray_install(window, icon_resource, tooltip);
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (window, icon_resource, tooltip);
    }
}

pub fn remove_tray_icon() {
    #[cfg(target_os = "windows")]
    taskbar_group::tray_remove();
}

/// Left-click/Show or Quit from the tray context menu.
pub fn take_tray_action() -> Option<TrayAction> {
    #[cfg(target_os = "windows")]
    {
        match taskbar_group::tray_take_action() {
            1 => Some(TrayAction::Show),
            2 => Some(TrayAction::Quit),
            _ => None,
        }
    }
    #[cfg(not(target_os = "windows"))]
    None
}

/// Start an OS window drag while keeping `RedrawRequested` active at ~60 Hz during
/// the Win32 modal move loop.
pub fn drag_window_live(window: &Window) {
    #[cfg(target_os = "windows")]
    live_drag::drag_window(window);
    #[cfg(not(target_os = "windows"))]
    {
        let _ = window.drag_window();
    }
}

/// Start an OS edge/corner resize drag while keeping `RedrawRequested` active at
/// ~60 Hz during the Win32 modal size loop.
pub fn drag_resize_window_live(window: &Window, dir: ResizeDirection) {
    #[cfg(target_os = "windows")]
    live_drag::drag_resize_window(window, dir);
    #[cfg(not(target_os = "windows"))]
    {
        let _ = window.drag_resize_window(dir);
    }
}

/// Hit-test a 6px logical border (with 12px corner zones) on an undecorated window.
/// `cursor` and `win_size` are physical pixels; `scale_factor` converts the
/// logical border width the same way `WindowManager::build_window_attrs` sizes the window.
pub fn hit_test_resize_border(
    cursor: (f32, f32),
    win_size: (u32, u32),
    scale_factor: f64,
) -> Option<ResizeDirection> {
    let dpi = scale_factor as f32;
    let (cw, ch) = (win_size.0 as f32, win_size.1 as f32);
    let (mx, my) = cursor;
    let edge = (6.0 * dpi).round();
    let corner = (12.0 * dpi).round();
    if mx < 0.0 || my < 0.0 || mx > cw || my > ch {
        return None;
    }
    let on_l = mx <= edge;
    let on_r = mx >= cw - edge;
    let on_t = my <= edge;
    let on_b = my >= ch - edge;
    if !(on_l || on_r || on_t || on_b) {
        return None;
    }
    if mx <= corner && my <= corner {
        Some(ResizeDirection::NorthWest)
    } else if mx >= cw - corner && my <= corner {
        Some(ResizeDirection::NorthEast)
    } else if mx <= corner && my >= ch - corner {
        Some(ResizeDirection::SouthWest)
    } else if mx >= cw - corner && my >= ch - corner {
        Some(ResizeDirection::SouthEast)
    } else if on_l {
        Some(ResizeDirection::West)
    } else if on_r {
        Some(ResizeDirection::East)
    } else if on_t {
        Some(ResizeDirection::North)
    } else {
        Some(ResizeDirection::South)
    }
}

/// Update the window's mouse cursor icon to show resize arrows when hovering an edge/corner.
pub fn update_resize_cursor(window: &Window, cursor: (f32, f32), win_size: (u32, u32)) {
    use winit::window::CursorIcon;
    let icon = match hit_test_resize_border(cursor, win_size, window.scale_factor()) {
        Some(ResizeDirection::North) | Some(ResizeDirection::South) => CursorIcon::NsResize,
        Some(ResizeDirection::East) | Some(ResizeDirection::West) => CursorIcon::EwResize,
        Some(ResizeDirection::NorthWest) | Some(ResizeDirection::SouthEast) => {
            CursorIcon::NwseResize
        }
        Some(ResizeDirection::NorthEast) | Some(ResizeDirection::SouthWest) => {
            CursorIcon::NeswResize
        }
        None => CursorIcon::Default,
    };
    window.set_cursor(icon);
}

/// If `cursor` is on the window's resize border, start a live resize drag and return `true`.
pub fn try_start_window_resize(window: &Window, cursor: (f32, f32), win_size: (u32, u32)) -> bool {
    if let Some(dir) = hit_test_resize_border(cursor, win_size, window.scale_factor()) {
        drag_resize_window_live(window, dir);
        true
    } else {
        false
    }
}

/// Whether `window` currently owns OS keyboard focus. Asked of the OS on every
/// call — focus can move without any event reaching us (and tracked state goes
/// stale exactly then), so callers must never infer it from event history.
#[cfg(target_os = "windows")]
pub fn is_foreground(window: &winit::window::Window) -> bool {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    #[link(name = "user32")]
    extern "system" {
        fn GetForegroundWindow() -> isize;
    }
    let Ok(wh) = window.window_handle() else {
        return false;
    };
    let RawWindowHandle::Win32(h) = wh.as_raw() else {
        return false;
    };
    unsafe { GetForegroundWindow() == h.hwnd.get() }
}

#[cfg(not(target_os = "windows"))]
pub fn is_foreground(_window: &winit::window::Window) -> bool {
    false
}

/// Declarative creation spec for a managed window (`WindowManager::ensure_window`).
#[derive(Clone, Debug)]
pub struct WindowSpec {
    pub key: &'static str,
    pub title: &'static str,
    pub width: f64,
    pub height: f64,
    pub min_width: f64,
    pub min_height: f64,
    pub undecorated: bool,
}

/// One live window: native surface + its private renderer / scene / UI state.
pub struct ManagedWindow {
    pub key: &'static str,
    pub title: String,
    pub window: Arc<Window>,
    pub renderer: Renderer,
    pub scene: Scene,
    pub ui_state: UiState,
    pub size: (u32, u32),
    pub visible: bool,
    /// Reveal retries already spent while waiting for the first frame.
    pub reveal_retries: u32,
    /// One-shot screenshot path: taken (and cleared) by the next `RedrawRequested`
    /// render of this window, e.g. `--open-window monitoring --screenshot out.png`.
    pub screenshot_once: Option<String>,
}

pub struct WindowManager {
    pub windows: Vec<ManagedWindow>,
    /// Theme window base color handed to every renderer: the class brush and
    /// first fill use it, so the pre-first-frame background matches the theme.
    base: crate::widgets::Color,
    icon_resource: Option<u16>,
}

/// One window's logical state next to the OS's own answers. The two disagree
/// exactly when a toggle looks like it "did nothing" or "came back".
#[derive(Clone, Debug)]
pub struct WindowProbe {
    pub key: &'static str,
    /// What the app believes (`ManagedWindow::visible`).
    pub logical_visible: bool,
    /// Show requested, first present not yet delivered.
    pub pending_reveal: bool,
    pub reveal_ms: Option<f64>,
    /// Frames actually presented since creation.
    pub frames: u64,
    /// `IsWindowVisible` — mapped per the OS.
    pub os_visible: bool,
    /// `DWMWA_CLOAK` — composed off-screen.
    pub cloaked: bool,
    pub size: (u32, u32),
    pub minimized: bool,
}

impl WindowProbe {
    /// `true` when what the user sees cannot match what the app reports:
    /// logically shown but the OS keeps it cloaked/hidden, or the reverse.
    pub fn diverged(&self) -> bool {
        self.logical_visible != (self.os_visible && !self.cloaked)
    }

    /// Compact one-line form for the event log.
    pub fn summary(&self) -> String {
        let pend = match self.reveal_ms {
            Some(ms) => format!("{ms:.0}ms"),
            None => "-".to_string(),
        };
        format!(
            "{}[log={} os={} cloak={} pend={} f={}]",
            self.key,
            u8::from(self.logical_visible),
            u8::from(self.os_visible),
            u8::from(self.cloaked),
            pend,
            self.frames
        )
    }
}

/// Apply the executable's embedded icon resource to a native window.
#[cfg(target_os = "windows")]
pub fn set_window_icon_from_executable(window: &Window, resource_id: u16) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    #[link(name = "user32")]
    extern "system" {
        fn LoadImageW(
            instance: isize,
            name: *const u16,
            kind: u32,
            cx: i32,
            cy: i32,
            flags: u32,
        ) -> isize;
        fn SendMessageW(hwnd: isize, msg: u32, wp: usize, lp: isize) -> isize;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetModuleHandleW(name: *const u16) -> isize;
    }
    const IMAGE_ICON: u32 = 1;
    const LR_DEFAULTSIZE: u32 = 0x0000_0040;
    const LR_SHARED: u32 = 0x0000_8000;
    const WM_SETICON: u32 = 0x0080;
    const ICON_SMALL: usize = 0;
    const ICON_BIG: usize = 1;
    if let Ok(wh) = window.window_handle() {
        if let RawWindowHandle::Win32(h) = wh.as_raw() {
            let hwnd = h.hwnd.get() as isize;
            unsafe {
                let module = GetModuleHandleW(std::ptr::null());
                for slot in [ICON_SMALL, ICON_BIG] {
                    let icon = LoadImageW(
                        module,
                        resource_id as usize as *const u16,
                        IMAGE_ICON,
                        0,
                        0,
                        LR_DEFAULTSIZE | LR_SHARED,
                    );
                    if icon != 0 {
                        SendMessageW(hwnd, WM_SETICON, slot, icon);
                    }
                }
            }
        }
    }
}

#[cfg(not(target_os = "windows"))]
pub fn set_window_icon_from_executable(_window: &Window, _resource_id: u16) {}

impl WindowManager {
    pub fn new(base: crate::widgets::Color) -> Self {
        Self {
            windows: Vec::new(),
            base,
            icon_resource: None,
        }
    }

    pub fn set_icon_resource(&mut self, resource_id: u16) {
        self.icon_resource = Some(resource_id);
        for window in &self.windows {
            set_window_icon_from_executable(&window.window, resource_id);
        }
    }

    /// Window attributes shared by every managed window: logical size, min size and —
    /// on Windows — the undecorated-shell treatment (rounded corners, drop shadow,
    /// `55,55,55` border) matching the primary window's look.
    pub fn build_window_attrs(spec: &WindowSpec) -> WindowAttributes {
        let mut attrs = Window::default_attributes()
            .with_title(spec.title)
            .with_resizable(true)
            .with_inner_size(LogicalSize::new(spec.width, spec.height))
            .with_min_inner_size(LogicalSize::new(spec.min_width, spec.min_height))
            .with_decorations(!spec.undecorated)
            .with_theme(Some(winit::window::Theme::Dark));

        #[cfg(target_os = "windows")]
        {
            use winit::platform::windows::{Color, CornerPreference, WindowAttributesExtWindows};
            attrs = attrs
                .with_undecorated_shadow(true)
                .with_corner_preference(CornerPreference::Round)
                .with_border_color(Some(Color::from_rgb(55, 55, 55)))
                .with_skip_taskbar(true);
        }

        attrs
    }

    /// Return the index of the window with `spec.key`, creating it (hidden when
    /// `visible` is `false`) if this is its first use. Cheap to call every frame.
    pub fn ensure_window(
        &mut self,
        event_loop: &ActiveEventLoop,
        spec: &WindowSpec,
        prefer: BackendKind,
        visible: bool,
    ) -> usize {
        if let Some(idx) = self.windows.iter().position(|w| w.key == spec.key) {
            return idx;
        }
        let attrs = Self::build_window_attrs(spec).with_visible(visible);
        let (window, mut renderer) = Renderer::new(event_loop, attrs, prefer, self.base);
        renderer.set_label(spec.key);
        let sz = window.inner_size();
        crate::diag::diag(&format!(
            "nui[{}] CREATE backend={:?} requested_visible={visible} size={}x{} pending_reveal={}",
            spec.key,
            renderer.active,
            sz.width,
            sz.height,
            renderer.probe().pending_reveal
        ));
        install_taskbar_group(&window);
        if let Some(id) = self.icon_resource {
            set_window_icon_from_executable(&window, id);
        }
        if visible {
            renderer.request_reveal();
        }
        self.windows.push(ManagedWindow {
            key: spec.key,
            title: spec.title.to_string(),
            window,
            renderer,
            scene: Scene::new(),
            ui_state: UiState::new(),
            size: (sz.width, sz.height),
            visible,
            reveal_retries: 0,
            screenshot_once: None,
        });
        self.windows.len() - 1
    }

    /// Make sure the window exists, then mark it for reveal + redraw. The actual
    /// `set_visible(true)` happens at the end of its first `render()` — after the
    /// frame is swapped — so the window never flashes white.
    pub fn show(&mut self, event_loop: &ActiveEventLoop, spec: &WindowSpec, prefer: BackendKind) {
        let existed = self.windows.iter().any(|w| w.key == spec.key);
        let idx = self.ensure_window(event_loop, spec, prefer, true);
        let w = &mut self.windows[idx];
        crate::diag::diag(&format!(
            "nui[{}] SHOW existed={existed} logical_was={} os_visible={} cloaked={} frames={}",
            spec.key,
            w.visible,
            w.renderer.os_visible(),
            w.renderer.os_cloaked(),
            w.renderer.probe().frames
        ));
        w.visible = true;
        w.renderer.request_reveal();
        w.window.request_redraw();
    }

    /// Hide the native surface but keep window + renderer alive for instant re-show.
    pub fn hide(&mut self, key: &str) {
        if let Some(w) = self.windows.iter_mut().find(|w| w.key == key) {
            w.visible = false;
            w.renderer.cloak_and_hide();
        }
    }

    /// Toggle visibility; returns the new visibility (`true` = now shown).
    pub fn toggle(
        &mut self,
        event_loop: &ActiveEventLoop,
        spec: &WindowSpec,
        prefer: BackendKind,
    ) -> bool {
        if self.is_visible(spec.key) {
            self.hide(spec.key);
            false
        } else {
            self.show(event_loop, spec, prefer);
            true
        }
    }

    pub fn is_visible(&self, key: &str) -> bool {
        self.windows
            .iter()
            .find(|w| w.key == key)
            .map(|w| w.visible)
            .unwrap_or(false)
    }

    /// Full state of one window — the host's watchdog input. `None` when the
    /// window has never been created this session.
    pub fn probe(&self, key: &str) -> Option<WindowProbe> {
        self.windows.iter().find(|w| w.key == key).map(|w| {
            let probe = w.renderer.probe();
            WindowProbe {
                key: w.key,
                logical_visible: w.visible,
                pending_reveal: probe.pending_reveal,
                reveal_ms: probe.reveal_ms,
                frames: probe.frames,
                os_visible: probe.os_visible,
                cloaked: probe.cloaked,
                size: w.size,
                minimized: w.window.is_minimized().unwrap_or(false),
            }
        })
    }

    /// Every created window's state, in creation order.
    pub fn probes(&self) -> Vec<WindowProbe> {
        self.windows
            .iter()
            .map(|w| {
                let probe = w.renderer.probe();
                WindowProbe {
                    key: w.key,
                    logical_visible: w.visible,
                    pending_reveal: probe.pending_reveal,
                    reveal_ms: probe.reveal_ms,
                    frames: probe.frames,
                    os_visible: probe.os_visible,
                    cloaked: probe.cloaked,
                    size: w.size,
                    minimized: w.window.is_minimized().unwrap_or(false),
                }
            })
            .collect()
    }

    /// Re-nudge reveals that have waited longer than `threshold_ms` for their
    /// first frame, up to `MAX_REVEAL_RETRIES`. Returns
    /// `(key, waited_ms, attempt)` for every retry so the host can log them —
    /// a silent recovery would hide the fact that the first request was lost.
    pub fn retry_pending_reveals(
        &mut self,
        threshold_ms: f64,
    ) -> Vec<(&'static str, f64, u32)> {
        const MAX_REVEAL_RETRIES: u32 = 12;
        let mut out = Vec::new();
        for w in &mut self.windows {
            let probe = w.renderer.probe();
            if !probe.pending_reveal {
                w.reveal_retries = 0;
                continue;
            }
            let waited = probe.reveal_ms.unwrap_or(0.0);
            if waited < threshold_ms || w.reveal_retries >= MAX_REVEAL_RETRIES {
                continue;
            }
            w.reveal_retries += 1;
            w.renderer.nudge_redraw();
            out.push((w.key, waited, w.reveal_retries));
        }
        out
    }

    pub fn find_by_id_mut(&mut self, id: WindowId) -> Option<&mut ManagedWindow> {
        self.windows.iter_mut().find(|w| w.window.id() == id)
    }

    pub fn find_by_key_mut(&mut self, key: &str) -> Option<&mut ManagedWindow> {
        self.windows.iter_mut().find(|w| w.key == key)
    }

    pub fn request_redraw_all(&self) {
        for w in &self.windows {
            if w.visible {
                w.window.request_redraw();
            }
        }
    }

    /// `true` when any visible window still has a UI transition in flight. Drives the
    /// host's 60 fps repaint timer so widget animations finish before going idle.
    pub fn any_animating(&self) -> bool {
        self.windows
            .iter()
            .any(|w| w.visible && w.ui_state.is_animating())
    }
}

impl<'a> Ui<'a> {
    /// Reusable secondary-window header: a `3.0rem` bar (`theme.shade2` fill +
    /// `1px` `theme.border2` bottom rule) with a left icon + title, an optional
    /// primary badge, and a right-side ghost close button (widget ID `990`).
    /// Returns `(header_height, close_clicked)`.
    pub fn secondary_window_header(
        &mut self,
        pw: f32,
        title: &str,
        icon: &str,
        badge_text: &str,
    ) -> (f32, bool) {
        let t = self.theme;
        let hdr_h = (t.rem * 3.0).round();
        self.scene.rect(0.0, 0.0, pw, hdr_h, t.shade2);
        self.scene.rect(0.0, hdr_h - 1.0, pw, 1.0, t.border2);

        let icon_sz = (t.rem * 1.25).round();
        self.scene.icon_centered(
            self.text_sys,
            icon,
            t.rem * 0.85 + icon_sz * 0.5,
            hdr_h * 0.5,
            icon_sz,
            t.text_dim,
        );
        let title_fs = t.font_base();
        let title_w = self.scene.text(
            self.text_sys,
            FontFace::Regular,
            self.font_engine,
            title,
            (t.rem * 2.35).round(),
            (hdr_h * 0.5 + title_fs * 0.34).round(),
            title_fs,
            t.text,
        );

        let badge_y = (hdr_h - (t.rem * 1.48).round()) * 0.5;
        let bx = t.rem * 2.35 + title_w + t.rem * 0.85;
        if !badge_text.is_empty() {
            let _ = self.badge(bx, badge_y, badge_text, BadgeVariant::Primary, None);
        }

        let btn_h = (t.rem * 1.75).round();
        let btn_y = (hdr_h - btn_h) * 0.5;
        let close_w = (t.rem * 2.1).round();
        let close_clicked = self.button(
            990,
            Rect::new(pw - t.rem * 0.6 - close_w, btn_y, close_w, btn_h),
            "",
            ICON_CLOSE,
            ButtonVariant::Ghost,
        );
        (hdr_h, close_clicked)
    }
}
