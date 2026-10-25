// NUI Rust (nui_wc2 v3.0.0 "slate" theme & component port)
//
// Faithful port of https://github.com/herrbasan/nui_wc2 visual tokens and components:
//   - Theme: exact light-dark() slate ramp (base, shade1..shade9, border1..4, highlight/dim, palettes)
//   - Components:
//       * nui-tabs (bottom border + 2px active highlight bar + hover shade2)
//       * nui-button (Primary, Outline, Ghost, Danger, Success, Icon)
//       * nui-button-container[variant="segmented"] (joined buttons, 1px dividers, highlight active)
//       * nui-input + nui-input-group (uppercase label, rgb(45,45,45) surface, caret/selection, clear button)
//       * nui-select (shade3 control, chevron arrow, top-layer popup with checkmark rows)
//       * nui-slider (0.25rem pill track, highlight fill, 1rem highlight thumb scaling 1.35x on drag, double-click reset)
//       * nui-checkbox (1.25rem box, 2px shade4 border, base bg, highlight checked + checkmark)
//       * nui-checkbox[variant="switch"] (2.25x1.25em pill, shade4 -> highlight, white thumb)
//       * nui-radio (1.25rem circle, 2px border, 0.5rem inner highlight dot)
//       * nui-badge (base bg, border-shade3, 6px left semantic stripe + optional status dot)

#![allow(dead_code)]

use crate::layout::LayoutCursor;
use crate::render::Scene;
use crate::text::{FontEngine, FontFace, TextSystem};

pub type Color = (u8, u8, u8, u8);

#[inline]
pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
    (r, g, b, 255)
}

#[inline]
pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Color {
    (r, g, b, a)
}

// ============================================================================
// Zero-dependency Win32 clipboard (CF_UNICODETEXT via user32 / kernel32 FFI)
// ============================================================================

#[cfg(target_os = "windows")]
mod clip {
    use std::ffi::c_void;

    type Hwnd = *mut c_void;
    type Handle = *mut c_void;

    #[link(name = "user32")]
    extern "system" {
        fn OpenClipboard(hwnd: Hwnd) -> i32;
        fn CloseClipboard() -> i32;
        fn EmptyClipboard() -> i32;
        fn GetClipboardData(format: u32) -> Handle;
        fn SetClipboardData(format: u32, mem: Handle) -> Handle;
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GlobalAlloc(flags: u32, bytes: usize) -> Handle;
        fn GlobalLock(mem: Handle) -> *mut c_void;
        fn GlobalUnlock(mem: Handle) -> i32;
        fn GlobalSize(mem: Handle) -> usize;
    }

    const CF_UNICODETEXT: u32 = 13;
    const GMEM_MOVEABLE: u32 = 0x0002;

    pub fn get_text() -> Option<String> {
        unsafe {
            if OpenClipboard(std::ptr::null_mut()) == 0 {
                return None;
            }
            let handle = GetClipboardData(CF_UNICODETEXT);
            let result = if handle.is_null() {
                None
            } else {
                let ptr = GlobalLock(handle) as *const u16;
                if ptr.is_null() {
                    None
                } else {
                    let count = GlobalSize(handle) / 2;
                    let mut len = 0usize;
                    while len < count && *ptr.add(len) != 0 {
                        len += 1;
                    }
                    let s = String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len));
                    GlobalUnlock(handle);
                    Some(s)
                }
            };
            CloseClipboard();
            result
        }
    }

    pub fn set_text(text: &str) {
        unsafe {
            let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
            if OpenClipboard(std::ptr::null_mut()) == 0 {
                return;
            }
            EmptyClipboard();
            let mem = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2);
            if !mem.is_null() {
                let dst = GlobalLock(mem) as *mut u16;
                if !dst.is_null() {
                    std::ptr::copy_nonoverlapping(wide.as_ptr(), dst, wide.len());
                    GlobalUnlock(mem);
                    // Ownership of `mem` transfers to the clipboard on success.
                    SetClipboardData(CF_UNICODETEXT, mem);
                }
            }
            CloseClipboard();
        }
    }
}

/// Read the system clipboard as UTF-8 text (Windows only; `None` elsewhere).
#[cfg(target_os = "windows")]
pub fn clipboard_get_text() -> Option<String> {
    clip::get_text()
}

/// Replace the system clipboard with UTF-8 text (Windows only; no-op elsewhere).
#[cfg(target_os = "windows")]
pub fn clipboard_set_text(text: &str) {
    clip::set_text(text)
}

#[cfg(not(target_os = "windows"))]
pub fn clipboard_get_text() -> Option<String> {
    None
}

#[cfg(not(target_os = "windows"))]
pub fn clipboard_set_text(_text: &str) {}

// MaterialIcons-Regular.ttf codepoints matching nui_wc2 sprite usage
pub const ICON_PLAY: &str = "\u{e037}";
pub const ICON_PAUSE: &str = "\u{e034}";
pub const ICON_PREV: &str = "\u{e045}";
pub const ICON_NEXT: &str = "\u{e044}";
pub const ICON_CHECK: &str = "\u{e5ca}";
pub const ICON_CLOSE: &str = "\u{e5cd}";
pub const ICON_CHEVRON_DOWN: &str = "\u{e5cf}";
pub const ICON_CHEVRON_UP: &str = "\u{e5ce}";
pub const ICON_SEARCH: &str = "\u{e8b6}";
pub const ICON_SETTINGS: &str = "\u{e8b8}";
pub const ICON_DARK_MODE: &str = "\u{e3a7}"; // brightness_2 (crescent moon)
pub const ICON_LIGHT_MODE: &str = "\u{e3e4}"; // flare (sun)
pub const ICON_EQ: &str = "\u{e01d}";
pub const ICON_VOLUME: &str = "\u{e050}";
pub const ICON_LOCK: &str = "\u{e897}";
pub const ICON_TUNE: &str = "\u{e429}";
pub const ICON_LOOP: &str = "\u{e040}";
pub const ICON_SHUFFLE: &str = "\u{e043}";
pub const ICON_RESTART: &str = "\u{e042}";
pub const ICON_ADD: &str = "\u{e145}";
pub const ICON_MENU: &str = "\u{e5d2}";
pub const ICON_CHEVRON_RIGHT: &str = "\u{e5cc}";
pub const ICON_CHEVRON_LEFT: &str = "\u{e5cb}";
pub const ICON_FOLDER: &str = "\u{e2c7}";
pub const ICON_INFO: &str = "\u{e88e}";
pub const ICON_MORE_VERT: &str = "\u{e5d4}";

// ============================================================================
// Interpolation helpers — every widget transition eases through these so hover,
// press and popup motion share one timing vocabulary.
// ============================================================================

#[inline]
pub fn lerp_f32(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

#[inline]
pub fn lerp_color(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    (
        (a.0 as f32 + (b.0 as f32 - a.0 as f32) * t).round() as u8,
        (a.1 as f32 + (b.1 as f32 - a.1 as f32) * t).round() as u8,
        (a.2 as f32 + (b.2 as f32 - a.2 as f32) * t).round() as u8,
        (a.3 as f32 + (b.3 as f32 - a.3 as f32) * t).round() as u8,
    )
}

#[inline]
pub fn ease_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// Scale a colour's alpha channel. Used for every popup/dialog fade: `lerp_color`
/// moves the RGB channels too, which is wrong when only the *opacity* should change.
#[inline]
pub fn scale_alpha(c: Color, factor: f32) -> Color {
    (c.0, c.1, c.2, (c.3 as f32 * factor.clamp(0.0, 1.0)).round() as u8)
}

/// Snappy deceleration curve matching `nui-theme.css` `cubic-bezier(0.1, 0.9, 0.2, 1.0)`:
/// fast initial velocity with a smooth, zero-overshoot settle.
#[inline]
pub fn ease_out_snappy(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(4)
}

/// Strong symmetric ease-in-out S-curve (quartic `easeInOut`):
/// crisp acceleration into a fast middle and smooth settle at both ends,
/// identical in both forward (`0 -> 1`) and reverse (`1 -> 0`) directions.
#[inline]
pub fn ease_in_out_strong(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        8.0 * t * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(4) * 0.5
    }
}

/// Exact nui_wc2 v3.0.0 "slate" theme tokens (NUI/css/nui-theme.css)
#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub dark: bool,
    pub rem: f32, // physical px for 1rem (default 15.0 * dpi_scale * ui_zoom)

    pub base: Color,
    pub contrast: Color,
    pub shade1: Color,
    pub shade2: Color,
    pub shade3: Color,
    pub shade4: Color,
    pub shade5: Color,
    pub shade6: Color,
    pub shade7: Color,
    pub shade8: Color,
    pub shade9: Color,

    pub border1: Color,
    pub border2: Color,
    pub border3: Color,
    pub border4: Color,

    pub text: Color,
    pub text_dim: Color,
    pub text_muted: Color,
    pub input_bg: Color,
    pub label_upper: Color,

    pub highlight: Color,
    pub highlight_dim: Color,
    pub palette_gray: Color,
    pub palette_gray_hi: Color,
    pub palette_mark: Color,
    pub palette_mark_hi: Color,
    pub palette_activate: Color,
    pub palette_activate_hi: Color,
    pub palette_alert: Color,
    pub palette_alert_hi: Color,
}

impl Theme {
    pub fn dark(rem: f32) -> Self {
        Self {
            dark: true,
            rem,
            base: rgb(20, 20, 20),
            contrast: rgb(255, 255, 255),
            shade1: rgb(30, 30, 30),
            shade2: rgb(40, 40, 40),
            shade3: rgb(60, 60, 60),
            shade4: rgb(80, 80, 80),
            shade5: rgb(128, 128, 128),
            shade6: rgb(160, 160, 160),
            shade7: rgb(180, 180, 180),
            shade8: rgb(200, 200, 200),
            shade9: rgb(220, 220, 220),

            border1: rgb(45, 45, 45),
            border2: rgb(55, 55, 55),
            border3: rgb(70, 70, 70),
            border4: rgb(80, 80, 80),

            text: rgb(230, 230, 230),
            text_dim: rgb(180, 180, 180),
            text_muted: rgb(160, 160, 160),
            input_bg: rgb(45, 45, 45),
            label_upper: rgba(255, 255, 255, 135),

            highlight: rgb(76, 132, 229),
            highlight_dim: rgb(67, 112, 191),
            palette_gray: rgb(130, 130, 130),
            palette_gray_hi: rgb(150, 150, 150),
            palette_mark: rgb(203, 184, 55),
            palette_mark_hi: rgb(197, 181, 83),
            palette_activate: rgb(29, 171, 10),
            palette_activate_hi: rgb(41, 228, 17),
            palette_alert: rgb(181, 36, 36),
            palette_alert_hi: rgb(222, 61, 61),
        }
    }

    pub fn light(rem: f32) -> Self {
        Self {
            dark: false,
            rem,
            base: rgb(255, 255, 255),
            contrast: rgb(0, 0, 0),
            shade1: rgb(250, 250, 250),
            shade2: rgb(240, 240, 240),
            shade3: rgb(230, 230, 230),
            shade4: rgb(210, 210, 210),
            shade5: rgb(128, 128, 128),
            shade6: rgb(100, 100, 100),
            shade7: rgb(80, 80, 80),
            shade8: rgb(60, 60, 60),
            shade9: rgb(40, 40, 40),

            border1: rgb(235, 235, 235),
            border2: rgb(220, 220, 220),
            border3: rgb(190, 190, 190),
            border4: rgb(120, 120, 120),

            text: rgb(30, 30, 30),
            text_dim: rgb(90, 90, 90),
            text_muted: rgb(100, 100, 100),
            input_bg: rgb(255, 255, 255),
            label_upper: rgba(0, 0, 0, 135),

            highlight: rgb(76, 132, 229),
            highlight_dim: rgb(67, 112, 191),
            palette_gray: rgb(100, 100, 100),
            palette_gray_hi: rgb(120, 120, 120),
            palette_mark: rgb(180, 160, 40),
            palette_mark_hi: rgb(170, 150, 60),
            palette_activate: rgb(40, 120, 30),
            palette_activate_hi: rgb(60, 150, 50),
            palette_alert: rgb(160, 25, 25),
            palette_alert_hi: rgb(200, 50, 50),
        }
    }

    /// `--border-radius1: var(--nui-space-eighth)`.
    #[inline]
    pub fn radius1(&self) -> f32 {
        self.space_eighth()
    }

    /// `--border-radius2: var(--nui-space-quarter)`.
    #[inline]
    pub fn radius2(&self) -> f32 {
        self.space_quarter()
    }

    /// `--border-radius3: var(--nui-space-half)`.
    #[inline]
    pub fn radius3(&self) -> f32 {
        self.space_half()
    }

    /// `--nui-space: 1rem` — the base spacing unit all gaps/padding derive from.
    #[inline]
    pub fn space(&self) -> f32 {
        self.rem.round()
    }

    /// `calc(var(--nui-space) * 0.125)`.
    #[inline]
    pub fn space_eighth(&self) -> f32 {
        (self.rem * 0.125).round().max(1.0)
    }

    /// `--nui-space-quarter` = `calc(var(--nui-space) * 0.25)`.
    #[inline]
    pub fn space_quarter(&self) -> f32 {
        (self.rem * 0.25).round().max(2.0)
    }

    /// `--nui-space-half` = `calc(var(--nui-space) * 0.5)`.
    #[inline]
    pub fn space_half(&self) -> f32 {
        (self.rem * 0.5).round()
    }

    /// `calc(var(--nui-space) * 2)`.
    #[inline]
    pub fn space_double(&self) -> f32 {
        (self.rem * 2.0).round()
    }

    /// `calc(var(--nui-space) * 3)`.
    #[inline]
    pub fn space_triple(&self) -> f32 {
        (self.rem * 3.0).round()
    }

    /// `calc(var(--nui-space) * 4)`.
    #[inline]
    pub fn space_quadruple(&self) -> f32 {
        (self.rem * 4.0).round()
    }

    /// `--nui-layout-gap` default `1rem`.
    #[inline]
    pub fn layout_gap(&self) -> f32 {
        self.space()
    }

    /// `nui-card` padding — `var(--nui-space)`.
    #[inline]
    pub fn card_padding(&self) -> f32 {
        self.space()
    }

    /// `nui-card` gap between children — `var(--nui-space)`.
    #[inline]
    pub fn card_gap(&self) -> f32 {
        self.space()
    }

    /// `nui-input-group` gap between label and control — `var(--nui-space-quarter)`.
    #[inline]
    pub fn label_gap(&self) -> f32 {
        self.space_quarter()
    }

    /// `nui-input-group` margin-bottom to the next group — `var(--nui-space)`.
    #[inline]
    pub fn group_gap(&self) -> f32 {
        self.space()
    }

    /// `nui-input-group > label` line box — `1.28rem` (font-size-xsmall × 1.6).
    #[inline]
    pub fn label_line_height(&self) -> f32 {
        (self.rem * 1.28).round()
    }

    /// `nui-form-row` gap between controls — `var(--nui-space)`.
    #[inline]
    pub fn form_row_gap(&self) -> f32 {
        self.space()
    }

    /// `nui-button-container` gap — `var(--nui-space-half)`.
    #[inline]
    pub fn button_gap(&self) -> f32 {
        self.space_half()
    }

    /// Base `button` min-height — `var(--nui-space-double)`.
    #[inline]
    pub fn button_height(&self) -> f32 {
        self.space_double()
    }

    /// Base `button` horizontal padding — `var(--nui-space)`.
    #[inline]
    pub fn button_pad_x(&self) -> f32 {
        self.space()
    }

    /// `nui-button button` min-width — `6rem`.
    #[inline]
    pub fn button_min_width(&self) -> f32 {
        (self.rem * 6.0).round()
    }

    /// `nui-tabs [role=tablist]` gap — `var(--nui-space-quarter)`.
    #[inline]
    pub fn tab_gap(&self) -> f32 {
        self.space_quarter()
    }

    /// `nui-tabs [role=tablist]` margin-bottom — `var(--nui-space)`.
    #[inline]
    pub fn tabs_margin_bottom(&self) -> f32 {
        self.space()
    }

    /// `nui-tabs [role=tab]` horizontal padding — `var(--nui-space)`.
    #[inline]
    pub fn tab_pad_x(&self) -> f32 {
        self.space()
    }

    /// `nui-link-list` item min-height — `var(--nui-space-double)`.
    #[inline]
    pub fn link_item_height(&self) -> f32 {
        self.space_double()
    }

    /// `nui-menu-bar` height — `var(--nui-space-double)`.
    #[inline]
    pub fn menu_bar_height(&self) -> f32 {
        self.space_double()
    }

    /// `nui-page > section` bottom margin — `var(--nui-space-triple)`.
    #[inline]
    pub fn section_gap(&self) -> f32 {
        self.space_triple()
    }

