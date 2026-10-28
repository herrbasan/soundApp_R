//! Independent multi-track Mixer output graph.
//!
//! Each stem owns a streaming FFmpeg decoder and a four-second ring. One cpal
//! callback mixes all stems on a shared transport clock; no main-player state
//! or audio rings are shared.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, StreamTrait};
use rtrb::RingBuffer;

use crate::audio::FFmpegStream;

pub const MAX_STEMS: usize = 20;
const RING_SECONDS: usize = 4;
const PREBUFFER_SECS: f64 = 0.200;
const FADE_SECS: f32 = 0.012;

struct SlotShared {
    enabled: AtomicBool,
    ready: AtomicBool,
    gain: AtomicU32,
    pan: AtomicU32,
    muted: AtomicBool,
    meter: AtomicU32,
    duration_frames: AtomicU64,
    flush_gen: AtomicU64,
    flush_ack: AtomicU64,
    played_frames: AtomicU64,
    underrun_frames: AtomicU64,
    ring_frames: AtomicU64,
    start_frames: AtomicU64,
}

impl SlotShared {
    fn new() -> Self {
        Self {
            enabled: AtomicBool::new(false),
            ready: AtomicBool::new(false),
            gain: AtomicU32::new(1.0f32.to_bits()),
            pan: AtomicU32::new(0.0f32.to_bits()),
            muted: AtomicBool::new(false),
            meter: AtomicU32::new(0.0f32.to_bits()),
            duration_frames: AtomicU64::new(0),
            flush_gen: AtomicU64::new(0),
            flush_ack: AtomicU64::new(0),
            played_frames: AtomicU64::new(0),
            underrun_frames: AtomicU64::new(0),
            ring_frames: AtomicU64::new(0),
            start_frames: AtomicU64::new(0),
        }
    }
}

struct Shared {
    playing: AtomicBool,
    play_pending: AtomicBool,
    looping: AtomicBool,
    master: AtomicU32,
    position_frames: AtomicU64,
    transport_frames: AtomicU64,
    transport_gen: AtomicU64,
    shutdown: AtomicBool,
    slots: Vec<SlotShared>,
}

/// Per-stem synchronization telemetry sampled from the Mixer's shared state.
#[derive(Clone, Debug)]
pub struct StemSyncSnapshot {
    pub slot: usize,
    pub enabled: bool,
    pub ready: bool,
    pub muted: bool,
    pub played_frames: u64,
    pub underrun_frames: u64,
    pub ring_frames: u64,
    pub start_frames: u64,
}

impl StemSyncSnapshot {
    pub fn drift_frames(&self, transport_frames: u64) -> i64 {
        transport_frames as i64 - self.played_frames as i64
    }
}

#[derive(Clone, Debug)]
pub struct MixerSyncSnapshot {
    pub rate: u32,
    pub position_secs: f64,
    pub transport_frames: u64,
    pub stems: Vec<StemSyncSnapshot>,
}

enum StemCmd {
    Load(PathBuf, f64),
    Seek(f64),
    Clear,
}

enum CallbackCmd {
    Attach(usize, rtrb::Consumer<f32>),
}

pub struct MixerAudio {
    _stream: cpal::Stream,
    pub rate: u32,
    pub channels: u16,
    shared: Arc<Shared>,
    stem_tx: Vec<Option<mpsc::Sender<StemCmd>>>,
    callback_tx: mpsc::Sender<CallbackCmd>,
}

impl MixerAudio {
    pub fn start() -> Result<Self, String> {
        Self::start_with(None)
    }

    pub fn start_with(device_name: Option<&str>) -> Result<Self, String> {
        let (device, config) = crate::audio::output_target(device_name)?;
        let rate = config.sample_rate();
        let channels = config.channels();
        let out_ch = channels as usize;
        let shared = Arc::new(Shared {
            playing: AtomicBool::new(false),
            play_pending: AtomicBool::new(false),
            looping: AtomicBool::new(true),
            master: AtomicU32::new(0.85f32.to_bits()),
            position_frames: AtomicU64::new(0),
            transport_frames: AtomicU64::new(0),
            transport_gen: AtomicU64::new(0),
            shutdown: AtomicBool::new(false),
            slots: (0..MAX_STEMS).map(|_| SlotShared::new()).collect(),
        });
        let (callback_tx, callback_rx) = mpsc::channel::<CallbackCmd>();
        let callback_shared = shared.clone();
        let error_shared = shared.clone();
        let mut output = MixerOutput::new(callback_rx);
        let stream = device
            .build_output_stream(
                config.config(),
                move |data: &mut [f32], _| {
                    output.render(data, out_ch, rate, &callback_shared);
                },
                move |e| {
                    eprintln!(
                        "[mixer cpal] stream error: {e}; close and reopen Mixer to rebuild output"
                    );
                    error_shared.playing.store(false, Ordering::SeqCst);
                    error_shared.play_pending.store(false, Ordering::SeqCst);
                    error_shared.shutdown.store(true, Ordering::SeqCst);
                },
                None,
            )
            .map_err(|e| e.to_string())?;
        stream.play().map_err(|e| e.to_string())?;
        let mut stem_tx = Vec::with_capacity(MAX_STEMS);
        stem_tx.resize_with(MAX_STEMS, || None);
        Ok(Self {
            _stream: stream,
            rate,
            channels,
            shared,
            stem_tx,
            callback_tx,
        })
    }

