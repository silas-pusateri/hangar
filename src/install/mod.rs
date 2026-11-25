//! Download cache, install-stanza matching, extracting zip files into the game dir, and removal.
//! Never overwrite an existing file. All file changes go through `FileTx` so a failed change set
//! can be rolled back.

pub mod cache;
mod matching;
mod metaregex;
mod tx;

use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::io::{BufReader, BufWriter, ErrorKind, Write};
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::instance::{Instance, STOCK_FOLDERS};
use crate::meta::Module;
use crate::registry::Registry;

pub use cache::DownloadCache;
pub use matching::module_files;
pub use metaregex::MetaRegex;
pub use tx::FileTx;

use matching::{check_relative, dirname};

/// One zip entry selected by an install stanza.
#[derive(Debug, Clone, PartialEq)]
pub struct InstallableFile {
    /// Index in the zip archive.
    pub zip_index: usize,
    /// Entry name in the zip (with `/`).
    pub source: String,
    /// Destination relative to the game dir (with `/`).
    pub destination: String,
    /// The entry is a directory.
    pub is_dir: bool,
    /// Parent dirs may be created.
    pub makedir: bool,
}

/// Directories under these names (after the dir being cleaned) are removed with the mod.
pub const AUTO_REMOVABLE_DIRS: [&str; 1] = ["@thumbs"];

/// Reserved dirs (relative to the game dir) that are never removed.
pub const RESERVED_DIRS: [&str; 13] = [
    "",
    "CKAN",
    "GameData",
    "Missions",
    "saves/scenarios",
    "saves/training",
    "Ships",
    "Ships/@thumbs",
    "Ships/VAB",
    "Ships/@thumbs/VAB",
    "Ships/SPH",
    "Ships/@thumbs/SPH",
    "Ships/Script",
];

type Zip = zip::ZipArchive<BufReader<fs::File>>;

fn corrupt(zip_path: &Path, e: impl std::fmt::Display) -> Error {
    Error::Io(format!("{}: corrupt zip in cache: {e}", zip_path.display()))
}

fn open_zip(zip_path: &Path) -> Result<Zip> {
    let f = fs::File::open(zip_path).map_err(|e| Error::Io(format!("{}: {e}", zip_path.display())))?;
    zip::ZipArchive::new(BufReader::new(f)).map_err(|e| corrupt(zip_path, e))
}

fn entry_names(za: &Zip, zip_path: &Path) -> Result<Vec<String>> {
    (0..za.len())
        .map(|i| match za.name_for_index(i) {
            Some(Ok(n)) => Ok(n.into_owned()),
            Some(Err(e)) => Err(corrupt(zip_path, e)),
            None => Err(corrupt(zip_path, format!("missing entry {i}"))),
        })
        .collect()
}

/// Zip entry names in archive order (index = position).
pub fn zip_entry_names(zip_path: &Path) -> Result<Vec<String>> {
    entry_names(&open_zip(zip_path)?, zip_path)
}

/// Join a checked relative path to the game dir.
fn game_path(game_dir: &Path, rel: &str) -> Result<PathBuf> {
    check_relative(rel)?;
    Ok(if rel.is_empty() { game_dir.to_path_buf() } else { game_dir.join(rel) })
}

fn exists_no_follow(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok()
}

/// Extract a module's files without registering it. Returns the relative paths written
/// (files, and dir entries when makedir). Used by `install_module`.
pub fn extract_module(
    inst: &Instance,
    reg: &Registry,
    m: &Module,
    zip_path: &Path,
    filters: &[String],
    tx: &mut FileTx,
) -> Result<Vec<String>> {
    let mut za = open_zip(zip_path)?;
    let entries = entry_names(&za, zip_path)?;
    let files = module_files(m, &entries, filters)?;

    // Replace a manually installed DLL of this mod (CKAN DllLocationMismatch rule).
    if let Some(dll) = reg.installed_dlls.get(&m.identifier).filter(|d| !d.is_empty()) {
        let folders: HashSet<&str> = files
            .iter()
            .filter(|f| Registry::dll_identifier(&f.destination).as_deref() == Some(m.identifier.as_str()))
            .map(|f| dirname(&f.destination))
            .collect();
        if !folders.is_empty() && reg.owner_of(dll).is_none() {
            if !folders.contains(dirname(dll)) {
                return Err(Error::Conflict(format!(
                    "{} is installed manually at {dll}, but {m} installs it somewhere else; remove the manual install first",
                    m.identifier
                )));
            }
            tx.remove_file(&game_path(&inst.game_dir, dll)?)?;
        }
    }

    let mut written = Vec::with_capacity(files.len());
    let mut buf = vec![0u8; 1 << 16];
    for f in &files {
        let abs = game_path(&inst.game_dir, &f.destination)?;
        let mut zf = za.by_index(f.zip_index).map_err(|e| corrupt(zip_path, e))?;
        if f.is_dir || zf.is_dir() {
            if !f.makedir || f.destination.is_empty() {
                continue;
            }
            tx.create_dir_all(&abs)?;
            written.push(f.destination.clone());
            continue;
        }
        if f.makedir
            && let Some(parent) = abs.parent()
        {
            tx.create_dir_all(parent)?;
        }
        if exists_no_follow(&abs) {
            let owner = reg.owner_of(&f.destination).map(|o| format!(" (owned by {o})")).unwrap_or_default();
            return Err(Error::Conflict(format!("{} already exists{owner}; {m} cannot install it", f.destination)));
        }
        let mut out = BufWriter::new(tx.create_file(&abs)?);
        loop {
            let n = match std::io::Read::read(&mut zf, &mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => return Err(corrupt(zip_path, format!("{}: {e}", f.source))),
            };
            out.write_all(buf.get(..n).unwrap_or(&[])).map_err(|e| Error::Io(format!("{}: {e}", abs.display())))?;
        }
        out.flush().map_err(|e| Error::Io(format!("{}: {e}", abs.display())))?;
        written.push(f.destination.clone());
    }
    Ok(written)
}

