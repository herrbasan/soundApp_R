//! Host diagnostics hook for window-lifecycle transitions.
//!
//! The library owns state a host cannot poll fast enough to reconstruct:
//! `pending_reveal`, the frame that uncloaked it, and the order between a
//! reveal request and a `cloak_and_hide`. A poller samples *around* those
//! transitions and loses exactly the ordering that matters, so nui writes the
//! transition itself, in place, through a sink the host installs.
//!
//! `nui` stays dependency-free: the sink is a plain `fn(&str)`.

use std::sync::Mutex;

static SINK: Mutex<Option<fn(&str)>> = Mutex::new(None);

/// Install the sink that receives every lifecycle line. Call once at startup;
/// a later call replaces the previous sink.
pub fn set_diag_sink(sink: fn(&str)) {
    *SINK.lock().unwrap_or_else(|e| e.into_inner()) = Some(sink);
}

/// True once a sink is installed — hosts use this to gate the expensive parts
/// of their own tracing.
pub fn diag_enabled() -> bool {
    SINK.lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_some()
}

pub(crate) fn diag(msg: &str) {
    let sink = *SINK.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(f) = sink {
        f(msg);
    }
}
