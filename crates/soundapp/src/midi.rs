//! Phase 2.4 — MIDI / SoundFont synthesis engine.
//!
//! Standard MIDI files (`.mid` / `.midi`) and a built-in General MIDI demo
//! sequence are synthesised through `rustysynth` — a pure-Rust, zero-dependency
//! SF2 SoundFont engine. The engine exposes the SoundApp parameter set:
//! `transpose`, `bpm_override`, `metronome` and live SoundFont switching.
//!
//! SoundFont loading prefers `D:\Work\_GIT\SoundApp\bin\soundfonts\default.sf2`
//! and falls back through alternate locations. If no SoundFont can be loaded the
//! engine switches to an internal additive synth that renders the same note
//! events — so MIDI still plays when the SoundFont path is moved.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustysynth::{MidiFile, MidiFileSequencer, SoundFont, Synthesizer, SynthesizerSettings};

/// The reference General-MIDI SoundFont shipped with SoundApp.
pub const DEFAULT_SOUNDFONT: &str = r"D:\Work\_GIT\SoundApp\bin\soundfonts\default.sf2";

/// Native tempo of the built-in demo sequence (BPM). `bpm_override` and the
/// playback-speed factor are expressed relative to this.
pub const DEMO_BPM: f32 = 120.0;

const METRONOME_HIGH_WAV: &str = r"D:\Work\_GIT\SoundApp\bin\metronome\metronome-high.wav";
const METRONOME_LOW_WAV: &str = r"D:\Work\_GIT\SoundApp\bin\metronome\metronome-low.wav";

// ---------------------------------------------------------------------------
// Public engine
// ---------------------------------------------------------------------------

/// Where the currently loaded sequence comes from.
#[derive(Clone, Debug)]
pub enum MidiSource {
    File(PathBuf),
    Demo,
}

enum Backend {
    Sf2 {
        sequencer: MidiFileSequencer,
        /// Kept alive alongside the sequencer's own `Arc<MidiFile>` borrow.
        _file: Arc<MidiFile>,
    },
    Simple(Box<SimpleSynth>),
    None,
}

/// A real-time SoundFont / MIDI synthesis engine.
pub struct MidiEngine {
    rate: u32,
    soundfont_path: Option<PathBuf>,
    soundfont: Option<Arc<SoundFont>>,
    backend: Backend,
    source: Option<MidiSource>,
    /// Current sequence bytes (already transposed) — the backend is rebuilt from
    /// these whenever a parameter that changes the notes is altered.
    smf: Vec<u8>,
    transpose: i32,
    bpm_override: Option<f32>,
    metronome: bool,
    click_high: Vec<f32>,
    click_low: Vec<f32>,
    click_pos: usize,
    click_active: bool,
    click_is_high: bool,
    last_beat: i64,
    duration: f64,
}

impl MidiEngine {
    /// Build the engine at `sample_rate`, loading the default SoundFont.
    pub fn new(sample_rate: u32) -> Result<Self, String> {
        let (soundfont, path) = load_default_soundfont();
        let rate = sample_rate;
        Ok(Self {
            rate,
            soundfont_path: path,
            soundfont,
            backend: Backend::None,
            source: None,
            smf: Vec::new(),
            transpose: 0,
            bpm_override: None,
            metronome: false,
            click_high: load_click(METRONOME_HIGH_WAV, 1800.0, rate as f32),
            click_low: load_click(METRONOME_LOW_WAV, 900.0, rate as f32),
            click_pos: 0,
            click_active: false,
            click_is_high: true,
            last_beat: i64::MIN,
            duration: 0.0,
        })
    }

    /// Path of the SoundFont currently in use (`None` when the fallback synth is
    /// active).
    pub fn soundfont_path(&self) -> Option<&Path> {
        self.soundfont_path.as_deref()
    }

    /// True when a SoundFont is loaded and the SF2 backend will be used (as
    /// opposed to the fallback synth).
    pub fn using_soundfont(&self) -> bool {
        self.soundfont.is_some()
    }

    /// Load a Standard MIDI file from disk.
    pub fn load_file(&mut self, path: &Path) -> Result<(), String> {
        self.source = Some(MidiSource::File(path.to_path_buf()));
        self.rebuild()
    }

    /// Load the built-in multi-channel General MIDI demo sequence.
    pub fn load_demo(&mut self) {
        self.source = Some(MidiSource::Demo);
        let _ = self.rebuild();
    }

