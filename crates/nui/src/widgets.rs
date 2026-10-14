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
pub const ICON_DARK_MODE: &str = "\u{e51c}";
pub const ICON_LIGHT_MODE: &str = "\u{e518}";
pub const ICON_EQ: &str = "\u{e01d}";
pub const ICON_VOLUME: &str = "\u{e050}";
pub const ICON_LOCK: &str = "\u{e897}";
pub const ICON_TUNE: &str = "\u{e429}";

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

    #[inline]
    pub fn radius1(&self) -> f32 {
        (self.rem * 0.14).round().max(2.0)
    }

    #[inline]
    pub fn radius2(&self) -> f32 {
        (self.rem * 0.25).round().max(3.0)
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

    last_click_time: std::time::Instant,
    last_click_pos: (f32, f32),
}

impl UiState {
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
            last_click_time: std::time::Instant::now(),
            last_click_pos: (-1000.0, -1000.0),
        }
    }

    pub fn on_mouse_press(&mut self) {
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
        // If a popup is open and the user clicks outside both the popup and its trigger,
        // we close the popup at the end of the frame if nothing claimed it.
    }

    pub fn end_frame(&mut self) {
        if self.mouse_pressed {
            if let (Some(_), Some(pr)) = (self.open_popup_id, self.popup_rect) {
                if !pr.contains(self.cursor.0, self.cursor.1) && self.active_id != self.open_popup_id
                {
                    self.open_popup_id = None;
                    self.popup_rect = None;
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
}

pub struct Ui<'a> {
    pub state: &'a mut UiState,
    pub scene: &'a mut Scene,
    pub text_sys: &'a mut TextSystem,
    pub font_engine: FontEngine,
    pub theme: Theme,
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
            layout_stack: Vec::new(),
        }
    }

    /// Draw an `nui-input-group` uppercase label above a control.
    pub fn input_label(&mut self, x: f32, y: f32, label: &str) {
        let fs = self.theme.font_xsmall();
        self.scene.text(
            self.text_sys,
            FontFace::SemiBold,
            self.font_engine,
            &label.to_uppercase(),
            x.round(),
            (y + fs * 0.85).round(),
            fs,
            self.theme.label_upper,
        );
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
        let icon_sz = (t.rem * 1.08).round();
        let pad_x = t.space();
        let gap = t.space_quarter();

        // Bottom border line across full tablist width
        self.scene.rect(r.x, r.y + r.h - 1.0, r.w, 1.0, t.border2);

        let mut tx = r.x;
        let cy = r.y + (r.h - 2.0) * 0.5;
        let baseline = (cy + fs * 0.33).round();

        for (i, &(icon, label)) in labels.iter().enumerate() {
            let id = base_id + i as u32;
            let text_w = self
                .text_sys
                .measure(FontFace::SemiBold, self.font_engine, label, fs);
            let icon_w = if icon.is_empty() {
                0.0
            } else {
                icon_sz + (t.rem * 0.4).round()
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
            if hovered && !is_selected {
                self.scene.rounded_rect(
                    tab_r.x,
                    tab_r.y + 2.0,
                    tab_r.w,
                    tab_r.h - 3.0,
                    t.radius1(),
                    t.shade2,
                    (0, 0, 0, 0),
                    0.0,
                );
            }

            let fg = if is_selected {
                t.highlight
            } else if hovered {
                t.text
            } else {
                t.text_dim
            };

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
                if is_selected {
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
                self.scene
                    .rect(tab_r.x, tab_r.y + tab_r.h - 2.0, tab_r.w, 2.0, t.highlight);
            }

            tx += tab_w + gap;
        }
        changed
    }

    /// `nui-button` (Primary, Outline, Ghost, Danger, Success).
    /// Matches nui-theme.css: uppercase xsmall text, border-radius1, highlight-dim -> highlight hover.
    pub fn button(
        &mut self,
        id: u32,
        r: Rect,
        label: &str,
        icon: &str,
        variant: ButtonVariant,
    ) -> bool {
        let t = self.theme;
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

        let (bg, border, border_w, fg) = match variant {
            ButtonVariant::Primary => {
                let bg = if hovered { t.highlight } else { t.highlight_dim };
                (bg, rgba(255, 255, 255, 32), 1.0, rgb(255, 255, 255))
            }
            ButtonVariant::Outline => {
                if hovered {
                    (t.palette_gray, t.palette_gray, 1.0, rgb(255, 255, 255))
                } else {
                    ((0, 0, 0, 0), t.palette_gray, 1.0, t.text_dim)
                }
            }
            ButtonVariant::Ghost => {
                if hovered {
                    (t.shade3, (0, 0, 0, 0), 0.0, t.text)
                } else {
                    ((0, 0, 0, 0), (0, 0, 0, 0), 0.0, t.text_dim)
                }
            }
            ButtonVariant::Danger => {
                let bg = if hovered {
                    t.palette_alert_hi
                } else {
                    t.palette_alert
                };
                (bg, rgba(255, 255, 255, 32), 1.0, rgb(255, 255, 255))
            }
            ButtonVariant::Success => {
                let bg = if hovered {
                    t.palette_activate_hi
                } else {
                    t.palette_activate
                };
                (bg, rgba(255, 255, 255, 32), 1.0, rgb(255, 255, 255))
            }
        };

        if bg.3 > 0 || border_w > 0.0 {
            self.scene
                .rounded_rect(r.x, r.y, r.w, r.h, t.radius1(), bg, border, border_w);
        }
        // Active click white flash (`nui-button button:active::after { opacity: 0.2 }`)
        if active && self.state.mouse_down {
            self.scene.rounded_rect(
                r.x,
                r.y,
                r.w,
                r.h,
                t.radius1(),
                rgba(255, 255, 255, 48),
                (0, 0, 0, 0),
                0.0,
            );
        }

        let upper = label.to_uppercase();
        let fs = t.font_xsmall();
        let icon_sz = (t.rem * 1.12).round();
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
            icon_sz + (t.rem * 0.35).round()
        };
        let total_w = icon_w + text_w;
        let mut cx = r.x + (r.w - total_w) * 0.5;
        let cy = r.y + r.h * 0.5;

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
        self.scene
            .rounded_rect(r.x, r.y, r.w, r.h, t.radius1(), t.base, t.shade3, 1.0);

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
            if is_active {
                self.scene.rounded_rect(
                    sr.x,
                    sr.y,
                    sr.w,
                    sr.h,
                    if i == 0 || i + 1 == n { t.radius1() } else { 0.0 },
                    t.highlight,
                    (0, 0, 0, 0),
                    0.0,
                );
                // Keep inner seam square when first or last segment is active
                if i == 0 && n > 1 {
                    self.scene
                        .rect(sr.x + sr.w - t.radius1(), sr.y, t.radius1(), sr.h, t.highlight);
                } else if i + 1 == n && n > 1 {
                    self.scene
                        .rect(sr.x, sr.y, t.radius1(), sr.h, t.highlight);
                }
            } else if hovered {
                self.scene
                    .rect(sr.x + 1.0, sr.y + 1.0, sr.w - 2.0, sr.h - 2.0, t.shade2);
            }

            if i + 1 < n && !is_active && *selected != i + 1 {
                self.scene
                    .rect(sr.x + sr.w - 1.0, sr.y + 1.0, 1.0, sr.h - 2.0, t.shade3);
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
            self.scene.text(
                self.text_sys,
                FontFace::SemiBold,
                self.font_engine,
                &upper,
                (sr.x + (sr.w - tw) * 0.5).round(),
                baseline,
                fs,
                fg,
            );
        }
        changed
    }

    /// `nui-checkbox`: 1.25rem box, 2px `shade4` border (`highlight` on hover/checked),
    /// `base` interior (`highlight` when checked + white checkmark).
    pub fn checkbox(&mut self, id: u32, r: Rect, label: &str, checked: &mut bool) -> bool {
        let t = self.theme;
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
        let (bg, border_col) = if *checked {
            (t.highlight, t.highlight)
        } else if hovered {
            (t.base, t.highlight)
        } else {
            (t.base, t.shade4)
        };
        self.scene
            .rounded_rect(r.x.round(), by, box_sz, box_sz, 3.0, bg, border_col, 2.0);

        if *checked {
            let icon_sz = (box_sz * 0.85).round();
            self.scene.icon_centered(
                self.text_sys,
                ICON_CHECK,
                r.x.round() + box_sz * 0.5,
                by + box_sz * 0.5,
                icon_sz,
                rgb(255, 255, 255),
            );
        }

        self.scene.text(
            self.text_sys,
            FontFace::Regular,
            self.font_engine,
            label,
            (r.x + box_sz + t.rem * 0.55).round(),
            (cy + fs * 0.33).round(),
            fs,
            if hovered { t.text } else { t.text_dim },
        );
        changed
    }

    /// `nui-checkbox[variant="switch"]`: 2.25em x 1.25em pill, `shade4` -> `highlight`, white thumb.
    pub fn switch(&mut self, id: u32, r: Rect, label: &str, on: &mut bool) -> bool {
        let t = self.theme;
        let sw_h = (t.rem * 1.24).round();
        let sw_w = (sw_h * 1.82).round();
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
        let track_col = if *on {
            t.highlight
        } else if hovered {
            t.shade5
        } else {
            t.shade4
        };
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
        let thumb_x = if *on {
            r.x.round() + sw_w - pad - thumb_d
        } else {
            r.x.round() + pad
        };
        self.scene.shadow(
            thumb_x,
            sy + pad + 1.0,
            thumb_d,
            thumb_d,
            thumb_d * 0.5,
            3.0,
            rgba(0, 0, 0, 70),
        );
        self.scene.rounded_rect(
            thumb_x,
            sy + pad,
            thumb_d,
            thumb_d,
            thumb_d * 0.5,
            rgb(255, 255, 255),
            (0, 0, 0, 0),
            0.0,
        );

        self.scene.text(
            self.text_sys,
            FontFace::Regular,
            self.font_engine,
            label,
            (r.x + sw_w + t.rem * 0.55).round(),
            (cy + fs * 0.33).round(),
            fs,
            if hovered { t.text } else { t.text_dim },
        );
        changed
    }

    /// `nui-radio`: 1.25rem circle, 2px `shade4` border (`highlight` when checked/hovered),
    /// `0.5rem` inner `highlight` dot when selected.
    pub fn radio(&mut self, id: u32, r: Rect, label: &str, selected: bool) -> bool {
        let t = self.theme;
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
        let border_col = if selected || hovered {
            t.highlight
        } else {
            t.shade4
        };
        self.scene
            .rounded_rect(rx, ry, d, d, d * 0.5, t.base, border_col, 2.0);

        if selected {
            let mut dot_d = (d * 0.44).round() as i32;
            if dot_d % 2 != 0 {
                dot_d += 1;
            }
            let dot_d = dot_d as f32;
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

        self.scene.text(
            self.text_sys,
            FontFace::Regular,
            self.font_engine,
            label,
            (rx + d + t.rem * 0.55).round(),
            (cy + fs * 0.33).round(),
            fs,
            if hovered || selected { t.text } else { t.text_dim },
        );
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

        // Top row: label on left, formatted value on right
        let lbl_y = (r.y + fs * 0.88).round();
        self.scene.text(
            self.text_sys,
            FontFace::Regular,
            self.font_engine,
            label,
            r.x.round(),
            lbl_y,
            fs,
            t.text_dim,
        );
        let vw = self
            .text_sys
            .measure(FontFace::SemiBold, self.font_engine, val_text, fs);
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
            4.0,
            rgba(0, 0, 0, 90),
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
            if focused { 6.0 } else { 3.5 },
            rgba(0, 0, 0, if t.dark { 55 } else { 25 }),
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
        let is_open = self.state.open_popup_id == Some(id);
        let hovered = r.contains(self.state.cursor.0, self.state.cursor.1)
            && (self.state.active_id.is_none() || self.state.active_id == Some(id));
        let mut changed = false;

        if hovered {
            self.state.hovered_id = Some(id);
            if self.state.mouse_pressed {
                self.state.active_id = Some(id);
                self.state.focused_id = Some(id);
                if is_open {
                    self.state.open_popup_id = None;
                    self.state.popup_rect = None;
                } else {
                    self.state.open_popup_id = Some(id);
                }
            }
        }

        let is_open = self.state.open_popup_id == Some(id);
        let bg = if is_open {
            t.shade3
        } else if hovered {
            if t.dark { t.shade4 } else { t.shade2 }
        } else if t.dark {
            t.shade3
        } else {
            t.base
        };
        let border_col = if is_open || hovered {
            t.shade5
        } else {
            t.shade4
        };

        let rad = t.radius2();
        self.scene.shadow(
            r.x,
            r.y + 1.0,
            r.w,
            r.h,
            rad,
            4.5,
            rgba(0, 0, 0, if t.dark { 75 } else { 30 }),
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

        self.scene.icon_centered(
            self.text_sys,
            if is_open {
                ICON_CHEVRON_UP
            } else {
                ICON_CHEVRON_DOWN
            },
            r.x + r.w - pad_x - arrow_sz * 0.35,
            cy,
            arrow_sz,
            t.shade5,
        );

        // Render top-layer popup in Scene's overlay pass so it paints above all page widgets
        if is_open {
            // Option rows: `padding: var(--nui-space-half) var(--nui-space)` -> 2.25rem.
            let row_h = (t.rem * 2.25).round();
            let pop_h = row_h * options.len() as f32 + 2.0;
            let pop_r = Rect::new(r.x, r.y + r.h, r.w, pop_h);
            self.state.popup_rect = Some(pop_r);

            self.scene.begin_overlay();

            // Rich soft SDF drop shadow + popup surface (`nui-select-popup` with `border-radius: 0 0 r r`)
            self.scene.shadow(
                pop_r.x,
                pop_r.y + 4.0,
                pop_r.w,
                pop_r.h,
                rad,
                10.0,
                rgba(0, 0, 0, if t.dark { 130 } else { 55 }),
            );
            let pop_bg = if t.dark { t.shade2 } else { t.base };
            self.scene.push_clip(pop_r.x, pop_r.y, pop_r.w, pop_r.h);
            self.scene.rounded_rect(
                pop_r.x,
                pop_r.y - rad,
                pop_r.w,
                pop_r.h + rad,
                rad,
                pop_bg,
                border_col,
                1.0,
            );
            self.scene.pop_clip();
            // 1px separator line between trigger and popup
            self.scene
                .rect(pop_r.x + 1.0, pop_r.y, pop_r.w - 2.0, 1.0, t.shade3);

            self.scene
                .push_clip(pop_r.x + 1.0, pop_r.y + 1.0, pop_r.w - 2.0, pop_r.h - 2.0);
            for (i, &opt) in options.iter().enumerate() {
                let opt_r = Rect::new(
                    pop_r.x + 1.0,
                    pop_r.y + 1.0 + i as f32 * row_h,
                    pop_r.w - 2.0,
                    row_h,
                );
                let opt_hov = opt_r.contains(self.state.cursor.0, self.state.cursor.1);
                let is_sel = *selected == i;

                if opt_hov {
                    self.state.hovered_id = Some(id);
                    if self.state.mouse_pressed {
                        *selected = i;
                        self.state.open_popup_id = None;
                        self.state.popup_rect = None;
                        changed = true;
                    }
                }

                if opt_hov {
                    self.scene
                        .rect(opt_r.x, opt_r.y, opt_r.w, opt_r.h, t.shade4);
                } else if is_sel {
                    let sel_bg = if t.dark {
                        rgb(50, 50, 50)
                    } else {
                        t.highlight_dim
                    };
                    self.scene.rect(opt_r.x, opt_r.y, opt_r.w, opt_r.h, sel_bg);
                }

                let fg = if is_sel {
                    rgb(255, 255, 255)
                } else if opt_hov {
                    t.text
                } else {
                    t.text_dim
                };

                let o_cy = opt_r.y + opt_r.h * 0.5;
                let o_base = (o_cy + fs * 0.33).round();

                self.scene.text(
                    self.text_sys,
                    if is_sel {
                        FontFace::SemiBold
                    } else {
                        FontFace::Regular
                    },
                    self.font_engine,
                    opt,
                    (pop_r.x + pad_x).round(),
                    o_base,
                    fs,
                    fg,
                );

                if is_sel {
                    let csz = (t.rem * 1.05).round();
                    self.scene.icon_centered(
                        self.text_sys,
                        ICON_CHECK,
                        pop_r.x + pop_r.w - pad_x - arrow_sz * 0.35,
                        o_cy,
                        csz,
                        fg,
                    );
                }
            }
            self.scene.pop_clip();
            self.scene.end_overlay();
        }

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

        self.scene.text(
            self.text_sys,
            FontFace::Regular,
            self.font_engine,
            index_str,
            (r.x + t.rem * 0.6).round(),
            baseline,
            fs,
            t.text_muted,
        );

        let title_x = r.x + t.rem * 3.4;
        let face = if selected {
            FontFace::SemiBold
        } else {
            FontFace::Regular
        };
        self.scene.text(
            self.text_sys,
            face,
            self.font_engine,
            title,
            title_x.round(),
            baseline,
            fs,
            t.text,
        );

        if !right_meta.is_empty() {
            let mw = self
                .text_sys
                .measure(FontFace::Regular, self.font_engine, right_meta, fs);
            self.scene.text(
                self.text_sys,
                FontFace::Regular,
                self.font_engine,
                right_meta,
                (r.x + r.w - mw - t.rem * 0.6).round(),
                baseline,
                fs,
                t.text_dim,
            );
        }

        clicked
    }
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
