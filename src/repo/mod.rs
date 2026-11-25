//! Metadata repositories: download CKAN-meta style tar.gz archives (with ETag checks),
//! keep the raw archive as the local cache, and parse it into a `RepoIndex`.
//!
//! Cache layout (Hangar's own dir, not CKAN's): `<hangar_dir>/repos/<HASH8(uri)>-<name>.tar.gz`
//! plus `<hangar_dir>/repos/etags.json` (`{uri: etag}` with quotes removed).

pub mod archive;
pub mod net;

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::meta::{GameVersion, Module, ModuleVersion, ReleaseStatus, VersionCriteria};

pub const DEFAULT_REPO_NAME: &str = "KSP-default";
pub const DEFAULT_REPO_URI: &str = "https://github.com/KSP-CKAN/CKAN-meta/archive/master.tar.gz";
/// List of well-known repositories (CKAN "repositories.json").
pub const REPO_LIST_URI: &str = "https://raw.githubusercontent.com/KSP-CKAN/CKAN-meta/master/repositories.json";

/// One metadata repository. Same JSON shape as CKAN's registry.json `sorted_repositories` values.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Repository {
    pub name: String,
    pub uri: String,
    #[serde(default)]
    pub priority: i32,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub x_mirror: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x_comment: Option<String>,
}

impl Repository {
    pub fn default_repo() -> Self {
        Repository {
            name: DEFAULT_REPO_NAME.into(),
            uri: DEFAULT_REPO_URI.into(),
            priority: 0,
            x_mirror: false,
            x_comment: None,
        }
    }
}

/// Parsed contents of one archive.
#[derive(Debug, Default)]
pub struct RepoData {
    /// In archive order.
    pub modules: Vec<Arc<Module>>,
    pub download_counts: HashMap<String, u64>,
    /// From `builds.json`: build id -> full version (`"3190" -> 1.12.5.3190`).
    pub builds: HashMap<String, GameVersion>,
    /// From `repositories.json` (only in the default repo).
    pub repositories: Vec<Repository>,
    /// Number of .ckan files that failed to parse or validate (skipped).
    pub skipped: usize,
}

/// All available modules from all repositories, merged by priority.
#[derive(Debug, Default)]
pub struct RepoIndex {
    /// identifier -> all versions, sorted DESCENDING by ModuleVersion.
    pub modules: HashMap<String, Vec<Arc<Module>>>,
    /// provided name -> identifiers that provide it in any version (includes the identifier itself).
    pub providers: HashMap<String, Vec<String>>,
    pub download_counts: HashMap<String, u64>,
    pub builds: HashMap<String, GameVersion>,
    /// Real released game versions, ascending (from builds).
    pub known_game_versions: Vec<GameVersion>,
    pub repositories: Vec<Repository>,
}

impl RepoIndex {
    /// Merge repos in priority order (lower `priority` first). The first repo that has an
    /// (identifier, version) wins; download counts come from the first repo that has one.
    pub fn build(mut repos: Vec<(Repository, RepoData)>) -> RepoIndex {
        repos.sort_by(|a, b| a.0.priority.cmp(&b.0.priority).then_with(|| a.0.name.cmp(&b.0.name)));
        let mut idx = RepoIndex::default();
        let mut seen_repo_uris: HashSet<String> = HashSet::new();
        for (_repo, data) in repos {
            for m in data.modules {
                idx.modules.entry(m.identifier.clone()).or_default().push(m);
            }
            for (k, v) in data.download_counts {
                idx.download_counts.entry(k).or_insert(v);
            }
            for (k, v) in data.builds {
                idx.builds.entry(k).or_insert(v);
            }
            for r in data.repositories {
                if seen_repo_uris.insert(r.uri.clone()) {
                    idx.repositories.push(r);
                }
            }
        }
        // Builds from the repos win; the embedded map fills gaps (and works before any update).
        for (k, v) in crate::instance::embedded_builds() {
            idx.builds.entry(k.clone()).or_insert(*v);
        }
        for list in idx.modules.values_mut() {
            // Stable sort keeps priority order among equal versions; dedup keeps the first.
            list.sort_by(|a, b| b.version.cmp(&a.version));
            list.dedup_by(|later, earlier| later.version == earlier.version);
            list.shrink_to_fit();
        }
        for (id, list) in &idx.modules {
            for m in list {
                for name in m.provides_list() {
                    let v = idx.providers.entry(name.to_string()).or_default();
                    if v.last().is_none_or(|last| last != id) {
                        v.push(id.clone());
                    }
                }
            }
        }
        for v in idx.providers.values_mut() {
            v.sort();
            v.dedup();
        }
        let mut known: Vec<GameVersion> = idx.builds.values().copied().collect();
        known.sort();
        known.dedup();
        idx.known_game_versions = known;
        idx
    }

