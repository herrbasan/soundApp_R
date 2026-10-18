// Unified NUI Scene Display-List + Dual Backend Renderer:
//   1. GpuBackend (glow OpenGL 3.3 Core): 1-draw-call instanced SDF + R8 Glyph Atlas
//   2. CpuBackend (softbuffer): Direct u32 AVX/row-fill + SDF corner/border + Gamma LUT glyph blit
#![allow(dead_code)]

use std::ffi::CString;
use std::num::NonZeroU32;
use std::sync::Arc;

use glow::HasContext;
use glutin::config::{ConfigTemplateBuilder, GlConfig};
use glutin::context::{
    ContextApi, ContextAttributesBuilder, NotCurrentGlContext, PossiblyCurrentContext,
    PossiblyCurrentGlContext, Version,
};
use glutin::display::{GetGlDisplay, GlDisplay};
use glutin::surface::{
    GlSurface, Surface as GlWindowSurface, SurfaceAttributesBuilder, SwapInterval, WindowSurface,
};
use glutin_winit::DisplayBuilder;
use raw_window_handle::HasWindowHandle;
use softbuffer::{Context as SbContext, Surface as SbSurface};
use winit::event_loop::ActiveEventLoop;
use winit::window::{Window, WindowAttributes};

use crate::text::{FontEngine, FontFace, PositionedGlyph, TextSystem, ATLAS_H, ATLAS_W};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BackendKind {
    Gpu,
    Cpu,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct QuadInstance {
    pub rect: [f32; 4],       // x, y, w, h (physical px)
    pub uv_rect: [f32; 4],    // ux, uy, uw, uh (atlas texels)
    pub fill_color: [f32; 4], // r, g, b, a (0..1)
    pub border_color: [f32; 4],
    pub params: [f32; 4],     // [radius, border_width, kind (0=rect, 1=swash_glyph, 2=fd_glyph), 0.0]
    pub clip_rect: [f32; 4],  // [cx0, cy0, cx1, cy1] (physical px)
}

pub struct Scene {
    pub bg: (u8, u8, u8),
    pub instances: Vec<QuadInstance>,
    pub overlay_instances: Vec<QuadInstance>,
    pub in_overlay: bool,
    pub clip_stack: Vec<[f32; 4]>,
    /// Clip stack used while `in_overlay` is true. Kept separate from `clip_stack` so
    /// top-layer overlays (select popups, dropzone) are never clipped by the cards or
    /// columns that own the widget they belong to.
    pub overlay_clip_stack: Vec<[f32; 4]>,
    pub static_count: usize,
    pub static_gen: u64,
    /// Optional single RGBA8 image (cover art) drawn via `image_quad` (`kind = 5`).
    pub image_rgba: Option<(u32, u32, Vec<u8>)>,
    /// Bumped by every `set_image`, so backends re-upload on change.
    pub image_gen: u64,
    glyph_scratch: Vec<PositionedGlyph>,
}

impl Scene {
    pub fn new() -> Self {
        Self {
            bg: (30, 30, 32),
            instances: Vec::with_capacity(2048),
            overlay_instances: Vec::with_capacity(256),
            in_overlay: false,
            clip_stack: Vec::with_capacity(16),
            overlay_clip_stack: Vec::with_capacity(8),
            static_count: 0,
            static_gen: 0,
            image_rgba: None,
            image_gen: 0,
            glyph_scratch: Vec::with_capacity(256),
        }
    }

    /// Replace the scene's RGBA image (e.g. embedded cover art).
    pub fn set_image(&mut self, w: u32, h: u32, rgba: Vec<u8>) {
        assert!(w > 0 && h > 0, "set_image: zero-sized image");
        assert_eq!(
            rgba.len(),
            (w as usize) * (h as usize) * 4,
            "set_image: RGBA buffer length does not match {w}x{h}"
        );
        self.image_rgba = Some((w, h, rgba));
        self.image_gen = self.image_gen.wrapping_add(1);
    }

    /// Clear the scene's RGBA image when switching to a track without cover art.
    pub fn clear_image(&mut self) {
        if self.image_rgba.is_some() {
            self.image_rgba = None;
            self.image_gen = self.image_gen.wrapping_add(1);
        }
    }

    /// Emit an image quad (`kind = 5`) sampling the scene image with 0..1 UVs.
    pub fn image_quad(&mut self, x: f32, y: f32, w: f32, h: f32) {
        if w <= 0.0 || h <= 0.0 || self.image_rgba.is_none() {
            return;
        }
        let clip_rect = self.current_clip();
        self.push_instance(QuadInstance {
            rect: [x, y, w, h],
            uv_rect: [0.0, 0.0, 1.0, 1.0],
            fill_color: [1.0; 4],
            border_color: [0.0; 4],
            params: [0.0, 0.0, 5.0, 0.0],
            clip_rect,
        });
    }

    /// Emit a pre-rotated (-45°) text run centered at `(center_x, center_y)` as a
    /// single atlas glyph quad. Works identically on the GPU and CPU backends.
    #[allow(clippy::too_many_arguments)]
    pub fn text_rotated_ccw45(
        &mut self,
        text_sys: &mut TextSystem,
        face: FontFace,
        engine: FontEngine,
        text: &str,
        center_x: f32,
        center_y: f32,
        px_size: f32,
        color: (u8, u8, u8, u8),
    ) {
        let Some(ag) = text_sys.layout_rotated_ccw45(face, engine, text, px_size) else {
            return;
        };
        let w = ag.w as f32;
        let h = ag.h as f32;
        let kind = match engine {
            FontEngine::Swash => 1.0,
            FontEngine::Fontdue => 2.0,
        };
        let clip_rect = self.current_clip();
        self.push_instance(QuadInstance {
            rect: [
                (center_x - w * 0.5).round(),
                (center_y - h * 0.5).round(),
                w,
                h,
            ],
            uv_rect: [ag.uv_x as f32, ag.uv_y as f32, w, h],
            fill_color: [
                color.0 as f32 / 255.0,
                color.1 as f32 / 255.0,
                color.2 as f32 / 255.0,
                color.3 as f32 / 255.0,
            ],
            border_color: [0.0; 4],
            params: [0.0, 0.0, kind, 0.0],
            clip_rect,
        });
    }

    pub fn clear_all(&mut self, bg: (u8, u8, u8)) {
        self.bg = bg;
        self.instances.clear();
        self.overlay_instances.clear();
        self.in_overlay = false;
        self.clip_stack.clear();
        self.overlay_clip_stack.clear();
        self.static_count = 0;
        self.static_gen = self.static_gen.wrapping_add(1);
    }

    pub fn freeze_static(&mut self) {
        self.static_count = self.instances.len();
    }

    pub fn clear_dynamic(&mut self) {
        self.instances.truncate(self.static_count);
        self.overlay_instances.clear();
        self.in_overlay = false;
        self.clip_stack.clear();
        self.overlay_clip_stack.clear();
    }

    pub fn current_clip(&self) -> [f32; 4] {
        if self.in_overlay {
            self.overlay_clip_stack
                .last()
                .copied()
                .unwrap_or([0.0, 0.0, 32768.0, 32768.0])
        } else {
            self.clip_stack
                .last()
                .copied()
                .unwrap_or([0.0, 0.0, 32768.0, 32768.0])
        }
    }

    pub fn push_clip(&mut self, x: f32, y: f32, w: f32, h: f32) {
        let prev = self.current_clip();
        let nx0 = x.max(prev[0]);
        let ny0 = y.max(prev[1]);
        let nx1 = (x + w).min(prev[2]).max(nx0);
        let ny1 = (y + h).min(prev[3]).max(ny0);
        if self.in_overlay {
            self.overlay_clip_stack.push([nx0, ny0, nx1, ny1]);
        } else {
            self.clip_stack.push([nx0, ny0, nx1, ny1]);
        }
    }

    pub fn pop_clip(&mut self) {
        if self.in_overlay {
            let _ = self.overlay_clip_stack.pop();
        } else {
            let _ = self.clip_stack.pop();
        }
    }

    pub fn begin_overlay(&mut self) {
        self.in_overlay = true;
    }

    pub fn end_overlay(&mut self) {
        self.in_overlay = false;
        self.overlay_clip_stack.clear();
    }

    pub fn finish_frame(&mut self) {
        self.in_overlay = false;
        self.overlay_clip_stack.clear();
        if !self.overlay_instances.is_empty() {
            self.instances.extend_from_slice(&self.overlay_instances);
            self.overlay_instances.clear();
        }
    }

    #[inline]
    fn push_instance(&mut self, inst: QuadInstance) {
        if self.in_overlay {
            self.overlay_instances.push(inst);
        } else {
            self.instances.push(inst);
        }
    }

    #[inline]
    pub fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, fill: (u8, u8, u8, u8)) {
        self.rounded_rect(x, y, w, h, 0.0, fill, (0, 0, 0, 0), 0.0);
    }

    #[inline]
    pub fn rounded_rect(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radius: f32,
        fill: (u8, u8, u8, u8),
        border_color: (u8, u8, u8, u8),
        border_width: f32,
    ) {
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let clip_rect = self.current_clip();
        self.push_instance(QuadInstance {
            rect: [x, y, w, h],
            uv_rect: [0.0, 0.0, 0.0, 0.0],
            fill_color: [
                fill.0 as f32 / 255.0,
                fill.1 as f32 / 255.0,
                fill.2 as f32 / 255.0,
                fill.3 as f32 / 255.0,
            ],
            border_color: [
                border_color.0 as f32 / 255.0,
                border_color.1 as f32 / 255.0,
                border_color.2 as f32 / 255.0,
                border_color.3 as f32 / 255.0,
            ],
            params: [radius, border_width, 0.0, 0.0],
            clip_rect,
        });
    }

    pub fn text(
        &mut self,
        text_sys: &mut TextSystem,
        face: FontFace,
        engine: FontEngine,
        s: &str,
        x: f32,
        baseline: f32,
        px: f32,
        color: (u8, u8, u8, u8),
    ) -> f32 {
        self.glyph_scratch.clear();
        let adv = text_sys.layout_run(face, engine, s, x, baseline, px, &mut self.glyph_scratch);
        let kind = match engine {
            FontEngine::Swash => 1.0,
            FontEngine::Fontdue => 2.0,
        };
        let fc = [
            color.0 as f32 / 255.0,
            color.1 as f32 / 255.0,
            color.2 as f32 / 255.0,
            color.3 as f32 / 255.0,
        ];
        let clip_rect = self.current_clip();
        let glyphs = std::mem::take(&mut self.glyph_scratch);
        for g in &glyphs {
            self.push_instance(QuadInstance {
                rect: [g.x as f32, g.y as f32, g.w as f32, g.h as f32],
                uv_rect: [g.uv_x as f32, g.uv_y as f32, g.w as f32, g.h as f32],
                fill_color: fc,
                border_color: [0.0; 4],
                params: [0.0, 0.0, kind, 0.0],
                clip_rect,
            });
        }
        self.glyph_scratch = glyphs;
        adv
    }

    /// Draw a Material Icon centered by its true rasterized pixel bounding box around `(cx, cy)`.
    pub fn icon_centered(
        &mut self,
        text_sys: &mut TextSystem,
        icon: &str,
        cx: f32,
        cy: f32,
        px: f32,
        color: (u8, u8, u8, u8),
    ) {
        if let Some(g) = text_sys.layout_icon_centered(icon, cx, cy, px) {
            let clip_rect = self.current_clip();
            self.push_instance(QuadInstance {
                rect: [g.x as f32, g.y as f32, g.w as f32, g.h as f32],
                uv_rect: [g.uv_x as f32, g.uv_y as f32, g.w as f32, g.h as f32],
                fill_color: [
                    color.0 as f32 / 255.0,
                    color.1 as f32 / 255.0,
                    color.2 as f32 / 255.0,
                    color.3 as f32 / 255.0,
                ],
                border_color: [0.0; 4],
                params: [0.0, 0.0, 1.0, 0.0],
                clip_rect,
            });
        }
    }

    /// Draw a soft SDF rounded-box drop shadow around `(x, y, w, h)`.
    pub fn shadow(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radius: f32,
        blur: f32,
        color: (u8, u8, u8, u8),
    ) {
        if w <= 0.0 || h <= 0.0 || blur <= 0.0 || color.3 == 0 {
            return;
        }
        let clip_rect = self.current_clip();
        self.push_instance(QuadInstance {
            rect: [x - blur, y - blur, w + blur * 2.0, h + blur * 2.0],
            uv_rect: [0.0; 4],
            fill_color: [
                color.0 as f32 / 255.0,
                color.1 as f32 / 255.0,
                color.2 as f32 / 255.0,
                color.3 as f32 / 255.0,
            ],
            border_color: [0.0; 4],
            params: [radius, blur, 3.0, 0.0],
            clip_rect,
        });
    }

    /// Anti-aliased capsule line segment (GPU/Cpu `kind = 4`).
    /// `(x0, y0)-(x1, y1)` are physical screen px; the fragment shader / CPU blitter
    /// recover the segment endpoints from `border_color` (there is no other slot).
    pub fn line(
        &mut self,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        thickness: f32,
        color: (u8, u8, u8, u8),
    ) {
        if thickness <= 0.0 || color.3 == 0 {
            return;
        }
        let pad = thickness * 0.5 + 1.5;
        let rx = x0.min(x1) - pad;
        let ry = y0.min(y1) - pad;
        let rw = (x0 - x1).abs() + pad * 2.0;
        let rh = (y0 - y1).abs() + pad * 2.0;
        let clip_rect = self.current_clip();
        self.push_instance(QuadInstance {
            rect: [rx, ry, rw, rh],
            uv_rect: [0.0; 4],
            fill_color: [
                color.0 as f32 / 255.0,
                color.1 as f32 / 255.0,
                color.2 as f32 / 255.0,
                color.3 as f32 / 255.0,
            ],
            border_color: [x0, y0, x1, y1],
            params: [thickness * 0.5, 0.0, 4.0, 0.0],
            clip_rect,
        });
    }

    /// Dashed outline: emits `kind = 4` line segments along all four edges of
    /// `(x, y, w, h)` with the given `dash` / `gap` run lengths.
    pub fn dashed_rect(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        dash: f32,
        gap: f32,
        thickness: f32,
        color: (u8, u8, u8, u8),
    ) {
        let step = (dash + gap).max(1.0);
        let mut dx = 0.0;
        while dx < w {
            let x0 = x + dx;
            let x1 = (x + dx + dash).min(x + w);
            self.line(x0, y, x1, y, thickness, color);
            self.line(x0, y + h, x1, y + h, thickness, color);
            dx += step;
        }
        let mut dy = 0.0;
        while dy < h {
            let y0 = y + dy;
            let y1 = (y + dy + dash).min(y + h);
            self.line(x, y0, x, y1, thickness, color);
            self.line(x + w, y0, x + w, y1, thickness, color);
            dy += step;
        }
    }
}

