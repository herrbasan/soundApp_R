//! Phase 2.5 — ProTracker `.mod` engine.
//!
//! A native parser for ProTracker modules (`.mod`, `M.K.` / `4CHN` / `6CHN` /
//! `8CHN` signatures) plus a 4–8 channel Paula-style tracker synthesiser and a
//! built-in demoscene chiptune so the engine always has something to play. It
//! exposes the SoundApp tracker parameter set: `pitch_semitones`, `tempo_ratio`,
//! `stereo_separation`, `interp_filter`, and per-channel VU levels.

use std::path::Path;

/// Maximum number of tracker channels supported.
pub const MAX_CHANNELS: usize = 8;

/// Amiga PAL Paula clock / 2 — maps a period to a frequency.
const PAULA_CLOCK: f64 = 3_546_894.6;

/// ProTracker period table (finetune 0), three octaves from C-1.
const PERIODS: [u16; 36] = [
    856, 808, 762, 720, 678, 640, 604, 570, 538, 508, 480, 453, 428, 404, 381, 360, 339, 320, 302,
    285, 269, 254, 240, 226, 214, 202, 190, 180, 169, 160, 151, 143, 135, 127, 120, 113,
];

// The table is exposed for tests / future note-name helpers.
#[allow(dead_code)]
fn period_of(note: usize) -> u16 {
    PERIODS[note.min(PERIODS.len() - 1)]
}

#[derive(Clone, Copy, Default)]
struct Cell {
    sample: u8,
    period: u16,
    effect: u8,
    param: u8,
}

#[derive(Clone)]
struct Sample {
    data: Vec<f32>,
    volume: u8,
    finetune: i8,
    loop_start: usize,
    loop_len: usize,
}

/// A parsed ProTracker module.
pub struct TrackerModule {
    pub title: String,
    pub channels: usize,
    samples: Vec<Sample>,
    patterns: Vec<Vec<[Cell; MAX_CHANNELS]>>,
    order: Vec<u8>,
    song_len: usize,
    restart: u8,
}

impl TrackerModule {
    /// Parse a ProTracker `.mod` from raw bytes.
    pub fn parse(data: &[u8]) -> Result<TrackerModule, String> {
        if data.len() < 1084 {
            return Err("module too short for a ProTracker header".into());
        }
        let title = read_title(&data[0..20]);

        // --- 31 sample headers -----------------------------
        let mut samples: Vec<Sample> = Vec::with_capacity(31);
        let mut sample_lens: Vec<usize> = Vec::with_capacity(31);
        for i in 0..31 {
            let off = 20 + i * 30;
            let len_words = u16::from_be_bytes([data[off + 22], data[off + 23]]) as usize;
            let finetune = {
                let ft = data[off + 24] & 0x0F;
                if ft >= 8 {
                    ft as i8 - 16
                } else {
                    ft as i8
                }
            };
            let volume = data[off + 25].min(64);
            let loop_start_words = u16::from_be_bytes([data[off + 26], data[off + 27]]) as usize;
            let loop_len_words = u16::from_be_bytes([data[off + 28], data[off + 29]]) as usize;
            samples.push(Sample {
                data: Vec::new(),
                volume,
                finetune,
                loop_start: loop_start_words * 2,
                loop_len: loop_len_words * 2,
            });
            sample_lens.push(len_words * 2);
        }

        // --- song layout -----------------------------------
        let song_len = (data[950] as usize).clamp(1, 128);
        let restart = data[951];
        let mut order = vec![0u8; 128];
        order.copy_from_slice(&data[952..1080]);
        let channels = detect_channels(&data[1080..1084]);

        let mut max_pat = 0usize;
        for &o in order.iter().take(song_len) {
            max_pat = max_pat.max(o as usize);
        }
        let n_patterns = max_pat + 1;

        let row_bytes = channels * 4;
        let pattern_bytes = 64 * row_bytes;
        let hdr = 1084usize;

        let mut patterns: Vec<Vec<[Cell; MAX_CHANNELS]>> = Vec::with_capacity(n_patterns);
        for p in 0..n_patterns {
            let base = hdr + p * pattern_bytes;
            let mut rows = vec![[Cell::default(); MAX_CHANNELS]; 64];
            if base + pattern_bytes <= data.len() {
                for r in 0..64 {
                    for c in 0..channels {
                        let o = base + (r * channels + c) * 4;
                        let b0 = data[o];
                        let b1 = data[o + 1];
                        let b2 = data[o + 2];
                        let b3 = data[o + 3];
                        rows[r][c] = Cell {
                            sample: (b0 & 0xF0) | (b2 >> 4),
                            period: (((b0 & 0x0F) as u16) << 8) | b1 as u16,
                            effect: b2 & 0x0F,
                            param: b3,
                        };
                    }
                }
            }
            patterns.push(rows);
        }

        // --- sample data (8-bit signed) --------------------
        let mut pos = hdr + n_patterns * pattern_bytes;
        for i in 0..samples.len() {
            let len = sample_lens[i];
            let end = (pos + len).min(data.len());
            let mut v: Vec<f32> = Vec::with_capacity(len);
            for &b in &data[pos..end] {
                v.push((b as i8) as f32 / 128.0);
            }
            // Drop a loop that points past the (possibly truncated) sample.
            if samples[i].loop_start + samples[i].loop_len > v.len().max(2) {
                samples[i].loop_len = 0;
            }
            samples[i].data = v;
            pos += len;
        }

        Ok(TrackerModule {
            title,
            channels,
            samples,
            patterns,
            order,
            song_len,
            restart,
        })
    }

