// Real-Time DSP analysis tap (Phase 3.3).
//
// Consumes the post-fader stereo stream published by the audio callback and derives
// everything the AUDIO MONITORING window draws:
//   * 2048-point Hann-windowed radix-2 FFT mapped onto 64 logarithmic bands
//     (20 Hz .. 20 kHz) with instant-attack / exponential-release ballistics and a
//     decaying peak-hold curve, plus the dominant peak `(peak_hz, peak_db)`.
//   * A zero-crossing-stabilized 256-point oscilloscope trace and stereo RMS.
//   * A 192-point stereo goniometer (Lissajous) with smoothed phase correlation.
//   * BS.1770-4 K-weighted loudness (momentary / short-term / integrated / true peak)
//     and stereo L/R peak + RMS meter levels.
//
// Zero external dependencies: the FFT is a plain iterative Cooley-Tukey transform,
// and the K-weighting is the standard two-stage biquad from ITU-R BS.1770-4.

use std::collections::VecDeque;
use std::f32::consts::PI;

pub const FFT_SIZE: usize = 2048;
const HOP: usize = FFT_SIZE / 2;
pub const BANDS: usize = 64;
pub const OSC_POINTS: usize = 256;
pub const GONIO_POINTS: usize = 192;

const F_MIN: f32 = 20.0;
const F_MAX: f32 = 20_000.0;
const DB_FLOOR: f32 = -60.0;
const EPS: f32 = 1e-10;
/// BS.1770 loudness offset (`-0.691`), the calibration constant for a 997 Hz sine.
const LUFS_OFFSET: f32 = -0.691;

/// One immutable analysis frame handed to the renderer each redraw.
#[derive(Clone)]
pub struct DspSnapshot {
    pub spectrum_db: [f32; BANDS],
    pub peak_hold_db: [f32; BANDS],
    pub peak_hz: f32,
    pub peak_db: f32,
    pub osc_points: Vec<(f32, f32)>,
    pub rms_db: f32,
    pub gonio_points: Vec<(f32, f32)>,
    pub correlation: f32,
    pub momentary_lufs: f32,
    pub short_term_lufs: f32,
    pub integrated_lufs: f32,
    pub true_peak_dbtp: f32,
    pub vu_l_db: f32,
    pub vu_r_db: f32,
    pub peak_l_db: f32,
    pub peak_r_db: f32,
}

impl Default for DspSnapshot {
    fn default() -> Self {
        Self {
            spectrum_db: [DB_FLOOR; BANDS],
            peak_hold_db: [DB_FLOOR; BANDS],
            peak_hz: 0.0,
            peak_db: DB_FLOOR,
            osc_points: Vec::new(),
            rms_db: DB_FLOOR,
            gonio_points: Vec::new(),
            correlation: 0.0,
            momentary_lufs: DB_FLOOR,
            short_term_lufs: DB_FLOOR,
            integrated_lufs: DB_FLOOR,
            true_peak_dbtp: DB_FLOOR,
            vu_l_db: DB_FLOOR,
            vu_r_db: DB_FLOOR,
            peak_l_db: DB_FLOOR,
            peak_r_db: DB_FLOOR,
        }
    }
}

/// Direct-form-I biquad (BS.1770 K-weighting stages).
#[derive(Clone, Copy)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl Biquad {
    fn new(b: [f32; 3], a: [f32; 3]) -> Self {
        Self {
            b0: b[0],
            b1: b[1],
            b2: b[2],
            a1: a[1],
            a2: a[2],
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1
            - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }

    fn reset(&mut self) {
        self.x1 = 0.0;
        self.x2 = 0.0;
        self.y1 = 0.0;
        self.y2 = 0.0;
    }
}

/// Fixed-capacity running mean of squared samples (BS.1770 gating windows).
struct SlidingMeanSquare {
    cap: usize,
    squares: Vec<f32>,
    idx: usize,
    count: usize,
    sum: f32,
}

