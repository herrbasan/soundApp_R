# Handover — ALPHATAP frameless-window composition offset (RESOLVED)

**Date:** 2026-10-25 · **Machine:** ALPHATAP (5th lab box, test-only, weakest hardware) · **Status:** root cause found, fixed in `crates/nui/src/render.rs`, verified by measurement.

## Symptom

`nui_demo.exe` and `soundapp_r.exe` showed their content offset on ALPHATAP: black strip at the top, content displaced down and slightly right, bottom clipped. The GL framebuffer itself was always correct (`--screenshot` / `glReadPixels`).

## Root cause

The offset **equals the native frame origin for the window's style at its DPI** — `AdjustWindowRectExForDpi(style, dpi)` returns `dx=11 dy=45` at 144 DPI for winit's steady style `0x16CF0000`, and the measured composition offset was exactly `(11,45)`.

winit's `to_window_styles()` unconditionally adds `WS_CAPTION | WS_BORDER | WS_SYSMENU` (+ `WS_SIZEBOX`, `WS_MINIMIZEBOX`, `WS_MAXIMIZEBOX`) and hides the frame only through its `WM_NCCALCSIZE` client-area override. The window manager therefore reports a client area covering the whole window (`GetClientRect` = full rect, `ClientToScreen(0,0)` = window origin — both verified healthy), while this driver stack places the window surface at the frame origin implied by the style bits.

## Measurement method (reusable — this is what the previous session lacked)

The bug is invisible to `glReadPixels`, so it was measured against the composited desktop:

1. `scripts/capture-window.ps1` / `scripts/dwm-window-experiment.ps1` capture the window rect with `CopyFromScreen` in a **per-monitor-v2** thread (physical pixels; without this the capture is DPI-virtualized to 1440×960 and rescaled).
2. `scripts/offset.py` correlates row/column brightness profiles of framebuffer vs capture and reports `(dx,dy)` plus residual; a stdev guard marks blank captures void.
3. `dwm-window-experiment.ps1` also reports `AdjustWindowRectExForDpi`'s frame origin next to the measured offset — the two numbers were identical, which is what identified the cause.

Result on the broken build: `dy=+45 dx=+11`, and `meanAbsDiff = 0.00` at that shift — the composed screen is **byte-identical** to the framebuffer when translated. No scaling, no blur, nothing clipped differently: a pure placement offset.

## Fix

In `crates/nui/src/render.rs`:

- `strip_frame_styles()` — removes `WS_CAPTION | WS_THICKFRAME | WS_SYSMENU | WS_MINIMIZEBOX | WS_MAXIMIZEBOX` (`0x00CF0000`) with `SWP_FRAMECHANGED`. Called at creation and again in `reveal_if_pending()` after `set_visible`, because winit re-applies the bits on every window-flag change. Frame origin becomes `(0,0)`.
- `install_nc_guard()` — a wndproc installed over winit's that answers `WM_NCACTIVATE` (return `TRUE`), `WM_NCPAINT`, `WM_NCUAHDRAW{CAPTION,FRAME}` (return `0`) and forwards everything else. **Required:** with the bits stripped, `WM_NCACTIVATE` still reaches `DefWindowProc`, which paints a light-grey inactive caption across the header on deactivation — the regression that made the previous session abandon stripping.

Being style-based, this is correct on every driver (frame origin `(0,0)` is right everywhere), not a per-machine workaround.

## Verified (measured on ALPHATAP)

| Case | Before | After |
|---|---|---|
| `nui_demo` GPU backend | `dy=+45 dx=+11`, meanAbsDiff 0.00 at shift | `dy=0 dx=0` at zero shift |
| `nui_demo` CPU/softbuffer | same | `dy=+1 dx=0` (1px softbuffer rounding; accepted) |
| `nui_demo` after forced `WM_NCACTIVATE(0)` | caption bar over header | no caption, content unmoved |
| `soundapp_r` (`01.m4a`, 720×417) | — | `dy=0 dx=0` |

## Ruled out by measurement (do not retry)

`SWP_FRAMECHANGED` alone; `DWMWA_NCRENDERING_POLICY = DWMNCRP_DISABLED`; `DwmExtendFrameIntoClientArea(-1,-1,-1,-1)`; the ±1px size nudge while cloaked; minimizing/restoring. DWM's render policies do not move the surface, and minimize kills a WGL window on this driver stack (context loss) — don't use min/restore as a probe.

## Interaction test (after the commit)

1. **Native drag and edge/corner resize work with `WS_THICKFRAME` removed** — confirmed by hand on ALPHATAP: header drag moves the window, edge and corner drags resize it. winit drives both by posting `WM_NCLBUTTONDOWN` with `HT*` codes (`HTCAPTION`, `HTBOTTOMRIGHT`, …), which is style-independent, so `SC_MOVE`/`SC_SIZE` and the frame bits are not needed. No need for a custom drag/resize loop in `live_drag`.

## Open items

1. `nui-demo` shows `dy=+1` on the CPU backend only — softbuffer presentation rounding, cosmetic.
2. Everything else on ALPHATAP is unchanged: `D:` mapping from `\\coolkid\Work` required for `soundapp` builds, test media at `\\BADKID\Storage\Test Files`, GUI binaries launched with `Start-Process -PassThru`.

## Tooling added (committed under `scripts/`)

`capture-window.ps1` (PMv2 window/desktop capture) · `run-and-capture.ps1` (launch + capture) · `dwm-window-experiment.ps1` (apply a DWM/window tweak, report frame metrics, capture) · `offset.py` (offset measurement; needs Pillow) · `nc-loop-probe.ps1` (NC loop probe — inconclusive without a real button press, see Open item 1) · plus the pre-existing `probe-window.ps1` / `probe-style-loop.ps1` Win32 metric probes in `target/`.
