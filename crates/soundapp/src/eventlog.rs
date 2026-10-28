// Input/window event recorder: focus changes, key presses and their global-
// pass decisions, window open/close/hide — the flight recorder for focus- and
// shortcut-related bugs. The 2026-10-28 "50/50 window toggle" hunt needed
// exactly this trace and had to build it as throwaway probes; this makes it
// permanent. Same model as perf.rs: a file next to `user.json`, works with no
// console (GUI-subsystem release build), truncated at each session start.
//
// What a toggle bug needs that an "I pressed M" note cannot give:
//   - ordering (one monotonic sequence number across every writer),
//   - the OS's own answer next to the app's belief (mapped / cloaked / focused),
//   - which window received the key, and whether that was the one in focus,
//   - how long a requested reveal waited for its first frame.
// Lines are human-rate only; per-frame noise is behind `--trace` /
// `SOUNDAPP_TRACE=1` via `trace()`.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::thread::ThreadId;
use std::time::Instant;

static T0: OnceLock<Instant> = OnceLock::new();
static LOG: OnceLock<PathBuf> = OnceLock::new();
static UI_THREAD: OnceLock<ThreadId> = OnceLock::new();
static VERBOSE: OnceLock<bool> = OnceLock::new();
static SEQ: AtomicU64 = AtomicU64::new(0);

/// Start the log for this session (truncates the previous session's file).
pub fn init() {
    let _ = T0.set(Instant::now());
    let _ = UI_THREAD.set(std::thread::current().id());
    let verbose = std::env::var_os("SOUNDAPP_TRACE").is_some()
        || std::env::args().any(|a| a == "--trace");
    let _ = VERBOSE.set(verbose);
    let path = crate::config::UserConfig::path().with_file_name("event_log.txt");
    let _ = std::fs::write(&path, b"");
    if LOG.set(path).is_ok() {
        note(&format!(
            "=== session pid={} exe={:?} args={:?} trace={verbose}",
            std::process::id(),
            std::env::current_exe().ok(),
            std::env::args().skip(1).collect::<Vec<_>>()
        ));
    }
}

/// Monotonic session milliseconds — use it to measure gaps between two notes.
pub fn ms() -> f64 {
    T0.get()
        .map(|t| t.elapsed().as_secs_f64() * 1000.0)
        .unwrap_or(0.0)
}

/// Whether per-frame / per-poll detail should be written.
pub fn trace_on() -> bool {
    VERBOSE.get().copied().unwrap_or(false)
}

/// Verbose-only line (no-op unless `--trace` / `SOUNDAPP_TRACE=1`).
pub fn trace(msg: &str) {
    if trace_on() {
        note(msg);
    }
}

/// Append one timestamped line. Human-rate traffic only (keys, focus,
/// window lifecycle) — opening per line like perf.rs keeps crashes lossless.
/// The `~` marker means the line came from a non-UI thread (ordering across
/// threads is only as good as the lock-free sequence number).
pub fn note(msg: &str) {
    let Some(path) = LOG.get() else { return };
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let tid = std::thread::current().id();
    let mt = if UI_THREAD.get() == Some(&tid) { " " } else { "~" };
    let line = format!("[{:9.2}ms{mt}#{seq:05}] {msg}\n", ms());
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(line.as_bytes());
        let _ = f.flush();
    }
}
