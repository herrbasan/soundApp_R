// NUI Rust — standalone Component Playground
//
// A zero-audio, zero-application-dependency harness for the `nui` crate: every
// interactive widget, the declarative layout engine, the vector/scene primitives,
// browser-grade typography, the virtualized list, the menu/dialog/sidebar shell —
// in six rich tabs. The binary is the canonical place to develop and verify a
// component before it ships.

use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

use nui::{
    BackendKind, BadgeVariant, ButtonVariant, Color, FontEngine, FontFace, KeyCmd,
    Length, LinkListEntry, MenuBarEntry, MenuItem, Rect, Renderer, Scene, TextSystem, Theme, Ui,
    UiState, WindowManager, WindowSpec, ICON_ADD, ICON_CHECK, ICON_CHEVRON_DOWN, ICON_CHEVRON_UP,
    ICON_CLOSE, ICON_DARK_MODE, ICON_EQ, ICON_INFO, ICON_LIGHT_MODE, ICON_LOCK, ICON_LOOP,
    ICON_MENU, ICON_MORE_VERT, ICON_NEXT, ICON_PAUSE, ICON_PLAY, ICON_PREV, ICON_RESTART,
    ICON_SEARCH, ICON_SETTINGS, ICON_SHUFFLE, ICON_TUNE, ICON_VOLUME,
};

/// Secondary window spec — the live `UiState` inspector.
static INSPECTOR_SPEC: WindowSpec = WindowSpec {
    key: "inspector",
    title: "NUI Inspector — Live UiState",
    width: 640.0,
    height: 440.0,
    min_width: 420.0,
    min_height: 300.0,
    undecorated: true,
};

const DEVICES: &[&str] = &[
    "WASAPI · Speakers (Realtek)",
    "WASAPI · Headphones (USB DAC)",
    "DirectSound · Primary",
    "ASIO · Studio Interface",
];

/// Synthetic library size for the virtualized list.
const ROW_COUNT: usize = 10_000;

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

const TRACK_ALBUMS: &[&str] = &[
    "Analog Drift",
    "Neon Meridian",
    "Deep Field",
    "Rust & Glass",
    "Slow Orbit",
    "Static Bloom",
];

/// The full Material Icons set exposed by `nui` — 20 constants, name-paired.
const ICONS: &[(&str, &str)] = &[
    (ICON_PLAY, "ICON_PLAY"),
    (ICON_PAUSE, "ICON_PAUSE"),
    (ICON_PREV, "ICON_PREV"),
    (ICON_NEXT, "ICON_NEXT"),
    (ICON_CHECK, "ICON_CHECK"),
    (ICON_CLOSE, "ICON_CLOSE"),
    (ICON_CHEVRON_DOWN, "ICON_CHEVRON_DOWN"),
    (ICON_CHEVRON_UP, "ICON_CHEVRON_UP"),
    (ICON_SEARCH, "ICON_SEARCH"),
    (ICON_SETTINGS, "ICON_SETTINGS"),
    (ICON_DARK_MODE, "ICON_DARK_MODE"),
    (ICON_LIGHT_MODE, "ICON_LIGHT_MODE"),
    (ICON_EQ, "ICON_EQ"),
    (ICON_VOLUME, "ICON_VOLUME"),
    (ICON_LOCK, "ICON_LOCK"),
    (ICON_TUNE, "ICON_TUNE"),
    (ICON_LOOP, "ICON_LOOP"),
    (ICON_SHUFFLE, "ICON_SHUFFLE"),
    (ICON_RESTART, "ICON_RESTART"),
    (ICON_ADD, "ICON_ADD"),
    (ICON_MENU, "ICON_MENU"),
    (ICON_INFO, "ICON_INFO"),
    (ICON_MORE_VERT, "ICON_MORE_VERT"),
];

/// Tab navigation table `(action id, icon, label)` — the single source of truth
/// for both the Components menu bar entries and the `nui-tabs` strip.
const TAB_NAV: [(&str, &str, &str); 6] = [
    ("tab:0", ICON_TUNE, "Controls & Inputs"),
    ("tab:1", ICON_SETTINGS, "Layout & Cards"),
    ("tab:2", ICON_EQ, "Vectors & Canvas"),
    ("tab:3", ICON_SEARCH, "Typography & Icons"),
    ("tab:4", ICON_LOOP, "Virtual List (10k)"),
    ("tab:5", ICON_MENU, "Menus & Overlays"),
];

/// `nui-select` options for the transition-duration preset (tab 5).
const TRANSITION_PRESETS: &[&str] =
    &["None (0.00s)", "Fast (0.15s)", "Standard (0.20s)", "Slow (0.45s)"];

const TYPO_TEXT: &str = "Hamburgefonstiv · 0123456789 · QWERTY";

fn track_name(idx: usize) -> String {
    TRACK_TITLES[idx % TRACK_TITLES.len()].to_string()
}

fn track_artist(idx: usize) -> &'static str {
    TRACK_ARTISTS[idx % TRACK_ARTISTS.len()]
}

fn track_album(idx: usize) -> &'static str {
    TRACK_ALBUMS[(idx / 7) % TRACK_ALBUMS.len()]
}

fn track_meta(idx: usize) -> String {
    let mm = 2 + (idx * 7) % 8;
    let ss = (idx * 13) % 60;
    format!("48 kHz · 24-bit · FLAC · {mm:02}:{ss:02}")
}

/// Deterministic 96-bar synthetic waveform peaks (`0.0..=1.0`).
fn synth_peaks(n: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let x = i as f32 / n as f32;
        let envelope = (x * std::f32::consts::PI).sin().max(0.05);
        let a = (x * 41.0).sin() * 0.55 + (x * 97.0).cos() * 0.3 + (x * 211.0).sin() * 0.15;
        out.push((a.abs() * envelope * 1.3).clamp(0.0, 1.0));
    }
    out
}

/// Multi-harmonic sine curve, normalized `(nx, ny)` for `Ui::polyline`.
fn harmonic_points(t: f32) -> Vec<(f32, f32)> {
    let mut pts = Vec::with_capacity(96);
    for i in 0..96 {
        let x = i as f32 / 95.0;
        let y = 0.5
            + 0.22 * (x * std::f32::consts::TAU * 2.0 + t * 1.7).sin()
            + 0.12 * (x * std::f32::consts::TAU * 5.0 - t * 2.4).sin()
            + 0.06 * (x * std::f32::consts::TAU * 11.0 + t * 3.1).cos();
        pts.push((x, y.clamp(0.02, 0.98)));
    }
    pts
}

/// Lissajous figure (3:2), normalized `(nx, ny)` for `Ui::polyline`.
fn lissajous_points(t: f32) -> Vec<(f32, f32)> {
    let mut pts = Vec::with_capacity(128);
    for i in 0..128 {
        let a = i as f32 / 127.0 * std::f32::consts::TAU;
        let nx = 0.5 + 0.45 * (3.0 * a + t * 0.9).sin();
        let ny = 0.5 + 0.45 * (2.0 * a).cos();
        pts.push((nx, ny));
    }
    pts
}

fn fmt_opt(id: Option<u32>) -> String {
    match id {
        Some(v) => format!("{v}"),
        None => "—".to_string(),
    }
}

/// One theme-token swatch: filled square + token name.
fn draw_swatch(ui: &mut Ui<'_>, r: Rect, name: &str, col: Color, theme: Theme) {
    let h = (theme.rem * 1.5).round();
    let y = (r.y + (r.h - h) * 0.5).round();
    ui.scene
        .rounded_rect(r.x, y, h, h, theme.radius1(), col, theme.border3, 1.0);
    let fs = theme.font_small();
    let tx = (r.x + h + theme.space_half()).round();
    ui.text_clipped(
        FontFace::Regular,
        ui.font_engine,
        name,
        tx,
        (y + h * 0.5 + fs * 0.33).round(),
        fs,
        (r.x + r.w - tx).max(0.0),
        theme.text_dim,
    );
}

fn window_spec_by_key(key: &str) -> Option<&'static WindowSpec> {
    if key == INSPECTOR_SPEC.key {
        Some(&INSPECTOR_SPEC)
    } else {
        None
    }
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
    window: Option<std::sync::Arc<Window>>,
    renderer: Option<Renderer>,
    scene: Scene,
    text_sys: TextSystem,
    ui_state: UiState,
    modifiers: ModifiersState,
    size: (u32, u32),
    t: f32,
    frame_ms: u64,
    nopaint: bool,
    extra_ops: usize,
    frames: u32,
    last_fps: u32,
    fps_at: Instant,
    next_frame: Instant,
    quitting: bool,

    secondary_windows: WindowManager,
    pending_window_toggles: Vec<&'static str>,
    open_window_spec: Option<&'static WindowSpec>,

    // Shell chrome: header · menu bar · collapsible `nui-sidebar`
    sidebar_open: bool,
    nav_groups: [bool; 3],
    active_nav_key: String,
    show_status_dialog: bool,
    last_menu_action: String,

    // Tab + component state
    active_tab: usize,
    // Last tab index the entrance transition was seeded for. A change re-seeds the
    // `0x7AB0_0000` animation key so the panel replays its fade-in.
    prev_tab: usize,
    // Tab 5 owns its controls: sharing tab 0's would couple the two playgrounds.
    anim_thumb: bool,
    anim_flash: bool,
    anim_check: bool,
    anim_check2: bool,
    anim_easing: usize,
    anim_duration: usize,
    input_main: String,
    input_second: String,
    seg_choice: usize,
    click_counter: u32,
    selected_device: usize,
    chk_formant: bool,
    chk_grid: bool,
    sw_hq: bool,
    sw_tray: bool,
    slider_std: f32,
    slider_bipolar: f32,
    radio_choice: usize,
    wave_peaks: Vec<f32>,
    playhead: f32,
    filter: String,
    filter_used: String,
    filtered: Vec<usize>,
    selected_row: usize,
}

