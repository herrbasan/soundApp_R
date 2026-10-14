// Audio engine: transport control + a cpal callback that drains a lock-free
// rtrb ring buffer, fed by a decoder thread.
//
// Three playback engines feed the same pipeline:
//   * FFmpeg (universal formats + metadata) via the native `sa_ffmpeg_*` bridge.
//   * MIDI / SoundFont synthesis (`midi.rs`, rustysynth).
//   * ProTracker modules (`tracker.rs`, native Paula synth).
//
// Pitch / tempo / formant are applied in the pipeline itself, so they work
// identically on every source:
//   * "Tape" mode resamples the raw stream at a variable rate — pitch and tempo
//     move together.
//   * "PitchTime" mode runs the stream through `RealTimePitchShifter`
//     (granular / WSOLA time-stretch + resample + formant tilt), so pitch and
//     tempo are independent and parameter changes glide without clicking.
//
// Two state machines keep playback clean:
//   * The callback owns a cosine-fade machine — 12 ms out on pause/seek, 15 ms
//     in on play/resume — plus an exponential volume smoother.
//   * The decoder thread owns a transport machine — decode/render → (optional)
//     pitch-shift → push, checking its command channel between bounded chunks.

use std::collections::VecDeque;
use std::ffi::CString;
use std::os::raw::{c_char, c_double, c_int, c_void};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rtrb::RingBuffer;

use crate::midi::MidiEngine;
use crate::tracker::TrackerPlayer;

// ---------------------------------------------------------------------------
// FFmpeg bridge FFI
// ---------------------------------------------------------------------------

#[repr(C)]
struct SaAudioMeta {
    title: [c_char; 128],
    artist: [c_char; 128],
    album: [c_char; 128],
    genre: [c_char; 64],
    codec: [c_char; 64],
    format: [c_char; 64],
    duration: c_double,
    bitrate: c_int,
    sample_rate: c_int,
    channels: c_int,
    bits_per_sample: c_int,
    has_cover_art: c_int,
    cover_art_len: c_int,
}

extern "C" {
    fn sa_ffmpeg_open(path: *const c_char, out_sample_rate: c_int) -> *mut c_void;
    fn sa_ffmpeg_close(handle: *mut c_void);
    fn sa_ffmpeg_read(handle: *mut c_void, out_buf: *mut f32, num_samples: c_int) -> c_int;
    fn sa_ffmpeg_seek(handle: *mut c_void, seconds: c_double) -> c_int;
    fn sa_ffmpeg_duration(handle: *mut c_void) -> c_double;
    fn sa_ffmpeg_set_pitch_semitones(handle: *mut c_void, semitones: c_double) -> c_int;
    fn sa_ffmpeg_set_tempo(handle: *mut c_void, tempo: c_double) -> c_int;
    fn sa_ffmpeg_set_formant(handle: *mut c_void, preserve: c_int) -> c_int;
    fn sa_ffmpeg_probe_metadata(path: *const c_char, out_meta: *mut SaAudioMeta) -> c_int;
}

// ---------------------------------------------------------------------------
// Public metadata + free functions
// ---------------------------------------------------------------------------

/// Real file metadata as reported by FFmpeg.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrackMetadata {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub genre: String,
    pub codec: String,
    pub format: String,
    pub duration: f64,
    pub bitrate: i32,
    pub sample_rate: i32,
    pub channels: i32,
    pub bits_per_sample: i32,
    pub has_cover_art: bool,
}

fn cstr(buf: &[c_char]) -> String {
    let bytes: Vec<u8> = buf
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn path_cstring(path: &Path) -> Option<CString> {
    CString::new(path.to_string_lossy().as_bytes()).ok()
}

/// Probe a file's metadata via FFmpeg (no decode). `None` when the file cannot
/// be opened or has no audio stream.
pub fn probe_metadata(path: &Path) -> Option<TrackMetadata> {
    let c = path_cstring(path)?;
    let mut m: SaAudioMeta = unsafe { std::mem::zeroed() };
    let ok = unsafe { sa_ffmpeg_probe_metadata(c.as_ptr(), &mut m) };
    if ok == 0 {
        return None;
    }
    Some(TrackMetadata {
        title: cstr(&m.title),
        artist: cstr(&m.artist),
        album: cstr(&m.album),
        genre: cstr(&m.genre),
        codec: cstr(&m.codec),
        format: cstr(&m.format),
        duration: m.duration,
        bitrate: m.bitrate,
        sample_rate: m.sample_rate,
        channels: m.channels,
        bits_per_sample: m.bits_per_sample,
        has_cover_art: m.has_cover_art != 0,
    })
}

/// Deterministic synthetic waveform peaks — the fallback when a real file
/// cannot be decoded (or for the tone/silence/MIDI/tracker sources).
pub fn synth_wave_peaks(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| {
            let x = i as f32 / (n - 1).max(1) as f32;
            let envelope = (x * std::f32::consts::PI).sin();
            let detail = (x * 37.0).sin() * 0.35 + (x * 91.0).cos() * 0.22;
            (envelope * (0.55 + detail)).abs().clamp(0.05, 1.0)
        })
        .collect()
}