    pub fn load_playlist(&mut self, paths: &[PathBuf]) {
        self.clear();
        self.shared.position_frames.store(0, Ordering::SeqCst);
        self.shared.transport_gen.fetch_add(1, Ordering::SeqCst);
        for (index, path) in paths.iter().take(MAX_STEMS).enumerate() {
            self.load_slot(index, path.clone(), 0.0);
        }
        self.shared.play_pending.store(
            self.shared
                .slots
                .iter()
                .any(|s| s.enabled.load(Ordering::Acquire))
                && !self.shared.shutdown.load(Ordering::Acquire),
            Ordering::SeqCst,
        );
    }

    pub fn load_stem(&mut self, index: usize, path: PathBuf) {
        self.load_stems(&[(index, path)]);
    }

    pub fn load_stems(&mut self, loads: &[(usize, PathBuf)]) {
        if loads.iter().any(|(index, _)| *index >= MAX_STEMS) {
            return;
        }
        let had_loaded = self
            .shared
            .slots
            .iter()
            .any(|slot| slot.enabled.load(Ordering::Acquire));
        let resume = self.is_playing() || !had_loaded;
        let position = self.position_secs();
        self.shared.transport_gen.fetch_add(1, Ordering::SeqCst);
        self.shared.playing.store(false, Ordering::SeqCst);
        // Re-arm every existing stem at the transport position, even while
        // paused: adding or replacing a stem after an underrun/pause would
        // otherwise leave the old stems at different source positions.
        for (index, sender) in self.stem_tx.iter().enumerate() {
            if self.shared.slots[index].enabled.load(Ordering::Acquire) {
                self.shared.slots[index]
                    .ready
                    .store(false, Ordering::Release);
                if let Some(tx) = sender {
                    let _ = tx.send(StemCmd::Seek(position));
                }
            }
        }
        for (index, path) in loads {
            self.load_slot(*index, path.clone(), position);
        }
        self.shared.play_pending.store(
            resume && !self.shared.shutdown.load(Ordering::Acquire),
            Ordering::SeqCst,
        );
    }

    fn load_slot(&mut self, index: usize, path: PathBuf, start_secs: f64) {
        let slot = &self.shared.slots[index];
        slot.gain.store(1.0f32.to_bits(), Ordering::Relaxed);
        slot.pan.store(0.0f32.to_bits(), Ordering::Relaxed);
        slot.muted.store(false, Ordering::Relaxed);
        slot.meter.store(0.0f32.to_bits(), Ordering::Relaxed);
        slot.duration_frames.store(0, Ordering::Release);
        slot.ready.store(false, Ordering::Release);
        slot.enabled.store(true, Ordering::Release);

        if self.stem_tx[index].is_none() {
            let cap = self.rate as usize * RING_SECONDS * 2;
            let (producer, consumer) = RingBuffer::<f32>::new(cap);
            let (tx, rx) = mpsc::channel::<StemCmd>();
            let sh = self.shared.clone();
            let rate = self.rate;
            thread::Builder::new()
                .name(format!("mixer-stem-{index}"))
                .spawn(move || stem_worker(index, rx, producer, rate, sh))
                .expect("failed to spawn Mixer stem worker");
            self.stem_tx[index] = Some(tx);
            let _ = self.callback_tx.send(CallbackCmd::Attach(index, consumer));
        }
        if let Some(tx) = &self.stem_tx[index] {
            let _ = tx.send(StemCmd::Load(path, start_secs));
        }
    }

    pub fn remove_stem(&mut self, index: usize) {
        if index >= MAX_STEMS {
            return;
        }
        self.clear_slot(index);
        if !self
            .shared
            .slots
            .iter()
            .any(|s| s.enabled.load(Ordering::Acquire))
        {
            self.shared.playing.store(false, Ordering::SeqCst);
            self.shared.play_pending.store(false, Ordering::SeqCst);
            self.shared.position_frames.store(0, Ordering::SeqCst);
        }
    }

