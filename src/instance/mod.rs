//! KSP1 game instances: validation, version detection, per-instance CKAN files
//! (`<game>/CKAN/compatible_ksp_versions.json`, `install_filters.json`, `playtime.json`,
//! `history/`), Steam discovery, launching, and fake instances for tests.

mod steam;
pub mod timespan;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use regex::Regex;

use crate::error::{Ctx, Error, Result};
use crate::meta::{GameVersion, VersionCriteria};

pub const COMPAT_FILE: &str = "compatible_ksp_versions.json";
pub const FILTERS_FILE: &str = "install_filters.json";
pub const PLAYTIME_FILE: &str = "playtime.json";
/// Game binary inside the game dir, started with cwd = game dir.
pub const KSP_BINARY: &str = "KSP.app/Contents/MacOS/KSP";
/// Default non-Steam install dir on macOS.
pub const MAC_APP_DIR: &str = "/Applications/Kerbal Space Program";

/// Well-known DLC: (identifier base, dir under GameData/SquadExpansion).
pub const DLC_DIRS: [(&str, &str); 2] = [("MakingHistory", "MakingHistory"), ("BreakingGround", "Serenity")];

/// EMBEDDED_BUILDS parsed once.
pub fn embedded_builds() -> &'static HashMap<String, GameVersion> {
    static B: OnceLock<HashMap<String, GameVersion>> = OnceLock::new();
    B.get_or_init(|| parse_builds(EMBEDDED_BUILDS))
}

fn build_id_re() -> Option<&'static Regex> {
    static R: OnceLock<Option<Regex>> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)^build id\s+=\s+0*(\d+)").ok()).as_ref()
}

fn readme_version_re() -> Option<&'static Regex> {
    static R: OnceLock<Option<Regex>> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)^Version\s+(\d+\.\d+\.\d+)").ok()).as_ref()
}

fn dlc_version_re() -> Option<&'static Regex> {
    static R: OnceLock<Option<Regex>> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)^Version\s+(\S+)").ok()).as_ref()
}

/// Text lines of a file (lossy UTF-8, BOM and CR removed). Empty if unreadable.
fn read_lines(path: &Path) -> Vec<String> {
    let Ok(bytes) = std::fs::read(path) else { return Vec::new() };
    let text = String::from_utf8_lossy(&bytes);
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    text.lines().map(|l| l.trim_end_matches('\r').to_string()).collect()
}

/// Parse a GameVersion from JSON text (strip one trailing `.`).
fn json_game_version(v: &serde_json::Value) -> Option<GameVersion> {
    let s = v.as_str()?;
    GameVersion::parse(s.strip_suffix('.').unwrap_or(s))
}

/// Embedded copy of CKAN's builds-ksp.json (`{"builds": {"3190": "1.12.5.3190"}}`).
pub const EMBEDDED_BUILDS: &str = include_str!("builds-ksp.json");

/// Stock folders: never removed, skipped by DLL scan and unmanaged-file listing.
pub const STOCK_FOLDERS: [&str; 8] = [
    "GameData/Squad",
    "GameData/SquadExpansion",
    "KSP_Data",
    "KSP_x64_Data",
    "KSPLauncher_Data",
    "Launcher_Data",
    "MonoBleedingEdge",
    "PDLauncher",
];

#[derive(Debug, Clone)]
pub struct Instance {
    pub name: String,
    pub game_dir: PathBuf,
    /// Detected game version (None if detection failed).
    pub version: Option<GameVersion>,
}

impl Instance {
    /// Open a game dir: check it is valid and detect the version with `builds`
    /// (falls back to EMBEDDED_BUILDS and readme.txt).
    pub fn open(name: &str, game_dir: &Path, builds: &HashMap<String, GameVersion>) -> Result<Instance> {
        if !is_valid_game_dir(game_dir) {
            return Err(Error::NotFound(format!(
                "{} is not a KSP game dir (needs buildID64.txt or buildID.txt and GameData)",
                game_dir.display()
            )));
        }
        let game_dir = std::path::absolute(game_dir).unwrap_or_else(|_| game_dir.to_path_buf());
        let version = detect_version(&game_dir, builds);
        Ok(Instance { name: name.to_string(), game_dir, version })
    }

    /// `<game>/CKAN`
    pub fn ckan_dir(&self) -> PathBuf {
        self.game_dir.join("CKAN")
    }