    /// Build a self-contained 4-channel demoscene chiptune.
    pub fn builtin_demoscene() -> TrackerModule {
        let square = oscillator(64, |p| if p < 0.5 { 0.6 } else { -0.6 });
        let saw = oscillator(32, |p| p * 2.0 - 1.0);
        let pulse = oscillator(48, |p| if p < 0.25 { 0.7 } else { -0.7 });
        let noise = prng_wave(64, 0x1234_5678);

        let samples = vec![
            Sample { data: square, volume: 64, finetune: 0, loop_start: 0, loop_len: 64 },
            Sample { data: saw, volume: 48, finetune: 0, loop_start: 0, loop_len: 32 },
            Sample { data: pulse, volume: 40, finetune: 0, loop_start: 0, loop_len: 48 },
            Sample { data: noise, volume: 56, finetune: 0, loop_start: 0, loop_len: 64 },
            Sample { data: drum_kick(0.35), volume: 64, finetune: 0, loop_start: 0, loop_len: 0 },
            Sample { data: drum_snare(0.25), volume: 64, finetune: 0, loop_start: 0, loop_len: 0 },
            Sample { data: drum_hat(0.06), volume: 48, finetune: 0, loop_start: 0, loop_len: 0 },
        ];

        let bass_notes: [u16; 16] = [
            428, 428, 508, 508, 570, 570, 508, 508, 428, 428, 508, 508, 453, 453, 508, 508,
        ];
        let lead_notes: [u16; 16] = [
            214, 240, 269, 240, 202, 226, 254, 226, 180, 202, 226, 254, 240, 214, 202, 190,
        ];
        const DRUM_PERIOD: u16 = 74; // one-shot drums play ~1:1 at 48 kHz

        let mut rows = vec![[Cell::default(); MAX_CHANNELS]; 64];
        for r in 0..64 {
            if r % 4 == 0 {
                rows[r][0] = Cell { sample: 1, period: bass_notes[(r / 4) % 16], effect: 0, param: 0 };
            }
            if r % 2 == 0 {
                rows[r][1] = Cell { sample: 2, period: lead_notes[(r / 2) % 16], effect: 0, param: 0 };
            }
            if r % 16 == 0 {
                rows[r][2] = Cell { sample: 3, period: 302, effect: 0, param: 0 };
            }
            if r % 8 == 0 {
                rows[r][3] = Cell { sample: 5, period: DRUM_PERIOD, effect: 0, param: 0 };
            } else if r % 8 == 4 {
                rows[r][3] = Cell { sample: 6, period: DRUM_PERIOD, effect: 0, param: 0 };
            } else {
                rows[r][3] = Cell { sample: 7, period: DRUM_PERIOD, effect: 0, param: 0 };
            }
        }

        let mut order = vec![0u8; 128];
        order[..8].fill(0);

        TrackerModule {
            title: "NUI Rust · Built-in Demoscene".to_string(),
            channels: 4,
            samples,
            patterns: vec![rows],
            order,
            song_len: 8,
            restart: 0,
        }
    }