impl SlidingMeanSquare {
    fn new(cap: usize) -> Self {
        let cap = cap.max(1);
        Self {
            cap,
            squares: vec![0.0; cap],
            idx: 0,
            count: 0,
            sum: 0.0,
        }
    }

    #[inline]
    fn push(&mut self, x: f32) {
        let sq = x * x;
        if self.count == self.cap {
            self.sum -= self.squares[self.idx];
        } else {
            self.count += 1;
        }
        self.squares[self.idx] = sq;
        self.sum += sq;
        self.idx = (self.idx + 1) % self.cap;
    }

    #[inline]
    fn mean(&self) -> f32 {
        if self.count == 0 {
            0.0
        } else {
            self.sum / self.count as f32
        }
    }

    fn reset(&mut self) {
        self.squares.iter_mut().for_each(|s| *s = 0.0);
        self.idx = 0;
        self.count = 0;
        self.sum = 0.0;
    }
}

/// In-place iterative radix-2 Cooley-Tukey FFT (input length must be a power of two).
fn fft_in_place(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    if n < 2 {
        return;
    }
    // Bit-reversal permutation.
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    // Butterfly stages.
    let mut len = 2usize;
    while len <= n {
        let ang = -2.0 * PI / len as f32;
        let (wr, wi) = (ang.cos(), ang.sin());
        let half = len >> 1;
        let mut i = 0usize;
        while i < n {
            let mut wpr = 1.0f32;
            let mut wpi = 0.0f32;
            for k in 0..half {
                let a = i + k;
                let b = a + half;
                let tr = re[b] * wpr - im[b] * wpi;
                let ti = re[b] * wpi + im[b] * wpr;
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
                let nwr = wpr * wr - wpi * wi;
                wpi = wpr * wi + wpi * wr;
                wpr = nwr;
            }
            i += len;
        }
        len <<= 1;
    }
}

#[inline]
fn to_db(amp: f32) -> f32 {
    20.0 * (amp.abs() + EPS).log10()
}

/// Track the peak envelope over a slowly decaying window (POC true-peak estimate).
#[inline]
fn true_peak_update(state: &mut f32, x: f32) {
    *state *= 0.999;
    let a = x.abs();
    if a > *state {
        *state = a;
    }
}

/// Stateful real-time analyzer. Feed it frames with [`DspAnalyzer::process`], then
/// read a consistent view with [`DspAnalyzer::snapshot`].
pub struct DspAnalyzer {
    rate: f32,
    window: [f32; FFT_SIZE],
    fft_re: [f32; FFT_SIZE],
    fft_im: [f32; FFT_SIZE],
    input: Vec<f32>,
    band_lo: [usize; BANDS],
    band_hi: [usize; BANDS],

    spectrum_db: [f32; BANDS],
    peak_hold_db: [f32; BANDS],
    peak_hz: f32,
    peak_db: f32,

    recent: VecDeque<(f32, f32)>,
    osc_points: Vec<(f32, f32)>,
    gonio_points: Vec<(f32, f32)>,
    rms_db: f32,

    correlation: f32,
    corr_ready: bool,

    vu_l_db: f32,
    vu_r_db: f32,
    peak_l_db: f32,
    peak_r_db: f32,
    peak_l_lin: f32,
    peak_r_lin: f32,

    kfilter: [[Biquad; 2]; 2],
    lu_mom: [SlidingMeanSquare; 2],
    lu_st: [SlidingMeanSquare; 2],
    block_acc: [f32; 2],
    block_n: usize,
    block_len: usize,
    blocks: Vec<f32>,
    integrated_lufs: f32,
    true_peak_lin: f32,
}