    /// Transpose every melodic channel by `semitones` (`-12..=12`).
    pub fn set_transpose(&mut self, semitones: i32) {
        let t = semitones.clamp(-12, 12);
        if t == self.transpose {
            return;
        }
        self.transpose = t;
        if self.source.is_some() {
            let _ = self.rebuild();
        }
    }

    /// Override the sequence tempo (`40..=240` BPM), or `None` for the file's
    /// native tempo.
    pub fn set_bpm(&mut self, bpm: Option<f32>) {
        self.bpm_override = bpm.map(|b| b.clamp(40.0, 240.0));
        self.apply_speed();
    }

    /// Enable or disable the metronome click track (high click on beat 1 of each
    /// bar, low click on the remaining beats).
    pub fn set_metronome(&mut self, on: bool) {
        self.metronome = on;
    }

    /// Switch to a different SoundFont at runtime.
    pub fn set_soundfont(&mut self, path: &Path) -> Result<(), String> {
        let sf = load_soundfont(path)?;
        self.soundfont = Some(sf);
        self.soundfont_path = Some(path.to_path_buf());
        if self.source.is_some() {
            self.rebuild()?;
        }
        Ok(())
    }

    pub fn duration(&self) -> f64 {
        self.duration
    }

    pub fn position(&self) -> f64 {
        match &self.backend {
            Backend::Sf2 { sequencer, .. } => sequencer.get_position(),
            Backend::Simple(s) => s.position(),
            Backend::None => 0.0,
        }
    }

