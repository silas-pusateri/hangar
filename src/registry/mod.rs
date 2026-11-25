//! Installed-mod state in CKAN's on-disk format: `<game>/CKAN/registry.json` (version 3),
//! `registry.locked`, `installed-<name>.ckan` and `history/` snapshots. Real CKAN and Hangar
//! can manage the same game folder.

mod time;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::Write as _;
use std::ops::Bound;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use crate::error::{Ctx, Error, Result};
use crate::instance::Instance;
use crate::meta::{Module, ModuleVersion};
use crate::repo::{RepoIndex, Repository};

/// registry.json version that this code reads and writes (CKAN LATEST_REGISTRY_VERSION).
pub const LATEST_REGISTRY_VERSION: u32 = 3;
/// Name of the registry file inside `<game>/CKAN`.
pub const REGISTRY_FILE: &str = "registry.json";
/// Name of the lock file inside `<game>/CKAN`.
pub const LOCK_FILE: &str = "registry.locked";
/// Version text used for a DLC whose version is not known (CKAN UnmanagedModuleVersion(null)).
pub const UNKNOWN_DLC_VERSION: &str = "0";

const OLD_DEFAULT_REPO_URI: &str = "https://github.com/KSP-CKAN/CKAN-meta/archive/master.zip";
/// Folder depth limit for the DLL scan (guards against symlink loops).
const MAX_SCAN_DEPTH: usize = 40;

/// Empty JSON object `{}` (CKAN InstalledModuleFile has no members; old `sha1_sum` keys are ignored).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FileEntry {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledModule {
    /// ISO-8601 UTC.
    #[serde(default)]
    pub install_time: String,
    pub source_module: Module,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub auto_installed: bool,
    /// Relative paths (with `/`), files and explicit dirs.
    #[serde(default, deserialize_with = "de_file_entries")]
    pub installed_files: BTreeMap<String, FileEntry>,
}

/// What is installed under an identifier (or a name another module provides).
#[derive(Debug, Clone, PartialEq)]
pub enum InstalledVersion {
    /// A managed module at this version.
    Module(ModuleVersion),
    /// An unmanaged DLL found in GameData (version unknown).
    Dll,
    /// A detected DLC (version may be unknown).
    Dlc(Option<ModuleVersion>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Registry {
    #[serde(default, serialize_with = "ser_latest_version")]
    pub registry_version: u32,
    #[serde(default)]
    pub sorted_repositories: BTreeMap<String, Repository>,
    /// identifier -> relative path of an unmanaged DLL.
    #[serde(default)]
    pub installed_dlls: BTreeMap<String, String>,
    /// Includes DLC entries (`source_module.kind == dlc`, no files).
    #[serde(default)]
    pub installed_modules: BTreeMap<String, InstalledModule>,
    /// relative path -> owner identifier.
    #[serde(default)]
    pub installed_files: BTreeMap<String, String>,
    /// Always written as `{}` (legacy keys kept for older CKAN clients).
    #[serde(default, serialize_with = "ser_empty_object", deserialize_with = "de_discard")]
    pub available_modules: serde_json::Map<String, serde_json::Value>,
    #[serde(default, serialize_with = "ser_empty_object", deserialize_with = "de_discard")]
    pub download_counts: serde_json::Map<String, serde_json::Value>,
}

// ---------------------------------------------------------------- serde helpers

/// Accept a null or missing map, and any value per file (old registries have `{"sha1_sum": ..}`).
fn de_file_entries<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<BTreeMap<String, FileEntry>, D::Error> {
    let m: Option<BTreeMap<String, serde::de::IgnoredAny>> = Option::deserialize(d)?;
    Ok(m.unwrap_or_default().into_keys().map(|k| (k, FileEntry {})).collect())
}

fn ser_latest_version<S: Serializer>(_v: &u32, s: S) -> std::result::Result<S::Ok, S::Error> {
    s.serialize_u32(LATEST_REGISTRY_VERSION)
}

/// Older CKAN clients crash on null here, so always write `{}`.
fn ser_empty_object<S: Serializer>(_v: &serde_json::Map<String, Value>, s: S) -> std::result::Result<S::Ok, S::Error> {
    serde_json::Map::new().serialize(s)
}

/// The legacy value is never used: skip it without building it in memory.
fn de_discard<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<serde_json::Map<String, Value>, D::Error> {
    serde::de::IgnoredAny::deserialize(d)?;
    Ok(serde_json::Map::new())
}

// ---------------------------------------------------------------- lock

/// Exclusive lock on `<game>/CKAN/registry.locked` (holds our PID). Removed on drop, but only
/// while it still holds our PID. A stale lock (dead PID) is removed before locking.
#[derive(Debug)]
pub struct RegistryLock {
    pub path: PathBuf,
    /// Kept open while locked, as CKAN does.
    _file: Option<std::fs::File>,
}

/// True if a process with this PID exists (EPERM means it exists but is not ours).
fn pid_alive(pid: i32) -> bool {
    if pid <= 0 {
        // kill(0 or negative) addresses process groups; .NET reports such ids as not running.
        return false;
    }
    // SAFETY: signal 0 only checks that the process exists; nothing is sent.
    let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if r == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

/// The program name of a running process (macOS only; None elsewhere or on failure).
fn process_name(pid: i32) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let mut buf = [0u8; 4096];
        // SAFETY: the buffer is valid for its full length; the call writes at most that much.
        let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), 4096) };
        let n = usize::try_from(n).ok().filter(|n| *n > 0)?;
        let path = String::from_utf8_lossy(buf.get(..n)?).into_owned();
        Path::new(&path).file_name().map(|f| f.to_string_lossy().into_owned())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = pid;
        None
    }
}