impl DspAnalyzer {
    pub fn new(sample_rate: u32) -> Self {
        let rate = sample_rate.max(8000) as f32;
        let mut window = [0.0f32; FFT_SIZE];
        for (i, w) in window.iter_mut().enumerate() {
            *w = 0.5 - 0.5 * (2.0 * PI * i as f32 / (FFT_SIZE as f32 - 1.0)).cos();
        }

        let mut band_lo = [0usize; BANDS];
        let mut band_hi = [1usize; BANDS];
        let ratio = F_MAX / F_MIN;
        for b in 0..BANDS {
            let f0 = F_MIN * ratio.powf(b as f32 / BANDS as f32);
            let f1 = F_MIN * ratio.powf((b + 1) as f32 / BANDS as f32);
            let k0 = ((f0 * FFT_SIZE as f32 / rate).floor() as usize).max(1);
            let k1 = ((f1 * FFT_SIZE as f32 / rate).ceil() as usize)
                .max(k0 + 1)
                .min(FFT_SIZE / 2);
            band_lo[b] = k0.min(FFT_SIZE / 2);
            band_hi[b] = k1;
        }

        // BS.1770-4 K-weighting coefficients (defined for 48 kHz).
        let stage1 = Biquad::new(
            [1.53512485958697, -2.69169618940638, 1.19839281085285],
            [1.0, -1.69065929318241, 0.73248077421585],
        );
        let stage2 = Biquad::new(
            [1.0, -2.0, 1.0],
            [1.0, -1.99004745483398, 0.99007225036621],
        );

        let mom_cap = (rate * 0.4) as usize;
        let st_cap = (rate * 3.0) as usize;

        Self {
            rate,
            window,
            fft_re: [0.0; FFT_SIZE],
            fft_im: [0.0; FFT_SIZE],
            input: Vec::with_capacity(FFT_SIZE * 4),
            band_lo,
            band_hi,
            spectrum_db: [DB_FLOOR; BANDS],
            peak_hold_db: [DB_FLOOR; BANDS],
            peak_hz: 0.0,
            peak_db: DB_FLOOR,
            recent: VecDeque::with_capacity(8192),
            osc_points: vec![(0.0, 0.5); OSC_POINTS],
            gonio_points: vec![(0.5, 0.5); GONIO_POINTS],
            rms_db: DB_FLOOR,
            correlation: 0.0,
            corr_ready: false,
            vu_l_db: DB_FLOOR,
            vu_r_db: DB_FLOOR,
            peak_l_db: DB_FLOOR,
            peak_r_db: DB_FLOOR,
            peak_l_lin: 0.0,
            peak_r_lin: 0.0,
            kfilter: [[stage1, stage2], [stage1, stage2]],
            lu_mom: [SlidingMeanSquare::new(mom_cap), SlidingMeanSquare::new(mom_cap)],
            lu_st: [SlidingMeanSquare::new(st_cap), SlidingMeanSquare::new(st_cap)],
            block_acc: [0.0; 2],
            block_n: 0,
            block_len: (rate * 0.1) as usize,
            blocks: Vec::new(),
            integrated_lufs: DB_FLOOR,
            true_peak_lin: 0.0,
        }
    }

