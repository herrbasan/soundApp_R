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
    /// Vertical gap added after each allocated row.
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
        let rem = self.theme.rem;
        let gap_px = gap.resolve(rem);
        let avail = (r.w - gap_px * (N as f32 - 1.0)).max(0.0);
        let widths = flex_widths(avail, rem, specs);

        let mut out = [Rect::new(0.0, 0.0, 0.0, 0.0); N];
        let mut cursor = r.x;
        for i in 0..N {
            let x0 = cursor.round();
            let is_last = i == N - 1;
            let x1 = if is_last {
                r.x + r.w
            } else {
                (cursor + widths[i]).round()
            };
            out[i] = Rect::new(x0, r.y, (x1 - x0).max(0.0), r.h);
            cursor += widths[i] + gap_px;
        }
        out
    }

    /// Split `r` vertically into `N` rows separated by `gap` (mirror of [`Ui::cols`]).
    pub fn rows<const N: usize>(&self, r: Rect, gap: Length, specs: [Length; N]) -> [Rect; N] {
        let rem = self.theme.rem;
        let gap_px = gap.resolve(rem);
        let avail = (r.h - gap_px * (N as f32 - 1.0)).max(0.0);
        let heights = flex_widths(avail, rem, specs);

        let mut out = [Rect::new(0.0, 0.0, 0.0, 0.0); N];
        let mut cursor = r.y;
        for i in 0..N {
            let y0 = cursor.round();
            let is_last = i == N - 1;
            let y1 = if is_last {
                r.y + r.h
            } else {
                (cursor + heights[i]).round()
            };
            out[i] = Rect::new(r.x, y0, r.w, (y1 - y0).max(0.0));
            cursor += heights[i] + gap_px;
        }
        out
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
            r.y + (t.rem * 0.25).round(),
            r.w,
            r.h,
            t.radius2(),
            (t.rem * 0.85).round(),
            crate::widgets::rgba(0, 0, 0, if t.dark { 75 } else { 28 }),
        );
        self.scene.rounded_rect(
            r.x,
            r.y,
            r.w,
            r.h,
            t.radius2(),
            t.shade2,
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

    /// Standard `nui-card` defaults from `nui-theme.css`: `1.25rem` padding, `1.0rem` gap.
    pub fn nui_card<R>(&mut self, r: Rect, f: impl FnOnce(&mut Self) -> R) -> R {
        self.card(r, Length::Rem(1.25), Length::Rem(1.0), f)
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

    /// Split `r` horizontally into `responsive_col_count` equal columns separated by `gap`
    /// (default `--nui-space`, 1rem). Edges snap to integer pixels; the last column takes
    /// the remainder so there are never subpixel seams.
    pub fn layout_grid(&self, r: Rect, target_cols: usize, gap: Length) -> Vec<Rect> {
        let n = self.responsive_col_count(r.w, target_cols);
        let gap_px = gap.resolve(self.theme.rem);
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
        self.cols(row, Length::Rem(1.0), specs)
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
    /// (`0.95rem`) plus the `--nui-space-quarter` label/control gap, then a control row of
    /// `control_h`. Returns the control rect.
    pub fn form_field(&mut self, label: &str, control_h: Length) -> Rect {
        let (x, y, w) = {
            let c = self.layout_stack.last().expect("Ui layout_stack is empty");
            (c.bounds.x, c.y, c.bounds.w)
        };
        self.input_label_clipped(x, y, w, label);
        let adv = (self.theme.rem * 0.95).round() + self.theme.space_quarter();
        {
            let c = self
                .layout_stack
                .last_mut()
                .expect("Ui layout_stack is empty");
            c.y = (c.y + adv).round();
        }
        self.take(control_h)
    }

    /// `nui-input-group` with the standard `--nui-form-row-height` (2.5rem) control height.
    pub fn input_group(&mut self, label: &str) -> Rect {
        self.form_field(label, Length::Rem(2.5))
    }
}