/// Who holds a lock, for the error text: "another Hangar window (process 123)".
fn lock_holder(text: &str) -> String {
    let Ok(pid) = text.trim().parse::<i32>() else {
        return "another process".to_string();
    };
    match process_name(pid).as_deref() {
        Some("hangar") => format!("another Hangar window (process {pid})"),
        Some(name) => format!("{name} (process {pid})"),
        None => format!("process {pid}"),
    }
}

/// CKAN CheckStaleLock: delete the lock only if it holds an integer PID of a dead process.
fn remove_stale_lock(path: &Path) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return; // missing or unreadable: keep
    };
    if let Ok(pid) = text.trim().parse::<i32>()
        && !pid_alive(pid)
    {
        let _ = std::fs::remove_file(path);
    }
}

impl RegistryLock {
    pub fn acquire(ckan_dir: &Path) -> Result<RegistryLock> {
        std::fs::create_dir_all(ckan_dir).ctx(ckan_dir.display())?;
        let path = ckan_dir.join(LOCK_FILE);
        remove_stale_lock(&path);
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(f) => {
                // Build the guard first so a failed write still removes the file.
                let mut lock = RegistryLock { path, _file: None };
                let mut f = f;
                if let Err(e) = f.write_all(std::process::id().to_string().as_bytes()).and_then(|_| f.flush()) {
                    // The file is empty or partial: it cannot hold our PID, so remove it here.
                    let _ = std::fs::remove_file(&lock.path);
                    return Err(Error::Io(format!("{}: {e}", lock.path.display())));
                }
                lock._file = Some(f);
                Ok(lock)
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let who = lock_holder(&std::fs::read_to_string(&path).unwrap_or_default());
                Err(Error::Locked(format!(
                    "{} is held by {who}. Close the other CKAN or Hangar, or delete the file if no other program uses this game folder.",
                    path.display()
                )))
            }
            Err(e) => Err(Error::Io(format!("{}: {e}", path.display()))),
        }
    }
}