    /// Feed a chunk of stereo frames (oldest → newest). Cheap for one frame of
    /// audio (≤ 1 FFT) and completes in a single pass for offline analysis.
    pub fn process(&mut self, frames: &[(f32, f32)]) {
        if frames.is_empty() {
            return;
        }
        let mut sum_l2 = 0.0f32;
        let mut sum_r2 = 0.0f32;
        let mut num = 0.0f32;
        let mut dl = 0.0f32;
        let mut dr = 0.0f32;
        let mut call_peak_l = 0.0f32;
        let mut call_peak_r = 0.0f32;

        for &(l, r) in frames {
            self.input.push((l + r) * 0.5);
            if self.recent.len() == 8192 {
                self.recent.pop_front();
            }
            self.recent.push_back((l, r));

            sum_l2 += l * l;
            sum_r2 += r * r;
            num += l * r;
            dl += l * l;
            dr += r * r;
            call_peak_l = call_peak_l.max(l.abs());
            call_peak_r = call_peak_r.max(r.abs());

            // K-weighted loudness chain.
            let s1l = self.kfilter[0][0].process(l);
            let kl = self.kfilter[0][1].process(s1l);
            let s1r = self.kfilter[1][0].process(r);
            let kr = self.kfilter[1][1].process(s1r);
            self.lu_mom[0].push(kl);
            self.lu_mom[1].push(kr);
            self.lu_st[0].push(kl);
            self.lu_st[1].push(kr);
            self.block_acc[0] += kl * kl;
            self.block_acc[1] += kr * kr;
            self.block_n += 1;
            if self.block_n >= self.block_len {
                let zl = self.block_acc[0] / self.block_n as f32;
                let zr = self.block_acc[1] / self.block_n as f32;
                if self.blocks.len() < 36_000 {
                    self.blocks.push(zl + zr);
                }
                self.block_acc = [0.0; 2];
                self.block_n = 0;
            }

            true_peak_update(&mut self.true_peak_lin, l);
            true_peak_update(&mut self.true_peak_lin, r);
        }

        let n = frames.len() as f32;
        let rms = ((sum_l2 + sum_r2) / (2.0 * n)).sqrt();
        self.rms_db = to_db(rms).clamp(DB_FLOOR, 0.0);

        // Correlation with a smoothing filter, but snapped on the first sample so a
        // single-shot (offline) analysis reads the true instantaneous value.
        if dl > EPS && dr > EPS {
            let inst = (num / (dl.sqrt() * dr.sqrt())).clamp(-1.0, 1.0);
            if self.corr_ready {
                self.correlation += (inst - self.correlation) * 0.2;
            } else {
                self.correlation = inst;
                self.corr_ready = true;
            }
        }

        // Meter ballistics: peak decays slowly, RMS follows the window.
        self.peak_l_lin = (self.peak_l_lin * 0.86).max(call_peak_l);
        self.peak_r_lin = (self.peak_r_lin * 0.86).max(call_peak_r);
        self.peak_l_db = to_db(self.peak_l_lin).clamp(DB_FLOOR, 0.0);
        self.peak_r_db = to_db(self.peak_r_lin).clamp(DB_FLOOR, 0.0);
        if sum_l2 > 0.0 {
            self.vu_l_db = to_db((sum_l2 / n).sqrt()).clamp(DB_FLOOR, 0.0);
        }
        if sum_r2 > 0.0 {
            self.vu_r_db = to_db((sum_r2 / n).sqrt()).clamp(DB_FLOOR, 0.0);
        }

        // FFT: consume the accumulation buffer, one transform per hop.
        while self.input.len() >= FFT_SIZE {
            self.run_fft();
            self.input.drain(0..HOP.min(self.input.len()));
        }

        self.update_waveforms();
        self.update_integrated();
    }

    fn run_fft(&mut self) {
        for i in 0..FFT_SIZE {
            self.fft_re[i] = self.input[i] * self.window[i];
            self.fft_im[i] = 0.0;
        }
        fft_in_place(&mut self.fft_re, &mut self.fft_im);

        let half = FFT_SIZE / 2;
        let norm = 1.0 / (FFT_SIZE as f32 * 0.25);
        let mag_at = |re: &[f32], im: &[f32], k: usize| -> f32 {
            (re[k] * re[k] + im[k] * im[k]).sqrt()
        };

        // Dominant peak (parabolic interpolation on the log-magnitude of the bin).
        let k_min = ((F_MIN * FFT_SIZE as f32 / self.rate).ceil() as usize).max(1);
        let k_max = ((F_MAX * FFT_SIZE as f32 / self.rate).floor() as usize).min(half - 1);
        let mut best_k = k_min;
        let mut best_mag = 0.0f32;
        for k in k_min..=k_max {
            let m = mag_at(&self.fft_re, &self.fft_im, k);
            if m > best_mag {
                best_mag = m;
                best_k = k;
            }
        }
        let mut peak_bin = best_k as f32;
        if best_k > k_min && best_k < k_max {
            let a = to_db(mag_at(&self.fft_re, &self.fft_im, best_k - 1) * norm);
            let b = to_db(mag_at(&self.fft_re, &self.fft_im, best_k) * norm);
            let c = to_db(mag_at(&self.fft_re, &self.fft_im, best_k + 1) * norm);
            let denom = a - 2.0 * b + c;
            if denom.abs() > EPS {
                peak_bin += 0.5 * (a - c) / denom;
            }
        }
        self.peak_hz = (peak_bin * self.rate / FFT_SIZE as f32).clamp(0.0, self.rate * 0.5);
        self.peak_db = to_db(best_mag * norm).clamp(DB_FLOOR, 0.0);

        // 64 logarithmic bands, instant attack / exponential release.
        for b in 0..BANDS {
            let lo = self.band_lo[b];
            let hi = self.band_hi[b].max(lo + 1).min(half);
            let mut band_mag = 0.0f32;
            for k in lo..hi {
                let m = mag_at(&self.fft_re, &self.fft_im, k);
                if m > band_mag {
                    band_mag = m;
                }
            }
            let db = to_db(band_mag * norm).clamp(DB_FLOOR, 0.0);
            let prev = self.spectrum_db[b];
            self.spectrum_db[b] = if db >= prev {
                db
            } else {
                prev * 0.85 + db * 0.15
            };
            self.peak_hold_db[b] = self.spectrum_db[b]
                .max(self.peak_hold_db[b] - 0.4)
                .max(DB_FLOOR);
        }
    }

