//! Phase 2.4 — MIDI / SoundFont synthesis engine over the official FluidSynth
//! 2.6.1 C API (`libfluidsynth-3.dll`, LGPL-2.1, dynamically linked — see
//! `vendor/fluidsynth/README.md` for the link + DLL staging contract).
//!
//! Semantics mirror the reference Electron SoundApp (`js/midi/*`, which drove
//! FluidSynth WASM through js-synthesizer 1.11):
//!
//! - **Transpose** intercepts note-on/note-off at the player's playback
//!   callback, adds semitones to the key (clamped 0..127), and **skips the
//!   percussion channel 9** — identical to the reference `SoundAppMidiHook`.
//!   Live: no rebuild, position preserved.
//! - **BPM override** uses `fluid_player_set_tempo(EXTERNAL_BPM, bpm)`; `None`
//!   returns to the file's own tempo map (`INTERNAL`, factor 1.0).
//! - **Metronome** mixes the reference WAV assets (high on bar starts, low on
//!   other beats, gains 0.7/0.4) on a time-signature-aware beat grid taken
//!   from the file. Falls back to a synthesized click if the WAVs are absent.
//! - **SoundFont switching** loads the new font (`reset_presets=1` re-selects
//!   each channel's bank/program), silences active notes, and lazily unloads
//!   the old one. Playback position is untouched. Channels whose bank/program
//!   the new font lacks fall silent until their next program change — that is
//!   the "where supported" boundary.
//! - **Seek** maps seconds → ticks through the tempo map parsed from the file
//!   (native) or a constant-tempo map (override), then `fluid_player_seek`.
//!   The seek is consumed by the next render call; the player mutes ringing
//!   channels and re-chases controllers at the target.
//! - **EOF** FluidSynth drains voices + a reverb grace period after the last
//!   event before flipping to DONE; this engine keeps a 0.25 s minimum tail
//!   (2.0 s hard cap) after the timeline end before reporting EOF.
//!
//! # Threading
//! Single-owner: create, render, and control this engine from one thread (the
//! eventual decoder thread). FluidSynth runs its player callback synchronously
//! inside `fluid_synth_write_float` (`player.timing-source=sample`), so there
//! is no hidden thread; rendering faster than real time (offline render) is
//! exact. The type is `!Send`/`!Sync` by construction.
//!
//! # Downstream neutrality
//! Transpose and BPM are applied natively here. The audio integration bypasses
//! the shared Tape/RubberBand stages for MIDI, matching the reference player.
//!
//! # Linking
//! This file declares the FFI surface but emits no `#[link]` attribute; the
//! owning crate's `build.rs` provides `cargo:rustc-link-lib=dylib=libfluidsynth-3`
//! and stages `libfluidsynth-3.dll` + `sndfile.dll` (see vendor README).

// The engine API is complete by design and exercised by this module's test
// suite (including `--ignored` live tests on the reference media); the audio
// integration uses a subset. `dead_code` would otherwise flag the
// introspection half on non-test builds.
#![allow(dead_code)]

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::fmt;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// FFI — official FluidSynth 2.6.1 public C API (signatures verified against
// vendor/fluidsynth/include/fluidsynth/*.h).
// ---------------------------------------------------------------------------

// FluidSynth opaque handle aliases — C names kept deliberately.
#[allow(non_camel_case_types)]
type fluid_settings_t = c_void;
#[allow(non_camel_case_types)]
type fluid_synth_t = c_void;
#[allow(non_camel_case_types)]
type fluid_player_t = c_void;
#[allow(non_camel_case_types)]
type fluid_midi_event_t = c_void;

extern "C" {
    // settings
    fn new_fluid_settings() -> *mut fluid_settings_t;
    fn delete_fluid_settings(s: *mut fluid_settings_t);
    fn fluid_settings_setnum(s: *mut fluid_settings_t, name: *const c_char, val: f64) -> c_int;
    fn fluid_settings_setint(s: *mut fluid_settings_t, name: *const c_char, val: c_int) -> c_int;
    fn fluid_settings_setstr(
        s: *mut fluid_settings_t,
        name: *const c_char,
        str: *const c_char,
    ) -> c_int;
    // synth
    fn new_fluid_synth(s: *mut fluid_settings_t) -> *mut fluid_synth_t;
    fn delete_fluid_synth(s: *mut fluid_synth_t);
    fn fluid_synth_sfload(
        s: *mut fluid_synth_t,
        filename: *const c_char,
        reset_presets: c_int,
    ) -> c_int;
    fn fluid_synth_sfunload(s: *mut fluid_synth_t, id: c_int, reset_presets: c_int) -> c_int;
    fn fluid_synth_write_float(
        s: *mut fluid_synth_t,
        len: c_int,
        lout: *mut c_void,
        loff: c_int,
        lincr: c_int,
        rout: *mut c_void,
        roff: c_int,
        rincr: c_int,
    ) -> c_int;
    fn fluid_synth_get_active_voice_count(s: *mut fluid_synth_t) -> c_int;
    fn fluid_synth_all_sounds_off(s: *mut fluid_synth_t, chan: c_int) -> c_int;
    fn fluid_synth_handle_midi_event(data: *mut c_void, event: *mut fluid_midi_event_t) -> c_int;
    // player
    fn new_fluid_player(s: *mut fluid_synth_t) -> *mut fluid_player_t;
    fn delete_fluid_player(p: *mut fluid_player_t);
    fn fluid_player_add_mem(p: *mut fluid_player_t, buffer: *const c_void, len: usize) -> c_int;
    fn fluid_player_play(p: *mut fluid_player_t) -> c_int;
    fn fluid_player_stop(p: *mut fluid_player_t) -> c_int;
    fn fluid_player_seek(p: *mut fluid_player_t, ticks: c_int) -> c_int;
    fn fluid_player_set_loop(p: *mut fluid_player_t, loop_: c_int) -> c_int;
    fn fluid_player_set_tempo(p: *mut fluid_player_t, tempo_type: c_int, tempo: f64) -> c_int;
    fn fluid_player_set_playback_callback(
        p: *mut fluid_player_t,
        handler: extern "C" fn(*mut c_void, *mut fluid_midi_event_t) -> c_int,
        handler_data: *mut c_void,
    ) -> c_int;
    fn fluid_player_get_status(p: *mut fluid_player_t) -> c_int;
    fn fluid_player_get_current_tick(p: *mut fluid_player_t) -> c_int;
    fn fluid_player_get_total_ticks(p: *mut fluid_player_t) -> c_int;
    fn fluid_player_get_division(p: *mut fluid_player_t) -> c_int;
    // events (used by the transpose hook)
    fn fluid_midi_event_get_type(evt: *const fluid_midi_event_t) -> c_int;
    fn fluid_midi_event_get_channel(evt: *const fluid_midi_event_t) -> c_int;
    fn fluid_midi_event_get_key(evt: *const fluid_midi_event_t) -> c_int;
    fn fluid_midi_event_set_key(evt: *mut fluid_midi_event_t, key: c_int) -> c_int;
    // logging
    fn fluid_set_log_function(
        level: c_int,
        fun: Option<extern "C" fn(c_int, *const c_char, *mut c_void)>,
        data: *mut c_void,
    );
}

/// `fluid_player_status`: READY=0, PLAYING=1, STOPPING=2, DONE=3.
const FLUID_PLAYER_DONE: c_int = 3;
/// `fluid_player_set_tempo_type`: INTERNAL = file tempo × multiplier.
const FLUID_PLAYER_TEMPO_INTERNAL: c_int = 0;
/// `fluid_player_set_tempo_type`: EXTERNAL_BPM = absolute BPM override.
const FLUID_PLAYER_TEMPO_EXTERNAL_BPM: c_int = 1;

/// MIDI event type values as returned by `fluid_midi_event_get_type` for
/// channel messages: the status byte without the channel part.
const MIDI_NOTE_OFF: c_int = 0x80;
const MIDI_NOTE_ON: c_int = 0x90;

/// FluidSynth channel that General MIDI reserves for percussion. Transpose
/// skips it, matching the reference app.
const PERCUSSION_CHANNEL: c_int = 9;

/// Default General-MIDI SoundFont: the user's config folder when present, else
/// the read-only copy shipped inside the installed application directory.
/// Distribution packages this 30.8 MB file rather than vendoring it into git.
pub fn default_soundfont() -> PathBuf {
    let config = crate::config::UserConfig::soundfonts_dir().join("default.sf2");
    if config.exists() {
        return config;
    }
    if let Some(bundled) = crate::config::UserConfig::bundled_soundfonts_dir() {
        let path = bundled.join("default.sf2");
        if path.exists() {
            return path;
        }
    }
    config
}

/// Reference metronome click WAVs (mixed at `set_metronome(true)`), resolved
/// from `<exe_dir>/metronome/` first (distribution layout) and the reference
/// SoundApp tree second (development). Missing/unreadable assets fall back to
/// a synthesized click, logged loudly.
const METRONOME_HIGH_WAV: &str = r"D:\Work\_GIT\SoundApp\bin\metronome\metronome-high.wav";
const METRONOME_LOW_WAV: &str = r"D:\Work\_GIT\SoundApp\bin\metronome\metronome-low.wav";