impl Drop for RegistryLock {
    fn drop(&mut self) {
        // Do not delete a lock that another process made after ours was removed by hand.
        let ours = std::fs::read_to_string(&self.path)
            .is_ok_and(|t| t.trim().parse::<u32>().ok() == Some(std::process::id()));
        if ours {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

// ---------------------------------------------------------------- load helpers

/// Result of parsing registry.json text.
enum Parsed {
    Ok(Box<Registry>),
    /// Not JSON, or not a JSON object: CKAN archives it and starts over.
    Corrupt(String),
}

/// Remove null object members at any depth (CKAN treats null as absent; old registries are full
/// of them). A null module `version` (DLC with unknown version) becomes UNKNOWN_DLC_VERSION.
fn strip_nulls(v: &mut Value) {
    match v {
        Value::Object(o) => {
            o.retain(|_, x| !x.is_null());
            for x in o.values_mut() {
                strip_nulls(x);
            }
        }
        Value::Array(a) => {
            for x in a.iter_mut() {
                strip_nulls(x);
            }
        }
        _ => {}
    }
}

/// Newtonsoft's value for a DateTime that is missing from the file (default(DateTime) in UTC).
const DEFAULT_INSTALL_TIME: &str = "0001-01-01T00:00:00Z";

/// Null members of an installed module count as absent (`"install_time": null` and
/// `"auto_installed": null` are valid for Newtonsoft). Keys of `installed_files` are kept.
fn prepare_installed_module(im: &mut Value) {
    if let Some(o) = im.as_object_mut() {
        o.retain(|k, x| !x.is_null() || k == "source_module");
    }
    prepare_source_module(im);
}

/// Drop null values from a `{string: string}` map (Newtonsoft accepts them; serde does not).
fn drop_null_values(v: &mut Value) {
    if let Some(o) = v.as_object_mut() {
        o.retain(|_, x| !x.is_null());
    }
}

fn prepare_source_module(im: &mut Value) {
    let Some(sm) = im.get_mut("source_module") else { return };
    if let Some(o) = sm.as_object_mut()
        && o.get("version").is_some_and(Value::is_null)
    {
        o.insert("version".into(), Value::String(UNKNOWN_DLC_VERSION.into()));
    }
    strip_nulls(sm);
}

fn json_err(what: &str, e: serde_json::Error) -> Error {
    Error::Json(format!("registry.json {what}: {e}"))
}

/// registry_version as Newtonsoft reads it: number, numeric string, null/missing = 0.
fn read_version(v: Option<&Value>) -> Result<i64> {
    match v {
        None | Some(Value::Null) => Ok(0),
        Some(Value::Number(n)) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f as i64))
            .ok_or_else(|| Error::Json(format!("registry.json: bad registry_version {n}"))),
        Some(Value::String(s)) => {
            s.trim().parse().map_err(|_| Error::Json(format!("registry.json: bad registry_version {s:?}")))
        }
        Some(o) => Err(Error::Json(format!("registry.json: bad registry_version {o}"))),
    }
}

/// Make a stored path relative to the game dir (only rooted paths change) and normalized.
/// A rooted path outside the game dir is an error, as in CKAN (ToRelativeGameDir throws):
/// keeping it would later join to a path outside the game folder.
fn relativize(game_dir: &Path, p: &str) -> Result<String> {
    let n = crate::util::normalize_path(p);
    if !n.starts_with('/') {
        return Ok(n);
    }
    let rel = crate::util::rel_path(game_dir, Path::new(&n)).ok_or_else(|| {
        Error::Metadata(format!("registry.json: installed path {n} is not inside {}", game_dir.display()))
    })?;
    // CKAN strips the leading slashes that remain after the root.
    Ok(rel.trim_start_matches('/').to_string())
}

fn parse_registry(text: &str, game_dir: &Path) -> Result<Parsed> {
    let root: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => return Ok(Parsed::Corrupt(e.to_string())),
    };
    let Value::Object(mut obj) = root else {
        return Ok(Parsed::Corrupt("top level is not a JSON object".into()));
    };
    let version = read_version(obj.get("registry_version"))?;
    if version > LATEST_REGISTRY_VERSION as i64 {
        return Err(Error::Metadata(format!(
            "registry.json has registry_version {version}, newer than supported ({LATEST_REGISTRY_VERSION}). \
             It was written by a newer CKAN; it is left unchanged."
        )));
    }

    let mut take = |k: &str| obj.remove(k).filter(|v| !v.is_null());

    let repos: BTreeMap<String, Repository> = match take("sorted_repositories") {
        Some(v) => serde_json::from_value(v).map_err(|e| json_err("sorted_repositories", e))?,
        None => BTreeMap::new(),
    };
    let dlls: BTreeMap<String, String> = match take("installed_dlls") {
        Some(mut v) => {
            drop_null_values(&mut v);
            serde_json::from_value(v).map_err(|e| json_err("installed_dlls", e))?
        }
        None => BTreeMap::new(),
    };
    let mut modules = BTreeMap::new();
    if let Some(v) = take("installed_modules") {
        let Value::Object(map) = v else {
            return Err(Error::Json("registry.json installed_modules: not an object".into()));
        };
        for (id, mut im) in map {
            prepare_installed_module(&mut im);
            let m: InstalledModule =
                serde_json::from_value(im).map_err(|e| json_err(&format!("installed_modules.{id}"), e))?;
            modules.insert(id, m);
        }
    }
    let files: Option<BTreeMap<String, String>> = match take("installed_files") {
        Some(mut v) => {
            drop_null_values(&mut v);
            Some(serde_json::from_value(v).map_err(|e| json_err("installed_files", e))?)
        }
        None => None,
    };
    let files_missing = files.is_none();

