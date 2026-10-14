// SoundApp & NUI Rust Showcase — Dual-Backend Renderer (GPU Instanced SDF + CPU Softbuffer)
// with nui_wc2 v3.0.0 "slate" theme & components and Swash browser-grade typography.

mod audio;
mod midi;
mod tracker;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

use audio::{Audio, PlaybackParams, Source, SourceKind};
use audio::TrackMetadata;
use nui::{
    BackendKind, BadgeVariant, ButtonVariant, FontEngine, FontFace, KeyCmd, Length, Rect, Renderer,
    Scene, TextSystem, Theme, Ui, UiState, WindowManager, WindowSpec, ICON_CLOSE, ICON_DARK_MODE,
    ICON_EQ, ICON_LIGHT_MODE, ICON_LOOP, ICON_NEXT, ICON_PAUSE, ICON_PLAY, ICON_PREV,
    ICON_RESTART, ICON_SEARCH, ICON_SETTINGS, ICON_SHUFFLE, ICON_TUNE, ICON_VOLUME,
};

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
    extra_ops: usize,
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

    // Interactive NUI component states
    active_tab: usize,
    seg_pipeline: usize,
    /// Previous pipeline selection, used to (re)load the right engine when the
    /// segmented control switches to the MIDI synth.
    last_pipeline: usize,
    /// Previous SoundFont selection, used to switch the live SoundFont.
    last_soundfont: usize,
    search_text: String,
    preset_name: String,
    selected_device: usize,
    selected_rate: usize,
    selected_soundfont: usize,
    pitch_semitones: f32,
    tempo_ratio: f32,
    master_vol: f32,
    stereo_sep: f32,
    chk_formant: bool,
    chk_locked: bool,
    sw_hq_mode: bool,
    sw_keep_tray: bool,
    radio_interp: usize,
    click_counter: u32,
    scrub_frac: f32,
    selected_track: usize,
    mixer_gains: [f32; 4],
    mixer_mutes: [bool; 4],
    /// Deterministic 96-bar synthetic waveform for the transport seekbar.
    wave_peaks: Vec<f32>,

    // Phase 3.1 — player transport state (loop / shuffle / compact window).
    pub loop_enabled: bool,
    pub shuffle_enabled: bool,
    /// `true` renders the 580x300 compact SoundApp Player; `false` the full
    /// 1120x700 studio/showcase layout. Toggled with `C` or the header button.
    pub compact_player_mode: bool,

    // Phase 3.2 — adaptive Parameters window (Audio / MIDI / Tracker tabs).
    /// Active Parameters tab: `0` = Audio, `1` = MIDI, `2` = Tracker.
    pub params_tab: usize,
    pub midi_transpose: f32,
    pub midi_bpm: f32,
    pub midi_metronome: bool,
    /// Kind of the source the engine reported as loaded (drives `params_tab`).
    loaded_kind: Option<SourceKind>,

    // Phase 2.1/2.2 — real playlist + FFmpeg metadata shown in the Left Card.
    /// Real `.m4a` tracks discovered on disk (the first slice of the 10k playlist).
    playlist: Vec<PathBuf>,
    /// Metadata of the currently-loaded file (from FFmpeg).
    track_meta: Option<TrackMetadata>,
    /// True while the waveform seekbar is being dragged (seek on release).
    scrub_active: bool,
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

const SOUNDFONTS: &[&str] = &[
    "TimGM6mb.sf2 (Default GM)",
    "FluidR3_GM.sf2 (142 MB Studio)",
    "CrisisGeneralMidi3.01.sf2",
    "Arachno_SoundFont_v1.0.sf2",
];

/// Resolve a SoundFont list index to an on-disk `.sf2` path (best effort —
/// non-existent files are reported by the engine when the switch is attempted).
fn soundfont_path_for(idx: usize) -> Option<PathBuf> {
    let dir = PathBuf::from(r"D:\Work\_GIT\SoundApp\bin\soundfonts");
    match idx {
        0 => Some(PathBuf::from(midi::DEFAULT_SOUNDFONT)),
        1 => Some(dir.join("FluidR3_GM.sf2")),
        2 => Some(dir.join("CrisisGeneralMidi3.01.sf2")),
        3 => Some(dir.join("Arachno_SoundFont_v1.0.sf2")),
        _ => None,
    }
}

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
    width: 760.0,
    height: 480.0,
    min_width: 560.0,
    min_height: 360.0,
    undecorated: true,
};

const SPEC_MIXER: WindowSpec = WindowSpec {
    key: "mixer",
    title: "SoundApp · Multi-Track Stem Mixer [M]",
    width: 820.0,
    height: 480.0,
    min_width: 600.0,
    min_height: 360.0,
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
    width: 640.0,
    height: 460.0,
    min_width: 500.0,
    min_height: 340.0,
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

/// (key, button label) pairs for the main window's window-toggle button row.
const WINDOW_TOGGLES: &[(&'static str, &'static str)] = &[
    ("parameters", "[P] Params"),
    ("monitoring", "[N] Monitor"),
    ("mixer", "[M] Mixer"),
    ("settings", "[S] Settings"),
    ("help", "[H] Help"),
];

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

/// Synthetic library size for the virtualized playlist demos (Phase 1.2).
const TRACK_COUNT: usize = 10_000;

const TRACK_ARTISTS: &[&str] = &[
    "Aurora Field",
    "Kraftwerk Delta",
    "Nova Ensemble",
    "Blue Static",
    "Echo Division",
    "Sunset Array",
    "Magnetic North",
    "Pale Horizon",
    "Liquid Chrome",
    "Cascade Theory",
    "Midnight Cartography",
    "Signal Bloom",
];

const TRACK_TITLES: &[&str] = &[
    "Analog Sunrise",
    "Chrome Cassette",
    "Midnight Modulation",
    "Vinyl Static",
    "Pulse Width",
    "Tape Hiss Serenade",
    "Neon Riverbed",
    "Slow Waveform",
    "Crystal Oscillator",
    "Deep Signal",
    "Harmonic Drift",
    "Sidechain Sunset",
    "Quarter Note Sky",
    "Ferric Dreams",
    "Patch Bay Romance",
    "Lowpass Meadow",
];

const TRACK_ALBUMS: &[&str] = &[
    "Analog Drift",
    "Neon Meridian",
    "Deep Field",
    "Rust & Glass",
    "Slow Orbit",
    "Static Bloom",
];

fn track_name(idx: usize) -> String {
    TRACK_TITLES[idx % TRACK_TITLES.len()].to_string()
}

fn track_credit(idx: usize) -> String {
    let artist = TRACK_ARTISTS[idx % TRACK_ARTISTS.len()];
    let album = TRACK_ALBUMS[(idx / 7) % TRACK_ALBUMS.len()];
    format!("{artist} — {album}")
}

fn track_meta(idx: usize) -> String {
    let mm = 2 + (idx * 7) % 8;
    let ss = (idx * 13) % 60;
    let lufs = 10.0 + ((idx * 17) % 80) as f32 / 10.0;
    format!("48 kHz · 24-bit · -{lufs:.1} LUFS · {mm:02}:{ss:02}")
}

/// The real `.m4a` tracks in SoundApp's mixer media folder, if present on disk.
/// These form the head of the virtual playlist; everything past them is synthetic.
fn default_playlist() -> Vec<PathBuf> {
    let dir = PathBuf::from(r"D:\Work\_GIT\SoundApp\_Material\mixer_media\Again");
    let mut tracks: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case("m4a"))
                == Some(true)
            {
                tracks.push(path);
            }
        }
    }
    tracks.sort();
    tracks
}

/// Short label for the transport row: `mm:ss`, or `--:--` when unknown.
fn fmt_meta_duration(secs: f64) -> String {
    if secs > 0.0 {
        fmt_time(secs)
    } else {
        "--:--".to_string()
    }
}

/// Intents produced by the shared transport row (Phase 3.1), applied after the
/// render borrow ends.
#[derive(Default)]
struct TransportIntents {
    toggle_play: bool,
    prev: bool,
    next: bool,
    loop_toggled: bool,
    shuffle_toggled: bool,
    vol_changed: bool,
    seek_to: Option<f64>,
}

/// Intents produced by the Phase 3.2 adaptive Parameters window.
#[derive(Default)]
struct ParamsWindowIntents {
    params_changed: bool,
    pipeline_changed: bool,
    reset_all: bool,
    load_midi: bool,
    load_tracker: bool,
}

/// Track header row: index pill badge + SemiBold title over dim format/codec info.
fn draw_track_header(ui: &mut Ui, r: Rect, pill: &str, title: &str, meta: &str) {
    let t = ui.theme;
    let badge_h = (t.rem * 1.48).round();
    let badge_w = ui.badge(r.x, r.y, pill, BadgeVariant::Primary, None);
    let fs = t.font_small();
    let title_x = r.x + badge_w + t.space_half();
    ui.text_clipped(
        FontFace::SemiBold,
        ui.font_engine,
        title,
        title_x.round(),
        (r.y + badge_h * 0.5 + fs * 0.34).round(),
        fs,
        (r.x + r.w - title_x).max(0.0),
        t.text,
    );
    let fs2 = t.font_xsmall();
    ui.text_clipped(
        FontFace::Regular,
        ui.font_engine,
        meta,
        r.x.round(),
        (r.y + badge_h + fs2 * 1.1).round(),
        fs2,
        r.w,
        t.text_dim,
    );
}