    /// Render `left.len()` stereo frames. Both slices must be the same length.
    pub fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        let start = self.position();
        match &mut self.backend {
            Backend::Sf2 { sequencer, .. } => sequencer.render(left, right),
            Backend::Simple(s) => s.render(left, right),
            Backend::None => {
                for v in left.iter_mut() {
                    *v = 0.0;
                }
                for v in right.iter_mut() {
                    *v = 0.0;
                }
            }
        }
        if self.metronome {
            self.mix_metronome(start, left, right);
        }
    }

    // -- internals ---------------------------------------------------------

    fn speed_factor(&self) -> f64 {
        let bpm = self.bpm_override.unwrap_or(DEMO_BPM);
        (bpm / DEMO_BPM) as f64
    }

    fn apply_speed(&mut self) {
        let s = self.speed_factor();
        match &mut self.backend {
            Backend::Sf2 { sequencer, .. } => sequencer.set_speed(s),
            Backend::Simple(syn) => syn.set_speed(s),
            Backend::None => {}
        }
    }

    fn rebuild(&mut self) -> Result<(), String> {
        let source = self
            .source
            .clone()
            .ok_or_else(|| "no MIDI source loaded".to_string())?;

        // Regenerate the (transposed) SMF bytes for the current source.
        let bytes = match source {
            MidiSource::Demo => MidiDemoSequence::to_smf_bytes(self.transpose),
            MidiSource::File(p) => {
                let raw = std::fs::read(&p).map_err(|e| format!("read {}: {e}", p.display()))?;
                transpose_smf(&raw, self.transpose)
            }
        };
        self.smf = bytes;

        self.backend = Backend::None;
        if self.smf.is_empty() {
            self.duration = 0.0;
            return Ok(());
        }

        match self.soundfont.clone() {
            Some(sf) => {
                let settings = SynthesizerSettings::new(self.rate as i32);
                let synth = Synthesizer::new(&sf, &settings)
                    .map_err(|e| format!("synthesizer init: {e:?}"))?;
                let mut sequencer = MidiFileSequencer::new(synth);
                let mut cur = Cursor::new(self.smf.clone());
                let mf = MidiFile::new(&mut cur).map_err(|e| format!("midi parse: {e:?}"))?;
                self.duration = mf.get_length();
                let file = Arc::new(mf);
                sequencer.play(&file, true);
                sequencer.set_speed(self.speed_factor());
                self.backend = Backend::Sf2 {
                    sequencer,
                    _file: file,
                };
            }
            None => {
                // No SoundFont available: fall back to the internal synth.
                let (events, dur) = parse_smf(&self.smf);
                self.duration = dur;
                let mut synth = SimpleSynth::new(self.rate as f32, events);
                synth.set_speed(self.speed_factor());
                self.backend = Backend::Simple(Box::new(synth));
            }
        }
        Ok(())
    }

    fn mix_metronome(&mut self, start: f64, left: &mut [f32], right: &mut [f32]) {
        let n = left.len().min(right.len());
        let bpm = self.bpm_override.unwrap_or(DEMO_BPM).max(20.0);
        let sec_per_beat = 60.0 / bpm as f64;
        let rate = self.rate as f64;
        for i in 0..n {
            let pos = start + i as f64 / rate;
            let beat = (pos / sec_per_beat).floor() as i64;
            if beat != self.last_beat {
                self.last_beat = beat;
                self.click_pos = 0;
                self.click_active = true;
                self.click_is_high = beat.rem_euclid(4) == 0;
            }
            if !self.click_active {
                continue;
            }
            let src = if self.click_is_high {
                &self.click_high
            } else {
                &self.click_low
            };
            if self.click_pos < src.len() {
                let v = src[self.click_pos];
                left[i] = (left[i] + v).clamp(-1.0, 1.0);
                right[i] = (right[i] + v).clamp(-1.0, 1.0);
            }
            self.click_pos += 1;
            if self.click_pos >= src.len() {
                self.click_active = false;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// SoundFont loading
// ---------------------------------------------------------------------------

fn soundfont_candidates() -> Vec<PathBuf> {
    vec![
        PathBuf::from(DEFAULT_SOUNDFONT),
        PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/default.sf2")),
        PathBuf::from(r"D:\Work\_GIT\SoundApp\bin\soundfonts\TimGM6mb.sf2"),
    ]
}

fn load_soundfont(path: &Path) -> Result<Arc<SoundFont>, String> {
    let data = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let mut cur = Cursor::new(data);
    let sf = SoundFont::new(&mut cur).map_err(|e| format!("parse {}: {e:?}", path.display()))?;
    Ok(Arc::new(sf))
}

fn load_default_soundfont() -> (Option<Arc<SoundFont>>, Option<PathBuf>) {
    let candidates = soundfont_candidates();
    for cand in &candidates {
        if !cand.exists() {
            continue;
        }
        match load_soundfont(cand) {
            Ok(sf) => {
                println!("[midi] loaded SoundFont {}", cand.display());
                return (Some(sf), Some(cand.clone()));
            }
            Err(e) => eprintln!("[midi] {e}"),
        }
    }
    eprintln!(
        "[midi] no SoundFont found (looked at {} locations) — using internal synth",
        candidates.len()
    );
    (None, None)
}

// ---------------------------------------------------------------------------
// Metronome click synthesis / WAV loading
// ---------------------------------------------------------------------------

fn load_click(path: &str, freq: f32, rate: f32) -> Vec<f32> {
    if let Ok(data) = std::fs::read(path) {
        if let Some((samples, src_rate)) = parse_wav_mono(&data) {
            return resample_mono(&samples, src_rate, rate);
        }
    }
    synth_click(freq, rate)
}

fn synth_click(freq: f32, rate: f32) -> Vec<f32> {
    let dur = 0.05f32;
    let n = (dur * rate) as usize;
    let mut v = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f32 / rate;
        let env = (-t * 45.0).exp();
        let s = (2.0 * std::f32::consts::PI * freq * t).sin();
        v.push((s * env * 0.6).clamp(-1.0, 1.0));
    }
    v
}

/// Minimal RIFF/WAVE reader → (mono samples, source rate). Supports 16-bit PCM,
/// 8-bit PCM and 32-bit float.
fn parse_wav_mono(data: &[u8]) -> Option<(Vec<f32>, u32)> {
    if data.len() < 12 || &data[0..4] != b"RIFF" || &data[8..12] != b"WAVE" {
        return None;
    }
    let mut pos = 12usize;
    let mut fmt: Option<(u16, u16, u32, u16)> = None;
    let mut data_slice: Option<&[u8]> = None;
    while pos + 8 <= data.len() {
        let id = &data[pos..pos + 4];
        let size =
            u32::from_le_bytes([data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7]]) as usize;
        let body = pos + 8;
        if id == b"fmt " && body + 16 <= data.len() {
            let tag = u16::from_le_bytes([data[body], data[body + 1]]);
            let ch = u16::from_le_bytes([data[body + 2], data[body + 3]]);
            let rate =
                u32::from_le_bytes([data[body + 4], data[body + 5], data[body + 6], data[body + 7]]);
            let bits = u16::from_le_bytes([data[body + 14], data[body + 15]]);
            fmt = Some((tag, ch, rate, bits));
        } else if id == b"data" {
            let end = (body + size).min(data.len());
            data_slice = Some(&data[body..end]);
        }
        pos = body + size + (size & 1);
    }
    let (tag, ch, rate, bits) = fmt?;
    let ds = data_slice?;
    let ch = ch.max(1) as usize;
    let mut mono = Vec::new();
    match (tag, bits) {
        (3, 32) => {
            let frames = ds.len() / (4 * ch);
            for f in 0..frames {
                let mut acc = 0.0f32;
                for c in 0..ch {
                    let o = (f * ch + c) * 4;
                    acc += f32::from_le_bytes([ds[o], ds[o + 1], ds[o + 2], ds[o + 3]]);
                }
                mono.push(acc / ch as f32);
            }
        }
        (1, 16) => {
            let frames = ds.len() / (2 * ch);
            for f in 0..frames {
                let mut acc = 0.0f32;
                for c in 0..ch {
                    let o = (f * ch + c) * 2;
                    let s = i16::from_le_bytes([ds[o], ds[o + 1]]) as f32;
                    acc += s / 32768.0;
                }
                mono.push(acc / ch as f32);
            }
        }
        (1, 8) => {
            let frames = ds.len() / ch;
            for f in 0..frames {
                let mut acc = 0.0f32;
                for c in 0..ch {
                    let s = ds[f * ch + c] as f32;
                    acc += (s - 128.0) / 128.0;
                }
                mono.push(acc / ch as f32);
            }
        }
        _ => return None,
    }
    Some((mono, rate))
}

fn resample_mono(src: &[f32], src_rate: u32, dst_rate: f32) -> Vec<f32> {
    if src.is_empty() {
        return Vec::new();
    }
    if (src_rate as f32 - dst_rate).abs() < 1.0 {
        return src.to_vec();
    }
    let ratio = src_rate as f64 / dst_rate as f64;
    let out_len = ((src.len() as f64) / ratio) as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let pos = i as f64 * ratio;
        let i0 = pos.floor() as usize;
        let frac = (pos - i0 as f64) as f32;
        let a = *src.get(i0).unwrap_or(&0.0);
        let b = *src.get(i0 + 1).unwrap_or(&a);
        out.push(a + (b - a) * frac);
    }
    out
}