    pub fn clear(&mut self) {
        self.shared.playing.store(false, Ordering::SeqCst);
        self.shared.play_pending.store(false, Ordering::SeqCst);
        for index in 0..MAX_STEMS {
            self.clear_slot(index);
        }
    }

    fn clear_slot(&mut self, index: usize) {
        let slot = &self.shared.slots[index];
        slot.enabled.store(false, Ordering::Release);
        slot.ready.store(false, Ordering::Release);
        slot.duration_frames.store(0, Ordering::Release);
        slot.meter.store(0.0f32.to_bits(), Ordering::Relaxed);
        if let Some(tx) = &self.stem_tx[index] {
            let _ = tx.send(StemCmd::Clear);
        }
    }

    pub fn set_playing(&mut self, play: bool) {
        if self.shared.shutdown.load(Ordering::Acquire) {
            return;
        }
        if !play {
            self.shared.playing.store(false, Ordering::SeqCst);
            self.shared.play_pending.store(false, Ordering::SeqCst);
            return;
        }
        if !self
            .shared
            .slots
            .iter()
            .any(|s| s.enabled.load(Ordering::Acquire))
        {
            return;
        }
        let duration = self.duration_secs();
        if duration > 0.0 && self.position_secs() >= duration - 0.001 {
            self.seek_secs(0.0);
        }
        if self.all_ready() {
            self.shared.playing.store(true, Ordering::SeqCst);
            self.shared.play_pending.store(false, Ordering::SeqCst);
        } else {
            self.shared.play_pending.store(true, Ordering::SeqCst);
        }
    }

    pub fn toggle_play(&mut self) -> bool {
        let play = !self.is_playing();
        self.set_playing(play);
        self.is_playing()
    }

    pub fn is_playing(&self) -> bool {
        self.shared.playing.load(Ordering::Acquire)
            || self.shared.play_pending.load(Ordering::Acquire)
    }

    pub fn set_loop(&self, enabled: bool) {
        self.shared.looping.store(enabled, Ordering::SeqCst);
    }

    pub fn set_master_volume(&self, volume: f32) {
        self.shared
            .master
            .store(volume.clamp(0.0, 2.0).to_bits(), Ordering::SeqCst);
    }

    pub fn set_stem(&self, index: usize, gain: f32, pan: f32, muted: bool) {
        if index >= MAX_STEMS {
            return;
        }
        let slot = &self.shared.slots[index];
        slot.gain
            .store(gain.clamp(0.0, 2.0).to_bits(), Ordering::Relaxed);
        slot.pan
            .store(pan.clamp(-1.0, 1.0).to_bits(), Ordering::Relaxed);
        slot.muted.store(muted, Ordering::Relaxed);
    }

    pub fn seek_secs(&mut self, seconds: f64) {
        let resume = self.is_playing();
        self.shared.playing.store(false, Ordering::SeqCst);
        self.shared.play_pending.store(
            resume && !self.shared.shutdown.load(Ordering::Acquire),
            Ordering::SeqCst,
        );
        let duration = self.duration_secs();
        let clamped = seconds.max(0.0);
        let clamped = if duration > 0.0 {
            clamped.min(duration)
        } else {
            clamped
        };
        self.shared
            .position_frames
            .store((clamped * self.rate as f64) as u64, Ordering::SeqCst);
        self.shared.transport_gen.fetch_add(1, Ordering::SeqCst);
        for (index, sender) in self.stem_tx.iter().enumerate() {
            if self.shared.slots[index].enabled.load(Ordering::Acquire) {
                self.shared.slots[index]
                    .ready
                    .store(false, Ordering::Release);
                if let Some(tx) = sender {
                    let _ = tx.send(StemCmd::Seek(clamped));
                }
            }
        }
    }

    pub fn position_secs(&self) -> f64 {
        self.shared.position_frames.load(Ordering::Relaxed) as f64 / self.rate as f64
    }

    pub fn duration_secs(&self) -> f64 {
        let mut durations = [0u64; MAX_STEMS];
        for (index, slot) in self.shared.slots.iter().enumerate() {
            durations[index] = slot.duration_frames.load(Ordering::Relaxed);
        }
        longest_duration_frames(&durations) as f64 / self.rate as f64
    }

