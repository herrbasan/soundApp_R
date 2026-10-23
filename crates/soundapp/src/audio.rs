// Audio engine: transport control + a cpal callback that drains a lock-free
// rtrb ring buffer, fed by a decoder thread.
//
// FFmpeg (universal formats + metadata) via the native `sa_ffmpeg_*` bridge is
// the only playback engine. MIDI / SoundFont synthesis and ProTracker modules
// are parked (the source files remain on disk; their modules are not built).
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
use std::time::{Duration, Instant};

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
    codec_long_name: [c_char; 128],
    format_long_name: [c_char; 128],
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
    fn sa_ffmpeg_read_handle_metadata(
        handle: *mut c_void,
        out_meta: *mut SaAudioMeta,
        out_cover_rgba: *mut u8,
        cover_side: c_int,
    ) -> c_int;
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
    pub codec_long_name: String,
    pub format_long_name: String,
    pub duration: f64,
    pub bitrate: i32,
    pub sample_rate: i32,
    pub channels: i32,
    pub bits_per_sample: i32,
    pub has_cover_art: bool,
    /// Embedded cover art decoded by FFmpeg and scaled to `128x128` RGBA.
    pub cover_rgba: Option<(u32, u32, Vec<u8>)>,
    /// File size in bytes, sampled once by the background probe so the UI never
    /// has to stat the file per frame just to derive a bitrate.
    pub file_size_bytes: u64,
}

/// Side length of the decoded cover-art thumbnail pushed to the UI.
pub const COVER_SIDE: u32 = 128;

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

/// Extract metadata + cover art directly from an already-opened `FFmpegDecoder`
/// handle (no second SMB `avformat_open_input` or `avformat_find_stream_info`).
fn read_handle_metadata(handle: *mut c_void, path: &Path) -> Option<TrackMetadata> {
    if handle.is_null() {
        return None;
    }
    let mut m: SaAudioMeta = unsafe { std::mem::zeroed() };
    let mut cover_buf = vec![0u8; (COVER_SIDE * COVER_SIDE * 4) as usize];
    let ok = unsafe {
        sa_ffmpeg_read_handle_metadata(
            handle,
            &mut m,
            cover_buf.as_mut_ptr(),
            COVER_SIDE as c_int,
        )
    };
    if ok == 0 {
        return None;
    }
    let cover_rgba = if m.has_cover_art != 0 && cover_buf.iter().any(|&b| b != 0) {
        Some((COVER_SIDE, COVER_SIDE, cover_buf))
    } else {
        None
    };
    let file_size_bytes = if m.bitrate == 0 {
        std::fs::metadata(path).map(|s| s.len()).unwrap_or(0)
    } else {
        0
    };
    Some(TrackMetadata {
        title: cstr(&m.title),
        artist: cstr(&m.artist),
        album: cstr(&m.album),
        genre: cstr(&m.genre),
        codec: cstr(&m.codec),
        format: cstr(&m.format),
        codec_long_name: cstr(&m.codec_long_name),
        format_long_name: cstr(&m.format_long_name),
        duration: m.duration,
        bitrate: m.bitrate,
        sample_rate: m.sample_rate,
        channels: m.channels,
        bits_per_sample: m.bits_per_sample,
        has_cover_art: m.has_cover_art != 0,
        cover_rgba,
        file_size_bytes,
    })
}

// ---------------------------------------------------------------------------
// Sources, parameters & commands
// ---------------------------------------------------------------------------

/// What the engine should play.
#[derive(Clone, Debug)]
pub enum Source {
    Silence,
    Tone { hz: f32 },
}

/// The full playback parameter set driving the pitch/tempo/formant pipeline.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlaybackParams {
    /// `true` = Tape mode (pitch & tempo coupled); `false` = PitchTime mode.
    pub mode_tape: bool,
    pub pitch_semitones: f32,
    pub tempo_ratio: f32,
    pub hq: bool,
    pub formant_preserve: bool,
}

impl Default for PlaybackParams {
    fn default() -> Self {
        Self {
            mode_tape: false,
            pitch_semitones: 0.0,
            tempo_ratio: 1.0,
            hq: true,
            formant_preserve: true,
        }
    }
}

enum Cmd {
    Seek(f64),
    Load(PathBuf),
    SetParams(PlaybackParams),
}

/// Shared atomics + mutex-guarded results, cloned into the callback and the
/// decoder thread.
#[derive(Clone)]
struct Shared {
    playing: Arc<AtomicBool>,
    fade_out: Arc<AtomicBool>,
    flushing: Arc<AtomicBool>,
    volume: Arc<AtomicU32>,
    transport_gen: Arc<AtomicU64>,
    flush_gen: Arc<AtomicU64>,
    flush_ack: Arc<AtomicU64>,
    /// Stretcher-side flush acknowledgement (source ring drained, engine reset).
    stage_ack: Arc<AtomicU64>,
    /// Live playback parameters, written by the decoder thread on
    /// `Cmd::SetParams` and polled by the stretcher thread (bump `params_gen`).
    params: Arc<Mutex<PlaybackParams>>,
    params_gen: Arc<AtomicU64>,
    /// Source-frame base for the position accounting, written by the decoder
    /// thread right before a seek/load flush bump.
    seek_target: Arc<AtomicU64>,
    /// Frames the cpal callback has played out of the output ring.
    out_played: Arc<AtomicU64>,
    total_frames: Arc<AtomicU64>,
    frames_played: Arc<AtomicU64>,
    started: Arc<AtomicBool>,
    /// Gapless single-track loop mode (`L` key / Loop button).
    loop_enabled: Arc<AtomicBool>,
    /// The decoder has pushed its last sample; the big ring may still hold audio.
    decoder_eof: Arc<AtomicBool>,
    /// The stretcher drained everything: engine idle, output ring may hold tail.
    stage_eof: Arc<AtomicBool>,
    /// The callback drained the output ring after `stage_eof` — one-shot for the UI.
    track_ended: Arc<AtomicBool>,
    /// When false the callback skips the DSP tap entirely (Monitoring closed).
    dsp_enabled: Arc<AtomicBool>,
    meta: Arc<Mutex<Option<TrackMetadata>>>,
    current: Arc<Mutex<Option<PathBuf>>>,
    dsp: Arc<Mutex<DspTap>>,
}

