//! Global settings and labels, stored in Hangar's own dir (`util::hangar_dir()`), in CKAN's
//! `config.json` and `labels.json` formats. On first run (no Hangar config yet) the instances,
//! auth tokens and labels from a real CKAN install (`util::ckan_app_dir()`) are imported read-only.

mod colors;

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::Result;
use crate::meta::ReleaseStatus;

pub const CONFIG_FILE: &str = "config.json";
pub const LABELS_FILE: &str = "labels.json";

/// Rename a corrupt file to `<name>_CORRUPTED_<stamp>` so the user can recover it.
/// An older copy with the same stamp is never replaced (`_2`, `_3`, ... are added).
fn rename_aside(path: &Path) {
    let mut base = path.as_os_str().to_owned();
    base.push(format!("_CORRUPTED_{}", crate::util::Utc::now().file_stamp()));
    for n in 1..100u32 {
        let mut s = base.clone();
        if n > 1 {
            s.push(format!("_{n}"));
        }
        let dest = PathBuf::from(s);
        if std::fs::symlink_metadata(&dest).is_err() {
            let _ = std::fs::rename(path, dest);
            return;
        }
    }
}

/// Remove keys whose value is null (serde defaults then apply).
fn drop_nulls(m: &mut Map<String, Value>) {
    m.retain(|_, v| !v.is_null());
}

/// Keep only the string items of a JSON array.
fn string_list(v: &Value) -> Option<Vec<String>> {
    Some(v.as_array()?.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
}

/// Valid `GameInstances` entries (Name and Path strings; Game defaults to KSP).
fn parse_instances(v: &Value) -> Vec<InstanceEntry> {
    let Some(a) = v.as_array() else { return Vec::new() };
    a.iter()
        .filter_map(|e| {
            let name = e.get("Name")?.as_str()?.to_string();
            let path = e.get("Path")?.as_str()?.to_string();
            let game = e.get("Game").and_then(Value::as_str).map(str::to_string).unwrap_or_else(default_game);
            Some(InstanceEntry { name, path, game })
        })
        .collect()
}

fn parse_tokens(v: &Value) -> BTreeMap<String, String> {
    v.as_object()
        .map(|o| o.iter().filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string()))).collect())
        .unwrap_or_default()
}

/// Parse a CKAN or Hangar config.json leniently (legacy keys, nulls, bad numbers).
/// None if the text is not a JSON object.
pub fn parse_config(bytes: &[u8]) -> Option<Config> {
    let mut m = match serde_json::from_slice::<Value>(bytes).ok()? {
        Value::Object(m) => m,
        _ => return None,
    };
    drop_nulls(&mut m);
    let mut c = Config::default();
    if let Some(v) = m.get("AutoStartInstance").and_then(Value::as_str) {
        c.auto_start_instance = Some(v.to_string()).filter(|s| !s.is_empty());
    }
    if let Some(v) = m.get("DownloadCacheDir").and_then(Value::as_str) {
        c.download_cache_dir = Some(v.to_string()).filter(|s| !s.is_empty());
    }
    c.cache_size_limit = m.get("CacheSizeLimit").and_then(Value::as_u64);
    c.refresh_rate = m.get("RefreshRate").and_then(Value::as_u64).filter(|n| *n > 0).and_then(|n| u32::try_from(n).ok());
    if let Some(v) = m.get("GameInstances").or_else(|| m.get("KspInstances")) {
        c.game_instances = parse_instances(v);
    }
    if let Some(v) = m.get("AuthTokens") {
        c.auth_tokens = parse_tokens(v);
    }
    if let Some(o) = m.get("GlobalInstallFiltersByGame").and_then(Value::as_object) {
        c.global_install_filters_by_game = o.iter().filter_map(|(k, v)| string_list(v).map(|l| (k.clone(), l))).collect();
    } else if let Some(l) = m.get("GlobalInstallFilters").and_then(string_list) {
        // Legacy array form applies to every game.
        for g in ["KSP", "KSP2"] {
            c.global_install_filters_by_game.insert(g.to_string(), l.clone());
        }
    }
    if let Some(h) = m.remove("Hangar") {
        c.hangar = serde_json::from_value(h).unwrap_or_default();
    }
    Some(c)
}

/// CKAN's config.json: `CKAN_CONFIG_FILE` overrides the default path, as in CKAN.
fn ckan_config_path() -> PathBuf {
    std::env::var_os("CKAN_CONFIG_FILE")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::util::ckan_app_dir().join(CONFIG_FILE))
}

/// Import the parts of CKAN's config.json that Hangar shares (instances, tokens, auto-start,
/// download cache dir). Read-only.
fn import_ckan_config(path: &Path) -> Config {
    let Some(src) = std::fs::read(path).ok().and_then(|b| parse_config(&b)) else {
        return Config::default();
    };
    Config {
        auto_start_instance: src.auto_start_instance,
        download_cache_dir: src.download_cache_dir,
        game_instances: src.game_instances,
        auth_tokens: src.auth_tokens,
        ..Config::default()
    }
}