    /// `nui-page > *` horizontal padding — `var(--nui-space-double)`.
    #[inline]
    pub fn page_pad_x(&self) -> f32 {
        self.space_double()
    }

    /// `nui-page > *` vertical padding — `var(--nui-space)`.
    #[inline]
    pub fn page_pad_y(&self) -> f32 {
        self.space()
    }

    /// `nui-checkbox` gap between box and label — `var(--nui-space-half)`.
    #[inline]
    pub fn checkbox_gap(&self) -> f32 {
        self.space_half()
    }

    /// Generic control min-height (`nui-checkbox` `2rem`).
    #[inline]
    pub fn control_height(&self) -> f32 {
        (self.rem * 2.0).round()
    }

    /// `--icon-size: 1.2rem`.
    #[inline]
    pub fn icon_size(&self) -> f32 {
        (self.rem * 1.2).round()
    }

    /// `--font-size-xlarge: 2rem`.
    #[inline]
    pub fn font_xlarge(&self) -> f32 {
        (self.rem * 2.0).round()
    }

    /// `--nui-form-row-height: 2.5rem`.
    #[inline]
    pub fn form_row_height(&self) -> f32 {
        (self.rem * 2.5).round()
    }

    /// `nui-form-row[size="small"]`: `2.0rem`.
    #[inline]
    pub fn form_row_small(&self) -> f32 {
        (self.rem * 2.0).round()
    }

    /// `--space-page-maxwidth: 56rem`.
    #[inline]
    pub fn page_maxwidth(&self) -> f32 {
        (self.rem * 56.0).round()
    }

    /// `--sidebar-width: 21rem`.
    #[inline]
    pub fn sidebar_width(&self) -> f32 {
        (self.rem * 21.0).round()
    }

    /// App header strip height: `3.25rem`.
    #[inline]
    pub fn header_height(&self) -> f32 {
        (self.rem * 3.25).round()
    }

    /// nui-theme.css type ramp (lines 20-26).
    #[inline]
    pub fn font_xsmall(&self) -> f32 {
        (self.rem * 0.80).round()
    }

    #[inline]
    pub fn font_small(&self) -> f32 {
        (self.rem * 0.90).round()
    }

    #[inline]
    pub fn font_base(&self) -> f32 {
        self.rem.round()
    }

    #[inline]
    pub fn font_medium(&self) -> f32 {
        (self.rem * 1.20).round()
    }

    #[inline]
    pub fn font_large(&self) -> f32 {
        (self.rem * 1.50).round()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    #[inline]
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    #[inline]
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ButtonVariant {
    Primary,
    Outline,
    Ghost,
    Danger,
    Success,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BadgeVariant {
    Primary,
    Success,
    Warning,
    Danger,
}

/// One row of a `nui-menu-dropdown` / `nui-context-menu`.
#[derive(Clone, Debug)]
pub enum MenuItem<'a> {
    Action {
        label: &'a str,
        icon: &'a str,
        shortcut: &'a str,
        action: &'a str,
        disabled: bool,
        checked: Option<bool>,
    },
    Separator,
    Submenu {
        label: &'a str,
        icon: &'a str,
        items: &'a [MenuItem<'a>],
        disabled: bool,
    },
}

/// A top-level `.nui-menu-bar` item and the dropdown it owns.
#[derive(Clone, Debug)]
pub struct MenuBarEntry<'a> {
    pub label: &'a str,
    pub items: &'a [MenuItem<'a>],
}

/// One row of an `nui-link-list[mode="fold"]`: a collapsible group, a navigable
/// item, or a hairline separator.
#[derive(Clone, Debug)]
pub enum LinkListEntry<'a> {
    Group {
        id: u32,
        label: &'a str,
        icon: &'a str,
        expanded: bool,
        items: &'a [LinkListEntry<'a>],
    },
    Item {
        key: &'a str,
        label: &'a str,
        icon: &'a str,
        badge: &'a str,
    },
    Separator,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KeyCmd {
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    Enter,
    Escape,
    SelectAll,
    Copy,
    Cut,
    Paste,
    Tab,
    ShiftTab,
    Activate,
}

pub struct UiState {
    pub cursor: (f32, f32),
    pub mouse_down: bool,
    pub mouse_pressed: bool,
    pub mouse_released: bool,
    pub right_pressed: bool,
    pub double_clicked: bool,
    pub wheel_y: f32,
    pub typed_chars: String,
    pub key_cmds: Vec<KeyCmd>,

    pub hovered_id: Option<u32>,
    pub active_id: Option<u32>,
    pub focused_id: Option<u32>,
    pub open_popup_id: Option<u32>,
    pub popup_rect: Option<Rect>,

    // TextInput editing state for the currently focused input
    pub text_cursor: usize,
    pub text_sel_anchor: Option<usize>,
    pub text_scroll_x: f32,

    // Scroll containers (Phase 1.2): persisted scroll offset per scroll-area id,
    // and the grab offset while dragging a scrollbar thumb.
    pub scroll_offsets: Vec<(u32, f32)>,
    pub scroll_drag_grab: f32,

    // Keyboard focus ring (Phase 1.4): focusable ids in draw order (rebuilt each
    // frame), whether the ring is visible (`:focus-visible`), and the OS file-drop
    // state driving the `nui-dropzone` overlay.
    pub focus_order: Vec<u32>,
    pub focus_visible: bool,
    pub hovered_file: Option<std::path::PathBuf>,
    /// When `hovered_file` was latched. The drop overlay expires itself after
    /// [`UiState::DROP_HOVER_GRACE`] when no `DroppedFile` /
    /// `HoveredFileCancelled` arrives — some environments (RDP drag proxying,
    /// elevated drag sources) never deliver the OLE DragLeave, which used to
    /// stick the overlay until restart.
    pub hovered_file_at: Option<std::time::Instant>,
    pub dropped_files: Vec<std::path::PathBuf>,

    // Frame clock: `dt` (seconds) is measured in `begin_frame` and every widget
    // transition advances from it, so the whole UI animates in lockstep and the
    // host can idle the repaint loop as soon as `animating` goes false.
    pub anim_values: Vec<(u32, f32)>,
    pub f32_anims: Vec<(u32, f32, f32, f32)>,
    pub flash_values: Vec<(u32, f32)>,
    pub animating: bool,
    pub dt: f32,
    last_frame_time: Option<std::time::Instant>,

    // Right-click menus: where the last one opened, and the submenu flyout that is
    // currently hovered as `(menu_id, row index)`.
    pub context_menu_pos: (f32, f32),
    pub active_submenu: Option<(u32, usize)>,

    // True while a modal (`dialog`) is drawing its own widgets. The popup rect blocks
    // hover for everything behind the modal, but the modal's own controls must stay live.
    pub inside_popup: bool,

    // Id of the modal that currently owns the popup slot. Without it a dialog cannot
    // tell "dismissed last frame" from "opened this frame" — both look like an empty slot.
    pub modal_id: Option<u32>,

    last_click_time: std::time::Instant,
    last_click_pos: (f32, f32),
}

impl UiState {
    /// Drag hover expires after this long without a drop or cancel: the OLE
    /// DragLeave is not delivered in some environments (RDP drag proxy,
    /// elevated drag source), which used to stick the drop overlay until
    /// restart. Generous enough that only pathological hovers expire.
    const DROP_HOVER_GRACE: std::time::Duration = std::time::Duration::from_secs(20);

    /// Latch/clear the file-drag hover (drives the `nui-dropzone` overlay)
    /// and stamp when it was latched, for the expiry check in
    /// [`UiState::dropzone_overlay`].
    pub fn set_hovered_file(&mut self, file: Option<std::path::PathBuf>) {
        self.hovered_file_at = file.as_ref().map(|_| std::time::Instant::now());
        self.hovered_file = file;
    }

    pub fn new() -> Self {
        Self {
            cursor: (-1000.0, -1000.0),
            mouse_down: false,
            mouse_pressed: false,
            mouse_released: false,
            right_pressed: false,
            double_clicked: false,
            wheel_y: 0.0,
            typed_chars: String::new(),
            key_cmds: Vec::new(),
            hovered_id: None,
            active_id: None,
            focused_id: None,
            open_popup_id: None,
            popup_rect: None,
            text_cursor: 0,
            text_sel_anchor: None,
            text_scroll_x: 0.0,
            scroll_offsets: Vec::new(),
            scroll_drag_grab: 0.0,
            focus_order: Vec::new(),
            focus_visible: false,
            hovered_file: None,
            hovered_file_at: None,
            dropped_files: Vec::new(),
            anim_values: Vec::new(),
            f32_anims: Vec::new(),
            flash_values: Vec::new(),
            animating: false,
            dt: 1.0 / 60.0,
            last_frame_time: None,
            context_menu_pos: (-1000.0, -1000.0),
            active_submenu: None,
            inside_popup: false,
            modal_id: None,
            last_click_time: std::time::Instant::now(),
            last_click_pos: (-1000.0, -1000.0),
        }
    }

    pub fn on_mouse_press(&mut self) {
        // Mouse interaction hides the keyboard-focus ring (`:focus-visible` semantics).
        self.focus_visible = false;
        let now = std::time::Instant::now();
        let dx = self.cursor.0 - self.last_click_pos.0;
        let dy = self.cursor.1 - self.last_click_pos.1;
        if now.duration_since(self.last_click_time).as_millis() < 340 && (dx * dx + dy * dy) < 64.0
        {
            self.double_clicked = true;
        }
        self.last_click_time = now;
        self.last_click_pos = self.cursor;
        self.mouse_down = true;
        self.mouse_pressed = true;
    }

    pub fn on_mouse_release(&mut self) {
        self.mouse_down = false;
        self.mouse_released = true;
    }

    pub fn begin_frame(&mut self) {
        self.hovered_id = None;
        self.focus_order.clear();
        self.inside_popup = false;
        let now = std::time::Instant::now();
        self.dt = match self.last_frame_time {
            Some(prev) => now.duration_since(prev).as_secs_f32().clamp(0.001, 0.05),
            None => 1.0 / 60.0,
        };
        self.last_frame_time = Some(now);
        self.animating = false;
    }

    pub fn end_frame(&mut self) {
        // Keyboard focus traversal: Tab / Shift+Tab advance the ring built this frame.
        let mut dir: i32 = 0;
        for cmd in &self.key_cmds {
            match cmd {
                KeyCmd::Tab => dir = 1,
                KeyCmd::ShiftTab => dir = -1,
                _ => {}
            }
        }
        if dir != 0 && !self.focus_order.is_empty() {
            self.focus_visible = true;
            let n = self.focus_order.len();
            let cur = self
                .focused_id
                .and_then(|id| self.focus_order.iter().position(|&x| x == id));
            let next = match cur {
                Some(i) => (i as i32 + dir).rem_euclid(n as i32) as usize,
                None => {
                    if dir >= 0 {
                        0
                    } else {
                        n - 1
                    }
                }
            };
            self.focused_id = Some(self.focus_order[next]);
        }
        // If a popup is open and the user clicks outside both the popup and its trigger,
        // we close the popup at the end of the frame if nothing claimed it.
        if self.mouse_pressed {
            if let (Some(_), Some(pr)) = (self.open_popup_id, self.popup_rect) {
                if !pr.contains(self.cursor.0, self.cursor.1) && self.active_id != self.open_popup_id
                {
                    self.open_popup_id = None;
                    self.popup_rect = None;
                    self.active_submenu = None;
                }
            }
        }
        if self.mouse_released {
            self.active_id = None;
        }
        self.mouse_pressed = false;
        self.mouse_released = false;
        self.right_pressed = false;
        self.double_clicked = false;
        self.wheel_y = 0.0;
        self.typed_chars.clear();
        self.key_cmds.clear();
    }

    #[inline]
    pub fn can_hover(&self, id: u32, r: Rect) -> bool {
        if !r.contains(self.cursor.0, self.cursor.1) {
            return false;
        }
        // While a modal dialog is open, block all background widgets outside the modal pass.
        if self.modal_id.is_some() && !self.inside_popup {
            return false;
        }
        // If a popup is open, block hover/click on underlying widgets inside the popup's rect
        if let (Some(pid), Some(pr)) = (self.open_popup_id, self.popup_rect) {
            if id != pid && pr.contains(self.cursor.0, self.cursor.1) {
                return false;
            }
        }
        self.active_id.is_none() || self.active_id == Some(id)
    }

    /// Persisted scroll offset for a scroll container (defaults to `0.0`).
    #[inline]
    pub fn scroll_offset(&self, id: u32) -> f32 {
        self.scroll_offsets
            .iter()
            .find(|(k, _)| *k == id)
            .map(|(_, v)| *v)
            .unwrap_or(0.0)
    }

    /// Update (or create) the persisted scroll offset for a scroll container.
    #[inline]
    pub fn set_scroll_offset(&mut self, id: u32, v: f32) {
        if let Some(entry) = self.scroll_offsets.iter_mut().find(|(k, _)| *k == id) {
            entry.1 = v;
        } else {
            self.scroll_offsets.push((id, v));
        }
    }

    /// Add a widget id to this frame's Tab-navigation order (draw order).
    #[inline]
    pub fn register_focusable(&mut self, id: u32) {
        if !self.focus_order.contains(&id) {
            self.focus_order.push(id);
        }
    }

    /// `true` while at least one widget transition started this frame is still in flight.
    #[inline]
    pub fn is_animating(&self) -> bool {
        self.animating
    }

    /// Ease a boolean toward `target` over `duration_s` using a strong `easeInOut` S-curve,
    /// returning the eased `0.0..=1.0` value. Linear progress is stepped by `dt / duration_s`
    /// so both `false -> true` and `true -> false` take the exact `duration_s` with symmetric easing.
    pub fn animate_bool(&mut self, key: u32, target: bool, duration_s: f32) -> f32 {
        let target_f = if target { 1.0 } else { 0.0 };
        let idx = match self.anim_values.iter().position(|(k, _)| *k == key) {
            Some(i) => i,
            None => {
                self.anim_values.push((key, target_f));
                return target_f;
            }
        };
        let step = self.dt / duration_s.max(0.001);
        let cur = &mut self.anim_values[idx].1;
        if target {
            *cur = (*cur + step).min(1.0);
        } else {
            *cur = (*cur - step).max(0.0);
        }
        if (*cur - target_f).abs() > 1e-4 {
            self.animating = true;
        }
        ease_in_out_strong(*cur)
    }

    /// Interpolate a continuous `f32` toward `target` over `duration_s` using `ease_in_out_strong`.
    /// Whenever `target` changes, the transition retargets smoothly from its current eased value.
    pub fn animate_f32(&mut self, key: u32, target: f32, duration_s: f32) -> f32 {
        let idx = match self.f32_anims.iter().position(|(k, _, _, _)| *k == key) {
            Some(i) => i,
            None => {
                self.f32_anims.push((key, target, target, 1.0));
                return target;
            }
        };
        let (_, start, prev_target, t) = self.f32_anims[idx];
        let cur_val = lerp_f32(start, prev_target, ease_in_out_strong(t));
        if (target - prev_target).abs() > 0.5 {
            self.f32_anims[idx] = (key, cur_val, target, 0.0);
        }
        let step = self.dt / duration_s.max(0.001);
        let entry = &mut self.f32_anims[idx];
        entry.3 = (entry.3 + step).min(1.0);
        if entry.3 < 1.0 {
            self.animating = true;
            lerp_f32(entry.1, entry.2, ease_in_out_strong(entry.3))
        } else {
            entry.1 = entry.2;
            entry.2
        }
    }

    /// Snappy `0.10s` press/click flash (`1.0` while held or on the click frame, then 100ms fade-out).
    pub fn flash_alpha(&mut self, id: u32, held: bool, clicked: bool) -> f32 {
        if held || clicked {
            let idx = match self.flash_values.iter().position(|(k, _)| *k == id) {
                Some(i) => i,
                None => {
                    self.flash_values.push((id, 1.0));
                    self.animating = true;
                    return 1.0;
                }
            };
            self.flash_values[idx].1 = 1.0;
            self.animating = true;
            return 1.0;
        }
        let dt = self.dt;
        let idx = match self.flash_values.iter().position(|(k, _)| *k == id) {
            Some(i) => i,
            None => return 0.0,
        };
        if self.flash_values[idx].1 <= 0.0 {
            return 0.0;
        }
        self.flash_values[idx].1 = (self.flash_values[idx].1 - dt / 0.10).max(0.0);
        if self.flash_values[idx].1 > 0.001 {
            self.animating = true;
        }
        ease_out_cubic(self.flash_values[idx].1)
    }
}