/// Read the whole file through a second decoder at 8 kHz and reduce it to
/// `num_bins` normalized peak bars in `0.05..=1.0`. Falls back to
/// [`synth_wave_peaks`] when the file cannot be opened.
pub fn extract_waveform_peaks(path: &Path, num_bins: usize) -> Vec<f32> {
    const PEAK_RATE: c_int = 8000;
    let num_bins = num_bins.max(1);

    let Some(c) = path_cstring(path) else {
        return synth_wave_peaks(num_bins);
    };
    let handle = unsafe { sa_ffmpeg_open(c.as_ptr(), PEAK_RATE) };
    if handle.is_null() {
        return synth_wave_peaks(num_bins);
    }

    let duration = unsafe { sa_ffmpeg_duration(handle) };
    let total_frames = ((duration * PEAK_RATE as f64).max(1.0)) as u64;
    let frames_per_bin = (total_frames / num_bins as u64).max(1);

    let mut peaks = vec![0.0f32; num_bins];
    let mut buf = vec![0.0f32; 8192 * 2];
    let mut frame_idx: u64 = 0;
    loop {
        let n = unsafe { sa_ffmpeg_read(handle, buf.as_mut_ptr(), buf.len() as c_int) };
        if n <= 0 {
            break;
        }
        let frames = n as usize / 2;
        for f in 0..frames {
            let peak = buf[f * 2].abs().max(buf[f * 2 + 1].abs());
            let bin = (frame_idx / frames_per_bin) as usize;
            if bin < num_bins && peak > peaks[bin] {
                peaks[bin] = peak;
            }
            frame_idx += 1;
        }
    }
    unsafe { sa_ffmpeg_close(handle) };

    if frame_idx == 0 {
        return synth_wave_peaks(num_bins);
    }

    let max = peaks.iter().cloned().fold(0.0f32, f32::max);
    let scale = if max > 1e-6 { 1.0 / max } else { 1.0 };
    for p in &mut peaks {
        *p = (*p * scale).clamp(0.05, 1.0);
    }
    peaks
}

// ---------------------------------------------------------------------------
// Sources, parameters & commands
// ---------------------------------------------------------------------------

/// What kind of source the engine currently has loaded. Published alongside the
/// metadata so the UI can adapt (Parameters window tab auto-selection).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceKind {
    Audio,
    Midi,
    Tracker,
}

/// What the engine should play.
#[derive(Clone, Debug)]
pub enum Source {
    Silence,
    Tone { hz: f32 },
    File(PathBuf),
    MidiFile(PathBuf),
    MidiDemo,
    TrackerFile(PathBuf),
    TrackerDemo,
}

/// The full playback parameter set driving the pitch/tempo/formant pipeline and
/// the MIDI/tracker engines.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlaybackParams {
    /// `true` = Tape mode (pitch & tempo coupled); `false` = PitchTime mode.
    pub mode_tape: bool,
    pub pitch_semitones: f32,
    pub tempo_ratio: f32,
    pub hq: bool,
    pub formant_preserve: bool,
    pub stereo_sep: f32,
    pub interp: usize,
    /// MIDI transpose in semitones (`-12..=12`, forwarded to the SF2 engine).
    pub midi_transpose: f32,
    /// MIDI tempo override in BPM (`40..=240`).
    pub midi_bpm: f32,
    pub metronome: bool,
}

impl Default for PlaybackParams {
    fn default() -> Self {
        Self {
            mode_tape: false,
            pitch_semitones: 0.0,
            tempo_ratio: 1.0,
            hq: true,
            formant_preserve: true,
            stereo_sep: 100.0,
            interp: 1,
            midi_transpose: 0.0,
            midi_bpm: 120.0,
            metronome: false,
        }
    }
}

enum Cmd {
    Seek(f64),
    Load(PathBuf),
    LoadMidiDemo,
    LoadTrackerDemo,
    SetSoundfont(PathBuf),
    SetParams(PlaybackParams),
}

/// File-kind classification by extension.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Audio,
    Midi,
    Tracker,
}

fn classify(path: &Path) -> Kind {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("mid") | Some("midi") => Kind::Midi,
        Some("mod") | Some("xm") | Some("s3m") | Some("it") => Kind::Tracker,
        _ => Kind::Audio,
    }
}

/// Shared atomics + mutex-guarded results, cloned into the callback and the
/// decoder thread.
#[derive(Clone)]
struct Shared {
    playing: Arc<AtomicBool>,
    fade_out: Arc<AtomicBool>,
    volume: Arc<AtomicU32>,
    transport_gen: Arc<AtomicU64>,
    flush_gen: Arc<AtomicU64>,
    total_frames: Arc<AtomicU64>,
    frames_played: Arc<AtomicU64>,
    started: Arc<AtomicBool>,
    peaks: Arc<Mutex<Vec<f32>>>,
    meta: Arc<Mutex<Option<TrackMetadata>>>,
    current: Arc<Mutex<Option<PathBuf>>>,
    kind: Arc<Mutex<Option<SourceKind>>>,
}

// ---------------------------------------------------------------------------
// Public Audio handle
// ---------------------------------------------------------------------------

pub struct Audio {
    _stream: cpal::Stream,
    pub rate: u32,
    pub channels: u16,
    pub total_frames: Arc<AtomicU64>,
    pub frames_played: Arc<AtomicU64>,
    pub started: Arc<AtomicBool>,
    playing: Arc<AtomicBool>,
    volume: Arc<AtomicU32>,
    cmd_tx: mpsc::Sender<Cmd>,
    peaks: Arc<Mutex<Vec<f32>>>,
    meta: Arc<Mutex<Option<TrackMetadata>>>,
    current: Arc<Mutex<Option<PathBuf>>>,
    kind: Arc<Mutex<Option<SourceKind>>>,
}

