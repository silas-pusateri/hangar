//! A `Session` ties one game instance together: config, labels, locked registry, repo index and
//! download cache. CLI and GUI call only this layer to change anything on disk.

mod apply;
mod guard;
#[cfg(test)]
mod tests;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::config::{Config, InstanceEntry, LabelList};
use crate::error::{Error, Result};
use crate::install::DownloadCache;
use crate::instance::Instance;
use crate::meta::Module;
use crate::registry::{Registry, RegistryLock};
use crate::repo::{Progress, RepoIndex, Repository, UpdateResult};
use crate::resolve::{Changeset, Context, Request, ResolverOptions};

/// First line of the `Error::Conflict` text that `apply` returns when unowned files are in the
/// way. Each following line is one relative path (two spaces in front). See `conflict_files`.
pub const UNOWNED_FILES_HEADER: &str =
    "These files already exist and no mod owns them. Move them to the Trash (or remove them) and try again:";

/// Session is `Send`: the GUI moves it into a worker thread for long jobs and gets it back.
pub struct Session {
    pub config: Config,
    pub labels: LabelList,
    pub instance: Instance,
    pub registry: Registry,
    pub index: Arc<RepoIndex>,
    pub cache: DownloadCache,
    pub held: HashSet<String>,
    /// Game version + compatible versions (refresh after changing compat versions).
    pub criteria: crate::meta::VersionCriteria,
    /// Problems found while loading (for example a repo cache that cannot be read). Not fatal.
    pub warnings: Vec<String>,
    /// Repo archive cache dir.
    repo_dir: PathBuf,
    _lock: RegistryLock,
}

/// What `apply` did.
#[derive(Debug, Default, Clone)]
pub struct ApplyReport {
    pub installed: Vec<String>,
    pub removed: Vec<String>,
    pub upgraded: Vec<String>,
    pub messages: Vec<String>,
}

/// The unowned files named in an `apply` conflict error (empty for any other error).
pub fn conflict_files(e: &Error) -> Vec<String> {
    let Error::Conflict(text) = e else { return Vec::new() };
    let mut lines = text.lines();
    if lines.next() != Some(UNOWNED_FILES_HEADER) {
        return Vec::new();
    }
    lines.filter_map(|l| l.strip_prefix("  ")).filter(|l| !l.is_empty()).map(str::to_string).collect()
}

fn conflict_error(files: &[String]) -> Error {
    let mut s = UNOWNED_FILES_HEADER.to_string();
    for f in files {
        s.push_str("\n  ");
        s.push_str(f);
    }
    Error::Conflict(s)
}

/// Register detected installs that are not registered yet. Steam installs are named "Steam",
/// others by their folder name. Returns true if any were added.
fn add_detected(config: &mut Config, found: &[PathBuf]) -> bool {
    let mut added = false;
    for dir in found {
        let path = dir.to_string_lossy().into_owned();
        if config.game_instances.iter().any(|e| e.path == path) {
            continue;
        }
        let steam = path.to_ascii_lowercase().contains("/steamapps/");
        let base = if steam {
            "Steam".to_string()
        } else {
            dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "KSP".to_string())
        };
        let mut name = base.clone();
        let mut n = 2;
        while config.instance(&name).is_some() {
            name = format!("{base} ({n})");
            n += 1;
        }
        config.game_instances.push(InstanceEntry { name, path, game: "KSP".into() });
        added = true;
    }
    added
}

/// By name; else the auto-start instance; else the first (also covers "the only one").
fn pick_entry<'a>(config: &'a Config, name: Option<&str>) -> Result<&'a InstanceEntry> {
    if let Some(n) = name {
        return config.instance(n).ok_or_else(|| Error::NotFound(format!("game instance \"{n}\" is not registered")));
    }
    if let Some(e) = config.auto_start_instance.as_deref().and_then(|n| config.instance(n)) {
        return Ok(e);
    }
    config
        .game_instances
        .first()
        .ok_or_else(|| Error::NotFound("no game instances are registered; add a KSP folder first".into()))
}

