// Audio engine: transport control + a cpal callback that drains a lock-free
// rtrb ring buffer, fed by a decoder thread.
//
// Two source engines feed the same raw-stereo pipeline:
//   * FFmpeg (universal formats + metadata) via the native `sa_ffmpeg_*` bridge;
//   * libopenmpt 0.8.9 (`crate::tracker`) for tracker modules — MOD/S3M/XM/IT…
//     rendered natively at the device rate with its own event-domain pitch /
//     tempo / separation / interpolation controls.
// Both push raw interleaved-stereo frames into the 8 s source ring; everything
// downstream is shared. MIDI / SoundFont synthesis remains parked.
//
// Pitch / tempo / formant are applied at the audible edge — downstream of the
// 8 s decode ring, inside the stretcher thread — so they work identically on
// every source and parameter changes are audible within the ~85 ms output
// ring, in BOTH modes:
//   * "Tape" mode resamples the raw stream at a variable rate in the
//     stretcher thread (`TapeResampler`) — pitch and tempo move together.
//   * "PitchTime" mode runs the stream through Rubber Band v4.0.0 (R3
//     "finer" engine, real-time mode — spectral pitch shift + time-stretch),
//     so pitch and tempo are independent and parameter changes glide without
//     clicking. Formant preservation is the R3 engine's true formant
//     preservation.
// The ring therefore always holds raw decoded frames: a mode switch never
// re-processes audio the previous mode already shaped.
//
// Three state machines keep playback clean:
//   * The callback owns a cosine-fade machine — 12 ms out on pause/seek, 15 ms
//     in on play/resume — plus an exponential volume smoother.
//   * The decoder thread owns a transport machine — decode/render → (optional)
//     pitch-shift → push, checking its command channel between bounded chunks.
//   * The engine-power monitor owns the device stream state: once the fade
//     reached steady silence and no flush handshake is pending, it stops the
//     WASAPI stream (`Stream::pause` → `IAudioClient::Stop`); resume restarts
//     it before the fade-in. cpal 0.18.2's WASAPI `Stream` is Send + Sync
//     (explicit unsafe impls + compile-time assertions), so the monitor shares
//     it via `Arc` while `Audio` keeps the owning handle.

use std::collections::VecDeque;
use std::ffi::CString;
use std::os::raw::{c_char, c_double, c_int, c_void};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
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

/// Single-owner streaming FFmpeg decoder used by the independent Mixer graph.
/// The main player keeps its own transport/ring state; this is only the codec.
pub(crate) struct FFmpegStream {
    handle: *mut c_void,
}

unsafe impl Send for FFmpegStream {}

impl FFmpegStream {
    pub(crate) fn open(path: &Path, out_rate: u32) -> Result<Self, String> {
        let c = path_cstring(path).ok_or_else(|| format!("invalid path: {}", path.display()))?;
        let handle = unsafe { sa_ffmpeg_open(c.as_ptr(), out_rate as c_int) };
        if handle.is_null() {
            return Err(format!("FFmpeg open failed for {}", path.display()));
        }
        Ok(Self { handle })
    }

    pub(crate) fn duration_secs(&self) -> f64 {
        unsafe { sa_ffmpeg_duration(self.handle) }
    }

    pub(crate) fn seek(&mut self, seconds: f64) -> Result<(), String> {
        if unsafe { sa_ffmpeg_seek(self.handle, seconds.max(0.0)) } == 0 {
            Err("FFmpeg seek failed".into())
        } else {
            Ok(())
        }
    }

    /// Returns interleaved-stereo samples, matching the bridge contract.
    pub(crate) fn read(&mut self, out: &mut [f32]) -> usize {
        unsafe { sa_ffmpeg_read(self.handle, out.as_mut_ptr(), out.len() as c_int) }.max(0) as usize
    }
}

impl Drop for FFmpegStream {
    fn drop(&mut self) {
        unsafe { sa_ffmpeg_close(self.handle) };
    }
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
        sa_ffmpeg_read_handle_metadata(handle, &mut m, cover_buf.as_mut_ptr(), COVER_SIDE as c_int)
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

/// The shared tape/Rubber-Band parameters apply only to FFmpeg audio at the
/// audible edge. `tracker` and `midi` are the NATIVE source-engine parameter
/// sets — applied by the decoder thread to the owning engine only and ignored
/// by the other sources. Native sources bypass the shared post chain entirely,
/// matching the reference player.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlaybackParams {
    /// `true` = Tape mode (pitch & tempo coupled); `false` = PitchTime mode.
    pub mode_tape: bool,
    pub pitch_semitones: f32,
    pub tempo_ratio: f32,
    pub hq: bool,
    pub formant_preserve: bool,
    pub tracker: crate::tracker::TrackerParams,
    pub midi: crate::midi::MidiParams,
}

impl PlaybackParams {
    /// Native tracker/MIDI engines expose their own pitch and tempo controls;
    /// force the shared post stage to raw passthrough regardless of audio-page
    /// state carried by parameter locking.
    pub fn for_native_source(&self) -> Self {
        Self {
            mode_tape: false,
            pitch_semitones: 0.0,
            tempo_ratio: 1.0,
            formant_preserve: false,
            ..*self
        }
    }
}

impl Default for PlaybackParams {
    fn default() -> Self {
        Self {
            mode_tape: false,
            pitch_semitones: 0.0,
            tempo_ratio: 1.0,
            hq: true,
            formant_preserve: false,
            tracker: crate::tracker::TrackerParams::default(),
            midi: crate::midi::MidiParams::default(),
        }
    }
}

enum Cmd {
    Seek(f64),
    Load(PathBuf),
    SetParams(PlaybackParams),
    /// Switch the SoundFont (MIDI sources). Applied live by the decoder
    /// thread — never restarts the track. Non-MIDI engines just record it
    /// as the font for the next MIDI open.
    SetSoundfont(PathBuf),
    SetTrackerMutes(Vec<bool>),
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
    /// Active SoundFont for MIDI sources. Written by the decoder thread
    /// (after a live `set_soundfont` succeeds, or directly when no MIDI
    /// engine is running); read at every MIDI open.
    soundfont: Arc<Mutex<PathBuf>>,
    /// Current tempo from the file's own tempo map, for the Parameters readout.
    midi_native_bpm: Arc<AtomicU32>,
    tracker_channels: Arc<AtomicU64>,
    tracker_vu: Arc<Mutex<Vec<crate::tracker::ChannelVu>>>,
    /// Device-level stream state: true once the engine-power monitor called
    /// `Stream::pause()` after the callback's fade-out drained. Gates the
    /// meter sources (DspTap / tracker VU) so paused displays read zeros.
    engine_stopped: Arc<AtomicBool>,
    /// Callback-published quiescence: steady gain-off, nothing being pulled.
    callback_idle: Arc<AtomicBool>,
    /// Idle-park condvar: transport-visible flips (play/pause/seek/load/
    /// params) notify it so the decoder, stretcher and monitor threads sleep
    /// deeply instead of polling. Payload is a notify generation.
    wake: Arc<(Mutex<u64>, Condvar)>,
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
    /// Set when the engine-power monitor stopped the device stream: the
    /// window no longer refreshes, so snapshots must report silence instead
    /// of letting meters freeze on the last pre-pause audio.
    engine_stopped: Arc<AtomicBool>,
}

impl DspTap {
    fn new(sample_rate: u32) -> Self {
        // Four seconds is enough to cover the BS.1770 3 s short-term window.
        let cap = (sample_rate as usize * 4).max(4096);
        Self {
            cap,
            buf: VecDeque::with_capacity(cap.min(1 << 16)),
            engine_stopped: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Wire the shared engine-stopped state (only the production `start_with`
    /// does; tests keep the fresh, never-stopped flag).
    fn set_engine_flag(&mut self, flag: Arc<AtomicBool>) {
        self.engine_stopped = flag;
    }

    #[inline]
    fn push(&mut self, l: f32, r: f32) {
        if self.buf.len() == self.cap {
            self.buf.pop_front();
        }
        self.buf.push_back((l, r));
    }

    /// Copy the most recent frames (oldest → newest) into `out`, clearing it first.
    /// With the engine stopped no fresh frames arrive, so this publishes an
    /// empty window — the Monitoring consumer falls back to its synthetic
    /// signal instead of freezing on stale audio.
    pub fn snapshot_into(&self, out: &mut Vec<(f32, f32)>) {
        out.clear();
        if self.engine_stopped.load(Ordering::Relaxed) {
            return;
        }
        out.extend(self.buf.iter().copied());
    }
}

// ---------------------------------------------------------------------------
// Public Audio handle
// ---------------------------------------------------------------------------

pub struct Audio {
    _stream: Arc<cpal::Stream>,
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
    /// Audibility anchor for parameter-latency measurements (tests).
    #[allow(dead_code)]
    out_played: Arc<AtomicU64>,
    cmd_tx: mpsc::Sender<Cmd>,
    meta: Arc<Mutex<Option<TrackMetadata>>>,
    current: Arc<Mutex<Option<PathBuf>>>,
    dsp: Arc<Mutex<DspTap>>,
    midi_native_bpm: Arc<AtomicU32>,
    tracker_channels: Arc<AtomicU64>,
    tracker_vu: Arc<Mutex<Vec<crate::tracker::ChannelVu>>>,
    engine_stopped: Arc<AtomicBool>,
    wake: Arc<(Mutex<u64>, Condvar)>,
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
    /// Frames the cpal callback has actually emitted to the device. The
    /// audibility anchor: parameter-latency measurements compare against it.
    #[allow(dead_code)]
    pub fn out_played_frames(&self) -> u64 {
        self.out_played.load(Ordering::Relaxed)
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
        wake_all(&self.wake);
    }
    pub fn pause(&self) {
        self.playing.store(false, Ordering::SeqCst);
        wake_all(&self.wake);
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
        wake_all(&self.wake);
    }
    /// Load any supported source by path. Immediately clears the published
    /// metadata and updates `current` on the caller thread so the very next UI
    /// frame never latches the previous track's cover art.
    pub fn load_file(&self, path: PathBuf) {
        self.transport_gen.fetch_add(1, Ordering::SeqCst);
        *self.meta.lock().unwrap() = None;
        *self.current.lock().unwrap() = Some(path.clone());
        let _ = self.cmd_tx.send(Cmd::Load(path));
        wake_all(&self.wake);
    }
    pub fn set_volume(&self, vol: f32) {
        self.volume
            .store(vol.clamp(0.0, 2.0).to_bits(), Ordering::SeqCst);
    }
    pub fn set_params(&self, params: PlaybackParams) {
        let _ = self.cmd_tx.send(Cmd::SetParams(params));
        wake_all(&self.wake);
    }
    /// Switch the SoundFont. A playing MIDI engine adopts it live (no
    /// restart); otherwise it becomes the font the next MIDI track opens
    /// with. A font the synth rejects is logged and NOT published — the
    /// active font stays until a valid one is chosen.
    pub fn set_soundfont(&self, path: PathBuf) {
        let _ = self.cmd_tx.send(Cmd::SetSoundfont(path));
        wake_all(&self.wake);
    }
    pub fn midi_native_bpm(&self) -> f32 {
        f32::from_bits(self.midi_native_bpm.load(Ordering::Relaxed))
    }
    pub fn tracker_channel_count(&self) -> usize {
        self.tracker_channels.load(Ordering::Relaxed) as usize
    }
    /// Tracker channel meters. While the device stream is stopped no fresh VU
    /// arrives — report silence instead of freezing on the last values.
    pub fn tracker_vu(&self) -> Vec<crate::tracker::ChannelVu> {
        let vu = self.tracker_vu.lock().unwrap().clone();
        if self.engine_stopped.load(Ordering::Relaxed) {
            return vec![crate::tracker::ChannelVu::default(); vu.len()];
        }
        vu
    }
    pub fn set_tracker_mutes(&self, mutes: Vec<bool>) {
        let _ = self.cmd_tx.send(Cmd::SetTrackerMutes(mutes));
        wake_all(&self.wake);
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
/// Maximum source-ring fill allowed for NATIVE synth engines (Openmpt/Midi).
/// Their parameters apply at the engine, upstream of this ring, so every
/// buffered frame is audio a parameter change can no longer reach — without
/// this cap a native control trails by up to RING_SECONDS. FFmpeg is exempt:
/// its decoder reads from disk/SMB and needs the deep backlog.
const NATIVE_HIGH_WATER_SECS: f64 = 0.100;
/// Depth of the post-stretcher output ring: the maximum audio latency between
/// a parameter change and the speakers (Phase B target ≤ 100 ms).
const OUT_RING_SECONDS: f64 = 0.08;
/// Output frames produced per decoder iteration before re-checking commands.
const CHUNK_FRAMES: usize = 8192;
/// Source frames the stretcher pulls per cycle.
const STRETCH_CHUNK: usize = 2048;
/// Input frames to keep buffered past the read head for the interpolators.
const LOOKAHEAD: usize = 4;

pub fn output_device_names() -> Vec<String> {
    let host = cpal::default_host();
    match host.output_devices() {
        Ok(devices) => devices
            .filter_map(|device| device.description().ok().map(|d| d.name().to_string()))
            .collect(),
        Err(e) => {
            eprintln!("[audio] output device enumeration failed: {e}");
            Vec::new()
        }
    }
}

fn output_device(name: Option<&str>) -> Option<cpal::Device> {
    let host = cpal::default_host();
    if let Some(name) = name.filter(|name| !name.is_empty()) {
        if let Ok(devices) = host.output_devices() {
            for device in devices {
                if device
                    .description()
                    .map(|d| d.name() == name)
                    .unwrap_or(false)
                {
                    return Some(device);
                }
            }
        }
        eprintln!("[audio] configured output device '{name}' not found; using system default");
    }
    host.default_output_device()
}

/// Resolve a device + stream config on the device's default rate. The app does
/// not expose higher output rates: a 192 kHz shared-mode stream makes Windows
/// resample in software continuously, even while paused, which costs more CPU
/// than the whole player.
pub(crate) fn output_target(
    device_name: Option<&str>,
) -> Result<(cpal::Device, cpal::SupportedStreamConfig), String> {
    let device = output_device(device_name).ok_or("no output device")?;
    let config = device.default_output_config().map_err(|e| e.to_string())?;
    Ok((device, config))
}

pub fn start(src: Source) -> Result<Audio, String> {
    start_with(src, None)
}

/// Start the output stream on the requested device at its default rate. A
/// missing device falls back to the system default (logged), never a dead app.
pub fn start_with(src: Source, device_name: Option<&str>) -> Result<Audio, String> {
    let (device, config) = output_target(device_name)?;
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
        // Live-stream tests play through the real output device; keep the
        // test harness quiet (10%). Gate frequency detection uses upward
        // zero crossings, which are amplitude-independent, so this does not
        // weaken any real-output measurement.
        #[cfg(test)]
        volume: Arc::new(AtomicU32::new(0.1f32.to_bits())),
        #[cfg(not(test))]
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
        soundfont: Arc::new(Mutex::new(crate::midi::default_soundfont())),
        midi_native_bpm: Arc::new(AtomicU32::new(120.0f32.to_bits())),
        tracker_channels: Arc::new(AtomicU64::new(0)),
        tracker_vu: Arc::new(Mutex::new(Vec::new())),
        engine_stopped: Arc::new(AtomicBool::new(false)),
        callback_idle: Arc::new(AtomicBool::new(false)),
        wake: Arc::new((Mutex::new(0u64), Condvar::new())),
    };
    shared.dsp
        .lock()
        .unwrap()
        .set_engine_flag(shared.engine_stopped.clone());

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
                // Publish quiescence for the engine-power monitor: the fade
                // machine is steady and nothing is being pulled, so the device
                // stream may stop. Republished every buffer — the monitor's
                // wait timeout is the safety net against a missed edge.
                sh.callback_idle.store(
                    matches!(fade, Fade::Steady) && !last_playing,
                    Ordering::Release,
                );
            },
            err_fn,
            None,
        )
        .map_err(|e| e.to_string())?;

    stream.play().map_err(|e| e.to_string())?;

    // Engine-power monitor: shares the stream via Arc (cpal 0.18.2's WASAPI
    // `Stream` is Send + Sync) and stops the device while paused. Ordering
    // contracts: stop only AFTER the callback's fade-out reached steady
    // silence (stopping mid-fade would cut the fade, and a stopped callback
    // could not acknowledge a flush); resume BEFORE the fade-in frames are
    // expected — the callback cannot run while stopped, so play() necessarily
    // precedes them.
    let stream = Arc::new(stream);
    {
        let sh = shared.clone();
        let st = stream.clone();
        thread::Builder::new()
            .name("engine-power".into())
            .spawn(move || engine_power_thread(st, sh))
            .map_err(|e| e.to_string())?;
    }

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
        out_played: shared.out_played.clone(),
        cmd_tx,
        meta: shared.meta.clone(),
        current: shared.current.clone(),
        dsp: shared.dsp.clone(),
        midi_native_bpm: shared.midi_native_bpm.clone(),
        tracker_channels: shared.tracker_channels.clone(),
        tracker_vu: shared.tracker_vu.clone(),
        engine_stopped: shared.engine_stopped.clone(),
        wake: shared.wake.clone(),
    })
}

/// Notify every idle-parked thread (decoder, stretcher, engine-power monitor).
fn wake_all(wake: &(Mutex<u64>, Condvar)) {
    let mut gen = wake.0.lock().unwrap();
    *gen = gen.wrapping_add(1);
    wake.1.notify_all();
}

/// Timed idle wait that a `wake_all` shortens. Drop-in for the idle-poll
/// sleeps: same timeout bound, deep sleep between transport events.
fn idle_wait(sh: &Shared, ms: u64) {
    let guard = sh.wake.0.lock().unwrap();
    let _ = sh.wake.1.wait_timeout(guard, Duration::from_millis(ms));
}