    pub fn poll(&mut self) {
        if self.shared.shutdown.load(Ordering::Acquire) {
            self.shared.playing.store(false, Ordering::SeqCst);
            self.shared.play_pending.store(false, Ordering::SeqCst);
            return;
        }
        let any_loaded = self
            .shared
            .slots
            .iter()
            .any(|s| s.enabled.load(Ordering::Acquire));
        if !any_loaded {
            self.shared.playing.store(false, Ordering::SeqCst);
            self.shared.play_pending.store(false, Ordering::SeqCst);
            return;
        }
        if self.shared.play_pending.load(Ordering::Acquire) && self.all_ready() {
            self.shared.play_pending.store(false, Ordering::SeqCst);
            self.shared.playing.store(true, Ordering::SeqCst);
        }
        let duration = self.duration_secs();
        match end_action(
            self.position_secs(),
            duration,
            self.shared.looping.load(Ordering::Acquire),
        ) {
            Some(true) if self.is_playing() => self.seek_secs(0.0),
            Some(_) => {
                self.shared.playing.store(false, Ordering::SeqCst);
                self.shared.play_pending.store(false, Ordering::SeqCst);
                self.shared
                    .position_frames
                    .store((duration * self.rate as f64) as u64, Ordering::SeqCst);
            }
            None => {}
        }
    }

    fn all_ready(&self) -> bool {
        let mut any = false;
        for slot in &self.shared.slots {
            if slot.enabled.load(Ordering::Acquire) {
                any = true;
                if !slot.ready.load(Ordering::Acquire) {
                    return false;
                }
            }
        }
        any
    }

    pub fn meter(&self, index: usize) -> f32 {
        self.shared
            .slots
            .get(index)
            .map(|s| f32::from_bits(s.meter.load(Ordering::Relaxed)))
            .unwrap_or(0.0)
    }

    pub fn sync_snapshot(&self) -> MixerSyncSnapshot {
        let stems = self
            .shared
            .slots
            .iter()
            .enumerate()
            .map(|(slot, state)| StemSyncSnapshot {
                slot,
                enabled: state.enabled.load(Ordering::Acquire),
                ready: state.ready.load(Ordering::Acquire),
                muted: state.muted.load(Ordering::Relaxed),
                played_frames: state.played_frames.load(Ordering::Relaxed),
                underrun_frames: state.underrun_frames.load(Ordering::Relaxed),
                ring_frames: state.ring_frames.load(Ordering::Relaxed),
                start_frames: state.start_frames.load(Ordering::Relaxed),
            })
            .collect();
        MixerSyncSnapshot {
            rate: self.rate,
            position_secs: self.position_secs(),
            transport_frames: self.shared.transport_frames.load(Ordering::Relaxed),
            stems,
        }
    }

    #[cfg(test)]
    pub fn stem_loaded(&self, index: usize) -> bool {
        self.shared
            .slots
            .get(index)
            .map(|s| s.enabled.load(Ordering::Acquire))
            .unwrap_or(false)
    }
}

impl Drop for MixerAudio {
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::SeqCst);
        self.shared.playing.store(false, Ordering::SeqCst);
        self.shared.play_pending.store(false, Ordering::SeqCst);
    }
}

// winit constructs App on one thread, so lazy ring attachment can use a
// thread-local sender without putting a mutex in the audio callback.
thread_local! {
    static CALLBACK_SENDER: std::cell::RefCell<Option<mpsc::Sender<CallbackCmd>>> =
        const { std::cell::RefCell::new(None) };
}

struct MixerOutput {
    rx: mpsc::Receiver<CallbackCmd>,
    consumers: Vec<Option<rtrb::Consumer<f32>>>,
    flush: Vec<u64>,
    levels: Vec<f32>,
    played: Vec<u64>,
    underruns: Vec<u64>,
    fade: f32,
    transport_gen: u64,
}

impl MixerOutput {
    fn new(rx: mpsc::Receiver<CallbackCmd>) -> Self {
        Self {
            rx,
            consumers: (0..MAX_STEMS).map(|_| None).collect(),
            flush: vec![0; MAX_STEMS],
            levels: vec![0.0; MAX_STEMS],
            played: vec![0; MAX_STEMS],
            underruns: vec![0; MAX_STEMS],
            fade: 0.0,
            transport_gen: 0,
        }
    }

