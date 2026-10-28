// SoundApp — native audio player built on the NUI Rust dual-backend renderer
// (GPU instanced SDF + CPU Softbuffer), nui_wc2 v3.0.0 "slate" theme and Swash typography.
//
// GUI subsystem: no console window ever opens with the app. When launched from
// a terminal, `attach_parent_console()` reroutes harness output back to it.

#![windows_subsystem = "windows"]

mod audio;
mod config;
mod dsp;
mod midi;
mod mixer;
mod perf;
mod single_instance;
mod tracker;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

use audio::TrackMetadata;
use audio::{Audio, PlaybackParams, Source};
use config::UserConfig;
use dsp::{DspAnalyzer, DspSnapshot, BANDS};
use nui::{
    rgb, rgba, BackendKind, ButtonVariant, FontEngine, FontFace, KeyCmd, Length, Rect, Renderer,
    Scene, TextSystem, Theme, Ui, UiState, WindowManager, WindowSpec, ICON_ADD, ICON_CHECK,
    ICON_CLOSE, ICON_EQ, ICON_LOOP, ICON_NEXT, ICON_PAUSE, ICON_PLAY, ICON_PREV, ICON_RESTART,
    ICON_LOCK, ICON_SEARCH, ICON_SETTINGS, ICON_SHUFFLE, ICON_TUNE, ICON_VOLUME,
};

/// One channel strip in the Phase 3.4 multitrack preview mixer.
#[derive(Clone, Debug)]
struct MixerStem {
    /// Display number (`01`..`20`).
    number: usize,
    /// Stable engine slot; UI removal never renumbers another stem's audio.
    slot: usize,
    path: Option<PathBuf>,
    /// Fader gain, `0.0..=2.0` (`1.0` = unity).
    gain: f32,
    /// Stereo pan, `-1.0` = hard left .. `+1.0` = hard right.
    pan: f32,
    muted: bool,
    solo: bool,
}

/// The Parameters window's playback-parameter state — one value per control,
/// split by engine so the dimmed engine card keeps its stored values while the
/// other engine is active (matching the reference Electron Parameters window).
#[derive(Clone, Copy, Debug, PartialEq)]
struct ParamState {
    /// `true` = Tape engine (resampling couples pitch & tempo); `false` = Rubber
    /// Band Pitch/Time (independent pitch / tempo / formants).
    tape: bool,
    /// Tape speed in semitones (`-12..=12`, `0` = normal), always a whole
    /// number — musical steps, as in the reference app. Tape physics: pitch
    /// and tempo move together — `+12 st` plays at double speed.
    tape_speed_st: f32,
    /// Pitch/Time pitch shift in semitones (`-12..=12`), always a whole number.
    pitch_st: f32,
    /// Pitch/Time time stretch as a speed factor (`0.5..=1.5`), quantized to
    /// whole percent (`1.0` = `100%`).
    stretch: f32,
    /// Pitch/Time formant preservation.
    formant: bool,
    /// Native tracker (libopenmpt) pitch in whole semitones (`-12..=12`,
    /// `0` = neutral). Active only while a tracker module plays; the event
    /// stream is unaffected — samples transpose, speed does not.
    trk_pitch_st: f32,
    /// Native tracker tempo in whole percent (`50..=150`, `100` = neutral).
    /// Event-domain: rows and envelopes play faster/slower, samples keep pitch.
    trk_tempo_pct: f32,
    /// Native mixer stereo separation, `0..=200` percent (`100` = libopenmpt
    /// default; `0` = mono).
    trk_separation: f32,
    /// Native mixer interpolation filter.
    trk_interp: tracker::Interpolation,
    /// Native MIDI (FluidSynth) transpose in whole semitones (`-12..=12`,
    /// `0` = neutral). Applies live at the note-on/off hook — never restarts,
    /// percussion channel 9 skips.
    midi_transpose_st: f32,
    /// Native MIDI tempo override in whole BPM (`40..=240`, default 120).
    /// Applied only while `midi_bpm_override` is on; otherwise the file's
    /// own tempo map rules.
    midi_bpm: f32,
    /// `true` = override the file's tempo with `midi_bpm` (absolute BPM).
    midi_bpm_override: bool,
    /// Native MIDI metronome click mixed over the rendered audio.
    midi_metronome: bool,
}

impl Default for ParamState {
    /// New-track defaults — matches the reference `DEFAULTS.audio`: tape engine,
    /// neutral speed / pitch / stretch, formants off. Native tracker and MIDI
    /// controls are neutral too; native sources ignore the shared audio set.
    fn default() -> Self {
        Self {
            tape: true,
            tape_speed_st: 0.0,
            pitch_st: 0.0,
            stretch: 1.0,
            formant: false,
            trk_pitch_st: 0.0,
            trk_tempo_pct: 100.0,
            trk_separation: 100.0,
            trk_interp: tracker::Interpolation::Default,
            midi_transpose_st: 0.0,
            midi_bpm: 120.0,
            midi_bpm_override: false,
            midi_metronome: false,
        }
    }
}

impl ParamState {
    /// Tape speed as a resample ratio (exponential: `+12 st` = `2.0x`).
    fn tape_ratio(&self) -> f32 {
        2.0f32.powf(self.tape_speed_st / 12.0)
    }

    /// The engine parameter set. The inactive engine's sliders are inert: tape
    /// mode carries only the tape speed (pitch is the resample coupling), and
    /// Pitch/Time mode ignores the tape slider. The native tracker set rides
    /// along in every push; the engine applies it only to a tracker source.
    fn engine_params(&self, hq: bool) -> PlaybackParams {
        let (tempo_ratio, pitch_semitones) = if self.tape {
            (self.tape_ratio(), 0.0)
        } else {
            (self.stretch, self.pitch_st)
        };
        PlaybackParams {
            mode_tape: self.tape,
            pitch_semitones,
            tempo_ratio,
            hq,
            formant_preserve: self.formant,
            tracker: tracker::TrackerParams {
                pitch_semitones: self.trk_pitch_st,
                tempo_ratio: self.trk_tempo_pct / 100.0,
                stereo_separation: self.trk_separation,
                interpolation: self.trk_interp,
            },
            midi: midi::MidiParams {
                transpose_semitones: self.midi_transpose_st as i32,
                bpm_override: if self.midi_bpm_override {
                    Some(self.midi_bpm)
                } else {
                    None
                },
                metronome: self.midi_metronome,
            },
        }
    }

    /// Native tracker parameters reset to neutral (per-card Reset button).
    fn reset_tracker(&mut self) {
        self.trk_pitch_st = 0.0;
        self.trk_tempo_pct = 100.0;
        self.trk_separation = 100.0;
        self.trk_interp = tracker::Interpolation::Default;
    }

    /// Native MIDI parameters reset to neutral (per-card Reset button).
    fn reset_midi(&mut self) {
        self.midi_transpose_st = 0.0;
        self.midi_bpm = 120.0;
        self.midi_bpm_override = false;
        self.midi_metronome = false;
    }
}

/// JS `Math.round`, which the reference app rounds every slider value with:
/// exact halves go **up**, not away from zero (`Math.round(-2.5) === -2`).
/// Rust's `f32::round` would give `-3` and land a whole semitone low on
/// negative halves, so the rounding mode is ported explicitly.
fn js_round(v: f32) -> f32 {
    (v + 0.5).floor()
}

/// Musical quantization for the semitone sliders, matching the reference
/// Electron app: `js/parameters/main.js` rounds every slider value before
/// displaying it AND before sending it to the engine (`applyTapeSpeed` in
/// `js/engines.js` re-integerizes with `| 0`), so whole semitones are the
/// applied unit — not just the display unit. The reference has no sub-semitone
/// adjustment: one semitone is the minimum move. `== 0.0` also normalizes
/// `-0.0` so the `+0` readout never renders as `-0` (JS `-0 >= 0` is true,
/// Rust prints `-0`).
fn snap_semitones(v: f32) -> f32 {
    snap_semitones_range(v, -12.0, 12.0)
}

/// `snap_semitones` for a different clamp range — the native tracker pitch
/// slider spans libopenmpt's full ±24 semitones.
fn snap_semitones_range(v: f32, min: f32, max: f32) -> f32 {
    let r = js_round(v).clamp(min, max);
    if r == 0.0 {
        0.0
    } else {
        r
    }
}

/// Whole-percent quantization for the unipolar tracker sliders (tempo,
/// separation): one percent is the visible and applied unit.
fn snap_percent(v: f32, min: f32, max: f32) -> f32 {
    js_round(v).clamp(min, max)
}

/// Time-stretch quantization to whole percent: the reference slider spans
/// `0.5..1.5` and displays `Math.round(v * 100)`, so 1% is the visible unit.
fn snap_stretch(v: f32) -> f32 {
    js_round(v * 100.0).clamp(50.0, 150.0) / 100.0
}

/// One wheel detent: `prev` advanced by exactly one visible unit, clamped to
/// the slider range. `Ui::slider`'s own detent is 2.5% of the *range* (0.6 st
/// on the ±12 sliders, 2.5% of the ratio slider), which lands between grid
/// points and then alternates between one and two units once snapped. The
/// reference app has no sub-unit adjustment at all, so a detent must move one
/// unit — whole semitone, whole percent.
fn wheel_step(prev: f32, wheel_y: f32, unit: f32, min: f32, max: f32) -> f32 {
    (prev + unit * wheel_y.signum()).clamp(min, max)
}

/// `Ui::slider` with the detent replaced by `wheel_step` (see there). Drag,
/// double-click / right-click reset and every visual stay the widget's; only a
/// wheel notch is re-based on the unit grid. Callers snap afterwards and
/// compare against their pre-call value so a detent at the range end pushes
/// nothing to the engine.
#[allow(clippy::too_many_arguments)]
fn slider_one_unit(
    ui: &mut Ui,
    id: u32,
    r: Rect,
    val: &mut f32,
    min: f32,
    max: f32,
    default_val: f32,
    unit: f32,
) -> bool {
    let prev = *val;
    let changed = ui.slider(id, r, "", val, min, max, default_val, "");
    if changed && ui.state.wheel_y.abs() > 0.01 {
        *val = wheel_step(prev, ui.state.wheel_y, unit, min, max);
    }
    changed
}

struct App {
    start: Instant,
    reported: bool,
    animate: bool,
    quit_at: Option<Instant>,
    /// Backend requested on the command line. By default the player and static
    /// utility windows use the CPU renderer; Monitoring and Mixer use the GPU.
    /// `--backend` locks this value for every window (benchmark/debug override).
    prefer_backend: BackendKind,
    backend_locked: bool,
    font_engine: FontEngine,
    dark_mode: bool,
    ui_zoom: f32,
    screenshot_path: Option<String>,
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    scene: Scene,
    text_sys: TextSystem,
    ui_state: UiState,
    modifiers: ModifiersState,
    size: (u32, u32),
    audio: Option<Audio>,
    audio_reported: bool,
    t: f32,
    frame_ms: u64,
    nopaint: bool,
    next_frame: Instant,
    quitting: bool,
    /// Pending interactive open: (request instant, file label) until the
    /// decoder publishes the track's metadata — the file-open measurement.
    open_pending: Option<(Instant, String)>,
    /// Frame pacing (UI responsiveness): frames per 10 s window and how many
    /// intervals missed the 15 Hz budget (>66 ms). Gaps >500 ms are a fresh
    /// burst after idle, not stalls, and are excluded.
    last_frame_at: Option<Instant>,
    pace_at: Instant,
    pace_frames: u32,
    pace_slow: u32,

    // Phase 1.5 multi-window: secondary windows sharing `text_sys`, plus queued
    // show/hide toggles executed in `window_event` where `event_loop` is available.
    secondary_windows: WindowManager,
    window_geom: std::collections::HashMap<String, SavedGeom>,
    /// Startup directory for the implicit playlist (empty = OS Music folder).
    default_dir: String,
    /// Cover pixels currently resting in the scene's image slot (`None` = the
    /// procedural default tile is showing). Updated per frame during a fade so
    /// a track change mid-transition starts from what is on screen.
    cover_resting: Option<(u32, u32, Vec<u8>)>,
    /// Active cover crossfade, if any.
    cover_fade: Option<CoverFade>,
    pending_window_toggles: Vec<&'static str>,
    /// MRU focus ring (`"main"` or a secondary key). Hiding the focused window
    /// restores focus to the previous visible entry, so keyboard shortcuts never
    /// fall into a focus void.
    focus_ring: VecDeque<&'static str>,
    /// File/focus requests from later process launches via the single-instance pipe.
    open_requests: Option<Receiver<String>>,
    open_window_spec: Option<&'static WindowSpec>,
    /// Last content-driven Parameters window height requested (physical px).
    parameters_window_height: Option<f32>,

    // Interactive player states
    /// Playback parameters shown in the Parameters window (engine mode + the
    /// per-engine sliders). See `ParamState`.
    params: ParamState,
    sw_hq_mode: bool,
    sw_keep_tray: bool,
    audio_devices: Vec<String>,
    selected_device_idx: usize,
    master_vol: f32,
    chk_locked: bool,
    scrub_frac: f32,
    selected_track: usize,
    /// Master volume in `0.0..=2.0` (`1.0` = unity); persisted in `user.json`.
    pub show_controls: bool,

    // MIDI source engine — SoundFonts available to the Parameters page and
    // the selection index (persisted as `selected_soundfont`).
    soundfonts: Vec<PathBuf>,
    selected_soundfont: usize,
    /// Tracker channel solo/mute state; cleared for every new module.
    tracker_mutes: Vec<bool>,

    // Phase 3.1 — player transport state (loop / shuffle).
    pub loop_enabled: bool,
    pub shuffle_enabled: bool,
    /// Fisher-Yates playback order while shuffle is on; entries index
    /// `playlist`. Empty while shuffle is off.
    shuffle_order: Vec<usize>,
    /// Played-track history under shuffle (indices into `playlist`) for Prev.
    shuffle_history: VecDeque<usize>,
    // Phase 2.1/2.2 — real playlist + FFmpeg metadata shown in the Left Card.
    /// Tracks discovered on disk — the implicit folder playlist.
    playlist: Vec<PathBuf>,
    /// Metadata of the currently-loaded file (from FFmpeg).
    track_meta: Option<TrackMetadata>,
    /// Background directory scanner so SMB `read_dir` / `is_dir` never touches the UI thread.
    scan_tx: std::sync::mpsc::Sender<(u64, ScanJob)>,
    scan_rx: std::sync::mpsc::Receiver<(u64, ScanOutcome)>,
    scan_gen: u64,

    // Phase 3.3 — real-time DSP analysis tap + last snapshot for the Monitoring window.
    dsp: DspAnalyzer,
    dsp_buf: Vec<(f32, f32)>,
    dsp_snap: DspSnapshot,
    /// Output-frame counter at the last Monitoring redraw (delta = new frames).
    last_dsp_frames: u64,

    // Phase 3.4 — independent multitrack stem preview mixer.
    mixer_audio: Option<mixer::MixerAudio>,
    mixer_stems: Vec<MixerStem>,
    mixer_playing: bool,
    mixer_loop: bool,
    mixer_master_vol: f32,
    mixer_pos_frac: f32,
    mixer_strip_targets: Vec<(Rect, usize)>,
    mixer_add_target: Option<Rect>,
    mixer_sync_debug: bool,
    mixer_sync_copied_at: Option<Instant>,
    mixer_sync_close_rect: Option<Rect>,
    mixer_sync_copy_rect: Option<Rect>,

    // Phase 3.5 — persistent user configuration.
    config: UserConfig,
    /// Set true when the Settings "Save Configuration" button fires, flushed after
    /// the render borrow ends.
    save_config_req: bool,
    /// Set true when the Settings "Show transport controls" checkbox fires; the
    /// resize is applied after the render borrow ends.
    apply_zoom_req: bool,
    /// Transient status text shown in the Settings footer after a save.
    config_status: String,
}

/// Phase 1.5 secondary windows (SoundApp `P` / `N` / `M` / `S` / `H`), all undecorated
/// like the primary window and managed by `nui::WindowManager`.
const SPEC_PARAMETERS: WindowSpec = WindowSpec {
    key: "parameters",
    title: "SoundApp · Parameters [P]",
    // Initial fallback only; the first layout replaces this with measured content height.
    width: 510.0,
    height: 800.0,
    min_width: 480.0,
    min_height: 600.0,
    undecorated: true,
};

const SPEC_MONITORING: WindowSpec = WindowSpec {
    key: "monitoring",
    title: "SoundApp · Real-Time DSP Monitoring [N]",
    width: 900.0,
    height: 600.0,
    min_width: 620.0,
    min_height: 420.0,
    undecorated: true,
};

const SPEC_MIXER: WindowSpec = WindowSpec {
    key: "mixer",
    title: "SoundApp · Multi-Track Stem Mixer [M]",
    width: 960.0,
    height: 560.0,
    min_width: 720.0,
    min_height: 420.0,
    undecorated: true,
};

const SPEC_SETTINGS: WindowSpec = WindowSpec {
    key: "settings",
    title: "SoundApp · Settings [S]",
    width: 620.0,
    height: 420.0,
    min_width: 480.0,
    min_height: 320.0,
    undecorated: true,
};

const SPEC_HELP: WindowSpec = WindowSpec {
    key: "help",
    title: "SoundApp · Keyboard Shortcuts & Help [H]",
    width: 720.0,
    height: 480.0,
    min_width: 520.0,
    min_height: 360.0,
    undecorated: true,
};

fn window_spec_by_key(key: &str) -> Option<&'static WindowSpec> {
    match key {
        "parameters" => Some(&SPEC_PARAMETERS),
        "monitoring" => Some(&SPEC_MONITORING),
        "mixer" => Some(&SPEC_MIXER),
        "settings" => Some(&SPEC_SETTINGS),
        "help" => Some(&SPEC_HELP),
        _ => None,
    }
}

/// Material icon shown in a secondary window's header.
fn window_header_icon(key: &str) -> &'static str {
    match key {
        "parameters" => ICON_TUNE,
        "monitoring" => ICON_VOLUME,
        "mixer" => ICON_EQ,
        "settings" => ICON_SETTINGS,
        _ => ICON_SEARCH,
    }
}

/// Mixer strips are created only for real media paths; no synthetic demo audio.
fn default_mixer_stems() -> Vec<MixerStem> {
    Vec::new()
}

/// Deterministic synthetic stereo signal used by the Monitoring window when the
/// audio tap has no frames yet (silent / headless), so the DSP panels stay alive.
fn synth_dsp_frames(t: f32, n: usize) -> Vec<(f32, f32)> {
    (0..n)
        .map(|i| {
            let x = i as f32 / n as f32;
            let ph = t * 3.0 + x * std::f32::consts::TAU * 4.0;
            let amp = 0.35 * (0.6 + 0.4 * (t * 0.7).sin());
            let l = amp * (ph.sin() + 0.4 * (ph * 2.7).sin() + 0.2 * (ph * 5.3).cos());
            let r = amp * (ph.sin() * 0.93 + 0.4 * (ph * 2.7 + 0.3).sin());
            (l.clamp(-1.0, 1.0), r.clamp(-1.0, 1.0))
        })
        .collect()
}

/// Keyboard-shortcut rows for the Help window's two-column table.
const HELP_ROWS: &[(&str, &str)] = &[
    ("Space", "Play / Pause transport"),
    ("Left / Right", "Previous / next track"),
    ("Ctrl+Left / Right", "Seek -10 s / +10 s"),
    ("Up / Down", "Master volume +5% / -5%"),
    ("L", "Toggle loop"),
    ("C", "Show / hide transport controls"),
    ("P", "Parameters window"),
    ("N", "Monitoring window"),
    ("M", "Mixer window"),
    ("S", "Settings window"),
    ("H", "This Help window"),
    ("I", "Show current file in Explorer"),
    ("F", "Toggle Swash / Fontdue engine"),
    ("X", "Toggle dark / light theme"),
    ("Ctrl + / - / 0", "UI zoom in / out / reset"),
    ("Ctrl+C / X / V", "Copy / Cut / Paste text"),
    ("Tab / Shift+Tab", "Move keyboard focus"),
    ("Dbl-Click / Right-Click", "Reset slider / fader to default"),
    ("Esc", "Close window / quit app"),
];

fn window_toggle_for_key(ch: &str) -> Option<&'static str> {
    match ch.chars().next()?.to_ascii_lowercase() {
        'p' => Some("parameters"),
        'n' => Some("monitoring"),
        'm' => Some("mixer"),
        's' => Some("settings"),
        'h' => Some("help"),
        _ => None,
    }
}

/// A key that works regardless of which SoundApp window is focused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum GlobalAction {
    ToggleWindow(&'static str),
    ToggleTheme,
    ToggleControls,
    RevealInExplorer,
}

/// Pure classification for the global key pass — unit-testable without winit.
fn global_action_for(ch: &str) -> Option<GlobalAction> {
    if let Some(key) = window_toggle_for_key(ch) {
        return Some(GlobalAction::ToggleWindow(key));
    }
    match ch.chars().next()?.to_ascii_lowercase() {
        'x' | 't' => Some(GlobalAction::ToggleTheme),
        'c' => Some(GlobalAction::ToggleControls),
        'i' => Some(GlobalAction::RevealInExplorer),
        _ => None,
    }
}

/// Open the OS file browser with `path` selected. `explorer.exe /select` needs
/// `/select,"<path>"` as ONE raw argument — std's default MSVC quoting escapes
/// the embedded quotes and Explorer then opens the folder without selecting.
/// spawn() only: Explorer hands off to the running shell process and a
/// blocking wait could hang. Falls back to opening the parent folder.
#[cfg(windows)]
fn reveal_in_explorer(path: &std::path::Path) {
    use std::os::windows::process::CommandExt;
    let raw = format!("/select,\"{}\"", path.display());
    if std::process::Command::new("explorer.exe")
        .raw_arg(raw)
        .spawn()
        .is_err()
    {
        eprintln!("[app] explorer /select failed; opening parent folder");
        if let Some(dir) = path.parent() {
            let _ = std::process::Command::new("explorer.exe").arg(dir).spawn();
        }
    }
}

#[cfg(not(windows))]
fn reveal_in_explorer(_path: &std::path::Path) {}

/// The OS "Music" known folder (FOLDERID_Music) — hand FFI in the
/// single_instance.rs pattern, no new dependencies.
#[cfg(windows)]
fn known_music_dir() -> Option<std::path::PathBuf> {
    #[repr(C)]
    struct Guid {
        data1: u32,
        data2: u16,
        data3: u16,
        data4: [u8; 8],
    }
    #[link(name = "shell32")]
    extern "system" {
        fn SHGetKnownFolderPath(
            rfid: *const Guid,
            flags: u32,
            token: *mut std::ffi::c_void,
            ppsz: *mut *mut u16,
        ) -> i32;
    }
    #[link(name = "ole32")]
    extern "system" {
        fn CoTaskMemFree(pv: *mut std::ffi::c_void);
    }
    // FOLDERID_Music {4BD8D571-6D19-48D3-BE97-03385F96FC70}
    let music = Guid {
        data1: 0x4BD8D571,
        data2: 0x6D19,
        data3: 0x48D3,
        data4: [0xBE, 0x97, 0x03, 0x38, 0x5F, 0x96, 0xFC, 0x70],
    };
    let mut ppsz: *mut u16 = std::ptr::null_mut();
    if unsafe { SHGetKnownFolderPath(&music, 0, std::ptr::null_mut(), &mut ppsz) } != 0 {
        return None;
    }
    unsafe {
        let len = (0..).take_while(|&i| *ppsz.add(i) != 0).count();
        let s = String::from_utf16_lossy(std::slice::from_raw_parts(ppsz, len));
        CoTaskMemFree(ppsz as *mut std::ffi::c_void);
        (!s.is_empty()).then(|| std::path::PathBuf::from(s))
    }
}

#[cfg(not(windows))]
fn known_music_dir() -> Option<std::path::PathBuf> {
    None
}

/// First supported audio file directly inside `dir` (name-sorted). The startup
/// track for the default-directory feature: its implicit sibling-folder
/// playlist then behaves exactly like any dropped file (flat, not recursive —
/// recursive folder scans stay available via an explicit folder drop).
fn first_audio_in_dir(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file() && is_supported_audio(p))
        .collect();
    files.sort();
    files.into_iter().next()
}

/// Cover images and the default tile are square RGBA of this side (the decode
/// bridge caps embedded art at 128).
const COVER_TILE_SIDE: u32 = 128;
/// Duration of one cover crossfade.
const COVER_FADE_SECS: f32 = 0.3;

/// One cover transition: the outgoing view (`None` = default tile) fading
/// into the incoming view over [`COVER_FADE_SECS`].
struct CoverFade {
    old: Option<(u32, u32, Vec<u8>)>,
    new: Option<(u32, u32, Vec<u8>)>,
    started: Instant,
}

/// Solid default-tile RGBA buffer (the has-track fallback blue).
fn cover_tile_rgba() -> Vec<u8> {
    let px = (COVER_TILE_SIDE as usize) * (COVER_TILE_SIDE as usize);
    let mut v = Vec::with_capacity(px * 4);
    for _ in 0..px {
        v.extend_from_slice(&[34, 150, 195, 255]);
    }
    v
}

/// Straight-alpha crossfade of two 128x128 RGBA views; a `None` side blends
/// the solid default-tile color. Mismatched sizes degrade to the tile (the
/// decode bridge caps covers at 128, so this is a guard, not an expectation).
fn blend_covers(
    old: Option<&(u32, u32, Vec<u8>)>,
    new: Option<&(u32, u32, Vec<u8>)>,
    t: f32,
) -> Vec<u8> {
    let side = COVER_TILE_SIDE as usize;
    let len = side * side * 4;
    let tile = cover_tile_rgba();
    let a: &[u8] = match old {
        Some((w, h, d)) if *w as usize == side && *h as usize == side && d.len() == len => d,
        _ => &tile,
    };
    let b: &[u8] = match new {
        Some((w, h, d)) if *w as usize == side && *h as usize == side && d.len() == len => d,
        _ => &tile,
    };
    let t = t.clamp(0.0, 1.0);
    let mut out = Vec::with_capacity(len);
    for i in 0..len {
        out.push((a[i] as f32 * (1.0 - t) + b[i] as f32 * t).round() as u8);
    }
    out
}

/// Persisted window placement (logical units) parsed from
/// `UserConfig::window_geometry`.
#[derive(Clone, Copy, Debug, PartialEq)]
struct SavedGeom {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    visible: bool,
}

/// Parse `key x y w h visible;key ...`. Unknown/garbage entries are skipped,
/// so a hand-edited file degrades to default placement.
fn parse_window_geometry(s: &str) -> std::collections::HashMap<String, SavedGeom> {
    let mut map = std::collections::HashMap::new();
    for entry in s.split(';') {
        let mut it = entry.split_whitespace();
        let (Some(key), Some(x), Some(y), Some(w), Some(h), Some(v)) = (
            it.next(),
            it.next(),
            it.next(),
            it.next(),
            it.next(),
            it.next(),
        ) else {
            continue;
        };
        let (Ok(x), Ok(y), Ok(w), Ok(h)) = (
            x.parse::<f32>(),
            y.parse::<f32>(),
            w.parse::<f32>(),
            h.parse::<f32>(),
        ) else {
            continue;
        };
        map.insert(
            key.to_string(),
            SavedGeom {
                x,
                y,
                w,
                h,
                visible: v == "1",
            },
        );
    }
    map
}

// ---------------------------------------------------------------------------
// Small draw helpers shared by the Monitoring / Mixer windows
// ---------------------------------------------------------------------------

/// Draw `s` horizontally centered inside `r`, vertically centered on its height.
fn draw_centered_text(
    ui: &mut Ui,
    r: Rect,
    s: &str,
    face: FontFace,
    fs: f32,
    color: (u8, u8, u8, u8),
) {
    let fs = fs.round();
    let w = ui.text_sys.measure(face, ui.font_engine, s, fs);
    let x = (r.x + (r.w - w) * 0.5).round();
    let baseline = (r.y + r.h * 0.5 + fs * 0.34).round();
    ui.scene
        .text(ui.text_sys, face, ui.font_engine, s, x, baseline, fs, color);
}

/// Draw `s` centered horizontally on `cx` with an exact `baseline`.
fn draw_text_at(
    ui: &mut Ui,
    cx: f32,
    baseline: f32,
    s: &str,
    face: FontFace,
    fs: f32,
    color: (u8, u8, u8, u8),
) {
    let fs = fs.round();
    let w = ui.text_sys.measure(face, ui.font_engine, s, fs);
    ui.scene.text(
        ui.text_sys,
        face,
        ui.font_engine,
        s,
        (cx - w * 0.5).round(),
        baseline.round(),
        fs,
        color,
    );
}

/// A dark `inset_card`-style readout box with an uppercase label and a value.
fn draw_readout(ui: &mut Ui, r: Rect, label: &str, value: &str) {
    let t = ui.theme;
    ui.scene
        .rounded_rect(r.x, r.y, r.w, r.h, t.radius2(), t.base, t.border1, 1.0);
    let pad = (t.rem * 0.4).round();
    let lx = (r.x + pad).round();
    let lfs = t.font_xsmall();
    ui.scene.text(
        ui.text_sys,
        FontFace::Regular,
        ui.font_engine,
        label,
        lx,
        (r.y + pad + lfs * 0.85).round(),
        lfs,
        t.text_muted,
    );
    let vfs = t.font_small();
    ui.scene.text(
        ui.text_sys,
        FontFace::SemiBold,
        ui.font_engine,
        value,
        lx,
        (r.y + r.h - pad - vfs * 0.15).round(),
        vfs,
        t.text,
    );
}

/// One color segment of a VU meter, limited to the region above the `top` fill line.
fn meter_segment(ui: &mut Ui, r: Rect, top: f32, lo_db: f32, hi_db: f32, color: (u8, u8, u8, u8)) {
    let db_to_y = |db: f32| r.y + r.h * (1.0 - ((db + 60.0) / 60.0).clamp(0.0, 1.0));
    let y_start = db_to_y(hi_db).max(top);
    let y_end = db_to_y(lo_db);
    if y_end > y_start + 0.5 {
        ui.scene.rect(
            r.x + 1.0,
            y_start,
            (r.w - 2.0).max(1.0),
            y_end - y_start,
            color,
        );
    }
}

/// Vertical L/R VU meter: dark slot, green/yellow/red fill, white peak tick, label.
fn draw_vu_meter(ui: &mut Ui, r: Rect, lvl_db: f32, peak_db: f32, label: &str) {
    let t = ui.theme;
    ui.scene
        .rounded_rect(r.x, r.y, r.w, r.h, 2.0, t.base, t.border1, 1.0);
    let top = r.y + r.h * (1.0 - ((lvl_db + 60.0) / 60.0).clamp(0.0, 1.0));
    meter_segment(ui, r, top, -60.0, -12.0, t.palette_activate);
    meter_segment(ui, r, top, -12.0, -3.0, t.palette_mark);
    meter_segment(ui, r, top, -3.0, 0.0, t.palette_alert);
    let py = (r.y + r.h * (1.0 - ((peak_db + 60.0) / 60.0).clamp(0.0, 1.0))).round();
    ui.scene.rect(r.x, py, r.w, 1.5, rgb(235, 235, 235));
    let lfs = t.font_xsmall();
    ui.scene.text(
        ui.text_sys,
        FontFace::SemiBold,
        ui.font_engine,
        label,
        (r.x + 2.0).round(),
        (r.y + r.h - 3.0).round(),
        lfs,
        t.text_dim,
    );
}

/// The Mute / Solo toggle cell used by the multitrack mixer strips.
fn mixer_toggle_cell(
    ui: &mut Ui,
    r: Rect,
    id: u32,
    label: &str,
    active: bool,
    active_col: (u8, u8, u8, u8),
) -> bool {
    let t = ui.theme;
    ui.state.register_focusable(id);
    let hovered = ui.state.can_hover(id, r);
    if hovered {
        ui.state.hovered_id = Some(id);
    }
    let mut clicked = false;
    if hovered && ui.state.mouse_pressed {
        ui.state.active_id = Some(id);
        ui.state.focused_id = Some(id);
        clicked = true;
    }
    let fill = if active {
        active_col
    } else if hovered {
        t.shade3
    } else {
        t.base
    };
    ui.scene
        .rounded_rect(r.x, r.y, r.w, r.h, t.radius1(), fill, t.border1, 1.0);
    draw_centered_text(
        ui,
        r,
        label,
        FontFace::SemiBold,
        t.font_small(),
        if active { t.contrast } else { t.text_dim },
    );
    ui.draw_focus_ring(id, r, t.radius1());
    clicked
}