// ============================================================================
// GPU Backend (glow OpenGL 3.3 Instanced SDF + Glyph Atlas)
// ============================================================================

struct GpuState {
    gl: glow::Context,
    gl_context: PossiblyCurrentContext,
    gl_surface: GlWindowSurface<WindowSurface>,
    program: glow::Program,
    vao: glow::VertexArray,
    vbo: glow::Buffer,
    atlas_tex: glow::Texture,
    image_tex: glow::Texture,
    u_screen: glow::UniformLocation,
    vbo_capacity: usize,
    surface_size: (u32, u32),
    /// `Scene::image_gen` this window's image texture was last uploaded from.
    uploaded_image_gen: u64,
    /// `TextSystem::atlas_gen` this window's atlas texture was last uploaded from.
    /// Every window owns a private GL atlas copy; a drift against the shared
    /// `TextSystem` generation triggers a re-upload in `render_gpu`.
    uploaded_atlas_gen: u64,
}

const VERT_SRC: &str = r#"#version 330 core
layout(location = 0) in vec4 a_rect;
layout(location = 1) in vec4 a_uv_rect;
layout(location = 2) in vec4 a_fill;
layout(location = 3) in vec4 a_border;
layout(location = 4) in vec4 a_params;
layout(location = 5) in vec4 a_clip;