/// Install one module from its cached zip: select files, refuse to overwrite existing files
/// (error names them), extract, register in `reg`. Metapackages register with no files.
/// DLC is an error. Returns the relative paths written.
/// A module that is already installed (managed) is skipped: Ok with no paths.
/// A zip read error gives `Error::Io` with "corrupt zip in cache"; the caller should purge it.
pub fn install_module(
    inst: &Instance,
    reg: &mut Registry,
    m: &Module,
    zip_path: Option<&Path>,
    auto_installed: bool,
    filters: &[String],
    tx: &mut FileTx,
) -> Result<Vec<String>> {
    if m.is_dlc() {
        return Err(Error::Conflict(format!("{m} is a DLC; it cannot be installed or removed here")));
    }
    if reg.installed(&m.identifier).is_some() {
        return Ok(Vec::new());
    }
    if m.is_metapackage() {
        reg.register(m.clone(), Vec::new(), auto_installed)?;
        return Ok(Vec::new());
    }
    let zip_path = zip_path.ok_or_else(|| Error::NotFound(format!("{m} is not in the download cache")))?;
    let written = extract_module(inst, reg, m, zip_path, filters, tx)?;
    reg.register(m.clone(), written.clone(), auto_installed)?;
    Ok(written)
}

/// What step 1 of removal found: the module's file list and the dirs to check later.
#[derive(Debug, Clone, Default)]
pub struct DeletedFiles {
    /// All relative paths listed for the module (files and dirs).
    pub files: Vec<String>,
    /// Listed dirs, and dirs of the files that were deleted.
    pub dirs: Vec<String>,
}

/// Step 1 of removal: delete the module's files (directories are only collected).
/// Errors if some files cannot be deleted. Missing files are ignored.
pub fn delete_module_files(inst: &Instance, reg: &Registry, id: &str, tx: &mut FileTx) -> Result<DeletedFiles> {
    let im = reg.installed(id).ok_or_else(|| Error::NotFound(format!("{id} is not installed")))?;
    if im.source_module.is_dlc() {
        return Err(Error::Conflict(format!("{id} is a DLC; it cannot be installed or removed here")));
    }
    let files: Vec<String> = im.installed_files.keys().cloned().collect();
    let mut dirs = BTreeSet::new();
    let mut undeletable = Vec::new();
    for rel in &files {
        let Ok(abs) = game_path(&inst.game_dir, rel) else { continue };
        match fs::symlink_metadata(&abs) {
            Ok(md) if md.is_dir() => {
                dirs.insert(rel.clone());
            }
            Ok(_) => {
                dirs.insert(dirname(rel).to_string());
                if tx.remove_file(&abs).is_err() {
                    undeletable.push(rel.clone());
                }
            }
            Err(_) => {}
        }
    }
    if !undeletable.is_empty() {
        return Err(Error::Conflict(format!("could not delete files of {id}: {}", undeletable.join(", "))));
    }
    Ok(DeletedFiles { files, dirs: dirs.into_iter().collect() })
}

/// Reserved dir test, ignoring ASCII case: macOS volumes are case-insensitive by default, so
/// `Ships/vab` (from a case-insensitive `find`) names the stock `Ships/VAB`.
fn is_reserved(d: &str) -> bool {
    RESERVED_DIRS.iter().any(|r| r.eq_ignore_ascii_case(d))
}

/// Stock folder test, ignoring ASCII case (see `is_reserved`).
fn is_stock(d: &str) -> bool {
    STOCK_FOLDERS.iter().any(|r| r.eq_ignore_ascii_case(d))
}