/// The master-volume pill (icon + horizontal track + fill/thumb).
fn master_volume_pill(ui: &mut Ui, r: Rect, vol: &mut f32, id: u32) {
    let t = ui.theme;
    ui.scene
        .rounded_rect(r.x, r.y, r.w, r.h, t.radius2(), t.base, t.border1, 1.0);
    let icon_sz = (t.rem * 1.1).round();
    ui.scene.icon_centered(
        ui.text_sys,
        ICON_VOLUME,
        r.x + t.rem * 0.7 + icon_sz * 0.5,
        r.y + r.h * 0.5,
        icon_sz,
        t.text_dim,
    );
    let track_x0 = r.x + t.rem * 2.0;
    let track_w = (r.x + r.w - t.rem * 0.6 - track_x0).max(1.0);

    ui.state.register_focusable(id);
    let hovered = ui.state.can_hover(id, r);
    if hovered {
        ui.state.hovered_id = Some(id);
        if ui.state.double_clicked || ui.state.right_pressed {
            *vol = 0.85;
        } else if ui.state.mouse_pressed {
            ui.state.active_id = Some(id);
            ui.state.focused_id = Some(id);
        }
        if ui.state.wheel_y.abs() > 0.01 {
            *vol = (*vol + 0.05 * ui.state.wheel_y.signum()).clamp(0.0, 2.0);
        }
    }
    if ui.state.active_id == Some(id) && ui.state.mouse_down {
        *vol = ((ui.state.cursor.0 - track_x0) / track_w * 2.0).clamp(0.0, 2.0);
    }

    let cy = r.y + r.h * 0.5;
    let th = (t.rem * 0.3).round().max(3.0);
    let track_bg = if t.dark {
        rgba(255, 255, 255, 38)
    } else {
        rgba(0, 0, 0, 38)
    };
    ui.scene.rounded_rect(
        track_x0,
        cy - th * 0.5,
        track_w,
        th,
        th * 0.5,
        track_bg,
        (0, 0, 0, 0),
        0.0,
    );
    let fill = track_w * (vol.clamp(0.0, 2.0) * 0.5);
    if fill > 0.5 {
        ui.scene.rounded_rect(
            track_x0,
            cy - th * 0.5,
            fill,
            th,
            th * 0.5,
            t.highlight,
            (0, 0, 0, 0),
            0.0,
        );
    }
    let thumb_d = (t.rem * 0.9).round();
    ui.scene.rounded_rect(
        track_x0 + fill - thumb_d * 0.5,
        cy - thumb_d * 0.5,
        thumb_d,
        thumb_d,
        thumb_d * 0.5,
        t.highlight,
        (0, 0, 0, 0),
        0.0,
    );
}

fn mixer_sync_snapshot_text(snapshot: &mixer::MixerSyncSnapshot, stems: &[MixerStem]) -> String {
    let mut out = format!(
        "SoundApp Mixer Sync\nrate={} transport={:.3}s epoch_frames={}\n",
        snapshot.rate,
        snapshot.position_secs,
        snapshot.transport_frames
    );
    for stem in stems {
        let Some(s) = snapshot.stems.iter().find(|s| s.slot == stem.slot) else {
            continue;
        };
        let name = stem
            .path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "-".into());
        out.push_str(&format!(
            "{:02} slot={} {:<32} ready={} muted={} drift={:+.1}ms buffer={:.1}ms underrun={:.1}ms source={:.3}s\n",
            stem.number,
            stem.slot,
            name,
            s.ready,
            s.muted,
            s.drift_frames(snapshot.transport_frames) as f64 / snapshot.rate.max(1) as f64 * 1000.0,
            s.ring_frames as f64 / snapshot.rate.max(1) as f64 * 1000.0,
            s.underrun_frames as f64 / snapshot.rate.max(1) as f64 * 1000.0,
            (s.start_frames + s.played_frames) as f64 / snapshot.rate.max(1) as f64,
        ));
    }
    out
}

fn draw_mixer_sync_debug(
    ui: &mut Ui,
    area: Rect,
    snapshot: Option<&mixer::MixerSyncSnapshot>,
    stems: &[MixerStem],
    copied: bool,
) -> (bool, bool, Rect, Rect) {
    let t = ui.theme;
    let panel_w = (area.w - t.rem).max(t.rem * 22.0).min(t.rem * 54.0);
    let panel_h = (area.h * 0.72).max(t.rem * 8.0).min(t.rem * 16.0);
    let panel = Rect::new(
        area.x + (area.w - panel_w) * 0.5,
        area.y + area.h - panel_h - t.space_half(),
        panel_w,
        panel_h,
    );
    let header_h = (t.rem * 2.2).round();
    let fs = (t.rem * 0.62).round().max(10.0);
    let row_h = (fs * 1.65).round().max(18.0);
    let pad = t.space();

    ui.scene.begin_overlay();
    ui.scene.shadow(
        panel.x,
        panel.y + 4.0,
        panel.w,
        panel.h,
        t.radius2(),
        10.0,
        rgba(0, 0, 0, 120),
    );
    ui.scene.rounded_rect(
        panel.x,
        panel.y,
        panel.w,
        panel.h,
        t.radius2(),
        t.shade1,
        t.border3,
        1.0,
    );
    ui.scene.rounded_rect(
        panel.x + 1.0,
        panel.y + 1.0,
        panel.w - 2.0,
        header_h,
        t.radius2(),
        t.shade2,
        (0, 0, 0, 0),
        0.0,
    );

    let header_base = (panel.y + header_h * 0.5 + fs * 0.35).round();
    ui.scene.text(
        ui.text_sys,
        FontFace::SemiBold,
        ui.font_engine,
        "SYNC DEBUG",
        (panel.x + pad).round(),
        header_base,
        fs,
        t.highlight,
    );
    let hint = "Ctrl+Shift+D";
    let hint_w = ui
        .text_sys
        .measure(FontFace::Regular, ui.font_engine, hint, fs);
    let close_d = (t.rem * 1.4).round();
    let copy_w = (t.rem * 5.2).round();
    let copy_r = Rect::new(
        panel.x + panel.w - pad - close_d - t.space_half() - copy_w,
        panel.y + (header_h - t.rem * 1.6) * 0.5,
        copy_w,
        (t.rem * 1.6).round(),
    );
    let mut copy_clicked = false;
    if ui.button(
        1397,
        copy_r,
        if copied { "COPIED" } else { "COPY" },
        "",
        ButtonVariant::Outline,
    ) {
        if let Some(snapshot) = snapshot {
            nui::clipboard_set_text(&mixer_sync_snapshot_text(snapshot, stems));
        }
        copy_clicked = true;
    }
    ui.scene.text(
        ui.text_sys,
        FontFace::Regular,
        ui.font_engine,
        hint,
        (copy_r.x - hint_w - t.space_half()).round(),
        header_base,
        fs,
        t.text_muted,
    );
    let close_r = Rect::new(
        panel.x + panel.w - pad - close_d,
        panel.y + (header_h - close_d) * 0.5,
        close_d,
        close_d,
    );
    let close_hot = ui.state.can_hover(1398, close_r);
    let mut close_clicked = false;
    if close_hot && ui.state.mouse_pressed {
        ui.state.mouse_pressed = false;
        close_clicked = true;
    }
    ui.scene.icon_centered(
        ui.text_sys,
        ICON_CLOSE,
        close_r.x + close_r.w * 0.5,
        close_r.y + close_r.h * 0.5,
        (t.rem * 0.9).round(),
        if close_hot { t.text } else { t.shade6 },
    );

    let (rate, transport, epoch, stems_snap) = snapshot
        .map(|s| (s.rate, s.position_secs, s.transport_frames, s.stems.as_slice()))
        .unwrap_or((0, 0.0, 0, &[]));
    let summary = format!(
        "T={:.3}s  EPOCH={:.1}ms  SR={}  N={}",
        transport,
        epoch as f64 / rate.max(1) as f64 * 1000.0,
        rate,
        stems.len()
    );
    let summary_base = (panel.y + header_h + fs * 1.15).round();
    ui.scene.text(
        ui.text_sys,
        FontFace::Regular,
        ui.font_engine,
        &summary,
        (panel.x + pad).round(),
        summary_base,
        fs,
        t.text_dim,
    );

    let col_y = (summary_base + row_h * 0.85).round();
    let x_num = panel.x + pad;
    let x_name = panel.x + t.rem * 2.0;
    let x_state = panel.x + panel.w * 0.30;
    let x_drift = panel.x + panel.w * 0.42;
    let x_buf = panel.x + panel.w * 0.58;
    let x_under = panel.x + panel.w * 0.72;
    let x_src = panel.x + panel.w * 0.86;
    ui.scene.rect(
        panel.x + 1.0,
        col_y + fs * 0.35,
        panel.w - 2.0,
        1.0,
        t.border2,
    );
    for (label, x) in [
        ("#", x_num),
        ("STEM", x_name),
        ("STATE", x_state),
        ("DRIFT", x_drift),
        ("BUF", x_buf),
        ("UND", x_under),
        ("SRC", x_src),
    ] {
        ui.scene.text(
            ui.text_sys,
            FontFace::SemiBold,
            ui.font_engine,
            label,
            x.round(),
            (col_y + fs).round(),
            fs,
            t.text_muted,
        );
    }

    let row_y0 = col_y + row_h;
    let rows_r = Rect::new(
        panel.x + 1.0,
        row_y0,
        panel.w - 2.0,
        (panel.y + panel.h - t.space_half() - row_y0).max(row_h),
    );
    let content_h = (stems.len() as f32 * row_h).max(row_h);
    ui.scroll_area(1396, rows_r, content_h, false, |ui, vp, scroll_y| {
        let first = (scroll_y / row_h).floor().max(0.0) as usize;
        let last = (((scroll_y + vp.h) / row_h).ceil() as usize + 1).min(stems.len());
        for i in first..last {
            let Some(stem) = stems.get(i) else { break };
            let y = (vp.y + i as f32 * row_h - scroll_y).round();
            if i % 2 == 0 {
                ui.scene
                    .rect(vp.x, y, vp.w, row_h, rgba(255, 255, 255, 6));
            }
            let snap = stems_snap.iter().find(|s| s.slot == stem.slot);
            let (ready, muted, drift, buf, under, src) = if let Some(s) = snap {
                (
                    s.ready,
                    s.muted,
                    s.drift_frames(epoch) as f64,
                    s.ring_frames as f64,
                    s.underrun_frames as f64,
                    (s.start_frames + s.played_frames) as f64,
                )
            } else {
                (false, false, 0.0, 0.0, 0.0, 0.0)
            };
            let rate = rate.max(1) as f64;
            let drift_ms = drift / rate * 1000.0;
            let buf_ms = buf / rate * 1000.0;
            let under_ms = under / rate * 1000.0;
            let src_s = src / rate;
            let state = if snap.map(|s| s.enabled).unwrap_or(false) {
                if !ready {
                    "PRE"
                } else if muted {
                    "MUTE"
                } else {
                    "PLAY"
                }
            } else {
                "LOAD"
            };
            let base = (y + row_h * 0.68).round();
            let name = stem
                .path
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "-".into());
            let drift_color = if drift_ms.abs() <= 2.0 {
                t.palette_activate
            } else if drift_ms.abs() <= 10.0 {
                t.palette_mark
            } else {
                t.palette_alert
            };
            ui.scene.text(
                ui.text_sys,
                FontFace::Regular,
                ui.font_engine,
                &format!("{:02}", stem.number),
                x_num.round(),
                base,
                fs,
                t.text_muted,
            );
            ui.text_clipped(
                FontFace::Regular,
                ui.font_engine,
                &name,
                x_name.round(),
                base,
                fs,
                (x_state - x_name - t.space_half()).max(0.0),
                t.text,
            );
            ui.scene.text(
                ui.text_sys,
                FontFace::Regular,
                ui.font_engine,
                state,
                x_state.round(),
                base,
                fs,
                if state == "PLAY" {
                    t.palette_activate
                } else if state == "MUTE" {
                    t.palette_alert
                } else {
                    t.palette_mark
                },
            );
            ui.scene.text(
                ui.text_sys,
                FontFace::SemiBold,
                ui.font_engine,
                &format!("{drift_ms:+.1}ms"),
                x_drift.round(),
                base,
                fs,
                drift_color,
            );
            ui.scene.text(
                ui.text_sys,
                FontFace::Regular,
                ui.font_engine,
                &format!("{buf_ms:.0}ms"),
                x_buf.round(),
                base,
                fs,
                if buf_ms < 20.0 {
                    t.palette_alert
                } else {
                    t.text_dim
                },
            );
            ui.scene.text(
                ui.text_sys,
                FontFace::Regular,
                ui.font_engine,
                &format!("{under_ms:.1}ms"),
                x_under.round(),
                base,
                fs,
                if under_ms > 0.0 {
                    t.palette_alert
                } else {
                    t.text_dim
                },
            );
            ui.scene.text(
                ui.text_sys,
                FontFace::Regular,
                ui.font_engine,
                &format!("{src_s:.2}s"),
                x_src.round(),
                base,
                fs,
                t.text_dim,
            );
        }
    });
    ui.scene.end_overlay();
    (copy_clicked, close_clicked, close_r, copy_r)
}

/// Every audio extension FFmpeg decodes that SoundApp treats as playable.
const AUDIO_EXTS: &[&str] = &[
    "mp3", "wav", "flac", "ogg", "opus", "m4a", "m4b", "aac", "mp4", "webm", "mpg", "mp2", "aif",
    "aiff", "wma", "asf", "ape", "wv", "wvc", "tta", "mka", "amr", "3ga", "ac3", "eac3", "dts",
    "dtshd", "caf", "au", "snd", "voc", "tak", "mpc",
];

/// Whether the path is playable: an FFmpeg-decoded audio format, a tracker
/// module libopenmpt claims (the library itself is asked — its list cannot
/// drift from the linked build, and it does not claim mp3/wav/flac), or a
/// MIDI file the FluidSynth engine claims (mid/midi/rmi).
fn is_supported_audio(p: &std::path::Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            AUDIO_EXTS.iter().any(|x| e.eq_ignore_ascii_case(x))
                || tracker::is_tracker_file(p)
                || midi::is_midi_file(p)
        })
        .unwrap_or(false)
}

/// Add-zone drop decision: while the player holds a track (`current_file`
/// present), a dropped file joins the playlist AND starts immediately;
/// with nothing loaded the plain append path stays.
fn add_drop_plays_now(current_file: Option<&std::path::Path>) -> bool {
    current_file.is_some()
}

/// The implicit playlist of a single file: every supported audio file in its
/// directory (flat `read_dir` order), plus the index of `file` in that list.
fn playlist_from_single_file(file: &std::path::Path) -> (Vec<PathBuf>, usize) {
    let Some(parent) = file.parent() else {
        return (vec![file.to_path_buf()], 0);
    };
    let target_name = file.file_name();
    let mut files = Vec::new();
    let mut idx: Option<usize> = None;
    if let Ok(entries) = std::fs::read_dir(parent) {
        for entry in entries.flatten() {
            // `DirEntry::file_type` reuses the `WIN32_FIND_DATAW` that
            // `FindNextFileW` already returned — no extra SMB round-trip, unlike
            // `Path::is_file()` / `canonicalize()`, which stat every entry.
            let Ok(ft) = entry.file_type() else { continue };
            if !ft.is_file() {
                continue;
            }
            let name = entry.file_name();
            let path = entry.path();
            if !is_supported_audio(&path) {
                continue;
            }
            if idx.is_none() && target_name.is_some_and(|t| name.eq_ignore_ascii_case(t)) {
                idx = Some(files.len());
            }
            files.push(path);
        }
    }
    if files.is_empty() {
        return (vec![file.to_path_buf()], 0);
    }
    (files, idx.unwrap_or(0))
}

/// Collect every supported audio file under `dir`, descending when `recursive`.
fn playlist_from_folder(dir: &std::path::Path, recursive: bool) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        let path = entry.path();
        if ft.is_dir() {
            if recursive {
                out.extend(playlist_from_folder(&path, true));
            }
        } else if ft.is_file() && is_supported_audio(&path) {
            out.push(path);
        }
    }
    out
}

/// Format-ribbon colour by uppercase extension.
fn ribbon_color(ext: &str) -> (u8, u8, u8, u8) {
    match ext {
        "MP3" => rgb(134, 14, 54),
        "M4A" | "M4B" | "AAC" | "MP4" => rgb(7, 62, 145),
        "FLAC" => rgb(15, 104, 23),
        "OGG" | "OPUS" => rgb(145, 58, 7),
        // Tracker modules (libopenmpt source engine) — violet family.
        "MOD" | "S3M" | "XM" | "IT" | "MPTM" | "MO3" | "STM" | "MTM" | "MED" | "FAR" | "M15"
        | "669" | "ULT" | "GDM" | "UMX" => rgb(88, 28, 135),
        // MIDI (FluidSynth source engine) — teal family.
        "MID" | "MIDI" | "RMI" => rgb(7, 94, 104),
        _ => rgb(100, 100, 100),
    }
}

/// Background directory-scan request so the UI thread never blocks on SMB I/O.
enum ScanJob {
    SingleFile(PathBuf),
    Path {
        path: PathBuf,
        append: bool,
        recursive: bool,
    },
    MixerPath {
        path: PathBuf,
        recursive: bool,
    },
    MixerDrop {
        path: PathBuf,
        replace_slot: Option<usize>,
        recursive: bool,
    },
}

enum ScanOutcome {
    SingleFile {
        files: Vec<PathBuf>,
        idx: usize,
    },
    Folder {
        files: Vec<PathBuf>,
        append: bool,
    },
    MixerStems(Vec<PathBuf>),
    MixerDrop {
        files: Vec<PathBuf>,
        replace_slot: Option<usize>,
    },
}

/// Which of the three drop zones the pointer is over.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DropZone {
    Add,
    Replace,
    Mixer,
}

/// Intents produced by the main window, applied after the render borrow ends.
#[derive(Default)]
struct MainIntents {
    toggle_play: bool,
    prev: bool,
    next: bool,
    loop_toggled: bool,
    shuffle_toggled: bool,
    vol_changed: bool,
    seek_to: Option<f64>,
    toggle_settings: bool,
    toggle_monitoring: bool,
    toggle_parameters: bool,
    toggle_controls: bool,
    close: bool,
}

/// Intents produced by the Parameters window, applied after the render borrow ends.
#[derive(Default)]
struct ParamsWindowIntents {
    /// A slider / switch / per-card Reset changed a value — push the full set.
    params_changed: bool,
    /// The engine card switched (tape ⇄ pitch/time) — push the new engine's set.
    pipeline_changed: bool,
    /// The SoundFont select moved — forward the new font to the engine (MIDI
    /// sources adopt it live; others open the next MIDI track with it).
    soundfont_idx: Option<usize>,
    open_soundfonts_folder: bool,
    tracker_mutes_changed: bool,
}

/// Small flat ghost icon button used by the minimalist top bar.
fn ghost_icon_btn(
    ui: &mut Ui,
    id: u32,
    r: Rect,
    icon: &str,
    color: (u8, u8, u8, u8),
    icon_px: f32,
    hover_fill: (u8, u8, u8, u8),
) -> bool {
    ui.state.register_focusable(id);
    let hovered = ui.state.can_hover(id, r);
    if hovered {
        ui.state.hovered_id = Some(id);
        if ui.state.mouse_pressed {
            ui.state.active_id = Some(id);
            ui.state.focused_id = Some(id);
        }
    }
    let clicked = hovered && ui.state.mouse_released && ui.state.active_id == Some(id);
    if hovered {
        ui.scene.rect(r.x, r.y, r.w, r.h, hover_fill);
    }
    ui.scene.icon_centered(
        ui.text_sys,
        icon,
        (r.x + r.w * 0.5).round(),
        (r.y + r.h * 0.5).round(),
        icon_px.round(),
        color,
    );
    clicked
}

/// Flat square transport button (`rgb(52,52,52)` in dark / `rgb(180,180,180)` in light,
/// blue when active — matching `SoundApp/css/main.css` `.controls .button`).
fn player_icon_btn(ui: &mut Ui, id: u32, r: Rect, icon: &str, active: bool, scale: f32) -> bool {
    let dark = ui.theme.dark;
    ui.state.register_focusable(id);
    let hovered = ui.state.can_hover(id, r);
    if hovered {
        ui.state.hovered_id = Some(id);
        if ui.state.mouse_pressed {
            ui.state.active_id = Some(id);
            ui.state.focused_id = Some(id);
        }
    }
    let clicked = hovered && ui.state.mouse_released && ui.state.active_id == Some(id);
    let fill = if active {
        rgb(76, 132, 229)
    } else if dark {
        if hovered {
            rgb(68, 68, 68)
        } else {
            rgb(52, 52, 52)
        }
    } else if hovered {
        rgb(150, 150, 150)
    } else {
        rgb(180, 180, 180)
    };
    ui.scene
        .rounded_rect(r.x, r.y, r.w, r.h, 2.0 * scale, fill, (0, 0, 0, 0), 0.0);
    let fg = if active {
        rgb(255, 255, 255)
    } else if dark {
        rgb(185, 185, 185)
    } else {
        rgb(30, 30, 30)
    };
    ui.scene.icon_centered(
        ui.text_sys,
        icon,
        (r.x + r.w * 0.5).round(),
        (r.y + r.h * 0.5).round(),
        (14.0 * scale).round(),
        fg,
    );
    clicked
}

/// Label/value row of the metadata block (right-aligned label column).
#[allow(clippy::too_many_arguments)]
fn draw_meta_row(
    ui: &mut Ui,
    x: f32,
    label_w: f32,
    gap: f32,
    baseline: f32,
    fs: f32,
    label: &str,
    value: &str,
    value_w: f32,
) {
    let dark = ui.theme.dark;
    let lbl_col = if dark {
        rgb(130, 130, 130)
    } else {
        rgb(105, 105, 105)
    };
    let val_col = if dark {
        rgb(205, 205, 205)
    } else {
        rgb(25, 25, 25)
    };
    if !label.is_empty() {
        let lw = ui
            .text_sys
            .measure(FontFace::Regular, ui.font_engine, label, fs);
        ui.scene.text(
            ui.text_sys,
            FontFace::Regular,
            ui.font_engine,
            label,
            (x + label_w - lw).round(),
            baseline,
            fs,
            lbl_col,
        );
    }
    ui.text_clipped(
        FontFace::Regular,
        ui.font_engine,
        value,
        (x + label_w + gap).round(),
        baseline,
        fs,
        value_w,
        val_col,
    );
}

/// Parameters engine-card helpers — modelled on the reference Electron
/// Parameters window (`docs/reference_ui/params_audio.png`): one card per
/// engine, the inactive one dimmed, click-to-activate.

/// Press/release hit for "click a dimmed engine card to activate it" (the
/// reference activates a disabled section on any click inside it). Active cards
/// never report a click — they are already the chosen engine.
fn engine_card_hit(ui: &mut Ui, id: u32, r: Rect, active: bool) -> bool {
    if active {
        return false;
    }
    ui.state.register_focusable(id);
    let hovered = ui.state.can_hover(id, r);
    if hovered {
        ui.state.hovered_id = Some(id);
        if ui.state.mouse_pressed {
            ui.state.active_id = Some(id);
            ui.state.focused_id = Some(id);
        }
    }
    hovered && ui.state.mouse_released && ui.state.active_id == Some(id)
}

/// Card title row: engine name left, blue check badge right when active.
fn draw_card_title(ui: &mut Ui, r: Rect, title: &str, active: bool) {
    let t = ui.theme;
    let fs = t.font_medium();
    ui.text_clipped(
        FontFace::SemiBold,
        ui.font_engine,
        title,
        r.x.round(),
        (r.y + r.h * 0.5 + fs * 0.36).round(),
        fs,
        (r.w - fs * 2.0).max(0.0),
        t.text,
    );
    if active {
        ui.scene.icon_centered(
            ui.text_sys,
            ICON_CHECK,
            (r.x + r.w - fs * 0.7).round(),
            (r.y + r.h * 0.5).round(),
            fs.round(),
            t.highlight,
        );
    }
}

/// Large centered value readout: big number with a small unit beside it
/// (`+0 semitones` / `100 %` in the reference Parameters cards).
fn draw_value_readout(ui: &mut Ui, r: Rect, value: &str, unit: &str) {
    draw_value_readout_scaled(ui, r, value, unit, 1.9);
}

fn draw_value_readout_scaled(ui: &mut Ui, r: Rect, value: &str, unit: &str, scale: f32) {
    let t = ui.theme;
    let vfs = (t.rem * scale).round();
    let ufs = t.font_small();
    let vw = ui
        .text_sys
        .measure(FontFace::SemiBold, ui.font_engine, value, vfs);
    let uw = ui
        .text_sys
        .measure(FontFace::Regular, ui.font_engine, unit, ufs);
    let gap = (t.rem * 0.4).round();
    let x0 = r.x + (r.w - vw - gap - uw) * 0.5;
    let cy = r.y + r.h * 0.5;
    ui.scene.text(
        ui.text_sys,
        FontFace::SemiBold,
        ui.font_engine,
        value,
        x0.round(),
        (cy + vfs * 0.36).round(),
        vfs,
        t.text,
    );
    ui.scene.text(
        ui.text_sys,
        FontFace::Regular,
        ui.font_engine,
        unit,
        (x0 + vw + gap).round(),
        (cy + ufs * 0.36).round(),
        ufs,
        t.text_dim,
    );
}

