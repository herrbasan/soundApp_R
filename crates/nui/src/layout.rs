// NUI Rust — Declarative Layout Engine (Phase 1.1)
//
// A lightweight, zero-dependency stack / column / row / grid layout cursor so that
// application code never has to accumulate manual `y += h` coordinate arithmetic.
//
//   Length       — a resolved dimension: fixed px, rem-relative, or flex weight.
//   LayoutCursor — one active column: its bounds, the current y pen, and the row gap.
//   Ui methods   — pad/pad_xy, cols/rows, with_column, card/inset_card, plus the stack
//                  cursor helpers (take, take_rest, take_cols, heading, kv_row, form_field).
//
// The cursor stack lives on `Ui::layout_stack`; `with_column`/`card`/`inset_card` push a
// scope and pop it on exit, so nesting is just closure nesting — no explicit begin/end.
#![allow(dead_code)]

use crate::text::FontFace;
use crate::widgets::{Color, Rect, Ui};

/// A resolved layout dimension.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Length {
    /// Fixed physical pixels.
    Px(f32),
    /// Multiples of the theme `rem` (physical px per 1rem).
    Rem(f32),
    /// A flex weight; resolved proportionally against remaining space.
    Fill(f32),
}

impl Length {
    #[inline]
    pub fn px(v: f32) -> Self {
        Length::Px(v)
    }

    #[inline]
    pub fn rem(v: f32) -> Self {
        Length::Rem(v)
    }

    /// Flex weight `1.0`.
    #[inline]
    pub fn fill() -> Self {
        Length::Fill(1.0)
    }

    /// Flex weight `w`.
    #[inline]
    pub fn fill_w(w: f32) -> Self {
        Length::Fill(w)
    }

    /// Resolve a fixed dimension to physical pixels. `Fill` has no intrinsic size and
    /// resolves to `0.0` — it takes its share of remaining space during `cols`/`rows`.
    #[inline]
    pub fn resolve(self, rem: f32) -> f32 {
        match self {
            Length::Px(v) => v,
            Length::Rem(v) => v * rem,
            Length::Fill(_) => 0.0,
        }
    }
}

/// One active column scope on `Ui::layout_stack`.
#[derive(Clone, Copy, Debug)]
pub struct LayoutCursor {
    /// The column's outer bounds (already inset by the owning card's padding).
    pub bounds: Rect,
    /// The current vertical pen — the top edge of the next allocated row.
    pub y: f32,
    /// Vertical gap between adjacent rows, as in CSS `gap`.
    pub gap: f32,
}

#[inline]
fn flex_widths<const N: usize>(avail: f32, rem: f32, specs: [Length; N]) -> [f32; N] {
    let mut fixed = 0.0;
    let mut fill_total = 0.0;
    for s in specs.iter() {
        match *s {
            Length::Fill(w) => fill_total += w.max(0.0),
            other => fixed += other.resolve(rem),
        }
    }
    let remaining = (avail - fixed).max(0.0);
    let mut out = [0.0f32; N];
    for i in 0..N {
        out[i] = match specs[i] {
            Length::Fill(w) => {
                if fill_total > 0.0 {
                    remaining * (w.max(0.0) / fill_total)
                } else {
                    0.0
                }
            }
            other => other.resolve(rem),
        };
    }
    out
}