/// Realtime 18-band spectrum analyzer with the stabilized `kind = 4` oscilloscope
/// polyline across the center. Top-right overlay: speed/pitch badge + a compact
/// `ICON_SETTINGS` button. Returns `true` when the settings button was clicked.
fn draw_spec_panel(
    ui: &mut Ui,
    r: Rect,
    t: f32,
    playing: bool,
    vol: f32,
    overlay_badge: &str,
) -> bool {
    let th = ui.theme;
    let rem = th.rem;
    ui.scene
        .rounded_rect(r.x, r.y, r.w, r.h, th.radius2(), th.base, th.border2, 1.0);
    ui.scene.push_clip(
        r.x + 1.0,
        r.y + 1.0,
        (r.w - 2.0).max(0.0),
        (r.h - 2.0).max(0.0),
    );
    let bands = 18usize;
    let inset = (rem * 0.4).round();
    let g = (rem * 0.3).round().max(2.0);
    let bar_w = ((r.w - inset * 2.0 - g * (bands as f32 - 1.0)) / bands as f32)
        .floor()
        .max(2.0);
    for i in 0..bands {
        let phase = t * 5.8 + i as f32 * 0.42;
        let amp = if playing {
            ((phase.sin() * 0.35 + (phase * 1.7).cos() * 0.25 + 0.45) * vol).clamp(0.06, 0.94)
        } else {
            (0.14 + 0.08 * (i as f32 * 0.5).sin()) * vol
        };
        let bh = ((r.h - rem * 1.0) * amp).max(2.0);
        let bx = r.x + inset + i as f32 * (bar_w + g);
        let by = r.y + r.h - rem * 0.5 - bh;
        ui.scene
            .rounded_rect(bx, by, bar_w, bh, 2.0, th.highlight, (0, 0, 0, 0), 0.0);
    }

    // Realtime oscilloscope / harmonic curve drawn ON TOP of the spectrum bars.
    let osc_pts: Vec<(f32, f32)> = (0..=64)
        .map(|i| {
            let nx = i as f32 / 64.0;
            let phase = t * 6.0 + nx * std::f32::consts::TAU * 2.0;
            let amp = 0.42 * phase.sin() + 0.20 * (phase * 2.3).cos() + 0.12 * (phase * 4.1).sin();
            let ny = (0.5 + amp * vol.max(0.2)).clamp(0.05, 0.95);
            (nx, ny)
        })
        .collect();
    ui.polyline(r, &osc_pts, 2.0, th.palette_mark);

    // Top-right overlay: speed/pitch badge + compact Parameters toggle. The
    // badge's left edge mirrors `Ui::badge` metrics so it can be right-anchored
    // against the button without a two-pass draw.
    let btn_h = (rem * 1.75).round();
    let btn_r = Rect::new(r.x + r.w - inset - btn_h, r.y + inset, btn_h, btn_h);
    let settings_clicked = ui.button(190, btn_r, "", ICON_SETTINGS, ButtonVariant::Ghost);
    let b_fs = (rem * 0.72).round();
    let b_w = (rem * 0.36).round().max(5.0)
        + (rem * 0.55).round() * 2.0
        + ui
            .text_sys
            .measure(FontFace::SemiBold, ui.font_engine, &overlay_badge.to_uppercase(), b_fs);
    let b_h = (rem * 1.48).round();
    let _ = ui.badge(
        (btn_r.x - rem * 0.45 - b_w).round(),
        r.y + inset + (btn_h - b_h) * 0.5,
        overlay_badge,
        BadgeVariant::Primary,
        None,
    );
    ui.scene.pop_clip();
    settings_clicked
}

/// Secondary-window launcher rows. One row per `rows` entry; five buttons split
/// `3 + 2` across two rows, or all five on a single row. Returns the clicked
/// window index (into `WINDOW_TOGGLES`), if any.
fn draw_window_launcher(ui: &mut Ui, rows: &[Rect], vis: &[bool; 5]) -> Option<usize> {
    let t = ui.theme;
    let gap = t.space_quarter();
    let per_row: &[usize] = if rows.len() >= 2 { &[3, 2] } else { &[5] };
    let mut clicked = None;
    let mut widx = 0usize;
    for (ri, row) in rows.iter().enumerate() {
        let n = per_row[ri];
        let cw = ((row.w - gap * (n as f32 - 1.0)) / n as f32).floor();
        for ci in 0..n {
            let (key, label) = WINDOW_TOGGLES[widx];
            let variant = if vis[widx] {
                ButtonVariant::Primary
            } else {
                ButtonVariant::Outline
            };
            let cr = Rect::new(row.x + (cw + gap) * ci as f32, row.y, cw, row.h);
            if ui.button(911 + widx as u32, cr, label, "", variant) {
                let _ = key;
                clicked = Some(widx);
            }
            widx += 1;
        }
    }
    clicked
}

/// Shared transport card interior (Phase 3.1): full-width interactive waveform
/// seekbar over a control row — elapsed time left, a centered Prev · Play/Pause ·
/// Next · Loop · Shuffle group, and remaining/total time + a compact master
/// volume slider right.
#[allow(clippy::too_many_arguments)]
fn draw_transport(
    ui: &mut Ui,
    interior: Rect,
    wave_h: Length,
    t: f32,
    peaks: &[f32],
    scrub_frac: &mut f32,
    scrub_active: &mut bool,
    pos: f64,
    dur: f64,
    playing: bool,
    vol: &mut f32,
    loop_on: bool,
    shuffle_on: bool,
) -> TransportIntents {
    let mut it = TransportIntents::default();
    let th = ui.theme;
    let rem = th.rem;

    // Seekbar: follows playback; while scrubbing it follows the pointer and the
    // seek is committed on release.
    let wave_r_h = wave_h.resolve(rem).round();
    let wave_r = Rect::new(interior.x, interior.y, interior.w, wave_r_h);
    let is_finite = dur > 0.0 && dur < 360_000.0;
    if ui.state.active_id != Some(500) {
        if is_finite {
            *scrub_frac = (pos / dur).clamp(0.0, 1.0) as f32;
        } else if playing {
            *scrub_frac = (t * 0.12).fract();
        }
    }
    let scrubbed = ui.waveform(500, wave_r, peaks, scrub_frac);
    if scrubbed {
        *scrub_active = true;
    } else if *scrub_active {
        *scrub_active = false;
        if is_finite {
            it.seek_to = Some(*scrub_frac as f64 * dur);
        }
    }

    let gap = th.space_half();
    let ctrl = Rect::new(
        interior.x,
        wave_r.y + wave_r.h + gap,
        interior.w,
        (interior.y + interior.h - wave_r.y - wave_r.h - gap).max(0.0),
    );
    let ctrl_cy = ctrl.y + ctrl.h * 0.5;
    let fs = th.font_small();

    // Elapsed — left.
    ui.scene.text(
        ui.text_sys,
        FontFace::SemiBold,
        ui.font_engine,
        &fmt_time(pos),
        ctrl.x.round(),
        (ctrl_cy + fs * 0.34).round(),
        fs,
        th.text,
    );

    // Centered transport group: Prev · Play/Pause · Next · Loop · Shuffle.
    let bw = (rem * 2.4).round();
    let bh = (rem * 1.85).round();
    let bgap = (rem * 0.35).round();
    let group_w = bw * 5.0 + bgap * 4.0;
    let mut bx = ctrl.x + (ctrl.w - group_w) * 0.5;
    let by = ctrl_cy - bh * 0.5;
    let defs = [
        (501u32, ICON_PREV, ButtonVariant::Ghost),
        (
            502u32,
            if playing { ICON_PAUSE } else { ICON_PLAY },
            ButtonVariant::Primary,
        ),
        (503u32, ICON_NEXT, ButtonVariant::Ghost),
        (
            504u32,
            ICON_LOOP,
            if loop_on {
                ButtonVariant::Primary
            } else {
                ButtonVariant::Ghost
            },
        ),
        (
            505u32,
            ICON_SHUFFLE,
            if shuffle_on {
                ButtonVariant::Primary
            } else {
                ButtonVariant::Ghost
            },
        ),
    ];
    for (id, icon, var) in defs {
        if ui.button(id, Rect::new(bx, by, bw, bh), "", icon, var) {
            match id {
                501 => it.prev = true,
                502 => it.toggle_play = true,
                503 => it.next = true,
                504 => it.loop_toggled = true,
                _ => it.shuffle_toggled = true,
            }
        }
        bx += bw + bgap;
    }

    // Remaining / total time + compact master volume — right.
    let remain = if is_finite {
        format!("-{} / {}", fmt_time((dur - pos).max(0.0)), fmt_time(dur))
    } else if playing {
        "LIVE".to_string()
    } else {
        "-03:42 / 03:42".to_string()
    };
    let slider_w = Length::Rem(10.0).resolve(rem).round();
    let slider_h = Length::Rem(2.9).resolve(rem).round();
    let slider_r = Rect::new(
        ctrl.x + ctrl.w - slider_w,
        ctrl_cy - slider_h * 0.5,
        slider_w,
        slider_h,
    );
    let vol_str = format!("{:.0}%", *vol * 100.0);
    if ui.slider(506, slider_r, "Master Volume", vol, 0.0, 1.0, 0.78, &vol_str) {
        it.vol_changed = true;
    }
    let rw = ui
        .text_sys
        .measure(FontFace::SemiBold, ui.font_engine, &remain, fs);
    ui.scene.text(
        ui.text_sys,
        FontFace::SemiBold,
        ui.font_engine,
        &remain,
        (slider_r.x - rw - rem * 0.75).round(),
        (ctrl_cy + fs * 0.34).round(),
        fs,
        th.text,
    );
    it
}

/// Caption lines for the Typography A/B cards, keyed by the CSS px size being shown.
fn typo_sample(css_px: f32) -> &'static str {
    if css_px <= 12.0 {
        "To AV We Yo · -14.2 LUFS · 48,000 Hz · Pack my box with five dozen liquor jugs"
    } else if css_px <= 15.0 {
        "Waveform & Spectrum Analyzer · 00:42 / 03:18 · Crisp Stems & Even Kerning"
    } else {
        "Hamburgefonstiv · QWERTY UIOP · Excellent Kerning & Hinting · 01:23 / 04:56"
    }
}