    /// Instance name with `/` and NUL removed (for installed-<name>.ckan file names).
    pub fn sanitized_name(&self) -> String {
        self.name.chars().filter(|c| *c != '/' && *c != '\0').collect()
    }

    /// Extra compatible versions from compatible_ksp_versions.json, or CKAN defaults if missing.
    pub fn compatible_versions(&self) -> Vec<GameVersion> {
        match self.read_compat_file() {
            Some((_, v)) => v,
            None => self.version.map(|g| crate::meta::game_version::default_compatible_versions(&g)).unwrap_or_default(),
        }
    }

    /// `(GameVersionWhenWritten, Versions)` from compatible_ksp_versions.json (legacy keys too).
    /// None if the file is missing or not valid JSON.
    pub fn read_compat_file(&self) -> Option<(Option<GameVersion>, Vec<GameVersion>)> {
        let bytes = std::fs::read(self.ckan_dir().join(COMPAT_FILE)).ok()?;
        let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
        let when = v.get("GameVersionWhenWritten").or_else(|| v.get("VersionOfKspWhenWritten")).and_then(json_game_version);
        let list = v
            .get("Versions")
            .or_else(|| v.get("CompatibleKspVersions"))
            .and_then(|l| l.as_array())
            .map(|a| a.iter().filter_map(json_game_version).collect())
            .unwrap_or_default();
        Some((when, list))
    }

    /// True when the stored game version of the compatible versions file differs from the
    /// detected version, so the UI should ask again (CKAN CompatibleVersionsAreFromDifferentGameVersion).
    /// A missing file counts as "stored version unknown": true when a version is detected,
    /// as in CKAN, which then shows the dialog with the defaulted versions.
    pub fn compat_from_other_game_version(&self) -> bool {
        let when = self.read_compat_file().and_then(|(when, _)| when);
        when != self.version
    }

    /// Write compatible_ksp_versions.json (distinct, sorted descending).
    pub fn set_compatible_versions(&self, versions: &[GameVersion]) -> Result<()> {
        let mut v = versions.to_vec();
        v.sort_by(|a, b| b.cmp(a));
        v.dedup();
        let doc = serde_json::json!({
            "GameVersionWhenWritten": self.version.map(|g| g.to_string()),
            "Versions": v.iter().map(|g| g.to_string()).collect::<Vec<_>>(),
        });
        let text = serde_json::to_string(&doc)?;
        crate::util::write_atomic(&self.ckan_dir().join(COMPAT_FILE), text.as_bytes())
    }

    /// Game version + compatible versions.
    pub fn criteria(&self) -> VersionCriteria {
        VersionCriteria::new(self.version, &self.compatible_versions())
    }

    /// `install_filters.json` (JSON array of substrings; matching files are not installed).
    pub fn install_filters(&self) -> Vec<String> {
        std::fs::read(self.ckan_dir().join(FILTERS_FILE))
            .ok()
            .and_then(|b| serde_json::from_slice::<Vec<Option<String>>>(&b).ok())
            .map(|v| v.into_iter().flatten().collect())
            .unwrap_or_default()
    }

    pub fn set_install_filters(&self, filters: &[String]) -> Result<()> {
        crate::util::write_json(&self.ckan_dir().join(FILTERS_FILE), &filters)
    }

    /// Total play time from playtime.json (`{"time": "d.hh:mm:ss.fffffff"}`).
    pub fn playtime(&self) -> Duration {
        std::fs::read(self.ckan_dir().join(PLAYTIME_FILE))
            .ok()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .and_then(|v| v.get("time").and_then(|t| t.as_str()).and_then(timespan::parse))
            .unwrap_or_default()
    }

    pub fn add_playtime(&self, d: Duration) -> Result<()> {
        self.set_playtime(self.playtime().saturating_add(d))
    }

    /// Overwrite the total play time (the GUI lets the user edit it).
    pub fn set_playtime(&self, d: Duration) -> Result<()> {
        let doc = serde_json::json!({ "time": timespan::format(d) });
        crate::util::write_json(&self.ckan_dir().join(PLAYTIME_FILE), &doc)
    }

    /// Start KSP (`KSP.app/Contents/MacOS/KSP`, cwd = game dir). Caller waits on the child to
    /// count play time.
    pub fn launch(&self) -> Result<std::process::Child> {
        let bin = self.launch_binary();
        if !bin.is_file() {
            return Err(Error::NotFound(format!("game binary {}", bin.display())));
        }
        std::process::Command::new(&bin).current_dir(&self.game_dir).spawn().ctx(bin.display())
    }