/// Distribution-first metronome asset path: `<exe_dir>/metronome/<name>` when
/// present, else the reference-tree path.
fn metronome_asset(dev_path: &str) -> String {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            if let Some(name) = std::path::Path::new(dev_path).file_name() {
                let p = dir.join("metronome").join(name);
                if p.exists() {
                    return p.to_string_lossy().into_owned();
                }
            }
        }
    }
    dev_path.to_string()
}
/// Metronome click gains, from the reference `midi.js` (0.7 / 0.4).
const METRONOME_HIGH_GAIN: f32 = 0.7;
const METRONOME_LOW_GAIN: f32 = 0.4;

/// Minimum rendered tail after the timeline end before EOF latches, and the
/// hard cap after which EOF latches regardless of lingering voices (seconds).
/// The cap must exceed FluidSynth's built-in end-of-song grace (~3 s), which
/// runs while the player is still PLAYING and is what makes the reverb decay
/// sound natural instead of cut — the audible "short reverb tail" is that
/// grace window, and EOF latches as soon as DONE flips after it.
const TAIL_MIN_S: f64 = 0.25;
const TAIL_MAX_S: f64 = 5.0;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum MidiError {
    /// SMF could not be parsed (malformed, format 2, SMPTE timing, …).
    Parse(String),
    /// SoundFont failed to load: (path, reason).
    SoundFont(PathBuf, String),
    /// A FluidSynth call unexpectedly returned FLUID_FAILED.
    Fluid(&'static str),
    /// Filesystem error with context.
    Io(String),
}

impl fmt::Display for MidiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MidiError::Parse(m) => write!(f, "MIDI parse error: {m}"),
            MidiError::SoundFont(p, m) => write!(f, "SoundFont {} : {m}", p.display()),
            MidiError::Fluid(call) => write!(f, "FluidSynth {call} failed"),
            MidiError::Io(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for MidiError {}

// ---------------------------------------------------------------------------
// File eligibility + native parameter set
// ---------------------------------------------------------------------------

/// Whether this path is a MIDI file this engine claims: `.mid`, `.midi` and
/// RIFF-wrapped `.rmi` (RMID). Extension check only — no I/O, no library call.
pub fn is_midi_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| matches!(e.to_ascii_lowercase().as_str(), "mid" | "midi" | "rmi"))
        .unwrap_or(false)
}

/// Native MIDI playback parameters, mirroring `tracker::TrackerParams`:
/// applied by the decoder thread directly to the `MidiEngine`, never copied
/// into the shared tape/RubberBand post chain. Neutral by default.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MidiParams {
    /// Whole semitones transposed live at the note-on/off hook (percussion
    /// channel 9 skips). `0` = neutral.
    pub transpose_semitones: i32,
    /// Absolute BPM override; `None` follows the file's own tempo map.
    pub bpm_override: Option<f32>,
    /// Mix the metronome click over rendered audio.
    pub metronome: bool,
}

impl Default for MidiParams {
    fn default() -> Self {
        Self {
            transpose_semitones: 0,
            bpm_override: None,
            metronome: false,
        }
    }
}

/// Every SF2/SF3 in the user-configuration `soundfonts/` folder plus the
/// application's bundled folder, sorted by path. A file present in both is
/// listed once, preferring the user's copy so an upgrade cannot shadow an
/// edited font. Falls back to the default path when nothing is found so the
/// select always has one entry and a failed open logs loudly.
pub fn soundfont_list() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut dirs = vec![crate::config::UserConfig::soundfonts_dir()];
    if let Some(bundled) = crate::config::UserConfig::bundled_soundfonts_dir() {
        dirs.push(bundled);
    }
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let is_font = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| matches!(e.to_ascii_lowercase().as_str(), "sf2" | "sf3"))
                .unwrap_or(false);
            let known = out.iter().any(|p| p.file_name() == path.file_name());
            if is_font && !known {
                out.push(path);
            }
        }
    }
    out.sort();
    if out.is_empty() {
        out.push(default_soundfont());
    }
    out
}

/// Unwrap a RIFF `RMID` container to the SMF bytes inside its `data` chunk.
/// Chunks are word-aligned (`size + (size & 1)` stride). Returns the input
/// unchanged when it is not an RMID wrapper; `None` when it claims to be one
/// but holds no SMF.
fn unwrap_rmid(bytes: &[u8]) -> Option<&[u8]> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"RMID" {
        return Some(bytes);
    }
    let mut pos = 12usize;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32::from_le_bytes([
            bytes[pos + 4],
            bytes[pos + 5],
            bytes[pos + 6],
            bytes[pos + 7],
        ]) as usize;
        let body = pos + 8;
        if body + size > bytes.len() {
            return None;
        }
        if id == b"data" && bytes[body..].starts_with(b"MThd") {
            return Some(&bytes[body..body + size]);
        }
        pos = body + size + (size & 1);
    }
    None
}

// ---------------------------------------------------------------------------
// Standard MIDI File parsing (zero-dependency, authoritative timeline)
// ---------------------------------------------------------------------------

/// A time-signature change (`FF 58`, tick in file ticks).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimeSig {
    pub tick: u32,
    pub numerator: u32,
    pub denominator: u32,
}

/// Parsed SMF facts. `duration_secs` is under the **native** tempo map.
#[derive(Clone, Debug)]
pub struct SmfInfo {
    pub format: u16,
    pub track_count: u16,
    pub ppq: u32,
    pub total_ticks: u32,
    /// Tempo changes (`FF 51`): (tick, microseconds per quarter note).
    pub tempos: Vec<(u32, u32)>,
    pub time_signatures: Vec<TimeSig>,
    pub duration_secs: f64,
}

/// Tick ⇄ seconds conversion. Segments are (start_tick, µs per quarter note);
/// the tempo active at tick *t* is the last segment with `start_tick <= t`.
/// A constant map (BPM override) is a single segment.
#[derive(Clone, Debug)]
struct TempoMap {
    ppq: f64,
    /// Segment start ticks (sorted) — always starts with tick 0.
    seg_ticks: Vec<u64>,
    /// µs per quarter note per segment.
    seg_usqn: Vec<f64>,
    /// Cumulative seconds at each segment start.
    seg_secs: Vec<f64>,
}

impl TempoMap {
    fn native(ppq: u32, mut tempos: Vec<(u32, u32)>) -> Self {
        // MIDI default tempo is 120 bpm when the file sets none.
        let mut segs: Vec<(u64, f64)> = vec![(0, 500_000.0)];
        tempos.sort_by_key(|(t, _)| *t);
        for (tick, usqn) in tempos {
            let tick = tick as u64;
            let usqn = usqn as f64;
            if tick == 0 {
                segs[0].1 = usqn;
            } else if segs.last().map(|(t, _)| *t) == Some(tick) {
                segs.last_mut().unwrap().1 = usqn;
            } else {
                segs.push((tick, usqn));
            }
        }
        Self::from_segments(ppq as f64, segs)
    }

    fn constant(ppq: u32, bpm: f64) -> Self {
        Self::from_segments(ppq as f64, vec![(0, 60_000_000.0 / bpm)])
    }

    fn from_segments(ppq: f64, segs: Vec<(u64, f64)>) -> Self {
        let mut seg_ticks = Vec::with_capacity(segs.len());
        let mut seg_usqn = Vec::with_capacity(segs.len());
        let mut seg_secs = Vec::with_capacity(segs.len());
        let mut acc = 0.0f64;
        for (i, (tick, usqn)) in segs.iter().enumerate() {
            if i > 0 {
                let dt = (*tick - segs[i - 1].0) as f64;
                acc += dt * segs[i - 1].1 / ppq / 1_000_000.0;
            }
            seg_ticks.push(*tick);
            seg_usqn.push(*usqn);
            seg_secs.push(acc);
        }
        TempoMap {
            ppq,
            seg_ticks,
            seg_usqn,
            seg_secs,
        }
    }

    /// µs per quarter note active at `tick`.
    fn usqn_at(&self, tick: f64) -> f64 {
        self.seg_usqn[self.segment_index(tick)]
    }

    fn segment_index(&self, tick: f64) -> usize {
        match self.seg_ticks.binary_search(&(tick.max(0.0) as u64)) {
            Ok(i) => i,
            Err(0) => 0,
            Err(i) => i - 1,
        }
    }

    fn tick_to_secs(&self, tick: f64) -> f64 {
        if tick <= 0.0 {
            return 0.0;
        }
        let i = self.segment_index(tick);
        let seg_start_tick = self.seg_ticks[i] as f64;
        self.seg_secs[i] + (tick - seg_start_tick) * self.seg_usqn[i] / self.ppq / 1_000_000.0
    }

    fn secs_to_tick(&self, secs: f64) -> f64 {
        if secs <= 0.0 {
            return 0.0;
        }
        let i = match self
            .seg_secs
            .binary_search_by(|s| s.partial_cmp(&secs).unwrap())
        {
            Ok(i) => i,
            Err(0) => 0,
            Err(i) => i - 1,
        };
        let usqn = self.seg_usqn[i];
        let dt_secs = secs - self.seg_secs[i];
        self.seg_ticks[i] as f64 + dt_secs * 1_000_000.0 * self.ppq / usqn
    }
}