/// Pause decision for [`engine_power_thread`]. Requires callback quiescence
/// (fade drained) and a settled flush handshake: the callback acknowledges
/// `flush_gen`, and a stopped stream cannot run it.
fn engine_wants_stop(sh: &Shared) -> bool {
    !sh.playing.load(Ordering::Relaxed)
        && !sh.flushing.load(Ordering::Acquire)
        && sh.callback_idle.load(Ordering::Acquire)
        && sh.flush_gen.load(Ordering::Acquire) == sh.flush_ack.load(Ordering::Acquire)
}

fn engine_power_thread(stream: Arc<cpal::Stream>, sh: Shared) {
    loop {
        idle_wait(&sh, 50);
        let stopped = sh.engine_stopped.load(Ordering::Acquire);
        let want_stop = engine_wants_stop(&sh);
        if want_stop && !stopped {
            match stream.pause() {
                Ok(()) => sh.engine_stopped.store(true, Ordering::Release),
                Err(e) => eprintln!("[audio] engine stop failed: {e}"),
            }
        } else if !want_stop && stopped {
            // Clear the meter gate before play(): the first frames after the
            // callback restarts are real audio and must reach the tap.
            sh.engine_stopped.store(false, Ordering::Release);
            if let Err(e) = stream.play() {
                eprintln!("[audio] engine start failed: {e}");
                sh.engine_stopped.store(true, Ordering::Release);
            }
        }
    }
}

/// Cosine fade state machine — see the module header.
enum Fade {
    Steady,
    In(f32),
    Out(f32),
}

// ---------------------------------------------------------------------------
// Rubber Band FFI + PitchStage implementation
// ---------------------------------------------------------------------------

/// Minimal bindings over the vendored Rubber Band Library v4.0.0 shim
/// (`vendor/rubberband/shim.cpp`, compiled with the full upstream library
/// into the FFmpeg-bridge static lib by `build.rs`). Buffers are
/// de-interleaved float, lengths are frames on the C side; the stage converts.
/// License: GPL-2.0-or-later or commercial (see the vendor dir).
mod rubberband {
    /// Public `RubberBandStretcher::Options` bits used here.
    pub const OPTION_PROCESS_REAL_TIME: u32 = 0x0000_0001;
    pub const OPTION_CHANNELS_TOGETHER: u32 = 0x1000_0000;
    pub const OPTION_ENGINE_FINER: u32 = 0x2000_0000;

    #[repr(C)]
    pub struct RbHandle {
        _private: [u8; 0],
    }

    extern "C" {
        fn rb_create(sample_rate: u32, channels: u32, options: u32) -> *mut RbHandle;
        fn rb_destroy(handle: *mut RbHandle);
        fn rb_reset(handle: *mut RbHandle);
        fn rb_set_time_ratio(handle: *mut RbHandle, ratio: f64);
        fn rb_set_pitch_scale(handle: *mut RbHandle, scale: f64);
        fn rb_set_formant_preserved(handle: *mut RbHandle, preserved: i32);
        fn rb_get_start_delay(handle: *mut RbHandle) -> usize;
        fn rb_available(handle: *mut RbHandle) -> i32;
        fn rb_process(
            handle: *mut RbHandle,
            in_l: *const f32,
            in_r: *const f32,
            frames: usize,
            final_: i32,
        );
        fn rb_retrieve(
            handle: *mut RbHandle,
            out_l: *mut f32,
            out_r: *mut f32,
            frames: usize,
        ) -> usize;
    }

    /// Owned engine handle, destroyed on drop. `Send` because the raw handle
    /// is only ever touched from the owning thread.
    pub struct Engine {
        handle: *mut RbHandle,
    }

    unsafe impl Send for Engine {}

    impl Engine {
        pub fn new(sample_rate: u32, channels: u32, options: u32) -> Engine {
            let handle = unsafe { rb_create(sample_rate, channels, options) };
            assert!(!handle.is_null(), "rb_create returned null");
            Engine { handle }
        }

        pub fn reset(&mut self) {
            unsafe { rb_reset(self.handle) }
        }

        pub fn set_time_ratio(&mut self, ratio: f64) {
            unsafe { rb_set_time_ratio(self.handle, ratio) }
        }

        pub fn set_pitch_scale(&mut self, scale: f64) {
            unsafe { rb_set_pitch_scale(self.handle, scale) }
        }

        pub fn set_formant_preserved(&mut self, preserved: bool) {
            unsafe { rb_set_formant_preserved(self.handle, preserved as i32) }
        }

        /// Output-domain pipeline delay in frames (RT mode). Depends on the
        /// current ratios; re-query after ratio changes.
        pub fn start_delay(&self) -> usize {
            unsafe { rb_get_start_delay(self.handle) }
        }

        /// Frames retrievable now; -1 once a final flush has fully drained.
        pub fn available(&self) -> i32 {
            unsafe { rb_available(self.handle) }
        }

        /// Feed one de-interleaved stereo block; `final_` marks end of stream
        /// (frames may be 0 then).
        pub fn process(&mut self, in_l: &[f32], in_r: &[f32], final_: bool) {
            debug_assert_eq!(in_l.len(), in_r.len());
            unsafe {
                rb_process(
                    self.handle,
                    in_l.as_ptr(),
                    in_r.as_ptr(),
                    in_l.len(),
                    final_ as i32,
                )
            }
        }

        /// Pull up to `out_l.len()` de-interleaved stereo frames; returns the
        /// frame count actually retrieved.
        pub fn retrieve(&mut self, out_l: &mut [f32], out_r: &mut [f32]) -> usize {
            debug_assert_eq!(out_l.len(), out_r.len());
            unsafe {
                rb_retrieve(
                    self.handle,
                    out_l.as_mut_ptr(),
                    out_r.as_mut_ptr(),
                    out_l.len(),
                )
            }
        }
    }

    impl Drop for Engine {
        fn drop(&mut self) {
            unsafe { rb_destroy(self.handle) }
        }
    }
}

/// Streaming pitch/tempo/formant stage over Rubber Band v4.0.0, R3 "finer"
/// engine in real-time mode with linked stereo processing.
///
/// Tempo drives the library time ratio (tempo 2.0 → time ratio 0.5); pitch is
/// the library pitch scale. Formant preserve maps to `OptionFormantPreserved`
/// — the R3 engine's true formant preservation. Real-time mode deliberately
/// skips the start-pad/output-trim protocol: the library's own RT choice
/// ("better to start with a swoosh than introduce more latency"), so engine
/// activation is instant and ratio changes never gap the output.
struct RubberBandStage {
    engine: rubberband::Engine,
    tempo: f32,
    /// Latch: the final-flush drain must not be repeated until reset.
    drained: bool,
    /// De-interleave/interleave scratch, owned so steady-state process()
    /// cycles don't allocate.
    in_l: Vec<f32>,
    in_r: Vec<f32>,
    out_l: Vec<f32>,
    out_r: Vec<f32>,
}

/// Frames pulled per retrieve cycle at most.
const RB_RETRIEVE: usize = 8192;

impl RubberBandStage {
    fn new(rate: u32) -> Self {
        let options = rubberband::OPTION_PROCESS_REAL_TIME
            | rubberband::OPTION_CHANNELS_TOGETHER
            | rubberband::OPTION_ENGINE_FINER;
        let engine = rubberband::Engine::new(rate, 2, options);
        Self {
            engine,
            tempo: 1.0,
            drained: false,
            in_l: Vec::new(),
            in_r: Vec::new(),
            out_l: vec![0.0; RB_RETRIEVE],
            out_r: vec![0.0; RB_RETRIEVE],
        }
    }

    /// Pull everything currently retrievable, interleaving into `out`.
    fn pull(&mut self, out: &mut Vec<f32>) {
        loop {
            let avail = self.engine.available();
            if avail <= 0 {
                break;
            }
            let want = (avail as usize).min(RB_RETRIEVE);
            let got = self
                .engine
                .retrieve(&mut self.out_l[..want], &mut self.out_r[..want]);
            if got == 0 {
                break;
            }
            let start = out.len();
            out.resize(start + got * 2, 0.0);
            for i in 0..got {
                out[start + i * 2] = self.out_l[i];
                out[start + i * 2 + 1] = self.out_r[i];
            }
        }
    }
}

impl PitchStage for RubberBandStage {
    fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        let frames = input.len() / 2;
        if frames == 0 {
            return;
        }
        self.in_l.clear();
        self.in_r.clear();
        self.in_l.extend(input.chunks_exact(2).map(|f| f[0]));
        self.in_r.extend(input.chunks_exact(2).map(|f| f[1]));
        self.engine.process(&self.in_l, &self.in_r, false);
        self.pull(out);
    }

    fn set_pitch(&mut self, semitones: f32) {
        let s = semitones.clamp(-12.0, 12.0);
        self.engine.set_pitch_scale(2f64.powf(s as f64 / 12.0));
    }

    fn set_tempo(&mut self, ratio: f32) {
        self.tempo = ratio.clamp(0.5, 2.0);
        // tempo 2.0 = play twice as fast = output duration halved = library
        // time ratio 1/2.
        self.engine.set_time_ratio(1.0 / self.tempo as f64);
    }

    fn set_formant(&mut self, preserve: bool) {
        self.engine.set_formant_preserved(preserve);
    }

    fn reset(&mut self) {
        self.engine.reset();
        self.drained = false;
        self.in_l.clear();
        self.in_r.clear();
    }

    fn latency_frames(&self, tempo: f32) -> usize {
        // getStartDelay is output-domain pipeline delay; media frames =
        // output frames x tempo (output frames per media frame is 1/tempo).
        (self.engine.start_delay() as f64 * tempo.max(0.1) as f64).round() as usize
    }

    fn drain(&mut self, out: &mut Vec<f32>) {
        if self.drained {
            return;
        }
        self.drained = true;
        // RT-mode EOF: an empty final block flushes the engine; the remaining
        // output then drains until available() reports completion (-1) or
        // stops yielding.
        self.engine.process(&[], &[], true);
        self.pull(out);
    }
}

// ---------------------------------------------------------------------------
// PitchStage trait (Phase B)
// ---------------------------------------------------------------------------

/// Streaming fractional-index resampler for Tape mode: pitch and tempo are
/// the single coupled ratio `tempo x 2^(pitch/12)` (media frames consumed per
/// output frame). It runs at the audible edge in the stretcher thread — the
/// 8 s source ring always holds raw media frames — so tape speed changes
/// become audible within the output-ring depth, exactly like Pitch/Time
/// changes, instead of draining the ring first.
///
/// Reuses the decoder's historical interpolators (8-tap Lanczos in HQ mode,
/// Catmull-Rom otherwise) so tape output is mathematically identical to the
/// former decode-side resampling.
struct TapeResampler {
    /// Interleaved media frames received but not yet emitted, with the
    /// `LOOKAHEAD` tail the interpolators read past the play head.
    hist: Vec<f32>,
    /// Fractional play head in frames, relative to `hist` start.
    pos: f64,
    pitch_semitones: f32,
    tempo_ratio: f32,
    hq: bool,
}

impl TapeResampler {
    fn new() -> Self {
        Self {
            hist: Vec::new(),
            pos: 0.0,
            pitch_semitones: 0.0,
            tempo_ratio: 1.0,
            hq: true,
        }
    }

    fn step(&self) -> f64 {
        self.tempo_ratio as f64 * 2f64.powf(self.pitch_semitones as f64 / 12.0)
    }
}

impl PitchStage for TapeResampler {
    fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        self.hist.extend_from_slice(input);
        let avail = self.hist.len() / 2;
        // Emit while a full interpolation window is available past the head.
        // `sinc8` reads i0-3 ..= i0+4; `cubic` reads i0-1 ..= i0+2 — LOOKAHEAD
        // (+1 for the fractional part) covers both.
        while (avail as f64 - self.pos) > LOOKAHEAD as f64 + 1.0 {
            let i0 = self.pos.floor() as usize;
            let frac = (self.pos - i0 as f64) as f32;
            let v_l = if self.hq {
                sinc8(&self.hist, i0, frac, 0)
            } else {
                cubic(&self.hist, i0, frac, 0)
            };
            let v_r = if self.hq {
                sinc8(&self.hist, i0, frac, 1)
            } else {
                cubic(&self.hist, i0, frac, 1)
            };
            out.push(v_l);
            out.push(v_r);
            self.pos += self.step();
        }
        let consumed = self.pos.floor() as usize;
        if consumed > 0 {
            self.hist.drain(0..consumed * 2);
            self.pos -= consumed as f64;
        }
    }

    fn set_pitch(&mut self, semitones: f32) {
        self.pitch_semitones = semitones.clamp(-12.0, 12.0);
    }

    fn set_tempo(&mut self, ratio: f32) {
        self.tempo_ratio = ratio.clamp(0.5, 2.0);
    }

    fn set_formant(&mut self, _preserve: bool) {}

    fn reset(&mut self) {
        self.hist.clear();
        self.pos = 0.0;
    }

    /// Media frames held but not yet emitted (interpolation window + any
    /// residue when the output ring throttled consumption).
    fn latency_frames(&self, _tempo: f32) -> usize {
        (self.hist.len() / 2).saturating_sub(self.pos.floor() as usize)
    }

    fn drain(&mut self, out: &mut Vec<f32>) {
        // The interpolatable tail at the current ratio; at most the last
        // LOOKAHEAD+1 frames are unresolvable without future input (~0.1 ms).
        self.process(&[], out);
    }
}

/// Which stage currently owns the audible edge.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum StageKind {
    /// Pitch/Time with neutral parameters: raw passthrough.
    Pass,
    /// Tape mode: coupled resampling at the audible edge.
    Tape,
    /// Pitch/Time with active parameters: Rubber Band R3.
    R3,
}

impl StageKind {
    fn of(params: &PlaybackParams) -> Self {
        let neutral =
            params.pitch_semitones.abs() < 1e-4 && (params.tempo_ratio - 1.0).abs() < 1e-4;
        if params.mode_tape {
            StageKind::Tape
        } else if neutral {
            StageKind::Pass
        } else {
            StageKind::R3
        }
    }
}

/// Mode-dispatching pitch/tempo chain: the single DSP funnel shared by the
/// live stretcher thread and `render_offline`. Entering a stage resets it so
/// no stale spectral/interpolation history bleeds across mode switches, and
/// the raw source ring means a switch never re-processes already-processed
/// audio.
struct PitchChain {
    rb: RubberBandStage,
    tape: TapeResampler,
    kind: StageKind,
}

impl PitchChain {
    fn new(rate: u32) -> Self {
        Self {
            rb: RubberBandStage::new(rate),
            tape: TapeResampler::new(),
            kind: StageKind::Pass,
        }
    }

    /// Feed interleaved-stereo `raw` (media frames); returns the frames to
    /// emit. Bounded-latency contract: everything returned was produced from
    /// the current parameter set.
    fn process<'a>(
        &mut self,
        params: &PlaybackParams,
        raw: &'a [f32],
        post: &'a mut Vec<f32>,
    ) -> &'a [f32] {
        let kind = StageKind::of(params);
        if kind != self.kind {
            match kind {
                StageKind::Tape => self.tape.reset(),
                StageKind::R3 => self.rb.reset(),
                StageKind::Pass => {}
            }
            self.kind = kind;
        }
        match kind {
            StageKind::Pass => raw,
            StageKind::Tape => {
                self.tape.set_pitch(params.pitch_semitones);
                self.tape.set_tempo(params.tempo_ratio);
                self.tape.hq = params.hq;
                post.clear();
                self.tape.process(raw, post);
                post
            }
            StageKind::R3 => {
                self.rb.set_pitch(params.pitch_semitones);
                self.rb.set_tempo(params.tempo_ratio);
                self.rb.set_formant(params.formant_preserve);
                post.clear();
                self.rb.process(raw, post);
                post
            }
        }
    }

    /// Media frames held internally but not yet emitted — position correction.
    fn latency_media_frames(&self, params: &PlaybackParams) -> usize {
        match StageKind::of(params) {
            StageKind::Pass => 0,
            StageKind::Tape => self.tape.latency_frames(params.tempo_ratio),
            StageKind::R3 => self.rb.latency_frames(params.tempo_ratio),
        }
    }

    /// Emit the end-of-stream tail of the active stage (decoder EOF).
    fn drain(&mut self, params: &PlaybackParams, out: &mut Vec<f32>) {
        match StageKind::of(params) {
            StageKind::Pass => {}
            StageKind::Tape => self.tape.drain(out),
            StageKind::R3 => self.rb.drain(out),
        }
    }
}

/// Swappable streaming pitch/tempo/formant engine. The consumer-side stretcher
/// thread drives one implementation; `render_offline` drives the same trait so
/// offline renders exercise the identical DSP. Implementation: Rubber Band
/// v4.0.0, R3 "finer" engine, real-time mode (see `RubberBandStage` above).
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
    /// Emit the end-of-stream tail (decoder EOF). After this, the engine holds
    /// no more pending audio.
    fn drain(&mut self, out: &mut Vec<f32>);
}

// ---------------------------------------------------------------------------
// Decoder / render thread
// ---------------------------------------------------------------------------

enum Engine {
    None,
    Ffmpeg(*mut c_void),
    /// Owned by the decoder thread exclusively (`TrackerEngine: Send`, single
    /// owner handoff). The cpal callback and UI never touch libopenmpt.
    Openmpt(crate::tracker::TrackerEngine),
    /// FluidSynth MIDI engine — same single-owner contract as `Openmpt`
    /// (`MidiEngine` is `!Send` by construction; created inside the decoder
    /// thread at open, never shared).
    Midi(crate::midi::MidiEngine),
}