uniform vec2 u_screen;

out vec2 v_screen_px;
out vec2 v_local_px;
out vec2 v_rect_size;
out vec2 v_uv;
out vec4 v_fill;
out vec4 v_border;
out vec4 v_params;
out vec4 v_clip;

void main() {
    // 6 vertices per instance (2 triangles)
    vec2 corner;
    int vid = gl_VertexID % 6;
    if (vid == 0) corner = vec2(0.0, 0.0);
    else if (vid == 1) corner = vec2(1.0, 0.0);
    else if (vid == 2) corner = vec2(1.0, 1.0);
    else if (vid == 3) corner = vec2(0.0, 0.0);
    else if (vid == 4) corner = vec2(1.0, 1.0);
    else corner = vec2(0.0, 1.0);

    vec2 px = a_rect.xy + corner * a_rect.zw;
    vec2 ndc = vec2(
        (px.x / u_screen.x) * 2.0 - 1.0,
        1.0 - (px.y / u_screen.y) * 2.0
    );
    gl_Position = vec4(ndc, 0.0, 1.0);

    v_screen_px = px;
    v_local_px = corner * a_rect.zw;
    v_rect_size = a_rect.zw;
    // kind 5 (image) carries normalized 0..1 UVs; every other kind indexes the atlas.
    if (int(a_params.z + 0.5) == 5) {
        v_uv = a_uv_rect.xy + corner * a_uv_rect.zw;
    } else {
        v_uv = (a_uv_rect.xy + corner * a_uv_rect.zw) / vec2(2048.0, 2048.0);
    }
    v_fill = a_fill;
    v_border = a_border;
    v_params = a_params;
    v_clip = a_clip;
}
"#;

const FRAG_SRC: &str = r#"#version 330 core
in vec2 v_screen_px;
in vec2 v_local_px;
in vec2 v_rect_size;
in vec2 v_uv;
in vec4 v_fill;
in vec4 v_border;
in vec4 v_params;
in vec4 v_clip;

uniform sampler2D u_atlas;
uniform sampler2D u_image;

out vec4 frag_color;

float sd_rounded_box(vec2 p, vec2 b, float r) {
    vec2 q = abs(p) - b + vec2(r);
    return min(max(q.x, q.y), 0.0) + length(max(q, 0.0)) - r;
}

void main() {
    if (v_screen_px.x < v_clip.x || v_screen_px.y < v_clip.y ||
        v_screen_px.x >= v_clip.z || v_screen_px.y >= v_clip.w) {
        discard;
    }
    int kind = int(v_params.z + 0.5);
    if (kind == 0) {
        float radius = v_params.x;
        float border_w = v_params.y;
        if (radius <= 0.0 && border_w <= 0.0) {
            frag_color = vec4(v_fill.rgb * v_fill.a, v_fill.a);
            return;
        }
        vec2 half_sz = v_rect_size * 0.5;
        vec2 p = v_local_px - half_sz;
        float r = min(radius, min(half_sz.x, half_sz.y));
        float d = sd_rounded_box(p, half_sz, r);
        float outer_a = clamp(0.5 - d, 0.0, 1.0);
        if (outer_a <= 0.0) discard;

        vec4 fill_pm = vec4(v_fill.rgb * v_fill.a, v_fill.a);
        if (border_w > 0.0) {
            float inner_d = d + border_w;
            float border_mix = clamp(0.5 + inner_d, 0.0, 1.0);
            vec4 border_pm = vec4(v_border.rgb * v_border.a, v_border.a);
            frag_color = mix(fill_pm, border_pm, border_mix) * outer_a;
        } else {
            frag_color = fill_pm * outer_a;
        }
    } else if (kind == 1) {
        // Browser-grade Swash glyph: gamma-corrected linear-light contrast boost for dark surfaces
        float cov = texture(u_atlas, v_uv).r;
        if (cov <= 0.002) discard;
        float lum = dot(v_fill.rgb, vec3(0.2126, 0.7152, 0.0722));
        float boosted = pow(cov, 1.0 / 1.48);
        float a = mix(cov, boosted, smoothstep(0.25, 0.75, lum)) * v_fill.a;
        frag_color = vec4(v_fill.rgb * a, a);
    } else if (kind == 2) {
        // Legacy Fontdue glyph: naive sRGB coverage
        float cov = texture(u_atlas, v_uv).r;
        if (cov <= 0.002) discard;
        float a = cov * v_fill.a;
        frag_color = vec4(v_fill.rgb * a, a);
    } else if (kind == 3) {
        // kind == 3: Soft SDF drop shadow (Gaussian half-plane edge integral: 0.5 at d=0, cubic falloff)
        float radius = v_params.x;
        float blur = max(v_params.y, 1.0);
        vec2 half_sz = max(v_rect_size * 0.5 - vec2(blur), vec2(1.0));
        vec2 p = v_local_px - v_rect_size * 0.5;
        float r = min(radius, min(half_sz.x, half_sz.y));
        float d = sd_rounded_box(p, half_sz, r);
        float t = clamp(1.0 - max(d, 0.0) / blur, 0.0, 1.0);
        float a = 0.5 * t * t * t * v_fill.a;
        if (a <= 0.002) discard;
        frag_color = vec4(v_fill.rgb * a, a);
    } else if (kind == 5) {
        // Premultiplied RGBA image quad (cover art), normalized UVs.
        vec4 c = texture(u_image, v_uv);
        if (c.a <= 0.002) discard;
        frag_color = vec4(c.rgb * c.a, c.a);
    } else if (kind == 4) {
        // 2D capsule line segment from v_border.xy to v_border.zw with radius v_params.x
        vec2 pa = v_screen_px - v_border.xy;
        vec2 ba = v_border.zw - v_border.xy;
        float baba = dot(ba, ba);
        float h = baba > 0.0001 ? clamp(dot(pa, ba) / baba, 0.0, 1.0) : 0.0;
        float d = length(pa - ba * h) - v_params.x;
        float cov = clamp(0.5 - d, 0.0, 1.0);
        if (cov <= 0.002) discard;
        float a = cov * v_fill.a;
        frag_color = vec4(v_fill.rgb * a, a);
    }
}
"#;

// ============================================================================
// CPU Fallback Backend (softbuffer + direct u32 blitter)
// ============================================================================

struct CpuState {
    _context: SbContext<Arc<Window>>,
    surface: SbSurface<Arc<Window>, Arc<Window>>,
    surface_size: (u32, u32),
    static_cache: Vec<u32>,
    cached_gen: u64,
}

pub struct Renderer {
    pub active: BackendKind,
    gpu: Option<GpuState>,
    cpu: Option<CpuState>,
    window: Arc<Window>,
    /// Set by `request_reveal`; the next completed present maps the window and
    /// focuses it, so the native surface is never shown unpainted (white flash).
    pending_reveal: bool,
}