/// Load the cached archives of `repos`. A missing cache is skipped; a cache that cannot be read
/// is skipped with a warning. Never fails.
fn load_index_lenient(repos: &[Repository], dir: &Path, warnings: &mut Vec<String>) -> RepoIndex {
    let mut seen = HashSet::new();
    let mut datas = Vec::new();
    for r in repos.iter().filter(|r| seen.insert(r.uri.as_str())) {
        let path = crate::repo::net::find_cache_file(r, dir)
            .or_else(|| crate::repo::net::local_path(&r.uri).filter(|p| p.is_file()));
        let Some(path) = path else { continue };
        match crate::repo::parse_archive(&path) {
            Ok(d) => datas.push((r.clone(), d)),
            Err(e) => warnings.push(format!(
                "Repository {}: the cached metadata cannot be read ({e}). Update the repositories.",
                r.name
            )),
        }
    }
    RepoIndex::build(datas)
}

/// Add a note to an error and keep its kind (so the UI can still tell a conflict from others).
fn with_note(e: Error, note: &str) -> Error {
    match e {
        Error::Io(s) => Error::Io(format!("{s}\n{note}")),
        Error::Json(s) => Error::Json(format!("{s}\n{note}")),
        Error::Net(s) => Error::Net(format!("{s}\n{note}")),
        Error::Metadata(s) => Error::Metadata(format!("{s}\n{note}")),
        Error::NotFound(s) => Error::NotFound(format!("{s}\n{note}")),
        Error::Locked(s) => Error::Locked(format!("{s}\n{note}")),
        Error::Conflict(s) => Error::Conflict(format!("{s}\n{note}")),
        Error::Hash(s) => Error::Hash(format!("{s}\n{note}")),
        Error::Other(s) => Error::Other(format!("{s}\n{note}")),
        other => Error::Other(format!("{other}\n{note}")),
    }
}

impl Session {
    /// Open an instance by name (or the auto-start / only / first registered one; if none are
    /// registered, auto-detect Steam installs and register them). Locks the registry, loads it,
    /// loads the cached repo index (empty if never updated), scans DLLs and DLC.
    pub fn open(config: Config, instance: Option<&str>) -> Result<Session> {
        let autodetect = std::env::var_os("HANGAR_NO_AUTODETECT").is_none();
        let repo_dir = config.repo_cache_dir();
        Self::open_with(config, instance, LabelList::load(), repo_dir, autodetect)
    }

    /// `open` with explicit labels, repo cache dir and auto-detect switch (tests use this, so
    /// they never depend on HANGAR_HOME).
    pub(crate) fn open_with(
        mut config: Config,
        instance: Option<&str>,
        labels: LabelList,
        repo_dir: PathBuf,
        autodetect: bool,
    ) -> Result<Session> {
        let mut warnings = Vec::new();
        if config.game_instances.is_empty() && autodetect && add_detected(&mut config, &crate::instance::find_installs())
            && let Err(e) = config.save()
        {
            warnings.push(format!("Could not save the settings: {e}"));
        }
        let entry = pick_entry(&config, instance)?;
        let name = entry.name.clone();
        let game_dir = PathBuf::from(&entry.path);
        if !crate::instance::is_valid_game_dir(&game_dir) {
            return Err(Error::NotFound(format!(
                "instance \"{name}\": {} is not a KSP game folder (needs buildID64.txt or buildID.txt and GameData)",
                game_dir.display()
            )));
        }
        let ckan_dir = game_dir.join("CKAN");
        // Lock before reading the registry, as CKAN does.
        let lock = RegistryLock::acquire(&ckan_dir)?;
        let registry = Registry::load(&ckan_dir)?;
        let index = load_index_lenient(&registry.repositories(), &repo_dir, &mut warnings);
        let instance = Instance::open(&name, &game_dir, &index.builds)?;
        if instance.version.is_none() {
            return Err(Error::Other(format!(
                "Cannot detect the game version of instance \"{name}\" at {}. Check buildID64.txt and readme.txt.",
                instance.game_dir.display()
            )));
        }
        if let Some(w) = guard::interrupted_warning(&game_dir, &registry) {
            warnings.push(w);
        }
        let criteria = instance.criteria();
        let held = labels.held(&name);
        let cache = DownloadCache::new(config.cache_dir());
        let mut s = Session {
            config,
            labels,
            instance,
            registry,
            index: Arc::new(index),
            cache,
            held,
            criteria,
            warnings,
            repo_dir,
            _lock: lock,
        };
        if s.scan()
            && let Err(e) = s.registry.save(&s.instance, None)
        {
            s.warnings.push(format!("Could not save the registry: {e}"));
        }
        Ok(s)
    }