pub struct Ui<'a> {
    pub state: &'a mut UiState,
    pub scene: &'a mut Scene,
    pub text_sys: &'a mut TextSystem,
    pub font_engine: FontEngine,
    pub theme: Theme,
    /// Window size in physical pixels. `nui-layout` resolves its column count from
    /// *viewport* media queries (as the web reference does), not from the container
    /// width, so the host publishes the window size here. `(0.0, 0.0)` means unknown.
    pub viewport: (f32, f32),
    /// Active declarative-layout column scopes (Phase 1.1). See `crate::layout`.
    pub layout_stack: Vec<LayoutCursor>,
}

impl<'a> Ui<'a> {
    pub fn new(
        state: &'a mut UiState,
        scene: &'a mut Scene,
        text_sys: &'a mut TextSystem,
        font_engine: FontEngine,
        theme: Theme,
    ) -> Self {
        Self {
            state,
            scene,
            text_sys,
            font_engine,
            theme,
            viewport: (0.0, 0.0),
            layout_stack: Vec::new(),
        }
    }

    /// Draw single-line text clipped to `max_w`, truncating with `"…"` if it exceeds
    /// `max_w`. Returns the drawn advance width (`0.0` when there is nothing to draw).
    #[allow(clippy::too_many_arguments)]
    pub fn text_clipped(
        &mut self,
        face: FontFace,
        engine: FontEngine,
        s: &str,
        x: f32,
        baseline: f32,
        px: f32,
        max_w: f32,
        color: Color,
    ) -> f32 {
        if max_w <= 2.0 || s.is_empty() {
            return 0.0;
        }
        let clip_x = x - 2.0;
        let clip_y = baseline - px * 1.35;
        let clip_w = max_w + 4.0;
        let clip_h = px * 1.85;
        let full_w = self.text_sys.measure(face, engine, s, px);
        if full_w <= max_w {
            self.scene.push_clip(clip_x, clip_y, clip_w, clip_h);
            let w = self
                .scene
                .text(self.text_sys, face, engine, s, x, baseline, px, color);
            self.scene.pop_clip();
            return w;
        }
        // Longest char-boundary prefix that still fits once an ellipsis is appended.
        let mut end = s.len();
        let drawn = loop {
            if end == 0 {
                break "…".to_string();
            }
            let cand = format!("{}…", &s[..end]);
            if self.text_sys.measure(face, engine, &cand, px) <= max_w {
                break cand;
            }
            end = prev_char_boundary(s, end);
        };
        self.scene.push_clip(clip_x, clip_y, clip_w, clip_h);
        let w = self
            .scene
            .text(self.text_sys, face, engine, &drawn, x, baseline, px, color);
        self.scene.pop_clip();
        w
    }

    /// Draw an `nui-input-group` uppercase label above a control, clipped to `max_w`.
    pub fn input_label_clipped(&mut self, x: f32, y: f32, max_w: f32, label: &str) {
        let fs = self.theme.font_xsmall();
        let upper = label.to_uppercase();
        self.text_clipped(
            FontFace::SemiBold,
            self.font_engine,
            &upper,
            x.round(),
            (y + fs * 0.85).round(),
            fs,
            max_w,
            self.theme.label_upper,
        );
    }

    /// Draw an `nui-input-group` uppercase label above a control.
    pub fn input_label(&mut self, x: f32, y: f32, label: &str) {
        self.input_label_clipped(x, y, 32768.0, label);
    }

    /// `nui-tabs` horizontal tab bar with bottom border and 2px highlight active underline.
    pub fn tabs(
        &mut self,
        base_id: u32,
        r: Rect,
        labels: &[(&str, &str)],
        active: &mut usize,
    ) -> bool {
        let mut changed = false;
        let t = self.theme;
        // `nui-tabs` (nui-theme.css line 4445): base-size label, `--nui-space` padding,
        // `--nui-space-quarter` gap between tabs.
        let fs = t.font_base();
        let icon_sz = t.icon_size();
        let pad_x = t.tab_pad_x();
        let gap = t.tab_gap();

        // Bottom border line across full tablist width
        self.scene.rect(r.x, r.y + r.h - 1.0, r.w, 1.0, t.border2);

        let mut tx = r.x;
        let cy = r.y + (r.h - 2.0) * 0.5;
        let baseline = (cy + fs * 0.33).round();
        let mut sel_x = 0.0f32;
        let mut sel_w = 0.0f32;

        self.scene.push_clip(r.x, r.y, r.w, r.h);
        for (i, &(icon, label)) in labels.iter().enumerate() {
            let id = base_id + i as u32;
            let text_w = self
                .text_sys
                .measure(FontFace::SemiBold, self.font_engine, label, fs);
            let icon_w = if icon.is_empty() {
                0.0
            } else {
                icon_sz + t.space_half()
            };
            let tab_w = (text_w + icon_w + pad_x * 2.0).round();
            let tab_r = Rect::new(tx, r.y, tab_w, r.h);

            let hovered = self.state.can_hover(id, tab_r);
            if hovered {
                self.state.hovered_id = Some(id);
                if self.state.mouse_pressed {
                    self.state.active_id = Some(id);
                    if *active != i {
                        *active = i;
                        changed = true;
                    }
                }
            }

            let is_selected = *active == i;
            let sel_t = self
                .state
                .animate_bool(id ^ 0x3400_0000, is_selected, 0.30);
            let hov_t = self
                .state
                .animate_bool(id ^ 0x3300_0000, hovered && !is_selected, 0.15);
            if hov_t > 0.01 {
                self.scene.rounded_rect(
                    tab_r.x,
                    tab_r.y + 2.0,
                    tab_r.w,
                    tab_r.h - 3.0,
                    t.radius1(),
                    lerp_color((t.shade2.0, t.shade2.1, t.shade2.2, 0), t.shade2, hov_t),
                    (0, 0, 0, 0),
                    0.0,
                );
            }

            let fg = lerp_color(
                lerp_color(t.text_dim, t.text, hov_t),
                t.highlight,
                sel_t,
            );

            let mut cx = tab_r.x + pad_x;
            if !icon.is_empty() {
                self.scene.icon_centered(
                    self.text_sys,
                    icon,
                    cx + icon_sz * 0.5,
                    cy,
                    icon_sz,
                    fg,
                );
                cx += icon_w;
            }
            self.scene.text(
                self.text_sys,
                if sel_t > 0.5 {
                    FontFace::SemiBold
                } else {
                    FontFace::Regular
                },
                self.font_engine,
                label,
                cx.round(),
                baseline,
                fs,
                fg,
            );

            if is_selected {
                sel_x = tab_r.x;
                sel_w = tab_r.w;
            }

            tx += tab_w + gap;
        }
        self.scene.pop_clip();

        // One animated 2px indicator that slides/morphs between tabs over 0.30s with strong easeInOut.
        let ind_x = self.state.animate_f32(base_id ^ 0x3100_0000, sel_x, 0.30);
        let ind_w = self.state.animate_f32(base_id ^ 0x3200_0000, sel_w, 0.30);
        if ind_w > 0.5 {
            self.scene
                .rect(ind_x.round(), r.y + r.h - 2.0, ind_w.round(), 2.0, t.highlight);
        }
        changed
    }

    /// `nui-button` (Primary, Outline, Ghost, Danger, Success). Matches the `nui-select`
    /// control treatment: `border-radius2`, drop shadow, `highlight-dim -> highlight`
    /// hover eased over 0.15s (`transition: background-color 0.15s ease`) and a 0.50s
    /// white click flash.
    pub fn button(
        &mut self,
        id: u32,
        r: Rect,
        label: &str,
        icon: &str,
        variant: ButtonVariant,
    ) -> bool {
        let t = self.theme;
        self.state.register_focusable(id);
        let hovered = self.state.can_hover(id, r);
        if hovered {
            self.state.hovered_id = Some(id);
            if self.state.mouse_pressed {
                self.state.active_id = Some(id);
                self.state.focused_id = Some(id);
            }
        }
        let active = self.state.active_id == Some(id);
        let clicked = hovered && self.state.mouse_released && active;
        let rad = t.radius2();
        let hov_t = self.state.animate_bool(id ^ 0x1100_0000, hovered, 0.15);

        let (bg, border, border_w, fg) = match variant {
            ButtonVariant::Primary => (
                lerp_color(t.highlight_dim, t.highlight, hov_t),
                lerp_color(rgba(255, 255, 255, 34), rgba(255, 255, 255, 68), hov_t),
                1.0,
                rgb(255, 255, 255),
            ),
            ButtonVariant::Outline => {
                let rest_bg = if t.dark { t.shade3 } else { t.base };
                let hov_bg = if t.dark { t.shade4 } else { t.shade2 };
                (
                    lerp_color(rest_bg, hov_bg, hov_t),
                    lerp_color(t.shade4, t.shade5, hov_t),
                    1.0,
                    lerp_color(t.text_dim, t.text, hov_t),
                )
            }
            ButtonVariant::Ghost => (
                lerp_color((t.shade3.0, t.shade3.1, t.shade3.2, 0), t.shade3, hov_t),
                (0, 0, 0, 0),
                0.0,
                lerp_color(t.text_dim, t.text, hov_t),
            ),
            ButtonVariant::Danger => (
                lerp_color(t.palette_alert, t.palette_alert_hi, hov_t),
                rgba(255, 255, 255, 38),
                1.0,
                rgb(255, 255, 255),
            ),
            ButtonVariant::Success => (
                lerp_color(t.palette_activate, t.palette_activate_hi, hov_t),
                rgba(255, 255, 255, 38),
                1.0,
                rgb(255, 255, 255),
            ),
        };

        // Soft drop shadow under every filled variant (Ghost is flat by design).
        if variant != ButtonVariant::Ghost {
            self.scene.shadow(
                r.x,
                r.y + 1.0,
                r.w,
                r.h,
                rad,
                3.5,
                rgba(0, 0, 0, if t.dark { 52 } else { 16 }),
            );
        }
        if bg.3 > 0 || border_w > 0.0 {
            self.scene
                .rounded_rect(r.x, r.y, r.w, r.h, rad, bg, border, border_w);
        }

        // Press/click flash: full-strength while held, then a 0.50s fade-out.
        let flash = self
            .state
            .flash_alpha(id, active && self.state.mouse_down, clicked);
        if flash > 0.01 {
            self.scene.rounded_rect(
                r.x,
                r.y,
                r.w,
                r.h,
                rad,
                rgba(255, 255, 255, (52.0 * flash).round() as u8),
                (0, 0, 0, 0),
                0.0,
            );
        }

        let upper = label.to_uppercase();
        let fs = t.font_xsmall();
        let icon_sz = (fs * 1.2).round();
        let gap = (t.rem * 0.375).round();
        let text_w = if upper.is_empty() {
            0.0
        } else {
            self.text_sys
                .measure(FontFace::SemiBold, self.font_engine, &upper, fs)
        };
        let icon_w = if icon.is_empty() {
            0.0
        } else if upper.is_empty() {
            icon_sz
        } else {
            icon_sz + gap
        };
        let total_w = icon_w + text_w;
        let mut cx = r.x + (r.w - total_w) * 0.5;
        let cy = r.y + r.h * 0.5;

        self.scene
            .push_clip(r.x + 2.0, r.y + 1.0, (r.w - 4.0).max(0.0), (r.h - 2.0).max(0.0));
        if !icon.is_empty() {
            self.scene.icon_centered(
                self.text_sys,
                icon,
                cx + icon_sz * 0.5,
                cy,
                icon_sz,
                fg,
            );
            cx += icon_w;
        }
        if !upper.is_empty() {
            self.scene.text(
                self.text_sys,
                FontFace::SemiBold,
                self.font_engine,
                &upper,
                cx.round(),
                (cy + fs * 0.355).round(),
                fs,
                fg,
            );
        }
        self.scene.pop_clip();
        self.draw_focus_ring(id, r, rad);
        clicked
    }

    /// `nui-button-container[variant="segmented"]`: joined buttons with 1px inner dividers,
    /// `base` inactive background, `shade2` hover, and `highlight` active fill.
    pub fn segmented(
        &mut self,
        base_id: u32,
        r: Rect,
        options: &[&str],
        selected: &mut usize,
    ) -> bool {
        let mut changed = false;
        if options.is_empty() {
            return false;
        }
        let t = self.theme;
        let n = options.len();
        let seg_w = (r.w / n as f32).round();
        let fs = t.font_xsmall();
        let cy = r.y + r.h * 0.5;
        let baseline = (cy + fs * 0.355).round();

        // Outer container fill + 1px border (active segment paints over its section of the border)
        let rad = t.radius2();
        self.scene
            .shadow(r.x, r.y + 1.0, r.w, r.h, rad, 3.5, rgba(0, 0, 0, if t.dark { 52 } else { 16 }));
        self.scene
            .rounded_rect(r.x, r.y, r.w, r.h, rad, t.base, t.shade4, 1.0);

        for (i, &opt) in options.iter().enumerate() {
            let id = base_id + i as u32;
            let sx = r.x + i as f32 * seg_w;
            let sw = if i + 1 == n { r.x + r.w - sx } else { seg_w };
            let sr = Rect::new(sx, r.y, sw, r.h);
            let hovered = self.state.can_hover(id, sr);
            if hovered {
                self.state.hovered_id = Some(id);
                if self.state.mouse_pressed {
                    self.state.active_id = Some(id);
                    if *selected != i {
                        *selected = i;
                        changed = true;
                    }
                }
            }

            let is_active = *selected == i;
            let hov_t = self.state.animate_bool(id ^ 0x1300_0000, hovered, 0.15);
            if is_active {
                self.scene.rounded_rect(
                    sr.x,
                    sr.y,
                    sr.w,
                    sr.h,
                    if i == 0 || i + 1 == n { rad } else { 0.0 },
                    t.highlight,
                    (0, 0, 0, 0),
                    0.0,
                );
                // Keep inner seam square when first or last segment is active
                if i == 0 && n > 1 {
                    self.scene
                        .rect(sr.x + sr.w - rad, sr.y, rad, sr.h, t.highlight);
                } else if i + 1 == n && n > 1 {
                    self.scene.rect(sr.x, sr.y, rad, sr.h, t.highlight);
                }
            } else if hov_t > 0.01 {
                self.scene.rect(
                    sr.x + 1.0,
                    sr.y + 1.0,
                    sr.w - 2.0,
                    sr.h - 2.0,
                    lerp_color(t.base, t.shade2, hov_t),
                );
            }

            if i + 1 < n && !is_active && *selected != i + 1 {
                self.scene
                    .rect(sr.x + sr.w - 1.0, sr.y + 1.0, 1.0, sr.h - 2.0, t.shade3);
            }

            let active = self.state.active_id == Some(id);
            let flash = self.state.flash_alpha(
                id,
                active && self.state.mouse_down,
                hovered && self.state.mouse_released && active,
            );
            if flash > 0.01 {
                self.scene.rect(
                    sr.x + 1.0,
                    sr.y + 1.0,
                    sr.w - 2.0,
                    sr.h - 2.0,
                    rgba(255, 255, 255, (56.0 * flash).round() as u8),
                );
            }

            let upper = opt.to_uppercase();
            let tw = self
                .text_sys
                .measure(FontFace::SemiBold, self.font_engine, &upper, fs);
            let fg = if is_active {
                rgb(255, 255, 255)
            } else {
                t.text
            };
            self.scene
                .push_clip(sr.x + 2.0, sr.y, (sr.w - 4.0).max(0.0), sr.h);
            self.text_clipped(
                FontFace::SemiBold,
                self.font_engine,
                &upper,
                (sr.x + (sr.w - tw) * 0.5).round(),
                baseline,
                fs,
                (sr.w - 4.0).max(0.0),
                fg,
            );
            self.scene.pop_clip();
        }
        changed
    }

    /// `nui-checkbox`: 1.25rem box, 2px `shade4` border (`highlight` on hover/checked),
    /// `base` interior (`highlight` when checked + white checkmark).
    pub fn checkbox(&mut self, id: u32, r: Rect, label: &str, checked: &mut bool) -> bool {
        let t = self.theme;
        self.state.register_focusable(id);
        let box_sz = (t.rem * 1.22).round();
        let fs = t.font_small();
        let hovered = self.state.can_hover(id, r);
        let mut changed = false;
        if hovered {
            self.state.hovered_id = Some(id);
            if self.state.mouse_pressed {
                self.state.active_id = Some(id);
            }
            if self.state.mouse_released && self.state.active_id == Some(id) {
                *checked = !*checked;
                changed = true;
            }
        }

        let cy = r.y + r.h * 0.5;
        let by = (cy - box_sz * 0.5).round();
        // Box fill/border colour settles over 0.25s; the tick itself pops in over 0.12s.
        let box_t = self.state.animate_bool(id ^ 0x5100_0000, *checked, 0.25);
        let chk_t = self.state.animate_bool(id ^ 0x5300_0000, *checked, 0.12);
        let hov_t = self.state.animate_bool(id ^ 0x5200_0000, hovered, 0.15);
        let bg = lerp_color(t.base, t.highlight, box_t);
        let border_col = lerp_color(lerp_color(t.shade4, t.highlight, hov_t), t.highlight, box_t);
        self.scene
            .rounded_rect(r.x.round(), by, box_sz, box_sz, 3.0, bg, border_col, 2.0);

        if chk_t > 0.01 {
            let icon_sz = (box_sz * 0.85 * (0.4 + 0.6 * chk_t)).round();
            self.scene.icon_centered(
                self.text_sys,
                ICON_CHECK,
                r.x.round() + box_sz * 0.5,
                by + box_sz * 0.5,
                icon_sz,
                rgba(255, 255, 255, (255.0 * chk_t).round() as u8),
            );
        }

        let ltx = (r.x + box_sz + t.checkbox_gap()).round();
        self.text_clipped(
            FontFace::Regular,
            self.font_engine,
            label,
            ltx,
            (cy + fs * 0.33).round(),
            fs,
            (r.x + r.w - ltx).max(0.0),
            lerp_color(t.text_dim, t.text, hov_t),
        );
        self.draw_focus_ring(id, r, t.radius1());
        changed
    }

