//! Tracker (module) source engine — a thin, safe owner around the vendored
//! libopenmpt 0.8.9 C API (`vendor/libopenmpt`, BSD-3-Clause, compiled and
//! linked by `build.rs`).
//!
//! # Ownership / threading
//!
//! One [`TrackerEngine`] owns exactly one `openmpt_module`. libopenmpt module
//! objects are safe to use from one thread at a time and never concurrently;
//! every call here goes through `&mut self` (or `&self` for reads) and the raw
//! handle never leaves the struct. `Send` is implemented so the engine can be
//! built on one thread and moved once to the decoder thread — the pattern
//! `audio.rs` already uses for its FFmpeg and Rubber Band handles. `Sync` is
//! deliberately *not* implemented.
//!
//! # Why native controls
//!
//! libopenmpt renders module music from events: pitch and tempo are mixer
//! domain parameters (`play.pitch_factor`, `play.tempo_factor`) applied
//! *before* the mixer, exactly like the tracker does, and independent of each
//! other. A tracker source therefore must not also be run through a
//! time/pitch stretcher — that double-applies the shift. Whether this module's
//! output enters any post chain is the caller's decision; nothing here forbids
//! it, but the neutral configuration is a direct path to the output ring.
//!
//! # Timeline
//!
//! libopenmpt's position and seek APIs use the current scaled playback clock,
//! including `play.tempo_factor`. This engine keeps its own output-timeline
//! position as well: `played_secs` counts rendered frames since open/last
//! seek, and duration is the module's default-clock duration divided by the
//! current tempo ratio.
//!
//! # End of stream
//!
//! `play.at_end` is set to `stop`, so the mixer ends without libopenmpt's own
//! fade-out and [`TrackerEngine::render`] returns `0` exactly once per pass.
//! With looping enabled the repeat count is infinite and the mixer restarts
//! inside the library, so `render` never returns `0`. Seeking clears the
//! library's song-end latch, so a seek after the end resumes playback.

// The engine API here is complete by design and exercised by this module's
// own test suite (including `--ignored` live tests); the audio integration
// uses a subset (open/render/seek/params/loop/metadata). `dead_code` would
// otherwise flag the introspection half on non-test builds.
#![allow(dead_code)]

use core::ffi::{c_char, c_double, c_int, c_void};
use std::ffi::{CStr, CString};
use std::fmt;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// C API surface (subset of libopenmpt.h actually used)
// ---------------------------------------------------------------------------

/// Opaque `openmpt_module`. Never dereferenced on the Rust side.
#[repr(C)]
struct Module {
    _private: [u8; 0],
}

type LogFunc = extern "C" fn(message: *const c_char, user: *mut c_void);
type ErrorFunc = extern "C" fn(error: c_int, user: *mut c_void) -> c_int;

/// `OPENMPT_MODULE_RENDER_*` ids from libopenmpt.h.
const RENDER_STEREOSEPARATION_PERCENT: c_int = 2;
const RENDER_INTERPOLATIONFILTER_LENGTH: c_int = 3;

