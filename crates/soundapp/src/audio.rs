// Audio engine: transport control + a cpal callback that drains a lock-free
// rtrb ring buffer, fed by a decoder thread.
//
// Decoding is done by the native FFmpeg bridge (`sa_ffmpeg_*`, see
// `ffmpeg_bridge.cpp`) — universal format support plus metadata. `symphonia`
// remains the pure-Rust path for the tone/silence sources and any future
// fallback.
//
// Two state machines keep playback clean:
//   * The callback owns a cosine-fade machine — 12 ms out on pause/seek, 15 ms
//     in on play/resume — so no transition ever steps the waveform, and an
//     exponential volume smoother so dragging the gain slider never zippers.
//   * The decoder thread owns a transport machine — decode → variable-rate
//     resample → push, checking its command channel between bounded chunks so a
//     seek or load takes effect within milliseconds even with a full ring.
//
// The FFmpeg output is float32 stereo at the device rate; "Tape" mode then
// resamples on the fly (8-tap Lanczos when HQ, cubic otherwise), while
// "PitchTime" mode delegates pitch/tempo to FFmpeg's rubberband/atempo filters.

use std::ffi::CString;
use std::os::raw::{c_char, c_double, c_int, c_void};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rtrb::RingBuffer;

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
/// cannot be decoded (or for the tone/silence sources).
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
// Sources & shared state
// ---------------------------------------------------------------------------

pub enum Source {
    Silence,
    Tone { hz: f32 },
    File(PathBuf),
}

enum Cmd {
    Seek(f64),
    Load(PathBuf),
    SetParams(Params),
}

#[derive(Clone, Copy)]
struct Params {
    mode_tape: bool,
    pitch: f32,
    tempo: f32,
    hq: bool,
}

impl Default for Params {
    fn default() -> Self {
        Self { mode_tape: false, pitch: 0.0, tempo: 1.0, hq: true }
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
    params: Mutex<Params>,
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
    pub fn load_file(&self, path: PathBuf) {
        let _ = self.cmd_tx.send(Cmd::Load(path));
    }
    pub fn set_volume(&self, vol: f32) {
        self.volume
            .store(vol.clamp(0.0, 4.0).to_bits(), Ordering::SeqCst);
    }
    pub fn set_params(&self, mode_tape: bool, pitch_semitones: f32, tempo_ratio: f32, hq_sinc: bool) {
        let params = Params { mode_tape, pitch: pitch_semitones, tempo: tempo_ratio, hq: hq_sinc };
        *self.params.lock().unwrap() = params;
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
    };

    let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();

    // Producer thread: decode → resample → push. Always spawned — even for the
    // silence/tone sources — so UI-driven `Load`/`Seek` commands always land.
    {
        let sh = shared.clone();
        thread::Builder::new()
            .name("decode".into())
            .spawn(move || decoder_thread(cmd_rx, producer, src, rate, out_ch, sh))
            .map_err(|e| e.to_string())?;
    }

    // Consumer: the cpal callback. Copy + fade + volume only — never decode.
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

                // A seek/load flushed the ring: drop everything queued so stale
                // audio at the old position cannot leak into the new one.
                if flush != local_flush {
                    local_flush = flush;
                    while consumer.pop().is_ok() {}
                }
                // Desired-state transition → (re)start the appropriate fade.
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
        params: Mutex::new(Params::default()),
    })
}

/// Cosine fade state machine — see the module header.
enum Fade {
    Steady,
    In(f32),
    Out(f32),
}

// ---------------------------------------------------------------------------
// Decoder thread
// ---------------------------------------------------------------------------