    fn update_waveforms(&mut self) {
        let n = self.recent.len();
        if n == 0 {
            return;
        }
        // Oscilloscope: anchor on the most recent upward zero crossing so the trace
        // stands still instead of scrolling.
        let mono: Vec<f32> = self.recent.iter().map(|&(l, _)| l).collect();
        let mut anchor = n.saturating_sub(OSC_POINTS);
        let scan_from = n.saturating_sub(OSC_POINTS * 2).max(1);
        for i in scan_from..n {
            if mono[i - 1] <= 0.0 && mono[i] > 0.0 {
                anchor = i;
            }
        }
        anchor = anchor.min(n - 1);
        let denom = (OSC_POINTS - 1) as f32;
        for (i, p) in self.osc_points.iter_mut().enumerate() {
            let idx = (anchor + i).min(n - 1);
            let s = mono[idx].clamp(-1.0, 1.0);
            *p = (i as f32 / denom, 0.5 + 0.5 * s);
        }

        // Goniometer: mid/side Lissajous, scaled by −1..1 and mapped to 0..1.
        let take = GONIO_POINTS.min(n);
        let start = n - take;
        for (i, p) in self.gonio_points.iter_mut().enumerate() {
            let j = start + (i * take / GONIO_POINTS).min(take.saturating_sub(1));
            let (l, r) = self.recent[j];
            let mid = (l + r) * 0.7071;
            let side = (l - r) * 0.7071;
            *p = (
                (0.5 + side * 0.5).clamp(0.0, 1.0),
                (0.5 + mid * 0.5).clamp(0.0, 1.0),
            );
        }
    }

    fn update_integrated(&mut self) {
        if self.lu_mom[0].count == 0 {
            return;
        }
        // Gate the integrated measurement per BS.1770-4: −70 LUFS absolute, then
        // −10 LU relative to the absolute-gated mean.
        if self.blocks.is_empty() {
            self.integrated_lufs = DB_FLOOR;
            return;
        }
        let z_to_lufs = |z: f32| LUFS_OFFSET + 10.0 * (z + EPS).log10();
        let abs_gated: Vec<f32> = self
            .blocks
            .iter()
            .copied()
            .filter(|&z| z_to_lufs(z) > -70.0)
            .collect();
        if abs_gated.is_empty() {
            self.integrated_lufs = DB_FLOOR;
            return;
        }
        let mean_abs = abs_gated.iter().sum::<f32>() / abs_gated.len() as f32;
        let rel_thresh = z_to_lufs(mean_abs) - 10.0;
        let gated: Vec<f32> = abs_gated
            .iter()
            .copied()
            .filter(|&z| z_to_lufs(z) > rel_thresh)
            .collect();
        if gated.is_empty() {
            self.integrated_lufs = DB_FLOOR;
            return;
        }
        let mean_gated = gated.iter().sum::<f32>() / gated.len() as f32;
        self.integrated_lufs = z_to_lufs(mean_gated).clamp(DB_FLOOR, 6.0);
    }