    fn render(&mut self, data: &mut [f32], out_ch: usize, rate: u32, sh: &Shared) {
        while let Ok(CallbackCmd::Attach(index, consumer)) = self.rx.try_recv() {
            self.consumers[index] = Some(consumer);
        }
        for (index, slot) in sh.slots.iter().enumerate() {
            let generation = slot.flush_gen.load(Ordering::Acquire);
            if generation != self.flush[index] {
                if let Some(consumer) = &mut self.consumers[index] {
                    while consumer.pop().is_ok() {}
                }
                self.flush[index] = generation;
                slot.flush_ack.store(generation, Ordering::Release);
                self.levels[index] = 0.0;
            }
        }
        let transport_gen = sh.transport_gen.load(Ordering::Acquire);
        if transport_gen != self.transport_gen {
            self.transport_gen = transport_gen;
            self.fade = 0.0;
            sh.transport_frames.store(0, Ordering::Relaxed);
            for slot in &sh.slots {
                slot.played_frames.store(0, Ordering::Relaxed);
                slot.underrun_frames.store(0, Ordering::Relaxed);
            }
            self.played.fill(0);
            self.underruns.fill(0);
        }
        let playing = sh.playing.load(Ordering::Acquire);
        let master = f32::from_bits(sh.master.load(Ordering::Relaxed));
        let fade_step = 1.0 / (FADE_SECS * rate as f32).max(1.0);
        let mut left_gain = [0.0f32; MAX_STEMS];
        let mut right_gain = [0.0f32; MAX_STEMS];
        let mut gains = [0.0f32; MAX_STEMS];
        let mut enabled = [false; MAX_STEMS];
        let mut audible = [false; MAX_STEMS];
        for (index, slot) in sh.slots.iter().enumerate() {
            let (left, right) = pan_gains(f32::from_bits(slot.pan.load(Ordering::Relaxed)));
            left_gain[index] = left;
            right_gain[index] = right;
            gains[index] = f32::from_bits(slot.gain.load(Ordering::Relaxed));
            enabled[index] = slot.enabled.load(Ordering::Relaxed);
            audible[index] = enabled[index] && !slot.muted.load(Ordering::Relaxed);
            self.levels[index] *= 0.985;
            if !audible[index] {
                self.levels[index] = 0.0;
            }
        }
        let frames = data.len() / out_ch;
        let mut frames_consumed = 0u64;
        for frame in 0..frames {
            let target = if playing { 1.0 } else { 0.0 };
            self.fade += (target - self.fade).clamp(-fade_step, fade_step);
            let consuming = playing || self.fade > 0.0001;
            let mut mix_l = 0.0f32;
            let mut mix_r = 0.0f32;
            if consuming {
                for index in 0..MAX_STEMS {
                    if !enabled[index] {
                        continue;
                    }
                    let popped = if let Some(consumer) = &mut self.consumers[index] {
                        match (consumer.pop(), consumer.pop()) {
                            (Ok(left), Ok(right)) => Some((left, right)),
                            _ => None,
                        }
                    } else {
                        None
                    };
                    let Some((left, right)) = popped else {
                        self.underruns[index] += 1;
                        continue;
                    };
                    // Muted stems must keep consuming the same transport frames.
                    // Skipping them freezes their ring and desynchronizes playback
                    // when they are unmuted.
                    self.played[index] += 1;
                    if !audible[index] {
                        continue;
                    }
                    let (left, right) = shape_stem(
                        left,
                        right,
                        gains[index],
                        left_gain[index],
                        right_gain[index],
                    );
                    mix_l += left;
                    mix_r += right;
                    let peak = left.abs().max(right.abs());
                    if peak > self.levels[index] {
                        self.levels[index] = peak.min(1.0);
                    }
                }
                sh.position_frames.fetch_add(1, Ordering::Relaxed);
                frames_consumed += 1;
            }
            let base = frame * out_ch;
            data[base] = mix_l * master * self.fade;
            if out_ch > 1 {
                data[base + 1] = mix_r * master * self.fade;
            }
            for channel in 2..out_ch {
                data[base + channel] = 0.0;
            }
        }
        for (index, slot) in sh.slots.iter().enumerate() {
            slot.meter
                .store(self.levels[index].to_bits(), Ordering::Relaxed);
            if self.played[index] > 0 {
                slot.played_frames
                    .fetch_add(self.played[index], Ordering::Relaxed);
                self.played[index] = 0;
            }
            if self.underruns[index] > 0 {
                slot.underrun_frames
                    .fetch_add(self.underruns[index], Ordering::Relaxed);
                self.underruns[index] = 0;
            }
        }
        if frames_consumed > 0 {
            sh.transport_frames
                .fetch_add(frames_consumed, Ordering::Relaxed);
        }
    }
}