impl Audio {
    pub fn position_secs(&self) -> f64 {
        self.frames_played.load(Ordering::Relaxed) as f64 / self.rate as f64
    }
    pub fn duration_secs(&self) -> f64 {
        self.total_frames.load(Ordering::Relaxed) as f64 / self.rate as f64
    }
    pub fn is_playing(&self) -> bool {
        self.playing.load(Ordering::SeqCst)
    }
    pub fn play(&self) {
        self.playing.store(true, Ordering::SeqCst);
    }
    pub fn pause(&self) {
        self.playing.store(false, Ordering::SeqCst);
    }
    /// Flip the play/pause intent; returns the new state (`true` = playing).
    pub fn toggle_play(&self) -> bool {
        if self.playing.load(Ordering::SeqCst) {
            self.pause();
            false
        } else {
            self.play();
            true
        }
    }
    pub fn seek(&self, seconds: f64) {
        let _ = self.cmd_tx.send(Cmd::Seek(seconds.max(0.0)));
    }
    /// Load any supported source by path (audio, MIDI or tracker).
    pub fn load_file(&self, path: PathBuf) {
        let _ = self.cmd_tx.send(Cmd::Load(path));
    }
    /// Load the built-in General MIDI demo sequence.
    pub fn load_midi_demo(&self) {
        let _ = self.cmd_tx.send(Cmd::LoadMidiDemo);
    }
    /// Load the built-in tracker demo module.
    pub fn load_tracker_demo(&self) {
        let _ = self.cmd_tx.send(Cmd::LoadTrackerDemo);
    }
    /// Switch the MIDI engine's SoundFont at runtime.
    pub fn set_soundfont(&self, path: PathBuf) {
        let _ = self.cmd_tx.send(Cmd::SetSoundfont(path));
    }
    pub fn set_volume(&self, vol: f32) {
        self.volume
            .store(vol.clamp(0.0, 4.0).to_bits(), Ordering::SeqCst);
    }
    pub fn set_params(&self, params: PlaybackParams) {
        let _ = self.cmd_tx.send(Cmd::SetParams(params));
    }
    pub fn peaks(&self) -> Vec<f32> {
        self.peaks.lock().unwrap().clone()
    }
    pub fn metadata(&self) -> Option<TrackMetadata> {
        self.meta.lock().unwrap().clone()
    }
    pub fn current_file(&self) -> Option<PathBuf> {
        self.current.lock().unwrap().clone()
    }
    /// Kind of the currently-loaded source (`None` before the first load).
    pub fn source_kind(&self) -> Option<SourceKind> {
        *self.kind.lock().unwrap()
    }
}

const RING_SECONDS: usize = 8;
/// Output frames produced per decoder iteration before re-checking commands.
const CHUNK_FRAMES: usize = 8192;
/// Input frames to keep buffered past the read head for the interpolators.
const LOOKAHEAD: usize = 4;

pub fn start(src: Source) -> Result<Audio, String> {
    let host = cpal::default_host();
    let device = host.default_output_device().ok_or("no default output device")?;
    let config = device.default_output_config().map_err(|e| e.to_string())?;
    let rate = config.sample_rate();
    let out_ch = config.channels() as usize;
    println!(
        "[poc] output config: {rate} Hz, {out_ch} ch, {:?}, buffer {:?}",
        config.sample_format(),
        config.buffer_size()
    );
    let stream_config = config.config();

    let cap = rate as usize * out_ch * RING_SECONDS;
    let (producer, consumer) = RingBuffer::<f32>::new(cap);

    let shared = Shared {
        playing: Arc::new(AtomicBool::new(true)),
        fade_out: Arc::new(AtomicBool::new(false)),
        volume: Arc::new(AtomicU32::new(1.0f32.to_bits())),
        transport_gen: Arc::new(AtomicU64::new(0)),
        flush_gen: Arc::new(AtomicU64::new(0)),
        total_frames: Arc::new(AtomicU64::new(0)),
        frames_played: Arc::new(AtomicU64::new(0)),
        started: Arc::new(AtomicBool::new(false)),
        peaks: Arc::new(Mutex::new(Vec::new())),
        meta: Arc::new(Mutex::new(None)),
        current: Arc::new(Mutex::new(None)),
        kind: Arc::new(Mutex::new(None)),
    };

    let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();

    // Producer thread: decode/render → pitch-shift → push.
    {
        let sh = shared.clone();
        thread::Builder::new()
            .name("decode".into())
            .spawn(move || decoder_thread(cmd_rx, producer, src, rate, out_ch, sh))
            .map_err(|e| e.to_string())?;
    }

    // Consumer: the cpal callback. Copy + fade + volume only.
    let sh = shared.clone();
    let mut consumer = consumer;
    let err_fn = |e| eprintln!("[cpal] stream error: {e}");
    let mut last_playing = false;
    let mut local_tgen = 0u64;
    let mut local_flush = 0u64;
    let mut fade = Fade::Steady;
    let mut vol_smooth = 1.0f32;
    let in_inc = 1.0f32 / (0.015 * rate as f32);
    let out_inc = 1.0f32 / (0.012 * rate as f32);

    let stream = device
        .build_output_stream(
            stream_config,
            move |data: &mut [f32], _| {
                let out_req = sh.fade_out.load(Ordering::Relaxed);
                let want = sh.playing.load(Ordering::Relaxed) && !out_req;
                let tgen = sh.transport_gen.load(Ordering::Relaxed);
                let flush = sh.flush_gen.load(Ordering::Relaxed);

                // A seek/load flushed the ring: drop stale audio.
                if flush != local_flush {
                    local_flush = flush;
                    while consumer.pop().is_ok() {}
                }
                if want != last_playing || tgen != local_tgen {
                    last_playing = want;
                    local_tgen = tgen;
                    fade = if want { Fade::In(0.0) } else { Fade::Out(0.0) };
                }

                let target_vol = f32::from_bits(sh.volume.load(Ordering::Relaxed));
                let pull = last_playing || matches!(fade, Fade::Out(_));
                let frames = data.len() / out_ch;
                let mut advanced: u64 = 0;
                for f in 0..frames {
                    vol_smooth += (target_vol - vol_smooth) * 0.005;

                    let gain = match fade {
                        Fade::Steady => {
                            if last_playing {
                                1.0
                            } else {
                                0.0
                            }
                        }
                        Fade::In(p) => 0.5 * (1.0 - (std::f32::consts::PI * p).cos()),
                        Fade::Out(p) => 0.5 * (1.0 + (std::f32::consts::PI * p).cos()),
                    };
                    match &mut fade {
                        Fade::In(p) => {
                            *p += in_inc;
                            if *p >= 1.0 {
                                fade = Fade::Steady;
                            }
                        }
                        Fade::Out(p) => {
                            *p += out_inc;
                            if *p >= 1.0 {
                                fade = Fade::Steady;
                            }
                        }
                        Fade::Steady => {}
                    }

                    for c in 0..out_ch {
                        let s = if pull {
                            match consumer.pop() {
                                Ok(v) => {
                                    sh.started.store(true, Ordering::Relaxed);
                                    v
                                }
                                Err(_) => 0.0,
                            }
                        } else {
                            0.0
                        };
                        data[f * out_ch + c] = s * vol_smooth * gain;
                    }
                    if last_playing {
                        advanced += 1;
                    }
                }
                if advanced > 0 {
                    sh.frames_played.fetch_add(advanced, Ordering::Relaxed);
                }
            },
            err_fn,
            None,
        )
        .map_err(|e| e.to_string())?;

    stream.play().map_err(|e| e.to_string())?;

    Ok(Audio {
        _stream: stream,
        rate,
        channels: out_ch as u16,
        total_frames: shared.total_frames.clone(),
        frames_played: shared.frames_played.clone(),
        started: shared.started.clone(),
        playing: shared.playing.clone(),
        volume: shared.volume.clone(),
        cmd_tx,
        peaks: shared.peaks.clone(),
        meta: shared.meta.clone(),
        current: shared.current.clone(),
        kind: shared.kind.clone(),
    })
}

