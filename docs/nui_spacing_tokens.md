# NUI spacing tokens — nui_wc2 v3.0.0 "slate" → Rust `nui`

Source of truth: `D:\Work\_GIT\nui_wc2\NUI\css\nui-theme.css` (+ `NUI/css/modules/*.css`),
cross-checked against the live playground (`herrbasan.github.io/nui_wc2/Playground`) via
computed styles. Web rem base = **16px** (`--nui-rem-base`); Rust `Theme.rem` is the
physical px per rem (16 × dpi_scale × zoom), so every web `rem` maps to `Theme.rem × k`.

## 1. Foundation scale

| Web token | CSS | rem | px @16 | Rust (existing / new) |
|---|---|---|---|---|
| `--nui-rem-base` | `16px` | 1 | 16 | `Theme.rem` |
| `--nui-space` | `1rem` | 1 | 16 | `Theme::space()` |
| `--nui-space-eighth` | `calc(space / 8)` | 0.125 | 2 | `Theme::space_eighth()` |
| `--nui-space-quarter` | `calc(space / 4)` | 0.25 | 4 | `Theme::space_quarter()` |
| `--nui-space-half` | `calc(space / 2)` | 0.5 | 8 | `Theme::space_half()` |
| `--nui-space-double` | `calc(space * 2)` | 2 | 32 | `Theme::space_double()` |
| `--nui-space-triple` | `calc(space * 3)` | 3 | 48 | `Theme::space_triple()` |
| `--nui-space-quadruple` | `calc(space * 4)` | 4 | 64 | `Theme::space_quadruple()` **(new)** |
| `--nui-layout-gap` | `1rem` (default) | 1 | 16 | `Theme::layout_gap()` **(new)** |
| `--nui-breakpoint-mobile` | `320px` | — | 320 | `Theme::breakpoint_mobile()` **(new)** |
| `--nui-breakpoint-tablet` | `640px` | — | 640 | `Theme::breakpoint_tablet()` **(new)** |

## 2. Type ramp

| Web token | CSS | px @16 | Rust |
|---|---|---|---|
| `--font-size-xsmall` | `0.8rem` | 12.8 | `Theme::font_xsmall()` |
| `--font-size-small` | `0.9rem` | 14.4 | `Theme::font_small()` |
| `--font-size-base` | `1rem` | 16 | `Theme::font_base()` |
| `--font-size-medium` | `1.2rem` | 19.2 | `Theme::font_medium()` |
| `--font-size-large` | `1.5rem` | 24 | `Theme::font_large()` |
| `--font-size-xlarge` | `2rem` | 32 | `Theme::font_xlarge()` **(new)** |
| `--icon-size` | `1.2rem` | 19.2 | `Theme::icon_size()` **(new)** |

## 3. Radii / borders / shadows

| Web token | CSS | px @16 | Rust |
|---|---|---|---|
| `--border-radius1` | `var(--nui-space-eighth)` | 2 | `Theme::radius1()` (was 0.14rem → now eighth) |
| `--border-radius2` | `var(--nui-space-quarter)` | 4 | `Theme::radius2()` |
| `--border-radius3` | `var(--nui-space-half)` | 8 | `Theme::radius3()` **(new)** |
| `--border-thickness` | `thin` (1px) | 1 | 1.0 px literals |
| `--shadow-radius0` | `var(--nui-space-half)` | 8 | — |
| `--shadow-radius1` | `var(--nui-space)` | 16 | — |

## 4. Component heights & paddings (measured on the live playground)