    let mut reg = Registry {
        registry_version: version.max(0) as u32,
        sorted_repositories: repos,
        installed_dlls: dlls,
        installed_modules: modules,
        installed_files: files.unwrap_or_default(),
        available_modules: serde_json::Map::new(),
        download_counts: serde_json::Map::new(),
    };
    reg.upgrade(version, files_missing, game_dir)?;
    Ok(Parsed::Ok(Box::new(reg)))
}

// ---------------------------------------------------------------- registry

impl Registry {
    /// Empty registry with the default repo.
    pub fn empty() -> Registry {
        let mut reg = Registry {
            registry_version: LATEST_REGISTRY_VERSION,
            sorted_repositories: BTreeMap::new(),
            installed_dlls: BTreeMap::new(),
            installed_modules: BTreeMap::new(),
            installed_files: BTreeMap::new(),
            available_modules: serde_json::Map::new(),
            download_counts: serde_json::Map::new(),
        };
        reg.ascertain_default_repo();
        reg
    }

    /// CKAN AscertainDefaultRepo: no repositories -> the default one.
    fn ascertain_default_repo(&mut self) {
        if self.sorted_repositories.is_empty() {
            let r = Repository::default_repo();
            self.sorted_repositories.insert(r.name.clone(), r);
        }
    }

    /// CKAN DeSerialisationFixes (Registry.cs) plus time normalisation.
    fn upgrade(&mut self, version: i64, files_missing: bool, game_dir: &Path) -> Result<()> {
        for m in self.installed_modules.values_mut() {
            if m.install_time.trim().is_empty() {
                // Missing in the file: CKAN reads default(DateTime). Never write "" back,
                // because CKAN cannot parse it and would archive the whole registry.
                m.install_time = DEFAULT_INSTALL_TIME.to_string();
            } else if let Some(t) = time::normalize_to_utc(&m.install_time) {
                m.install_time = t;
            }
        }
        if files_missing {
            self.reindex();
        }
        if version < 1 {
            let mut files = BTreeMap::new();
            for (k, v) in std::mem::take(&mut self.installed_files) {
                files.insert(relativize(game_dir, &k)?, v);
            }
            self.installed_files = files;
            for m in self.installed_modules.values_mut() {
                let mut own = BTreeMap::new();
                for k in std::mem::take(&mut m.installed_files).into_keys() {
                    own.insert(relativize(game_dir, &k)?, FileEntry {});
                }
                m.installed_files = own;
            }
        }
        if version < 2 {
            const OLD: &str = "001ControlLock";
            const NEW: &str = "ControlLock";
            if let Some(mut im) = self.installed_modules.remove(OLD) {
                im.source_module.identifier = NEW.to_string();
                im.install_time = time::now_string();
                self.installed_modules.insert(NEW.to_string(), im);
                self.reindex();
            }
        }
        if let Some(r) = self.sorted_repositories.get_mut("default")
            && r.uri == OLD_DEFAULT_REPO_URI
        {
            r.uri = crate::repo::DEFAULT_REPO_URI.to_string();
        }
        self.renumber_priorities();
        self.registry_version = LATEST_REGISTRY_VERSION;
        self.ascertain_default_repo();
        Ok(())
    }

    /// Sort repositories by (priority, name) and give them priorities 0..n-1.
    fn renumber_priorities(&mut self) {
        // Sort (priority, name, key); the key is needed to update the entry.
        let mut keyed: Vec<(i32, String, String)> =
            self.sorted_repositories.iter().map(|(k, r)| (r.priority, r.name.clone(), k.clone())).collect();
        keyed.sort();
        for (i, (_, _, key)) in keyed.into_iter().enumerate() {
            if let Some(r) = self.sorted_repositories.get_mut(&key) {
                r.priority = i as i32;
            }
        }
    }