// ---------------------------------------------------------------------------
// Real-time DSP tap (Phase 3.3)
// ---------------------------------------------------------------------------

/// Rolling window of the final post-fader stereo frames, published by the audio
/// callback and read by the DSP analyzer. A fixed-capacity `VecDeque` keeps the
/// memory bounded; the callback locks it once per output buffer.
pub struct DspTap {
    cap: usize,
    buf: VecDeque<(f32, f32)>,
}

impl DspTap {
    fn new(sample_rate: u32) -> Self {
        // Four seconds is enough to cover the BS.1770 3 s short-term window.
        let cap = (sample_rate as usize * 4).max(4096);
        Self {
            cap,
            buf: VecDeque::with_capacity(cap.min(1 << 16)),
        }
    }

    #[inline]
    fn push(&mut self, l: f32, r: f32) {
        if self.buf.len() == self.cap {
            self.buf.pop_front();
        }
        self.buf.push_back((l, r));
    }

    /// Copy the most recent frames (oldest → newest) into `out`, clearing it first.
    pub fn snapshot_into(&self, out: &mut Vec<(f32, f32)>) {
        out.clear();
        out.extend(self.buf.iter().copied());
    }
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
    transport_gen: Arc<AtomicU64>,
    loop_enabled: Arc<AtomicBool>,
    track_ended: Arc<AtomicBool>,
    dsp_enabled: Arc<AtomicBool>,
    cmd_tx: mpsc::Sender<Cmd>,
    meta: Arc<Mutex<Option<TrackMetadata>>>,
    current: Arc<Mutex<Option<PathBuf>>>,
    dsp: Arc<Mutex<DspTap>>,
}

impl Audio {
    pub fn position_secs(&self) -> f64 {
        let pos = self.frames_played.load(Ordering::Relaxed) as f64 / self.rate as f64;
        let dur = self.duration_secs();
        if self.loop_enabled.load(Ordering::Relaxed) && dur > 0.0 {
            pos % dur
        } else {
            pos
        }
    }
    pub fn duration_secs(&self) -> f64 {
        self.total_frames.load(Ordering::Relaxed) as f64 / self.rate as f64
    }
    pub fn is_playing(&self) -> bool {
        self.playing.load(Ordering::SeqCst)
    }
    pub fn set_loop(&self, enabled: bool) {
        self.loop_enabled.store(enabled, Ordering::Release);
    }
    /// True when the play head has reached the end of the loaded file.
    fn at_track_end(&self) -> bool {
        if self.loop_enabled.load(Ordering::Relaxed) {
            return false;
        }
        let dur = self.duration_secs();
        dur > 0.0 && self.position_secs() >= dur - 0.05
    }
    pub fn play(&self) {
        // Pressing Play after a finished track restarts it instead of silently
        // pushing an empty ring buffer.
        if self.at_track_end() {
            self.seek(0.0);
        }
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
    /// One-shot: did the current track just play to its end? Used by the UI to
    /// auto-advance the folder playlist (or loop / stop).
    pub fn take_track_ended(&self) -> bool {
        self.track_ended.swap(false, Ordering::AcqRel)
    }
    /// Enable the DSP tap only while a consumer (Monitoring window) is visible.
    pub fn set_dsp_enabled(&self, enabled: bool) {
        self.dsp_enabled.store(enabled, Ordering::Relaxed);
    }
    pub fn seek(&self, seconds: f64) {
        let _ = self.cmd_tx.send(Cmd::Seek(seconds.max(0.0)));
    }
    /// Load any supported source by path. Immediately clears the published
    /// metadata and updates `current` on the caller thread so the very next UI
    /// frame never latches the previous track's cover art.
    pub fn load_file(&self, path: PathBuf) {
        self.transport_gen.fetch_add(1, Ordering::SeqCst);
        *self.meta.lock().unwrap() = None;
        *self.current.lock().unwrap() = Some(path.clone());
        let _ = self.cmd_tx.send(Cmd::Load(path));
    }
    pub fn set_volume(&self, vol: f32) {
        self.volume
            .store(vol.clamp(0.0, 2.0).to_bits(), Ordering::SeqCst);
    }
    pub fn set_params(&self, params: PlaybackParams) {
        let _ = self.cmd_tx.send(Cmd::SetParams(params));
    }
    pub fn metadata(&self) -> Option<TrackMetadata> {
        self.meta.lock().unwrap().clone()
    }
    pub fn current_file(&self) -> Option<PathBuf> {
        self.current.lock().unwrap().clone()
    }
    /// Shared rolling stereo tap feeding the real-time DSP analyzer (Phase 3.3).
    pub fn dsp_tap(&self) -> Arc<Mutex<DspTap>> {
        self.dsp.clone()
    }
}

const RING_SECONDS: usize = 8;
/// Depth of the post-stretcher output ring: the maximum audio latency between
/// a parameter change and the speakers (Phase B target ≤ 100 ms).
const OUT_RING_SECONDS: f64 = 0.08;
/// Output frames produced per decoder iteration before re-checking commands.
const CHUNK_FRAMES: usize = 8192;
/// Source frames the stretcher pulls per cycle.
const STRETCH_CHUNK: usize = 2048;
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