fn stem_worker(
    index: usize,
    rx: mpsc::Receiver<StemCmd>,
    mut producer: rtrb::Producer<f32>,
    rate: u32,
    sh: Arc<Shared>,
) {
    let mut decoder: Option<FFmpegStream> = None;
    let mut eof = true;
    let mut decoded_frames = 0u64;
    let mut tmp = vec![0.0f32; 8192 * 2];
    let ring_cap_samples = rate as usize * RING_SECONDS * 2;
    loop {
        if sh.shutdown.load(Ordering::Acquire) {
            break;
        }
        while let Ok(cmd) = rx.try_recv() {
            match cmd {
                StemCmd::Load(path, start) => {
                    decoder = None;
                    let slot = &sh.slots[index];
                    slot.duration_frames.store(0, Ordering::Release);
                    slot.ready.store(false, Ordering::Release);
                    slot.start_frames
                        .store((start.max(0.0) * rate as f64) as u64, Ordering::Relaxed);
                    match FFmpegStream::open(&path, rate) {
                        Ok(mut opened) => {
                            let duration = opened.duration_secs();
                            slot.duration_frames
                                .store((duration * rate as f64).max(0.0) as u64, Ordering::Release);
                            if let Err(e) = opened.seek(start) {
                                eprintln!(
                                    "[mixer] initial seek failed for {}: {e}",
                                    path.display()
                                );
                            }
                            decoder = Some(opened);
                            slot.enabled.store(true, Ordering::Release);
                            eof = false;
                            decoded_frames = 0;
                        }
                        Err(e) => {
                            eprintln!("[mixer] {e}");
                            slot.enabled.store(false, Ordering::Release);
                            eof = true;
                        }
                    }
                    let generation = slot.flush_gen.fetch_add(1, Ordering::AcqRel) + 1;
                    wait_flush_ack(slot, generation, &sh);
                }
                StemCmd::Seek(seconds) => {
                    let slot = &sh.slots[index];
                    slot.ready.store(false, Ordering::Release);
                    slot.start_frames
                        .store((seconds.max(0.0) * rate as f64) as u64, Ordering::Relaxed);
                    decoded_frames = 0;
                    if let Some(active) = &mut decoder {
                        if let Err(e) = active.seek(seconds) {
                            eprintln!("[mixer] seek failed: {e}");
                        }
                        eof = false;
                    }
                    let generation = slot.flush_gen.fetch_add(1, Ordering::AcqRel) + 1;
                    wait_flush_ack(slot, generation, &sh);
                }
                StemCmd::Clear => {
                    decoder = None;
                    eof = true;
                    let slot = &sh.slots[index];
                    slot.enabled.store(false, Ordering::Release);
                    slot.duration_frames.store(0, Ordering::Release);
                    slot.ready.store(false, Ordering::Release);
                    slot.start_frames.store(0, Ordering::Relaxed);
                    slot.ring_frames.store(0, Ordering::Relaxed);
                    let generation = slot.flush_gen.fetch_add(1, Ordering::AcqRel) + 1;
                    wait_flush_ack(slot, generation, &sh);
                }
            }
        }

        if sh.shutdown.load(Ordering::Acquire) {
            break;
        }
        let ring_frames = ring_cap_samples.saturating_sub(producer.slots()) / 2;
        sh.slots[index]
            .ring_frames
            .store(ring_frames as u64, Ordering::Relaxed);
        if decoder.is_none() || eof {
            thread::sleep(Duration::from_millis(4));
            continue;
        }
        let free = producer.slots() & !1;
        if free < 2 {
            thread::sleep(Duration::from_millis(2));
            continue;
        }
        let want = free.min(tmp.len()) & !1;
        let n = decoder.as_mut().unwrap().read(&mut tmp[..want]);
        if n == 0 {
            eof = true;
            sh.slots[index].ready.store(true, Ordering::Release);
            continue;
        }
        for sample in &tmp[..n] {
            if producer.push(*sample).is_err() {
                break;
            }
        }
        decoded_frames += (n / 2) as u64;
        if decoded_frames >= (rate as f64 * PREBUFFER_SECS) as u64 {
            sh.slots[index].ready.store(true, Ordering::Release);
        }
    }
}

fn wait_flush_ack(slot: &SlotShared, generation: u64, sh: &Shared) {
    let deadline = Instant::now() + Duration::from_millis(500);
    while slot.flush_ack.load(Ordering::Acquire) < generation {
        if sh.shutdown.load(Ordering::Acquire) {
            return;
        }
        if Instant::now() >= deadline {
            eprintln!("[mixer] callback flush acknowledgement timed out; transport stopped");
            sh.playing.store(false, Ordering::SeqCst);
            sh.play_pending.store(false, Ordering::SeqCst);
            return;
        }
        thread::sleep(Duration::from_millis(1));
    }
}

fn pan_gains(pan: f32) -> (f32, f32) {
    let angle = (pan.clamp(-1.0, 1.0) + 1.0) * std::f32::consts::FRAC_PI_4;
    (angle.cos(), angle.sin())
}

fn shape_stem(left: f32, right: f32, gain: f32, left_gain: f32, right_gain: f32) -> (f32, f32) {
    (left * gain * left_gain, right * gain * right_gain)
}

fn longest_duration_frames(durations: &[u64]) -> u64 {
    durations.iter().copied().max().unwrap_or(0)
}

