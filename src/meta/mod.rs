//! CKAN metadata model (.ckan files). Field order of `Module` = CKAN write order.
//! Unknown keys are ignored (never use deny_unknown_fields).

pub mod game_version;
pub mod relationship;
pub mod shared;
pub mod version;

#[cfg(test)]
mod golden;
#[cfg(test)]
mod real_data;

use std::borrow::Cow;
use std::fmt;

use serde::de::{self, Deserializer};
use serde::ser::Serializer;
use serde::{Deserialize, Serialize};

pub use game_version::{GameVersion, VersionCriteria};
pub use relationship::{Relationship, RelationshipTarget};
pub use shared::{IStr, List};
pub use version::ModuleVersion;

use crate::error::{Error, Result};

/// Highest spec_version this client accepts (CKAN: a module loads only if ClientSpec > spec_version).
pub const CLIENT_SPEC: &str = "v1.36.999";

// ---------------------------------------------------------------- small enums

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum ReleaseStatus {
    #[default]
    Stable,
    Testing,
    Development,
}

impl ReleaseStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ReleaseStatus::Stable => "stable",
            ReleaseStatus::Testing => "testing",
            ReleaseStatus::Development => "development",
        }
    }

}

impl Serialize for ReleaseStatus {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ReleaseStatus {
    /// CKAN JsonReleaseStatusConverter: null -> stable, exact `alpha`/`beta` aliases, `""` is an
    /// error, then Newtonsoft StringEnumConverter (enum names ignoring case, or integers 0-2).
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let bad = |s: &dyn fmt::Display| de::Error::custom(format!("bad release_status {s}"));
        match Option::<serde_json::Value>::deserialize(d)? {
            None | Some(serde_json::Value::Null) => Ok(ReleaseStatus::Stable),
            Some(serde_json::Value::String(s)) => match s.as_str() {
                "alpha" => Ok(ReleaseStatus::Development),
                "beta" => Ok(ReleaseStatus::Testing),
                _ => match s.to_ascii_lowercase().as_str() {
                    "stable" => Ok(ReleaseStatus::Stable),
                    "testing" => Ok(ReleaseStatus::Testing),
                    "development" => Ok(ReleaseStatus::Development),
                    _ => Err(bad(&format!("{s:?}"))),
                },
            },
            Some(serde_json::Value::Number(n)) => match n.as_u64() {
                Some(0) => Ok(ReleaseStatus::Stable),
                Some(1) => Ok(ReleaseStatus::Testing),
                Some(2) => Ok(ReleaseStatus::Development),
                _ => Err(bad(&n)),
            },
            Some(o) => Err(bad(&o)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Kind {
    #[default]
    Package,
    Metapackage,
    Dlc,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Package => "package",
            Kind::Metapackage => "metapackage",
            Kind::Dlc => "dlc",
        }
    }
}

impl Serialize for Kind {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Kind {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let v: Option<String> = Option::deserialize(d)?;
        match v.as_deref().map(str::to_ascii_lowercase).as_deref() {
            None | Some("package") => Ok(Kind::Package),
            Some("metapackage") => Ok(Kind::Metapackage),
            Some("dlc") => Ok(Kind::Dlc),
            Some(o) => Err(de::Error::custom(format!("bad kind {o:?}"))),
        }
    }
}

/// `spec_version`: integer `1` or string `"v1.4"`. Kept as written for round trips.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SpecVersion {
    Int(u64),
    Str(IStr),
}

impl Default for SpecVersion {
    fn default() -> Self {
        SpecVersion::Int(1)
    }
}

impl SpecVersion {
    pub fn as_version(&self) -> ModuleVersion {
        match self {
            SpecVersion::Int(n) => ModuleVersion::new(&n.to_string()),
            SpecVersion::Str(s) => ModuleVersion::new(s),
        }
    }
}

// ---------------------------------------------------------------- one-or-many helpers

/// `"a"` or `["a","b"]` or null -> Vec. Written back as a scalar when there is one item.
pub mod one_or_many {
    use super::*;

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<Option<String>>),
    }

    pub fn deserialize<'de, D: Deserializer<'de>, C: From<Vec<String>>>(d: D) -> std::result::Result<C, D::Error> {
        Ok(C::from(deserialize_opt(d)?.unwrap_or_default()))
    }

    /// Like `deserialize`, but JSON null gives None (CKAN keeps the field null).
    pub fn deserialize_opt<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<Option<Vec<String>>, D::Error> {
        Ok(match Option::<OneOrMany>::deserialize(d)? {
            None => None,
            Some(OneOrMany::One(s)) => Some(vec![s]),
            Some(OneOrMany::Many(v)) => {
                let mut v: Vec<String> = v.into_iter().flatten().collect();
                v.shrink_to_fit();
                Some(v)
            }
        })
    }

    pub fn serialize<S: Serializer>(v: &[String], s: S) -> std::result::Result<S::Ok, S::Error> {
        match v {
            [one] => s.serialize_str(one),
            _ => v.serialize(s),
        }
    }
}