fn effective_params_for_engine(engine: &Engine, params: PlaybackParams) -> PlaybackParams {
    if matches!(engine, Engine::Openmpt(_) | Engine::Midi(_)) {
        params.for_native_source()
    } else {
        params
    }
}

fn publish_decoder_params(sh: &Shared, engine: &Engine, requested: PlaybackParams) {
    let effective = effective_params_for_engine(engine, requested);
    *sh.params.lock().unwrap() = effective;
    sh.params_gen.fetch_add(1, Ordering::SeqCst);
}

fn close_engine(engine: &mut Engine) {
    if let Engine::Ffmpeg(h) = engine {
        if !h.is_null() {
            unsafe { sa_ffmpeg_close(*h) };
        }
    }
    // Engine::Openmpt drops TrackerEngine → openmpt_module_destroy.
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
    let mut requested_params = *sh.params.lock().unwrap();

    // Source-ring geometry for the native high-water gate. The decoder sees only
    // FREE space, so occupancy needs the total capacity: read it from the ring
    // itself and check it against the allocation constant. A stale constant
    // would silently understate the backlog (the exact bug this gate fixes),
    // so the mismatch is fatal instead.
    let src_ring_frames = producer.buffer().capacity() / out_ch;
    assert_eq!(
        src_ring_frames,
        out_rate as usize * RING_SECONDS,
        "source ring capacity disagrees with RING_SECONDS"
    );
    let native_high_water = (out_rate as f64 * NATIVE_HIGH_WATER_SECS) as usize;

    // FFmpeg state.
    let mut in_buf: Vec<f32> = Vec::new();
    let mut read_pos = 0.0f64;
    let mut eof = false;
    let mut tmp = vec![0.0f32; 8192 * 2];
    // Planar scratch for the MIDI engine (render is de-interleaved).
    let mut midi_l = vec![0.0f32; CHUNK_FRAMES];
    let mut midi_r = vec![0.0f32; CHUNK_FRAMES];

    // Scratch buffers reused across iterations.
    let mut raw: Vec<f32> = Vec::new();
    let mut tracker_vu_buf: Vec<crate::tracker::ChannelVu> = Vec::new();

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
        let mut latest_soundfont: Option<PathBuf> = None;
        let mut latest_mutes: Option<Vec<bool>> = None;
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
                Cmd::SetSoundfont(p) => {
                    latest_soundfont = Some(p);
                }
                Cmd::SetTrackerMutes(mutes) => {
                    latest_mutes = Some(mutes);
                }
            }
        }
        if let Some(p) = latest_params {
            requested_params = p;
            // Keep the requested audio-page values across source switches;
            // only the published effective set is neutralized for native
            // engines. This must not depend on the UI's current-file timing.
            publish_decoder_params(&sh, &engine, requested_params);
            // Native tracker parameters are decoder-side: applied to the
            // libopenmpt module directly. A native tempo change also changes
            // the output-timeline duration, so total_frames follows.
            if let Engine::Openmpt(eng) = &mut engine {
                if eng.params() != p.tracker {
                    if let Err(e) = eng.apply_params(p.tracker) {
                        eprintln!("[audio] native tracker params rejected: {e}");
                    } else if let Some(dur) = eng.duration_secs() {
                        sh.total_frames
                            .store((dur * out_rate as f64) as u64, Ordering::SeqCst);
                    }
                }
            }
            // Native MIDI parameters are decoder-side too — transpose, BPM
            // override and the metronome all apply live (no restart); a BPM
            // change rescales the output timeline, so total_frames follows.
            if let Engine::Midi(eng) = &mut engine {
                let mp = p.midi;
                if eng.transpose() != mp.transpose_semitones {
                    eng.set_transpose(mp.transpose_semitones);
                }
                if eng.bpm_override() != mp.bpm_override {
                    eng.set_bpm(mp.bpm_override);
                }
                if eng.metronome() != mp.metronome {
                    eng.set_metronome(mp.metronome);
                }
                sh.total_frames
                    .store((eng.duration() * out_rate as f64) as u64, Ordering::SeqCst);
            }
        }
        if let Some(p) = latest_soundfont {
            match &mut engine {
                Engine::Midi(eng) => match eng.set_soundfont(&p) {
                    Ok(()) => {
                        *sh.soundfont.lock().unwrap() = p;
                    }
                    Err(e) => {
                        eprintln!(
                            "[audio] SoundFont switch to {} failed ({e}) — keeping {}",
                            p.display(),
                            sh.soundfont.lock().unwrap().display()
                        );
                    }
                },
                _ => {
                    *sh.soundfont.lock().unwrap() = p;
                }
            }
        }
        if let Some(mutes) = latest_mutes {
            if let Engine::Openmpt(eng) = &mut engine {
                for (channel, mute) in mutes.into_iter().enumerate() {
                    if !eng.set_channel_mute(channel, mute) {
                        eprintln!("[audio] tracker channel mute rejected: ch{channel}");
                    }
                }
            }
        }
        if let Some(p) = latest_load {
            let t0 = Instant::now();
            load_path = p.clone();
            load_t0 = Some(t0);
            first_chunk_logged = false;
            in_buf.clear();
            read_pos = 0.0;
            eof = false;
            silence = false;
            tone_hz = None;
            if crate::tracker::is_tracker_file(&p) {
                // libopenmpt reads metadata in-memory at create; publish at open.
                meta_pending = false;
                open_tracker(&mut engine, &p, out_rate, &sh);
            } else if crate::midi::is_midi_file(&p) {
                // FluidSynth parses the whole SMF at create; publish at open.
                meta_pending = false;
                open_midi(&mut engine, &p, out_rate, &sh);
            } else {
                // FFmpeg metadata follows the first decoded chunk.
                meta_pending = true;
                open_ffmpeg(&mut engine, &p, out_rate, &sh);
            }
            publish_decoder_params(&sh, &engine, requested_params);
        }
        if let Some(secs) = latest_seek {
            match &mut engine {
                Engine::Ffmpeg(handle) if !handle.is_null() => {
                    // The stretcher re-bases its position accounting from this
                    // before it drains the rings on the flush below.
                    sh.seek_target
                        .store((secs * out_rate as f64) as u64, Ordering::SeqCst);
                    let flush_target = pre_load(&sh);
                    sh.decoder_eof.store(false, Ordering::Release);
                    sh.track_ended.store(false, Ordering::Release);
                    let ok = unsafe { sa_ffmpeg_seek(*handle, secs) };
                    in_buf.clear();
                    read_pos = 0.0;
                    eof = false;
                    wait_flush_ack(&sh, flush_target);
                    let _ = ok;
                    sh.fade_out.store(false, Ordering::SeqCst);
                    sh.flushing.store(false, Ordering::SeqCst);
                    sh.transport_gen.fetch_add(1, Ordering::SeqCst);
                }
                Engine::Openmpt(eng) => {
                    sh.seek_target
                        .store((secs * out_rate as f64) as u64, Ordering::SeqCst);
                    let flush_target = pre_load(&sh);
                    sh.decoder_eof.store(false, Ordering::Release);
                    sh.track_ended.store(false, Ordering::Release);
                    if let Err(e) = eng.seek(secs) {
                        eprintln!("[audio] tracker seek rejected: {e}");
                    }
                    eof = false;
                    wait_flush_ack(&sh, flush_target);
                    sh.fade_out.store(false, Ordering::SeqCst);
                    sh.flushing.store(false, Ordering::SeqCst);
                    sh.transport_gen.fetch_add(1, Ordering::SeqCst);
                }
                Engine::Midi(eng) => {
                    sh.seek_target
                        .store((secs * out_rate as f64) as u64, Ordering::SeqCst);
                    let flush_target = pre_load(&sh);
                    sh.decoder_eof.store(false, Ordering::Release);
                    sh.track_ended.store(false, Ordering::Release);
                    // Engine clamps to 0..=duration; a seek after EOF restarts
                    // the player internally.
                    eng.seek(secs);
                    eof = false;
                    wait_flush_ack(&sh, flush_target);
                    sh.fade_out.store(false, Ordering::SeqCst);
                    sh.flushing.store(false, Ordering::SeqCst);
                    sh.transport_gen.fetch_add(1, Ordering::SeqCst);
                }
                _ => {}
            }
        }

        if let Some(hz) = tone_hz {
            push_tone(&mut producer, &sh, hz, out_rate, out_ch);
            continue;
        }
        if silence {
            idle_wait(&sh, 5);
            continue;
        }

        if matches!(engine, Engine::None) || !sh.playing.load(Ordering::Relaxed) {
            idle_wait(&sh, 5);
            continue;
        }

        let free_frames = producer.slots() / out_ch;
        if free_frames == 0 {
            idle_wait(&sh, 3);
            continue;
        }
        let mut cap = free_frames.min(CHUNK_FRAMES);

        // Native high-water: `producer.slots()` is FREE space, so occupancy is
        // capacity minus free. Derived from the live ring state each pass, it
        // automatically accounts for audio still buffered across a track
        // open / engine switch whose pre-load flush ack raced the 30 ms
        // deadline: rendering simply waits until the stale backlog drains
        // below the mark (it keeps playing — nothing is dropped).
        if matches!(engine, Engine::Openmpt(_) | Engine::Midi(_)) {
            let occupied = src_ring_frames - free_frames;
            if occupied >= native_high_water {
                idle_wait(&sh, 3);
                continue;
            }
            cap = cap.min(native_high_water - occupied);
        }

        raw.clear();
        let produced: usize;

        match &mut engine {
            Engine::Ffmpeg(handle) => {
                let h = *handle;
                if h.is_null() {
                    idle_wait(&sh, 5);
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
                produced = copy_frames(&in_buf, avail, cap, &mut read_pos, &mut raw);

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
                    idle_wait(&sh, 5);
                    continue;
                }
            }
            Engine::Openmpt(eng) => {
                // Native loop follows the shared loop intent; libopenmpt loops
                // pattern-accurately, so no seam crossfade is needed (that is
                // FFmpeg-encoder-padding surgery only).
                let want_loop = sh.loop_enabled.load(Ordering::Relaxed);
                if eng.looping() != want_loop {
                    if let Err(e) = eng.set_loop(want_loop) {
                        eprintln!("[audio] tracker set_loop({want_loop}) failed: {e}");
                    }
                }
                // EOF with loop enabled after the fact: rewind explicitly — a
                // repeat_count set after the song ended stays ended.
                if eof {
                    if want_loop {
                        sh.decoder_eof.store(false, Ordering::Release);
                        if let Err(e) = eng.seek(0.0) {
                            eprintln!("[audio] tracker loop rewind failed: {e}");
                        }
                        eof = false;
                    } else {
                        sh.decoder_eof.store(true, Ordering::Release);
                        idle_wait(&sh, 5);
                        continue;
                    }
                }
                let frames = cap.min(tmp.len() / 2);
                let n = eng.render(&mut tmp[..frames * 2]);
                if !first_chunk_logged && n > 0 {
                    first_chunk_logged = true;
                    if let Some(t0) = load_t0 {
                        println!(
                            "[audio] first samples rendered in {:.1} ms for {}",
                            t0.elapsed().as_secs_f64() * 1000.0,
                            load_path.display()
                        );
                    }
                }
                if n == 0 {
                    eof = true;
                    produced = 0;
                } else {
                    eng.channel_vu_into(&mut tracker_vu_buf);
                    {
                        let mut published = sh.tracker_vu.lock().unwrap();
                        published.clear();
                        published.extend_from_slice(&tracker_vu_buf);
                    }
                    raw.extend_from_slice(&tmp[..n * 2]);
                    produced = n;
                }
            }
            Engine::Midi(eng) => {
                sh.midi_native_bpm
                    .store((eng.native_bpm() as f32).to_bits(), Ordering::Relaxed);
                // Loop intent mirrors the tracker arm: the player loops inside
                // FluidSynth (tick-accurate), so no seam surgery. Loop enabled
                // after EOF: rewind explicitly — a loop set after the song
                // ended stays ended.
                let want_loop = sh.loop_enabled.load(Ordering::Relaxed);
                if eng.looping() != want_loop {
                    eng.set_loop(want_loop);
                }
                if eng.eof() {
                    if want_loop {
                        sh.decoder_eof.store(false, Ordering::Release);
                        eng.seek(0.0);
                        eof = false;
                    } else {
                        sh.decoder_eof.store(true, Ordering::Release);
                        idle_wait(&sh, 5);
                        continue;
                    }
                }
                let frames = cap.min(midi_l.len());
                let n = eng.render(&mut midi_l[..frames], &mut midi_r[..frames]);
                if !first_chunk_logged && n > 0 {
                    first_chunk_logged = true;
                    if let Some(t0) = load_t0 {
                        println!(
                            "[audio] first samples rendered in {:.1} ms for {}",
                            t0.elapsed().as_secs_f64() * 1000.0,
                            load_path.display()
                        );
                    }
                }
                if n == 0 {
                    // eof latched inside the engine (timeline + tail drained)
                    eof = true;
                    produced = 0;
                } else {
                    for i in 0..n {
                        raw.push(midi_l[i]);
                        raw.push(midi_r[i]);
                    }
                    produced = n;
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
            // Engine::Openmpt publishes its metadata at open (open_tracker) —
            // libopenmpt reads everything in-memory during create.
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
/// Runs on its own thread rather than inside the cpal callback: Rubber Band's
/// R3 engine has non-trivial per-block compute and heap use, while an 80 ms
/// output ring absorbs any scheduling hiccup that would glitch a true RT
/// callback.
fn stretcher_thread(
    mut src: rtrb::Consumer<f32>,
    mut out: rtrb::Producer<f32>,
    out_rate: u32,
    out_ch: usize,
    sh: Shared,
) {
    let mut chain = PitchChain::new(out_rate);
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
                let pending = out_produced.saturating_sub(sh.out_played.load(Ordering::Relaxed));
                eprintln!(
                    "[latency] pitch={:+.1} tempo={:.2}{} applied; {:.0} ms of processed audio buffered ahead",
                    p.pitch_semitones,
                    p.tempo_ratio,
                    if p.mode_tape { " tape" } else { "" },
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
            chain.rb.reset();
            chain.tape.reset();
            chain.kind = StageKind::Pass;
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
                // Ring frames are raw media frames in every mode (tape
                // resamples downstream at the audible edge) — 1:1 accounting.
                media_consumed += frames as f64;
                let src_out = chain.process(&params, &scratch, &mut post);
                carry.extend_from_slice(src_out);
                out_produced += push_frames_fit(&mut out, &mut carry, out_ch) as u64;
            }
        }

        // Input dry: either idle, or end-of-track draining.
        if !did_work && carry.is_empty() {
            if sh.decoder_eof.load(Ordering::Acquire) && src.slots() == 0 && !eof_seen {
                post.clear();
                chain.drain(&params, &mut post);
                carry.extend_from_slice(&post);
                out_produced += push_frames_fit(&mut out, &mut carry, out_ch) as u64;
                if carry.is_empty() {
                    eof_seen = true;
                    sh.stage_eof.store(true, Ordering::Release);
                }
            } else if !sh.decoder_eof.load(Ordering::Acquire) {
                idle_wait(&sh, 3);
            }
        }

        // --- Publish the audible media position. -----------------------------
        // Everything pulled but not yet heard: stage-internal latency (media
        // frames) plus carry + output-ring fill (output frames -> media via
        // the effective output/media rate).
        let played = sh.out_played.load(Ordering::Relaxed);
        let pending = (out_produced.saturating_sub(played) + (carry.len() / 2) as u64) as f64;
        let eff = if params.mode_tape {
            compute_step(&params)
        } else {
            params.tempo_ratio as f64
        };
        let lat_media = chain.latency_media_frames(&params) as f64;
        let pos = base as f64 + media_consumed - lat_media - pending * eff;
        sh.frames_played
            .store(pos.max(base as f64) as u64, Ordering::Release);

        // Anomaly trace (1 Hz): a stage is active, source is available, but
        // nothing is queued ahead — the callback is running on fumes.
        if StageKind::of(&params) != StageKind::Pass
            && pending == 0.0
            && src.slots() > 0
            && sh.playing.load(Ordering::Relaxed)
            && last_trace.elapsed() >= Duration::from_secs(1)
        {
            last_trace = Instant::now();
            eprintln!(
                "[stretch] starving: out_produced={} played={} carry={}f src={}f",
                out_produced,
                played,
                carry.len() / 2,
                src.slots() / out_ch
            );
        }

        if !did_work && !carry.is_empty() {
            thread::sleep(Duration::from_millis(3));
        }
    }
}

/// Copy up to `cap` interleaved-stereo frames from `in_buf` starting at the
/// fractional `read_pos`. All pitch/tempo shaping (tape resampling included)
/// happens downstream at the audible edge, so this is a plain 1:1 advance.
fn copy_frames(
    in_buf: &[f32],
    avail: usize,
    cap: usize,
    read_pos: &mut f64,
    raw: &mut Vec<f32>,
) -> usize {
    let mut produced = 0usize;
    while produced < cap {
        let i0 = read_pos.floor() as usize;
        if i0 + 1 >= avail {
            break;
        }
        raw.push(frame_at(in_buf, i0 as isize, 0));
        raw.push(frame_at(in_buf, i0 as isize, 1));
        *read_pos += 1.0;
        produced += 1;
    }
    produced
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

fn open_ffmpeg(engine: &mut Engine, path: &Path, out_rate: u32, sh: &Shared) {
    close_engine(engine);
    sh.tracker_channels.store(0, Ordering::Release);
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
    *engine = Engine::Ffmpeg(handle);
    post_load(sh, total, flush_target);
}

/// Map libopenmpt's module metadata onto the UI's `TrackMetadata` (the FFmpeg
/// shape — same fields the player window renders, no cover art).
fn tracker_track_metadata(
    path: &Path,
    md: &crate::tracker::TrackerMetadata,
    out_rate: u32,
    duration_secs: Option<f64>,
) -> TrackMetadata {
    let dur = duration_secs.unwrap_or(0.0);
    // libopenmpt reports no bitrate; derive one from file size over duration
    // so the UI's kbps readout is real rather than zero.
    let bitrate = if dur > 0.0 {
        (md.file_size_bytes as f64 * 8.0 / dur / 1000.0).round() as i32
    } else {
        0
    };
    // Honest filename fallback: modules with no title tag show the file stem,
    // never an invented name.
    let title = if md.title.trim().is_empty() {
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Unknown")
            .to_string()
    } else {
        md.title.clone()
    };
    TrackMetadata {
        title,
        artist: md.artist.clone(),
        album: String::new(),
        genre: String::new(),
        codec: md.format.clone(),
        format: if md.container.is_empty() {
            md.format.clone()
        } else {
            md.container.clone()
        },
        codec_long_name: md.format_long.clone(),
        format_long_name: md.tracker.clone(),
        duration: dur,
        bitrate,
        sample_rate: out_rate as i32,
        channels: 2,
        bits_per_sample: 32,
        has_cover_art: false,
        cover_rgba: None,
        file_size_bytes: md.file_size_bytes,
    }
}

/// Open a tracker module on the decoder thread. Native loop state and the
/// pending native parameter set (the locked-across-tracks case — `SetParams`
/// is queued before `Load` and already published to `sh.params`) are applied
/// to the fresh module before any audio flows.
fn open_tracker(engine: &mut Engine, path: &Path, out_rate: u32, sh: &Shared) {
    close_engine(engine);
    sh.tracker_channels.store(0, Ordering::Release);
    sh.seek_target.store(0, Ordering::SeqCst);
    let flush_target = pre_load(sh);

    *sh.current.lock().unwrap() = Some(path.to_path_buf());
    *sh.meta.lock().unwrap() = None;

    match crate::tracker::TrackerEngine::open(path, out_rate) {
        Ok(mut eng) => {
            let params = *sh.params.lock().unwrap();
            if let Err(e) = eng.apply_params(params.tracker) {
                eprintln!("[audio] native tracker params rejected at open: {e}");
            }
            if let Err(e) = eng.set_loop(sh.loop_enabled.load(Ordering::Relaxed)) {
                eprintln!("[audio] tracker set_loop at open failed: {e}");
            }
            sh.tracker_channels
                .store(eng.channel_count() as u64, Ordering::Release);
            sh.tracker_vu.lock().unwrap().clear();
            let meta = tracker_track_metadata(path, eng.metadata(), out_rate, eng.duration_secs());
            // u64::MAX marks a module with no finite end (the tone source uses
            // the same marker); at_track_end then never fires.
            let total = eng
                .duration_secs()
                .map(|d| (d * out_rate as f64) as u64)
                .unwrap_or(u64::MAX);
            *sh.meta.lock().unwrap() = Some(meta);
            *engine = Engine::Openmpt(eng);
            post_load(sh, total, flush_target);
        }
        Err(e) => {
            eprintln!("[audio] libopenmpt open failed for {}: {e}", path.display());
            *engine = Engine::None;
            post_load(sh, 0, flush_target);
        }
    }
}

/// Map the parsed SMF onto the UI's `TrackMetadata`. SMF carries no title/
/// artist tags in a form this app parses, so the title is the honest filename
/// fallback — never an invented name. Duration is the ACTIVE engine timeline
/// (native tempo map, or the override at open).
fn midi_track_metadata(
    path: &Path,
    info: &crate::midi::SmfInfo,
    duration_secs: f64,
    out_rate: u32,
) -> TrackMetadata {
    let file_size_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    // Same derived-bitrate convention as the tracker card: byte rate of the
    // source file over its rendered duration.
    let bitrate = if duration_secs > 0.0 {
        (file_size_bytes as f64 * 8.0 / duration_secs / 1000.0).round() as i32
    } else {
        0
    };
    TrackMetadata {
        title: path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("MIDI")
            .to_string(),
        artist: String::new(),
        album: String::new(),
        genre: String::new(),
        codec: path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("mid")
            .to_lowercase(),
        format: format!("SMF format {}", info.format),
        codec_long_name: format!(
            "Standard MIDI File · {} track{} · {} PPQ",
            info.track_count,
            if info.track_count == 1 { "" } else { "s" },
            info.ppq
        ),
        format_long_name: "FluidSynth SoundFont synthesis".to_string(),
        duration: duration_secs,
        bitrate,
        sample_rate: out_rate as i32,
        channels: 2,
        bits_per_sample: 32,
        has_cover_art: false,
        cover_rgba: None,
        file_size_bytes,
    }
}

/// Open a MIDI file on the decoder thread. The pending native parameter set
/// (the locked-across-tracks case) and the loop intent are applied before any
/// audio flows; the SoundFont comes from `sh.soundfont` (last published).
fn open_midi(engine: &mut Engine, path: &Path, out_rate: u32, sh: &Shared) {
    close_engine(engine);
    sh.tracker_channels.store(0, Ordering::Release);
    sh.seek_target.store(0, Ordering::SeqCst);
    let flush_target = pre_load(sh);

    *sh.current.lock().unwrap() = Some(path.to_path_buf());
    *sh.meta.lock().unwrap() = None;

    let soundfont = sh.soundfont.lock().unwrap().clone();
    match crate::midi::MidiEngine::open(path, out_rate, &soundfont) {
        Ok(mut eng) => {
            let params = *sh.params.lock().unwrap();
            eng.apply_params(params.midi);
            eng.set_loop(sh.loop_enabled.load(Ordering::Relaxed));
            let dur = eng.duration();
            sh.midi_native_bpm
                .store((eng.native_bpm() as f32).to_bits(), Ordering::Relaxed);
            let meta = midi_track_metadata(path, eng.info(), dur, out_rate);
            *sh.meta.lock().unwrap() = Some(meta);
            *engine = Engine::Midi(eng);
            post_load(sh, (dur * out_rate as f64) as u64, flush_target);
        }
        Err(e) => {
            eprintln!(
                "[audio] FluidSynth open failed for {} (font {}): {e}",
                path.display(),
                soundfont.display()
            );
            *engine = Engine::None;
            post_load(sh, 0, flush_target);
        }
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
fn push_tone(producer: &mut rtrb::Producer<f32>, sh: &Shared, _hz: f32, _rate: u32, out_ch: usize) {
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
        idle_wait(sh, 3);
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

/// Raw-stereo chunk source for offline rendering: FFmpeg file decode,
/// native libopenmpt render or native FluidSynth render. One shared
/// processing loop consumes all three, so a rendered tracker or MIDI file
/// passes through the exact shared post chain (tape/Rubber Band) that live
/// playback uses.
enum OfflineDecoder {
    Ffmpeg {
        handle: *mut c_void,
        in_buf: Vec<f32>,
        read_pos: f64,
        eof: bool,
        tmp: Vec<f32>,
        in_frames_read: u64,
        in_limit_frames: Option<u64>,
    },
    Openmpt {
        eng: crate::tracker::TrackerEngine,
        tmp: Vec<f32>,
        limit_secs: Option<f64>,
    },
    Midi {
        eng: crate::midi::MidiEngine,
        left: Vec<f32>,
        right: Vec<f32>,
        /// Rendered-seconds cap (the timeline, not the engine's tail).
        rendered_secs: f64,
        limit_secs: Option<f64>,
    },
}

impl OfflineDecoder {
    /// Fill `raw` with up to `cap` interleaved-stereo frames. Returns the
    /// frame count produced; `0` means the source is exhausted.
    fn next_chunk(&mut self, cap: usize, raw: &mut Vec<f32>) -> usize {
        match self {
            Self::Ffmpeg {
                handle,
                in_buf,
                read_pos,
                eof,
                tmp,
                in_frames_read,
                in_limit_frames,
            } => {
                let h = *handle;
                loop {
                    let want_in = read_pos.ceil() as usize + LOOKAHEAD + 1;
                    while !*eof && in_buf.len() / 2 < want_in {
                        if let Some(lim) = in_limit_frames {
                            if *in_frames_read >= *lim {
                                *eof = true;
                                break;
                            }
                        }
                        let n = unsafe { sa_ffmpeg_read(h, tmp.as_mut_ptr(), tmp.len() as c_int) };
                        if n <= 0 {
                            *eof = true;
                            break;
                        }
                        *in_frames_read += n as u64 / 2;
                        in_buf.extend_from_slice(&tmp[..n as usize]);
                    }
                    let avail = in_buf.len() / 2;
                    if avail < 2 {
                        if *eof {
                            return 0;
                        }
                        continue;
                    }
                    let produced = copy_frames(in_buf, avail, cap, read_pos, raw);
                    if *read_pos > 32768.0 {
                        let consumed = read_pos.floor() as usize;
                        in_buf.drain(0..consumed * 2);
                        *read_pos -= consumed as f64;
                    }
                    if produced == 0 {
                        if *eof {
                            return 0;
                        }
                        continue;
                    }
                    return produced;
                }
            }
            Self::Openmpt {
                eng,
                tmp,
                limit_secs,
            } => {
                if let Some(lim) = limit_secs {
                    if eng.position_secs() >= *lim {
                        return 0;
                    }
                }
                let frames = cap.min(tmp.len() / 2);
                let n = eng.render(&mut tmp[..frames * 2]);
                raw.extend_from_slice(&tmp[..n * 2]);
                n
            }
            Self::Midi {
                eng,
                left,
                right,
                rendered_secs,
                limit_secs,
            } => {
                if let Some(lim) = limit_secs {
                    if *rendered_secs >= *lim {
                        return 0;
                    }
                }
                let frames = cap.min(left.len()).min(right.len());
                let n = eng.render(&mut left[..frames], &mut right[..frames]);
                *rendered_secs += n as f64 / eng.rate() as f64;
                for i in 0..n {
                    raw.push(left[i]);
                    raw.push(right[i]);
                }
                n
            }
        }
    }
}

impl Drop for OfflineDecoder {
    fn drop(&mut self) {
        if let Self::Ffmpeg { handle, .. } = self {
            if !handle.is_null() {
                unsafe { sa_ffmpeg_close(*handle) };
            }
        }
    }
}

/// Render `path` through the exact live DSP path — FFmpeg decode, native
/// libopenmpt render or native FluidSynth render → the `PitchChain` (tape
/// resampling or Rubber Band R3) at the audible edge — as fast as the CPU
/// allows, with no audio device and no ring buffer, and write a float32 WAV.
/// `limit_secs` caps the source duration (for long files under test).
/// Tracker/MIDI files apply only their native parameter set; shared audio
/// post-processing is bypassed. `soundfont` is used for MIDI sources only.
pub fn render_offline(
    path: &Path,
    params: &PlaybackParams,
    out_path: &Path,
    limit_secs: Option<f64>,
    rate: u32,
    soundfont: &Path,
) -> Result<RenderResult, String> {
    let native_source = crate::tracker::is_tracker_file(path) || crate::midi::is_midi_file(path);
    let source_params = if native_source {
        params.for_native_source()
    } else {
        *params
    };
    let params = &source_params;
    let (mut dec, in_secs) = if crate::tracker::is_tracker_file(path) {
        let mut eng = crate::tracker::TrackerEngine::open(path, rate)
            .map_err(|e| format!("libopenmpt open {}: {e}", path.display()))?;
        if let Err(e) = eng.apply_params(params.tracker) {
            return Err(format!("native tracker params: {e}"));
        }
        let full = eng.duration_secs().unwrap_or(0.0);
        (
            OfflineDecoder::Openmpt {
                eng,
                tmp: vec![0.0f32; 8192 * 2],
                limit_secs,
            },
            limit_secs.map(|l| l.min(full)).unwrap_or(full),
        )
    } else if crate::midi::is_midi_file(path) {
        let mut eng = crate::midi::MidiEngine::open(path, rate, soundfont)
            .map_err(|e| format!("FluidSynth open {}: {e}", path.display()))?;
        eng.apply_params(params.midi);
        let full = eng.duration();
        (
            OfflineDecoder::Midi {
                eng,
                left: vec![0.0f32; 8192],
                right: vec![0.0f32; 8192],
                rendered_secs: 0.0,
                limit_secs,
            },
            limit_secs.map(|l| l.min(full)).unwrap_or(full),
        )
    } else {
        let mut handle: *mut c_void = std::ptr::null_mut();
        if let Some(c) = path_cstring(path) {
            handle = unsafe { sa_ffmpeg_open(c.as_ptr(), rate as c_int) };
        }
        if handle.is_null() {
            return Err(format!("FFmpeg open failed for {}", path.display()));
        }
        let full_dur = unsafe { sa_ffmpeg_duration(handle) };
        let in_secs = limit_secs.map(|l| l.min(full_dur)).unwrap_or(full_dur);
        (
            OfflineDecoder::Ffmpeg {
                handle,
                in_buf: Vec::new(),
                read_pos: 0.0,
                eof: false,
                tmp: vec![0.0f32; 8192 * 2],
                in_frames_read: 0,
                in_limit_frames: limit_secs.map(|l| (l * rate as f64) as u64),
            },
            in_secs,
        )
    };

    let mut chain = PitchChain::new(rate);
    let mut raw: Vec<f32> = Vec::new();
    let mut post: Vec<f32> = Vec::new();
    let mut pcm: Vec<f32> = Vec::new();

    loop {
        raw.clear();
        let produced = dec.next_chunk(CHUNK_FRAMES, &mut raw);
        if produced == 0 {
            break;
        }
        let src = chain.process(params, &raw, &mut post);
        pcm.extend_from_slice(src);
    }

    // Active stage: emit the tail so the render covers the full input.
    if StageKind::of(params) != StageKind::Pass {
        post.clear();
        chain.drain(params, &mut post);
        pcm.extend_from_slice(&post);
    }

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
    fn native_source_params_force_shared_post_passthrough() {
        let params = PlaybackParams {
            mode_tape: true,
            pitch_semitones: 12.0,
            tempo_ratio: 2.0,
            formant_preserve: true,
            ..Default::default()
        };
        let native = params.for_native_source();
        assert_eq!(StageKind::of(&native), StageKind::Pass);
        assert_eq!(native.tracker, params.tracker);
        assert_eq!(native.midi, params.midi);
    }

    /// Stage-level quality gate: 3 s of 440 Hz at -9 dBFS at pitch +5
    /// through `RubberBandStage` with formant shifted (the default mode).
    /// Guards the R3 wiring end-to-end: deinterleave, ratios, retrieve,
    /// interleave. True formant preservation legitimately changes a pure
    /// sine's level (it re-aligns the spectral envelope), so the gate pins
    /// the shifted mode where a clean +5 semitone tone must come through.
    #[test]
    fn r3_stage_pitch_quality_gate() {
        let rate = 48_000u32;
        let frames = rate as usize * 3;
        let f0 = 440.0f64;
        let semi = 5.0f64;
        let expect = f0 * 2f64.powf(semi / 12.0);

        let params = PlaybackParams {
            pitch_semitones: 5.0,
            tempo_ratio: 1.0,
            formant_preserve: false,
            ..Default::default()
        };
        // Output-domain pipeline delay for the report (ratio-dependent).
        {
            let mut eng = rubberband::Engine::new(
                rate,
                2,
                rubberband::OPTION_PROCESS_REAL_TIME
                    | rubberband::OPTION_CHANNELS_TOGETHER
                    | rubberband::OPTION_ENGINE_FINER,
            );
            eng.set_time_ratio(1.0);
            eng.set_pitch_scale(2f64.powf(semi / 12.0));
            let d_shift = eng.start_delay();
            eng.set_time_ratio(2.0);
            let d_stretch = eng.start_delay();
            eprintln!(
                "[quality] r3 start delay: {:.1} ms @ pitch+5 tempo1, {:.1} ms @ pitch+5 tempo0.5",
                d_shift as f64 / rate as f64 * 1000.0,
                d_stretch as f64 / rate as f64 * 1000.0,
            );
        }
        let mut stage = RubberBandStage::new(rate);
        let mut mono: Vec<f32> = Vec::new();
        let mut post: Vec<f32> = Vec::new();
        let mut phase = 0.0f64;
        let block = CHUNK_FRAMES;
        let mut fed = 0usize;
        while fed < frames {
            let mut raw = Vec::with_capacity(block * 2);
            for _ in 0..block {
                let v = (phase * 2.0 * std::f64::consts::PI).sin() as f32 * 0.35355;
                phase += f0 / rate as f64;
                if phase >= 1.0 {
                    phase -= 1.0;
                }
                raw.push(v);
                raw.push(v);
            }
            stage.set_pitch(params.pitch_semitones);
            stage.set_tempo(params.tempo_ratio);
            stage.set_formant(params.formant_preserve);
            post.clear();
            stage.process(&raw, &mut post);
            mono.extend(post.chunks_exact(2).map(|f| f[0]));
            fed += block;
        }
        post.clear();
        stage.drain(&mut post);
        mono.extend(post.chunks_exact(2).map(|f| f[0]));
        let n = mono.len();
        let rms =
            (mono.iter().map(|v| (*v as f64) * (*v as f64)).sum::<f64>() / n.max(1) as f64).sqrt();
        let g = |f: f64| -> f64 {
            let mid = &mono[(n / 2).min(n.saturating_sub(rate as usize))..];
            let mid = &mid[..mid.len().min(rate as usize)];
            if mid.is_empty() {
                return 0.0;
            }
            let k = 2.0 * std::f64::consts::PI * f / rate as f64;
            let coeff = 2.0 * k.cos();
            let (mut s1, mut s2) = (0.0f64, 0.0f64);
            for &v in mid {
                let s0 = v as f64 + coeff * s1 - s2;
                s2 = s1;
                s1 = s0;
            }
            ((s1 * s1 + s2 * s2 - coeff * s1 * s2) / (mid.len() as f64).powi(2)).sqrt()
        };
        eprintln!(
            "[quality] r3 shifted: out={n}f rms={:+.1}dBFS g({expect:.0}Hz)={:.4} g({f0:.0}Hz)={:.4}",
            20.0 * rms.log10(),
            g(expect),
            g(f0),
        );
        assert!(
            mono.iter().all(|v| v.is_finite()),
            "stage produced non-finite samples"
        );
        assert!(n > frames, "stage produced no meaningful output");
        let rms_db = 20.0 * rms.log10();
        assert!(rms_db > -16.0, "output level collapsed: {rms_db:.1} dBFS");
        assert!(
            g(expect) > 0.12,
            "shifted tone missing: g({expect:.0}Hz)={:.4}",
            g(expect)
        );
        assert!(
            g(f0) < 0.005,
            "residual at original pitch too high: {:.4}",
            g(f0)
        );
    }

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
    fn pitch_stage_is_neutral_at_unity_and_preserves_length() {
        let mut s = RubberBandStage::new(48_000);
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
        assert!(
            out.iter().all(|v| v.is_finite()),
            "stage produced non-finite samples"
        );
        let produced = out.len() / 2;
        assert!(produced > 0, "stage produced no output while streaming");
        // At unity ratios the stream length must hold within one engine
        // window (RT mode has no pad/trim compensation).
        s.drain(&mut out);
        let total = out.len() / 2;
        assert!(total >= produced, "drain removed audio");
        assert!(
            (total as isize - frames as isize).abs() <= 16_384,
            "unity stream length not preserved: in {frames} out {total}"
        );
        let energy: f32 = out.iter().map(|v| v * v).sum();
        assert!(energy > 0.0);
    }

    #[test]
    fn drain_is_oneshot_until_reset() {
        let mut s = RubberBandStage::new(48_000);
        s.set_pitch(5.0);
        s.set_tempo(1.0);
        let input = vec![0.1f32; 24_000 * 2];
        let mut out = Vec::new();
        for chunk in input.chunks(4096) {
            s.process(chunk, &mut out);
        }
        s.drain(&mut out);
        let after_first = out.len();
        assert!(after_first > 0, "streaming + drain produced no audio");
        // The final-flush drain latches: a second call must not emit more
        // audio (the stretcher thread's eof_seen depends on it).
        s.drain(&mut out);
        assert_eq!(after_first, out.len(), "second drain emitted more audio");
        s.reset();
        let mut again = Vec::new();
        for chunk in input.chunks(4096) {
            s.process(chunk, &mut again);
        }
        s.drain(&mut again);
        assert!(again.len() > 0, "reset did not re-arm the stage");
    }

    #[test]
    fn stretcher_thread_reaches_stage_eof_with_active_engine() {
        // Regression: an engine whose drain() can emit repeatedly must not
        // stall the EOF path — stage_eof has to latch once the tail is out.
        let rate = 48_000u32;
        let sh = Shared {
            playing: Arc::new(AtomicBool::new(true)),
            fade_out: Arc::new(AtomicBool::new(false)),
            flushing: Arc::new(AtomicBool::new(false)),
            volume: Arc::new(AtomicU32::new(1.0f32.to_bits())),
            transport_gen: Arc::new(AtomicU64::new(0)),
            flush_gen: Arc::new(AtomicU64::new(0)),
            flush_ack: Arc::new(AtomicU64::new(0)),
            stage_ack: Arc::new(AtomicU64::new(0)),
            params: Arc::new(Mutex::new(PlaybackParams {
                pitch_semitones: 5.0,
                ..Default::default()
            })),
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
            soundfont: Arc::new(Mutex::new(crate::midi::default_soundfont())),
            midi_native_bpm: Arc::new(AtomicU32::new(120.0f32.to_bits())),
            tracker_channels: Arc::new(AtomicU64::new(0)),
            tracker_vu: Arc::new(Mutex::new(Vec::new())),
            engine_stopped: Arc::new(AtomicBool::new(false)),
            callback_idle: Arc::new(AtomicBool::new(false)),
            wake: Arc::new((Mutex::new(0u64), Condvar::new())),
        };
        let (mut src_prod, src_cons) = RingBuffer::<f32>::new(48_000 * 2);
        let (out_prod, mut out_cons) = RingBuffer::<f32>::new(4096 * 2);
        for _ in 0..12_000 {
            let _ = src_prod.push(0.25);
            let _ = src_prod.push(-0.25);
        }
        drop(src_prod);
        sh.decoder_eof.store(true, Ordering::Release);
        {
            let sh = sh.clone();
            thread::spawn(move || stretcher_thread(src_cons, out_prod, rate, 2, sh));
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while !sh.stage_eof.load(Ordering::Acquire) {
            assert!(
                Instant::now() < deadline,
                "stage_eof never fired — EOF drain did not terminate"
            );
            // Play the role of the cpal callback so the ring keeps draining.
            while out_cons.pop().is_ok() {}
            thread::sleep(Duration::from_millis(2));
        }
        while out_cons.pop().is_ok() {}
    }

    /// Build the same `Shared` bundle `start()` wires up, for live-topology
    /// tests that spawn the real threads.
    fn test_shared(rate: u32) -> Shared {
        Shared {
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
            meta: Arc::new(Mutex::new(None)),
            current: Arc::new(Mutex::new(None)),
            dsp: Arc::new(Mutex::new(DspTap::new(rate))),
            soundfont: Arc::new(Mutex::new(crate::midi::default_soundfont())),
            midi_native_bpm: Arc::new(AtomicU32::new(120.0f32.to_bits())),
            tracker_channels: Arc::new(AtomicU64::new(0)),
            tracker_vu: Arc::new(Mutex::new(Vec::new())),
            engine_stopped: Arc::new(AtomicBool::new(false)),
            callback_idle: Arc::new(AtomicBool::new(false)),
            wake: Arc::new((Mutex::new(0u64), Condvar::new())),
        }
    }

    #[test]
    fn engine_pause_waits_for_fade_and_flush() {
        let sh = test_shared(48_000);
        // Playing: never stop.
        assert!(!engine_wants_stop(&sh));
        // Paused, but the callback has not drained its fade-out yet.
        sh.playing.store(false, Ordering::SeqCst);
        assert!(!engine_wants_stop(&sh));
        sh.callback_idle.store(true, Ordering::Release);
        // Unsettled flush handshake: a stopped stream could not acknowledge it.
        sh.flush_gen.store(1, Ordering::SeqCst);
        assert!(!engine_wants_stop(&sh));
        sh.flush_ack.store(1, Ordering::SeqCst);
        assert!(engine_wants_stop(&sh));
        // An in-flight flush blocks the stop outright (seek while paused).
        sh.flushing.store(true, Ordering::SeqCst);
        assert!(!engine_wants_stop(&sh));
    }

    #[test]
    fn dsp_tap_reports_silence_while_engine_stopped() {
        let mut tap = DspTap::new(48_000);
        tap.push(0.5, -0.5);
        let mut out = Vec::new();
        tap.snapshot_into(&mut out);
        assert_eq!(out, vec![(0.5, -0.5)]);
        let flag = Arc::new(AtomicBool::new(true));
        tap.set_engine_flag(flag);
        tap.snapshot_into(&mut out);
        assert!(out.is_empty(), "stopped engine must not publish stale audio");
    }

    /// Estimate dominant frequency of `win` (mono f32) via upward zero
    /// crossings. For sine-like signals this is robust to the small AM the
    /// R3 engine imposes.
    fn zc_freq(win: &[f32], rate: u32) -> f64 {
        if win.len() < 2 {
            return 0.0;
        }
        let mut up = 0usize;
        for i in 1..win.len() {
            if win[i - 1] <= 0.0 && win[i] > 0.0 {
                up += 1;
            }
        }
        up as f64 * rate as f64 / win.len() as f64
    }

    /// Live-topology param-latency measurement: the real `stretcher_thread`
    /// over the real 8 s source ring + 80 ms output ring, a synthetic decoder
    /// feeding a phase-continuous 440 Hz sine, and a real-time-paced drain
    /// playing the cpal callback's role. The pitch actually leaving the
    /// "speaker" is measured from the waveform — not from any internal
    /// counter. Verifies both the bypass→active activation and an
    /// active→active retune.
    #[test]
    fn pitch_change_audible_latency_live_topology() {
        let rate = 48_000u32;
        let out_ch = 2usize;
        let sh = test_shared(rate);

        let cap = rate as usize * out_ch * RING_SECONDS;
        let (mut src_prod, src_cons) = RingBuffer::<f32>::new(cap);
        let out_cap = ((rate as f64 * OUT_RING_SECONDS) as usize).max(4096) * out_ch;
        let (out_prod, mut out_cons) = RingBuffer::<f32>::new(out_cap);

        // Synthetic decoder: keep the source ring topped with a 440 Hz sine,
        // polling like the real decode thread (3 ms cadence).
        {
            let sh = sh.clone();
            thread::spawn(move || {
                let f0 = 440.0f64;
                let mut phase = 0.0f64;
                loop {
                    let free = src_prod.slots() / out_ch;
                    if free == 0 {
                        thread::sleep(Duration::from_millis(1));
                        continue;
                    }
                    let n = free.min(CHUNK_FRAMES);
                    for _ in 0..n {
                        let v = (phase * 2.0 * std::f64::consts::PI).sin() as f32 * 0.25;
                        phase += f0 / rate as f64;
                        if phase >= 1.0 {
                            phase -= 1.0;
                        }
                        let _ = src_prod.push(v);
                        let _ = src_prod.push(v);
                    }
                    if !sh.playing.load(Ordering::Relaxed) {
                        thread::sleep(Duration::from_millis(3));
                    }
                }
            });
        }
        let sh_stretch = sh.clone();
        thread::spawn(move || stretcher_thread(src_cons, out_prod, rate, out_ch, sh_stretch));

        // Real-time-paced drain = cpal callback. 10 ms ticks, 480 frames.
        let tick = Duration::from_millis(10);
        let frames_per_tick = (rate as usize / 100) * out_ch;
        let t_start = Instant::now();
        let mut next = t_start;
        let mut captured: Vec<f32> = Vec::with_capacity(rate as usize * 8 * out_ch);
        let mut starved_samples: usize = 0;

        let publish = |p: PlaybackParams| {
            *sh.params.lock().unwrap() = p;
            sh.params_gen.fetch_add(1, Ordering::SeqCst);
        };
        let change1_at = Duration::from_secs(2);
        let change2_at = Duration::from_secs(4);
        let stop_at = Duration::from_millis(6_600);
        let mut played_at_change1 = 0usize;
        let mut played_at_change2 = 0usize;
        let mut played_frames = 0usize;

        loop {
            let now = Instant::now();
            let elapsed = now - t_start;
            if elapsed >= change1_at && played_at_change1 == 0 {
                // Simulate the decoder thread's publish path exactly.
                publish(PlaybackParams {
                    pitch_semitones: 5.0,
                    ..Default::default()
                });
                played_at_change1 = played_frames;
            }
            if elapsed >= change2_at && played_at_change2 == 0 {
                publish(PlaybackParams {
                    pitch_semitones: -5.0,
                    ..Default::default()
                });
                played_at_change2 = played_frames;
            }
            if elapsed >= stop_at {
                break;
            }
            next += tick;
            let mut got = 0usize;
            while got < frames_per_tick {
                match out_cons.pop() {
                    Ok(v) => {
                        captured.push(v);
                        got += 1;
                    }
                    Err(_) => {
                        starved_samples += frames_per_tick - got;
                        // The cpal callback would emit zeros here.
                        for _ in 0..(frames_per_tick - got) {
                            captured.push(0.0);
                        }
                        got = frames_per_tick;
                    }
                }
            }
            played_frames += frames_per_tick / out_ch;
            sh.out_played
                .fetch_add((frames_per_tick / out_ch) as u64, Ordering::Relaxed);
            let wait = next.saturating_duration_since(Instant::now());
            if !wait.is_zero() {
                thread::sleep(wait);
            }
        }
        sh.playing.store(false, Ordering::SeqCst);

        // ---- Analyze the audible stream ---------------------------------
        let mono: Vec<f32> = captured.chunks_exact(2).map(|f| f[0]).collect();
        let n = mono.len();
        let win = 3072usize;
        let hop = 256usize;
        let f_low = 440.0f64;
        let f_up = 440.0 * 2f64.powf(5.0 / 12.0); // 587.33
        let f_dn = 440.0 * 2f64.powf(-5.0 / 12.0); // 329.63
        let mut freqs: Vec<(usize, f64)> = Vec::new();
        let mut dropouts = 0usize;
        let mut start = 0usize;
        while start + win <= n {
            let w = &mono[start..start + win];
            let rms =
                (w.iter().map(|v| (*v as f64) * (*v as f64)).sum::<f64>() / win as f64).sqrt();
            if rms < 1e-4 && start > rate as usize {
                dropouts += 1;
            }
            freqs.push((start + win / 2, zc_freq(w, rate)));
            start += hop;
        }
        let mid_up = (f_low + f_up) / 2.0;
        let mid_dn = (f_up + f_dn) / 2.0;
        let transition = |from: usize, above: bool, mid: f64| -> Option<usize> {
            freqs
                .iter()
                .skip_while(|(i, _)| *i < from)
                .find(|(_, f)| if above { *f > mid } else { *f < mid })
                .map(|(i, _)| *i)
        };
        let t1 = transition(played_at_change1, true, mid_up);
        let t2 = transition(played_at_change2, false, mid_dn);
        let ms_of = |opt: Option<usize>, base: usize| -> f64 {
            opt.map(|i| i.saturating_sub(base) as f64 / rate as f64 * 1000.0)
                .unwrap_or(f64::NAN)
        };
        let lat1 = ms_of(t1, played_at_change1);
        let lat2 = ms_of(t2, played_at_change2);
        eprintln!(
            "[latency-test] activation 0->+5: transition at out-frame {:?}, audible latency {:.0} ms",
            t1, lat1
        );
        eprintln!(
            "[latency-test] retune +5->-5:  transition at out-frame {:?}, audible latency {:.0} ms",
            t2, lat2
        );
        eprintln!(
            "[latency-test] starved drain samples: {} ({:.0} ms), dropout windows: {}",
            starved_samples,
            starved_samples as f64 / out_ch as f64 / rate as f64 * 1000.0,
            dropouts
        );
        // Per-50ms frequency trace around each change for glide shape.
        for (i, f) in freqs.iter().step_by(8) {
            let ms = *i as f64 / rate as f64 * 1000.0;
            if (900.0..=2400.0).contains(&ms) || (3100.0..=4600.0).contains(&ms) {
                eprintln!("[latency-test]   {:6.0} ms  {:7.1} Hz", ms, f);
            }
        }
        assert!(
            lat1.is_finite() && lat1 < 400.0,
            "activation latency {lat1:.0} ms exceeds 400 ms budget"
        );
        assert!(
            lat2.is_finite() && lat2 < 400.0,
            "retune latency {lat2:.0} ms exceeds 400 ms budget"
        );
    }

    /// Full real-pipeline latency measurement: the real `start()` engine —
    /// real command channel + coalescing, real FFmpeg decoder thread reading
    /// a WAV from disk, real stretcher thread, real cpal/WASAPI callback —
    /// captured at the speaker via the `DspTap` (post-fader callback output).
    /// The only thing replaced is the human: parameter changes are published
    /// on a schedule, exactly like the Parameters window does.
    #[test]
    fn pitch_change_audible_latency_real_pipeline() {
        let sine = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/audiotest/sine_1k_60s.wav");
        if !sine.exists() {
            panic!(
                "missing test signal {} — run scripts/audiotest/gen_signals.py (60 s variant)",
                sine.display()
            );
        }
        let a = start(Source::Silence).expect("audio device");
        a.set_dsp_enabled(true);
        let tap = a.dsp_tap();
        a.load_file(sine);

        // Wait for real audio to flow (decoder open + first decode + ring fill).
        let deadline = Instant::now() + Duration::from_secs(10);
        while a.out_played_frames() < 48_000 {
            assert!(Instant::now() < deadline, "audio never started");
            thread::sleep(Duration::from_millis(20));
        }

        // Record the tap continuously; `have` = frames already stored,
        // anchored to the callback's own out_played counter.
        let rate = a.rate as u64;
        let mut rec: Vec<(f32, f32)> = Vec::with_capacity((rate * 10) as usize);
        let mut have: u64 = 0;
        let t0 = Instant::now();
        let change1_at = Duration::from_secs(3);
        let change2_at = Duration::from_secs(6);
        let stop_at = Duration::from_millis(8_600);
        let mut played_at_change1: u64 = 0;
        let mut played_at_change2: u64 = 0;

        loop {
            let elapsed = Instant::now() - t0;
            if elapsed >= change1_at && played_at_change1 == 0 {
                a.set_params(PlaybackParams {
                    pitch_semitones: 5.0,
                    ..Default::default()
                });
                played_at_change1 = a.out_played_frames();
            }
            if elapsed >= change2_at && played_at_change2 == 0 {
                a.set_params(PlaybackParams {
                    pitch_semitones: -5.0,
                    ..Default::default()
                });
                played_at_change2 = a.out_played_frames();
            }
            if elapsed >= stop_at {
                break;
            }
            let total = a.out_played_frames();
            let mut snap: Vec<(f32, f32)> = Vec::new();
            tap.lock().unwrap().snapshot_into(&mut snap);
            let fresh = total.saturating_sub(have) as usize;
            if fresh > 0 && !snap.is_empty() {
                let take = fresh.min(snap.len());
                rec.extend_from_slice(&snap[snap.len() - take..]);
                have += take as u64;
            }
            thread::sleep(Duration::from_millis(25));
        }
        let mono: Vec<f32> = rec.iter().map(|&(l, _)| l).collect();
        let n = mono.len();
        assert!(n > 48_000 * 6, "captured only {n} frames of output");

        // Frequency trace + transition detection (same estimator as the
        // topology test). 1 kHz source; +5 st -> 1334.8 Hz; -5 -> 996.1 Hz.
        let win = 3072usize;
        let hop = 256usize;
        let f0 = 1000.0f64;
        let f_up = f0 * 2f64.powf(5.0 / 12.0);
        let f_dn = f0 * 2f64.powf(-5.0 / 12.0);
        let mid_up = (f0 + f_up) / 2.0;
        let mid_dn = (f_up + f_dn) / 2.0;
        let mut freqs: Vec<(usize, f64)> = Vec::new();
        let mut start = 0usize;
        while start + win <= n {
            freqs.push((start + win / 2, zc_freq(&mono[start..start + win], a.rate)));
            start += hop;
        }
        let transition = |from: u64, above: bool, mid: f64| -> Option<usize> {
            freqs
                .iter()
                .skip_while(|(i, _)| (*i as u64) < from)
                .find(|(_, f)| if above { *f > mid } else { *f < mid })
                .map(|(i, _)| *i)
        };
        // `have` counts from engine start; out_played counters start at 0 —
        // the recorder may have missed the first frames, so anchor by offset.
        let missed = a.out_played_frames() - have;
        let _ = missed;
        let lat_ms = |opt: Option<usize>, base: u64| -> f64 {
            opt.map(|i| {
                let frames = i as i64 + missed as i64 - base as i64;
                frames as f64 / a.rate as f64 * 1000.0
            })
            .unwrap_or(f64::NAN)
        };
        let t1 = transition(played_at_change1, true, mid_up);
        let t2 = transition(played_at_change2, false, mid_dn);
        let lat1 = lat_ms(t1, played_at_change1);
        let lat2 = lat_ms(t2, played_at_change2);
        eprintln!(
            "[real-pipeline] activation 0->+5: audible latency {:.0} ms (transition @{:?})",
            lat1, t1
        );
        eprintln!(
            "[real-pipeline] retune +5->-5:  audible latency {:.0} ms (transition @{:?})",
            lat2, t2
        );
        for (i, f) in freqs.iter().step_by(8) {
            let ms = *i as f64 / a.rate as f64 * 1000.0;
            if (2000.0..=5000.0).contains(&ms) || (5500.0..=7600.0).contains(&ms) {
                eprintln!("[real-pipeline]   {:6.0} ms  {:7.1} Hz", ms, f);
            }
        }
        assert!(
            lat1.is_finite() && lat1 < 400.0,
            "real-pipeline activation latency {lat1:.0} ms exceeds 400 ms budget"
        );
        assert!(
            lat2.is_finite() && lat2 < 400.0,
            "real-pipeline retune latency {lat2:.0} ms exceeds 400 ms budget"
        );
    }

    /// Real-pipeline latency under the remaining real-world conditions the
    /// basic test misses: pitch retune with tempo active, retune immediately
    /// after a seek, a continuous UI-style staircase of changes, and the
    /// Tape→PitchTime mode transition (the 8 s source ring may still hold
    /// tape-resampled audio when Pitch/Time engages).
    #[test]
    fn pitch_change_latency_real_pipeline_matrix() {
        let sine = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/audiotest/sine_1k_60s.wav");
        if !sine.exists() {
            panic!("missing test signal {}", sine.display());
        }
        let a = start(Source::Silence).expect("audio device");
        a.set_dsp_enabled(true);
        let tap = a.dsp_tap();
        a.load_file(sine);
        let deadline = Instant::now() + Duration::from_secs(10);
        while a.out_played_frames() < 48_000 {
            assert!(Instant::now() < deadline, "audio never started");
            thread::sleep(Duration::from_millis(20));
        }

        // (wall_elapsed, params, from_hz, to_hz, label)
        let f = |st: f64| 1000.0 * 2f64.powf(st / 12.0);
        let mut events: Vec<(Duration, PlaybackParams, f64, f64, &str)> = vec![
            (
                Duration::from_secs(2),
                PlaybackParams {
                    tempo_ratio: 1.25,
                    pitch_semitones: -5.0,
                    ..Default::default()
                },
                f(0.0),
                f(-5.0),
                "tempo1.25 engage + pitch -5",
            ),
            (
                Duration::from_secs(5),
                PlaybackParams {
                    tempo_ratio: 1.25,
                    pitch_semitones: 5.0,
                    ..Default::default()
                },
                f(-5.0),
                f(5.0),
                "retune at tempo 1.25",
            ),
            (
                Duration::from_secs(9),
                PlaybackParams {
                    tempo_ratio: 1.0,
                    pitch_semitones: -7.0,
                    ..Default::default()
                },
                f(5.0),
                f(-7.0),
                "post-seek retune",
            ),
        ];
        let seek_at = Duration::from_secs(7);
        // UI-style staircase: 8 x +1 st every 250 ms, from -7 up to +1.
        let stair_start = Duration::from_secs(11);
        for k in 0..8i32 {
            events.push((
                stair_start + Duration::from_millis(250 * k as u64),
                PlaybackParams {
                    tempo_ratio: 1.0,
                    pitch_semitones: (k - 6) as f32,
                    ..Default::default()
                },
                f((k - 7) as f64),
                f((k - 6) as f64),
                "staircase step",
            ));
        }
        let stop_at = Duration::from_millis(14_500);

        let mut rec: Vec<(f32, f32)> = Vec::with_capacity(a.rate as usize * 24);
        let mut have: u64 = 0;
        let t0 = Instant::now();
        let mut marks: Vec<(u64, f64, f64, &str)> = Vec::new();
        let mut ev_i = 0usize;
        let mut sought = false;

        loop {
            let elapsed = Instant::now() - t0;
            while ev_i < events.len() && elapsed >= events[ev_i].0 {
                let (_, p, from, to, label) = &events[ev_i];
                a.set_params(*p);
                marks.push((a.out_played_frames(), *from, *to, label));
                ev_i += 1;
            }
            if !sought && elapsed >= seek_at {
                sought = true;
                a.seek(20.0);
            }
            if elapsed >= stop_at {
                break;
            }
            let total = a.out_played_frames();
            let mut snap: Vec<(f32, f32)> = Vec::new();
            tap.lock().unwrap().snapshot_into(&mut snap);
            let fresh = total.saturating_sub(have) as usize;
            if fresh > 0 && !snap.is_empty() {
                let take = fresh.min(snap.len());
                rec.extend_from_slice(&snap[snap.len() - take..]);
                have += take as u64;
            }
            thread::sleep(Duration::from_millis(15));
        }

        let mono: Vec<f32> = rec.iter().map(|&(l, _)| l).collect();
        let n = mono.len();
        assert!(n > 48_000 * 10, "captured only {n} frames");
        let win = 3072usize;
        let hop = 128usize;
        let mut freqs: Vec<(usize, f64)> = Vec::new();
        let mut start = 0usize;
        while start + win <= n {
            freqs.push((start + win / 2, zc_freq(&mono[start..start + win], a.rate)));
            start += hop;
        }
        let missed = a.out_played_frames() - have;
        let mut worst = 0.0f64;
        for (base, from, to, label) in &marks {
            let above = to > from;
            let mid = (from + to) / 2.0;
            let hit = freqs
                .iter()
                .skip_while(|(i, _)| (*i as u64) < *base)
                .find(|(_, fq)| if above { *fq > mid } else { *fq < mid })
                .map(|(i, _)| *i as i64 + missed as i64 - *base as i64);
            let ms = hit
                .map(|fr| fr as f64 / a.rate as f64 * 1000.0)
                .unwrap_or(f64::NAN);
            eprintln!("[matrix] {label:<42} from {from:7.1} to {to:7.1} Hz -> {ms:7.0} ms");
            assert!(
                ms.is_finite() && ms < 500.0,
                "{label}: audible latency {ms:.0} ms exceeds 500 ms budget"
            );
            worst = worst.max(ms);
        }
        eprintln!("[matrix] worst-case audible latency: {worst:.0} ms");
    }

    /// THE regression for the tape-latency bug: with tape resampling applied
    /// by the decoder upstream of the 8 s source ring, a tape pitch change
    /// took seconds to become audible (ring drain; measured 2192 ms in a
    /// partially drained ring, up to ~8 s full) and switching Tape→Pitch/Time
    /// double-shifted the stale tape-processed ring contents for seconds.
    /// With the downstream `TapeResampler` both must respond within the
    /// output-ring budget and a mode switch must land exactly on target.
    #[test]
    fn tape_param_change_latency_real_pipeline() {
        let sine = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/audiotest/sine_1k_60s.wav");
        if !sine.exists() {
            panic!("missing test signal {}", sine.display());
        }
        let a = start(Source::Silence).expect("audio device");
        a.set_dsp_enabled(true);
        let tap = a.dsp_tap();
        a.load_file(sine);
        let deadline = Instant::now() + Duration::from_secs(10);
        while a.out_played_frames() < 48_000 {
            assert!(Instant::now() < deadline, "audio never started");
            thread::sleep(Duration::from_millis(20));
        }

        let f = |st: f64| 1000.0 * 2f64.powf(st / 12.0);
        let tape5 = PlaybackParams {
            mode_tape: true,
            pitch_semitones: 5.0,
            ..Default::default()
        };
        let tape_m5 = PlaybackParams {
            mode_tape: true,
            pitch_semitones: -5.0,
            ..Default::default()
        };
        let pt5 = PlaybackParams {
            mode_tape: false,
            pitch_semitones: 5.0,
            ..Default::default()
        };
        // t=4.5: tape +5 (engages the tape stage at the audible edge).
        // t=7.5: tape +5 -> -5 (a tape retune).
        // t=10.5: Tape -> Pitch/Time at +5 (must land on the same pitch — no
        //         double shift of stale ring audio).
        let t_tape = Duration::from_millis(4_500);
        let t_retune = Duration::from_millis(7_500);
        let t_switch = Duration::from_millis(10_500);
        let stop_at = Duration::from_millis(13_000);

        let mut rec: Vec<(f32, f32)> = Vec::with_capacity(a.rate as usize * 14);
        let mut have: u64 = 0;
        let t0 = Instant::now();
        let mut m_tape = 0u64;
        let mut m_retune = 0u64;
        let mut m_switch = 0u64;
        loop {
            let elapsed = Instant::now() - t0;
            if m_tape == 0 && elapsed >= t_tape {
                a.set_params(tape5);
                m_tape = a.out_played_frames();
            }
            if m_retune == 0 && elapsed >= t_retune {
                a.set_params(tape_m5);
                m_retune = a.out_played_frames();
            }
            if m_switch == 0 && elapsed >= t_switch {
                a.set_params(pt5);
                m_switch = a.out_played_frames();
            }
            if elapsed >= stop_at {
                break;
            }
            let total = a.out_played_frames();
            let mut snap: Vec<(f32, f32)> = Vec::new();
            tap.lock().unwrap().snapshot_into(&mut snap);
            let fresh = total.saturating_sub(have) as usize;
            if fresh > 0 && !snap.is_empty() {
                let take = fresh.min(snap.len());
                rec.extend_from_slice(&snap[snap.len() - take..]);
                have += take as u64;
            }
            thread::sleep(Duration::from_millis(15));
        }

        let mono: Vec<f32> = rec.iter().map(|&(l, _)| l).collect();
        let n = mono.len();
        assert!(n > 48_000 * 10, "captured only {n} frames");
        let win = 3072usize;
        let hop = 128usize;
        let mut freqs: Vec<(usize, f64)> = Vec::new();
        let mut start = 0usize;
        while start + win <= n {
            freqs.push((start + win / 2, zc_freq(&mono[start..start + win], a.rate)));
            start += hop;
        }
        let missed = a.out_played_frames() - have;
        let lat_of = |base: u64, from: f64, to: f64| -> f64 {
            let above = to > from;
            let mid = (from + to) / 2.0;
            freqs
                .iter()
                .skip_while(|(i, _)| (*i as u64) < base)
                .find(|(_, fq)| if above { *fq > mid } else { *fq < mid })
                .map(|(i, _)| {
                    (*i as i64 + missed as i64 - base as i64) as f64 / a.rate as f64 * 1000.0
                })
                .unwrap_or(f64::NAN)
        };
        let lat_tape = lat_of(m_tape, f(0.0), f(5.0));
        let lat_retune = lat_of(m_retune, f(5.0), f(-5.0));
        eprintln!("[tape] engage  0->+5 st: {lat_tape:.0} ms (was 2192 ms + ring drain pre-fix)");
        eprintln!("[tape] retune +5->-5 st: {lat_retune:.0} ms");
        assert!(
            lat_tape.is_finite() && lat_tape < 400.0,
            "tape engage latency {lat_tape:.0} ms exceeds 400 ms — tape is still upstream of the ring"
        );
        assert!(
            lat_retune.is_finite() && lat_retune < 400.0,
            "tape retune latency {lat_retune:.0} ms exceeds 400 ms"
        );
        // Mode switch: after the switch the pitch must sit at f(5) — the
        // stale-ring double shift would burst to f(10), the raw-passthrough
        // bug would sit at 1 kHz.
        let band_lo = f(5.0) * 0.94;
        let band_hi = f(5.0) * 1.06;
        let mut outliers = 0usize;
        let mut checked = 0usize;
        for (i, fq) in &freqs {
            let g = *i as i64 + missed as i64;
            // Window centers fully inside [switch+250 ms, switch+2000 ms].
            if g > m_switch as i64 + (a.rate as i64 / 4) && g < m_switch as i64 + a.rate as i64 * 2
            {
                checked += 1;
                if *fq < band_lo || *fq > band_hi {
                    outliers += 1;
                }
            }
        }
        eprintln!(
            "[tape] tape->pitchtime switch: {outliers}/{checked} windows outside f(5) band [{band_lo:.0},{band_hi:.0}] Hz"
        );
        assert!(
            checked > 20 && outliers == 0,
            "tape->pitchtime switch left the output off the +5 st target ({outliers}/{checked} windows out of band)"
        );
    }

    // ------------------------------------------------------------------
    // Tracker source engine (libopenmpt) — focused wiring tests.
    // Media-dependent tests use the canonical share; each returns with a
    // notice when the share is not mounted so the suite stays runnable
    // anywhere (the same media checks also run device-free via
    // `cargo test -- --ignored` in the tracker module itself).
    // ------------------------------------------------------------------

    fn canonical_tracker_media(name: &str) -> Option<PathBuf> {
        let path = PathBuf::from(r"Y:\Test Files").join(name);
        if path.exists() {
            Some(path)
        } else {
            eprintln!("[tracker-test] {} not mounted — skipping", path.display());
            None
        }
    }

    #[test]
    fn decoder_source_switches_recompute_effective_post_params() {
        let Some(module) = canonical_tracker_media("alpmar-3rd_world.mod") else {
            return;
        };
        let audio = PathBuf::from(r"Y:\Test Files\Murr_01.mp3");
        if !audio.exists() {
            eprintln!("[decoder-test] {} not mounted — skipping", audio.display());
            return;
        }
        let rate = 48_000u32;
        let sh = test_shared(rate);
        let cap = rate as usize * 2 * RING_SECONDS;
        let (producer, mut consumer) = RingBuffer::<f32>::new(cap);
        let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
        let sh_dec = sh.clone();
        thread::spawn(move || decoder_thread(cmd_rx, producer, Source::Silence, rate, 2, sh_dec));
        let requested = PlaybackParams {
            mode_tape: true,
            pitch_semitones: 12.0,
            tempo_ratio: 2.0,
            ..Default::default()
        };
        cmd_tx.send(Cmd::SetParams(requested)).unwrap();
        cmd_tx.send(Cmd::Load(module)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while sh.total_frames.load(Ordering::Relaxed) == 0 {
            assert!(Instant::now() < deadline, "tracker load never completed");
            while consumer.pop().is_ok() {}
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(StageKind::of(&sh.params.lock().unwrap()), StageKind::Pass);

        cmd_tx.send(Cmd::Load(audio)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            while consumer.pop().is_ok() {}
            if *sh.params.lock().unwrap() == requested {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "FFmpeg load did not restore requested post params"
            );
            thread::sleep(Duration::from_millis(5));
        }
        drop(cmd_tx);
    }

    /// The load path routes tracker files to `Engine::Openmpt`, publishes the
    /// mapped metadata, computes the native total, services native SetParams
    /// (tempo rescaling total_frames) and seek (seek_target rebase) — all on
    /// the real decoder thread, no audio device.
    #[test]
    fn decoder_routes_tracker_files_end_to_end() {
        let Some(path) = canonical_tracker_media("alpmar-3rd_world.mod") else {
            return;
        };
        let rate = 48_000u32;
        let sh = test_shared(rate);
        let cap = rate as usize * 2 * RING_SECONDS;
        let (producer, mut consumer) = RingBuffer::<f32>::new(cap);
        let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
        let sh_dec = sh.clone();
        thread::spawn(move || decoder_thread(cmd_rx, producer, Source::Silence, rate, 2, sh_dec));

        cmd_tx.send(Cmd::Load(path.clone())).unwrap();
        // Drain continuously while polling — without a stretcher thread the
        // source ring would fill and the decoder would stop pushing.
        let deadline = Instant::now() + Duration::from_secs(10);
        while sh.total_frames.load(Ordering::Relaxed) == 0 {
            assert!(Instant::now() < deadline, "tracker load never completed");
            while consumer.pop().is_ok() {}
            thread::sleep(Duration::from_millis(10));
        }

        // Metadata fallback: mapped shape, real duration, no cover art.
        let meta = sh.meta.lock().unwrap().clone().expect("tracker metadata");
        assert_eq!(meta.codec, "mod", "codec is the libopenmpt format id");
        assert!(!meta.title.is_empty(), "alpmar carries a title");
        assert!(!meta.cover_rgba.is_some(), "modules have no cover art");
        assert!(
            meta.duration > 200.0 && meta.duration < 240.0,
            "alpmar ≈ 231.5 s, got {}",
            meta.duration
        );

        // Native tempo parameter must rescale total_frames by the ratio.
        let total_1x = sh.total_frames.load(Ordering::Relaxed) as f64;
        cmd_tx
            .send(Cmd::SetParams(PlaybackParams {
                tracker: crate::tracker::TrackerParams {
                    tempo_ratio: 2.0,
                    ..Default::default()
                },
                ..Default::default()
            }))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            while consumer.pop().is_ok() {}
            let total_2x = sh.total_frames.load(Ordering::Relaxed) as f64;
            if (total_2x - total_1x / 2.0).abs() < 4.0 * rate as f64 / 1000.0 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "native tempo did not rescale total_frames ({total_1x} -> {})",
                total_2x
            );
            thread::sleep(Duration::from_millis(10));
        }

        // Seek rebases the stretcher accounting target in output frames.
        cmd_tx.send(Cmd::Seek(60.0)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while sh.seek_target.load(Ordering::Relaxed) != (60.0 * rate as f64) as u64 {
            assert!(
                Instant::now() < deadline,
                "seek(60) never rebased seek_target"
            );
            while consumer.pop().is_ok() {}
            thread::sleep(Duration::from_millis(5));
        }

        // And the engine keeps producing fresh audio after the seek.
        let mut frames = 0usize;
        let deadline = Instant::now() + Duration::from_secs(2);
        while frames < rate as usize {
            assert!(Instant::now() < deadline, "no tracker audio after seek");
            while consumer.pop().is_ok() {
                frames += 1;
            }
            thread::sleep(Duration::from_millis(5));
        }
        drop(cmd_tx);
    }

    /// Offline render of a tracker module: one full pass reproduces the
    /// reported duration; native tempo halves it; shared audio post params
    /// are ignored rather than stacked on the native engine.
    #[test]
    fn render_offline_tracker_native_post_bypass() {
        let Some(path) = canonical_tracker_media("alpmar-3rd_world.mod") else {
            return;
        };
        let out_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target");
        let rate = 48_000u32;

        let neutral = render_offline(
            &path,
            &PlaybackParams::default(),
            &out_dir.join("tracker_neutral.wav"),
            None,
            rate,
            &crate::midi::default_soundfont(),
        )
        .expect("neutral tracker render");
        let reported = neutral.in_secs;
        assert!(
            reported > 200.0 && reported < 240.0,
            "alpmar ≈ 231.5 s, got {reported}"
        );
        assert!(
            (neutral.out_secs - reported).abs() < 0.5,
            "neutral pass must reproduce duration ({:.2} vs {:.2})",
            neutral.out_secs,
            reported
        );

        let native_2x = render_offline(
            &path,
            &PlaybackParams {
                tracker: crate::tracker::TrackerParams {
                    tempo_ratio: 2.0,
                    ..Default::default()
                },
                ..Default::default()
            },
            &out_dir.join("tracker_native2x.wav"),
            None,
            rate,
            &crate::midi::default_soundfont(),
        )
        .expect("native tempo render");
        assert!(
            (native_2x.out_secs - reported / 2.0).abs() < 0.5,
            "native 2x tempo must halve the output ({:.2} vs {:.2})",
            native_2x.out_secs,
            reported / 2.0
        );

        // Shared Tape parameters belong to FFmpeg audio and must not alter a
        // native tracker render, even when carried in by parameter locking.
        let shared_ignored = render_offline(
            &path,
            &PlaybackParams {
                mode_tape: true,
                pitch_semitones: 12.0,
                ..Default::default()
            },
            &out_dir.join("tracker_shared_ignored.wav"),
            None,
            rate,
            &crate::midi::default_soundfont(),
        )
        .expect("tracker render with ignored audio post params");
        assert!(
            (shared_ignored.out_secs - reported).abs() < 0.5,
            "shared audio post params must not alter tracker duration ({:.2} vs {:.2})",
            shared_ignored.out_secs,
            reported
        );
    }

    // ------------------------------------------------------------------
    // MIDI source engine (FluidSynth) — wiring tests, tracker pattern.
    // ------------------------------------------------------------------

    fn canonical_midi_media(name: &str) -> Option<PathBuf> {
        let path = PathBuf::from(r"Y:\Test Files").join(name);
        if path.exists() {
            Some(path)
        } else {
            eprintln!("[midi-test] {} not mounted — skipping", path.display());
            None
        }
    }

    fn reference_soundfont() -> Option<PathBuf> {
        let path = crate::midi::default_soundfont();
        if path.exists() {
            Some(path)
        } else {
            eprintln!(
                "[midi-test] {} missing — skipping FluidSynth tests",
                path.display()
            );
            None
        }
    }

    /// Read a float32 WAV written by `write_wav_f32` and return (frames, peak).
    fn wav_f32_frames_peak(path: &Path) -> (usize, f32) {
        let bytes = std::fs::read(path).expect("render output missing");
        assert_eq!(&bytes[0..4], b"RIFF", "not a WAV");
        let data_len = u32::from_le_bytes([bytes[40], bytes[41], bytes[42], bytes[43]]) as usize;
        let samples: Vec<f32> = bytes[44..44 + data_len]
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        let peak = samples.iter().fold(0.0f32, |a, s| a.max(s.abs()));
        (samples.len() / 2, peak)
    }

    /// The load path routes MIDI files to `Engine::Midi`, publishes the
    /// honest filename-fallback metadata, computes the timeline total,
    /// services native SetParams (BPM override rescales total_frames) and
    /// seek (seek_target rebase) — all on the real decoder thread, no audio
    /// device.
    #[test]
    fn decoder_routes_midi_files_end_to_end() {
        let Some(path) = canonical_midi_media("AERITHO.MID") else {
            return;
        };
        let Some(font) = reference_soundfont() else {
            return;
        };
        let rate = 48_000u32;
        let sh = test_shared(rate);
        *sh.soundfont.lock().unwrap() = font;
        let cap = rate as usize * 2 * RING_SECONDS;
        let (producer, mut consumer) = RingBuffer::<f32>::new(cap);
        let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
        let sh_dec = sh.clone();
        thread::spawn(move || decoder_thread(cmd_rx, producer, Source::Silence, rate, 2, sh_dec));

        cmd_tx.send(Cmd::Load(path.clone())).unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let total = sh.total_frames.load(Ordering::SeqCst) as f64 / rate as f64;
            if total > 30.0 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "MIDI open never published a duration (total={total}s)"
            );
            while consumer.pop().is_ok() {}
            thread::sleep(Duration::from_millis(10));
        }
        // Metadata: honest filename fallback title + SMF facts.
        let meta = sh.meta.lock().unwrap().clone().expect("midi metadata");
        assert_eq!(meta.title, "AERITHO");
        assert_eq!(meta.channels, 2);
        assert_eq!(meta.sample_rate, rate as i32);
        assert!(meta.duration > 30.0);
        assert!(meta.format.starts_with("SMF format"), "{}", meta.format);

        // Native params: BPM override must rescale the timeline total live.
        let native_total = sh.total_frames.load(Ordering::SeqCst);
        cmd_tx
            .send(Cmd::SetParams(PlaybackParams {
                midi: crate::midi::MidiParams {
                    bpm_override: Some(240.0),
                    ..Default::default()
                },
                ..Default::default()
            }))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let now = sh.total_frames.load(Ordering::SeqCst);
            if now != native_total {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "BPM override did not rescale total_frames"
            );
            while consumer.pop().is_ok() {}
            thread::sleep(Duration::from_millis(10));
        }
        while consumer.pop().is_ok() {}

        // Seek rebases the stretcher accounting target in output frames.
        cmd_tx.send(Cmd::Seek(30.0)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while sh.seek_target.load(Ordering::Relaxed) != (30.0 * rate as f64) as u64 {
            assert!(
                Instant::now() < deadline,
                "seek(30) never rebased seek_target"
            );
            while consumer.pop().is_ok() {}
            thread::sleep(Duration::from_millis(5));
        }
        // The engine keeps producing fresh audio after the seek.
        let mut frames = 0usize;
        let deadline = Instant::now() + Duration::from_secs(2);
        while frames < rate as usize {
            assert!(Instant::now() < deadline, "no MIDI audio after seek");
            while consumer.pop().is_ok() {
                frames += 1;
            }
            thread::sleep(Duration::from_millis(5));
        }
        drop(cmd_tx);
    }

    /// Offline MIDI rendering reproduces the native timeline, applies BPM
    /// override in the source engine, and ignores shared audio post params.
    #[test]
    fn render_offline_midi_native_post_bypass() {
        let Some(path) = canonical_midi_media("AERITHO.MID") else {
            return;
        };
        let Some(font) = reference_soundfont() else {
            return;
        };
        let out_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target");
        let rate = 48_000u32;

        let neutral = render_offline(
            &path,
            &PlaybackParams::default(),
            &out_dir.join("midi_neutral.wav"),
            None,
            rate,
            &font,
        )
        .expect("neutral midi render");
        eprintln!(
            "[midi-test] AERITHO native timeline: {:.2}s",
            neutral.in_secs
        );
        assert!(neutral.in_secs > 30.0, "implausible timeline");
        let neutral_tail = neutral.out_secs - neutral.in_secs;
        assert!(
            (-0.05..=5.5).contains(&neutral_tail),
            "neutral render must preserve the SMF timeline plus its reverb tail ({:.2}s tail)",
            neutral_tail
        );
        let (frames, peak) = wav_f32_frames_peak(&out_dir.join("midi_neutral.wav"));
        assert!(peak > 0.01, "neutral render is silent (peak {peak})");
        assert!(frames > rate as usize * 30, "neutral render too short");

        // Native BPM override: the timeline itself shrinks (AERITHO's average
        // tempo is well under 240 BPM).
        let bpm_up = render_offline(
            &path,
            &PlaybackParams {
                midi: crate::midi::MidiParams {
                    bpm_override: Some(240.0),
                    ..Default::default()
                },
                ..Default::default()
            },
            &out_dir.join("midi_bpm240.wav"),
            None,
            rate,
            &font,
        )
        .expect("bpm override render");
        eprintln!(
            "[midi-test] BPM 240 timeline: {:.2}s (was {:.2}s)",
            bpm_up.in_secs, neutral.in_secs
        );
        assert!(
            bpm_up.in_secs < neutral.in_secs * 0.75,
            "BPM override must shorten the timeline ({:.2} vs {:.2})",
            bpm_up.in_secs,
            neutral.in_secs
        );
        let bpm_tail = bpm_up.out_secs - bpm_up.in_secs;
        assert!(
            (-0.05..=5.5).contains(&bpm_tail),
            "BPM render must preserve its own timeline plus its reverb tail ({:.2}s tail)",
            bpm_tail
        );

        // Shared Tape +12 st belongs to FFmpeg audio. A native MIDI render
        // must ignore it and retain the requested source timeline.
        let shared_ignored = render_offline(
            &path,
            &PlaybackParams {
                mode_tape: true,
                pitch_semitones: 12.0,
                ..Default::default()
            },
            &out_dir.join("midi_shared_ignored.wav"),
            Some(10.0),
            rate,
            &font,
        )
        .expect("midi render with ignored audio post params");
        assert!(
            (shared_ignored.out_secs - 10.0).abs() < 0.3,
            "shared audio post params must not alter MIDI speed ({:.2}s for a 10s limit)",
            shared_ignored.out_secs
        );
        let (_, shared_peak) = wav_f32_frames_peak(&out_dir.join("midi_shared_ignored.wav"));
        assert!(
            shared_peak > 0.01,
            "MIDI render with ignored post params is silent"
        );
    }

    /// Live device test: a MIDI track plays through the full real pipeline
    /// (decoder → ring → stretcher → cpal), native transpose/BPM/font changes
    /// never restart it, and seek lands. Requires the default output device,
    /// the reference SoundFont and the canonical share.
    #[test]
    fn midi_live_engine_playback_controls_without_restart() {
        let Some(path) = canonical_midi_media("AERITHO.MID") else {
            return;
        };
        let Some(font) = reference_soundfont() else {
            return;
        };
        let a = start(Source::Silence).expect("audio device");
        a.set_soundfont(font.clone());
        a.load_file(path);

        // Wait for real audio.
        let deadline = Instant::now() + Duration::from_secs(15);
        while a.out_played_frames() < (a.rate as u64 * 3) {
            assert!(Instant::now() < deadline, "MIDI audio never started");
            thread::sleep(Duration::from_millis(20));
        }
        let dur = a.duration_secs();
        assert!(dur > 30.0, "implausible duration {dur}");
        assert_eq!(a.metadata().map(|m| m.title), Some("AERITHO".to_string()));

        // Native transpose: position must keep advancing (no restart).
        let pos_before = a.position_secs();
        thread::sleep(Duration::from_millis(300));
        a.set_params(PlaybackParams {
            midi: crate::midi::MidiParams {
                transpose_semitones: 5,
                ..Default::default()
            },
            ..Default::default()
        });
        thread::sleep(Duration::from_millis(700));
        let pos_after = a.position_secs();
        assert!(
            pos_after > pos_before + 0.5,
            "transpose restarted or stalled playback ({pos_before:.2} -> {pos_after:.2})"
        );
        assert!(a.is_playing(), "transpose must not stop playback");

        // Native BPM override: duration rescales, playback continues.
        let dur_native = a.duration_secs();
        a.set_params(PlaybackParams {
            midi: crate::midi::MidiParams {
                bpm_override: Some(240.0),
                ..Default::default()
            },
            ..Default::default()
        });
        thread::sleep(Duration::from_millis(700));
        let dur_bpm = a.duration_secs();
        assert!(
            (dur_bpm - dur_native).abs() > 1.0,
            "BPM override did not rescale duration ({dur_native} -> {dur_bpm})"
        );
        assert!(a.is_playing(), "BPM override must not stop playback");

        // SoundFont (re)load: live, no restart.
        let pos_before = a.position_secs();
        a.set_soundfont(font);
        thread::sleep(Duration::from_millis(700));
        assert!(
            a.position_secs() > pos_before + 0.5,
            "soundfont switch restarted playback"
        );
        assert!(a.is_playing(), "soundfont switch must not stop playback");

        // Seek lands on the output timeline.
        a.seek(30.0);
        thread::sleep(Duration::from_millis(800));
        let pos = a.position_secs();
        assert!(
            (pos - 30.0).abs() < 2.0,
            "seek(30) did not land ({pos:.2}s)"
        );
        assert!(a.is_playing());

        // Shared post stays optional and neutral by default: the native domain
        // above never leaked into it.
        let e = PlaybackParams::default();
        assert_eq!(e.midi, crate::midi::MidiParams::default());
        assert_eq!(e.tracker, crate::tracker::TrackerParams::default());
        assert_eq!(e.pitch_semitones, 0.0);
        assert_eq!(e.tempo_ratio, 1.0);
    }

    // ------------------------------------------------------------------
    // Native-control latency regressions (tracker + MIDI).
    //
    // Openmpt/Midi parameters apply at the engine, upstream of the 8 s
    // source ring; the ring is therefore the only buffer that can hide a
    // native control behind seconds of stale-parameter audio. The high-water
    // mark caps that at ~100 ms. These tests measure the REAL post-callback
    // output (DspTap, post-fader, exactly what the speakers get) — a native
    // parameter change must be audible within 500 ms. Against the ungated
    // 8 s backlog the tracker transition lands ~5.5 s late and fails.
    // ------------------------------------------------------------------

    /// Generated media dir shared with scripts/audiotest.
    fn gen_media_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/audiotest")
    }

    /// Write a self-contained ProTracker MOD whose audio is one steady tone:
    /// sample 1 = 8-bit sine looping with 8 bytes/cycle, played as a single
    /// held note (period 416 → ~1.07 kHz) on channel 0, sustained by a
    /// 128-order pattern loop. Deterministic media beats a canonical music
    /// module here: the frequency-transition estimator needs a signal with one
    /// dominant fundamental.
    ///
    /// Two MOD traps, both measured against libopenmpt 0.8.9 (each renders the
    /// whole 983 s timeline as digital silence, with a correct duration, so the
    /// failure looks like "the engine plays nothing"):
    ///   1. Cell layout is byte0 = (sample & 0xF0) | (period >> 8),
    ///      byte1 = (period & 0xF0) | (sample & 0x0F). The LOW nibble of byte1
    ///      is the sample number — putting the period's low byte there (0xAC)
    ///      selects sample 12, which does not exist.
    ///   2. A pattern whose cells carry NO effect never advances ticks until a
    ///      speed is set, so no note is ever processed. Row 0 must set speed
    ///      (`F06`), exactly as real modules do.
    fn gen_steady_tone_mod() -> PathBuf {
        let path = gen_media_dir().join("gen_steady_tone.mod");
        let mut b: Vec<u8> = Vec::with_capacity(1084 + 1024 + 8192);
        let mut title = [b' '; 20];
        title[..10].copy_from_slice(b"latencyreg");
        b.extend_from_slice(&title);
        // 31 sample headers; sample 1 loops its full 4096 words at volume 64.
        let mut hdr = [0u8; 30];
        hdr[..22].copy_from_slice(&{
            let mut n = [b' '; 22];
            n[..4].copy_from_slice(b"tone");
            n
        });
        hdr[22..24].copy_from_slice(&4096u16.to_be_bytes());
        hdr[25] = 64;
        hdr[28..30].copy_from_slice(&4096u16.to_be_bytes());
        b.extend_from_slice(&hdr);
        for _ in 1..31 {
            b.extend_from_slice(&[0u8; 30]);
        }
        b.push(128); // song length: pattern 0 repeats all the way
        b.push(0);
        b.extend_from_slice(&[0u8; 128]);
        b.extend_from_slice(b"M.K.");
        let mut pat = [0u8; 1024];
        // Row 0, channel 0: sample 1, period 416 (0x1A0), effect F06 (speed 6).
        pat[0] = 0x01; // (sample & 0xF0) | (period >> 8)
        pat[1] = 0xA1; // (period & 0xF0) | (sample & 0x0F)
        pat[2] = 0x1F; // effect F: set speed
        pat[3] = 0x06;
        b.extend_from_slice(&pat);
        for i in 0..8192usize {
            let v = (120.0 * (std::f64::consts::PI * 2.0 * (i % 8) as f64 / 8.0).sin()) as i8;
            b.push(v as u8);
        }
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &b).unwrap();
        path
    }

    fn gen_steady_tone_midi() -> PathBuf {
        fn vlq(v: u32) -> Vec<u8> {
            let mut out = vec![(v & 0x7f) as u8];
            let mut rest = v >> 7;
            while rest > 0 {
                out.push(0x80 | (rest & 0x7f) as u8);
                rest >>= 7;
            }
            out.reverse();
            out
        }

        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/midiwork/native-tone.mid");
        let mut smf = Vec::new();
        smf.extend_from_slice(b"MThd");
        smf.extend_from_slice(&6u32.to_be_bytes());
        smf.extend_from_slice(&0u16.to_be_bytes());
        smf.extend_from_slice(&1u16.to_be_bytes());
        smf.extend_from_slice(&480u16.to_be_bytes());
        let mut track = Vec::new();
        track.extend(vlq(0));
        track.extend_from_slice(&[0xFF, 0x51, 0x03, 0x07, 0xA1, 0x20]);
        track.extend(vlq(0));
        // GM program 19 (Church Organ) — the reference SoundFont sustains it.
        // The pattern is repeated NOTE-ONS with a single note-off at the end:
        // a retriggered same-key voice holds constant amplitude, so every
        // zero-crossing window stays on the plateau. Measured offline: any
        // note-off/on retrigger cycle decays to silence in this engine, and
        // decaying presets turn the window estimates into noise. The
        // repetition is still the point: the parameter change kills all
        // voices, so audibility waits for the next note-on — exactly the
        // real-world worst case the gate must measure.
        track.extend_from_slice(&[0xC0, 19]);
        for _ in 0..96 {
            track.extend(vlq(240));
            track.extend_from_slice(&[0x90, 60, 100]);
        }
        track.extend(vlq(200));
        track.extend_from_slice(&[0x80, 60, 0]);
        track.extend(vlq(40));
        track.extend_from_slice(&[0xFF, 0x2F, 0x00]);
        smf.extend_from_slice(b"MTrk");
        smf.extend_from_slice(&(track.len() as u32).to_be_bytes());
        smf.extend_from_slice(&track);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &smf).unwrap();
        path
    }

    /// Real-pipeline native-control capture: `media` plays through the full
    /// live engine (real decoder thread + rings + stretcher + cpal callback),
    /// `change` is published once 2.5 s of audio is audible (exactly like the
    /// Parameters window does), the post-callback DspTap records until
    /// `stop_after_change` s. Returns (left channel, rate, out-frames at the
    /// change, frames the recorder missed before its first snapshot).
    fn capture_native_change_latency(
        media: &Path,
        change: PlaybackParams,
        stop_after_change: f64,
    ) -> (Vec<f32>, u32, u64, u64) {
        let a = start(Source::Silence).expect("audio device");
        a.set_dsp_enabled(true);
        let tap = a.dsp_tap();
        a.load_file(media.to_path_buf());

        let deadline = Instant::now() + Duration::from_secs(15);
        while a.out_played_frames() < a.rate as u64 {
            assert!(
                Instant::now() < deadline,
                "audio never started for {}",
                media.display()
            );
            thread::sleep(Duration::from_millis(20));
        }

        let mut rec: Vec<(f32, f32)> = Vec::with_capacity(a.rate as usize * 12);
        let mut have: u64 = 0;
        let t0 = Instant::now();
        let change_at = Duration::from_secs_f64(2.5);
        let stop_at = Duration::from_secs_f64(2.5 + stop_after_change);
        let mut played_at_change: u64 = 0;

        loop {
            let elapsed = Instant::now() - t0;
            if elapsed >= change_at && played_at_change == 0 {
                a.set_params(change);
                played_at_change = a.out_played_frames();
            }
            if elapsed >= stop_at {
                break;
            }
            let total = a.out_played_frames();
            let mut snap: Vec<(f32, f32)> = Vec::new();
            tap.lock().unwrap().snapshot_into(&mut snap);
            let fresh = total.saturating_sub(have) as usize;
            if fresh > 0 && !snap.is_empty() {
                let take = fresh.min(snap.len());
                rec.extend_from_slice(&snap[snap.len() - take..]);
                have += take as u64;
            }
            thread::sleep(Duration::from_millis(25));
        }
        assert!(played_at_change > 0, "parameter change never fired");
        assert!(
            rec.len() > a.rate as usize * 4,
            "captured only {} frames of output",
            rec.len() / 2
        );
        let missed = a.out_played_frames() - have;
        let mono: Vec<f32> = rec.iter().map(|&(l, _)| l).collect();
        (mono, a.rate, played_at_change, missed)
    }

    /// Normalized power of `win` at exactly `freq` (Goertzel). The SoundFont
    /// organ presets carry tremolo/chorus modulation that swings
    /// zero-crossing counts ±30% between windows even at constant pitch, but
    /// the energy at the sounding partial is stable — so the settle check
    /// matches partial energy instead of crossing counts.
    fn tone_power(win: &[f32], rate: u32, freq: f64) -> f64 {
        let n = win.len();
        if n < 16 {
            return 0.0;
        }
        let k = (freq * n as f64 / rate as f64).round();
        if k < 1.0 || k >= n as f64 / 2.0 {
            return 0.0;
        }
        let w = 2.0 * std::f64::consts::PI * k / n as f64;
        let c = w.cos();
        let (mut s1, mut s2) = (0.0f64, 0.0f64);
        for &v in win {
            let s0 = v as f64 + 2.0 * c * s1 - s2;
            s2 = s1;
            s1 = s0;
        }
        (s2 * s2 + s1 * s1 - 2.0 * c * s1 * s2) / (n as f64 * n as f64)
    }

    /// Measure when the dominant frequency of the captured output first
    /// leaves the pre-change plateau towards `f_pre * 2^(semitones/12)`,
    /// and whether it settles there. `anchor`/`missed` map rec indices to
    /// the callback's out_played counter. Returns (latency_ms, settled).
    fn native_change_latency_ms(
        mono: &[f32],
        rate: u32,
        anchor: u64,
        missed: u64,
        semitones: f64,
        f0_grid: Option<f64>,
        tag: &str,
    ) -> (f64, bool) {
        let win = 3072usize;
        let hop = 256usize;
        let n = mono.len();
        let mut freqs: Vec<(usize, f64, f64)> = Vec::new();
        let mut start = 0usize;
        while start + win <= n {
            let w = &mono[start..start + win];
            let rms =
                (w.iter().map(|v| *v as f64 * *v as f64).sum::<f64>() / w.len() as f64).sqrt();
            freqs.push((start + win / 2, zc_freq(w, rate), rms));
            start += hop;
        }
        let anchor_i = anchor.saturating_sub(missed) as usize;
        let pre: Vec<f64> = freqs
            .iter()
            .filter(|(i, ..)| {
                *i + win / 2 < anchor_i && *i > anchor_i.saturating_sub(rate as usize)
            })
            .map(|(_, f, _)| *f)
            .collect();
        assert!(pre.len() > 8, "no pre-change plateau windows");
        let mut sorted = pre.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let f_pre = sorted[sorted.len() / 2];
        let f_target = f_pre * 2f64.powf(semitones / 12.0);
        let mid = (f_pre + f_target) / 2.0;
        let transition = freqs
            .iter()
            .skip_while(|(i, ..)| *i < anchor_i)
            .find(|(_, f, _)| *f > mid)
            .map(|(i, ..)| *i);
        let lat = transition
            .map(|i| i.saturating_sub(anchor_i) as f64 / rate as f64 * 1000.0)
            .unwrap_or(f64::NAN);
        // Settled, modulated presets (MIDI SoundFont organs): in the final
        // 0.5 s the energy AT the target partial must dominate the energy at
        // the corresponding pre-change partial in most windows. Tremolo/
        // chorus modulation swings zero-crossing medians ±30% at constant
        // pitch, so crossing counts decide the (coarse) latency but not the
        // plateau; the probe is snapped to `f0_grid` (key 60, C4, equal
        // temperament) so crossing quantization cannot shift it off the
        // partial. Steady synthetic tones (tracker): the zc tail median sits
        // on the target within `tolerance`.
        let tail_start = n.saturating_sub(rate as usize / 2 + win);
        let settled = if let Some(f0) = f0_grid {
            // Probe the FUNDAMENTAL, not a zc-derived multiple: zc counts
            // crossings of the summed waveform (non-integer "k"), and any
            // higher partial sits in the chorus skirt of the pre-change
            // tone. 1.5× the old fundamental is a non-integer multiple, so
            // the new probe frequency carries no direct pre-change energy.
            let p_old_freq = f0;
            let p_new_freq = f0 * 2f64.powf(semitones / 12.0);
            let med = |mut v: Vec<f64>| -> f64 {
                v.sort_by(|a, b| a.partial_cmp(b).unwrap());
                v.get(v.len() / 2).copied().unwrap_or(f64::NAN)
            };
            let mut pre_old = Vec::new();
            let mut pre_new = Vec::new();
            let mut s = 0usize;
            while s + win <= n {
                let c = s + win / 2;
                if c + win / 2 < anchor_i && c > anchor_i.saturating_sub(rate as usize) {
                    let w = &mono[s..s + win];
                    pre_old.push(tone_power(w, rate, p_old_freq));
                    pre_new.push(tone_power(w, rate, p_new_freq));
                }
                s += hop;
            }
            let mut tail_old = Vec::new();
            let mut tail_new = Vec::new();
            let mut s = tail_start.saturating_sub(win / 2);
            while s + win <= n {
                let w = &mono[s..s + win];
                tail_old.push(tone_power(w, rate, p_old_freq));
                tail_new.push(tone_power(w, rate, p_new_freq));
                s += hop;
            }
            let (m_po, m_pn) = (med(pre_old), med(pre_new));
            let (m_to, m_tn) = (med(tail_old), med(tail_new));
            // Target present: tail energy at the new partial clearly above
            // its pre-change (leakage) level. Old partial gone: tail energy
            // well below the pre-change tone level. Medians ride out the
            // preset's tremolo nulls.
            let ok = m_tn > 4.0 * m_pn && m_to < 0.25 * m_po;
            eprintln!(
                "[{tag}] partial power old {:.3e}->{:.3e}, new {:.3e}->{:.3e} (pre->tail)",
                m_po, m_to, m_pn, m_tn
            );
            ok
        } else {
            const TOLERANCE: f64 = 0.08;
            let mut tail: Vec<f64> = freqs
                .iter()
                .filter(|(i, ..)| *i >= tail_start)
                .map(|(_, f, _)| *f)
                .collect();
            tail.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let f_tail = if tail.is_empty() {
                f64::NAN
            } else {
                tail[tail.len() / 2]
            };
            (f_tail - f_target).abs() <= f_target * TOLERANCE
        };
        eprintln!(
            "[{tag}] plateau {:.0} Hz -> target {:.0} Hz; audible latency {:.0} ms",
            f_pre, f_target, lat
        );
        for (i, f, rms) in freqs.iter().step_by(8) {
            let ms = (*i as f64 - anchor_i as f64) / rate as f64 * 1000.0;
            if (-500.0..=1000.0).contains(&ms) {
                eprintln!("[{tag}]   change{:+6.0} ms  {:7.1} Hz  rms {rms:.5}", ms, f);
            }
        }
        (lat, settled)
    }

    /// Device-free proof of the high-water derivation itself: a native
    /// tracker engine with NO consumer must stop filling the source ring on
    /// its own at the ~100 ms mark (not at 8 s), and must resume after the
    /// ring drains. Catches occupancy/free-space confusion directly.
    #[test]
    fn native_source_ring_respects_high_water() {
        let path = gen_steady_tone_mod();
        let rate = 48_000u32;
        let sh = test_shared(rate);
        let cap = rate as usize * 2 * RING_SECONDS;
        let (producer, mut consumer) = RingBuffer::<f32>::new(cap);
        let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
        let sh_dec = sh.clone();
        thread::spawn(move || decoder_thread(cmd_rx, producer, Source::Silence, rate, 2, sh_dec));

        cmd_tx.send(Cmd::Load(path)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while consumer.slots() == 0 {
            assert!(Instant::now() < deadline, "tracker never produced audio");
            thread::sleep(Duration::from_millis(5));
        }
        // Give the producer ample time to blow past the mark if ungated.
        thread::sleep(Duration::from_millis(400));
        let occupied = consumer.slots() / 2;
        let mark = (rate as f64 * NATIVE_HIGH_WATER_SECS) as usize;
        eprintln!("[high-water] ring settled at {occupied} frames (mark {mark})");
        assert!(
            occupied <= mark + rate as usize / 10,
            "native source ring holds {occupied} frames ({:.0} ms) — high-water {} frames not enforced",
            occupied as f64 / rate as f64 * 1000.0,
            mark
        );
        assert!(
            occupied >= mark / 2,
            "ring under-filled ({occupied} frames) — gate closed too early"
        );

        // Drain fully, then the gate must reopen and keep producing.
        while consumer.pop().is_ok() {}
        thread::sleep(Duration::from_millis(200));
        let refilled = consumer.slots() / 2;
        assert!(
            refilled >= mark / 2,
            "ring did not refill after drain ({refilled} frames) — playback would stall"
        );
        drop(cmd_tx);
    }

    /// REAL output regression: native MIDI transpose must reach captured
    /// post-callback audio within 500 ms, including the intentional voice
    /// silence at the parameter boundary and the wait for the next note-on.
    #[test]
    fn native_midi_param_audible_latency_real_pipeline() {
        let path = gen_steady_tone_midi();
        let (mono, rate, anchor, missed) = capture_native_change_latency(
            &path,
            PlaybackParams {
                midi: crate::midi::MidiParams {
                    transpose_semitones: 7,
                    ..Default::default()
                },
                ..Default::default()
            },
            8.0,
        );
        let (lat, settled) = native_change_latency_ms(
            &mono,
            rate,
            anchor,
            missed,
            7.0,
            Some(261.625_565_3),
            "midi-native",
        );
        assert!(
            lat.is_finite() && lat < 500.0,
            "native MIDI param change audible after {lat:.0} ms (budget 500 ms)"
        );
        assert!(settled, "output did not settle on the +7 st plateau");
    }

    /// REAL output regression: a native tracker pitch change must be audible
    /// in the captured post-callback audio within 500 ms (target 100–200 ms).
    /// Against the ungated 8 s source ring the same change trails by the
    /// full buffered backlog (~5.5 s here) and this test fails.
    #[test]
    fn native_tracker_param_audible_latency_real_pipeline() {
        let path = gen_steady_tone_mod();
        let (mono, rate, anchor, missed) = capture_native_change_latency(
            &path,
            PlaybackParams {
                tracker: crate::tracker::TrackerParams {
                    pitch_semitones: 7.0,
                    ..Default::default()
                },
                ..Default::default()
            },
            8.0,
        );
        // Uninterrupted playback: the steady tone never drops out.
        let win = 3072usize;
        let mut dropouts: Vec<usize> = Vec::new();
        let mut start = rate as usize; // skip the fade-in region
        while start + win <= mono.len() {
            let w = &mono[start..start + win];
            let rms =
                (w.iter().map(|v| (*v as f64) * (*v as f64)).sum::<f64>() / win as f64).sqrt();
            if rms < 1e-4 {
                dropouts.push(start);
            }
            start += win;
        }
        eprintln!(
            "[tracker-native] {} dropout windows at {:?} (ms from change: {:?})",
            dropouts.len(),
            dropouts.iter().take(6).collect::<Vec<_>>(),
            dropouts
                .iter()
                .take(6)
                .map(|d| *d as f64 / rate as f64 * 1000.0 - anchor as f64 / rate as f64 * 1000.0)
                .collect::<Vec<_>>()
        );
        assert!(
            dropouts.is_empty(),
            "{} silent windows — playback interrupted",
            dropouts.len()
        );

        let (lat, settled) =
            native_change_latency_ms(&mono, rate, anchor, missed, 7.0, None, "tracker-native");
        assert!(
            lat.is_finite() && lat < 500.0,
            "native tracker param change audible after {lat:.0} ms (budget 500 ms; \
             ungated 8 s ring would read ~5500 ms)"
        );
        assert!(settled, "output did not settle on the +7 st plateau");
    }
}