    fn order_len(&self) -> usize {
        self.song_len.clamp(1, 128)
    }
}

fn read_title(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).trim().to_string()
}

fn detect_channels(sig: &[u8]) -> usize {
    match sig {
        b"6CHN" => 6,
        b"8CHN" | b"FLT8" | b"OKTA" | b"CD81" => 8,
        _ => 4,
    }
}

fn oscillator(n: usize, f: impl Fn(f32) -> f32) -> Vec<f32> {
    (0..n).map(|i| f(i as f32 / n as f32)).collect()
}

fn prng_wave(n: usize, seed: u32) -> Vec<f32> {
    let mut st = seed | 1;
    let mut v = Vec::with_capacity(n);
    for _ in 0..n {
        st ^= st << 13;
        st ^= st >> 17;
        st ^= st << 5;
        v.push((st as f32 / u32::MAX as f32) * 2.0 - 1.0);
    }
    v
}

fn drum_kick(seconds: f32) -> Vec<f32> {
    const R: f32 = 48_000.0;
    let n = (seconds * R) as usize;
    let mut v = Vec::with_capacity(n);
    let mut phase = 0.0f32;
    for i in 0..n {
        let t = i as f32 / R;
        let env = (-t * 12.0).exp();
        let f = 150.0 * (-t * 18.0).exp() + 45.0;
        phase += 2.0 * std::f32::consts::PI * f / R;
        v.push((phase.sin() * env * 0.9).clamp(-1.0, 1.0));
    }
    v
}

fn drum_snare(seconds: f32) -> Vec<f32> {
    const R: f32 = 48_000.0;
    let n = (seconds * R) as usize;
    let mut v = Vec::with_capacity(n);
    let mut st = 0x9E37_79B9u32;
    for i in 0..n {
        let t = i as f32 / R;
        st ^= st << 13;
        st ^= st >> 17;
        st ^= st << 5;
        let noise = (st as f32 / u32::MAX as f32) * 2.0 - 1.0;
        let tone = (2.0 * std::f32::consts::PI * 180.0 * t).sin();
        let env = (-t * 22.0).exp();
        v.push(((noise * 0.7 + tone * 0.3) * env).clamp(-1.0, 1.0));
    }
    v
}

fn drum_hat(seconds: f32) -> Vec<f32> {
    const R: f32 = 48_000.0;
    let n = (seconds * R) as usize;
    let mut st = 0xDEAD_BEEFu32;
    let mut v = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f32 / R;
        st ^= st << 13;
        st ^= st >> 17;
        st ^= st << 5;
        let noise = (st as f32 / u32::MAX as f32) * 2.0 - 1.0;
        let env = (-t * 90.0).exp();
        v.push((noise * env * 0.5).clamp(-1.0, 1.0));
    }
    v
}

// ---------------------------------------------------------------------------
// Player
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Default)]
struct ChannelState {
    sample: u8,
    period: u16,
    vol: f32,
    pos: f64,
    active: bool,
}

