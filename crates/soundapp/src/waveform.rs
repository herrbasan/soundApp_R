//! Whole-file overview data for the Monitoring window.
//!
//! Mirrors the reference Electron app's two overview modes:
//! * audio → per-bucket peak magnitudes of `max(|L|, |R|)` over the whole file
//!   (`getWaveformStreaming` in the original decoder),
//! * MIDI → per-channel note-on activity lanes (`midi_analyzer.js`).
//!
//! Analysis runs on its own thread against its own FFmpeg handle, so the audio
//! path never waits on it and a new track cancels the old pass by generation.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use crate::audio::FFmpegStream;
use crate::midi;

/// Points in an overview — independent of window width; the painter folds them
/// into the available columns.
pub const PEAK_POINTS: usize = 1600;
/// Decode rate for the overview pass: peaks are per-bucket maxima, so a low
/// rate is inaudible and much cheaper than the playback rate. 8 kHz keeps
/// every peak the 1600-point display can resolve while halving the frame
/// count of the former 16 kHz (the codec decode dominates, so the saving is
/// partial — but it comes free).
const OVERVIEW_RATE: u32 = 8_000;
const READ_FRAMES: usize = 8_192;
/// Instant-feel budget: a scan whose projected full-speed cost fits inside
/// this window runs UNPACED. A 2-5 min track decodes at 16 kHz in a few
/// hundred ms, so most overviews land instantly; the budget is the ceiling
/// the user perceives as "instant".
const INSTANT_BUDGET: f32 = 1.0;
/// CPU ceiling for scans that overrun the instant budget (fraction of one
/// core). A 1 h track settles here — the overview trickles in over tens of
/// seconds instead of pegging a core. Between the two regimes the duty
/// decays smoothly (`INSTANT_BUDGET / projected`), so there is no cliff.
const LONG_TRACK_DUTY: f32 = 0.30;
/// MIDI note-ons closer than this stay in the same activity segment.
const MIDI_GAP_MS: f32 = 1_000.0;

#[derive(Clone, Debug)]
pub enum WaveformData {
    /// Nothing to draw (unsupported source, failed decode).
    Empty,
    /// Peak magnitude per point, `0..1`.
    Peaks(Vec<f32>),
    /// One lane per active MIDI channel, segments in milliseconds.
    Midi(Vec<midi::MidiLane>),
}

#[derive(Clone, Debug)]
pub struct WaveformUpdate {
    pub gen: u64,
    /// Source duration in seconds (0 when unknown).
    pub duration: f64,
    pub progress: f32,
    pub data: WaveformData,
}

struct Job {
    gen: u64,
    path: PathBuf,
}

pub struct WaveformWorker {
    job_tx: mpsc::Sender<Job>,
    rx: mpsc::Receiver<WaveformUpdate>,
    latest: Arc<AtomicU64>,
    next_gen: u64,
}

impl WaveformWorker {
    pub fn new() -> Self {
        let (job_tx, job_rx) = mpsc::channel::<Job>();
        let (tx, rx) = mpsc::channel::<WaveformUpdate>();
        let latest = Arc::new(AtomicU64::new(0));
        let worker_latest = latest.clone();
        std::thread::Builder::new()
            .name("waveform-overview".into())
            .spawn(move || {
                while let Ok(job) = job_rx.recv() {
                    if worker_latest.load(Ordering::Relaxed) != job.gen {
                        continue;
                    }
                    let update = analyse(&job, &worker_latest, &tx);
                    if let Some(update) = update {
                        let _ = tx.send(update);
                    }
                }
            })
            .expect("failed to spawn waveform worker");
        Self {
            job_tx,
            rx,
            latest,
            next_gen: 1,
        }
    }

    /// Queue an analysis pass; older passes are abandoned. Returns the
    /// generation id that matching updates carry.
    pub fn request(&mut self, path: PathBuf) -> u64 {
        let gen = self.next_gen;
        self.next_gen += 1;
        self.latest.store(gen, Ordering::Relaxed);
        let _ = self.job_tx.send(Job { gen, path });
        gen
    }

    /// Drain completed / partial updates (non-blocking).
    pub fn poll(&self) -> Vec<WaveformUpdate> {
        let mut out = Vec::new();
        while let Ok(update) = self.rx.try_recv() {
            out.push(update);
        }
        out
    }

    /// Abandon the running pass (Monitoring window closed) — the worker checks
    /// the generation between read chunks and exits instead of decoding a file
    /// nobody is looking at.
    pub fn cancel(&self) {
        self.latest.store(u64::MAX, Ordering::Relaxed);
    }
}