/// Non-interactive slider snapshot for a dimmed card: track, bipolar center
/// tick, fill and thumb — same geometry as `Ui::slider`, no hit target.
fn draw_static_slider(ui: &mut Ui, r: Rect, val: f32, min: f32, max: f32) {
    let t = ui.theme;
    let label_h = (t.rem * 1.25).round();
    let track_area = Rect::new(r.x, r.y + label_h, r.w, r.h - label_h);
    let thumb_r = (t.rem * 0.5).round();
    let track_x0 = (r.x + thumb_r).round();
    let track_w = (r.w - thumb_r * 2.0).round().max(1.0);
    let track_h = (t.rem * 0.25).round().max(4.0);
    let track_cy = (track_area.y + track_area.h * 0.5).round();
    let track_bg = if t.dark {
        rgba(255, 255, 255, 38)
    } else {
        rgba(0, 0, 0, 38)
    };
    ui.scene.rounded_rect(
        track_x0,
        track_cy - track_h * 0.5,
        track_w,
        track_h,
        track_h * 0.5,
        track_bg,
        (0, 0, 0, 0),
        0.0,
    );
    let norm = ((val - min) / (max - min)).clamp(0.0, 1.0);
    let fill_w = track_w * norm;
    if min < 0.0 && max > 0.0 {
        let zero_norm = (0.0 - min) / (max - min);
        let zx = (track_x0 + track_w * zero_norm).round();
        let tick_h = (t.rem * 0.72).round();
        ui.scene
            .rect(zx - 1.0, track_cy - tick_h * 0.5, 2.0, tick_h, t.shade5);
        let f0 = zx.min(track_x0 + fill_w);
        let f1 = zx.max(track_x0 + fill_w);
        if f1 > f0 {
            ui.scene.rounded_rect(
                f0,
                track_cy - track_h * 0.5,
                f1 - f0,
                track_h,
                track_h * 0.5,
                t.highlight,
                (0, 0, 0, 0),
                0.0,
            );
        }
    } else if fill_w > 0.5 {
        ui.scene.rounded_rect(
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
    let d = (t.rem * 1.0).round();
    ui.scene.rounded_rect(
        (track_x0 + fill_w - d * 0.5).round(),
        (track_cy - d * 0.5).round(),
        d,
        d,
        d * 0.5,
        t.highlight,
        (0, 0, 0, 0),
        0.0,
    );
}

/// Scale labels under a slider: left edge / center / right edge, muted.
fn draw_scale_labels(ui: &mut Ui, r: Rect, left: &str, mid: &str, right: &str) {
    let t = ui.theme;
    let fs = t.font_xsmall();
    let baseline = (r.y + r.h * 0.5 + fs * 0.35).round();
    ui.scene.text(
        ui.text_sys,
        FontFace::Regular,
        ui.font_engine,
        left,
        r.x.round(),
        baseline,
        fs,
        t.text_muted,
    );
    let mw = ui
        .text_sys
        .measure(FontFace::Regular, ui.font_engine, mid, fs);
    ui.scene.text(
        ui.text_sys,
        FontFace::Regular,
        ui.font_engine,
        mid,
        (r.x + (r.w - mw) * 0.5).round(),
        baseline,
        fs,
        t.text_muted,
    );
    let rw = ui
        .text_sys
        .measure(FontFace::Regular, ui.font_engine, right, fs);
    ui.scene.text(
        ui.text_sys,
        FontFace::Regular,
        ui.font_engine,
        right,
        (r.x + r.w - rw).round(),
        baseline,
        fs,
        t.text_muted,
    );
}

/// Static outline button snapshot (dimmed card) — no hit target.
fn draw_ghost_button(ui: &mut Ui, r: Rect, label: &str) {
    let t = ui.theme;
    ui.scene.rounded_rect(
        r.x.round(),
        r.y.round(),
        r.w.round(),
        r.h.round(),
        t.radius1(),
        t.base,
        t.border2,
        1.0,
    );
    draw_centered_text(ui, r, label, FontFace::Regular, t.font_small(), t.text_dim);
}

/// Static checkbox snapshot for a dimmed card.
fn draw_ghost_checkbox(ui: &mut Ui, r: Rect, label: &str, on: bool) {
    let t = ui.theme;
    let side = (t.rem * 1.1).round();
    let cy = r.y + r.h * 0.5;
    let box_r = Rect::new(r.x, (cy - side * 0.5).round(), side, side);
    ui.scene.rounded_rect(
        box_r.x,
        box_r.y,
        box_r.w,
        box_r.h,
        t.radius1(),
        t.base,
        t.border3,
        1.0,
    );
    if on {
        ui.scene.icon_centered(
            ui.text_sys,
            ICON_CHECK,
            box_r.x + side * 0.5,
            box_r.y + side * 0.5,
            side * 0.85,
            t.text_dim,
        );
    }
    let fs = t.font_small();
    let ltx = box_r.x + side + t.checkbox_gap();
    ui.text_clipped(
        FontFace::Regular,
        ui.font_engine,
        label,
        ltx.round(),
        (cy + fs * 0.33).round(),
        fs,
        (r.x + r.w - ltx).max(0.0),
        t.text_dim,
    );
}

/// Dim veil over an inactive engine card — the reference `disabled` section
/// look (a translucent page-background wash over the whole card, like CSS
/// `opacity` on the section).
fn dim_card(ui: &mut Ui, r: Rect) {
    let t = ui.theme;
    let veil = if t.dark {
        rgba(19, 19, 19, 150)
    } else {
        rgba(235, 235, 235, 150)
    };
    ui.scene.rounded_rect(
        r.x + 1.0,
        r.y + 1.0,
        (r.w - 2.0).max(1.0),
        (r.h - 2.0).max(1.0),
        t.radius2(),
        veil,
        (0, 0, 0, 0),
        0.0,
    );
}

/// Tape Speed engine card body — AUDIO page only.
fn draw_tape_speed_card(
    ui: &mut Ui,
    title: &str,
    active: bool,
    params: &mut ParamState,
    intents: &mut ParamsWindowIntents,
) {
    let title_r = ui.take(Length::Rem(1.5));
    draw_card_title(ui, title_r, title, active);
    let body_r = ui.take(Length::Rem(5.9));
    let vrem = Length::Rem(2.2).resolve(ui.theme.rem);
    let srem = Length::Rem(2.6).resolve(ui.theme.rem);
    let val_r = Rect::new(body_r.x, body_r.y, body_r.w, vrem);
    let sl_r = Rect::new(body_r.x, body_r.y + vrem, body_r.w, srem);
    let lab_r = Rect::new(
        body_r.x,
        body_r.y + vrem + srem,
        body_r.w,
        body_r.h - vrem - srem,
    );
    let v = format!("{:+.0}", params.tape_speed_st);
    draw_value_readout(ui, val_r, &v, "semitones");
    if active {
        let prev = params.tape_speed_st;
        if slider_one_unit(
            ui,
            1210,
            sl_r,
            &mut params.tape_speed_st,
            -12.0,
            12.0,
            0.0,
            1.0,
        ) {
            params.tape_speed_st = snap_semitones(params.tape_speed_st);
            intents.params_changed |= params.tape_speed_st != prev;
        }
    } else {
        draw_static_slider(ui, sl_r, params.tape_speed_st, -12.0, 12.0);
    }
    draw_scale_labels(ui, lab_r, "-12", "0", "+12");
    let [_, btn_r] = ui.take_cols(
        Length::Rem(1.9),
        Length::rem(1.0),
        [Length::fill(), Length::rem(6.5)],
    );
    if !active {
        draw_ghost_button(ui, btn_r, "Reset");
    } else if ui.button(1211, btn_r, "Reset", ICON_RESTART, ButtonVariant::Outline) {
        params.tape_speed_st = 0.0;
        intents.params_changed = true;
    }
}

/// Rubber Band Pitch / Time engine card body — AUDIO page only.
#[allow(clippy::too_many_arguments)]
fn draw_pitch_time_card(
    ui: &mut Ui,
    title: &str,
    active: bool,
    params: &mut ParamState,
    intents: &mut ParamsWindowIntents,
) {
    let theme = ui.theme;
    let title_r = ui.take(Length::Rem(1.5));
    draw_card_title(ui, title_r, title, active);
    let fs = theme.font_small();
    let sub_h = Length::Rem(1.0).resolve(theme.rem);
    let val_h = Length::Rem(1.9).resolve(theme.rem);
    let sl_h = Length::Rem(2.6).resolve(theme.rem);
    // Pitch Shift group
    let g1 = ui.take(Length::Rem(6.6));
    let g1_sub = Rect::new(g1.x, g1.y, g1.w, sub_h);
    let g1_val = Rect::new(g1.x, g1.y + sub_h, g1.w, val_h);
    let g1_sl = Rect::new(g1.x, g1.y + sub_h + val_h, g1.w, sl_h);
    let g1_lab = Rect::new(
        g1.x,
        g1.y + sub_h + val_h + sl_h,
        g1.w,
        g1.h - sub_h - val_h - sl_h,
    );
    ui.text_clipped(
        FontFace::SemiBold,
        ui.font_engine,
        "Pitch Shift",
        g1_sub.x.round(),
        (g1_sub.y + g1_sub.h * 0.5 + fs * 0.35).round(),
        fs,
        g1_sub.w,
        theme.text_dim,
    );
    let v = format!("{:+.0}", params.pitch_st);
    draw_value_readout(ui, g1_val, &v, "semitones");
    if active {
        let prev = params.pitch_st;
        if slider_one_unit(ui, 1220, g1_sl, &mut params.pitch_st, -12.0, 12.0, 0.0, 1.0) {
            params.pitch_st = snap_semitones(params.pitch_st);
            intents.params_changed |= params.pitch_st != prev;
        }
    } else {
        draw_static_slider(ui, g1_sl, params.pitch_st, -12.0, 12.0);
    }
    draw_scale_labels(ui, g1_lab, "-12", "0", "+12");
    // Time Stretch group
    let g2 = ui.take(Length::Rem(6.6));
    let g2_sub = Rect::new(g2.x, g2.y, g2.w, sub_h);
    let g2_val = Rect::new(g2.x, g2.y + sub_h, g2.w, val_h);
    let g2_sl = Rect::new(g2.x, g2.y + sub_h + val_h, g2.w, sl_h);
    let g2_lab = Rect::new(
        g2.x,
        g2.y + sub_h + val_h + sl_h,
        g2.w,
        g2.h - sub_h - val_h - sl_h,
    );
    ui.text_clipped(
        FontFace::SemiBold,
        ui.font_engine,
        "Time Stretch",
        g2_sub.x.round(),
        (g2_sub.y + g2_sub.h * 0.5 + fs * 0.35).round(),
        fs,
        g2_sub.w,
        theme.text_dim,
    );
    let v = format!("{:.0}", params.stretch * 100.0);
    draw_value_readout(ui, g2_val, &v, "%");
    // Driven in a ±0.5 bias domain around 100%: the reference
    // slider carries a center mark at 100% and fills outward
    // from it (`<div class="center-mark">` in the Electron
    // markup), which the widget only draws for a range that
    // spans zero. `stretch` itself stays a 0.5..1.5 ratio.
    if active {
        let prev = params.stretch;
        let mut bias = params.stretch - 1.0;
        if slider_one_unit(ui, 1221, g2_sl, &mut bias, -0.5, 0.5, 0.0, 0.01) {
            params.stretch = snap_stretch(bias + 1.0);
            intents.params_changed |= params.stretch != prev;
        }
    } else {
        draw_static_slider(ui, g2_sl, params.stretch - 1.0, -0.5, 0.5);
    }
    draw_scale_labels(ui, g2_lab, "50%", "100%", "150%");
    let [sw_r, btn_r] = ui.take_cols(
        Length::Rem(1.9),
        Length::rem(1.0),
        [Length::fill(), Length::rem(6.5)],
    );
    if !active {
        draw_ghost_checkbox(ui, sw_r, "Preserve Formants", params.formant);
        draw_ghost_button(ui, btn_r, "Reset");
    } else {
        if ui.checkbox(1222, sw_r, "Preserve Formants", &mut params.formant) {
            intents.params_changed = true;
        }
        if ui.button(1223, btn_r, "Reset", ICON_RESTART, ButtonVariant::Outline) {
            params.pitch_st = 0.0;
            params.stretch = 1.0;
            params.formant = false;
            intents.params_changed = true;
        }
    }
}

/// Native tracker (libopenmpt) control labels for the interpolation select,
/// index-aligned with `tracker::Interpolation::ALL`.
const TRK_INTERP_OPTIONS: [&str; 5] = [
    "Library Default",
    "None (authentic)",
    "Linear",
    "Cubic",
    "Sinc 8 (HQ)",
];

/// Native Tracker Engine card body — TRACKER page only. Pitch and tempo are
/// the event-domain controls (samples transpose without speed change / rows
/// speed up without transposition); separation and interpolation shape the
/// module mixer. Always active: this card IS the source engine.
fn draw_tracker_native_card(
    ui: &mut Ui,
    params: &mut ParamState,
    intents: &mut ParamsWindowIntents,
) {
    let theme = ui.theme;
    let fs = theme.font_small();
    let sub_h = Length::Rem(1.0).resolve(theme.rem);
    let val_h = Length::Rem(1.9).resolve(theme.rem);
    let sl_h = Length::Rem(2.6).resolve(theme.rem);

    let group_label = |ui: &mut Ui, r: Rect, label: &str| {
        ui.text_clipped(
            FontFace::SemiBold,
            ui.font_engine,
            label,
            r.x.round(),
            (r.y + r.h * 0.5 + fs * 0.35).round(),
            fs,
            r.w,
            theme.text_dim,
        );
    };

    // Each control group spans the full card width and the groups stack
    // vertically, as on the reference page (`params_tracker.png`) — no
    // half-width pitch/tempo or separation/interpolation pairing.
    let full_group = |ui: &mut Ui, h: f32| -> (Rect, Rect, Rect, Rect) {
        let g = ui.take(Length::Rem(h));
        let sub = Rect::new(g.x, g.y, g.w, sub_h);
        let val = Rect::new(g.x, g.y + sub_h, g.w, val_h);
        let sl = Rect::new(g.x, g.y + sub_h + val_h, g.w, sl_h);
        let lab = Rect::new(
            g.x,
            g.y + sub_h + val_h + sl_h,
            g.w,
            g.h - sub_h - val_h - sl_h,
        );
        (sub, val, sl, lab)
    };

    let (sub, val, sl, lab) = full_group(ui, 6.6);
    group_label(ui, sub, "Pitch Shift");
    let v = format!("{:+.0}", params.trk_pitch_st);
    draw_value_readout(ui, val, &v, "semitones");
    {
        let prev = params.trk_pitch_st;
        if slider_one_unit(
            ui,
            1261,
            sl,
            &mut params.trk_pitch_st,
            -12.0,
            12.0,
            0.0,
            1.0,
        ) {
            params.trk_pitch_st = snap_semitones_range(params.trk_pitch_st, -12.0, 12.0);
            intents.params_changed |= params.trk_pitch_st != prev;
        }
    }
    draw_scale_labels(ui, lab, "-12", "0", "+12");

    let (sub, val, sl, lab) = full_group(ui, 6.6);
    group_label(ui, sub, "Tempo");
    let v = format!("{:.0}", params.trk_tempo_pct);
    draw_value_readout(ui, val, &v, "%");
    {
        let prev = params.trk_tempo_pct;
        if slider_one_unit(
            ui,
            1263,
            sl,
            &mut params.trk_tempo_pct,
            50.0,
            150.0,
            100.0,
            1.0,
        ) {
            params.trk_tempo_pct = snap_percent(params.trk_tempo_pct, 50.0, 150.0);
            intents.params_changed |= params.trk_tempo_pct != prev;
        }
    }
    draw_scale_labels(ui, lab, "50%", "100%", "150%");

    let [_, btn_r] = ui.take_cols(
        Length::Rem(1.9),
        Length::rem(1.0),
        [Length::fill(), Length::rem(6.5)],
    );
    if ui.button(1268, btn_r, "Reset", ICON_RESTART, ButtonVariant::Outline) {
        params.reset_tracker();
        intents.params_changed = true;
    }
}

fn toggle_tracker_solo(mutes: &mut [bool], index: usize, additive: bool) {
    if index >= mutes.len() {
        return;
    }
    let has_solo = mutes.iter().any(|muted| *muted);
    let is_solo = has_solo && !mutes[index];
    if additive {
        if is_solo {
            mutes[index] = true;
            if mutes.iter().all(|muted| *muted) {
                mutes.fill(false);
            }
        } else {
            if !has_solo {
                mutes.fill(true);
            }
            mutes[index] = false;
        }
    } else {
        let only_solo = is_solo
            && mutes
                .iter()
                .enumerate()
                .all(|(channel, muted)| channel == index || *muted);
        if only_solo {
            mutes.fill(false);
        } else {
            mutes.fill(true);
            mutes[index] = false;
        }
    }
}

fn draw_tracker_channel_card(
    ui: &mut Ui,
    channel_count: usize,
    vu: &[tracker::ChannelVu],
    mutes: &mut Vec<bool>,
    additive_solo: bool,
    intents: &mut ParamsWindowIntents,
) {
    let theme = ui.theme;
    let title = ui.take(Length::Rem(1.5));
    draw_card_title(ui, title, "Channel Mixer", false);
    let count = format!("{channel_count} channels");
    let fs = theme.font_small();
    let count_w = ui
        .text_sys
        .measure(FontFace::Regular, ui.font_engine, &count, fs);
    ui.scene.text(
        ui.text_sys,
        FontFace::Regular,
        ui.font_engine,
        &count,
        (title.x + title.w - count_w).round(),
        (title.y + title.h * 0.5 + fs * 0.35).round(),
        fs,
        theme.text_dim,
    );
    if mutes.len() != channel_count {
        mutes.resize(channel_count, false);
    }
    let bars = ui.take(Length::Rem(3.2));
    let labels = ui.take(Length::Rem(1.0));
    let bar_w = Length::Rem(1.5).resolve(theme.rem).round();
    let gap = Length::Rem(0.35).resolve(theme.rem).round();
    let has_solo = mutes.iter().any(|muted| *muted);
    for channel in 0..channel_count {
        let x = bars.x + channel as f32 * (bar_w + gap);
        if x + bar_w > bars.x + bars.w {
            break;
        }
        let bar = Rect::new(x, bars.y, bar_w, bars.h);
        if engine_card_hit(ui, 1280 + channel as u32, bar, false) {
            toggle_tracker_solo(mutes, channel, additive_solo);
            intents.tracker_mutes_changed = true;
        }
        let solo = has_solo && !mutes[channel];
        let border = if solo { theme.highlight } else { theme.border2 };
        ui.scene.rounded_rect(
            bar.x,
            bar.y,
            bar.w,
            bar.h,
            theme.radius1(),
            theme.base,
            border,
            if solo { 2.0 } else { 1.0 },
        );
        let level = vu
            .get(channel)
            .map(|level| level.peak().clamp(0.0, 1.0))
            .unwrap_or(0.0);
        let fill_h = (bar.h * level).round();
        if fill_h > 0.0 && !mutes[channel] {
            ui.scene.rect(
                bar.x + 1.0,
                bar.y + bar.h - fill_h,
                (bar.w - 2.0).max(1.0),
                fill_h,
                rgb(74, 170, 74),
            );
        }
        let label = (channel + 1).to_string();
        draw_centered_text(
            ui,
            Rect::new(x, labels.y, bar_w, labels.h),
            &label,
            FontFace::Regular,
            theme.font_xsmall(),
            if mutes[channel] {
                theme.text_dim
            } else {
                theme.text
            },
        );
    }
}

fn draw_tracker_separation_card(
    ui: &mut Ui,
    params: &mut ParamState,
    intents: &mut ParamsWindowIntents,
) {
    let theme = ui.theme;
    let title = ui.take(Length::Rem(1.5));
    draw_card_title(ui, title, "Stereo Separation", false);
    let value = ui.take(Length::Rem(1.6));
    let v = format!("{:.0}", params.trk_separation);
    draw_value_readout_scaled(ui, value, &v, "%", 1.4);
    let slider = ui.take(Length::Rem(2.6));
    let previous = params.trk_separation;
    if slider_one_unit(
        ui,
        1265,
        slider,
        &mut params.trk_separation,
        0.0,
        200.0,
        100.0,
        1.0,
    ) {
        params.trk_separation = snap_percent(params.trk_separation, 0.0, 200.0);
        intents.params_changed |= params.trk_separation != previous;
    }
    let labels = ui.take(Length::Rem(0.8));
    draw_scale_labels(ui, labels, "0%", "100%", "200%");

    let interpolation_label = ui.take(Length::Rem(1.0));
    ui.text_clipped(
        FontFace::Regular,
        ui.font_engine,
        "Interpolation (extension)",
        interpolation_label.x,
        (interpolation_label.y + interpolation_label.h * 0.5 + theme.font_small() * 0.35).round(),
        theme.font_small(),
        interpolation_label.w,
        theme.text_dim,
    );
    let select = ui.take(Length::Rem(2.2));
    let mut index = tracker::Interpolation::ALL
        .iter()
        .position(|interpolation| *interpolation == params.trk_interp)
        .unwrap_or(0);
    if ui.select(1267, select, &TRK_INTERP_OPTIONS, &mut index) {
        params.trk_interp = tracker::Interpolation::ALL[index];
        intents.params_changed = true;
    }
}

/// Native MIDI controls — transpose, tempo and metronome are applied by
/// FluidSynth itself (live, no restart).
#[allow(clippy::too_many_arguments)]
fn draw_midi_native_card(
    ui: &mut Ui,
    params: &mut ParamState,
    native_bpm: f32,
    intents: &mut ParamsWindowIntents,
) {
    let theme = ui.theme;
    let fs = theme.font_small();
    let sub_h = Length::Rem(1.0).resolve(theme.rem);
    let val_h = Length::Rem(1.9).resolve(theme.rem);
    let sl_h = Length::Rem(2.6).resolve(theme.rem);

    let group_label = |ui: &mut Ui, r: Rect, label: &str| {
        ui.text_clipped(
            FontFace::SemiBold,
            ui.font_engine,
            label,
            r.x.round(),
            (r.y + r.h * 0.5 + fs * 0.35).round(),
            fs,
            r.w,
            theme.text_dim,
        );
    };

    // Each control group spans the full card width and the groups stack
    // vertically, as on the reference page (`params_midi.png`) — no
    // half-width transpose/tempo pairing.
    let full_group = |ui: &mut Ui, h: f32| -> (Rect, Rect, Rect, Rect) {
        let g = ui.take(Length::Rem(h));
        let sub = Rect::new(g.x, g.y, g.w, sub_h);
        let val = Rect::new(g.x, g.y + sub_h, g.w, val_h);
        let sl = Rect::new(g.x, g.y + sub_h + val_h, g.w, sl_h);
        let lab = Rect::new(
            g.x,
            g.y + sub_h + val_h + sl_h,
            g.w,
            g.h - sub_h - val_h - sl_h,
        );
        (sub, val, sl, lab)
    };

    let (sub, val, sl, lab) = full_group(ui, 6.6);
    group_label(ui, sub, "Global Transpose");
    let v = format!("{:+.0}", params.midi_transpose_st);
    draw_value_readout(ui, val, &v, "semitones");
    {
        let prev = params.midi_transpose_st;
        if slider_one_unit(
            ui,
            1272,
            sl,
            &mut params.midi_transpose_st,
            -12.0,
            12.0,
            0.0,
            1.0,
        ) {
            params.midi_transpose_st = snap_semitones_range(params.midi_transpose_st, -12.0, 12.0);
            intents.params_changed |= params.midi_transpose_st != prev;
        }
    }
    draw_scale_labels(ui, lab, "-12", "0", "+12");

    let (sub, val, sl, lab) = full_group(ui, 6.6);
    group_label(ui, sub, "Tempo");
    let hint = format!("(Original: {:.0})", native_bpm);
    let hint_x = sub.x
        + ui.text_sys
            .measure(FontFace::SemiBold, ui.font_engine, "Tempo", fs)
        + theme.rem;
    ui.text_clipped(
        FontFace::Regular,
        ui.font_engine,
        &hint,
        hint_x.round(),
        (sub.y + sub.h * 0.5 + fs * 0.35).round(),
        fs,
        (sub.x + sub.w - hint_x).max(0.0),
        theme.text_dim,
    );
    let mut bpm = if params.midi_bpm_override {
        params.midi_bpm
    } else {
        native_bpm
    };
    let v = format!("{:.0}", bpm);
    draw_value_readout(ui, val, &v, "BPM");
    if slider_one_unit(
        ui,
        1273,
        sl,
        &mut bpm,
        40.0,
        240.0,
        native_bpm.clamp(40.0, 240.0),
        1.0,
    ) {
        params.midi_bpm = snap_percent(bpm, 40.0, 240.0);
        params.midi_bpm_override = true;
        intents.params_changed = true;
    }
    draw_scale_labels(ui, lab, "40", "140", "240");

    let sw2 = ui.take(Length::Rem(2.2));
    if ui.checkbox(1275, sw2, "Metronome", &mut params.midi_metronome) {
        intents.params_changed = true;
    }

    let [_, btn_r] = ui.take_cols(
        Length::Rem(1.9),
        Length::rem(1.0),
        [Length::fill(), Length::rem(11.0)],
    );
    if ui.button(
        1276,
        btn_r,
        "Reset Parameters",
        ICON_RESTART,
        ButtonVariant::Outline,
    ) {
        params.reset_midi();
        intents.params_changed = true;
    }
}

fn draw_midi_soundfont_card(
    ui: &mut Ui,
    fonts: &[PathBuf],
    selected_font: &mut usize,
    intents: &mut ParamsWindowIntents,
) {
    let title_r = ui.take(Length::Rem(1.5));
    draw_card_title(ui, title_r, "Active SoundFont", false);
    let sel = ui.take(Length::Rem(2.4));
    let names: Vec<String> = fonts
        .iter()
        .map(|p| {
            p.file_stem()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.display().to_string())
        })
        .collect();
    let options: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
    let mut font_idx = (*selected_font).min(options.len().saturating_sub(1));
    if ui.select(1271, sel, &options, &mut font_idx) {
        *selected_font = font_idx;
        intents.soundfont_idx = Some(font_idx);
    }
    let [_, btn_r] = ui.take_cols(
        Length::Rem(2.2),
        Length::rem(1.0),
        [Length::fill(), Length::rem(15.0)],
    );
    if ui.button(
        1277,
        btn_r,
        "Open Soundfonts Folder",
        "",
        ButtonVariant::Outline,
    ) {
        intents.open_soundfonts_folder = true;
    }
}

fn drop_zone_at(cx: f32, cy: f32, pw: f32, ph: f32) -> DropZone {
    if cx >= 0.78 * pw {
        DropZone::Mixer
    } else if cy >= 0.5 * ph {
        DropZone::Replace
    } else {
        DropZone::Add
    }
}

/// Three-zone drop overlay: add / replace / open-in-mixer. Returns the dropped
/// path and the zone the pointer was over when it landed.
fn draw_drop_overlay(ui: &mut Ui, pw: f32, ph: f32, scale: f32) -> Option<(PathBuf, DropZone)> {
    let hovering = ui.state.hovered_file.is_some();
    if !hovering && ui.state.dropped_files.is_empty() {
        return None;
    }
    let split_x = 0.78 * pw;
    let mid_y = 0.5 * ph;
    let cx = ui.state.cursor.0;
    let cy = ui.state.cursor.1;
    let zone = drop_zone_at(cx, cy, pw, ph);

    ui.scene.begin_overlay();
    ui.scene.rect(0.0, 0.0, pw, ph, rgba(20, 20, 20, 225));

    let zones = [
        (
            Rect::new(
                6.0 * scale,
                6.0 * scale,
                split_x - 12.0 * scale,
                mid_y - 12.0 * scale,
            ),
            DropZone::Add,
            "Add to current",
            "Release to add",
        ),
        (
            Rect::new(
                6.0 * scale,
                mid_y + 6.0 * scale,
                split_x - 12.0 * scale,
                ph - mid_y - 12.0 * scale,
            ),
            DropZone::Replace,
            "Replace playlist",
            "Release to replace",
        ),
        (
            Rect::new(
                split_x + 6.0 * scale,
                6.0 * scale,
                pw - split_x - 12.0 * scale,
                ph - 12.0 * scale,
            ),
            DropZone::Mixer,
            "Open in Mixer",
            "Release for Mixer",
        ),
    ];
    let fs = (13.0 * scale).round();
    for (r, z, label, active_label) in zones {
        let hot = z == zone;
        ui.scene.rounded_rect(
            r.x,
            r.y,
            r.w,
            r.h,
            (4.0 * scale).round(),
            if hot {
                rgba(76, 132, 229, 96)
            } else {
                rgba(255, 255, 255, 10)
            },
            if hot {
                rgb(76, 132, 229)
            } else {
                rgba(255, 255, 255, 30)
            },
            if hot { 2.0 } else { 1.0 },
        );
        let baseline = (r.y + r.h * 0.5 + fs * 0.33).round();
        let (text, color) = if hot {
            (active_label, rgb(255, 255, 255))
        } else {
            (label, rgb(165, 165, 165))
        };
        let tw = ui
            .text_sys
            .measure(FontFace::SemiBold, ui.font_engine, text, fs);
        ui.scene.text(
            ui.text_sys,
            FontFace::SemiBold,
            ui.font_engine,
            text,
            (r.x + (r.w - tw) * 0.5).round(),
            baseline,
            fs,
            color,
        );
    }
    ui.scene.end_overlay();

    ui.state.dropped_files.pop().map(|p| (p, zone))
}

impl App {
    fn main_backend(&self) -> BackendKind {
        if self.backend_locked {
            self.prefer_backend
        } else {
            BackendKind::Cpu
        }
    }

    fn secondary_backend(&self, key: &str) -> BackendKind {
        if self.backend_locked {
            self.prefer_backend
        } else {
            match key {
                "monitoring" | "mixer" => BackendKind::Gpu,
                _ => BackendKind::Cpu,
            }
        }
    }

    fn new(
        audio: Option<Audio>,
        animate: bool,
        quit_at: Option<Instant>,
        prefer_backend: BackendKind,
        font_engine: FontEngine,
        config: UserConfig,
    ) -> Self {
        let mixer_stems = default_mixer_stems();
        let soundfonts = midi::soundfont_list();
        let selected_soundfont = config
            .selected_soundfont
            .min(soundfonts.len().saturating_sub(1));
        if let Some(audio) = &audio {
            if let Some(font) = soundfonts.get(selected_soundfont) {
                audio.set_soundfont(font.clone());
            }
        }
        let audio_devices = audio::output_device_names();
        let selected_device_idx = config
            .output_device_name()
            .and_then(|name| audio_devices.iter().position(|d| d == name))
            .map(|idx| idx + 1)
            .unwrap_or(0);
        let (scan_tx, job_rx) = std::sync::mpsc::channel::<(u64, ScanJob)>();
        let (res_tx, scan_rx) = std::sync::mpsc::channel::<(u64, ScanOutcome)>();
        let _ = std::thread::Builder::new()
            .name("playlist-scan".into())
            .spawn(move || {
                let mut deferred = None;
                loop {
                    let next = match deferred.take() {
                        Some(job) => Ok(job),
                        None => job_rx.recv(),
                    };
                    let Ok((mut gen, mut job)) = next else {
                        break;
                    };
                    // Coalesce ordinary scans, but never discard Mixer drops.
                    while let Ok((g2, j2)) = job_rx.try_recv() {
                        if matches!(job, ScanJob::MixerDrop { .. })
                            || matches!(j2, ScanJob::MixerDrop { .. })
                        {
                            deferred = Some((g2, j2));
                            break;
                        }
                        gen = g2;
                        job = j2;
                    }
                    let out = match job {
                        ScanJob::SingleFile(p) => {
                            let (files, idx) = playlist_from_single_file(&p);
                            ScanOutcome::SingleFile { files, idx }
                        }
                        ScanJob::Path {
                            path,
                            append,
                            recursive,
                        } => {
                            if path.is_dir() {
                                let files = playlist_from_folder(&path, recursive);
                                ScanOutcome::Folder { files, append }
                            } else if append {
                                ScanOutcome::Folder {
                                    files: vec![path],
                                    append: true,
                                }
                            } else {
                                let (files, idx) = playlist_from_single_file(&path);
                                ScanOutcome::SingleFile { files, idx }
                            }
                        }
                        ScanJob::MixerPath { path, recursive } => {
                            let files = if path.is_dir() {
                                playlist_from_folder(&path, recursive)
                            } else {
                                playlist_from_single_file(&path).0
                            };
                            ScanOutcome::MixerStems(
                                files.into_iter().take(mixer::MAX_STEMS).collect(),
                            )
                        }
                        ScanJob::MixerDrop {
                            path,
                            replace_slot,
                            recursive,
                        } => {
                            let files = if path.is_dir() {
                                playlist_from_folder(&path, recursive)
                            } else {
                                vec![path]
                            };
                            ScanOutcome::MixerDrop {
                                files: files.into_iter().take(mixer::MAX_STEMS).collect(),
                                replace_slot,
                            }
                        }
                    };
                    let _ = res_tx.send((gen, out));
                }
            });
        let theme_base = if config.dark_mode {
            Theme::dark(1.0).base
        } else {
            Theme::light(1.0).base
        };
        let mut secondary_windows = WindowManager::new(theme_base);
        secondary_windows.set_icon_resource(1);
        let window_geom = parse_window_geometry(&config.window_geometry);
        let default_dir = config.default_dir.clone();
        Self {
            start: Instant::now(),
            reported: false,
            animate,
            quit_at,
            prefer_backend,
            backend_locked: false,
            font_engine,
            dark_mode: config.dark_mode, // Dark mode by default (accessibility requirement)
            ui_zoom: config.ui_zoom.clamp(0.75, 2.0),
            screenshot_path: None,
            window: None,
            renderer: None,
            scene: Scene::new(),
            text_sys: TextSystem::new(),
            ui_state: UiState::new(),
            modifiers: ModifiersState::empty(),
            size: (580, 390),
            audio,
            audio_reported: false,
            t: 0.0,
            frame_ms: 16,
            nopaint: false,
            next_frame: Instant::now(),
            quitting: false,
            open_pending: None,
            last_frame_at: None,
            pace_at: Instant::now(),
            pace_frames: 0,
            pace_slow: 0,

            secondary_windows: secondary_windows,
            window_geom,
            default_dir,
            cover_resting: None,
            cover_fade: None,
            pending_window_toggles: Vec::new(),
            focus_ring: VecDeque::from(["main"]),
            open_requests: None,
            open_window_spec: None,
            parameters_window_height: None,

            params: ParamState::default(),
            sw_hq_mode: config.sw_hq_mode,
            sw_keep_tray: config.sw_keep_tray,
            audio_devices,
            selected_device_idx,
            master_vol: config.master_vol.clamp(0.0, 2.0),
            chk_locked: config.chk_locked,
            scrub_frac: 0.0,
            selected_track: 0,
            show_controls: config.show_controls,
            soundfonts,
            selected_soundfont,
            tracker_mutes: Vec::new(),
            loop_enabled: false,
            shuffle_enabled: false,
            shuffle_order: Vec::new(),
            shuffle_history: VecDeque::new(),
            playlist: Vec::new(),
            track_meta: None,
            scan_tx,
            scan_rx,
            scan_gen: 0,
            dsp: DspAnalyzer::new(48_000),
            last_dsp_frames: 0,
            dsp_buf: Vec::new(),
            dsp_snap: DspSnapshot::default(),
            mixer_audio: None,
            mixer_stems,
            mixer_playing: false,
            mixer_loop: true,
            mixer_master_vol: 0.85,
            mixer_pos_frac: 0.0,
            mixer_strip_targets: Vec::new(),
            mixer_add_target: None,
            mixer_sync_debug: false,
            mixer_sync_copied_at: None,
            mixer_sync_close_rect: None,
            mixer_sync_copy_rect: None,
            config,
            save_config_req: false,
            apply_zoom_req: false,
            config_status: String::new(),
        }
    }

    /// Capture the current UI state as a persistable config and write `user.json`.
    /// Saved placement for `key`, if this session's config carried one.
    fn saved_geometry(&self, key: &str) -> Option<SavedGeom> {
        self.window_geom.get(key).copied()
    }

    /// Snapshot every live window's placement (logical units) into the config:
    /// `main` plus every secondary that exists this session. Windows never
    /// opened keep their default placement next launch.
    fn capture_window_geometry(&mut self) {
        let mut out = String::new();
        if let Some(w) = &self.window {
            if let Ok(pos) = w.outer_position() {
                let size = w.inner_size();
                let s = w.scale_factor() as f32;
                out.push_str(&format!(
                    "main {} {} {} {} 1;",
                    (pos.x as f32 / s).round() as i32,
                    (pos.y as f32 / s).round() as i32,
                    (size.width as f32 / s).round() as i32,
                    (size.height as f32 / s).round() as i32,
                ));
            }
        }
        for mw in &self.secondary_windows.windows {
            let Ok(pos) = mw.window.outer_position() else {
                continue;
            };
            let size = mw.window.inner_size();
            let s = mw.window.scale_factor() as f32;
            out.push_str(&format!(
                "{} {} {} {} {} {};",
                mw.key,
                (pos.x as f32 / s).round() as i32,
                (pos.y as f32 / s).round() as i32,
                (size.width as f32 / s).round() as i32,
                (size.height as f32 / s).round() as i32,
                if mw.visible { 1 } else { 0 },
            ));
        }
        self.config.window_geometry = out.trim_end_matches(';').to_string();
    }

    /// True when the saved rect lands on a connected monitor (logical-space
    /// comparison per monitor scale) — a window saved on an unplugged display
    /// must not come back somewhere unreachable.
    fn saved_rect_reachable(event_loop: &ActiveEventLoop, g: SavedGeom) -> bool {
        let mut any = false;
        for m in event_loop.available_monitors() {
            any = true;
            let s = m.scale_factor() as f32;
            let pos = m.position();
            let size = m.size();
            let (mx, my, mw, mh) = (
                pos.x as f32 / s,
                pos.y as f32 / s,
                size.width as f32 / s,
                size.height as f32 / s,
            );
            if g.x < mx + mw && g.x + g.w > mx && g.y < my + mh && g.y + g.h > my {
                return true;
            }
        }
        !any
    }

    fn save_config(&mut self) {
        self.capture_window_geometry();
        self.config.default_dir = self.default_dir.clone();
        self.config.dark_mode = self.dark_mode;
        self.config.ui_zoom = self.ui_zoom;
        self.config.master_vol = self.master_vol;
        self.config.show_controls = self.show_controls;
        self.config.chk_locked = self.chk_locked;
        self.config.sw_hq_mode = self.sw_hq_mode;
        self.config.sw_keep_tray = self.sw_keep_tray;
        self.config.output_device = self
            .selected_device_idx
            .checked_sub(1)
            .and_then(|idx| self.audio_devices.get(idx))
            .cloned()
            .unwrap_or_default();
        self.config.selected_soundfont = self.selected_soundfont;
        match self.config.save() {
            Ok(()) => {
                self.config_status = format!("Saved to {}", UserConfig::path().display());
            }
            Err(e) => {
                self.config_status = format!("Save failed: {e}");
            }
        }
    }

    /// Logical dimensions of the minimalist player window.
    const WIDTH: f64 = 480.0;
    const HEIGHT_WITH_CONTROLS: f64 = 278.0;
    const HEIGHT_WITHOUT_CONTROLS: f64 = 221.0;
    /// Minimum window height while the transport bar is shown.
    const MIN_HEIGHT_WITH_CONTROLS: f64 = 276.0;

    fn base_size(&self) -> (f64, f64) {
        (
            Self::WIDTH,
            if self.show_controls {
                Self::HEIGHT_WITH_CONTROLS
            } else {
                Self::HEIGHT_WITHOUT_CONTROLS
            },
        )
    }

    fn set_zoom(&mut self, new_zoom: f32) {
        let clamped = ((new_zoom * 20.0).round() / 20.0).clamp(0.75, 2.0);
        if (clamped - self.ui_zoom).abs() < 0.001 {
            return;
        }
        self.ui_zoom = clamped;
        self.apply_zoom();
    }

    fn open_mixer_with_paths(&mut self, paths: Vec<PathBuf>) {
        if let Some(main) = &self.audio {
            main.pause();
        }
        if self.mixer_audio.is_none() {
            match mixer::MixerAudio::start() {
                Ok(engine) => {
                    println!(
                        "[mixer] independent output: {} Hz, {} ch",
                        engine.rate, engine.channels
                    );
                    self.mixer_audio = Some(engine);
                }
                Err(e) => {
                    eprintln!("[mixer] failed to start independent output: {e}");
                    return;
                }
            }
        }
        self.mixer_stems = paths
            .iter()
            .take(mixer::MAX_STEMS)
            .enumerate()
            .map(|(number, path)| MixerStem {
                number: number + 1,
                slot: number,
                path: Some(path.clone()),
                gain: 1.0,
                pan: 0.0,
                muted: false,
                solo: false,
            })
            .collect();
        self.mixer_playing = !paths.is_empty();
        self.mixer_pos_frac = 0.0;
        if let Some(engine) = &mut self.mixer_audio {
            engine.load_playlist(&paths);
            self.sync_mixer_controls();
        }
    }

    fn open_mixer_from_playlist(&mut self) {
        let mut paths: Vec<PathBuf> = self
            .playlist
            .iter()
            .take(mixer::MAX_STEMS)
            .cloned()
            .collect();
        if paths.is_empty() {
            if let Some(path) = self.audio.as_ref().and_then(|a| a.current_file()) {
                paths.push(path);
            }
        }
        self.open_mixer_with_paths(paths);
    }

    fn apply_mixer_drop(&mut self, files: Vec<PathBuf>, replace_slot: Option<usize>) {
        if files.is_empty() || !self.secondary_windows.is_visible("mixer") {
            return;
        }
        if self.mixer_audio.is_none() {
            self.open_mixer_with_paths(files);
            return;
        }
        // Multi-Track takes over playback: the main player must not keep
        // sounding while the new stems load (the fresh-engine branch above
        // pauses inside open_mixer_with_paths).
        if let Some(main) = &self.audio {
            main.pause();
        }
        if let Some(slot) = replace_slot {
            let path = files[0].clone();
            if let Some(stem) = self.mixer_stems.iter_mut().find(|stem| stem.slot == slot) {
                stem.path = Some(path.clone());
                if let Some(engine) = &mut self.mixer_audio {
                    engine.load_stem(slot, path);
                }
            }
        } else {
            let mut loads = Vec::new();
            for path in files {
                if self.mixer_stems.len() + loads.len() >= mixer::MAX_STEMS {
                    break;
                }
                let slot = (0..mixer::MAX_STEMS).find(|slot| {
                    !self.mixer_stems.iter().any(|stem| stem.slot == *slot)
                        && !loads.iter().any(|(used, _)| used == slot)
                });
                let Some(slot) = slot else { break };
                loads.push((slot, path));
            }
            for (slot, path) in &loads {
                self.mixer_stems.push(MixerStem {
                    number: slot + 1,
                    slot: *slot,
                    path: Some(path.clone()),
                    gain: 1.0,
                    pan: 0.0,
                    muted: false,
                    solo: false,
                });
            }
            self.mixer_stems.sort_by_key(|stem| stem.slot);
            if let Some(engine) = &mut self.mixer_audio {
                engine.load_stems(&loads);
            }
        }
        self.mixer_playing = self
            .mixer_audio
            .as_ref()
            .map(|engine| engine.is_playing())
            .unwrap_or(false);
        self.sync_mixer_controls();
    }

    fn close_mixer(&mut self) {
        self.mixer_audio = None;
        self.mixer_stems.clear();
        self.mixer_playing = false;
        self.mixer_pos_frac = 0.0;
        self.mixer_sync_debug = false;
        self.mixer_sync_copied_at = None;
        self.mixer_sync_close_rect = None;
        self.mixer_sync_copy_rect = None;
    }

    fn sync_mixer_controls(&self) {
        let Some(engine) = &self.mixer_audio else {
            return;
        };
        engine.set_loop(self.mixer_loop);
        engine.set_master_volume(self.mixer_master_vol);
        let any_solo = self.mixer_stems.iter().any(|stem| stem.solo);
        for stem in &self.mixer_stems {
            let inaudible = stem.muted || (any_solo && !stem.solo);
            engine.set_stem(stem.slot, stem.gain, stem.pan, inaudible);
        }
    }

    fn poll_mixer(&mut self) {
        let Some(engine) = &mut self.mixer_audio else {
            return;
        };
        engine.poll();
        self.mixer_playing = engine.is_playing();
        let duration = engine.duration_secs();
        let scrubbing = self
            .secondary_windows
            .find_by_key_mut("mixer")
            .map(|window| window.ui_state.active_id == Some(1305) && window.ui_state.mouse_down)
            .unwrap_or(false);
        if !scrubbing {
            self.mixer_pos_frac = if duration > 0.0 {
                (engine.position_secs() / duration) as f32
            } else {
                0.0
            };
        }
    }

    fn adjusted_window_spec(&self, spec: &WindowSpec) -> WindowSpec {
        let mut spec = spec.clone();
        if let Some(g) = self.saved_geometry(spec.key) {
            spec.width = g.w as f64;
            spec.height = g.h as f64;
        }
        spec
    }

    /// The current UI parameter state as an engine `PlaybackParams`.
    fn playback_params(&self) -> PlaybackParams {
        let params = self.params.engine_params(self.sw_hq_mode);
        if self.tracker_source_active() || self.midi_source_active() {
            params.for_native_source()
        } else {
            params
        }
    }

    /// Whether the loaded track is a tracker module (libopenmpt source
    /// engine). Asks the linked library, so recognition cannot drift from
    /// the build; no file I/O — extension only.
    fn tracker_source_active(&self) -> bool {
        self.audio
            .as_ref()
            .and_then(|a| a.current_file())
            .map(|p| tracker::is_tracker_file(&p))
            .unwrap_or(false)
    }

    /// Whether the loaded track is a MIDI file (FluidSynth source engine).
    /// Extension check only, mirroring `tracker_source_active`.
    fn midi_source_active(&self) -> bool {
        self.audio
            .as_ref()
            .and_then(|a| a.current_file())
            .map(|p| midi::is_midi_file(&p))
            .unwrap_or(false)
    }

    /// Push the full parameter set to the engine. Parameters are applied
    /// entirely in the Rust pipeline, so no engine reload is needed.
    fn sync_pipeline_and_params(&mut self) {
        let params = self.playback_params();
        if let Some(a) = &self.audio {
            a.set_params(params);
        }
    }

    /// Rebuild the output stream for the selected device/sample rate while
    /// preserving the current file, position, transport state and parameters.
    fn restart_audio_output(&mut self) {
        let device = self
            .selected_device_idx
            .checked_sub(1)
            .and_then(|idx| self.audio_devices.get(idx))
            .cloned();
        let (file, position, playing, looping, volume) = match &self.audio {
            Some(audio) => (
                audio.current_file(),
                audio.position_secs(),
                audio.is_playing(),
                self.loop_enabled,
                self.master_vol,
            ),
            None => (None, 0.0, false, self.loop_enabled, self.master_vol),
        };
        let params = self.playback_params();
        let font = self.soundfonts.get(self.selected_soundfont).cloned();
        let mixer_open = self.secondary_windows.is_visible("mixer");
        let mixer_playing = self
            .mixer_audio
            .as_ref()
            .map(|engine| engine.is_playing())
            .unwrap_or(false);
        let mixer_position = self
            .mixer_audio
            .as_ref()
            .map(|engine| engine.position_secs())
            .unwrap_or(0.0);
        let mixer_loads: Vec<(usize, PathBuf)> = self
            .mixer_stems
            .iter()
            .filter_map(|stem| stem.path.clone().map(|path| (stem.slot, path)))
            .collect();
        match audio::start_with(Source::Silence, device.as_deref()) {
            Ok(audio) => {
                audio.set_volume(volume);
                audio.set_params(params);
                audio.set_loop(looping);
                if let Some(font) = font {
                    audio.set_soundfont(font);
                }
                if let Some(file) = file {
                    audio.load_file(file);
                    if position > 0.001 {
                        audio.seek(position);
                    }
                    if playing {
                        audio.play();
                    } else {
                        audio.pause();
                    }
                }
                self.audio = Some(audio);
                if mixer_open {
                    match mixer::MixerAudio::start_with(device.as_deref()) {
                        Ok(mut engine) => {
                            engine.set_master_volume(self.mixer_master_vol);
                            engine.set_loop(self.mixer_loop);
                            if !mixer_loads.is_empty() {
                                engine.load_stems(&mixer_loads);
                                if mixer_position > 0.001 {
                                    engine.seek_secs(mixer_position);
                                }
                                engine.set_playing(mixer_playing);
                            }
                            self.mixer_audio = Some(engine);
                        }
                        Err(error) => {
                            eprintln!("[mixer] output restart failed: {error}");
                            self.mixer_audio = None;
                        }
                    }
                }
                self.config_status = format!(
                    "Output: {} / device default rate",
                    device.as_deref().unwrap_or("System Default")
                );
            }
            Err(error) => {
                self.config_status = format!("Output change failed: {error}");
            }
        }
    }

    /// New-track rule: transient playback parameters reset to their defaults
    /// unless "Lock Settings" is set. The reset is pushed to
    /// the engine in the same breath — `Cmd::SetParams` is queued before
    /// `Cmd::Load`, so the decoder computes the new track's tape step from the
    /// fresh values and no stale parameter set survives into the new track.
    fn on_new_track(&mut self) {
        self.tracker_mutes.clear();
        if self.chk_locked {
            return;
        }
        self.params = ParamState::default();
        self.sync_pipeline_and_params();
    }

    /// xorshift64* step — allocation-free PRNG for the playlist shuffle
    /// (no `rand` dependency; seeded from the wall clock, never deterministic).
    fn shuffle_next(state: &mut u64) -> u64 {
        let mut x = *state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        *state = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }

    /// Regenerate `shuffle_order`: Fisher-Yates over `0..playlist.len()`,
    /// rotated so the current track comes first. Entries index `playlist`.
    fn rebuild_shuffle_order(&mut self) {
        let n = self.playlist.len();
        let mut seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64 ^ (u64::from(std::process::id()) << 32))
            .unwrap_or(0x9E3779B97F4A7C15)
            | 1;
        for _ in 0..4 {
            Self::shuffle_next(&mut seed);
        }
        let mut order: Vec<usize> = (0..n).collect();
        for i in (1..n).rev() {
            let j = ((Self::shuffle_next(&mut seed) >> 11) % (i as u64 + 1)) as usize;
            order.swap(i, j);
        }
        let cur = self.selected_track.min(n.saturating_sub(1));
        if let Some(pos) = order.iter().position(|&i| i == cur) {
            order.rotate_left(pos);
        }
        self.shuffle_order = order;
    }

    /// Single funnel for every playlist mutation: regenerate the shuffle order
    /// for the new playlist length and prune history entries that no longer
    /// resolve. Keeps `shuffle_order`/`shuffle_history` indexes in sync.
    fn sync_shuffle_state(&mut self) {
        if !self.shuffle_enabled || self.playlist.is_empty() {
            self.shuffle_order.clear();
            self.shuffle_history.clear();
            return;
        }
        self.shuffle_history.retain(|&i| i < self.playlist.len());
        self.rebuild_shuffle_order();
    }

    /// Load the next playlist entry (wrapping only when `wrap` is set).
    /// Walks `shuffle_order` while shuffle is on; `selected_track` stays an
    /// index into `playlist` either way. Returns `true` when a track was loaded.
    fn advance_track(&mut self, wrap: bool) -> bool {
        if self.playlist.is_empty() {
            return false;
        }
        if !self.shuffle_order.is_empty() {
            let cur = self.selected_track.min(self.playlist.len() - 1);
            let pos = match self.shuffle_order.iter().position(|&i| i == cur) {
                Some(pos) => pos,
                // Desynced order (mutation bypassed the sync funnel): rebuild.
                None => {
                    self.rebuild_shuffle_order();
                    0
                }
            };
            let last = pos + 1 == self.shuffle_order.len();
            if last && !wrap {
                return false;
            }
            let idx = if last {
                self.shuffle_order[0]
            } else {
                self.shuffle_order[pos + 1]
            };
            self.shuffle_history.push_back(cur);
            self.load_playlist_index(idx);
            return true;
        }
        let cur = self.selected_track.min(self.playlist.len() - 1);
        let idx = if cur + 1 < self.playlist.len() {
            cur + 1
        } else if wrap {
            0
        } else {
            return false;
        };
        self.load_playlist_index(idx);
        true
    }

    /// Load the previous playlist entry (wrapping only when `wrap` is set).
    /// Under shuffle this unwinds the play history first, then steps backwards
    /// through `shuffle_order`.
    fn retreat_track(&mut self, wrap: bool) -> bool {
        if self.playlist.is_empty() {
            return false;
        }
        if !self.shuffle_order.is_empty() {
            let cur = self.selected_track.min(self.playlist.len() - 1);
            let idx = if let Some(prev) = self.shuffle_history.pop_back() {
                prev
            } else {
                let pos = self
                    .shuffle_order
                    .iter()
                    .position(|&i| i == cur)
                    .unwrap_or(0);
                if pos > 0 {
                    self.shuffle_order[pos - 1]
                } else if wrap {
                    self.shuffle_order[self.shuffle_order.len() - 1]
                } else {
                    return false;
                }
            };
            self.load_playlist_index(idx);
            return true;
        }
        let cur = self.selected_track.min(self.playlist.len() - 1);
        let idx = if cur > 0 {
            cur - 1
        } else if wrap {
            self.playlist.len() - 1
        } else {
            return false;
        };
        self.load_playlist_index(idx);
        true
    }

    /// Begin a cover crossfade from the currently displayed view to
    /// `incoming` (`None` = the default tile). Default→default is nothing
    /// visual and hard-clears instead.
    fn begin_cover_fade(&mut self, incoming: Option<(u32, u32, Vec<u8>)>) {
        if self.cover_resting.is_none() && incoming.is_none() {
            self.cover_fade = None;
            self.scene.clear_image();
            return;
        }
        self.cover_fade = Some(CoverFade {
            old: self.cover_resting.clone(),
            new: incoming,
            started: Instant::now(),
        });
    }

    /// Advance the cover crossfade one frame: composite old→new at the elapsed
    /// fraction into the scene's single image slot. The composited frame also
    /// becomes `cover_resting`, so a track change mid-fade starts the next
    /// transition from what is actually on screen.
    fn step_cover_fade(&mut self) {
        let Some(f) = self.cover_fade.as_ref() else { return };
        let t = f.started.elapsed().as_secs_f32() / COVER_FADE_SECS;
        if t >= 1.0 {
            let done = self.cover_fade.take().unwrap();
            match done.new {
                Some((w, h, rgba)) => {
                    self.scene.set_image(w, h, rgba.clone());
                    self.cover_resting = Some((w, h, rgba));
                }
                None => {
                    self.scene.clear_image();
                    self.cover_resting = None;
                }
            }
            return;
        }
        let buf = blend_covers(f.old.as_ref(), f.new.as_ref(), t);
        self.scene
            .set_image(COVER_TILE_SIDE, COVER_TILE_SIDE, buf.clone());
        self.cover_resting = Some((COVER_TILE_SIDE, COVER_TILE_SIDE, buf));
    }

    /// Load `playlist[idx]` into the engine.
    fn load_playlist_index(&mut self, idx: usize) {
        let Some(path) = self.playlist.get(idx).cloned() else {
            return;
        };
        self.selected_track = idx;
        self.on_new_track();
        self.scrub_frac = 0.0;
        self.track_meta = None;
        // The old cover STAYS on screen: the metadata-sync block in
        // build_scene owns the transition once the new track's meta lands.
        self.open_pending = Some((Instant::now(), path.display().to_string()));
        if let Some(a) = &self.audio {
            a.load_file(path);
        }
    }

    /// Open a file or folder without blocking the UI thread on SMB directory I/O.
    fn open_path(&mut self, path: PathBuf, append: bool, recursive_dirs: bool) {
        if is_supported_audio(&path) {
            if append && !self.playlist.is_empty() {
                self.playlist.push(path);
                self.sync_shuffle_state();
                return;
            }
            self.on_new_track();
            self.scrub_frac = 0.0;
            self.track_meta = None;
            // Old cover stays visible; build_scene's metadata sync fades it
            // into whatever the new track brings.
            if let Some(a) = &self.audio {
                a.load_file(path.clone());
            }
            self.open_pending = Some((Instant::now(), path.display().to_string()));
            self.playlist = vec![path.clone()];
            self.selected_track = 0;
            self.sync_shuffle_state();
            self.scan_gen = self.scan_gen.wrapping_add(1);
            let _ = self
                .scan_tx
                .send((self.scan_gen, ScanJob::SingleFile(path)));
        } else {
            self.scan_gen = self.scan_gen.wrapping_add(1);
            let _ = self.scan_tx.send((
                self.scan_gen,
                ScanJob::Path {
                    path,
                    append,
                    recursive: recursive_dirs,
                },
            ));
        }
    }

    /// Add-zone drop: append-plus-immediate-playback when a track is loaded,
    /// today's append-only behavior otherwise. Folder drops and unsupported
    /// files always take the normal open_path scan route.
    fn open_path_add_drop(&mut self, path: PathBuf, recursive_dirs: bool) {
        let current = self.audio.as_ref().and_then(|a| a.current_file());
        if !self.playlist.is_empty()
            && is_supported_audio(&path)
            && add_drop_plays_now(current.as_deref())
        {
            self.playlist.push(path);
            self.sync_shuffle_state();
            let new_idx = self.playlist.len() - 1;
            self.load_playlist_index(new_idx);
            return;
        }
        self.open_path(path, true, recursive_dirs);
    }

    /// Drain paths forwarded by later launches. An empty message is a plain
    /// "bring the player forward" request.
    fn poll_open_requests(&mut self) {
        let mut messages = Vec::new();
        if let Some(receiver) = &self.open_requests {
            while let Ok(message) = receiver.try_recv() {
                messages.push(message);
            }
        }
        for message in messages {
            if !message.is_empty() {
                perf::note(&format!("single-instance open {message}"));
                let path = PathBuf::from(&message);
                if path.exists() {
                    self.open_path(path, false, true);
                } else {
                    eprintln!("[single-instance] forwarded path not found: {message}");
                }
            } else {
                perf::note("single-instance focus request");
            }
            self.reveal_and_focus_main();
        }
    }

    fn reveal_and_focus_main(&mut self) {
        if let Some(renderer) = &mut self.renderer {
            renderer.request_reveal();
        } else if let Some(window) = &self.window {
            window.set_visible(true);
        }
        if let Some(window) = &self.window {
            let _ = window.set_minimized(false);
            nui::focus_window_now(window);
        }
        self.note_focus("main");
    }

    /// Keep the shell tray icon in sync with the Settings checkbox.
    fn sync_tray_icon(&mut self) {
        if self.sw_keep_tray {
            if let Some(window) = &self.window {
                nui::install_tray_icon(window, 1, "SoundApp R");
            }
        } else {
            nui::remove_tray_icon();
        }
    }

    /// Hide the player to the tray when the option is enabled; otherwise close.
    fn hide_to_tray(&mut self) {
        if let Some(renderer) = &mut self.renderer {
            renderer.cloak_and_hide();
        }
        perf::note("hidden to system tray");
    }

    fn request_close(&mut self, event_loop: &ActiveEventLoop) {
        if self.sw_keep_tray {
            self.hide_to_tray();
        } else {
            self.quitting = true;
            self.quit_at = Some(Instant::now());
            event_loop.exit();
        }
    }

    /// Poll background directory-scan completions and end-of-track transitions.
    fn poll_transport_and_scans(&mut self) {
        let mut redraw = false;
        while let Ok((gen, out)) = self.scan_rx.try_recv() {
            match out {
                ScanOutcome::SingleFile { files, idx } => {
                    if gen == self.scan_gen && !files.is_empty() {
                        self.playlist = files;
                        self.selected_track = idx;
                        self.sync_shuffle_state();
                        redraw = true;
                    }
                }
                ScanOutcome::Folder { files, append } => {
                    if files.is_empty() {
                        continue;
                    }
                    if append && !self.playlist.is_empty() {
                        self.playlist.extend(files);
                        self.sync_shuffle_state();
                        redraw = true;
                    } else if gen == self.scan_gen {
                        self.playlist = files;
                        self.selected_track = 0;
                        self.sync_shuffle_state();
                        self.load_playlist_index(0);
                        redraw = true;
                    }
                }
                ScanOutcome::MixerStems(files) => {
                    let opening = self.secondary_windows.is_visible("mixer")
                        || self.pending_window_toggles.contains(&"mixer");
                    if opening && !files.is_empty() {
                        self.open_mixer_with_paths(files);
                        redraw = true;
                    }
                }
                ScanOutcome::MixerDrop {
                    files,
                    replace_slot,
                } => {
                    self.apply_mixer_drop(files, replace_slot);
                    redraw = true;
                }
            }
        }
        let ended = self
            .audio
            .as_ref()
            .map(|a| a.take_track_ended())
            .unwrap_or(false);
        if ended {
            if self.advance_track(true) {
                self.animate = true;
            }
            redraw = true;
        }
        if redraw {
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
    }

    /// Resize the native window to the minimalist base size scaled by the UI zoom.
    fn apply_zoom(&mut self) {
        let Some(w) = &self.window else { return };
        let z = self.ui_zoom as f64;
        let (base_w, base_h) = self.base_size();
        // Lower the minimum first so the window is allowed to shrink.
        let min_h = if self.show_controls {
            Self::MIN_HEIGHT_WITH_CONTROLS
        } else {
            Self::HEIGHT_WITHOUT_CONTROLS
        };
        let _ = w.set_min_inner_size(Some(winit::dpi::LogicalSize::new(
            Self::WIDTH * z,
            min_h * z,
        )));
        let _ = w.request_inner_size(winit::dpi::LogicalSize::new(base_w * z, base_h * z));
        self.size = (w.inner_size().width, w.inner_size().height);
        w.request_redraw();
    }

    fn build_scene(&mut self, w: u32, h: u32, _dpi_scale: f32) {
        // The whole minimalist UI scales with the *actual* window size, so window
        // border/corner drags, `+/-/0` and `Ctrl+Wheel` (which resizes the window)
        // all produce the same smooth scaling.
        let base_h = if self.show_controls {
            Self::HEIGHT_WITH_CONTROLS as f32
        } else {
            Self::HEIGHT_WITHOUT_CONTROLS as f32
        };
        let scale = (w as f32 / Self::WIDTH as f32)
            .min(h as f32 / base_h)
            .max(0.5);
        let _ = self.text_sys.prepare_frame();
        // `--nui-rem-base: 16px` (nui-theme.css)
        let rem = (16.0 * scale).round();
        let theme = if self.dark_mode {
            Theme::dark(rem)
        } else {
            Theme::light(rem)
        };
        let win_bg = if theme.dark {
            rgb(44, 44, 44)
        } else {
            rgb(235, 235, 235)
        };
        self.scene.clear_all((win_bg.0, win_bg.1, win_bg.2));
        self.ui_state.begin_frame();

        let pw = w as f32;
        let ph = h as f32;

        // Pull the real metadata published by the decoder thread. Cover art
        // changes crossfade over COVER_FADE_SECS (previous view stays on
        // screen underneath); a `None` meta while a load is still pending is
        // the decoder's transient reset, not a final "no art" state.
        self.step_cover_fade();
        let meta = self.audio.as_ref().and_then(|a| a.metadata());
        if meta != self.track_meta {
            let transient_reset = meta.is_none() && self.open_pending.is_some();
            if !transient_reset {
                let incoming = meta.as_ref().and_then(|m| m.cover_rgba.clone());
                self.begin_cover_fade(incoming);
                let arrived = meta.is_some();
                self.track_meta = meta;
                if arrived {
                    if let Some((t0, file)) = self.open_pending.take() {
                        let ms = t0.elapsed().as_secs_f64() * 1000.0;
                        println!("[poc] track ready at {ms:.1} ms ({file})");
                        perf::note(&format!("track_ready_ms={ms:.1} file={file}"));
                    }
                }
            }
        }
        let playing = self.audio.as_ref().map(|a| a.is_playing()).unwrap_or(false);
        let (pos, dur) = self
            .audio
            .as_ref()
            .map(|a| (a.position_secs(), a.duration_secs()))
            .unwrap_or((0.0, 0.0));
        let file = self.audio.as_ref().and_then(|a| a.current_file());

        // Every string the metadata block shows, computed before the render borrow.
        let ext_lbl = file
            .as_ref()
            .and_then(|p| {
                p.extension()
                    .map(|e| e.to_string_lossy().to_ascii_uppercase())
            })
            .unwrap_or_default();
        let folder_lbl = file
            .as_ref()
            .and_then(|p| p.parent())
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "-".to_string());
        let file_lbl = file
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "-".to_string());
        let md = self.track_meta.clone();
        // Prefer FFmpeg's descriptive codec name; only prefix the extension when
        // the name does not already lead with it ("MP3 (MPEG audio layer 3)").
        let format_lbl = if ext_lbl.is_empty() {
            "-".to_string()
        } else if let Some(name) = md
            .as_ref()
            .map(|m| m.codec_long_name.clone())
            .filter(|s| !s.is_empty())
        {
            if name.to_ascii_uppercase().starts_with(&ext_lbl) {
                name
            } else {
                format!("{ext_lbl} ({name})")
            }
        } else if let Some(codec) = md
            .as_ref()
            .map(|m| m.codec.clone())
            .filter(|s| !s.is_empty())
        {
            format!("{ext_lbl} ({codec})")
        } else {
            format!("{ext_lbl} (unknown codec)")
        };
        let (sr, ch) = md
            .as_ref()
            .map(|m| (m.sample_rate, m.channels))
            .unwrap_or((0, 0));
        let ch_str = match ch {
            2 => "stereo".to_string(),
            1 => "mono".to_string(),
            n if n > 0 => format!("{n} ch"),
            _ => "-".to_string(),
        };
        let kbps = match md.as_ref().map(|m| m.bitrate).unwrap_or(0) {
            0 => {
                // Approximate when FFmpeg reports no bitrate: file bytes over duration.
                // The size was sampled once by the background metadata probe, so no
                // SMB stat happens on the render path.
                let bytes = md.as_ref().map(|m| m.file_size_bytes).unwrap_or(0);
                if dur > 0.1 {
                    (bytes as f64 * 8.0 / dur / 1000.0).round() as i32
                } else {
                    0
                }
            }
            b => b / 1000,
        };
        let format2_lbl = if sr > 0 {
            if kbps > 0 {
                format!("{kbps} kbps / {ch_str} / {sr} Hz")
            } else {
                format!("{ch_str} / {sr} Hz")
            }
        } else {
            String::new()
        };
        let artist_lbl = md
            .as_ref()
            .map(|m| m.artist.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "-".to_string());
        let album_lbl = md
            .as_ref()
            .map(|m| m.album.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "-".to_string());
        let title_lbl = md
            .as_ref()
            .map(|m| m.title.clone())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                file.as_ref()
                    .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
            })
            .unwrap_or_else(|| "-".to_string());
        let total_tracks = self.playlist.len().max(1);
        let counter_lbl = format!(
            "{} of {}",
            (self.selected_track + 1).min(total_tracks),
            total_tracks
        );
        let vol_pct = (self.master_vol * 100.0).round() as i32;
        let db_str = if self.master_vol <= 0.0001 {
            "-inf dB".to_string()
        } else {
            format!("{:+.1} dB", 20.0 * self.master_vol.log10())
        };
        let vol_lbl = format!("Volume: {vol_pct}% ({db_str})");

        let monitoring_visible = self.secondary_windows.is_visible("monitoring");
        let parameters_visible = self.secondary_windows.is_visible("parameters");
        let parameters_icon = if self.chk_locked { ICON_LOCK } else { ICON_TUNE };
        if let Some(a) = &self.audio {
            a.set_dsp_enabled(monitoring_visible);
        }

        let mut intents = MainIntents::default();
        let pending_drop: Option<(PathBuf, DropZone)>;

        {
            let mut ui = Ui::new(
                &mut self.ui_state,
                &mut self.scene,
                &mut self.text_sys,
                self.font_engine,
                theme,
            );
            ui.viewport = (pw, ph);

            // ---- Window surface: the window itself IS the flat player card ----
            let border_col = if theme.dark {
                rgb(60, 60, 60)
            } else {
                rgb(200, 200, 200)
            };
            ui.scene.rect(0.0, 0.0, pw, ph, win_bg);
            ui.scene.rounded_rect(
                0.5,
                0.5,
                (pw - 1.0).max(0.0),
                (ph - 1.0).max(0.0),
                0.0,
                (0, 0, 0, 0),
                border_col,
                1.0,
            );

            // ---- Row 1: minimalist top bar ----
            let top_h = (28.0 * scale).round();
            ui.scene.rect(
                0.0,
                0.0,
                pw,
                top_h,
                if theme.dark {
                    rgb(35, 35, 35)
                } else {
                    rgb(226, 226, 226)
                },
            );
            ui.scene.rect(
                0.0,
                top_h - 1.0,
                pw,
                1.0,
                if theme.dark {
                    rgba(255, 255, 255, 20)
                } else {
                    rgba(0, 0, 0, 35)
                },
            );

            let icon_px = 14.0 * scale;
            let ghost_hover = if theme.dark {
                rgba(255, 255, 255, 15)
            } else {
                rgba(0, 0, 0, 15)
            };
            let top_fg_active = if theme.dark {
                rgb(255, 255, 255)
            } else {
                rgb(15, 15, 15)
            };
            let top_fg_mid = if theme.dark {
                rgb(185, 185, 185)
            } else {
                rgb(55, 55, 55)
            };
            let top_fg_dim = if theme.dark {
                rgb(115, 115, 115)
            } else {
                rgb(135, 135, 135)
            };
            if ghost_icon_btn(
                &mut ui,
                601,
                Rect::new(0.0, 0.0, top_h, top_h),
                if playing { ICON_PAUSE } else { ICON_PLAY },
                if playing { top_fg_active } else { top_fg_mid },
                icon_px,
                ghost_hover,
            ) {
                intents.toggle_play = true;
            }
            if ghost_icon_btn(
                &mut ui,
                602,
                Rect::new(top_h, 0.0, top_h, top_h),
                ICON_LOOP,
                if self.loop_enabled {
                    top_fg_active
                } else {
                    top_fg_dim
                },
                icon_px,
                ghost_hover,
            ) {
                intents.loop_toggled = true;
            }
            if ghost_icon_btn(
                &mut ui,
                603,
                Rect::new(top_h * 2.0, 0.0, top_h, top_h),
                ICON_SHUFFLE,
                if self.shuffle_enabled {
                    top_fg_active
                } else {
                    top_fg_dim
                },
                icon_px,
                ghost_hover,
            ) {
                intents.shuffle_toggled = true;
            }

            let counter_fs = (12.5 * scale).round();
            ui.scene.text(
                ui.text_sys,
                FontFace::Regular,
                ui.font_engine,
                &counter_lbl,
                (92.0 * scale).round(),
                (top_h * 0.5 + counter_fs * 0.34).round(),
                counter_fs,
                if theme.dark {
                    rgb(210, 210, 210)
                } else {
                    rgb(25, 25, 25)
                },
            );

            let right_btn_w = (34.0 * scale).round();
            if ghost_icon_btn(
                &mut ui,
                604,
                Rect::new(pw - right_btn_w * 2.0, 0.0, right_btn_w, top_h),
                ICON_SETTINGS,
                top_fg_mid,
                icon_px,
                ghost_hover,
            ) {
                intents.toggle_settings = true;
            }
            if ghost_icon_btn(
                &mut ui,
                605,
                Rect::new(pw - right_btn_w, 0.0, right_btn_w, top_h),
                ICON_CLOSE,
                top_fg_mid,
                icon_px,
                rgb(145, 58, 7),
            ) {
                intents.close = true;
            }

            // ---- Row 2: cover tile + metadata rows ----
            let cover_s = 126.0 * scale;
            let cover = Rect::new(
                (14.0 * scale).round(),
                (42.0 * scale).round(),
                cover_s,
                cover_s,
            );
            let has_track = file.is_some();
            // The scene's image slot is live during a fade even when the
            // incoming view is the default tile (the fade composites it).
            let showing_image = self.cover_fade.is_some() || self.cover_resting.is_some();
            ui.scene.push_clip(cover.x, cover.y, cover.w, cover.h);
            if showing_image {
                ui.scene.image_quad(cover.x, cover.y, cover.w, cover.h);
                // While a fade involves the default view on one side, the
                // volume glyph rides on top with that side's weight so it
                // fades with the tile instead of popping when the procedural
                // fallback takes over at the end.
                if let Some(f) = &self.cover_fade {
                    let ft =
                        (f.started.elapsed().as_secs_f32() / COVER_FADE_SECS).clamp(0.0, 1.0);
                    let w = if f.new.is_none() {
                        ft
                    } else if f.old.is_none() {
                        1.0 - ft
                    } else {
                        0.0
                    };
                    if w > 0.005 && has_track {
                        ui.scene.icon_centered(
                            ui.text_sys,
                            ICON_VOLUME,
                            (cover.x + cover.w * 0.5).round(),
                            (cover.y + cover.h * 0.5).round(),
                            (58.0 * scale).round(),
                            rgba(0, 0, 0, (140.0 * w) as u8),
                        );
                    }
                }
            } else {
                ui.scene.rect(
                    cover.x,
                    cover.y,
                    cover.w,
                    cover.h,
                    if has_track {
                        rgb(34, 150, 195)
                    } else {
                        rgb(60, 60, 60)
                    },
                );
                ui.scene.icon_centered(
                    ui.text_sys,
                    ICON_VOLUME,
                    (cover.x + cover.w * 0.5).round(),
                    (cover.y + cover.h * 0.5).round(),
                    (58.0 * scale).round(),
                    if has_track {
                        rgba(0, 0, 0, 140)
                    } else {
                        rgba(255, 255, 255, 55)
                    },
                );
            }
            // Diagonal -45° format ribbon, hidden while the cursor is on the cover.
            // The endpoints sit well outside the clipped square so the capsule caps
            // are cut off and the sash runs edge-to-edge.
            if !ext_lbl.is_empty() && !cover.contains(ui.state.cursor.0, ui.state.cursor.1) {
                let x1 = cover.x + cover.w;
                let y1 = cover.y + cover.h;
                let d = cover.w * 0.42;
                let ext = 32.0 * scale;
                ui.scene.line(
                    x1 - d - ext,
                    y1 + ext,
                    x1 + ext,
                    y1 - d - ext,
                    22.0 * scale,
                    ribbon_color(&ext_lbl),
                );
                ui.scene.text_rotated_ccw45(
                    ui.text_sys,
                    FontFace::SemiBold,
                    ui.font_engine,
                    &ext_lbl,
                    x1 - d * 0.5,
                    y1 - d * 0.5,
                    (11.0 * scale).round(),
                    rgb(255, 255, 255),
                );
            }
            ui.scene.pop_clip();

            let rows_x = (156.0 * scale).round();
            let label_w = 48.0 * scale;
            let row_gap = 10.0 * scale;
            let value_w = (pw - 14.0 * scale - (rows_x + label_w + row_gap)).max(10.0);
            let mfs = (11.5 * scale).round();
            let step = 15.5 * scale;
            let gap_v = 7.0 * scale;
            let mut ry = 42.0 * scale;
            let baseline = |y: f32| (y + mfs * 0.95).round();
            draw_meta_row(
                &mut ui,
                rows_x,
                label_w,
                row_gap,
                baseline(ry),
                mfs,
                "Folder:",
                &folder_lbl,
                value_w,
            );
            ry += step;
            draw_meta_row(
                &mut ui,
                rows_x,
                label_w,
                row_gap,
                baseline(ry),
                mfs,
                "File:",
                &file_lbl,
                value_w,
            );
            ry += step + gap_v;
            draw_meta_row(
                &mut ui,
                rows_x,
                label_w,
                row_gap,
                baseline(ry),
                mfs,
                "Format:",
                &format_lbl,
                value_w,
            );
            ry += step;
            draw_meta_row(
                &mut ui,
                rows_x,
                label_w,
                row_gap,
                baseline(ry),
                mfs,
                "",
                &format2_lbl,
                value_w,
            );
            ry += step + gap_v;
            draw_meta_row(
                &mut ui,
                rows_x,
                label_w,
                row_gap,
                baseline(ry),
                mfs,
                "Artist:",
                &artist_lbl,
                value_w,
            );
            ry += step;
            draw_meta_row(
                &mut ui,
                rows_x,
                label_w,
                row_gap,
                baseline(ry),
                mfs,
                "Album:",
                &album_lbl,
                value_w,
            );
            ry += step;
            draw_meta_row(
                &mut ui,
                rows_x,
                label_w,
                row_gap,
                baseline(ry),
                mfs,
                "Title:",
                &title_lbl,
                value_w,
            );

            // ---- Row 3: flat progress bar + status line ----
            let bar_x = (14.0 * scale).round();
            let bar_w = (pw - 28.0 * scale).max(1.0);
            let bar_y = (182.0 * scale).round();
            let bar_h = (5.0 * scale).round().max(4.0);
            let is_finite = dur > 0.0 && dur < 360_000.0;
            let scrub_id = 610u32;
            if ui.state.active_id != Some(scrub_id) && is_finite {
                self.scrub_frac = (pos / dur).clamp(0.0, 1.0) as f32;
            }
            let track_bar_bg = if theme.dark {
                rgb(68, 68, 68)
            } else {
                rgb(180, 180, 180)
            };
            let track_bar_fill = if theme.dark {
                rgb(180, 180, 180)
            } else {
                rgb(50, 50, 50)
            };
            let track = Rect::new(bar_x, bar_y, bar_w, bar_h);
            let hit = Rect::new(
                0.0,
                bar_y - 14.0 * scale,
                pw,
                14.0 * scale + bar_h + 26.0 * scale,
            );
            ui.flat_slider(
                scrub_id,
                track,
                hit,
                &mut self.scrub_frac,
                0.0,
                1.0,
                [track_bar_bg, track_bar_fill],
            );
            if ui.state.active_id == Some(scrub_id) && ui.state.mouse_released && is_finite {
                intents.seek_to = Some(self.scrub_frac as f64 * dur);
            }

            let sfs = (11.0 * scale).round();
            let sy = (bar_y + bar_h + 6.0 * scale + sfs).round();
            let time_col = if theme.dark {
                rgb(190, 190, 190)
            } else {
                rgb(35, 35, 35)
            };
            let vol_txt_col = if theme.dark {
                rgb(130, 130, 130)
            } else {
                rgb(100, 100, 100)
            };
            ui.scene.text(
                ui.text_sys,
                FontFace::Regular,
                ui.font_engine,
                &fmt_time(pos),
                bar_x,
                sy,
                sfs,
                time_col,
            );
            let vw = ui
                .text_sys
                .measure(FontFace::Regular, ui.font_engine, &vol_lbl, sfs);
            ui.scene.text(
                ui.text_sys,
                FontFace::Regular,
                ui.font_engine,
                &vol_lbl,
                (pw * 0.5 - vw * 0.5).round(),
                sy,
                sfs,
                vol_txt_col,
            );
            let dur_lbl = fmt_time(dur);
            let dw = ui
                .text_sys
                .measure(FontFace::Regular, ui.font_engine, &dur_lbl, sfs);
            ui.scene.text(
                ui.text_sys,
                FontFace::Regular,
                ui.font_engine,
                &dur_lbl,
                (bar_x + bar_w - dw).round(),
                sy,
                sfs,
                time_col,
            );

            // ---- Row 4: bottom controls bar (optional) ----
            if self.show_controls {
                let ctrl_h = 55.0 * scale;
                let bar_top = ph - ctrl_h;
                ui.scene.rect(
                    0.0,
                    bar_top,
                    pw,
                    ctrl_h,
                    if theme.dark {
                        rgb(37, 37, 37)
                    } else {
                        rgb(220, 220, 220)
                    },
                );
                ui.scene.rect(
                    0.0,
                    bar_top,
                    pw,
                    1.0,
                    if theme.dark {
                        rgb(58, 58, 58)
                    } else {
                        rgb(180, 180, 180)
                    },
                );

                let bs = (28.0 * scale).round();
                let by = (bar_top + (ctrl_h - bs) * 0.5).round();
                let bgap = (6.0 * scale).round();
                let bleft = (14.0 * scale).round();
                if player_icon_btn(
                    &mut ui,
                    620,
                    Rect::new(bleft, by, bs, bs),
                    ICON_EQ,
                    monitoring_visible,
                    scale,
                ) {
                    intents.toggle_monitoring = true;
                }
                if player_icon_btn(
                    &mut ui,
                    621,
                    Rect::new(bleft + bs + bgap, by, bs, bs),
                    parameters_icon,
                    parameters_visible,
                    scale,
                ) {
                    intents.toggle_parameters = true;
                }

                // Volume: speaker glyph + flat horizontal bar.
                let vx = (14.0 * scale + bs * 2.0 + bgap + 18.0 * scale).round();
                ui.scene.icon_centered(
                    ui.text_sys,
                    ICON_VOLUME,
                    vx + 7.5 * scale,
                    by + bs * 0.5,
                    (15.0 * scale).round(),
                    if theme.dark {
                        rgb(185, 185, 185)
                    } else {
                        rgb(50, 50, 50)
                    },
                );
                let vol_x = (vx + 22.0 * scale).round();
                let vol_w = (96.0 * scale).round();
                let vol_h = (5.0 * scale).round().max(4.0);
                let vol_y = by + bs * 0.5 - vol_h * 0.5;
                let vol_id = 640u32;
                let vol_hit = Rect::new(vx, by, vol_x + vol_w - vx, bs);
                if ui.flat_slider(
                    vol_id,
                    Rect::new(vol_x, vol_y, vol_w, vol_h),
                    vol_hit,
                    &mut self.master_vol,
                    0.0,
                    1.0,
                    [track_bar_bg, track_bar_fill],
                ) {
                    intents.vol_changed = true;
                }

                // Right transport group.
                let group_w = bs * 5.0 + bgap * 4.0;
                let mut tx = pw - 14.0 * scale - group_w;
                let defs = [
                    (630u32, ICON_LOOP, self.loop_enabled),
                    (631u32, ICON_SHUFFLE, self.shuffle_enabled),
                    (632u32, ICON_PREV, false),
                    (633u32, if playing { ICON_PAUSE } else { ICON_PLAY }, false),
                    (634u32, ICON_NEXT, false),
                ];
                for (id, icon, active) in defs {
                    if player_icon_btn(
                        &mut ui,
                        id,
                        Rect::new(tx.round(), by, bs, bs),
                        icon,
                        active,
                        scale,
                    ) {
                        match id {
                            630 => intents.loop_toggled = true,
                            631 => intents.shuffle_toggled = true,
                            632 => intents.prev = true,
                            633 => intents.toggle_play = true,
                            _ => intents.next = true,
                        }
                    }
                    tx += bs + bgap;
                }
            }

            // ---- Multi-zone OS drag-and-drop overlay ----
            pending_drop = draw_drop_overlay(&mut ui, pw, ph, scale);

            // Append any open dropdown popups onto the top of the draw list!
            ui.scene.finish_frame();
            ui.state.end_frame();
        }

        // --- Apply the main-window intents outside the render borrow.
        if intents.loop_toggled {
            self.loop_enabled = !self.loop_enabled;
            if let Some(a) = &self.audio {
                a.set_loop(self.loop_enabled);
            }
        }
        if intents.shuffle_toggled {
            self.shuffle_enabled = !self.shuffle_enabled;
            if self.shuffle_enabled {
                self.shuffle_history.clear();
                self.rebuild_shuffle_order();
            } else {
                self.shuffle_order.clear();
                self.shuffle_history.clear();
            }
        }
        if intents.toggle_controls {
            self.show_controls = !self.show_controls;
            self.apply_zoom();
        }
        if intents.toggle_settings {
            self.pending_window_toggles.push("settings");
        }
        if intents.toggle_monitoring {
            self.pending_window_toggles.push("monitoring");
        }
        if intents.toggle_parameters {
            self.pending_window_toggles.push("parameters");
        }
        if intents.toggle_play {
            if let Some(a) = &self.audio {
                self.animate = a.toggle_play();
            }
        }
        if intents.prev {
            self.retreat_track(true);
        }
        if intents.next {
            self.advance_track(true);
        }
        if let Some(secs) = intents.seek_to {
            if let Some(a) = &self.audio {
                a.seek(secs);
            }
        }
        if intents.vol_changed {
            if let Some(a) = &self.audio {
                a.set_volume(self.master_vol);
            }
        }
        if let Some((path, zone)) = pending_drop {
            perf::note(&format!(
                "drop zone={zone:?} cursor=({:.0},{:.0}) path={}",
                self.ui_state.cursor.0,
                self.ui_state.cursor.1,
                path.display()
            ));
            // Ctrl held → flat (non-recursive) folder collection.
            let recursive = !self.modifiers.control_key();
            match zone {
                DropZone::Add => self.open_path_add_drop(path, recursive),
                DropZone::Replace => self.open_path(path, false, recursive),
                DropZone::Mixer => {
                    // Main playback stops the moment the files land: the scan
                    // is async and open_mixer_with_paths (which pauses again,
                    // idempotently) only fires once it completes — until then
                    // both players would sound at once.
                    if let Some(main) = &self.audio {
                        main.pause();
                    }
                    self.scan_gen = self.scan_gen.wrapping_add(1);
                    let _ = self
                        .scan_tx
                        .send((self.scan_gen, ScanJob::MixerPath { path, recursive }));
                    if !self.secondary_windows.is_visible("mixer") {
                        self.pending_window_toggles.push("mixer");
                    }
                }
            }
        }
        if intents.close {
            if self.sw_keep_tray {
                self.hide_to_tray();
            } else {
                self.quitting = true;
                self.quit_at = Some(Instant::now());
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
        }
    }

    /// Active theme's window base color — the pre-first-frame class brush and
    /// fill use it, so a light-mode launch never flashes the dark color (and
    /// no launch flashes the white default brush).
    fn theme_base(&self) -> nui::Color {
        if self.dark_mode {
            Theme::dark(self.ui_zoom).base
        } else {
            Theme::light(self.ui_zoom).base
        }
    }

    /// Emit one frame-pacing summary line and reset the window counters.
    fn flush_pacing(&mut self, tag: &str) {
        if self.pace_frames > 0 {
            perf::note(&format!(
                "frame_pacing frames={} slow_gt66ms={} window={tag}",
                self.pace_frames, self.pace_slow
            ));
            self.pace_frames = 0;
            self.pace_slow = 0;
        }
    }

    fn refresh_drag_cursor(&mut self) {
        let Some(window) = self.window.as_ref() else {
            return;
        };
        if let Some(pos) = nui::cursor_position_in_window(window) {
            self.ui_state.cursor = pos;
        }
    }

    fn sync_file_drag(&mut self) {
        if self.ui_state.hovered_file.is_none() {
            return;
        }
        if !nui::primary_mouse_button_down() {
            self.ui_state.set_hovered_file(None);
            return;
        }
        self.refresh_drag_cursor();
        let (w, h) = self.size;
        let (x, y) = self.ui_state.cursor;
        if x < 0.0 || y < 0.0 || x >= w as f32 || y >= h as f32 {
            self.ui_state.set_hovered_file(None);
        }
    }

    fn render(&mut self) {
        self.poll_transport_and_scans();
        let Some(window) = self.window.clone() else {
            return;
        };
        let (w, h) = self.size;
        if w == 0 || h == 0 {
            return;
        }
        self.sync_file_drag();
        let scale = window.scale_factor() as f32;

        if self.nopaint {
            if let Some(renderer) = &mut self.renderer {
                renderer.present_nopaint(w, h, (30, 30, 30));
            }
            return;
        }

        self.build_scene(w, h, scale);
        if let Some(renderer) = &mut self.renderer {
            // With --open-window the screenshot belongs to that secondary window.
            let waiting_for_meta =
                self.audio.is_some() && !self.playlist.is_empty() && self.track_meta.is_none();
            let shot = if self.open_window_spec.is_some() || waiting_for_meta {
                None
            } else {
                self.screenshot_path.take()
            };
            renderer.render(w, h, &self.scene, &mut self.text_sys, shot.as_deref());
            if let Some(path) = shot {
                println!("[poc] saved screenshot to {}", path);
            }
        }

        // Frame pacing (UI responsiveness): 10 s windows counting frames and
        // intervals that missed the 15 Hz budget. A >500 ms gap is a fresh
        // burst after idle, not a stall.
        let now = Instant::now();
        if let Some(last) = self.last_frame_at {
            let dt = now.duration_since(last);
            if dt < Duration::from_millis(500) {
                self.pace_frames += 1;
                if dt > Duration::from_millis(66) {
                    self.pace_slow += 1;
                }
            }
        }
        self.last_frame_at = Some(now);
        if now.duration_since(self.pace_at) >= Duration::from_secs(10) {
            self.flush_pacing("10s");
            self.pace_at = now;
        }

        if !self.reported {
            self.reported = true;
            let bname = match self.renderer.as_ref().map(|r| r.active) {
                Some(BackendKind::Gpu) => "gpu-gl33",
                Some(BackendKind::Cpu) => "cpu-softbuffer",
                None => "none",
            };
            println!(
                "[poc] first frame at {:.1} ms (window {}x{}, scale {:.2}, backend {})",
                self.start.elapsed().as_secs_f64() * 1000.0,
                w,
                h,
                scale,
                bname
            );
            perf::note(&format!(
                "first_frame_ms={:.1} backend={bname}",
                self.start.elapsed().as_secs_f64() * 1000.0
            ));
            #[cfg(target_os = "windows")]
            if let Some((cw, ch)) = nui::win32_client_size(&window) {
                println!("[poc] win32 client {cw}x{ch} vs inner {w}x{h}");
                perf::note(&format!("client={cw}x{ch} inner={w}x{h}"));
            }
        }
        if !self.audio_reported {
            if let Some(a) = &self.audio {
                if a.started.load(std::sync::atomic::Ordering::Relaxed) {
                    self.audio_reported = true;
                    println!(
                        "[poc] audio started at {:.1} ms ({} Hz, {} ch)",
                        self.start.elapsed().as_secs_f64() * 1000.0,
                        a.rate,
                        a.channels
                    );
                    perf::note(&format!(
                        "audio_started_ms={:.1} rate={} ch={}",
                        self.start.elapsed().as_secs_f64() * 1000.0,
                        a.rate,
                        a.channels
                    ));
                }
            }
        }
    }

    /// Record a window as the most recently focused SoundApp window.
    fn note_focus(&mut self, key: &'static str) {
        self.focus_ring.retain(|entry| *entry != key);
        self.focus_ring.push_front(key);
        while self.focus_ring.len() > 8 {
            self.focus_ring.pop_back();
        }
    }

    /// After hiding `hidden`, focus the newest still-visible window in the ring
    /// (falling back to the player). Without this, closing a secondary window can
    /// leave the process with no foreground window and keyboard shortcuts stop.
    fn restore_focus_after_hide(&mut self, hidden: &'static str) {
        self.focus_ring.retain(|entry| *entry != hidden);
        let target = self
            .focus_ring
            .iter()
            .copied()
            .find(|key| {
                if *key == "main" {
                    self.window.is_some()
                } else {
                    self.secondary_windows.is_visible(key)
                }
            })
            .unwrap_or("main");
        if target == "main" {
            if let Some(window) = &self.window {
                nui::focus_window_now(window);
            }
        } else if let Some(managed) = self
            .secondary_windows
            .windows
            .iter()
            .find(|window| window.key == target)
        {
            nui::focus_window_now(&managed.window);
        }
    }

    /// Execute queued secondary-window show/hide toggles. Toggles are queued from the
    /// keyboard shortcuts and the main window's toggle buttons (both handled during
    /// scene build, where no `event_loop` exists) and executed here from the primary
    /// window's `RedrawRequested` arm in `window_event`.
    /// True while a text input owns keyboard focus (global shortcuts must
    /// never steal typing). `focused_id` alone is NOT this — every clickable
    /// widget takes focus on click; only ids registered here count as text
    /// inputs. soundapp has none today; ids join this list as fields appear.
    fn any_text_input_focused(&self) -> bool {
        const TEXT_INPUT_IDS: &[u32] = &[];
        let is_text = |id: Option<u32>| id.is_some_and(|id| TEXT_INPUT_IDS.contains(&id));
        is_text(self.ui_state.focused_id)
            || self
                .secondary_windows
                .windows
                .iter()
                .any(|w| is_text(w.ui_state.focused_id))
    }

    /// One key pass that runs BEFORE the per-window partition, so its
    /// vocabulary works no matter which window is focused. Guards: no
    /// Ctrl/Alt/OS-logo (Ctrl+P & co. must stay free), no text focus.
    fn handle_global_shortcut(
        &mut self,
        event_loop: &ActiveEventLoop,
        event: &winit::event::KeyEvent,
    ) -> bool {
        // Auto-repeat fires Pressed ~30x/s while the key is held; without this
        // guard a slightly long press toggles the window two or three times
        // (the "opens twice / sometimes doesn't open" unreliability).
        if event.repeat {
            return false;
        }
        let Key::Character(ch) = &event.logical_key else {
            return false;
        };
        let m = &self.modifiers;
        if m.control_key() || m.alt_key() || self.any_text_input_focused() {
            return false;
        }
        match global_action_for(ch) {
            Some(GlobalAction::ToggleWindow(key)) => {
                self.pending_window_toggles.push(key);
                self.process_window_toggles(event_loop);
            }
            Some(GlobalAction::ToggleTheme) => {
                self.dark_mode = !self.dark_mode;
                self.secondary_windows.request_redraw_all();
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            Some(GlobalAction::ToggleControls) => {
                self.show_controls = !self.show_controls;
                self.apply_zoom();
            }
            Some(GlobalAction::RevealInExplorer) => self.reveal_current_file_in_explorer(),
            None => return false,
        }
        true
    }

    /// `I`: show the currently playing file in the OS file browser, falling
    /// back to the selected playlist entry when no engine is running.
    fn reveal_current_file_in_explorer(&self) {
        let path = self
            .audio
            .as_ref()
            .and_then(|a| a.current_file())
            .or_else(|| self.playlist.get(self.selected_track).cloned());
        if let Some(path) = path {
            reveal_in_explorer(&path);
        }
    }

    /// Parameters-window close rule: transient edits reset to defaults unless
    /// "Lock Settings" is active — one flag, one meaning: locked parameters
    /// are sticky across track changes AND window close. Pushed to the engine
    /// exactly like `on_new_track`.
    fn on_parameters_window_closed(&mut self) {
        if self.chk_locked {
            return;
        }
        self.params = ParamState::default();
        self.tracker_mutes.clear();
        self.sync_pipeline_and_params();
        if let Some(a) = &self.audio {
            a.set_tracker_mutes(self.tracker_mutes.clone());
        }
    }

    fn process_window_toggles(&mut self, event_loop: &ActiveEventLoop) {
        if self.pending_window_toggles.is_empty() {
            return;
        }
        let pending = std::mem::take(&mut self.pending_window_toggles);
        for key in pending {
            let Some(spec) = window_spec_by_key(key) else {
                continue;
            };
            // `show()` requests reveal + redraw itself, so no synchronous paint here.
            let backend = self.secondary_backend(key);
            let was_visible = self.secondary_windows.is_visible(key);
            let existed = self.secondary_windows.windows.iter().any(|w| w.key == key);
            let spec = self.adjusted_window_spec(spec);
            self.secondary_windows
                .toggle(event_loop, &spec, backend);
            // First creation this session: place at the saved position (when
            // it reaches a connected monitor). Later toggles never reposition
            // — the user's drags this session win.
            if !existed && self.secondary_windows.is_visible(key) {
                if let Some(g) = self.saved_geometry(key) {
                    if Self::saved_rect_reachable(event_loop, g) {
                        if let Some(w) = self.secondary_windows.find_by_key_mut(key) {
                            w.window.set_outer_position(
                                winit::dpi::LogicalPosition::new(g.x as f64, g.y as f64),
                            );
                        }
                    }
                }
            }
            if key == "parameters" {
                if was_visible {
                    // Toggled off: the same close rule as the other paths.
                    self.on_parameters_window_closed();
                } else {
                    self.parameters_window_height = None;
                }
            }
            if key == "mixer" {
                if was_visible {
                    self.close_mixer();
                } else if self.mixer_audio.is_none() {
                    self.open_mixer_from_playlist();
                }
            }
            if was_visible {
                self.restore_focus_after_hide(key);
            } else {
                self.note_focus(key);
            }
        }
        // The toggle buttons' Primary/Outline variants follow visibility — refresh.
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// Event handler for a managed secondary window: `Escape` / `CloseRequested` HIDE
    /// it (the primary window owns the process lifetime), input updates that window's
    /// private `UiState`, and `RedrawRequested` renders it against the shared
    /// `TextSystem`.
    fn secondary_window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        id: WindowId,
        event: WindowEvent,
    ) {
        // `key` is `&'static str` (Copy) so resolving it first never holds a borrow.
        let Some(key) = self
            .secondary_windows
            .windows
            .iter()
            .find(|w| w.window.id() == id)
            .map(|w| w.key)
        else {
            return;
        };

        match event {
            WindowEvent::CloseRequested => {
                self.secondary_windows.hide(key);
                if key == "parameters" {
                    self.parameters_window_height = None;
                    self.on_parameters_window_closed();
                }
                if key == "mixer" {
                    self.close_mixer();
                }
                self.restore_focus_after_hide(key);
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            WindowEvent::Resized(sz) => {
                if let Some(w) = self.secondary_windows.find_by_key_mut(key) {
                    w.size = (sz.width, sz.height);
                    w.window.request_redraw();
                }
            }
            WindowEvent::Focused(true) => {
                self.note_focus(key);
                nui::restore_taskbar_group_if_needed();
            }
            WindowEvent::CursorMoved { position, .. } => {
                if let Some(w) = self.secondary_windows.find_by_key_mut(key) {
                    w.ui_state.cursor = (position.x as f32, position.y as f32);
                    nui::update_resize_cursor(&w.window, w.ui_state.cursor, w.size);
                    w.window.request_redraw();
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => y,
                    winit::event::MouseScrollDelta::PixelDelta(p) => (p.y as f32) / 40.0,
                };
                if dy.abs() > 0.01 {
                    if let Some(w) = self.secondary_windows.find_by_key_mut(key) {
                        w.ui_state.wheel_y += dy;
                        w.window.request_redraw();
                    }
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if button == MouseButton::Left {
                    if state == ElementState::Pressed
                        && key == "mixer"
                        && self.mixer_sync_debug
                    {
                        let cursor = self
                            .secondary_windows
                            .find_by_key_mut(key)
                            .map(|w| w.ui_state.cursor);
                        if let Some(cursor) = cursor {
                            let on_close = self
                                .mixer_sync_close_rect
                                .map(|r| r.contains(cursor.0, cursor.1))
                                .unwrap_or(false);
                            let on_copy = self
                                .mixer_sync_copy_rect
                                .map(|r| r.contains(cursor.0, cursor.1))
                                .unwrap_or(false);
                            if on_close {
                                self.mixer_sync_debug = false;
                                self.mixer_sync_copied_at = None;
                                self.mixer_sync_close_rect = None;
                                self.mixer_sync_copy_rect = None;
                                if let Some(w) = self.secondary_windows.find_by_key_mut(key) {
                                    w.window.request_redraw();
                                }
                                return;
                            }
                            if on_copy {
                                if let Some(engine) = &self.mixer_audio {
                                    let snapshot = engine.sync_snapshot();
                                    nui::clipboard_set_text(&mixer_sync_snapshot_text(
                                        &snapshot,
                                        &self.mixer_stems,
                                    ));
                                }
                                self.mixer_sync_copied_at = Some(Instant::now());
                                if let Some(w) = self.secondary_windows.find_by_key_mut(key) {
                                    w.window.request_redraw();
                                }
                                return;
                            }
                        }
                    }
                    if let Some(w) = self.secondary_windows.find_by_key_mut(key) {
                        match state {
                            ElementState::Pressed => {
                                // The resize border wins over the header drag:
                                // both live in the top-left corner region.
                                if nui::try_start_window_resize(
                                    &w.window,
                                    w.ui_state.cursor,
                                    w.size,
                                ) {
                                    return;
                                }
                                // Drag by the empty header area (left of the close button),
                                // exactly like the primary window's undecorated header.
                                let rem = 16.0 * w.window.scale_factor() as f32;
                                let hdr_h = (rem * 3.0).round();
                                if w.ui_state.cursor.1 < hdr_h
                                    && w.ui_state.cursor.0 < w.size.0 as f32 - rem * 2.9
                                {
                                    let win = w.window.clone();
                                    nui::drag_window_live(&win);
                                } else {
                                    w.ui_state.on_mouse_press();
                                    w.window.request_redraw();
                                }
                            }
                            ElementState::Released => {
                                w.ui_state.on_mouse_release();
                                w.window.request_redraw();
                            }
                        }
                    }
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers.state();
            }
            WindowEvent::HoveredFile(path) => {
                if let Some(w) = self.secondary_windows.find_by_key_mut(key) {
                    w.ui_state.hovered_file = Some(path);
                    w.window.request_redraw();
                }
            }
            WindowEvent::HoveredFileCancelled => {
                if let Some(w) = self.secondary_windows.find_by_key_mut(key) {
                    w.ui_state.hovered_file = None;
                    w.window.request_redraw();
                }
            }
            WindowEvent::DroppedFile(path) => {
                if key == "mixer" {
                    let cursor = self
                        .secondary_windows
                        .find_by_key_mut(key)
                        .map(|w| w.ui_state.cursor)
                        .unwrap_or((-1.0, -1.0));
                    let target = if self
                        .mixer_add_target
                        .map(|r| r.contains(cursor.0, cursor.1))
                        .unwrap_or(false)
                    {
                        Some(None)
                    } else {
                        self.mixer_strip_targets
                            .iter()
                            .find(|(r, _)| r.contains(cursor.0, cursor.1))
                            .map(|(_, slot)| Some(*slot))
                    };
                    if let Some(replace_slot) = target {
                        let _ = self.scan_tx.send((
                            self.scan_gen,
                            ScanJob::MixerDrop {
                                path,
                                replace_slot,
                                recursive: true,
                            },
                        ));
                    }
                }
                if let Some(w) = self.secondary_windows.find_by_key_mut(key) {
                    w.ui_state.hovered_file = None;
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed {
                    // Global keys are handled before the window partition in
                    // `window_event`; only window-local keys remain here.
                    if key == "mixer"
                        && self.modifiers.control_key()
                        && self.modifiers.shift_key()
                    {
                        if let Key::Character(ch) = &event.logical_key {
                            if ch.eq_ignore_ascii_case("d") {
                                self.mixer_sync_debug = !self.mixer_sync_debug;
                                self.mixer_sync_copied_at = None;
                                if !self.mixer_sync_debug {
                                    self.mixer_sync_close_rect = None;
                                    self.mixer_sync_copy_rect = None;
                                }
                                if let Some(w) = self.secondary_windows.find_by_key_mut(key) {
                                    w.window.request_redraw();
                                }
                                return;
                            }
                        }
                    }
                    if matches!(&event.logical_key, Key::Named(NamedKey::Escape)) {
                        let popup_open = self
                            .secondary_windows
                            .find_by_key_mut(key)
                            .map(|w| w.ui_state.open_popup_id.is_some())
                            .unwrap_or(false);
                        if popup_open {
                            if let Some(w) = self.secondary_windows.find_by_key_mut(key) {
                                w.ui_state.open_popup_id = None;
                                w.ui_state.popup_rect = None;
                                w.window.request_redraw();
                            }
                        } else {
                            self.secondary_windows.hide(key);
                            if key == "parameters" {
                                self.parameters_window_height = None;
                                self.on_parameters_window_closed();
                            }
                            if key == "mixer" {
                                self.close_mixer();
                            }
                            self.restore_focus_after_hide(key);
                            if let Some(w) = &self.window {
                                w.request_redraw();
                            }
                        }
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                self.render_secondary_window(key);
            }
            _ => {}
        }
    }

    /// Render one secondary window's frame: shared `TextSystem`, per-window
    /// `Scene` + `UiState` + `Renderer`, with a clean, spacious `nui_wc2` layout
    /// driven by the same app state as the main window.
    fn render_secondary_window(&mut self, key: &'static str) {
        let Some(spec) = window_spec_by_key(key) else {
            return;
        };

        // Phase 3.2 — intents queued by the Parameters window, applied after the
        // scene-build borrow ends (immediate engine forwarding).
        let mut params_intents = ParamsWindowIntents::default();
        let mut parameters_required_height = None;
        // Adaptive Parameters page: a tracker module swaps the source-engine
        // section for the native libopenmpt card, a MIDI file for the native
        // FluidSynth card (computed before the window borrow; the tracker
        // check asks the linked library, the MIDI check its extensions).
        let tracker_page = key == "parameters" && self.tracker_source_active();
        let midi_page = key == "parameters" && !tracker_page && self.midi_source_active();
        let mut settings_output_changed = false;
        let mut settings_hq_changed = false;
        let mut settings_tray_changed = false;

        // Phase 1 — build the scene (borrows: mw.scene/ui_state + self.text_sys).
        let (w, h, close_clicked, shot) = {
            let Some(mw) = self.secondary_windows.find_by_key_mut(key) else {
                return;
            };
            let sz = mw.window.inner_size();
            let (w, h) = (sz.width, sz.height);
            mw.size = (w, h);
            if w == 0 || h == 0 {
                return;
            }
            let shot = mw.screenshot_once.take();
            let scale = mw.window.scale_factor() as f32;
            let rem = (16.0 * scale).round();
            let theme = if self.dark_mode {
                Theme::dark(rem)
            } else {
                Theme::light(rem)
            };
            let page_bg = if theme.dark {
                (theme.shade1.0, theme.shade1.1, theme.shade1.2)
            } else {
                (theme.shade2.0, theme.shade2.1, theme.shade2.2)
            };
            // Phase 3.3 — refresh the DSP snapshot before the UI reads it (Monitoring
            // only). Feed exactly the frames produced since the last redraw; when the
            // engine is silent, fall back to a synthetic signal so the panels stay live.
            if key == "monitoring" {
                let mut new_frames: Vec<(f32, f32)> = Vec::new();
                if let Some(a) = &self.audio {
                    let fp = a.frames_played.load(std::sync::atomic::Ordering::Relaxed);
                    let delta = fp.saturating_sub(self.last_dsp_frames) as usize;
                    self.last_dsp_frames = fp;
                    let tap = a.dsp_tap();
                    if let Ok(g) = tap.lock() {
                        g.snapshot_into(&mut self.dsp_buf);
                    }
                    if delta > 0 && !self.dsp_buf.is_empty() {
                        let take = delta.min(self.dsp_buf.len());
                        let start = self.dsp_buf.len() - take;
                        new_frames.extend_from_slice(&self.dsp_buf[start..]);
                    }
                }
                if new_frames.is_empty() {
                    new_frames = synth_dsp_frames(self.t, 2048);
                }
                self.dsp.process(&new_frames);
                self.dsp_snap = self.dsp.snapshot();
            }

            let close_clicked;
            {
                let scene = &mut mw.scene;
                let ui_state = &mut mw.ui_state;
                scene.clear_all(page_bg);
                ui_state.begin_frame();

                let pw = w as f32;
                let ph = h as f32;
                let mut ui = Ui::new(ui_state, scene, &mut self.text_sys, self.font_engine, theme);
                ui.viewport = (pw, ph);

                // The Parameters window is audio-pipeline only now.
                let (hdr_icon, hdr_title): (&str, &str) = if key == "parameters" {
                    // Locked parameters are sticky — the padlock says so at a glance.
                    let ic = if self.chk_locked { ICON_LOCK } else { ICON_TUNE };
                    if tracker_page {
                        (ic, "PARAMETERS - TRACKER (MOD)")
                    } else if midi_page {
                        (ic, "PARAMETERS - MIDI")
                    } else {
                        (ic, "PARAMETERS - AUDIO")
                    }
                } else if key == "monitoring" {
                    (ICON_EQ, "AUDIO MONITORING")
                } else if key == "mixer" {
                    (ICON_EQ, "MULTITRACK PREVIEW")
                } else {
                    (window_header_icon(key), spec.title)
                };
                let (hdr_h, close) = ui.secondary_window_header(pw, hdr_title, hdr_icon, "");
                close_clicked = close;
                let m = Length::Rem(if key == "parameters" { 2.25 } else { 1.25 })
                    .resolve(rem)
                    .round();

                if key == "parameters" {
                    // ========================================================
                    // Parameters — adaptive page. AUDIO: Tape Speed and
                    // Rubber Band Pitch/Time, with the inactive card dimmed.
                    // TRACKER/MIDI: only the native source card and lock;
                    // shared audio post-processing is bypassed for these
                    // engines, matching the reference player.
                    // ========================================================
                    let card_gap = Length::Rem(1.5).resolve(rem).round();
                    let card_w = (pw - m * 2.0).max(0.0);
                    let mut cy = hdr_h + m;

                    if tracker_page {
                        let (_, card_h) = ui.nui_card_auto(Rect::new(m, cy, card_w, 0.0), |ui| {
                            draw_tracker_native_card(ui, &mut self.params, &mut params_intents);
                        });
                        cy += card_h + card_gap;
                        let channel_count = self
                            .audio
                            .as_ref()
                            .map(|audio| audio.tracker_channel_count())
                            .unwrap_or(0);
                        let channel_vu = self
                            .audio
                            .as_ref()
                            .map(|audio| audio.tracker_vu())
                            .unwrap_or_default();
                        let (_, card_h) = ui.nui_card_auto(Rect::new(m, cy, card_w, 0.0), |ui| {
                            draw_tracker_channel_card(
                                ui,
                                channel_count,
                                &channel_vu,
                                &mut self.tracker_mutes,
                                self.modifiers.shift_key(),
                                &mut params_intents,
                            );
                        });
                        cy += card_h + card_gap;
                        let (_, card_h) = ui.nui_card_auto(Rect::new(m, cy, card_w, 0.0), |ui| {
                            draw_tracker_separation_card(ui, &mut self.params, &mut params_intents);
                        });
                        cy += card_h + card_gap;
                    } else if midi_page {
                        let (_, card_h) = ui.nui_card_auto(Rect::new(m, cy, card_w, 0.0), |ui| {
                            draw_midi_soundfont_card(
                                ui,
                                &self.soundfonts,
                                &mut self.selected_soundfont,
                                &mut params_intents,
                            );
                        });
                        cy += card_h + card_gap;
                        let native_bpm = self
                            .audio
                            .as_ref()
                            .map(|a| a.midi_native_bpm())
                            .unwrap_or(120.0);
                        let (_, card_h) = ui.nui_card_auto(Rect::new(m, cy, card_w, 0.0), |ui| {
                            draw_midi_native_card(
                                ui,
                                &mut self.params,
                                native_bpm,
                                &mut params_intents,
                            );
                        });
                        cy += card_h + card_gap;
                    }

                    if !tracker_page && !midi_page {
                        let tape_active = self.params.tape;
                        let (_, tape_h) = ui.nui_card_auto(Rect::new(m, cy, card_w, 0.0), |ui| {
                            draw_tape_speed_card(
                                ui,
                                "Tape Speed",
                                tape_active,
                                &mut self.params,
                                &mut params_intents,
                            );
                        });
                        let tape_r = Rect::new(m, cy, card_w, tape_h);
                        cy += tape_h + card_gap;
                        let tape_click = engine_card_hit(&mut ui, 1200, tape_r, tape_active);
                        if tape_click {
                            self.params.tape = true;
                            params_intents.pipeline_changed = true;
                        }
                        if !tape_active {
                            dim_card(&mut ui, tape_r);
                        }

                        let pt_active = !self.params.tape;
                        let (_, pt_h) = ui.nui_card_auto(Rect::new(m, cy, card_w, 0.0), |ui| {
                            draw_pitch_time_card(
                                ui,
                                "Pitch / Time",
                                pt_active,
                                &mut self.params,
                                &mut params_intents,
                            );
                        });
                        let pt_r = Rect::new(m, cy, card_w, pt_h);
                        cy += pt_h + card_gap;
                        let pt_click = engine_card_hit(&mut ui, 1250, pt_r, pt_active);
                        if pt_click {
                            self.params.tape = false;
                            params_intents.pipeline_changed = true;
                        }
                        if !pt_active {
                            dim_card(&mut ui, pt_r);
                        }
                    }

                    if !tracker_page && !midi_page {
                        let (_, lock_h) = ui.nui_card_auto(Rect::new(m, cy, card_w, 0.0), |ui| {
                            let sw_r = ui.take(Length::Rem(2.0));
                            ui.checkbox(1190, sw_r, "Lock Settings", &mut self.chk_locked);
                        });
                        cy += lock_h + card_gap;
                    }
                    parameters_required_height = Some(cy - card_gap + m);
                } else {
                    let body = Rect::new(
                        m,
                        hdr_h + m,
                        (pw - m * 2.0).max(0.0),
                        (ph - hdr_h - m * 2.0).max(80.0),
                    );
                    let gap = theme.space_half();
                    match key {
                        // ====================================================
                        // Phase 3.3 — AUDIO MONITORING: 2x2 DSP panel grid.
                        // ====================================================
                        "monitoring" => {
                            let snap = &self.dsp_snap;
                            let [top_row, bot_row] =
                                ui.rows(body, Length::Px(gap), [Length::fill(), Length::fill()]);
                            let [tl, tr] =
                                ui.cols(top_row, Length::Px(gap), [Length::fill(), Length::fill()]);
                            let [bl, br] =
                                ui.cols(bot_row, Length::Px(gap), [Length::fill(), Length::fill()]);

                            // ---- Top-Left: 64-band logarithmic FFT spectrum ----
                            ui.card(tl, Length::Rem(0.85), Length::Rem(0.45), |ui| {
                                let hdr = ui.take(Length::Rem(1.2));
                                let fs = theme.font_small();
                                let baseline = (hdr.y + fs * 0.85).round();
                                ui.text_clipped(
                                    FontFace::SemiBold,
                                    ui.font_engine,
                                    "SPECTRUM",
                                    hdr.x.round(),
                                    baseline,
                                    fs,
                                    hdr.w,
                                    theme.text_dim,
                                );
                                let peak_txt =
                                    format!("Peak: {:.0}Hz ({:+.1}dB)", snap.peak_hz, snap.peak_db);
                                let ptw = ui.text_sys.measure(
                                    FontFace::SemiBold,
                                    ui.font_engine,
                                    &peak_txt,
                                    fs,
                                );
                                ui.scene.text(
                                    ui.text_sys,
                                    FontFace::SemiBold,
                                    ui.font_engine,
                                    &peak_txt,
                                    (hdr.x + hdr.w - ptw).round(),
                                    baseline,
                                    fs,
                                    theme.highlight,
                                );
                                let canvas = ui.take_rest();
                                ui.scene.rounded_rect(
                                    canvas.x,
                                    canvas.y,
                                    canvas.w,
                                    canvas.h,
                                    theme.radius1(),
                                    theme.base,
                                    theme.border1,
                                    1.0,
                                );
                                ui.scene.push_clip(
                                    canvas.x + 1.0,
                                    canvas.y + 1.0,
                                    (canvas.w - 2.0).max(1.0),
                                    (canvas.h - 2.0).max(1.0),
                                );
                                for f in [
                                    50.0f32, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0,
                                ] {
                                    let nx = (f / 20.0).ln() / (20000.0f32 / 20.0).ln();
                                    let gx = (canvas.x + nx * canvas.w).round();
                                    ui.scene.rect(gx, canvas.y, 1.0, canvas.h, theme.border1);
                                }
                                for db in [-12.0f32, -24.0, -36.0, -48.0] {
                                    let ny = (db + 60.0) / 60.0;
                                    let gy = (canvas.y + (1.0 - ny) * canvas.h).round();
                                    ui.scene.rect(canvas.x, gy, canvas.w, 1.0, theme.border1);
                                }
                                let n = BANDS;
                                let bgap = 1.0f32;
                                let bw = ((canvas.w - bgap * (n as f32 - 1.0)) / n as f32).max(1.0);
                                for i in 0..n {
                                    let h = ((snap.spectrum_db[i] + 60.0) / 60.0).clamp(0.0, 1.0)
                                        * canvas.h;
                                    if h > 0.5 {
                                        let bx = canvas.x + i as f32 * (bw + bgap);
                                        ui.scene.rounded_rect(
                                            bx,
                                            canvas.y + canvas.h - h,
                                            bw,
                                            h,
                                            1.5,
                                            theme.highlight,
                                            (0, 0, 0, 0),
                                            0.0,
                                        );
                                    }
                                }
                                let pts: Vec<(f32, f32)> = (0..n)
                                    .map(|i| {
                                        (
                                            (i as f32 + 0.5) / n as f32,
                                            ((snap.peak_hold_db[i] + 60.0) / 60.0).clamp(0.0, 1.0),
                                        )
                                    })
                                    .collect();
                                ui.polyline(canvas, &pts, 1.5, rgb(235, 235, 235));
                                ui.scene.pop_clip();
                            });

                            // ---- Top-Right: zero-crossing-stabilized oscilloscope ----
                            ui.card(tr, Length::Rem(0.85), Length::Rem(0.45), |ui| {
                                let hdr = ui.take(Length::Rem(1.2));
                                let fs = theme.font_small();
                                let baseline = (hdr.y + fs * 0.85).round();
                                ui.text_clipped(
                                    FontFace::SemiBold,
                                    ui.font_engine,
                                    "OSCILLOSCOPE",
                                    hdr.x.round(),
                                    baseline,
                                    fs,
                                    hdr.w,
                                    theme.text_dim,
                                );
                                let rms_txt = format!("RMS: {:+.1} dB", snap.rms_db);
                                let rtw = ui.text_sys.measure(
                                    FontFace::SemiBold,
                                    ui.font_engine,
                                    &rms_txt,
                                    fs,
                                );
                                ui.scene.text(
                                    ui.text_sys,
                                    FontFace::SemiBold,
                                    ui.font_engine,
                                    &rms_txt,
                                    (hdr.x + hdr.w - rtw).round(),
                                    baseline,
                                    fs,
                                    theme.highlight,
                                );
                                let canvas = ui.take_rest();
                                ui.scene.rounded_rect(
                                    canvas.x,
                                    canvas.y,
                                    canvas.w,
                                    canvas.h,
                                    theme.radius1(),
                                    theme.base,
                                    theme.border1,
                                    1.0,
                                );
                                ui.scene.push_clip(
                                    canvas.x + 1.0,
                                    canvas.y + 1.0,
                                    (canvas.w - 2.0).max(1.0),
                                    (canvas.h - 2.0).max(1.0),
                                );
                                for ny in [0.75f32, 0.5, 0.25] {
                                    let gy = (canvas.y + (1.0 - ny) * canvas.h).round();
                                    ui.scene.rect(canvas.x, gy, canvas.w, 1.0, theme.border1);
                                }
                                ui.polyline(canvas, &snap.osc_points, 2.0, theme.highlight);
                                ui.scene.pop_clip();
                            });

                            // ---- Bottom-Left: stereo goniometer + correlation ----
                            ui.card(bl, Length::Rem(0.85), Length::Rem(0.45), |ui| {
                                let hdr = ui.take(Length::Rem(1.2));
                                let fs = theme.font_small();
                                let baseline = (hdr.y + fs * 0.85).round();
                                ui.text_clipped(
                                    FontFace::SemiBold,
                                    ui.font_engine,
                                    "STEREO FIELD (GONIOMETER)",
                                    hdr.x.round(),
                                    baseline,
                                    fs,
                                    hdr.w,
                                    theme.text_dim,
                                );
                                let corr_txt = format!("Correlation: {:+.2}", snap.correlation);
                                let ctw = ui.text_sys.measure(
                                    FontFace::SemiBold,
                                    ui.font_engine,
                                    &corr_txt,
                                    fs,
                                );
                                ui.scene.text(
                                    ui.text_sys,
                                    FontFace::SemiBold,
                                    ui.font_engine,
                                    &corr_txt,
                                    (hdr.x + hdr.w - ctw).round(),
                                    baseline,
                                    fs,
                                    theme.highlight,
                                );
                                let rest = ui.take_rest();
                                let [radar, corr] = ui.rows(
                                    rest,
                                    Length::Px(theme.space_half()),
                                    [Length::fill(), Length::rem(1.5)],
                                );
                                ui.scene.rounded_rect(
                                    radar.x,
                                    radar.y,
                                    radar.w,
                                    radar.h,
                                    theme.radius1(),
                                    theme.base,
                                    theme.border1,
                                    1.0,
                                );
                                let cx = radar.x + radar.w * 0.5;
                                let cy = radar.y + radar.h * 0.5;
                                let rad = (radar.w.min(radar.h) * 0.5 - 8.0).max(4.0);
                                let tau = std::f32::consts::TAU;
                                for i in 0..32 {
                                    let a0 = i as f32 / 32.0 * tau;
                                    let a1 = (i + 1) as f32 / 32.0 * tau;
                                    ui.scene.line(
                                        cx + rad * a0.cos(),
                                        cy + rad * a0.sin(),
                                        cx + rad * a1.cos(),
                                        cy + rad * a1.sin(),
                                        1.0,
                                        theme.border2,
                                    );
                                }
                                let d = rad * 0.7071;
                                ui.scene
                                    .line(cx - d, cy + d, cx + d, cy - d, 1.0, theme.border1);
                                ui.scene
                                    .line(cx - d, cy - d, cx + d, cy + d, 1.0, theme.border1);
                                ui.scene
                                    .line(cx, cy - rad, cx, cy + rad, 1.0, theme.border1);
                                ui.scene
                                    .line(cx - rad, cy, cx + rad, cy, 1.0, theme.border1);
                                let lfs = theme.font_xsmall();
                                for (lbl, lx, ly) in [
                                    ("L", cx - d - 12.0, cy - d + 4.0),
                                    ("R", cx + d + 2.0, cy + d + 6.0),
                                    ("+M", cx + 3.0, cy - rad + 6.0),
                                    ("-S", cx + 3.0, cy + rad),
                                ] {
                                    ui.scene.text(
                                        ui.text_sys,
                                        FontFace::SemiBold,
                                        ui.font_engine,
                                        lbl,
                                        lx.round(),
                                        ly.round(),
                                        lfs,
                                        theme.text_muted,
                                    );
                                }
                                ui.polyline(radar, &snap.gonio_points, 1.5, theme.highlight);

                                // Phase correlation bar: centre 0, fill toward ±1.
                                ui.scene.rounded_rect(
                                    corr.x,
                                    corr.y,
                                    corr.w,
                                    corr.h,
                                    theme.radius1(),
                                    theme.base,
                                    theme.border1,
                                    1.0,
                                );
                                let mid = (corr.x + corr.w * 0.5).round();
                                ui.scene.rect(
                                    mid - 1.0,
                                    corr.y + 2.0,
                                    2.0,
                                    corr.h - 4.0,
                                    theme.border3,
                                );
                                let c = snap.correlation.clamp(-1.0, 1.0);
                                let half = corr.w * 0.5;
                                let (bx0, bwid) = if c >= 0.0 {
                                    (mid, half * c)
                                } else {
                                    (mid + half * c, -half * c)
                                };
                                if bwid > 0.5 {
                                    ui.scene.rect(
                                        bx0,
                                        corr.y + 2.0,
                                        bwid,
                                        corr.h - 4.0,
                                        theme.highlight,
                                    );
                                }
                                let lfs2 = theme.font_xsmall();
                                ui.scene.text(
                                    ui.text_sys,
                                    FontFace::Regular,
                                    ui.font_engine,
                                    "-1",
                                    (corr.x + 3.0).round(),
                                    (corr.y + corr.h * 0.5 + 4.0).round(),
                                    lfs2,
                                    theme.text_muted,
                                );
                                ui.scene.text(
                                    ui.text_sys,
                                    FontFace::Regular,
                                    ui.font_engine,
                                    "+1",
                                    (corr.x + corr.w - 16.0).round(),
                                    (corr.y + corr.h * 0.5 + 4.0).round(),
                                    lfs2,
                                    theme.text_muted,
                                );
                            });

                            // ---- Bottom-Right: meters + BS.1770 loudness ----
                            ui.card(br, Length::Rem(0.85), Length::Rem(0.45), |ui| {
                                let hdr = ui.take(Length::Rem(1.2));
                                let fs = theme.font_small();
                                let baseline = (hdr.y + fs * 0.85).round();
                                ui.text_clipped(
                                    FontFace::SemiBold,
                                    ui.font_engine,
                                    "LEVELS & LOUDNESS (LUFS)",
                                    hdr.x.round(),
                                    baseline,
                                    fs,
                                    hdr.w,
                                    theme.text_dim,
                                );
                                let int_txt = format!("Int: {:+.1} LUFS", snap.integrated_lufs);
                                let itw = ui.text_sys.measure(
                                    FontFace::SemiBold,
                                    ui.font_engine,
                                    &int_txt,
                                    fs,
                                );
                                ui.scene.text(
                                    ui.text_sys,
                                    FontFace::SemiBold,
                                    ui.font_engine,
                                    &int_txt,
                                    (hdr.x + hdr.w - itw).round(),
                                    baseline,
                                    fs,
                                    theme.highlight,
                                );
                                let rest = ui.take_rest();
                                let [meters_r, lufs_r] = ui.cols(
                                    rest,
                                    Length::Px(theme.space_half()),
                                    [Length::rem(6.0), Length::fill()],
                                );
                                let [lm, sm, rm] = ui.cols(
                                    meters_r,
                                    Length::Px(2.0),
                                    [Length::fill(), Length::rem(1.5), Length::fill()],
                                );
                                draw_vu_meter(ui, lm, snap.vu_l_db, snap.peak_l_db, "L");
                                draw_vu_meter(ui, rm, snap.vu_r_db, snap.peak_r_db, "R");
                                let sfs = theme.font_xsmall();
                                for (db, lbl) in [
                                    (0.0f32, "0"),
                                    (-12.0, "-12"),
                                    (-24.0, "-24"),
                                    (-36.0, "-36"),
                                    (-48.0, "-48"),
                                ] {
                                    let y =
                                        sm.y + (1.0 - ((db + 60.0) / 60.0).clamp(0.0, 1.0)) * sm.h;
                                    ui.scene.text(
                                        ui.text_sys,
                                        FontFace::Regular,
                                        ui.font_engine,
                                        lbl,
                                        sm.x.round(),
                                        (y + 3.0).round(),
                                        sfs,
                                        theme.text_muted,
                                    );
                                }
                                let [grid_r, btn_row] = ui.rows(
                                    lufs_r,
                                    Length::Px(theme.space_half()),
                                    [Length::fill(), Length::rem(2.1)],
                                );
                                let [r0, r1] = ui.rows(
                                    grid_r,
                                    Length::Px(theme.space_half()),
                                    [Length::fill(), Length::fill()],
                                );
                                let [a0, a1] = ui.cols(
                                    r0,
                                    Length::Px(theme.space_half()),
                                    [Length::fill(), Length::fill()],
                                );
                                let [b0, b1] = ui.cols(
                                    r1,
                                    Length::Px(theme.space_half()),
                                    [Length::fill(), Length::fill()],
                                );
                                draw_readout(
                                    ui,
                                    a0,
                                    "MOMENTARY",
                                    &format!("{:+.1} LUFS", snap.momentary_lufs),
                                );
                                draw_readout(
                                    ui,
                                    a1,
                                    "SHORT-TERM",
                                    &format!("{:+.1} LUFS", snap.short_term_lufs),
                                );
                                draw_readout(
                                    ui,
                                    b0,
                                    "INTEGRATED",
                                    &format!("{:+.1} LUFS", snap.integrated_lufs),
                                );
                                draw_readout(
                                    ui,
                                    b1,
                                    "TRUE PEAK",
                                    &format!("{:+.1} dBTP", snap.true_peak_dbtp),
                                );
                                let bw2 = Length::Rem(8.0).resolve(rem).round();
                                let btn = Rect::new(
                                    btn_row.x + btn_row.w - bw2,
                                    btn_row.y,
                                    bw2,
                                    btn_row.h,
                                );
                                if ui.button(
                                    1290,
                                    btn,
                                    "RESET LUFS",
                                    ICON_RESTART,
                                    ButtonVariant::Outline,
                                ) {
                                    self.dsp.reset_lufs();
                                }
                            });
                        }

                        // ====================================================
                        // Phase 3.4 — MULTITRACK PREVIEW mixer.
                        // ====================================================
                        "mixer" => {
                            self.mixer_strip_targets.clear();
                            self.mixer_add_target = None;
                            // The sync panel is a developer overlay: while it is open,
                            // wheel input belongs to its table, not the faders behind it.
                            let mixer_sync_wheel = if self.mixer_sync_debug {
                                Some(std::mem::take(&mut ui.state.wheel_y))
                            } else {
                                None
                            };
                            let pad = Length::Rem(1.25).resolve(rem).round();
                            let inner = Rect::new(
                                pad,
                                hdr_h + pad,
                                (pw - pad * 2.0).max(0.0),
                                (ph - hdr_h - pad * 2.0).max(0.0),
                            );
                            let toolbar_h = Length::Rem(2.5).resolve(rem).round();
                            let seek_h = Length::Rem(2.4).resolve(rem).round();
                            let toolbar = Rect::new(inner.x, inner.y, inner.w, toolbar_h);

                            // ---- Top toolbar: transport + master volume ----
                            let sq = Length::Rem(2.5).resolve(rem).round();
                            let bh = Length::Rem(2.3).resolve(rem).round();
                            let bgap = Length::Rem(0.4).resolve(rem).round();
                            let by = toolbar.y + (toolbar.h - bh) * 0.5;
                            let play_icon = if self.mixer_playing {
                                ICON_PAUSE
                            } else {
                                ICON_PLAY
                            };
                            if ui.button(
                                1301,
                                Rect::new(toolbar.x, by, sq, bh),
                                "",
                                play_icon,
                                ButtonVariant::Outline,
                            ) {
                                if let Some(engine) = &mut self.mixer_audio {
                                    self.mixer_playing = engine.toggle_play();
                                }
                            }
                            if ui.button(
                                1302,
                                Rect::new(toolbar.x + (sq + bgap), by, sq, bh),
                                "",
                                ICON_RESTART,
                                ButtonVariant::Outline,
                            ) {
                                if let Some(engine) = &mut self.mixer_audio {
                                    engine.seek_secs(0.0);
                                }
                                self.mixer_pos_frac = 0.0;
                            }
                            let loop_var = if self.mixer_loop {
                                ButtonVariant::Primary
                            } else {
                                ButtonVariant::Outline
                            };
                            if ui.button(
                                1303,
                                Rect::new(toolbar.x + (sq + bgap) * 2.0, by, sq, bh),
                                "",
                                ICON_LOOP,
                                loop_var,
                            ) {
                                self.mixer_loop = !self.mixer_loop;
                                if let Some(engine) = &self.mixer_audio {
                                    engine.set_loop(self.mixer_loop);
                                }
                            }
                            let vol_w = Length::Rem(15.0).resolve(rem).round();
                            let vol_r = Rect::new(toolbar.x + toolbar.w - vol_w, by, vol_w, bh);
                            master_volume_pill(&mut ui, vol_r, &mut self.mixer_master_vol, 1304);
                            if let Some(engine) = &self.mixer_audio {
                                engine.set_master_volume(self.mixer_master_vol);
                            }

                            // ---- Channel strip matrix ----
                            let matrix_y = toolbar.y + toolbar.h + gap;
                            let seek_y = inner.y + inner.h - seek_h;
                            let matrix = Rect::new(
                                inner.x,
                                matrix_y,
                                inner.w,
                                (seek_y - matrix_y - gap * 3.0).max(40.0),
                            );
                            let n = self.mixer_stems.len();
                            let col_gap = Length::Rem(0.4).resolve(rem).round();
                            let plus_w = Length::Rem(3.2).resolve(rem).round();
                            let strip_w = ((matrix.w - plus_w - col_gap * n.max(1) as f32)
                                / n.max(1) as f32)
                                .max(24.0)
                                .floor();
                            let any_solo = self.mixer_stems.iter().any(|s| s.solo);
                            let m_h = Length::Rem(1.55).resolve(rem).round();
                            let lbl_h = Length::Rem(1.65).resolve(rem).round();
                            let cgap = 2.0f32;
                            let fader_h = (matrix.h - (m_h * 3.0 + lbl_h) - cgap * 4.0).max(30.0);
                            let mut remove_i: Option<usize> = None;

                            for i in 0..n {
                                let x = matrix.x + i as f32 * (strip_w + col_gap);
                                let mut y = matrix.y;
                                self.mixer_strip_targets.push((
                                    Rect::new(x, matrix.y, strip_w, matrix.h),
                                    self.mixer_stems[i].slot,
                                ));

                                // Mute cell
                                let m_r = Rect::new(x, y, strip_w, m_h);
                                y += m_h + cgap;
                                let is_muted = self.mixer_stems[i].muted;
                                if mixer_toggle_cell(
                                    &mut ui,
                                    m_r,
                                    1310 + i as u32,
                                    "M",
                                    is_muted,
                                    theme.palette_alert,
                                ) {
                                    self.mixer_stems[i].muted = !is_muted;
                                }

                                // Solo cell
                                let s_r = Rect::new(x, y, strip_w, m_h);
                                y += m_h + cgap;
                                let is_solo = self.mixer_stems[i].solo;
                                if mixer_toggle_cell(
                                    &mut ui,
                                    s_r,
                                    1330 + i as u32,
                                    "S",
                                    is_solo,
                                    theme.palette_mark,
                                ) {
                                    self.mixer_stems[i].solo = !is_solo;
                                }

                                // Tall touch-friendly vertical fader + green VU
                                let fader_r = Rect::new(x, y, strip_w, fader_h);
                                y += fader_h + cgap;
                                let fid = 1350 + i as u32;
                                ui.state.register_focusable(fid);
                                let fhov = ui.state.can_hover(fid, fader_r);
                                let mut gain = self.mixer_stems[i].gain;
                                if fhov {
                                    ui.state.hovered_id = Some(fid);
                                    if ui.state.double_clicked || ui.state.right_pressed {
                                        gain = 1.0;
                                    } else if ui.state.mouse_pressed {
                                        ui.state.active_id = Some(fid);
                                        ui.state.focused_id = Some(fid);
                                    }
                                    if ui.state.wheel_y.abs() > 0.01 {
                                        gain = (gain + 0.05 * ui.state.wheel_y.signum())
                                            .clamp(0.0, 2.0);
                                    }
                                }
                                if ui.state.active_id == Some(fid) && ui.state.mouse_down {
                                    gain = ((1.0 - (ui.state.cursor.1 - fader_r.y) / fader_r.h)
                                        * 2.0)
                                        .clamp(0.0, 2.0);
                                }
                                self.mixer_stems[i].gain = gain;

                                ui.scene.rounded_rect(
                                    fader_r.x,
                                    fader_r.y,
                                    fader_r.w,
                                    fader_r.h,
                                    theme.radius1(),
                                    theme.base,
                                    theme.border1,
                                    1.0,
                                );
                                let inaudible = self.mixer_stems[i].muted
                                    || (any_solo && !self.mixer_stems[i].solo);
                                let lvl = if inaudible {
                                    0.0
                                } else {
                                    self.mixer_audio
                                        .as_ref()
                                        .map(|engine| {
                                            (engine.meter(self.mixer_stems[i].slot) * 6.0)
                                                .clamp(0.0, 1.0)
                                        })
                                        .unwrap_or(0.0)
                                };
                                let vu_h = (fader_r.h * lvl).clamp(0.0, fader_r.h);
                                if vu_h > 1.0 {
                                    ui.scene.rect(
                                        fader_r.x + 1.0,
                                        fader_r.y + fader_r.h - vu_h,
                                        (fader_r.w - 2.0).max(1.0),
                                        vu_h,
                                        rgb(58, 175, 52),
                                    );
                                }
                                let line_y = (fader_r.y
                                    + fader_r.h * (1.0 - (gain / 2.0).clamp(0.0, 1.0)))
                                .round();
                                ui.scene.rect(
                                    fader_r.x,
                                    line_y,
                                    fader_r.w,
                                    1.5,
                                    rgb(230, 230, 230),
                                );
                                draw_text_at(
                                    &mut ui,
                                    fader_r.x + fader_r.w * 0.5,
                                    line_y - 5.0,
                                    &format!("{:.2}", gain),
                                    FontFace::SemiBold,
                                    theme.font_xsmall(),
                                    rgb(245, 245, 245),
                                );
                                ui.draw_focus_ring(fid, fader_r, theme.radius1());

                                // Horizontal pan cell
                                let pan_r = Rect::new(x, y, strip_w, m_h);
                                y += m_h + cgap;
                                let pid = 1370 + i as u32;
                                ui.state.register_focusable(pid);
                                let phov = ui.state.can_hover(pid, pan_r);
                                let mut pan = self.mixer_stems[i].pan;
                                if phov {
                                    ui.state.hovered_id = Some(pid);
                                    if ui.state.double_clicked || ui.state.right_pressed {
                                        pan = 0.0;
                                    } else if ui.state.mouse_pressed {
                                        ui.state.active_id = Some(pid);
                                        ui.state.focused_id = Some(pid);
                                    }
                                    if ui.state.wheel_y.abs() > 0.01 {
                                        pan = (pan + 0.05 * ui.state.wheel_y.signum())
                                            .clamp(-1.0, 1.0);
                                    }
                                }
                                if ui.state.active_id == Some(pid) && ui.state.mouse_down {
                                    pan = (((ui.state.cursor.0 - pan_r.x) / pan_r.w) * 2.0 - 1.0)
                                        .clamp(-1.0, 1.0);
                                }
                                self.mixer_stems[i].pan = pan;
                                ui.scene.rounded_rect(
                                    pan_r.x,
                                    pan_r.y,
                                    pan_r.w,
                                    pan_r.h,
                                    theme.radius1(),
                                    theme.base,
                                    theme.border1,
                                    1.0,
                                );
                                let px =
                                    (pan_r.x + 4.0 + (pan_r.w - 8.0) * ((pan + 1.0) * 0.5)).round();
                                let tick_h = pan_r.h * 0.55;
                                ui.scene.rect(
                                    px - 1.0,
                                    pan_r.y + (pan_r.h - tick_h) * 0.5,
                                    2.0,
                                    tick_h,
                                    rgb(230, 230, 230),
                                );

                                // Bottom label + remove ×
                                let lbl_r = Rect::new(x, y, strip_w, lbl_h);
                                ui.scene.rounded_rect(
                                    lbl_r.x,
                                    lbl_r.y,
                                    lbl_r.w,
                                    lbl_r.h,
                                    theme.radius1(),
                                    theme.base,
                                    theme.border1,
                                    1.0,
                                );
                                let lfs = theme.font_xsmall();
                                let num = format!("{:02}", self.mixer_stems[i].number);
                                ui.scene.text(
                                    ui.text_sys,
                                    FontFace::SemiBold,
                                    ui.font_engine,
                                    &num,
                                    (lbl_r.x + (rem * 0.45).round()).round(),
                                    (lbl_r.y + lbl_r.h * 0.5 + lfs * 0.34).round(),
                                    lfs,
                                    theme.text_dim,
                                );
                                let xbtn = (rem * 1.2).round();
                                let xr = Rect::new(
                                    lbl_r.x + lbl_r.w - xbtn - 2.0,
                                    lbl_r.y + (lbl_r.h - xbtn) * 0.5,
                                    xbtn,
                                    xbtn,
                                );
                                if ui.button(
                                    1390 + i as u32,
                                    xr,
                                    "",
                                    ICON_CLOSE,
                                    ButtonVariant::Ghost,
                                ) {
                                    remove_i = Some(i);
                                }
                            }

                            if let Some(i) = remove_i {
                                let slot = self.mixer_stems[i].slot;
                                if let Some(engine) = &mut self.mixer_audio {
                                    engine.remove_stem(slot);
                                }
                                self.mixer_stems.remove(i);
                            }
                            let any_solo_now = self.mixer_stems.iter().any(|stem| stem.solo);
                            if let Some(engine) = &self.mixer_audio {
                                engine.set_loop(self.mixer_loop);
                                engine.set_master_volume(self.mixer_master_vol);
                                for stem in &self.mixer_stems {
                                    let inaudible = stem.muted || (any_solo_now && !stem.solo);
                                    engine.set_stem(stem.slot, stem.gain, stem.pan, inaudible);
                                }
                            }

                            // `+` add-stem column: a drop target, not a button. The
                            // whole column adds files dropped onto it.
                            let plus_r =
                                Rect::new(matrix.x + matrix.w - plus_w, matrix.y, plus_w, matrix.h);
                            self.mixer_add_target = Some(plus_r);
                            let add_active = ui.state.hovered_file.is_some();
                            let add_border = if add_active {
                                theme.highlight
                            } else {
                                theme.border1
                            };
                            let add_fill = if add_active {
                                rgba(255, 255, 255, if theme.dark { 22 } else { 40 })
                            } else {
                                theme.base
                            };
                            ui.scene.rounded_rect(
                                plus_r.x,
                                plus_r.y,
                                plus_r.w,
                                plus_r.h,
                                theme.radius2(),
                                add_fill,
                                add_border,
                                1.0,
                            );
                            let icon_sz = (rem * 1.5).round();
                            ui.scene.icon_centered(
                                ui.text_sys,
                                ICON_ADD,
                                plus_r.x + plus_r.w * 0.5,
                                plus_r.y + plus_r.h * 0.5 - rem * 0.8,
                                icon_sz,
                                if add_active {
                                    theme.highlight
                                } else {
                                    theme.text_dim
                                },
                            );
                            draw_text_at(
                                &mut ui,
                                plus_r.x + plus_r.w * 0.5,
                                plus_r.y + plus_r.h * 0.5 + rem * 0.5,
                                "DROP",
                                FontFace::SemiBold,
                                theme.font_xsmall(),
                                if add_active {
                                    theme.highlight
                                } else {
                                    theme.text_dim
                                },
                            );
                            draw_text_at(
                                &mut ui,
                                plus_r.x + plus_r.w * 0.5,
                                plus_r.y + plus_r.h * 0.5 + rem * 1.5,
                                "AUDIO",
                                FontFace::Regular,
                                theme.font_xsmall(),
                                theme.text_muted,
                            );

                            // ---- Bottom seekbar + time readouts ----
                            // Same flat track height/colors as the main player's
                            // progress bar, with a taller hit area for touch.
                            let scale = rem / 16.0;
                            let bar_h = (5.0 * scale).round().max(4.0);
                            let bar_r = Rect::new(
                                inner.x,
                                (seek_y + (seek_h - bar_h) * 0.5).round(),
                                inner.w,
                                bar_h,
                            );
                            let track_bar_bg = if theme.dark {
                                rgb(68, 68, 68)
                            } else {
                                rgb(180, 180, 180)
                            };
                            let track_bar_fill = if theme.dark {
                                rgb(180, 180, 180)
                            } else {
                                rgb(50, 50, 50)
                            };
                            let sid = 1305u32;
                            let hit = Rect::new(
                                inner.x,
                                bar_r.y - 14.0 * scale,
                                inner.w,
                                14.0 * scale + bar_h + 20.0 * scale,
                            );
                            ui.flat_slider(
                                sid,
                                bar_r,
                                hit,
                                &mut self.mixer_pos_frac,
                                0.0,
                                1.0,
                                [track_bar_bg, track_bar_fill],
                            );
                            if ui.state.active_id == Some(sid) && ui.state.mouse_released {
                                if let Some(engine) = &mut self.mixer_audio {
                                    engine.seek_secs(
                                        engine.duration_secs() * self.mixer_pos_frac as f64,
                                    );
                                }
                            }
                            let (position, duration) = self
                                .mixer_audio
                                .as_ref()
                                .map(|engine| (engine.position_secs(), engine.duration_secs()))
                                .unwrap_or((0.0, 0.0));
                            let position_text = fmt_time(position);
                            let total_text = fmt_time(duration);
                            let tfs = theme.font_xsmall();
                            let tbase = (seek_y + seek_h - 2.0).round();
                            ui.scene.text(
                                ui.text_sys,
                                FontFace::SemiBold,
                                ui.font_engine,
                                &position_text,
                                inner.x.round(),
                                tbase,
                                tfs,
                                theme.text_dim,
                            );
                            let tw = ui.text_sys.measure(
                                FontFace::SemiBold,
                                ui.font_engine,
                                &total_text,
                                tfs,
                            );
                            ui.scene.text(
                                ui.text_sys,
                                FontFace::SemiBold,
                                ui.font_engine,
                                &total_text,
                                (inner.x + inner.w - tw).round(),
                                tbase,
                                tfs,
                                theme.text_dim,
                            );

                            if self.mixer_sync_debug {
                                if let Some(wheel) = mixer_sync_wheel {
                                    ui.state.wheel_y = wheel;
                                }
                                let snapshot = self
                                    .mixer_audio
                                    .as_ref()
                                    .map(|engine| engine.sync_snapshot());
                                let copied = self
                                    .mixer_sync_copied_at
                                    .map(|at| at.elapsed() < Duration::from_millis(1200))
                                    .unwrap_or(false);
                                let (copy, close, close_r, copy_r) = draw_mixer_sync_debug(
                                    &mut ui,
                                    matrix,
                                    snapshot.as_ref(),
                                    &self.mixer_stems,
                                    copied,
                                );
                                self.mixer_sync_close_rect = Some(close_r);
                                self.mixer_sync_copy_rect = Some(copy_r);
                                if copy {
                                    self.mixer_sync_copied_at = Some(Instant::now());
                                }
                                if close {
                                    self.mixer_sync_debug = false;
                                    self.mixer_sync_close_rect = None;
                                    self.mixer_sync_copy_rect = None;
                                }
                            }
                        }

                        // ====================================================
                        // Phase 3.5 — Settings (persistent user.json).
                        // ====================================================
                        "settings" => {
                            let mut device_opts = vec!["System Default".to_string()];
                            device_opts.extend(self.audio_devices.iter().cloned());
                            let device_refs: Vec<&str> =
                                device_opts.iter().map(String::as_str).collect();
                            let mut device_changed = false;
                            let mut hq_changed = false;
                            ui.nui_card(body, |ui| {
                                let sel_dev = ui.input_group("Default Output Device");
                                let [c1, c2] = ui.take_cols(
                                    Length::rem(1.8),
                                    Length::rem(1.0),
                                    [Length::fill(), Length::fill()],
                                );
                                settings_tray_changed = ui.checkbox(
                                    752,
                                    c1,
                                    "Keep Running in System Tray",
                                    &mut self.sw_keep_tray,
                                );
                                hq_changed =
                                    ui.checkbox(753, c2, "HQ Oversampling", &mut self.sw_hq_mode);
                                let [c3, c4] = ui.take_cols(
                                    Length::rem(1.8),
                                    Length::rem(1.0),
                                    [Length::fill(), Length::fill()],
                                );
                                ui.checkbox(754, c3, "Lock Settings", &mut self.chk_locked);
                                ui.checkbox(
                                    755,
                                    c4,
                                    "Dark Mode (Accessibility Default)",
                                    &mut self.dark_mode,
                                );
                                let [c5] = ui.take_cols(
                                    Length::rem(1.8),
                                    Length::rem(1.0),
                                    [Length::fill()],
                                );
                                if ui.checkbox(
                                    757,
                                    c5,
                                    "Show transport controls [C]",
                                    &mut self.show_controls,
                                ) {
                                    self.apply_zoom_req = true;
                                }
                                // Default start folder: the first audio file
                                // (sorted) inside it opens at startup and its
                                // sibling folder becomes the implicit playlist.
                                let [dcol, bcol1, bcol2] = ui.take_cols(
                                    Length::rem(1.8),
                                    Length::rem(1.0),
                                    [Length::fill(), Length::rem(8.2), Length::rem(5.4)],
                                );
                                let dir_label = if self.default_dir.is_empty() {
                                    "Default start folder: (OS Music folder)".to_string()
                                } else {
                                    format!("Default start folder: {}", self.default_dir)
                                };
                                let fs2 = theme.font_xsmall();
                                ui.text_clipped(
                                    FontFace::Regular,
                                    ui.font_engine,
                                    &dir_label,
                                    dcol.x.round(),
                                    (dcol.y + (dcol.h - fs2) * 0.5).round(),
                                    fs2,
                                    dcol.w,
                                    theme.text_muted,
                                );
                                if ui.button(
                                    758,
                                    bcol1,
                                    "Use Current",
                                    ICON_ADD,
                                    ButtonVariant::Outline,
                                ) {
                                    let cur = self
                                        .audio
                                        .as_ref()
                                        .and_then(|a| a.current_file())
                                        .or_else(|| {
                                            self.playlist.get(self.selected_track).cloned()
                                        });
                                    if let Some(dir) =
                                        cur.and_then(|p| p.parent().map(|d| d.to_path_buf()))
                                    {
                                        self.default_dir = dir.display().to_string();
                                    }
                                }
                                if ui.button(
                                    759,
                                    bcol2,
                                    "Reset",
                                    ICON_RESTART,
                                    ButtonVariant::Outline,
                                ) {
                                    self.default_dir.clear();
                                }
                                let [apply_r] = ui.take_form_row(true, [Length::fill()]);
                                if ui.button(
                                    756,
                                    apply_r,
                                    "Save Configuration",
                                    ICON_SETTINGS,
                                    ButtonVariant::Primary,
                                ) {
                                    self.save_config_req = true;
                                }
                                let fs = theme.font_xsmall();
                                ui.text_clipped(
                                    FontFace::Regular,
                                    ui.font_engine,
                                    &self.config_status,
                                    apply_r.x.round(),
                                    (apply_r.y + apply_r.h + fs * 1.1).round(),
                                    fs,
                                    apply_r.w,
                                    theme.text_muted,
                                );
                                // Render selects last so open popups layer above siblings.
                                device_changed = ui.select(
                                    760,
                                    sel_dev,
                                    &device_refs,
                                    &mut self.selected_device_idx,
                                );
                            });
                            settings_output_changed = device_changed;
                            settings_hq_changed = hq_changed;
                        }

                        // ---- Help: two-column keyboard-shortcut table ----
                        _ => {
                            ui.nui_card(body, |ui| {
                                let table = ui.take_rest();
                                let [lc, rc] = ui.cols(
                                    table,
                                    Length::rem(1.25),
                                    [Length::fill(), Length::fill()],
                                );
                                let half = HELP_ROWS.len().div_ceil(2);
                                let rh = (table.h / half.max(1) as f32)
                                    .floor()
                                    .min((rem * 2.0).round());
                                let kfs = theme.font_xsmall();
                                let dfs = theme.font_small();
                                let kw = Length::Rem(8.5).resolve(rem).round();
                                for (idx, &(k, v)) in HELP_ROWS.iter().enumerate() {
                                    let (col, row) = if idx < half {
                                        (lc, idx)
                                    } else {
                                        (rc, idx - half)
                                    };
                                    let y = (col.y + row as f32 * rh).round();
                                    let kbd_h = (rh - 6.0).max(16.0);
                                    let kbd_y = (y + (rh - kbd_h) * 0.5).round();
                                    let kbd_w = (kw - 10.0).max(24.0);
                                    ui.scene.rounded_rect(
                                        col.x,
                                        kbd_y,
                                        kbd_w,
                                        kbd_h,
                                        theme.radius1(),
                                        theme.base,
                                        theme.border3,
                                        1.0,
                                    );
                                    let baseline = (y + rh * 0.5 + dfs * 0.34).round();
                                    ui.text_clipped(
                                        FontFace::SemiBold,
                                        ui.font_engine,
                                        k,
                                        (col.x + 6.0).round(),
                                        baseline,
                                        kfs,
                                        kbd_w - 12.0,
                                        theme.highlight,
                                    );
                                    ui.text_clipped(
                                        FontFace::Regular,
                                        ui.font_engine,
                                        v,
                                        (col.x + kw).round(),
                                        baseline,
                                        dfs,
                                        (col.w - kw).max(0.0),
                                        theme.text,
                                    );
                                }
                            });
                        }
                    }
                } // non-parameters secondary windows

                ui.scene.finish_frame();
                ui.state.end_frame();
            }
            (w, h, close_clicked, shot)
        };

        if settings_output_changed {
            self.restart_audio_output();
        }
        if settings_hq_changed {
            self.sync_pipeline_and_params();
        }
        if settings_tray_changed {
            self.sync_tray_icon();
        }

        if let Some(required) = parameters_required_height {
            if let Some(mw) = self.secondary_windows.find_by_key_mut("parameters") {
                let scale = mw.window.scale_factor() as f32;
                let logical_h = (required / scale).ceil() as f64;
                mw.window
                    .set_min_inner_size(Some(winit::dpi::LogicalSize::new(
                        spec.min_width,
                        logical_h,
                    )));
                let changed = self
                    .parameters_window_height
                    .map(|previous| (previous - required).abs() > 0.5)
                    .unwrap_or(true);
                if changed {
                    let logical_w = mw.size.0 as f64 / scale as f64;
                    let _ = mw
                        .window
                        .request_inner_size(winit::dpi::LogicalSize::new(logical_w, logical_h));
                    self.parameters_window_height = Some(required);
                }
            }
        }

        // Phase 3.5 — the Settings window's "Save Configuration" button persists
        // `user.json` (applied here, after the scene-build borrow has ended).
        if self.save_config_req {
            self.save_config_req = false;
            self.save_config();
            if let Some(mw) = self.secondary_windows.find_by_key_mut("settings") {
                mw.window.request_redraw();
            }
        }
        if self.apply_zoom_req {
            self.apply_zoom_req = false;
            self.apply_zoom();
        }

        // --- Parameters-window intents: forward every change to the engine
        // immediately (same frame), then refresh the main window's redraw.
        if params_intents.params_changed || params_intents.pipeline_changed {
            self.sync_pipeline_and_params();
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
        if let Some(idx) = params_intents.soundfont_idx {
            self.selected_soundfont = idx;
            self.config.selected_soundfont = idx;
            if let Some(a) = &self.audio {
                if let Some(p) = self.soundfonts.get(idx) {
                    a.set_soundfont(p.clone());
                }
            }
        }
        if params_intents.tracker_mutes_changed {
            if let Some(audio) = &self.audio {
                audio.set_tracker_mutes(self.tracker_mutes.clone());
            }
        }
        if params_intents.open_soundfonts_folder {
            // Always the user-writable config folder: the bundled folder inside
            // Program Files is read-only and offering it for font drops only
            // produces failed copies.
            let folder = crate::config::UserConfig::soundfonts_dir();
            let opened = std::fs::create_dir_all(&folder).and_then(|_| {
                std::process::Command::new("explorer.exe")
                    .arg(&folder)
                    .spawn()
                    .map(|_| ())
            });
            if let Err(e) = opened {
                eprintln!("[parameters] failed to open SoundFont folder: {e}");
            }
        }

        if close_clicked {
            // The Mixer owns a real engine and is destroyed on close; the other
            // secondary windows only hide their rendering surfaces.
            self.secondary_windows.hide(key);
            if key == "parameters" {
                self.parameters_window_height = None;
                self.on_parameters_window_closed();
            }
            if key == "mixer" {
                self.close_mixer();
            }
            self.restore_focus_after_hide(key);
            if let Some(w) = &self.window {
                w.request_redraw();
            }
            return;
        }

        if let Some(mw) = self.secondary_windows.find_by_key_mut(key) {
            mw.renderer
                .render(w, h, &mw.scene, &mut self.text_sys, shot.as_deref());
            if let Some(p) = shot {
                println!("[poc] saved screenshot to {}", p);
            }
        }
    }
}

fn fmt_time(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let min_h = if self.show_controls {
            Self::MIN_HEIGHT_WITH_CONTROLS
        } else {
            Self::HEIGHT_WITHOUT_CONTROLS
        };
        let mut attrs = Window::default_attributes()
            .with_title("SoundApp")
            .with_inner_size(winit::dpi::LogicalSize::new(
                self.base_size().0 * self.ui_zoom as f64,
                self.base_size().1 * self.ui_zoom as f64,
            ))
            .with_min_inner_size(winit::dpi::LogicalSize::new(
                Self::WIDTH * self.ui_zoom as f64,
                min_h * self.ui_zoom as f64,
            ))
            .with_resizable(true)
            .with_decorations(false);

        // Restore last-saved placement when it lands on a connected monitor;
        // the saved SIZE wins over the zoom-derived default (zoom changes
        // resize explicitly later, via apply_zoom).
        if let Some(g) = self.saved_geometry("main") {
            if Self::saved_rect_reachable(event_loop, g) {
                attrs = attrs
                    .with_position(winit::dpi::LogicalPosition::new(g.x as f64, g.y as f64))
                    .with_inner_size(winit::dpi::LogicalSize::new(g.w as f64, g.h as f64));
            }
        }

        #[cfg(target_os = "windows")]
        {
            use winit::platform::windows::{Color, CornerPreference, WindowAttributesExtWindows};
            attrs = attrs
                .with_undecorated_shadow(true)
                .with_corner_preference(CornerPreference::Round)
                .with_border_color(Some(Color::from_rgb(55, 55, 55)));
        }

        let main_backend = self.main_backend();
        let (window, mut renderer) =
            Renderer::new(event_loop, attrs, main_backend, self.theme_base());
        nui::set_window_icon_from_executable(&window, 1);
        nui::install_taskbar_group(&window);
        let sz = window.inner_size();
        self.size = (sz.width, sz.height);
        self.window = Some(window.clone());
        renderer.request_reveal();
        self.renderer = Some(renderer);
        self.render();
        self.sync_tray_icon();

        // Phase 1.5: `--open-window <key>` pops the requested secondary window at
        // startup; a `--screenshot` alongside it captures THAT window, not the primary.
        if let Some(spec) = self.open_window_spec {
            let spec = self.adjusted_window_spec(spec);
            let backend = self.secondary_backend(spec.key);
            self.secondary_windows.show(event_loop, &spec, backend);
            if spec.key == "mixer" {
                self.open_mixer_from_playlist();
            }
            if let Some(path) = self.screenshot_path.take() {
                if let Some(w) = self.secondary_windows.find_by_key_mut(spec.key) {
                    w.screenshot_once = Some(path);
                }
            }
        }

        // Reopen the secondaries that were visible at exit (position and size
        // come through the first-creation path in process_window_toggles).
        // The mixer is NOT auto-reopened: it owns a live engine whose stems
        // come from this session's playlist, which starts empty.
        if self.open_window_spec.is_none() {
            for key in ["parameters", "monitoring", "settings", "help"] {
                if self.saved_geometry(key).is_some_and(|g| g.visible) {
                    self.pending_window_toggles.push(key);
                }
            }
            self.process_window_toggles(event_loop);
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        // Global keys first: window toggles, theme, controls and the Explorer
        // reveal work from ANY app window. (ModifiersChanged events keep
        // `self.modifiers` current before the key arrives, whichever branch
        // owns the window.)
        if let WindowEvent::KeyboardInput { event: key, .. } = &event {
            if key.state == ElementState::Pressed
                && self.handle_global_shortcut(event_loop, key)
            {
                return;
            }
        }
        // Phase 1.5 routing: the primary window owns the process lifetime; every other
        // window is a managed secondary (Escape / CloseRequested hide, not exit).
        if self.window.as_ref().map(|w| w.id()) != Some(id) {
            if self.secondary_windows.find_by_id_mut(id).is_some() {
                self.secondary_window_event(event_loop, id, event);
            }
            return;
        }
        match event {
            WindowEvent::CloseRequested => self.request_close(event_loop),
            WindowEvent::Resized(sz) => {
                self.size = (sz.width, sz.height);
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                if let Some(w) = &self.window {
                    let sz = w.inner_size();
                    self.size = (sz.width, sz.height);
                    w.request_redraw();
                }
            }
            WindowEvent::Focused(true) => {
                self.note_focus("main");
                nui::restore_taskbar_group_if_needed();
            }
            WindowEvent::HoveredFile(path) => {
                self.refresh_drag_cursor();
                self.ui_state.set_hovered_file(Some(path));
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            WindowEvent::HoveredFileCancelled => {
                self.ui_state.set_hovered_file(None);
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            WindowEvent::DroppedFile(path) => {
                self.refresh_drag_cursor();
                self.ui_state.set_hovered_file(None);
                self.ui_state.dropped_files.push(path);
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            WindowEvent::ModifiersChanged(m) => {
                self.modifiers = m.state();
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.ui_state.cursor = (position.x as f32, position.y as f32);
                // Undecorated shell: show the native resize arrow over the 6px edge
                // band (12px corners) so press and hover agree on what the drag does.
                if let Some(w) = &self.window {
                    nui::update_resize_cursor(w, self.ui_state.cursor, self.size);
                }
                // While playing with animation on, the 60fps `about_to_wait` timer already
                // redraws every frame — only force a redraw here for immediate drag feedback
                // (mouse held) or when the timer is idle, so a 1000Hz mouse cannot spike FPS.
                let playing = self.audio.as_ref().map(|a| a.is_playing()).unwrap_or(false);
                if !(playing && self.animate) || self.ui_state.mouse_down {
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => y,
                    winit::event::MouseScrollDelta::PixelDelta(p) => (p.y as f32) / 40.0,
                };
                if dy.abs() > 0.01 {
                    if self.modifiers.control_key() {
                        let step = if dy > 0.0 { 0.1 } else { -0.1 };
                        self.set_zoom(self.ui_zoom + step);
                    } else {
                        // Wheel anywhere in the main window trims the master volume.
                        self.master_vol = (self.master_vol + 0.05 * dy.signum()).clamp(0.0, 2.0);
                        if let Some(a) = &self.audio {
                            a.set_volume(self.master_vol);
                        }
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                    }
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                match button {
                    MouseButton::Left => match state {
                        ElementState::Pressed => {
                            let (cw, ch) = (self.size.0 as f32, self.size.1 as f32);
                            // Undecorated window: dragging within the 6 px edge band or
                            // the 12 px corners resizes, like a native frame.
                            if let Some(w) = self.window.clone() {
                                if nui::try_start_window_resize(&w, self.ui_state.cursor, self.size)
                                {
                                    return;
                                }
                            }
                            let scale = (cw / Self::WIDTH as f32)
                                .min(
                                    ch / if self.show_controls {
                                        Self::HEIGHT_WITH_CONTROLS as f32
                                    } else {
                                        Self::HEIGHT_WITHOUT_CONTROLS as f32
                                    },
                                )
                                .max(0.5);
                            let top_h = (28.0 * scale).round();
                            let pw = self.size.0 as f32;
                            // Drag by the empty middle of the top bar (between the
                            // track counter and the right-side settings/close pair).
                            let drag_zone = self.ui_state.cursor.1 < top_h
                                && self.ui_state.cursor.0 > 130.0 * scale
                                && self.ui_state.cursor.0 < pw - 72.0 * scale;
                            if drag_zone {
                                if let Some(w) = self.window.clone() {
                                    nui::drag_window_live(&w);
                                }
                            } else {
                                self.ui_state.on_mouse_press();
                                if let Some(w) = &self.window {
                                    w.request_redraw();
                                }
                            }
                        }
                        ElementState::Released => {
                            self.ui_state.on_mouse_release();
                            if let Some(w) = &self.window {
                                w.request_redraw();
                            }
                        }
                    },
                    MouseButton::Right => {
                        if state == ElementState::Pressed {
                            self.ui_state.right_pressed = true;
                            if let Some(w) = &self.window {
                                w.request_redraw();
                            }
                        }
                    }
                    _ => {}
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed {
                    let focused_text = self.any_text_input_focused();

                    match &event.logical_key {
                        Key::Named(NamedKey::Escape) => {
                            if self.ui_state.open_popup_id.is_some() {
                                self.ui_state.open_popup_id = None;
                                self.ui_state.popup_rect = None;
                            } else if focused_text {
                                self.ui_state.key_cmds.push(KeyCmd::Escape);
                            } else {
                                self.request_close(event_loop);
                                return;
                            }
                        }
                        Key::Named(NamedKey::Backspace) => {
                            self.ui_state.key_cmds.push(KeyCmd::Backspace);
                        }
                        Key::Named(NamedKey::Delete) => {
                            self.ui_state.key_cmds.push(KeyCmd::Delete);
                        }
                        Key::Named(NamedKey::ArrowLeft) => {
                            if focused_text {
                                self.ui_state.key_cmds.push(KeyCmd::Left);
                            } else if self.modifiers.control_key() {
                                if let Some(a) = &self.audio {
                                    a.seek((a.position_secs() - 10.0).max(0.0));
                                }
                            } else if self.retreat_track(true) {
                                self.animate = true;
                            }
                        }
                        Key::Named(NamedKey::ArrowRight) => {
                            if focused_text {
                                self.ui_state.key_cmds.push(KeyCmd::Right);
                            } else if self.modifiers.control_key() {
                                if let Some(a) = &self.audio {
                                    a.seek(a.position_secs() + 10.0);
                                }
                            } else if self.advance_track(true) {
                                self.animate = true;
                            }
                        }
                        Key::Named(NamedKey::ArrowUp) if !focused_text => {
                            self.master_vol = (self.master_vol + 0.05).clamp(0.0, 2.0);
                            if let Some(a) = &self.audio {
                                a.set_volume(self.master_vol);
                            }
                        }
                        Key::Named(NamedKey::ArrowDown) if !focused_text => {
                            self.master_vol = (self.master_vol - 0.05).clamp(0.0, 2.0);
                            if let Some(a) = &self.audio {
                                a.set_volume(self.master_vol);
                            }
                        }
                        Key::Named(NamedKey::Space) if !focused_text && !event.repeat => {
                            if let Some(a) = &self.audio {
                                self.animate = a.toggle_play();
                            }
                        }
                        Key::Named(NamedKey::Home) => {
                            self.ui_state.key_cmds.push(KeyCmd::Home);
                        }
                        Key::Named(NamedKey::End) => {
                            self.ui_state.key_cmds.push(KeyCmd::End);
                        }
                        Key::Named(NamedKey::Enter) => {
                            self.ui_state.key_cmds.push(KeyCmd::Enter);
                        }
                        Key::Named(NamedKey::Tab) => {
                            if self.modifiers.shift_key() {
                                self.ui_state.key_cmds.push(KeyCmd::ShiftTab);
                            } else {
                                self.ui_state.key_cmds.push(KeyCmd::Tab);
                            }
                        }
                        Key::Character(ch)
                            if self.modifiers.control_key() && ch.eq_ignore_ascii_case("a") =>
                        {
                            self.ui_state.key_cmds.push(KeyCmd::SelectAll);
                        }
                        Key::Character(ch)
                            if self.modifiers.control_key() && ch.eq_ignore_ascii_case("c") =>
                        {
                            self.ui_state.key_cmds.push(KeyCmd::Copy);
                        }
                        Key::Character(ch)
                            if self.modifiers.control_key() && ch.eq_ignore_ascii_case("x") =>
                        {
                            self.ui_state.key_cmds.push(KeyCmd::Cut);
                        }
                        Key::Character(ch)
                            if self.modifiers.control_key() && ch.eq_ignore_ascii_case("v") =>
                        {
                            self.ui_state.key_cmds.push(KeyCmd::Paste);
                        }
                        Key::Character(ch) if !focused_text && ch.eq_ignore_ascii_case("f") => {
                            self.font_engine = match self.font_engine {
                                FontEngine::Swash => FontEngine::Fontdue,
                                FontEngine::Fontdue => FontEngine::Swash,
                            };
                        }
                        Key::Character(ch)
                            if (!focused_text || self.modifiers.control_key())
                                && (ch == "+" || ch == "=") =>
                        {
                            self.set_zoom(self.ui_zoom + 0.1);
                        }
                        Key::Character(ch)
                            if (!focused_text || self.modifiers.control_key())
                                && (ch == "-" || ch == "_") =>
                        {
                            self.set_zoom(self.ui_zoom - 0.1);
                        }
                        Key::Character(ch)
                            if (!focused_text || self.modifiers.control_key()) && ch == "0" =>
                        {
                            self.set_zoom(1.0);
                        }
                        Key::Character(ch)
                            if !focused_text && !event.repeat && ch.eq_ignore_ascii_case("l") =>
                        {
                            self.loop_enabled = !self.loop_enabled;
                            if let Some(a) = &self.audio {
                                a.set_loop(self.loop_enabled);
                            }
                        }
                        _ => {
                            if focused_text && !self.modifiers.control_key() {
                                if let Some(txt) = &event.text {
                                    for c in txt.chars() {
                                        if !c.is_control() {
                                            self.ui_state.typed_chars.push(c);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                self.render();
                // Execute queued P/N/M/S/H toggles now that `event_loop` is available.
                self.process_window_toggles(event_loop);
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        match nui::take_tray_action() {
            Some(nui::TrayAction::Show) => self.reveal_and_focus_main(),
            Some(nui::TrayAction::Quit) => {
                self.quitting = true;
                self.quit_at = Some(Instant::now());
            }
            None => {}
        }
        if let Some(q) = self.quit_at {
            if Instant::now() >= q {
                if !self.quitting {
                    self.quitting = true;
                    println!("[poc] auto-quit reached");
                    self.flush_pacing("auto-quit");
                }
                event_loop.exit();
                return;
            }
        }
        self.poll_open_requests();
        self.poll_transport_and_scans();
        self.poll_mixer();

        let playing = self.audio.as_ref().map(|a| a.is_playing()).unwrap_or(false);
        let mixer_running = self.secondary_windows.is_visible("mixer")
            && self
                .mixer_audio
                .as_ref()
                .map(|engine| engine.is_playing())
                .unwrap_or(false);
        let pending_meta =
            self.audio.is_some() && !self.playlist.is_empty() && self.track_meta.is_none();
        let file_drag = self.ui_state.hovered_file.is_some();
        let sync_debug = self.mixer_sync_debug && self.secondary_windows.is_visible("mixer");
        // Widget transitions (hover fades, popups, dialogs) drive their own 60 fps
        // window so they finish and then the loop drops back to `Wait` / 0% CPU.
        let ui_anim = self.ui_state.is_animating() || self.secondary_windows.any_animating();
        let cover_anim = self.cover_fade.is_some();
        let animating = (playing && self.animate)
            || pending_meta
            || ui_anim
            || mixer_running
            || file_drag
            || sync_debug
            || cover_anim;

        let now = Instant::now();
        if animating && now >= self.next_frame {
            // Low-footprint cadence: the base player only needs ~15 fps for its
            // progress bar and MM:SS text; full-rate only while a DSP window is up.
            let dsp_open = self.secondary_windows.is_visible("monitoring")
                || self.secondary_windows.is_visible("mixer");
            let frame_ms = if pending_meta || dsp_open || ui_anim || file_drag || sync_debug || cover_anim {
                self.frame_ms
            } else {
                66
            };
            self.next_frame = now + Duration::from_millis(frame_ms);
            self.t += frame_ms as f32 / 1000.0;
            if let Some(w) = &self.window {
                w.request_redraw();
            }
            // Keep visible secondary windows (Monitoring spectrum, Mixer meters) alive.
            self.secondary_windows.request_redraw_all();
        }

        let mut next: Option<Instant> = if animating {
            Some(self.next_frame)
        } else {
            None
        };
        if let Some(q) = self.quit_at {
            next = Some(match next {
                Some(n) if n < q => n,
                _ => q,
            });
        }
        match next {
            Some(t) => event_loop.set_control_flow(ControlFlow::WaitUntil(t)),
            None => event_loop.set_control_flow(ControlFlow::Wait),
        }
    }

    /// Phase 3.5 — auto-save the configuration on a clean exit.
    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.capture_window_geometry();
        self.config.default_dir = self.default_dir.clone();
        self.config.dark_mode = self.dark_mode;
        self.config.ui_zoom = self.ui_zoom;
        self.config.master_vol = self.master_vol;
        self.config.show_controls = self.show_controls;
        self.config.chk_locked = self.chk_locked;
        self.config.sw_hq_mode = self.sw_hq_mode;
        self.config.sw_keep_tray = self.sw_keep_tray;
        self.config.output_device = self
            .selected_device_idx
            .checked_sub(1)
            .and_then(|idx| self.audio_devices.get(idx))
            .cloned()
            .unwrap_or_default();
        self.config.selected_soundfont = self.selected_soundfont;
        self.flush_pacing("exit");
        if let Err(e) = self.config.save() {
            eprintln!("[poc] config save failed: {e}");
        }
    }
}

/// The release build has no console; when launched from a terminal, attach the
/// parent console and reroute stdout/stderr to it so harness output
/// (--render, --latency-probe, screenshots, timings) stays visible. A stdout
/// that is already a pipe or file (measure.ps1, `> log`) is left untouched,
/// and double-clicked launches fail the attach silently — no console flash.
fn attach_parent_console() {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetStdHandle(n_std_handle: u32) -> *mut std::ffi::c_void;
        fn GetFileType(h_file: *mut std::ffi::c_void) -> u32;
        fn AttachConsole(dw_process_id: u32) -> i32;
        fn SetStdHandle(n_std_handle: u32, h_handle: *mut std::ffi::c_void) -> i32;
    }
    const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
    const STD_ERROR_HANDLE: u32 = -12i32 as u32;
    const FILE_TYPE_UNKNOWN: u32 = 0;
    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;

    use std::os::windows::io::AsRawHandle;
    unsafe {
        let out = GetStdHandle(STD_OUTPUT_HANDLE);
        let ty = if out.is_null() {
            FILE_TYPE_UNKNOWN
        } else {
            GetFileType(out)
        };
        if ty != FILE_TYPE_UNKNOWN {
            // Console (dev build), pipe or file: output is already routed.
            return;
        }
        if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            return;
        }
        for slot in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            if let Ok(con) = std::fs::OpenOptions::new().write(true).open("CONOUT$") {
                // Leak the handle: it must outlive every println! in the process.
                SetStdHandle(slot, con.as_raw_handle() as *mut std::ffi::c_void);
                std::mem::forget(con);
            }
        }
    }
}

fn main() {
    attach_parent_console();
    perf::init();
    let mut file: Option<String> = None;
    let mut tone = false;
    let mut silence = false;
    let mut animate = true;
    let mut nopaint = false;
    let mut seconds: Option<f64> = None;
    let mut fps: f64 = 60.0;
    let mut backend = BackendKind::Gpu;
    let mut backend_locked = false;
    let mut font_engine = FontEngine::Swash;
    let mut app_screenshot: Option<String> = None;
    let mut open_window: Option<String> = None;
    let mut sync_debug = false;
    // Offline render / latency harness (phase A instruments).
    let mut render_in: Option<String> = None;
    let mut render_out: Option<String> = None;
    let mut render_pitch: f32 = 0.0;
    let mut render_tempo: f32 = 1.0;
    let mut render_formant = false;
    let mut render_tape: Option<f32> = None;
    let mut render_limit: Option<f64> = None;
    // Native tracker (libopenmpt) render parameters — applied by the module
    // engine itself; shared flags (--pitch/--tempo/--tape) affect FFmpeg only.
    let mut trk_pitch: f32 = 0.0;
    let mut trk_tempo: f32 = 1.0;
    let mut trk_separation: f32 = 100.0;
    let mut trk_interp: i32 = 0;
    let mut latency_probe = false;

    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--tone" => tone = true,
            "--silence" => silence = true,
            "--static" => animate = false,
            "--nopaint" => nopaint = true,
            "--open-window" => open_window = args.next(),
            "--sync-debug" => sync_debug = true,
            "--render" => render_in = args.next(),
            "--out" => render_out = args.next(),
            "--pitch" => render_pitch = args.next().and_then(|v| v.parse().ok()).unwrap_or(0.0),
            "--tempo" => render_tempo = args.next().and_then(|v| v.parse().ok()).unwrap_or(1.0),
            "--formant" => render_formant = true,
            "--no-formant" => render_formant = false,
            "--tape" => render_tape = args.next().and_then(|v| v.parse().ok()),
            "--limit" => render_limit = args.next().and_then(|v| v.parse().ok()),
            "--trk-pitch" => trk_pitch = args.next().and_then(|v| v.parse().ok()).unwrap_or(0.0),
            "--trk-tempo" => trk_tempo = args.next().and_then(|v| v.parse().ok()).unwrap_or(1.0),
            "--trk-separation" => {
                trk_separation = args.next().and_then(|v| v.parse().ok()).unwrap_or(100.0)
            }
            "--trk-interp" => trk_interp = args.next().and_then(|v| v.parse().ok()).unwrap_or(0),
            "--latency-probe" => latency_probe = true,
            "--backend" => {
                if let Some(b) = args.next() {
                    backend_locked = true;
                    if b.eq_ignore_ascii_case("cpu") || b.eq_ignore_ascii_case("softbuffer") {
                        backend = BackendKind::Cpu;
                    } else {
                        backend = BackendKind::Gpu;
                    }
                }
            }
            "--font" => {
                if let Some(f) = args.next() {
                    if f.eq_ignore_ascii_case("fontdue") {
                        font_engine = FontEngine::Fontdue;
                    } else {
                        font_engine = FontEngine::Swash;
                    }
                }
            }
            "--seconds" => seconds = args.next().and_then(|v| v.parse().ok()),
            "--fps" => fps = args.next().and_then(|v| v.parse().ok()).unwrap_or(60.0),
            "--screenshot" => app_screenshot = args.next(),
            other => file = Some(other.to_string()),
        }
    }
    const DEFAULT_MURR: &str = r"Y:\Test Files\Murr_01.mp3";
    const DEFAULT_START: &str = r"D:\Work\_GIT\SoundApp\_Material\mixer_media\Again\01.m4a";
    // No file given → prefer the reference test track; its implicit folder
    // playlist then collects every playable file beside it. `--tone` / `--silence`
    // skip the startup path entirely.
    let default_start = if std::path::Path::new(DEFAULT_MURR).exists() {
        DEFAULT_MURR
    } else {
        DEFAULT_START
    };
    let cfg = UserConfig::load();
    let startup_path: Option<PathBuf> = if tone || silence {
        None
    } else {
        Some(match file.as_ref().map(PathBuf::from) {
            Some(f) => f,
            None => {
                // Default directory: the configured folder when set, else the
                // OS Music known folder. Its first audio file (sorted) becomes
                // the startup track; the implicit sibling-folder playlist then
                // behaves exactly like any dropped file. The dev reference
                // tracks remain the last-resort fallback.
                let dir = if cfg.default_dir.is_empty() {
                    known_music_dir()
                } else {
                    Some(PathBuf::from(&cfg.default_dir))
                };
                dir.as_deref()
                    .and_then(first_audio_in_dir)
                    .unwrap_or_else(|| PathBuf::from(default_start))
            }
        })
    };

    // Normal playback launches are single-instance. Harness flags (render,
    // latency probe, screenshot, nopaint, silence/tone) stay independent so
    // benchmarks can run alongside the player.
    let exclusive_launch = render_in.is_none()
        && !latency_probe
        && app_screenshot.is_none()
        && seconds.is_none()
        && !nopaint
        && !tone
        && !silence
        && open_window.is_none();
    let mut open_requests: Option<Receiver<String>> = None;
    if exclusive_launch {
        match single_instance::claim() {
            Ok(true) => {
                let (sender, receiver) = std::sync::mpsc::channel();
                single_instance::start_server(sender);
                open_requests = Some(receiver);
            }
            Ok(false) => {
                let forwarded = file.as_deref().map(PathBuf::from);
                if !single_instance::forward(forwarded.as_deref()) {
                    eprintln!("[single-instance] running player did not accept the request");
                }
                return;
            }
            Err(error) => eprintln!("[single-instance] {error}; continuing without the gate"),
        }
    }

    // Offline render: same DSP path as live playback, no audio device, no
    // window. Runs faster than realtime and exits.
    if let Some(in_path) = render_in {
        let out = render_out.unwrap_or_else(|| "render_out.wav".to_string());
        let interp = tracker::Interpolation::from_filter_length(trk_interp)
            .unwrap_or(tracker::Interpolation::Default);
        let params = PlaybackParams {
            mode_tape: render_tape.is_some(),
            pitch_semitones: render_tape.unwrap_or(render_pitch),
            tempo_ratio: render_tempo,
            hq: true,
            formant_preserve: render_formant,
            tracker: tracker::TrackerParams {
                pitch_semitones: trk_pitch,
                tempo_ratio: trk_tempo,
                stereo_separation: trk_separation,
                interpolation: interp,
            },
            midi: midi::MidiParams::default(),
        };
        match audio::render_offline(
            &PathBuf::from(in_path),
            &params,
            &PathBuf::from(&out),
            render_limit,
            48_000,
            &midi::default_soundfont(),
        ) {
            Ok(r) => {
                println!(
                    "[render] in={:.3}s out={:.3}s mode={} pitch={:+.1} tempo={:.2} trk(pitch={:+.1} tempo={:.2} sep={:.0} interp={}) -> {}",
                    r.in_secs,
                    r.out_secs,
                    if params.mode_tape { "tape" } else { "pitchtime" },
                    params.pitch_semitones,
                    params.tempo_ratio,
                    params.tracker.pitch_semitones,
                    params.tracker.tempo_ratio,
                    params.tracker.stereo_separation,
                    params.tracker.interpolation.filter_length(),
                    out
                );
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("[render] failed: {e}");
                std::process::exit(1);
            }
        }
    }

    // Live-path parameter-latency measurement: play the default/arg track for
    // 4 s (ring fills), then change pitch — the decoder thread logs how many
    // seconds of pre-change audio the ring still holds (the audible delay).
    if latency_probe {
        let a = match audio::start(crate::audio::Source::Silence) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("[probe] audio init failed: {e}");
                std::process::exit(1);
            }
        };
        let path = startup_path
            .clone()
            .unwrap_or_else(|| PathBuf::from(default_start));
        println!("[probe] loading {}", path.display());
        a.load_file(path);
        std::thread::sleep(std::time::Duration::from_secs(4));
        let t0 = Instant::now();
        let p1 = PlaybackParams {
            pitch_semitones: 5.0,
            ..Default::default()
        };
        a.set_params(p1);
        println!(
            "[probe] pitch +5 sent at t={:.2}s (pos {:.2}s)",
            t0.elapsed().as_secs_f32(),
            a.position_secs()
        );
        for _ in 0..3 {
            std::thread::sleep(std::time::Duration::from_millis(500));
            println!(
                "[probe]   t={:.2}s pos {:.3}s",
                t0.elapsed().as_secs_f32(),
                a.position_secs()
            );
        }
        a.seek(30.0);
        println!("[probe] seek(30) at t={:.2}s", t0.elapsed().as_secs_f32());
        for _ in 0..2 {
            std::thread::sleep(std::time::Duration::from_millis(500));
            println!(
                "[probe]   t={:.2}s pos {:.3}s",
                t0.elapsed().as_secs_f32(),
                a.position_secs()
            );
        }
        let p2 = PlaybackParams {
            pitch_semitones: 6.0,
            tempo_ratio: 1.25,
            ..Default::default()
        };
        a.set_params(p2);
        println!(
            "[probe] pitch +6 tempo 1.25 sent at t={:.2}s (pos {:.2}s)",
            t0.elapsed().as_secs_f32(),
            a.position_secs()
        );
        for _ in 0..3 {
            std::thread::sleep(std::time::Duration::from_millis(500));
            println!(
                "[probe]   t={:.2}s pos {:.3}s",
                t0.elapsed().as_secs_f32(),
                a.position_secs()
            );
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
        println!(
            "[probe] end pos {:.3}s / dur {:.3}s",
            a.position_secs(),
            a.duration_secs()
        );
        std::process::exit(0);
    }

    let source = if tone {        Source::Tone { hz: 440.0 }
    } else {
        // The startup playlist (file or folder) is loaded by `open_path` below;
        // the engine starts on silence so nothing is decoded twice.
        Source::Silence
    };

    let audio = match audio::start_with(source, cfg.output_device_name()) {
        Ok(a) => Some(a),
        Err(e) => {
            eprintln!("[poc] audio init failed: {e}");
            None
        }
    };

    let event_loop = EventLoop::new().unwrap();
    event_loop.set_control_flow(ControlFlow::Wait);
    let quit_at = seconds.map(|s| Instant::now() + Duration::from_secs_f64(s));
    println!("[poc] config: {}", UserConfig::path().display());
    let mut app = App::new(audio, animate, quit_at, backend, font_engine, cfg);
    app.open_requests = open_requests;
    app.backend_locked = backend_locked;
    // Push the initial UI state into the engine (gain + the full parameter set).
    let initial_params = app.playback_params();
    let initial_vol = app.master_vol;
    if let Some(a) = &app.audio {
        a.set_volume(initial_vol);
        a.set_params(initial_params);
    }
    // Collect the startup playlist: a folder recurses (recursive=true), a file
    // collects its sibling folder and starts at the matched index.
    if let Some(path) = startup_path {
        app.open_path(path, false, true);
    }
    if fps > 0.0 {
        app.frame_ms = (1000.0 / fps).round().max(1.0) as u64;
    }
    app.nopaint = nopaint;
    app.screenshot_path = app_screenshot;
    // Phase 1.5: pop a secondary window at startup. With `--screenshot` alongside it,
    // the capture is taken from THAT window (handled in `resumed`).
    app.open_window_spec = open_window.as_deref().and_then(window_spec_by_key);
    app.mixer_sync_debug = sync_debug;
    if let Some(k) = open_window.as_deref() {
        if app.open_window_spec.is_none() {
            eprintln!("[poc] --open-window: unknown key '{k}' (parameters|monitoring|mixer|settings|help)");
        }
    }
    event_loop.run_app(&mut app).unwrap();
}

#[cfg(test)]
mod playlist_tests {
    use super::*;
    use std::fs;

    #[test]
    fn drop_zone_partitions_match_the_drawn_overlay() {
        let (pw, ph) = (480.0, 278.0);
        assert_eq!(drop_zone_at(100.0, 50.0, pw, ph), DropZone::Add);
        assert_eq!(drop_zone_at(100.0, 200.0, pw, ph), DropZone::Replace);
        assert_eq!(drop_zone_at(450.0, 50.0, pw, ph), DropZone::Mixer);
        assert_eq!(drop_zone_at(450.0, 200.0, pw, ph), DropZone::Mixer);
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "soundapp_playlist_{}_{}_{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn supported_audio_extensions_are_case_insensitive() {
        assert!(is_supported_audio(std::path::Path::new("a.MP3")));
        assert!(is_supported_audio(std::path::Path::new("b.FlAc")));
        assert!(!is_supported_audio(std::path::Path::new("c.txt")));
        assert!(!is_supported_audio(std::path::Path::new("noext")));
    }

    #[test]
    fn tracker_modules_are_playlist_eligible() {
        // libopenmpt claims these — the playlist scan must list them so a
        // module dropped into a music folder shows up beside the mp3s.
        for ext in ["mod", "s3m", "xm", "it", "XM", "MO3"] {
            let name = format!("track.{ext}");
            let p = std::path::Path::new(&name);
            assert!(is_supported_audio(p), "{ext} must be playable");
        }
        // FFmpeg formats the library does not claim stay routed to FFmpeg.
        assert!(is_supported_audio(std::path::Path::new("x.mp3")));
        assert!(is_supported_audio(std::path::Path::new("x.wav")));
    }

    #[test]
    fn single_file_collects_siblings_and_finds_index() {
        let dir = temp_dir("single");
        for name in ["01.m4a", "02.m4a", "notes.txt"] {
            fs::write(dir.join(name), b"x").unwrap();
        }
        let (list, idx) = playlist_from_single_file(&dir.join("02.m4a"));
        assert_eq!(list.len(), 2);
        assert_eq!(list[idx].file_name().unwrap(), "02.m4a");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn folder_collection_respects_recursion() {
        let dir = temp_dir("folder");
        let sub = dir.join("disc2");
        fs::create_dir_all(&sub).unwrap();
        fs::write(dir.join("a.mp3"), b"x").unwrap();
        fs::write(dir.join("b.txt"), b"x").unwrap();
        fs::write(sub.join("c.flac"), b"x").unwrap();
        assert_eq!(playlist_from_folder(&dir, false).len(), 1);
        assert_eq!(playlist_from_folder(&dir, true).len(), 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn single_file_with_empty_folder_returns_itself() {
        let dir = temp_dir("empty");
        let file = dir.join("only.ogg");
        fs::write(&file, b"x").unwrap();
        let (list, idx) = playlist_from_single_file(&file);
        assert_eq!(list, vec![file]);
        assert_eq!(idx, 0);
        let _ = fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod params_tests {
    use super::*;

    fn modified() -> ParamState {
        ParamState {
            tape: false,
            tape_speed_st: 5.0,
            pitch_st: 7.0,
            stretch: 1.5,
            formant: true,
            trk_pitch_st: 3.0,
            trk_tempo_pct: 150.0,
            trk_separation: 140.0,
            trk_interp: tracker::Interpolation::Cubic,
            midi_transpose_st: 2.0,
            midi_bpm: 90.0,
            midi_bpm_override: true,
            midi_metronome: true,
        }
    }

    #[test]
    fn defaults_are_tape_neutral() {
        let p = ParamState::default();
        assert!(p.tape);
        assert_eq!(p.tape_speed_st, 0.0);
        assert_eq!(p.pitch_st, 0.0);
        assert_eq!(p.stretch, 1.0);
        assert!(!p.formant);
        let e = p.engine_params(true);
        assert!(e.mode_tape);
        assert_eq!(e.tempo_ratio, 1.0);
        assert_eq!(e.pitch_semitones, 0.0);
        assert!(!e.formant_preserve);
    }

    #[test]
    fn tape_semitones_map_exponentially() {
        let up = ParamState {
            tape_speed_st: 12.0,
            ..ParamState::default()
        };
        assert!((up.tape_ratio() - 2.0).abs() < 1e-4);
        let down = ParamState {
            tape_speed_st: -12.0,
            ..ParamState::default()
        };
        assert!((down.tape_ratio() - 0.5).abs() < 1e-4);
    }

    #[test]
    fn tape_engine_ignores_pitchtime_sliders() {
        let p = ParamState {
            pitch_st: 7.0,
            stretch: 1.5,
            formant: true,
            ..ParamState::default()
        };
        let e = p.engine_params(false);
        assert!(e.mode_tape);
        assert_eq!(e.pitch_semitones, 0.0);
        assert_eq!(e.tempo_ratio, 1.0);
    }

    #[test]
    fn pitchtime_engine_passes_sliders_through() {
        let p = ParamState {
            tape: false,
            pitch_st: -3.0,
            stretch: 1.25,
            formant: true,
            ..ParamState::default()
        };
        let e = p.engine_params(true);
        assert!(!e.mode_tape);
        assert_eq!(e.pitch_semitones, -3.0);
        assert_eq!(e.tempo_ratio, 1.25);
        assert!(e.formant_preserve);
    }

    #[test]
    fn rounding_follows_js_math_round_not_rust_round() {
        // JS `Math.round` sends exact halves up; `f32::round` sends them away
        // from zero, so a ported `round()` would read a semitone low at every
        // negative half position.
        assert_eq!(js_round(2.5), 3.0);
        assert_eq!(js_round(-2.5), -2.0);
        assert_eq!(js_round(-2.6), -3.0);
        assert_eq!(js_round(-0.5), 0.0);
        assert_eq!(js_round(0.49), 0.0);
    }

    #[test]
    fn snap_semitones_quantizes_to_musical_steps() {
        assert_eq!(snap_semitones(3.47), 3.0);
        assert_eq!(snap_semitones(-2.5), -2.0); // JS Math.round: halves go up
        assert_eq!(snap_semitones(2.5), 3.0);
        assert_eq!(snap_semitones(0.4), 0.0);
        assert_eq!(snap_semitones(13.0), 12.0); // clamp even if the caller didn't
                                                // -0 must never render as "-0"; the reference shows "+0".
        assert_eq!(snap_semitones(-0.4), 0.0);
        assert_eq!(format!("{:+.0}", snap_semitones(-0.4)), "+0");
    }

    #[test]
    fn snap_stretch_quantizes_to_whole_percent() {
        assert_eq!(snap_stretch(1.034), 1.03);
        assert_eq!(snap_stretch(1.256), 1.26);
        assert_eq!(snap_stretch(0.5), 0.5);
        assert_eq!(snap_stretch(1.6), 1.5); // slider clamp range
                                            // Quantization is idempotent — stored values stay on the grid.
        let s = snap_stretch(1.234);
        assert_eq!(snap_stretch(s), s);
        // Display is exact: whole percent, no rounding drift.
        assert_eq!(format!("{:.0}", s * 100.0), "123");
    }

    #[test]
    fn snapped_values_map_exactly_to_engine_params() {
        // Tape: whole semitones in, 2^(st/12) tempo ratio out (pitch rides
        // the resample — no independent pitch).
        let p = ParamState {
            tape_speed_st: snap_semitones(2.8),
            ..ParamState::default()
        };
        assert_eq!(p.tape_speed_st, 3.0);
        assert!((p.tape_ratio() - 2.0f32.powf(3.0 / 12.0)).abs() < 1e-6);
        let e = p.engine_params(true);
        assert!(e.mode_tape);
        assert!((e.tempo_ratio - 2.0f32.powf(0.25)).abs() < 1e-6);
        assert_eq!(e.pitch_semitones, 0.0);

        // Pitch/Time: independent whole-semitone pitch and whole-percent tempo.
        let pt = ParamState {
            tape: false,
            pitch_st: snap_semitones(-4.6),
            stretch: snap_stretch(1.07),
            ..ParamState::default()
        };
        let e = pt.engine_params(true);
        assert_eq!(e.pitch_semitones, -5.0);
        assert_eq!(e.tempo_ratio, 1.07);
    }

    #[test]
    fn wheel_detent_moves_exactly_one_visible_unit() {
        // The widget's own detent is 2.5% of the range (0.6 st here) — snapped
        // it would read as one semitone, but 2.5% of the ratio slider would
        // alternate 2%/3%. A detent is re-based on the visible unit instead.
        for st in [-11.0, -7.0, 0.0, 5.0, 11.0] {
            let up = snap_semitones(wheel_step(st, 1.0, 1.0, -12.0, 12.0));
            let down = snap_semitones(wheel_step(st, -1.0, 1.0, -12.0, 12.0));
            assert_eq!(up, st + 1.0, "one notch up from {st}");
            assert_eq!(down, st - 1.0, "one notch down from {st}");
        }
        // Range ends clamp and stay on the grid.
        assert_eq!(
            snap_semitones(wheel_step(12.0, 1.0, 1.0, -12.0, 12.0)),
            12.0
        );
        assert_eq!(
            snap_semitones(wheel_step(-12.0, -1.0, 1.0, -12.0, 12.0)),
            -12.0
        );
    }

    #[test]
    fn wheel_detent_moves_time_stretch_one_percent() {
        // Stretch is driven in a ±0.5 bias domain around 100%; a detent is 1%
        // and never alternates between 2% and 3%.
        let bias = |pct: f32| pct / 100.0 - 1.0;
        let apply = |b: f32| snap_stretch(b + 1.0);
        assert_eq!(apply(wheel_step(bias(100.0), 1.0, 0.01, -0.5, 0.5)), 1.01);
        assert_eq!(apply(wheel_step(bias(103.0), 1.0, 0.01, -0.5, 0.5)), 1.04);
        assert_eq!(apply(wheel_step(bias(103.0), -1.0, 0.01, -0.5, 0.5)), 1.02);
        assert_eq!(apply(wheel_step(bias(150.0), 1.0, 0.01, -0.5, 0.5)), 1.50);
        assert_eq!(apply(wheel_step(bias(50.0), -1.0, 0.01, -0.5, 0.5)), 0.50);
    }

    #[test]
    fn drag_across_the_track_visits_every_semitone_once() {
        // Absolute drag over a 240 px track spanning ±12 st: exactly 25 values,
        // each reached once and no fractional values in between.
        let mut seen: Vec<f32> = Vec::new();
        for px in 0..=240 {
            let v = snap_semitones(-12.0 + (px as f32 / 240.0) * 24.0);
            if seen.last() != Some(&v) {
                seen.push(v);
            }
        }
        assert_eq!(seen.len(), 25);
        assert_eq!(seen.first(), Some(&-12.0));
        assert_eq!(seen.last(), Some(&12.0));
        assert!(seen.windows(2).all(|w| (w[1] - w[0] - 1.0).abs() < 1e-6));
    }

    #[test]
    fn tempo_sweep_visits_every_percent_once() {
        let mut seen: Vec<f32> = Vec::new();
        for px in 0..=100 {
            let bias = -0.5 + (px as f32 / 100.0);
            let v = snap_stretch(bias + 1.0);
            if seen.last() != Some(&v) {
                seen.push(v);
            }
        }
        assert_eq!(seen.len(), 101);
        assert_eq!(seen.first(), Some(&0.5));
        assert_eq!(seen.last(), Some(&1.5));
    }

    fn test_app() -> App {
        App::new(
            None,
            false,
            None,
            BackendKind::Gpu,
            FontEngine::Swash,
            UserConfig::default(),
        )
    }

    #[test]
    fn unlocked_new_track_resets_params_to_defaults() {
        let mut app = test_app();
        app.chk_locked = false;
        app.playlist = vec![PathBuf::from("Z:/soundapp-test-missing.mp3")];
        app.params = modified();
        app.load_playlist_index(0);
        assert_eq!(app.params, ParamState::default());
        // The engine-facing view of the reset state must be neutral too —
        // shared AND native tracker domains.
        let e = app.playback_params();
        assert!(e.mode_tape);
        assert_eq!(e.tempo_ratio, 1.0);
        assert_eq!(e.pitch_semitones, 0.0);
        assert_eq!(e.tracker, tracker::TrackerParams::default());
    }

    #[test]
    fn locked_new_track_keeps_params() {
        let mut app = test_app();
        app.chk_locked = true;
        app.playlist = vec![PathBuf::from("Z:/soundapp-test-missing.mp3")];
        app.params = modified();
        let before = app.params;
        app.load_playlist_index(0);
        assert_eq!(app.params, before);
    }

    #[test]
    fn cover_blend_interpolates_and_defaults_to_tile() {
        let side = COVER_TILE_SIDE as usize;
        let solid = |r: u8, g: u8, b: u8| {
            let mut v = Vec::with_capacity(side * side * 4);
            for _ in 0..side * side {
                v.extend_from_slice(&[r, g, b, 255]);
            }
            (COVER_TILE_SIDE, COVER_TILE_SIDE, v)
        };
        let red = solid(255, 0, 0);
        let blue = solid(0, 0, 255);
        assert_eq!(&blend_covers(Some(&red), Some(&blue), 0.0)[0..4], &[255, 0, 0, 255]);
        assert_eq!(&blend_covers(Some(&red), Some(&blue), 1.0)[0..4], &[0, 0, 255, 255]);
        assert_eq!(&blend_covers(Some(&red), Some(&blue), 0.5)[0..4], &[128, 0, 128, 255]);
        // A None side blends against the solid default-tile color.
        assert_eq!(&blend_covers(None, Some(&red), 0.0)[0..4], &[34, 150, 195, 255]);
        assert_eq!(&blend_covers(Some(&red), None, 1.0)[0..4], &[34, 150, 195, 255]);
    }

    #[test]
    fn cover_fade_completes_to_the_incoming_view() {
        let mut app = test_app();
        let side = COVER_TILE_SIDE;
        let cover = (side, side, vec![7u8; (side as usize) * (side as usize) * 4]);
        app.begin_cover_fade(Some(cover.clone()));
        assert!(app.cover_fade.is_some());
        // Backdate the start so the next step completes the transition.
        if let Some(f) = &mut app.cover_fade {
            f.started = Instant::now() - Duration::from_secs(1);
        }
        app.step_cover_fade();
        assert!(app.cover_fade.is_none());
        let (_, _, px) = app.cover_resting.clone().expect("resting cover");
        assert_eq!(px, cover.2);

        // Fading back to the default tile clears the slot at completion.
        app.begin_cover_fade(None);
        if let Some(f) = &mut app.cover_fade {
            f.started = Instant::now() - Duration::from_secs(1);
        }
        app.step_cover_fade();
        assert!(app.cover_fade.is_none());
        assert!(app.cover_resting.is_none());
    }

    #[test]
    fn window_geometry_parses_and_skips_garbage() {
        let map = parse_window_geometry(
            "main 120 80 756 438 1;parameters 200 140 510 800 0;bogus 1 2 3",
        );
        assert_eq!(
            map.get("main"),
            Some(&SavedGeom {
                x: 120.0,
                y: 80.0,
                w: 756.0,
                h: 438.0,
                visible: true
            })
        );
        assert_eq!(map.get("parameters").map(|g| g.visible), Some(false));
        assert!(!map.contains_key("bogus"));
        assert!(parse_window_geometry("").is_empty());
    }

    #[test]
    fn global_keys_classify() {
        assert_eq!(
            global_action_for("p"),
            Some(GlobalAction::ToggleWindow("parameters"))
        );
        assert_eq!(
            global_action_for("M"),
            Some(GlobalAction::ToggleWindow("mixer"))
        );
        assert_eq!(global_action_for("x"), Some(GlobalAction::ToggleTheme));
        assert_eq!(global_action_for("T"), Some(GlobalAction::ToggleTheme));
        assert_eq!(global_action_for("c"), Some(GlobalAction::ToggleControls));
        assert_eq!(global_action_for("i"), Some(GlobalAction::RevealInExplorer));
        assert_eq!(global_action_for("q"), None);
        assert_eq!(global_action_for(""), None);
    }

    #[test]
    fn unlocked_parameters_close_resets_params() {
        let mut app = test_app();
        app.chk_locked = false;
        app.params = modified();
        app.on_parameters_window_closed();
        assert_eq!(app.params, ParamState::default());
    }

    #[test]
    fn locked_parameters_close_keeps_params() {
        let mut app = test_app();
        app.chk_locked = true;
        app.params = modified();
        let before = app.params;
        app.on_parameters_window_closed();
        assert_eq!(app.params, before);
    }

    #[test]
    fn auto_advance_follows_the_same_reset_rule_as_explicit_load() {
        let mut app = test_app();
        app.chk_locked = false;
        app.playlist = vec![
            PathBuf::from("Z:/soundapp-test-missing-1.mp3"),
            PathBuf::from("Z:/soundapp-test-missing-2.mp3"),
        ];
        app.params = modified();
        assert!(app.advance_track(false));
        assert_eq!(app.params, ParamState::default());
    }

    #[test]
    fn shuffle_order_is_a_permutation_starting_at_the_current_track() {
        let mut app = test_app();
        app.playlist = (0..12)
            .map(|i| PathBuf::from(format!("Z:/soundapp-test-shuf-{i}.mp3")))
            .collect();
        app.selected_track = 5;
        app.shuffle_enabled = true;
        app.rebuild_shuffle_order();
        assert_eq!(app.shuffle_order.len(), 12);
        let mut seen = app.shuffle_order.clone();
        seen.sort_unstable();
        assert_eq!(seen, (0..12).collect::<Vec<usize>>());
        assert_eq!(app.shuffle_order[0], 5);
        // Regenerating for a shorter playlist stays a valid permutation and
        // keeps the (still in-range) current track first.
        app.playlist.truncate(4);
        app.selected_track = 2;
        app.sync_shuffle_state();
        let mut seen = app.shuffle_order.clone();
        seen.sort_unstable();
        assert_eq!(seen, (0..4).collect::<Vec<usize>>());
        assert_eq!(app.shuffle_order[0], 2);
    }

    #[test]
    fn shuffled_advance_visits_every_track_once_and_returns_to_start() {
        let mut app = test_app();
        let n = 9;
        app.playlist = (0..n)
            .map(|i| PathBuf::from(format!("Z:/soundapp-test-adv-{i}.mp3")))
            .collect();
        app.selected_track = 3;
        app.shuffle_enabled = true;
        app.rebuild_shuffle_order();
        let start = app.selected_track;
        assert_eq!(app.shuffle_order[0], start);
        let mut visited = Vec::new();
        for _ in 0..n {
            assert!(app.advance_track(true));
            visited.push(app.selected_track);
        }
        let mut seen = visited;
        seen.sort_unstable();
        assert_eq!(seen, (0..n).collect::<Vec<usize>>());
        assert_eq!(app.selected_track, start);
    }

    #[test]
    fn shuffled_retreat_unwinds_history_then_steps_back_through_the_order() {
        let mut app = test_app();
        app.playlist = (0..7)
            .map(|i| PathBuf::from(format!("Z:/soundapp-test-ret-{i}.mp3")))
            .collect();
        app.selected_track = 0;
        app.shuffle_enabled = true;
        app.rebuild_shuffle_order();
        let start = app.selected_track;
        assert!(app.advance_track(true));
        assert!(app.advance_track(true));
        assert!(app.retreat_track(true));
        assert!(app.retreat_track(true));
        assert_eq!(app.selected_track, start);
        // History exhausted → Prev steps backwards through the shuffle order.
        assert!(app.retreat_track(true));
        assert_ne!(app.selected_track, start);
    }

    #[test]
    fn add_drop_plays_now_only_when_a_track_is_loaded() {
        assert!(add_drop_plays_now(Some(std::path::Path::new("Z:/song.mp3"))));
        assert!(!add_drop_plays_now(None));
    }

    #[test]
    fn add_zone_drop_without_a_loaded_track_keeps_append_only() {
        let mut app = test_app();
        app.playlist = vec![
            PathBuf::from("Z:/soundapp-test-addzone-1.mp3"),
            PathBuf::from("Z:/soundapp-test-addzone-2.mp3"),
        ];
        app.selected_track = 0;
        // test_app has no engine → nothing is loaded → plain append, no jump.
        app.open_path_add_drop(PathBuf::from("Z:/soundapp-test-addzone-3.mp3"), false);
        assert_eq!(app.playlist.len(), 3);
        assert_eq!(app.selected_track, 0);
    }

    #[test]
    fn appending_while_shuffling_regenerates_a_valid_order() {
        let mut app = test_app();
        app.playlist = (0..5)
            .map(|i| PathBuf::from(format!("Z:/soundapp-test-app-{i}.mp3")))
            .collect();
        app.selected_track = 2;
        app.shuffle_enabled = true;
        app.rebuild_shuffle_order();
        // The open_path append funnel keeps order and history valid.
        app.open_path(PathBuf::from("Z:/soundapp-test-app-extra.mp3"), true, false);
        assert_eq!(app.playlist.len(), 6);
        assert_eq!(app.shuffle_order.len(), 6);
        assert_eq!(app.shuffle_order[0], 2);
        assert!(app.shuffle_order.iter().all(|&i| i < 6));
        // Stale history entries are pruned; in-range ones survive truncation.
        app.shuffle_history.clear();
        app.shuffle_history.push_back(1);
        app.shuffle_history.push_back(4);
        app.playlist.truncate(3);
        app.sync_shuffle_state();
        assert!(app.shuffle_history.iter().all(|&i| i < 3));
        // Every shuffled step still resolves inside the live playlist.
        for _ in 0..3 {
            assert!(app.advance_track(true));
            assert!(app.selected_track < app.playlist.len());
        }
    }

    #[test]
    fn tracker_params_map_to_the_engine_domain() {
        // UI percent domain → engine ratio domain; interpolation passes through.
        let e = modified().engine_params(true);
        assert_eq!(e.tracker.pitch_semitones, 3.0);
        assert_eq!(e.tracker.tempo_ratio, 1.5);
        assert_eq!(e.tracker.stereo_separation, 140.0);
        assert_eq!(e.tracker.interpolation, tracker::Interpolation::Cubic);
        // Neutral defaults must hold on the engine side as well.
        let n = ParamState::default().engine_params(true);
        assert_eq!(n.tracker, tracker::TrackerParams::default());
    }

    #[test]
    fn native_tracker_sliders_quantize_like_the_reference() {
        // Whole semitones across libopenmpt's full ±24 range, ±0 normalized.
        assert_eq!(snap_semitones_range(2.5, -24.0, 24.0), 3.0);
        assert_eq!(snap_semitones_range(-2.5, -24.0, 24.0), -2.0);
        assert_eq!(snap_semitones_range(30.0, -24.0, 24.0), 24.0);
        assert_eq!(snap_semitones_range(-0.4, -24.0, 24.0), 0.0);
        let s = snap_semitones_range(-0.0, -24.0, 24.0).to_string();
        assert_eq!(s, "0");
        // Whole percent on the unipolar trackers; clamped at both ends.
        assert_eq!(snap_percent(99.5, 25.0, 400.0), 100.0);
        assert_eq!(snap_percent(10.0, 25.0, 400.0), 25.0);
        assert_eq!(snap_percent(999.0, 25.0, 400.0), 400.0);
        assert_eq!(snap_percent(140.4, 0.0, 200.0), 140.0);
        // One wheel detent = exactly one unit everywhere.
        assert_eq!(snap_percent(100.0 + 1.0, 25.0, 400.0), 101.0);
        assert_eq!(snap_semitones_range(5.0 + 1.0, -24.0, 24.0), 6.0);
    }

    #[test]
    fn reset_tracker_returns_the_native_domain_to_neutral() {
        let mut p = modified();
        p.reset_tracker();
        assert_eq!(p.trk_pitch_st, 0.0);
        assert_eq!(p.trk_tempo_pct, 100.0);
        assert_eq!(p.trk_separation, 100.0);
        assert_eq!(p.trk_interp, tracker::Interpolation::Default);
        // Shared controls are untouched by the native reset.
        assert_eq!(p.tape_speed_st, 5.0);
        assert_eq!(p.stretch, 1.5);
    }

    #[test]
    fn tracker_click_makes_exclusive_solo_and_second_click_restores_all() {
        let mut mutes = vec![false; 4];
        toggle_tracker_solo(&mut mutes, 1, false);
        assert_eq!(mutes, vec![true, false, true, true]);
        toggle_tracker_solo(&mut mutes, 1, false);
        assert_eq!(mutes, vec![false; 4]);
    }

    #[test]
    fn tracker_shift_click_edits_solo_group_and_last_solo_restores_all() {
        let mut mutes = vec![false; 4];
        toggle_tracker_solo(&mut mutes, 1, true);
        toggle_tracker_solo(&mut mutes, 3, true);
        assert_eq!(mutes, vec![true, false, true, false]);
        toggle_tracker_solo(&mut mutes, 1, true);
        assert_eq!(mutes, vec![true, true, true, false]);
        toggle_tracker_solo(&mut mutes, 3, true);
        assert_eq!(mutes, vec![false; 4]);
    }
}