/// Shared axis splitter behind [`Ui::cols`]/[`Ui::rows`]: reserves fixed `Px`/`Rem`
/// sizes first, shares the remainder between `Fill(w)` weights, snaps edges to integer
/// pixels and gives the last cell the remainder so there are never subpixel seams.
/// Neighbouring cells can never overlap: cell `i` ends at or before cell `i+1` starts.
fn flex_split<const N: usize>(
    horizontal: bool,
    main_start: f32,
    cross_start: f32,
    main_len: f32,
    cross_len: f32,
    gap_px: f32,
    rem: f32,
    specs: [Length; N],
) -> [Rect; N] {
    let avail = (main_len - gap_px * (N as f32 - 1.0)).max(0.0);
    let sizes = flex_widths(avail, rem, specs);

    let mut out = [Rect::new(0.0, 0.0, 0.0, 0.0); N];
    let mut cursor = main_start;
    for i in 0..N {
        let a0 = cursor.round();
        let is_last = i == N - 1;
        let a1 = if is_last {
            main_start + main_len
        } else {
            (cursor + sizes[i]).round()
        };
        let cell = (a1 - a0).max(0.0);
        out[i] = if horizontal {
            Rect::new(a0, cross_start, cell, cross_len)
        } else {
            Rect::new(cross_start, a0, cross_len, cell)
        };
        cursor += sizes[i] + gap_px;
    }
    out
}

/// Split `r` into `n` equal columns separated by `gap_px`; the last column absorbs the
/// rounding remainder (`n` comes pre-resolved from the responsive/viewport policy).
fn grid_rects(n: usize, r: Rect, gap_px: f32) -> Vec<Rect> {
    let col_w = ((r.w - gap_px * (n as f32 - 1.0)).max(0.0) / n as f32).round();
    let mut out = Vec::with_capacity(n);
    let mut cursor = r.x;
    for i in 0..n {
        let x0 = cursor.round();
        let x1 = if i + 1 == n { r.x + r.w } else { x0 + col_w };
        out.push(Rect::new(x0, r.y, (x1 - x0).max(0.0), r.h));
        cursor += col_w + gap_px;
    }
    out
}

impl<'a> Ui<'a> {
    /// Inset `r` on all four sides by `pad`.
    pub fn pad(&self, r: Rect, pad: Length) -> Rect {
        let p = pad.resolve(self.theme.rem);
        Rect::new(
            r.x + p,
            r.y + p,
            (r.w - p * 2.0).max(0.0),
            (r.h - p * 2.0).max(0.0),
        )
    }

    /// Inset `r` horizontally by `px` and vertically by `py`.
    pub fn pad_xy(&self, r: Rect, px: Length, py: Length) -> Rect {
        let x = px.resolve(self.theme.rem);
        let y = py.resolve(self.theme.rem);
        Rect::new(
            r.x + x,
            r.y + y,
            (r.w - x * 2.0).max(0.0),
            (r.h - y * 2.0).max(0.0),
        )
    }

    /// Split `r` horizontally into `N` columns separated by `gap`.
    ///
    /// Fixed `Px`/`Rem` widths are reserved first; the remaining width is shared
    /// proportionally between `Fill(w)` columns. Edges are rounded and the final column
    /// is snapped to `r.x + r.w` so there are never subpixel seams between columns.
    pub fn cols<const N: usize>(&self, r: Rect, gap: Length, specs: [Length; N]) -> [Rect; N] {
        flex_split(
            true,
            r.x,
            r.y,
            r.w,
            r.h,
            gap.resolve(self.theme.rem),
            self.theme.rem,
            specs,
        )
    }

    /// Split `r` vertically into `N` rows separated by `gap` (mirror of [`Ui::cols`]).
    pub fn rows<const N: usize>(&self, r: Rect, gap: Length, specs: [Length; N]) -> [Rect; N] {
        flex_split(
            false,
            r.y,
            r.x,
            r.h,
            r.w,
            gap.resolve(self.theme.rem),
            self.theme.rem,
            specs,
        )
    }