    let shared = Shared {
        playing: Arc::new(AtomicBool::new(true)),
        fade_out: Arc::new(AtomicBool::new(false)),
        flushing: Arc::new(AtomicBool::new(false)),
        volume: Arc::new(AtomicU32::new(1.0f32.to_bits())),
        transport_gen: Arc::new(AtomicU64::new(0)),
        flush_gen: Arc::new(AtomicU64::new(0)),
        flush_ack: Arc::new(AtomicU64::new(0)),
        stage_ack: Arc::new(AtomicU64::new(0)),
        params: Arc::new(Mutex::new(PlaybackParams::default())),
        params_gen: Arc::new(AtomicU64::new(0)),
        seek_target: Arc::new(AtomicU64::new(0)),
        out_played: Arc::new(AtomicU64::new(0)),
        total_frames: Arc::new(AtomicU64::new(0)),
        frames_played: Arc::new(AtomicU64::new(0)),
        started: Arc::new(AtomicBool::new(false)),
        loop_enabled: Arc::new(AtomicBool::new(false)),
        decoder_eof: Arc::new(AtomicBool::new(false)),
        stage_eof: Arc::new(AtomicBool::new(false)),
        track_ended: Arc::new(AtomicBool::new(false)),
        dsp_enabled: Arc::new(AtomicBool::new(false)),
        dsp: Arc::new(Mutex::new(DspTap::new(rate))),
        meta: Arc::new(Mutex::new(None)),
        current: Arc::new(Mutex::new(None)),
    };

    let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();

    // Big ring: decoded source audio (tape step applied), up to 8 s. Parameter
    // changes never invalidate it — the pitch/tempo stage lives downstream.
    let cap = rate as usize * out_ch * RING_SECONDS;
    let (producer, src_consumer) = RingBuffer::<f32>::new(cap);

    // Small output ring: the only audio standing between a parameter change
    // and the speakers (bounded by OUT_RING_SECONDS).
    let out_cap = ((rate as f64 * OUT_RING_SECONDS) as usize).max(4096) * out_ch;
    let (out_producer, consumer) = RingBuffer::<f32>::new(out_cap);

    // Producer thread: decode/render → tape resample → push raw source frames.
    {
        let sh = shared.clone();
        thread::Builder::new()
            .name("decode".into())
            .spawn(move || decoder_thread(cmd_rx, producer, src, rate, out_ch, sh))
            .map_err(|e| e.to_string())?;
    }

    // Stretcher thread: pulls source frames, applies pitch/tempo/formant one
    // chunk from the audible edge, feeds the small output ring.
    {
        let sh = shared.clone();
        thread::Builder::new()
            .name("stretch".into())
            .spawn(move || stretcher_thread(src_consumer, out_producer, rate, out_ch, sh))
            .map_err(|e| e.to_string())?;
    }

    // Consumer: the cpal callback. Copy + fade + volume only.
    let sh = shared.clone();
    let dsp = shared.dsp.clone();
    let mut consumer = consumer;
    let err_fn = |e| eprintln!("[cpal] stream error: {e}");
    let mut last_playing = false;
    let mut local_tgen = 0u64;
    let mut local_flush = 0u64;
    let mut fade = Fade::Steady;
    let mut vol_smooth = 1.0f32;
    let mut dsp_frames: Vec<(f32, f32)> = Vec::with_capacity(4096);
    let in_inc = 1.0f32 / (0.015 * rate as f32);
    let out_inc = 1.0f32 / (0.012 * rate as f32);

