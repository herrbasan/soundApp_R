// SoundApp — native audio player built on the NUI Rust dual-backend renderer
// (GPU instanced SDF + CPU Softbuffer), nui_wc2 v3.0.0 "slate" theme and Swash typography.

mod audio;
mod config;
mod dsp;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{ResizeDirection, Window, WindowId};

use audio::{Audio, PlaybackParams, Source};
use audio::TrackMetadata;
use config::UserConfig;
use dsp::{DspAnalyzer, DspSnapshot, BANDS};
use nui::{
    rgb, rgba, BackendKind, ButtonVariant, FontEngine, FontFace, KeyCmd, Length,
    Rect, Renderer, Scene, TextSystem, Theme, Ui, UiState, WindowManager, WindowSpec, ICON_ADD,
    ICON_CLOSE, ICON_EQ, ICON_LOOP, ICON_NEXT, ICON_PAUSE,
    ICON_PLAY, ICON_PREV, ICON_RESTART, ICON_SEARCH, ICON_SETTINGS, ICON_SHUFFLE, ICON_TUNE,
    ICON_VOLUME,
};

/// One channel strip in the Phase 3.4 multitrack preview mixer.
#[derive(Clone, Debug)]
struct MixerStem {
    /// Display number (`01`..`20`).
    number: usize,
    /// Fader gain, `0.0..=2.0` (`1.0` = unity).
    gain: f32,
    /// Stereo pan, `-1.0` = hard left .. `+1.0` = hard right.
    pan: f32,
    muted: bool,
    solo: bool,
}

struct App {
    start: Instant,
    reported: bool,
    animate: bool,
    quit_at: Option<Instant>,
    prefer_backend: BackendKind,
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
    frames: u32,
    last_fps: u32,
    fps_at: Instant,
    next_frame: Instant,
    quitting: bool,

    // Phase 1.5 multi-window: secondary windows sharing `text_sys`, plus queued
    // show/hide toggles executed in `window_event` where `event_loop` is available.
    secondary_windows: WindowManager,
    pending_window_toggles: Vec<&'static str>,
    open_window_spec: Option<&'static WindowSpec>,

    // Interactive player states
    seg_pipeline: usize,
    /// Previous pipeline selection, used to re-apply the parameter set.
    last_pipeline: usize,
    selected_device: usize,
    selected_rate: usize,
    pitch_semitones: f32,
    tempo_ratio: f32,
    master_vol: f32,
    chk_formant: bool,
    chk_locked: bool,
    sw_hq_mode: bool,
    sw_keep_tray: bool,
    scrub_frac: f32,
    selected_track: usize,
    /// Master volume in `0.0..=2.0` (`1.0` = unity); persisted in `user.json`.
    pub show_controls: bool,

    // Phase 3.1 — player transport state (loop / shuffle).
    pub loop_enabled: bool,
    pub shuffle_enabled: bool,

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