    /// Push a layout column over `r` (row gap `gap`), run `f`, then pop the scope.
    pub fn with_column<R>(&mut self, r: Rect, gap: Length, f: impl FnOnce(&mut Self) -> R) -> R {
        let gap_px = gap.resolve(self.theme.rem).round();
        self.layout_stack.push(LayoutCursor {
            bounds: r,
            y: r.y.round(),
            gap: gap_px,
        });
        // A `4px` pad keeps `:focus-visible` rings and slider thumbs at the column edge
        // intact while clipping any overflowing text or child rows to the column.
        self.scene.push_clip(
            r.x - 4.0,
            r.y - 4.0,
            (r.w + 8.0).max(0.0),
            (r.h + 8.0).max(0.0),
        );
        let out = f(self);
        self.scene.pop_clip();
        self.layout_stack.pop();
        out
    }

    /// Draw a `shade2`/`border2` card surface (`nui-card`) and lay out its padded interior.
    /// The `nui-theme.css` elevation (`box-shadow: 0 0.3rem 1rem rgba(0,0,0,0.3)`) is emitted
    /// before the surface so the opaque fill covers the shadow's interior.
    pub fn card<R>(&mut self, r: Rect, pad: Length, gap: Length, f: impl FnOnce(&mut Self) -> R) -> R {
        let t = self.theme;
        self.scene.shadow(
            r.x,
            r.y + (t.rem * 0.3).round(),
            r.w,
            r.h,
            t.radius2(),
            t.space(),
            crate::widgets::rgba(0, 0, 0, if t.dark { 77 } else { 26 }),
        );
        self.scene.rounded_rect(
            r.x,
            r.y,
            r.w,
            r.h,
            t.radius2(),
            if t.dark { t.shade2 } else { t.shade1 },
            t.border2,
            1.0,
        );
        let inner = self.pad(r, pad);
        self.scene.push_clip(
            r.x + 1.0,
            r.y + 1.0,
            (r.w - 2.0).max(0.0),
            (r.h - 2.0).max(0.0),
        );
        let out = self.with_column(inner, gap, f);
        self.scene.pop_clip();
        out
    }

    /// Standard `nui-card` defaults from `nui-theme.css`: `var(--nui-space)` padding and gap.
    pub fn nui_card<R>(&mut self, r: Rect, f: impl FnOnce(&mut Self) -> R) -> R {
        let pad = self.theme.card_padding();
        let gap = self.theme.card_gap();
        self.card(r, Length::Px(pad), Length::Px(gap), f)
    }

    /// Draw a `base` surface with an explicit `border` (inset panel) and lay out its interior.
    pub fn inset_card<R>(
        &mut self,
        r: Rect,
        pad: Length,
        gap: Length,
        border: Color,
        f: impl FnOnce(&mut Self) -> R,
    ) -> R {
        let t = self.theme;
        self.scene
            .rounded_rect(r.x, r.y, r.w, r.h, t.radius2(), t.base, border, 1.0);
        let inner = self.pad(r, pad);
        self.scene.push_clip(
            r.x + 1.0,
            r.y + 1.0,
            (r.w - 2.0).max(0.0),
            (r.h - 2.0).max(0.0),
        );
        let out = self.with_column(inner, gap, f);
        self.scene.pop_clip();
        out
    }

    // ------------------------------------------------------------------
    // `nui-layout` responsive grid (`documentation/components/layout.md`)
    // ------------------------------------------------------------------

    /// Column count for a `nui-layout` grid over `avail_w`: 1 column below `30rem`
    /// (< 480px mobile), at most 2 below `48rem` (< 768px tablet), else `target_cols`.
    pub fn responsive_col_count(&self, avail_w: f32, target_cols: usize) -> usize {
        let w_rem = avail_w / self.theme.rem;
        if w_rem < 30.0 {
            1
        } else if w_rem < 48.0 {
            target_cols.clamp(1, 2)
        } else {
            target_cols.max(1)
        }
    }

    /// Column count for a `nui-layout` grid at the current viewport width.
    ///
    /// The web reference clamps `nui-layout` columns with *media queries*
    /// (`min-width: 30rem` / `48rem`) — i.e. against the **viewport**, not the
    /// container — so a 3-column grid stays 3 columns inside a narrow card.
    /// Without a published viewport the requested count is used unchanged.
    pub fn viewport_col_count(&self, target_cols: usize) -> usize {
        if self.viewport.0 <= 0.0 {
            return target_cols.max(1);
        }
        self.responsive_col_count(self.viewport.0, target_cols)
    }