    /// Resolver context for this session.
    pub fn context(&self) -> Context<'_> {
        Context {
            index: &self.index,
            registry: &self.registry,
            criteria: &self.criteria,
            held: &self.held,
            stability: self.config.hangar.stability,
        }
    }

    /// DLL and DLC scan. True if the registry changed.
    fn scan(&mut self) -> bool {
        let dlls = self.registry.scan_dlls(&self.instance.game_dir);
        let dlc = self.registry.set_dlc(&self.instance.detect_dlc(), Some(&self.index));
        dlls || dlc
    }

    /// Re-read the cached repo archives, then refresh the game version, criteria and DLC.
    fn reload_index(&mut self) {
        let mut warnings = Vec::new();
        let index = load_index_lenient(&self.registry.repositories(), &self.repo_dir, &mut warnings);
        self.warnings = warnings;
        self.index = Arc::new(index);
        if let Some(v) = crate::instance::detect_version(&self.instance.game_dir, &self.index.builds) {
            self.instance.version = Some(v);
        }
        self.criteria = self.instance.criteria();
        if self.registry.set_dlc(&self.instance.detect_dlc(), Some(&self.index))
            && let Err(e) = self.registry.save(&self.instance, None)
        {
            self.warnings.push(format!("Could not save the registry: {e}"));
        }
    }

    /// Download changed repos, reload the index, re-scan DLC with repo metadata.
    pub fn update_repos(&mut self, force: bool, progress: Progress) -> Result<UpdateResult> {
        let repos = self.registry.repositories();
        let res = crate::repo::update(&repos, &self.repo_dir, force, progress);
        // Also after a partial failure: the repos that worked have new data.
        if !matches!(res, Ok(UpdateResult::NoChanges)) || self.index.modules.is_empty() {
            progress("Loading repository data", None);
            self.reload_index();
        }
        res
    }

    pub fn plan(&self, req: &Request, opts: &ResolverOptions) -> Result<Changeset> {
        Ok(crate::resolve::plan(&self.context(), req, opts)?)
    }

    /// Download everything first (up to 4 parallel downloads), then in one FileTx: remove,
    /// then install in order; on error roll back files and restore the registry. Saves the
    /// registry (+ installed-*.ckan, history) on success.
    ///
    /// Unowned files in the way give an `Error::Conflict` that `conflict_files` can read.
    pub fn apply(&mut self, cs: &Changeset, progress: Progress) -> Result<ApplyReport> {
        apply::apply(self, cs, progress)
    }

    /// Download modules into the cache without installing.
    pub fn download(&self, mods: &[Arc<Module>], progress: Progress) -> Result<()> {
        let need: Vec<Arc<Module>> = mods
            .iter()
            .filter(|m| !m.is_metapackage() && !m.is_dlc() && self.cache.get(m).is_none())
            .cloned()
            .collect();
        apply::download_all(&self.cache, &self.config, &need, progress).map(|_| ())
    }

    /// Reload labels and recompute `held`.
    pub fn reload_labels(&mut self) {
        self.labels = LabelList::load();
        self.held = self.labels.held(&self.instance.name);
    }

    /// Switch compatible game versions (writes the file, updates criteria).
    pub fn set_compatible_versions(&mut self, v: &[crate::meta::GameVersion]) -> Result<()> {
        self.instance.set_compatible_versions(v)?;
        self.criteria = self.instance.criteria();
        Ok(())
    }

    /// Mark a module auto-installed or user-installed and save.
    pub fn set_auto_installed(&mut self, id: &str, auto: bool) -> Result<()> {
        match self.registry.installed(id) {
            None => return Err(Error::NotFound(format!("{id} is not installed"))),
            Some(im) if im.source_module.is_dlc() => {
                return Err(Error::Conflict(format!("{id} is a DLC; it has no auto-installed flag")));
            }
            Some(im) if im.auto_installed == auto => return Ok(()),
            Some(_) => {}
        }
        self.registry.set_auto_installed(id, auto);
        self.save_registry()
    }

    /// Save registry.json (+ installed-*.ckan and history).
    pub fn save_registry(&self) -> Result<()> {
        self.registry.save(&self.instance, Some(&self.index))
    }

    /// Re-scan unmanaged DLLs and DLC; save if changed. Returns true if changed.
    pub fn rescan(&mut self) -> Result<bool> {
        let changed = self.scan();
        if changed {
            self.registry.save(&self.instance, None)?;
        }
        Ok(changed)
    }

    /// Replace the instance's repository list (stored in registry.json) and save.
    /// List order is the priority order (the first repo wins); names must be unique and not
    /// empty. The index is reloaded from the caches of the new list.
    pub fn set_repositories(&mut self, repos: Vec<crate::repo::Repository>) -> Result<()> {
        let mut map = std::collections::BTreeMap::new();
        for (i, mut r) in repos.into_iter().enumerate() {
            if r.name.trim().is_empty() || r.uri.trim().is_empty() {
                return Err(Error::Other("a repository needs a name and a URL".into()));
            }
            if map.contains_key(&r.name) {
                return Err(Error::Conflict(format!("two repositories are named \"{}\"", r.name)));
            }
            r.priority = i32::try_from(i).unwrap_or(i32::MAX);
            map.insert(r.name.clone(), r);
        }
        let old = std::mem::replace(&mut self.registry.sorted_repositories, map);
        if let Err(e) = self.registry.save(&self.instance, None) {
            self.registry.sorted_repositories = old;
            return Err(e);
        }
        self.reload_index();
        Ok(())
    }

    /// Move unowned files (relative paths, e.g. from an Error::Conflict) to the macOS Trash so an
    /// install can proceed. Refuses paths owned by a module or outside the game dir.
    pub fn trash_files(&self, rel_paths: &[String]) -> Result<()> {
        let game = &self.instance.game_dir;
        let canon_game = std::fs::canonicalize(game).map_err(|e| Error::Io(format!("{}: {e}", game.display())))?;
        // The volume may ignore case, so compare without case.
        let owned: Vec<String> = self.registry.installed_files.keys().map(|k| k.to_ascii_lowercase()).collect();
        let mut targets = Vec::with_capacity(rel_paths.len());
        for raw in rel_paths {
            let rel = crate::util::normalize_path(raw);
            let refuse = |why: &str| Error::Conflict(format!("{raw}: {why}; not moved to the Trash"));
            if rel.is_empty()
                || rel.starts_with('/')
                || rel.split('/').any(|p| p.is_empty() || p == "." || p == "..")
            {
                return Err(refuse("not a relative path inside the game folder"));
            }
            let lower = rel.to_ascii_lowercase();
            let under = |root: &str| {
                let r = root.to_ascii_lowercase();
                lower == r || lower.starts_with(&format!("{r}/"))
            };
            if under("CKAN") || crate::instance::STOCK_FOLDERS.iter().any(|s| under(s)) {
                return Err(refuse("it belongs to the game or to the mod manager"));
            }
            if crate::install::RESERVED_DIRS.iter().any(|d| d.eq_ignore_ascii_case(&rel)) {
                return Err(refuse("it is a game folder"));
            }
            let prefix = format!("{lower}/");
            if owned.iter().any(|o| *o == lower || o.starts_with(&prefix)) {
                return Err(refuse("it is owned by an installed mod"));
            }
            let abs = game.join(&rel);
            if std::fs::symlink_metadata(&abs).is_err() {
                return Err(Error::NotFound(format!("{raw} does not exist in the game folder")));
            }
            // A symlinked parent must not lead out of the game folder.
            let parent_ok = abs
                .parent()
                .and_then(|p| std::fs::canonicalize(p).ok())
                .is_some_and(|p| p.starts_with(&canon_game));
            if !parent_ok {
                return Err(refuse("it is outside the game folder"));
            }
            targets.push(abs);
        }
        for t in targets {
            crate::util::trash(&t)?;
        }
        Ok(())
    }
}