/// A real-time ProTracker playback engine.
pub struct TrackerPlayer {
    rate: f32,
    module: TrackerModule,
    /// Ticks per row (effect `Fxx` / speed). Default 6.
    speed: u32,
    /// Beats per minute (effect `Fxx` / tempo). Default 125.
    tempo: u32,
    order_pos: usize,
    row: usize,
    tick: u32,
    frames_to_tick: f64,
    channels: [ChannelState; MAX_CHANNELS],
    pitch_semitones: f32,
    tempo_ratio: f32,
    stereo_separation: f32,
    interp_filter: usize,
    pending_break: Option<usize>,
    pending_jump: Option<usize>,
    /// Per-channel VU levels (0..1), updated every rendered frame.
    pub channel_vu: [f32; MAX_CHANNELS],
}

impl TrackerPlayer {
    /// Create a player for `sample_rate`, loaded with the built-in module.
    pub fn new(sample_rate: u32) -> TrackerPlayer {
        let mut p = TrackerPlayer {
            rate: sample_rate as f32,
            module: TrackerModule::builtin_demoscene(),
            speed: 6,
            tempo: 125,
            order_pos: 0,
            row: 0,
            tick: 0,
            frames_to_tick: 0.0,
            channels: [ChannelState::default(); MAX_CHANNELS],
            pitch_semitones: 0.0,
            tempo_ratio: 1.0,
            stereo_separation: 100.0,
            interp_filter: 1,
            pending_break: None,
            pending_jump: None,
            channel_vu: [0.0; MAX_CHANNELS],
        };
        p.reset();
        p
    }

    /// Load and parse a `.mod` file, replacing the current module.
    pub fn load_file(&mut self, path: &Path) -> Result<(), String> {
        let data = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
        self.module = TrackerModule::parse(&data)?;
        self.reset();
        Ok(())
    }

    /// Replace the current module with the built-in demoscene chiptune.
    pub fn load_builtin(&mut self) {
        self.module = TrackerModule::builtin_demoscene();
        self.reset();
    }

    /// Set the global pitch offset (`-12..=12` semitones).
    pub fn set_pitch(&mut self, semitones: f32) {
        self.pitch_semitones = semitones.clamp(-12.0, 12.0);
    }

    /// Set the tempo ratio (`0.5..=2.0`; `1.0` = the module's native speed).
    pub fn set_tempo(&mut self, ratio: f32) {
        self.tempo_ratio = ratio.clamp(0.5, 2.0);
    }

    /// Set stereo separation (`0` = mono, `100` = Amiga LRRL, `200` = wide).
    pub fn set_stereo_separation(&mut self, sep: f32) {
        self.stereo_separation = sep.clamp(0.0, 200.0);
    }

    /// Select the interpolation filter (0 nearest, 1 linear, 2 cubic, 3 sinc-8).
    pub fn set_interp(&mut self, interp: usize) {
        self.interp_filter = interp.min(3);
    }

    /// Title of the loaded module.
    pub fn title(&self) -> &str {
        &self.module.title
    }

    fn reset(&mut self) {
        self.order_pos = 0;
        self.row = 0;
        self.tick = 0;
        self.frames_to_tick = 0.0;
        self.channels = [ChannelState::default(); MAX_CHANNELS];
        self.channel_vu = [0.0; MAX_CHANNELS];
        self.pending_break = None;
        self.pending_jump = None;
    }

    fn tick_frames(&self) -> f64 {
        // PAL timing: one tick = 2.5/tempo seconds.
        let sec = 2.5 / self.tempo.max(1) as f64;
        (sec * self.rate as f64 / self.tempo_ratio.max(0.01) as f64).max(1.0)
    }