fn is_false(b: &bool) -> bool {
    !*b
}
fn is_zero(n: &u64) -> bool {
    *n == 0
}
fn default_install_to() -> String {
    "GameData".to_string()
}
fn default_license() -> List<String> {
    vec!["unknown".to_string()].into()
}
/// CKAN: a null license becomes `["unknown"]` after load.
fn de_license<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<List<String>, D::Error> {
    Ok(one_or_many::deserialize_opt(d)?.map_or_else(default_license, List::from))
}
/// Fields that Newtonsoft leaves at their default when the JSON value is null.
fn null_default<'de, D: Deserializer<'de>, T: Deserialize<'de> + Default>(d: D) -> std::result::Result<T, D::Error> {
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

/// Every license string CKAN accepts (CKAN.schema `definitions.license`).
pub const LICENSES: [&str; 92] = [
    "public-domain", "AFL-3.0", "AGPL-3.0", "Apache", "Apache-1.0", "Apache-2.0", "APSL-2.0", "Artistic",
    "Artistic-1.0", "Artistic-2.0", "BSD-2-clause", "BSD-3-clause", "BSD-4-clause", "ISC", "CC-BY", "CC-BY-1.0",
    "CC-BY-2.0", "CC-BY-2.5", "CC-BY-3.0", "CC-BY-4.0", "CC-BY-SA", "CC-BY-SA-1.0", "CC-BY-SA-2.0",
    "CC-BY-SA-2.5", "CC-BY-SA-3.0", "CC-BY-SA-4.0", "CC-BY-NC", "CC-BY-NC-1.0", "CC-BY-NC-2.0", "CC-BY-NC-2.5",
    "CC-BY-NC-3.0", "CC-BY-NC-4.0", "CC-BY-NC-SA", "CC-BY-NC-SA-1.0", "CC-BY-NC-SA-2.0", "CC-BY-NC-SA-2.5",
    "CC-BY-NC-SA-3.0", "CC-BY-NC-SA-4.0", "CC-BY-NC-ND", "CC-BY-NC-ND-1.0", "CC-BY-NC-ND-2.0",
    "CC-BY-NC-ND-2.5", "CC-BY-NC-ND-3.0", "CC-BY-NC-ND-4.0", "CC-BY-ND", "CC-BY-ND-1.0", "CC-BY-ND-2.0",
    "CC-BY-ND-2.5", "CC-BY-ND-3.0", "CC-BY-ND-4.0", "CC0", "CDDL", "CPL", "EFL-1.0", "EFL-2.0", "Expat", "MIT",
    "GPL-1.0", "GPL-2.0", "GPL-3.0", "LGPL-2.0", "LGPL-2.1", "LGPL-3.0", "GFDL-1.0", "GFDL-1.1", "GFDL-1.2",
    "GFDL-1.3", "GFDL-NIV-1.0", "GFDL-NIV-1.1", "GFDL-NIV-1.2", "GFDL-NIV-1.3", "LPPL-1.0", "LPPL-1.1",
    "LPPL-1.2", "LPPL-1.3c", "MPL-1.0", "MPL-1.1", "MPL-2.0", "Ms-PL", "Ms-RL", "Perl", "Python-2.0", "QPL-1.0",
    "Unlicense", "W3C", "WTFPL", "Zlib", "Zope", "open-source", "restricted", "unrestricted", "unknown",
];