impl App {
    fn new(
        audio: Option<Audio>,
        animate: bool,
        quit_at: Option<Instant>,
        prefer_backend: BackendKind,
        font_engine: FontEngine,
    ) -> Self {
        Self {
            start: Instant::now(),
            reported: false,
            animate,
            quit_at,
            prefer_backend,
            font_engine,
            dark_mode: true, // Dark mode by default (accessibility requirement)
            ui_zoom: 1.0,
            screenshot_path: None,
            window: None,
            renderer: None,
            scene: Scene::new(),
            text_sys: TextSystem::new(),
            ui_state: UiState::new(),
            modifiers: ModifiersState::empty(),
            size: (1120, 700),
            audio,
            audio_reported: false,
            t: 0.0,
            frame_ms: 16,
            nopaint: false,
            extra_ops: 0,
            frames: 0,
            last_fps: 0,
            fps_at: Instant::now(),
            next_frame: Instant::now(),
            quitting: false,

            secondary_windows: WindowManager::new(),
            pending_window_toggles: Vec::new(),
            open_window_spec: None,

            active_tab: 0,
            seg_pipeline: 1,
            last_pipeline: 1,
            last_soundfont: 0,
            search_text: "OpenSans + Swash Subpixel".to_string(),
            preset_name: "Studio Mastering (-14 LUFS)".to_string(),
            selected_device: 0,
            selected_rate: 0,
            selected_soundfont: 0,
            pitch_semitones: 3.0,
            tempo_ratio: 1.18,
            master_vol: 0.78,
            stereo_sep: 100.0,
            chk_formant: true,
            chk_locked: false,
            sw_hq_mode: true,
            sw_keep_tray: true,
            radio_interp: 1,
            click_counter: 0,
            scrub_frac: 0.35,
            selected_track: 0,
            mixer_gains: [0.8, 0.7, 0.75, 0.6],
            mixer_mutes: [false; 4],
            wave_peaks: audio::synth_wave_peaks(96),
            loop_enabled: false,
            shuffle_enabled: false,
            compact_player_mode: false,
            params_tab: 0,
            midi_transpose: 0.0,
            midi_bpm: 120.0,
            midi_metronome: false,
            loaded_kind: None,
            playlist: default_playlist(),
            track_meta: None,
            scrub_active: false,
        }
    }

    /// Logical base size of the main window for the active player mode.
    fn base_window_size(&self) -> (f64, f64) {
        if self.compact_player_mode {
            (580.0, 300.0)
        } else {
            (1120.0, 700.0)
        }
    }

    fn set_zoom(&mut self, new_zoom: f32) {
        let clamped = ((new_zoom * 20.0).round() / 20.0).clamp(0.75, 2.0);
        if (clamped - self.ui_zoom).abs() < 0.001 {
            return;
        }
        self.ui_zoom = clamped;
        self.apply_player_mode();
    }

    /// The current UI parameter state as an engine `PlaybackParams`.
    fn playback_params(&self) -> PlaybackParams {
        PlaybackParams {
            mode_tape: self.seg_pipeline == 0,
            pitch_semitones: self.pitch_semitones,
            tempo_ratio: self.tempo_ratio,
            hq: self.sw_hq_mode,
            formant_preserve: self.chk_formant,
            stereo_sep: self.stereo_sep,
            interp: self.radio_interp,
            midi_transpose: self.midi_transpose,
            midi_bpm: self.midi_bpm,
            metronome: self.midi_metronome,
        }
    }

    /// Re-apply the pipeline selection and push the full parameter set to the
    /// engine. Switching into the MIDI synth pipeline loads the built-in GM
    /// sequence; leaving it reloads the selected playlist track.
    fn sync_pipeline_and_params(&mut self) {
        let params = self.playback_params();
        let Some(a) = &self.audio else { return };
        if self.seg_pipeline == 2 && self.last_pipeline != 2 {
            a.load_midi_demo();
        } else if self.seg_pipeline != 2 && self.last_pipeline == 2 && !self.playlist.is_empty() {
            let idx = self.selected_track.min(self.playlist.len() - 1);
            a.load_file(self.playlist[idx].clone());
        }
        self.last_pipeline = self.seg_pipeline;
        a.set_params(params);
    }

    /// Apply the active player mode to the native window: compact mode resizes
    /// to 580x300 (with a matching minimum size), studio mode to 1120x700.
    fn apply_player_mode(&mut self) {
        let Some(w) = &self.window else { return };
        let z = self.ui_zoom as f64;
        let (min_w, min_h) = if self.compact_player_mode {
            (420.0, 240.0)
        } else {
            (760.0, 480.0)
        };
        let (bw, bh) = self.base_window_size();
        // Lower the minimum first so the window is allowed to shrink.
        let _ = w.set_min_inner_size(Some(winit::dpi::LogicalSize::new(min_w * z, min_h * z)));
        let _ = w.request_inner_size(winit::dpi::LogicalSize::new(bw * z, bh * z));
        self.size = (w.inner_size().width, w.inner_size().height);
        w.request_redraw();
    }

