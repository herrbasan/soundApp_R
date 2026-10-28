// Input/window event recorder: focus changes, key presses and their global-
// pass decisions, window open/close/hide — the flight recorder for focus- and
// shortcut-related bugs. The 2026-10-28 "50/50 window toggle" hunt needed
// exactly this trace and had to build it as throwaway probes; this makes it
// permanent. Same model as perf.rs: a file next to `user.json`, works with no
// console (GUI-subsystem release build), truncated at each session start.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Instant;

static T0: OnceLock<Instant> = OnceLock::new();
static LOG: OnceLock<PathBuf> = OnceLock::new();

/// Start the log for this session (truncates the previous session's file).
pub fn init() {
    let _ = T0.set(Instant::now());
    let path = crate::config::UserConfig::path().with_file_name("event_log.txt");
    let _ = std::fs::write(&path, b"");
    if LOG.set(path).is_ok() {
        note(&format!(
            "=== session pid={} args={:?}",
            std::process::id(),
            std::env::args().skip(1).collect::<Vec<_>>()
        ));
    }
}

/// Append one timestamped line. Human-rate traffic only (keys, focus,
/// window lifecycle) — opening per line like perf.rs keeps crashes lossless.
pub fn note(msg: &str) {
    let Some(path) = LOG.get() else { return };
    let t0 = T0.get().copied().unwrap_or_else(Instant::now);
    let line = format!("[{:9.2}ms] {}\n", t0.elapsed().as_secs_f64() * 1000.0, msg);
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(line.as_bytes());
        let _ = f.flush();
    }
}
