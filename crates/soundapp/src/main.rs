// SoundApp & NUI Rust Showcase — Dual-Backend Renderer (GPU Instanced SDF + CPU Softbuffer)
// with nui_wc2 v3.0.0 "slate" theme & components and Swash browser-grade typography.

mod audio;

use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

use audio::{Audio, Source};
use nui::{
    BackendKind, BadgeVariant, ButtonVariant, FontEngine, FontFace, KeyCmd, Length, Rect, Renderer,
    Scene, TextSystem, Theme, Ui, UiState, ICON_CLOSE, ICON_DARK_MODE, ICON_EQ, ICON_LIGHT_MODE,
    ICON_NEXT, ICON_PAUSE, ICON_PLAY, ICON_PREV, ICON_SEARCH, ICON_SETTINGS, ICON_TUNE,
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

    // Interactive NUI component states
    active_tab: usize,
    seg_pipeline: usize,
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
    /// Deterministic 96-bar synthetic waveform for the transport seekbar.
    wave_peaks: Vec<f32>,
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

/// Deterministic synthetic audio waveform peaks (stable across frames so the
/// interactive seekbar does not jitter while scrubbing).
fn synth_wave_peaks(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| {
            let x = i as f32 / (n - 1).max(1) as f32;
            let envelope = (x * std::f32::consts::PI).sin();
            let detail = (x * 37.0).sin() * 0.35 + (x * 91.0).cos() * 0.22;
            (envelope * (0.55 + detail)).abs().clamp(0.05, 1.0)
        })
        .collect()
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

            active_tab: 0,
            seg_pipeline: 1,
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
            selected_track: 2,
            wave_peaks: synth_wave_peaks(96),
        }
    }

    fn set_zoom(&mut self, new_zoom: f32) {
        let clamped = ((new_zoom * 20.0).round() / 20.0).clamp(0.75, 2.0);
        if (clamped - self.ui_zoom).abs() < 0.001 {
            return;
        }
        self.ui_zoom = clamped;
        if let Some(w) = &self.window {
            let _ = w.request_inner_size(winit::dpi::LogicalSize::new(
                1120.0 * self.ui_zoom as f64,
                700.0 * self.ui_zoom as f64,
            ));
            self.size = (w.inner_size().width, w.inner_size().height);
            w.request_redraw();
        }
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
        let playing = self
            .audio
            .as_ref()
            .map(|a| a.started.load(std::sync::atomic::Ordering::Relaxed))
            .unwrap_or(false);

        let mut toggle_backend_req = false;
        let mut toggle_font_req = false;
        let mut toggle_theme_req = false;
        let mut zoom_delta: f32 = 0.0;
        let mut close_req = false;

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

            // Clip header left zone (logo, title, badges) so it never collides with right-side buttons
            let right_zone_w = (rem * 20.8).round();
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

            // Header right controls (Backend, Font, Theme, Zoom -, Zoom +, Close)
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
            rx -= z_w + rem * 0.3;
            if ui.button(91, Rect::new(rx, btn_y, z_w, btn_h), "+", "", ButtonVariant::Outline) {
                zoom_delta = 0.1;
            }
            rx -= z_w + rem * 0.25;
            if ui.button(92, Rect::new(rx, btn_y, z_w, btn_h), "-", "", ButtonVariant::Outline) {
                zoom_delta = -0.1;
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

            // ================================================================
            // 2. Main Layout Grid (Left Player/Analyzer Card + Right NUI Tabs Card)
            //    nui_wc2 proportions: 1.25rem window padding + inter-card gap.
            // ================================================================
            let m = Length::Rem(1.25).resolve(rem).round();
            let bot_h = Length::Rem(5.6).resolve(rem).round();
            let top_y = hdr_h + m;
            let main_h = (ph - top_y - bot_h - m * 2.0).max(120.0);
            let left_w = Length::Rem(20.0).resolve(rem).round();

            // Left Card — standard `nui-card` (elevation shadow, 1.25rem pad, 1rem gap):
            // heading, status rows, master slider, then the spectrum filling the rest.
            let left_rect = Rect::new(m, top_y, left_w, main_h);
            ui.nui_card(left_rect, |ui| {
                ui.heading("Realtime Audio & VU");
                let pipeline = if self.seg_pipeline == 0 {
                    "Normal (Tape)"
                } else {
                    "RubberBand"
                };
                ui.kv_row("Pipeline:", pipeline, Length::rem(6.4));
                let pitch_speed =
                    format!("{:+.1} st · {:.2}x", self.pitch_semitones, self.tempo_ratio);
                ui.kv_row("Pitch / Speed:", &pitch_speed, Length::rem(6.4));
                let scale_dpi = format!("{:.0}% ({:.2}x)", self.ui_zoom * 100.0, scale);
                ui.kv_row("Scale / DPI:", &scale_dpi, Length::rem(6.4));

                let vol_str = format!("{:.0}%", self.master_vol * 100.0);
                let slider_r = ui.take(Length::rem(2.85));
                ui.slider(
                    101,
                    slider_r,
                    "Master Gain",
                    &mut self.master_vol,
                    0.0,
                    1.0,
                    0.78,
                    &vol_str,
                );

                // Remaining height: one spacious 18-band realtime spectrum analyzer
                // (the 10k-track virtual list lives on its own 4th tab).
                let spec_r = ui.take_rest();
                ui.scene.rounded_rect(
                    spec_r.x,
                    spec_r.y,
                    spec_r.w,
                    spec_r.h,
                    theme.radius2(),
                    theme.base,
                    theme.border2,
                    1.0,
                );
                ui.scene.push_clip(
                    spec_r.x + 1.0,
                    spec_r.y + 1.0,
                    (spec_r.w - 2.0).max(0.0),
                    (spec_r.h - 2.0).max(0.0),
                );
                let bands = 18usize;
                let inset = (rem * 0.4).round();
                let g = (rem * 0.3).round().max(2.0);
                let bar_w = ((spec_r.w - inset * 2.0 - g * (bands as f32 - 1.0)) / bands as f32)
                    .floor()
                    .max(2.0);
                for i in 0..bands {
                    let phase = self.t * 5.8 + i as f32 * 0.42;
                    let amp = if playing {
                        ((phase.sin() * 0.35 + (phase * 1.7).cos() * 0.25 + 0.45)
                            * self.master_vol)
                            .clamp(0.06, 0.94)
                    } else {
                        (0.14 + 0.08 * (i as f32 * 0.5).sin()) * self.master_vol
                    };
                    let bh = ((spec_r.h - rem * 1.0) * amp).max(2.0);
                    let bx = spec_r.x + inset + i as f32 * (bar_w + g);
                    let by = spec_r.y + spec_r.h - rem * 0.5 - bh;
                    ui.scene
                        .rounded_rect(bx, by, bar_w, bh, 2.0, theme.highlight, (0, 0, 0, 0), 0.0);
                }

                // Realtime stereo oscilloscope / harmonic curve drawn ON TOP of the
                // spectrum bars — exercises `Scene::line` (kind = 4 anti-aliased
                // capsule segments) alongside the kind = 0 spectrum rectangles.
                let osc_pts: Vec<(f32, f32)> = (0..=64)
                    .map(|i| {
                        let nx = i as f32 / 64.0;
                        let phase = self.t * 6.0 + nx * std::f32::consts::TAU * 2.0;
                        let amp = 0.42 * phase.sin()
                            + 0.20 * (phase * 2.3).cos()
                            + 0.12 * (phase * 4.1).sin();
                        let ny = (0.5 + amp * self.master_vol.max(0.2)).clamp(0.05, 0.95);
                        (nx, ny)
                    })
                    .collect();
                ui.polyline(spec_r, &osc_pts, 2.0, theme.palette_mark);
                ui.scene.pop_clip();
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
                        ui.segmented(
                            310,
                            seg,
                            &["Tape Speed", "Pitch / Time", "MIDI Synth"],
                            &mut self.seg_pipeline,
                        );

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
                        ui.switch(331, sw_r, "HQ Oversampling", &mut self.sw_hq_mode);
                    });

                    // ---- Right column: `nui-select`, `nui-slider`, `nui-radio` ----
                    ui.with_column(col2, Length::rem(1.0), |ui| {
                        sel1_r = ui.input_group("nui-select (audio output device)");

                        let pitch_lbl = format!("{:+.1} st", self.pitch_semitones);
                        let pr = ui.take(Length::rem(2.9));
                        ui.slider(
                            340,
                            pr,
                            "Pitch Shift (dbl-click reset)",
                            &mut self.pitch_semitones,
                            -12.0,
                            12.0,
                            0.0,
                            &pitch_lbl,
                        );

                        let tempo_lbl = format!("{:.2}x", self.tempo_ratio);
                        let tr = ui.take(Length::rem(2.9));
                        ui.slider(
                            341,
                            tr,
                            "Tempo / Speed (wheel fine-tune)",
                            &mut self.tempo_ratio,
                            0.5,
                            2.0,
                            1.0,
                            &tempo_lbl,
                        );

                        let rr =
                            ui.form_field("Interpolation Filter (nui-radio)", Length::rem(2.0));
                        let r_w = (rr.w / 3.0).floor();
                        for (ri, rlabel) in ["Linear", "Cubic", "Sinc 8-tap"].iter().enumerate() {
                            if ui.radio(
                                350 + ri as u32,
                                Rect::new(rr.x + ri as f32 * r_w, rr.y, r_w, rr.h),
                                rlabel,
                                self.radio_interp == ri,
                            ) {
                                self.radio_interp = ri;
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
                    let info_x = ui.badge(
                        bx,
                        by,
                        &format!("{} TRACKS", TRACK_COUNT),
                        BadgeVariant::Primary,
                        None,
                    ) + theme.space();
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
                        let meta = track_meta(idx);
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
                            &track_name(idx),
                            title_x.round(),
                            baseline,
                            fs,
                            (credit_x - title_x - rem * 0.4).max(0.0),
                            if selected { theme.highlight } else { theme.text },
                        );
                        ui.text_clipped(
                            FontFace::Regular,
                            ui.font_engine,
                            &track_credit(idx),
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
            // 4. Bottom Transport Card (4.8rem `nui-card`, 1.0rem pad):
            //    full-width seekbar on top, transport row below.
            // ================================================================
            let bot_y = ph - bot_h - m;
            ui.card(
                Rect::new(m, bot_y, pw - m * 2.0, bot_h),
                Length::rem(1.0),
                Length::rem(0.5),
                |ui| {
                    // Interactive waveform seekbar (`ui.waveform`: 96 peaked bars,
                    // hover highlight, crisp kind = 4 white playhead, click/drag scrub).
                    let (pos, dur) = self
                        .audio
                        .as_ref()
                        .map(|a| (a.position_secs(), a.duration_secs()))
                        .unwrap_or((0.0, 0.0));
                    let is_finite = dur > 0.0 && dur < 360_000.0;
                    if ui.state.active_id != Some(500) {
                        if is_finite {
                            self.scrub_frac = (pos / dur).clamp(0.0, 1.0) as f32;
                        } else if playing {
                            self.scrub_frac = (self.t * 0.12).fract();
                        }
                    }
                    let wave = ui.take(Length::Rem(1.85));
                    let _ = ui.waveform(500, wave, &self.wave_peaks, &mut self.scrub_frac);

                    // Transport row: elapsed | Prev · Play/Pause · Next | remaining.
                    let ctrl = ui.take_rest();
                    let ctrl_cy = ctrl.y + ctrl.h * 0.5;
                    let fs = theme.font_small();
                    ui.scene.text(
                        ui.text_sys,
                        FontFace::SemiBold,
                        ui.font_engine,
                        &fmt_time(pos),
                        ctrl.x.round(),
                        (ctrl_cy + fs * 0.34).round(),
                        fs,
                        theme.text,
                    );

                    let tbtn_w = (rem * 2.4).round();
                    let tbtn_h = (rem * 1.85).round();
                    let tbtn_y = (ctrl_cy - tbtn_h * 0.5).round();
                    let cx = ctrl.x + ctrl.w * 0.5;
                    let _ = ui.button(
                        501,
                        Rect::new(cx - tbtn_w * 1.6, tbtn_y, tbtn_w, tbtn_h),
                        "",
                        ICON_PREV,
                        ButtonVariant::Ghost,
                    );
                    if ui.button(
                        502,
                        Rect::new(cx - tbtn_w * 0.5, tbtn_y, tbtn_w, tbtn_h),
                        "",
                        if playing { ICON_PAUSE } else { ICON_PLAY },
                        ButtonVariant::Primary,
                    ) {
                        self.animate = !self.animate;
                    }
                    let _ = ui.button(
                        503,
                        Rect::new(cx + tbtn_w * 0.6, tbtn_y, tbtn_w, tbtn_h),
                        "",
                        ICON_NEXT,
                        ButtonVariant::Ghost,
                    );

                    let remain = if is_finite {
                        fmt_time((dur - pos).max(0.0))
                    } else if playing {
                        "LIVE".to_string()
                    } else {
                        "03:42".to_string()
                    };
                    let rw_time = ui.text_sys.measure(FontFace::SemiBold, ui.font_engine, &remain, fs);
                    ui.scene.text(
                        ui.text_sys,
                        FontFace::SemiBold,
                        ui.font_engine,
                        &remain,
                        (ctrl.x + ctrl.w - rw_time).round(),
                        (ctrl_cy + fs * 0.34).round(),
                        fs,
                        theme.text,
                    );
                },
            );

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
            }

            // Append any open dropdown popups onto the top of the draw list!
            ui.scene.finish_frame();
            ui.state.end_frame();
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
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
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
                    .map(|a| a.started.load(std::sync::atomic::Ordering::Relaxed))
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
                            // Drag window if clicking the empty header area (left of right-side buttons)
                            let right_btns_x = self.size.0 as f32 - rem * 21.0;
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
                            self.ui_state.key_cmds.push(KeyCmd::Left);
                        }
                        Key::Named(NamedKey::ArrowRight) => {
                            self.ui_state.key_cmds.push(KeyCmd::Right);
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
            .map(|a| a.started.load(std::sync::atomic::Ordering::Relaxed))
            .unwrap_or(false);
        let animating = playing && self.animate;

        let now = Instant::now();
        if animating && now >= self.next_frame {
            self.next_frame = now + Duration::from_millis(self.frame_ms);
            self.t += self.frame_ms as f32 / 1000.0;
            if let Some(w) = &self.window {
                w.request_redraw();
            }
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

    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--tone" => tone = true,
            "--silence" => silence = true,
            "--static" => animate = false,
            "--nopaint" => nopaint = true,
            "--open-select" => open_select_demo = true,
            "--drop-preview" => drop_preview = true,
            "--focus-preview" => focus_preview = true,
            "--tab" => initial_tab = args.next().and_then(|v| v.parse().ok()).unwrap_or(0),
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
        Some(Source::Tone { hz: 440.0 })
    } else if silence {
        Some(Source::Silence)
    } else {
        file.map(|p| Source::File(std::path::PathBuf::from(p)))
    };

    let audio = match source {
        None => None,
        Some(src) => match audio::start(src) {
            Ok(a) => Some(a),
            Err(e) => {
                eprintln!("[poc] audio init failed: {e}");
                None
            }
        },
    };

    let event_loop = EventLoop::new().unwrap();
    event_loop.set_control_flow(ControlFlow::Wait);
    let quit_at = seconds.map(|s| Instant::now() + Duration::from_secs_f64(s));
    let mut app = App::new(audio, animate, quit_at, backend, font_engine);
    if fps > 0.0 {
        app.frame_ms = (1000.0 / fps).round().max(1.0) as u64;
    }
    app.nopaint = nopaint;
    app.extra_ops = extra_ops;
    app.screenshot_path = app_screenshot;
    app.active_tab = initial_tab.min(3);
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
