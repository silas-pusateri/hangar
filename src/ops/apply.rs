//! Parallel downloads and the install / remove transaction behind `Session::apply`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::{ApplyReport, Session, conflict_error, with_note};
use crate::config::Config;
use crate::error::{Error, Result};
use crate::install::{self, DownloadCache, FileTx};
use crate::instance::Instance;
use crate::meta::Module;
use crate::registry::Registry;
use crate::repo::Progress;
use crate::resolve::{ChangeKind, Changeset};

/// Downloads that run at the same time.
pub const MAX_PARALLEL_DOWNLOADS: usize = 4;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Download `mods` into the cache on up to 4 threads. Progress gets each file's message and the
/// overall fraction (by bytes when all sizes are known). Returns the cached paths in the order
/// of `mods`. Every module is tried; if any fails, the error names all failures.
/// Modules with the same first download URL (one zip for several mods, as CKAN allows) are
/// downloaded once: two downloads of one URL would write the same partial and cache file.
pub(super) fn download_all(
    cache: &DownloadCache,
    config: &Config,
    mods: &[Arc<Module>],
    progress: Progress,
) -> Result<Vec<PathBuf>> {
    if mods.is_empty() {
        return Ok(Vec::new());
    }
    // leader[i] = index in `uniq` of the download that gives mods[i] its file.
    let mut uniq: Vec<&Arc<Module>> = Vec::new();
    let mut leader: Vec<usize> = Vec::with_capacity(mods.len());
    let mut by_url: HashMap<&str, usize> = HashMap::new();
    for m in mods {
        let k = match m.download.first() {
            Some(url) => *by_url.entry(url.as_str()).or_insert(uniq.len()),
            None => uniq.len(),
        };
        if k == uniq.len() {
            uniq.push(m);
        }
        leader.push(k);
    }
    let need: u64 = uniq.iter().map(|m| m.download_size).fold(0, u64::saturating_add);
    check_space(&config.cache_dir(), need, "store the downloads")?;
    let sized = uniq.iter().all(|m| m.download_size > 0);
    let weights: Vec<f64> = uniq.iter().map(|m| if sized { m.download_size as f64 } else { 1.0 }).collect();
    let total: f64 = weights.iter().sum::<f64>().max(1.0);
    let done = Mutex::new(vec![0f32; uniq.len()]);
    let results: Mutex<Vec<Option<Result<PathBuf>>>> = Mutex::new(vec![None; uniq.len()]);
    let next = AtomicUsize::new(0);
    let agent = crate::repo::http_agent();
    let auth = |host: &str| config.auth_tokens.get(host).cloned();
    let overall = |i: usize, f: Option<f32>| -> f32 {
        let mut d = lock(&done);
        // No fraction (size unknown): keep the last one so the total never goes back.
        if let (Some(slot), Some(f)) = (d.get_mut(i), f) {
            *slot = f.clamp(0.0, 1.0);
        }
        let sum: f64 = d.iter().zip(&weights).map(|(f, w)| f64::from(*f) * w).sum();
        (sum / total) as f32
    };
    let what = if uniq.len() == 1 { "1 file".to_string() } else { format!("{} files", uniq.len()) };
    progress(&format!("Downloading {what}"), Some(0.0));
    std::thread::scope(|s| {
        for _ in 0..uniq.len().min(MAX_PARALLEL_DOWNLOADS) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(m) = uniq.get(i) else { break };
                    let cb = |msg: &str, f: Option<f32>| {
                        let all = overall(i, f);
                        progress(msg, Some(all));
                    };
                    let r = cache.download(m, &agent, &auth, &cb);
                    if r.is_ok() {
                        overall(i, Some(1.0));
                    }
                    if let Some(slot) = lock(&results).get_mut(i) {
                        *slot = Some(r);
                    }
                }
            });
        }
    });
    let results = results.into_inner().unwrap_or_else(|e| e.into_inner());
    let mut errors: Vec<Error> = Vec::new();
    for (m, r) in uniq.iter().zip(&results) {
        match r {
            Some(Ok(_)) => {}
            Some(Err(e)) => errors.push(e.clone()),
            None => errors.push(Error::Other(format!("{m}: download did not run"))),
        }
    }
    match errors.len() {
        0 => {}
        1 => return Err(errors.remove(0)),
        n => {
            let list: Vec<String> = errors.iter().map(|e| e.to_string()).collect();
            return Err(Error::Net(format!("{n} downloads failed:\n  {}", list.join("\n  "))));
        }
    }
    let mut paths = Vec::with_capacity(mods.len());
    for (m, k) in mods.iter().zip(leader) {
        match results.get(k) {
            Some(Some(Ok(p))) => paths.push(p.clone()),
            _ => return Err(Error::Other(format!("{m}: download did not run"))),
        }
    }
    Ok(paths)
}

