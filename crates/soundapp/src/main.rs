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
    BackendKind, BadgeVariant, ButtonVariant, FontEngine, FontFace, KeyCmd, Rect, Renderer, Scene,
    TextSystem, Theme, Ui, UiState, ICON_CLOSE, ICON_DARK_MODE, ICON_EQ, ICON_LIGHT_MODE,
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
            size: (1260, 720),
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
                860.0 * self.ui_zoom as f64,
                490.0 * self.ui_zoom as f64,
            ));
            self.size = (w.inner_size().width, w.inner_size().height);
            w.request_redraw();
        }
    }

    fn build_scene(&mut self, w: u32, h: u32, scale: f32) {
        let _ = self.text_sys.prepare_frame();
        let rem = (14.5 * scale).round();
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
            let mut ui = Ui {
                state: &mut self.ui_state,
                scene: &mut self.scene,
                text_sys: &mut self.text_sys,
                font_engine: self.font_engine,
                theme,
            };

            // ================================================================
            // 1. NUI App Header (`nui-app-header`)
            // ================================================================
            let hdr_h = (rem * 2.55).round();
            ui.scene.rect(0.0, 0.0, pw, hdr_h, theme.shade2);
            ui.scene
                .rect(0.0, hdr_h - 1.0, pw, 1.0, theme.border2);

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
            // ================================================================
            let m = (rem * 0.85).round();
            let pad = (rem * 0.95).round();
            let bot_h = (rem * 4.35).round();
            let top_y = hdr_h + m;
            let main_h = (ph - top_y - bot_h - m * 2.0).max(120.0);
            let left_w = (rem * 16.5).round();

            // Left Card (`shade2` surface with `border2` border)
            ui.scene.rounded_rect(
                m,
                top_y,
                left_w,
                main_h,
                theme.radius2(),
                theme.shade2,
                theme.border2,
                1.0,
            );

            let lx = m + pad;
            let lw = left_w - pad * 2.0;
            let mut ly = top_y + pad + theme.font_base() * 0.8;
            ui.scene.text(
                ui.text_sys,
                FontFace::SemiBold,
                ui.font_engine,
                "Realtime Audio & VU",
                lx,
                ly,
                theme.font_base(),
                theme.text,
            );
            ly += rem * 1.35;

            for (k, v) in [
                ("Pipeline:", if self.seg_pipeline == 0 { "Normal (Tape)" } else { "RubberBand" }),
                ("Pitch / Speed:", &format!("{:+.1} st · {:.2}x", self.pitch_semitones, self.tempo_ratio)),
                ("Scale / DPI:", &format!("{:.0}% ({:.2}x)", self.ui_zoom * 100.0, scale)),
            ] {
                ui.scene.text(
                    ui.text_sys,
                    FontFace::Regular,
                    ui.font_engine,
                    k,
                    lx,
                    ly,
                    theme.font_small(),
                    theme.text_dim,
                );
                ui.scene.text(
                    ui.text_sys,
                    FontFace::SemiBold,
                    ui.font_engine,
                    v,
                    lx + rem * 6.4,
                    ly,
                    theme.font_small(),
                    theme.text,
                );
                ly += rem * 1.28;
            }

            // Master Volume `nui-slider`
            ly += rem * 0.25;
            let vol_str = format!("{:.0}%", self.master_vol * 100.0);
            ui.slider(
                101,
                Rect::new(lx, ly, lw, rem * 2.5),
                "Master Gain",
                &mut self.master_vol,
                0.0,
                1.0,
                0.78,
                &vol_str,
            );
            ly += rem * 2.85;

            // Live 16-band spectrum analyzer canvas (`base` surface)
            let spec_h = (top_y + main_h - pad - ly).max(rem * 3.0);
            ui.scene.rounded_rect(
                lx,
                ly,
                lw,
                spec_h,
                theme.radius2(),
                theme.base,
                theme.border2,
                1.0,
            );
            let bands = 18usize;
            let gap = (rem * 0.2).round().max(2.0);
            let bar_w = (lw - gap * (bands as f32 + 1.0)) / bands as f32;
            for i in 0..bands {
                let phase = self.t * 5.8 + i as f32 * 0.42;
                let amp = if playing {
                    ((phase.sin() * 0.35 + (phase * 1.7).cos() * 0.25 + 0.45) * self.master_vol)
                        .clamp(0.06, 0.94)
                } else {
                    (0.14 + 0.08 * (i as f32 * 0.5).sin()) * self.master_vol
                };
                let bh = (spec_h - rem * 0.7) * amp;
                let bx = lx + gap + i as f32 * (bar_w + gap);
                let by = ly + spec_h - rem * 0.35 - bh;
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

            // ================================================================
            // 3. Right Panel: NUI Component Gallery & SoundApp Parameters (`nui-tabs`)
            // ================================================================
            let right_x = m + left_w + m;
            let right_w = (pw - right_x - m).max(200.0);
            ui.scene.rounded_rect(
                right_x,
                top_y,
                right_w,
                main_h,
                theme.radius2(),
                theme.shade2,
                theme.border2,
                1.0,
            );

            let rx = right_x + pad;
            let rw = right_w - pad * 2.0;
            let tabs_h = (rem * 2.25).round();
            ui.tabs(
                200,
                Rect::new(rx, top_y + rem * 0.3, rw, tabs_h),
                &[
                    (ICON_TUNE, "NUI Components"),
                    (ICON_SETTINGS, "Audio Parameters"),
                    (ICON_SEARCH, "Typography A/B"),
                ],
                &mut self.active_tab,
            );

            let content_y = top_y + rem * 0.3 + tabs_h + rem * 0.85;
            let col_gap = rem * 1.1;
            let col_w = ((rw - col_gap) * 0.5).floor();
            let col1_x = rx;
            let col2_x = rx + col_w + col_gap;
            let row_h = (rem * 2.15).round();

            match self.active_tab {
                0 => {
                    // ---- TAB 0: NUI Components (`nui-input`, `nui-select`, `nui-button`, `segmented`, `checkbox`, `switch`, `radio`) ----
                    let mut y1 = content_y;

                    // 1. nui-input-group + nui-input (Editable text input with selection & clear button)
                    ui.input_label(col1_x, y1, "nui-input (click to edit, drag to select)");
                    y1 += rem * 1.15;
                    ui.text_input(
                        301,
                        Rect::new(col1_x, y1, col_w, row_h),
                        "Type filter or track title...",
                        &mut self.search_text,
                    );
                    y1 += row_h + rem * 0.85;

                    // 2. Segmented Button Container (`nui-button-container[variant="segmented"]`)
                    ui.input_label(col1_x, y1, "nui-button-container [variant=segmented]");
                    y1 += rem * 1.15;
                    ui.segmented(
                        310,
                        Rect::new(col1_x, y1, col_w, row_h),
                        &["Tape Speed", "Pitch / Time", "MIDI Synth"],
                        &mut self.seg_pipeline,
                    );
                    y1 += row_h + rem * 0.85;

                    // 3. Button Variants (`Primary`, `Outline`, `Ghost`, `Success`, `Danger`)
                    ui.input_label(
                        col1_x,
                        y1,
                        &format!("nui-button variants (clicked: {})", self.click_counter),
                    );
                    y1 += rem * 1.15;
                    let b_gap = (rem * 0.4).round();
                    let bw4 = ((col_w - b_gap * 3.0) / 4.0).floor();
                    if ui.button(
                        320,
                        Rect::new(col1_x, y1, bw4, row_h),
                        "Primary",
                        "",
                        ButtonVariant::Primary,
                    ) {
                        self.click_counter += 1;
                    }
                    if ui.button(
                        321,
                        Rect::new(col1_x + (bw4 + b_gap), y1, bw4, row_h),
                        "Outline",
                        "",
                        ButtonVariant::Outline,
                    ) {
                        self.click_counter += 1;
                    }
                    if ui.button(
                        322,
                        Rect::new(col1_x + (bw4 + b_gap) * 2.0, y1, bw4, row_h),
                        "Success",
                        "",
                        ButtonVariant::Success,
                    ) {
                        self.click_counter += 1;
                    }
                    if ui.button(
                        323,
                        Rect::new(col1_x + (bw4 + b_gap) * 3.0, y1, bw4, row_h),
                        "Danger",
                        "",
                        ButtonVariant::Danger,
                    ) {
                        self.click_counter += 1;
                    }
                    y1 += row_h + rem * 0.75;

                    // 4. Checkboxes & Switches (`nui-checkbox` & `variant="switch"`)
                    let half_c = (col_w * 0.5).floor();
                    ui.checkbox(
                        330,
                        Rect::new(col1_x, y1, half_c, rem * 1.7),
                        "Preserve Formants",
                        &mut self.chk_formant,
                    );
                    ui.switch(
                        331,
                        Rect::new(col1_x + half_c, y1, half_c, rem * 1.7),
                        "HQ Oversampling",
                        &mut self.sw_hq_mode,
                    );

                    // ---- Right Column of Tab 0: `nui-select`, `nui-slider`, `nui-radio` ----
                    let mut y2 = content_y;

                    // Notice: we draw lower widgets or trigger `nui-select` (which pushes its popup to Scene's overlay layer!)
                    ui.input_label(col2_x, y2, "nui-select (audio output device)");
                    y2 += rem * 1.15;
                    let sel1_y = y2;
                    y2 += row_h + rem * 0.85;

                    // Bipolar Pitch Slider (`nui-slider`, double-click to reset to 0.0 st)
                    let pitch_lbl = format!("{:+.1} st", self.pitch_semitones);
                    ui.slider(
                        340,
                        Rect::new(col2_x, y2, col_w, rem * 2.55),
                        "Pitch Shift (dbl-click reset)",
                        &mut self.pitch_semitones,
                        -12.0,
                        12.0,
                        0.0,
                        &pitch_lbl,
                    );
                    y2 += rem * 2.85;

                    // Tempo Slider (`nui-slider`, double-click to reset to 1.00x)
                    let tempo_lbl = format!("{:.2}x", self.tempo_ratio);
                    ui.slider(
                        341,
                        Rect::new(col2_x, y2, col_w, rem * 2.55),
                        "Tempo / Speed (wheel fine-tune)",
                        &mut self.tempo_ratio,
                        0.5,
                        2.0,
                        1.0,
                        &tempo_lbl,
                    );
                    y2 += rem * 2.85;

                    // Radio buttons (`nui-radio`)
                    let r_w = (col_w / 3.0).floor();
                    for (ri, rlabel) in ["Linear", "Cubic", "Sinc 8-tap"].iter().enumerate() {
                        if ui.radio(
                            350 + ri as u32,
                            Rect::new(col2_x + ri as f32 * r_w, y2, r_w, rem * 1.7),
                            rlabel,
                            self.radio_interp == ri,
                        ) {
                            self.radio_interp = ri;
                        }
                    }

                    // Render `nui-select` (its open popup goes to the top overlay layer!)
                    ui.select(
                        360,
                        Rect::new(col2_x, sel1_y, col_w, row_h),
                        DEVICES,
                        &mut self.selected_device,
                    );
                }
                1 => {
                    // ---- TAB 1: SoundApp Parameters & Engine Configuration ----
                    let mut y1 = content_y;
                    ui.input_label(col1_x, y1, "Preset Name (nui-input)");
                    y1 += rem * 1.15;
                    ui.text_input(
                        401,
                        Rect::new(col1_x, y1, col_w, row_h),
                        "Enter preset name...",
                        &mut self.preset_name,
                    );
                    y1 += row_h + rem * 0.85;

                    ui.input_label(col1_x, y1, "MIDI SoundFont (.sf2)");
                    y1 += rem * 1.15;
                    let sf_y = y1;
                    y1 += row_h + rem * 0.85;

                    ui.input_label(col1_x, y1, "Target Sample Rate");
                    y1 += rem * 1.15;
                    let sr_y = y1;

                    let mut y2 = content_y;
                    let sep_str = format!("{:.0}%", self.stereo_sep);
                    ui.slider(
                        410,
                        Rect::new(col2_x, y2, col_w, rem * 2.6),
                        "Tracker Stereo Separation",
                        &mut self.stereo_sep,
                        0.0,
                        200.0,
                        100.0,
                        &sep_str,
                    );
                    y2 += rem * 3.1;

                    ui.checkbox(
                        411,
                        Rect::new(col2_x, y2, col_w, rem * 1.8),
                        "Lock Parameters Across Track Changes",
                        &mut self.chk_locked,
                    );
                    y2 += rem * 2.1;

                    ui.switch(
                        412,
                        Rect::new(col2_x, y2, col_w, rem * 1.8),
                        "Keep Running in System Tray (0% CPU Idle)",
                        &mut self.sw_keep_tray,
                    );
                    y2 += rem * 2.3;

                    let bw2 = (col_w - rem * 0.5) * 0.5;
                    if ui.button(
                        420,
                        Rect::new(col2_x, y2, bw2, row_h),
                        "Reset Defaults",
                        "",
                        ButtonVariant::Outline,
                    ) {
                        self.pitch_semitones = 0.0;
                        self.tempo_ratio = 1.0;
                        self.stereo_sep = 100.0;
                        self.master_vol = 0.78;
                    }
                    let _ = ui.button(
                        421,
                        Rect::new(col2_x + bw2 + rem * 0.5, y2, bw2, row_h),
                        "Save Preset",
                        "",
                        ButtonVariant::Primary,
                    );

                    ui.select(
                        430,
                        Rect::new(col1_x, sf_y, col_w, row_h),
                        SOUNDFONTS,
                        &mut self.selected_soundfont,
                    );
                    ui.select(
                        431,
                        Rect::new(col1_x, sr_y, col_w, row_h),
                        SAMPLE_RATES,
                        &mut self.selected_rate,
                    );
                }
                _ => {
                    // ---- TAB 2: Side-by-Side Typography A/B Comparison ----
                    let box_h = ((top_y + main_h - pad - content_y - rem * 0.6) * 0.5).max(rem * 4.0);
                    ui.scene.rounded_rect(
                        rx,
                        content_y,
                        rw,
                        box_h,
                        theme.radius2(),
                        theme.base,
                        theme.highlight_dim,
                        1.0,
                    );
                    let mut sy = content_y + rem * 1.3;
                    ui.scene.text(
                        ui.text_sys,
                        FontFace::SemiBold,
                        FontEngine::Swash,
                        "A. SWASH — OpenType Kerning + Hinting + 4x Subpixel + Linear-Light Gamma",
                        rx + rem * 0.75,
                        sy,
                        theme.font_xsmall(),
                        theme.highlight,
                    );
                    sy += rem * 1.45;
                    ui.scene.text(
                        ui.text_sys,
                        FontFace::Regular,
                        FontEngine::Swash,
                        "12px: To AV We Yo · -14.2 LUFS · 48,000 Hz · Pack my box with five dozen liquor jugs",
                        rx + rem * 0.75,
                        sy,
                        rem * 0.85,
                        theme.text,
                    );
                    sy += rem * 1.55;
                    ui.scene.text(
                        ui.text_sys,
                        FontFace::Regular,
                        FontEngine::Swash,
                        "15px: Waveform & Spectrum Analyzer · 00:42 / 03:18 · Crisp Stems & Even Kerning",
                        rx + rem * 0.75,
                        sy,
                        rem * 1.05,
                        theme.text,
                    );

                    let fy = content_y + box_h + rem * 0.6;
                    ui.scene.rounded_rect(
                        rx,
                        fy,
                        rw,
                        box_h,
                        theme.radius2(),
                        theme.base,
                        theme.border2,
                        1.0,
                    );
                    let mut fty = fy + rem * 1.3;
                    ui.scene.text(
                        ui.text_sys,
                        FontFace::SemiBold,
                        FontEngine::Fontdue,
                        "B. FONTDUE (Legacy POC) — Unhinted + Integer-Snapped + Naive sRGB Alpha",
                        rx + rem * 0.75,
                        fty,
                        theme.font_xsmall(),
                        theme.text_dim,
                    );
                    fty += rem * 1.45;
                    ui.scene.text(
                        ui.text_sys,
                        FontFace::Regular,
                        FontEngine::Fontdue,
                        "12px: To AV We Yo · -14.2 LUFS · 48,000 Hz · Pack my box with five dozen liquor jugs",
                        rx + rem * 0.75,
                        fty,
                        rem * 0.85,
                        theme.text,
                    );
                    fty += rem * 1.55;
                    ui.scene.text(
                        ui.text_sys,
                        FontFace::Regular,
                        FontEngine::Fontdue,
                        "15px: Waveform & Spectrum Analyzer · 00:42 / 03:18 · Crisp Stems & Even Kerning",
                        rx + rem * 0.75,
                        fty,
                        rem * 1.05,
                        theme.text,
                    );
                }
            }

            // ================================================================
            // 4. Bottom Transport Card
            // ================================================================
            let bot_y = ph - bot_h - m;
            ui.scene.rounded_rect(
                m,
                bot_y,
                pw - m * 2.0,
                bot_h,
                theme.radius2(),
                theme.shade2,
                theme.border2,
                1.0,
            );

            let bar_x = m + pad;
            let bar_y = bot_y + rem * 0.75;
            let bar_w = pw - (m + pad) * 2.0;
            let bar_h = (rem * 0.38).round().max(5.0);
            let bar_hit = Rect::new(bar_x, bar_y - rem * 0.4, bar_w, bar_h + rem * 0.8);

            if ui.state.can_hover(500, bar_hit) {
                ui.state.hovered_id = Some(500);
                if ui.state.mouse_pressed {
                    ui.state.active_id = Some(500);
                }
            }
            if ui.state.active_id == Some(500) && ui.state.mouse_down {
                self.scrub_frac = ((ui.state.cursor.0 - bar_x) / bar_w).clamp(0.0, 1.0);
            }

            ui.scene.rounded_rect(
                bar_x,
                bar_y,
                bar_w,
                bar_h,
                bar_h * 0.5,
                theme.base,
                (0, 0, 0, 0),
                0.0,
            );

            let (pos, dur) = self
                .audio
                .as_ref()
                .map(|a| (a.position_secs(), a.duration_secs()))
                .unwrap_or((0.0, 0.0));
            let is_finite = dur > 0.0 && dur < 360_000.0;
            let frac = if ui.state.active_id == Some(500) {
                self.scrub_frac
            } else if is_finite {
                (pos / dur).clamp(0.0, 1.0) as f32
            } else if playing {
                (self.t * 0.12).fract()
            } else {
                self.scrub_frac
            };

            ui.scene.rounded_rect(
                bar_x,
                bar_y,
                (bar_w * frac).max(bar_h),
                bar_h,
                bar_h * 0.5,
                theme.highlight,
                (0, 0, 0, 0),
                0.0,
            );

            let ctrl_y = bot_y + rem * 3.15;
            ui.scene.text(
                ui.text_sys,
                FontFace::SemiBold,
                ui.font_engine,
                &fmt_time(pos),
                bar_x,
                ctrl_y,
                theme.font_small(),
                theme.text,
            );

            let tbtn_w = (rem * 2.4).round();
            let tbtn_h = (rem * 1.85).round();
            let tbtn_y = bot_y + rem * 1.75;
            let cx = pw * 0.5;
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
            let rw_time = ui.text_sys.measure(
                FontFace::SemiBold,
                ui.font_engine,
                &remain,
                theme.font_small(),
            );
            ui.scene.text(
                ui.text_sys,
                FontFace::SemiBold,
                ui.font_engine,
                &remain,
                bar_x + bar_w - rw_time,
                ctrl_y,
                theme.font_small(),
                theme.text,
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
            .with_inner_size(winit::dpi::LogicalSize::new(860.0, 490.0))
            .with_min_inner_size(winit::dpi::LogicalSize::new(640.0, 380.0))
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
            WindowEvent::ModifiersChanged(m) => {
                self.modifiers = m.state();
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.ui_state.cursor = (position.x as f32, position.y as f32);
                if let Some(w) = &self.window {
                    w.request_redraw();
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
                            let rem = (14.5 * scale).round();
                            let hdr_h = (rem * 2.55).round();
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
                        Key::Character(ch) if self.modifiers.control_key() && ch.eq_ignore_ascii_case("a") => {
                            self.ui_state.key_cmds.push(KeyCmd::SelectAll);
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

    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--tone" => tone = true,
            "--silence" => silence = true,
            "--static" => animate = false,
            "--nopaint" => nopaint = true,
            "--open-select" => open_select_demo = true,
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
    app.active_tab = initial_tab.min(2);
    if open_select_demo {
        app.ui_state.open_popup_id = Some(360);
    }
    event_loop.run_app(&mut app).unwrap();
}
