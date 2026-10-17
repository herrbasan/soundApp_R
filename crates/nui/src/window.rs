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
    /// One-shot screenshot path: taken (and cleared) by the next `RedrawRequested`
    /// render of this window, e.g. `--open-window monitoring --screenshot out.png`.
    pub screenshot_once: Option<String>,
}

pub struct WindowManager {
    pub windows: Vec<ManagedWindow>,
}

impl WindowManager {
    pub fn new() -> Self {
        Self { windows: Vec::new() }
    }

    /// Window attributes shared by every managed window: logical size, min size and —
    /// on Windows — the undecorated-shell treatment (rounded corners, drop shadow,
    /// `55,55,55` border) matching the primary window's look.
    pub fn build_window_attrs(spec: &WindowSpec) -> WindowAttributes {
        let mut attrs = Window::default_attributes()
            .with_title(spec.title)
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
                .with_border_color(Some(Color::from_rgb(55, 55, 55)));
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
        let (window, mut renderer) = Renderer::new(event_loop, attrs, prefer);
        if visible {
            renderer.request_reveal();
        }
        let sz = window.inner_size();
        self.windows.push(ManagedWindow {
            key: spec.key,
            title: spec.title.to_string(),
            window,
            renderer,
            scene: Scene::new(),
            ui_state: UiState::new(),
            size: (sz.width, sz.height),
            visible,
            screenshot_once: None,
        });
        self.windows.len() - 1
    }

    /// Make sure the window exists, then mark it for reveal + redraw. The actual
    /// `set_visible(true)` happens at the end of its first `render()` — after the
    /// frame is swapped — so the window never flashes white.
    pub fn show(&mut self, event_loop: &ActiveEventLoop, spec: &WindowSpec, prefer: BackendKind) {
        let idx = self.ensure_window(event_loop, spec, prefer, true);
        let w = &mut self.windows[idx];
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
}

impl<'a> Ui<'a> {
    /// Reusable secondary-window header: a `3.0rem` bar (`theme.shade2` fill +
    /// `1px` `theme.border2` bottom rule) with the left icon + SemiBold title +
    /// primary badge, and a right-side `ICON_CLOSE` danger button (widget ID `990`).
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
            t.highlight,
        );
        let title_fs = t.font_base();
        let title_w = self.scene.text(
            self.text_sys,
            FontFace::SemiBold,
            self.font_engine,
            title,
            (t.rem * 2.35).round(),
            (hdr_h * 0.5 + title_fs * 0.34).round(),
            title_fs,
            t.text,
        );

        let badge_y = (hdr_h - (t.rem * 1.48).round()) * 0.5;
        let bx = t.rem * 2.35 + title_w + t.rem * 0.85;
        let _ = self.badge(bx, badge_y, badge_text, BadgeVariant::Primary, None);

        let btn_h = (t.rem * 1.75).round();
        let btn_y = (hdr_h - btn_h) * 0.5;
        let close_w = (t.rem * 2.1).round();
        let close_clicked = self.button(
            990,
            Rect::new(pw - t.rem * 0.6 - close_w, btn_y, close_w, btn_h),
            "",
            ICON_CLOSE,
            ButtonVariant::Danger,
        );
        (hdr_h, close_clicked)
    }
}