/// Parse a labels.json leniently (legacy `module_identifiers`, nulls). None if not a JSON object.
pub fn parse_labels(bytes: &[u8]) -> Option<LabelList> {
    let v: Value = serde_json::from_slice(bytes).ok()?;
    let top = v.as_object()?;
    let mut labels = Vec::new();
    for item in top.get("labels").and_then(Value::as_array).into_iter().flatten() {
        let Some(mut m) = item.as_object().cloned() else { continue };
        drop_nulls(&mut m);
        if let Some(old) = m.remove("module_identifiers")
            && !m.contains_key("module_identifiers_by_game")
        {
            // Legacy form: an array applies to every game; an object is already by game.
            let by_game = match old {
                Value::Array(_) => {
                    let l = string_list(&old).unwrap_or_default();
                    (!l.is_empty()).then(|| serde_json::json!({ "KSP": l.clone(), "KSP2": l }))
                }
                Value::Object(_) => Some(old),
                _ => None,
            };
            if let Some(b) = by_game {
                m.insert("module_identifiers_by_game".into(), b);
            }
        }
        if let Some(Value::Object(o)) = m.get_mut("module_identifiers_by_game") {
            let clean: Map<String, Value> = o
                .iter()
                .filter_map(|(k, v)| string_list(v).map(|l| (k.clone(), Value::from(l))))
                .collect();
            *o = clean;
        }
        if let Ok(l) = serde_json::from_value::<Label>(Value::Object(m)) {
            labels.push(l);
        }
    }
    // Instance-specific labels first, then by instance name (stable).
    labels.sort_by(|a, b| {
        a.instance_name.is_none().cmp(&b.instance_name.is_none()).then_with(|| a.instance_name.cmp(&b.instance_name))
    });
    Some(LabelList { labels })
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct InstanceEntry {
    pub name: String,
    pub path: String,
    #[serde(default = "default_game")]
    pub game: String,
}

fn default_game() -> String {
    "KSP".into()
}

/// Read a percent leniently: any JSON number is rounded and limited to the u16 range, and a
/// value of another type gives 100. A bad hand-edited value must not make the whole `Hangar`
/// block fail to parse, because that would reset all the other Hangar settings.
fn lenient_percent<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<u16, D::Error> {
    let v = Value::deserialize(d)?;
    Ok(match v.as_f64() {
        // `as` saturates a float to the u16 range (and gives 0 for NaN).
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        Some(f) => f.round() as u16,
        None => 100,
    })
}

/// Hangar-only settings (CKAN ignores unknown keys, so they live in the same file).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HangarSettings {
    pub stability: ReleaseStatus,
    pub hide_epochs: bool,
    pub hide_v: bool,
    pub update_repos_on_launch: bool,
    pub with_recommends: bool,
    pub with_suggests: bool,
    /// Hidden column names in the mod list.
    pub hidden_columns: Vec<String>,
    /// Saved filter / search of the main list.
    pub last_search: String,
    pub dark_mode: Option<bool>,
    /// Text size of the window in percent (100 = normal).
    #[serde(deserialize_with = "lenient_percent")]
    pub text_size: u16,
}

impl Default for HangarSettings {
    fn default() -> Self {
        HangarSettings {
            stability: ReleaseStatus::Stable,
            hide_epochs: true,
            hide_v: false,
            update_repos_on_launch: true,
            with_recommends: true,
            with_suggests: false,
            hidden_columns: Vec::new(),
            last_search: String::new(),
            dark_mode: None,
            text_size: 100,
        }
    }
}

/// CKAN config.json shape (PascalCase keys), plus `Hangar` settings.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct Config {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_start_instance: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub download_cache_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_size_limit: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_rate: Option<u32>,
    #[serde(alias = "KspInstances")]
    pub game_instances: Vec<InstanceEntry>,
    pub auth_tokens: BTreeMap<String, String>,
    pub global_install_filters_by_game: BTreeMap<String, Vec<String>>,
    pub hangar: HangarSettings,
}