extern "C" {
    fn openmpt_get_library_version() -> u32;
    fn openmpt_get_string(key: *const c_char) -> *const c_char;
    fn openmpt_free_string(s: *const c_char);
    fn openmpt_get_supported_extensions() -> *const c_char;
    fn openmpt_is_extension_supported(extension: *const c_char) -> c_int;
    fn openmpt_module_create_from_memory2(
        filedata: *const c_void,
        filesize: usize,
        logfunc: Option<LogFunc>,
        loguser: *mut c_void,
        errfunc: Option<ErrorFunc>,
        erruser: *mut c_void,
        error: *mut c_int,
        error_message: *mut *const c_char,
        ctls: *const c_void,
    ) -> *mut Module;
    fn openmpt_module_destroy(module: *mut Module);
    fn openmpt_module_read_interleaved_float_stereo(
        module: *mut Module,
        samplerate: c_int,
        count: usize,
        interleaved_stereo: *mut f32,
    ) -> usize;
    fn openmpt_module_set_repeat_count(module: *mut Module, repeat_count: i32) -> c_int;
    fn openmpt_module_set_position_seconds(module: *mut Module, seconds: c_double) -> c_double;
    fn openmpt_module_get_duration_seconds(module: *mut Module) -> c_double;
    fn openmpt_module_set_render_param(module: *mut Module, param: c_int, value: i32) -> c_int;
    fn openmpt_module_ctl_set_boolean(
        module: *mut Module,
        ctl: *const c_char,
        value: c_int,
    ) -> c_int;
    fn openmpt_module_ctl_set_floatingpoint(
        module: *mut Module,
        ctl: *const c_char,
        value: c_double,
    ) -> c_int;
    fn openmpt_module_ctl_set_text(
        module: *mut Module,
        ctl: *const c_char,
        value: *const c_char,
    ) -> c_int;
    fn openmpt_module_get_metadata(module: *mut Module, key: *const c_char) -> *const c_char;
    fn openmpt_module_get_num_channels(module: *mut Module) -> i32;
    fn openmpt_module_get_num_orders(module: *mut Module) -> i32;
    fn openmpt_module_get_num_patterns(module: *mut Module) -> i32;
    fn openmpt_module_get_num_instruments(module: *mut Module) -> i32;
    fn openmpt_module_get_num_samples(module: *mut Module) -> i32;
    fn openmpt_module_get_num_subsongs(module: *mut Module) -> i32;
    fn openmpt_module_get_current_channel_vu_left(module: *mut Module, channel: i32) -> f32;
    fn openmpt_module_get_current_channel_vu_right(module: *mut Module, channel: i32) -> f32;
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum TrackerError {
    /// The file could not be read from disk.
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    /// libopenmpt refused to load the data (not a module, unsupported, damaged).
    Open {
        path: PathBuf,
        code: i32,
        detail: String,
    },
    /// libopenmpt rejected a `ctl` value.
    Control { ctl: &'static str, value: String },
    /// libopenmpt rejected a render parameter.
    RenderParam { param: &'static str, value: i32 },
    /// A caller-supplied parameter was not a usable number.
    InvalidParam { param: &'static str, value: f64 },
    /// Output sample rate outside libopenmpt's documented range.
    Rate { rate: u32 },
}

impl fmt::Display for TrackerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TrackerError::Read { path, source } => {
                write!(f, "tracker source read {}: {source}", path.display())
            }
            TrackerError::Open { path, code, detail } => write!(
                f,
                "libopenmpt could not load {} (error {code}): {detail}",
                path.display()
            ),
            TrackerError::Control { ctl, value } => {
                write!(f, "libopenmpt rejected ctl {ctl} = {value}")
            }
            TrackerError::RenderParam { param, value } => {
                write!(f, "libopenmpt rejected render param {param} = {value}")
            }
            TrackerError::InvalidParam { param, value } => {
                write!(f, "invalid tracker parameter {param} = {value}")
            }
            TrackerError::Rate { rate } => write!(
                f,
                "output rate {rate} Hz outside libopenmpt's 8000..=192000 Hz range"
            ),
        }
    }
}

impl std::error::Error for TrackerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            TrackerError::Read { source, .. } => Some(source),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Parameter model
// ---------------------------------------------------------------------------

/// Mixer interpolation filter. Only the values libopenmpt implements.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Interpolation {
    /// libopenmpt's recommended default (currently 8-tap windowed sinc).
    #[default]
    Default,
    /// No interpolation (zero order hold) — bright, aliasing, tracker-authentic.
    ZeroOrderHold,
    Linear,
    Cubic,
    WindowedSinc8,
}

impl Interpolation {
    pub const ALL: [Interpolation; 5] = [
        Interpolation::Default,
        Interpolation::ZeroOrderHold,
        Interpolation::Linear,
        Interpolation::Cubic,
        Interpolation::WindowedSinc8,
    ];

    /// The `RENDER_INTERPOLATIONFILTER_LENGTH` value libopenmpt expects.
    pub fn filter_length(self) -> i32 {
        match self {
            Interpolation::Default => 0,
            Interpolation::ZeroOrderHold => 1,
            Interpolation::Linear => 2,
            Interpolation::Cubic => 4,
            Interpolation::WindowedSinc8 => 8,
        }
    }