    /// Split `r` horizontally into `responsive_col_count` equal columns separated by `gap`
    /// (default `--nui-space`, 1rem). Edges snap to integer pixels; the last column takes
    /// the remainder so there are never subpixel seams.
    ///
    /// When the host published a viewport, the column count resolves from the *viewport*
    /// media queries (`min-width: 30rem` / `48rem`), exactly like the web `nui-layout` —
    /// a 3-column grid keeps 3 columns inside a narrow card. Without a published viewport
    /// the count falls back to the container width (legacy behaviour).
    pub fn layout_grid(&self, r: Rect, target_cols: usize, gap: Length) -> Vec<Rect> {
        let measure_w = if self.viewport.0 > 0.0 {
            self.viewport.0
        } else {
            r.w
        };
        let n = self.responsive_col_count(measure_w, target_cols);
        let gap_px = gap.resolve(self.theme.rem);
        grid_rects(n, r, gap_px)
    }

    /// Allocate a full-width grid row of `row_h` at the cursor, then grid it into columns.
    pub fn take_layout_grid(
        &mut self,
        row_h: Length,
        target_cols: usize,
        gap: Length,
    ) -> Vec<Rect> {
        let row = self.take(row_h);
        self.layout_grid(row, target_cols, gap)
    }

    // ------------------------------------------------------------------
    // Stack cursor helpers (operate on the innermost scope on `layout_stack`)
    // ------------------------------------------------------------------

    /// The bounds of the innermost layout column.
    #[inline]
    pub fn bounds(&self) -> Rect {
        self.layout_stack
            .last()
            .expect("Ui layout_stack is empty")
            .bounds
    }

    /// Allocate a row of height `h` at the cursor and advance the pen past it (plus `gap`).
    pub fn take(&mut self, h: Length) -> Rect {
        let rem = self.theme.rem;
        let h_px = h.resolve(rem);
        let c = self
            .layout_stack
            .last_mut()
            .expect("Ui layout_stack is empty");
        let rect = Rect::new(c.bounds.x, c.y, c.bounds.w, h_px);
        c.y = (c.y + h_px + c.gap).round();
        rect
    }

    /// Allocate a row of height `rem_h * rem`.
    pub fn take_rem(&mut self, rem_h: f32) -> Rect {
        self.take(Length::Rem(rem_h))
    }

    /// Allocate every remaining pixel of the column and consume it.
    pub fn take_rest(&mut self) -> Rect {
        let c = self
            .layout_stack
            .last_mut()
            .expect("Ui layout_stack is empty");
        let h = (c.bounds.y + c.bounds.h - c.y).max(0.0);
        let rect = Rect::new(c.bounds.x, c.y, c.bounds.w, h);
        c.y = c.bounds.y + c.bounds.h;
        rect
    }

    /// Allocate a full-width row of height `h`, then split it into `N` columns.
    pub fn take_cols<const N: usize>(
        &mut self,
        h: Length,
        col_gap: Length,
        specs: [Length; N],
    ) -> [Rect; N] {
        let row = self.take(h);
        self.cols(row, col_gap, specs)
    }

    /// `nui-form-row`: `2.0rem` (`compact`) or `2.5rem` (`--nui-form-row-height`) tall row
    /// split into `N` control slots separated by `--nui-space` gaps.
    pub fn take_form_row<const N: usize>(
        &mut self,
        compact: bool,
        specs: [Length; N],
    ) -> [Rect; N] {
        let h = if compact {
            self.theme.form_row_small()
        } else {
            self.theme.form_row_height()
        };
        let row = self.take(Length::Px(h));
        self.cols(row, Length::Px(self.theme.form_row_gap()), specs)
    }