| Component | Web rule | px @16 | Rust target |
|---|---|---|---|
| `--nui-form-row-height` | `2.5rem` | 40 | `Theme::form_row_height()` |
| form row `size="small"` | `var(--nui-space-double)` | 32 | `Theme::form_row_small()` |
| `nui-input` | `height: var(--nui-form-row-height)` | 40 | `input_group()` control height |
| `nui-input` radius | `--border-radius1` | 2 | `radius1()` |
| inner `<input>` padding | `0 var(--nui-space)` | 0 / 16 | `text_input()` `pad_x = space()` |
| `nui-input-group` | `flex column; gap: var(--nui-space-quarter)` | 4 | `Theme::label_gap()` **(new)** |
| `nui-input-group` margin-bottom | `var(--nui-space)` | 16 | `Theme::group_gap()` **(new)** |
| `nui-input-group > label` | `font-size: var(--font-size-xsmall)` | 12.8 | `font_xsmall()` |
| label line box | measured line-height 20.48px | 1.28rem | `Theme::label_line_height()` **(new)** |
| label colour | `rgba(255,255,255,0.5)` | — | `Theme::label_upper` (135 alpha) |
| `nui-card` padding | `var(--nui-space)` | 16 | `Theme::card_padding()` **(new)** → `nui_card()` |
| `nui-card` gap | `var(--nui-space)` | 16 | `Theme::card_gap()` **(new)** → `nui_card()` |
| `nui-card` margin-bottom | `var(--nui-space)` | 16 | card stack gap |
| `nui-card` radius | `--border-radius2` | 4 | `radius2()` |
| `nui-card` shadow | `0 0.3rem 1rem rgba(0,0,0,.3)` | y 4.8 / blur 16 | `card()` shadow |
| `nui-form-row` gap | `var(--nui-space)` | 16 | `Theme::form_row_gap()` **(new)** |
| `nui-button-container` gap | `var(--nui-space-half)` | 8 | `Theme::button_gap()` **(new)** |
| base `button` min-height | `var(--nui-space-double)` | 32 | `Theme::button_height()` **(new)** |
| base `button` padding | `var(--nui-space-quarter) var(--nui-space)` | 4 / 16 | `Theme::button_pad_x()` **(new)** |
| `nui-button button` min-width | `6rem` | 96 | `Theme::button_min_width()` **(new)** |
| `nui-button button` font | `var(--font-size-xsmall)` uppercase | 12.8 | `font_xsmall()` |
| `nui-select` control height | `var(--nui-form-row-height)` | 40 | `select()` |
| `nui-select` control padding | `0 var(--nui-space)` | 0 / 16 | `select()` |
| `nui-checkbox` gap | `var(--nui-space-half)` | 8 | `Theme::checkbox_gap()` **(new)** |
| `nui-checkbox` min-height | `2rem` | 32 | `Theme::control_height()` **(new)** |
| `nui-badge` padding | `0.25rem 0.6rem` | 4 / 9.6 | `badge()` |
| `nui-badge` font | `0.75rem` | 12 | `badge()` |
| `nui-tabs [role=tablist]` gap | `var(--nui-space-quarter)` | 4 | `Theme::tab_gap()` **(new)** |
| `nui-tabs [role=tablist]` margin-bottom | `var(--nui-space)` | 16 | `Theme::tabs_margin_bottom()` **(new)** |
| `nui-tabs [role=tab]` padding | `var(--nui-space-half) var(--nui-space)` | 8 / 16 | `Theme::tab_pad_x()` **(new)** |
| `nui-tabs [role=tab]` gap | `var(--nui-space-half)` | 8 | `tabs()` icon gap |
| `nui-link-list li` min-height | `var(--nui-space-double)` (fold: triple) | 32 / 48 | `Theme::link_item_height()` **(new)** |
| `nui-link-list ul` margin-bottom | `var(--nui-space)` | 16 | sidebar stack |
| `nui-menu-bar` height | `var(--nui-space-double)` | 32 | `Theme::menu_bar_height()` **(new)** |
| `nui-menu-item` padding | `0 var(--nui-space)` | 0 / 16 | `menu_bar()` |
| `nui-menu-item` font | `0.875rem` | 14 | `menu_bar()` |
| `nui-app-header` height | `--app-header-height: 4rem` | 64 | `Theme::header_height()` |
| `nui-app` titlebar / statusbar | `3rem` / `2rem` | 48 / 32 | `window.rs` header |
| `nui-sidebar` width | `--sidebar-width: 21rem` | 336 | `Theme::sidebar_width()` |
| `nui-page > *` padding | `var(--nui-space) var(--nui-space-double)` | 16 / 32 | `Theme::page_pad_x()` / `page_pad_y()` **(new)** |
| `nui-page > section` margin-bottom | `var(--nui-space-triple)` | 48 | `Theme::section_gap()` **(new)** |
| `nui-page > header` margin-bottom | `var(--nui-space-double)` | 32 | `Theme::section_gap()` |
| `--space-page-maxwidth` | `56rem` | 896 | `Theme::page_maxwidth()` |

## 5. Headings (web `h1/h2/h3`)

| Element | Font size | Line height | Margin-bottom | Colour / weight |
|---|---|---|---|---|
| `h1` | `2rem` (xlarge) | `2.2rem` | `var(--nui-space)` | highlight, 300 |
| `h2` | `1.5rem` (large) | `1.8rem` | `var(--nui-space)` | highlight, 300 |
| `h3` | `1.2rem` (medium) | `2rem` | `0` | contrast, 400 |

Rust `Ui::heading()` ≈ web `h3`: `font_medium()` (1.2rem) with a `2rem` line box.