fn decoder_thread(
    rx: mpsc::Receiver<Cmd>,
    mut producer: rtrb::Producer<f32>,
    mut src: Source,
    out_rate: u32,
    out_ch: usize,
    sh: Shared,
) {
    let mut handle: *mut c_void = std::ptr::null_mut();
    let mut params = Params::default();
    let mut step = 1.0f64;
    let mut in_buf: Vec<f32> = Vec::new(); // interleaved stereo at out_rate
    let mut read_pos = 0.0f64;
    let mut eof = false;
    let mut tmp = vec![0.0f32; 8192 * 2];

    // An initial file source is loaded exactly like a `Load` command.
    if let Source::File(p) = &src {
        let p = p.clone();
        open_into(&mut handle, &p, out_rate, &mut params, &mut step, &sh);
        src = Source::File(PathBuf::new()); // consumed; mark so we don't re-open
    }

    loop {
        // --- Commands (non-blocking) -----------------------------------------
        while let Ok(cmd) = rx.try_recv() {
            match cmd {
                Cmd::Seek(secs) => {
                    sh.fade_out.store(true, Ordering::SeqCst);
                    thread::sleep(Duration::from_millis(13));
                    sh.flush_gen.fetch_add(1, Ordering::SeqCst);
                    if !handle.is_null() {
                        unsafe {
                            sa_ffmpeg_seek(handle, secs);
                        }
                    }
                    in_buf.clear();
                    read_pos = 0.0;
                    eof = false;
                    sh.frames_played
                        .store((secs * out_rate as f64) as u64, Ordering::SeqCst);
                    sh.fade_out.store(false, Ordering::SeqCst);
                    sh.transport_gen.fetch_add(1, Ordering::SeqCst);
                }
                Cmd::Load(p) => {
                    open_into(&mut handle, &p, out_rate, &mut params, &mut step, &sh);
                    eof = false;
                }
                Cmd::SetParams(p) => {
                    params = p;
                    apply_params(handle, &params);
                    step = compute_step(&params);
                }
            }
        }

        // --- Tone / silence sources ------------------------------------------
        match &src {
            Source::Tone { hz } => {
                push_tone(&mut producer, &sh, *hz, out_rate, out_ch);
                continue;
            }
            Source::Silence => {
                thread::sleep(Duration::from_millis(5));
                continue;
            }
            Source::File(_) => {}
        }

        if handle.is_null() || !sh.playing.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_millis(5));
            continue;
        }

        // --- Produce a bounded chunk so commands stay responsive -------------
        let free_frames = producer.slots() / out_ch;
        if free_frames == 0 {
            thread::sleep(Duration::from_millis(3));
            continue;
        }
        let cap = free_frames.min(CHUNK_FRAMES);
        let want_in = read_pos.ceil() as usize + LOOKAHEAD + 1;

        while !eof && in_buf.len() / 2 < want_in {
            let n = unsafe { sa_ffmpeg_read(handle, tmp.as_mut_ptr(), tmp.len() as c_int) };
            if n <= 0 {
                eof = true;
                break;
            }
            in_buf.extend_from_slice(&tmp[..n as usize]);
        }

        let avail = in_buf.len() / 2;
        let mut produced = 0usize;
        while produced < cap {
            let i0 = read_pos.floor() as usize;
            if i0 + 1 >= avail {
                break;
            }
            let frac = (read_pos - i0 as f64) as f32;
            for c in 0..out_ch {
                let sc = c.min(1);
                let v = if step == 1.0 {
                    frame_at(&in_buf, i0 as isize, sc)
                } else if params.hq {
                    sinc8(&in_buf, i0, frac, sc)
                } else {
                    cubic(&in_buf, i0, frac, sc)
                };
                // Exactly `cap` slots are free, so this push cannot fail.
                let _ = producer.push(v);
            }
            read_pos += step;
            produced += 1;
        }

        if produced == 0 && eof {
            // End of stream: park until the next Load/Seek restarts playback.
            sh.playing.store(false, Ordering::SeqCst);
            thread::sleep(Duration::from_millis(5));
            continue;
        }

        // Drop consumed input frames once they are safely behind the read head.
        if read_pos > 32768.0 {
            let consumed = read_pos.floor() as usize;
            in_buf.drain(0..consumed * 2);
            read_pos -= consumed as f64;
        }
    }
}

/// Open `path` into `handle`, probe metadata + waveform, apply params, and
/// reset the decoder position to 0.
fn open_into(
    handle: &mut *mut c_void,
    path: &Path,
    out_rate: u32,
    params: &mut Params,
    step: &mut f64,
    sh: &Shared,
) {
    sh.fade_out.store(true, Ordering::SeqCst);
    thread::sleep(Duration::from_millis(13));
    sh.flush_gen.fetch_add(1, Ordering::SeqCst);

    if !handle.is_null() {
        unsafe { sa_ffmpeg_close(*handle) };
        *handle = std::ptr::null_mut();
    }

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
            println!(
                "[audio] metadata probe returned nothing for {}",
                path.display()
            );
            *sh.meta.lock().unwrap() = None;
        }
    }

    let peaks = extract_waveform_peaks(path, 96);
    println!("[audio] waveform peaks: {} bins", peaks.len());
    *sh.peaks.lock().unwrap() = peaks;
    *sh.current.lock().unwrap() = Some(path.to_path_buf());

    if let Some(c) = path_cstring(path) {
        *handle = unsafe { sa_ffmpeg_open(c.as_ptr(), out_rate as c_int) };
    }

    if handle.is_null() {
        eprintln!("[audio] FFmpeg open failed for {}", path.display());
        sh.total_frames.store(0, Ordering::SeqCst);
    } else {
        let dur = unsafe { sa_ffmpeg_duration(*handle) };
        sh.total_frames
            .store((dur * out_rate as f64) as u64, Ordering::SeqCst);
    }

    apply_params(*handle, params);
    *step = compute_step(params);
    sh.frames_played.store(0, Ordering::SeqCst);
    sh.fade_out.store(false, Ordering::SeqCst);
    sh.playing.store(true, Ordering::SeqCst);
    sh.transport_gen.fetch_add(1, Ordering::SeqCst);
}

fn apply_params(handle: *mut c_void, p: &Params) {
    if handle.is_null() {
        return;
    }
    unsafe {
        if p.mode_tape {
            // Tape mode: no FFmpeg DSP — the variable-rate resample moves both
            // tempo and pitch together.
            sa_ffmpeg_set_pitch_semitones(handle, 0.0);
            sa_ffmpeg_set_tempo(handle, 1.0);
        } else {
            // PitchTime: FFmpeg's rubberband/atempo does pitch & tempo.
            sa_ffmpeg_set_pitch_semitones(handle, p.pitch as f64);
            sa_ffmpeg_set_tempo(handle, p.tempo as f64);
        }
    }
}

fn compute_step(p: &Params) -> f64 {
    if p.mode_tape {
        (p.tempo as f64) * 2f64.powf(p.pitch as f64 / 12.0)
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
    // Catmull-Rom.
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
        // Muted — the tone path exists to exercise the pipeline without sound.
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
        // Interleaved stereo: left = 1.0, right = 0.5 for every frame.
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
        // A gentle ramp is nearly linear, so both interpolators should agree at
        // the midpoint.
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
        let p = Params { mode_tape: true, pitch: 12.0, tempo: 1.0, hq: true };
        assert!((compute_step(&p) - 2.0).abs() < 1e-9);
        let q = Params { mode_tape: false, pitch: 12.0, tempo: 1.5, hq: true };
        assert!((compute_step(&q) - 1.0).abs() < 1e-9);
    }
}