fn analyse(
    job: &Job,
    latest: &AtomicU64,
    tx: &mpsc::Sender<WaveformUpdate>,
) -> Option<WaveformUpdate> {
    let stale = |gen: u64| latest.load(Ordering::Relaxed) != gen;
    let empty = |progress: f32, duration: f64| WaveformUpdate {
        gen: job.gen,
        duration,
        progress,
        data: WaveformData::Empty,
    };
    if midi::is_midi_file(&job.path) {
        let parsed = std::fs::read(&job.path)
            .ok()
            .and_then(|bytes| midi::channel_activity(&bytes, MIDI_GAP_MS));
        return Some(match parsed {
            Some((lanes, duration)) => WaveformUpdate {
                gen: job.gen,
                duration,
                progress: 1.0,
                data: WaveformData::Midi(lanes),
            },
            None => empty(1.0, 0.0),
        });
    }
    // Tracker modules render through libopenmpt, not FFmpeg; an overview would
    // need a full module render. No overview instead of a wrong one.
    if crate::tracker::is_tracker_file(&job.path) {
        return Some(empty(1.0, 0.0));
    }

    let mut stream = match FFmpegStream::open(&job.path, OVERVIEW_RATE) {
        Ok(stream) => stream,
        Err(e) => {
            eprintln!("[waveform] overview unavailable: {e}");
            return Some(empty(1.0, 0.0));
        }
    };
    let duration = stream.duration_secs();
    if duration <= 0.0 {
        return Some(empty(1.0, 0.0));
    }
    let total_frames = (duration * OVERVIEW_RATE as f64) as u64;
    let frames_per_point = (total_frames / PEAK_POINTS as u64).max(1);
    let mut peaks = vec![0.0f32; PEAK_POINTS];
    let mut point = 0usize;
    let mut acc = 0u64;
    let mut bucket_peak = 0.0f32;
    let mut buf = vec![0.0f32; READ_FRAMES * 2];
    let mut sent = 0.0f32;
    let scan_start = Instant::now();
    let mut decoded_frames = 0u64;
    let mut work_acc = Duration::ZERO;
    loop {
        if stale(job.gen) {
            return None;
        }
        let chunk_t0 = Instant::now();
        let read = stream.read(&mut buf);
        if read == 0 {
            break;
        }
        for frame in buf[..read].chunks_exact(2) {
            let peak = frame[0].abs().max(frame[1].abs());
            if peak > bucket_peak {
                bucket_peak = peak;
            }
            acc += 1;
            if acc >= frames_per_point {
                if point < PEAK_POINTS {
                    peaks[point] = bucket_peak.min(1.0);
                    point += 1;
                }
                acc = 0;
                bucket_peak = 0.0;
                if point >= PEAK_POINTS {
                    break;
                }
            }
        }
        if point >= PEAK_POINTS {
            break;
        }
        // Coarse progress ticks keep the panel visibly alive on long files.
        let progress = point as f32 / PEAK_POINTS as f32;
        if progress - sent >= 0.05 {
            sent = progress;
            let _ = tx.send(WaveformUpdate {
                gen: job.gen,
                duration,
                progress,
                data: WaveformData::Peaks(peaks.clone()),
            });
        }
        // Adaptive pacing. Cumulative work over decoded fraction projects
        // the full-speed scan cost: a projection inside INSTANT_BUDGET runs
        // unpaced (a 2-5 min track finishes in a few hundred ms — instant),
        // a larger one decays the duty towards LONG_TRACK_DUTY so a 1 h
        // track trickles in instead of pegging a core. The stale check at
        // the loop top bounds cancellation latency to one paced chunk.
        let work = chunk_t0.elapsed();
        work_acc += work;
        decoded_frames += read as u64;
        let duty = if decoded_frames < total_frames / 50 {
            // Under 2% decoded the projection is warmup noise — run free;
            // the window is small by construction.
            1.0
        } else {
            let projected =
                work_acc.as_secs_f32() * total_frames as f32 / decoded_frames as f32;
            (INSTANT_BUDGET / projected).clamp(LONG_TRACK_DUTY, 1.0)
        };
        let rest = work.mul_f32((1.0 - duty) / duty);
        if rest > Duration::ZERO {
            std::thread::sleep(rest);
        }
    }
    eprintln!(
        "[waveform] overview ready in {:.0} ms ({:.0} s audio)",
        scan_start.elapsed().as_secs_f64() * 1000.0,
        duration
    );
    // Trailing partial bucket (the file ended mid-bucket).
    if point < PEAK_POINTS && bucket_peak > 0.0 {
        peaks[point] = bucket_peak.min(1.0);
    }
    // Peak-normalise the overview: the reference drew raw peaks, which turns any
    // quietly-mastered track into a flat line (−32 dBTP reads as 2 % height).
    let loudest = peaks.iter().copied().fold(0.0f32, f32::max);
    if loudest > 1e-4 {
        let scale = 1.0 / loudest;
        for peak in &mut peaks {
            *peak = (*peak * scale).min(1.0);
        }
    }
    Some(WaveformUpdate {
        gen: job.gen,
        duration,
        progress: 1.0,
        data: WaveformData::Peaks(peaks),
    })
}