    /// Advance the pen by `h` without adding the row gap (used for tight label stacks).
    pub fn spacer(&mut self, h: Length) {
        let rem = self.theme.rem;
        let h_px = h.resolve(rem);
        let c = self
            .layout_stack
            .last_mut()
            .expect("Ui layout_stack is empty");
        c.y = (c.y + h_px).round();
    }

    /// Section heading — `font_base` SemiBold in `text`, one `1.35rem` line, no extra gap.
    pub fn heading(&mut self, text: &str) {
        let rem = self.theme.rem;
        let fs = self.theme.font_base();
        let h = (rem * 1.35).round();
        let (x, y, w) = {
            let c = self
                .layout_stack
                .last_mut()
                .expect("Ui layout_stack is empty");
            let x = c.bounds.x;
            let y = c.y;
            let w = c.bounds.w;
            c.y = (c.y + h).round();
            (x, y, w)
        };
        let color = self.theme.text;
        self.text_clipped(
            FontFace::SemiBold,
            self.font_engine,
            text,
            x.round(),
            (y + fs * 0.8).round(),
            fs,
            w,
            color,
        );
    }

    /// Key/value row — `Regular`/`text_dim` key at `x`, `SemiBold`/`text` value at `x + key_w`.
    pub fn kv_row(&mut self, key: &str, val: &str, key_w: Length) {
        let rem = self.theme.rem;
        let fs = self.theme.font_small();
        let kw = key_w.resolve(rem);
        let h = (rem * 1.45).round();
        let (x, y, w) = {
            let c = self
                .layout_stack
                .last_mut()
                .expect("Ui layout_stack is empty");
            let x = c.bounds.x;
            let y = c.y;
            let w = c.bounds.w;
            c.y = (c.y + h).round();
            (x, y, w)
        };
        let key_color = self.theme.text_dim;
        let val_color = self.theme.text;
        let baseline = (y + fs * 0.8).round();
        self.text_clipped(
            FontFace::Regular,
            self.font_engine,
            key,
            x.round(),
            baseline,
            fs,
            (kw - 4.0).max(0.0),
            key_color,
        );
        self.text_clipped(
            FontFace::SemiBold,
            self.font_engine,
            val,
            (x + kw).round(),
            baseline,
            fs,
            (w - kw).max(0.0),
            val_color,
        );
    }

    /// `nui-input-group` field: uppercase label at the cursor, advance by the label line
    /// box (`1.28rem`, web `label { line-height }`) plus the `--nui-space-quarter`
    /// label/control gap, then a control row of `control_h`. Returns the control rect.
    ///
    /// The web group also carries `margin-bottom: var(--nui-space)`; here the enclosing
    /// column gap plays that role, so the pen is topped up to a full `group_gap()` below
    /// the control when the host gap is smaller — consecutive groups sit exactly
    /// `--nui-space` apart regardless of the card's own gap.
    pub fn form_field(&mut self, label: &str, control_h: Length) -> Rect {
        let (x, y, w) = {
            let c = self.layout_stack.last().expect("Ui layout_stack is empty");
            (c.bounds.x, c.y, c.bounds.w)
        };
        self.input_label_clipped(x, y, w, label);
        let adv = self.theme.label_line_height() + self.theme.label_gap();
        {
            let c = self
                .layout_stack
                .last_mut()
                .expect("Ui layout_stack is empty");
            c.y = (c.y + adv).round();
        }
        let control = self.take(control_h);
        let cur_gap = self.layout_stack.last().map(|c| c.gap).unwrap_or(0.0);
        let top_up = self.theme.group_gap() - cur_gap;
        if top_up > 0.5 {
            self.spacer(Length::Px(top_up));
        }
        control
    }

