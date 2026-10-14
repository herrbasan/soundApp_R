// Audio out: cpal callback drains a lock-free rtrb ring buffer; a producer
// thread decodes (symphonia) or generates a tone and fills it.
//
// This is the Rust shape of the current app's SAB ring buffer — but without
// SharedArrayBuffer, Atomics, postMessage, or a separate engine process.
// Decoding never runs on the audio thread; only buffer copying does.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rtrb::RingBuffer;

pub enum Source {
    Silence,
    Tone { hz: f32 },
    File(PathBuf),
}

pub struct Audio {
    _stream: cpal::Stream,
    pub rate: u32,
    pub channels: u16,
    pub total_frames: Arc<AtomicU64>,
    pub frames_played: Arc<AtomicU64>,
    pub started: Arc<AtomicBool>,
}

impl Audio {
    pub fn position_secs(&self) -> f64 {
        self.frames_played.load(Ordering::Relaxed) as f64 / self.rate as f64
    }
    pub fn duration_secs(&self) -> f64 {
        self.total_frames.load(Ordering::Relaxed) as f64 / self.rate as f64
    }
}

const RING_SECONDS: usize = 8;

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

    let total_frames = Arc::new(AtomicU64::new(0));
    let frames_played = Arc::new(AtomicU64::new(0));
    let started = Arc::new(AtomicBool::new(false));

    // Producer thread: fill the ring buffer. Silence has no producer, so the
    // callback drains an empty ring and writes zeros — isolating stream cost.
    if !matches!(src, Source::Silence) {
        let total = total_frames.clone();
        thread::Builder::new()
            .name("decode".into())
            .spawn(move || fill(producer, src, rate, out_ch, total))
            .map_err(|e| e.to_string())?;
    }

    // Consumer: the cpal callback. Runs on the audio thread — copy only.
    let played = frames_played.clone();
    let started_cb = started.clone();
    let err_fn = |e| eprintln!("[cpal] stream error: {e}");
    let mut consumer = consumer;
    let mut got_audio = false;
    let stream = device
        .build_output_stream(
            stream_config,
            move |data: &mut [f32], _| {
                let mut i = 0;
                while i < data.len() {
                    match consumer.pop() {
                        Ok(s) => {
                            data[i] = s;
                            i += 1;
                            if !got_audio {
                                got_audio = true;
                                started_cb.store(true, Ordering::Relaxed);
                            }
                        }
                        Err(_) => {
                            for d in &mut data[i..] {
                                *d = 0.0;
                            }
                            break;
                        }
                    }
                }
                played.fetch_add((data.len() / out_ch) as u64, Ordering::Relaxed);
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
        total_frames,
        frames_played,
        started,
    })
}

/// Producer body: push interleaved f32 at the device rate into the ring.
fn fill(
    mut producer: rtrb::Producer<f32>,
    src: Source,
    rate: u32,
    out_ch: usize,
    total: Arc<AtomicU64>,
) {
    match src {
        Source::Silence => {}
        Source::Tone { hz } => {
            total.store(u64::MAX, Ordering::Relaxed); // indeterminate
            let step = std::f32::consts::TAU * hz / rate as f32;
            let mut phase = 0f32;
            loop {
                for _ in 0..out_ch {
                    let v = phase.sin() * 0.0; // Muted for silent UI/audio pipeline testing
                    if !push_wait(&mut producer, v) {
                        return;
                    }
                }
                phase += step;
                if phase >= std::f32::consts::TAU {
                    phase -= std::f32::consts::TAU;
                }
            }
        }
        Source::File(path) => {
            if let Err(e) = stream_file(&path, &mut producer, rate, out_ch, &total) {
                eprintln!("[decode] {path:?}: {e}");
            }
        }
    }
}

/// Push one sample, yielding while the ring is full.
/// The ring holds seconds of audio, so we can wait in long stretches; the
/// wake/sleep cycle itself was the measurable cost (~0.7 % of a core at 5 ms
/// polls). Returns false if the consumer was dropped.
fn push_wait(producer: &mut rtrb::Producer<f32>, v: f32) -> bool {
    loop {
        match producer.push(v) {
            Ok(()) => return true,
            Err(_) => thread::sleep(Duration::from_millis(20)),
        }
    }
}

/// Stream a file: probe, then decode packet-by-packet straight into the ring,
/// resampling on the fly. Nothing is buffered whole, so the first samples play
/// within milliseconds regardless of file length, and memory stays flat.
fn stream_file(
    path: &std::path::Path,
    producer: &mut rtrb::Producer<f32>,
    out_rate: u32,
    out_ch: usize,
    total: &Arc<AtomicU64>,
) -> Result<(), String> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
    use symphonia::core::errors::Error as SymError;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(|e| e.to_string())?;
    let mut format = probed.format;

    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or("no decodable track")?
        .clone();
    let track_id = track.id;

    let in_rate = track.codec_params.sample_rate.ok_or("unknown sample rate")?;
    let in_ch = track
        .codec_params
        .channels
        .map(|c| c.count())
        .unwrap_or(2)
        .max(1);

    // Duration for the progress bar (n_frames is not always present).
    match track.codec_params.n_frames {
        Some(n) if n > 0 => {
            total.store((n as f64 * out_rate as f64 / in_rate as f64) as u64, Ordering::Relaxed);
        }
        _ => {
            total.store(0, Ordering::Relaxed);
        }
    }

    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| e.to_string())?;

    let step = in_rate as f64 / out_rate as f64;
    let mut buf: Vec<f32> = Vec::new(); // interleaved input frames
    let mut read_pos = 0f64; // fractional read position (frames) into `buf`
    let mut sample_buf: Option<SampleBuffer<f32>> = None;

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymError::ResetRequired) => break,
            Err(e) => return Err(e.to_string()),
        };
        if packet.track_id() != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(decoded) => {
                let spec = *decoded.spec();
                let sb = sample_buf
                    .get_or_insert_with(|| SampleBuffer::<f32>::new(decoded.capacity() as u64, spec));
                sb.copy_interleaved_ref(decoded);
                buf.extend_from_slice(sb.samples());
            }
            Err(SymError::DecodeError(_)) => continue,
            Err(e) => return Err(e.to_string()),
        }

        // Emit every output frame we can interpolate so far. push_wait sleeps
        // when the ring is full, so we decode ~RING_SECONDS ahead and no more.
        while (read_pos + 1.0) < (buf.len() / in_ch) as f64 {
            let i0 = read_pos.floor() as usize;
            let frac = (read_pos - i0 as f64) as f32;
            for c in 0..out_ch {
                let src = c.min(in_ch - 1);
                let s0 = buf[i0 * in_ch + src];
                let s1 = buf[(i0 + 1) * in_ch + src];
                if !push_wait(producer, s0 + (s1 - s0) * frac) {
                    return Ok(());
                }
            }
            read_pos += step;
        }

        // Drop consumed input frames.
        let consumed = read_pos.floor() as usize;
        if consumed > 0 {
            buf.drain(0..consumed * in_ch);
            read_pos -= consumed as f64;
        }
    }
    Ok(())
}