impl Renderer {
    pub fn new(
        event_loop: &ActiveEventLoop,
        attrs: WindowAttributes,
        prefer: BackendKind,
    ) -> (Arc<Window>, Self) {
        // Ask DWM for a dark frame/surface from creation.
        let attrs = attrs.with_theme(Some(winit::window::Theme::Dark));

        if prefer == BackendKind::Gpu {
            if let Some((win, gpu)) = Self::try_init_gpu(event_loop, attrs.clone()) {
                #[cfg(target_os = "windows")]
                {
                    Self::set_win32_dark_background(&win);
                    Self::set_win32_cloak(&win, true);
                }
                return (
                    win.clone(),
                    Self {
                        active: BackendKind::Gpu,
                        gpu: Some(gpu),
                        cpu: None,
                        window: win,
                        pending_reveal: true,
                    },
                );
            }
            eprintln!("[poc] GPU init unavailable, falling back to CPU softbuffer");
        }

        let win = Arc::new(event_loop.create_window(attrs).expect("create window"));
        #[cfg(target_os = "windows")]
        {
            Self::set_win32_dark_background(&win);
            Self::set_win32_cloak(&win, true);
        }
        let cpu = Self::init_cpu(&win);
        (
            win.clone(),
            Self {
                active: BackendKind::Cpu,
                gpu: None,
                cpu: Some(cpu),
                window: win,
                pending_reveal: true,
            },
        )
    }

    #[cfg(target_os = "windows")]
    fn set_win32_dark_background(win: &Window) {
        use raw_window_handle::RawWindowHandle;
        #[link(name = "gdi32")]
        extern "system" {
            fn CreateSolidBrush(color: u32) -> isize;
            fn FillRect(hdc: isize, lprc: *const [i32; 4], hbr: isize) -> i32;
        }
        #[link(name = "user32")]
        extern "system" {
            fn SetClassLongPtrW(hwnd: isize, index: i32, new_long: isize) -> usize;
            fn GetDC(hwnd: isize) -> isize;
            fn ReleaseDC(hwnd: isize, hdc: isize) -> i32;
            fn GetClientRect(hwnd: isize, lprc: *mut [i32; 4]) -> i32;
        }
        const GCLP_HBRBACKGROUND: i32 = -10;
        if let Ok(wh) = win.window_handle() {
            if let RawWindowHandle::Win32(h) = wh.as_raw() {
                unsafe {
                    let hwnd = h.hwnd.get();
                    // COLORREF 0x00BBGGRR = rgb(19, 19, 19) (`Theme::dark` base)
                    let brush = CreateSolidBrush(0x0013_1313);
                    if brush != 0 {
                        SetClassLongPtrW(hwnd, GCLP_HBRBACKGROUND, brush);
                        let hdc = GetDC(hwnd);
                        if hdc != 0 {
                            let mut rc = [0i32; 4];
                            if GetClientRect(hwnd, &mut rc) != 0 {
                                FillRect(hdc, &rc, brush);
                            }
                            ReleaseDC(hwnd, hdc);
                        }
                    }
                }
            }
        }
    }

    #[cfg(target_os = "windows")]
    fn set_win32_cloak(win: &Window, cloak: bool) {
        use raw_window_handle::RawWindowHandle;
        #[link(name = "dwmapi")]
        extern "system" {
            fn DwmSetWindowAttribute(
                hwnd: isize,
                dw_attribute: u32,
                pv_attribute: *const u32,
                cb_attribute: u32,
            ) -> i32;
        }
        const DWMWA_CLOAK: u32 = 13;
        if let Ok(wh) = win.window_handle() {
            if let RawWindowHandle::Win32(h) = wh.as_raw() {
                let val: u32 = if cloak { 1 } else { 0 };
                unsafe {
                    let _ = DwmSetWindowAttribute(
                        h.hwnd.get(),
                        DWMWA_CLOAK,
                        &val as *const u32,
                        std::mem::size_of::<u32>() as u32,
                    );
                }
            }
        }
    }

    /// Request that the window be revealed and focused once the next frame is presented.
    pub fn request_reveal(&mut self) {
        self.pending_reveal = true;
        #[cfg(target_os = "windows")]
        Self::set_win32_cloak(&self.window, true);
        self.window.set_visible(true);
        self.window.request_redraw();
    }

    /// Cloak and hide the window so re-showing it later via `request_reveal()`
    /// still receives `WM_PAINT` (`RedrawRequested`) without flashing.
    pub fn cloak_and_hide(&mut self) {
        self.pending_reveal = false;
        #[cfg(target_os = "windows")]
        Self::set_win32_cloak(&self.window, true);
        self.window.set_visible(false);
    }

    /// Uncloak + map + focus the window after the first completed present.
    fn reveal_if_pending(&mut self) {
        if self.pending_reveal {
            self.pending_reveal = false;
            self.window.set_visible(true);
            #[cfg(target_os = "windows")]
            Self::set_win32_cloak(&self.window, false);
            self.window.focus_window();
        }
    }

    fn init_cpu(win: &Arc<Window>) -> CpuState {
        let context = SbContext::new(win.clone()).expect("softbuffer context");
        let surface = SbSurface::new(&context, win.clone()).expect("softbuffer surface");
        CpuState {
            _context: context,
            surface,
            surface_size: (0, 0),
            static_cache: Vec::new(),
            cached_gen: 0,
        }
    }