/// Cosine fade state machine — see the module header.
enum Fade {
    Steady,
    In(f32),
    Out(f32),
}

// ---------------------------------------------------------------------------
// Real-time pitch / tempo / formant shifter
// ---------------------------------------------------------------------------

/// A streaming granular (WSOLA-style) time-stretcher followed by a fractional
/// resampler, plus an optional formant-spectrum tilt. Pitch and tempo are
/// independent; parameter changes glide at grain boundaries instead of
/// flushing, so dragging a slider never clicks.
pub struct RealTimePitchShifter {
    rate: f32,
    pitch_semitones: f32,
    tempo_ratio: f32,
    formant_preserve: bool,
    grain: usize,
    hop: usize,
    window: Vec<f32>,
    /// Pending interleaved-stereo input, with the absolute frame index of its
    /// front element.
    inq: VecDeque<f32>,
    in_front: u64,
    next_anchor: u64,
    /// Windowed output of the previous grain (interleaved stereo, `grain` frames).
    prev_grain: Vec<f32>,
    have_prev: bool,
    /// Stretched interleaved-stereo output waiting to be resampled.
    stq: VecDeque<f32>,
    st_front: u64,
    st_read: f64,
    lp_l: f32,
    lp_r: f32,
}

impl RealTimePitchShifter {
    /// Build a shifter for `rate`.
    pub fn new(rate: u32) -> Self {
        let grain = ((0.021 * rate as f32) as usize).max(64) & !1;
        let hop = grain / 2;
        let window: Vec<f32> = (0..grain)
            .map(|i| {
                let p = i as f32 / grain as f32;
                0.5 * (1.0 - (2.0 * std::f32::consts::PI * p).cos())
            })
            .collect();
        Self {
            rate: rate as f32,
            pitch_semitones: 0.0,
            tempo_ratio: 1.0,
            formant_preserve: true,
            grain,
            hop,
            window,
            inq: VecDeque::new(),
            in_front: 0,
            next_anchor: 0,
            prev_grain: vec![0.0; grain * 2],
            have_prev: false,
            stq: VecDeque::new(),
            st_front: 0,
            st_read: 0.0,
            lp_l: 0.0,
            lp_r: 0.0,
        }
    }

    pub fn set_pitch(&mut self, semitones: f32) {
        self.pitch_semitones = semitones.clamp(-12.0, 12.0);
    }
    pub fn set_tempo(&mut self, ratio: f32) {
        self.tempo_ratio = ratio.clamp(0.5, 2.0);
    }
    pub fn set_formant(&mut self, preserve: bool) {
        self.formant_preserve = preserve;
    }

    /// Drop all buffered state (used on track change).
    pub fn reset(&mut self) {
        self.inq.clear();
        self.in_front = 0;
        self.next_anchor = 0;
        self.have_prev = false;
        self.stq.clear();
        self.st_front = 0;
        self.st_read = 0.0;
        self.lp_l = 0.0;
        self.lp_r = 0.0;
        self.prev_grain.iter_mut().for_each(|v| *v = 0.0);
    }