    /// Inverse of [`Interpolation::filter_length`]; `None` for lengths
    /// libopenmpt documents as unsupported (e.g. 3, 5..7).
    pub fn from_filter_length(length: i32) -> Option<Self> {
        match length {
            0 => Some(Interpolation::Default),
            1 => Some(Interpolation::ZeroOrderHold),
            2 => Some(Interpolation::Linear),
            4 => Some(Interpolation::Cubic),
            8 => Some(Interpolation::WindowedSinc8),
            _ => None,
        }
    }
}

/// Native tracker playback parameters. Neutral by default — 0 semitones,
/// tempo ratio 1.0, libopenmpt's 100 % separation, default interpolation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrackerParams {
    /// Event-domain pitch shift, independent of tempo. Range ±24 semitones
    /// (`play.pitch_factor` accepts (0, 4], and 4 == +24 semitones).
    pub pitch_semitones: f32,
    /// Event-domain speed multiplier; 2.0 plays twice as fast. libopenmpt
    /// accepts (0, 4]; this engine clamps to 0.25..=4.0.
    pub tempo_ratio: f32,
    /// Mixer stereo separation in percent, 0 (mono) ..= 200 (wide).
    pub stereo_separation: f32,
    pub interpolation: Interpolation,
}

impl Default for TrackerParams {
    fn default() -> Self {
        Self {
            pitch_semitones: 0.0,
            tempo_ratio: 1.0,
            stereo_separation: 100.0,
            interpolation: Interpolation::Default,
        }
    }
}

/// Per-channel approximate mixer VU. Velocity based — libopenmpt documents
/// that these do not reflect the waveform of the playing sample.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ChannelVu {
    pub left: f32,
    pub right: f32,
}

impl ChannelVu {
    /// Louder of the two sides.
    pub fn peak(self) -> f32 {
        self.left.max(self.right)
    }
}

/// Module metadata as reported by libopenmpt, captured once at open.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrackerMetadata {
    pub title: String,
    pub artist: String,
    /// Tracker software that wrote the file (e.g. "Impulse Tracker").
    pub tracker: String,
    /// Short format id, e.g. "mod".
    pub format: String,
    /// Long format name, e.g. "ProTracker 3.x".
    pub format_long: String,
    /// Container format when the module was wrapped (xpk, mo3, umx, ...).
    pub container: String,
    pub date: String,
    /// Song message, newline separated, as embedded by the author.
    pub message: String,
    /// Loader warnings — non-fatal format oddities libopenmpt absorbed.
    pub warnings: String,
    /// Event-domain duration in seconds at tempo 1.0, when finite.
    pub duration_secs: Option<f64>,
    /// Pattern channels (informational; not the output channel count).
    pub channels: i32,
    pub orders: i32,
    pub patterns: i32,
    pub instruments: i32,
    pub samples: i32,
    pub subsongs: i32,
    pub file_size_bytes: u64,
}

// ---------------------------------------------------------------------------
// Library-level helpers
// ---------------------------------------------------------------------------

/// libopenmpt version as `(major, minor, patch)`.
pub fn library_version() -> (u32, u32, u32) {
    let v = unsafe { openmpt_get_library_version() };
    (v >> 24, (v >> 16) & 0xff, v & 0xffff)
}

/// libopenmpt's verbose build string (e.g. `"0.8.9 (2026-08-19)"`).
pub fn library_version_string() -> String {
    let key = CString::new("library_version").expect("static key");
    take_string(unsafe { openmpt_get_string(key.as_ptr()) }).unwrap_or_default()
}