    fn build_scene(&mut self, w: u32, h: u32, scale: f32) {
        let _ = self.text_sys.prepare_frame();
        // `--nui-rem-base: 16px` (nui-theme.css)
        let rem = (16.0 * scale).round();
        let theme = if self.dark_mode {
            Theme::dark(rem)
        } else {
            Theme::light(rem)
        };

        // Page background (`body { background: light-dark(shade2, shade1) }` in nui-theme.css)
        let page_bg = if theme.dark {
            (theme.shade1.0, theme.shade1.1, theme.shade1.2)
        } else {
            (theme.shade2.0, theme.shade2.1, theme.shade2.2)
        };
        self.scene.clear_all(page_bg);
        self.ui_state.begin_frame();

        let pw = w as f32;
        let ph = h as f32;
        let active_backend = self
            .renderer
            .as_ref()
            .map(|r| r.active)
            .unwrap_or(self.prefer_backend);
        // Pull real metadata + waveform peaks from the decoder thread (it
        // publishes them when a file is loaded) — dark-mode Left Card + seekbar.
        let (peaks, meta) = match &self.audio {
            Some(a) => (a.peaks(), a.metadata()),
            None => (Vec::new(), None),
        };
        if !peaks.is_empty() && peaks != self.wave_peaks {
            self.wave_peaks = peaks;
        }
        if meta != self.track_meta {
            self.track_meta = meta;
        }
        // Phase 3.2 — the Parameters window's tab follows the loaded source kind
        // (audio file -> Audio, MIDI -> MIDI, tracker module -> Tracker).
        if let Some(k) = self.audio.as_ref().and_then(|a| a.source_kind()) {
            if self.loaded_kind != Some(k) {
                self.loaded_kind = Some(k);
                self.params_tab = match k {
                    SourceKind::Audio => 0,
                    SourceKind::Midi => 1,
                    SourceKind::Tracker => 2,
                };
            }
        }

        let playing = self.audio.as_ref().map(|a| a.is_playing()).unwrap_or(false);
        let (pos, dur) = self
            .audio
            .as_ref()
            .map(|a| (a.position_secs(), a.duration_secs()))
            .unwrap_or((0.0, 0.0));

        let mut toggle_backend_req = false;
        let mut toggle_font_req = false;
        let mut toggle_theme_req = false;
        let mut zoom_delta: f32 = 0.0;
        let mut close_req = false;
        let mut compact_req = false;
        // Phase 3.1 transport intents from the shared transport row, applied
        // after the render borrow ends.
        let mut transport_intents = TransportIntents::default();
        let mut load_path: Option<PathBuf> = None;
        let mut params_changed = false;

        {
            let mut ui = Ui::new(
                &mut self.ui_state,
                &mut self.scene,
                &mut self.text_sys,
                self.font_engine,
                theme,
            );

            // ================================================================
            // 1. NUI App Header (`nui-app-header`, 3.25rem)
            // ================================================================
            let hdr_h = theme.header_height();
            ui.scene.rect(0.0, 0.0, pw, hdr_h, theme.shade2);
            ui.scene
                .rect(0.0, hdr_h - 1.0, pw, 1.0, theme.border2);

            // Clip header left zone (logo, title, badges) so it never collides with right-side buttons.
            // The compact player keeps only Theme + Compact + Close (10.7rem);
            // the studio layout shows the full button stack (26.5rem).
            let right_zone_w = (rem * if self.compact_player_mode { 10.7 } else { 26.5 }).round();
            ui.scene
                .push_clip(0.0, 0.0, (pw - right_zone_w).max(0.0), hdr_h);

            let icon_sz = (rem * 1.25).round();
            ui.scene.icon_centered(
                ui.text_sys,
                ICON_EQ,
                rem * 0.85 + icon_sz * 0.5,
                hdr_h * 0.5,
                icon_sz,
                theme.highlight,
            );
            let title_fs = theme.font_base();
            let title_w = ui.scene.text(
                ui.text_sys,
                FontFace::SemiBold,
                ui.font_engine,
                "SoundApp · NUI Rust",
                (rem * 2.35).round(),
                (hdr_h * 0.5 + title_fs * 0.34).round(),
                title_fs,
                theme.text,
            );

            // Status badges (`nui-badge`)
            let badge_y = (hdr_h - (rem * 1.45).round()) * 0.5;
            let mut bx = rem * 2.35 + title_w + rem * 0.85;
            let b_lbl = match active_backend {
                BackendKind::Gpu => "GPU · GL 3.3 SDF",
                BackendKind::Cpu => "CPU · Softbuffer",
            };
            let b_var = match active_backend {
                BackendKind::Gpu => BadgeVariant::Primary,
                BackendKind::Cpu => BadgeVariant::Warning,
            };
            bx += ui.badge(bx, badge_y, b_lbl, b_var, Some(true)) + rem * 0.45;

            let fps_badge = if self.last_fps > 0 {
                format!("{} FPS", self.last_fps)
            } else {
                "0% IDLE".to_string()
            };
            let _ = ui.badge(bx, badge_y, &fps_badge, BadgeVariant::Success, None);
            ui.scene.pop_clip();

            // Header right controls. Studio mode: Backend / Font / Theme / Zoom ± /
            // Compact / Close. Compact player: Theme / Compact / Close only, so
            // the 580px window keeps a usable title zone.
            let btn_h = (rem * 1.75).round();
            let btn_y = (hdr_h - btn_h) * 0.5;
            let close_w = (rem * 2.1).round();
            let mut rx = pw - rem * 0.6 - close_w;

            if ui.button(
                90,
                Rect::new(rx, btn_y, close_w, btn_h),
                "",
                ICON_CLOSE,
                ButtonVariant::Danger,
            ) {
                close_req = true;
            }

            let z_w = (rem * 1.75).round();
            if !self.compact_player_mode {
                rx -= z_w + rem * 0.3;
                if ui.button(91, Rect::new(rx, btn_y, z_w, btn_h), "+", "", ButtonVariant::Outline) {
                    zoom_delta = 0.1;
                }
                rx -= z_w + rem * 0.25;
                if ui.button(92, Rect::new(rx, btn_y, z_w, btn_h), "-", "", ButtonVariant::Outline) {
                    zoom_delta = -0.1;
                }
            }

            let th_w = (rem * 2.1).round();
            rx -= th_w + rem * 0.35;
            let th_icon = if self.dark_mode {
                ICON_LIGHT_MODE
            } else {
                ICON_DARK_MODE
            };
            if ui.button(
                93,
                Rect::new(rx, btn_y, th_w, btn_h),
                "",
                th_icon,
                ButtonVariant::Ghost,
            ) {
                toggle_theme_req = true;
            }

            if !self.compact_player_mode {
                let fn_w = (rem * 5.8).round();
                rx -= fn_w + rem * 0.35;
                let fn_lbl = match self.font_engine {
                    FontEngine::Swash => "Swash [F]",
                    FontEngine::Fontdue => "Fontdue [F]",
                };
                if ui.button(
                    94,
                    Rect::new(rx, btn_y, fn_w, btn_h),
                    fn_lbl,
                    "",
                    ButtonVariant::Outline,
                ) {
                    toggle_font_req = true;
                }

                let bk_w = (rem * 5.2).round();
                rx -= bk_w + rem * 0.35;
                let bk_lbl = match active_backend {
                    BackendKind::Gpu => "GPU [B]",
                    BackendKind::Cpu => "CPU [B]",
                };
                if ui.button(
                    95,
                    Rect::new(rx, btn_y, bk_w, btn_h),
                    bk_lbl,
                    "",
                    ButtonVariant::Primary,
                ) {
                    toggle_backend_req = true;
                }
            }

            let cp_w = (rem * 5.2).round();
            rx -= cp_w + rem * 0.35;
            let cp_lbl = if self.compact_player_mode {
                "Studio [C]"
            } else {
                "Compact [C]"
            };
            if ui.button(
                96,
                Rect::new(rx, btn_y, cp_w, btn_h),
                cp_lbl,
                "",
                ButtonVariant::Outline,
            ) {
                compact_req = true;
            }

            // ================================================================
            // 2. Main Layout Grid. Studio mode: Left Player Card + Right NUI
            //    Tabs Card + Bottom Transport Card. Compact mode (`C`): the
            //    580x300 SoundApp Player — the player card IS the whole body.
            //    nui_wc2 proportions: 1.25rem window padding + inter-card gap.
            // ================================================================
            let m = Length::Rem(1.25).resolve(rem).round();
            let top_y = hdr_h + m;

            // Shared player-card data (both layouts): playlist pill, track
            // title, format/codec line, speed/pitch overlay, window visibility.
            let vis = [
                self.secondary_windows.is_visible("parameters"),
                self.secondary_windows.is_visible("monitoring"),
                self.secondary_windows.is_visible("mixer"),
                self.secondary_windows.is_visible("settings"),
                self.secondary_windows.is_visible("help"),
            ];
            let (title_lbl, meta_lbl) = match &self.track_meta {
                Some(m) => (
                    self.audio
                        .as_ref()
                        .and_then(|a| a.current_file())
                        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                        .unwrap_or_else(|| "(loaded)".to_string()),
                    format!(
                        "{} · {} Hz · {} kbps · {}",
                        if m.codec.is_empty() { "unknown" } else { &m.codec },
                        m.sample_rate,
                        m.bitrate / 1000,
                        fmt_meta_duration(m.duration)
                    ),
                ),
                None => (
                    track_name(self.selected_track),
                    track_meta(self.selected_track),
                ),
            };
            let pill = format!(
                "{:02}/{:02}",
                (self.selected_track + 1).min(99),
                self.playlist.len().max(13)
            );
            let overlay_badge =
                format!("{:.2}x · {:+.0}st", self.tempo_ratio, self.pitch_semitones);

            if self.compact_player_mode {
                // ---- Phase 3.1 compact SoundApp Player (580x300) ----
                let cm = Length::Rem(0.75).resolve(rem).round();
                let body_r = Rect::new(cm, top_y, pw - cm * 2.0, (ph - top_y - cm).max(60.0));
                ui.card(body_r, Length::rem(0.75), Length::rem(0.45), |ui| {
                    let inner = ui.bounds();
                    let [th_r, spec_r, launch_r, tp_r] = ui.rows(
                        inner,
                        Length::rem(0.45),
                        [
                            Length::rem(2.2),
                            Length::fill(),
                            Length::rem(1.6),
                            Length::Rem(4.2),
                        ],
                    );
                    draw_track_header(ui, th_r, &pill, &title_lbl, &meta_lbl);
                    if draw_spec_panel(ui, spec_r, self.t, playing, self.master_vol, &overlay_badge)
                    {
                        self.pending_window_toggles.push("parameters");
                    }
                    if let Some(wi) = draw_window_launcher(ui, &[launch_r], &vis) {
                        self.pending_window_toggles.push(WINDOW_TOGGLES[wi].0);
                    }
                    transport_intents = draw_transport(
                        ui,
                        tp_r,
                        Length::rem(1.6),
                        self.t,
                        &self.wave_peaks,
                        &mut self.scrub_frac,
                        &mut self.scrub_active,
                        pos,
                        dur,
                        playing,
                        &mut self.master_vol,
                        self.loop_enabled,
                        self.shuffle_enabled,
                    );
                });
            } else {
                let bot_h = Length::Rem(6.4).resolve(rem).round();
                let main_h = (ph - top_y - bot_h - m * 2.0).max(120.0);
                let left_w = Length::Rem(20.0).resolve(rem).round();

                // Left Card — Phase 3.1 player panel: track header row, realtime
                // spectrum + oscilloscope with the speed/pitch overlay, then the
                // secondary-window launcher rows.
                let left_rect = Rect::new(m, top_y, left_w, main_h);
                ui.nui_card(left_rect, |ui| {
                    let inner = ui.bounds();
                    let [th_r, spec_r, l1, l2] = ui.rows(
                        inner,
                        Length::rem(0.8),
                        [
                            Length::rem(3.0),
                            Length::fill(),
                            Length::rem(1.7),
                            Length::rem(1.7),
                        ],
                    );
                    draw_track_header(ui, th_r, &pill, &title_lbl, &meta_lbl);
                    if draw_spec_panel(
                        ui,
                        spec_r,
                        self.t,
                        playing,
                        self.master_vol,
                        &overlay_badge,
                    ) {
                        self.pending_window_toggles.push("parameters");
                    }
                    if let Some(wi) = draw_window_launcher(ui, &[l1, l2], &vis) {
                        self.pending_window_toggles.push(WINDOW_TOGGLES[wi].0);
                    }
                });

            // ================================================================
            // 3. Right Panel: NUI Component Gallery & SoundApp Parameters (`nui-tabs`)
            //    Declarative: nui-card -> tabs row -> per-tab layout body.
            // ================================================================
            let right_x = m + left_w + m;
            let right_w = (pw - right_x - m).max(200.0);
            let right_rect = Rect::new(right_x, top_y, right_w, main_h);

            ui.nui_card(right_rect, |ui| {
                let tabs_r = ui.take(Length::rem(2.6));
                ui.tabs(
                    200,
                    tabs_r,
                    &[
                        (ICON_TUNE, "NUI Components"),
                        (ICON_SETTINGS, "Audio Parameters"),
                        (ICON_SEARCH, "Typography A/B"),
                        (ICON_EQ, "Virtual List (10k)"),
                    ],
                    &mut self.active_tab,
                );
                ui.spacer(Length::rem(0.5));

                let body = ui.take_rest();

                match self.active_tab {
                0 => {
                    // ---- TAB 0: NUI Components — spacious 2-column `nui-layout` grid ----
                    let [col1, col2] =
                        ui.cols(body, Length::rem(1.5), [Length::fill(), Length::fill()]);
                    let mut sel1_r = Rect::new(0.0, 0.0, 0.0, 0.0);

                    ui.with_column(col1, Length::rem(1.0), |ui| {
                        // 1. nui-input-group + nui-input (standard 2.5rem control)
                        let inp = ui.input_group("nui-input (click to edit, drag to select)");
                        ui.text_input(
                            301,
                            inp,
                            "Type filter or track title...",
                            &mut self.search_text,
                        );

                        // 2. Segmented button container
                        let seg = ui.input_group("nui-button-container [variant=segmented]");
                        if ui.segmented(
                            310,
                            seg,
                            &["Tape Speed", "Pitch / Time", "MIDI Synth"],
                            &mut self.seg_pipeline,
                        ) {
                            params_changed = true;
                        }

                        // 3. Button variants (0.5rem gaps)
                        let blabel =
                            format!("nui-button variants (clicked: {})", self.click_counter);
                        let btns = ui.form_field(&blabel, Length::rem(2.25));
                        let b_gap = theme.space_half();
                        let bw4 = ((btns.w - b_gap * 3.0) / 4.0).floor();
                        let variants = [
                            (320u32, "Primary", ButtonVariant::Primary),
                            (321u32, "Outline", ButtonVariant::Outline),
                            (322u32, "Success", ButtonVariant::Success),
                            (323u32, "Danger", ButtonVariant::Danger),
                        ];
                        for (i, (bid, lab, var)) in variants.into_iter().enumerate() {
                            let br =
                                Rect::new(btns.x + (bw4 + b_gap) * i as f32, btns.y, bw4, btns.h);
                            if ui.button(bid, br, lab, "", var) {
                                self.click_counter += 1;
                            }
                        }

                        // 4. Checkbox + switch on a shared 2rem form row
                        ui.spacer(Length::rem(0.25));
                        let [chk_r, sw_r] = ui.take_cols(
                            Length::rem(2.0),
                            Length::rem(1.0),
                            [Length::fill(), Length::fill()],
                        );
                        ui.checkbox(330, chk_r, "Preserve Formants", &mut self.chk_formant);
                        if ui.switch(331, sw_r, "HQ Oversampling", &mut self.sw_hq_mode) {
                            params_changed = true;
                        }
                    });

                    // ---- Right column: `nui-select`, `nui-slider`, `nui-radio` ----
                    ui.with_column(col2, Length::rem(1.0), |ui| {
                        sel1_r = ui.input_group("nui-select (audio output device)");

                        let pitch_lbl = format!("{:+.1} st", self.pitch_semitones);
                        let pr = ui.take(Length::rem(2.9));
                        if ui.slider(
                            340,
                            pr,
                            "Pitch Shift (dbl-click reset)",
                            &mut self.pitch_semitones,
                            -12.0,
                            12.0,
                            0.0,
                            &pitch_lbl,
                        ) {
                            params_changed = true;
                        }

                        let tempo_lbl = format!("{:.2}x", self.tempo_ratio);
                        let tr = ui.take(Length::rem(2.9));
                        if ui.slider(
                            341,
                            tr,
                            "Tempo / Speed (wheel fine-tune)",
                            &mut self.tempo_ratio,
                            0.5,
                            2.0,
                            1.0,
                            &tempo_lbl,
                        ) {
                            params_changed = true;
                        }

                        let rr =
                            ui.form_field("Interpolation Filter (nui-radio)", Length::rem(2.0));
                        let r_w = (rr.w / 4.0).floor();
                        for (ri, rlabel) in
                            ["Nearest", "Linear", "Cubic", "Sinc 8-tap"].iter().enumerate()
                        {
                            if ui.radio(
                                350 + ri as u32,
                                Rect::new(rr.x + ri as f32 * r_w, rr.y, r_w, rr.h),
                                rlabel,
                                self.radio_interp == ri,
                            ) && self.radio_interp != ri
                            {
                                self.radio_interp = ri;
                                params_changed = true;
                            }
                        }
                    });

                    // Drawn after both columns so the open popup layers above every sibling.
                    ui.select(360, sel1_r, DEVICES, &mut self.selected_device);
                }
                1 => {
                    // ---- TAB 1: SoundApp Parameters — responsive `nui-layout` grid ----
                    let grid = ui.layout_grid(body, 2, Length::rem(1.5));
                    // `layout_grid` collapses to a single column below 30rem. Stack the two
                    // parameter groups vertically in that case instead of indexing [1].
                    let (col_a, col_b) = if grid.len() >= 2 {
                        (grid[0], grid[1])
                    } else {
                        let g = grid[0];
                        let [top, bottom] =
                            ui.rows(g, Length::rem(1.5), [Length::fill(), Length::fill()]);
                        (top, bottom)
                    };

                    ui.with_column(col_a, Length::rem(1.0), |ui| {
                        let pn = ui.input_group("Preset Name (nui-input)");
                        ui.text_input(401, pn, "Enter preset name...", &mut self.preset_name);

                        let sf = ui.input_group("MIDI SoundFont (.sf2)");
                        ui.select(430, sf, SOUNDFONTS, &mut self.selected_soundfont);

                        let sr = ui.input_group("Target Sample Rate");
                        ui.select(431, sr, SAMPLE_RATES, &mut self.selected_rate);
                    });

                    ui.with_column(col_b, Length::rem(1.0), |ui| {
                        let sep_str = format!("{:.0}%", self.stereo_sep);
                        let s1 = ui.take(Length::rem(2.9));
                        ui.slider(
                            410,
                            s1,
                            "Tracker Stereo Separation",
                            &mut self.stereo_sep,
                            0.0,
                            200.0,
                            100.0,
                            &sep_str,
                        );

                        let c1 = ui.take(Length::rem(2.0));
                        ui.checkbox(
                            411,
                            c1,
                            "Lock Parameters Across Track Changes",
                            &mut self.chk_locked,
                        );

                        let s2 = ui.take(Length::rem(2.0));
                        ui.switch(
                            412,
                            s2,
                            "Keep Running in System Tray (0% CPU Idle)",
                            &mut self.sw_keep_tray,
                        );

                        let [reset_r, save_r] =
                            ui.take_form_row(false, [Length::fill(), Length::fill()]);
                        if ui.button(420, reset_r, "Reset Defaults", "", ButtonVariant::Outline)
                        {
                            self.pitch_semitones = 0.0;
                            self.tempo_ratio = 1.0;
                            self.stereo_sep = 100.0;
                            self.master_vol = 0.78;
                        }
                        let _ = ui.button(421, save_r, "Save Preset", "", ButtonVariant::Primary);
                    });
                }
                2 => {
                    // ---- TAB 2: Side-by-Side Typography A/B at 12/15/18 px ----
                    let [abox, bbox] =
                        ui.rows(body, Length::rem(0.65), [Length::fill(), Length::fill()]);

                    ui.inset_card(
                        abox,
                        Length::rem(1.25),
                        Length::rem(0.65),
                        theme.highlight_dim,
                        |ui| {
                            let b = ui.bounds();
                            let tx = b.x.round();
                            let fs_head = theme.font_xsmall();
                            let mut sy = b.y + fs_head * 1.3;
                            ui.text_clipped(
                                FontFace::SemiBold,
                                FontEngine::Swash,
                                "A. SWASH — OpenType Kerning + Hinting + 4x Subpixel + Linear-Light Gamma",
                                tx,
                                sy.round(),
                                fs_head,
                                b.w,
                                theme.highlight,
                            );
                            for (lbl, css_px) in [("12px", 12.0f32), ("15px", 15.0), ("18px", 18.0)]
                            {
                                sy += rem * 1.5;
                                ui.text_clipped(
                                    FontFace::Regular,
                                    FontEngine::Swash,
                                    &format!("{lbl}: {}", typo_sample(css_px)),
                                    tx,
                                    sy.round(),
                                    rem * (css_px / 16.0),
                                    b.w,
                                    theme.text,
                                );
                            }
                        },
                    );

                    ui.inset_card(
                        bbox,
                        Length::rem(1.25),
                        Length::rem(0.65),
                        theme.border2,
                        |ui| {
                            let b = ui.bounds();
                            let tx = b.x.round();
                            let fs_head = theme.font_xsmall();
                            let mut fy = b.y + fs_head * 1.3;
                            ui.text_clipped(
                                FontFace::SemiBold,
                                FontEngine::Fontdue,
                                "B. FONTDUE (Legacy POC) — Unhinted + Integer-Snapped + Naive sRGB Alpha",
                                tx,
                                fy.round(),
                                fs_head,
                                b.w,
                                theme.text_dim,
                            );
                            for (lbl, css_px) in [("12px", 12.0f32), ("15px", 15.0), ("18px", 18.0)]
                            {
                                fy += rem * 1.5;
                                ui.text_clipped(
                                    FontFace::Regular,
                                    FontEngine::Fontdue,
                                    &format!("{lbl}: {}", typo_sample(css_px)),
                                    tx,
                                    fy.round(),
                                    rem * (css_px / 16.0),
                                    b.w,
                                    theme.text,
                                );
                            }
                        },
                    );
                }
                _ => {
                    // ---- TAB 3: header bar + full-width 10,000-track virtual list ----
                    // `body` is an already-consumed rect: split IT (never the card cursor,
                    // which take_rest exhausted) into header + list rows.
                    let [hdr_r, list_r] =
                        ui.rows(body, Length::rem(0.5), [Length::rem(2.4), Length::fill()]);
                    let badge_h = (rem * 1.45).round();
                    let bx = hdr_r.x;
                    let by = hdr_r.y + (hdr_r.h - badge_h) * 0.5;
                    // `ui.badge` returns the badge's width, NOT `bx + w` — anchor the
                    // description text at `bx + w + gap` so it never overlaps the badge.
                    let info_x = bx
                        + ui.badge(
                            bx,
                            by,
                            &format!("{} TRACKS", TRACK_COUNT),
                            BadgeVariant::Primary,
                            None,
                        )
                        + theme.space();
                    let fs_info = theme.font_small();
                    ui.text_clipped(
                        FontFace::Regular,
                        ui.font_engine,
                        "All Genres · no filter · sorted by Artist — click a row to load",
                        info_x.round(),
                        (hdr_r.y + hdr_r.h * 0.5 + fs_info * 0.33).round(),
                        fs_info,
                        (hdr_r.x + hdr_r.w - info_x).max(0.0),
                        theme.text_muted,
                    );

                    let row_h = (rem * 2.25).round();
                    ui.virtual_list(600, list_r, TRACK_COUNT, row_h, |ui, idx, rr| {
                        let row_id = 20_000u32.wrapping_add(idx as u32);
                        let hovered = ui.state.can_hover(row_id, rr);
                        if hovered {
                            ui.state.hovered_id = Some(row_id);
                            if ui.state.mouse_pressed {
                                ui.state.active_id = Some(row_id);
                            }
                        }
                        let active = ui.state.active_id == Some(row_id);
                        if hovered && ui.state.mouse_released && active {
                            self.selected_track = idx;
                            if idx < self.playlist.len() {
                                load_path = Some(self.playlist[idx].clone());
                            }
                        }
                        let selected = idx == self.selected_track;

                        if selected {
                            ui.scene.rect(rr.x, rr.y, rr.w, rr.h, theme.shade3);
                            ui.scene.rect(rr.x, rr.y, 3.0, rr.h, theme.highlight);
                        } else if hovered {
                            ui.scene.rect(rr.x, rr.y, rr.w, rr.h, theme.shade2);
                        }
                        ui.scene.rect(rr.x, rr.y + rr.h - 1.0, rr.w, 1.0, theme.border1);

                        // Generous fixed columns: #index | title | artist — album | tech meta.
                        let fs = theme.font_small();
                        let fs2 = theme.font_xsmall();
                        let baseline = (rr.y + rr.h * 0.5 + fs * 0.355).round();
                        let face = if selected {
                            FontFace::SemiBold
                        } else {
                            FontFace::Regular
                        };

                        let index_str = format!("#{:05}", idx + 1);
                        let idx_x = rr.x + rem * 0.75;
                        let title_x = rr.x + rem * 5.0;
                        let credit_x = rr.x + rr.w * 0.42;
                        // Real files head the playlist; everything past them is synthetic.
                        let (title, credit, meta) = if idx < self.playlist.len() {
                            let name = self.playlist[idx]
                                .file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_else(|| format!("track {}", idx + 1));
                            let meta = if idx == self.selected_track {
                                match &self.track_meta {
                                    Some(m) => format!(
                                        "{} · {} Hz · {} kbps · {}",
                                        m.codec,
                                        m.sample_rate,
                                        m.bitrate / 1000,
                                        fmt_meta_duration(m.duration)
                                    ),
                                    None => "FFmpeg · probing…".to_string(),
                                }
                            } else {
                                "FFmpeg · m4a".to_string()
                            };
                            (name, "Local File".to_string(), meta)
                        } else {
                            (track_name(idx), track_credit(idx), track_meta(idx))
                        };
                        let mw = ui
                            .text_sys
                            .measure(FontFace::Regular, ui.font_engine, &meta, fs2);
                        let meta_w = mw + rem * 0.75;
                        let meta_x = rr.x + rr.w - meta_w;

                        ui.text_clipped(
                            FontFace::Regular,
                            ui.font_engine,
                            &index_str,
                            idx_x.round(),
                            baseline,
                            fs,
                            (title_x - idx_x - rem * 0.4).max(0.0),
                            theme.text_muted,
                        );
                        ui.text_clipped(
                            face,
                            ui.font_engine,
                            &title,
                            title_x.round(),
                            baseline,
                            fs,
                            (credit_x - title_x - rem * 0.4).max(0.0),
                            if selected { theme.highlight } else { theme.text },
                        );
                        ui.text_clipped(
                            FontFace::Regular,
                            ui.font_engine,
                            &credit,
                            credit_x.round(),
                            baseline,
                            fs2,
                            (meta_x - credit_x - rem * 0.4).max(0.0),
                            theme.text_dim,
                        );
                        ui.text_clipped(
                            FontFace::Regular,
                            ui.font_engine,
                            &meta,
                            meta_x.round(),
                            baseline,
                            fs2,
                            meta_w.max(0.0),
                            theme.text_muted,
                        );
                    });
                }
                }
            });

            // ================================================================
            // 4. Bottom Transport Card (Phase 3.1): full-width waveform seekbar
            //    over Prev/Play/Next/Loop/Shuffle — elapsed left, remaining/total
            //    and a compact master volume slider right.
            // ================================================================
            let bot_y = ph - bot_h - m;
            ui.card(
                Rect::new(m, bot_y, pw - m * 2.0, bot_h),
                Length::rem(1.0),
                Length::rem(0.5),
                |ui| {
                    let inner = ui.bounds();
                    transport_intents = draw_transport(
                        ui,
                        inner,
                        Length::rem(1.85),
                        self.t,
                        &self.wave_peaks,
                        &mut self.scrub_frac,
                        &mut self.scrub_active,
                        pos,
                        dur,
                        playing,
                        &mut self.master_vol,
                        self.loop_enabled,
                        self.shuffle_enabled,
                    );
                },
            );
            } // studio-only layout (player card + gallery + transport)

            // Synthetic UI complexity stress test (--ops N)
            let iw = (pw as usize).max(1);
            let ih = (ph as usize).max(1);
            for i in 0..self.extra_ops {
                let x = ((i * 37) % iw) as f32;
                let y = ((i * 53) % ih) as f32;
                ui.scene.rect(
                    x,
                    y,
                    9.0 * scale,
                    7.0 * scale,
                    ((i % 200) as u8, 120, 160, 255),
                );
            }

            // Full-window `nui-dropzone` overlay (OS drag-and-drop). A dropped file
            // is loaded into the search field.
            if let Some(path) =
                ui.dropzone_overlay(Rect::new(0.0, 0.0, pw, ph), "Drop audio file to load")
            {
                self.search_text = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.to_string_lossy().into_owned());
                load_path = Some(path);
            }

            // Append any open dropdown popups onto the top of the draw list!
            ui.scene.finish_frame();
            ui.state.end_frame();
        }