    /// Load `<game>/CKAN/registry.json` with CKAN's upgrade fixes. Missing file -> empty registry.
    /// Corrupt JSON -> rename to `registry.json_CORRUPTED_<yyyyMMddHHmmss>` and start empty.
    /// registry_version > 3 -> error (never overwrite a newer registry).
    pub fn load(ckan_dir: &Path) -> Result<Registry> {
        let path = ckan_dir.join(REGISTRY_FILE);
        let game_dir = ckan_dir.parent().unwrap_or(ckan_dir);
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::fresh(game_dir)),
            Err(e) => return Err(Error::Io(format!("{}: {e}", path.display()))),
        };
        // .NET File.ReadAllText(UTF8) replaces bad bytes with U+FFFD; it does not fail.
        let text = String::from_utf8_lossy(&bytes);
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
        if text.trim().is_empty() {
            // Newtonsoft PopulateObject on empty text changes nothing: an empty registry.
            return Ok(Self::fresh(game_dir));
        }
        match parse_registry(text, game_dir)? {
            Parsed::Ok(reg) => Ok(*reg),
            Parsed::Corrupt(_why) => {
                Self::archive_corrupt(&path)?;
                Ok(Self::fresh(game_dir))
            }
        }
    }

    /// New registry for a game dir (CKAN RegistryManager.Create): default repo + DLL scan.
    fn fresh(game_dir: &Path) -> Registry {
        let mut reg = Registry::empty();
        reg.scan_dlls(game_dir);
        reg
    }

    /// Rename a corrupt registry. Never replaces an older archive made in the same second.
    fn archive_corrupt(path: &Path) -> Result<PathBuf> {
        let mut base = path.as_os_str().to_owned();
        base.push(format!("_CORRUPTED_{}", time::local_compact_stamp()));
        let mut dest = PathBuf::from(&base);
        let mut n = 1u32;
        while std::fs::symlink_metadata(&dest).is_ok() {
            if n > 1000 {
                return Err(Error::Io(format!("{}: too many archived copies", path.display())));
            }
            let mut name = base.clone();
            name.push(format!("_{n}"));
            dest = PathBuf::from(name);
            n += 1;
        }
        std::fs::rename(path, &dest).ctx(path.display())?;
        Ok(dest)
    }

    /// registry.json text exactly as CKAN formats it (one token per line, no indent, trailing newline).
    pub fn to_ckan_json(&self) -> Result<Vec<u8>> {
        let mut buf = Vec::with_capacity(64 * 1024);
        let fmt = serde_json::ser::PrettyFormatter::with_indent(b"");
        let mut ser = serde_json::Serializer::with_formatter(&mut buf, fmt);
        self.serialize(&mut ser)?;
        buf.push(b'\n');
        Ok(buf)
    }

    /// Write registry.json (atomic) and, if `index` is given, `installed-<name>.ckan` and a
    /// `history/installed-<name>-<stamp>.ckan` snapshot (modpack of user-installed mods with versions).
    pub fn save(&self, inst: &Instance, index: Option<&RepoIndex>) -> Result<()> {
        let ckan = inst.ckan_dir();
        std::fs::create_dir_all(&ckan).ctx(ckan.display())?;
        crate::util::write_atomic(&ckan.join(REGISTRY_FILE), &self.to_ckan_json()?)?;
        if let Some(index) = index {
            let hist = ckan.join("history");
            std::fs::create_dir_all(&hist).ctx(hist.display())?;
            let opts = crate::modlist::ModpackOptions {
                with_versions: true,
                ..crate::modlist::ModpackOptions::defaults_for(inst)
            };
            let text = crate::modlist::generate_modpack(inst, self, index, &opts).to_json_pretty();
            if text.is_empty() {
                return Err(Error::Json("could not write the installed-mods modpack".into()));
            }
            let name = inst.sanitized_name();
            crate::util::write_atomic(&ckan.join(format!("installed-{name}.ckan")), text.as_bytes())?;
            let stamp = time::local_file_stamp();
            crate::util::write_atomic(&hist.join(format!("installed-{name}-{stamp}.ckan")), text.as_bytes())?;
        }
        Ok(())
    }

    /// Repositories sorted by (priority, name).
    pub fn repositories(&self) -> Vec<Repository> {
        let mut v: Vec<Repository> = self.sorted_repositories.values().cloned().collect();
        v.sort_by(|a, b| a.priority.cmp(&b.priority).then_with(|| a.name.cmp(&b.name)));
        v
    }

    pub fn installed(&self, id: &str) -> Option<&InstalledModule> {
        self.installed_modules.get(id)
    }

    /// Installed version of `id`: managed module, DLC, or unmanaged DLL. With `with_provides`,
    /// also an installed module that provides `id`.
    pub fn installed_version(&self, id: &str, with_provides: bool) -> Option<InstalledVersion> {
        if let Some(im) = self.installed_modules.get(id) {
            let m = &im.source_module;
            if m.is_dlc() {
                let v = m.version.as_str();
                let known = !v.is_empty() && v != UNKNOWN_DLC_VERSION;
                return Some(InstalledVersion::Dlc(known.then(|| m.version.clone())));
            }
            return Some(InstalledVersion::Module(m.version.clone()));
        }
        if self.installed_dlls.contains_key(id) {
            return Some(InstalledVersion::Dll);
        }
        if with_provides {
            return self
                .installed_modules
                .values()
                .find(|im| im.source_module.provides.iter().any(|p| p == id))
                .map(|im| InstalledVersion::Module(im.source_module.version.clone()));
        }
        None
    }

    /// Managed, non-DLC installed modules.
    pub fn installed_mods(&self) -> impl Iterator<Item = &InstalledModule> {
        self.installed_modules.values().filter(|m| !m.source_module.is_dlc())
    }

    /// Owner of a relative path, if any.
    pub fn owner_of(&self, rel: &str) -> Option<&str> {
        self.installed_files.get(rel).map(String::as_str)
    }

    /// True if `p` has a child path in `set` or in the owned-file index (so it is a directory).
    fn has_child(p: &str, set: &BTreeSet<String>, owned: &BTreeMap<String, String>) -> bool {
        let prefix = format!("{p}/");
        let lo = (Bound::Included(prefix.as_str()), Bound::Unbounded);
        set.range::<str, _>(lo).next().is_some_and(|f| f.starts_with(&prefix))
            || owned.range::<str, _>(lo).next().is_some_and(|(f, _)| f.starts_with(&prefix))
    }

    /// Record a newly installed module. `files` are relative paths. Error if a file (not a dir)
    /// is already owned. Removes matching installed_dlls entries.
    ///
    /// There is no game dir here, so a path counts as a directory when another listed or owned
    /// path is inside it (installers list each directory together with its files).
    pub fn register(&mut self, m: Module, files: Vec<String>, auto_installed: bool) -> Result<()> {
        let id = m.identifier.clone();
        if self.installed_modules.contains_key(&id) {
            return Err(Error::Conflict(format!("{id} is already registered as installed")));
        }
        let mut rel: BTreeSet<String> = BTreeSet::new();
        for f in files {
            let n = crate::util::normalize_path(&f);
            if n.is_empty() {
                continue;
            }
            if n.starts_with('/') || n.contains('\0') || n.split('/').any(|c| c == "..") {
                return Err(Error::Conflict(format!("{id}: installed file path must be relative and inside the game folder: {n}")));
            }
            rel.insert(n);
        }
        let conflicts: Vec<String> = rel
            .iter()
            .filter_map(|f| {
                let owner = self.installed_files.get(f)?;
                if Self::has_child(f, &rel, &self.installed_files) {
                    return None; // directories may be shared
                }
                Some(format!("{id} wants to install {f}, which is owned by {owner}"))
            })
            .collect();
        if !conflicts.is_empty() {
            return Err(Error::Conflict(conflicts.join("\n")));
        }
        for f in &rel {
            self.installed_files.insert(f.clone(), id.clone());
        }
        self.installed_dlls.retain(|k, v| *k != id && !rel.contains(v.as_str()));
        let installed_files = rel.into_iter().map(|f| (f, FileEntry {})).collect();
        let auto_installed = auto_installed && !m.is_dlc();
        self.installed_modules.insert(
            id,
            InstalledModule { install_time: time::now_string(), source_module: m, auto_installed, installed_files },
        );
        Ok(())
    }

    /// Forget a module. Error if any of its listed files still exists as a file under `game_dir`.
    pub fn deregister(&mut self, id: &str, game_dir: &Path) -> Result<()> {
        let Some(im) = self.installed_modules.get(id) else {
            return Err(Error::NotFound(format!("{id} is not installed")));
        };
        let left: Vec<String> = im
            .installed_files
            .keys()
            .filter(|f| dotnet_file_exists(&game_dir.join(f)))
            .map(|f| format!("{f} (from {id}) was not removed"))
            .collect();
        if !left.is_empty() {
            return Err(Error::Conflict(left.join("\n")));
        }
        let Some(im) = self.installed_modules.remove(id) else {
            return Ok(());
        };
        // Drop our entries. A shared directory goes back to another module that lists it.
        let mut orphaned = Vec::new();
        for f in im.installed_files.keys() {
            if self.installed_files.get(f).is_some_and(|o| o == id) {
                self.installed_files.remove(f);
                orphaned.push(f);
            }
        }
        if !orphaned.is_empty() {
            for (other, om) in &self.installed_modules {
                for f in &orphaned {
                    if om.installed_files.contains_key(*f) {
                        self.installed_files.entry((*f).clone()).or_insert_with(|| other.clone());
                    }
                }
            }
        }
        Ok(())
    }

    pub fn set_auto_installed(&mut self, id: &str, auto: bool) {
        if let Some(m) = self.installed_modules.get_mut(id)
            && !m.source_module.is_dlc()
        {
            m.auto_installed = auto;
        }
    }

    /// Rebuild `installed_files` from the modules' file lists.
    pub fn reindex(&mut self) {
        let mut files = BTreeMap::new();
        for (id, im) in &self.installed_modules {
            for f in im.installed_files.keys() {
                files.insert(f.clone(), id.clone());
            }
        }
        self.installed_files = files;
    }

    /// CKAN SetDlls: keep entries whose identifier is not an installed module and whose path is
    /// not owned. Returns true if `installed_dlls` changed.
    pub fn set_dlls(&mut self, dlls: BTreeMap<String, String>) -> bool {
        let keep: BTreeMap<String, String> = dlls
            .into_iter()
            .filter(|(id, p)| !self.installed_modules.contains_key(id) && !self.installed_files.contains_key(p))
            .collect();
        if keep != self.installed_dlls {
            self.installed_dlls = keep;
            true
        } else {
            false
        }
    }

    /// Scan GameData for unmanaged *.dll files (skip stock folders) and update installed_dlls.
    /// Returns true if anything changed.
    pub fn scan_dlls(&mut self, game_dir: &Path) -> bool {
        let mut found: BTreeMap<String, String> = BTreeMap::new();
        for rel in find_dlls(game_dir) {
            if let Some(id) = Self::dll_identifier(&rel) {
                found.entry(id).or_insert(rel); // first path wins
            }
        }
        self.set_dlls(found)
    }

    /// Update DLC entries from `detected` (identifier, version); use repo metadata when the
    /// (id, version) is in `index`, else a synthetic `kind: dlc` module. Returns true if changed.
    pub fn set_dlc(&mut self, detected: &[(String, Option<String>)], index: Option<&RepoIndex>) -> bool {
        let unknown = |v: &str| v.is_empty() || v == UNKNOWN_DLC_VERSION;
        let want: BTreeMap<&str, Option<&str>> = detected
            .iter()
            .map(|(id, v)| (id.as_str(), v.as_deref().map(str::trim).filter(|v| !unknown(v))))
            .collect();
        let have: BTreeMap<&str, Option<&str>> = self
            .installed_modules
            .iter()
            .filter(|(_, im)| im.source_module.is_dlc())
            .map(|(id, im)| {
                let v = im.source_module.version.as_str();
                (id.as_str(), (!unknown(v)).then_some(v))
            })
            .collect();
        if want == have {
            return false;
        }
        let gone: Vec<String> = have.keys().filter(|id| !want.contains_key(*id)).map(|s| s.to_string()).collect();
        let want: Vec<(String, Option<String>)> =
            want.into_iter().map(|(id, v)| (id.to_string(), v.map(str::to_string))).collect();
        for id in gone {
            self.installed_modules.remove(&id);
        }
        for (id, ver) in want {
            let from_repo = match (&ver, index) {
                (Some(v), Some(ix)) => ix.get(&id, &ModuleVersion::new(v)).map(|m| Module::clone(m)),
                _ => None,
            };
            let Some(module) = from_repo.or_else(|| synthetic_dlc(&id, ver.as_deref())) else {
                continue;
            };
            self.installed_modules.insert(
                id,
                InstalledModule {
                    install_time: time::now_string(),
                    source_module: module,
                    auto_installed: false,
                    installed_files: BTreeMap::new(),
                },
            );
        }
        true
    }

    /// Map a GameData-relative DLL path to an identifier (`GameData/kOS/Plugins/kOS.Safe.dll` -> `kOS`).
    pub fn dll_identifier(rel: &str) -> Option<String> {
        if !rel.starts_with("GameData/") {
            return None;
        }
        static RE: OnceLock<Option<regex::Regex>> = OnceLock::new();
        let re = RE.get_or_init(|| regex::Regex::new(r"(?i)^(?:.*/)?(?P<identifier>[^.]+).*\.dll$").ok()).as_ref()?;
        let caps = re.captures(rel)?;
        let id = crate::util::sanitize_identifier(caps.name("identifier")?.as_str());
        (!id.is_empty()).then_some(id)
    }

    /// Files listed for `id` that are missing on disk.
    #[cfg(test)]
    pub fn missing_files(&self, id: &str, game_dir: &Path) -> Vec<String> {
        let Some(im) = self.installed_modules.get(id) else {
            return Vec::new();
        };
        im.installed_files
            .keys()
            .filter(|f| std::fs::symlink_metadata(game_dir.join(f)).is_err())
            .cloned()
            .collect()
    }
}