    fn advance_tick(&mut self) {
        if self.tick == 0 {
            self.process_row();
        }
        self.tick += 1;
        if self.tick >= self.speed.max(1) {
            self.tick = 0;
            if let Some(pos) = self.pending_jump.take() {
                self.order_pos = pos;
                self.row = self.pending_break.take().unwrap_or(0);
            } else if let Some(row) = self.pending_break.take() {
                self.row = row;
                self.order_pos += 1;
            } else {
                self.row += 1;
            }
            if self.row >= 64 {
                self.row = 0;
                self.order_pos += 1;
            }
            let used = self.module.order_len();
            if self.order_pos >= used {
                self.order_pos = (self.module.restart as usize).min(used.saturating_sub(1));
            }
        }
    }

    fn process_row(&mut self) {
        let pat_idx = *self.module.order.get(self.order_pos).unwrap_or(&0) as usize;
        if pat_idx >= self.module.patterns.len() {
            return;
        }
        let row = self.row.min(63);
        let cells = self.module.patterns[pat_idx][row];
        let n_samples = self.module.samples.len();
        let n_ch = self.module.channels.clamp(1, MAX_CHANNELS);

        for c in 0..n_ch {
            let cell = cells[c];
            if cell.sample != 0 {
                let si = cell.sample as usize;
                if si <= n_samples {
                    let vol = self.module.samples[si - 1].volume as f32 / 64.0;
                    let empty = self.module.samples[si - 1].data.is_empty();
                    let s = &mut self.channels[c];
                    s.sample = cell.sample;
                    s.vol = vol;
                    if cell.period == 0 {
                        s.pos = 0.0;
                        s.active = !empty;
                    }
                }
            }
            if cell.period != 0 {
                let si = self.channels[c].sample as usize;
                let active = si >= 1 && si <= n_samples && !self.module.samples[si - 1].data.is_empty();
                let s = &mut self.channels[c];
                s.period = cell.period;
                s.pos = 0.0;
                s.active = active;
            }
            let p = cell.param;
            match cell.effect {
                0xB => self.pending_jump = Some((p as usize).min(127)),
                0xC => self.channels[c].vol = p.min(64) as f32 / 64.0,
                0xD => {
                    let row = ((p >> 4) as usize) * 10 + (p & 0x0F) as usize;
                    self.pending_break = Some(row.min(63));
                }
                0xF => {
                    if p >= 0x20 {
                        self.tempo = p as u32;
                    } else if p > 0 {
                        self.speed = p as u32;
                    }
                }
                _ => {}
            }
        }
    }

    /// Render `left.len()` stereo frames.
    pub fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        let n = left.len().min(right.len());
        let pitch_factor = 2f64.powf(self.pitch_semitones as f64 / 12.0);
        let sep = (self.stereo_separation / 100.0) as f64;
        let n_ch = self.module.channels.clamp(1, MAX_CHANNELS);