/// Every extension this libopenmpt build can load, lower-case, without dots.
pub fn supported_extensions() -> Vec<String> {
    take_string(unsafe { openmpt_get_supported_extensions() })
        .map(|list| {
            list.split(';')
                .filter(|e| !e.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Whether libopenmpt can load this path's extension. Asks the library rather
/// than a hard-coded list, so the answer cannot drift from the linked build.
pub fn is_tracker_file(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    let Ok(ext) = CString::new(ext) else {
        return false;
    };
    unsafe { openmpt_is_extension_supported(ext.as_ptr()) == 1 }
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

/// Output-timeline playhead plus the library handle it belongs to.
pub struct TrackerEngine {
    module: *mut Module,
    path: PathBuf,
    out_rate: u32,
    metadata: TrackerMetadata,
    params: TrackerParams,
    /// Event-domain duration at tempo 1.0; `None` when libopenmpt reports a
    /// non-finite length (a module whose pattern graph never ends).
    module_duration_secs: Option<f64>,
    /// Output-timeline position, counted from open or the last seek.
    played_secs: f64,
    looping: bool,
    /// The last [`TrackerEngine::render`] hit the end of the song.
    finished: bool,
}

/// SAFETY: the `openmpt_module` handle is only reachable through `&self` /
/// `&mut self`, so it is used by exactly one thread at a time. libopenmpt
/// documents that distinct modules may be used from distinct threads; sharing
/// one module is the only prohibited case. Moving the engine to the decoder
/// thread transfers the sole owner.
unsafe impl Send for TrackerEngine {}

impl TrackerEngine {
    /// Load a module and prepare it to render interleaved stereo `f32` at
    /// `out_rate`. libopenmpt renders at the device rate directly — no
    /// resampling stage is needed downstream.
    ///
    /// The file is read into memory because libopenmpt's in-memory loader is
    /// the fast path and copies what it needs; nothing of the buffer survives
    /// this call.
    pub fn open(path: &Path, out_rate: u32) -> Result<Self, TrackerError> {
        if !(8_000..=192_000).contains(&out_rate) {
            return Err(TrackerError::Rate { rate: out_rate });
        }
        let bytes = std::fs::read(path).map_err(|source| TrackerError::Read {
            path: path.to_path_buf(),
            source,
        })?;

        let mut error: c_int = 0;
        let mut error_message: *const c_char = std::ptr::null();
        let module = unsafe {
            openmpt_module_create_from_memory2(
                bytes.as_ptr().cast(),
                bytes.len(),
                None,
                std::ptr::null_mut(),
                None,
                std::ptr::null_mut(),
                &mut error,
                &mut error_message,
                std::ptr::null(),
            )
        };
        // libopenmpt allocates the message even on failure; always free it.
        let detail = take_string(error_message);
        if module.is_null() {
            return Err(TrackerError::Open {
                path: path.to_path_buf(),
                code: error,
                detail: detail.unwrap_or_else(|| "no detail reported".into()),
            });
        }

        let mut engine = Self {
            module,
            path: path.to_path_buf(),
            out_rate,
            metadata: TrackerMetadata::default(),
            params: TrackerParams::default(),
            module_duration_secs: None,
            played_secs: 0.0,
            looping: false,
            finished: false,
        };
        // Any error here drops `engine`, which destroys the module.
        engine.configure_end_handling()?;
        engine.read_metadata()?;
        engine.apply_params(TrackerParams::default())?;
        Ok(engine)
    }

    fn configure_end_handling(&mut self) -> Result<(), TrackerError> {
        // Deterministic EOF instead of libopenmpt's own fade-out: the caller
        // owns fades, and `render` must be able to report "pass finished".
        self.set_ctl_text("play.at_end", "stop")?;
        // Seek on the sample grid, so a seek lands on the PCM the module would
        // have produced rather than on the row boundary.
        self.set_ctl_bool("seek.sync_samples", true)?;
        Ok(())
    }

    fn read_metadata(&mut self) -> Result<(), TrackerError> {
        let duration = unsafe { openmpt_module_get_duration_seconds(self.module) };
        let module_duration_secs = if duration.is_finite() && duration >= 0.0 {
            Some(duration)
        } else {
            None
        };
        self.module_duration_secs = module_duration_secs;
        self.metadata = TrackerMetadata {
            title: self.meta_string("title"),
            artist: self.meta_string("artist"),
            tracker: self.meta_string("tracker"),
            format: self.meta_string("type"),
            format_long: self.meta_string("type_long"),
            container: self.meta_string("container_long"),
            date: self.meta_string("date"),
            message: self.meta_string("message"),
            warnings: self.meta_string("warnings"),
            duration_secs: module_duration_secs,
            channels: unsafe { openmpt_module_get_num_channels(self.module) },
            orders: unsafe { openmpt_module_get_num_orders(self.module) },
            patterns: unsafe { openmpt_module_get_num_patterns(self.module) },
            instruments: unsafe { openmpt_module_get_num_instruments(self.module) },
            samples: unsafe { openmpt_module_get_num_samples(self.module) },
            subsongs: unsafe { openmpt_module_get_num_subsongs(self.module) },
            file_size_bytes: std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0),
        };
        Ok(())
    }

    fn meta_string(&self, key: &str) -> String {
        let key = CString::new(key).expect("static metadata key");
        let value = unsafe { openmpt_module_get_metadata(self.module, key.as_ptr()) };
        take_string(value).unwrap_or_default()
    }

    fn set_ctl_bool(&mut self, ctl: &'static str, value: bool) -> Result<(), TrackerError> {
        let key = CString::new(ctl).expect("static ctl key");
        let ok =
            unsafe { openmpt_module_ctl_set_boolean(self.module, key.as_ptr(), value as c_int) };
        if ok == 1 {
            Ok(())
        } else {
            Err(TrackerError::Control {
                ctl,
                value: value.to_string(),
            })
        }
    }

    fn set_ctl_text(&mut self, ctl: &'static str, value: &str) -> Result<(), TrackerError> {
        let key = CString::new(ctl).expect("static ctl key");
        let text = CString::new(value).expect("static ctl value");
        let ok = unsafe { openmpt_module_ctl_set_text(self.module, key.as_ptr(), text.as_ptr()) };
        if ok == 1 {
            Ok(())
        } else {
            Err(TrackerError::Control {
                ctl,
                value: value.to_owned(),
            })
        }
    }

    // -- parameters ---------------------------------------------------------

    /// Apply a complete parameter set. Values are clamped into libopenmpt's
    /// supported ranges; non-finite input is rejected rather than guessed at.
    pub fn apply_params(&mut self, params: TrackerParams) -> Result<(), TrackerError> {
        self.set_pitch_semitones(params.pitch_semitones)?;
        self.set_tempo_ratio(params.tempo_ratio)?;
        self.set_stereo_separation(params.stereo_separation)?;
        self.set_interpolation(params.interpolation)?;
        Ok(())
    }

    pub fn params(&self) -> TrackerParams {
        self.params
    }

    /// `play.pitch_factor`. Independent of tempo: the event stream is
    /// unaffected, only the sample pitch.
    pub fn set_pitch_semitones(&mut self, semitones: f32) -> Result<(), TrackerError> {
        let value = clamp_param(
            "pitch_semitones",
            semitones,
            PITCH_SEMITONES_MAX,
            -PITCH_SEMITONES_MAX,
        )?;
        let factor = 2.0f64.powf(value as f64 / 12.0);
        let ctl = CString::new("play.pitch_factor").expect("static ctl key");
        let ok = unsafe { openmpt_module_ctl_set_floatingpoint(self.module, ctl.as_ptr(), factor) };
        if ok == 1 {
            self.params.pitch_semitones = value;
            Ok(())
        } else {
            Err(TrackerError::Control {
                ctl: "play.pitch_factor",
                value: format!("{factor} ({value} semitones)"),
            })
        }
    }

    /// `play.tempo_factor`. Independent of pitch; changes both the speed and
    /// the output-timeline duration.
    pub fn set_tempo_ratio(&mut self, ratio: f32) -> Result<(), TrackerError> {
        let value = clamp_param("tempo_ratio", ratio, TEMPO_RATIO_MAX, TEMPO_RATIO_MIN)?;
        let ctl = CString::new("play.tempo_factor").expect("static ctl key");
        let ok = unsafe {
            openmpt_module_ctl_set_floatingpoint(self.module, ctl.as_ptr(), value as f64)
        };
        if ok == 1 {
            self.params.tempo_ratio = value;
            Ok(())
        } else {
            Err(TrackerError::Control {
                ctl: "play.tempo_factor",
                value: value.to_string(),
            })
        }
    }

    /// Mixer stereo separation, 0..=200 percent.
    pub fn set_stereo_separation(&mut self, percent: f32) -> Result<(), TrackerError> {
        let value = clamp_param("stereo_separation", percent, STEREO_SEPARATION_MAX, 0.0)?;
        let rounded = value.round() as i32;
        set_render_param(
            self.module,
            RENDER_STEREOSEPARATION_PERCENT,
            rounded,
            "stereo_separation",
        )?;
        self.params.stereo_separation = rounded as f32;
        Ok(())
    }

    pub fn set_interpolation(&mut self, interpolation: Interpolation) -> Result<(), TrackerError> {
        let length = interpolation.filter_length();
        set_render_param(
            self.module,
            RENDER_INTERPOLATIONFILTER_LENGTH,
            length,
            "interpolation",
        )?;
        self.params.interpolation = interpolation;
        Ok(())
    }

    // -- transport ----------------------------------------------------------

    /// Repeating playback natively inside libopenmpt (`repeat_count = -1`).
    /// With looping on, [`TrackerEngine::render`] never returns 0; toggling it
    /// off mid-loop stops at the next song end.
    pub fn set_loop(&mut self, enabled: bool) -> Result<(), TrackerError> {
        let repeat = if enabled { -1 } else { 0 };
        let ok = unsafe { openmpt_module_set_repeat_count(self.module, repeat) };
        if ok != 1 {
            return Err(TrackerError::Control {
                ctl: "repeat_count",
                value: repeat.to_string(),
            });
        }
        self.looping = enabled;
        Ok(())
    }

    pub fn looping(&self) -> bool {
        self.looping
    }

    /// Output-timeline duration in seconds at the current tempo ratio, or
    /// `None` when the module has no finite end. Always finite when `Some`.
    pub fn duration_secs(&self) -> Option<f64> {
        self.module_duration_secs
            .map(|d| d / self.params.tempo_ratio as f64)
    }

    /// Output-timeline position: rendered frames since open or the last seek,
    /// in seconds. With looping on it wraps into `0..duration` once a pass
    /// completes.
    pub fn position_secs(&self) -> f64 {
        self.played_secs
    }

    /// Seek on the output timeline. Values outside `0..=duration` are clamped;
    /// the position actually reached is returned. Non-finite input is a caller
    /// bug and returns an error rather than a guess.
    pub fn seek(&mut self, seconds: f64) -> Result<f64, TrackerError> {
        if !seconds.is_finite() {
            return Err(TrackerError::InvalidParam {
                param: "seek_seconds",
                value: seconds,
            });
        }
        let mut target = seconds.max(0.0);
        if let Some(duration) = self.duration_secs() {
            target = target.min(duration);
        }
        let reached = unsafe { openmpt_module_set_position_seconds(self.module, target) };
        self.played_secs = reached.max(0.0);
        self.finished = false;
        Ok(self.played_secs)
    }

    /// Render interleaved stereo `f32` into `out` and return the frame count
    /// written. Only the first `2 * returned` samples are touched. A return of
    /// `0` means the end of the song and this engine is not looping.
    ///
    /// Samples are nominal `-1..1` but are **not** clipped by libopenmpt, so a
    /// hot module can overshoot; the output stage owns limiting.
    pub fn render(&mut self, out: &mut [f32]) -> usize {
        assert!(
            out.len() % 2 == 0,
            "interleaved stereo buffer must hold whole frames, got {} samples",
            out.len()
        );
        let frames = out.len() / 2;
        if frames == 0 {
            return 0;
        }
        let rendered = unsafe {
            openmpt_module_read_interleaved_float_stereo(
                self.module,
                self.out_rate as c_int,
                frames,
                out.as_mut_ptr(),
            )
        };
        self.played_secs += rendered as f64 / self.out_rate as f64;
        if rendered == 0 {
            self.finished = true;
        } else if self.looping {
            if let Some(duration) = self.duration_secs() {
                if duration > 0.0 && self.played_secs >= duration {
                    self.played_secs %= duration;
                }
            }
        }
        rendered
    }

    /// `true` once [`TrackerEngine::render`] has reported the end of the song.
    pub fn finished(&self) -> bool {
        self.finished
    }

    // -- introspection ------------------------------------------------------

    pub fn out_rate(&self) -> u32 {
        self.out_rate
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn metadata(&self) -> &TrackerMetadata {
        &self.metadata
    }

    /// Pattern channels in the module (not the output channel count, which is
    /// always stereo here).
    pub fn channel_count(&self) -> u32 {
        self.metadata.channels.max(0) as u32
    }

    /// VU/level view of every pattern channel, resizing `out` to
    /// `channel_count()` and reusing its allocation across frames.
    pub fn channel_vu_into(&self, out: &mut Vec<ChannelVu>) {
        let count = self.channel_count() as usize;
        out.clear();
        out.reserve(count);
        for index in 0..count {
            out.push(unsafe {
                ChannelVu {
                    left: openmpt_module_get_current_channel_vu_left(self.module, index as i32),
                    right: openmpt_module_get_current_channel_vu_right(self.module, index as i32),
                }
            });
        }
    }

    /// Loudest channel's VU in one pass, for a single meter.
    pub fn vu_peak(&self) -> ChannelVu {
        let mut peak = ChannelVu::default();
        for index in 0..self.channel_count() as i32 {
            let left = unsafe { openmpt_module_get_current_channel_vu_left(self.module, index) };
            let right = unsafe { openmpt_module_get_current_channel_vu_right(self.module, index) };
            peak.left = peak.left.max(left);
            peak.right = peak.right.max(right);
        }
        peak
    }
}

impl Drop for TrackerEngine {
    fn drop(&mut self) {
        unsafe { openmpt_module_destroy(self.module) };
    }
}

impl fmt::Debug for TrackerEngine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TrackerEngine")
            .field("path", &self.path)
            .field("out_rate", &self.out_rate)
            .field("params", &self.params)
            .field("duration_secs", &self.duration_secs())
            .field("position_secs", &self.played_secs)
            .field("looping", &self.looping)
            .field("finished", &self.finished)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

/// libopenmpt accepts (0, 4] for both play factors; factor 4 is +24 semitones.
const PITCH_SEMITONES_MAX: f32 = 24.0;
/// Lower bound chosen as the reciprocal of the library's upper bound, so the
/// control travels symmetrically (0.25x .. 4.0x).
const TEMPO_RATIO_MIN: f32 = 0.25;
const TEMPO_RATIO_MAX: f32 = 4.0;
const STEREO_SEPARATION_MAX: f32 = 200.0;

fn clamp_param(param: &'static str, value: f32, high: f32, low: f32) -> Result<f32, TrackerError> {
    if !value.is_finite() {
        return Err(TrackerError::InvalidParam {
            param,
            value: value as f64,
        });
    }
    Ok(value.clamp(low, high))
}

fn set_render_param(
    module: *mut Module,
    param: c_int,
    value: i32,
    name: &'static str,
) -> Result<(), TrackerError> {
    let ok = unsafe { openmpt_module_set_render_param(module, param, value) };
    if ok == 1 {
        Ok(())
    } else {
        Err(TrackerError::RenderParam { param: name, value })
    }
}

/// Take ownership of a string libopenmpt allocated and free it. Every
/// `const char *` the library returns must go through `openmpt_free_string` —
/// the C runtime on the other side of the ABI is not necessarily ours.
fn take_string(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    let value = unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned();
    unsafe { openmpt_free_string(ptr) };
    Some(value)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linked_library_is_the_vendored_release() {
        assert_eq!(library_version(), (0, 8, 9));
        assert!(library_version_string().starts_with("0.8.9"));
    }

    #[test]
    fn extension_support_comes_from_the_library() {
        for ext in ["mod", "s3m", "xm", "it", "mptm", "umx", "mo3"] {
            assert!(
                is_tracker_file(Path::new(&format!("x.{ext}"))),
                "{ext} must be supported"
            );
        }
        for ext in ["mp3", "flac", "wav", "mid", "txt"] {
            assert!(
                !is_tracker_file(Path::new(&format!("x.{ext}"))),
                "{ext} must not be claimed"
            );
        }
        assert!(
            is_tracker_file(Path::new("A.MOD")),
            "extension case is ignored"
        );
        assert!(!is_tracker_file(Path::new("no_extension")));
        assert!(supported_extensions().len() > 40);
    }

    #[test]
    fn interpolation_round_trips_through_filter_lengths() {
        for interpolation in Interpolation::ALL {
            assert_eq!(
                Interpolation::from_filter_length(interpolation.filter_length()),
                Some(interpolation)
            );
        }
        assert_eq!(Interpolation::from_filter_length(3), None);
        assert_eq!(Interpolation::default(), Interpolation::Default);
    }

    #[test]
    fn defaults_are_neutral() {
        let p = TrackerParams::default();
        assert_eq!(p.pitch_semitones, 0.0);
        assert_eq!(p.tempo_ratio, 1.0);
        assert_eq!(p.stereo_separation, 100.0);
        assert_eq!(p.interpolation, Interpolation::Default);
    }

    #[test]
    fn out_of_range_rate_is_rejected_before_touching_the_file() {
        let err = TrackerEngine::open(Path::new("irrelevant.mod"), 4_000).unwrap_err();
        assert!(matches!(err, TrackerError::Rate { rate: 4_000 }));
    }

    #[test]
    fn missing_file_is_a_read_error() {
        let err = TrackerEngine::open(Path::new("Z:/definitely/not/here.mod"), 44_100).unwrap_err();
        assert!(matches!(err, TrackerError::Read { .. }), "{err}");
    }

    #[test]
    fn non_module_data_is_rejected_by_the_library() {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let err = TrackerEngine::open(&manifest.join("Cargo.toml"), 44_100).unwrap_err();
        assert!(matches!(err, TrackerError::Open { .. }), "{err}");
    }

    #[test]
    fn non_finite_parameters_are_rejected() {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        // Prove the range check fires before any file access.
        let err = TrackerEngine::open(&manifest.join("Cargo.toml"), 192_001).unwrap_err();
        assert!(matches!(err, TrackerError::Rate { rate: 192_001 }), "{err}");
        assert!(matches!(
            clamp_param("tempo_ratio", f32::NAN, TEMPO_RATIO_MAX, TEMPO_RATIO_MIN),
            Err(TrackerError::InvalidParam { .. })
        ));
        assert_eq!(
            clamp_param("tempo_ratio", 100.0, TEMPO_RATIO_MAX, TEMPO_RATIO_MIN).unwrap(),
            TEMPO_RATIO_MAX
        );
        assert_eq!(
            clamp_param(
                "pitch_semitones",
                -100.0,
                PITCH_SEMITONES_MAX,
                -PITCH_SEMITONES_MAX
            )
            .unwrap(),
            -PITCH_SEMITONES_MAX
        );
    }

    /// Media-dependent checks. Enable with `--ignored` on a machine that has
    /// the canonical test share mounted.
    #[test]
    #[ignore = "requires Y:\\Test Files tracker media"]
    fn native_tempo_seek_continues_render() {
        let path = Path::new(r"Y:\Test Files\alpmar-3rd_world.mod");
        let mut engine = TrackerEngine::open(&path, 48_000).expect("module must load");
        engine.set_tempo_ratio(2.0).expect("native tempo");
        let mut buffer = vec![0.0f32; 4096 * 2];
        for seconds in [0.0, 10.0, 30.0, 60.0] {
            let reached = engine.seek(seconds).expect("seek on scaled timeline");
            assert!(
                (reached - seconds).abs() < 1.0,
                "seek {seconds}s reached {reached}s"
            );
            assert_eq!(
                engine.render(&mut buffer),
                4096,
                "seek {seconds}s ended playback"
            );
        }
    }

    /// Media-dependent checks. Enable with `--ignored` on a machine that has
    /// the canonical test share mounted.
    #[test]
    #[ignore = "requires Y:\\Test Files tracker media"]
    fn loads_and_renders_the_canonical_modules() {
        for name in ["alpmar-3rd_world.mod", "metroplx.s3m", "sunday_7_34.xm"] {
            let path = Path::new(r"Y:\Test Files").join(name);
            let mut engine = TrackerEngine::open(&path, 44_100).expect("module must load");
            assert!(engine.channel_count() > 0, "{name}");
            assert!(
                engine.duration_secs().is_some(),
                "{name} needs a finite length"
            );
            // Five seconds, not one block: an XM/S3M can legitimately open with
            // a long silent lead-in, and "renders silence" must mean the module
            // produces no audio, not that block 0 was empty.
            let blocks = 5 * 44_100 / 4096 + 1;
            let mut buffer = vec![0.0f32; 4096 * 2];
            let mut peak = 0.0f32;
            let mut frames = 0;
            for _ in 0..blocks {
                let rendered = engine.render(&mut buffer);
                assert_eq!(rendered, 4096, "{name} must render a full block");
                frames += rendered;
                peak = buffer.iter().fold(peak, |acc, s| acc.max(s.abs()));
            }
            assert!(frames > 0, "{name} rendered nothing");
            assert!(peak > 1e-3, "{name} must not render silence (peak {peak})");
        }
    }
}