        // --- Phase 2/3 transport + player intents, applied outside the render borrow
        if transport_intents.loop_toggled {
            self.loop_enabled = !self.loop_enabled;
        }
        if transport_intents.shuffle_toggled {
            self.shuffle_enabled = !self.shuffle_enabled;
        }
        if let Some(a) = &self.audio {
            if transport_intents.toggle_play {
                self.animate = a.toggle_play();
            }
            if transport_intents.prev || transport_intents.next {
                if !self.playlist.is_empty() {
                    let cur = self.selected_track.min(self.playlist.len() - 1);
                    let idx = if transport_intents.prev {
                        cur.saturating_sub(1)
                    } else {
                        (cur + 1).min(self.playlist.len() - 1)
                    };
                    self.selected_track = idx;
                    if !self.chk_locked {
                        self.pitch_semitones = 0.0;
                        self.tempo_ratio = 1.0;
                        self.stereo_sep = 100.0;
                    }
                    a.load_file(self.playlist[idx].clone());
                }
            }
            if let Some(secs) = transport_intents.seek_to {
                a.seek(secs);
            }
            if transport_intents.vol_changed {
                a.set_volume(self.master_vol);
            }
            if self.selected_soundfont != self.last_soundfont {
                self.last_soundfont = self.selected_soundfont;
                if let Some(p) = soundfont_path_for(self.selected_soundfont) {
                    a.set_soundfont(p);
                }
            }
        }
        if let Some(path) = load_path.take() {
            if !self.chk_locked {
                self.pitch_semitones = 0.0;
                self.tempo_ratio = 1.0;
                self.stereo_sep = 100.0;
            }
            if let Some(a) = &self.audio {
                a.load_file(path);
            }
        }
        if params_changed {
            self.sync_pipeline_and_params();
        }