    /// `nui-checkbox[variant="switch"]`: 2.25em x 1.25em pill, `shade4` -> `highlight`, white thumb.
    pub fn switch(&mut self, id: u32, r: Rect, label: &str, on: &mut bool) -> bool {
        let t = self.theme;
        self.state.register_focusable(id);
        let sw_h = (t.rem * 1.25).round();
        let sw_w = (sw_h * 1.8).round();
        let fs = t.font_small();
        let hovered = self.state.can_hover(id, r);
        let mut changed = false;
        if hovered {
            self.state.hovered_id = Some(id);
            if self.state.mouse_pressed {
                self.state.active_id = Some(id);
            }
            if self.state.mouse_released && self.state.active_id == Some(id) {
                *on = !*on;
                changed = true;
            }
        }

        let cy = r.y + r.h * 0.5;
        let sy = (cy - sw_h * 0.5).round();
        let on_t = self.state.animate_bool(id ^ 0x4100_0000, *on, 0.30);
        let hov_t = self.state.animate_bool(id ^ 0x4200_0000, hovered, 0.15);
        let track_col = lerp_color(lerp_color(t.shade4, t.shade5, hov_t), t.highlight, on_t);
        self.scene.rounded_rect(
            r.x.round(),
            sy,
            sw_w,
            sw_h,
            sw_h * 0.5,
            track_col,
            (0, 0, 0, 0),
            0.0,
        );

        let pad = (sw_h * 0.16).round().max(2.0);
        let thumb_d = sw_h - pad * 2.0;
        // The thumb elongates at mid-flight (peak stretch when `on_t = 0.5`), which is
        // what gives the toggle its "flipped" feel instead of a plain slide.
        let thumb_w = thumb_d + (1.0 - (2.0 * on_t - 1.0).abs()) * (t.rem * 0.22);
        let thumb_x =
            (r.x.round() + pad + on_t * (sw_w - pad * 2.0 - thumb_d)).round();
        self.scene.shadow(
            thumb_x,
            sy + pad + 1.0,
            thumb_w,
            thumb_d,
            thumb_d * 0.5,
            2.5,
            rgba(0, 0, 0, if t.dark { 55 } else { 22 }),
        );
        self.scene.rounded_rect(
            thumb_x,
            sy + pad,
            thumb_w,
            thumb_d,
            thumb_d * 0.5,
            rgb(255, 255, 255),
            (0, 0, 0, 0),
            0.0,
        );

        let ltx = (r.x + sw_w + t.checkbox_gap()).round();
        self.text_clipped(
            FontFace::Regular,
            self.font_engine,
            label,
            ltx,
            (cy + fs * 0.33).round(),
            fs,
            (r.x + r.w - ltx).max(0.0),
            lerp_color(t.text_dim, t.text, hov_t),
        );
        self.draw_focus_ring(id, r, t.radius1());
        changed
    }

    /// `nui-radio`: 1.25rem circle, 2px `shade4` border (`highlight` when checked/hovered),
    /// `0.5rem` inner `highlight` dot when selected.
    pub fn radio(&mut self, id: u32, r: Rect, label: &str, selected: bool) -> bool {
        let t = self.theme;
        self.state.register_focusable(id);
        // Ensure even integer diameter so center dot aligns cleanly
        let mut d = (t.rem * 1.22).round() as i32;
        if d % 2 != 0 {
            d += 1;
        }
        let d = d as f32;
        let fs = t.font_small();
        let hovered = self.state.can_hover(id, r);
        let mut clicked = false;
        if hovered {
            self.state.hovered_id = Some(id);
            if self.state.mouse_pressed {
                self.state.active_id = Some(id);
            }
            if self.state.mouse_released && self.state.active_id == Some(id) {
                clicked = true;
            }
        }

        let cy = r.y + r.h * 0.5;
        let rx = r.x.round();
        let ry = (cy - d * 0.5).round();
        let sel_t = self.state.animate_bool(id ^ 0x6100_0000, selected, 0.25);
        let hov_t = self.state.animate_bool(id ^ 0x6200_0000, hovered, 0.15);
        let border_col = lerp_color(t.shade4, t.highlight, hov_t.max(sel_t));
        self.scene
            .rounded_rect(rx, ry, d, d, d * 0.5, t.base, border_col, 2.0);

        if sel_t > 0.01 {
            // `transform: scale(0) -> scale(1)` — the dot grows from nothing rather
            // than popping in at a fixed size.
            let dot_d = ((d * 0.44).round() * sel_t).round();
            if dot_d >= 1.0 {
                let off = (d - dot_d) * 0.5;
                self.scene.rounded_rect(
                    rx + off,
                    ry + off,
                    dot_d,
                    dot_d,
                    dot_d * 0.5,
                    t.highlight,
                    (0, 0, 0, 0),
                    0.0,
                );
            }
        }

        let ltx = (rx + d + t.checkbox_gap()).round();
        self.text_clipped(
            FontFace::Regular,
            self.font_engine,
            label,
            ltx,
            (cy + fs * 0.33).round(),
            fs,
            (r.x + r.w - ltx).max(0.0),
            lerp_color(t.text_dim, t.text, hov_t.max(sel_t)),
        );
        self.draw_focus_ring(id, r, t.radius1());
        clicked
    }

    /// `nui-slider`: 0.25rem pill track (`rgba(255,255,255,0.15)` in dark), `highlight` fill,
    /// optional center mark (when `min < 0 && max > 0`), `1rem` circular `highlight` thumb
    /// scaling `1.35x` when active. Supports drag capture, wheel fine-adjust, and double-click/right-click reset.
    pub fn slider(
        &mut self,
        id: u32,
        r: Rect,
        label: &str,
        val: &mut f32,
        min: f32,
        max: f32,
        default_val: f32,
        val_text: &str,
    ) -> bool {
        let t = self.theme;
        self.state.register_focusable(id);
        let fs = t.font_small();
        let label_h = (t.rem * 1.25).round();
        let track_area = Rect::new(r.x, r.y + label_h, r.w, r.h - label_h);
        let hovered = self.state.can_hover(id, r);
        let mut changed = false;

        if hovered {
            self.state.hovered_id = Some(id);
            if self.state.double_clicked || self.state.right_pressed {
                *val = default_val;
                self.state.active_id = None;
                changed = true;
            } else if self.state.mouse_pressed {
                self.state.active_id = Some(id);
                self.state.focused_id = Some(id);
            }
            if self.state.wheel_y.abs() > 0.01 {
                let step = (max - min) * 0.025 * self.state.wheel_y.signum();
                *val = (*val + step).clamp(min, max);
                changed = true;
            }
        }

        let dragging = self.state.active_id == Some(id) && self.state.mouse_down;
        let thumb_base_r = (t.rem * 0.5).round();
        let track_x0 = (r.x + thumb_base_r).round();
        let track_w = (r.w - thumb_base_r * 2.0).round().max(1.0);

        if dragging {
            let norm = ((self.state.cursor.0 - track_x0) / track_w).clamp(0.0, 1.0);
            let new_v = min + norm * (max - min);
            if (new_v - *val).abs() > 1e-5 {
                *val = new_v;
                changed = true;
            }
        }

        // Top row: label on left (clipped so it never collides with the value), value on right.
        let lbl_y = (r.y + fs * 0.88).round();
        let vw = self
            .text_sys
            .measure(FontFace::SemiBold, self.font_engine, val_text, fs);
        self.text_clipped(
            FontFace::Regular,
            self.font_engine,
            label,
            r.x.round(),
            lbl_y,
            fs,
            (r.w - vw - t.rem * 0.5).max(0.0),
            t.text_dim,
        );
        self.scene.text(
            self.text_sys,
            FontFace::SemiBold,
            self.font_engine,
            val_text,
            (r.x + r.w - vw).round(),
            lbl_y,
            fs,
            t.highlight,
        );

        // Track (0.25rem height, pill radius)
        let track_h = (t.rem * 0.25).round().max(4.0);
        let track_cy = (track_area.y + track_area.h * 0.5).round();
        let track_bg = if t.dark {
            rgba(255, 255, 255, 38)
        } else {
            rgba(0, 0, 0, 38)
        };
        self.scene.rounded_rect(
            track_x0,
            track_cy - track_h * 0.5,
            track_w,
            track_h,
            track_h * 0.5,
            track_bg,
            (0, 0, 0, 0),
            0.0,
        );

        let norm = ((*val - min) / (max - min)).clamp(0.0, 1.0);
        let fill_w = track_w * norm;

        // Bipolar center tick mark (like SoundApp pitch/speed sliders)
        if min < 0.0 && max > 0.0 {
            let zero_norm = (0.0 - min) / (max - min);
            let zx = (track_x0 + track_w * zero_norm).round();
            let tick_h = (t.rem * 0.72).round();
            self.scene.rect(
                zx - 1.0,
                track_cy - tick_h * 0.5,
                2.0,
                tick_h,
                t.shade5,
            );
            let f_start = zx.min(track_x0 + fill_w);
            let f_end = zx.max(track_x0 + fill_w);
            if f_end > f_start {
                self.scene.rounded_rect(
                    f_start,
                    track_cy - track_h * 0.5,
                    f_end - f_start,
                    track_h,
                    track_h * 0.5,
                    t.highlight,
                    (0, 0, 0, 0),
                    0.0,
                );
            }
        } else if fill_w > 0.5 {
            self.scene.rounded_rect(
                track_x0,
                track_cy - track_h * 0.5,
                fill_w,
                track_h,
                track_h * 0.5,
                t.highlight,
                (0, 0, 0, 0),
                0.0,
            );
        }

        // Thumb (`1rem` diameter, scales `1.35x` when dragging/active + subtle shadow)
        let thumb_d = if dragging {
            (t.rem * 1.34).round()
        } else if hovered {
            (t.rem * 1.12).round()
        } else {
            (t.rem * 1.0).round()
        };
        let tx = (track_x0 + fill_w - thumb_d * 0.5).round();
        let ty = (track_cy - thumb_d * 0.5).round();
        self.scene.shadow(
            tx,
            ty + 1.0,
            thumb_d,
            thumb_d,
            thumb_d * 0.5,
            3.0,
            rgba(0, 0, 0, if t.dark { 60 } else { 24 }),
        );
        self.scene.rounded_rect(
            tx,
            ty,
            thumb_d,
            thumb_d,
            thumb_d * 0.5,
            t.highlight,
            (0, 0, 0, 0),
            0.0,
        );
        self.draw_focus_ring(id, r, t.radius1());

        changed
    }