    /// All versions of `id`, newest first. Empty if unknown.
    pub fn versions(&self, id: &str) -> &[Arc<Module>] {
        self.modules.get(id).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn get(&self, id: &str, v: &ModuleVersion) -> Option<&Arc<Module>> {
        self.versions(id).iter().find(|m| &m.version == v)
    }

    /// Newest version of `id` with release_status <= `stability` and, if `crit` is given,
    /// compatible with it.
    pub fn latest(&self, id: &str, crit: Option<&VersionCriteria>, stability: ReleaseStatus) -> Option<&Arc<Module>> {
        self.versions(id)
            .iter()
            .find(|m| m.release_status <= stability && crit.is_none_or(|c| m.is_compatible(c)))
    }

    pub fn download_count(&self, id: &str) -> u64 {
        self.download_counts.get(id).copied().unwrap_or(0)
    }

    /// Identifiers that provide `name` (in any version), including `name` itself if it is a module.
    pub fn providers_of(&self, name: &str) -> &[String] {
        self.providers.get(name).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Look up a module version by download hash (uppercase hex sha1 or sha256). For zip import.
    /// A full scan (about 31k modules); it is called rarely, so no index is kept.
    pub fn find_by_hash(&self, sha1: &str, sha256: &str) -> Vec<Arc<Module>> {
        let hit = |want: &str, have: Option<&str>| {
            !want.is_empty() && have.is_some_and(|h| h.eq_ignore_ascii_case(want))
        };
        let mut by256 = Vec::new();
        let mut by1 = Vec::new();
        for list in self.modules.values() {
            for m in list {
                let Some(h) = &m.download_hash else { continue };
                if hit(sha256, h.sha256.as_deref()) {
                    by256.push(Arc::clone(m));
                } else if hit(sha1, h.sha1.as_deref()) {
                    by1.push(Arc::clone(m));
                }
            }
        }
        let mut out = if by256.is_empty() { by1 } else { by256 };
        out.sort_by(|a, b| a.identifier.cmp(&b.identifier).then_with(|| b.version.cmp(&a.version)));
        out
    }

    /// Total number of module versions.
    pub fn module_count(&self) -> usize {
        self.modules.values().map(Vec::len).sum()
    }

    /// Map a buildID to a game version.
    #[cfg(test)]
    pub fn build_version(&self, build_id: &str) -> Option<GameVersion> {
        self.builds.get(build_id).copied()
    }
}

/// Progress callback for long jobs: (message, fraction 0..1 or None).
pub type Progress<'a> = &'a (dyn Fn(&str, Option<f32>) + Sync);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateResult {
    Updated,
    NoChanges,
}

/// Download each repo whose ETag changed (or all if `force`) into the cache dir.
/// `file://` URIs and local paths are read directly. Errors leave old caches in place.
///
/// Each repo is written on its own (temp file + rename); etags.json keeps the ETags of the repos
/// that succeeded. The first error is returned after all repos were tried.
pub fn update(repos: &[Repository], cache_dir: &Path, force: bool, progress: Progress) -> Result<UpdateResult> {
    net::update(repos, cache_dir, force, progress)
}

/// Parse all cached archives for these repos (missing caches are skipped) and build the index.
/// Must be fast: stream the tar.gz once, parse .ckan blobs on up to 8 std threads.
/// A local repo with no cache yet is read from its own path. A cache that fails to parse is an error.
#[cfg(test)]
pub fn load_index(repos: &[Repository], cache_dir: &Path) -> Result<RepoIndex> {
    let mut seen = HashSet::new();
    let mut datas = Vec::new();
    for r in repos.iter().filter(|r| seen.insert(r.uri.as_str())) {
        let path = net::find_cache_file(r, cache_dir).or_else(|| net::local_path(&r.uri).filter(|p| p.is_file()));
        let Some(path) = path else { continue };
        let data = parse_archive(&path)?;
        datas.push((r.clone(), data));
    }
    Ok(RepoIndex::build(datas))
}

/// Parse one archive (tar.gz or zip, detected by magic bytes) from memory or a file.
pub fn parse_archive(path: &Path) -> Result<RepoData> {
    archive::parse(path)
}

/// Fetch CKAN's list of known repositories (`repositories.json`).
pub fn fetch_repository_list() -> Result<Vec<Repository>> {
    net::fetch_repository_list()
}

/// Shared HTTP agent: rustls, 30 s connect, 60 s with no data on a read (a stalled server
/// must not hang a worker forever; big downloads are fine), User-Agent "Hangar/<version>".
/// Auth tokens are not added here (this signature has no config).
pub fn http_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(std::time::Duration::from_secs(30)))
        .timeout_per_read(Some(std::time::Duration::from_secs(60)))
        .user_agent(concat!("Hangar/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

#[cfg(test)]
mod tests;