/// Valid licenses that are NOT in `redistributable_license` (CKAN will not mirror these).
#[cfg(test)]
pub const NON_REDISTRIBUTABLE_LICENSES: [&str; 8] =
    ["AFL-3.0", "AGPL-3.0", "APSL-2.0", "MPL-1.0", "Ms-PL", "Ms-RL", "restricted", "unknown"];

pub fn is_valid_license(l: &str) -> bool {
    LICENSES.contains(&l)
}

#[cfg(test)]
pub fn is_redistributable_license(l: &str) -> bool {
    is_valid_license(l) && !NON_REDISTRIBUTABLE_LICENSES.contains(&l)
}
fn is_stable(r: &ReleaseStatus) -> bool {
    *r == ReleaseStatus::Stable
}
fn is_package(k: &Kind) -> bool {
    *k == Kind::Package
}

/// `Option<GameVersion>`: null -> None, strip one trailing `.`, parse; failure rejects the module.
mod opt_game_version {
    use super::*;

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<Option<GameVersion>, D::Error> {
        let v: Option<serde_json::Value> = Option::deserialize(d)?;
        let s = match v {
            None | Some(serde_json::Value::Null) => return Ok(None),
            Some(serde_json::Value::String(s)) => s,
            Some(serde_json::Value::Number(n)) => n.to_string(),
            Some(o) => return Err(de::Error::custom(format!("bad game version {o}"))),
        };
        GameVersion::parse_json(&s).map(Some).ok_or_else(|| de::Error::custom(format!("bad game version {s:?}")))
    }

    pub fn serialize<S: Serializer>(v: &Option<GameVersion>, s: S) -> std::result::Result<S::Ok, S::Error> {
        match v {
            Some(g) => s.serialize_str(&g.to_string()),
            None => s.serialize_none(),
        }
    }
}

// ---------------------------------------------------------------- resources, hashes, install

/// Known `resources` keys. `spacedock` and `curse` may be `{ "url": ... }` objects.
pub const RESOURCE_KEYS: [&str; 16] = [
    "homepage",
    "spacedock",
    "curse",
    "repository",
    "bugtracker",
    "discussions",
    "ci",
    "license",
    "manual",
    "metanetkan",
    "remote-avc",
    "remote-swinfo",
    "store",
    "steamstore",
    "gogstore",
    "epicstore",
];

/// Known resources as (key, url), sorted by key (the JSON order). Shared by reference count.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Resources(List<(&'static str, Box<str>)>);

impl Resources {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.iter().find(|(k, _)| *k == key).map(|(_, v)| &**v)
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn iter(&self) -> impl Iterator<Item = (&'static str, &str)> {
        self.0.iter().map(|(k, v)| (*k, &**v))
    }
    pub fn share(&mut self, other: &Resources) {
        self.0.share(&other.0);
    }
}

impl Serialize for Resources {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.collect_map(self.iter())
    }
}

impl<'de> Deserialize<'de> for Resources {
    /// `{key: "url" | {"url": "url", ...}}` or null. Unknown keys and other values are skipped;
    /// for a repeated key the last value counts. Read directly, without a JSON tree.
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        use serde::de::{IgnoredAny, MapAccess, Visitor};