    /// `nui-input`: single-line text input with `input_bg` (`rgb(45,45,45)` in dark),
    /// `shade3` border (`shade5` on hover/focus), subtle drop shadow, caret, click-drag selection,
    /// horizontal scroll clip, and trailing clear (`×`) button.
    pub fn text_input(
        &mut self,
        id: u32,
        r: Rect,
        placeholder: &str,
        text: &mut String,
    ) -> bool {
        let t = self.theme;
        let fs = t.font_small();
        // `padding: 0 var(--nui-space)` (nui-theme.css line 3284) — input text starts on the
        // same vertical line as select/button text.
        let pad_x = t.space();
        let clear_btn_sz = (t.rem * 1.4).round();
        let clear_rect = Rect::new(
            (r.x + r.w - clear_btn_sz - t.rem * 0.35).round(),
            (r.y + (r.h - clear_btn_sz) * 0.5).round(),
            clear_btn_sz,
            clear_btn_sz,
        );

        self.state.register_focusable(id);
        let hovered = self.state.can_hover(id, r);
        let clear_hovered = !text.is_empty()
            && clear_rect.contains(self.state.cursor.0, self.state.cursor.1)
            && hovered;
        let mut changed = false;

        if hovered {
            self.state.hovered_id = Some(id);
            if self.state.mouse_pressed {
                if clear_hovered {
                    text.clear();
                    self.state.text_cursor = 0;
                    self.state.text_sel_anchor = None;
                    self.state.text_scroll_x = 0.0;
                    self.state.focused_id = Some(id);
                    changed = true;
                } else {
                    self.state.active_id = Some(id);
                    let was_focused = self.state.focused_id == Some(id);
                    self.state.focused_id = Some(id);
                    if !was_focused {
                        self.state.text_scroll_x = 0.0;
                    }
                    let local_x =
                        self.state.cursor.0 - (r.x + pad_x) + self.state.text_scroll_x;
                    let idx = self.text_sys.hit_test_byte(
                        FontFace::Regular,
                        self.font_engine,
                        text,
                        local_x,
                        fs,
                    );
                    if self.state.double_clicked {
                        self.state.text_sel_anchor = Some(0);
                        self.state.text_cursor = text.len();
                    } else {
                        self.state.text_cursor = idx;
                        self.state.text_sel_anchor = Some(idx);
                    }
                }
            }
        }

        if self.state.active_id == Some(id) && self.state.mouse_down && !clear_hovered {
            let local_x = self.state.cursor.0 - (r.x + pad_x) + self.state.text_scroll_x;
            let idx = self.text_sys.hit_test_byte(
                FontFace::Regular,
                self.font_engine,
                text,
                local_x,
                fs,
            );
            self.state.text_cursor = idx;
        }

        let focused = self.state.focused_id == Some(id);
        if focused {
            self.state.text_cursor = self.state.text_cursor.min(text.len());
            if let Some(a) = self.state.text_sel_anchor {
                if a > text.len() {
                    self.state.text_sel_anchor = Some(text.len());
                }
            }

            let cmds = self.state.key_cmds.clone();
            for cmd in cmds {
                match cmd {
                    KeyCmd::SelectAll => {
                        self.state.text_sel_anchor = Some(0);
                        self.state.text_cursor = text.len();
                    }
                    KeyCmd::Left => {
                        if let Some(sel) = self.selection_range() {
                            self.state.text_cursor = sel.0;
                            self.state.text_sel_anchor = None;
                        } else if self.state.text_cursor > 0 {
                            self.state.text_cursor =
                                prev_char_boundary(text, self.state.text_cursor);
                        }
                    }
                    KeyCmd::Right => {
                        if let Some(sel) = self.selection_range() {
                            self.state.text_cursor = sel.1;
                            self.state.text_sel_anchor = None;
                        } else if self.state.text_cursor < text.len() {
                            self.state.text_cursor =
                                next_char_boundary(text, self.state.text_cursor);
                        }
                    }
                    KeyCmd::Home => {
                        self.state.text_cursor = 0;
                        self.state.text_sel_anchor = None;
                    }
                    KeyCmd::End => {
                        self.state.text_cursor = text.len();
                        self.state.text_sel_anchor = None;
                    }
                    KeyCmd::Backspace => {
                        if let Some((s0, s1)) = self.selection_range() {
                            text.replace_range(s0..s1, "");
                            self.state.text_cursor = s0;
                            self.state.text_sel_anchor = None;
                            changed = true;
                        } else if self.state.text_cursor > 0 {
                            let prev = prev_char_boundary(text, self.state.text_cursor);
                            text.replace_range(prev..self.state.text_cursor, "");
                            self.state.text_cursor = prev;
                            changed = true;
                        }
                    }
                    KeyCmd::Delete => {
                        if let Some((s0, s1)) = self.selection_range() {
                            text.replace_range(s0..s1, "");
                            self.state.text_cursor = s0;
                            self.state.text_sel_anchor = None;
                            changed = true;
                        } else if self.state.text_cursor < text.len() {
                            let next = next_char_boundary(text, self.state.text_cursor);
                            text.replace_range(self.state.text_cursor..next, "");
                            changed = true;
                        }
                    }
                    KeyCmd::Copy => {
                        if let Some((s0, s1)) = self.selection_range() {
                            clipboard_set_text(&text[s0..s1]);
                        }
                    }
                    KeyCmd::Cut => {
                        if let Some((s0, s1)) = self.selection_range() {
                            clipboard_set_text(&text[s0..s1]);
                            text.replace_range(s0..s1, "");
                            self.state.text_cursor = s0;
                            self.state.text_sel_anchor = None;
                            changed = true;
                        }
                    }
                    KeyCmd::Paste => {
                        if let Some(mut clip) = clipboard_get_text() {
                            // Single-line input: strip line breaks from pasted text.
                            clip.retain(|c| c != '\r' && c != '\n');
                            if !clip.is_empty() {
                                if let Some((s0, s1)) = self.selection_range() {
                                    text.replace_range(s0..s1, "");
                                    self.state.text_cursor = s0;
                                    self.state.text_sel_anchor = None;
                                }
                                text.insert_str(self.state.text_cursor, &clip);
                                self.state.text_cursor += clip.len();
                                changed = true;
                            }
                        }
                    }
                    KeyCmd::Escape | KeyCmd::Enter => {
                        self.state.focused_id = None;
                        self.state.text_sel_anchor = None;
                    }
                    _ => {}
                }
            }

            if !self.state.typed_chars.is_empty() {
                let ins = self.state.typed_chars.clone();
                if let Some((s0, s1)) = self.selection_range() {
                    text.replace_range(s0..s1, "");
                    self.state.text_cursor = s0;
                    self.state.text_sel_anchor = None;
                }
                text.insert_str(self.state.text_cursor, &ins);
                self.state.text_cursor += ins.len();
                changed = true;
            }
        }

        // Subtle drop shadow + box surface & border (`nui-input` in nui-theme.css)
        self.scene.shadow(
            r.x,
            r.y + 1.0,
            r.w,
            r.h,
            t.radius1(),
            if focused { 4.5 } else { 3.0 },
            rgba(0, 0, 0, if t.dark { 42 } else { 14 }),
        );
        let bg = if focused && t.dark {
            t.shade3
        } else {
            t.input_bg
        };
        let border_col = if focused || hovered {
            t.shade5
        } else {
            t.shade3
        };
        self.scene
            .rounded_rect(r.x, r.y, r.w, r.h, t.radius1(), bg, border_col, 1.0);

        let inner_w = (r.w - pad_x - clear_btn_sz - t.rem * 0.6).max(20.0);
        let text_x = (r.x + pad_x).round();
        let cy = r.y + r.h * 0.5;
        let baseline = (cy + fs * 0.33).round();

        if focused {
            let caret_off = self.text_sys.byte_x_offset(
                FontFace::Regular,
                self.font_engine,
                text,
                self.state.text_cursor,
                fs,
            );
            if caret_off - self.state.text_scroll_x > inner_w - 4.0 {
                self.state.text_scroll_x = caret_off - (inner_w - 4.0);
            } else if caret_off - self.state.text_scroll_x < 0.0 {
                self.state.text_scroll_x = caret_off;
            }
            let total_w =
                self.text_sys
                    .measure(FontFace::Regular, self.font_engine, text, fs);
            if total_w <= inner_w {
                self.state.text_scroll_x = 0.0;
            }
        }
        let scroll_x = if focused { self.state.text_scroll_x } else { 0.0 };

        self.scene.push_clip(text_x, r.y + 2.0, inner_w, r.h - 4.0);

        if focused {
            if let Some((s0, s1)) = self.selection_range() {
                let sx0 = self.text_sys.byte_x_offset(
                    FontFace::Regular,
                    self.font_engine,
                    text,
                    s0,
                    fs,
                );
                let sx1 = self.text_sys.byte_x_offset(
                    FontFace::Regular,
                    self.font_engine,
                    text,
                    s1,
                    fs,
                );
                let sel_h = (fs * 1.3).round();
                let sel_y = (cy - sel_h * 0.5).round();
                self.scene.rect(
                    text_x - scroll_x + sx0,
                    sel_y,
                    (sx1 - sx0).max(2.0),
                    sel_h,
                    rgba(t.highlight.0, t.highlight.1, t.highlight.2, 95),
                );
            }
        }

        if text.is_empty() {
            self.scene.text(
                self.text_sys,
                FontFace::Regular,
                self.font_engine,
                placeholder,
                text_x,
                baseline,
                fs,
                t.shade5,
            );
        } else {
            self.scene.text(
                self.text_sys,
                FontFace::Regular,
                self.font_engine,
                text,
                (text_x - scroll_x).round(),
                baseline,
                fs,
                t.text,
            );
        }

        if focused {
            let cx = self.text_sys.byte_x_offset(
                FontFace::Regular,
                self.font_engine,
                text,
                self.state.text_cursor,
                fs,
            );
            let caret_h = (fs * 1.25).round();
            let caret_y = (cy - caret_h * 0.5).round();
            self.scene.rect(
                (text_x - scroll_x + cx).round(),
                caret_y,
                1.5,
                caret_h,
                t.highlight,
            );
        }

        self.scene.pop_clip();

        // Clear (`×`) button when non-empty
        if !text.is_empty() {
            if clear_hovered {
                self.scene.rounded_rect(
                    clear_rect.x,
                    clear_rect.y,
                    clear_rect.w,
                    clear_rect.h,
                    t.radius1(),
                    t.shade2,
                    (0, 0, 0, 0),
                    0.0,
                );
            }
            let c_icon_sz = (t.rem * 0.98).round();
            self.scene.icon_centered(
                self.text_sys,
                ICON_CLOSE,
                clear_rect.x + clear_rect.w * 0.5,
                clear_rect.y + clear_rect.h * 0.5,
                c_icon_sz,
                if clear_hovered { t.text_dim } else { t.shade5 },
            );
        }

        self.draw_focus_ring(id, r, t.radius1());
        changed
    }

    fn selection_range(&self) -> Option<(usize, usize)> {
        let anchor = self.state.text_sel_anchor?;
        let cur = self.state.text_cursor;
        if anchor == cur {
            None
        } else if anchor < cur {
            Some((anchor, cur))
        } else {
            Some((cur, anchor))
        }
    }

    /// `nui-select`: `shade3` control with `shade4` border, `border-radius2`, right chevron,
    /// and a top-layer overlay popup (`shade2` background, checkmark on active option).
    pub fn select(
        &mut self,
        id: u32,
        r: Rect,
        options: &[&str],
        selected: &mut usize,
    ) -> bool {
        let t = self.theme;
        let fs = t.font_small();
        // `padding: 0 var(--nui-space)` (nui-theme.css line 3425) — same text origin as nui-input.
        let pad_x = t.space();
        self.state.register_focusable(id);
        let is_open = self.state.open_popup_id == Some(id);
        let hovered = self.state.can_hover(id, r);
        let mut changed = false;

        if hovered {
            self.state.hovered_id = Some(id);
            if self.state.mouse_pressed {
                self.state.active_id = Some(id);
                self.state.focused_id = Some(id);
                self.state.mouse_pressed = false;
                if is_open {
                    self.state.open_popup_id = None;
                    self.state.popup_rect = None;
                } else {
                    self.state.open_popup_id = Some(id);
                }
            }
        }

        let is_open = self.state.open_popup_id == Some(id);
        // One call, one advance: `animate_bool` steps its value by `dt`, so asking twice
        // in a frame would move the transition at double speed.
        let hov_t = self
            .state
            .animate_bool(id ^ 0x1200_0000, hovered || is_open, 0.15);
        let open_t = self.state.animate_bool(id ^ 0x2200_0000, is_open, 0.30);
        // `nui-select.is-open .nui-select-arrow { transform: rotate(180deg); }`
        let chev_t = self.state.animate_bool(id ^ 0x2300_0000, is_open, 0.30);
        let rest_bg = if t.dark { t.shade3 } else { t.base };
        let hov_bg = if t.dark { t.shade4 } else { t.shade2 };
        let bg = lerp_color(rest_bg, hov_bg, hov_t);
        let border_col = lerp_color(t.shade4, t.shade5, hov_t);

        let rad = t.radius2();
        self.scene.shadow(
            r.x,
            r.y + 1.0,
            r.w,
            r.h,
            rad,
            3.5,
            rgba(0, 0, 0, if t.dark { 52 } else { 16 }),
        );
        if is_open {
            // `border-radius: r r 0 0` via clip
            self.scene.push_clip(r.x, r.y, r.w, r.h);
            self.scene
                .rounded_rect(r.x, r.y, r.w, r.h + rad, rad, bg, border_col, 1.0);
            self.scene.pop_clip();
        } else {
            self.scene
                .rounded_rect(r.x, r.y, r.w, r.h, rad, bg, border_col, 1.0);
        }

        let val_str = options.get(*selected).copied().unwrap_or("-");
        let arrow_sz = (t.rem * 1.2).round();
        let cy = r.y + r.h * 0.5;
        let baseline = (cy + fs * 0.33).round();

        self.scene
            .push_clip(r.x + pad_x, r.y, r.w - pad_x * 2.0 - arrow_sz, r.h);
        self.scene.text(
            self.text_sys,
            FontFace::Regular,
            self.font_engine,
            val_str,
            (r.x + pad_x).round(),
            baseline,
            fs,
            t.text,
        );
        self.scene.pop_clip();

        self.draw_chevron_rotated(
            r.x + r.w - pad_x - arrow_sz * 0.35,
            cy,
            (t.rem * 0.28).round().max(3.5),
            1.8,
            chev_t * std::f32::consts::PI,
            t.shade6,
        );

        // Render top-layer popup in Scene's overlay pass so it paints above all page widgets
        if is_open || open_t > 0.01 {
            // Option rows: `padding: var(--nui-space-half) var(--nui-space)` -> 2.25rem.
            let row_h = (t.rem * 2.25).round();
            let pop_h = row_h * options.len() as f32 + 2.0;
            let pop_r = Rect::new(r.x, r.y + r.h, r.w, pop_h);
            if is_open {
                self.state.popup_rect = Some(pop_r);
            }
            let view_r = Rect::new(
                pop_r.x,
                pop_r.y + (1.0 - open_t) * -3.0,
                pop_r.w,
                (pop_h * (0.35 + 0.65 * open_t)).round(),
            );

            self.scene.begin_overlay();

            // Rich soft SDF drop shadow + popup surface (`nui-select-popup` with `border-radius: 0 0 r r`)
            self.scene.shadow(
                view_r.x,
                view_r.y + 3.0,
                view_r.w,
                view_r.h,
                rad,
                8.0,
                scale_alpha(rgba(0, 0, 0, if t.dark { 85 } else { 26 }), open_t),
            );
            let pop_bg = scale_alpha(if t.dark { t.shade2 } else { t.base }, open_t);
            self.scene
                .push_clip(view_r.x, view_r.y, view_r.w, view_r.h);
            self.scene.rounded_rect(
                view_r.x,
                view_r.y - rad,
                view_r.w,
                view_r.h + rad,
                rad,
                pop_bg,
                scale_alpha(border_col, open_t),
                1.0,
            );
            self.scene.pop_clip();
            // 1px separator line between trigger and popup
            self.scene.rect(
                view_r.x + 1.0,
                view_r.y,
                view_r.w - 2.0,
                1.0,
                scale_alpha(t.shade3, open_t),
            );

            self.scene.push_clip(
                view_r.x + 1.0,
                view_r.y + 1.0,
                view_r.w - 2.0,
                (view_r.h - 2.0).max(1.0),
            );
            for (i, &opt) in options.iter().enumerate() {
                let opt_r = Rect::new(
                    view_r.x + 1.0,
                    view_r.y + 1.0 + i as f32 * row_h,
                    view_r.w - 2.0,
                    row_h,
                );
                let opt_hov = is_open && opt_r.contains(self.state.cursor.0, self.state.cursor.1);
                let is_sel = *selected == i;

                if opt_hov {
                    self.state.hovered_id = Some(id);
                    if self.state.mouse_pressed {
                        *selected = i;
                        self.state.active_id = Some(id);
                        self.state.mouse_pressed = false;
                        self.state.open_popup_id = None;
                        self.state.popup_rect = None;
                        changed = true;
                    }
                }

                if opt_hov {
                    self.scene
                        .rect(opt_r.x, opt_r.y, opt_r.w, opt_r.h, scale_alpha(t.shade4, open_t));
                } else if is_sel {
                    let sel_bg = if t.dark {
                        rgb(50, 50, 50)
                    } else {
                        t.highlight_dim
                    };
                    self.scene
                        .rect(opt_r.x, opt_r.y, opt_r.w, opt_r.h, scale_alpha(sel_bg, open_t));
                }

                let fg = scale_alpha(
                    if is_sel {
                        rgb(255, 255, 255)
                    } else if opt_hov {
                        t.text
                    } else {
                        t.text_dim
                    },
                    open_t,
                );

                let o_cy = opt_r.y + opt_r.h * 0.5;
                let o_base = (o_cy + fs * 0.33).round();

                self.text_clipped(
                    if is_sel {
                        FontFace::SemiBold
                    } else {
                        FontFace::Regular
                    },
                    self.font_engine,
                    opt,
                    (view_r.x + pad_x).round(),
                    o_base,
                    fs,
                    (view_r.w - pad_x * 2.0 - arrow_sz).max(0.0),
                    fg,
                );

                if is_sel {
                    let csz = (t.rem * 1.05).round();
                    self.scene.icon_centered(
                        self.text_sys,
                        ICON_CHECK,
                        view_r.x + view_r.w - pad_x - arrow_sz * 0.35,
                        o_cy,
                        csz,
                        fg,
                    );
                }
            }
            self.scene.pop_clip();
            self.scene.end_overlay();
        }

        self.draw_focus_ring(id, r, t.radius2());
        changed
    }

    /// `nui-badge`: `base` bg, `border-shade3` 1px border, 6px left semantic stripe, uppercase bold label,
    /// plus optional status dot (`Some(true)` = online green, `Some(false)` = warning).
    pub fn badge(
        &mut self,
        x: f32,
        y: f32,
        label: &str,
        variant: BadgeVariant,
        status_dot: Option<bool>,
    ) -> f32 {
        let t = self.theme;
        let fs = (t.rem * 0.72).round();
        let h = (t.rem * 1.48).round();
        let stripe_w = (t.rem * 0.36).round().max(5.0);
        let pad_x = (t.rem * 0.55).round();
        let mut dot_d = (t.rem * 0.46).round() as i32;
        if dot_d % 2 != 0 {
            dot_d += 1;
        }
        let dot_d = dot_d as f32;
        let dot_extra = if status_dot.is_some() {
            dot_d + (t.rem * 0.38).round()
        } else {
            0.0
        };

        let upper = label.to_uppercase();
        let tw = self
            .text_sys
            .measure(FontFace::SemiBold, self.font_engine, &upper, fs);
        let w = (stripe_w + pad_x * 2.0 + dot_extra + tw).round();
        let x = x.round();
        let y = y.round();

        let stripe_col = match variant {
            BadgeVariant::Primary => t.highlight,
            BadgeVariant::Success => t.palette_activate,
            BadgeVariant::Warning => t.palette_mark,
            BadgeVariant::Danger => t.palette_alert,
        };

        self.scene
            .rounded_rect(x, y, w, h, t.radius1(), t.base, (0, 0, 0, 0), 0.0);
        self.scene
            .push_clip(x, y, stripe_w, h);
        self.scene
            .rounded_rect(x, y, w, h, t.radius1(), stripe_col, (0, 0, 0, 0), 0.0);
        self.scene.pop_clip();
        self.scene
            .rounded_rect(x, y, w, h, t.radius1(), (0, 0, 0, 0), t.border3, 1.0);

        let cy = y + h * 0.5;
        let mut tx = x + stripe_w + pad_x;
        if let Some(online) = status_dot {
            let dcol = if online {
                t.palette_activate
            } else {
                t.palette_mark
            };
            self.scene.rounded_rect(
                tx.round(),
                (cy - dot_d * 0.5).round(),
                dot_d,
                dot_d,
                dot_d * 0.5,
                dcol,
                (0, 0, 0, 0),
                0.0,
            );
            tx += dot_extra;
        }

        self.scene.text(
            self.text_sys,
            FontFace::SemiBold,
            self.font_engine,
            &upper,
            tx.round(),
            (cy + fs * 0.355).round(),
            fs,
            t.text,
        );
        w
    }

    // ==================================================================
    // Phase 1.2 — Scrollable & Virtualized List
    // ==================================================================