fn end_action(position: f64, duration: f64, looping: bool) -> Option<bool> {
    if duration > 0.0 && position >= duration {
        Some(looping)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_shared() -> Shared {
        Shared {
            playing: AtomicBool::new(true),
            play_pending: AtomicBool::new(false),
            looping: AtomicBool::new(true),
            master: AtomicU32::new(1.0f32.to_bits()),
            position_frames: AtomicU64::new(0),
            transport_frames: AtomicU64::new(0),
            transport_gen: AtomicU64::new(0),
            shutdown: AtomicBool::new(false),
            slots: (0..MAX_STEMS).map(|_| SlotShared::new()).collect(),
        }
    }

    #[test]
    fn equal_power_pan_preserves_power_and_reaches_hard_edges() {
        let (left, right) = pan_gains(0.0);
        assert!((left - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
        assert!((right - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
        assert!((left * left + right * right - 1.0).abs() < 1e-6);
        assert_eq!(pan_gains(-1.0), (1.0, 0.0));
        let (left, right) = pan_gains(1.0);
        assert!(left.abs() < 1e-6);
        assert!((right - 1.0).abs() < 1e-6);
    }

    #[test]
    fn post_fader_mixing_obeys_gain_and_pan() {
        let (left_gain, right_gain) = pan_gains(-1.0);
        let (left, right) = shape_stem(0.5, -0.25, 1.5, left_gain, right_gain);
        assert!((left - 0.75).abs() < 1e-6);
        assert_eq!(right, 0.0);
        let (left_gain, right_gain) = pan_gains(1.0);
        let (left, right) = shape_stem(0.5, -0.25, 2.0, left_gain, right_gain);
        assert!(left.abs() < 1e-6);
        assert_eq!(right, -0.5);
    }

    #[test]
    fn longest_stem_defines_transport_duration() {
        assert_eq!(longest_duration_frames(&[]), 0);
        assert_eq!(longest_duration_frames(&[100, 500, 250]), 500);
    }

    #[test]
    fn callback_mixes_stems_on_one_transport_clock() {
        let sh = test_shared();
        sh.slots[0].enabled.store(true, Ordering::Relaxed);
        sh.slots[1].enabled.store(true, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        let (mut producer0, consumer0) = RingBuffer::<f32>::new(16);
        let (mut producer1, consumer1) = RingBuffer::<f32>::new(16);
        tx.send(CallbackCmd::Attach(0, consumer0)).unwrap();
        tx.send(CallbackCmd::Attach(1, consumer1)).unwrap();
        for _ in 0..4 {
            producer0.push(0.25).unwrap();
            producer0.push(0.25).unwrap();
            producer1.push(0.5).unwrap();
            producer1.push(-0.5).unwrap();
        }
        let mut output = MixerOutput::new(rx);
        output.fade = 1.0;
        let mut data = [0.0f32; 4];
        output.render(&mut data, 2, 48_000, &sh);
        let center = std::f32::consts::FRAC_1_SQRT_2;
        assert!((data[0] - 0.75 * center).abs() < 1e-6);
        assert!((data[1] + 0.25 * center).abs() < 1e-6);
        assert_eq!(sh.position_frames.load(Ordering::Relaxed), 2);
        assert!((sh.slots[0].meter.load(Ordering::Relaxed) as f32) > 0.0);

        sh.slots[1].muted.store(true, Ordering::Relaxed);
        output.render(&mut data[..2], 2, 48_000, &sh);
        assert!((data[0] - 0.25 * center).abs() < 1e-6);
        assert!((data[1] - 0.25 * center).abs() < 1e-6);
        assert_eq!(
            f32::from_bits(sh.slots[1].meter.load(Ordering::Relaxed)),
            0.0
        );
    }

    #[test]
    fn muted_stem_keeps_consuming_transport_frames() {
        let sh = test_shared();
        sh.slots[0].enabled.store(true, Ordering::Relaxed);
        sh.slots[1].enabled.store(true, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        let (mut producer0, consumer0) = RingBuffer::<f32>::new(16);
        let (mut producer1, consumer1) = RingBuffer::<f32>::new(16);
        tx.send(CallbackCmd::Attach(0, consumer0)).unwrap();
        tx.send(CallbackCmd::Attach(1, consumer1)).unwrap();
        for value in [0.5f32, 0.5, 0.5, 0.5] {
            producer0.push(value).unwrap();
        }
        for value in [0.5f32, 0.5, 1.0, 1.0] {
            producer1.push(value).unwrap();
        }
        let mut output = MixerOutput::new(rx);
        output.fade = 1.0;
        let center = std::f32::consts::FRAC_1_SQRT_2;

        sh.slots[1].muted.store(true, Ordering::Relaxed);
        let mut data = [0.0f32; 2];
        output.render(&mut data, 2, 48_000, &sh);
        assert!((data[0] - 0.5 * center).abs() < 1e-6);
        assert_eq!(sh.slots[1].played_frames.load(Ordering::Relaxed), 1);
        assert_eq!(sh.slots[1].underrun_frames.load(Ordering::Relaxed), 0);

        sh.slots[1].muted.store(false, Ordering::Relaxed);
        output.render(&mut data, 2, 48_000, &sh);
        assert!(
            (data[0] - 1.5 * center).abs() < 1e-6,
            "unmuted stem must resume on the next transport frame, not its stale sample"
        );
        assert_eq!(sh.slots[1].played_frames.load(Ordering::Relaxed), 2);
        assert_eq!(sh.transport_frames.load(Ordering::Relaxed), 2);
    }

    #[test]
    #[ignore = "requires real output hardware and the reference Mixer media"]
    fn adding_stem_while_playing_keeps_stems_aligned() {
        let dir = PathBuf::from(r"D:\Work\_GIT\SoundApp\_Material\mixer_media\Again");
        let paths = [dir.join("01.m4a"), dir.join("02.m4a")];
        let added = dir.join("03.m4a");
        if paths.iter().any(|path| !path.exists()) || !added.exists() {
            eprintln!("reference Mixer media unavailable");
            return;
        }
        let mut engine = MixerAudio::start().expect("Mixer output");
        engine.set_master_volume(0.03);
        engine.load_playlist(&paths);
        let deadline = Instant::now() + Duration::from_secs(8);
        while Instant::now() < deadline && engine.position_secs() < 0.6 {
            engine.poll();
            thread::sleep(Duration::from_millis(10));
        }
        assert!(engine.position_secs() >= 0.6, "initial stems did not run");

        engine.load_stems(&[(2, added)]);
        let deadline = Instant::now() + Duration::from_secs(8);
        while Instant::now() < deadline && engine.position_secs() < 1.4 {
            engine.poll();
            thread::sleep(Duration::from_millis(10));
        }
        let snapshot = engine.sync_snapshot();
        assert!(snapshot.stems.iter().filter(|s| s.enabled).count() >= 3);
        for stem in snapshot.stems.iter().filter(|s| s.enabled) {
            let drift = stem.drift_frames(snapshot.transport_frames).abs();
            assert!(
                drift < 480,
                "stem {} drifted {drift} frames after a live add",
                stem.slot
            );
        }
    }

    #[test]
    #[ignore = "requires real output hardware and the reference Mixer media"]
    fn real_media_play_pause_and_seek() {
        let paths = [
            PathBuf::from(r"D:\Work\_GIT\SoundApp\_Material\mixer_media\Again\01.m4a"),
            PathBuf::from(r"D:\Work\_GIT\SoundApp\_Material\mixer_media\Again\02.m4a"),
        ];
        if paths.iter().any(|path| !path.exists()) {
            eprintln!("reference Mixer media unavailable");
            return;
        }
        let mut engine = MixerAudio::start().expect("Mixer output");
        assert!(engine.rate > 0);
        assert!(engine.channels > 0);
        engine.set_master_volume(0.05);
        engine.load_playlist(&paths);
        let deadline = Instant::now() + Duration::from_secs(8);
        while Instant::now() < deadline {
            engine.poll();
            if engine.position_secs() > 0.35
                && (engine.meter(0) > 0.0001 || engine.meter(1) > 0.0001)
            {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(engine.stem_loaded(0) && engine.stem_loaded(1));
        assert!(engine.duration_secs() > 1.0);
        assert!(engine.position_secs() > 0.2, "real stems did not advance");
        assert!(engine.meter(0) > 0.0 || engine.meter(1) > 0.0);

        engine.set_playing(false);
        thread::sleep(Duration::from_millis(80));
        let paused = engine.position_secs();
        thread::sleep(Duration::from_millis(50));
        assert!((engine.position_secs() - paused).abs() < 0.001);

        engine.seek_secs(0.5);
        engine.set_playing(true);
        let deadline = Instant::now() + Duration::from_secs(4);
        while Instant::now() < deadline && engine.position_secs() < 0.55 {
            engine.poll();
            thread::sleep(Duration::from_millis(10));
        }
        assert!(engine.position_secs() >= 0.5);
    }

    #[test]
    fn eof_loops_only_when_requested() {
        assert_eq!(end_action(10.0, 10.0, true), Some(true));
        assert_eq!(end_action(10.0, 10.0, false), Some(false));
        assert_eq!(end_action(9.0, 10.0, true), None);
        assert_eq!(end_action(1.0, 0.0, true), None);
    }
}