    // Phase 3.4 — multitrack stem preview mixer state.
    mixer_stems: Vec<MixerStem>,
    mixer_playing: bool,
    mixer_loop: bool,
    mixer_master_vol: f32,
    mixer_pos_frac: f32,

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

const DEVICES: &[&str] = &[
    "WASAPI Shared · Default Output (48 kHz)",
    "WASAPI Exclusive · Studio DAC (96 kHz)",
    "ASIO · RME Babyface Pro FS",
    "Null Output (Benchmark Sink)",
];

const SAMPLE_RATES: &[&str] = &[
    "48,000 Hz (Standard)",
    "96,000 Hz (HQ Oversampled)",
    "192,000 Hz (Ultra HQ)",
];

/// Phase 1.5 secondary windows (SoundApp `P` / `N` / `M` / `S` / `H`), all undecorated
/// like the primary window and managed by `nui::WindowManager`.
const SPEC_PARAMETERS: WindowSpec = WindowSpec {
    key: "parameters",
    title: "SoundApp · Parameters [P]",
    width: 680.0,
    height: 460.0,
    min_width: 520.0,
    min_height: 340.0,
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

fn window_header_badge(key: &str) -> &'static str {
    match key {
        "parameters" => "LIVE PARAMS",
        "monitoring" => "DSP TAP",
        "mixer" => "STEMS",
        "settings" => "CONFIG",
        _ => "SHORTCUTS",
    }
}

/// The nine default multitrack stems (Phase 3.4). Media lives in
/// `D:\Work\_GIT\SoundApp\_Material\mixer_media\Again\01.m4a .. 09.m4a`.
fn default_mixer_stems() -> Vec<MixerStem> {
    (1..=9)
        .map(|n| MixerStem {
            number: n,
            gain: 1.0,
            pan: 0.0,
            muted: false,
            solo: false,
        })
        .collect()
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
    ("Left / Right", "Seek -5 s / +5 s"),
    ("Up / Down", "Master volume +5% / -5%"),
    ("L", "Toggle loop"),
    ("C", "Show / hide transport controls"),
    ("P", "Parameters window"),
    ("N", "Monitoring window"),
    ("M", "Mixer window"),
    ("S", "Settings window"),
    ("H", "This Help window"),
    ("B", "Toggle GPU / CPU backend"),
    ("F", "Toggle Swash / Fontdue engine"),
    ("T", "Toggle dark / light theme"),
    ("Ctrl + / - / 0", "UI zoom in / out / reset"),
    ("Ctrl+C / X / V", "Copy / Cut / Paste text"),
    ("Tab / Shift+Tab", "Move keyboard focus"),
    ("Dbl-Click / Right-Click", "Reset slider / fader to default"),
    ("Esc", "Close window / quit app"),
];

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
fn meter_segment(
    ui: &mut Ui,
    r: Rect,
    top: f32,
    lo_db: f32,
    hi_db: f32,
    color: (u8, u8, u8, u8),
) {
    let db_to_y = |db: f32| r.y + r.h * (1.0 - ((db + 60.0) / 60.0).clamp(0.0, 1.0));
    let y_start = db_to_y(hi_db).max(top);
    let y_end = db_to_y(lo_db);
    if y_end > y_start + 0.5 {
        ui.scene
            .rect(r.x + 1.0, y_start, (r.w - 2.0).max(1.0), y_end - y_start, color);
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
    let track_x0 = r.x + t.rem * 1.5;
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


/// Every audio extension FFmpeg decodes that SoundApp treats as playable.
const AUDIO_EXTS: &[&str] = &[
    "mp3", "wav", "flac", "ogg", "opus", "m4a", "m4b", "aac", "mp4", "webm", "mpg", "mp2",
    "aif", "aiff", "wma", "asf", "ape", "wv", "wvc", "tta", "mka", "amr", "3ga", "ac3",
    "eac3", "dts", "dtshd", "caf", "au", "snd", "voc", "tak", "mpc",
];

fn is_supported_audio(p: &std::path::Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| AUDIO_EXTS.iter().any(|x| e.eq_ignore_ascii_case(x)))
        .unwrap_or(false)
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
    MixerStems(usize),
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

/// Intents produced by the Phase 3.2 adaptive Parameters window.
#[derive(Default)]
struct ParamsWindowIntents {
    params_changed: bool,
    pipeline_changed: bool,
    reset_all: bool,
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
fn player_icon_btn(
    ui: &mut Ui,
    id: u32,
    r: Rect,
    icon: &str,
    active: bool,
    scale: f32,
) -> bool {
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
    ui.scene.rounded_rect(
        r.x,
        r.y,
        r.w,
        r.h,
        2.0 * scale,
        fill,
        (0, 0, 0, 0),
        0.0,
    );
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
    let zone = if cx >= split_x {
        DropZone::Mixer
    } else if cy >= mid_y {
        DropZone::Replace
    } else {
        DropZone::Add
    };

    ui.scene.begin_overlay();
    ui.scene.rect(0.0, 0.0, pw, ph, rgba(20, 20, 20, 225));

    let zones = [
        (Rect::new(6.0 * scale, 6.0 * scale, split_x - 12.0 * scale, mid_y - 12.0 * scale), DropZone::Add, "Add to current"),
        (Rect::new(6.0 * scale, mid_y + 6.0 * scale, split_x - 12.0 * scale, ph - mid_y - 12.0 * scale), DropZone::Replace, "Replace playlist"),
        (Rect::new(split_x + 6.0 * scale, 6.0 * scale, pw - split_x - 12.0 * scale, ph - 12.0 * scale), DropZone::Mixer, "Open in Mixer"),
    ];
    let fs = (13.0 * scale).round();
    for (r, z, label) in zones {
        let hot = z == zone;
        ui.scene.rounded_rect(
            r.x,
            r.y,
            r.w,
            r.h,
            (4.0 * scale).round(),
            if hot { rgba(76, 132, 229, 70) } else { rgba(255, 255, 255, 10) },
            if hot { rgb(76, 132, 229) } else { rgba(255, 255, 255, 30) },
            1.0,
        );
        draw_centered_text(
            ui,
            r,
            label,
            FontFace::SemiBold,
            fs,
            if hot {
                rgb(255, 255, 255)
            } else {
                rgb(165, 165, 165)
            },
        );
    }
    ui.scene.end_overlay();

    ui.state.dropped_files.pop().map(|p| (p, zone))
}

impl App {
    fn new(
        audio: Option<Audio>,
        animate: bool,
        quit_at: Option<Instant>,
        prefer_backend: BackendKind,
        font_engine: FontEngine,
        config: UserConfig,
    ) -> Self {
        let mixer_stems = default_mixer_stems();
        let (scan_tx, job_rx) = std::sync::mpsc::channel::<(u64, ScanJob)>();
        let (res_tx, scan_rx) = std::sync::mpsc::channel::<(u64, ScanOutcome)>();
        let _ = std::thread::Builder::new()
            .name("playlist-scan".into())
            .spawn(move || {
                while let Ok((mut gen, mut job)) = job_rx.recv() {
                    // Coalesce if newer non-append scans arrived while busy.
                    while let Ok((g2, j2)) = job_rx.try_recv() {
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
                            ScanOutcome::MixerStems(files.len().min(20))
                        }
                    };
                    let _ = res_tx.send((gen, out));
                }
            });
        Self {
            start: Instant::now(),
            reported: false,
            animate,
            quit_at,
            prefer_backend,
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
            frames: 0,
            last_fps: 0,
            fps_at: Instant::now(),
            next_frame: Instant::now(),
            quitting: false,

            secondary_windows: WindowManager::new(),
            pending_window_toggles: Vec::new(),
            open_window_spec: None,

            seg_pipeline: 1,
            last_pipeline: 1,
            selected_device: 0,
            selected_rate: 0,
            pitch_semitones: 0.0,
            tempo_ratio: 1.0,
            master_vol: config.master_vol.clamp(0.0, 2.0),
            chk_formant: true,
            chk_locked: config.chk_locked,
            sw_hq_mode: config.sw_hq_mode,
            sw_keep_tray: config.sw_keep_tray,
            scrub_frac: 0.0,
            selected_track: 0,
            show_controls: config.show_controls,
            loop_enabled: false,
            shuffle_enabled: false,
            playlist: Vec::new(),
            track_meta: None,
            scan_tx,
            scan_rx,
            scan_gen: 0,
            dsp: DspAnalyzer::new(48_000),
            last_dsp_frames: 0,
            dsp_buf: Vec::new(),
            dsp_snap: DspSnapshot::default(),
            mixer_stems,
            mixer_playing: true,
            mixer_loop: true,
            mixer_master_vol: 0.85,
            mixer_pos_frac: 0.74,
            config,
            save_config_req: false,
            apply_zoom_req: false,
            config_status: String::new(),
        }
    }

    /// Capture the current UI state as a persistable config and write `user.json`.
    fn save_config(&mut self) {
        self.config.dark_mode = self.dark_mode;
        self.config.ui_zoom = self.ui_zoom;
        self.config.master_vol = self.master_vol;
        self.config.show_controls = self.show_controls;
        self.config.chk_locked = self.chk_locked;
        self.config.sw_hq_mode = self.sw_hq_mode;
        self.config.sw_keep_tray = self.sw_keep_tray;
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

    /// The current UI parameter state as an engine `PlaybackParams`.
    fn playback_params(&self) -> PlaybackParams {
        PlaybackParams {
            mode_tape: self.seg_pipeline == 0,
            pitch_semitones: self.pitch_semitones,
            tempo_ratio: self.tempo_ratio,
            hq: self.sw_hq_mode,
            formant_preserve: self.chk_formant,
        }
    }

    /// Re-apply the pipeline selection and push the full parameter set to the
    /// engine. Parameters are applied entirely in the Rust pipeline, so no
    /// engine reload is needed.
    fn sync_pipeline_and_params(&mut self) {
        let params = self.playback_params();
        self.last_pipeline = self.seg_pipeline;
        if let Some(a) = &self.audio {
            a.set_params(params);
        }
    }

    /// Load the next playlist entry (wrapping only when `wrap` is set).
    /// Returns `true` when a track was loaded.
    fn advance_track(&mut self, wrap: bool) -> bool {
        if self.playlist.is_empty() {
            return false;
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

    /// Load `playlist[idx]` into the engine.
    fn load_playlist_index(&mut self, idx: usize) {
        let Some(path) = self.playlist.get(idx).cloned() else {
            return;
        };
        self.selected_track = idx;
        if !self.chk_locked {
            self.pitch_semitones = 0.0;
            self.tempo_ratio = 1.0;
        }
        self.scrub_frac = 0.0;
        self.track_meta = None;
        self.scene.clear_image();
        if let Some(a) = &self.audio {
            a.load_file(path);
        }
    }

    /// Open a file or folder without blocking the UI thread on SMB directory I/O.
    fn open_path(&mut self, path: PathBuf, append: bool, recursive_dirs: bool) {
        if is_supported_audio(&path) {
            if append && !self.playlist.is_empty() {
                self.playlist.push(path);
                return;
            }
            if !self.chk_locked {
                self.pitch_semitones = 0.0;
                self.tempo_ratio = 1.0;
            }
            self.scrub_frac = 0.0;
            self.track_meta = None;
            self.scene.clear_image();
            if let Some(a) = &self.audio {
                a.load_file(path.clone());
            }
            self.playlist = vec![path.clone()];
            self.selected_track = 0;
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

    /// Poll background directory-scan completions and end-of-track transitions.
    fn poll_transport_and_scans(&mut self) {
        let mut redraw = false;
        while let Ok((gen, out)) = self.scan_rx.try_recv() {
            match out {
                ScanOutcome::SingleFile { files, idx } => {
                    if gen == self.scan_gen && !files.is_empty() {
                        self.playlist = files;
                        self.selected_track = idx;
                        redraw = true;
                    }
                }
                ScanOutcome::Folder { files, append } => {
                    if files.is_empty() {
                        continue;
                    }
                    if append && !self.playlist.is_empty() {
                        self.playlist.extend(files);
                        redraw = true;
                    } else if gen == self.scan_gen {
                        self.playlist = files;
                        self.selected_track = 0;
                        self.load_playlist_index(0);
                        redraw = true;
                    }
                }
                ScanOutcome::MixerStems(n) => {
                    if n > 0 {
                        self.mixer_stems = (0..n)
                            .map(|i| MixerStem {
                                number: i + 1,
                                gain: 1.0,
                                pan: 0.0,
                                muted: false,
                                solo: false,
                            })
                            .collect();
                        redraw = true;
                    }
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

        // Pull the real metadata published by the decoder thread and sync cover art
        // whenever the track metadata changes (including clearing when absent).
        let meta = self.audio.as_ref().and_then(|a| a.metadata());
        if meta != self.track_meta {
            if let Some((cw, ch, rgba)) = meta.as_ref().and_then(|m| m.cover_rgba.clone()) {
                self.scene.set_image(cw, ch, rgba);
            } else {
                self.scene.clear_image();
            }
            self.track_meta = meta;
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
            .and_then(|p| p.extension().map(|e| e.to_string_lossy().to_ascii_uppercase()))
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
        let (sr, ch) = md.as_ref().map(|m| (m.sample_rate, m.channels)).unwrap_or((0, 0));
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
            let has_cover = self
                .track_meta
                .as_ref()
                .map(|m| m.cover_rgba.is_some())
                .unwrap_or(false);
            ui.scene.push_clip(cover.x, cover.y, cover.w, cover.h);
            if has_cover {
                ui.scene.image_quad(cover.x, cover.y, cover.w, cover.h);
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
            draw_meta_row(&mut ui, rows_x, label_w, row_gap, baseline(ry), mfs, "Folder:", &folder_lbl, value_w);
            ry += step;
            draw_meta_row(&mut ui, rows_x, label_w, row_gap, baseline(ry), mfs, "File:", &file_lbl, value_w);
            ry += step + gap_v;
            draw_meta_row(&mut ui, rows_x, label_w, row_gap, baseline(ry), mfs, "Format:", &format_lbl, value_w);
            ry += step;
            draw_meta_row(&mut ui, rows_x, label_w, row_gap, baseline(ry), mfs, "", &format2_lbl, value_w);
            ry += step + gap_v;
            draw_meta_row(&mut ui, rows_x, label_w, row_gap, baseline(ry), mfs, "Artist:", &artist_lbl, value_w);
            ry += step;
            draw_meta_row(&mut ui, rows_x, label_w, row_gap, baseline(ry), mfs, "Album:", &album_lbl, value_w);
            ry += step;
            draw_meta_row(&mut ui, rows_x, label_w, row_gap, baseline(ry), mfs, "Title:", &title_lbl, value_w);

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
            let hit = Rect::new(bar_x, bar_y - 6.0 * scale, bar_w, bar_h + 12.0 * scale);
            ui.state.register_focusable(scrub_id);
            let scrub_hover = ui.state.can_hover(scrub_id, hit);
            if scrub_hover {
                ui.state.hovered_id = Some(scrub_id);
                if ui.state.mouse_pressed {
                    ui.state.active_id = Some(scrub_id);
                    ui.state.focused_id = Some(scrub_id);
                }
            }
            if ui.state.active_id == Some(scrub_id) && ui.state.mouse_down {
                self.scrub_frac = ((ui.state.cursor.0 - bar_x) / bar_w).clamp(0.0, 1.0);
            }
            if ui.state.active_id == Some(scrub_id) && ui.state.mouse_released && is_finite {
                intents.seek_to = Some(self.scrub_frac as f64 * dur);
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
            ui.scene.rect(bar_x, bar_y, bar_w, bar_h, track_bar_bg);
            let fill_w = bar_w * self.scrub_frac.clamp(0.0, 1.0);
            if fill_w > 0.5 {
                ui.scene.rect(bar_x, bar_y, fill_w, bar_h, track_bar_fill);
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
                    ICON_TUNE,
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
                let vol_hit = Rect::new(vol_x, vol_y - 6.0 * scale, vol_w, vol_h + 12.0 * scale);
                ui.state.register_focusable(vol_id);
                if ui.state.can_hover(vol_id, vol_hit) {
                    ui.state.hovered_id = Some(vol_id);
                    if ui.state.mouse_pressed {
                        ui.state.active_id = Some(vol_id);
                        ui.state.focused_id = Some(vol_id);
                    }
                }
                if ui.state.active_id == Some(vol_id) && ui.state.mouse_down {
                    self.master_vol = ((ui.state.cursor.0 - vol_x) / vol_w).clamp(0.0, 1.0);
                    intents.vol_changed = true;
                }
                ui.scene.rect(vol_x, vol_y, vol_w, vol_h, track_bar_bg);
                let vfill = vol_w * self.master_vol.clamp(0.0, 1.0);
                if vfill > 0.5 {
                    ui.scene.rect(vol_x, vol_y, vfill, vol_h, track_bar_fill);
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
                    if player_icon_btn(&mut ui, id, Rect::new(tx.round(), by, bs, bs), icon, active, scale) {
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
        if intents.prev || intents.next {
            if !self.playlist.is_empty() {
                let len = self.playlist.len();
                let cur = self.selected_track.min(len - 1);
                let idx = if intents.prev {
                    (cur + len - 1) % len
                } else {
                    (cur + 1) % len
                };
                self.load_playlist_index(idx);
            }
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
            // Ctrl held → flat (non-recursive) folder collection.
            let recursive = !self.modifiers.control_key();
            match zone {
                DropZone::Add => self.open_path(path, true, recursive),
                DropZone::Replace => self.open_path(path, false, recursive),
                DropZone::Mixer => {
                    self.scan_gen = self.scan_gen.wrapping_add(1);
                    let _ = self.scan_tx.send((
                        self.scan_gen,
                        ScanJob::MixerPath {
                            path,
                            recursive,
                        },
                    ));
                    if !self.secondary_windows.is_visible("mixer") {
                        self.pending_window_toggles.push("mixer");
                    }
                }
            }
        }
        if intents.close {
            self.quitting = true;
            self.quit_at = Some(Instant::now());
            if let Some(w) = &self.window {
                w.request_redraw();
            }
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
        let scale = window.scale_factor() as f32;

        if self.nopaint {
            if let Some(renderer) = &mut self.renderer {
                renderer.present_nopaint(w, h, (30, 30, 30));
            }
            self.frames += 1;
            if self.fps_at.elapsed() >= Duration::from_millis(1000) {
                self.last_fps = self.frames;
                println!("[poc] {} fps", self.frames);
                self.frames = 0;
                self.fps_at = Instant::now();
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

        self.frames += 1;
        if self.fps_at.elapsed() >= Duration::from_millis(1000) {
            self.last_fps = self.frames;
            println!("[poc] {} fps", self.frames);
            self.frames = 0;
            self.fps_at = Instant::now();
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
                }
            }
        }
    }

    /// Execute queued secondary-window show/hide toggles. Toggles are queued from the
    /// keyboard shortcuts and the main window's toggle buttons (both handled during
    /// scene build, where no `event_loop` exists) and executed here from the primary
    /// window's `RedrawRequested` arm in `window_event`.
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
            self.secondary_windows
                .toggle(event_loop, spec, self.prefer_backend);
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
            WindowEvent::CursorMoved { position, .. } => {
                if let Some(w) = self.secondary_windows.find_by_key_mut(key) {
                    w.ui_state.cursor = (position.x as f32, position.y as f32);
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
                    if let Some(w) = self.secondary_windows.find_by_key_mut(key) {
                        match state {
                            ElementState::Pressed => {
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
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed {
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
                    let fp = a
                        .frames_played
                        .load(std::sync::atomic::Ordering::Relaxed);
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
                let mut ui =
                    Ui::new(ui_state, scene, &mut self.text_sys, self.font_engine, theme);

                // The Parameters window is audio-pipeline only now.
                let mixer_badge = format!("{} STEMS", self.mixer_stems.len());
                let (hdr_icon, hdr_title, hdr_badge): (&str, &str, &str) = if key == "parameters" {
                    (ICON_TUNE, "PARAMETERS", "AUDIO")
                } else if key == "monitoring" {
                    (ICON_EQ, "AUDIO MONITORING", "REALTIME DSP")
                } else if key == "mixer" {
                    (ICON_EQ, "MULTITRACK PREVIEW", &mixer_badge)
                } else {
                    (window_header_icon(key), spec.title, window_header_badge(key))
                };
                let (hdr_h, close) =
                    ui.secondary_window_header(pw, hdr_title, hdr_icon, hdr_badge);
                close_clicked = close;
                let m = Length::Rem(1.25).resolve(rem).round();

                if key == "parameters" {
                    // ========================================================
                    // Phase 3.2 — Parameters window: one audio card, footer below.
                    // ========================================================
                    let footer_h = Length::Rem(3.0).resolve(rem).round();
                    let card_y = hdr_h + m;
                    let card_h = (ph - Length::Rem(3.2).resolve(rem) - card_y).max(80.0);
                    ui.nui_card(
                        Rect::new(m, card_y, (pw - m * 2.0).max(0.0), card_h),
                        |ui| {
                            {
                                    // ---- pipeline mode + speed/pitch/formants ----
                                    let [lbl_r, ctl_r] = ui.take_cols(
                                        Length::rem(2.0),
                                        Length::rem(1.0),
                                        [Length::rem(5.0), Length::fill()],
                                    );
                                    let fs = theme.font_small();
                                    ui.text_clipped(
                                        FontFace::Regular,
                                        ui.font_engine,
                                        "Mode",
                                        lbl_r.x.round(),
                                        (lbl_r.y + lbl_r.h * 0.5 + fs * 0.33).round(),
                                        fs,
                                        lbl_r.w,
                                        theme.text_dim,
                                    );
                                    let half_w =
                                        ((ctl_r.w - theme.space_half()) * 0.5).floor();
                                    if ui.radio(
                                        1110,
                                        Rect::new(ctl_r.x, ctl_r.y, half_w, ctl_r.h),
                                        "Normal (Tape)",
                                        self.seg_pipeline == 0,
                                    ) && self.seg_pipeline != 0
                                    {
                                        self.seg_pipeline = 0;
                                        params_intents.pipeline_changed = true;
                                    }
                                    if ui.radio(
                                        1111,
                                        Rect::new(
                                            ctl_r.x + half_w + theme.space_half(),
                                            ctl_r.y,
                                            (ctl_r.w - half_w - theme.space_half()).max(0.0),
                                            ctl_r.h,
                                        ),
                                        "RubberBand (Pitch/Time)",
                                        self.seg_pipeline == 1,
                                    ) && self.seg_pipeline != 1
                                    {
                                        self.seg_pipeline = 1;
                                        params_intents.pipeline_changed = true;
                                    }
                                    ui.spacer(Length::rem(0.35));
                                    let speed_lbl = format!("{:.2}x", self.tempo_ratio);
                                    let sp = ui.take(Length::rem(2.9));
                                    if ui.slider(
                                        1112,
                                        sp,
                                        "Playback Speed",
                                        &mut self.tempo_ratio,
                                        0.5,
                                        2.0,
                                        1.0,
                                        &speed_lbl,
                                    ) {
                                        params_intents.params_changed = true;
                                    }
                                    let pitch_lbl = format!("{:+.1} st", self.pitch_semitones);
                                    let pt = ui.take(Length::rem(2.9));
                                    if ui.slider(
                                        1113,
                                        pt,
                                        "Pitch",
                                        &mut self.pitch_semitones,
                                        -12.0,
                                        12.0,
                                        0.0,
                                        &pitch_lbl,
                                    ) {
                                        params_intents.params_changed = true;
                                    }
                                    ui.spacer(Length::rem(0.35));
                                    let sw = ui.take(Length::rem(2.0));
                                    if ui.switch(
                                        1114,
                                        sw,
                                        "Preserve Formants (Spectral Envelope)",
                                        &mut self.chk_formant,
                                    ) {
                                        params_intents.params_changed = true;
                                    }
                            }
                        },
                    );

                    // Footer bar: parameter lock (left) + Reset All (right).
                    let fcy = ph - footer_h * 0.5;
                    let lock_h = Length::Rem(2.0).resolve(rem);
                    let lock_r = Rect::new(m, fcy - lock_h * 0.5, (pw * 0.55).max(0.0), lock_h);
                    ui.switch(1190, lock_r, "Lock Parameters Across Tracks", &mut self.chk_locked);
                    let btn_w = Length::Rem(7.0).resolve(rem).round();
                    let btn_h = Length::Rem(1.85).resolve(rem).round();
                    if ui.button(
                        1191,
                        Rect::new(pw - m - btn_w, fcy - btn_h * 0.5, btn_w, btn_h),
                        "Reset All",
                        ICON_RESTART,
                        ButtonVariant::Outline,
                    ) {
                        params_intents.reset_all = true;
                    }
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
                            let [tl, tr] = ui.cols(top_row, Length::Px(gap), [Length::fill(), Length::fill()]);
                            let [bl, br] = ui.cols(bot_row, Length::Px(gap), [Length::fill(), Length::fill()]);

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
                                    canvas.x, canvas.y, canvas.w, canvas.h, theme.radius1(),
                                    theme.base, theme.border1, 1.0,
                                );
                                ui.scene.push_clip(
                                    canvas.x + 1.0, canvas.y + 1.0,
                                    (canvas.w - 2.0).max(1.0), (canvas.h - 2.0).max(1.0),
                                );
                                for f in [50.0f32, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0] {
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
                                            bx, canvas.y + canvas.h - h, bw, h, 1.5,
                                            theme.highlight, (0, 0, 0, 0), 0.0,
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
                                    FontFace::SemiBold, ui.font_engine, "OSCILLOSCOPE",
                                    hdr.x.round(), baseline, fs, hdr.w, theme.text_dim,
                                );
                                let rms_txt = format!("RMS: {:+.1} dB", snap.rms_db);
                                let rtw = ui.text_sys.measure(
                                    FontFace::SemiBold, ui.font_engine, &rms_txt, fs,
                                );
                                ui.scene.text(
                                    ui.text_sys, FontFace::SemiBold, ui.font_engine, &rms_txt,
                                    (hdr.x + hdr.w - rtw).round(), baseline, fs, theme.highlight,
                                );
                                let canvas = ui.take_rest();
                                ui.scene.rounded_rect(
                                    canvas.x, canvas.y, canvas.w, canvas.h, theme.radius1(),
                                    theme.base, theme.border1, 1.0,
                                );
                                ui.scene.push_clip(
                                    canvas.x + 1.0, canvas.y + 1.0,
                                    (canvas.w - 2.0).max(1.0), (canvas.h - 2.0).max(1.0),
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
                                    FontFace::SemiBold, ui.font_engine, "STEREO FIELD (GONIOMETER)",
                                    hdr.x.round(), baseline, fs, hdr.w, theme.text_dim,
                                );
                                let corr_txt = format!("Correlation: {:+.2}", snap.correlation);
                                let ctw = ui.text_sys.measure(
                                    FontFace::SemiBold, ui.font_engine, &corr_txt, fs,
                                );
                                ui.scene.text(
                                    ui.text_sys, FontFace::SemiBold, ui.font_engine, &corr_txt,
                                    (hdr.x + hdr.w - ctw).round(), baseline, fs, theme.highlight,
                                );
                                let rest = ui.take_rest();
                                let [radar, corr] = ui.rows(
                                    rest, Length::Px(theme.space_half()),
                                    [Length::fill(), Length::rem(1.5)],
                                );
                                ui.scene.rounded_rect(
                                    radar.x, radar.y, radar.w, radar.h, theme.radius1(),
                                    theme.base, theme.border1, 1.0,
                                );
                                let cx = radar.x + radar.w * 0.5;
                                let cy = radar.y + radar.h * 0.5;
                                let rad = (radar.w.min(radar.h) * 0.5 - 8.0).max(4.0);
                                let tau = std::f32::consts::TAU;
                                for i in 0..32 {
                                    let a0 = i as f32 / 32.0 * tau;
                                    let a1 = (i + 1) as f32 / 32.0 * tau;
                                    ui.scene.line(
                                        cx + rad * a0.cos(), cy + rad * a0.sin(),
                                        cx + rad * a1.cos(), cy + rad * a1.sin(),
                                        1.0, theme.border2,
                                    );
                                }
                                let d = rad * 0.7071;
                                ui.scene.line(cx - d, cy + d, cx + d, cy - d, 1.0, theme.border1);
                                ui.scene.line(cx - d, cy - d, cx + d, cy + d, 1.0, theme.border1);
                                ui.scene.line(cx, cy - rad, cx, cy + rad, 1.0, theme.border1);
                                ui.scene.line(cx - rad, cy, cx + rad, cy, 1.0, theme.border1);
                                let lfs = theme.font_xsmall();
                                for (lbl, lx, ly) in [
                                    ("L", cx - d - 12.0, cy - d + 4.0),
                                    ("R", cx + d + 2.0, cy + d + 6.0),
                                    ("+M", cx + 3.0, cy - rad + 6.0),
                                    ("-S", cx + 3.0, cy + rad),
                                ] {
                                    ui.scene.text(
                                        ui.text_sys, FontFace::SemiBold, ui.font_engine, lbl,
                                        lx.round(), ly.round(), lfs, theme.text_muted,
                                    );
                                }
                                ui.polyline(radar, &snap.gonio_points, 1.5, theme.highlight);

                                // Phase correlation bar: centre 0, fill toward ±1.
                                ui.scene.rounded_rect(
                                    corr.x, corr.y, corr.w, corr.h, theme.radius1(),
                                    theme.base, theme.border1, 1.0,
                                );
                                let mid = (corr.x + corr.w * 0.5).round();
                                ui.scene.rect(mid - 1.0, corr.y + 2.0, 2.0, corr.h - 4.0, theme.border3);
                                let c = snap.correlation.clamp(-1.0, 1.0);
                                let half = corr.w * 0.5;
                                let (bx0, bwid) = if c >= 0.0 {
                                    (mid, half * c)
                                } else {
                                    (mid + half * c, -half * c)
                                };
                                if bwid > 0.5 {
                                    ui.scene.rect(bx0, corr.y + 2.0, bwid, corr.h - 4.0, theme.highlight);
                                }
                                let lfs2 = theme.font_xsmall();
                                ui.scene.text(
                                    ui.text_sys, FontFace::Regular, ui.font_engine, "-1",
                                    (corr.x + 3.0).round(), (corr.y + corr.h * 0.5 + 4.0).round(),
                                    lfs2, theme.text_muted,
                                );
                                ui.scene.text(
                                    ui.text_sys, FontFace::Regular, ui.font_engine, "+1",
                                    (corr.x + corr.w - 16.0).round(),
                                    (corr.y + corr.h * 0.5 + 4.0).round(), lfs2, theme.text_muted,
                                );
                            });

                            // ---- Bottom-Right: meters + BS.1770 loudness ----
                            ui.card(br, Length::Rem(0.85), Length::Rem(0.45), |ui| {
                                let hdr = ui.take(Length::Rem(1.2));
                                let fs = theme.font_small();
                                let baseline = (hdr.y + fs * 0.85).round();
                                ui.text_clipped(
                                    FontFace::SemiBold, ui.font_engine, "LEVELS & LOUDNESS (LUFS)",
                                    hdr.x.round(), baseline, fs, hdr.w, theme.text_dim,
                                );
                                let int_txt = format!("Int: {:+.1} LUFS", snap.integrated_lufs);
                                let itw = ui.text_sys.measure(
                                    FontFace::SemiBold, ui.font_engine, &int_txt, fs,
                                );
                                ui.scene.text(
                                    ui.text_sys, FontFace::SemiBold, ui.font_engine, &int_txt,
                                    (hdr.x + hdr.w - itw).round(), baseline, fs, theme.highlight,
                                );
                                let rest = ui.take_rest();
                                let [meters_r, lufs_r] = ui.cols(
                                    rest, Length::Px(theme.space_half()),
                                    [Length::rem(6.0), Length::fill()],
                                );
                                let [lm, sm, rm] = ui.cols(
                                    meters_r, Length::Px(2.0),
                                    [Length::fill(), Length::rem(1.5), Length::fill()],
                                );
                                draw_vu_meter(ui, lm, snap.vu_l_db, snap.peak_l_db, "L");
                                draw_vu_meter(ui, rm, snap.vu_r_db, snap.peak_r_db, "R");
                                let sfs = theme.font_xsmall();
                                for (db, lbl) in [
                                    (0.0f32, "0"), (-12.0, "-12"), (-24.0, "-24"),
                                    (-36.0, "-36"), (-48.0, "-48"),
                                ] {
                                    let y = sm.y
                                        + (1.0 - ((db + 60.0) / 60.0).clamp(0.0, 1.0)) * sm.h;
                                    ui.scene.text(
                                        ui.text_sys, FontFace::Regular, ui.font_engine, lbl,
                                        sm.x.round(), (y + 3.0).round(), sfs, theme.text_muted,
                                    );
                                }
                                let [grid_r, btn_row] = ui.rows(
                                    lufs_r, Length::Px(theme.space_half()),
                                    [Length::fill(), Length::rem(2.1)],
                                );
                                let [r0, r1] = ui.rows(
                                    grid_r, Length::Px(theme.space_half()),
                                    [Length::fill(), Length::fill()],
                                );
                                let [a0, a1] = ui.cols(
                                    r0, Length::Px(theme.space_half()),
                                    [Length::fill(), Length::fill()],
                                );
                                let [b0, b1] = ui.cols(
                                    r1, Length::Px(theme.space_half()),
                                    [Length::fill(), Length::fill()],
                                );
                                draw_readout(ui, a0, "MOMENTARY", &format!("{:+.1} LUFS", snap.momentary_lufs));
                                draw_readout(ui, a1, "SHORT-TERM", &format!("{:+.1} LUFS", snap.short_term_lufs));
                                draw_readout(ui, b0, "INTEGRATED", &format!("{:+.1} LUFS", snap.integrated_lufs));
                                draw_readout(ui, b1, "TRUE PEAK", &format!("{:+.1} dBTP", snap.true_peak_dbtp));
                                let bw2 = Length::Rem(8.0).resolve(rem).round();
                                let btn = Rect::new(btn_row.x + btn_row.w - bw2, btn_row.y, bw2, btn_row.h);
                                if ui.button(
                                    1290, btn, "RESET LUFS", ICON_RESTART, ButtonVariant::Outline,
                                ) {
                                    self.dsp.reset_lufs();
                                }
                            });
                        }

                        // ====================================================
                        // Phase 3.4 — MULTITRACK PREVIEW mixer.
                        // ====================================================
                        "mixer" => {
                            let pad = Length::Rem(1.25).resolve(rem).round();
                            let inner = Rect::new(
                                pad, hdr_h + pad,
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
                            let play_icon = if self.mixer_playing { ICON_PAUSE } else { ICON_PLAY };
                            if ui.button(1301, Rect::new(toolbar.x, by, sq, bh), "", play_icon, ButtonVariant::Outline) {
                                self.mixer_playing = !self.mixer_playing;
                            }
                            if ui.button(1302, Rect::new(toolbar.x + (sq + bgap), by, sq, bh), "", ICON_RESTART, ButtonVariant::Outline) {
                                self.mixer_pos_frac = 0.0;
                            }
                            let loop_var = if self.mixer_loop { ButtonVariant::Primary } else { ButtonVariant::Outline };
                            if ui.button(1303, Rect::new(toolbar.x + (sq + bgap) * 2.0, by, sq, bh), "", ICON_LOOP, loop_var) {
                                self.mixer_loop = !self.mixer_loop;
                            }
                            let vol_w = Length::Rem(15.0).resolve(rem).round();
                            let vol_r = Rect::new(toolbar.x + toolbar.w - vol_w, by, vol_w, bh);
                            master_volume_pill(&mut ui, vol_r, &mut self.mixer_master_vol, 1304);

                            // ---- Channel strip matrix ----
                            let matrix_y = toolbar.y + toolbar.h + gap;
                            let seek_y = inner.y + inner.h - seek_h;
                            let matrix = Rect::new(
                                inner.x, matrix_y, inner.w, (seek_y - matrix_y - gap).max(40.0),
                            );
                            let n = self.mixer_stems.len().max(1);
                            let col_gap = Length::Rem(0.4).resolve(rem).round();
                            let plus_w = Length::Rem(3.2).resolve(rem).round();
                            let strip_w = ((matrix.w - plus_w - col_gap * n as f32) / n as f32)
                                .max(24.0)
                                .floor();
                            let any_solo = self.mixer_stems.iter().any(|s| s.solo);
                            let m_h = Length::Rem(1.55).resolve(rem).round();
                            let lbl_h = Length::Rem(1.65).resolve(rem).round();
                            let cgap = 2.0f32;
                            let fader_h = (matrix.h - (m_h + m_h + lbl_h) - cgap * 4.0).max(30.0);
                            let mut remove_i: Option<usize> = None;

                            for i in 0..n {
                                let x = matrix.x + i as f32 * (strip_w + col_gap);
                                let mut y = matrix.y;

                                // Mute cell
                                let m_r = Rect::new(x, y, strip_w, m_h);
                                y += m_h + cgap;
                                let is_muted = self.mixer_stems[i].muted;
                                if mixer_toggle_cell(&mut ui, m_r, 1310 + i as u32, "M", is_muted, theme.palette_alert) {
                                    self.mixer_stems[i].muted = !is_muted;
                                }

                                // Solo cell
                                let s_r = Rect::new(x, y, strip_w, m_h);
                                y += m_h + cgap;
                                let is_solo = self.mixer_stems[i].solo;
                                if mixer_toggle_cell(&mut ui, s_r, 1330 + i as u32, "S", is_solo, theme.palette_mark) {
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
                                        gain = (gain + 0.05 * ui.state.wheel_y.signum()).clamp(0.0, 2.0);
                                    }
                                }
                                if ui.state.active_id == Some(fid) && ui.state.mouse_down {
                                    gain = ((1.0 - (ui.state.cursor.1 - fader_r.y) / fader_r.h) * 2.0)
                                        .clamp(0.0, 2.0);
                                }
                                self.mixer_stems[i].gain = gain;

                                ui.scene.rounded_rect(
                                    fader_r.x, fader_r.y, fader_r.w, fader_r.h, theme.radius1(),
                                    theme.base, theme.border1, 1.0,
                                );
                                let lvl = if self.mixer_stems[i].muted
                                    || (any_solo && !self.mixer_stems[i].solo)
                                {
                                    0.0
                                } else {
                                    let phase = self.t * 2.2 + i as f32 * 0.6;
                                    let base = 0.45 + 0.35 * (phase.sin() * 0.5 + 0.5);
                                    (base * gain.min(1.0)).clamp(0.0, 1.0)
                                };
                                let vu_h = (fader_r.h * lvl).clamp(0.0, fader_r.h);
                                if vu_h > 1.0 {
                                    ui.scene.rect(
                                        fader_r.x + 1.0, fader_r.y + fader_r.h - vu_h,
                                        (fader_r.w - 2.0).max(1.0), vu_h, rgb(58, 175, 52),
                                    );
                                }
                                let line_y = (fader_r.y
                                    + fader_r.h * (1.0 - (gain / 2.0).clamp(0.0, 1.0)))
                                    .round();
                                ui.scene.rect(fader_r.x, line_y, fader_r.w, 1.5, rgb(230, 230, 230));
                                draw_text_at(
                                    &mut ui, fader_r.x + fader_r.w * 0.5, line_y - 5.0,
                                    &format!("{:.2}", gain), FontFace::SemiBold,
                                    theme.font_xsmall(), rgb(245, 245, 245),
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
                                        pan = (pan + 0.05 * ui.state.wheel_y.signum()).clamp(-1.0, 1.0);
                                    }
                                }
                                if ui.state.active_id == Some(pid) && ui.state.mouse_down {
                                    pan = (((ui.state.cursor.0 - pan_r.x) / pan_r.w) * 2.0 - 1.0)
                                        .clamp(-1.0, 1.0);
                                }
                                self.mixer_stems[i].pan = pan;
                                ui.scene.rounded_rect(
                                    pan_r.x, pan_r.y, pan_r.w, pan_r.h, theme.radius1(),
                                    theme.base, theme.border1, 1.0,
                                );
                                let px = (pan_r.x + 4.0
                                    + (pan_r.w - 8.0) * ((pan + 1.0) * 0.5))
                                    .round();
                                let tick_h = pan_r.h * 0.55;
                                ui.scene.rect(
                                    px - 1.0, pan_r.y + (pan_r.h - tick_h) * 0.5,
                                    2.0, tick_h, rgb(230, 230, 230),
                                );

                                // Bottom label + remove ×
                                let lbl_r = Rect::new(x, y, strip_w, lbl_h);
                                ui.scene.rounded_rect(
                                    lbl_r.x, lbl_r.y, lbl_r.w, lbl_r.h, theme.radius1(),
                                    theme.base, theme.border1, 1.0,
                                );
                                let lfs = theme.font_xsmall();
                                let num = format!("{:02}", self.mixer_stems[i].number);
                                ui.scene.text(
                                    ui.text_sys, FontFace::SemiBold, ui.font_engine, &num,
                                    (lbl_r.x + 5.0).round(),
                                    (lbl_r.y + lbl_r.h * 0.5 + lfs * 0.34).round(),
                                    lfs, theme.text_dim,
                                );
                                let xbtn = (rem * 1.2).round();
                                let xr = Rect::new(
                                    lbl_r.x + lbl_r.w - xbtn - 2.0,
                                    lbl_r.y + (lbl_r.h - xbtn) * 0.5, xbtn, xbtn,
                                );
                                if ui.button(1390 + i as u32, xr, "", ICON_CLOSE, ButtonVariant::Ghost) {
                                    remove_i = Some(i);
                                }
                            }

                            if let Some(i) = remove_i {
                                if self.mixer_stems.len() > 1 {
                                    self.mixer_stems.remove(i);
                                }
                            }

                            // `+` add-stem column (up to 20 stems).
                            let plus_r = Rect::new(
                                matrix.x + matrix.w - plus_w, matrix.y, plus_w, matrix.h,
                            );
                            if ui.button(1340, plus_r, "", ICON_ADD, ButtonVariant::Outline) {
                                if self.mixer_stems.len() < 20 {
                                    let num = self.mixer_stems.len() + 1;
                                    self.mixer_stems.push(MixerStem {
                                        number: num, gain: 1.0, pan: 0.0,
                                        muted: false, solo: false,
                                    });
                                }
                            }

                            // ---- Bottom seekbar + time readouts ----
                            let bar_h = 6.0f32;
                            let bar_r = Rect::new(inner.x, seek_y + 3.0, inner.w, bar_h);
                            ui.scene.rounded_rect(
                                bar_r.x, bar_r.y, bar_r.w, bar_h, bar_h * 0.5,
                                theme.base, theme.border1, 1.0,
                            );
                            let played = bar_r.w * self.mixer_pos_frac.clamp(0.0, 1.0);
                            if played > 0.5 {
                                ui.scene.rounded_rect(
                                    bar_r.x, bar_r.y, played, bar_h, bar_h * 0.5,
                                    rgb(180, 180, 180), (0, 0, 0, 0), 0.0,
                                );
                            }
                            let sid = 1305u32;
                            ui.state.register_focusable(sid);
                            let hit = Rect::new(bar_r.x, bar_r.y - 5.0, bar_r.w, bar_h + 10.0);
                            let shov = ui.state.can_hover(sid, hit);
                            if shov && ui.state.mouse_pressed {
                                ui.state.active_id = Some(sid);
                            }
                            if ui.state.active_id == Some(sid) && ui.state.mouse_down {
                                self.mixer_pos_frac =
                                    ((ui.state.cursor.0 - bar_r.x) / bar_r.w).clamp(0.0, 1.0);
                            }
                            let tfs = theme.font_xsmall();
                            let tbase = (seek_y + seek_h - 2.0).round();
                            ui.scene.text(
                                ui.text_sys, FontFace::SemiBold, ui.font_engine, "2:34",
                                inner.x.round(), tbase, tfs, theme.text_dim,
                            );
                            let tot = "3:26";
                            let tw = ui.text_sys.measure(FontFace::SemiBold, ui.font_engine, tot, tfs);
                            ui.scene.text(
                                ui.text_sys, FontFace::SemiBold, ui.font_engine, tot,
                                (inner.x + inner.w - tw).round(), tbase, tfs, theme.text_dim,
                            );
                        }

                        // ====================================================
                        // Phase 3.5 — Settings (persistent user.json).
                        // ====================================================
                        "settings" => {
                            ui.nui_card(body, |ui| {
                                let sel_dev = ui.input_group("Default Output Device");
                                let sel_rate = ui.input_group("Default Sample Rate");
                                let [c1, c2] = ui.take_cols(
                                    Length::rem(1.8), Length::rem(1.0),
                                    [Length::fill(), Length::fill()],
                                );
                                ui.checkbox(752, c1, "Keep Running in System Tray", &mut self.sw_keep_tray);
                                ui.checkbox(753, c2, "HQ Oversampling", &mut self.sw_hq_mode);
                                let [c3, c4] = ui.take_cols(
                                    Length::rem(1.8), Length::rem(1.0),
                                    [Length::fill(), Length::fill()],
                                );
                                ui.checkbox(754, c3, "Lock Parameters Across Tracks", &mut self.chk_locked);
                                ui.checkbox(755, c4, "Dark Mode (Accessibility Default)", &mut self.dark_mode);
                                let [c5] = ui.take_cols(
                                    Length::rem(1.8), Length::rem(1.0),
                                    [Length::fill()],
                                );
                                if ui.checkbox(757, c5, "Show transport controls [C]", &mut self.show_controls) {
                                    self.apply_zoom_req = true;
                                }
                                let [apply_r] = ui.take_form_row(true, [Length::fill()]);
                                if ui.button(756, apply_r, "Save Configuration", ICON_SETTINGS, ButtonVariant::Primary) {
                                    self.save_config_req = true;
                                }
                                let fs = theme.font_xsmall();
                                ui.text_clipped(
                                    FontFace::Regular, ui.font_engine, &self.config_status,
                                    apply_r.x.round(), (apply_r.y + apply_r.h + fs * 1.1).round(),
                                    fs, apply_r.w, theme.text_muted,
                                );
                                // Render selects last so open popups layer above siblings
                                ui.select(760, sel_dev, DEVICES, &mut self.selected_device);
                                ui.select(750, sel_rate, SAMPLE_RATES, &mut self.selected_rate);
                            });
                        }

                        // ---- Help: two-column keyboard-shortcut table ----
                        _ => {
                            ui.nui_card(body, |ui| {
                                let table = ui.take_rest();
                                let [lc, rc] = ui.cols(
                                    table, Length::rem(1.25), [Length::fill(), Length::fill()],
                                );
                                let half = HELP_ROWS.len().div_ceil(2);
                                let rh = (table.h / half.max(1) as f32).floor().min((rem * 2.0).round());
                                let kfs = theme.font_xsmall();
                                let dfs = theme.font_small();
                                let kw = Length::Rem(8.5).resolve(rem).round();
                                for (idx, &(k, v)) in HELP_ROWS.iter().enumerate() {
                                    let (col, row) = if idx < half { (lc, idx) } else { (rc, idx - half) };
                                    let y = (col.y + row as f32 * rh).round();
                                    let kbd_h = (rh - 6.0).max(16.0);
                                    let kbd_y = (y + (rh - kbd_h) * 0.5).round();
                                    let kbd_w = (kw - 10.0).max(24.0);
                                    ui.scene.rounded_rect(
                                        col.x, kbd_y, kbd_w, kbd_h, theme.radius1(),
                                        theme.base, theme.border3, 1.0,
                                    );
                                    let baseline = (y + rh * 0.5 + dfs * 0.34).round();
                                    ui.text_clipped(
                                        FontFace::SemiBold, ui.font_engine, k,
                                        (col.x + 6.0).round(), baseline, kfs, kbd_w - 12.0, theme.highlight,
                                    );
                                    ui.text_clipped(
                                        FontFace::Regular, ui.font_engine, v,
                                        (col.x + kw).round(), baseline, dfs,
                                        (col.w - kw).max(0.0), theme.text,
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

        // --- Phase 3.2 Parameters-window intents: forward every change to the
        // engine immediately (same frame), then refresh the main window's
        // speed/pitch overlay badge.
        if params_intents.reset_all {
            self.pitch_semitones = 0.0;
            self.tempo_ratio = 1.0;
            self.sync_pipeline_and_params();
        }
        if params_intents.params_changed || params_intents.pipeline_changed {
            self.sync_pipeline_and_params();
        }
        if params_intents.params_changed || params_intents.pipeline_changed || params_intents.reset_all
        {
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }

        if close_clicked {
            // The header close button hides (never exits) a secondary window.
            self.secondary_windows.hide(key);
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

        #[cfg(target_os = "windows")]
        {
            use winit::platform::windows::{Color, CornerPreference, WindowAttributesExtWindows};
            attrs = attrs
                .with_undecorated_shadow(true)
                .with_corner_preference(CornerPreference::Round)
                .with_border_color(Some(Color::from_rgb(55, 55, 55)));
        }

        let (window, mut renderer) = Renderer::new(event_loop, attrs, self.prefer_backend);
        let sz = window.inner_size();
        self.size = (sz.width, sz.height);
        self.window = Some(window.clone());
        renderer.request_reveal();
        self.renderer = Some(renderer);
        self.render();

        // Phase 1.5: `--open-window <key>` pops the requested secondary window at
        // startup; a `--screenshot` alongside it captures THAT window, not the primary.
        if let Some(spec) = self.open_window_spec {
            self.secondary_windows.show(event_loop, spec, self.prefer_backend);
            if let Some(path) = self.screenshot_path.take() {
                if let Some(w) = self.secondary_windows.find_by_key_mut(spec.key) {
                    w.screenshot_once = Some(path);
                }
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        // Phase 1.5 routing: the primary window owns the process lifetime; every other
        // window is a managed secondary (Escape / CloseRequested hide, not exit).
        if self.window.as_ref().map(|w| w.id()) != Some(id) {
            if self.secondary_windows.find_by_id_mut(id).is_some() {
                self.secondary_window_event(event_loop, id, event);
            }
            return;
        }
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
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
            WindowEvent::HoveredFile(path) => {
                self.ui_state.hovered_file = Some(path);
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            WindowEvent::HoveredFileCancelled => {
                self.ui_state.hovered_file = None;
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            WindowEvent::DroppedFile(path) => {
                self.ui_state.hovered_file = None;
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
                // While playing with animation on, the 60fps `about_to_wait` timer already
                // redraws every frame — only force a redraw here for immediate drag feedback
                // (mouse held) or when the timer is idle, so a 1000Hz mouse cannot spike FPS.
                let playing = self
                    .audio
                    .as_ref()
                    .map(|a| a.is_playing())
                    .unwrap_or(false);
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
                        self.master_vol =
                            (self.master_vol + 0.05 * dy.signum()).clamp(0.0, 2.0);
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
                            let dpi = self
                                .window
                                .as_ref()
                                .map(|w| w.scale_factor() as f32)
                                .unwrap_or(1.0);
                            let (cw, ch) = (self.size.0 as f32, self.size.1 as f32);
                            let (mx, my) = self.ui_state.cursor;
                            let edge = 6.0 * dpi;
                            // Undecorated window: dragging within 6 px of an edge or
                            // corner resizes, like a native frame.
                            let dir = if mx <= edge && my <= edge {
                                Some(ResizeDirection::NorthWest)
                            } else if mx >= cw - edge && my <= edge {
                                Some(ResizeDirection::NorthEast)
                            } else if mx <= edge && my >= ch - edge {
                                Some(ResizeDirection::SouthWest)
                            } else if mx >= cw - edge && my >= ch - edge {
                                Some(ResizeDirection::SouthEast)
                            } else if mx <= edge {
                                Some(ResizeDirection::West)
                            } else if mx >= cw - edge {
                                Some(ResizeDirection::East)
                            } else if my <= edge {
                                Some(ResizeDirection::North)
                            } else if my >= ch - edge {
                                Some(ResizeDirection::South)
                            } else {
                                None
                            };
                            if let Some(dir) = dir {
                                if let Some(w) = self.window.clone() {
                                    nui::drag_resize_window_live(&w, dir);
                                }
                                return;
                            }
                            let scale = (cw / Self::WIDTH as f32)
                                .min(ch / if self.show_controls {
                                    Self::HEIGHT_WITH_CONTROLS as f32
                                } else {
                                    Self::HEIGHT_WITHOUT_CONTROLS as f32
                                })
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
                    let focused_text = self.ui_state.focused_id == Some(301)
                        || self.ui_state.focused_id == Some(401);

                    match &event.logical_key {
                        Key::Named(NamedKey::Escape) => {
                            if self.ui_state.open_popup_id.is_some() {
                                self.ui_state.open_popup_id = None;
                                self.ui_state.popup_rect = None;
                            } else if focused_text {
                                self.ui_state.key_cmds.push(KeyCmd::Escape);
                            } else {
                                event_loop.exit();
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
                            } else if let Some(a) = &self.audio {
                                a.seek((a.position_secs() - 5.0).max(0.0));
                            }
                        }
                        Key::Named(NamedKey::ArrowRight) => {
                            if focused_text {
                                self.ui_state.key_cmds.push(KeyCmd::Right);
                            } else if let Some(a) = &self.audio {
                                a.seek(a.position_secs() + 5.0);
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
                        Key::Named(NamedKey::Space) if !focused_text => {
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
                        Key::Character(ch) if self.modifiers.control_key() && ch.eq_ignore_ascii_case("a") => {
                            self.ui_state.key_cmds.push(KeyCmd::SelectAll);
                        }
                        Key::Character(ch) if self.modifiers.control_key() && ch.eq_ignore_ascii_case("c") => {
                            self.ui_state.key_cmds.push(KeyCmd::Copy);
                        }
                        Key::Character(ch) if self.modifiers.control_key() && ch.eq_ignore_ascii_case("x") => {
                            self.ui_state.key_cmds.push(KeyCmd::Cut);
                        }
                        Key::Character(ch) if self.modifiers.control_key() && ch.eq_ignore_ascii_case("v") => {
                            self.ui_state.key_cmds.push(KeyCmd::Paste);
                        }
                        Key::Character(ch) if !focused_text && ch.eq_ignore_ascii_case("b") => {
                            if let Some(r) = &mut self.renderer {
                                r.toggle_backend();
                            }
                        }
                        Key::Character(ch) if !focused_text && ch.eq_ignore_ascii_case("f") => {
                            self.font_engine = match self.font_engine {
                                FontEngine::Swash => FontEngine::Fontdue,
                                FontEngine::Fontdue => FontEngine::Swash,
                            };
                        }
                        Key::Character(ch) if !focused_text && ch.eq_ignore_ascii_case("t") => {
                            self.dark_mode = !self.dark_mode;
                        }
                        Key::Character(ch) if (!focused_text || self.modifiers.control_key()) && (ch == "+" || ch == "=") => {
                            self.set_zoom(self.ui_zoom + 0.1);
                        }
                        Key::Character(ch) if (!focused_text || self.modifiers.control_key()) && (ch == "-" || ch == "_") => {
                            self.set_zoom(self.ui_zoom - 0.1);
                        }
                        Key::Character(ch) if (!focused_text || self.modifiers.control_key()) && ch == "0" => {
                            self.set_zoom(1.0);
                        }
                        Key::Character(ch) if !focused_text && ch.eq_ignore_ascii_case("l") => {
                            self.loop_enabled = !self.loop_enabled;
                            if let Some(a) = &self.audio {
                                a.set_loop(self.loop_enabled);
                            }
                        }
                        Key::Character(ch) if !focused_text && ch.eq_ignore_ascii_case("c") => {
                            self.show_controls = !self.show_controls;
                            self.apply_zoom();
                        }
                        Key::Character(ch)
                            if !focused_text
                                && matches!(
                                    ch.as_str(),
                                    "p" | "P" | "n" | "N" | "m" | "M" | "s" | "S" | "h" | "H"
                                ) =>
                        {
                            // Queue the secondary-window toggle; executed right after this
                            // frame in the RedrawRequested arm where `event_loop` lives.
                            let key = match ch.as_str() {
                                "p" | "P" => "parameters",
                                "n" | "N" => "monitoring",
                                "m" | "M" => "mixer",
                                "s" | "S" => "settings",
                                _ => "help",
                            };
                            self.pending_window_toggles.push(key);
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

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {        if let Some(q) = self.quit_at {
            if Instant::now() >= q {
                if !self.quitting {
                    self.quitting = true;
                    println!("[poc] auto-quit reached");
                }
                event_loop.exit();
                return;
            }
        }
        self.poll_transport_and_scans();

        let playing = self
            .audio
            .as_ref()
            .map(|a| a.is_playing())
            .unwrap_or(false);
        let pending_meta =
            self.audio.is_some() && !self.playlist.is_empty() && self.track_meta.is_none();
        let animating = (playing && self.animate) || pending_meta;

        let now = Instant::now();
        if animating && now >= self.next_frame {
            // Low-footprint cadence: the base player only needs ~15 fps for its
            // progress bar and MM:SS text; full-rate only while a DSP window is up.
            let dsp_open = self.secondary_windows.is_visible("monitoring")
                || self.secondary_windows.is_visible("mixer");
            let frame_ms = if pending_meta || dsp_open {
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

        let mut next: Option<Instant> = if animating { Some(self.next_frame) } else { None };
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
        self.config.dark_mode = self.dark_mode;
        self.config.ui_zoom = self.ui_zoom;
        self.config.master_vol = self.master_vol;
        self.config.show_controls = self.show_controls;
        self.config.chk_locked = self.chk_locked;
        self.config.sw_hq_mode = self.sw_hq_mode;
        self.config.sw_keep_tray = self.sw_keep_tray;
        if let Err(e) = self.config.save() {
            eprintln!("[poc] config save failed: {e}");
        }
    }
}

fn main() {
    let mut file: Option<String> = None;
    let mut tone = false;
    let mut silence = false;
    let mut animate = true;
    let mut nopaint = false;
    let mut seconds: Option<f64> = None;
    let mut fps: f64 = 60.0;
    let mut backend = BackendKind::Gpu;
    let mut font_engine = FontEngine::Swash;
    let mut app_screenshot: Option<String> = None;
    let mut open_window: Option<String> = None;

    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--tone" => tone = true,
            "--silence" => silence = true,
            "--static" => animate = false,
            "--nopaint" => nopaint = true,
            "--open-window" => open_window = args.next(),
            "--backend" => {
                if let Some(b) = args.next() {
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
    const DEFAULT_START: &str =
        r"D:\Work\_GIT\SoundApp\_Material\mixer_media\Again\01.m4a";
    // No file given → prefer the reference test track; its implicit folder
    // playlist then collects every playable file beside it. `--tone` / `--silence`
    // skip the startup path entirely.
    let default_start = if std::path::Path::new(DEFAULT_MURR).exists() {
        DEFAULT_MURR
    } else {
        DEFAULT_START
    };
    let startup_path: Option<PathBuf> = if tone || silence {
        None
    } else {
        Some(
            file.as_ref()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(default_start)),
        )
    };
    let source = if tone {
        Source::Tone { hz: 440.0 }
    } else {
        // The startup playlist (file or folder) is loaded by `open_path` below;
        // the engine starts on silence so nothing is decoded twice.
        Source::Silence
    };

    let audio = match audio::start(source) {
        Ok(a) => Some(a),
        Err(e) => {
            eprintln!("[poc] audio init failed: {e}");
            None
        }
    };

    let event_loop = EventLoop::new().unwrap();
    event_loop.set_control_flow(ControlFlow::Wait);
    let quit_at = seconds.map(|s| Instant::now() + Duration::from_secs_f64(s));
    // Phase 3.5 — restore the persisted configuration before building the app.
    let cfg = UserConfig::load();
    println!("[poc] config: {}", UserConfig::path().display());
    let mut app = App::new(audio, animate, quit_at, backend, font_engine, cfg);
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