/// .NET File.Exists on Unix: stat (follows links), else lstat (a broken link counts), and the
/// entry is not a directory.
fn dotnet_file_exists(p: &Path) -> bool {
    match std::fs::metadata(p) {
        Ok(md) => !md.is_dir(),
        Err(_) => std::fs::symlink_metadata(p).is_ok_and(|md| !md.is_dir()),
    }
}

/// CKAN's stand-in module for a DLC that the repositories do not know (Registry.SetDlcs).
fn synthetic_dlc(id: &str, version: Option<&str>) -> Option<Module> {
    let v = serde_json::json!({
        "spec_version": "v1.28",
        "identifier": id,
        "name": id,
        "abstract": "An official expansion pack",
        "author": ["SQUAD"],
        "version": version.unwrap_or(UNKNOWN_DLC_VERSION),
        "license": ["restricted"],
        "kind": "dlc",
    });
    serde_json::from_value(v).ok()
}

/// Relative paths of `*.dll` files under `<game>/GameData`, stock folders skipped. Order: per
/// folder, files by name, then sub-folders breadth first (close to .NET EnumerateFiles).
/// Symlinked folders are followed, but each real folder is read only once (no link loops).
fn find_dlls(game_dir: &Path) -> Vec<String> {
    use std::os::unix::fs::MetadataExt as _;
    // "GameData/Squad" -> ("GameData/Squad", "GameData/Squad/"), computed once.
    let stock: Vec<(&str, String)> = crate::instance::STOCK_FOLDERS
        .iter()
        .copied()
        .filter(|f| f.starts_with("GameData/"))
        .map(|f| (f, format!("{f}/")))
        .collect();
    let in_stock = |rel: &str| stock.iter().any(|(s, p)| rel == *s || rel.starts_with(p.as_str()));
    let mut out = Vec::new();
    let mut seen: std::collections::HashSet<(u64, u64)> = std::collections::HashSet::new();
    let mut queue: VecDeque<(PathBuf, usize)> = VecDeque::new();
    queue.push_back((game_dir.join("GameData"), 0));
    while let Some((dir, depth)) = queue.pop_front() {
        let Ok(md) = std::fs::metadata(&dir) else { continue };
        if !seen.insert((md.dev(), md.ino())) {
            continue;
        }
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        let mut entries: Vec<std::fs::DirEntry> = rd.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let path = e.path();
            let Ok(ft) = e.file_type() else { continue };
            let is_dir = if ft.is_symlink() { path.is_dir() } else { ft.is_dir() };
            let Some(rel) = crate::util::rel_path(game_dir, &path) else { continue };
            if is_dir {
                if !in_stock(&rel) && depth < MAX_SCAN_DEPTH {
                    queue.push_back((path, depth + 1));
                }
                continue;
            }
            let is_dll = rel.len() >= 4 && rel.get(rel.len() - 4..).is_some_and(|ext| ext.eq_ignore_ascii_case(".dll"));
            if is_dll && !in_stock(&rel) {
                out.push(rel);
            }
        }
    }
    out
}