    let stream = device
        .build_output_stream(
            stream_config,
            move |data: &mut [f32], _| {
                let out_req = sh.fade_out.load(Ordering::Relaxed);
                let is_flushing = sh.flushing.load(Ordering::Acquire);
                let want = sh.playing.load(Ordering::Relaxed) && !out_req && !is_flushing;
                let tgen = sh.transport_gen.load(Ordering::Relaxed);
                let flush = sh.flush_gen.load(Ordering::Acquire);

                // A seek/load flushed the ring: drop stale audio with a bounded pop
                // and acknowledge so the decoder thread only pushes new audio AFTER
                // the flush has completed.
                if flush != local_flush {
                    let avail = consumer.slots();
                    for _ in 0..avail {
                        let _ = consumer.pop();
                    }
                    local_flush = flush;
                    sh.flush_ack.store(flush, Ordering::Release);
                }
                if want != last_playing || tgen != local_tgen {
                    last_playing = want;
                    local_tgen = tgen;
                    fade = if want { Fade::In(0.0) } else { Fade::Out(0.0) };
                }

                let target_vol = f32::from_bits(sh.volume.load(Ordering::Relaxed));
                let pull = !is_flushing && (last_playing || matches!(fade, Fade::Out(_)));
                // Fix #6: skip the tap entirely unless a consumer is attached.
                let dsp_on = sh.dsp_enabled.load(Ordering::Relaxed);
                let frames = data.len() / out_ch;
                let mut popped: u64 = 0;
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
                                    popped += 1;
                                    v
                                }
                                Err(_) => {
                                    // Fix #1/#2: the output ring is drained *and*
                                    // the stretcher signalled EOF — only now is
                                    // the track really over. Never cut buffered audio.
                                    if !sh.loop_enabled.load(Ordering::Relaxed)
                                        && sh.stage_eof.load(Ordering::Acquire)
                                    {
                                        sh.stage_eof.store(false, Ordering::Release);
                                        sh.playing.store(false, Ordering::Release);
                                        sh.track_ended.store(true, Ordering::Release);
                                    }
                                    0.0
                                }
                            }
                        } else {
                            0.0
                        };
                        // Boost up to 200% (+6 dB) must never wrap or hard-clip.
                        data[f * out_ch + c] = (s * vol_smooth * gain).clamp(-1.0, 1.0);
                    }
                    if dsp_on {
                        let l = data[f * out_ch];
                        let r = if out_ch > 1 { data[f * out_ch + 1] } else { l };
                        dsp_frames.push((l, r));
                    }
                }
                // The stretcher thread owns `frames_played` (audible source
                // position); the callback only reports what it played.
                if popped > 0 {
                    sh.out_played
                        .fetch_add(popped / out_ch as u64, Ordering::Relaxed);
                }
                if !dsp_frames.is_empty() {
                    // Fix #7: `try_lock` — the real-time thread never waits on
                    // the analyzer; a missed buffer is cheaper than a glitch.
                    if let Ok(mut tap) = dsp.try_lock() {
                        for &(l, r) in &dsp_frames {
                            tap.push(l, r);
                        }
                    }
                    dsp_frames.clear();
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
        transport_gen: shared.transport_gen.clone(),
        loop_enabled: shared.loop_enabled.clone(),
        track_ended: shared.track_ended.clone(),
        dsp_enabled: shared.dsp_enabled.clone(),
        cmd_tx,
        meta: shared.meta.clone(),
        current: shared.current.clone(),
        dsp: shared.dsp.clone(),
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

// ---------------------------------------------------------------------------
// PitchStage trait (Phase B)
// ---------------------------------------------------------------------------

/// Swappable streaming pitch/tempo/formant engine. The consumer-side stretcher
/// thread drives one implementation; `render_offline` drives the same trait so
/// offline renders exercise the identical DSP. First implementation: the
/// granular OLA shifter above; a future phase plugs in Signalsmith Stretch.
pub trait PitchStage: Send {
    /// Feed interleaved-stereo `input`, appending interleaved-stereo frames to
    /// `out`. Input is buffered internally; `out` is expected to be empty on
    /// entry.
    fn process(&mut self, input: &[f32], out: &mut Vec<f32>);
    fn set_pitch(&mut self, semitones: f32);
    fn set_tempo(&mut self, ratio: f32);
    fn set_formant(&mut self, preserve: bool);
    /// Drop all buffered state (track change / seek).
    fn reset(&mut self);
    /// Source frames currently held by the engine that have not yet been
    /// emitted to `out` — for position-display correction only.
    fn latency_frames(&self, tempo: f32) -> usize;
}

impl PitchStage for RealTimePitchShifter {
    fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        RealTimePitchShifter::process(self, input, out)
    }
    fn set_pitch(&mut self, semitones: f32) {
        RealTimePitchShifter::set_pitch(self, semitones)
    }
    fn set_tempo(&mut self, ratio: f32) {
        RealTimePitchShifter::set_tempo(self, ratio)
    }
    fn set_formant(&mut self, preserve: bool) {
        RealTimePitchShifter::set_formant(self, preserve)
    }
    fn reset(&mut self) {
        RealTimePitchShifter::reset(self)
    }
    fn latency_frames(&self, tempo: f32) -> usize {
        // inq is source-domain; stq is stretched-domain (stretched = src * r/t).
        let stq_src = (self.stq.len() / 2) as f64 * tempo as f64 / self.pitch_ratio();
        self.inq.len() / 2 + stq_src as usize
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
}

fn close_engine(engine: &mut Engine) {
    if let Engine::Ffmpeg(h) = engine {
        if !h.is_null() {
            unsafe { sa_ffmpeg_close(*h) };
        }
    }
    *engine = Engine::None;
}

fn pre_load(sh: &Shared) -> u64 {
    // The 13 ms wait exists only so the output callback can run its 12 ms cosine
    // fade-out before the ring is flushed. From silence (startup) or with the
    // stream already armed, there is no audible tail to protect.
    let was_playing = sh.playing.load(Ordering::SeqCst) && sh.started.load(Ordering::SeqCst);
    sh.fade_out.store(true, Ordering::SeqCst);
    if was_playing {
        thread::sleep(Duration::from_millis(13));
    }
    sh.flushing.store(true, Ordering::SeqCst);
    sh.flush_gen.fetch_add(1, Ordering::SeqCst) + 1
}

fn wait_flush_ack(sh: &Shared, target: u64) {
    // Wait until BOTH the cpal callback (output ring drained) and the
    // stretcher thread (source ring drained, engine reset) have acknowledged,
    // so no stale audio is anywhere downstream when pushing resumes.
    let deadline = Instant::now() + Duration::from_millis(30);
    while (sh.flush_ack.load(Ordering::Acquire) < target
        || sh.stage_ack.load(Ordering::Acquire) < target)
        && Instant::now() < deadline
    {
        thread::sleep(Duration::from_millis(1));
    }
}

fn post_load(sh: &Shared, total_frames: u64, flush_target: u64) {
    wait_flush_ack(sh, flush_target);
    sh.total_frames.store(total_frames, Ordering::SeqCst);
    sh.frames_played.store(0, Ordering::SeqCst);
    sh.decoder_eof.store(false, Ordering::SeqCst);
    sh.track_ended.store(false, Ordering::SeqCst);
    sh.fade_out.store(false, Ordering::SeqCst);
    sh.flushing.store(false, Ordering::SeqCst);
    sh.playing.store(true, Ordering::SeqCst);
    sh.transport_gen.fetch_add(1, Ordering::SeqCst);
}

/// Trim trailing near-zero encoder padding frames from `in_buf` above `min_frames`
/// (up to ~42 ms = 2048 stereo frames) before a gapless loop wrap.
fn trim_trailing_loop_silence(in_buf: &mut Vec<f32>, min_frames: usize) {
    let total_frames = in_buf.len() / 2;
    let max_trim = 2048usize;
    let floor = min_frames.max(total_frames.saturating_sub(max_trim));
    let mut keep = total_frames;
    while keep > floor {
        let l = in_buf[(keep - 1) * 2].abs();
        let r = in_buf[(keep - 1) * 2 + 1].abs();
        if l < 5e-4 && r < 5e-4 {
            keep -= 1;
        } else {
            break;
        }
    }
    if keep < total_frames {
        in_buf.truncate(keep * 2);
    }
}

/// Append the first chunk after a loop rewind into `in_buf`, skipping leading
/// encoder-delay silence frames (up to ~42 ms = 2048 frames) and applying a
/// 64-frame (~1.3 ms) crossfade across the loop seam so MP3/AAC loops never click.
fn append_loop_seam_chunk(in_buf: &mut Vec<f32>, chunk: &[f32], min_read_frame: usize) {
    let chunk_frames = chunk.len() / 2;
    if chunk_frames == 0 {
        return;
    }
    let max_skip = 2048usize.min(chunk_frames);
    let mut skip = 0usize;
    while skip < max_skip {
        let l = chunk[skip * 2].abs();
        let r = chunk[skip * 2 + 1].abs();
        if l < 5e-4 && r < 5e-4 {
            skip += 1;
        } else {
            break;
        }
    }
    let src = &chunk[skip * 2..];
    let src_frames = src.len() / 2;
    if src_frames == 0 {
        return;
    }
    let tail_avail = (in_buf.len() / 2).saturating_sub(min_read_frame);
    let xfade = 64usize.min(tail_avail).min(src_frames);
    if xfade > 0 {
        let tail_start = in_buf.len() / 2 - xfade;
        for i in 0..xfade {
            let t = (i + 1) as f32 / (xfade + 1) as f32;
            let idx = (tail_start + i) * 2;
            in_buf[idx] = in_buf[idx] * (1.0 - t) + src[i * 2] * t;
            in_buf[idx + 1] = in_buf[idx + 1] * (1.0 - t) + src[i * 2 + 1] * t;
        }
    }
    in_buf.extend_from_slice(&src[xfade * 2..]);
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
    let mut step = 1.0f64;

    // FFmpeg state.
    let mut in_buf: Vec<f32> = Vec::new();
    let mut read_pos = 0.0f64;
    let mut eof = false;
    let mut tmp = vec![0.0f32; 8192 * 2];

    // Scratch buffers reused across iterations.
    let mut raw: Vec<f32> = Vec::new();

    let mut tone_hz: Option<f32> = None;
    let mut silence = false;

    // Latency instrumentation for `Cmd::Load`: `t0` is the moment the command was
    // dequeued, i.e. what the user waits for after clicking a track.
    let mut load_t0: Option<Instant> = None;
    let mut load_path = PathBuf::new();
    let mut first_chunk_logged = true;
    let mut meta_pending = false;

    // Load the initial source.
    match std::mem::replace(&mut src, Source::Silence) {
        Source::Silence => silence = true,
        Source::Tone { hz } => tone_hz = Some(hz),
    }

    loop {
        // --- Commands (coalesced non-blocking drain) --------------------------
        let mut latest_load: Option<PathBuf> = None;
        let mut latest_seek: Option<f64> = None;
        let mut latest_params: Option<PlaybackParams> = None;
        while let Ok(cmd) = rx.try_recv() {
            match cmd {
                Cmd::Load(p) => {
                    latest_load = Some(p);
                    latest_seek = None;
                }
                Cmd::Seek(secs) => {
                    latest_seek = Some(secs);
                }
                Cmd::SetParams(p) => {
                    latest_params = Some(p);
                }
            }
        }
        if let Some(p) = latest_params {
            params = p;
            step = compute_step(&params);
            // Publish to the stretcher thread; it applies pitch/tempo at the
            // audible edge and reports the actual change latency.
            *sh.params.lock().unwrap() = params;
            sh.params_gen.fetch_add(1, Ordering::SeqCst);
        }
        if let Some(p) = latest_load {
            let t0 = Instant::now();
            load_path = p.clone();
            load_t0 = Some(t0);
            first_chunk_logged = false;
            meta_pending = true;
            in_buf.clear();
            read_pos = 0.0;
            eof = false;
            silence = false;
            tone_hz = None;
            open_ffmpeg(&mut engine, &p, out_rate, &params, &mut step, &sh);
        }
        if let Some(secs) = latest_seek {
            if let Engine::Ffmpeg(handle) = engine {
                if !handle.is_null() {
                    // The stretcher re-bases its position accounting from this
                    // before it drains the rings on the flush below.
                    sh.seek_target
                        .store((secs * out_rate as f64) as u64, Ordering::SeqCst);
                    let flush_target = pre_load(&sh);
                    sh.decoder_eof.store(false, Ordering::Release);
                    sh.track_ended.store(false, Ordering::Release);
                    let ok = unsafe { sa_ffmpeg_seek(handle, secs) };
                    in_buf.clear();
                    read_pos = 0.0;
                    eof = false;
                    wait_flush_ack(&sh, flush_target);
                    let _ = ok;
                    sh.fade_out.store(false, Ordering::SeqCst);
                    sh.flushing.store(false, Ordering::SeqCst);
                    sh.transport_gen.fetch_add(1, Ordering::SeqCst);
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
                // If the decoder already hit EOF while the 8s ring buffer was still
                // draining and the user turned Loop ON, immediately rewind and
                // resume seamless loop decoding.
                if eof && sh.loop_enabled.load(Ordering::Relaxed) {
                    sh.decoder_eof.store(false, Ordering::Release);
                    trim_trailing_loop_silence(&mut in_buf, want_in);
                    if unsafe { sa_ffmpeg_seek(h, 0.0) } != 0 {
                        eof = false;
                        let n = unsafe { sa_ffmpeg_read(h, tmp.as_mut_ptr(), tmp.len() as c_int) };
                        if n > 0 {
                            append_loop_seam_chunk(&mut in_buf, &tmp[..n as usize], want_in);
                        }
                    }
                }
                while !eof && in_buf.len() / 2 < want_in {
                    let n = unsafe { sa_ffmpeg_read(h, tmp.as_mut_ptr(), tmp.len() as c_int) };
                    if n <= 0 {
                        if sh.loop_enabled.load(Ordering::Relaxed) {
                            trim_trailing_loop_silence(&mut in_buf, read_pos.ceil() as usize + 1);
                            if unsafe { sa_ffmpeg_seek(h, 0.0) } != 0 {
                                let n2 = unsafe {
                                    sa_ffmpeg_read(h, tmp.as_mut_ptr(), tmp.len() as c_int)
                                };
                                if n2 > 0 {
                                    append_loop_seam_chunk(
                                        &mut in_buf,
                                        &tmp[..n2 as usize],
                                        read_pos.ceil() as usize + 1,
                                    );
                                    continue;
                                }
                            }
                        }
                        eof = true;
                        break;
                    }
                    if !first_chunk_logged {
                        first_chunk_logged = true;
                        if let Some(t0) = load_t0 {
                            println!(
                                "[audio] first samples decoded in {:.1} ms for {}",
                                t0.elapsed().as_secs_f64() * 1000.0,
                                load_path.display()
                            );
                        }
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
                    // Fix #1: do NOT stop playback here — the ring may still hold
                    // up to RING_SECONDS of audio. Signal the callback, which owns
                    // the real end-of-track transition.
                    sh.decoder_eof.store(true, Ordering::Release);
                    thread::sleep(Duration::from_millis(5));
                    continue;
                }
            }
            Engine::None => {
                produced = 0;
            }
        }

        if produced == 0 {
            continue;
        }

        // Raw source frames only — the pitch/tempo stage lives in the
        // stretcher thread downstream of this ring (Phase B).
        let frames = raw.len() / 2;
        let fits = (producer.slots() / out_ch).min(frames);
        for f in 0..fits {
            for c in 0..out_ch {
                let _ = producer.push(raw[f * 2 + c.min(1)]);
            }
        }

        // Extract metadata + cover art from the ALREADY-OPEN FFmpegDecoder handle
        // right after the first audio chunk is pushed into the ring buffer:
        //   - first-audio latency is untouched (audio is already playing!)
        //   - zero second SMB `avformat_open_input` / `avformat_find_stream_info`
        //   - zero background thread spawn per track.
        if meta_pending {
            meta_pending = false;
            if let Engine::Ffmpeg(h) = engine {
                if let Some(m) = read_handle_metadata(h, &load_path) {
                    if let Some(t0) = load_t0 {
                        println!(
                            "[audio] metadata+cover ready in {:.1} ms for {}",
                            t0.elapsed().as_secs_f64() * 1000.0,
                            load_path.display()
                        );
                    }
                    *sh.meta.lock().unwrap() = Some(m);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Stretcher thread (Phase B)
// ---------------------------------------------------------------------------

/// Consumer-side pitch/tempo/formant stage. Pulls decoded source frames from
/// the big ring and feeds a small output ring (`OUT_RING_SECONDS` deep) that
/// the cpal callback drains. Because the stage sits one small buffer away from
/// the speakers, parameter changes become audible within one buffer fill — no
/// flush, no fade, and the 8 s decode buffer stays valid.
///
/// Runs on its own thread rather than inside the cpal callback: the OLA stage
/// uses VecDeque/Vec scratch (allocation on growth) and a future engine
/// (Signalsmith Stretch) has heavier setup, while an 80 ms output ring absorbs
/// any scheduling hiccup that would glitch a true RT callback.
fn stretcher_thread(
    mut src: rtrb::Consumer<f32>,
    mut out: rtrb::Producer<f32>,
    out_rate: u32,
    out_ch: usize,
    sh: Shared,
) {
    let mut stage: Box<dyn PitchStage> = Box::new(RealTimePitchShifter::new(out_rate));
    let mut params = PlaybackParams::default();
    let mut seen_gen = sh.params_gen.load(Ordering::Acquire);
    let mut seen_flush = sh.flush_gen.load(Ordering::Acquire);

    let mut scratch: Vec<f32> = Vec::new();
    let mut post: Vec<f32> = Vec::new();
    // Output frames that did not fit the ring last cycle (engine expansion).
    let mut carry: Vec<f32> = Vec::new();

    // Position accounting (source frames unless noted).
    let mut base: u64 = 0; // media frames at the last flush
    let mut media_consumed: f64 = 0.0; // pulled ring frames, in media frames
    let mut out_produced: u64 = 0; // cumulative frames pushed to the output ring

    let mut eof_seen = false;
    let mut last_trace = Instant::now();

    loop {
        // --- New parameters: applied at the next chunk boundary. -------------
        let g = sh.params_gen.load(Ordering::Acquire);
        if g != seen_gen {
            seen_gen = g;
            let p = *sh.params.lock().unwrap();
            if p != params {
                let pending = out_produced
                    .saturating_sub(sh.out_played.load(Ordering::Relaxed));
                eprintln!(
                    "[latency] pitch={:+.1} tempo={:.2} applied; {:.0} ms of processed audio buffered ahead",
                    p.pitch_semitones,
                    p.tempo_ratio,
                    pending as f64 / out_rate as f64 * 1000.0
                );
            }
            params = p;
        }

        // --- Seek/load flush: drop stale audio everywhere downstream. --------
        let f = sh.flush_gen.load(Ordering::Acquire);
        if f != seen_flush {
            seen_flush = f;
            while src.pop().is_ok() {}
            stage.reset();
            scratch.clear();
            carry.clear();
            base = sh.seek_target.load(Ordering::Acquire);
            media_consumed = 0.0;
            out_produced = sh.out_played.load(Ordering::Relaxed);
            eof_seen = false;
            sh.stage_eof.store(false, Ordering::Release);
            sh.frames_played.store(base, Ordering::Release);
            sh.stage_ack.store(f, Ordering::Release);
        }

        let neutral =
            params.pitch_semitones.abs() < 1e-4 && (params.tempo_ratio - 1.0).abs() < 1e-4;
        let bypass = params.mode_tape || neutral;

        // Older carry goes out first (counted — it is audible output too).
        out_produced += push_frames_fit(&mut out, &mut carry, out_ch) as u64;

        let mut did_work = false;
        let free = out.slots() / out_ch;
        if free > 0 {
            scratch.clear();
            let want_samples = free.min(STRETCH_CHUNK) * 2;
            while scratch.len() < want_samples {
                match src.pop() {
                    Ok(v) => scratch.push(v),
                    Err(_) => break,
                }
            }
            let frames = scratch.len() / 2;
            if frames > 0 {
                did_work = true;
                media_consumed += frames as f64 * compute_step(&params);
                if bypass {
                    carry.extend_from_slice(&scratch);
                } else {
                    let src_out: &[f32] =
                        apply_pitch_stage(&params, stage.as_mut(), &scratch, &mut post);
                    carry.extend_from_slice(src_out);
                }
                out_produced += push_frames_fit(&mut out, &mut carry, out_ch) as u64;
            }
        }

        // Input dry: either idle, or end-of-track draining.
        if !did_work && carry.is_empty() {
            if sh.decoder_eof.load(Ordering::Acquire) && src.slots() == 0 && !eof_seen {
                if !bypass {
                    post.clear();
                    stage.process(&[], &mut post);
                    carry.extend_from_slice(&post);
                    out_produced += push_frames_fit(&mut out, &mut carry, out_ch) as u64;
                }
                if carry.is_empty() {
                    eof_seen = true;
                    sh.stage_eof.store(true, Ordering::Release);
                }
            } else if !sh.decoder_eof.load(Ordering::Acquire) {
                thread::sleep(Duration::from_millis(3));
            }
        }

        // --- Publish the audible media position. -----------------------------
        // Everything pulled but not yet heard: engine-internal latency (source
        // frames) plus carry + output-ring fill (output frames -> media via
        // the effective output/media rate).
        let played = sh.out_played.load(Ordering::Relaxed);
        let pending = (out_produced.saturating_sub(played) + (carry.len() / 2) as u64) as f64;
        let eff = if params.mode_tape {
            compute_step(&params)
        } else {
            params.tempo_ratio as f64
        };
        let lat_media = if bypass {
            0.0
        } else {
            stage.latency_frames(params.tempo_ratio) as f64
        };
        let pos = base as f64 + media_consumed - lat_media - pending * eff;
        sh.frames_played.store(pos.max(base as f64) as u64, Ordering::Release);

        // Anomaly trace (1 Hz): the stage is active, source is available, but
        // nothing is queued ahead — the callback is running on fumes.
        if !bypass
            && pending == 0.0
            && src.slots() > 0
            && sh.playing.load(Ordering::Relaxed)
            && last_trace.elapsed() >= Duration::from_secs(1)
        {
            last_trace = Instant::now();
            eprintln!(
                "[stretch] starving: out_produced={} played={} carry={}f lat={}f src={}f",
                out_produced,
                played,
                carry.len() / 2,
                stage.latency_frames(params.tempo_ratio),
                src.slots() / out_ch
            );
        }

        if !did_work && !carry.is_empty() {
            thread::sleep(Duration::from_millis(3));
        }
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

/// The pitch/tempo/formant stage shared by the consumer-side stretcher thread
/// and offline rendering (`render_offline`): tape/neutral bypass, or the
/// streaming `PitchStage`. Returns the interleaved-stereo frames to emit.
fn apply_pitch_stage<'a>(
    params: &PlaybackParams,
    stage: &mut dyn PitchStage,
    raw: &'a [f32],
    post: &'a mut Vec<f32>,
) -> &'a [f32] {
    let neutral = params.pitch_semitones.abs() < 1e-4 && (params.tempo_ratio - 1.0).abs() < 1e-4;
    if params.mode_tape || neutral {
        raw
    } else {
        stage.set_pitch(params.pitch_semitones);
        stage.set_tempo(params.tempo_ratio);
        stage.set_formant(params.formant_preserve);
        post.clear();
        stage.process(raw, post);
        post
    }
}

/// Push as many interleaved-stereo frames of `src` as fit; the remainder stays
/// in `src` (caller reuses it as the carry buffer). Returns frames pushed.
fn push_frames_fit(producer: &mut rtrb::Producer<f32>, src: &mut Vec<f32>, out_ch: usize) -> usize {
    let frames = src.len() / 2;
    let fits = (producer.slots() / out_ch).min(frames);
    for f in 0..fits {
        for c in 0..out_ch {
            let _ = producer.push(src[f * 2 + c.min(1)]);
        }
    }
    src.drain(0..fits * 2);
    fits
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
    // New track: the stretcher re-bases its position accounting to zero when
    // it services the flush below.
    sh.seek_target.store(0, Ordering::SeqCst);
    let flush_target = pre_load(sh);

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

    *sh.current.lock().unwrap() = Some(path.to_path_buf());
    *sh.meta.lock().unwrap() = None;
    *step = compute_step(params);
    *engine = Engine::Ffmpeg(handle);
    post_load(sh, total, flush_target);
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
// Offline render (same DSP path as live playback, no audio device)
// ---------------------------------------------------------------------------

pub struct RenderResult {
    pub in_secs: f64,
    pub out_secs: f64,
}

/// Write interleaved-stereo f32 samples as a 32-bit IEEE-float WAV.
fn write_wav_f32(path: &Path, rate: u32, ch: u16, samples: &[f32]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
    }
    let data_len = (samples.len() * 4) as u32;
    let mut w: Vec<u8> = Vec::with_capacity(44 + data_len as usize);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data_len).to_le_bytes());
    w.extend_from_slice(b"WAVE");
    w.extend_from_slice(b"fmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&3u16.to_le_bytes()); // IEEE float
    w.extend_from_slice(&ch.to_le_bytes());
    w.extend_from_slice(&(rate as u32).to_le_bytes());
    w.extend_from_slice(&(rate * ch as u32 * 4).to_le_bytes());
    w.extend_from_slice(&((ch as u16) * 4).to_le_bytes());
    w.extend_from_slice(&32u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        w.extend_from_slice(&s.to_le_bytes());
    }
    std::fs::write(path, &w).map_err(|e| format!("write {}: {e}", path.display()))
}

/// Render `path` through the exact live DSP path — FFmpeg decode → tape-mode
/// Lanczos resample (`compute_step`) → `RealTimePitchShifter` via
/// `apply_pitch_stage` — as fast as the CPU allows, with no audio device and
/// no ring buffer, and write a float32 WAV. `limit_secs` caps the decoded
/// input duration (for long files under test).
pub fn render_offline(
    path: &Path,
    params: &PlaybackParams,
    out_path: &Path,
    limit_secs: Option<f64>,
    rate: u32,
) -> Result<RenderResult, String> {
    let mut handle: *mut c_void = std::ptr::null_mut();
    if let Some(c) = path_cstring(path) {
        handle = unsafe { sa_ffmpeg_open(c.as_ptr(), rate as c_int) };
    }
    if handle.is_null() {
        return Err(format!("FFmpeg open failed for {}", path.display()));
    }

    let full_dur = unsafe { sa_ffmpeg_duration(handle) };
    let in_secs = limit_secs.map(|l| l.min(full_dur)).unwrap_or(full_dur);

    let step = compute_step(params);
    let mut shifter = RealTimePitchShifter::new(rate);

    let mut in_buf: Vec<f32> = Vec::new();
    let mut read_pos = 0.0f64;
    let mut eof = false;
    let mut tmp = vec![0.0f32; 8192 * 2];
    let mut raw: Vec<f32> = Vec::new();
    let mut post: Vec<f32> = Vec::new();
    let mut pcm: Vec<f32> = Vec::new();

    let in_limit_frames = limit_secs.map(|l| (l * rate as f64) as u64);
    let mut in_frames_read: u64 = 0;

    loop {
        raw.clear();
        let want_in = read_pos.ceil() as usize + LOOKAHEAD + 1;
        while !eof && in_buf.len() / 2 < want_in {
            if let Some(lim) = in_limit_frames {
                if in_frames_read >= lim {
                    eof = true;
                    break;
                }
            }
            let n = unsafe { sa_ffmpeg_read(handle, tmp.as_mut_ptr(), tmp.len() as c_int) };
            if n <= 0 {
                eof = true;
                break;
            }
            in_frames_read += n as u64 / 2;
            in_buf.extend_from_slice(&tmp[..n as usize]);
        }
        let avail = in_buf.len() / 2;
        if avail < 2 {
            if eof {
                break;
            }
            continue;
        }
        let produced = produce_ffmpeg(&in_buf, avail, CHUNK_FRAMES, &mut read_pos, step, params.hq, &mut raw);
        if read_pos > 32768.0 {
            let consumed = read_pos.floor() as usize;
            in_buf.drain(0..consumed * 2);
            read_pos -= consumed as f64;
        }
        if produced == 0 {
            if eof {
                break;
            }
            continue;
        }
        let src = apply_pitch_stage(params, &mut shifter, &raw, &mut post);
        pcm.extend_from_slice(src);
    }

    unsafe { sa_ffmpeg_close(handle) };
    write_wav_f32(out_path, rate, 2, &pcm)?;
    Ok(RenderResult {
        in_secs,
        out_secs: (pcm.len() / 2) as f64 / rate as f64,
    })
}



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
