//! Ctrl-C (and SIGTERM, SIGHUP) while `apply` changes files: the signal is held until the
//! change set is done or rolled back, so the game folder and the registry stay in step.
//! During downloads the signal stops the process at once, as usual.

use std::sync::atomic::{AtomicBool, Ordering};

static CRITICAL: AtomicBool = AtomicBool::new(false);
static PENDING: AtomicBool = AtomicBool::new(false);

const SIGNALS: [libc::c_int; 3] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP];

const HELD_MSG: &[u8] = b"\nInterrupt received: finishing the current change set first, then stopping.\n";

extern "C" fn on_signal(sig: libc::c_int) {
    if CRITICAL.load(Ordering::SeqCst) {
        let first = !PENDING.swap(true, Ordering::SeqCst);
        if first {
            // SAFETY: write(2) is async-signal-safe; the buffer is static.
            unsafe {
                libc::write(2, HELD_MSG.as_ptr().cast(), HELD_MSG.len());
            }
        }
    } else {
        // SAFETY: signal(2) and raise(3) are async-signal-safe. Default action: stop now.
        unsafe {
            libc::signal(sig, libc::SIG_DFL);
            libc::raise(sig);
        }
    }
}

/// Holds termination signals while it lives and the phase is critical. Drop restores the old
/// handlers.
pub struct Guard {
    old: Vec<(libc::c_int, libc::sighandler_t)>,
}

impl Guard {
    /// Install the handlers. The phase starts critical.
    pub fn new() -> Guard {
        PENDING.store(false, Ordering::SeqCst);
        CRITICAL.store(true, Ordering::SeqCst);
        let handler = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
        let mut old = Vec::with_capacity(SIGNALS.len());
        for sig in SIGNALS {
            // SAFETY: installs a handler that only touches atomics and async-signal-safe calls.
            let prev = unsafe { libc::signal(sig, handler) };
            if prev == libc::SIG_IGN {
                // Keep a signal ignored (for example SIGHUP under nohup).
                // SAFETY: as above.
                unsafe { libc::signal(sig, libc::SIG_IGN) };
            } else if prev != libc::SIG_ERR {
                old.push((sig, prev));
            }
        }
        Guard { old }
    }

    /// Critical: hold signals. Not critical: a signal stops the process.
    pub fn set_critical(on: bool) {
        CRITICAL.store(on, Ordering::SeqCst);
    }

    /// A signal arrived while it was held.
    pub fn interrupted(&self) -> bool {
        PENDING.load(Ordering::SeqCst)
    }

    /// Progress messages that start with "Downloading" are the download phase (not critical);
    /// every other message is a file or registry change (critical).
    pub fn note_progress(msg: &str) {
        Self::set_critical(!msg.starts_with("Downloading"));
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        CRITICAL.store(false, Ordering::SeqCst);
        for (sig, prev) in &self.old {
            // SAFETY: restores the handler that was in place before `new`.
            unsafe { libc::signal(*sig, *prev) };
        }
    }
}