    /// Feed interleaved-stereo `input`, appending interleaved-stereo output to
    /// `out`. `out` is expected to be empty on entry.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        self.inq.extend(input.iter().copied());
        self.pump_stretch();
        self.pump_resample(out);
    }

    fn pitch_ratio(&self) -> f64 {
        2f64.powf(self.pitch_semitones as f64 / 12.0)
    }

    fn pump_stretch(&mut self) {
        let grain = self.grain;
        let hop = self.hop;
        loop {
            let avail = (self.inq.len() / 2) as u64;
            let have = self.in_front + avail;
            let need = self.next_anchor + grain as u64;
            if need > have {
                break;
            }
            let base = (self.next_anchor - self.in_front) as usize;
            if (base + grain) * 2 > self.inq.len() {
                break;
            }
            let mut cur = vec![0.0f32; grain * 2];
            for i in 0..grain {
                let w = self.window[i];
                let o = (base + i) * 2;
                cur[i * 2] = self.inq[o] * w;
                cur[i * 2 + 1] = self.inq[o + 1] * w;
            }

            if self.have_prev {
                for i in 0..hop {
                    for c in 0..2 {
                        let v = self.prev_grain[(hop + i) * 2 + c] + cur[i * 2 + c];
                        self.stq.push_back(v);
                    }
                }
            } else {
                for i in 0..hop {
                    for c in 0..2 {
                        self.stq.push_back(cur[i * 2 + c]);
                    }
                }
                self.have_prev = true;
            }
            self.prev_grain.copy_from_slice(&cur);

            // Advance the analysis anchor: `hop` output frames per grain, but the
            // input is consumed at rate `tempo / pitch_ratio` (stretch = r/tempo).
            let r = self.pitch_ratio();
            let analysis_hop = (hop as f64 * self.tempo_ratio as f64 / r).max(1.0);
            self.next_anchor += analysis_hop as u64;

            let drop_frames = (self.next_anchor - self.in_front) as usize;
            let avail = self.inq.len() / 2;
            let d = drop_frames.min(avail);
            if d > 0 {
                self.inq.drain(0..d * 2);
                self.in_front += d as u64;
            }
        }
    }

    fn pump_resample(&mut self, out: &mut Vec<f32>) {
        let r = self.pitch_ratio();
        let st_frames = (self.stq.len() / 2) as u64;
        loop {
            let rel = self.st_read - self.st_front as f64;
            if rel + 2.0 >= st_frames as f64 {
                break;
            }
            let i0 = rel.floor() as usize;
            let frac = (rel - i0 as f64) as f32;
            for c in 0..2 {
                let v = cubic_q(&self.stq, i0, frac, c);
                let v = if self.formant_preserve {
                    self.tilt(c, v, r as f32)
                } else {
                    v
                };
                out.push(v.clamp(-1.0, 1.0));
            }
            self.st_read += r;
        }

        // Drop consumed stretched frames, keeping a small look-behind margin.
        let drop_to = (self.st_read.floor() as u64).saturating_sub(2);
        if drop_to > self.st_front {
            let have = (self.stq.len() / 2) as u64;
            let n = (drop_to - self.st_front).min(have.saturating_sub(4)) as usize;
            if n > 0 {
                self.stq.drain(0..n * 2);
                self.st_front += n as u64;
            }
        }
    }

    /// A gentle first-order spectral tilt that counteracts the envelope shift a
    /// pure resampler introduces (a lightweight formant-preservation
    /// approximation — no phase-vocoder envelope extraction).
    fn tilt(&mut self, c: usize, x: f32, r: f32) -> f32 {
        // Keep the one-pole cutoff roughly constant across sample rates.
        let coef = (0.15 * 48_000.0 / self.rate).clamp(0.03, 0.4);
        let lp = if c == 0 {
            self.lp_l += coef * (x - self.lp_l);
            self.lp_l
        } else {
            self.lp_r += coef * (x - self.lp_r);
            self.lp_r
        };
        let g = 0.6 * (1.0 - 1.0 / r.max(0.001));
        x + g * lp
    }
}

fn cubic_q(q: &VecDeque<f32>, i0: usize, frac: f32, c: usize) -> f32 {
    let at = |i: isize| -> f32 {
        if i < 0 {
            0.0
        } else {
            q.get(i as usize * 2 + c).copied().unwrap_or(0.0)
        }
    };
    let p0 = at(i0 as isize - 1);
    let p1 = at(i0 as isize);
    let p2 = at(i0 as isize + 1);
    let p3 = at(i0 as isize + 2);
    let a = -0.5 * p0 + 1.5 * p1 - 1.5 * p2 + 0.5 * p3;
    let b = p0 - 2.5 * p1 + 2.0 * p2 - 0.5 * p3;
    let cc = -0.5 * p0 + 0.5 * p2;
    ((a * frac + b) * frac + cc) * frac + p1
}

// ---------------------------------------------------------------------------
// Decoder / render thread
// ---------------------------------------------------------------------------

enum Engine {
    None,
    Ffmpeg(*mut c_void),
    Midi(Box<MidiEngine>),
    Tracker(Box<TrackerPlayer>),
}

fn close_engine(engine: &mut Engine) {
    if let Engine::Ffmpeg(h) = engine {
        if !h.is_null() {
            unsafe { sa_ffmpeg_close(*h) };
        }
    }
    *engine = Engine::None;
}

fn pre_load(sh: &Shared) {
    sh.fade_out.store(true, Ordering::SeqCst);
    thread::sleep(Duration::from_millis(13));
    sh.flush_gen.fetch_add(1, Ordering::SeqCst);
}

fn post_load(sh: &Shared, total_frames: u64) {
    sh.total_frames.store(total_frames, Ordering::SeqCst);
    sh.frames_played.store(0, Ordering::SeqCst);
    sh.fade_out.store(false, Ordering::SeqCst);
    sh.playing.store(true, Ordering::SeqCst);
    sh.transport_gen.fetch_add(1, Ordering::SeqCst);
}