        /// A resources key: its index in RESOURCE_KEYS, or None.
        struct Key(Option<&'static str>);
        impl<'de> Deserialize<'de> for Key {
            fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
                struct KV;
                impl Visitor<'_> for KV {
                    type Value = Key;
                    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                        f.write_str("a key")
                    }
                    fn visit_str<E: de::Error>(self, k: &str) -> std::result::Result<Key, E> {
                        Ok(Key(RESOURCE_KEYS.iter().find(|x| **x == k).copied()))
                    }
                }
                d.deserialize_str(KV)
            }
        }

        /// A string, or an object with a string `url` (last `url` wins); anything else is None.
        struct Url(Option<Box<str>>);
        impl<'de> Deserialize<'de> for Url {
            fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
                struct UV;
                impl<'de> Visitor<'de> for UV {
                    type Value = Url;
                    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                        f.write_str("a url")
                    }
                    fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<Url, E> {
                        Ok(Url(Some(v.into())))
                    }
                    fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> std::result::Result<Url, A::Error> {
                        let mut url = None;
                        while let Some(k) = m.next_key::<Cow<'de, str>>()? {
                            if k == "url" {
                                url = m.next_value::<serde_json::Value>()?.as_str().map(Box::from);
                            } else {
                                m.next_value::<IgnoredAny>()?;
                            }
                        }
                        Ok(Url(url))
                    }
                    fn visit_bool<E: de::Error>(self, _: bool) -> std::result::Result<Url, E> {
                        Ok(Url(None))
                    }
                    fn visit_i64<E: de::Error>(self, _: i64) -> std::result::Result<Url, E> {
                        Ok(Url(None))
                    }
                    fn visit_u64<E: de::Error>(self, _: u64) -> std::result::Result<Url, E> {
                        Ok(Url(None))
                    }
                    fn visit_f64<E: de::Error>(self, _: f64) -> std::result::Result<Url, E> {
                        Ok(Url(None))
                    }
                    fn visit_unit<E: de::Error>(self) -> std::result::Result<Url, E> {
                        Ok(Url(None))
                    }
                    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut a: A) -> std::result::Result<Url, A::Error> {
                        while a.next_element::<IgnoredAny>()?.is_some() {}
                        Ok(Url(None))
                    }
                }
                d.deserialize_any(UV)
            }
        }

        struct MV;
        impl<'de> Visitor<'de> for MV {
            type Value = Resources;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a resources object")
            }
            fn visit_unit<E: de::Error>(self) -> std::result::Result<Resources, E> {
                Ok(Resources::default())
            }
            fn visit_none<E: de::Error>(self) -> std::result::Result<Resources, E> {
                Ok(Resources::default())
            }
            fn visit_some<D: Deserializer<'de>>(self, d: D) -> std::result::Result<Resources, D::Error> {
                d.deserialize_map(MV)
            }
            fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> std::result::Result<Resources, A::Error> {
                let mut out: Vec<(&'static str, Box<str>)> = Vec::new();
                while let Some(Key(k)) = m.next_key::<Key>()? {
                    let Some(k) = k else {
                        m.next_value::<IgnoredAny>()?;
                        continue;
                    };
                    // A repeated key: the last value counts, also when it is not a url.
                    out.retain(|(x, _)| *x != k);
                    if let Url(Some(u)) = m.next_value::<Url>()? {
                        out.push((k, u));
                    }
                }
                out.sort_unstable_by_key(|(k, _)| *k);
                Ok(Resources(out.into()))
            }
        }
        d.deserialize_option(MV)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DownloadHash {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha1: Option<Box<str>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<Box<str>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstallStanza {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub find: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub find_regexp: Option<String>,
    #[serde(default = "default_install_to")]
    pub install_to: String,
    #[serde(rename = "as", default, skip_serializing_if = "Option::is_none")]
    pub as_: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty", with = "one_or_many")]
    pub filter: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty", with = "one_or_many")]
    pub filter_regexp: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty", with = "one_or_many")]
    pub include_only: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty", with = "one_or_many")]
    pub include_only_regexp: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub find_matches_files: bool,
}

impl InstallStanza {
    /// The default stanza used when a module has no `install` list.
    pub fn default_for(identifier: &str) -> Self {
        InstallStanza {
            file: None,
            find: Some(identifier.to_string()),
            find_regexp: None,
            install_to: default_install_to(),
            as_: None,
            filter: Vec::new(),
            filter_regexp: Vec::new(),
            include_only: Vec::new(),
            include_only_regexp: Vec::new(),
            find_matches_files: false,
        }
    }

    /// Short text such as `find "kOS" -> GameData`.
    pub fn describe(&self) -> String {
        let (k, v) = if let Some(f) = &self.file {
            ("file", f)
        } else if let Some(f) = &self.find {
            ("find", f)
        } else if let Some(f) = &self.find_regexp {
            ("find_regexp", f)
        } else {
            ("?", &self.install_to)
        };
        format!("{k} {v:?} -> {}", self.install_to)
    }
}

// ---------------------------------------------------------------- relationships (de)serialization

fn de_relationships<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<List<Relationship>, D::Error> {
    let v: Option<serde_json::Value> = Option::deserialize(d)?;
    match v {
        Some(serde_json::Value::Array(items)) => {
            Relationship::list_from_json(items).map(List::from).map_err(de::Error::custom)
        }
        _ => Ok(List::new()), // CKAN silently drops a non-array value
    }
}

// ---------------------------------------------------------------- load helpers

/// True when the top-level JSON object has `key` with a non-null value.
fn has_non_null_key(bytes: &[u8], key: &str) -> bool {
    serde_json::from_slice::<serde_json::Map<String, serde_json::Value>>(bytes)
        .ok()
        .and_then(|o| o.get(key).map(|v| !v.is_null()))
        .unwrap_or(false)
}

/// CKANPathUtils.NormalizePath: `\` -> `/`, then (for 2+ UTF-16 units) trim ALL trailing `/`.
/// None when the path is already normal (no allocation).
fn ckan_normalize_path(p: &str) -> Option<String> {
    let long = p.encode_utf16().nth(1).is_some();
    if !p.contains('\\') && !(long && p.ends_with('/')) {
        return None;
    }
    let s = p.replace('\\', "/");
    Some(if long { s.trim_end_matches('/').to_string() } else { s })
}

// ---------------------------------------------------------------- the module

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Module {
    #[serde(default)]
    pub spec_version: SpecVersion,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<Box<str>>,
    pub identifier: String,
    #[serde(default, deserialize_with = "null_default")]
    pub name: IStr,
    #[serde(rename = "abstract", default, deserialize_with = "null_default")]
    pub abstract_: IStr,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<IStr>,
    // Always written: an `author: []` that was read must not turn into a missing author
    // (which spec >= v1.28 rejects on the next load).
    #[serde(default, with = "one_or_many")]
    pub author: List<String>,
    pub version: ModuleVersion,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "opt_game_version")]
    pub ksp_version: Option<GameVersion>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "opt_game_version")]
    pub ksp_version_min: Option<GameVersion>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "opt_game_version")]
    pub ksp_version_max: Option<GameVersion>,
    #[serde(default, skip_serializing_if = "is_false", deserialize_with = "null_default")]
    pub ksp_version_strict: bool,
    #[serde(default = "default_license", deserialize_with = "de_license", serialize_with = "one_or_many::serialize")]
    pub license: List<String>,
    #[serde(default, skip_serializing_if = "is_stable")]
    pub release_status: ReleaseStatus,
    #[serde(default, skip_serializing_if = "Resources::is_empty")]
    pub resources: Resources,
    #[serde(default, skip_serializing_if = "List::is_empty", deserialize_with = "null_default")]
    pub tags: List<String>,
    #[serde(default, skip_serializing_if = "List::is_empty", deserialize_with = "null_default")]
    pub localizations: List<String>,
    #[serde(default, skip_serializing_if = "List::is_empty", deserialize_with = "null_default")]
    pub provides: List<String>,
    #[serde(default, skip_serializing_if = "List::is_empty", deserialize_with = "de_relationships")]
    pub depends: List<Relationship>,
    #[serde(default, skip_serializing_if = "List::is_empty", deserialize_with = "de_relationships")]
    pub recommends: List<Relationship>,
    #[serde(default, skip_serializing_if = "List::is_empty", deserialize_with = "de_relationships")]
    pub suggests: List<Relationship>,
    #[serde(default, skip_serializing_if = "List::is_empty", deserialize_with = "de_relationships")]
    pub supports: List<Relationship>,
    #[serde(default, skip_serializing_if = "List::is_empty", deserialize_with = "de_relationships")]
    pub conflicts: List<Relationship>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaced_by: Option<Box<RelationshipTarget>>,
    #[serde(default, skip_serializing_if = "List::is_empty", deserialize_with = "null_default")]
    pub install: List<InstallStanza>,
    #[serde(default, skip_serializing_if = "List::is_empty", with = "one_or_many")]
    pub download: List<String>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub download_size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download_hash: Option<Box<DownloadHash>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download_content_type: Option<IStr>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub install_size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_date: Option<Box<str>>,
    #[serde(default, skip_serializing_if = "is_package")]
    pub kind: Kind,
}

