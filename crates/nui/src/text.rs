// Browser-grade typography (swash: OpenType shaping + TrueType hinting + 4x subpixel positioning
// + gamma-corrected linear-light blending) alongside the original fontdue baseline for live A/B comparison.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use fontdue::{Font as FdFont, FontSettings};
use swash::scale::{Render, ScaleContext, Source, StrikeWith};
use swash::shape::ShapeContext;
use swash::zeno::{Format, Vector};
use swash::{CacheKey, FontRef};

pub const ATLAS_W: usize = 2048;
pub const ATLAS_H: usize = 2048;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FontEngine {
    Swash,   // Shaped + hinted + 4x subpixel + gamma-corrected linear-light blend
    Fontdue, // Unhinted + integer-snapped + naive sRGB blend (legacy baseline)
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum FontFace {
    Regular = 0,
    SemiBold = 1,
    Icons = 2,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct GlyphKey {
    engine: u8,
    face: u8,
    glyph_id: u32, // glyph index for Swash, char codepoint for Fontdue
    px_tenths: u16,
    subpx_bin: u8, // 0..4 (0.0, 0.25, 0.50, 0.75 px horizontal shift)
}

#[derive(Clone, Copy, Debug)]
pub struct AtlasGlyph {
    pub uv_x: u16,
    pub uv_y: u16,
    pub w: u16,
    pub h: u16,
    pub offset_x: i16,
    pub offset_y: i16, // top offset relative to baseline (y-down screen coords)
}

#[derive(Clone, Copy, Debug)]
pub struct PositionedGlyph {
    pub x: i32,
    pub y: i32,
    pub uv_x: u16,
    pub uv_y: u16,
    pub w: u16,
    pub h: u16,
}

pub struct GammaLut {
    srgb_to_lin: [u16; 256], // 0..4095
    lin_to_srgb: [u8; 4096],
    cov_boost: [u16; 256],   // 0..256 stem-darkening coverage LUT for light text on dark bg
}

impl GammaLut {
    pub fn new() -> Self {
        let mut srgb_to_lin = [0u16; 256];
        let mut cov_boost = [0u16; 256];
        for i in 0..256 {
            let s = i as f32 / 255.0;
            let lin = if s <= 0.04045 {
                s / 12.92
            } else {
                ((s + 0.055) / 1.055).powf(2.4)
            };
            srgb_to_lin[i] = (lin.clamp(0.0, 1.0) * 4095.0 + 0.5) as u16;
            cov_boost[i] = (s.powf(1.0 / 1.35) * 256.0 + 0.5) as u16;
        }
        let mut lin_to_srgb = [0u8; 4096];
        for i in 0..4096 {
            let l = i as f32 / 4095.0;
            let s = if l <= 0.0031308 {
                l * 12.92
            } else {
                1.055 * l.powf(1.0 / 2.4) - 0.055
            };
            lin_to_srgb[i] = (s.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        }
        Self {
            srgb_to_lin,
            lin_to_srgb,
            cov_boost,
        }
    }

    #[inline(always)]
    pub fn blend_linear(&self, dst_rgb: u32, r: u8, g: u8, b: u8, cov: u8) -> u32 {
        if cov == 0 {
            return dst_rgb;
        }
        if cov == 255 {
            return ((r as u32) << 16) | ((g as u32) << 8) | (b as u32);
        }
        // Integer luminance check + LUT lookup (zero floats / zero powf)
        let a = if (r as u32 * 54 + g as u32 * 183 + b as u32 * 19) > (90 * 256) {
            self.cov_boost[cov as usize] as u32
        } else {
            (cov as u32 * 256) / 255
        };
        let inv = 256 - a;

        let dr = ((dst_rgb >> 16) & 0xFF) as usize;
        let dg = ((dst_rgb >> 8) & 0xFF) as usize;
        let db = (dst_rgb & 0xFF) as usize;

        let lr = ((self.srgb_to_lin[r as usize] as u32 * a
            + self.srgb_to_lin[dr] as u32 * inv)
            >> 8) as usize;
        let lg = ((self.srgb_to_lin[g as usize] as u32 * a
            + self.srgb_to_lin[dg] as u32 * inv)
            >> 8) as usize;
        let lb = ((self.srgb_to_lin[b as usize] as u32 * a
            + self.srgb_to_lin[db] as u32 * inv)
            >> 8) as usize;

        let or = self.lin_to_srgb[lr.min(4095)] as u32;
        let og = self.lin_to_srgb[lg.min(4095)] as u32;
        let ob = self.lin_to_srgb[lb.min(4095)] as u32;
        (or << 16) | (og << 8) | ob
    }

    #[inline(always)]
    pub fn blend_naive_srgb(dst_rgb: u32, r: u8, g: u8, b: u8, cov: u8) -> u32 {
        if cov == 0 {
            return dst_rgb;
        }
        let a = cov as u32;
        let inv = 255 - a;
        let dr = (dst_rgb >> 16) & 0xFF;
        let dg = (dst_rgb >> 8) & 0xFF;
        let db = dst_rgb & 0xFF;
        let or = (r as u32 * a + dr * inv) / 255;
        let og = (g as u32 * a + dg * inv) / 255;
        let ob = (b as u32 * a + db * inv) / 255;
        (or << 16) | (og << 8) | ob
    }
}

struct SwashFontData {
    bytes: &'static [u8],
    offset: u32,
    key: CacheKey,
    fd: FdFont,
}

impl SwashFontData {
    fn new(bytes: &'static [u8]) -> Self {
        let fref = FontRef::from_index(bytes, 0).expect("valid font");
        let fd = FdFont::from_bytes(bytes, FontSettings::default()).expect("fontdue load");
        Self {
            bytes,
            offset: fref.offset,
            key: fref.key,
            fd,
        }
    }

    fn as_ref(&self) -> FontRef<'static> {
        FontRef {
            data: self.bytes,
            offset: self.offset,
            key: self.key,
        }
    }
}

pub struct TextSystem {
    fonts: [SwashFontData; 3],
    shape_ctx: ShapeContext,
    scale_ctx: ScaleContext,
    cache: HashMap<GlyphKey, AtlasGlyph>,
    shape_cache: HashMap<(u8, u16, u64), Vec<(u16, f32, f32, f32)>>,
    pub atlas: Vec<u8>,
    cursor_x: u16,
    cursor_y: u16,
    row_h: u16,
    pub atlas_dirty: bool,
    /// Generation counter bumped on every atlas mutation (full clear or new glyph
    /// allocation). Each window's `GpuState` keeps the generation it last uploaded
    /// (`uploaded_atlas_gen`) and re-uploads whenever it drifts, so N windows each
    /// owning a private atlas texture stay synchronized with this shared system.
    pub atlas_gen: u64,
    pub gamma: GammaLut,
}

impl TextSystem {
    pub fn new() -> Self {
        let regular = SwashFontData::new(include_bytes!("../assets/fonts/OpenSans-Regular.ttf"));
        let semibold = SwashFontData::new(include_bytes!("../assets/fonts/OpenSans-SemiBold.ttf"));
        let icons = SwashFontData::new(include_bytes!("../assets/fonts/MaterialIcons-Regular.ttf"));
        Self {
            fonts: [regular, semibold, icons],
            shape_ctx: ShapeContext::new(),
            scale_ctx: ScaleContext::new(),
            cache: HashMap::with_capacity(512),
            shape_cache: HashMap::with_capacity(128),
            atlas: vec![0u8; ATLAS_W * ATLAS_H],
            cursor_x: 1,
            cursor_y: 1,
            row_h: 0,
            atlas_dirty: true,
            atlas_gen: 1,
            gamma: GammaLut::new(),
        }
    }

    /// Call before building a frame: if the atlas is nearing capacity, flush it cleanly
    /// at the frame boundary so all static + dynamic glyphs are re-emitted together.
    pub fn prepare_frame(&mut self) -> bool {
        if self.cursor_y + 160 >= ATLAS_H as u16 {
            self.atlas.fill(0);
            self.cache.clear();
            self.cursor_x = 1;
            self.cursor_y = 1;
            self.row_h = 0;
            self.atlas_dirty = true;
            self.atlas_gen = self.atlas_gen.wrapping_add(1);
            return true;
        }
        false
    }

    fn alloc_atlas(&mut self, w: u16, h: u16, bitmap: &[u8]) -> (u16, u16) {
        if w == 0 || h == 0 {
            return (0, 0);
        }
        let pad = 1u16;
        if self.cursor_x + w + pad >= ATLAS_W as u16 {
            self.cursor_x = 1;
            self.cursor_y = self.cursor_y.saturating_add(self.row_h + pad);
            self.row_h = 0;
        }
        if self.cursor_y + h + pad >= ATLAS_H as u16 {
            return (0, 0);
        }
        let ux = self.cursor_x;
        let uy = self.cursor_y;
        self.cursor_x += w + pad;
        if h > self.row_h {
            self.row_h = h;
        }
        for row in 0..h as usize {
            let dst_start = (uy as usize + row) * ATLAS_W + ux as usize;
            let src_start = row * w as usize;
            self.atlas[dst_start..dst_start + w as usize]
                .copy_from_slice(&bitmap[src_start..src_start + w as usize]);
        }
        self.atlas_dirty = true;
        self.atlas_gen = self.atlas_gen.wrapping_add(1);
        (ux, uy)
    }

    fn hash_str(s: &str) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        s.hash(&mut h);
        h.finish()
    }

    fn get_or_shape(&mut self, face: FontFace, text: &str, px: f32) -> &[(u16, f32, f32, f32)] {
        let px_tenths = (px * 10.0).round() as u16;
        let skey = (face as u8, px_tenths, Self::hash_str(text));
        if !self.shape_cache.contains_key(&skey) {
            let fref = self.fonts[face as usize].as_ref();
            let mut shaped = Vec::with_capacity(text.len());
            let mut shaper = self
                .shape_ctx
                .builder(fref)
                .size(px)
                .features(&[("kern", 1), ("liga", 1)])
                .build();
            shaper.add_str(text);
            shaper.shape_with(|cluster| {
                for g in cluster.glyphs {
                    shaped.push((g.id, g.advance, g.x, g.y));
                }
            });
            self.shape_cache.insert(skey, shaped);
        }
        self.shape_cache.get(&skey).unwrap()
    }

    /// Measure text width in physical pixels.
    pub fn measure(&mut self, face: FontFace, engine: FontEngine, text: &str, px: f32) -> f32 {
        if text.is_empty() {
            return 0.0;
        }
        match engine {
            FontEngine::Fontdue => {
                let fd = &self.fonts[face as usize].fd;
                text.chars().map(|c| fd.metrics(c, px).advance_width).sum()
            }
            FontEngine::Swash => self
                .get_or_shape(face, text, px)
                .iter()
                .map(|g| g.1)
                .sum(),
        }
    }

    /// Return X offset (in physical px) for a given UTF-8 byte index in `text`.
    pub fn byte_x_offset(
        &mut self,
        face: FontFace,
        engine: FontEngine,
        text: &str,
        byte_idx: usize,
        px: f32,
    ) -> f32 {
        let clamped = byte_idx.min(text.len());
        if clamped == 0 {
            return 0.0;
        }
        self.measure(face, engine, &text[..clamped], px)
    }

    /// Map a local X coordinate (physical px from start of string) to the nearest UTF-8 char boundary.
    pub fn hit_test_byte(
        &mut self,
        face: FontFace,
        engine: FontEngine,
        text: &str,
        local_x: f32,
        px: f32,
    ) -> usize {
        if local_x <= 0.0 || text.is_empty() {
            return 0;
        }
        let mut prev_idx = 0;
        let mut prev_x = 0.0f32;
        for (idx, ch) in text.char_indices() {
            let next_idx = idx + ch.len_utf8();
            let next_x = self.measure(face, engine, &text[..next_idx], px);
            let mid = (prev_x + next_x) * 0.5;
            if local_x < mid {
                return prev_idx;
            }
            prev_idx = next_idx;
            prev_x = next_x;
        }
        text.len()
    }

    /// Rasterize a single Material Icon glyph and center its true pixel bounding box at `(cx, cy)`.
    pub fn layout_icon_centered(
        &mut self,
        icon: &str,
        cx: f32,
        cy: f32,
        px: f32,
    ) -> Option<PositionedGlyph> {
        let px_tenths = (px * 10.0).round() as u16;
        let shaped = self.get_or_shape(FontFace::Icons, icon, px);
        let &(gid, _, _, _) = shaped.first()?;
        let key = GlyphKey {
            engine: 0,
            face: FontFace::Icons as u8,
            glyph_id: gid as u32,
            px_tenths,
            subpx_bin: 0,
        };
        let ag = if let Some(&cached) = self.cache.get(&key) {
            cached
        } else {
            let fref = self.fonts[FontFace::Icons as usize].as_ref();
            let mut scaler = self.scale_ctx.builder(fref).size(px).hint(true).build();
            let img = Render::new(&[Source::Outline, Source::Bitmap(StrikeWith::BestFit)])
                .format(Format::Alpha)
                .render(&mut scaler, gid);
            let entry = if let Some(img) = img {
                let w = img.placement.width as u16;
                let h = img.placement.height as u16;
                let (ux, uy) = self.alloc_atlas(w, h, &img.data);
                AtlasGlyph {
                    uv_x: ux,
                    uv_y: uy,
                    w,
                    h,
                    offset_x: img.placement.left as i16,
                    offset_y: -(img.placement.top as i16),
                }
            } else {
                AtlasGlyph {
                    uv_x: 0,
                    uv_y: 0,
                    w: 0,
                    h: 0,
                    offset_x: 0,
                    offset_y: 0,
                }
            };
            self.cache.insert(key, entry);
            entry
        };
        if ag.w == 0 || ag.h == 0 {
            return None;
        }
        Some(PositionedGlyph {
            x: (cx - ag.w as f32 * 0.5).round() as i32,
            y: (cy - ag.h as f32 * 0.5).round() as i32,
            uv_x: ag.uv_x,
            uv_y: ag.uv_y,
            w: ag.w,
            h: ag.h,
        })
    }

    /// Shape and lay out `text` into positioned atlas quads.
    pub fn layout_run(
        &mut self,
        face: FontFace,
        engine: FontEngine,
        text: &str,
        x: f32,
        baseline: f32,
        px: f32,
        out: &mut Vec<PositionedGlyph>,
    ) -> f32 {
        let px_tenths = (px * 10.0).round() as u16;
        let baseline_y = baseline.round() as i32;

        match engine {
            FontEngine::Fontdue => {
                let mut pen_x = x;
                for ch in text.chars() {
                    let key = GlyphKey {
                        engine: 1,
                        face: face as u8,
                        glyph_id: ch as u32,
                        px_tenths,
                        subpx_bin: 0,
                    };
                    let ag = if let Some(&cached) = self.cache.get(&key) {
                        cached
                    } else {
                        let (m, bmp) = self.fonts[face as usize].fd.rasterize(ch, px);
                        let (ux, uy) = self.alloc_atlas(m.width as u16, m.height as u16, &bmp);
                        let entry = AtlasGlyph {
                            uv_x: ux,
                            uv_y: uy,
                            w: m.width as u16,
                            h: m.height as u16,
                            offset_x: m.xmin as i16,
                            offset_y: -((m.height as i32 + m.ymin) as i16),
                        };
                        self.cache.insert(key, entry);
                        entry
                    };
                    let adv = self.fonts[face as usize].fd.metrics(ch, px).advance_width;
                    if ag.w > 0 && ag.h > 0 {
                        out.push(PositionedGlyph {
                            x: (pen_x + ag.offset_x as f32).round() as i32,
                            y: baseline_y + ag.offset_y as i32,
                            uv_x: ag.uv_x,
                            uv_y: ag.uv_y,
                            w: ag.w,
                            h: ag.h,
                        });
                    }
                    pen_x += adv;
                }
                pen_x - x
            }
            FontEngine::Swash => {
                let skey = (face as u8, px_tenths, Self::hash_str(text));
                if !self.shape_cache.contains_key(&skey) {
                    let _ = self.get_or_shape(face, text, px);
                }
                let fref = self.fonts[face as usize].as_ref();
                let shaped = self.shape_cache.get(&skey).unwrap();
                let mut pen_x = x;
                for i in 0..shaped.len() {
                    let (gid, advance, gx_off, gy_off) = self.shape_cache.get(&skey).unwrap()[i];
                    let exact_x = pen_x + gx_off;
                    let base_x = exact_x.floor();
                    let frac_x = exact_x - base_x;
                    // Quantize fractional X to 4 subpixel bins: 0.0, 0.25, 0.50, 0.75
                    let bin = ((frac_x * 4.0).round() as u8).min(3);
                    let (int_x, subpx_bin) = if (frac_x * 4.0).round() >= 4.0 {
                        (base_x as i32 + 1, 0u8)
                    } else {
                        (base_x as i32, bin)
                    };

                    let key = GlyphKey {
                        engine: 0,
                        face: face as u8,
                        glyph_id: gid as u32,
                        px_tenths,
                        subpx_bin,
                    };

                    let ag = if let Some(&cached) = self.cache.get(&key) {
                        cached
                    } else {
                        let subpx_shift = subpx_bin as f32 * 0.25;
                        let mut scaler = self
                            .scale_ctx
                            .builder(fref)
                            .size(px)
                            .hint(true)
                            .build();
                        let img = Render::new(&[
                            Source::Outline,
                            Source::Bitmap(StrikeWith::BestFit),
                        ])
                        .format(Format::Alpha)
                        .offset(Vector::new(subpx_shift, 0.0))
                        .render(&mut scaler, gid);

                        let entry = if let Some(img) = img {
                            let w = img.placement.width as u16;
                            let h = img.placement.height as u16;
                            let (ux, uy) = self.alloc_atlas(w, h, &img.data);
                            AtlasGlyph {
                                uv_x: ux,
                                uv_y: uy,
                                w,
                                h,
                                offset_x: img.placement.left as i16,
                                offset_y: -(img.placement.top as i16),
                            }
                        } else {
                            AtlasGlyph {
                                uv_x: 0,
                                uv_y: 0,
                                w: 0,
                                h: 0,
                                offset_x: 0,
                                offset_y: 0,
                            }
                        };
                        self.cache.insert(key, entry);
                        entry
                    };

                    if ag.w > 0 && ag.h > 0 {
                        out.push(PositionedGlyph {
                            x: int_x + ag.offset_x as i32,
                            y: (baseline - gy_off).round() as i32 + ag.offset_y as i32,
                            uv_x: ag.uv_x,
                            uv_y: ag.uv_y,
                            w: ag.w,
                            h: ag.h,
                        });
                    }
                    pen_x += advance;
                }
                pen_x - x
            }
        }
    }
}