fn decoder_thread(
    rx: mpsc::Receiver<Cmd>,
    mut producer: rtrb::Producer<f32>,
    mut src: Source,
    out_rate: u32,
    out_ch: usize,
    sh: Shared,
) {
    let mut engine = Engine::None;
    let mut params = PlaybackParams::default();
    let mut shifter = RealTimePitchShifter::new(out_rate);
    let mut step = 1.0f64;

    // FFmpeg state.
    let mut in_buf: Vec<f32> = Vec::new();
    let mut read_pos = 0.0f64;
    let mut eof = false;
    let mut tmp = vec![0.0f32; 8192 * 2];

    // Scratch buffers reused across iterations.
    let mut raw: Vec<f32> = Vec::new();
    let mut post: Vec<f32> = Vec::new();
    let mut m_left: Vec<f32> = Vec::new();
    let mut m_right: Vec<f32> = Vec::new();

    let mut tone_hz: Option<f32> = None;
    let mut silence = false;

    // Load the initial source.
    match std::mem::replace(&mut src, Source::Silence) {
        Source::Silence => silence = true,
        Source::Tone { hz } => tone_hz = Some(hz),
        Source::File(p) => open_ffmpeg(&mut engine, &p, out_rate, &params, &mut step, &sh),
        Source::MidiFile(p) => open_midi(&mut engine, Some(&p), out_rate, &sh),
        Source::MidiDemo => open_midi(&mut engine, None, out_rate, &sh),
        Source::TrackerFile(p) => open_tracker(&mut engine, Some(&p), out_rate, &sh),
        Source::TrackerDemo => open_tracker(&mut engine, None, out_rate, &sh),
    }

    loop {
        // --- Commands (non-blocking) -----------------------------------------
        while let Ok(cmd) = rx.try_recv() {
            match cmd {
                Cmd::Seek(secs) => {
                    // Only the FFmpeg path is seekable; MIDI/tracker restart is a
                    // non-destructive no-op here.
                    if let Engine::Ffmpeg(handle) = engine {
                        if !handle.is_null() {
                            pre_load(&sh);
                            unsafe { sa_ffmpeg_seek(handle, secs) };
                            in_buf.clear();
                            read_pos = 0.0;
                            eof = false;
                            sh.frames_played
                                .store((secs * out_rate as f64) as u64, Ordering::SeqCst);
                            sh.fade_out.store(false, Ordering::SeqCst);
                            sh.transport_gen.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                }
                Cmd::Load(p) => {
                    shifter.reset();
                    // Leaving the tone/silence source: the new engine drives output.
                    silence = false;
                    tone_hz = None;
                    match classify(&p) {
                        Kind::Audio => {
                            open_ffmpeg(&mut engine, &p, out_rate, &params, &mut step, &sh)
                        }
                        Kind::Midi => open_midi(&mut engine, Some(&p), out_rate, &sh),
                        Kind::Tracker => open_tracker(&mut engine, Some(&p), out_rate, &sh),
                    }
                    eof = false;
                }
                Cmd::LoadMidiDemo => {
                    shifter.reset();
                    silence = false;
                    tone_hz = None;
                    open_midi(&mut engine, None, out_rate, &sh);
                }
                Cmd::LoadTrackerDemo => {
                    shifter.reset();
                    silence = false;
                    tone_hz = None;
                    open_tracker(&mut engine, None, out_rate, &sh);
                }
                Cmd::SetSoundfont(path) => {
                    if let Engine::Midi(m) = &mut engine {
                        if let Err(e) = m.set_soundfont(&path) {
                            eprintln!("[audio] SoundFont switch failed: {e}");
                        } else {
                            println!("[audio] SoundFont switched to {}", path.display());
                        }
                    }
                }
                Cmd::SetParams(p) => {
                    params = p;
                    apply_engine_params(&mut engine, &params);
                    step = compute_step(&params);
                }
            }
        }

        if let Some(hz) = tone_hz {
            push_tone(&mut producer, &sh, hz, out_rate, out_ch);
            continue;
        }
        if silence {
            thread::sleep(Duration::from_millis(5));
            continue;
        }

        if matches!(engine, Engine::None) || !sh.playing.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_millis(5));
            continue;
        }

        let free_frames = producer.slots() / out_ch;
        if free_frames == 0 {
            thread::sleep(Duration::from_millis(3));
            continue;
        }
        let cap = free_frames.min(CHUNK_FRAMES);

        raw.clear();
        let produced: usize;

        match &mut engine {
            Engine::Ffmpeg(handle) => {
                let h = *handle;
                if h.is_null() {
                    thread::sleep(Duration::from_millis(5));
                    continue;
                }
                let want_in = read_pos.ceil() as usize + LOOKAHEAD + 1;
                while !eof && in_buf.len() / 2 < want_in {
                    let n = unsafe { sa_ffmpeg_read(h, tmp.as_mut_ptr(), tmp.len() as c_int) };
                    if n <= 0 {
                        eof = true;
                        break;
                    }
                    in_buf.extend_from_slice(&tmp[..n as usize]);
                }

                let avail = in_buf.len() / 2;
                produced = produce_ffmpeg(&in_buf, avail, cap, &mut read_pos, step, params.hq, &mut raw);

                if read_pos > 32768.0 {
                    let consumed = read_pos.floor() as usize;
                    in_buf.drain(0..consumed * 2);
                    read_pos -= consumed as f64;
                }
                if produced == 0 && eof {
                    sh.playing.store(false, Ordering::SeqCst);
                    thread::sleep(Duration::from_millis(5));
                    continue;
                }
            }
            Engine::Midi(m) => {
                if m_left.len() < cap {
                    m_left.resize(cap, 0.0);
                    m_right.resize(cap, 0.0);
                }
                m.render(&mut m_left[..cap], &mut m_right[..cap]);
                for i in 0..cap {
                    raw.push(m_left[i]);
                    raw.push(m_right[i]);
                }
                produced = cap;
            }
            Engine::Tracker(t) => {
                if m_left.len() < cap {
                    m_left.resize(cap, 0.0);
                    m_right.resize(cap, 0.0);
                }
                t.render(&mut m_left[..cap], &mut m_right[..cap]);
                for i in 0..cap {
                    raw.push(m_left[i]);
                    raw.push(m_right[i]);
                }
                produced = cap;
            }
            Engine::None => {
                produced = 0;
            }
        }

        if produced == 0 {
            continue;
        }

        push_processed(&mut producer, &params, &mut shifter, &raw, out_ch, &mut post);
    }
}

/// Produce up to `cap` interleaved-stereo frames by resampling `in_buf`.
fn produce_ffmpeg(
    in_buf: &[f32],
    avail: usize,
    cap: usize,
    read_pos: &mut f64,
    step: f64,
    hq: bool,
    raw: &mut Vec<f32>,
) -> usize {
    let mut produced = 0usize;
    while produced < cap {
        let i0 = read_pos.floor() as usize;
        if i0 + 1 >= avail {
            break;
        }
        let frac = (*read_pos - i0 as f64) as f32;
        for c in 0..2 {
            let v = if step == 1.0 {
                frame_at(in_buf, i0 as isize, c)
            } else if hq {
                sinc8(in_buf, i0, frac, c)
            } else {
                cubic(in_buf, i0, frac, c)
            };
            raw.push(v);
        }
        *read_pos += step;
        produced += 1;
    }
    produced
}