impl fmt::Display for Module {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.identifier, self.version)
    }
}

impl Module {
    /// Parse and validate one .ckan JSON document.
    pub fn from_json(bytes: &[u8]) -> Result<Module> {
        let mut m: Module = serde_json::from_slice(bytes).map_err(|e| Error::Metadata(e.to_string()))?;
        m.validate()?;
        // CKAN CheckHealth: a missing (or null) author is only allowed before spec v1.28.
        // `author: []` is a present, empty list in CKAN: keep it. The raw JSON is read again
        // only on this rare path (0 real modules).
        if m.author.is_empty() && !has_non_null_key(bytes, "author") {
            if m.spec_version.as_version() < ModuleVersion::new("v1.28") {
                m.author = vec![String::new()].into();
            } else {
                return Err(Error::Metadata(format!("{}: missing author", m.identifier)));
            }
        }
        // CKAN tags are a HashSet: drop duplicates, keep the first order.
        if m.tags.len() > 1 {
            let mut seen = std::collections::HashSet::with_capacity(m.tags.len());
            if !m.tags.iter().all(|t| seen.insert(t.as_str())) {
                let mut seen = std::collections::HashSet::with_capacity(m.tags.len());
                m.tags = m.tags.iter().filter(|t| seen.insert(t.as_str())).cloned().collect();
            }
        }
        // ModuleInstallDescriptor normalizes install_to on load.
        if m.install.iter().any(|st| ckan_normalize_path(&st.install_to).is_some()) {
            let mut install = m.install.to_vec();
            for st in &mut install {
                if let Some(n) = ckan_normalize_path(&st.install_to) {
                    st.install_to = n;
                }
            }
            m.install = install.into();
        }
        Ok(m)
    }