fn parse_smf(bytes: &[u8]) -> Result<SmfInfo, MidiError> {
    if bytes.len() < 14 || &bytes[0..4] != b"MThd" {
        return Err(MidiError::Parse("missing MThd header".into()));
    }
    let hlen = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    if hlen < 6 {
        return Err(MidiError::Parse("MThd length < 6".into()));
    }
    let format = u16::from_be_bytes([bytes[8], bytes[9]]);
    if format > 1 {
        return Err(MidiError::Parse(format!(
            "MIDI format {format} not supported (0/1 only)"
        )));
    }
    let ntrks = u16::from_be_bytes([bytes[10], bytes[11]]);
    let division = u16::from_be_bytes([bytes[12], bytes[13]]);
    if division & 0x8000 != 0 {
        return Err(MidiError::Parse(
            "SMPTE-timed SMF not supported (FluidSynth rejects it too)".into(),
        ));
    }
    if division == 0 {
        return Err(MidiError::Parse("division (PPQ) is zero".into()));
    }
    let ppq = division as u32;

    let mut pos = 8 + hlen as usize;
    let mut tempos = Vec::new();
    let mut time_sigs = Vec::new();
    let mut total_ticks: u32 = 0;
    let mut tracks_seen = 0u16;

    while tracks_seen < ntrks {
        if pos + 8 > bytes.len() {
            return Err(MidiError::Parse(
                "truncated: fewer MTrk chunks than ntrks".into(),
            ));
        }
        let id = &bytes[pos..pos + 4];
        let len = u32::from_be_bytes([
            bytes[pos + 4],
            bytes[pos + 5],
            bytes[pos + 6],
            bytes[pos + 7],
        ]) as usize;
        let body = pos + 8;
        if body + len > bytes.len() {
            return Err(MidiError::Parse("MTrk chunk overruns file".into()));
        }
        if id != b"MTrk" {
            pos = body + len; // unknown chunk type — skip (parallel to FluidSynth)
            continue;
        }
        tracks_seen += 1;
        let track = &bytes[body..body + len];
        let mut i = 0usize;
        let mut tick: u32 = 0;
        let mut status: u8 = 0;
        let eat_varlen = |i: &mut usize| -> Result<u32, MidiError> {
            let mut v: u32 = 0;
            for _ in 0..4 {
                let b = *track
                    .get(*i)
                    .ok_or_else(|| MidiError::Parse("unexpected end of track".into()))?;
                *i += 1;
                v = (v << 7) | (b & 0x7f) as u32;
                if b & 0x80 == 0 {
                    return Ok(v);
                }
            }
            Err(MidiError::Parse(
                "variable-length number exceeds 4 bytes".into(),
            ))
        };
        loop {
            if i >= track.len() {
                break; // no EOT — tolerate, use last tick (FluidSynth does the same)
            }
            tick = tick.saturating_add(eat_varlen(&mut i)?);
            let mut b = *track
                .get(i)
                .ok_or_else(|| MidiError::Parse("unexpected end of track".into()))?;
            i += 1;
            if b & 0x80 == 0 {
                if status == 0 {
                    return Err(MidiError::Parse("data byte with no running status".into()));
                }
                b = status;
                i -= 1; // re-read as first data byte
            } else {
                status = b;
            }
            match b {
                0xFF => {
                    let ty = *track
                        .get(i)
                        .ok_or_else(|| MidiError::Parse("unexpected end of track".into()))?;
                    i += 1;
                    let mlen = eat_varlen(&mut i)? as usize;
                    if i + mlen > track.len() {
                        return Err(MidiError::Parse("meta event overruns track".into()));
                    }
                    match ty {
                        0x51 if mlen == 3 => {
                            let d = &track[i..i + 3];
                            let usqn = ((d[0] as u32) << 16) | ((d[1] as u32) << 8) | d[2] as u32;
                            tempos.push((tick, usqn));
                        }
                        0x58 if mlen >= 2 => {
                            let d = &track[i..i + 2];
                            time_sigs.push(TimeSig {
                                tick,
                                numerator: d[0] as u32,
                                denominator: 1u32 << d[1],
                            });
                        }
                        0x2F => {
                            break; // end of track (payload, if any, is dead)
                        }
                        _ => {}
                    }
                    i += mlen;
                }
                0xF0 | 0xF7 => {
                    let mlen = eat_varlen(&mut i)? as usize;
                    if i + mlen > track.len() {
                        return Err(MidiError::Parse("sysex overruns track".into()));
                    }
                    i += mlen;
                }
                0x80..=0xEF => {
                    let n = if (0xC0..=0xDF).contains(&b) { 1 } else { 2 };
                    if i + n > track.len() {
                        return Err(MidiError::Parse("channel event overruns track".into()));
                    }
                    i += n;
                }
                _ => return Err(MidiError::Parse(format!("invalid status byte 0x{b:02X}"))),
            }
        }
        total_ticks = total_ticks.max(tick);
        pos = body + len;
    }

    time_sigs.sort_by_key(|t| t.tick);
    let map = TempoMap::native(ppq, tempos.clone());
    let duration_secs = map.tick_to_secs(total_ticks as f64);
    Ok(SmfInfo {
        format,
        track_count: ntrks,
        ppq,
        total_ticks,
        tempos,
        time_signatures: time_sigs,
        duration_secs,
    })
}

// ---------------------------------------------------------------------------
// Metronome click assets
// ---------------------------------------------------------------------------

/// Monaural click sample at the engine rate.
struct Click {
    samples: Vec<f32>,
    /// True when this is a synthesized fallback (asset missing/unreadable).
    #[allow(dead_code)]
    synthesized: bool,
}

fn load_click(path: &str, fallback_freq: f32, rate: u32) -> Click {
    if let Ok(data) = std::fs::read(path) {
        if let Some((samples, src_rate)) = parse_wav_mono(&data) {
            return Click {
                samples: resample_mono(&samples, src_rate as f64, rate as f64),
                synthesized: false,
            };
        }
        eprintln!("[midi] metronome asset {path} is not a supported WAV — using synthesized click");
    } else {
        eprintln!("[midi] metronome asset {path} missing — using synthesized click");
    }
    Click {
        samples: synth_click(fallback_freq, rate as f64),
        synthesized: true,
    }
}

/// Minimal RIFF/WAVE reader → (mono samples, source rate). 8/16/24-bit PCM
/// and 32-bit float, any channel count (mixed down).
fn parse_wav_mono(data: &[u8]) -> Option<(Vec<f32>, u32)> {
    if data.len() < 12 || &data[0..4] != b"RIFF" || &data[8..12] != b"WAVE" {
        return None;
    }
    let mut pos = 12usize;
    let mut fmt: Option<(u16, u16, u32, u16)> = None; // (tag, channels, rate, bits)
    let mut data_slice: Option<&[u8]> = None;
    while pos + 8 <= data.len() {
        let id = &data[pos..pos + 4];
        let size = u32::from_le_bytes([data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7]])
            as usize;
        let body = pos + 8;
        if id == b"fmt " && body + 16 <= data.len() {
            let tag = u16::from_le_bytes([data[body], data[body + 1]]);
            let ch = u16::from_le_bytes([data[body + 2], data[body + 3]]);
            let rate = u32::from_le_bytes([
                data[body + 4],
                data[body + 5],
                data[body + 6],
                data[body + 7],
            ]);
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
    let sample = |o: usize| -> f32 {
        match (tag, bits) {
            (1, 8) => (ds[o] as f32 - 128.0) / 128.0,
            (1, 16) => i16::from_le_bytes([ds[o], ds[o + 1]]) as f32 / 32768.0,
            (1, 24) => {
                let v = (((ds[o] as i32) << 24)
                    | ((ds[o + 1] as i32) << 16)
                    | ((ds[o + 2] as i32) << 8))
                    >> 8;
                v as f32 / 8_388_608.0
            }
            (3, 32) => f32::from_le_bytes([ds[o], ds[o + 1], ds[o + 2], ds[o + 3]]),
            _ => 0.0,
        }
    };
    let bytes_per = (bits as usize / 8).max(1);
    if !matches!((tag, bits), (1, 8) | (1, 16) | (1, 24) | (3, 32)) {
        return None;
    }
    let frames = ds.len() / (bytes_per * ch);
    let mut mono = Vec::with_capacity(frames);
    for f in 0..frames {
        let mut acc = 0.0f32;
        for c in 0..ch {
            acc += sample((f * ch + c) * bytes_per);
        }
        mono.push(acc / ch as f32);
    }
    Some((mono, rate))
}

fn resample_mono(src: &[f32], src_rate: f64, dst_rate: f64) -> Vec<f32> {
    if src_rate == dst_rate || src.is_empty() {
        return src.to_vec();
    }
    let ratio = src_rate / dst_rate;
    let n = (src.len() as f64 / ratio).ceil() as usize;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let p = i as f64 * ratio;
        let i0 = p.floor() as usize;
        let i1 = (i0 + 1).min(src.len() - 1);
        let f = (p - i0 as f64) as f32;
        out.push(src[i0] * (1.0 - f) + src[i1] * f);
    }
    out
}