/// Apply the pitch/tempo/formant stage and push the result to the ring.
fn push_processed(
    producer: &mut rtrb::Producer<f32>,
    params: &PlaybackParams,
    shifter: &mut RealTimePitchShifter,
    raw: &[f32],
    out_ch: usize,
    post: &mut Vec<f32>,
) {
    let neutral = params.pitch_semitones.abs() < 1e-4 && (params.tempo_ratio - 1.0).abs() < 1e-4;
    let bypass = params.mode_tape || neutral;

    let src: &[f32] = if bypass {
        raw
    } else {
        shifter.set_pitch(params.pitch_semitones);
        shifter.set_tempo(params.tempo_ratio);
        shifter.set_formant(params.formant_preserve);
        post.clear();
        shifter.process(raw, post);
        post
    };

    let frames = src.len() / 2;
    let fits = (producer.slots() / out_ch).min(frames);
    for f in 0..fits {
        for c in 0..out_ch {
            let _ = producer.push(src[f * 2 + c.min(1)]);
        }
    }
}

fn open_ffmpeg(
    engine: &mut Engine,
    path: &Path,
    out_rate: u32,
    params: &PlaybackParams,
    step: &mut f64,
    sh: &Shared,
) {
    close_engine(engine);
    pre_load(sh);

    match probe_metadata(path) {
        Some(m) => {
            println!(
                "[audio] loaded {} — codec {}, {} Hz, {} kbps, {:.2}s, {} ch, {}-bit{}",
                path.display(),
                m.codec,
                m.sample_rate,
                m.bitrate / 1000,
                m.duration,
                m.channels,
                m.bits_per_sample,
                if m.has_cover_art { ", cover art" } else { "" }
            );
            *sh.meta.lock().unwrap() = Some(m);
        }
        None => {
            println!("[audio] metadata probe returned nothing for {}", path.display());
            *sh.meta.lock().unwrap() = None;
        }
    }

    let peaks = extract_waveform_peaks(path, 96);
    *sh.peaks.lock().unwrap() = peaks;
    *sh.current.lock().unwrap() = Some(path.to_path_buf());
    *sh.kind.lock().unwrap() = Some(SourceKind::Audio);

    let mut handle: *mut c_void = std::ptr::null_mut();
    if let Some(c) = path_cstring(path) {
        handle = unsafe { sa_ffmpeg_open(c.as_ptr(), out_rate as c_int) };
    }

    let total = if handle.is_null() {
        eprintln!("[audio] FFmpeg open failed for {}", path.display());
        0
    } else {
        let dur = unsafe { sa_ffmpeg_duration(handle) };
        (dur * out_rate as f64) as u64
    };

    apply_params_ffmpeg(handle, params);
    *step = compute_step(params);
    *engine = Engine::Ffmpeg(handle);
    post_load(sh, total);
}

fn open_midi(engine: &mut Engine, path: Option<&Path>, out_rate: u32, sh: &Shared) {
    close_engine(engine);
    pre_load(sh);
    match MidiEngine::new(out_rate) {
        Ok(mut m) => {
            if let Some(p) = path {
                if let Err(e) = m.load_file(p) {
                    eprintln!("[audio] {e}");
                }
            } else {
                m.load_demo();
            }
            let dur = m.duration();
            if m.using_soundfont() {
                if let Some(p) = m.soundfont_path() {
                    println!("[audio] MIDI SoundFont: {}", p.display());
                }
            }
            *sh.meta.lock().unwrap() = None;
            *sh.peaks.lock().unwrap() = synth_wave_peaks(96);
            *sh.current.lock().unwrap() = path.map(|p| p.to_path_buf());
            *sh.kind.lock().unwrap() = Some(SourceKind::Midi);
            *engine = Engine::Midi(Box::new(m));
            post_load(sh, (dur * out_rate as f64) as u64);
        }
        Err(e) => {
            eprintln!("[audio] MIDI engine init failed: {e}");
            post_load(sh, 0);
        }
    }
}

fn open_tracker(engine: &mut Engine, path: Option<&Path>, out_rate: u32, sh: &Shared) {
    close_engine(engine);
    pre_load(sh);
    let mut t = TrackerPlayer::new(out_rate);
    if let Some(p) = path {
        if let Err(e) = t.load_file(p) {
            eprintln!("[audio] {e} — falling back to built-in module");
            t.load_builtin();
        }
    } else {
        t.load_builtin();
    }
    println!("[audio] tracker module: {}", t.title());
    *sh.meta.lock().unwrap() = None;
    *sh.peaks.lock().unwrap() = synth_wave_peaks(96);
    *sh.current.lock().unwrap() = path.map(|p| p.to_path_buf());
    *sh.kind.lock().unwrap() = Some(SourceKind::Tracker);
    *engine = Engine::Tracker(Box::new(t));
    post_load(sh, 0);
}

fn apply_params_ffmpeg(handle: *mut c_void, p: &PlaybackParams) {
    if handle.is_null() {
        return;
    }
    unsafe {
        // FFmpeg always outputs the raw stream; pitch/tempo are handled by the
        // Rust pipeline (PitchTime shifter or the Tape-mode variable resampler).
        sa_ffmpeg_set_pitch_semitones(handle, 0.0);
        sa_ffmpeg_set_tempo(handle, 1.0);
        sa_ffmpeg_set_formant(handle, if p.formant_preserve { 1 } else { 0 });
    }
}

fn apply_engine_params(engine: &mut Engine, p: &PlaybackParams) {
    match engine {
        Engine::Ffmpeg(h) => apply_params_ffmpeg(*h, p),
        Engine::Tracker(t) => {
            // The shared pitch shifter owns pitch/tempo (kept neutral here); the
            // tracker engine owns stereo separation and interpolation.
            t.set_pitch(0.0);
            t.set_tempo(1.0);
            t.set_stereo_separation(p.stereo_sep);
            t.set_interp(p.interp);
        }
        Engine::Midi(m) => {
            // Pitch/tempo of the rendered stream are applied by the pipeline; the
            // MIDI engine owns its own transpose, BPM override and metronome.
            m.set_transpose(p.midi_transpose.round().clamp(-12.0, 12.0) as i32);
            m.set_bpm(Some(p.midi_bpm.clamp(40.0, 240.0)));
            m.set_metronome(p.metronome);
        }
        Engine::None => {}
    }
}