// ---------------------------------------------------------------------------
// Standard MIDI File — note transposition
// ---------------------------------------------------------------------------

fn be_u16(d: &[u8], o: usize) -> u16 {
    u16::from_be_bytes([d[o], d[o + 1]])
}
fn be_u32(d: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]])
}

/// Shift every melodic (non-percussion) note in an SMF by `semitones`. Note
/// events keep their byte width, so the rewrite is applied in place.
fn transpose_smf(data: &[u8], semitones: i32) -> Vec<u8> {
    let mut out = data.to_vec();
    if semitones == 0 || data.len() < 14 || &data[0..4] != b"MThd" {
        return out;
    }
    let hlen = be_u32(data, 4) as usize;
    let ntrks = be_u16(data, 10) as usize;
    let mut pos = 8 + hlen;
    for _ in 0..ntrks {
        if pos + 8 > data.len() || &data[pos..pos + 4] != b"MTrk" {
            break;
        }
        let tlen = be_u32(data, pos + 4) as usize;
        let start = pos + 8;
        let end = (start + tlen).min(data.len());
        transpose_track(data, start, end, semitones, &mut out);
        pos = start + tlen;
    }
    out
}

fn skip_vlq(d: &[u8], mut i: usize, end: usize) -> usize {
    while i < end && d[i] & 0x80 != 0 {
        i += 1;
    }
    if i < end {
        i + 1
    } else {
        i
    }
}

fn read_vlq(d: &[u8], mut i: usize, end: usize) -> (u32, usize) {
    let mut v = 0u32;
    while i < end {
        let b = d[i];
        i += 1;
        v = (v << 7) | (b & 0x7F) as u32;
        if b & 0x80 == 0 {
            break;
        }
    }
    (v, i)
}