fn synth_click(freq: f32, rate: f64) -> Vec<f32> {
    let n = (0.05 * rate) as usize;
    (0..n)
        .map(|i| {
            let t = i as f32 / rate as f32;
            (2.0 * std::f32::consts::PI * freq * t).sin() * (-t * 45.0).exp() * 0.6
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Transpose playback hook
// ---------------------------------------------------------------------------

/// State shared with the FluidSynth playback callback. The callback replaces
/// the default synth handler, so it must forward every event itself.
struct HookCtx {
    synth: *mut fluid_synth_t,
    semitones: std::sync::atomic::AtomicI32,
}

extern "C" fn playback_hook(data: *mut c_void, event: *mut fluid_midi_event_t) -> c_int {
    let ctx = unsafe { &*(data as *const HookCtx) };
    let t = ctx.semitones.load(std::sync::atomic::Ordering::Relaxed);
    if t != 0 {
        let ty = unsafe { fluid_midi_event_get_type(event) };
        if (ty == MIDI_NOTE_ON || ty == MIDI_NOTE_OFF)
            && unsafe { fluid_midi_event_get_channel(event) } != PERCUSSION_CHANNEL
        {
            let key = unsafe { fluid_midi_event_get_key(event) } + t;
            unsafe { fluid_midi_event_set_key(event, key.clamp(0, 127)) };
        }
    }
    unsafe { fluid_synth_handle_midi_event(ctx.synth as *mut c_void, event) }
}

extern "C" fn fluid_log_hook(level: c_int, message: *const c_char, _data: *mut c_void) {
    // PANIC(0)/ERR(1)/WARN(2) surface on stderr; INFO(3 — including the
    // harmless "unrecognized meta event 0x21" chatter)/DBG(4) dropped.
    if level <= 2 {
        let msg = unsafe { CStr::from_ptr(message) }.to_string_lossy();
        eprintln!("[fluidsynth] {msg}");
    }
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

/// A SoundFont-backed Standard MIDI File playback engine.
///
/// All methods must be called from the owning (decoder) thread. `render`
/// pulls de-interleaved stereo f32 exactly like the FFmpeg decode path, so
/// the engine drops into the existing decoder-thread → ring → mixer chain.
pub struct MidiEngine {
    rate: u32,
    smf_bytes: Vec<u8>,
    info: SmfInfo,
    native_map: TempoMap,
    /// Active map — native, or constant when a BPM override is set.
    map: TempoMap,
    bpm_override: Option<f32>,
    loop_on: bool,
    eof_latched: bool,
    /// Total frames pulled by the caller (timeline anchor for the metronome).
    frames: u64,
    settings: *mut fluid_settings_t,
    synth: *mut fluid_synth_t,
    player: *mut fluid_player_t,
    sf_id: Option<c_int>,
    sf_path: PathBuf,
    /// Kept alive for the player callback; freed after `player` in Drop.
    hook: Option<Box<HookCtx>>,
    metronome: bool,
    click_high: Click,
    click_low: Click,
    /// Scratch for the seek-consume and open-prime renders.
    scratch: Vec<f32>,
}

impl MidiEngine {
    /// Open `path` (`.mid`/`.midi`, SMF format 0/1) at `out_rate` Hz with the
    /// SoundFont at `soundfont_path` (SF2/SF3). Fails fast on a malformed
    /// SMF, an unloadable SoundFont, or unexpected FluidSynth errors.
    pub fn open(
        path: &Path,
        out_rate: u32,
        soundfont_path: &Path,
    ) -> Result<MidiEngine, MidiError> {
        if out_rate == 0 || out_rate > 1_000_000 {
            return Err(MidiError::Parse(format!("invalid sample rate {out_rate}")));
        }
        let raw = std::fs::read(path)
            .map_err(|e| MidiError::Io(format!("read {}: {e}", path.display())))?;
        // `.rmi` wraps the SMF in a RIFF RMID container; the parser and
        // FluidSynth both want the bare SMF.
        let smf_bytes = unwrap_rmid(&raw)
            .filter(|smf| smf.len() >= 14 && &smf[0..4] == b"MThd")
            .map(|smf| smf.to_vec())
            .ok_or_else(|| {
                MidiError::Parse(format!("{} is not a Standard MIDI File", path.display()))
            })?;
        let info = parse_smf(&smf_bytes)?;
        if info.total_ticks == 0 {
            return Err(MidiError::Parse("SMF contains no events".into()));
        }
        let native_map = TempoMap::native(info.ppq, info.tempos.clone());

        // FluidSynth logging: route warnings/errors to our stderr, silence
        // the per-file INFO chatter. Global per process; idempotent.
        for level in 0..=4 {
            unsafe { fluid_set_log_function(level, Some(fluid_log_hook), std::ptr::null_mut()) };
        }

        let sf_path_c = CString::new(soundfont_path.to_str().ok_or_else(|| {
            MidiError::SoundFont(soundfont_path.to_path_buf(), "path is not UTF-8".into())
        })?)
        .map_err(|_| {
            MidiError::SoundFont(soundfont_path.to_path_buf(), "path contains NUL".into())
        })?;

        unsafe {
            let settings = new_fluid_settings();
            if settings.is_null() {
                return Err(MidiError::Fluid("new_fluid_settings"));
            }
            let _ = fluid_settings_setnum(settings, c"synth.sample-rate".as_ptr(), out_rate as f64);
            // Reference app runs polyphony 512.
            let _ = fluid_settings_setint(settings, c"synth.polyphony".as_ptr(), 512);
            // Default; set explicitly — the whole design relies on the
            // player being clocked by rendered samples, not wall time.
            let _ = fluid_settings_setstr(
                settings,
                c"player.timing-source".as_ptr(),
                c"sample".as_ptr(),
            );

            let synth = new_fluid_synth(settings);
            if synth.is_null() {
                delete_fluid_settings(settings);
                return Err(MidiError::Fluid("new_fluid_synth"));
            }
            let sf_id = fluid_synth_sfload(synth, sf_path_c.as_ptr(), 1);
            if sf_id < 0 {
                delete_fluid_synth(synth);
                delete_fluid_settings(settings);
                return Err(MidiError::SoundFont(
                    soundfont_path.to_path_buf(),
                    "fluid_synth_sfload failed".into(),
                ));
            }

            let player = new_fluid_player(synth);
            if player.is_null() {
                delete_fluid_synth(synth);
                delete_fluid_settings(settings);
                return Err(MidiError::Fluid("new_fluid_player"));
            }

            let mut hook = Box::new(HookCtx {
                synth,
                semitones: std::sync::atomic::AtomicI32::new(0),
            });
            let cleanup = |player: *mut fluid_player_t,
                           synth: *mut fluid_synth_t,
                           settings: *mut fluid_settings_t| {
                delete_fluid_player(player);
                delete_fluid_synth(synth);
                delete_fluid_settings(settings);
            };
            if fluid_player_set_playback_callback(
                player,
                playback_hook,
                hook.as_mut() as *mut HookCtx as *mut c_void,
            ) < 0
            {
                cleanup(player, synth, settings);
                return Err(MidiError::Fluid("fluid_player_set_playback_callback"));
            }
            if fluid_player_add_mem(player, smf_bytes.as_ptr() as *const c_void, smf_bytes.len())
                < 0
            {
                cleanup(player, synth, settings);
                return Err(MidiError::Fluid("fluid_player_add_mem"));
            }
            if fluid_player_play(player) < 0 {
                cleanup(player, synth, settings);
                return Err(MidiError::Fluid("fluid_player_play"));
            }

            let mut engine = MidiEngine {
                rate: out_rate,
                smf_bytes,
                info,
                map: native_map.clone(),
                native_map,
                bpm_override: None,
                loop_on: false,
                eof_latched: false,
                frames: 0,
                settings,
                synth,
                player,
                sf_id: Some(sf_id),
                sf_path: soundfont_path.to_path_buf(),
                hook: Some(hook),
                metronome: false,
                click_high: load_click(&metronome_asset(METRONOME_HIGH_WAV), 1600.0, out_rate),
                click_low: load_click(&metronome_asset(METRONOME_LOW_WAV), 900.0, out_rate),
                scratch: vec![0.0; 64],
            };

            // Prime: one tiny render forces the lazy playlist load so
            // division/total_ticks become valid; the rewind below replays the
            // priming audio for the caller.
            let (l, r) = engine.scratch.split_at_mut(32);
            if fluid_synth_write_float(
                engine.synth,
                32,
                l.as_mut_ptr() as *mut c_void,
                0,
                1,
                r.as_mut_ptr() as *mut c_void,
                0,
                1,
            ) != 0
            {
                return Err(MidiError::Fluid("fluid_synth_write_float (prime)"));
            }
            let division = fluid_player_get_division(engine.player);
            let total_ticks = fluid_player_get_total_ticks(engine.player);
            if division != engine.info.ppq as c_int {
                return Err(MidiError::Fluid("player division mismatch (parser bug?)"));
            }
            if (total_ticks as i64 - engine.info.total_ticks as i64).abs() > 2 {
                return Err(MidiError::Fluid(
                    "player total_ticks mismatch (parser bug?)",
                ));
            }
            if fluid_player_seek(engine.player, 0) < 0 {
                return Err(MidiError::Fluid("fluid_player_seek (rewind after prime)"));
            }
            Ok(engine)
        }
    }

    /// Render `left.len()` stereo frames (de-interleaved; both slices must
    /// have equal length). Returns frames written — `0` once EOF has fully
    /// latched (timeline end + reverb tail drained).
    pub fn render(&mut self, left: &mut [f32], right: &mut [f32]) -> usize {
        assert_eq!(
            left.len(),
            right.len(),
            "render needs equal-length stereo slices"
        );
        let n = left.len();
        if n == 0 || self.eof_latched {
            return 0;
        }
        assert!(
            n <= i32::MAX as usize,
            "render chunk too large for FluidSynth c_int"
        );

        unsafe {
            let rc = fluid_synth_write_float(
                self.synth,
                n as c_int,
                left.as_mut_ptr() as *mut c_void,
                0,
                1,
                right.as_mut_ptr() as *mut c_void,
                0,
                1,
            );
            assert_eq!(rc, 0, "fluid_synth_write_float failed");
        }
        if self.metronome {
            self.mix_metronome(left, right);
        }
        self.frames += n as u64;

        if !self.loop_on && unsafe { fluid_player_get_status(self.player) } == FLUID_PLAYER_DONE {
            let dur_frames = self.duration_frames();
            let min_tail = dur_frames + (TAIL_MIN_S * self.rate as f64) as u64;
            let max_tail = dur_frames + (TAIL_MAX_S * self.rate as f64) as u64;
            let voices = unsafe { fluid_synth_get_active_voice_count(self.synth) };
            if self.frames >= max_tail || (self.frames >= min_tail && voices == 0) {
                self.eof_latched = true;
            }
        }
        n
    }

    /// True when the timeline and the reverb tail have fully drained and
    /// `render` will keep returning 0 (until `seek` restarts playback).
    pub fn eof(&self) -> bool {
        self.eof_latched
    }

    /// Duration in seconds under the active tempo map (native or overridden).
    pub fn duration(&self) -> f64 {
        self.map.tick_to_secs(self.info.total_ticks as f64)
    }

    /// Playback position in seconds (wrapped to the timeline when looping).
    /// Seconds are relative to the ACTIVE tempo map — when `set_bpm` switches
    /// the map, `position()` and `duration()` rescale together; the underlying
    /// tick continuity is visible via `position_ticks()`.
    pub fn position(&self) -> f64 {
        let tick = self.player_tick_wrapped();
        self.map.tick_to_secs(tick)
    }

    /// Playback position in file ticks (PPQ), wrapped to the timeline when
    /// looping. Continuous across tempo-map switches and seeks.
    pub fn position_ticks(&self) -> u32 {
        self.player_tick_wrapped() as u32
    }

    /// Seek to `seconds` (clamped to `0..=duration`). Takes effect at the
    /// next render call; safe to call right after `open` and after EOF.
    pub fn seek(&mut self, seconds: f64) {
        let s = seconds.clamp(0.0, self.duration());
        let tick = (self.map.secs_to_tick(s).round() as i64).clamp(0, self.info.total_ticks as i64)
            as c_int;
        self.eof_latched = false;
        unsafe {
            // After EOF the player is DONE: restart it first (play() from
            // DONE rewinds to the playlist start; the seek repositions).
            if fluid_player_get_status(self.player) == FLUID_PLAYER_DONE {
                let _ = fluid_player_play(self.player);
            }
            let mut rc = fluid_player_seek(self.player, tick);
            if rc < 0 {
                // A previous seek was still queued (no render consumed it).
                // Consume it with a tiny render, then retry once.
                let (l, r) = self.scratch.split_at_mut(32);
                let _ = fluid_synth_write_float(
                    self.synth,
                    32,
                    l.as_mut_ptr() as *mut c_void,
                    0,
                    1,
                    r.as_mut_ptr() as *mut c_void,
                    0,
                    1,
                );
                rc = fluid_player_seek(self.player, tick);
            }
            assert!(rc >= 0, "fluid_player_seek({tick}) failed twice");
        }
        // Re-anchor the timeline; the ≤32-frame micro-render above is
        // replaced by the seek-chase at the next render call.
        self.frames = (s * self.rate as f64) as u64;
    }

    /// Loop the whole sequence forever, or stop looping at the timeline end.
    pub fn set_loop(&mut self, on: bool) {
        self.loop_on = on;
        unsafe {
            let _ = fluid_player_set_loop(self.player, if on { -1 } else { 0 });
        }
    }

    pub fn looping(&self) -> bool {
        self.loop_on
    }

    /// Silence active voices before changing playback state. In particular,
    /// transposing an already-playing note's future note-off would otherwise
    /// target a different key and leave the original voice stuck. Sounds-off
    /// is deliberate: notes-off leaves the old voice audible in release.
    fn silence_active_notes(&mut self) {
        let rc = unsafe { fluid_synth_all_sounds_off(self.synth, -1) };
        assert_eq!(rc, 0, "fluid_synth_all_sounds_off failed");
    }

    /// Transpose melodic channels by `semitones` (percussion channel 9 is
    /// skipped; transposed keys clamp to 0..127). Live — no restart.
    pub fn set_transpose(&mut self, semitones: i32) {
        if self.transpose() == semitones {
            return;
        }
        self.silence_active_notes();
        if let Some(hook) = &self.hook {
            hook.semitones
                .store(semitones, std::sync::atomic::Ordering::Relaxed);
        }
    }

    pub fn transpose(&self) -> i32 {
        self.hook
            .as_ref()
            .map(|h| h.semitones.load(std::sync::atomic::Ordering::Relaxed))
            .unwrap_or(0)
    }

    /// Override the tempo with a constant `bpm` (clamped 5..=300), or `None`
    /// for the file's own tempo map. Position (in ticks) is preserved across
    /// the switch; reported duration/position rescale to the new map.
    pub fn set_bpm(&mut self, bpm: Option<f32>) {
        let clamped = bpm.map(|b| b.clamp(5.0, 300.0));
        if self.bpm_override == clamped {
            return;
        }
        self.silence_active_notes();
        let tick = self.player_tick_wrapped();
        unsafe {
            match clamped {
                Some(b) => {
                    let _ = fluid_player_set_tempo(
                        self.player,
                        FLUID_PLAYER_TEMPO_EXTERNAL_BPM,
                        b as f64,
                    );
                    self.map = TempoMap::constant(self.info.ppq, b as f64);
                }
                None => {
                    let _ = fluid_player_set_tempo(self.player, FLUID_PLAYER_TEMPO_INTERNAL, 1.0);
                    self.map = self.native_map.clone();
                }
            }
        }
        self.frames = (self.map.tick_to_secs(tick) * self.rate as f64) as u64;
        self.bpm_override = clamped;
    }

    /// Active BPM override, if any.
    pub fn bpm_override(&self) -> Option<f32> {
        self.bpm_override
    }

    /// Native tempo at the current position (BPM), from the file's tempo map.
    pub fn native_bpm(&self) -> f64 {
        let tick = self.player_tick_wrapped();
        60_000_000.0 / self.native_map.usqn_at(tick)
    }

    /// Mix the metronome click over rendered audio (high on bar starts).
    pub fn set_metronome(&mut self, on: bool) {
        if self.metronome == on {
            return;
        }
        self.silence_active_notes();
        self.metronome = on;
    }

    pub fn metronome(&self) -> bool {
        self.metronome
    }

    /// Apply a complete native parameter set (transpose / BPM override /
    /// metronome). Live on every control — no restart, position preserved.
    /// Mirrors `TrackerEngine::apply_params` for the decoder thread's
    /// `Cmd::SetParams` arm.
    pub fn apply_params(&mut self, params: MidiParams) {
        self.set_transpose(params.transpose_semitones);
        if self.bpm_override() != params.bpm_override {
            self.set_bpm(params.bpm_override);
        }
        self.set_metronome(params.metronome);
    }

    /// Switch the SoundFont at runtime. Active notes are silenced, the new
    /// font is loaded and all channel bank/programs re-selected from it; the
    /// old font is unloaded lazily. Position is preserved.
    pub fn set_soundfont(&mut self, path: &Path) -> Result<(), MidiError> {
        let path_c =
            CString::new(path.to_str().ok_or_else(|| {
                MidiError::SoundFont(path.to_path_buf(), "path is not UTF-8".into())
            })?)
            .map_err(|_| MidiError::SoundFont(path.to_path_buf(), "path contains NUL".into()))?;
        unsafe {
            let new_id = fluid_synth_sfload(self.synth, path_c.as_ptr(), 1);
            if new_id < 0 {
                return Err(MidiError::SoundFont(
                    path.to_path_buf(),
                    "fluid_synth_sfload failed".into(),
                ));
            }
            self.silence_active_notes();
            if let Some(old) = self.sf_id {
                // reset_presets=0: keep old-font voices until they release.
                if fluid_synth_sfunload(self.synth, old, 0) < 0 {
                    eprintln!(
                        "[midi] lazy unload of old font id {old} failed — keeping both loaded"
                    );
                }
            }
            self.sf_id = Some(new_id);
        }
        self.sf_path = path.to_path_buf();
        Ok(())
    }

    pub fn soundfont_path(&self) -> &Path {
        &self.sf_path
    }

    /// Parsed SMF facts (tempo map, time signatures, native duration) for UI
    /// and monitoring.
    pub fn info(&self) -> &SmfInfo {
        &self.info
    }

    pub fn ppq(&self) -> u32 {
        self.info.ppq
    }

    pub fn total_ticks(&self) -> u32 {
        self.info.total_ticks
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// Raw SMF bytes as loaded (for hashing, metadata probes, re-parses).
    pub fn smf_bytes(&self) -> &[u8] {
        &self.smf_bytes
    }

    // -- internals ---------------------------------------------------------

    fn duration_frames(&self) -> u64 {
        (self.duration() * self.rate as f64) as u64
    }

    /// Player tick wrapped into `0..total_ticks` (position within the loop).
    fn player_tick_wrapped(&self) -> f64 {
        let t = unsafe { fluid_player_get_current_tick(self.player) };
        let total = (self.info.total_ticks as f64).max(1.0);
        (t.max(0) as f64) % total
    }

    /// Mix clicks over the just-rendered chunk. Stateless per call: the beat
    /// grid is derived from the chunk's tick span, so seeks and loop wraps
    /// need no cleanup.
    fn mix_metronome(&mut self, left: &mut [f32], right: &mut [f32]) {
        let n = left.len();
        let total = self.info.total_ticks as f64;
        let rate = self.rate as f64;
        // Tick span of this chunk from the frame timeline (the player follows
        // the same active map).
        let t0 = self.map.secs_to_tick(self.frames as f64 / rate);
        let t1 = self
            .map
            .secs_to_tick((self.frames + n as u64) as f64 / rate);
        let span = t1 - t0;
        if span <= 0.0 {
            return;
        }
        // Position within the loop at chunk start.
        let mut w0 = t0 % total;
        let ticks_per_sec = span / (n as f64 / rate);

        let grid = beat_grid(self.info.ppq, &self.info.time_signatures);
        let (high, low) = (&self.click_high, &self.click_low);
        let mut cursor = 0usize; // frames already covered

        while cursor < n {
            let w_now = w0 + (span * cursor as f64 / n as f64);
            let remaining_ticks = span * (1.0 - cursor as f64 / n as f64);
            let to_wrap = total - w_now;
            let step_ticks = remaining_ticks.min(to_wrap.max(0.0));
            let b1 = w_now + step_ticks;

            // Time-signature segments intersecting [w_now, b1).
            for (idx, &(ts_tick, numer, beat_ticks)) in grid.iter().enumerate() {
                let seg_end = grid.get(idx + 1).map(|e| e.0).unwrap_or(total);
                if seg_end <= w_now || ts_tick >= b1 {
                    continue;
                }
                let from = w_now.max(ts_tick);
                let mut k = ((from - ts_tick) / beat_ticks).ceil().max(0.0);
                while k.is_finite() {
                    let beat = ts_tick + k * beat_ticks;
                    if beat >= b1 || beat >= seg_end || beat >= total {
                        break;
                    }
                    if beat >= w_now {
                        let rel = (beat - w0) / span;
                        let frame = (rel * n as f64) as usize;
                        if frame < n {
                            let accent = (k as i64).rem_euclid(numer.max(1.0) as i64) == 0;
                            let click = if accent { high } else { low };
                            let gain = if accent {
                                METRONOME_HIGH_GAIN
                            } else {
                                METRONOME_LOW_GAIN
                            };
                            let len = click.samples.len().min(n - frame);
                            for i in 0..len {
                                let v = click.samples[i] * gain;
                                left[frame + i] = (left[frame + i] + v).clamp(-1.0, 1.0);
                                right[frame + i] = (right[frame + i] + v).clamp(-1.0, 1.0);
                            }
                        }
                    }
                    k += 1.0;
                }
            }

            let step_frames = (step_ticks / ticks_per_sec * rate).round() as usize;
            if step_frames == 0 || cursor + step_frames >= n {
                break;
            }
            cursor += step_frames;
            // Loop wrap: the beat grid restarts from tick 0.
            w0 = 0.0;
        }
    }
}

/// Beat grid from time signatures: (ts_tick, numerator, beat_ticks) with
/// beat_ticks = ppq·4/denominator. Defaults to 4/4 at tick 0 when the file
/// sets none — matching the reference worklet.
fn beat_grid(ppq: u32, sigs: &[TimeSig]) -> Vec<(f64, f64, f64)> {
    let ppq = ppq as f64;
    let sigs = if sigs.is_empty() {
        vec![TimeSig {
            tick: 0,
            numerator: 4,
            denominator: 4,
        }]
    } else {
        let mut sigs = sigs.to_vec();
        if sigs.first().map(|t| t.tick) != Some(0) {
            sigs.insert(
                0,
                TimeSig {
                    tick: 0,
                    numerator: 4,
                    denominator: 4,
                },
            );
        }
        sigs
    };
    sigs.into_iter()
        .map(|ts| {
            let den = ts.denominator.max(1) as f64;
            (ts.tick as f64, ts.numerator.max(1) as f64, ppq * 4.0 / den)
        })
        .collect()
}

impl Drop for MidiEngine {
    fn drop(&mut self) {
        unsafe {
            if !self.player.is_null() {
                let _ = fluid_player_stop(self.player);
                delete_fluid_player(self.player);
            }
            // After the player is gone its callback can no longer fire; now
            // the HookCtx box (raw to FluidSynth until here) may drop.
            self.hook = None;
            if !self.synth.is_null() {
                delete_fluid_synth(self.synth);
            }
            if !self.settings.is_null() {
                delete_fluid_settings(self.settings);
            }
        }
    }
}

// The engine owns FluidSynth handles: single-thread by contract; the raw
// pointers make it !Send/!Sync automatically (no impls added).

// ---------------------------------------------------------------------------
// Tests — pure parser/map tests run everywhere; `#[ignore]` live tests need
// the reference SoundFont + Y:\Test Files media and run via the scratch
// harness (target/midiwork/miditest) with `cargo test -- --ignored`.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ---- synthetic SMF builder -------------------------------------------

    fn varlen(v: u32) -> Vec<u8> {
        let mut out = vec![(v & 0x7f) as u8];
        let mut v = v >> 7;
        while v > 0 {
            out.push(0x80 | (v & 0x7f) as u8);
            v >>= 7;
        }
        out.reverse();
        out
    }

    fn chunk(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut c = id.to_vec();
        c.extend_from_slice(&(body.len() as u32).to_be_bytes());
        c.extend_from_slice(body);
        c
    }

    /// Format-1 SMF, ppq 480: conductor track with 120?240 bpm at tick 480,
    /// 3/4 at tick 0; one voice track with running-status notes.
    fn synth_smf() -> Vec<u8> {
        let mut head = vec![0, 1]; // format 1
        head.extend_from_slice(&2u16.to_be_bytes()); // ntrks
        head.extend_from_slice(&480u16.to_be_bytes()); // ppq
        let mut smf = chunk(b"MThd", &head);

        let mut t0 = Vec::new();
        t0.extend(varlen(0)); // @0: 4/4... actually 3/4
        t0.extend([0xFF, 0x58, 0x04, 3, 2, 24, 8]);
        t0.extend(varlen(0)); // @0: 120 bpm
        t0.extend([0xFF, 0x51, 0x03, 0x07, 0xA1, 0x20]); // 500000
        t0.extend(varlen(480)); // @480: 240 bpm
        t0.extend([0xFF, 0x51, 0x03, 0x03, 0xD0, 0x90]); // 250000
        t0.extend(varlen(480)); // @960: EOT
        t0.extend([0xFF, 0x2F, 0x00]);
        smf.extend(chunk(b"MTrk", &t0));

        let mut t1 = Vec::new();
        t1.extend(varlen(0));
        t1.extend([0x90, 60, 100]); // note on C4
        t1.extend(varlen(240));
        t1.extend([0x50]); // running status note off... (0x50 = key 80) — build running status manually below instead
        t1.clear();
        t1.extend(varlen(0));
        t1.extend([0x90, 60, 100]);
        t1.extend(varlen(240));
        t1.extend([80, 0]); // running status: key 80, vel 0 (= note off)
        t1.extend(varlen(240));
        t1.extend([0x90, 62, 90]); // explicit status again
        t1.extend(varlen(480));
        t1.extend([0x82, 62, 0]); // explicit note off
        t1.extend(varlen(0));
        t1.extend([0xFF, 0x2F, 0x00]); // EOT @960
        smf.extend(chunk(b"MTrk", &t1));
        smf
    }

    #[test]
    fn parse_synthetic_smf() {
        let info = parse_smf(&synth_smf()).expect("parse");
        assert_eq!(info.format, 1);
        assert_eq!(info.track_count, 2);
        assert_eq!(info.ppq, 480);
        assert_eq!(info.total_ticks, 960);
        assert_eq!(info.tempos, vec![(0, 500_000), (480, 250_000)]);
        assert_eq!(
            info.time_signatures,
            vec![TimeSig {
                tick: 0,
                numerator: 3,
                denominator: 4
            }]
        );
    }

    #[test]
    fn tempo_map_math_native() {
        let map = TempoMap::native(480, vec![(0, 500_000), (480, 250_000)]);
        assert!((map.tick_to_secs(0.0)).abs() < 1e-12);
        // 480 ticks @500000us/qn, 480 ppq = exactly 0.5 s
        assert!((map.tick_to_secs(480.0) - 0.5).abs() < 1e-9);
        // + 480 ticks @250000 = +0.25 s
        assert!((map.tick_to_secs(960.0) - 0.75).abs() < 1e-9);
        // inverse roundtrip across segments
        for t in [0.0, 100.0, 479.5, 480.0, 700.0, 960.0] {
            let s = map.tick_to_secs(t);
            assert!((map.secs_to_tick(s) - t).abs() < 1e-6, "roundtrip at {t}");
        }
    }

    #[test]
    fn tempo_map_default_and_constant() {
        let map = TempoMap::native(96, vec![]); // no tempo events ? 120 bpm
        assert!((map.tick_to_secs(96.0) - 0.5).abs() < 1e-9);
        let m = TempoMap::constant(96, 240.0);
        assert!((m.tick_to_secs(96.0) - 0.25).abs() < 1e-9);
        assert!((m.usqn_at(1.0e9) - 250_000.0).abs() < 1e-9);
    }

    #[test]
    fn tempo_at_later_tick_overrides() {
        let info = parse_smf(&synth_smf()).unwrap();
        let map = TempoMap::native(info.ppq, info.tempos.clone());
        assert!((map.usqn_at(0.0) - 500_000.0).abs() < 1e-9);
        assert!((map.usqn_at(1000.0) - 250_000.0).abs() < 1e-9);
    }

    #[test]
    fn rejects_bad_smf() {
        assert!(parse_smf(b"").is_err());
        assert!(parse_smf(b"RIFF____").is_err());
        // format 2
        let mut smf = synth_smf();
        smf[8] = 0;
        smf[9] = 2;
        assert!(parse_smf(&smf).is_err());
        // SMPTE division
        let mut smf = synth_smf();
        smf[12] = 0xE7; // -25 fps
        smf[13] = 40;
        assert!(parse_smf(&smf).is_err());
    }

    #[test]
    fn midi_file_eligibility() {
        for ext in ["mid", "MID", "midi", "MiDi", "rmi", "RMI"] {
            assert!(is_midi_file(Path::new(&format!("song.{ext}"))), "{ext}");
        }
        for ext in ["mp3", "mod", "wav", "sf2", "txt"] {
            assert!(!is_midi_file(Path::new(&format!("song.{ext}"))), "{ext}");
        }
        assert!(!is_midi_file(Path::new("no_extension")));
    }

    #[test]
    fn rmid_wrapper_is_unwrapped_to_the_smf() {
        let smf = synth_smf();
        // RIFF <size> RMID + one pad-less "data" chunk holding the SMF.
        let data_len = smf.len() as u32;
        let mut rmid: Vec<u8> = Vec::new();
        rmid.extend(b"RIFF");
        rmid.extend(&((4 + 8 + data_len) as u32).to_le_bytes());
        rmid.extend(b"RMID");
        rmid.extend(b"data");
        rmid.extend(&data_len.to_le_bytes());
        rmid.extend_from_slice(&smf);
        let unwrapped = unwrap_rmid(&rmid).expect("RMID must unwrap");
        assert_eq!(unwrapped, &smf[..]);
        assert_eq!(parse_smf(unwrapped).unwrap().ppq, 480);
        // Odd-sized leading chunk: the +1 pad byte must be skipped.
        let mut padded: Vec<u8> = Vec::new();
        padded.extend(b"RIFF");
        padded.extend(&(4 + 8 + 3 + 1 + 8 + data_len).to_le_bytes());
        padded.extend(b"RMID");
        padded.extend(b"junk");
        padded.extend(&3u32.to_le_bytes());
        padded.extend_from_slice(b"abc"); // odd size
        padded.push(0); // RIFF pad
        padded.extend(b"data");
        padded.extend(&data_len.to_le_bytes());
        padded.extend_from_slice(&smf);
        assert_eq!(unwrap_rmid(&padded).unwrap(), &smf[..]);
        // Not a wrapper: bytes pass through untouched.
        assert_eq!(unwrap_rmid(&smf), Some(&smf[..]));
        // Claims RMID but has no data chunk: rejected.
        let mut broken = Vec::new();
        broken.extend(b"RIFF");
        broken.extend(&4u32.to_le_bytes());
        broken.extend(b"RMID");
        assert!(unwrap_rmid(&broken).is_none());
    }

    #[test]
    fn neutral_midi_params_default() {
        assert_eq!(MidiParams::default().transpose_semitones, 0);
        assert_eq!(MidiParams::default().bpm_override, None);
        assert!(!MidiParams::default().metronome);
    }

    #[test]
    fn beat_grid_defaults_and_signatures() {
        assert_eq!(beat_grid(480, &[]), vec![(0.0, 4.0, 480.0)]);
        let sigs = [TimeSig {
            tick: 480,
            numerator: 7,
            denominator: 8,
        }];
        // implicit 4/4 at 0, explicit 7/8 at 480 (beat = 480*4/8 = 240)
        assert_eq!(
            beat_grid(480, &sigs),
            vec![(0.0, 4.0, 480.0), (480.0, 7.0, 240.0)]
        );
    }

    #[test]
    fn wav_mono_parse() {
        // stereo 16-bit PCM, 2 frames: L=0x4000, R=0x2000 / L=0, R=0
        let mut wav: Vec<u8> = Vec::new();
        wav.extend(b"RIFF");
        wav.extend(&36u32.to_le_bytes());
        wav.extend(b"WAVE");
        wav.extend(b"fmt ");
        wav.extend(&16u32.to_le_bytes());
        wav.extend(&1u16.to_le_bytes()); // PCM
        wav.extend(&2u16.to_le_bytes()); // channels
        wav.extend(&44_100u32.to_le_bytes());
        wav.extend(&176_400u32.to_le_bytes());
        wav.extend(&4u16.to_le_bytes());
        wav.extend(&16u16.to_le_bytes());
        wav.extend(b"data");
        wav.extend(&8u32.to_le_bytes());
        wav.extend(&0x4000i16.to_le_bytes());
        wav.extend(&0x2000i16.to_le_bytes());
        wav.extend(&0i16.to_le_bytes());
        wav.extend(&0i16.to_le_bytes());
        let (mono, rate) = parse_wav_mono(&wav).expect("wav");
        assert_eq!(rate, 44_100);
        assert_eq!(mono.len(), 2);
        assert!((mono[0] - (0x4000 as f32 / 32768.0 + 0x2000 as f32 / 32768.0) / 2.0).abs() < 1e-6);
        assert_eq!(mono[1], 0.0);
    }

    #[test]
    fn resample_ratio() {
        let src = vec![0.0f32, 0.5, 1.0];
        let out = resample_mono(&src, 96_000.0, 48_000.0);
        assert_eq!(out.len(), 2);
        assert!((out[0] - 0.0).abs() < 1e-6, "first output samples src[0]");
        assert!(
            (out[1] - 1.0).abs() < 1e-6,
            "decimation by 2 samples positions 0.0 and 2.0"
        );
        let same = resample_mono(&src, 48_000.0, 48_000.0);
        assert_eq!(same, src);
    }

    // ---- live tests (reference assets required) --------------------------

    const RATE: u32 = 48_000;
    const CHUNK: usize = 4096;

    fn sustained_smf() -> Vec<u8> {
        let mut head = vec![0, 0];
        head.extend_from_slice(&1u16.to_be_bytes());
        head.extend_from_slice(&480u16.to_be_bytes());
        let mut smf = chunk(b"MThd", &head);
        let mut track = Vec::new();
        track.extend(varlen(0));
        track.extend([0xFF, 0x51, 0x03, 0x07, 0xA1, 0x20]);
        track.extend(varlen(0));
        track.extend([0x90, 60, 100]);
        track.extend(varlen(4800));
        track.extend([0x80, 60, 0]);
        track.extend(varlen(0));
        track.extend([0xFF, 0x2F, 0x00]);
        smf.extend(chunk(b"MTrk", &track));
        smf
    }

    fn test_midis() -> Vec<&'static str> {
        vec![
            r"Y:\Test Files\AERITHO.MID",
            r"Y:\Test Files\ALLSTARS.MID",
            r"Y:\Test Files\Guit Blues.mid",
            r"Y:\Test Files\SCHMOOZE.MID",
        ]
    }

    fn open_midi(path: &str) -> MidiEngine {
        MidiEngine::open(Path::new(path), RATE, &default_soundfont())
            .unwrap_or_else(|e| panic!("open {path}: {e}"))
    }

    fn render_secs(e: &mut MidiEngine, secs: f64) -> (f64, f64, usize) {
        let mut peak = 0.0f64;
        let mut sum_sq = 0.0f64;
        let mut frames = 0usize;
        let mut l = vec![0f32; CHUNK];
        let mut r = vec![0f32; CHUNK];
        let total = (secs * RATE as f64) as usize;
        while frames < total {
            let n = e.render(&mut l, &mut r).min(CHUNK.min(total - frames));
            if n == 0 {
                break;
            }
            for i in 0..n {
                peak = peak.max(l[i].abs() as f64).max(r[i].abs() as f64);
                sum_sq += (l[i] as f64 * l[i] as f64 + r[i] as f64 * r[i] as f64) / 2.0;
            }
            frames += n;
        }
        (peak, (sum_sq / frames.max(1) as f64).sqrt(), frames)
    }

    #[test]
    #[ignore = "needs reference SoundFont + Y: test media"]
    fn live_open_render_all_four() {
        for path in test_midis() {
            let mut e = open_midi(path);
            let (peak, rms, frames) = render_secs(&mut e, 5.0);
            assert_eq!(frames, 5 * RATE as usize, "{path}: frames");
            assert!(peak > 0.005, "{path}: silent (peak {peak})");
            assert!(
                e.duration() > 30.0,
                "{path}: implausible duration {}",
                e.duration()
            );
            let pos = e.position();
            assert!((pos - 5.0).abs() < 0.5, "{path}: position {pos} after 5 s");
            eprintln!(
                "{path}: ppq={} total_ticks={} dur={:.1}s 5s: peak={peak:.4} rms={rms:.5}",
                e.ppq(),
                e.total_ticks(),
                e.duration()
            );
        }
    }

    #[test]
    #[ignore = "needs reference SoundFont + Y: test media"]
    fn live_seek() {
        let mut e = open_midi(r"Y:\Test Files\AERITHO.MID");
        render_secs(&mut e, 1.0);
        let target = e.duration() * 0.25;
        e.seek(target);
        let (peak, _, _) = render_secs(&mut e, 3.0);
        assert!(peak > 0.005, "post-seek silent");
        let pos = e.position();
        assert!(
            (pos - (target + 3.0)).abs() < 1.0,
            "position {pos} after seek+3s (target {})",
            target + 3.0
        );
        e.seek(0.0);
        let (peak0, _, _) = render_secs(&mut e, 2.0);
        assert!(peak0 > 0.005, "seek(0) silent");
        assert!(e.position() < 3.0, "seek(0) position {}", e.position());
    }

    #[test]
    #[ignore = "needs reference SoundFont + Y: test media"]
    fn live_transpose() {
        let mut a = open_midi(r"Y:\Test Files\Guit Blues.mid");
        let mut b = open_midi(r"Y:\Test Files\Guit Blues.mid");
        b.set_transpose(5);
        assert_eq!(b.transpose(), 5);
        let n = 3 * RATE as usize;
        let mut la = vec![0f32; CHUNK];
        let mut ra = vec![0f32; CHUNK];
        let mut lb = vec![0f32; CHUNK];
        let mut rb = vec![0f32; CHUNK];
        let mut done = 0usize;
        let mut peak_a = 0.0f32;
        let mut peak_b = 0.0f32;
        let mut diff = 0.0f32;
        while done < n {
            a.render(&mut la, &mut ra);
            b.render(&mut lb, &mut rb);
            for i in 0..CHUNK {
                peak_a = peak_a.max(la[i].abs());
                peak_b = peak_b.max(lb[i].abs());
                diff = diff.max((la[i] - lb[i]).abs());
            }
            done += CHUNK;
        }
        assert!(peak_a > 0.005 && peak_b > 0.005, "silent render");
        assert!(
            diff > 0.01,
            "transpose produced identical audio (diff {diff})"
        );
        assert!(
            (a.duration() - b.duration()).abs() < 1e-6,
            "transpose must not change duration"
        );
    }

    #[test]
    #[ignore = "needs reference SoundFont"]
    fn live_transpose_releases_pre_change_notes() {
        let path = std::env::temp_dir().join(format!(
            "soundapp-midi-notes-off-{}.mid",
            std::process::id()
        ));
        std::fs::write(&path, sustained_smf()).expect("write sustained-note SMF");
        let opened = MidiEngine::open(&path, RATE, &default_soundfont());
        std::fs::remove_file(&path).expect("remove sustained-note SMF");
        let mut e = opened.expect("open sustained-note SMF");
        render_secs(&mut e, 0.5);
        assert!(
            unsafe { fluid_synth_get_active_voice_count(e.synth) } > 0,
            "test needs an active note"
        );
        e.set_transpose(7);
        render_secs(&mut e, 1.0);
        assert_eq!(
            unsafe { fluid_synth_get_active_voice_count(e.synth) },
            0,
            "pre-transpose note remained stuck"
        );
    }

    #[test]
    #[ignore = "needs reference SoundFont + Y: test media"]
    fn live_bpm_override() {
        let mut e = open_midi(r"Y:\Test Files\AERITHO.MID");
        let native = e.duration();
        assert!(native > 10.0);
        e.set_bpm(Some(120.0));
        let expect = e.total_ticks() as f64 * 60.0 / (120.0 * e.ppq() as f64);
        assert!(
            (e.duration() - expect).abs() < 0.01,
            "constant-map duration {} != {expect}",
            e.duration()
        );
        assert_eq!(e.bpm_override(), Some(120.0));
        let (peak, _, _) = render_secs(&mut e, 2.0);
        assert!(peak > 0.005, "bpm override render silent");
        let pos_before = e.position();
        let tick_before = e.position_ticks();
        e.set_bpm(None);
        assert_eq!(e.bpm_override(), None);
        assert!(
            (e.duration() - native).abs() < 0.01,
            "native duration not restored"
        );
        // Seconds rescale with the map; continuity holds in ticks.
        assert_eq!(
            e.position_ticks(),
            tick_before,
            "tick jumped on tempo switch"
        );
        assert!(e.position() != pos_before || native == e.duration());
    }

    #[test]
    #[ignore = "needs reference SoundFont + Y: test media"]
    fn live_soundfont_switch_preserves_position() {
        let alt = std::env::var("APPDATA")
            .ok()
            .map(|appdata| PathBuf::from(appdata).join(r"SoundApp\soundfonts"))
            .and_then(|dir| {
                std::fs::read_dir(&dir)
                    .ok()?
                    .filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .find(|p| {
                        p.extension()
                            .map(|x| x.eq_ignore_ascii_case("sf2"))
                            .unwrap_or(false)
                    })
            })
            .expect("no alternate SoundFont in %APPDATA%\\SoundApp\\soundfonts");
        let mut e = open_midi(r"Y:\Test Files\ALLSTARS.MID");
        render_secs(&mut e, 1.0);
        let pos = e.position();
        e.set_soundfont(&alt).expect("soundfont switch");
        assert_eq!(e.soundfont_path(), alt);
        assert!(
            (e.position() - pos).abs() < 0.25,
            "position changed on font switch"
        );
        let (peak, _, _) = render_secs(&mut e, 2.0);
        assert!(peak > 0.005, "silent after font switch");
    }

    #[test]
    #[ignore = "needs reference SoundFont + Y: test media"]
    fn live_eof_tail_and_restart() {
        let mut e = open_midi(r"Y:\Test Files\Guit Blues.mid");
        assert!(!e.looping());
        let dur = e.duration();
        let mut l = vec![0f32; CHUNK];
        let mut r = vec![0f32; CHUNK];
        let mut frames = 0usize;
        while !e.eof() {
            let n = e.render(&mut l, &mut r);
            if n == 0 {
                break;
            }
            frames += n;
            assert!(
                frames < (dur + 10.0) as usize * RATE as usize,
                "EOF never latched"
            );
        }
        assert!(e.eof(), "eof not latched");
        let tail = frames as f64 / RATE as f64 - dur;
        // FluidSynth's end-of-song reverb grace lands around 3 s; cap at 5.
        assert!(
            (-0.05..=TAIL_MAX_S + 0.5).contains(&tail),
            "tail {tail}s out of range"
        );
        assert_eq!(
            e.render(&mut l, &mut r),
            0,
            "render after EOF must return 0"
        );
        // restart via seek
        e.seek(dur * 0.5);
        assert!(!e.eof());
        let (peak, _, _) = render_secs(&mut e, 2.0);
        assert!(peak > 0.005, "silent after post-EOF seek");
        eprintln!("Guit Blues: dur={dur:.2}s tail={tail:.3}s frames={frames}");
    }

    #[test]
    #[ignore = "needs reference SoundFont + Y: test media"]
    fn live_loop_wraps() {
        let mut e = open_midi(r"Y:\Test Files\Guit Blues.mid");
        e.set_loop(true);
        assert!(e.looping());
        let dur = e.duration();
        let (_, _, frames) = render_secs(&mut e, dur + 3.0);
        assert_eq!(
            frames,
            ((dur + 3.0) * RATE as f64) as usize,
            "looped render underran"
        );
        assert!(!e.eof(), "loop must not EOF");
        assert!(
            e.position() >= 0.0 && e.position() < dur,
            "loop position {} not wrapped",
            e.position()
        );
        let (peak, _, _) = render_secs(&mut e, 1.0);
        assert!(peak > 0.005, "silent after loop wrap");
    }

    #[test]
    #[ignore = "needs reference SoundFont + Y: test media + metronome WAVs"]
    fn live_metronome_clicks() {
        let mut base = open_midi(r"Y:\Test Files\Guit Blues.mid");
        let (_, _, _) = render_secs(&mut base, 0.0); // no-op keep symmetric
        let mut l = vec![0f32; CHUNK];
        let mut r = vec![0f32; CHUNK];
        let mut base_frames: Vec<f32> = Vec::new();
        let n = 4 * RATE as usize;
        while base_frames.len() < n {
            let got = base.render(&mut l, &mut r);
            assert!(got > 0);
            base_frames.extend_from_slice(&l[..got]);
        }
        let mut with = open_midi(r"Y:\Test Files\Guit Blues.mid");
        with.set_metronome(true);
        let mut click_frames: Vec<f32> = Vec::new();
        while click_frames.len() < n {
            let got = with.render(&mut l, &mut r);
            assert!(got > 0);
            click_frames.extend_from_slice(&l[..got]);
        }
        let mut added = 0usize;
        let mut max_added = 0.0f32;
        for i in 0..n {
            let d = (click_frames[i] - base_frames[i]).abs();
            if d > 0.05 {
                added += 1;
            }
            max_added = max_added.max(d);
        }
        assert!(
            max_added > 0.2,
            "metronome click not audible (max delta {max_added})"
        );
        assert!(
            added > 100,
            "too few click samples ({added}) for 4 s of beats"
        );
        eprintln!("metronome: {added} click samples, max delta {max_added:.3}");
    }
}