        for i in 0..n {
            if self.frames_to_tick <= 0.0 {
                self.advance_tick();
                self.frames_to_tick = self.tick_frames();
            }
            self.frames_to_tick -= 1.0;

            let mut l = 0.0f64;
            let mut r = 0.0f64;
            let mut frame_peak = [0.0f32; MAX_CHANNELS];

            for c in 0..n_ch {
                let ch = self.channels[c];
                if !ch.active || ch.sample == 0 || ch.period == 0 {
                    continue;
                }
                let si = ch.sample as usize;
                if si > self.module.samples.len() {
                    continue;
                }
                let sample = &self.module.samples[si - 1];
                if sample.data.is_empty() {
                    continue;
                }
                let v = sample_at(&sample.data, ch.pos, self.interp_filter);
                let out = v * ch.vol;
                frame_peak[c] = out.abs();

                // Amiga LRRL panning scaled by separation.
                let base = if c % 4 == 0 || c % 4 == 3 { -1.0 } else { 1.0 };
                let pan = base * sep;
                l += (out as f64) * 0.5 * (1.0 - pan);
                r += (out as f64) * 0.5 * (1.0 + pan);

                let finetune = sample.finetune as f64 / 96.0;
                let freq = PAULA_CLOCK / ch.period as f64 * 2f64.powf(finetune) * pitch_factor;
                let step = freq / self.rate as f64;
                let mut pos = ch.pos + step;
                let loop_end = sample.loop_start + sample.loop_len;
                if sample.loop_len > 2 && loop_end <= sample.data.len() {
                    while pos >= loop_end as f64 {
                        pos -= sample.loop_len as f64;
                    }
                    if pos < 0.0 {
                        pos = 0.0;
                    }
                } else if pos >= sample.data.len() as f64 {
                    self.channels[c].active = false;
                    pos = 0.0;
                }
                self.channels[c].pos = pos;
            }

            left[i] = (l * 0.5).clamp(-1.0, 1.0) as f32;
            right[i] = (r * 0.5).clamp(-1.0, 1.0) as f32;
            for c in 0..MAX_CHANNELS {
                self.channel_vu[c] = self.channel_vu[c] * 0.9 + frame_peak[c] * 0.1;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Interpolators
// ---------------------------------------------------------------------------

#[inline]
fn tap(data: &[f32], i: isize) -> f32 {
    if i < 0 {
        return 0.0;
    }
    *data.get(i as usize).unwrap_or(&0.0)
}

fn sample_at(data: &[f32], pos: f64, interp: usize) -> f32 {
    let i0 = pos.floor();
    let frac = (pos - i0) as f32;
    let i0i = i0 as isize;
    match interp {
        1 => {
            let a = tap(data, i0i);
            let b = tap(data, i0i + 1);
            a + (b - a) * frac
        }
        2 => {
            let p0 = tap(data, i0i - 1);
            let p1 = tap(data, i0i);
            let p2 = tap(data, i0i + 1);
            let p3 = tap(data, i0i + 2);
            let a = -0.5 * p0 + 1.5 * p1 - 1.5 * p2 + 0.5 * p3;
            let b = p0 - 2.5 * p1 + 2.0 * p2 - 0.5 * p3;
            let c = -0.5 * p0 + 0.5 * p2;
            ((a * frac + b) * frac + c) * frac + p1
        }
        3 => {
            let mut acc = 0.0f32;
            let mut wsum = 0.0f32;
            for k in -3i32..=4 {
                let x = frac - k as f32;
                let w = lanczos(x);
                acc += w * tap(data, i0i + k as isize);
                wsum += w;
            }
            if wsum.abs() > 1e-6 {
                acc / wsum
            } else {
                tap(data, i0i)
            }
        }
        _ => tap(data, i0i),
    }
}

#[inline]
fn lanczos(x: f32) -> f32 {
    if x.abs() < 1e-6 {
        return 1.0;
    }
    if x.abs() >= 4.0 {
        return 0.0;
    }
    let px = std::f32::consts::PI * x;
    let ax = px / 4.0;
    (px.sin() / px) * (ax.sin() / ax)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_builtin_module_in_stereo() {
        let mut p = TrackerPlayer::new(48_000);
        p.load_builtin();
        p.set_stereo_separation(100.0);
        p.set_interp(2);
        let mut l = vec![0.0f32; 4800];
        let mut r = vec![0.0f32; 4800];
        p.render(&mut l, &mut r);
        let energy: f32 = l.iter().chain(r.iter()).map(|v| v * v).sum();
        assert!(energy > 0.0, "expected non-zero tracker output");
        assert!(
            p.channel_vu.iter().any(|&v| v > 0.0),
            "expected non-zero channel VU: {:?}",
            p.channel_vu
        );
    }

    #[test]
    fn builtin_module_has_expected_shape() {
        let m = TrackerModule::builtin_demoscene();
        assert_eq!(m.channels, 4);
        assert_eq!(m.samples.len(), 7);
        assert_eq!(m.patterns.len(), 1);
        assert_eq!(m.order_len(), 8);
    }
}