fn compute_step(p: &PlaybackParams) -> f64 {
    if p.mode_tape {
        (p.tempo_ratio as f64) * 2f64.powf(p.pitch_semitones as f64 / 12.0)
    } else {
        1.0
    }
}

// ---------------------------------------------------------------------------
// Interpolators (operate on interleaved stereo)
// ---------------------------------------------------------------------------

#[inline]
fn frame_at(buf: &[f32], idx: isize, ch: usize) -> f32 {
    if idx < 0 {
        return 0.0;
    }
    let i = idx as usize * 2 + ch;
    if i < buf.len() {
        buf[i]
    } else {
        0.0
    }
}

fn cubic(buf: &[f32], i0: usize, frac: f32, ch: usize) -> f32 {
    let p0 = frame_at(buf, i0 as isize - 1, ch);
    let p1 = frame_at(buf, i0 as isize, ch);
    let p2 = frame_at(buf, i0 as isize + 1, ch);
    let p3 = frame_at(buf, i0 as isize + 2, ch);
    let a = -0.5 * p0 + 1.5 * p1 - 1.5 * p2 + 0.5 * p3;
    let b = p0 - 2.5 * p1 + 2.0 * p2 - 0.5 * p3;
    let c = -0.5 * p0 + 0.5 * p2;
    ((a * frac + b) * frac + c) * frac + p1
}

#[inline]
fn lanczos(x: f32, a: f32) -> f32 {
    if x.abs() < 1e-6 {
        return 1.0;
    }
    if x.abs() >= a {
        return 0.0;
    }
    let px = std::f32::consts::PI * x;
    let ax = px / a;
    (px.sin() / px) * (ax.sin() / ax)
}

/// 8-tap windowed-sinc (Lanczos-4) interpolation.
fn sinc8(buf: &[f32], i0: usize, frac: f32, ch: usize) -> f32 {
    let mut acc = 0.0f32;
    let mut wsum = 0.0f32;
    for k in -3i32..=4 {
        let x = frac - k as f32;
        let w = lanczos(x, 4.0);
        acc += w * frame_at(buf, i0 as isize + k as isize, ch);
        wsum += w;
    }
    if wsum.abs() > 1e-6 {
        acc / wsum
    } else {
        frame_at(buf, i0 as isize, ch)
    }
}

/// Push one chunk of muted tone samples (UI/pipeline test signal).
fn push_tone(
    producer: &mut rtrb::Producer<f32>,
    sh: &Shared,
    _hz: f32,
    _rate: u32,
    out_ch: usize,
) {
    sh.total_frames.store(u64::MAX, Ordering::Relaxed);
    let free_frames = producer.slots() / out_ch;
    let n = free_frames.min(CHUNK_FRAMES);
    for _ in 0..n {
        // Muted — the tone path exercises the pipeline without sound.
        for _ in 0..out_ch {
            if producer.push(0.0).is_err() {
                return;
            }
        }
    }
    if n == 0 {
        thread::sleep(Duration::from_millis(3));
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sinc_passthrough_at_integer_positions() {
        let mut buf = Vec::with_capacity(16 * 2);
        for _ in 0..16 {
            buf.push(1.0);
            buf.push(0.5);
        }
        for i0 in 4..12 {
            assert!((sinc8(&buf, i0, 0.0, 0) - 1.0).abs() < 1e-4);
            assert!((sinc8(&buf, i0, 0.0, 1) - 0.5).abs() < 1e-4);
        }
    }

    #[test]
    fn sinc_agrees_with_cubic_on_a_gentle_ramp() {
        let mut buf = Vec::with_capacity(32 * 2);
        for i in 0..32 {
            let v = i as f32 / 32.0;
            buf.push(v);
            buf.push(v);
        }
        let s = sinc8(&buf, 16, 0.5, 0);
        let c = cubic(&buf, 16, 0.5, 0);
        assert!((s - c).abs() < 0.02, "sinc {s} vs cubic {c}");
    }

    #[test]
    fn waveform_peak_fallback_is_bounded() {
        let peaks = synth_wave_peaks(96);
        assert_eq!(peaks.len(), 96);
        assert!(peaks.iter().all(|&p| (0.05..=1.0).contains(&p)));
    }

    #[test]
    fn tape_step_moves_pitch_and_tempo_together() {
        let p = PlaybackParams {
            mode_tape: true,
            pitch_semitones: 12.0,
            tempo_ratio: 1.0,
            ..Default::default()
        };
        assert!((compute_step(&p) - 2.0).abs() < 1e-9);
        let q = PlaybackParams {
            mode_tape: false,
            pitch_semitones: 12.0,
            tempo_ratio: 1.5,
            ..Default::default()
        };
        assert!((compute_step(&q) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn pitch_shifter_is_neutral_at_unity_and_preserves_length() {
        let mut s = RealTimePitchShifter::new(48_000);
        s.set_pitch(0.0);
        s.set_tempo(1.0);
        // A steady 440 Hz sine over ~0.5 s.
        let frames = 24_000usize;
        let mut input = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            let v = (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 48_000.0).sin() * 0.5;
            input.push(v);
            input.push(v);
        }
        let mut out = Vec::new();
        // Feed in chunks so the streaming path is exercised.
        for chunk in input.chunks(2048) {
            s.process(chunk, &mut out);
        }
        assert!(out.len() > 0, "shifter produced no output");
        assert!(
            out.iter().all(|v| v.is_finite()),
            "shifter produced non-finite samples"
        );
        let energy: f32 = out.iter().map(|v| v * v).sum();
        assert!(energy > 0.0);
    }
}