        if toggle_backend_req {
            if let Some(r) = &mut self.renderer {
                r.toggle_backend();
            }
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
        if toggle_font_req {
            self.font_engine = match self.font_engine {
                FontEngine::Swash => FontEngine::Fontdue,
                FontEngine::Fontdue => FontEngine::Swash,
            };
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
        if toggle_theme_req {
            self.dark_mode = !self.dark_mode;
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
        if zoom_delta.abs() > 0.001 {
            self.set_zoom(self.ui_zoom + zoom_delta);
        }
        if close_req {
            self.quitting = true;
            self.quit_at = Some(Instant::now());
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
        if compact_req {
            self.compact_player_mode = !self.compact_player_mode;
            self.apply_player_mode();
        }
    }

    fn render(&mut self) {
        let Some(window) = self.window.clone() else {
            return;
        };
        let (w, h) = self.size;
        if w == 0 || h == 0 {
            return;
        }
        let scale = (window.scale_factor() as f32) * self.ui_zoom;

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
            let shot = self.screenshot_path.take();
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
                                    let _ = w.window.drag_window();
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
            let playing = self
                .audio
                .as_ref()
                .map(|a| a.is_playing())
                .unwrap_or(false);

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

                // The Parameters window's header badge follows its active tab.
                let (hdr_icon, hdr_title, hdr_badge) = if key == "parameters" {
                    (
                        ICON_TUNE,
                        "PARAMETERS",
                        match self.params_tab {
                            0 => "AUDIO",
                            1 => "MIDI",
                            _ => "TRACKER",
                        },
                    )
                } else {
                    (window_header_icon(key), spec.title, window_header_badge(key))
                };
                let (hdr_h, close) =
                    ui.secondary_window_header(pw, hdr_title, hdr_icon, hdr_badge);
                close_clicked = close;
                let m = Length::Rem(1.25).resolve(rem).round();

                if key == "parameters" {
                    // ========================================================
                    // Phase 3.2 — adaptive Parameters window: header badge follows
                    // the tab, tab bar above the card, footer bar (3.0rem) below.
                    // ========================================================
                    let gap = theme.space_half();
                    let tabs_h = Length::Rem(2.6).resolve(rem).round();
                    let footer_h = Length::Rem(3.0).resolve(rem).round();
                    let tabs_r = Rect::new(m, hdr_h + m, (pw - m * 2.0).max(0.0), tabs_h);
                    ui.tabs(
                        1100,
                        tabs_r,
                        &[
                            (ICON_EQ, "Audio"),
                            (ICON_TUNE, "MIDI"),
                            (ICON_SETTINGS, "Tracker"),
                        ],
                        &mut self.params_tab,
                    );

                    // Main parameters card — leaves 3.2rem at the bottom for the footer.
                    let card_y = tabs_r.y + tabs_h + gap;
                    let card_h = (ph - Length::Rem(3.2).resolve(rem) - card_y).max(80.0);
                    ui.nui_card(
                        Rect::new(m, card_y, (pw - m * 2.0).max(0.0), card_h),
                        |ui| {
                            match self.params_tab {
                                0 => {
                                    // ---- AUDIO tab: pipeline mode + speed/pitch/formants ----
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
                                1 => {
                                    // ---- MIDI tab: SoundFont / transpose / BPM / metronome ----
                                    let sf_r = ui.input_group("SoundFont (.sf2)");
                                    let tr_lbl = format!("{:+.0} st", self.midi_transpose);
                                    let tr = ui.take(Length::rem(2.7));
                                    if ui.slider(
                                        1121,
                                        tr,
                                        "Transpose",
                                        &mut self.midi_transpose,
                                        -12.0,
                                        12.0,
                                        0.0,
                                        &tr_lbl,
                                    ) {
                                        params_intents.params_changed = true;
                                    }
                                    let bpm_lbl = format!("{:.0} BPM", self.midi_bpm);
                                    let bp = ui.take(Length::rem(2.7));
                                    if ui.slider(
                                        1122,
                                        bp,
                                        "BPM Tempo",
                                        &mut self.midi_bpm,
                                        40.0,
                                        240.0,
                                        120.0,
                                        &bpm_lbl,
                                    ) {
                                        params_intents.params_changed = true;
                                    }
                                    let [sw_r, btn_r] = ui.take_cols(
                                        Length::rem(2.0),
                                        Length::rem(1.0),
                                        [Length::fill(), Length::rem(10.5)],
                                    );
                                    if ui.switch(
                                        1123,
                                        sw_r,
                                        "Metronome Click Track",
                                        &mut self.midi_metronome,
                                    ) {
                                        params_intents.params_changed = true;
                                    }
                                    if ui.button(
                                        1124,
                                        btn_r,
                                        "Load GM MIDI Demo",
                                        ICON_PLAY,
                                        ButtonVariant::Primary,
                                    ) {
                                        params_intents.load_midi = true;
                                    }
                                    // Drawn last so an open dropdown layers above the tab body.
                                    ui.select(1120, sf_r, SOUNDFONTS, &mut self.selected_soundfont);
                                }
                                _ => {
                                    // ---- TRACKER tab: separation / pitch / tempo / interp ----
                                    let sep_lbl = format!("{:.0}%", self.stereo_sep);
                                    let sp = ui.take(Length::rem(2.5));
                                    if ui.slider(
                                        1130,
                                        sp,
                                        "Stereo Separation",
                                        &mut self.stereo_sep,
                                        0.0,
                                        200.0,
                                        100.0,
                                        &sep_lbl,
                                    ) {
                                        params_intents.params_changed = true;
                                    }
                                    let pitch_lbl = format!("{:+.1} st", self.pitch_semitones);
                                    let pt = ui.take(Length::rem(2.5));
                                    if ui.slider(
                                        1131,
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
                                    let tempo_lbl = format!("{:.2}x", self.tempo_ratio);
                                    let tp = ui.take(Length::rem(2.5));
                                    if ui.slider(
                                        1132,
                                        tp,
                                        "Tempo",
                                        &mut self.tempo_ratio,
                                        0.5,
                                        2.0,
                                        1.0,
                                        &tempo_lbl,
                                    ) {
                                        params_intents.params_changed = true;
                                    }
                                    // Interpolation filter (widget ids 1133..1137) with
                                    // the demo loader sharing the control row.
                                    let ir = ui.form_field("Interpolation", Length::rem(2.0));
                                    let btn_w = Length::Rem(8.0).resolve(rem).round();
                                    let rad_w =
                                        ((ir.w - btn_w - theme.space_half() * 4.0) / 4.0).floor();
                                    for (i, lbl) in
                                        ["Nearest", "Linear", "Cubic", "Sinc 8-tap"]
                                            .iter()
                                            .enumerate()
                                    {
                                        if ui.radio(
                                            1133 + i as u32,
                                            Rect::new(
                                                ir.x + (rad_w + theme.space_half()) * i as f32,
                                                ir.y,
                                                rad_w,
                                                ir.h,
                                            ),
                                            lbl,
                                            self.radio_interp == i,
                                        ) && self.radio_interp != i
                                        {
                                            self.radio_interp = i;
                                            params_intents.params_changed = true;
                                        }
                                    }
                                    if ui.button(
                                        1138,
                                        Rect::new(
                                            ir.x + ir.w - btn_w,
                                            ir.y,
                                            btn_w,
                                            ir.h,
                                        ),
                                        "Load Tracker Demo",
                                        ICON_PLAY,
                                        ButtonVariant::Primary,
                                    ) {
                                        params_intents.load_tracker = true;
                                    }
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
                    ui.nui_card(body, |ui| {
                        match key {
                            "monitoring" => {
                            ui.heading("Real-Time DSP Monitoring");
                            ui.kv_row(
                                "Tap:",
                                "Post-Master · 48 kHz · 32-band spectrum",
                                Length::rem(4.0),
                            );
                            let panel = ui.take_rest();
                            ui.scene.rounded_rect(
                                panel.x,
                                panel.y,
                                panel.w,
                                panel.h,
                                theme.radius2(),
                                theme.base,
                                theme.border2,
                                1.0,
                            );
                            ui.scene.push_clip(
                                panel.x + 1.0,
                                panel.y + 1.0,
                                (panel.w - 2.0).max(0.0),
                                (panel.h - 2.0).max(0.0),
                            );
                            // Upper half: 32-band realtime spectrum.
                            let bands = 32usize;
                            let inset = (rem * 0.4).round();
                            let g = (rem * 0.3).round().max(2.0);
                            let spec_h = panel.h * 0.52;
                            let bar_w = ((panel.w - inset * 2.0 - g * (bands as f32 - 1.0))
                                / bands as f32)
                                .floor()
                                .max(2.0);
                            for i in 0..bands {
                                let phase = self.t * 5.8 + i as f32 * 0.42;
                                let amp = if playing {
                                    ((phase.sin() * 0.35
                                        + (phase * 1.7).cos() * 0.25
                                        + 0.45)
                                        * self.master_vol)
                                        .clamp(0.06, 0.94)
                                } else {
                                    (0.14 + 0.08 * (i as f32 * 0.5).sin()) * self.master_vol
                                };
                                let bh = ((spec_h - rem * 0.6) * amp).max(2.0);
                                let bx = panel.x + inset + i as f32 * (bar_w + g);
                                let by = panel.y + spec_h - rem * 0.4 - bh;
                                ui.scene.rounded_rect(
                                    bx,
                                    by,
                                    bar_w,
                                    bh,
                                    2.0,
                                    theme.highlight,
                                    (0, 0, 0, 0),
                                    0.0,
                                );
                            }
                            // Lower half: oscilloscope curve (`kind = 4` polyline).
                            let scope = Rect::new(
                                panel.x + inset,
                                panel.y + spec_h,
                                (panel.w - inset * 2.0).max(0.0),
                                (panel.h - spec_h - inset).max(0.0),
                            );
                            let pts: Vec<(f32, f32)> = (0..=96)
                                .map(|i| {
                                    let nx = i as f32 / 96.0;
                                    let phase = self.t * 6.0 + nx * std::f32::consts::TAU * 2.0;
                                    let amp = 0.42 * phase.sin()
                                        + 0.20 * (phase * 2.3).cos()
                                        + 0.12 * (phase * 4.1).sin();
                                    let ny =
                                        (0.5 + amp * self.master_vol.max(0.2)).clamp(0.05, 0.95);
                                    (nx, ny)
                                })
                                .collect();
                            ui.polyline(scope, &pts, 2.0, theme.palette_mark);
                            ui.scene.pop_clip();
                        }
                        "mixer" => {
                            ui.heading("Multi-Track Stem Mixer");
                            ui.kv_row(
                                "Session:",
                                "4 stems · 48 kHz · synchronized preview",
                                Length::rem(4.0),
                            );
                            for i in 0..4usize {
                                let stem = TRACK_ARTISTS[i % TRACK_ARTISTS.len()];
                                let gain_lbl = format!("{:.0}%", self.mixer_gains[i] * 100.0);
                                let [sld_r, mute_r] = ui.take_cols(
                                    Length::rem(2.4),
                                    Length::rem(1.0),
                                    [Length::fill(), Length::rem(9.0)],
                                );
                                ui.slider(
                                    720 + i as u32,
                                    sld_r,
                                    stem,
                                    &mut self.mixer_gains[i],
                                    0.0,
                                    1.0,
                                    0.8,
                                    &gain_lbl,
                                );
                                ui.checkbox(
                                    730 + i as u32,
                                    mute_r,
                                    "Mute",
                                    &mut self.mixer_mutes[i],
                                );
                            }
                            let [apply_r] = ui.take_form_row(false, [Length::fill()]);
                            let _ = ui.button(
                                740,
                                apply_r,
                                "Apply Mix Downstem",
                                "",
                                ButtonVariant::Primary,
                            );
                        }
                        "settings" => {
                            ui.heading("Settings");
                            let sel_rate = ui.input_group("Default Sample Rate");
                            ui.select(750, sel_rate, SAMPLE_RATES, &mut self.selected_rate);
                            let sel_sf = ui.input_group("MIDI SoundFont (.sf2)");
                            ui.select(751, sel_sf, SOUNDFONTS, &mut self.selected_soundfont);
                            let [c1, c2] = ui.take_cols(
                                Length::rem(2.0),
                                Length::rem(1.0),
                                [Length::fill(), Length::fill()],
                            );
                            ui.checkbox(
                                752,
                                c1,
                                "Keep Running in System Tray",
                                &mut self.sw_keep_tray,
                            );
                            ui.checkbox(753, c2, "HQ Oversampling", &mut self.sw_hq_mode);
                            let [apply_r] = ui.take_form_row(false, [Length::fill()]);
                            let _ =
                                ui.button(754, apply_r, "Apply & Save", "", ButtonVariant::Primary);
                        }
                        _ => {
                            ui.heading("Keyboard Shortcuts & Help");
                            for (k, v) in [
                                (
                                    "P / N / M / S / H",
                                    "Pop open or hide Parameters / Monitoring / Mixer / Settings / this window",
                                ),
                                (
                                    "Drag header",
                                    "Move this undecorated window; the red button (or Esc) hides it",
                                ),
                                ("Space", "Play / Pause transport"),
                                (
                                    "B / F / T",
                                    "Toggle GPU-CPU backend / Swash-Fontdue engine / dark-light theme",
                                ),
                                ("Ctrl +  / - / 0", "UI zoom in / out / reset"),
                                ("Esc (main window)", "Quit SoundApp"),
                            ] {
                                ui.kv_row(k, v, Length::rem(7.5));
                            }
                        }
                    }
                });
                } // non-parameters secondary windows

                ui.scene.finish_frame();
                ui.state.end_frame();
            }
            (w, h, close_clicked, shot)
        };

        // --- Phase 3.2 Parameters-window intents: forward every change to the
        // engine immediately (same frame), then refresh the main window's
        // speed/pitch overlay badge.
        if params_intents.reset_all {
            self.pitch_semitones = 0.0;
            self.tempo_ratio = 1.0;
            self.midi_transpose = 0.0;
            self.midi_bpm = 120.0;
            self.stereo_sep = 100.0;
            self.sync_pipeline_and_params();
        }
        if params_intents.load_midi {
            self.params_tab = 1;
            if let Some(a) = &self.audio {
                a.load_midi_demo();
            }
        }
        if params_intents.load_tracker {
            self.params_tab = 2;
            if let Some(a) = &self.audio {
                a.load_tracker_demo();
            }
        }
        if params_intents.params_changed || params_intents.pipeline_changed {
            self.sync_pipeline_and_params();
        }
        if self.selected_soundfont != self.last_soundfont {
            self.last_soundfont = self.selected_soundfont;
            if let Some(a) = &self.audio {
                if let Some(p) = soundfont_path_for(self.selected_soundfont) {
                    a.set_soundfont(p);
                }
            }
        }
        if params_intents.params_changed
            || params_intents.pipeline_changed
            || params_intents.reset_all
            || params_intents.load_midi
            || params_intents.load_tracker
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
    format!("{:02}:{:02}", s / 60, s % 60)
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let mut attrs = Window::default_attributes()
            .with_title("SoundApp POC — NUI Rust")
            .with_inner_size(winit::dpi::LogicalSize::new(1120.0, 700.0))
            .with_min_inner_size(winit::dpi::LogicalSize::new(760.0, 480.0))
            .with_decorations(false);

        #[cfg(target_os = "windows")]
        {
            use winit::platform::windows::{Color, CornerPreference, WindowAttributesExtWindows};
            attrs = attrs
                .with_undecorated_shadow(true)
                .with_corner_preference(CornerPreference::Round)
                .with_border_color(Some(Color::from_rgb(55, 55, 55)));
        }

        let (window, renderer) = Renderer::new(event_loop, attrs, self.prefer_backend);
        let sz = window.inner_size();
        self.size = (sz.width, sz.height);
        self.window = Some(window.clone());
        self.renderer = Some(renderer);
        window.request_redraw();

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
                        self.ui_state.wheel_y += dy;
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
                            let scale = self
                                .window
                                .as_ref()
                                .map(|w| w.scale_factor() as f32 * self.ui_zoom)
                                .unwrap_or(1.0);
                            let rem = (16.0 * scale).round();
                            let hdr_h = (rem * 3.25).round();
                            // Drag window if clicking the empty header area (left of right-side buttons).
                            // Matches the header's right-zone width per player mode.
                            let right_btns_x = self.size.0 as f32
                                - rem * if self.compact_player_mode { 10.7 } else { 26.5 };
                            if self.ui_state.cursor.1 < hdr_h && self.ui_state.cursor.0 < right_btns_x {
                                if let Some(w) = &self.window {
                                    let _ = w.drag_window();
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
                            self.master_vol = (self.master_vol + 0.05).clamp(0.0, 1.0);
                            if let Some(a) = &self.audio {
                                a.set_volume(self.master_vol);
                            }
                        }
                        Key::Named(NamedKey::ArrowDown) if !focused_text => {
                            self.master_vol = (self.master_vol - 0.05).clamp(0.0, 1.0);
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
                        }
                        Key::Character(ch) if !focused_text && ch.eq_ignore_ascii_case("c") => {
                            self.compact_player_mode = !self.compact_player_mode;
                            self.apply_player_mode();
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

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(q) = self.quit_at {
            if Instant::now() >= q {
                if !self.quitting {
                    self.quitting = true;
                    println!("[poc] auto-quit reached");
                }
                event_loop.exit();
                return;
            }
        }
        let playing = self
            .audio
            .as_ref()
            .map(|a| a.is_playing())
            .unwrap_or(false);
        let animating = playing && self.animate;

        let now = Instant::now();
        if animating && now >= self.next_frame {
            self.next_frame = now + Duration::from_millis(self.frame_ms);
            self.t += self.frame_ms as f32 / 1000.0;
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
}

fn main() {
    let mut file: Option<String> = None;
    let mut tone = false;
    let mut silence = false;
    let mut midi_demo = false;
    let mut tracker_demo = false;
    let mut animate = true;
    let mut nopaint = false;
    let mut extra_ops: usize = 0;
    let mut seconds: Option<f64> = None;
    let mut fps: f64 = 60.0;
    let mut backend = BackendKind::Gpu;
    let mut font_engine = FontEngine::Swash;
    let mut app_screenshot: Option<String> = None;
    let mut open_select_demo = false;
    let mut initial_tab: usize = 0;
    let mut drop_preview = false;
    let mut focus_preview = false;
    let mut open_window: Option<String> = None;

    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--tone" => tone = true,
            "--silence" => silence = true,
            "--midi-demo" => midi_demo = true,
            "--tracker-demo" => tracker_demo = true,
            "--static" => animate = false,
            "--nopaint" => nopaint = true,
            "--open-select" => open_select_demo = true,
            "--drop-preview" => drop_preview = true,
            "--focus-preview" => focus_preview = true,
            "--tab" => initial_tab = args.next().and_then(|v| v.parse().ok()).unwrap_or(0),
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
            "--ops" => extra_ops = args.next().and_then(|v| v.parse().ok()).unwrap_or(0),
            "--seconds" => seconds = args.next().and_then(|v| v.parse().ok()),
            "--fps" => fps = args.next().and_then(|v| v.parse().ok()).unwrap_or(60.0),
            "--screenshot" => app_screenshot = args.next(),
            other => file = Some(other.to_string()),
        }
    }
    let source = if tone {
        Source::Tone { hz: 440.0 }
    } else if silence {
        Source::Silence
    } else if midi_demo {
        Source::MidiDemo
    } else if tracker_demo {
        Source::TrackerDemo
    } else if let Some(p) = file {
        let pb = PathBuf::from(p);
        let ext = pb
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase());
        match ext.as_deref() {
            Some("mid") | Some("midi") => Source::MidiFile(pb),
            Some("mod") | Some("xm") | Some("s3m") | Some("it") => Source::TrackerFile(pb),
            _ => Source::File(pb),
        }
    } else {
        // No file given: start the engine on silence so the UI can still load
        // tracks from the playlist / dropzone and drive the transport.
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
    let mut app = App::new(audio, animate, quit_at, backend, font_engine);
    // Push the initial UI state into the engine (gain + the full parameter set).
    let initial_params = app.playback_params();
    let initial_vol = app.master_vol;
    if let Some(a) = &app.audio {
        a.set_volume(initial_vol);
        a.set_params(initial_params);
    }
    if fps > 0.0 {
        app.frame_ms = (1000.0 / fps).round().max(1.0) as u64;
    }
    app.nopaint = nopaint;
    app.extra_ops = extra_ops;
    app.screenshot_path = app_screenshot;
    app.active_tab = initial_tab.min(3);
    // Phase 1.5: pop a secondary window at startup. With `--screenshot` alongside it,
    // the capture is taken from THAT window (handled in `resumed`).
    app.open_window_spec = open_window.as_deref().and_then(window_spec_by_key);
    if let Some(k) = open_window.as_deref() {
        if app.open_window_spec.is_none() {
            eprintln!("[poc] --open-window: unknown key '{k}' (parameters|monitoring|mixer|settings|help)");
        }
    }
    if open_select_demo {
        app.ui_state.open_popup_id = Some(360);
    }
    if drop_preview {
        app.ui_state.hovered_file =
            Some(std::path::PathBuf::from("D:\\Work\\_GIT\\SoundApp\\_Material\\test_track.flac"));
    }
    if focus_preview {
        app.ui_state.focused_id = Some(301);
        app.ui_state.focus_visible = true;
    }
    event_loop.run_app(&mut app).unwrap();
}