fn transpose_track(data: &[u8], start: usize, end: usize, semitones: i32, out: &mut [u8]) {
    let mut i = start;
    let mut running = 0u8;
    while i < end {
        i = skip_vlq(data, i, end);
        if i >= end {
            break;
        }
        let mut status = data[i];
        if status & 0x80 != 0 {
            i += 1;
            running = status;
        } else {
            status = running;
        }
        match status {
            0xFF => {
                if i >= end {
                    break;
                }
                let mtype = data[i];
                i += 1;
                let (nlen, ni) = read_vlq(data, i, end);
                i = (ni + nlen as usize).min(end);
                if mtype == 0x2F {
                    break;
                }
            }
            0xF0 | 0xF7 => {
                let (nlen, ni) = read_vlq(data, i, end);
                i = (ni + nlen as usize).min(end);
            }
            _ => {
                let hi = status & 0xF0;
                let ch = status & 0x0F;
                let dlen = if hi == 0xC0 || hi == 0xD0 { 1 } else { 2 };
                if i + dlen > end {
                    break;
                }
                if (hi == 0x80 || hi == 0x90) && ch != 9 {
                    let key = (data[i] as i32 + semitones).clamp(0, 127) as u8;
                    out[i] = key;
                }
                i += dlen;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Built-in General MIDI demo sequence
// ---------------------------------------------------------------------------

/// A rich multi-channel General MIDI sequence (piano chords, bassline, square
/// lead, string pad and drums) generated as a Standard MIDI File.
pub struct MidiDemoSequence;

impl MidiDemoSequence {
    /// Serialise the demo sequence as a format-0 Standard MIDI File, with the
    /// melodic channels transposed by `transpose` semitones.
    pub fn to_smf_bytes(transpose: i32) -> Vec<u8> {
        let ppq = 480u32;
        let mut b = SmfBuilder::new(ppq, transpose);
        b.tempo(0, 120);
        b.program(0, 0, 0); // Acoustic Grand Piano
        b.program(0, 1, 33); // Finger Bass
        b.program(0, 2, 80); // Square Lead
        b.program(0, 3, 48); // Strings

        // I–vi–IV–V progression.
        let chords: [([i32; 3], i32); 4] = [
            ([60, 64, 67], 48), // C
            ([57, 60, 64], 45), // Am
            ([53, 57, 60], 41), // F
            ([55, 59, 62], 43), // G
        ];
        let lead: [i32; 16] = [
            72, 76, 79, 76, 74, 77, 81, 77, 72, 76, 79, 83, 74, 71, 67, 74,
        ];
        let bars = 8u32;
        for bar in 0..bars {
            let base = bar * ppq * 4;
            let (chord, bass_root) = chords[(bar % 4) as usize];

            // Piano: a chord on beat 1 and beat 3.
            for &beat in &[0u32, 2] {
                let t = base + beat * ppq;
                for &n in &chord {
                    b.note(t, 0, n, 72, 2 * ppq - 12);
                }
            }

            // String pad: sustained chord an octave down.
            for &n in &chord {
                b.note(base, 3, n - 12, 48, 4 * ppq - 24);
            }

            // Bass: eighth notes on the root, an octave down.
            for i in 0..8u32 {
                let t = base + i * (ppq / 2);
                b.note(t, 1, bass_root - 12, 96, ppq / 2 - 20);
            }

            // Lead: a quarter-note melody line.
            for i in 0..4u32 {
                let t = base + i * ppq;
                let n = lead[((bar * 4 + i) % 16) as usize];
                b.note(t, 2, n, 88, ppq - 30);
            }

            // Drums (channel 9): kick, snare and closed hats.
            for i in 0..4u32 {
                let t = base + i * ppq;
                if i % 2 == 0 {
                    b.note(t, 9, 36, 110, ppq / 4); // kick
                } else {
                    b.note(t, 9, 38, 100, ppq / 4); // snare
                }
            }
            for i in 0..8u32 {
                let t = base + i * (ppq / 2);
                b.note(t, 9, 42, 70, 60); // closed hi-hat
            }
        }
        b.to_bytes()
    }
}

struct SmfBuilder {
    ppq: u32,
    transpose: i32,
    ev: Vec<(u32, Vec<u8>)>,
}

impl SmfBuilder {
    fn new(ppq: u32, transpose: i32) -> Self {
        Self {
            ppq,
            transpose,
            ev: Vec::new(),
        }
    }

    fn key(&self, ch: u8, key: i32) -> u8 {
        if ch == 9 {
            key.clamp(0, 127) as u8
        } else {
            (key + self.transpose).clamp(0, 127) as u8
        }
    }

    fn tempo(&mut self, tick: u32, bpm: u32) {
        let us = 60_000_000 / bpm.max(1);
        self.ev.push((
            tick,
            vec![0xFF, 0x51, 0x03, (us >> 16) as u8, (us >> 8) as u8, us as u8],
        ));
    }

    fn program(&mut self, tick: u32, ch: u8, prog: u8) {
        self.ev.push((tick, vec![0xC0 | (ch & 0x0F), prog & 0x7F]));
    }

    fn note(&mut self, tick: u32, ch: u8, key: i32, vel: u8, dur: u32) {
        let k = self.key(ch, key);
        self.ev
            .push((tick, vec![0x90 | (ch & 0x0F), k, vel.min(127)]));
        self.ev
            .push((tick + dur.max(1), vec![0x80 | (ch & 0x0F), k, 0x40]));
    }

    fn to_bytes(mut self) -> Vec<u8> {
        // Stable sort keeps note-on/off ordering for identical ticks.
        self.ev.sort_by_key(|(t, _)| *t);
        let mut track = Vec::new();
        let mut last = 0u32;
        for (tick, bytes) in &self.ev {
            write_vlq(&mut track, tick - last);
            last = *tick;
            track.extend_from_slice(bytes);
        }
        track.extend_from_slice(&[0x00, 0xFF, 0x2F, 0x00]); // end of track

        let mut out = Vec::new();
        out.extend_from_slice(b"MThd");
        out.extend_from_slice(&6u32.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes()); // format 0
        out.extend_from_slice(&1u16.to_be_bytes()); // one track
        out.extend_from_slice(&(self.ppq as u16).to_be_bytes());
        out.extend_from_slice(b"MTrk");
        out.extend_from_slice(&(track.len() as u32).to_be_bytes());
        out.extend_from_slice(&track);
        out
    }
}

fn write_vlq(out: &mut Vec<u8>, mut v: u32) {
    let mut tmp = [0u8; 5];
    let mut n = 0;
    tmp[n] = (v & 0x7F) as u8;
    n += 1;
    v >>= 7;
    while v > 0 {
        tmp[n] = ((v & 0x7F) as u8) | 0x80;
        n += 1;
        v >>= 7;
    }
    for j in (0..n).rev() {
        out.push(tmp[j]);
    }
}

// ---------------------------------------------------------------------------
// Fallback: SMF event parsing + additive synth
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum Ev {
    NoteOn { ch: u8, key: u8, vel: u8 },
    NoteOff { ch: u8, key: u8 },
    Program { ch: u8, prog: u8 },
    Tempo(u32),
}

/// Parse an SMF into a time-sorted event list (seconds) plus the total duration.
fn parse_smf(data: &[u8]) -> (Vec<(f64, Ev)>, f64) {
    if data.len() < 14 || &data[0..4] != b"MThd" {
        return (Vec::new(), 0.0);
    }
    let hlen = be_u32(data, 4) as usize;
    let ntrks = be_u16(data, 10) as usize;
    let division = be_u16(data, 12).max(1) as f64;
    let mut ticked: Vec<(u32, Ev)> = Vec::new();
    let mut pos = 8 + hlen;
    for _ in 0..ntrks {
        if pos + 8 > data.len() || &data[pos..pos + 4] != b"MTrk" {
            break;
        }
        let tlen = be_u32(data, pos + 4) as usize;
        let end = (pos + 8 + tlen).min(data.len());
        parse_track(data, pos + 8, end, &mut ticked);
        pos = pos + 8 + tlen;
    }
    ticked.sort_by_key(|(t, _)| *t);

    let mut out: Vec<(f64, Ev)> = Vec::with_capacity(ticked.len());
    let mut time = 0.0f64;
    let mut last_tick = 0u32;
    let mut us_per_qn = 500_000f64;
    for (tick, ev) in &ticked {
        time += (*tick - last_tick) as f64 * us_per_qn / division / 1_000_000.0;
        last_tick = *tick;
        if let Ev::Tempo(us) = ev {
            us_per_qn = *us as f64;
        }
        out.push((time, *ev));
    }
    (out, time)
}

fn parse_track(data: &[u8], start: usize, end: usize, out: &mut Vec<(u32, Ev)>) {
    let mut i = start;
    let mut running = 0u8;
    let mut tick = 0u32;
    while i < end {
        let (dt, ni) = read_vlq(data, i, end);
        i = ni;
        tick = tick.wrapping_add(dt);
        if i >= end {
            break;
        }
        let mut status = data[i];
        if status & 0x80 != 0 {
            i += 1;
            running = status;
        } else {
            status = running;
        }
        match status {
            0xFF => {
                if i >= end {
                    break;
                }
                let mtype = data[i];
                i += 1;
                let (nlen, ni) = read_vlq(data, i, end);
                i = ni;
                if mtype == 0x51 && nlen == 3 && i + 3 <= end {
                    let us =
                        ((data[i] as u32) << 16) | ((data[i + 1] as u32) << 8) | data[i + 2] as u32;
                    out.push((tick, Ev::Tempo(us)));
                }
                i = (i + nlen as usize).min(end);
                if mtype == 0x2F {
                    break;
                }
            }
            0xF0 | 0xF7 => {
                let (nlen, ni) = read_vlq(data, i, end);
                i = (ni + nlen as usize).min(end);
            }
            _ => {
                let hi = status & 0xF0;
                let ch = status & 0x0F;
                let dlen = if hi == 0xC0 || hi == 0xD0 { 1 } else { 2 };
                if i + dlen > end {
                    break;
                }
                match hi {
                    0x90 => {
                        let key = data[i];
                        let vel = data[i + 1];
                        if vel == 0 {
                            out.push((tick, Ev::NoteOff { ch, key }));
                        } else {
                            out.push((tick, Ev::NoteOn { ch, key, vel }));
                        }
                    }
                    0x80 => out.push((tick, Ev::NoteOff { ch, key: data[i] })),
                    0xC0 => out.push((tick, Ev::Program { ch, prog: data[i] })),
                    _ => {}
                }
                i += dlen;
            }
        }
    }
}

struct Voice {
    key: u8,
    is_drum: bool,
    is_kick: bool,
    wave: u8,
    inc: f32,
    phase: f32,
    env: f32,
    stage: u8, // 0 attack, 1 decay, 2 sustain, 3 release
    vel: f32,
    noise: u32,
}

struct SimpleSynth {
    rate: f32,
    events: Vec<(f64, Ev)>,
    idx: usize,
    time: f64,
    speed: f64,
    programs: [u8; 16],
    voices: Vec<Voice>,
}

impl SimpleSynth {
    fn new(rate: f32, events: Vec<(f64, Ev)>) -> Self {
        Self {
            rate,
            events,
            idx: 0,
            time: 0.0,
            speed: 1.0,
            programs: [0; 16],
            voices: Vec::new(),
        }
    }

    fn set_speed(&mut self, s: f64) {
        self.speed = s.max(0.01);
    }

    fn position(&self) -> f64 {
        self.time
    }

    fn wave_for(&self, ch: u8) -> (u8, bool) {
        if ch == 9 {
            return (3, true);
        }
        let p = self.programs[ch as usize];
        let wave = match p {
            0..=7 => 0,   // piano → sine-ish
            32..=39 => 1, // bass → saw
            48..=55 => 4, // strings → soft sine
            80..=87 => 2, // lead → square
            _ => 0,
        };
        (wave, false)
    }

    fn note_on(&mut self, ch: u8, key: u8, vel: u8) {
        let (wave, is_drum) = self.wave_for(ch);
        let is_kick = is_drum && (key == 35 || key == 36);
        let inc = if is_drum {
            0.0
        } else {
            let freq = 440.0 * 2f32.powf((key as f32 - 69.0) / 12.0);
            freq / self.rate
        };
        if self.voices.len() >= 48 {
            self.voices.remove(0);
        }
        self.voices.push(Voice {
            key,
            is_drum,
            is_kick,
            wave,
            inc,
            phase: 0.0,
            env: 0.0,
            stage: 0,
            vel: vel as f32 / 127.0,
            noise: (key as u32)
                .wrapping_mul(2654435761)
                .wrapping_add(0x9E37_79B9)
                | 1,
        });
    }

    fn note_off(&mut self, _ch: u8, key: u8) {
        for v in &mut self.voices {
            if v.key == key && v.stage != 3 {
                v.stage = 3;
            }
        }
    }

    fn apply(&mut self, ev: Ev) {
        match ev {
            Ev::NoteOn { ch, key, vel } => self.note_on(ch, key, vel),
            Ev::NoteOff { ch, key } => self.note_off(ch, key),
            Ev::Program { ch, prog } => self.programs[ch as usize] = prog,
            Ev::Tempo(_) => {}
        }
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        let n = left.len().min(right.len());
        let atk = 1.0 / (0.005 * self.rate);
        let dec = 1.0 / (0.10 * self.rate);
        let rel = 1.0 / (0.15 * self.rate);
        let dt = (self.speed / self.rate as f64) as f32;

        for i in 0..n {
            while self.idx < self.events.len() && self.events[self.idx].0 <= self.time {
                let ev = self.events[self.idx].1;
                self.apply(ev);
                self.idx += 1;
            }
            let mut sl = 0.0f32;
            let mut sr = 0.0f32;
            for v in &mut self.voices {
                match v.stage {
                    0 => {
                        v.env += atk;
                        if v.env >= 1.0 {
                            v.env = 1.0;
                            v.stage = 1;
                        }
                    }
                    1 => {
                        v.env -= dec * 0.6;
                        if v.env <= 0.6 {
                            v.env = 0.6;
                            v.stage = 2;
                        }
                    }
                    2 => {}
                    _ => {
                        v.env -= rel;
                        if v.env < 0.0 {
                            v.env = 0.0;
                        }
                    }
                }
                if v.env <= 0.0 && v.stage == 3 {
                    continue;
                }
                v.noise ^= v.noise << 13;
                v.noise ^= v.noise >> 17;
                v.noise ^= v.noise << 5;
                let noise = (v.noise as f32 / u32::MAX as f32) * 2.0 - 1.0;

                let s = if v.is_kick {
                    // Kick: sine with a downward pitch sweep.
                    v.phase.sin()
                } else if v.is_drum {
                    noise
                } else {
                    let ph = v.phase;
                    match v.wave {
                        0 => ph.sin(),
                        1 => 2.0 * (ph / (2.0 * std::f32::consts::PI)).fract() - 1.0,
                        2 => {
                            if ph < std::f32::consts::PI {
                                1.0
                            } else {
                                -1.0
                            }
                        }
                        _ => (ph * 0.5).sin(),
                    }
                };

                sl += s * v.env * v.vel * 0.25;
                sr += s * v.env * v.vel * 0.25;

                if v.is_kick {
                    let f = (120.0 * (-v.phase * 0.5).exp()).max(45.0);
                    v.phase += 2.0 * std::f32::consts::PI * f / self.rate;
                } else {
                    v.phase += v.inc;
                }
                if v.phase > 2.0 * std::f32::consts::PI {
                    v.phase -= 2.0 * std::f32::consts::PI;
                }
            }
            left[i] = sl.clamp(-1.0, 1.0);
            right[i] = sr.clamp(-1.0, 1.0);
            self.time += dt as f64;

            if self.idx >= self.events.len() && self.voices.iter().all(|v| v.stage == 3) {
                // Loop the sequence.
                self.idx = 0;
                self.time = 0.0;
                self.voices.clear();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_gm_demo_through_soundfont() {
        let mut eng = MidiEngine::new(48_000).expect("midi engine");
        assert!(
            eng.using_soundfont(),
            "default.sf2 should have loaded from {DEFAULT_SOUNDFONT}"
        );
        eng.load_demo();
        let mut l = vec![0.0f32; 4800];
        let mut r = vec![0.0f32; 4800];
        eng.render(&mut l, &mut r);
        let energy: f32 = l.iter().chain(r.iter()).map(|v| v * v).sum();
        assert!(energy > 0.0, "expected non-zero SF2 output, got energy {energy}");
    }

    #[test]
    fn transpose_shifts_melodic_notes_only() {
        let bytes = MidiDemoSequence::to_smf_bytes(0);
        let shifted = transpose_smf(&bytes, 12);
        assert_eq!(bytes.len(), shifted.len());
        assert_ne!(bytes, shifted);

        let (a, _) = parse_smf(&bytes);
        let (b, _) = parse_smf(&shifted);
        let melodic = |evs: &[(f64, Ev)]| -> Vec<u8> {
            evs.iter()
                .filter_map(|(_, e)| match e {
                    Ev::NoteOn { ch, key, .. } if *ch != 9 => Some(*key),
                    _ => None,
                })
                .collect()
        };
        let drums = |evs: &[(f64, Ev)]| -> Vec<u8> {
            evs.iter()
                .filter_map(|(_, e)| match e {
                    Ev::NoteOn { ch, key, .. } if *ch == 9 => Some(*key),
                    _ => None,
                })
                .collect()
        };
        let ma = melodic(&a);
        let mb = melodic(&b);
        assert_eq!(ma.len(), mb.len());
        assert!(!ma.is_empty());
        for (x, y) in ma.iter().zip(mb.iter()) {
            assert_eq!((*x as i32 + 12).clamp(0, 127) as u8, *y);
        }
        assert_eq!(drums(&a), drums(&b), "percussion must not transpose");
    }

    #[test]
    fn fallback_synth_renders_without_soundfont() {
        let bytes = MidiDemoSequence::to_smf_bytes(0);
        let (events, dur) = parse_smf(&bytes);
        assert!(dur > 0.0);
        let mut synth = SimpleSynth::new(48_000.0, events);
        let mut l = vec![0.0f32; 4800];
        let mut r = vec![0.0f32; 4800];
        synth.render(&mut l, &mut r);
        let energy: f32 = l.iter().chain(r.iter()).map(|v| v * v).sum();
        assert!(energy > 0.0, "fallback synth produced silence");
    }
}