    /// A clipped, mouse-wheel-scrollable viewport with a draggable `--space-scrollbar-width`
    /// (`0.5rem`) thumb. `f` receives the viewport rect and the current scroll offset and must
    /// lay its content out in an unclipped coordinate space (it is clipped for you).
    pub fn scroll_area<R>(
        &mut self,
        id: u32,
        r: Rect,
        content_h: f32,
        draw_bg: bool,
        f: impl FnOnce(&mut Self, Rect, f32) -> R,
    ) -> R {
        let t = self.theme;
        if draw_bg {
            self.scene
                .rounded_rect(r.x, r.y, r.w, r.h, t.radius2(), t.base, t.border2, 1.0);
        }

        let inner_x = r.x + 1.0;
        let inner_y = r.y + 1.0;
        let inner_h = (r.h - 2.0).max(1.0);
        let max_scroll = (content_h - inner_h).max(0.0);
        let has_sb = max_scroll > 0.5;
        let sb_w = (t.rem * 0.5).round().max(6.0);
        let sb_pad = 3.0;
        let viewport_w = if has_sb {
            (r.w - 2.0 - sb_w - sb_pad * 2.0).max(10.0)
        } else {
            (r.w - 2.0).max(10.0)
        };

        let mut scroll_y = self.state.scroll_offset(id);

        // Mouse-wheel scrolling (consume the wheel so it does not double-apply).
        if self.state.can_hover(id, r) && self.state.wheel_y.abs() > 0.01 {
            scroll_y = (scroll_y - self.state.wheel_y * (t.rem * 2.6)).clamp(0.0, max_scroll);
            self.state.wheel_y = 0.0;
        }

        let sb_id = id.wrapping_add(0x4000_0000);
        let track_rect = Rect::new(
            r.x + r.w - 1.0 - sb_w - sb_pad,
            inner_y + sb_pad,
            sb_w,
            (inner_h - sb_pad * 2.0).max(10.0),
        );
        let thumb_h = if has_sb {
            ((inner_h / content_h.max(1.0)) * track_rect.h)
                .round()
                .clamp((t.rem * 1.4).round(), track_rect.h)
        } else {
            track_rect.h
        };
        let thumb_travel = (track_rect.h - thumb_h).max(1.0);

        if has_sb {
            let thumb_y = (track_rect.y + (scroll_y / max_scroll) * thumb_travel).round();
            let thumb_rect = Rect::new(track_rect.x, thumb_y, sb_w, thumb_h);

            let over_thumb = self.state.can_hover(sb_id, thumb_rect);
            let over_track = self.state.can_hover(sb_id, track_rect);

            if over_thumb && self.state.mouse_pressed {
                self.state.active_id = Some(sb_id);
                self.state.scroll_drag_grab = self.state.cursor.1 - thumb_rect.y;
            }

            if self.state.active_id == Some(sb_id) {
                if self.state.mouse_down {
                    let target = self.state.cursor.1 - self.state.scroll_drag_grab - track_rect.y;
                    scroll_y = ((target / thumb_travel) * max_scroll).clamp(0.0, max_scroll);
                }
            } else if over_track && self.state.mouse_pressed {
                // Click-to-jump: center the thumb under the cursor.
                let target = self.state.cursor.1 - track_rect.y - thumb_h * 0.5;
                scroll_y = ((target / thumb_travel) * max_scroll).clamp(0.0, max_scroll);
            }
        }

        self.state.set_scroll_offset(id, scroll_y);

        // Content (clipped to the viewport, excluding the scrollbar gutter).
        self.scene.push_clip(inner_x, inner_y, viewport_w, inner_h);
        let out = f(
            self,
            Rect::new(inner_x, inner_y, viewport_w, inner_h),
            scroll_y,
        );
        self.scene.pop_clip();

        // Scrollbar drawn on top of the content, outside the clip.
        if has_sb {
            let thumb_y = (track_rect.y + (scroll_y / max_scroll) * thumb_travel).round();
            let thumb_rect = Rect::new(track_rect.x, thumb_y, sb_w, thumb_h);
            let dragging = self.state.active_id == Some(sb_id);
            let hovered = dragging || self.state.can_hover(sb_id, thumb_rect);

            self.scene.rounded_rect(
                track_rect.x,
                track_rect.y,
                track_rect.w,
                track_rect.h,
                sb_w * 0.5,
                t.shade1,
                (0, 0, 0, 0),
                0.0,
            );
            let col = if dragging {
                t.highlight
            } else if hovered {
                t.shade5
            } else {
                t.shade4
            };
            self.scene.rounded_rect(
                thumb_rect.x,
                thumb_rect.y,
                thumb_rect.w,
                thumb_rect.h,
                sb_w * 0.5,
                col,
                (0, 0, 0, 0),
                0.0,
            );
        }

        out
    }

    /// A virtualized vertical list: only the rows intersecting the viewport are emitted.
    /// Suitable for a 10,000-track playlist at negligible draw cost.
    pub fn virtual_list(
        &mut self,
        id: u32,
        r: Rect,
        row_count: usize,
        row_h: f32,
        mut row_fn: impl FnMut(&mut Self, usize, Rect),
    ) {
        let content_h = row_count as f32 * row_h;
        self.scroll_area(id, r, content_h, true, |ui, vp, scroll_y| {
            let first = (scroll_y / row_h).floor().max(0.0) as usize;
            let last = (((scroll_y + vp.h) / row_h).ceil() as usize + 1).min(row_count);
            for idx in first..last {
                let ry = (vp.y + idx as f32 * row_h - scroll_y).round();
                row_fn(ui, idx, Rect::new(vp.x, ry, vp.w, row_h));
            }
        });
    }

    /// One interactive row for [`Ui::virtual_list`]. Returns `true` on click.
    pub fn list_row(
        &mut self,
        id: u32,
        r: Rect,
        selected: bool,
        index_str: &str,
        title: &str,
        right_meta: &str,
    ) -> bool {
        let t = self.theme;
        let hovered = self.state.can_hover(id, r);
        if hovered {
            self.state.hovered_id = Some(id);
            if self.state.mouse_pressed {
                self.state.active_id = Some(id);
            }
        }
        let active = self.state.active_id == Some(id);
        let clicked = hovered && self.state.mouse_released && active;

        if selected {
            self.scene.rect(r.x, r.y, r.w, r.h, t.shade3);
            self.scene.rect(r.x, r.y, 3.0, r.h, t.highlight);
        } else if hovered {
            self.scene.rect(r.x, r.y, r.w, r.h, t.shade2);
        }
        // Bottom hairline divider.
        self.scene
            .rect(r.x, r.y + r.h - 1.0, r.w, 1.0, t.border1);

        let fs = t.font_small();
        let baseline = (r.y + r.h * 0.5 + fs * 0.355).round();

        let title_x = r.x + t.rem * 3.4;
        let meta_w = if right_meta.is_empty() {
            0.0
        } else {
            self.text_sys
                .measure(FontFace::Regular, self.font_engine, right_meta, fs)
                + t.rem * 0.6
        };
        let meta_x = r.x + r.w - meta_w;

        self.text_clipped(
            FontFace::Regular,
            self.font_engine,
            index_str,
            (r.x + t.rem * 0.6).round(),
            baseline,
            fs,
            (title_x - (r.x + t.rem * 0.6) - t.rem * 0.4).max(0.0),
            t.text_muted,
        );

        let face = if selected {
            FontFace::SemiBold
        } else {
            FontFace::Regular
        };
        self.text_clipped(
            face,
            self.font_engine,
            title,
            title_x.round(),
            baseline,
            fs,
            (meta_x - title_x - t.rem * 0.4).max(0.0),
            t.text,
        );

        if !right_meta.is_empty() {
            self.text_clipped(
                FontFace::Regular,
                self.font_engine,
                right_meta,
                meta_x.round(),
                baseline,
                fs,
                meta_w.max(0.0),
                t.text_dim,
            );
        }

        clicked
    }

    /// Draw the `:focus-visible` ring (2px `highlight` outline, 2px outside `r`)
    /// when `id` is the currently keyboard-focused widget and the ring is visible.
    pub fn draw_focus_ring(&mut self, id: u32, r: Rect, radius: f32) {
        if self.state.focused_id == Some(id) && self.state.focus_visible {
            let hl = self.theme.highlight;
            self.scene.rounded_rect(
                r.x - 3.0,
                r.y - 3.0,
                r.w + 6.0,
                r.h + 6.0,
                radius + 2.0,
                (0, 0, 0, 0),
                hl,
                2.0,
            );
        }
    }

    /// `nui-graph` vector curve: connects normalized `(nx, ny)` points
    /// (`0.0..=1.0`, `ny = 0.0` at the bottom) with `kind = 4` anti-aliased
    /// capsule segments, clipped to `r`.
    pub fn polyline(&mut self, r: Rect, points: &[(f32, f32)], thickness: f32, color: Color) {
        if points.len() < 2 {
            return;
        }
        self.scene.push_clip(r.x, r.y, r.w, r.h);
        let to_screen = |nx: f32, ny: f32| -> (f32, f32) {
            (r.x + nx * r.w, r.y + r.h - ny * r.h)
        };
        let (mut px, mut py) = to_screen(points[0].0, points[0].1);
        for &(nx, ny) in &points[1..] {
            let (x, y) = to_screen(nx, ny);
            self.scene.line(px, py, x, y, thickness, color);
            px = x;
            py = y;
        }
        self.scene.pop_clip();
    }

    /// Draw an anti-aliased vector chevron centred at `(cx, cy)` with `angle_rad`
    /// (`0.0` = pointing down `⌄`, `PI` = pointing up `^`, `-PI/2` = pointing right `›`).
    /// Uses `Scene::line` (`kind = 4` capsule SDF) so rotation animations (`nui-select`
    /// 0->180deg, `nui-link-list` group caret -90->0deg) are subpixel-smooth at any angle.
    pub fn draw_chevron_rotated(
        &mut self,
        cx: f32,
        cy: f32,
        arm: f32,
        thickness: f32,
        angle_rad: f32,
        color: Color,
    ) {
        // Base chevron at angle 0 points DOWN: (-arm, -arm*0.45) -> (0, arm*0.45) -> (arm, -arm*0.45)
        let (s, c) = angle_rad.sin_cos();
        let rot = |lx: f32, ly: f32| -> (f32, f32) {
            (cx + lx * c - ly * s, cy + lx * s + ly * c)
        };
        let p0 = rot(-arm, -arm * 0.45);
        let p1 = rot(0.0, arm * 0.45);
        let p2 = rot(arm, -arm * 0.45);
        self.scene.line(p0.0, p0.1, p1.0, p1.1, thickness, color);
        self.scene.line(p1.0, p1.1, p2.0, p2.1, thickness, color);
    }

    /// Interactive waveform seekbar: 96+ vertical peak bars between a centre
    /// zero-line, a crisp `2px` playhead, hover highlight, and click/drag
    /// scrubbing that writes `*playhead` (0.0..=1.0). Returns `true` when scrubbed.
    pub fn waveform(&mut self, id: u32, r: Rect, peaks: &[f32], playhead: &mut f32) -> bool {
        let t = self.theme;
        self.state.register_focusable(id);

        let hovered = self.state.can_hover(id, r);
        let mut scrubbed = false;
        if hovered {
            self.state.hovered_id = Some(id);
            if self.state.mouse_pressed {
                self.state.active_id = Some(id);
                self.state.focused_id = Some(id);
            }
        }
        if self.state.active_id == Some(id) && self.state.mouse_down && r.w > 0.0 {
            *playhead = ((self.state.cursor.0 - r.x) / r.w).clamp(0.0, 1.0);
            scrubbed = true;
        }

        self.scene
            .rounded_rect(r.x, r.y, r.w, r.h, t.radius1(), t.base, t.border2, 1.0);
        self.scene
            .push_clip(r.x + 1.0, r.y + 1.0, (r.w - 2.0).max(1.0), (r.h - 2.0).max(1.0));

        let cy = (r.y + r.h * 0.5).round();
        self.scene.rect(r.x + 2.0, cy, (r.w - 4.0).max(1.0), 1.0, t.border1);

        let n = peaks.len();
        if n > 0 && r.w > 6.0 {
            let half = ((r.h - 4.0) * 0.5).max(1.0);
            let step = ((r.w - 4.0) / n as f32).max(1.0);
            let bw = (step * 0.6).clamp(1.0, 3.0);
            for (i, &p) in peaks.iter().enumerate() {
                let bar_frac = (i as f32 + 0.5) / n as f32;
                let amp = p.abs().clamp(0.0, 1.0);
                let bh = (amp * half).max(1.0);
                let bx = (r.x + 2.0 + i as f32 * step).round();
                let col = if bar_frac <= *playhead {
                    t.highlight
                } else if hovered {
                    t.shade5
                } else {
                    t.shade4
                };
                self.scene
                    .rect(bx, (cy - bh).round(), bw, (bh * 2.0).round(), col);
            }
        }

        self.scene.pop_clip();

        // Crisp white playhead.
        let px = (r.x + r.w * (*playhead)).round();
        self.scene
            .rect(px - 1.0, r.y + 1.0, 2.0, (r.h - 2.0).max(1.0), rgb(255, 255, 255));

        self.draw_focus_ring(id, r, t.radius1());
        scrubbed
    }

    /// `nui-dropzone` full-window overlay: when `state.hovered_file` is set, dims
    /// the whole window and draws a dashed drop target with the file path. Returns
    /// the oldest queued dropped file (from `state.dropped_files`), if any.
    pub fn dropzone_overlay(&mut self, win_r: Rect, title: &str) -> Option<std::path::PathBuf> {
        // Self-heal a lost OLE DragLeave: expire the hover after the grace
        // period. A drop still works regardless of this overlay's visibility.
        if let Some(at) = self.state.hovered_file_at {
            if at.elapsed() > UiState::DROP_HOVER_GRACE {
                self.state.hovered_file = None;
                self.state.hovered_file_at = None;
            }
        }
        let t = self.theme;
        if let Some(hov_path) = self.state.hovered_file.clone() {
            self.scene.begin_overlay();
            self.scene.rect(
                win_r.x,
                win_r.y,
                win_r.w,
                win_r.h,
                if t.dark {
                    rgba(20, 20, 20, 235)
                } else {
                    rgba(240, 240, 240, 235)
                },
            );
            let pad = t.space_double();
            let dx = win_r.x + pad;
            let dy = win_r.y + pad;
            let dw = (win_r.w - pad * 2.0).max(1.0);
            let dh = (win_r.h - pad * 2.0).max(1.0);
            self.scene.rounded_rect(
                dx,
                dy,
                dw,
                dh,
                t.radius2(),
                rgba(76, 132, 229, 28),
                (0, 0, 0, 0),
                0.0,
            );
            self.scene.dashed_rect(
                dx,
                dy,
                dw,
                dh,
                (t.rem * 0.6).round(),
                (t.rem * 0.35).round(),
                2.0,
                t.highlight,
            );

            let cx = dx + dw * 0.5;
            let cy = dy + dh * 0.5;
            let icon_sz = (t.rem * 3.2).round();
            self.scene
                .icon_centered(self.text_sys, ICON_EQ, cx, cy - t.rem * 3.0, icon_sz, t.highlight);

            let fs_large = t.font_large();
            let tw = self
                .text_sys
                .measure(FontFace::SemiBold, self.font_engine, title, fs_large);
            self.scene.text(
                self.text_sys,
                FontFace::SemiBold,
                self.font_engine,
                title,
                (cx - tw * 0.5).round(),
                (cy - t.rem * 0.2 + fs_large * 0.34).round(),
                fs_large,
                t.highlight,
            );

            let sub = hov_path.to_string_lossy();
            let fs_base = t.font_base();
            let sw = self
                .text_sys
                .measure(FontFace::Regular, self.font_engine, &sub, fs_base);
            self.scene.text(
                self.text_sys,
                FontFace::Regular,
                self.font_engine,
                &sub,
                (cx - sw * 0.5).round(),
                (cy + t.rem * 1.4 + fs_base * 0.34).round(),
                fs_base,
                t.text,
            );

            self.scene.end_overlay();
        }
        self.state.dropped_files.pop()
    }

    // ==================================================================
    // Menus — `.nui-menu-bar`, `.nui-menu-dropdown`, `nui-context-menu`
    // ==================================================================