/// Free bytes for this user on the volume that holds `dir` (or its nearest existing parent).
pub(super) fn free_bytes(dir: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let existing = dir.ancestors().find(|p| p.is_dir())?;
    let c = std::ffi::CString::new(existing.as_os_str().as_bytes()).ok()?;
    // SAFETY: statvfs only writes into `st`; an all-zero statvfs is a valid value.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a valid C string and `st` a valid out pointer.
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    Some(u64::from(st.f_bavail).saturating_mul(st.f_frsize))
}

/// CKAN CheckFreeSpace: refuse when `need` bytes do not fit in `dir`. Unknown free space passes.
fn check_space(dir: &Path, need: u64, what: &str) -> Result<()> {
    match free_bytes(dir) {
        Some(have) if need > have => Err(Error::Io(format!(
            "Not enough free space in {} to {what}: {} needed, {} free.",
            dir.display(),
            crate::util::human_size(need),
            crate::util::human_size(have)
        ))),
        _ => Ok(()),
    }
}

/// CKAN cache trim order: files of no repo module go first, then files of modules with no
/// compatible version, then the rest; oldest first within each group. The hash map over all
/// module versions is built only when the cache is over the limit.
fn trim_cache(s: &Session, limit: u64) -> Result<()> {
    if s.cache.total_size() <= limit {
        return Ok(());
    }
    let mut rank: HashMap<String, u8> = HashMap::new();
    for m in s.index.modules.values().flatten() {
        let r = if m.is_compatible(&s.criteria) { 2 } else { 1 };
        for url in &m.download {
            let h = crate::install::cache::url_hash(url);
            let slot = rank.entry(h.get(..8).unwrap_or("").to_string()).or_insert(r);
            *slot = (*slot).max(r);
        }
    }
    s.cache.trim_ranked(limit, &|h| rank.get(h).copied().unwrap_or(0))
}

fn is_corrupt_zip(e: &Error) -> bool {
    matches!(e, Error::Io(s) if s.contains("corrupt zip in cache"))
}

/// A corrupt cached zip: delete it and say so in the error.
fn purge_corrupt(cache: &DownloadCache, m: &Module, e: Error) -> Error {
    if !is_corrupt_zip(&e) {
        return e;
    }
    match cache.purge(m) {
        Ok(()) => with_note(e, &format!("The cached download of {m} was deleted; apply again to download it again.")),
        Err(pe) => with_note(e, &format!("Could not delete the cached download of {m}: {pe}")),
    }
}

/// Everything `run` needs, borrowed from the session.
struct Job<'a> {
    inst: &'a Instance,
    reg: &'a mut Registry,
    cache: &'a DownloadCache,
    zips: &'a HashMap<String, PathBuf>,
    filters: &'a [String],
    progress: Progress<'a>,
}

impl Job<'_> {
    /// Remove, check for unowned files in the way, then install. Changes go through `tx`.
    fn run(&mut self, cs: &Changeset, tx: &mut FileTx, report: &mut ApplyReport) -> Result<()> {
        let steps = cs.changes.len().max(1) as f32;
        let mut step = 0f32;
        // Old sides first: Remove, the installed copy of an Upgrade, the replaced module.
        for c in &cs.changes {
            let id = match c.kind {
                ChangeKind::Remove | ChangeKind::Upgrade => c.module.identifier.as_str(),
                ChangeKind::Replace => match c.replaces.as_deref() {
                    Some(r) => r,
                    None => continue,
                },
                ChangeKind::Install => continue,
            };
            let Some(old) = self.reg.installed(id) else {
                if c.kind == ChangeKind::Remove {
                    return Err(Error::NotFound(format!("{id} is not installed")));
                }
                continue;
            };
            let old_text = format!("{id} {}", old.source_module.version);
            (self.progress)(&format!("Removing {old_text}"), Some(step / steps));
            let kept = install::uninstall_module_ex(self.inst, self.reg, id, tx)?;
            if c.kind != ChangeKind::Upgrade {
                report.removed.push(old_text);
                for d in kept {
                    report.messages.push(format!("{d} was kept: it holds files that no mod owns"));
                }
            }
            if c.kind == ChangeKind::Remove {
                step += 1.0;
            }
        }

        // Unowned files in the way, for all modules at once (the UI offers to trash them).
        let mut unowned: Vec<String> = Vec::new();
        for c in cs.changes.iter().filter(|c| c.kind != ChangeKind::Remove) {
            let m = &c.module;
            if m.is_metapackage() || m.is_dlc() || self.reg.installed(&m.identifier).is_some() {
                continue;
            }
            let Some(zip) = self.zips.get(&m.identifier) else { continue };
            let entries = install::zip_entry_names(zip).map_err(|e| purge_corrupt(self.cache, m, e))?;
            let files = install::module_files(m, &entries, self.filters)?;
            // A manually installed DLL at a destination is replaced by the installer.
            let dll = self.reg.installed_dlls.get(&m.identifier);
            for f in install::existing_conflicts(self.inst, self.reg, &files) {
                if dll != Some(&f) && !unowned.contains(&f) {
                    unowned.push(f);
                }
            }
        }
        if !unowned.is_empty() {
            unowned.sort();
            return Err(conflict_error(&unowned));
        }

        for c in cs.changes.iter().filter(|c| c.kind != ChangeKind::Remove) {
            let m = &c.module;
            (self.progress)(&format!("Installing {m}"), Some(step / steps));
            let zip = self.zips.get(&m.identifier).map(PathBuf::as_path);
            install::install_module(self.inst, self.reg, m, zip, c.auto_installed, self.filters, tx)
                .map_err(|e| purge_corrupt(self.cache, m, e))?;
            match c.kind {
                ChangeKind::Upgrade => {
                    let old = c.old_version.as_ref().map(|v| v.to_string()).unwrap_or_else(|| "?".into());
                    report.upgraded.push(format!("{} {old} -> {}", m.identifier, m.version));
                }
                _ => report.installed.push(m.to_string()),
            }
            step += 1.0;
        }
        Ok(())
    }
}