    /// `nui-input-group` with the standard `--nui-form-row-height` (2.5rem) control height.
    pub fn input_group(&mut self, label: &str) -> Rect {
        self.form_field(label, Length::Px(self.theme.form_row_height()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::Scene;
    use crate::text::{FontEngine, TextSystem};
    use crate::widgets::{Theme, UiState};

    /// A real `Ui` over CPU-only primitives — no window, no GPU. Every cursor/geometry
    /// path runs exactly as in the app. Leaked boxes keep the lifetimes self-contained.
    fn test_ui(rem: f32) -> Ui<'static> {
        let state = Box::leak(Box::new(UiState::new()));
        let scene = Box::leak(Box::new(Scene::new()));
        let text = Box::leak(Box::new(TextSystem::new()));
        Ui::new(state, scene, text, FontEngine::Swash, Theme::dark(rem))
    }

    const EPS: f32 = 0.51;

    #[test]
    fn cols_fixed_and_fill_have_no_overlap_and_exact_edges() {
        // The nui-demo `cols([Rem(7), Fill(1)])` case @1.5x dpi (rem 24).
        let ui = test_ui(24.0);
        let r = Rect::new(100.0, 50.0, 800.0, 60.0);
        let [fixed, fill] = ui.cols(r, Length::rem(0.5), [Length::rem(7.0), Length::fill()]);
        assert_eq!(fixed.w, 7.0 * 24.0);
        assert_eq!(fixed.x, r.x);
        assert!(
            fill.x >= fixed.x + fixed.w + 12.0 - EPS,
            "fill.x {} overlaps fixed column ending at {}",
            fill.x,
            fixed.x + fixed.w
        );
        assert!((fill.w - (800.0 - 168.0 - 12.0)).abs() <= 1.0);
        assert!((fill.x + fill.w - (r.x + r.w)).abs() <= EPS);
    }

    #[test]
    fn rows_mirror_cols_on_the_vertical_axis() {
        let ui = test_ui(16.0);
        let r = Rect::new(10.0, 20.0, 300.0, 400.0);
        let [a, b] = ui.rows(r, Length::rem(1.0), [Length::fill(), Length::fill()]);
        assert_eq!(a.x, r.x);
        assert_eq!(a.w, r.w);
        assert!(b.y >= a.y + a.h + 16.0 - EPS, "rows overlap");
        assert_eq!(b.y + b.h, r.y + r.h);
    }

    #[test]
    fn layout_grid_uses_viewport_media_queries_not_container_width() {
        // Regression: a 3-col grid inside a ~653px-wide card @1.5x (27.2rem container)
        // collapsed to 1 column under container-based resolution. The web reference
        // clamps against the viewport (76.2rem) and keeps 3 columns.
        let mut ui = test_ui(24.0);
        ui.viewport = (1830.0, 1110.0);
        let card = Rect::new(0.0, 0.0, 653.0, 76.0);
        let cells = ui.layout_grid(card, 3, Length::rem(0.5));
        assert_eq!(cells.len(), 3, "viewport-based count must keep 3 columns");
        for w in cells.windows(2) {
            assert!(w[1].x >= w[0].x + w[0].w + 12.0 - EPS, "cells overlap");
        }
        assert_eq!(cells[2].x + cells[2].w, card.x + card.w);
    }

    #[test]
    fn layout_grid_falls_back_to_container_without_published_viewport() {
        let ui = test_ui(16.0);
        let cells = ui.layout_grid(Rect::new(0.0, 0.0, 400.0, 50.0), 3, Length::rem(0.5));
        assert_eq!(cells.len(), 1, "no viewport => legacy container resolution");
        let cells = ui.layout_grid(Rect::new(0.0, 0.0, 1200.0, 50.0), 3, Length::rem(0.5));
        assert_eq!(cells.len(), 3);
    }

    #[test]
    fn responsive_breakpoints_match_web_media_queries() {
        let ui = test_ui(16.0);
        assert_eq!(ui.responsive_col_count(479.0, 3), 1, "< 30rem => 1");
        assert_eq!(ui.responsive_col_count(480.0, 3), 2, "30rem => clamp 2");
        assert_eq!(ui.responsive_col_count(767.0, 3), 2);
        assert_eq!(ui.responsive_col_count(768.0, 3), 3, "48rem => target");
        assert_eq!(ui.viewport_col_count(3), 3, "unpublished viewport => target");
    }

    #[test]
    fn form_field_label_block_uses_label_line_height_and_gap_tokens() {
        let mut ui = test_ui(16.0);
        ui.with_column(Rect::new(0.0, 0.0, 400.0, 400.0), Length::rem(0.62), |ui| {
            let y0 = ui.bounds().y;
            let control = ui.form_field("label", Length::rem(2.5));
            let t = &ui.theme;
            let label_block = t.label_line_height() + t.label_gap();
            assert!((control.y - (y0 + label_block)).abs() <= 1.0);
            assert_eq!(control.h, t.form_row_height());
        });
    }

    #[test]
    fn consecutive_input_groups_sit_one_group_gap_apart_in_tight_cards() {
        let mut ui = test_ui(16.0);
        ui.with_column(Rect::new(0.0, 0.0, 400.0, 600.0), Length::rem(0.62), |ui| {
            let g1 = ui.input_group("first");
            let y_after_g1 = ui.layout_stack.last().expect("stack").y;
            // Web: control bottom + margin-bottom var(--nui-space).
            assert_eq!(y_after_g1, g1.y + g1.h + ui.theme.group_gap());
            let g2 = ui.input_group("second");
            // g2's label starts at the pen; its control sits one label block below.
            let label_block = ui.theme.label_line_height() + ui.theme.label_gap();
            assert_eq!(g2.y, y_after_g1 + label_block);
        });
    }

    #[test]
    fn heading_row_then_taken_caption_row_never_overlap() {
        // Pins the nui-demo Layout & Cards fix: the caption must be allocated *below*
        // the heading's line box via the pen, not drawn from the column top.
        let mut ui = test_ui(16.0);
        ui.with_column(Rect::new(0.0, 0.0, 300.0, 200.0), Length::rem(0.2), |ui| {
            let top = ui.bounds().y;
            ui.heading("Rem(7.0)");
            let cap = ui.take(Length::rem(1.45));
            let heading_h = (ui.theme.rem * 1.35).round();
            assert!(cap.y >= top + heading_h - EPS, "caption overlaps heading");
        });
    }

    #[test]
    fn spacing_token_scale_is_rem_derived() {
        for rem in [16.0f32, 24.0] {
            let t = Theme::dark(rem);
            assert_eq!(t.space(), (rem * 1.0).round());
            assert_eq!(t.space_eighth(), (rem * 0.125).round());
            assert_eq!(t.space_quarter(), (rem * 0.25).round());
            assert_eq!(t.space_half(), (rem * 0.5).round());
            assert_eq!(t.space_double(), (rem * 2.0).round());
            assert_eq!(t.space_triple(), (rem * 3.0).round());
            assert_eq!(t.space_quadruple(), (rem * 4.0).round());
            assert_eq!(t.radius1(), t.space_eighth());
            assert_eq!(t.radius2(), t.space_quarter());
            assert_eq!(t.radius3(), t.space_half());
            assert_eq!(t.menu_bar_height(), t.space_double());
            assert_eq!(t.button_height(), t.space_double());
            assert_eq!(t.link_item_height(), t.space_double());
            assert_eq!(t.group_gap(), t.space());
            assert_eq!(t.form_row_gap(), t.space());
            assert_eq!(t.checkbox_gap(), t.space_half());
            assert_eq!(t.button_gap(), t.space_half());
            assert_eq!(t.label_gap(), t.space_quarter());
            assert_eq!(t.label_line_height(), (rem * 1.28).round());
        }
    }
}