    /// Clear all loudness integration (momentary / short-term / integrated / true peak).
    pub fn reset_lufs(&mut self) {
        for ch in 0..2 {
            self.kfilter[ch][0].reset();
            self.kfilter[ch][1].reset();
            self.lu_mom[ch].reset();
            self.lu_st[ch].reset();
        }
        self.block_acc = [0.0; 2];
        self.block_n = 0;
        self.blocks.clear();
        self.integrated_lufs = DB_FLOOR;
        self.true_peak_lin = 0.0;
    }

    pub fn snapshot(&self) -> DspSnapshot {
        let z_to_lufs = |z: f32| (LUFS_OFFSET + 10.0 * (z + EPS).log10()).clamp(DB_FLOOR, 6.0);
        let mom = z_to_lufs(self.lu_mom[0].mean() + self.lu_mom[1].mean());
        let st = z_to_lufs(self.lu_st[0].mean() + self.lu_st[1].mean());
        DspSnapshot {
            spectrum_db: self.spectrum_db,
            peak_hold_db: self.peak_hold_db,
            peak_hz: self.peak_hz,
            peak_db: self.peak_db,
            osc_points: self.osc_points.clone(),
            rms_db: self.rms_db,
            gonio_points: self.gonio_points.clone(),
            correlation: self.correlation,
            momentary_lufs: mom,
            short_term_lufs: st,
            integrated_lufs: self.integrated_lufs,
            true_peak_dbtp: to_db(self.true_peak_lin).clamp(DB_FLOOR, 6.0),
            vu_l_db: self.vu_l_db,
            vu_r_db: self.vu_r_db,
            peak_l_db: self.peak_l_db,
            peak_r_db: self.peak_r_db,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fft_resolves_a_1khz_sine() {
        let rate = 48_000u32;
        let freq = 1000.0f32;
        let amp = 0.5f32;
        let mut analyzer = DspAnalyzer::new(rate);
        let frames: Vec<(f32, f32)> = (0..rate)
            .map(|i| {
                let s = amp * (2.0 * PI * freq * i as f32 / rate as f32).sin();
                (s, s)
            })
            .collect();
        analyzer.process(&frames);
        let snap = analyzer.snapshot();

        assert!(
            (snap.peak_hz - 1000.0).abs() < 60.0,
            "peak_hz {} not near 1000 Hz",
            snap.peak_hz
        );
        assert!(
            snap.peak_db > -12.0 && snap.peak_db < 0.0,
            "peak_db {} out of range",
            snap.peak_db
        );
        assert!(
            snap.correlation > 0.95,
            "correlation {} not > 0.95",
            snap.correlation
        );
        // A sine of amplitude 0.5 has RMS = 0.3536 → −9.03 dBFS.
        assert!(
            (snap.rms_db + 9.03).abs() < 0.6,
            "rms_db {} not near −9.03 dB",
            snap.rms_db
        );
        assert!(
            snap.momentary_lufs.is_finite()
                && snap.momentary_lufs > -40.0
                && snap.momentary_lufs < 0.0,
            "momentary_lufs {} out of range",
            snap.momentary_lufs
        );
        assert!(snap.short_term_lufs.is_finite());
        assert_eq!(snap.osc_points.len(), OSC_POINTS);
        assert_eq!(snap.gonio_points.len(), GONIO_POINTS);
    }

    #[test]
    fn reset_lufs_clears_the_integrator() {
        let mut analyzer = DspAnalyzer::new(48_000);
        let frames: Vec<(f32, f32)> = (0..24_000)
            .map(|i| {
                let s = 0.4 * (2.0 * PI * 500.0 * i as f32 / 48_000.0).sin();
                (s, -s)
            })
            .collect();
        analyzer.process(&frames);
        analyzer.reset_lufs();
        assert!(analyzer.snapshot().integrated_lufs <= DB_FLOOR + 0.001);
    }
}