/// Undo the files and restore the registry; keep the error kind.
fn undo(tx: FileTx, reg: &mut Registry, backup: Registry, e: Error) -> Error {
    *reg = backup;
    match tx.rollback() {
        Ok(()) => e,
        Err(re) => with_note(e, &format!("Undoing the file changes also failed: {re}")),
    }
}

pub(super) fn apply(s: &mut Session, cs: &Changeset, progress: Progress) -> Result<ApplyReport> {
    let mut report = ApplyReport::default();
    if cs.is_empty() {
        return Ok(report);
    }

    // 1. Downloads. Nothing is installed unless every download worked.
    let mut zips: HashMap<String, PathBuf> = HashMap::new();
    let mut need: Vec<Arc<Module>> = Vec::new();
    for m in cs.downloads() {
        match s.cache.get(&m) {
            Some(p) => {
                zips.insert(m.identifier.clone(), p);
            }
            None => need.push(m),
        }
    }
    let fetched = download_all(&s.cache, &s.config, &need, progress)?;
    for (m, p) in need.iter().zip(fetched) {
        zips.insert(m.identifier.clone(), p);
    }

    // install_size is 0 when the metadata does not give it.
    let install_size: u64 = cs
        .changes
        .iter()
        .filter(|c| c.kind != ChangeKind::Remove)
        .map(|c| c.module.install_size)
        .fold(0, u64::saturating_add);
    check_space(&s.instance.game_dir, install_size, "install the mods")?;

    // 2. One file transaction for all changes. Ctrl-C and SIGTERM wait until it is done.
    let _hold = super::guard::SignalGuard::new();
    let mut filters = s.config.global_install_filters();
    filters.extend(s.instance.install_filters());
    // CKAN scans DLLs before install, so a manual copy of a mod is seen.
    s.registry.scan_dlls(&s.instance.game_dir);
    let backup = s.registry.clone();
    let mut tx = FileTx::begin(&s.instance.game_dir)?;
    let mut job =
        Job { inst: &s.instance, reg: &mut s.registry, cache: &s.cache, zips: &zips, filters: &filters, progress };
    if let Err(e) = job.run(cs, &mut tx, &mut report) {
        return Err(undo(tx, &mut s.registry, backup, e));
    }

    // 3. registry.json before the commit: if it cannot be written, undo everything.
    progress("Saving the registry", Some(1.0));
    if let Err(e) = s.registry.save(&s.instance, None) {
        return Err(undo(tx, &mut s.registry, backup, e));
    }
    drop(backup);
    if let Err(e) = tx.commit() {
        report.messages.push(format!("Could not clean up the undo files: {e}"));
    }
    // installed-<name>.ckan and the history snapshot.
    if let Err(e) = s.registry.save(&s.instance, Some(&s.index)) {
        report.messages.push(format!("Could not write the installed-mods list: {e}"));
    }
    if let Some(limit) = s.config.cache_size_limit.filter(|l| *l > 0)
        && let Err(e) = trim_cache(s, limit)
    {
        report.messages.push(format!("Could not trim the download cache: {e}"));
    }
    Ok(report)
}