    fn try_init_gpu(
        event_loop: &ActiveEventLoop,
        attrs: WindowAttributes,
    ) -> Option<(Arc<Window>, GpuState)> {
        let template = ConfigTemplateBuilder::new().with_alpha_size(8);
        let display_builder = DisplayBuilder::new().with_window_attributes(Some(attrs));
        let (window, gl_config) = display_builder
            .build(event_loop, template, |configs| {
                configs
                    .reduce(|accum, config| {
                        if config.num_samples() < accum.num_samples() {
                            config
                        } else {
                            accum
                        }
                    })
                    .unwrap()
            })
            .ok()?;

        let window = Arc::new(window?);
        let raw_window_handle = window.window_handle().ok()?.as_raw();
        let gl_display = gl_config.display();

        let context_attrs = ContextAttributesBuilder::new()
            .with_context_api(ContextApi::OpenGl(Some(Version::new(3, 3))))
            .build(Some(raw_window_handle));

        let not_current = unsafe { gl_display.create_context(&gl_config, &context_attrs).ok()? };

        let sz = window.inner_size();
        let (w, h) = (sz.width.max(1), sz.height.max(1));
        let surf_attrs = SurfaceAttributesBuilder::<WindowSurface>::new().build(
            raw_window_handle,
            NonZeroU32::new(w).unwrap(),
            NonZeroU32::new(h).unwrap(),
        );

        let gl_surface = unsafe {
            gl_display
                .create_window_surface(&gl_config, &surf_attrs)
                .ok()?
        };
        let gl_context = not_current.make_current(&gl_surface).ok()?;

        // Disable driver-blocking vsync so winit's WaitUntil timer controls exact frame pacing
        let _ = gl_surface.set_swap_interval(&gl_context, SwapInterval::DontWait);

        let gl = unsafe {
            glow::Context::from_loader_function(|s| {
                let cs = CString::new(s).unwrap();
                gl_display.get_proc_address(&cs) as *const _
            })
        };

        unsafe {
            let program = gl.create_program().ok()?;
            let vs = gl.create_shader(glow::VERTEX_SHADER).ok()?;
            gl.shader_source(vs, VERT_SRC);
            gl.compile_shader(vs);
            if !gl.get_shader_compile_status(vs) {
                eprintln!("[poc] VS compile error: {}", gl.get_shader_info_log(vs));
                return None;
            }
            let fs = gl.create_shader(glow::FRAGMENT_SHADER).ok()?;
            gl.shader_source(fs, FRAG_SRC);
            gl.compile_shader(fs);
            if !gl.get_shader_compile_status(fs) {
                eprintln!("[poc] FS compile error: {}", gl.get_shader_info_log(fs));
                return None;
            }
            gl.attach_shader(program, vs);
            gl.attach_shader(program, fs);
            gl.link_program(program);
            if !gl.get_program_link_status(program) {
                eprintln!("[poc] link error: {}", gl.get_program_info_log(program));
                return None;
            }
            gl.delete_shader(vs);
            gl.delete_shader(fs);

            let u_screen = gl.get_uniform_location(program, "u_screen")?;
            let u_atlas = gl.get_uniform_location(program, "u_atlas")?;
            let u_image = gl.get_uniform_location(program, "u_image")?;
            gl.use_program(Some(program));
            gl.uniform_1_i32(Some(&u_atlas), 0);
            gl.uniform_1_i32(Some(&u_image), 1);

            let vao = gl.create_vertex_array().ok()?;
            let vbo = gl.create_buffer().ok()?;
            gl.bind_vertex_array(Some(vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));

            let vbo_capacity = 4096;
            let stride = std::mem::size_of::<QuadInstance>() as i32;
            gl.buffer_data_size(
                glow::ARRAY_BUFFER,
                (vbo_capacity * stride as usize) as i32,
                glow::STREAM_DRAW,
            );

            for i in 0..6u32 {
                gl.enable_vertex_attrib_array(i);
                gl.vertex_attrib_pointer_f32(
                    i,
                    4,
                    glow::FLOAT,
                    false,
                    stride,
                    (i as i32) * 16,
                );
                gl.vertex_attrib_divisor(i, 1);
            }

            let atlas_tex = gl.create_texture().ok()?;
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(atlas_tex));
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MIN_FILTER,
                glow::NEAREST as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MAG_FILTER,
                glow::NEAREST as i32,
            );
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::R8 as i32,
                ATLAS_W as i32,
                ATLAS_H as i32,
                0,
                glow::RED,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(None),
            );

            // Texture unit 1: the optional single RGBA image (cover art).
            let image_tex = gl.create_texture().ok()?;
            gl.active_texture(glow::TEXTURE1);
            gl.bind_texture(glow::TEXTURE_2D, Some(image_tex));
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MIN_FILTER,
                glow::LINEAR as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MAG_FILTER,
                glow::LINEAR as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_S,
                glow::CLAMP_TO_EDGE as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_T,
                glow::CLAMP_TO_EDGE as i32,
            );
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA8 as i32,
                1,
                1,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(&[0u8, 0, 0, 0])),
            );
            gl.active_texture(glow::TEXTURE0);

            gl.enable(glow::BLEND);
            gl.blend_func(glow::ONE, glow::ONE_MINUS_SRC_ALPHA);

            Some((
                window,
                GpuState {
                    gl,
                    gl_context,
                    gl_surface,
                    program,
                    vao,
                    vbo,
                    atlas_tex,
                    image_tex,
                    u_screen,
                    vbo_capacity,
                    surface_size: (0, 0),
                    uploaded_image_gen: 0,
                    uploaded_atlas_gen: 0,
                },
            ))
        }
    }

    pub fn toggle_backend(&mut self) {
        match self.active {
            BackendKind::Gpu => {
                if self.cpu.is_none() {
                    self.cpu = Some(Self::init_cpu(&self.window));
                }
                self.active = BackendKind::Cpu;
            }
            BackendKind::Cpu => {
                if self.gpu.is_some() {
                    self.active = BackendKind::Gpu;
                }
            }
        }
    }

    pub fn present_nopaint(&mut self, w: u32, h: u32, bg: (u8, u8, u8)) {
        if w == 0 || h == 0 {
            return;
        }
        match self.active {
            BackendKind::Gpu => {
                if let Some(gpu) = &mut self.gpu {
                    // Multi-window: rendering runs on one UI thread, so another window's
                    // context may be current — bind THIS window's context before GL calls.
                    let _ = gpu.gl_context.make_current(&gpu.gl_surface);
                    if gpu.surface_size != (w, h) {
                        gpu.gl_surface.resize(
                            &gpu.gl_context,
                            NonZeroU32::new(w).unwrap(),
                            NonZeroU32::new(h).unwrap(),
                        );
                        gpu.surface_size = (w, h);
                    }
                    unsafe {
                        gpu.gl.viewport(0, 0, w as i32, h as i32);
                        gpu.gl.clear_color(
                            bg.0 as f32 / 255.0,
                            bg.1 as f32 / 255.0,
                            bg.2 as f32 / 255.0,
                            1.0,
                        );
                        gpu.gl.clear(glow::COLOR_BUFFER_BIT);
                    }
                    let _ = gpu.gl_surface.swap_buffers(&gpu.gl_context);
                }
            }
            BackendKind::Cpu => {
                if let Some(cpu) = &mut self.cpu {
                    if cpu.surface_size != (w, h) {
                        let _ = cpu
                            .surface
                            .resize(NonZeroU32::new(w).unwrap(), NonZeroU32::new(h).unwrap());
                        cpu.surface_size = (w, h);
                    }
                    if let Ok(mut b) = cpu.surface.buffer_mut() {
                        let packed = ((bg.0 as u32) << 16) | ((bg.1 as u32) << 8) | (bg.2 as u32);
                        b.fill(packed);
                        let _ = b.present();
                    }
                }
            }
        }
        self.reveal_if_pending();
    }

    pub fn render(
        &mut self,
        w: u32,
        h: u32,
        scene: &Scene,
        text_sys: &mut TextSystem,
        screenshot_path: Option<&str>,
    ) {
        if w == 0 || h == 0 {
            return;
        }
        match self.active {
            BackendKind::Gpu => self.render_gpu(w, h, scene, text_sys, screenshot_path),
            BackendKind::Cpu => self.render_cpu(w, h, scene, text_sys, screenshot_path),
        }
        self.reveal_if_pending();
    }

    fn render_gpu(
        &mut self,
        w: u32,
        h: u32,
        scene: &Scene,
        text_sys: &mut TextSystem,
        screenshot_path: Option<&str>,
    ) {
        let Some(gpu) = &mut self.gpu else { return };
        // Multi-window: rendering runs on one UI thread, so another window's context may
        // be current — always bind THIS window's context + surface before any GL call.
        let _ = gpu.gl_context.make_current(&gpu.gl_surface);
        if gpu.surface_size != (w, h) {
            gpu.gl_surface.resize(
                &gpu.gl_context,
                NonZeroU32::new(w).unwrap(),
                NonZeroU32::new(h).unwrap(),
            );
            gpu.surface_size = (w, h);
        }

        unsafe {
            if gpu.uploaded_image_gen != scene.image_gen {
                if let Some((iw, ih, rgba)) = &scene.image_rgba {
                    gpu.gl.active_texture(glow::TEXTURE1);
                    gpu.gl.bind_texture(glow::TEXTURE_2D, Some(gpu.image_tex));
                    gpu.gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
                    gpu.gl.tex_image_2d(
                        glow::TEXTURE_2D,
                        0,
                        glow::RGBA8 as i32,
                        *iw as i32,
                        *ih as i32,
                        0,
                        glow::RGBA,
                        glow::UNSIGNED_BYTE,
                        glow::PixelUnpackData::Slice(Some(rgba)),
                    );
                    gpu.gl.active_texture(glow::TEXTURE0);
                }
                gpu.uploaded_image_gen = scene.image_gen;
            }

            if gpu.uploaded_atlas_gen != text_sys.atlas_gen {
                gpu.gl.bind_texture(glow::TEXTURE_2D, Some(gpu.atlas_tex));
                gpu.gl.tex_sub_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    0,
                    0,
                    ATLAS_W as i32,
                    ATLAS_H as i32,
                    glow::RED,
                    glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::Slice(Some(&text_sys.atlas)),
                );
                gpu.uploaded_atlas_gen = text_sys.atlas_gen;
                text_sys.atlas_dirty = false;
            }

            gpu.gl.viewport(0, 0, w as i32, h as i32);
            gpu.gl.clear_color(
                scene.bg.0 as f32 / 255.0,
                scene.bg.1 as f32 / 255.0,
                scene.bg.2 as f32 / 255.0,
                1.0,
            );
            gpu.gl.clear(glow::COLOR_BUFFER_BIT);

            if !scene.instances.is_empty() {
                gpu.gl.use_program(Some(gpu.program));
                gpu.gl.uniform_2_f32(Some(&gpu.u_screen), w as f32, h as f32);
                gpu.gl.bind_vertex_array(Some(gpu.vao));
                gpu.gl.bind_buffer(glow::ARRAY_BUFFER, Some(gpu.vbo));

                let count = scene.instances.len();
                let byte_len = count * std::mem::size_of::<QuadInstance>();
                if count > gpu.vbo_capacity {
                    gpu.vbo_capacity = count.next_power_of_two();
                    gpu.gl.buffer_data_size(
                        glow::ARRAY_BUFFER,
                        (gpu.vbo_capacity * std::mem::size_of::<QuadInstance>()) as i32,
                        glow::STREAM_DRAW,
                    );
                }
                let raw_bytes =
                    std::slice::from_raw_parts(scene.instances.as_ptr() as *const u8, byte_len);
                gpu.gl
                    .buffer_sub_data_u8_slice(glow::ARRAY_BUFFER, 0, raw_bytes);
                gpu.gl
                    .draw_arrays_instanced(glow::TRIANGLES, 0, 6, count as i32);
            }

            if let Some(path) = screenshot_path {
                let mut raw = vec![0u8; (w as usize) * (h as usize) * 4];
                gpu.gl.read_pixels(
                    0,
                    0,
                    w as i32,
                    h as i32,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    glow::PixelPackData::Slice(Some(&mut raw)),
                );
                if let Some(mut pm) = tiny_skia::Pixmap::new(w, h) {
                    let dst = pm.data_mut();
                    let row_bytes = (w as usize) * 4;
                    for y in 0..h as usize {
                        let src_row = (h as usize - 1 - y) * row_bytes;
                        let dst_row = y * row_bytes;
                        dst[dst_row..dst_row + row_bytes]
                            .copy_from_slice(&raw[src_row..src_row + row_bytes]);
                        for px in dst[dst_row..dst_row + row_bytes].chunks_exact_mut(4) {
                            px[3] = 255;
                        }
                    }
                    let _ = pm.save_png(path);
                }
            }
        }
        let _ = gpu.gl_surface.swap_buffers(&gpu.gl_context);
    }

    fn render_cpu(
        &mut self,
        w: u32,
        h: u32,
        scene: &Scene,
        text_sys: &TextSystem,
        screenshot_path: Option<&str>,
    ) {
        let Some(cpu) = &mut self.cpu else { return };
        if cpu.surface_size != (w, h) {
            if cpu
                .surface
                .resize(NonZeroU32::new(w).unwrap(), NonZeroU32::new(h).unwrap())
                .is_err()
            {
                return;
            }
            cpu.surface_size = (w, h);
            cpu.cached_gen = 0;
        }
        let n_px = (w as usize) * (h as usize);
        let bg_packed =
            ((scene.bg.0 as u32) << 16) | ((scene.bg.1 as u32) << 8) | (scene.bg.2 as u32);

        if cpu.cached_gen != scene.static_gen || cpu.static_cache.len() != n_px {
            cpu.static_cache.resize(n_px, bg_packed);
            cpu.static_cache.fill(bg_packed);
            let static_end = scene.static_count.min(scene.instances.len());
            Self::rasterize_slice(
                &mut cpu.static_cache,
                w,
                h,
                &scene.instances[..static_end],
                text_sys,
                scene.image_rgba.as_ref(),
            );
            cpu.cached_gen = scene.static_gen;
        }

        let Ok(mut buffer) = cpu.surface.buffer_mut() else {
            return;
        };
        let buf: &mut [u32] = &mut buffer;
        buf.copy_from_slice(&cpu.static_cache);

        let static_end = scene.static_count.min(scene.instances.len());
        Self::rasterize_slice(
            buf,
            w,
            h,
            &scene.instances[static_end..],
            text_sys,
            scene.image_rgba.as_ref(),
        );

        if let Some(path) = screenshot_path {
            if let Some(mut pm) = tiny_skia::Pixmap::new(w, h) {
                let dst = pm.data_mut();
                for (i, &p) in buf.iter().enumerate() {
                    dst[i * 4] = ((p >> 16) & 0xFF) as u8;
                    dst[i * 4 + 1] = ((p >> 8) & 0xFF) as u8;
                    dst[i * 4 + 2] = (p & 0xFF) as u8;
                    dst[i * 4 + 3] = 255;
                }
                let _ = pm.save_png(path);
            }
        }

        let _ = buffer.present();
    }

    fn rasterize_slice(
        buf: &mut [u32],
        w: u32,
        h: u32,
        instances: &[QuadInstance],
        text_sys: &TextSystem,
        image: Option<&(u32, u32, Vec<u8>)>,
    ) {
        let pw = w as i32;
        let ph = h as i32;
        let stride = w as usize;

        for inst in instances {
            let cx0 = (inst.clip_rect[0].floor() as i32).clamp(0, pw);
            let cy0 = (inst.clip_rect[1].floor() as i32).clamp(0, ph);
            let cx1 = (inst.clip_rect[2].ceil() as i32).clamp(0, pw);
            let cy1 = (inst.clip_rect[3].ceil() as i32).clamp(0, ph);
            if cx1 <= cx0 || cy1 <= cy0 {
                continue;
            }

            let kind = (inst.params[2] + 0.5) as i32;
            let r = (inst.fill_color[0] * 255.0 + 0.5) as u8;
            let g = (inst.fill_color[1] * 255.0 + 0.5) as u8;
            let b = (inst.fill_color[2] * 255.0 + 0.5) as u8;
            let a = (inst.fill_color[3] * 255.0 + 0.5) as u8;

            if kind == 0 {
                let x = inst.rect[0];
                let y = inst.rect[1];
                let rw = inst.rect[2];
                let rh = inst.rect[3];
                let radius = inst.params[0];
                let border_w = inst.params[1];

                let x0 = (x.floor() as i32).clamp(cx0, cx1);
                let y0 = (y.floor() as i32).clamp(cy0, cy1);
                let x1 = ((x + rw).ceil() as i32).clamp(cx0, cx1);
                let y1 = ((y + rh).ceil() as i32).clamp(cy0, cy1);
                if x1 <= x0 || y1 <= y0 {
                    continue;
                }

                // Fast path: axis-aligned, sharp, borderless, opaque rect -> slice::fill (SIMD/rep stosd)
                if radius <= 0.0 && border_w <= 0.0 && a == 255 {
                    let packed = ((r as u32) << 16) | ((g as u32) << 8) | (b as u32);
                    for py in y0..y1 {
                        let row_off = py as usize * stride;
                        buf[row_off + x0 as usize..row_off + x1 as usize].fill(packed);
                    }
                    continue;
                }

                // Rounded / bordered / translucent rect via SDF
                let half_w = rw * 0.5;
                let half_h = rh * 0.5;
                let cx = x + half_w;
                let cy = y + half_h;
                let rad = radius.min(half_w).min(half_h);
                let br = (inst.border_color[0] * 255.0 + 0.5) as u8;
                let bg_c = (inst.border_color[1] * 255.0 + 0.5) as u8;
                let bb = (inst.border_color[2] * 255.0 + 0.5) as u8;
                let ba = (inst.border_color[3] * 255.0 + 0.5) as u8;
                let fill_packed = ((r as u32) << 16) | ((g as u32) << 8) | (b as u32);

                let corner_x0 = ((x + rad).ceil() as i32).clamp(x0, x1);
                let corner_x1 = ((x + rw - rad).floor() as i32).clamp(x0, x1);
                let inner_x0 = ((x + border_w + 0.5).ceil() as i32).clamp(x0, x1);
                let inner_x1 = ((x + rw - border_w - 0.5).floor() as i32).clamp(x0, x1);
                let inner_y0 = ((y + rad.max(border_w)).ceil() as i32).clamp(y0, y1);
                let inner_y1 = ((y + rh - rad.max(border_w)).floor() as i32).clamp(y0, y1);

                for py in y0..y1 {
                    let row_off = py as usize * stride;
                    let dy = py as f32 + 0.5 - cy;
                    if a == 255 && py >= inner_y0 && py < inner_y1 && inner_x1 > inner_x0 {
                        buf[row_off + inner_x0 as usize..row_off + inner_x1 as usize]
                            .fill(fill_packed);
                        for px in (x0..inner_x0).chain(inner_x1..x1) {
                            Self::cpu_sdf_pixel(
                                &mut buf[row_off + px as usize],
                                px as f32 + 0.5 - cx,
                                dy,
                                half_w,
                                half_h,
                                rad,
                                border_w,
                                (r, g, b, a),
                                (br, bg_c, bb, ba),
                            );
                        }
                    } else if corner_x1 > corner_x0 {
                        // Top/bottom border/corner rows: only corners [x0..corner_x0] and [corner_x1..x1] curve;
                        // the straight horizontal middle span [corner_x0..corner_x1] has constant SDF d = |dy| - half_h!
                        for px in (x0..corner_x0).chain(corner_x1..x1) {
                            Self::cpu_sdf_pixel(
                                &mut buf[row_off + px as usize],
                                px as f32 + 0.5 - cx,
                                dy,
                                half_w,
                                half_h,
                                rad,
                                border_w,
                                (r, g, b, a),
                                (br, bg_c, bb, ba),
                            );
                        }
                        let d_mid = dy.abs() - half_h;
                        let outer_a = (0.5 - d_mid).clamp(0.0, 1.0);
                        if outer_a > 0.0 {
                            let bmix = if border_w > 0.0 {
                                (0.5 + d_mid + border_w).clamp(0.0, 1.0)
                            } else {
                                0.0
                            };
                            for dst in
                                &mut buf[row_off + corner_x0 as usize..row_off + corner_x1 as usize]
                            {
                                Self::blend_sdf_color(
                                    dst,
                                    outer_a,
                                    bmix,
                                    (r, g, b, a),
                                    (br, bg_c, bb, ba),
                                );
                            }
                        }
                    } else {
                        for px in x0..x1 {
                            Self::cpu_sdf_pixel(
                                &mut buf[row_off + px as usize],
                                px as f32 + 0.5 - cx,
                                dy,
                                half_w,
                                half_h,
                                rad,
                                border_w,
                                (r, g, b, a),
                                (br, bg_c, bb, ba),
                            );
                        }
                    }
                }
            } else if kind == 3 {
                // Soft SDF drop shadow on CPU
                let x = inst.rect[0];
                let y = inst.rect[1];
                let rw = inst.rect[2];
                let rh = inst.rect[3];
                let radius = inst.params[0];
                let blur = inst.params[1].max(1.0);
                let half_w = (rw * 0.5 - blur).max(1.0);
                let half_h = (rh * 0.5 - blur).max(1.0);
                let cx = x + rw * 0.5;
                let cy = y + rh * 0.5;
                let rad = radius.min(half_w).min(half_h);

                let x0 = (x.floor() as i32).clamp(cx0, cx1);
                let y0 = (y.floor() as i32).clamp(cy0, cy1);
                let x1 = ((x + rw).ceil() as i32).clamp(cx0, cx1);
                let y1 = ((y + rh).ceil() as i32).clamp(cy0, cy1);
                for py in y0..y1 {
                    let row_off = py as usize * stride;
                    let dy = (py as f32 + 0.5 - cy).abs() - half_h + rad;
                    for px in x0..x1 {
                        let dx = (px as f32 + 0.5 - cx).abs() - half_w + rad;
                        let d = dx.max(dy).min(0.0) + dx.max(0.0).hypot(dy.max(0.0)) - rad;
                        if d > 0.0 {
                            let t = (1.0 - d / blur).clamp(0.0, 1.0);
                            let cov = (0.5 * t * t * t * a as f32) as u8;
                            if cov > 0 {
                                let dst = &mut buf[row_off + px as usize];
                                *dst = crate::text::GammaLut::blend_naive_srgb(*dst, r, g, b, cov);
                            }
                        }
                    }
                }
            } else if kind == 4 {
                // Anti-aliased capsule line segment (matches the GPU kind==4 shader).
                // Endpoints ride in border_color; radius in params.x.
                let ax = inst.border_color[0];
                let ay = inst.border_color[1];
                let bx = inst.border_color[2];
                let by = inst.border_color[3];
                let rad = inst.params[0];
                let bax = bx - ax;
                let bay = by - ay;
                let baba = bax * bax + bay * bay;

                let x0 = (inst.rect[0].floor() as i32).clamp(cx0, cx1);
                let y0 = (inst.rect[1].floor() as i32).clamp(cy0, cy1);
                let x1 = ((inst.rect[0] + inst.rect[2]).ceil() as i32).clamp(cx0, cx1);
                let y1 = ((inst.rect[1] + inst.rect[3]).ceil() as i32).clamp(cy0, cy1);
                for py in y0..y1 {
                    let row_off = py as usize * stride;
                    let sy = py as f32 + 0.5;
                    for px in x0..x1 {
                        let sx = px as f32 + 0.5;
                        let pax = sx - ax;
                        let pay = sy - ay;
                        let h = if baba > 0.0001 {
                            ((pax * bax + pay * bay) / baba).clamp(0.0, 1.0)
                        } else {
                            0.0
                        };
                        let qx = pax - bax * h;
                        let qy = pay - bay * h;
                        let d = (qx * qx + qy * qy).sqrt() - rad;
                        let cov = ((0.5 - d).clamp(0.0, 1.0) * a as f32 + 0.5) as u8;
                        if cov > 0 {
                            let dst = &mut buf[row_off + px as usize];
                            *dst = crate::text::GammaLut::blend_naive_srgb(*dst, r, g, b, cov);
                        }
                    }
                }
            } else if kind == 5 {
                // RGBA image quad, bilinearly sampled (matches the GPU kind==5 shader).
                let Some((iw, ih, rgba)) = image else {
                    continue;
                };
                let (iw, ih) = (*iw as usize, *ih as usize);
                let x = inst.rect[0];
                let y = inst.rect[1];
                let rw = inst.rect[2];
                let rh = inst.rect[3];
                let x0 = (x.floor() as i32).clamp(cx0, cx1);
                let y0 = (y.floor() as i32).clamp(cy0, cy1);
                let x1 = ((x + rw).ceil() as i32).clamp(cx0, cx1);
                let y1 = ((y + rh).ceil() as i32).clamp(cy0, cy1);
                if x1 <= x0 || y1 <= y0 {
                    continue;
                }
                let sx_scale = (iw.saturating_sub(1)) as f32;
                let sy_scale = (ih.saturating_sub(1)) as f32;
                for py in y0..y1 {
                    let v = ((py as f32 + 0.5 - y) / rh).clamp(0.0, 1.0) * sy_scale;
                    let sy0 = v.floor() as usize;
                    let sy1 = (sy0 + 1).min(ih - 1);
                    let fy = v - sy0 as f32;
                    let row_off = py as usize * stride;
                    for px in x0..x1 {
                        let u = ((px as f32 + 0.5 - x) / rw).clamp(0.0, 1.0) * sx_scale;
                        let sx0 = u.floor() as usize;
                        let sx1 = (sx0 + 1).min(iw - 1);
                        let fx = u - sx0 as f32;
                        let i00 = (sy0 * iw + sx0) * 4;
                        let i10 = (sy0 * iw + sx1) * 4;
                        let i01 = (sy1 * iw + sx0) * 4;
                        let i11 = (sy1 * iw + sx1) * 4;
                        let lerp = |a: u8, b: u8, c: u8, d: u8| -> u8 {
                            let top = a as f32 * (1.0 - fx) + b as f32 * fx;
                            let bot = c as f32 * (1.0 - fx) + d as f32 * fx;
                            (top * (1.0 - fy) + bot * fy).clamp(0.0, 255.0) as u8
                        };
                        let a = lerp(rgba[i00 + 3], rgba[i10 + 3], rgba[i01 + 3], rgba[i11 + 3]);
                        if a == 0 {
                            continue;
                        }
                        let cr = lerp(rgba[i00], rgba[i10], rgba[i01], rgba[i11]);
                        let cg = lerp(rgba[i00 + 1], rgba[i10 + 1], rgba[i01 + 1], rgba[i11 + 1]);
                        let cb = lerp(rgba[i00 + 2], rgba[i10 + 2], rgba[i01 + 2], rgba[i11 + 2]);
                        let dst = &mut buf[row_off + px as usize];
                        *dst = crate::text::GammaLut::blend_naive_srgb(*dst, cr, cg, cb, a);
                    }
                }
            } else {
                // Atlas glyph (kind 1 = Swash gamma-corrected, kind 2 = Fontdue naive sRGB)
                let gx = inst.rect[0] as i32;
                let gy = inst.rect[1] as i32;
                let gw = inst.rect[2] as i32;
                let gh = inst.rect[3] as i32;
                let ux = inst.uv_rect[0] as usize;
                let uy = inst.uv_rect[1] as usize;

                for row in 0..gh {
                    let py = gy + row;
                    if py < cy0 || py >= cy1 {
                        continue;
                    }
                    let dst_row = py as usize * stride;
                    let src_row = (uy + row as usize) * ATLAS_W + ux;
                    for col in 0..gw {
                        let px = gx + col;
                        if px < cx0 || px >= cx1 {
                            continue;
                        }
                        let cov = text_sys.atlas[src_row + col as usize];
                        if cov == 0 {
                            continue;
                        }
                        let dst_idx = dst_row + px as usize;
                        buf[dst_idx] = if kind == 1 {
                            text_sys.gamma.blend_linear(buf[dst_idx], r, g, b, cov)
                        } else {
                            crate::text::GammaLut::blend_naive_srgb(buf[dst_idx], r, g, b, cov)
                        };
                    }
                }
            }
        }
    }

    pub fn save_png(&mut self, path: &str, w: u32, h: u32, scene: &Scene, text_sys: &TextSystem) {
        if w == 0 || h == 0 {
            return;
        }
        let n_px = (w as usize) * (h as usize);
        let bg_packed =
            ((scene.bg.0 as u32) << 16) | ((scene.bg.1 as u32) << 8) | (scene.bg.2 as u32);
        let mut pixels = vec![bg_packed; n_px];
        Self::rasterize_slice(
            &mut pixels,
            w,
            h,
            &scene.instances,
            text_sys,
            scene.image_rgba.as_ref(),
        );

        if let Some(mut pm) = tiny_skia::Pixmap::new(w, h) {
            let dst = pm.data_mut();
            for (i, &p) in pixels.iter().enumerate() {
                dst[i * 4] = ((p >> 16) & 0xFF) as u8;
                dst[i * 4 + 1] = ((p >> 8) & 0xFF) as u8;
                dst[i * 4 + 2] = (p & 0xFF) as u8;
                dst[i * 4 + 3] = 255;
            }
            let _ = pm.save_png(path);
        }
    }

    #[inline(always)]
    fn blend_sdf_color(
        dst: &mut u32,
        outer_a: f32,
        bmix: f32,
        fill: (u8, u8, u8, u8),
        border: (u8, u8, u8, u8),
    ) {
        let fa = fill.3 as f32 / 255.0;
        let ba = border.3 as f32 / 255.0;
        let inv = 1.0 - bmix;
        let pm_r = (fill.0 as f32 * fa * inv + border.0 as f32 * ba * bmix) * outer_a;
        let pm_g = (fill.1 as f32 * fa * inv + border.1 as f32 * ba * bmix) * outer_a;
        let pm_b = (fill.2 as f32 * fa * inv + border.2 as f32 * ba * bmix) * outer_a;
        let pm_a = (fa * inv + ba * bmix) * outer_a;
        let inv_dst = 1.0 - pm_a;
        let dr = ((*dst >> 16) & 0xFF) as f32;
        let dg = ((*dst >> 8) & 0xFF) as f32;
        let db = (*dst & 0xFF) as f32;
        let or = (pm_r + dr * inv_dst + 0.5).clamp(0.0, 255.0) as u32;
        let og = (pm_g + dg * inv_dst + 0.5).clamp(0.0, 255.0) as u32;
        let ob = (pm_b + db * inv_dst + 0.5).clamp(0.0, 255.0) as u32;
        *dst = (or << 16) | (og << 8) | ob;
    }

    #[inline(always)]
    fn cpu_sdf_pixel(
        dst: &mut u32,
        dx: f32,
        dy: f32,
        half_w: f32,
        half_h: f32,
        rad: f32,
        border_w: f32,
        fill: (u8, u8, u8, u8),
        border: (u8, u8, u8, u8),
    ) {
        let qx = dx.abs() - half_w + rad;
        let qy = dy.abs() - half_h + rad;
        let d = qx.max(qy).min(0.0) + (qx.max(0.0).hypot(qy.max(0.0))) - rad;
        let outer_a = (0.5 - d).clamp(0.0, 1.0);
        if outer_a <= 0.0 {
            return;
        }
        let bmix = if border_w > 0.0 {
            (0.5 + d + border_w).clamp(0.0, 1.0)
        } else {
            0.0
        };
        Self::blend_sdf_color(dst, outer_a, bmix, fill, border);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `kind = 4` capsule primitive must paint a filled anti-aliased band,
    /// not just its outline (a regression here renders one-pixel hairlines).
    #[test]
    fn kind4_line_fills_a_band() {
        let mut scene = Scene::new();
        scene.clear_all((0, 0, 0));
        scene.line(10.0, 10.0, 90.0, 10.0, 10.0, (255, 0, 255, 255));
        let (w, h) = (100u32, 40u32);
        let mut buf = vec![0u32; (w * h) as usize];
        let ts = TextSystem::new();
        Renderer::rasterize_slice(&mut buf, w, h, &scene.instances, &ts, scene.image_rgba.as_ref());
        let filled = buf.iter().filter(|&&p| p != 0).count();
        assert!(filled > 400, "kind4 band covers only {filled} px");
    }
}
