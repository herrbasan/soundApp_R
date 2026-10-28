# Window-toggle diagnostics — what `event_log.txt` records and what it already proved

**Status 2026-10-28.** Written after instrumenting every path that shows, hides or
focuses a window. Field symptom under investigation: *"press once, the window shows;
press again, it hides and instantly reappears; other times several presses do
nothing at all."*

## 1. Why the old log could not answer this

`event_log.txt` recorded key presses and a `toggle 'x' visible=true -> false` line.
That is the app's **belief**. Every one of the field symptoms is a *disagreement*
between that belief and what the OS actually does, so the log could not distinguish:

| Symptom | Needs |
|---|---|
| "several presses do nothing" | logical `visible` vs `IsWindowVisible` vs DWM cloak; frames presented |
| "hides then instantly reappears" | which code path toggled twice, and how fast |
| "shortcut does nothing" | which HWND received the key vs which had focus |

## 2. What is logged now

One file, `event_log.txt` next to `user.json`, truncated per session, flushed per
line, every line carrying `[ms  #seq]` (one monotonic sequence across all writers;
`~` instead of a space = non-UI thread).

| Line | Meaning |
|---|---|
| `key 'm' Pressed repeat=false win=main osfg=monitoring mods=- d_press=63ms d_rel=718ms :: <all windows>` | every key event: target window, OS foreground **at that instant**, physical modifiers (asked of `GetAsyncKeyState`), gap since the last press/release, and the full window state |
| `DUP-PRESS 'm' after 41ms (no repeat flag)` | a second non-repeat `Pressed` for the same key inside one physical press — two toggles per press. Recorded, never suppressed |
| `global 'm' -> ToggleWindow("mixer") fg=…` / `global 'm' SUPPRESSED mods=ctrl text_focus=false` | the single global key pass and **why** it acted or refused |
| `global 's' REPEAT-ECHO suppressed (physical key still down)` | a press refused because the same physical key is still held — the focus-change re-delivery, see §4 |
| `toggle mixer src=global-key logical=false -> true fg=… d_prev=1540ms before=<win state>` | the queued request with **which path asked** (`global-key`, `toolbar`, `dropzone`, `startup`), plus the full state before |
| `toggle mixer applied logical_now=true after=mixer[log=1 os=1 cloak=1 pend=7ms f=1]` | state immediately after the toggle ran |
| `BOUNCE mixer toggled twice in 84ms` | same window toggled again within 200 ms — the "comes back" signature |
| `watch mixer[log=1 os=1 cloak=0 …] -> log=1 os=1 cloak=1 divergence=1` | the divergence watchdog: 10 Hz comparison of the app's flag against `IsWindowVisible` + `DWMWA_CLOAKED` |
| `STALL reveal-stall mixer[log=1 os=1 cloak=1 pend=403ms f=1] held=…ms fg=… :: <all windows>` | a show that never got its frame (once per episode, then 1/s) |
| `STALL diverged …` / `STALL … cleared after Nms` | stuck divergence and its end |
| `reveal RETRY mixer waited=62ms attempt=1` | a bounded, logged re-nudge of a reveal that produced no frame |
| `restore-focus after=mixer -> monitoring` | focus hand-off after a hide |
| `nui[<key>] reveal REQUEST / cloak set hr=… readback=… / set_visible(true) -> cloak=… / strip_frame_styles -> cloak=…` | nui-internal ordering, including HRESULTs |
| `nui[<key>] reveal DONE wait=18ms frames=1 focus=setfg=0 …` | the frame that completed a reveal and how long it waited |
| `nui[<key>] cloak+HIDE pending_was=false …` | hide path |
| `nui focus_now hwnd=0x… mapped=… fg=0x…->0x… granted=false` | every `SetForegroundWindow`: **`granted`** is what Windows actually did |
| `nui taskbar-group RESTORE hwnd=…` | the group-restore path that maps windows |
| `frame <key> while reveal-pending` / `while HIDDEN` | a frame arriving for a window that should not be painting |
| `hide-to-tray`, `request-close`, `tray Show`, `occluded <key> true` | the remaining lifecycle edges |

`--trace` (or `SOUNDAPP_TRACE=1`) adds every presented frame of every secondary window.

### Reading a session

```
Select-String -Path event_log.txt -Pattern 'key |toggle |BOUNCE|DUP-PRESS|STALL|RETRY|focus'
```

*"Nothing happened"* → look at `key … win=…` vs `osfg=…`: if the target is not the
window that had focus, or `global … SUPPRESSED`, the key never reached the pass.
If a `toggle … -> true` **is** followed by `STALL reveal-stall`, the request was
accepted and the frame was lost (§3).

## 3. Root cause already reproduced: the lost first paint

**Symptom covered:** *"toggles to show, but the window never appears"* (and the
"it sometimes takes three presses" variant).

`Renderer::request_reveal` maps the window with `set_visible(true)` and asks for a
redraw. On Windows **`winit::Window::set_visible` is deferred** — it is queued on
winit's thread executor and applied when the event loop next clears events
(`WindowState::set_window_flags`). `request_redraw` → `RedrawWindow(RDW_INTERNALPAINT)`
issued in the same breath therefore reaches an **HWND that is still hidden**, and
Windows discards the internal paint. The window is then mapped by the executor a
moment later with an empty update region: no `WM_PAINT`, no `RedrawRequested`,
`pending_reveal` stays true, and the surface stays **cloaked** — mapped, logically
visible, and invisible to the user.