impl App {
    fn new(
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
            size: (1220, 740),
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

            sidebar_open: true,
            nav_groups: [true, true, true],
            active_nav_key: "tab:0".to_string(),
            show_status_dialog: false,
            last_menu_action: "None (use menu bar or right-click)".to_string(),

            active_tab: 0,
            prev_tab: 0,
            anim_thumb: true,
            anim_flash: false,
            anim_check: true,
            anim_check2: false,
            anim_easing: 1,
            anim_duration: 2,
            input_main: "NUI Rust playground".to_string(),
            input_second: String::new(),
            seg_choice: 0,
            click_counter: 0,
            selected_device: 0,
            chk_formant: true,
            chk_grid: false,
            sw_hq: true,
            sw_tray: false,
            slider_std: 62.0,
            slider_bipolar: -3.5,
            radio_choice: 1,
            wave_peaks: synth_peaks(96),
            playhead: 0.35,
            filter: String::new(),
            filter_used: String::new(),
            filtered: (0..ROW_COUNT).collect(),
            selected_row: 0,
        }
    }

    /// Dispatch one action id emitted by `menu_bar` / `context_menu`. Runs outside the
    /// `Ui` borrow so it is free to call [`App::set_zoom`] and touch widget state.
    fn apply_nav_action(
        &mut self,
        action: &str,
        toggle_backend_req: &mut bool,
        toggle_font_req: &mut bool,
        toggle_theme_req: &mut bool,
        toggle_drop_req: &mut bool,
        close_req: &mut bool,
    ) {
        self.last_menu_action = action.to_string();
        match action {
            "diag:dialog" => self.show_status_dialog = true,
            "file:clear" => self.input_main.clear(),
            "file:exit" => *close_req = true,
            "view:sidebar" => self.sidebar_open = !self.sidebar_open,
            "view:theme" => *toggle_theme_req = true,
            "zoom:80" => self.set_zoom(0.8),
            "zoom:100" => self.set_zoom(1.0),
            "zoom:125" => self.set_zoom(1.25),
            "zoom:150" => self.set_zoom(1.5),
            "diag:inspector" => self.pending_window_toggles.push("inspector"),
            "diag:backend" => *toggle_backend_req = true,
            "diag:font" => *toggle_font_req = true,
            "diag:drop" => *toggle_drop_req = true,
            "diag:anim" => {
                self.animate = !self.animate;
                if self.animate {
                    self.next_frame = Instant::now();
                }
            }
            a if a.starts_with("tab:") => {
                if let Some(n) = a[4..].parse::<usize>().ok().filter(|n| *n <= 5) {
                    self.active_tab = n;
                    self.active_nav_key = a.to_string();
                }
            }
            _ => {}
        }
    }

    fn set_zoom(&mut self, new_zoom: f32) {
        let clamped = ((new_zoom * 20.0).round() / 20.0).clamp(0.75, 2.0);
        if (clamped - self.ui_zoom).abs() < 0.001 {
            return;
        }
        self.ui_zoom = clamped;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn build_scene(&mut self, w: u32, h: u32, scale: f32) {
        let _ = self.text_sys.prepare_frame();
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
        self.scene.clear_all(page_bg);
        self.ui_state.begin_frame();

        let pw = w as f32;
        let ph = h as f32;
        let active_backend = self
            .renderer
            .as_ref()
            .map(|r| r.active)
            .unwrap_or(self.prefer_backend);

        let mut toggle_backend_req = false;
        let mut toggle_font_req = false;
        let mut toggle_theme_req = false;
        let mut toggle_drop_req = false;
        let mut zoom_delta: f32 = 0.0;
        let mut close_req = false;
        // Menu bar and context menu both yield an action id; both are dispatched
        // after the `Ui` borrow is released.
        let menu_action: Option<String>;
        let ctx_action: Option<String>;

        {
            let mut ui = Ui::new(
                &mut self.ui_state,
                &mut self.scene,
                &mut self.text_sys,
                self.font_engine,
                theme,
            );

            // ================================================================
            // 1. Application header (`nui-app-header`, 3.25rem) — chrome only.
            //    No badges, no demo buttons: the middle stays empty so it can act
            //    as the live window drag zone.
            // ================================================================
            let hdr_h = theme.header_height();
            ui.scene.rect(0.0, 0.0, pw, hdr_h, theme.shade2);
            ui.scene.rect(0.0, hdr_h - 1.0, pw, 1.0, theme.border2);

            let btn_h = (rem * 1.75).round();
            let btn_y = (hdr_h - btn_h) * 0.5;

            if ui.button(
                89,
                Rect::new((rem * 0.6).round(), btn_y, (rem * 2.1).round(), btn_h),
                "",
                ICON_MENU,
                ButtonVariant::Ghost,
            ) {
                self.sidebar_open = !self.sidebar_open;
            }

            let title_fs = theme.font_medium();
            ui.scene
                .push_clip(rem * 3.1, 0.0, (pw - rem * 13.0).max(0.0), hdr_h);
            let _ = ui.scene.text(
                ui.text_sys,
                FontFace::Regular,
                ui.font_engine,
                "NUI Playground",
                (rem * 3.1).round(),
                (hdr_h * 0.5 + title_fs * 0.34).round(),
                title_fs,
                theme.text,
            );
            ui.scene.pop_clip();

            // Right cluster: close · diagnostics · inspector · theme. Four identical
            // 2rem icon buttons, matching the web Playground header exactly.
            let icon_btn = (rem * 2.0).round();
            let btn_gap = rem * 0.25;
            let mut rx = pw - rem * 0.6 - icon_btn;
            if ui.button(
                90,
                Rect::new(rx, btn_y, icon_btn, icon_btn),
                "",
                ICON_CLOSE,
                ButtonVariant::Ghost,
            ) {
                close_req = true;
            }

            rx -= icon_btn + btn_gap;
            if ui.button(
                98,
                Rect::new(rx, btn_y, icon_btn, icon_btn),
                "",
                ICON_SETTINGS,
                if self.show_status_dialog {
                    ButtonVariant::Primary
                } else {
                    ButtonVariant::Ghost
                },
            ) {
                self.show_status_dialog = !self.show_status_dialog;
            }

            rx -= icon_btn + btn_gap;
            if ui.button(
                91,
                Rect::new(rx, btn_y, icon_btn, icon_btn),
                "",
                ICON_TUNE,
                if self.secondary_windows.is_visible("inspector") {
                    ButtonVariant::Primary
                } else {
                    ButtonVariant::Ghost
                },
            ) {
                self.pending_window_toggles.push("inspector");
            }

            rx -= icon_btn + btn_gap;
            let th_icon = if self.dark_mode {
                ICON_LIGHT_MODE
            } else {
                ICON_DARK_MODE
            };
            if ui.button(
                96,
                Rect::new(rx, btn_y, icon_btn, icon_btn),
                "",
                th_icon,
                ButtonVariant::Ghost,
            ) {
                toggle_theme_req = true;
            }

            // ================================================================
            // 2. `.nui-menu-bar` — the whole demo control surface
            //    The shell geometry is needed first: the sidebar occupies the
            //    full-height left column under the header, and the menu bar
            //    starts immediately to its right.
            // ================================================================
            let m = Length::Rem(1.0).resolve(rem).round();
            let shell_y = hdr_h;
            let shell_h = (ph - shell_y).max(0.0);

            let side_t = ui.state.animate_bool(8800, self.sidebar_open, 0.30);
            let side_w = ((rem * 15.0).round() * side_t).round();

            let menu_h = (rem * 2.0).round();
            let zoom_items: &[MenuItem<'_>] = &[
                MenuItem::Action {
                    label: "80%",
                    icon: "",
                    shortcut: "",
                    action: "zoom:80",
                    disabled: false,
                    checked: Some((self.ui_zoom - 0.80).abs() < 0.01),
                },
                MenuItem::Action {
                    label: "100%",
                    icon: "",
                    shortcut: "",
                    action: "zoom:100",
                    disabled: false,
                    checked: Some((self.ui_zoom - 1.00).abs() < 0.01),
                },
                MenuItem::Action {
                    label: "125%",
                    icon: "",
                    shortcut: "",
                    action: "zoom:125",
                    disabled: false,
                    checked: Some((self.ui_zoom - 1.25).abs() < 0.01),
                },
                MenuItem::Action {
                    label: "150%",
                    icon: "",
                    shortcut: "",
                    action: "zoom:150",
                    disabled: false,
                    checked: Some((self.ui_zoom - 1.50).abs() < 0.01),
                },
            ];

            let file_items: &[MenuItem<'_>] = &[
                MenuItem::Action {
                    label: "Status & Demo Controls…",
                    icon: "",
                    shortcut: "F1",
                    action: "diag:dialog",
                    disabled: false,
                    checked: None,
                },
                MenuItem::Action {
                    label: "Clear Primary Input",
                    icon: "",
                    shortcut: "Ctrl+L",
                    action: "file:clear",
                    disabled: false,
                    checked: None,
                },
                MenuItem::Separator,
                MenuItem::Action {
                    label: "Exit Playground",
                    icon: "",
                    shortcut: "Esc",
                    action: "file:exit",
                    disabled: false,
                    checked: None,
                },
            ];

            let view_items: &[MenuItem<'_>] = &[
                MenuItem::Action {
                    label: "Show Sidebar",
                    icon: "",
                    shortcut: "Ctrl+B",
                    action: "view:sidebar",
                    disabled: false,
                    checked: Some(self.sidebar_open),
                },
                MenuItem::Action {
                    label: "Dark Theme",
                    icon: "",
                    shortcut: "T",
                    action: "view:theme",
                    disabled: false,
                    checked: Some(self.dark_mode),
                },
                MenuItem::Separator,
                MenuItem::Submenu {
                    label: "UI Zoom",
                    icon: "",
                    disabled: false,
                    items: zoom_items,
                },
            ];

            let comp_items: Vec<MenuItem<'_>> = TAB_NAV
                .iter()
                .enumerate()
                .map(|(i, &(action, _icon, label))| MenuItem::Action {
                    label,
                    icon: "",
                    shortcut: "",
                    action,
                    disabled: false,
                    checked: Some(i == self.active_tab),
                })
                .collect();

            let diag_items: &[MenuItem<'_>] = &[
                MenuItem::Action {
                    label: "Status & Settings Dialog…",
                    icon: "",
                    shortcut: "",
                    action: "diag:dialog",
                    disabled: false,
                    checked: Some(self.show_status_dialog),
                },
                MenuItem::Action {
                    label: "UiState Inspector Window",
                    icon: "",
                    shortcut: "W",
                    action: "diag:inspector",
                    disabled: false,
                    checked: None,
                },
                MenuItem::Separator,
                MenuItem::Action {
                    label: "Switch Backend (GPU / CPU)",
                    icon: "",
                    shortcut: "B",
                    action: "diag:backend",
                    disabled: false,
                    checked: None,
                },
                MenuItem::Action {
                    label: "Switch Font Engine (Swash / Fontdue)",
                    icon: "",
                    shortcut: "F",
                    action: "diag:font",
                    disabled: false,
                    checked: None,
                },
                MenuItem::Action {
                    label: "Toggle Dropzone Preview",
                    icon: "",
                    shortcut: "D",
                    action: "diag:drop",
                    disabled: false,
                    checked: Some(ui.state.hovered_file.is_some()),
                },
                MenuItem::Action {
                    label: "Toggle 60fps Stress Animation",
                    icon: "",
                    shortcut: "Space",
                    action: "diag:anim",
                    disabled: false,
                    checked: Some(self.animate),
                },
            ];

            let menus: Vec<MenuBarEntry<'_>> = vec![
                MenuBarEntry { label: "File", items: file_items },
                MenuBarEntry { label: "View", items: view_items },
                MenuBarEntry { label: "Components", items: &comp_items },
                MenuBarEntry { label: "Diagnostics", items: diag_items },
            ];
            menu_action = ui.menu_bar(
                500,
                Rect::new(side_w, hdr_h, (pw - side_w).max(0.0), menu_h),
                &menus,
            );

            // ================================================================
            // 3. Collapsible `nui-sidebar` + `nui-link-list[mode="fold"]`
            //    The sidebar is a full-height left column that starts directly
            //    under the header; the menu bar sits to its right.
            // ================================================================
            if side_w > 2.0 {
                ui.scene.rect(0.0, shell_y, side_w, shell_h, theme.shade2);
                ui.scene
                    .rect(side_w - 1.0, shell_y, 1.0, shell_h, theme.border2);

                let g_components: &[LinkListEntry<'_>] = &[
                    LinkListEntry::Item {
                        key: "tab:0",
                        label: "Controls & Inputs",
                        icon: "",
                        badge: "",
                    },
                    LinkListEntry::Item {
                        key: "tab:1",
                        label: "Layout & Cards",
                        icon: "",
                        badge: "",
                    },
                    LinkListEntry::Item {
                        key: "tab:5",
                        label: "Menus & Overlays",
                        icon: "",
                        badge: "",
                    },
                ];
                let g_rendering: &[LinkListEntry<'_>] = &[
                    LinkListEntry::Item {
                        key: "tab:2",
                        label: "Vectors & Canvas",
                        icon: "",
                        badge: "",
                    },
                    LinkListEntry::Item {
                        key: "tab:3",
                        label: "Typography & Icons",
                        icon: "",
                        badge: "",
                    },
                    LinkListEntry::Item {
                        key: "tab:4",
                        label: "Virtual List (10k)",
                        icon: "",
                        badge: "",
                    },
                ];
                let g_system: &[LinkListEntry<'_>] = &[
                    LinkListEntry::Item {
                        key: "dlg:status",
                        label: "Diagnostics & Status",
                        icon: "",
                        badge: "",
                    },
                    LinkListEntry::Item {
                        key: "win:inspector",
                        label: "Inspector Window",
                        icon: "",
                        badge: "",
                    },
                ];
                let nav_entries: Vec<LinkListEntry<'_>> = vec![
                    LinkListEntry::Group {
                        id: 1,
                        label: "Components",
                        icon: ICON_TUNE,
                        expanded: self.nav_groups[0],
                        items: g_components,
                    },
                    LinkListEntry::Group {
                        id: 2,
                        label: "Rendering & Data",
                        icon: ICON_EQ,
                        expanded: self.nav_groups[1],
                        items: g_rendering,
                    },
                    LinkListEntry::Separator,
                    LinkListEntry::Group {
                        id: 3,
                        label: "System & Tools",
                        icon: ICON_INFO,
                        expanded: self.nav_groups[2],
                        items: g_system,
                    },
                ];

                let mut toggled_group: Option<u32> = None;
                ui.scene
                    .push_clip(0.0, shell_y, (side_w - 1.0).max(0.0), shell_h);
                let nav_clicked = ui.link_list(
                    850,
                    Rect::new(
                        0.0,
                        shell_y,
                        (rem * 15.0).round(),
                        shell_h,
                    ),
                    &nav_entries,
                    &mut self.active_nav_key,
                    &mut toggled_group,
                );
                ui.scene.pop_clip();

                if let Some(g) = toggled_group.filter(|g| (1..=3).contains(g)) {
                    let slot = &mut self.nav_groups[(g - 1) as usize];
                    *slot = !*slot;
                }
                if nav_clicked {
                    let key = self.active_nav_key.clone();
                    if let Some(n) = key.strip_prefix("tab:").and_then(|s| s.parse().ok()) {
                        if n <= 5 {
                            self.active_tab = n;
                        }
                    } else if key == "dlg:status" {
                        self.show_status_dialog = true;
                        self.active_nav_key = format!("tab:{}", self.active_tab);
                    } else if key == "win:inspector" {
                        self.pending_window_toggles.push("inspector");
                        self.active_nav_key = format!("tab:{}", self.active_tab);
                    }
                }
            }

            let main_x = side_w + m;
            let main_w = (pw - side_w - m * 2.0).max(0.0);
            let tab_labels: Vec<(&str, &str)> =
                TAB_NAV.iter().map(|&(_, icon, label)| (icon, label)).collect();
            let tabs_r = Rect::new(
                main_x,
                hdr_h + menu_h,
                main_w,
                (rem * 2.5).round(),
            );
            if ui.tabs(100, tabs_r, &tab_labels, &mut self.active_tab) {
                self.active_nav_key = format!("tab:{}", self.active_tab);
            }

            let body_y = tabs_r.y + tabs_r.h + (rem * 0.6).round();
            // Tab panel entrance (`@keyframes ani-fade-in`, nui-theme.css:4529): fade in
            // plus a short upward slide. Re-seeding the key at 0.0 is what makes it
            // replay — `animate_bool` snaps to the target the first time it sees a key.
            const TAB_IN_KEY: u32 = 0x7AB0_0000;
            if self.active_tab != self.prev_tab {
                self.prev_tab = self.active_tab;
                ui.state
                    .anim_values
                    .retain(|(k, _)| *k != TAB_IN_KEY);
                ui.state.anim_values.push((TAB_IN_KEY, 0.0));
            } else if !ui
                .state
                .anim_values
                .iter()
                .any(|(k, _)| *k == TAB_IN_KEY)
            {
                ui.state.anim_values.push((TAB_IN_KEY, 1.0));
            }
            let tab_in = ui.state.animate_bool(TAB_IN_KEY, true, 0.30);
            let body = Rect::new(
                main_x,
                body_y + (1.0 - tab_in) * (rem * 0.35),
                main_w,
                (ph - body_y - m).max(80.0),
            );

            match self.active_tab {
                // ============================================================
                // TAB 0 — Controls & Inputs
                // ============================================================
                0 => {
                    let [col_l, col_r] =
                        ui.cols(body, Length::rem(1.25), [Length::fill(), Length::fill()]);
                    let mut select_r = Rect::new(0.0, 0.0, 0.0, 0.0);

                    ui.card(col_l, Length::rem(1.25), Length::rem(0.62), |ui| {
                        let inp = ui.input_group("nui-input · primary field");
                        ui.text_input(301, inp, "Type something…", &mut self.input_main);

                        let [clr_r, hint_r] = ui.take_cols(
                            Length::rem(2.0),
                            Length::rem(0.75),
                            [Length::rem(6.5), Length::fill()],
                        );
                        if ui.button(303, clr_r, "Clear", "", ButtonVariant::Outline) {
                            self.input_main.clear();
                        }
                        let fs = theme.font_small();
                        ui.text_clipped(
                            FontFace::Regular,
                            ui.font_engine,
                            "Ctrl+A / C / X / V · Tab cycles focus",
                            hint_r.x.round(),
                            (hint_r.y + hint_r.h * 0.5 + fs * 0.33).round(),
                            fs,
                            hint_r.w,
                            theme.text_muted,
                        );

                        let inp2 = ui.input_group("nui-input · secondary field");
                        ui.text_input(304, inp2, "Second field…", &mut self.input_second);

                        let seg = ui.input_group("nui-button-container [variant=segmented]");
                        let _ =
                            ui.segmented(310, seg, &["Tape", "Pitch/Time", "MIDI"], &mut self.seg_choice);

                        let btn_lbl = format!("nui-button · variants (clicks: {})", self.click_counter);
                        let row = ui.form_field(&btn_lbl, Length::rem(2.25));
                        let gap = theme.space_half();
                        let bw = ((row.w - gap * 4.0) / 5.0).floor();
                        let variants = [
                            (320u32, "Primary", ButtonVariant::Primary),
                            (321u32, "Outline", ButtonVariant::Outline),
                            (322u32, "Ghost", ButtonVariant::Ghost),
                            (323u32, "Success", ButtonVariant::Success),
                            (324u32, "Danger", ButtonVariant::Danger),
                        ];
                        for (i, (id, lab, var)) in variants.into_iter().enumerate() {
                            let br = Rect::new(row.x + (bw + gap) * i as f32, row.y, bw, row.h);
                            if ui.button(id, br, lab, "", var) {
                                self.click_counter += 1;
                            }
                        }

                        let b_row = ui.take(Length::rem(2.0));
                        let badge_h = (rem * 1.48).round();
                        let by = b_row.y + (b_row.h - badge_h) * 0.5;
                        let mut bx = b_row.x;
                        for (lab, var) in [
                            ("Primary", BadgeVariant::Primary),
                            ("Success", BadgeVariant::Success),
                            ("Warning", BadgeVariant::Warning),
                            ("Danger", BadgeVariant::Danger),
                        ] {
                            bx += ui.badge(bx, by, lab, var, None) + theme.space_half();
                        }
                    });

                    ui.card(col_r, Length::rem(1.25), Length::rem(0.9), |ui| {
                        select_r = ui.input_group("nui-select · popup dropdown (ID 360)");

                        let c1 = ui.take(Length::rem(2.0));
                        if ui.checkbox(330, c1, "Preserve formants", &mut self.chk_formant) {
                            self.click_counter += 0;
                        }
                        let c2 = ui.take(Length::rem(2.0));
                        ui.checkbox(331, c2, "Show alignment grid", &mut self.chk_grid);

                        let s1 = ui.take(Length::rem(2.0));
                        ui.switch(332, s1, "High-quality oversampling", &mut self.sw_hq);
                        let s2 = ui.take(Length::rem(2.0));
                        ui.switch(333, s2, "Keep running when hidden", &mut self.sw_tray);

                        let sl1 = ui.take(Length::rem(2.9));
                        let l1 = format!("{:.0}%", self.slider_std);
                        let _ = ui.slider(
                            340,
                            sl1,
                            "Standard slider (0..100)",
                            &mut self.slider_std,
                            0.0,
                            100.0,
                            50.0,
                            &l1,
                        );

                        let sl2 = ui.take(Length::rem(2.9));
                        let l2 = format!("{:+.1} st", self.slider_bipolar);
                        let _ = ui.slider(
                            341,
                            sl2,
                            "Bipolar slider (-12..+12)",
                            &mut self.slider_bipolar,
                            -12.0,
                            12.0,
                            0.0,
                            &l2,
                        );

                        let rr = ui.form_field("nui-radio · interpolation", Length::rem(2.0));
                        let rw = (rr.w / 4.0).floor();
                        for (i, lab) in ["Nearest", "Linear", "Cubic", "Sinc"].iter().enumerate() {
                            if ui.radio(
                                350 + i as u32,
                                Rect::new(rr.x + i as f32 * rw, rr.y, rw, rr.h),
                                lab,
                                self.radio_choice == i,
                            ) {
                                self.radio_choice = i;
                            }
                        }
                    });

                    // Drawn after both columns so the popup layers above every sibling.
                    ui.select(360, select_r, DEVICES, &mut self.selected_device);
                }

                // ============================================================
                // TAB 1 — Layout & Cards
                // ============================================================
                1 => {
                    let grid = ui.layout_grid(body, 2, Length::rem(1.25));
                    let (lc, rc) = if grid.len() >= 2 {
                        (grid[0], grid[1])
                    } else {
                        let g = grid[0];
                        let [top, bottom] =
                            ui.rows(g, Length::rem(1.25), [Length::fill(), Length::fill()]);
                        (top, bottom)
                    };

                    ui.nui_card(lc, |ui| {
                        ui.heading("nui_card · kv_row");
                        ui.kv_row("theme", "slate v3.0.0", Length::rem(8.0));
                        ui.kv_row("rem", &format!("{rem:.1} px"), Length::rem(8.0));
                        ui.kv_row("zoom", &format!("{:.0}%", self.ui_zoom * 100.0), Length::rem(8.0));
                        ui.kv_row(
                            "backend",
                            match active_backend {
                                BackendKind::Gpu => "GPU · instanced SDF",
                                BackendKind::Cpu => "CPU · softbuffer",
                            },
                            Length::rem(8.0),
                        );
                        ui.kv_row(
                            "font engine",
                            match self.font_engine {
                                FontEngine::Swash => "Swash",
                                FontEngine::Fontdue => "Fontdue",
                            },
                            Length::rem(8.0),
                        );
                        ui.kv_row("active tab", &format!("{} / 4", self.active_tab), Length::rem(8.0));
                        ui.spacer(Length::rem(0.35));

                        ui.heading("Spacing tokens");
                        ui.kv_row("space", &format!("{:.0} px", theme.space()), Length::rem(8.0));
                        ui.kv_row(
                            "space_half",
                            &format!("{:.0} px", theme.space_half()),
                            Length::rem(8.0),
                        );
                        ui.kv_row(
                            "space_double",
                            &format!("{:.0} px", theme.space_double()),
                            Length::rem(8.0),
                        );

                        ui.spacer(Length::rem(0.35));
                        ui.heading("cols([Rem(7), Fill(1)])");
                        let row = ui.take(Length::rem(3.6));
                        let [fixed_r, fill_r] =
                            ui.cols(row, Length::rem(0.5), [Length::rem(7.0), Length::fill()]);
                        ui.inset_card(
                            fixed_r,
                            Length::rem(0.6),
                            Length::rem(0.2),
                            theme.border2,
                            |ui| {
                                ui.heading("Rem(7.0)");
                                let fs = theme.font_small();
                                let b = ui.bounds();
                                ui.text_clipped(
                                    FontFace::Regular,
                                    ui.font_engine,
                                    "fixed column",
                                    b.x.round(),
                                    (b.y + fs).round(),
                                    fs,
                                    b.w,
                                    theme.text_muted,
                                );
                            },
                        );
                        ui.inset_card(
                            fill_r,
                            Length::rem(0.6),
                            Length::rem(0.2),
                            theme.highlight_dim,
                            |ui| {
                                ui.heading("Fill(1.0)");
                                let fs = theme.font_small();
                                let b = ui.bounds();
                                ui.text_clipped(
                                    FontFace::Regular,
                                    ui.font_engine,
                                    "takes the remainder",
                                    b.x.round(),
                                    (b.y + fs).round(),
                                    fs,
                                    b.w,
                                    theme.highlight,
                                );
                            },
                        );

                        ui.spacer(Length::rem(0.35));
                        ui.heading("layout_grid(3 cols)");
                        let g = ui.take(Length::rem(3.2));
                        for (i, r) in ui.layout_grid(g, 3, Length::rem(0.5)).into_iter().enumerate() {
                            ui.scene.rounded_rect(
                                r.x,
                                r.y,
                                r.w,
                                r.h,
                                theme.radius1(),
                                theme.shade3,
                                theme.border3,
                                1.0,
                            );
                            let fs = theme.font_small();
                            ui.text_clipped(
                                FontFace::SemiBold,
                                ui.font_engine,
                                &format!("col {i}"),
                                (r.x + theme.space_half()).round(),
                                (r.y + r.h * 0.5 + fs * 0.33).round(),
                                fs,
                                (r.w - theme.space()).max(0.0),
                                theme.text,
                            );
                        }
                    });

                    ui.nui_card(rc, |ui| {
                        ui.heading("Theme tokens");
                        let left_tokens: [(&str, Color); 10] = [
                            ("base", theme.base),
                            ("shade1", theme.shade1),
                            ("shade2", theme.shade2),
                            ("shade3", theme.shade3),
                            ("shade4", theme.shade4),
                            ("shade5", theme.shade5),
                            ("shade6", theme.shade6),
                            ("shade7", theme.shade7),
                            ("shade8", theme.shade8),
                            ("shade9", theme.shade9),
                        ];
                        let right_tokens: [(&str, Color); 10] = [
                            ("border1", theme.border1),
                            ("border2", theme.border2),
                            ("border3", theme.border3),
                            ("border4", theme.border4),
                            ("highlight", theme.highlight),
                            ("highlight_dim", theme.highlight_dim),
                            ("palette_gray", theme.palette_gray),
                            ("palette_mark", theme.palette_mark),
                            ("palette_activate", theme.palette_activate),
                            ("palette_alert", theme.palette_alert),
                        ];
                        let rest = ui.take_rest();
                        let [c1, c2] = ui.cols(rest, Length::rem(0.75), [Length::fill(), Length::fill()]);
                        ui.with_column(c1, Length::rem(0.15), |ui| {
                            for &(name, col) in left_tokens.iter() {
                                let r = ui.take(Length::rem(2.1));
                                draw_swatch(ui, r, name, col, theme);
                            }
                        });
                        ui.with_column(c2, Length::rem(0.15), |ui| {
                            for &(name, col) in right_tokens.iter() {
                                let r = ui.take(Length::rem(2.1));
                                draw_swatch(ui, r, name, col, theme);
                            }
                        });
                    });
                }

                // ============================================================
                // TAB 2 — Vectors & Canvas
                // ============================================================
                2 => {
                    let [wave_r, row1, row2] = ui.rows(
                        body,
                        Length::rem(1.25),
                        [Length::rem(10.0), Length::fill(), Length::fill()],
                    );
                    let harmonic = harmonic_points(self.t);
                    let lissajous = lissajous_points(self.t);

                    ui.card(wave_r, Length::rem(1.25), Length::rem(0.6), |ui| {
                        let b = ui.bounds();
                        let [hdr, wf] =
                            ui.rows(b, Length::rem(0.6), [Length::rem(2.4), Length::fill()]);
                        let [hl, sw_r] = ui.cols(
                            hdr,
                            Length::rem(0.75),
                            [Length::fill(), Length::rem(11.0)],
                        );
                        let badge_h = (rem * 1.48).round();
                        let bw = ui.badge(
                            hl.x,
                            hl.y + (hl.h - badge_h) * 0.5,
                            "KIND 4 · CAPSULE LINES",
                            BadgeVariant::Primary,
                            None,
                        );
                        let fs = theme.font_small();
                        let tx = (hl.x + bw + theme.space()).round();
                        ui.text_clipped(
                            FontFace::Regular,
                            ui.font_engine,
                            "96 synthetic peaks · drag to scrub the playhead",
                            tx,
                            (hl.y + hl.h * 0.5 + fs * 0.33).round(),
                            fs,
                            (hl.x + hl.w - tx).max(0.0),
                            theme.text_muted,
                        );
                        ui.switch(701, sw_r, "Animate", &mut self.animate);
                        ui.waveform(700, wf, &self.wave_peaks, &mut self.playhead);
                    });

                    let [poly_r, lissa_r] =
                        ui.cols(row1, Length::rem(1.25), [Length::fill(), Length::fill()]);
                    ui.card(poly_r, Length::rem(1.25), Length::rem(0.5), |ui| {
                        ui.heading("ui.polyline · multi-harmonic sine");
                        let plot = ui.take_rest();
                        ui.scene.rounded_rect(
                            plot.x,
                            plot.y,
                            plot.w,
                            plot.h,
                            theme.radius1(),
                            theme.base,
                            theme.border2,
                            1.0,
                        );
                        let mid = plot.y + plot.h * 0.5;
                        ui.scene.rect(plot.x + 1.0, mid, (plot.w - 2.0).max(1.0), 1.0, theme.border1);
                        ui.polyline(plot, &harmonic, 2.0, theme.highlight);
                    });

                    ui.card(lissa_r, Length::rem(1.25), Length::rem(0.5), |ui| {
                        ui.heading("ui.polyline · Lissajous 3:2");
                        let plot = ui.take_rest();
                        ui.scene.rounded_rect(
                            plot.x,
                            plot.y,
                            plot.w,
                            plot.h,
                            theme.radius1(),
                            theme.base,
                            theme.border2,
                            1.0,
                        );
                        ui.polyline(plot, &lissajous, 1.5, theme.palette_activate);
                    });

                    let [dash_r, fan_r] =
                        ui.cols(row2, Length::rem(1.25), [Length::fill(), Length::fill()]);
                    ui.card(dash_r, Length::rem(1.25), Length::rem(0.5), |ui| {
                        ui.heading("Scene::dashed_rect");
                        let area = ui.take_rest();
                        ui.scene.dashed_rect(
                            area.x + 2.0,
                            area.y + 2.0,
                            (area.w - 4.0).max(1.0),
                            (area.h - 4.0).max(1.0),
                            (rem * 0.55).round(),
                            (rem * 0.35).round(),
                            2.0,
                            theme.highlight,
                        );
                        let fs = theme.font_small();
                        ui.text_clipped(
                            FontFace::Regular,
                            ui.font_engine,
                            "4 edges · anti-aliased capsule dashes",
                            (area.x + theme.space()).round(),
                            (area.y + area.h - theme.space()).round(),
                            fs,
                            (area.w - theme.space_double()).max(0.0),
                            theme.text_muted,
                        );
                    });

                    ui.card(fan_r, Length::rem(1.25), Length::rem(0.5), |ui| {
                        ui.heading("Anti-aliased line fan (Scene::line)");
                        let area = ui.take_rest();
                        let cx = area.x + area.w * 0.5;
                        let cy = area.y + area.h * 0.5;
                        let radius = (area.h.min(area.w) * 0.42).max(8.0);
                        for i in 0..36 {
                            let a = i as f32 / 36.0 * std::f32::consts::TAU + self.t * 0.25;
                            let col = if i % 3 == 0 {
                                theme.highlight
                            } else if i % 3 == 1 {
                                theme.palette_activate
                            } else {
                                theme.palette_mark
                            };
                            ui.scene.line(
                                cx,
                                cy,
                                cx + a.cos() * radius,
                                cy + a.sin() * radius,
                                1.5,
                                col,
                            );
                        }
                        ui.scene.rounded_rect(
                            cx - 3.0,
                            cy - 3.0,
                            6.0,
                            6.0,
                            3.0,
                            theme.highlight,
                            (0, 0, 0, 0),
                            0.0,
                        );
                    });
                }

                // ============================================================
                // TAB 3 — Typography & Icons
                // ============================================================
                3 => {
                    let [cmp, icons_r] =
                        ui.rows(body, Length::rem(1.25), [Length::fill(), Length::rem(13.0)]);
                    let [a, b] =
                        ui.cols(cmp, Length::rem(1.25), [Length::fill(), Length::fill()]);

                    ui.inset_card(
                        a,
                        Length::rem(1.25),
                        Length::rem(0.4),
                        theme.highlight_dim,
                        |ui| {
                            ui.heading("Swash — Hinted + 4× Subpixel + Linear Gamma");
                            for px in [12.0f32, 14.0, 16.0, 18.0, 22.0] {
                                let r = ui.take(Length::rem(3.0));
                                let fs = rem * (px / 16.0);
                                ui.text_clipped(
                                    FontFace::SemiBold,
                                    ui.font_engine,
                                    &format!("{px:.0}px"),
                                    r.x.round(),
                                    (r.y + r.h * 0.5 + theme.font_xsmall() * 0.33).round(),
                                    theme.font_xsmall(),
                                    rem * 3.4,
                                    theme.highlight,
                                );
                                ui.text_clipped(
                                    FontFace::Regular,
                                    FontEngine::Swash,
                                    TYPO_TEXT,
                                    (r.x + rem * 3.6).round(),
                                    (r.y + r.h * 0.5 + fs * 0.33).round(),
                                    fs,
                                    (r.w - rem * 3.6).max(0.0),
                                    theme.text,
                                );
                            }
                        },
                    );

                    ui.inset_card(
                        b,
                        Length::rem(1.25),
                        Length::rem(0.4),
                        theme.border2,
                        |ui| {
                            ui.heading("Fontdue — Unhinted + sRGB Alpha");
                            for px in [12.0f32, 14.0, 16.0, 18.0, 22.0] {
                                let r = ui.take(Length::rem(3.0));
                                let fs = rem * (px / 16.0);
                                ui.text_clipped(
                                    FontFace::SemiBold,
                                    ui.font_engine,
                                    &format!("{px:.0}px"),
                                    r.x.round(),
                                    (r.y + r.h * 0.5 + theme.font_xsmall() * 0.33).round(),
                                    theme.font_xsmall(),
                                    rem * 3.4,
                                    theme.text_dim,
                                );
                                ui.text_clipped(
                                    FontFace::Regular,
                                    FontEngine::Fontdue,
                                    TYPO_TEXT,
                                    (r.x + rem * 3.6).round(),
                                    (r.y + r.h * 0.5 + fs * 0.33).round(),
                                    fs,
                                    (r.w - rem * 3.6).max(0.0),
                                    theme.text,
                                );
                            }
                        },
                    );

                    ui.nui_card(icons_r, |ui| {
                        ui.heading("Material Icons — 23 constants · layout_icon_centered");
                        let area = ui.take_rest();
                        let cols = 5usize;
                        let cell_w = (area.w / cols as f32).floor();
                        let cell_h = (area.h / 5.0).floor();
                        for (i, &(glyph, name)) in ICONS.iter().enumerate() {
                            let cx = area.x + (i % cols) as f32 * cell_w;
                            let cy = area.y + (i / cols) as f32 * cell_h;
                            ui.scene.rounded_rect(
                                cx + 2.0,
                                cy + 2.0,
                                (cell_w - 4.0).max(1.0),
                                (cell_h - 4.0).max(1.0),
                                theme.radius1(),
                                theme.shade2,
                                theme.border1,
                                1.0,
                            );
                            let isz = (rem * 1.5).round();
                            ui.scene.icon_centered(
                                ui.text_sys,
                                glyph,
                                cx + rem * 1.75,
                                cy + cell_h * 0.5,
                                isz,
                                theme.highlight,
                            );
                            let fs = theme.font_small();
                            ui.text_clipped(
                                FontFace::Regular,
                                ui.font_engine,
                                name,
                                (cx + rem * 3.1).round(),
                                (cy + cell_h * 0.5 + fs * 0.33).round(),
                                fs,
                                (cell_w - rem * 3.4).max(0.0),
                                theme.text_dim,
                            );
                        }
                    });
                }

                // ============================================================
                // TAB 4 — Virtual List (10k)
                // ============================================================
                4 => {
                    // Rebuild the filtered index set only when the query changes.
                    let needle = self.filter.trim().to_lowercase();
                    if self.filter != self.filter_used {
                        self.filter_used = self.filter.clone();
                        self.filtered.clear();
                        for i in 0..ROW_COUNT {
                            if needle.is_empty() || track_name(i).to_lowercase().contains(&needle) {
                                self.filtered.push(i);
                            }
                        }
                        if !self.filtered.is_empty() && !self.filtered.contains(&self.selected_row) {
                            self.selected_row = self.filtered[0];
                        }
                    }

                    let [hdr_r, list_r, det_r] = ui.rows(
                        body,
                        Length::rem(1.0),
                        [Length::rem(2.5), Length::fill(), Length::rem(13.0)],
                    );

                    let [filter_r, badge_r] = ui.cols(
                        hdr_r,
                        Length::rem(0.75),
                        [Length::rem(26.0), Length::fill()],
                    );
                    ui.text_input(801, filter_r, "Filter 10,000 rows…", &mut self.filter);
                    let badge_h = (rem * 1.48).round();
                    let count_lbl = format!("{} MATCHING", self.filtered.len());
                    let _ = ui.badge(
                        badge_r.x,
                        badge_r.y + (badge_r.h - badge_h) * 0.5,
                        &count_lbl,
                        BadgeVariant::Primary,
                        None,
                    );
                    let fs = theme.font_small();
                    let info_x = badge_r.x + rem * 11.0;
                    ui.text_clipped(
                        FontFace::Regular,
                        ui.font_engine,
                        "Only the visible rows are emitted each frame",
                        info_x.round(),
                        (badge_r.y + badge_r.h * 0.5 + fs * 0.33).round(),
                        fs,
                        (badge_r.x + badge_r.w - info_x).max(0.0),
                        theme.text_muted,
                    );

                    let row_h = (rem * 2.25).round();
                    ui.virtual_list(810, list_r, self.filtered.len(), row_h, |ui, idx, rr| {
                        let orig = self.filtered[idx];
                        let row_id = 40_000u32.wrapping_add(idx as u32);
                        let selected = orig == self.selected_row;
                        let index_str = format!("#{:05}", orig + 1);
                        let title = track_name(orig);
                        let meta = format!("{} · {}", track_artist(orig), track_album(orig));
                        if ui.list_row(row_id, rr, selected, &index_str, &title, &meta) {
                            self.selected_row = orig;
                        }
                    });

                    ui.nui_card(det_r, |ui| {
                        ui.heading("Selected row");
                        let i = self.selected_row;
                        ui.kv_row("index", &format!("#{:05}", i + 1), Length::rem(7.0));
                        ui.kv_row("title", &track_name(i), Length::rem(7.0));
                        ui.kv_row("artist", track_artist(i), Length::rem(7.0));
                        ui.kv_row("album", track_album(i), Length::rem(7.0));
                        ui.kv_row("format", &track_meta(i), Length::rem(7.0));
                    });
                }

                // ============================================================
                // TAB 5 — Menus & Overlays
                // ============================================================
                5 => {
                    let [col_l, col_r] =
                        ui.cols(body, Length::rem(1.25), [Length::fill(), Length::fill()]);

                    ui.nui_card(col_l, |ui| {
                        ui.heading("nui-menu, nui-context-menu & nui-dialog");
                        ui.kv_row(
                            "last menu action",
                            &self.last_menu_action,
                            Length::rem(9.0),
                        );

                        let [dlg_r, ctx_r] = ui.take_cols(
                            Length::rem(2.0),
                            Length::rem(0.6),
                            [Length::fill(), Length::fill()],
                        );
                        if ui.button(
                            560,
                            dlg_r,
                            "Open Diagnostics Dialog",
                            ICON_INFO,
                            ButtonVariant::Primary,
                        ) {
                            self.show_status_dialog = true;
                        }
                        if ui.button(
                            561,
                            ctx_r,
                            "Pop Context Menu",
                            ICON_MORE_VERT,
                            ButtonVariant::Outline,
                        ) {
                            ui.state.open_popup_id = Some(550);
                            ui.state.active_submenu = None;
                            ui.state.context_menu_pos =
                                (col_l.x + col_l.w * 0.5, col_l.y + col_l.h * 0.55);
                        }

                        let [sb_r, insp_r] = ui.take_cols(
                            Length::rem(2.0),
                            Length::rem(0.6),
                            [Length::fill(), Length::fill()],
                        );
                        if ui.button(
                            562,
                            sb_r,
                            "Toggle Sidebar",
                            ICON_MENU,
                            ButtonVariant::Outline,
                        ) {
                            self.sidebar_open = !self.sidebar_open;
                        }
                        if ui.button(
                            563,
                            insp_r,
                            "Inspector Window (W)",
                            ICON_SETTINGS,
                            ButtonVariant::Outline,
                        ) {
                            self.pending_window_toggles.push("inspector");
                        }

                        let ctx_zone = ui.take_rest();
                        ui.inset_card(
                            ctx_zone,
                            Length::rem(0.9),
                            Length::rem(0.5),
                            theme.border2,
                            |ui| {
                                ui.heading("nui-context-menu");
                                let b = ui.bounds();
                                let fs = theme.font_small();
                                ui.text_clipped(
                                    FontFace::Regular,
                                    ui.font_engine,
                                    "Right-click anywhere in the workspace to open nui-context-menu (with nested submenus, checkmarks, and shortcuts).",
                                    b.x.round(),
                                    (b.y + fs).round(),
                                    fs,
                                    b.w,
                                    theme.text_muted,
                                );
                            },
                        );
                    });

                    ui.nui_card(col_r, |ui| {
                        ui.heading("Animation System & Transition Polish");

                        let live_anim = ui.state.is_animating();
                        let st_row = ui.take(Length::rem(1.9));
                        let badge_h = (rem * 1.48).round();
                        let by = (st_row.y + (st_row.h - badge_h) * 0.5).round();
                        let (st_lbl, st_var) = if live_anim {
                            ("TRANSITION ACTIVE (60Hz)", BadgeVariant::Success)
                        } else {
                            ("0% CPU IDLE (ControlFlow::Wait)", BadgeVariant::Primary)
                        };
                        let _ = ui.badge(st_row.x, by, st_lbl, st_var, None);

                        let s1 = ui.take(Length::rem(2.0));
                        ui.switch(570, s1, "Smooth thumb sliding (0.30s easeInOut)", &mut self.anim_thumb);
                        let s2 = ui.take(Length::rem(2.0));
                        ui.switch(
                            571,
                            s2,
                            "Button click flash (0.10s)",
                            &mut self.anim_flash,
                        );

                        let c1 = ui.take(Length::rem(2.0));
                        ui.checkbox(572, c1, "Check pop-in", &mut self.anim_check);
                        let c2 = ui.take(Length::rem(2.0));
                        ui.checkbox(573, c2, "Preserve formants", &mut self.anim_check2);

                        let rr = ui.form_field("nui-radio · easing curve", Length::rem(2.0));
                        let rw = (rr.w / 4.0).floor();
                        for (i, lab) in ["Linear", "Ease", "Spring", "Bounce"]
                            .iter()
                            .enumerate()
                        {
                            if ui.radio(
                                580 + i as u32,
                                Rect::new(rr.x + i as f32 * rw, rr.y, rw, rr.h),
                                lab,
                                self.anim_easing == i,
                            ) {
                                self.anim_easing = i;
                            }
                        }

                        let sel_r = ui.input_group("nui-select · dropdown slide-open");
                        ui.select(590, sel_r, TRANSITION_PRESETS, &mut self.anim_duration);
                    });
                }
                // `active_tab` is clamped to 0..=5 at every write site (CLI, menu,
                // sidebar, `nui-tabs`); this arm exists only to satisfy usize
                // exhaustiveness.
                _ => {}
            }

            // ================================================================
            // 4. Overlay layers — context menu (right-click), then the modal dialog.
            //    Drawn last so they sit above every sibling in the display list.
            // ================================================================
            let ctx_items: &[MenuItem<'_>] = &[
                MenuItem::Action {
                    label: "Status & Demo Controls…",
                    icon: "",
                    shortcut: "F1",
                    action: "diag:dialog",
                    disabled: false,
                    checked: Some(self.show_status_dialog),
                },
                MenuItem::Action {
                    label: "Show Sidebar",
                    icon: "",
                    shortcut: "Ctrl+B",
                    action: "view:sidebar",
                    disabled: false,
                    checked: Some(self.sidebar_open),
                },
                MenuItem::Action {
                    label: "Dark Theme",
                    icon: "",
                    shortcut: "T",
                    action: "view:theme",
                    disabled: false,
                    checked: Some(self.dark_mode),
                },
                MenuItem::Action {
                    label: "Clear Primary Input",
                    icon: "",
                    shortcut: "Ctrl+L",
                    action: "file:clear",
                    disabled: false,
                    checked: None,
                },
                MenuItem::Separator,
                MenuItem::Submenu {
                    label: "UI Zoom",
                    icon: "",
                    disabled: false,
                    items: zoom_items,
                },
            ];
            ctx_action = if self.show_status_dialog {
                // The modal owns the popup slot and its backdrop — a right-click must
                // not open a context menu underneath it.
                None
            } else {
                ui.context_menu(
                    550,
                    Rect::new(main_x, shell_y, main_w, shell_h),
                    Rect::new(0.0, 0.0, pw, ph),
                    ctx_items,
                )
            };

            ui.dialog(
                950,
                Rect::new(0.0, 0.0, pw, ph),
                &mut self.show_status_dialog,
                "Diagnostics, Status & Demo Controls",
                ICON_SETTINGS,
                (rem * 38.0).round().min(pw - rem * 2.0),
                (rem * 24.5).round().min(ph - rem * 2.0),
                |ui, body_r| {
                    ui.with_column(ui.pad(body_r, Length::rem(1.25)), Length::rem(0.75), |ui| {
                        let st_row = ui.take(Length::rem(1.8));
                        let badge_y = (st_row.y + (st_row.h - (rem * 1.48).round()) * 0.5).round();
                        let mut bx = st_row.x;
                        let (b_lbl, b_var) = match active_backend {
                            BackendKind::Gpu => ("GPU · GL 3.3 SDF", BadgeVariant::Primary),
                            BackendKind::Cpu => ("CPU · SOFTBUFFER", BadgeVariant::Warning),
                        };
                        bx += ui.badge(bx, badge_y, b_lbl, b_var, Some(true))
                            + theme.space_half();

                        let f_lbl = match self.font_engine {
                            FontEngine::Swash => "SWASH",
                            FontEngine::Fontdue => "FONTDUE",
                        };
                        bx += ui.badge(bx, badge_y, f_lbl, BadgeVariant::Primary, None)
                            + theme.space_half();

                        let fps_lbl = if self.last_fps > 0 {
                            format!("{} FPS", self.last_fps)
                        } else {
                            "0% IDLE".to_string()
                        };
                        let _ = ui.badge(bx, badge_y, &fps_lbl, BadgeVariant::Success, None);

                        let anim_lbl = if ui.state.is_animating() {
                            "yes"
                        } else {
                            "no"
                        };
                        let left: [(&str, String); 3] = [
                            (
                                "active backend",
                                match active_backend {
                                    BackendKind::Gpu => "GPU · instanced SDF".to_string(),
                                    BackendKind::Cpu => "CPU · softbuffer".to_string(),
                                },
                            ),
                            (
                                "font engine",
                                match self.font_engine {
                                    FontEngine::Swash => "Swash".to_string(),
                                    FontEngine::Fontdue => "Fontdue".to_string(),
                                },
                            ),
                            (
                                "theme",
                                if self.dark_mode { "dark" } else { "light" }.to_string(),
                            ),
                        ];
                        let right: [(&str, String); 3] = [
                            ("ui zoom", format!("{:.0}%", self.ui_zoom * 100.0)),
                            (
                                "quad instances",
                                format!("{}", ui.scene.instances.len()),
                            ),
                            ("animating", anim_lbl.to_string()),
                        ];

                        let diag_r = ui.take(Length::rem(7.0));
                        let [d1, d2] =
                            ui.cols(diag_r, Length::rem(1.0), [Length::fill(), Length::fill()]);
                        ui.with_column(d1, Length::rem(0.0), |ui| {
                            for (k, v) in left.iter() {
                                ui.kv_row(k, v, Length::rem(8.5));
                            }
                        });
                        ui.with_column(d2, Length::rem(0.0), |ui| {
                            for (k, v) in right.iter() {
                                ui.kv_row(k, v, Length::rem(8.5));
                            }
                        });

                        let r1 = ui.take(Length::rem(2.0));
                        let [a1, a2, a3] = ui.cols(
                            r1,
                            Length::rem(0.6),
                            [Length::fill(), Length::fill(), Length::fill()],
                        );
                        if ui.button(960, a1, "Backend [B]", "", ButtonVariant::Primary) {
                            toggle_backend_req = true;
                        }
                        if ui.button(961, a2, "Font [F]", "", ButtonVariant::Outline) {
                            toggle_font_req = true;
                        }
                        if ui.button(962, a3, "Theme [T]", "", ButtonVariant::Outline) {
                            toggle_theme_req = true;
                        }

                        let zoom_lbl = format!("{:.0}% Reset", self.ui_zoom * 100.0);
                        let r2 = ui.take(Length::rem(2.0));
                        let [b1, b2, b3] = ui.cols(
                            r2,
                            Length::rem(0.6),
                            [Length::fill(), Length::rem(6.5), Length::fill()],
                        );
                        if ui.button(963, b1, "Zoom -", "", ButtonVariant::Outline) {
                            zoom_delta = -0.1;
                        }
                        if ui.button(964, b2, &zoom_lbl, "", ButtonVariant::Outline) {
                            zoom_delta = 1.0 - self.ui_zoom;
                        }
                        if ui.button(965, b3, "Zoom +", "", ButtonVariant::Outline) {
                            zoom_delta = 0.1;
                        }

                        let r3 = ui.take(Length::rem(2.0));
                        let [c1, c2, c3] = ui.cols(
                            r3,
                            Length::rem(0.6),
                            [Length::fill(), Length::fill(), Length::fill()],
                        );
                        if ui.button(966, c1, "Dropzone [D]", "", ButtonVariant::Outline)
                        {
                            toggle_drop_req = true;
                        }
                        if ui.button(967, c2, "Inspector [W]", "", ButtonVariant::Outline) {
                            self.pending_window_toggles.push("inspector");
                        }
                        let anim_var = if self.animate {
                            ButtonVariant::Success
                        } else {
                            ButtonVariant::Outline
                        };
                        if ui.button(968, c3, "60fps [Space]", "", anim_var) {
                            self.animate = !self.animate;
                        }
                    });
                },
            );

            // Synthetic UI complexity stress test (`--ops N`).
            let iw = (pw as usize).max(1);
            let ih = (ph as usize).max(1);
            for i in 0..self.extra_ops {
                let x = ((i * 37) % iw) as f32;
                let y = ((i * 53) % ih) as f32;
                ui.scene
                    .rect(x, y, 9.0 * scale, 7.0 * scale, ((i % 200) as u8, 120, 160, 255));
            }

            // Full-window `nui-dropzone` overlay (OS drag-and-drop).
            if let Some(path) = ui.dropzone_overlay(
                Rect::new(0.0, 0.0, pw, ph),
                "Drop a file to fill the primary input",
            ) {
                self.input_main = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.to_string_lossy().into_owned());
            }

            ui.scene.finish_frame();
            ui.state.end_frame();
        }

        for action in [menu_action, ctx_action].into_iter().flatten() {
            self.apply_nav_action(
                &action,
                &mut toggle_backend_req,
                &mut toggle_font_req,
                &mut toggle_theme_req,
                &mut toggle_drop_req,
                &mut close_req,
            );
        }

        if toggle_backend_req {
            if let Some(r) = &mut self.renderer {
                r.toggle_backend();
            }
        }
        if toggle_font_req {
            self.font_engine = match self.font_engine {
                FontEngine::Swash => FontEngine::Fontdue,
                FontEngine::Fontdue => FontEngine::Swash,
            };
        }
        if toggle_theme_req {
            self.dark_mode = !self.dark_mode;
        }
        if toggle_drop_req {
            self.ui_state.hovered_file = if self.ui_state.hovered_file.is_some() {
                None
            } else {
                Some(std::path::PathBuf::from("D:\\drop-preview.flac"))
            };
        }
        if zoom_delta.abs() > 0.001 {
            self.set_zoom(self.ui_zoom + zoom_delta);
        }
        if close_req {
            self.quitting = true;
            self.quit_at = Some(Instant::now());
        }
        if toggle_backend_req || toggle_font_req || toggle_theme_req || close_req {
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
            self.count_frame();
            return;
        }

        self.build_scene(w, h, scale);
        if let Some(renderer) = &mut self.renderer {
            // With --open-window the screenshot belongs to that secondary window.
            let shot = if self.open_window_spec.is_some() {
                None
            } else {
                self.screenshot_path.take()
            };
            renderer.render(w, h, &self.scene, &mut self.text_sys, shot.as_deref());
            if let Some(path) = shot {
                println!("[nui-demo] saved screenshot to {path}");
            }
        }

        self.count_frame();

        if !self.reported {
            self.reported = true;
            let bname = match self.renderer.as_ref().map(|r| r.active) {
                Some(BackendKind::Gpu) => "gpu-gl33",
                Some(BackendKind::Cpu) => "cpu-softbuffer",
                None => "none",
            };
            println!(
                "[nui-demo] first frame at {:.1} ms (window {}x{}, scale {:.2}, backend {})",
                self.start.elapsed().as_secs_f64() * 1000.0,
                w,
                h,
                scale,
                bname
            );
        }
    }

    fn count_frame(&mut self) {
        self.frames += 1;
        if self.fps_at.elapsed() >= Duration::from_millis(1000) {
            self.last_fps = self.frames;
            println!("[nui-demo] {} fps", self.frames);
            self.frames = 0;
            self.fps_at = Instant::now();
        }
    }

    /// Execute queued inspector show/hide toggles where `event_loop` is available.
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
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn secondary_window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        id: WindowId,
        event: WindowEvent,
    ) {
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
                    if let Some(w) = self.secondary_windows.find_by_key_mut(key) {
                        match state {
                            ElementState::Pressed => {
                                if nui::try_start_window_resize(
                                    &w.window,
                                    w.ui_state.cursor,
                                    w.size,
                                ) {
                                    return;
                                }
                                let rem = 16.0 * w.window.scale_factor() as f32;
                                let hdr_h = (rem * 3.0).round();
                                if w.ui_state.cursor.1 < hdr_h
                                    && w.ui_state.cursor.0 < w.size.0 as f32 - rem * 3.0
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
                if event.state == ElementState::Pressed
                    && matches!(&event.logical_key, Key::Named(NamedKey::Escape))
                {
                    self.secondary_windows.hide(key);
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                self.render_secondary_window(key);
            }
            _ => {}
        }
    }

    /// Render the inspector: live `UiState` diagnostics of the PRIMARY window.
    fn render_secondary_window(&mut self, key: &'static str) {
        let Some(_spec) = window_spec_by_key(key) else {
            return;
        };

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

            let close_clicked;
            {
                let scene = &mut mw.scene;
                let ui_state = &mut mw.ui_state;
                scene.clear_all(page_bg);
                ui_state.begin_frame();

                let pw = w as f32;
                let ph = h as f32;
                let mut ui = Ui::new(ui_state, scene, &mut self.text_sys, self.font_engine, theme);

                let (hdr_h, close) = ui.secondary_window_header(
                    pw,
                    "NUI INSPECTOR",
                    ICON_SETTINGS,
                    "LIVE UI STATE",
                );
                close_clicked = close;

                let m = Length::Rem(1.25).resolve(rem).round();
                let card_r = Rect::new(
                    m,
                    hdr_h + m,
                    (pw - m * 2.0).max(0.0),
                    (ph - hdr_h - m * 2.0).max(80.0),
                );

                let s = &self.ui_state;
                let backend_lbl = match self.renderer.as_ref().map(|r| r.active) {
                    Some(BackendKind::Cpu) => "CPU · softbuffer".to_string(),
                    _ => "GPU · GL 3.3 SDF".to_string(),
                };
                let cursor_lbl = format!("{:.1}, {:.1}", s.cursor.0, s.cursor.1);
                let hovered = fmt_opt(s.hovered_id);
                let active = fmt_opt(s.active_id);
                let focused = fmt_opt(s.focused_id);
                let popup = fmt_opt(s.open_popup_id);
                let focus_n = format!("{} widgets", s.focus_order.len());
                let instances = format!("{}", self.scene.instances.len());
                let overlay_n = format!("{}", self.scene.overlay_instances.len());
                let theme_lbl = if self.dark_mode { "dark" } else { "light" };
                let zoom_lbl = format!("{:.0}%", self.ui_zoom * 100.0);
                let diag: [(&str, String); 11] = [
                    ("cursor", cursor_lbl),
                    ("hovered id", hovered),
                    ("active id", active),
                    ("focused id", focused),
                    ("open popup", popup),
                    ("focus ring", focus_n),
                    ("instances", instances),
                    ("overlay instances", overlay_n),
                    ("theme", theme_lbl.to_string()),
                    ("backend", backend_lbl),
                    ("zoom", zoom_lbl),
                ];
                let shortcuts: [(&str, &str); 8] = [
                    ("Tab / Shift+Tab", "focus ring"),
                    ("Ctrl+C / X / V / A", "clipboard"),
                    ("Ctrl+B / Ctrl+L", "sidebar / clear input"),
                    ("F1", "diagnostics dialog"),
                    ("B / F / T", "backend / font / theme"),
                    ("D / W", "dropzone / inspector"),
                    ("+ / - / 0", "zoom in / out / reset"),
                    ("Space / Esc", "animate / exit"),
                ];

                ui.nui_card(card_r, |ui| {
                    ui.heading("UiState diagnostics");
                    let d = ui.take(Length::rem(9.0));
                    let [d1, d2] =
                        ui.cols(d, Length::rem(1.0), [Length::fill(), Length::fill()]);
                    ui.with_column(d1, Length::rem(0.0), |ui| {
                        for (k, v) in diag.iter().take(6) {
                            ui.kv_row(k, v, Length::rem(8.5));
                        }
                    });
                    ui.with_column(d2, Length::rem(0.0), |ui| {
                        for (k, v) in diag.iter().skip(6) {
                            ui.kv_row(k, v, Length::rem(8.5));
                        }
                    });
                    ui.spacer(Length::rem(0.3));
                    ui.heading("Keyboard shortcuts");
                    let sc = ui.take(Length::rem(5.0));
                    let [s1, s2] =
                        ui.cols(sc, Length::rem(1.0), [Length::fill(), Length::fill()]);
                    ui.with_column(s1, Length::rem(0.0), |ui| {
                        for &(k, v) in shortcuts.iter().take(4) {
                            ui.kv_row(k, v, Length::rem(8.5));
                        }
                    });
                    ui.with_column(s2, Length::rem(0.0), |ui| {
                        for &(k, v) in shortcuts.iter().skip(4) {
                            ui.kv_row(k, v, Length::rem(8.5));
                        }
                    });
                });

                ui.scene.finish_frame();
                ui.state.end_frame();
            }
            (w, h, close_clicked, shot)
        };

        if close_clicked {
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
                println!("[nui-demo] saved screenshot to {p}");
            }
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let mut attrs = Window::default_attributes()
            .with_title("NUI Rust — Component Playground")
            .with_inner_size(winit::dpi::LogicalSize::new(1220.0, 740.0))
            .with_min_inner_size(winit::dpi::LogicalSize::new(900.0, 600.0))
            .with_resizable(true)
            .with_decorations(false);

        #[cfg(target_os = "windows")]
        {
            use winit::platform::windows::{CornerPreference, WindowAttributesExtWindows};
            attrs = attrs
                .with_undecorated_shadow(true)
                .with_corner_preference(CornerPreference::Round)
                .with_border_color(Some(winit::platform::windows::Color::from_rgb(55, 55, 55)));
        }

        let (window, mut renderer) = Renderer::new(event_loop, attrs, self.prefer_backend);
        renderer.request_reveal();
        let sz = window.inner_size();
        self.size = (sz.width, sz.height);
        self.window = Some(window.clone());
        self.renderer = Some(renderer);

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
        // The primary window owns the process lifetime; every other window is managed.
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
                if let Some(w) = &self.window {
                    nui::update_resize_cursor(w, self.ui_state.cursor, self.size);
                }
                // While the animation timer runs, `about_to_wait` already redraws;
                // only force a redraw for immediate drag feedback or when idle.
                if !self.animate || self.ui_state.mouse_down {
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
            WindowEvent::MouseInput { state, button, .. } => match button {
                MouseButton::Left => match state {
                    ElementState::Pressed => {
                        // Edge/corner resize wins over every in-window interaction.
                        if let Some(w) = self.window.clone() {
                            if nui::try_start_window_resize(&w, self.ui_state.cursor, self.size) {
                                return;
                            }
                        }
                        let scale = self
                            .window
                            .as_ref()
                            .map(|w| w.scale_factor() as f32 * self.ui_zoom)
                            .unwrap_or(1.0);
                        let rem = (16.0 * scale).round();
                        let hdr_h = (rem * 3.25).round();
                        // Drag by the empty header strip between the title and the
                        // right-hand button cluster.
                        if self.ui_state.cursor.1 < hdr_h
                            && self.ui_state.cursor.0 > rem * 3.0
                            && self.ui_state.cursor.0 < self.size.0 as f32 - rem * 10.0
                        {
                            if let Some(w) = &self.window {
                                nui::drag_window_live(w);
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
            },
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed {
                    let focused_text = matches!(
                        self.ui_state.focused_id,
                        Some(301) | Some(304) | Some(801)
                    );

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
                        Key::Named(NamedKey::F1) => {
                            self.show_status_dialog = true;
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
                            }
                        }
                        Key::Named(NamedKey::ArrowRight) => {
                            if focused_text {
                                self.ui_state.key_cmds.push(KeyCmd::Right);
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
                        Key::Character(ch) if self.modifiers.control_key() && ch.eq_ignore_ascii_case("l") => {
                            self.input_main.clear();
                        }
                        Key::Character(ch) if self.modifiers.control_key() && ch.eq_ignore_ascii_case("b") => {
                            self.sidebar_open = !self.sidebar_open;
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
                        Key::Character(ch) if !focused_text && ch.eq_ignore_ascii_case("d") => {
                            self.ui_state.hovered_file =
                                if self.ui_state.hovered_file.is_some() {
                                    None
                                } else {
                                    Some(std::path::PathBuf::from("D:\\drop-preview.flac"))
                                };
                        }
                        Key::Character(ch) if !focused_text && ch.eq_ignore_ascii_case("w") => {
                            self.pending_window_toggles.push("inspector");
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
                        Key::Named(NamedKey::Space) if !focused_text => {
                            self.animate = !self.animate;
                            if self.animate {
                                self.next_frame = Instant::now();
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
                    println!("[nui-demo] auto-quit reached");
                }
                event_loop.exit();
                return;
            }
        }

        // A widget transition (menu slide, sidebar collapse, dialog pop, button flash)
        // is animation too: keep the frame loop alive until every one has settled.
        let ui_anim = self.ui_state.is_animating() || self.secondary_windows.any_animating();
        let animating = self.animate || ui_anim;

        // Idle means 0% CPU: reset the FPS counter so the header badge reads IDLE.
        if !animating {
            self.last_fps = 0;
            self.frames = 0;
            self.fps_at = Instant::now();
        }

        let now = Instant::now();
        if animating && now >= self.next_frame {
            self.next_frame = now + Duration::from_millis(self.frame_ms);
            if self.animate {
                self.t += self.frame_ms as f32 / 1000.0;
            }
            if let Some(w) = &self.window {
                w.request_redraw();
            }
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
}

fn main() {
    let mut animate = false;
    let mut nopaint = false;
    let mut extra_ops: usize = 0;
    let mut seconds: Option<f64> = None;
    let mut fps: f64 = 60.0;
    let mut backend = BackendKind::Gpu;
    let mut font_engine = FontEngine::Swash;
    let mut screenshot: Option<String> = None;
    let mut open_select = false;
    let mut open_menu = false;
    let mut open_ctx = false;
    let mut open_dialog = false;
    let mut initial_tab: usize = 0;
    let mut drop_preview = false;
    let mut focus_preview = false;
    let mut open_window: Option<String> = None;

    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--animate" => animate = true,
            "--static" => animate = false,
            "--nopaint" => nopaint = true,
            "--open-select" => open_select = true,
            "--open-menu" => open_menu = true,
            "--open-context-menu" => open_ctx = true,
            "--open-dialog" => open_dialog = true,
            "--drop-preview" => drop_preview = true,
            "--focus-preview" => focus_preview = true,
            "--tab" => initial_tab = args.next().and_then(|v| v.parse().ok()).unwrap_or(0),
            "--open-window" => open_window = args.next(),
            "--backend" => {
                if let Some(b) = args.next() {
                    backend = if b.eq_ignore_ascii_case("cpu")
                        || b.eq_ignore_ascii_case("softbuffer")
                    {
                        BackendKind::Cpu
                    } else {
                        BackendKind::Gpu
                    };
                }
            }
            "--font" => {
                if let Some(f) = args.next() {
                    font_engine = if f.eq_ignore_ascii_case("fontdue") {
                        FontEngine::Fontdue
                    } else {
                        FontEngine::Swash
                    };
                }
            }
            "--ops" => extra_ops = args.next().and_then(|v| v.parse().ok()).unwrap_or(0),
            "--seconds" => seconds = args.next().and_then(|v| v.parse().ok()),
            "--fps" => fps = args.next().and_then(|v| v.parse().ok()).unwrap_or(60.0),
            "--screenshot" => screenshot = args.next(),
            other => eprintln!("[nui-demo] unknown argument: {other}"),
        }
    }

    let event_loop = EventLoop::new().unwrap();
    event_loop.set_control_flow(ControlFlow::Wait);
    let quit_at = seconds.map(|s| Instant::now() + Duration::from_secs_f64(s));

    let mut app = App::new(animate, quit_at, backend, font_engine);
    if fps > 0.0 {
        app.frame_ms = (1000.0 / fps).round().max(1.0) as u64;
    }
    app.nopaint = nopaint;
    app.extra_ops = extra_ops;
    app.screenshot_path = screenshot;
    app.active_tab = initial_tab.min(5);
    app.active_nav_key = format!("tab:{}", app.active_tab);
    app.open_window_spec = open_window.as_deref().and_then(window_spec_by_key);
    if let Some(k) = open_window.as_deref() {
        if app.open_window_spec.is_none() {
            eprintln!("[nui-demo] --open-window: unknown key '{k}' (inspector)");
        }
    }
    if open_select {
        app.active_tab = 0;
        app.active_nav_key = "tab:0".to_string();
        app.ui_state.open_popup_id = Some(360);
    }
    if open_dialog {
        app.show_status_dialog = true;
    }
    if open_menu {
        app.ui_state.open_popup_id = Some(501);
    }
    if open_ctx {
        app.ui_state.open_popup_id = Some(550);
        app.ui_state.context_menu_pos = (520.0, 260.0);
    }
    if drop_preview {
        app.ui_state.hovered_file =
            Some(std::path::PathBuf::from("D:\\drop-preview.flac"));
    }
    if focus_preview {
        app.ui_state.focused_id = Some(301);
        app.ui_state.focus_visible = true;
    }

    println!(
        "[nui-demo] NUI Rust — Component Playground (tab {}, backend {:?}, font {:?})",
        app.active_tab, backend, font_engine
    );
    event_loop.run_app(&mut app).unwrap();
}