    /// Natural width/height of a dropdown panel for `items` (FNV-stable, cached-free:
    /// menus are measured once per frame, which is a handful of shaped runs).
    fn menu_metrics(&mut self, items: &[MenuItem<'_>]) -> (f32, f32) {
        let t = self.theme;
        let row_h = (t.rem * 1.95).round();
        let sep_h = (t.rem * 0.5).round();
        let pad_x = t.space();
        let lead_gap = t.space_half();
        let fs = (t.rem * 0.875).round();
        let fs_short = (t.rem * 0.75).round();
        let icon_sz = (t.rem * 1.0).round();

        let mut content_w = 0.0f32;
        // `nui-menu.css` panels are `padding: var(--nui-space-quarter) 0` — 4px of
        // breathing room above the first row and below the last one.
        let pad_y = t.space_quarter();
        let mut drop_h = pad_y * 2.0;
        for it in items {
            let (label, icon, check, shortcut, chevron) = match it {
                MenuItem::Separator => {
                    drop_h += sep_h;
                    continue;
                }
                MenuItem::Action {
                    label,
                    icon,
                    shortcut,
                    checked,
                    ..
                } => (*label, *icon, checked.is_some(), *shortcut, false),
                MenuItem::Submenu { label, icon, .. } => (*label, *icon, false, "", true),
            };
            drop_h += row_h;
            let lead = if icon.is_empty() { 0.0 } else { icon_sz + lead_gap }
                + if check { icon_sz + lead_gap } else { 0.0 };
            let lw = self
                .text_sys
                .measure(FontFace::Regular, self.font_engine, label, fs);
            let sw = if shortcut.is_empty() {
                0.0
            } else {
                self.text_sys
                    .measure(FontFace::Regular, self.font_engine, shortcut, fs_short)
                    + lead_gap
            };
            let cw = if chevron { icon_sz + lead_gap } else { 0.0 };
            content_w = content_w.max(lead + lw + sw + cw);
        }
        ((content_w + pad_x * 2.0).round().max((t.rem * 12.0).round()), drop_h)
    }

    /// Draw one dropdown panel and everything inside it, including the hovered
    /// submenu flyout. `live` is `false` while the close animation plays, so a
    /// fading panel cannot be clicked. Returns the `action` of the row that fired.
    #[allow(clippy::too_many_arguments)]
    fn draw_menu_dropdown(
        &mut self,
        menu_id: u32,
        live: bool,
        drop_x: f32,
        drop_y: f32,
        win_w: f32,
        win_h: Option<f32>,
        items: &[MenuItem<'_>],
    ) -> Option<String> {
        let t = self.theme;
        let row_h = (t.rem * 1.95).round();
        let sep_h = (t.rem * 0.5).round();
        let pad_x = t.space();
        let pad_y = t.space_quarter();
        let icon_sz = (t.rem * 1.0).round();
        let child_id = menu_id ^ 0x9A00_0000;

        let (drop_w, drop_h) = self.menu_metrics(items);
        // Keep the panel on screen instead of letting rows run off the edge.
        let mut x0 = drop_x.max(0.0);
        if x0 + drop_w > win_w {
            x0 = (win_w - drop_w - 4.0).max(0.0);
        }
        let mut y0 = drop_y;
        if let Some(bottom) = win_h {
            if y0 + drop_h > bottom {
                y0 = (bottom - drop_h - 4.0).max(0.0);
            }
        }

        let open_t = self.state.animate_bool(menu_id ^ 0x9500_0000, live, 0.25);
        if !live && open_t <= 0.01 {
            if self.state.active_submenu.map(|(m, _)| m) == Some(menu_id) {
                self.state.active_submenu = None;
            }
            return None;
        }

        let cursor = self.state.cursor;
        // Geometry pass first: the flyout has to stay open while the cursor is inside
        // it, which is only knowable once every row's position is fixed.
        let mut row_tops: Vec<f32> = Vec::with_capacity(items.len());
        let mut sub_idx: Option<usize> = None;
        let mut y = y0 + pad_y;
        for (i, it) in items.iter().enumerate() {
            row_tops.push(y);
            match it {
                MenuItem::Separator => y += sep_h,
                MenuItem::Submenu { items: kids, disabled, .. } => {
                    let row_r = Rect::new(x0 + 1.0, y, drop_w - 2.0, row_h);
                    if !*disabled
                        && !kids.is_empty()
                        && row_r.contains(cursor.0, cursor.1)
                    {
                        sub_idx = Some(i);
                    }
                    y += row_h;
                }
                MenuItem::Action { .. } => y += row_h,
            }
        }
        let flyout_x = |child_w: f32| -> f32 {
            let right = x0 + drop_w - 2.0;
            if right + child_w <= win_w {
                right
            } else {
                (x0 - child_w).max(0.0)
            }
        };
        if sub_idx.is_none() {
            if let Some(i) = self
                .state
                .active_submenu
                .filter(|(m, _)| *m == menu_id)
                .map(|(_, i)| i)
            {
                if let Some(MenuItem::Submenu { items: kids, disabled: false, .. }) = items.get(i) {
                    let (cw, _) = self.menu_metrics(kids);
                    if Rect::new(flyout_x(cw), row_tops[i], cw, drop_h)
                        .contains(cursor.0, cursor.1)
                    {
                        sub_idx = Some(i);
                    }
                }
            }
        }
        self.state.active_submenu = sub_idx.map(|i| (menu_id, i));

        let mut panel_r = Rect::new(x0, y0, drop_w, drop_h);
        if let Some(i) = sub_idx {
            if let Some(MenuItem::Submenu { items: kids, .. }) = items.get(i) {
                let (cw, ch) = self.menu_metrics(kids);
                panel_r = union_rect(panel_r, Rect::new(flyout_x(cw), row_tops[i], cw, ch));
            }
        }
        if live {
            self.state.popup_rect = Some(panel_r);
        }

        // `nui-dropdown.css`: `opacity: 0 -> 1` with `transform: translateY(-4px) -> translateY(0)`
        // over 0.15s. The panel keeps its full height while opening — clipping it to
        // `drop_h * open_t` cut the bottom rows off during the fade.
        let view_r = Rect::new(x0, y0 + (1.0 - open_t) * -4.0, drop_w, drop_h);
        self.scene.begin_overlay();
        self.scene.shadow(
            view_r.x,
            view_r.y + 3.0,
            view_r.w,
            view_r.h,
            t.radius2(),
            t.rem * 0.7,
            scale_alpha(rgba(0, 0, 0, if t.dark { 85 } else { 26 }), open_t),
        );
        self.scene.push_clip(view_r.x, view_r.y, view_r.w, view_r.h);
        self.scene.rounded_rect(
            view_r.x,
            view_r.y,
            view_r.w,
            view_r.h,
            t.radius2(),
            scale_alpha(t.shade2, open_t),
            scale_alpha(t.border2, open_t),
            1.0,
        );
        self.scene.pop_clip();

        self.scene.push_clip(
            view_r.x + 1.0,
            view_r.y + 1.0,
            view_r.w - 2.0,
            (view_r.h - 2.0).max(1.0),
        );
        let mut result = None;
        let mut y = view_r.y + pad_y;
        for (idx, it) in items.iter().enumerate() {
            match it {
                MenuItem::Separator => {
                    self.scene.rect(
                        view_r.x + 1.0,
                        (y + sep_h * 0.5).round(),
                        (view_r.w - 2.0).max(0.0),
                        1.0,
                        scale_alpha(t.border2, open_t),
                    );
                    y += sep_h;
                }
                MenuItem::Action {
                    label,
                    icon,
                    shortcut,
                    action,
                    disabled,
                    checked,
                } => {
                    let row_r = Rect::new(view_r.x + 1.0, y, view_r.w - 2.0, row_h);
                    let hovered = live
                        && !*disabled
                        && row_r.contains(cursor.0, cursor.1);
                    if hovered {
                        self.state.hovered_id = Some(menu_id);
                        // Only the menu that owns the entry clears it: a row in a flyout
                        // must not close the parent's flyout out from under the cursor.
                        if self.state.active_submenu.map(|(m, _)| m) == Some(menu_id) {
                            self.state.active_submenu = None;
                        }
                        if self.state.mouse_pressed {
                            result = Some(action.to_string());
                            self.state.open_popup_id = None;
                            self.state.active_submenu = None;
                            self.state.mouse_pressed = false;
                        }
                    }
                    if hovered {
                        self.scene.rect(
                            row_r.x,
                            row_r.y,
                            row_r.w,
                            row_r.h,
                            scale_alpha(t.shade3, open_t),
                        );
                    }
                    self.menu_row_content(
                        row_r,
                        pad_x,
                        icon_sz,
                        row_h,
                        matches!(checked, Some(true)),
                        icon,
                        label,
                        shortcut,
                        false,
                        if *disabled {
                            scale_alpha(t.text_muted, 0.5 * open_t)
                        } else {
                            scale_alpha(t.text, open_t)
                        },
                        scale_alpha(t.text_muted, open_t),
                    );
                    y += row_h;
                }
                MenuItem::Submenu { label, icon, disabled, .. } => {
                    let row_r = Rect::new(view_r.x + 1.0, y, view_r.w - 2.0, row_h);
                    let hovered = live
                        && sub_idx == Some(idx)
                        && row_r.contains(cursor.0, cursor.1);
                    if hovered {
                        self.state.hovered_id = Some(menu_id);
                        self.scene.rect(
                            row_r.x,
                            row_r.y,
                            row_r.w,
                            row_r.h,
                            scale_alpha(t.shade3, open_t),
                        );
                    }
                    self.menu_row_content(
                        row_r,
                        pad_x,
                        icon_sz,
                        row_h,
                        false,
                        icon,
                        label,
                        "",
                        true,
                        if *disabled {
                            scale_alpha(t.text_muted, 0.5 * open_t)
                        } else {
                            scale_alpha(t.text, open_t)
                        },
                        scale_alpha(t.text_dim, open_t),
                    );
                    y += row_h;
                }
            }
        }
        self.scene.pop_clip();

        if let Some(i) = sub_idx {
            if let Some(MenuItem::Submenu { items: kids, .. }) = items.get(i) {
                let (cw, _) = self.menu_metrics(kids);
                if let Some(a) =
                    self.draw_menu_dropdown(child_id, live, flyout_x(cw), row_tops[i], win_w, win_h, kids)
                {
                    result = Some(a);
                }
            }
        }

        self.scene.end_overlay();
        result
    }

    /// One dropdown row: optional check mark / leading icon, the label, and a
    /// right-aligned hint — shortcut text for actions, chevron for submenus.
    #[allow(clippy::too_many_arguments)]
    fn menu_row_content(
        &mut self,
        row_r: Rect,
        pad_x: f32,
        icon_sz: f32,
        row_h: f32,
        check: bool,
        icon: &str,
        label: &str,
        shortcut: &str,
        chevron: bool,
        fg: Color,
        hint_color: Color,
    ) {
        let t = self.theme;
        let lead_gap = t.space_half();
        let fs = (t.rem * 0.875).round();
        let fs_short = (t.rem * 0.75).round();
        let cy = row_r.y + row_h * 0.5;
        let baseline = (cy + fs * 0.33).round();

        let mut cx = row_r.x + pad_x;
        if check {
            self.scene
                .icon_centered(self.text_sys, ICON_CHECK, cx + icon_sz * 0.5, cy, icon_sz, fg);
            cx += icon_sz + lead_gap;
        }
        if !icon.is_empty() {
            self.scene
                .icon_centered(self.text_sys, icon, cx + icon_sz * 0.5, cy, icon_sz, fg);
            cx += icon_sz + lead_gap;
        }

        let trailing_w = if chevron {
            icon_sz
        } else if shortcut.is_empty() {
            0.0
        } else {
            self.text_sys
                .measure(FontFace::Regular, self.font_engine, shortcut, fs_short)
        };
        self.text_clipped(
            FontFace::Regular,
            self.font_engine,
            label,
            cx.round(),
            baseline,
            fs,
            (row_r.x + row_r.w - pad_x - trailing_w - cx).max(0.0),
            fg,
        );
        if chevron {
            self.scene.icon_centered(
                self.text_sys,
                ICON_CHEVRON_RIGHT,
                row_r.x + row_r.w - pad_x - icon_sz * 0.5,
                cy,
                icon_sz,
                hint_color,
            );
        } else if !shortcut.is_empty() {
            self.text_clipped(
                FontFace::Regular,
                self.font_engine,
                shortcut,
                (row_r.x + row_r.w - pad_x - trailing_w).round(),
                baseline,
                fs_short,
                trailing_w.max(0.0),
                hint_color,
            );
        }
    }

    /// `.nui-menu-bar`: hover or press a top-level item to open its dropdown. While
    /// any menu of this bar is open, sliding sideways switches to the neighbour.
    pub fn menu_bar<'b>(
        &mut self,
        base_id: u32,
        r: Rect,
        entries: &[MenuBarEntry<'b>],
    ) -> Option<String> {
        let t = self.theme;
        self.scene
            .rect(r.x, r.y, r.w, r.h, if t.dark { t.shade1 } else { t.shade2 });
        self.scene.rect(r.x, r.y + r.h - 1.0, r.w, 1.0, t.border2);

        let fs = (t.rem * 0.875).round();
        let pad_x = t.rem;
        let baseline = (r.y + r.h * 0.5 + fs * 0.33).round();
        let n = entries.len() as u32;
        let mut item_x: Vec<f32> = Vec::with_capacity(entries.len());
        let mut tx = r.x;

        for (i, entry) in entries.iter().enumerate() {
            let menu_id = base_id + i as u32;
            let tw = self
                .text_sys
                .measure(FontFace::Regular, self.font_engine, entry.label, fs);
            let item_w = (tw + pad_x * 2.0).round();
            let item_r = Rect::new(tx, r.y, item_w, r.h);
            item_x.push(tx);
            tx += item_w;

            let hovered = self.state.can_hover(menu_id, item_r);
            let was_open = self.state.open_popup_id == Some(menu_id);
            let any_open = matches!(self.state.open_popup_id, Some(p) if p >= base_id && p < base_id + n);
            if hovered {
                self.state.hovered_id = Some(menu_id);
                if self.state.mouse_pressed {
                    self.state.open_popup_id = if was_open { None } else { Some(menu_id) };
                    self.state.active_submenu = None;
                    // Consumed so `end_frame` does not read this same click as an
                    // outside click and immediately close the menu we just opened.
                    self.state.mouse_pressed = false;
                } else if any_open && !was_open {
                    self.state.open_popup_id = Some(menu_id);
                    self.state.active_submenu = None;
                }
            }

            let is_open = self.state.open_popup_id == Some(menu_id);
            let hov_t = self.state.animate_bool(menu_id ^ 0x9100_0000, hovered, 0.15);
            if is_open {
                // `nui-menu.css`: the open item fills the full bar height (minus the
                // 1px bottom rule) with a square accent block, not a floating pill.
                self.scene.rect(
                    item_r.x,
                    item_r.y,
                    item_r.w,
                    (item_r.h - 1.0).max(0.0),
                    t.highlight,
                );
                self.scene.text(
                    self.text_sys,
                    FontFace::Regular,
                    self.font_engine,
                    entry.label,
                    (item_r.x + pad_x).round(),
                    baseline,
                    fs,
                    rgb(255, 255, 255),
                );
            } else {
                if hov_t > 0.01 {
                    self.scene.rect(
                        item_r.x,
                        item_r.y,
                        item_r.w,
                        (item_r.h - 1.0).max(0.0),
                        scale_alpha(t.shade3, hov_t),
                    );
                }
                self.scene.text(
                    self.text_sys,
                    FontFace::Regular,
                    self.font_engine,
                    entry.label,
                    (item_r.x + pad_x).round(),
                    baseline,
                    fs,
                    t.text,
                );
            }
        }

        let open = self
            .state
            .open_popup_id
            .filter(|p| *p >= base_id && *p < base_id + n)
            .map(|p| (p - base_id) as usize);
        match open {
            Some(i) => self.draw_menu_dropdown(
                base_id + i as u32,
                true,
                item_x[i],
                r.y + r.h,
                r.x + r.w,
                None,
                entries[i].items,
            ),
            None => None,
        }
    }

    /// `nui-context-menu`: right-click inside `trigger_r` opens the menu at the cursor,
    /// clamped so it never runs off the window.
    pub fn context_menu<'b>(
        &mut self,
        id: u32,
        trigger_r: Rect,
        win_r: Rect,
        items: &[MenuItem<'b>],
    ) -> Option<String> {
        if self.state.right_pressed
            && trigger_r.contains(self.state.cursor.0, self.state.cursor.1)
        {
            self.state.open_popup_id = Some(id);
            self.state.context_menu_pos = self.state.cursor;
            self.state.active_submenu = None;
            self.state.right_pressed = false;
        }
        let is_open = self.state.open_popup_id == Some(id);
        let (cx, cy) = self.state.context_menu_pos;
        self.draw_menu_dropdown(
            id,
            is_open,
            cx,
            cy,
            win_r.x + win_r.w,
            Some(win_r.y + win_r.h),
            items,
        )
    }

    // ==================================================================
    // nui-link-list[mode="fold"] — collapsible navigation
    // ==================================================================

    /// Walk the tree once, advancing every group's expansion animation exactly once and
    /// recording `(anim_key, eased, natural children height)` so the render pass can
    /// size each group without re-stepping (a second step would double the speed).
    fn link_list_measure(&mut self, entries: &[LinkListEntry<'_>], anims: &mut Vec<(u32, f32, f32)>) -> f32 {
        let t = self.theme;
        // `nui-theme.css:633` — group headers are `min-height: var(--nui-space-triple)`.
        let hdr_h = (t.rem * 3.0).round();
        // `nui-link-list[mode="fold"] ul li { min-height: var(--nui-space-triple) }`.
        let row_h = t.space_triple();
        let sep_h = (t.rem * 0.5).round();
        let mut h = 0.0;
        for e in entries {
            match e {
                LinkListEntry::Separator => h += sep_h,
                LinkListEntry::Item { .. } => h += row_h,
                LinkListEntry::Group {
                    id,
                    expanded,
                    items,
                    ..
                } => {
                    h += hdr_h;
                    let a = self.state.animate_bool(*id ^ 0x7100_0000, *expanded, 0.30);
                    let kids_h = self.link_list_measure(items, anims);
                    anims.push((*id ^ 0x7100_0000, a, kids_h));
                    h += kids_h * a;
                }
            }
        }
        h
    }

    /// `nui-link-list[mode="fold"]` inside a scroll viewport. Returns `true` when a row
    /// was activated; `active_key` and `toggled_group_id` are updated in place so the
    /// host keeps ownership of the navigation state.
    pub fn link_list<'b>(
        &mut self,
        base_id: u32,
        r: Rect,
        entries: &[LinkListEntry<'b>],
        active_key: &mut String,
        toggled_group_id: &mut Option<u32>,
    ) -> bool {
        let mut anims: Vec<(u32, f32, f32)> = Vec::new();
        let content_h = self.link_list_measure(entries, &mut anims);
        self.scroll_area(base_id, r, content_h, false, |ui, vp, scroll_y| {
            ui.link_list_items(
                base_id,
                Rect::new(vp.x, vp.y - scroll_y, vp.w, content_h),
                entries,
                active_key,
                toggled_group_id,
                &anims,
            )
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn link_list_items(
        &mut self,
        base_id: u32,
        r: Rect,
        entries: &[LinkListEntry<'_>],
        active_key: &mut String,
        toggled_group_id: &mut Option<u32>,
        anims: &[(u32, f32, f32)],
    ) -> bool {
        let t = self.theme;
        let hdr_h = (t.rem * 3.0).round();
        // `nui-link-list[mode="fold"] ul li { min-height: var(--nui-space-triple) }`.
        let row_h = t.space_triple();
        let sep_h = (t.rem * 0.5).round();
        let fs = t.font_small();
        let pad_x = t.space();
        let icon_sz = (t.rem * 1.05).round();
        let chev_sz = (t.rem * 1.0).round();
        let accent_w = (t.rem * 0.3).round().max(4.0);
        let mut clicked = false;
        let mut y = r.y;

        for entry in entries {
            match entry {
                LinkListEntry::Separator => {
                    self.scene.rect(
                        r.x + pad_x * 0.5,
                        (y + sep_h * 0.5).round(),
                        (r.w - pad_x).max(0.0),
                        1.0,
                        t.border1,
                    );
                    y += sep_h;
                }
                LinkListEntry::Item {
                    key,
                    label,
                    icon,
                    badge,
                } => {
                    let id = base_id ^ fnv1a(key);
                    let row_r = Rect::new(r.x, y, r.w, row_h);
                    let hovered = self.state.can_hover(id, row_r);
                    let is_active = *active_key == *key;
                    if hovered {
                        self.state.hovered_id = Some(id);
                        if self.state.mouse_pressed {
                            self.state.active_id = Some(id);
                        }
                        if self.state.mouse_released && self.state.active_id == Some(id) {
                            *active_key = (*key).to_string();
                            clicked = true;
                        }
                    }

                    let hov_t = self
                        .state
                        .animate_bool(id ^ 0x7200_0000, hovered && !is_active, 0.15);
                    let act_t = self.state.animate_bool(id ^ 0x7300_0000, is_active, 0.15);
                    // `nui-theme.css:736` / `:741` — hover and active are distinct
                    // flat fills, not a single shade ramp.
                    let hov_bg = if t.dark { rgb(55, 55, 55) } else { rgb(240, 240, 240) };
                    let act_bg = if t.dark { rgb(60, 65, 70) } else { rgb(230, 235, 240) };
                    if hov_t > 0.01 {
                        self.scene.rect(
                            row_r.x,
                            row_r.y,
                            row_r.w,
                            row_h,
                            lerp_color((hov_bg.0, hov_bg.1, hov_bg.2, 0), hov_bg, hov_t),
                        );
                    }
                    if act_t > 0.01 {
                        self.scene.rect(
                            row_r.x,
                            row_r.y,
                            row_r.w,
                            row_h,
                            lerp_color((act_bg.0, act_bg.1, act_bg.2, 0), act_bg, act_t),
                        );
                        self.scene.rect(
                            row_r.x,
                            row_r.y,
                            accent_w,
                            row_h,
                            scale_alpha(t.highlight, act_t),
                        );
                        // `nui-theme.css:595` `li.list-item.active::before` — the
                        // right-edge chevron marks the current page.
                        self.draw_chevron_rotated(
                            row_r.x + row_r.w - pad_x - chev_sz * 0.35,
                            row_r.y + row_h * 0.5,
                            (t.rem * 0.22).round().max(3.0),
                            1.8,
                            -std::f32::consts::FRAC_PI_2,
                            scale_alpha(t.highlight, act_t),
                        );
                    }

                    let fg = lerp_color(lerp_color(t.text_dim, t.text, hov_t), t.highlight, act_t);
                    let icon_col = lerp_color(t.shade6, t.highlight, act_t.max(hov_t));
                    // `nui-theme.css:726` — `.group-items > li a span { padding-left: 2.7rem }`
                    // indents children so their labels align under the parent label.
                    let mut cx = if icon.is_empty() {
                        row_r.x + (t.rem * 2.7).round()
                    } else {
                        row_r.x + pad_x
                    };
                    if !icon.is_empty() {
                        self.scene.icon_centered(
                            self.text_sys,
                            icon,
                            cx + icon_sz * 0.5,
                            row_r.y + row_h * 0.5,
                            icon_sz,
                            icon_col,
                        );
                        cx += icon_sz + t.space_half();
                    }

                    let label_max = if badge.is_empty() {
                        row_r.x + row_r.w - pad_x - cx
                    } else {
                        let bfs = (t.rem * 0.7).round();
                        let upper = badge.to_uppercase();
                        let bt = self.text_sys.measure(
                            FontFace::SemiBold,
                            self.font_engine,
                            &upper,
                            bfs,
                        );
                        let bw = (bt + t.rem * 0.85).round();
                        let bh = (t.rem * 1.25).round();
                        let bx = (row_r.x + row_r.w - pad_x - bw).round();
                        let by = (row_r.y + (row_h - bh) * 0.5).round();
                        self.scene.rounded_rect(
                            bx, by, bw, bh, bh * 0.5, t.shade4, (0, 0, 0, 0), 0.0,
                        );
                        self.scene.text(
                            self.text_sys,
                            FontFace::SemiBold,
                            self.font_engine,
                            &upper,
                            (bx + t.rem * 0.42).round(),
                            (by + bh * 0.5 + bfs * 0.355).round(),
                            bfs,
                            t.text_dim,
                        );
                        bx - t.space_half() - cx
                    };
                    self.text_clipped(
                        FontFace::Regular,
                        self.font_engine,
                        label,
                        cx.round(),
                        (row_r.y + row_h * 0.5 + fs * 0.33).round(),
                        fs,
                        label_max.max(0.0),
                        fg,
                    );
                    y += row_h;
                }
                LinkListEntry::Group {
                    id,
                    label,
                    icon,
                    expanded,
                    items,
                } => {
                    let gid = base_id ^ id.wrapping_mul(0x9E37_79B9);
                    let hdr_r = Rect::new(r.x, y, r.w, hdr_h);
                    // Expansion comes from the measure pass so the tray height stays
                    // identical in the layout and the paint pass; the caret reads it here.
                    let (a, kids_h) = anims
                        .iter()
                        .find(|(k, _, _)| *k == (*id ^ 0x7100_0000))
                        .map(|(_, a, h)| (*a, *h))
                        .unwrap_or((if *expanded { 1.0 } else { 0.0 }, 0.0));
                    let hovered = self.state.can_hover(gid, hdr_r);
                    if hovered {
                        self.state.hovered_id = Some(gid);
                        if self.state.mouse_pressed {
                            *toggled_group_id = Some(*id);
                            self.state.mouse_pressed = false;
                        }
                    }
                    // Header background: hover tints it, an expanded group lifts it a step
                    // (`nui-theme.css:731`).
                    let lifted = if t.dark { rgb(48, 48, 48) } else { rgb(240, 240, 240) };
                    self.scene
                        .rect(hdr_r.x, hdr_r.y, hdr_r.w, hdr_h, scale_alpha(lifted, a));
                    if hovered {
                        self.scene.rect(
                            hdr_r.x,
                            hdr_r.y,
                            hdr_r.w,
                            hdr_h,
                            scale_alpha(t.shade3, 1.0 - a),
                        );
                    }

                    let mut cx = hdr_r.x + pad_x;
                    if !icon.is_empty() {
                        self.scene.icon_centered(
                            self.text_sys,
                            icon,
                            cx + icon_sz * 0.5,
                            hdr_r.y + hdr_h * 0.5,
                            icon_sz,
                            lerp_color(t.text_dim, t.text, a),
                        );
                        cx += icon_sz + t.space_half();
                    }
                    // Caret rotates from right (-90deg, collapsed) to down (0deg, expanded).
                    self.draw_chevron_rotated(
                        hdr_r.x + hdr_r.w - pad_x - chev_sz * 0.35,
                        hdr_r.y + hdr_h * 0.5,
                        (t.rem * 0.24).round().max(3.5),
                        1.8,
                        (a - 1.0) * std::f32::consts::FRAC_PI_2,
                        lerp_color(
                            rgba(t.text_dim.0, t.text_dim.1, t.text_dim.2, 90),
                            t.highlight,
                            a,
                        ),
                    );
                    self.text_clipped(
                        FontFace::Regular,
                        self.font_engine,
                        label,
                        cx.round(),
                        (hdr_r.y + hdr_h * 0.5 + fs * 0.33).round(),
                        fs,
                        (hdr_r.x
                            + hdr_r.w
                            - pad_x
                            - chev_sz
                            - t.space_half()
                            - cx)
                            .max(0.0),
                        lerp_color(t.text_dim, t.text, a.max(if hovered { 1.0 } else { 0.0 })),
                    );

                    let vis_h = kids_h * a;
                    if vis_h > 0.5 {
                        let kids_r = Rect::new(r.x, y + hdr_h, r.w, vis_h);
                        // Inset tray behind the children (nui-theme.css:720).
                        let tray_bg = if t.dark { rgb(30, 30, 30) } else { rgb(255, 255, 255) };
                        self.scene.rect(kids_r.x, kids_r.y, kids_r.w, kids_r.h, tray_bg);
                        self.scene.push_clip(r.x, kids_r.y, r.w, vis_h);
                        clicked |= self.link_list_items(
                            base_id,
                            kids_r,
                            items,
                            active_key,
                            toggled_group_id,
                            anims,
                        );
                        self.scene.pop_clip();
                    }
                    y += hdr_h + vis_h;
                }
            }
        }
        clicked
    }

    // ==================================================================
    // nui-dialog — modal with backdrop, header bar and an arbitrary body
    // ==================================================================

    /// Modal dialog centred in `win_r`. While `*open` it claims the popup slot, so
    /// widgets behind the backdrop cannot be hovered or clicked and a click on the
    /// backdrop (or `Escape`) clears the slot, which syncs `*open` back to `false`
    /// on the next frame. Closing plays the same scale/slide out as the open.
    pub fn dialog(
        &mut self,
        id: u32,
        win_r: Rect,
        open: &mut bool,
        title: &str,
        icon: &str,
        dialog_w: f32,
        dialog_h: f32,
        body_fn: impl FnOnce(&mut Self, Rect),
    ) {
        let t = self.theme;
        let open_t = self.state.animate_bool(id ^ 0x8100_0000, *open, 0.30);
        if !*open && open_t <= 0.01 {
            if self.state.modal_id == Some(id) {
                self.state.modal_id = None;
            }
            return;
        }

        let sc = 0.95 + 0.05 * open_t;
        let dw = (dialog_w * sc).round();
        let dh = (dialog_h * sc).round();
        let dx = (win_r.x + (win_r.w - dw) * 0.5).round();
        let dy = (win_r.y + (win_r.h - dh) * 0.5 + (1.0 - open_t) * 8.0).round();
        let dialog_r = Rect::new(dx, dy, dw, dh);

        if *open {
            self.state.modal_id = Some(id);
            if self.state.key_cmds.contains(&KeyCmd::Escape) && self.state.open_popup_id.is_none() {
                *open = false;
                self.state.modal_id = None;
            }
        }
        // Everything from here on is drawn inside the modal, so its controls stay
        // hoverable while the widgets behind the backdrop do not.
        self.state.inside_popup = true;

        self.scene.begin_overlay();
        self.scene.rect(
            win_r.x,
            win_r.y,
            win_r.w,
            win_r.h,
            rgba(15, 15, 15, (195.0 * open_t).round() as u8),
        );
        self.scene.shadow(
            dx,
            dy + 3.0,
            dw,
            dh,
            t.radius2(),
            t.rem * 1.1,
            rgba(0, 0, 0, ((if t.dark { 105.0 } else { 32.0 }) * open_t).round() as u8),
        );
        self.scene.rounded_rect(
            dx,
            dy,
            dw,
            dh,
            t.radius2(),
            scale_alpha(if t.dark { t.shade1 } else { t.base }, open_t),
            scale_alpha(t.border2, open_t),
            1.0,
        );

        let hdr_h = (t.rem * 3.0).round();
        self.scene.rect(
            dx + 1.0,
            dy + 1.0,
            (dw - 2.0).max(0.0),
            hdr_h - 1.0,
            scale_alpha(t.shade2, open_t),
        );
        self.scene.rect(dx, dy + hdr_h, dw, 1.0, scale_alpha(t.border2, open_t));

        let fs = t.font_small();
        let icon_sz = (t.rem * 1.15).round();
        let mut tx = dx + t.space();
        if !icon.is_empty() {
            self.scene.icon_centered(
                self.text_sys,
                icon,
                tx + icon_sz * 0.5,
                dy + hdr_h * 0.5,
                icon_sz,
                scale_alpha(t.highlight, open_t),
            );
            tx += icon_sz + t.space_half();
        }
        self.text_clipped(
            FontFace::SemiBold,
            self.font_engine,
            title,
            tx.round(),
            (dy + hdr_h * 0.5 + fs * 0.33).round(),
            fs,
            (dx + dw - t.rem * 3.0 - tx).max(0.0),
            scale_alpha(t.text, open_t),
        );

        let cb = (t.rem * 1.75).round();
        let cb_r = Rect::new(
            dx + dw - t.rem * 0.6 - cb,
            dy + (hdr_h - cb) * 0.5,
            cb,
            cb,
        );
        if self.button(id + 1, cb_r, "", ICON_CLOSE, ButtonVariant::Ghost) {
            *open = false;
            self.state.modal_id = None;
        }

        let body_r = Rect::new(
            dx + 1.0,
            dy + hdr_h + 1.0,
            (dw - 2.0).max(0.0),
            (dh - hdr_h - 2.0).max(0.0),
        );
        self.scene
            .push_clip(body_r.x, body_r.y, body_r.w, body_r.h);
        body_fn(self, body_r);
        self.state.inside_popup = false;
        self.scene.pop_clip();
        self.scene.end_overlay();

        // Clicking the dim backdrop outside the dialog (and outside any open dropdown) closes it.
        if *open && self.state.mouse_pressed && !dialog_r.contains(self.state.cursor.0, self.state.cursor.1) {
            let in_dropdown = self
                .state
                .popup_rect
                .map(|pr| pr.contains(self.state.cursor.0, self.state.cursor.1))
                .unwrap_or(false);
            if !in_dropdown {
                *open = false;
                self.state.modal_id = None;
                self.state.mouse_pressed = false;
            }
        }
    }
}

fn union_rect(a: Rect, b: Rect) -> Rect {
    let x0 = a.x.min(b.x);
    let y0 = a.y.min(b.y);
    let x1 = (a.x + a.w).max(b.x + b.w);
    let y1 = (a.y + a.h).max(b.y + b.h);
    Rect::new(x0, y0, x1 - x0, y1 - y0)
}

/// FNV-1a so a `&str` key maps to the same widget id every frame.
fn fnv1a(s: &str) -> u32 {
    let mut h: u32 = 2166136261;
    for b in s.as_bytes() {
        h ^= *b as u32;
        h = h.wrapping_mul(16777619);
    }
    h
}

fn prev_char_boundary(s: &str, idx: usize) -> usize {
    let mut i = idx.min(s.len());
    while i > 0 {
        i -= 1;
        if s.is_char_boundary(i) {
            return i;
        }
    }
    0
}

fn next_char_boundary(s: &str, idx: usize) -> usize {
    let mut i = idx.min(s.len());
    while i < s.len() {
        i += 1;
        if s.is_char_boundary(i) {
            return i;
        }
    }
    s.len()
}