/// Add parents up to (not including) the game root, stopping at a stock folder; drop reserved
/// dirs; sort longest first.
pub(crate) fn with_parents(dirs: &[String]) -> Vec<String> {
    let mut all = BTreeSet::new();
    for d in dirs {
        let d = crate::util::normalize_path(d);
        let mut p = d.as_str();
        while !p.is_empty() {
            if is_stock(p) {
                break;
            }
            all.insert(p.to_string());
            p = dirname(p);
        }
    }
    let mut v: Vec<String> = all.into_iter().filter(|d| !is_reserved(d)).collect();
    v.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    v
}

/// All entries below `abs_dir`, relative to the game dir, without following symlinks.
fn walk(game_dir: &Path, abs_dir: &Path, out: &mut Vec<String>) {
    let Ok(rd) = fs::read_dir(abs_dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let Ok(ft) = e.file_type() else { continue };
        if let Some(rel) = crate::util::rel_path(game_dir, &p) {
            out.push(rel);
        }
        if ft.is_dir() {
            walk(game_dir, &p, out);
        }
    }
}

/// CKAN GroupFilesByRemovable: split the contents of `rel_root` into auto-removable paths
/// (no owner, not in `already_removing`, and an `@thumbs` piece below the root) and the rest.
/// Both lists are sorted longest first.
pub fn group_by_removable(
    rel_root: &str,
    reg: &Registry,
    already_removing: &HashSet<&str>,
    rel_paths: &[String],
) -> (Vec<String>, Vec<String>) {
    let mut removable = Vec::new();
    let mut not_removable = Vec::new();
    for rel in rel_paths {
        let below = rel.get(rel_root.len()..).unwrap_or("");
        let auto = reg.owner_of(rel).is_none()
            && !already_removing.contains(rel.as_str())
            && below.split('/').filter(|s| !s.is_empty()).any(|s| AUTO_REMOVABLE_DIRS.contains(&s));
        if auto { removable.push(rel.clone()) } else { not_removable.push(rel.clone()) }
    }
    removable.sort_by_key(|r| std::cmp::Reverse(r.len()));
    not_removable.sort_by_key(|r| std::cmp::Reverse(r.len()));
    (removable, not_removable)
}

/// Step 3 of removal (after `deregister`): remove empty dirs, longest first, keeping reserved
/// and stock dirs and dirs with files of other mods or unmanaged files. Files under `@thumbs`
/// that no mod owns are removed too. Returns dirs that hold only unmanaged files (the UI may
/// offer to delete them).
pub fn remove_empty_dirs(inst: &Instance, reg: &Registry, deleted: &DeletedFiles, tx: &mut FileTx) -> Vec<String> {
    let game = &inst.game_dir;
    let own: HashSet<&str> = deleted.files.iter().map(String::as_str).collect();
    let mut config_only: Vec<String> = Vec::new();
    for d in with_parents(&deleted.dirs) {
        if is_reserved(&d) || is_stock(&d) {
            continue;
        }
        let Ok(abs) = game_path(game, &d) else { continue };
        let mut entries = Vec::new();
        if abs.is_dir() {
            walk(game, &abs, &mut entries);
        }
        let (removable, not_removable) = group_by_removable(&d, reg, &own, &entries);
        for rel in &removable {
            let Ok(p) = game_path(game, rel) else { continue };
            match fs::symlink_metadata(&p) {
                Ok(md) if md.is_dir() => {
                    let _ = fs::remove_dir(&p);
                }
                Ok(_) => {
                    let _ = tx.remove_file(&p);
                }
                Err(_) => {}
            }
        }
        if not_removable.is_empty() {
            // Not journaled (as in CKAN); a rollback recreates parents of restored files.
            let _ = fs::remove_dir(&abs);
        } else if not_removable
            .iter()
            .filter(|r| !config_only.iter().any(|c| c == *r))
            .any(|r| reg.owner_of(r).is_some() || own.contains(r.as_str()))
        {
            // Holds files of other mods: keep.
        } else {
            config_only.push(d);
        }
    }
    config_only
}

/// Remove one installed module and return dirs that now hold only unmanaged files.
pub fn uninstall_module_ex(inst: &Instance, reg: &mut Registry, id: &str, tx: &mut FileTx) -> Result<Vec<String>> {
    let deleted = delete_module_files(inst, reg, id, tx)?;
    reg.deregister(id, &inst.game_dir)?;
    Ok(remove_empty_dirs(inst, reg, &deleted, tx))
}


/// Unowned files that already exist at the module's destinations (to ask the user before
/// install). Returns relative paths.
pub fn existing_conflicts(inst: &Instance, reg: &Registry, files: &[InstallableFile]) -> Vec<String> {
    files
        .iter()
        .filter(|f| !f.is_dir && reg.owner_of(&f.destination).is_none())
        .filter(|f| game_path(&inst.game_dir, &f.destination).is_ok_and(|p| p.is_file()))
        .map(|f| f.destination.clone())
        .collect()
}

#[cfg(test)]
mod tests;
