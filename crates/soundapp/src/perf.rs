// Field-performance log: a handful of lines per session next to `user.json`,
// so startup, file-open and frame-pacing timings survive on machines without a
// terminal (the release build is a GUI-subsystem binary whose stdout goes
// nowhere when double-clicked).

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

static T0: OnceLock<Instant> = OnceLock::new();
static LOG: OnceLock<PathBuf> = OnceLock::new();
static FAILED: AtomicBool = AtomicBool::new(false);

/// Anchor the session clock and start the log next to `user.json`, falling
/// back to `%TEMP%` when the install folder is unwritable — field machines
/// must always produce data.
pub fn init() {
    let _ = T0.set(Instant::now());
    let exe_dir_log = crate::config::UserConfig::path().with_file_name("perf_log.txt");
    let path = if writable(exe_dir_log.parent()) {
        exe_dir_log
    } else {
        std::env::temp_dir().join("soundapp_r_perf_log.txt")
    };
    if LOG.set(path).is_ok() {
        note(&format!(
            "=== session pid={} args={:?}",
            std::process::id(),
            std::env::args().skip(1).collect::<Vec<_>>()
        ));
    }
}

/// Probe writability by creating and deleting a marker file — a read-only
/// install dir must not silently swallow the perf log.
fn writable(dir: Option<&std::path::Path>) -> bool {
    let Some(dir) = dir else { return false };
    let probe = dir.join(format!(".perf-probe-{}", std::process::id()));
    match std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Append one line. A handful of lines per session — no handle is kept open.
pub fn note(msg: &str) {
    let Some(path) = LOG.get() else { return };
    let t0 = T0.get().copied().unwrap_or_else(Instant::now);
    let line = format!("[{:8.1} ms] {}\n", t0.elapsed().as_secs_f64() * 1000.0, msg);
    match OpenOptions::new().create(true).append(true).open(path) {
        Ok(mut f) => {
            let _ = f.write_all(line.as_bytes());
        }
        // Unwritable install dir: report once, then stop trying.
        Err(e) => {
            if !FAILED.swap(true, Ordering::Relaxed) {
                eprintln!("[perf] {} unwritable: {e}", path.display());
            }
        }
    }
}