Measured 2026-10-28 (2nd and later shows of an existing window):

```
[  5596.80ms] nui[mixer] SHOW existed=true logical_was=false os_visible=false cloaked=true frames=1
[  5600.62ms] nui[mixer]   set_visible(true) -> cloak=true os_visible=true
[  5601.35ms] nui[mixer]   strip_frame_styles -> cloak=true os_visible=true
       (no reveal DONE, frames stays 1 — for 1.8 s and beyond)
[  6000.39ms] STALL reveal-stall mixer[log=1 os=1 cloak=1 pend=403ms f=1] …
[  7084.52ms] STALL reveal-stall mixer[log=1 os=1 cloak=1 pend=1475ms f=1] …
```

Fresh windows are unaffected (the window is created visible=false and the first
mapping flushes the queue with a dirty region), which is why it looked random.

**Fix in place (kept, not just logged):**

1. `about_to_wait` treats a pending reveal as "animating", so the loop is awake
   while a show is in flight instead of sleeping in `ControlFlow::Wait`.
   `request_redraw_all()` then re-requests the paint **after** the executor has
   mapped the window — measured `reveal DONE wait=21ms`.
2. `WindowManager::retry_pending_reveals(25 ms)` — a bounded (12 attempts) retry
   that logs `reveal RETRY …` so a lost paint can never again be silent.
3. `Renderer::request_reveal` also `InvalidateRect`s the client area, because a
   re-mapped window can have an empty update region.

Verified: 3 × 6-show/6-hide probe rounds, `stalls=0`, at most one logged retry.

## 4. Root cause of both remaining symptoms: the focus-change key re-delivery

**Found in a real field trace 2026-10-28** (26 s of continuous toggling from one
key press). Windows re-delivers `WM_KEYDOWN` to the window that gains focus while
a key is held, and **that delivery carries no repeat flag**, so `event.repeat`
cannot see it. Every shortcut that shows a window moves focus to it — so the
re-delivery fires the same shortcut again, hides the window, moves focus back, and
fires again:

```
3950.56 key 's' Pressed  win=main     osfg=main      ← the user's one press
3950.92 global 's' -> ToggleWindow("settings")
3951.13 nui[settings] SHOW …                          ← focus moves to settings
3957.34 key 's' Released win=main     osfg=settings   ← release still goes to main
3957.41 key 's' Pressed  win=settings osfg=settings   ← RE-DELIVERED, repeat=false
3957.44 DUP-PRESS 's' after 7ms (no repeat flag) win=settings
3957.69 toggle settings logical=true -> false          ← hides again
3957.74 BOUNCE settings toggled twice in 7ms
3960.31 restore-focus after=settings -> main
3960.71 key 's' Pressed  win=main     osfg=main       ← and back …
3967.05 … repeats at ~3 presses/second for as long as the key is held
```

Symptom mapping: *"hides but instantly reappears"* = the re-delivery re-showing the
window; *"several presses do nothing"* = the storm leaving the window hidden, and
the same key re-delivering to whichever window it lands on. Holding any global key
(theme `x` too) while this runs flaps that setting as well.

**Fix:** the global pass now remembers which physical keys were *held* when it acted
(`GetAsyncKeyState`, the same OS-truth doctrine as the modifier fix) and refuses a
press for a key still down, logging `REPEAT-ECHO suppressed`. The entry clears as
soon as the OS reports the key up — on the next loop turn *or* on a release event,
but only when the OS confirms the key is up, because the storm also produces a
synthetic *release* to the window that just lost focus. The user-visible behaviour
is unchanged: one press, one toggle; holding the key toggles once, not thirty
times a second.

## 5. Remaining suspect, not yet observed in a trace

`nui::focus_window_now` and `Renderer::focus_window_fast` both call
`SetWindowPos(… SWP_SHOWWINDOW)`. `SWP_SHOWWINDOW` **maps** the target, so any focus
call on a window that was just cloaked re-shows the HWND while the cloak keeps it
off-screen. Call sites are `restore_focus_after_hide` (picks only a *visible*
target) and `reveal_and_focus_main` (main only), so this is a latent hazard rather
than a live bug. The `nui focus_now … mapped=…->…` line exists to catch it.

## 6. Reproducing deterministically

`scripts/toggle-probe.ps1` launches the release build with `--silence --static`,
finds the `SoundApp` HWND, and posts real `WM_KEYDOWN`/`WM_KEYUP` to it (no focus
stealing, no `SendKeys` into whatever window happens to be foreground), then copies
`event_log.txt` to `target/probe_log.txt` and prints per-run counts
(toggles / reveals / retries / stalls).

**Keep in mind:** synthetic presses are exactly why the field bug hid for so long.
The probe verifies the *plumbing*; it cannot reproduce focus loss, Alt-Tab, the
tray, or a real keyboard's delivery. Field traces from **alphatap** are the real test.