impl Config {
    /// Load `<hangar_dir>/config.json`; if missing, import from CKAN's config, else defaults.
    /// Never fails: a corrupt or unreadable file is renamed aside and defaults are used, so a
    /// later `save` cannot overwrite the user's file.
    pub fn load() -> Config {
        let path = Self::path();
        match std::fs::read(&path) {
            Ok(bytes) => match parse_config(&bytes) {
                Some(c) => c,
                None => {
                    rename_aside(&path);
                    Config::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let c = import_ckan_config(&ckan_config_path());
                // Save once so the import happens only on the first run.
                let _ = c.save();
                c
            }
            Err(_) => {
                rename_aside(&path);
                Config::default()
            }
        }
    }

    /// `<hangar_dir>/config.json`
    pub fn path() -> PathBuf {
        crate::util::hangar_dir().join(CONFIG_FILE)
    }

    pub fn save(&self) -> Result<()> {
        crate::util::write_json(&Self::path(), self)
    }

    /// Download cache dir: `DownloadCacheDir` or `<hangar_dir>/downloads`.
    /// A relative `DownloadCacheDir` is taken relative to the Hangar dir.
    pub fn cache_dir(&self) -> PathBuf {
        match self.download_cache_dir.as_deref().filter(|s| !s.trim().is_empty()) {
            Some(d) => {
                let p = PathBuf::from(d);
                if p.is_absolute() { p } else { crate::util::hangar_dir().join(p) }
            }
            None => crate::util::hangar_dir().join("downloads"),
        }
    }

    /// Entry of a registered instance by name.
    pub fn instance(&self, name: &str) -> Option<&InstanceEntry> {
        self.game_instances.iter().find(|e| e.name == name)
    }

    /// Repo archive cache dir: `<hangar_dir>/repos`.
    pub fn repo_cache_dir(&self) -> PathBuf {
        crate::util::hangar_dir().join("repos")
    }

    /// Global install filters for KSP.
    pub fn global_install_filters(&self) -> Vec<String> {
        self.global_install_filters_by_game.get("KSP").cloned().unwrap_or_default()
    }
}

/// CKAN ModuleLabel. Bool flags are omitted when false.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Label {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_name: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hide: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub notify_on_change: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub remove_on_change: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub alert_on_install: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub remove_on_install: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hold_version: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ignore_missing_files: bool,
    /// `{"KSP": ["Kopernicus"]}`. Legacy `module_identifiers` (array or object) is accepted on load.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub module_identifiers_by_game: BTreeMap<String, Vec<String>>,
}

impl Label {
    pub fn contains(&self, id: &str) -> bool {
        self.module_identifiers_by_game.get("KSP").is_some_and(|v| v.iter().any(|x| x == id))
    }

    /// Color as RGB: known .NET color names, `"R, G, B"`, `"A, R, G, B"`, or `#RRGGBB`.
    pub fn rgb(&self) -> Option<[u8; 3]> {
        colors::parse(self.color.as_deref()?)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LabelList {
    pub labels: Vec<Label>,
}

impl LabelList {
    /// Load `<hangar_dir>/labels.json`; if missing, import CKAN's labels.json; else defaults
    /// (Favourites, Hidden with hide, Held with hold_version). A corrupt or unreadable file is
    /// renamed aside.
    pub fn load() -> LabelList {
        let path = Self::path();
        match std::fs::read(&path) {
            Ok(bytes) => match parse_labels(&bytes) {
                Some(l) => l,
                None => {
                    rename_aside(&path);
                    LabelList::defaults()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                std::fs::read(crate::util::ckan_app_dir().join(LABELS_FILE))
                    .ok()
                    .and_then(|b| parse_labels(&b))
                    .unwrap_or_else(LabelList::defaults)
            }
            // Unreadable (permissions, a dir): keep it aside so `save` cannot replace it.
            Err(_) => {
                rename_aside(&path);
                LabelList::defaults()
            }
        }
    }

    /// `<hangar_dir>/labels.json`
    pub fn path() -> PathBuf {
        crate::util::hangar_dir().join(LABELS_FILE)
    }

    pub fn save(&self) -> Result<()> {
        crate::util::write_json(&Self::path(), self)
    }

    pub fn defaults() -> LabelList {
        LabelList {
            labels: vec![
                Label { name: "Favourites".into(), color: Some("PaleGreen".into()), ..Label::default() },
                Label { name: "Hidden".into(), color: Some("PaleVioletRed".into()), hide: true, ..Label::default() },
                Label {
                    name: "Held".into(),
                    color: Some("255, 255, 176".into()),
                    hold_version: true,
                    ..Label::default()
                },
            ],
        }
    }


    /// Labels that apply to this instance (instance_name None or equal).
    pub fn for_instance<'a>(&'a self, instance: &'a str) -> impl Iterator<Item = &'a Label> + 'a {
        self.labels.iter().filter(move |l| l.instance_name.as_deref().is_none_or(|n| n == instance))
    }

    /// Identifiers held at their installed version (labels with hold_version).
    pub fn held(&self, instance: &str) -> HashSet<String> {
        self.for_instance(instance)
            .filter(|l| l.hold_version)
            .flat_map(|l| l.module_identifiers_by_game.get("KSP").into_iter().flatten().cloned())
            .collect()
    }

    /// Identifiers hidden from the list (labels with hide).
    #[cfg(test)]
    pub fn hidden(&self, instance: &str) -> HashSet<String> {
        self.for_instance(instance)
            .filter(|l| l.hide)
            .flat_map(|l| l.module_identifiers_by_game.get("KSP").into_iter().flatten().cloned())
            .collect()
    }

    /// Add or remove `id` from the label at `index`.
    pub fn toggle(&mut self, index: usize, id: &str) {
        let Some(l) = self.labels.get_mut(index) else { return };
        let v = l.module_identifiers_by_game.entry("KSP".into()).or_default();
        if let Some(p) = v.iter().position(|x| x == id) {
            v.remove(p);
        } else {
            v.push(id.to_string());
            v.sort();
        }
    }
}

#[cfg(test)]
mod tests;