    /// CKAN 4-space-indented JSON with a trailing newline.
    pub fn to_json_pretty(&self) -> String {
        let mut buf = Vec::new();
        let fmt = serde_json::ser::PrettyFormatter::with_indent(b"    ");
        let mut ser = serde_json::Serializer::with_formatter(&mut buf, fmt);
        if self.serialize(&mut ser).is_err() {
            return String::new();
        }
        let mut s = String::from_utf8(buf).unwrap_or_default();
        s.push('\n');
        s
    }

    /// Load-time rules from CkanModule.cs and ModuleInstallDescriptor.cs.
    pub fn validate(&self) -> Result<()> {
        let bad = |s: String| Err(Error::Metadata(format!("{}: {s}", self.identifier)));
        let id_ok = self.identifier.len() >= 2
            && self.identifier.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
            && self.identifier.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
        if !id_ok {
            return bad("invalid identifier".into());
        }
        if self.ksp_version.is_some() && (self.ksp_version_min.is_some() || self.ksp_version_max.is_some()) {
            return bad("ksp_version together with ksp_version_min/max".into());
        }
        if let Some(l) = self.license.iter().find(|l| !is_valid_license(l)) {
            return bad(format!("invalid license {l:?}"));
        }
        if self.kind == Kind::Package && self.download.is_empty() {
            return bad("package without download".into());
        }
        if self.spec_version.as_version() >= ModuleVersion::new(CLIENT_SPEC) {
            return bad(format!("unsupported spec_version {:?}", self.spec_version));
        }
        for st in &self.install {
            let n = st.file.is_some() as u8 + st.find.is_some() as u8 + st.find_regexp.is_some() as u8;
            if n != 1 {
                return bad("install stanza needs exactly one of file/find/find_regexp".into());
            }
            let has_filter = !st.filter.is_empty() || !st.filter_regexp.is_empty();
            let has_include = !st.include_only.is_empty() || !st.include_only_regexp.is_empty();
            if has_filter && has_include {
                return bad("install stanza mixes filter and include_only".into());
            }
        }
        Ok(())
    }