    /// `<game>/KSP.app/Contents/MacOS/KSP`.
    pub fn launch_binary(&self) -> PathBuf {
        self.game_dir.join(KSP_BINARY)
    }

    /// `<game>/CKAN/history/*.ckan`, newest first.
    pub fn history(&self) -> Vec<PathBuf> {
        let dir = self.ckan_dir().join("history");
        let Ok(rd) = std::fs::read_dir(&dir) else { return Vec::new() };
        let mut v: Vec<(std::time::SystemTime, PathBuf)> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_file() && p.extension().and_then(|x| x.to_str()) == Some("ckan"))
            .map(|p| {
                let t = std::fs::metadata(&p)
                    .and_then(|m| m.created().or_else(|_| m.modified()))
                    .unwrap_or(std::time::UNIX_EPOCH);
                (t, p)
            })
            .collect();
        // Newest first; names hold a time stamp, so they break ties.
        v.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
        v.into_iter().map(|(_, p)| p).collect()
    }

    /// Detected DLC: `(identifier, version)`, e.g. `("MakingHistory-DLC", Some("1.9.1"))`.
    pub fn detect_dlc(&self) -> Vec<(String, Option<String>)> {
        let mut out: Vec<(String, Option<String>)> = Vec::new();
        // Test stubs: <game>/CKAN/dlc/<Name>.dlc holds the version text.
        if let Ok(rd) = std::fs::read_dir(self.ckan_dir().join("dlc")) {
            let mut stubs: Vec<PathBuf> = rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_file() && p.extension().and_then(|x| x.to_str()) == Some("dlc"))
                .collect();
            stubs.sort();
            for p in stubs {
                let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else { continue };
                let text = std::fs::read_to_string(&p).unwrap_or_default();
                let ver = text.trim();
                out.push((format!("{stem}-DLC"), (!ver.is_empty()).then(|| ver.to_string())));
            }
        }
        // Real DLC: a readme.txt must exist (Steam leaves empty dirs behind).
        for (ident, dir) in DLC_DIRS {
            let id = format!("{ident}-DLC");
            if out.iter().any(|(i, _)| *i == id) {
                continue;
            }
            let readme = self.game_dir.join("GameData/SquadExpansion").join(dir).join("readme.txt");
            if !readme.is_file() {
                continue;
            }
            let ver = dlc_version_re().and_then(|re| {
                read_lines(&readme).iter().find_map(|l| re.captures(l).and_then(|c| c.get(1)).map(|m| m.as_str().to_string()))
            });
            let ver = match (ident, ver) {
                ("MakingHistory", Some(v)) if v == "1.0" => Some("1.0.0".to_string()),
                (_, v) => v,
            };
            out.push((id, ver));
        }
        out
    }

    /// Relative paths of files under the game dir that are not in CKAN/, not in stock folders,
    /// and not owned by any registry entry (`is_owned`).
    pub fn unmanaged_files(&self, is_owned: &dyn Fn(&str) -> bool) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack: Vec<(PathBuf, String)> = vec![(self.game_dir.clone(), String::new())];
        while let Some((dir, rel)) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                let r = if rel.is_empty() { name } else { format!("{rel}/{name}") };
                let Ok(ft) = e.file_type() else { continue };
                if ft.is_dir() {
                    // Skip the CKAN dir and stock folders as whole trees.
                    if r == "CKAN" || STOCK_FOLDERS.contains(&r.as_str()) {
                        continue;
                    }
                    stack.push((e.path(), r));
                } else if (!ft.is_symlink() || e.path().is_file()) && !is_owned(&r) {
                    out.push(r);
                }
            }
        }
        out.sort();
        out
    }
}

/// `buildID64.txt` or `buildID.txt`, and a `GameData` dir.
pub fn is_valid_game_dir(dir: &Path) -> bool {
    (dir.join("buildID64.txt").is_file() || dir.join("buildID.txt").is_file()) && dir.join("GameData").is_dir()
}

