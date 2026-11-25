//! Keep a file change set whole: defer Ctrl-C / SIGTERM / SIGHUP while game files change, and
//! find what an earlier run that was killed anyway (SIGKILL, power loss) left behind.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicI32, Ordering};

use crate::registry::Registry;

const SIGNALS: [libc::c_int; 3] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP];

/// The last deferred signal (0 = none).
static PENDING: AtomicI32 = AtomicI32::new(0);

/// Number of live guards and the handlers they replaced (set by the first guard).
static STATE: Mutex<(usize, Vec<(libc::c_int, libc::sighandler_t)>)> = Mutex::new((0, Vec::new()));

extern "C" fn defer(sig: libc::c_int) {
    // Only an atomic store: safe in a signal handler.
    PENDING.store(sig, Ordering::SeqCst);
}

/// While a guard lives, the signals above are recorded instead of stopping the process. When
/// the last guard drops, the old handlers come back and a recorded signal is raised again, so
/// the process stops after the change set is done, not in the middle of it.
pub(super) struct SignalGuard(());

impl SignalGuard {
    pub(super) fn new() -> SignalGuard {
        let mut st = STATE.lock().unwrap_or_else(|e| e.into_inner());
        if st.0 == 0 {
            st.1.clear();
            let handler = defer as extern "C" fn(libc::c_int) as libc::sighandler_t;
            for sig in SIGNALS {
                // SAFETY: `defer` is async-signal-safe; the old handler is put back on drop.
                let old = unsafe { libc::signal(sig, handler) };
                if old != libc::SIG_ERR {
                    st.1.push((sig, old));
                }
            }
        }
        st.0 += 1;
        SignalGuard(())
    }
}

impl Drop for SignalGuard {
    fn drop(&mut self) {
        let mut st = STATE.lock().unwrap_or_else(|e| e.into_inner());
        st.0 = st.0.saturating_sub(1);
        if st.0 > 0 {
            return;
        }
        for (sig, old) in st.1.drain(..) {
            // SAFETY: puts back the handler that was there before the first guard.
            unsafe { libc::signal(sig, old) };
        }
        drop(st);
        let sig = PENDING.swap(0, Ordering::SeqCst);
        if sig != 0 {
            // SAFETY: raise only sends a signal to this process.
            unsafe { libc::raise(sig) };
        }
    }
}

/// Take the deferred signal (tests only; a real run raises it on drop).
#[cfg(test)]
pub(super) fn take_pending() -> i32 {
    PENDING.swap(0, Ordering::SeqCst)
}

/// Undo dirs (`CKAN/tx-*`) left by a run that stopped in the middle of a change set.
/// Call only while holding the registry lock (then no other run is using them).
pub(super) fn leftover_undo_dirs(game_dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(game_dir.join("CKAN")) else { return Vec::new() };
    let mut v: Vec<PathBuf> = rd
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("tx-"))
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect();
    v.sort();
    v
}

/// A warning when an earlier change set did not finish: where the removed files are, and
/// which installed mods now miss files. None if no undo dir is left.
pub(super) fn interrupted_warning(game_dir: &Path, reg: &Registry) -> Option<String> {
    let dirs = leftover_undo_dirs(game_dir);
    if dirs.is_empty() {
        return None;
    }
    let names: Vec<String> = dirs
        .iter()
        .map(|d| d.strip_prefix(game_dir).unwrap_or(d).to_string_lossy().into_owned())
        .collect();
    let mut broken: Vec<String> = reg
        .installed_mods()
        .filter(|im| !im.source_module.is_dlc())
        .filter(|im| im.installed_files.keys().any(|f| std::fs::symlink_metadata(game_dir.join(f)).is_err()))
        .map(|im| im.source_module.identifier.clone())
        .collect();
    broken.sort();
    let mut s = format!(
        "An earlier install or removal in this game folder was stopped before it finished. \
         Files it removed are kept in: {}.",
        names.join(", ")
    );
    if broken.is_empty() {
        s.push_str(" No installed mod is missing files; you can move that folder to the Trash.");
    } else {
        s.push_str(&format!(" These mods are missing files; remove them and install them again: {}.", broken.join(", ")));
    }
    s.push_str(" Files it added that no mod owns are listed as conflicts on the next install.");
    Some(s)
}