    /// Install stanzas, or the default `{find: identifier, install_to: GameData}`.
    pub fn install_stanzas(&self) -> Cow<'_, [InstallStanza]> {
        if self.install.is_empty() {
            Cow::Owned(vec![InstallStanza::default_for(&self.identifier)])
        } else {
            Cow::Borrowed(&self.install[..])
        }
    }

    pub fn is_dlc(&self) -> bool {
        self.kind == Kind::Dlc
    }

    pub fn is_metapackage(&self) -> bool {
        self.kind == Kind::Metapackage
    }

    /// `[identifier] + provides`.
    pub fn provides_list(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.identifier.as_str()).chain(self.provides.iter().map(String::as_str))
    }

    /// CKAN StandardName: `<identifier>-<version with [^A-Za-z0-9_.-] -> "-">.zip`.
    /// The .NET regex works on UTF-16 units, so a char outside the BMP gives two `-`.
    pub fn standard_name(&self) -> String {
        let mut v = String::with_capacity(self.version.as_str().len());
        for c in self.version.as_str().chars() {
            if c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-' {
                v.push(c);
            } else {
                for _ in 0..c.len_utf16() {
                    v.push('-');
                }
            }
        }
        format!("{}-{}.zip", self.identifier, v)
    }

    pub fn display_name(&self) -> &str {
        if self.name.is_empty() { &self.identifier } else { &self.name }
    }

    /// Point the fields that equal those of `other` (normally the previous version of the same
    /// identifier) to its copy, so equal data is in memory once. The JSON output does not change.
    pub fn share_with(&mut self, other: &Module) {
        if let (SpecVersion::Str(a), SpecVersion::Str(b)) = (&mut self.spec_version, &other.spec_version) {
            a.share(b);
        }
        self.name.share(&other.name);
        self.abstract_.share(&other.abstract_);
        if let (Some(a), Some(b)) = (&mut self.description, &other.description) {
            a.share(b);
        }
        if let (Some(a), Some(b)) = (&mut self.download_content_type, &other.download_content_type) {
            a.share(b);
        }
        self.author.share(&other.author);
        self.license.share(&other.license);
        self.resources.share(&other.resources);
        self.tags.share(&other.tags);
        self.localizations.share(&other.localizations);
        self.provides.share(&other.provides);
        self.depends.share(&other.depends);
        self.recommends.share(&other.recommends);
        self.suggests.share(&other.suggests);
        self.supports.share(&other.supports);
        self.conflicts.share(&other.conflicts);
        self.install.share(&other.install);
    }

    /// Same identifier and version (CKAN module equality).
    #[cfg(test)]
    pub fn same_release(&self, other: &Module) -> bool {
        self.identifier == other.identifier && self.version == other.version
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_and_shapes() {
        let j = br#"{"spec_version":1,"identifier":"Foo","name":"Foo","abstract":"x","author":"me",
            "version":"1.0","license":["MIT","GPL-3.0"],"download":"https://x/y.zip",
            "resources":{"spacedock":{"url":"https://sd/1"},"homepage":"https://h","x_screenshot":"s"},
            "depends":[{"name":"Bar","min_version":"1.0"},{"any_of":[{"name":"A"},{"name":"B"}]}],
            "conflicts":"junk","install":[{"find":"Foo","install_to":"GameData","filter":".DS_Store"}],
            "release_status":"beta","x_extra":1}"#;
        let m = Module::from_json(j).unwrap();
        assert_eq!(m.author, vec!["me"]);
        assert_eq!(m.license.len(), 2);
        assert_eq!(m.resources.get("spacedock"), Some("https://sd/1"));
        assert!(m.resources.get("x_screenshot").is_none());
        assert_eq!(m.depends.len(), 2);
        assert!(m.conflicts.is_empty());
        assert_eq!(m.release_status, ReleaseStatus::Testing);
        assert_eq!(m.install[0].filter, vec![".DS_Store"]);
        let back = m.to_json_pretty();
        assert!(back.contains("\"author\": \"me\""));
        let m2 = Module::from_json(back.as_bytes()).unwrap();
        assert_eq!(m2.depends, m.depends);
    }

    fn load(extra: &str) -> Result<Module> {
        let j = format!(r#"{{"identifier":"Foo","version":"1.0","download":"https://x/y.zip"{extra}}}"#);
        Module::from_json(j.as_bytes())
    }

    #[test]
    fn load_rules() {
        // Author: optional before v1.28 (becomes [""]), required after.
        assert_eq!(load("").unwrap().author, vec![""]);
        assert!(load(r#","spec_version":"v1.28""#).is_err());
        assert!(load(r#","spec_version":"v1.28","author":"a""#).is_ok());
        // License: null or absent -> unknown; an unknown string is rejected.
        assert_eq!(load(r#","license":null"#).unwrap().license, vec!["unknown"]);
        assert_eq!(load("").unwrap().license, vec!["unknown"]);
        assert!(load(r#","license":"MIT-ish""#).is_err());
        assert!(is_redistributable_license("MIT") && !is_redistributable_license("restricted"));
        // Nulls that Newtonsoft ignores.
        let m = load(r#","name":null,"abstract":null,"tags":null,"provides":null,"install":null,"ksp_version_strict":null"#)
            .unwrap();
        assert_eq!(m.name, "");
        assert!(m.install.is_empty());
        // release_status.
        assert_eq!(load(r#","release_status":"alpha""#).unwrap().release_status, ReleaseStatus::Development);
        assert_eq!(load(r#","release_status":"Testing""#).unwrap().release_status, ReleaseStatus::Testing);
        assert_eq!(load(r#","release_status":null"#).unwrap().release_status, ReleaseStatus::Stable);
        assert_eq!(load(r#","release_status":2"#).unwrap().release_status, ReleaseStatus::Development);
        assert!(load(r#","release_status":"""#).is_err());
        assert!(load(r#","release_status":"BETA""#).is_err());
        assert!(load(r#","release_status":7"#).is_err());
        // Tags dedupe, install_to normalized.
        let m = load(r#","tags":["a","b","a"],"install":[{"find":"Foo","install_to":"GameData\\Sub/"}]"#).unwrap();
        assert_eq!(m.tags, vec!["a", "b"]);
        assert_eq!(m.install[0].install_to, "GameData/Sub");
        // Install stanza rules.
        assert!(load(r#","install":[{"install_to":"GameData"}]"#).is_err());
        assert!(load(r#","install":[{"find":"a","file":"b"}]"#).is_err());
        assert!(load(r#","install":[{"find":"a","filter":"x","include_only":"y"}]"#).is_err());
        // Game versions.
        assert!(load(r#","ksp_version":"1.12","ksp_version_min":"1.8""#).is_err());
        assert!(load(r#","ksp_version":"1.12.x""#).is_err());
        assert_eq!(load(r#","ksp_version_max":"1.1.""#).unwrap().ksp_version_max, GameVersion::parse("1.1"));
        // Spec version.
        assert!(load(r#","spec_version":"v1.36","author":"a""#).is_ok());
        assert!(load(r#","spec_version":"v1.37","author":"a""#).is_err());
        // Version with an overflowing epoch.
        assert!(Module::from_json(br#"{"identifier":"Foo","version":"99999999999:1","download":"u"}"#).is_err());
        // Package needs a download; a metapackage does not.
        assert!(Module::from_json(br#"{"identifier":"Foo","version":"1"}"#).is_err());
        assert!(Module::from_json(br#"{"identifier":"Foo","version":"1","kind":"metapackage"}"#).is_ok());
    }

    #[test]
    fn review_regressions() {
        // author: [] is present in CKAN (non-null list): accepted for spec >= v1.28, kept empty,
        // and written back so the next load still accepts it. null still counts as missing.
        let m = load(r#","spec_version":"v1.28","author":[]"#).unwrap();
        assert!(m.author.is_empty());
        let back = m.to_json_pretty();
        assert!(back.contains("\"author\": []"));
        assert!(Module::from_json(back.as_bytes()).is_ok());
        assert!(load(r#","spec_version":"v1.28","author":null"#).is_err());
        assert_eq!(load(r#","author":[]"#).unwrap().author, Vec::<String>::new());
        // install_to: CKAN NormalizePath trims ALL trailing slashes (TrimEnd).
        let m = load(r#","install":[{"find":"Foo","install_to":"GameData//Sub\\\\"}]"#).unwrap();
        assert_eq!(m.install[0].install_to, "GameData//Sub");
        assert_eq!(ckan_normalize_path("/"), None);
        assert_eq!(ckan_normalize_path("\\").as_deref(), Some("/"));
        assert_eq!(ckan_normalize_path("//").as_deref(), Some(""));
        assert_eq!(ckan_normalize_path("GameData"), None);
        // StandardName replaces per UTF-16 unit: the emoji gives "--", the colon "-".
        let j = "{\"identifier\":\"Foo\",\"version\":\"1.0 b\\ud83d\\ude00:x\",\"download\":\"u\"}";
        let m = Module::from_json(j.as_bytes()).unwrap();
        assert_eq!(m.standard_name(), "Foo-1.0-b---x.zip");
    }
}