/// Detect a game version from buildID files (with `builds`, then EMBEDDED_BUILDS) or readme.txt.
pub fn detect_version(dir: &Path, builds: &HashMap<String, GameVersion>) -> Option<GameVersion> {
    let mut found: Vec<GameVersion> = Vec::new();
    if let Some(re) = build_id_re() {
        for f in ["buildID64.txt", "buildID.txt"] {
            let lines = read_lines(&dir.join(f));
            let id = lines.iter().find_map(|l| re.captures(l).and_then(|c| c.get(1)).map(|m| m.as_str().to_string()));
            if let Some(v) = id.and_then(|id| builds.get(&id).or_else(|| embedded_builds().get(&id)).copied()) {
                found.push(v);
            }
        }
    }
    if let Some(v) = found.into_iter().max() {
        return Some(v);
    }
    let re = readme_version_re()?;
    read_lines(&dir.join("readme.txt"))
        .iter()
        .find_map(|l| re.captures(l).and_then(|c| c.get(1)).and_then(|m| GameVersion::parse(m.as_str())))
}

/// Parse a builds.json document into a map.
/// Bad entries are skipped; a bad document gives an empty map.
pub fn parse_builds(json: &str) -> HashMap<String, GameVersion> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else { return HashMap::new() };
    let Some(map) = v.get("builds").and_then(|b| b.as_object()) else { return HashMap::new() };
    map.iter().filter_map(|(k, v)| json_game_version(v).map(|g| (k.clone(), g))).collect()
}

/// Candidate KSP dirs: Steam libraries (libraryfolders.vdf + appmanifest acf) and
/// `/Applications/Kerbal Space Program`. Only valid dirs are returned.
pub fn find_installs() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let steam_root = home.join("Library/Application Support/Steam");
    find_installs_in(&steam_root, &[PathBuf::from(MAC_APP_DIR)])
}

/// `find_installs` with an explicit Steam root and extra candidate dirs.
pub fn find_installs_in(steam_root: &Path, extra: &[PathBuf]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut seen: Vec<PathBuf> = Vec::new();
    for d in steam::steam_game_dirs(steam_root).into_iter().chain(extra.iter().cloned()) {
        if !is_valid_game_dir(&d) {
            continue;
        }
        let canon = std::fs::canonicalize(&d).unwrap_or_else(|_| d.clone());
        if !seen.contains(&canon) {
            seen.push(canon);
            out.push(d);
        }
    }
    out
}

/// Create a fake KSP dir for tests and `instance fake`: buildID64.txt, readme.txt, GameData/Squad,
/// Ships/{VAB,SPH,@thumbs/...}, saves/{scenarios,training}, Missions, and optional DLC readmes.
///
/// `dlc` items are `(name, version)`; name is `MakingHistory`, `BreakingGround` (with or without
/// `-DLC`), or `Serenity`. The dir must be missing or empty.
pub fn create_fake(dir: &Path, version: &GameVersion, dlc: &[(&str, &str)]) -> Result<()> {
    if version.is_any() {
        return Err(Error::Other("fake instance needs a real game version".into()));
    }
    if dir.exists() {
        let mut rd = std::fs::read_dir(dir).ctx(dir.display())?;
        if rd.next().is_some() {
            return Err(Error::Conflict(format!("{} is not empty", dir.display())));
        }
    }
    let dirs = [
        "GameData/Squad",
        "Ships/VAB",
        "Ships/SPH",
        "Ships/@thumbs/VAB",
        "Ships/@thumbs/SPH",
        "saves/scenarios",
        "saves/training",
        "Missions",
    ];
    for d in dirs {
        let p = dir.join(d);
        std::fs::create_dir_all(&p).ctx(p.display())?;
    }
    let short = version.without_build();
    if version.build >= 0 {
        for f in ["buildID64.txt", "buildID.txt"] {
            std::fs::write(dir.join(f), format!("build id = {}", version.build)).ctx(f)?;
        }
    } else {
        // No build number: still write an anchor file so the dir is valid; the readme gives the version.
        std::fs::write(dir.join("buildID64.txt"), short.to_string()).ctx("buildID64.txt")?;
    }
    std::fs::write(dir.join("readme.txt"), format!("Version {short}")).ctx("readme.txt")?;
    for (name, ver) in dlc {
        let base = name.strip_suffix("-DLC").unwrap_or(name);
        let sub = match base {
            "MakingHistory" => "MakingHistory",
            "BreakingGround" | "Serenity" => "Serenity",
            other => return Err(Error::Other(format!("unknown DLC {other}"))),
        };
        let p = dir.join("GameData/SquadExpansion").join(sub);
        std::fs::create_dir_all(&p).ctx(p.display())?;
        std::fs::write(p.join("readme.txt"), format!("Version {ver}")).ctx(p.display())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
