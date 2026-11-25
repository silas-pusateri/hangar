//! Relationship descriptors: `{name, version?, min_version?, max_version?}` or `{any_of: [...]}`.

use std::fmt;

use serde::ser::{SerializeMap, Serializer};
use serde::{Deserialize, Serialize};

use super::{Module, ModuleVersion};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelationshipTarget {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<ModuleVersion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_version: Option<ModuleVersion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_version: Option<ModuleVersion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choice_help_text: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub suppress_recommendations: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Relationship {
    One(RelationshipTarget),
    AnyOf { any_of: Vec<Relationship>, choice_help_text: Option<String>, suppress_recommendations: bool },
}

impl RelationshipTarget {
    pub fn simple(name: &str) -> Self {
        RelationshipTarget {
            name: name.to_string(),
            version: None,
            min_version: None,
            max_version: None,
            choice_help_text: None,
            suppress_recommendations: false,
        }
    }


    /// Bounds check. An exact `version` wins over min/max. Both bounds are inclusive.
    pub fn version_ok(&self, v: &ModuleVersion) -> bool {
        if let Some(want) = &self.version {
            return want == v;
        }
        self.min_version.as_ref().is_none_or(|m| m <= v) && self.max_version.as_ref().is_none_or(|m| m >= v)
    }

    /// True for the module with this identifier in bounds, or for any module that `provides` the name
    /// (provides never checks the version). A DLC with an unknown version (CKAN
    /// UnmanagedModuleVersion.IsUnknownVersion) is always in bounds.
    pub fn within_bounds(&self, m: &Module) -> bool {
        if m.identifier == self.name {
            is_unknown_dlc_version(m) || self.version_ok(&m.version)
        } else {
            m.provides.contains(&self.name)
        }
    }
}

impl fmt::Display for RelationshipTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name)?;
        if let Some(v) = &self.version {
            write!(f, " {v}")?;
        } else {
            match (&self.min_version, &self.max_version) {
                (Some(a), Some(b)) => write!(f, " {a}–{b}")?,
                (Some(a), None) => write!(f, " >= {a}")?,
                (None, Some(b)) => write!(f, " <= {b}")?,
                (None, None) => {}
            }
        }
        Ok(())
    }
}

/// A DLC module whose version is unknown: the registry stores CKAN's null DLC version as
/// `registry::UNKNOWN_DLC_VERSION` (or an empty string).
pub fn is_unknown_dlc_version(m: &Module) -> bool {
    let v = m.version.as_str();
    m.is_dlc() && (v.is_empty() || v == crate::registry::UNKNOWN_DLC_VERSION)
}

/// What satisfied a relationship in `Relationship::matches_any`.
#[cfg(test)]
#[derive(Debug, Clone, Copy)]
pub enum Match<'a> {
    Module(&'a Module),
    Dll,
    Dlc,
}

impl Relationship {
    /// CKAN JsonRelationshipConverter rules: skip non-objects and objects without name/any_of;
    /// `any_of` together with name/version keys is an error.
    pub fn list_from_json(items: Vec<serde_json::Value>) -> Result<Vec<Relationship>, String> {
        let mut out = Vec::new();
        for item in items {
            let serde_json::Value::Object(mut obj) = item else { continue };
            // A JSON null value still counts as present (Newtonsoft JValue null != C# null).
            if let Some(any) = obj.remove("any_of") {
                if ["name", "version", "min_version", "max_version"].iter().any(|k| obj.contains_key(*k)) {
                    return Err("any_of combined with name or version".into());
                }
                let mut children = match any {
                    serde_json::Value::Array(a) => Self::list_from_json(a)?,
                    _ => Vec::new(),
                };
                children.shrink_to_fit();
                let choice_help_text = obj.get("choice_help_text").and_then(|v| v.as_str()).map(String::from);
                let suppress_recommendations =
                    obj.get("suppress_recommendations").and_then(|v| v.as_bool()).unwrap_or(false);
                out.push(Relationship::AnyOf { any_of: children, choice_help_text, suppress_recommendations });
            } else if let Some(name) = obj.get_mut("name") {
                // CKAN accepts `"name": null` (a descriptor that matches nothing); use "".
                if name.is_null() {
                    *name = serde_json::Value::String(String::new());
                }
                let t: RelationshipTarget =
                    serde_json::from_value(serde_json::Value::Object(obj)).map_err(|e| e.to_string())?;
                out.push(Relationship::One(t));
            }
        }
        Ok(out)
    }

    /// CKAN MatchesAny: a DLL with the name matches any version; else the first module in
    /// bounds; else an installed DLC with the name in bounds. `dlc(name)` gives Some(version), where
    /// a None version means unknown (always in bounds).
    /// AnyOf: the first child matched by a MODULE. CKAN QUIRK (AnyOfRelationshipDescriptor):
    /// a child matched only by a DLL or a DLC gives no module, so it does not count.
    #[cfg(test)]
    pub fn matches_any<'a>(
        &self,
        mods: &[&'a Module],
        dll: &dyn Fn(&str) -> bool,
        dlc: &dyn Fn(&str) -> Option<Option<ModuleVersion>>,
    ) -> Option<Match<'a>> {
        match self {
            Relationship::One(t) => {
                if dll(&t.name) {
                    return Some(Match::Dll);
                }
                if let Some(m) = mods.iter().find(|m| t.within_bounds(m)) {
                    return Some(Match::Module(m));
                }
                match dlc(&t.name) {
                    Some(None) => Some(Match::Dlc),
                    Some(Some(v)) if t.version_ok(&v) => Some(Match::Dlc),
                    _ => None,
                }
            }
            Relationship::AnyOf { any_of, .. } => any_of.iter().find_map(|r| match r.matches_any(mods, dll, dlc) {
                Some(Match::Module(m)) => Some(Match::Module(m)),
                _ => None,
            }),
        }
    }

    pub fn within_bounds(&self, m: &Module) -> bool {
        match self {
            Relationship::One(t) => t.within_bounds(m),
            Relationship::AnyOf { any_of, .. } => any_of.iter().any(|r| r.within_bounds(m)),
        }
    }

    /// True if the descriptor names any identifier for which `has` is true.
    pub fn contains_any(&self, has: &dyn Fn(&str) -> bool) -> bool {
        match self {
            Relationship::One(t) => has(&t.name),
            Relationship::AnyOf { any_of, .. } => any_of.iter().any(|r| r.contains_any(has)),
        }
    }

    /// All names in the descriptor (one, or the any_of children, flattened).
    pub fn names(&self) -> Vec<&str> {
        match self {
            Relationship::One(t) => vec![t.name.as_str()],
            Relationship::AnyOf { any_of, .. } => any_of.iter().flat_map(|r| r.names()).collect(),
        }
    }

    pub fn suppress_recommendations(&self) -> bool {
        match self {
            Relationship::One(t) => t.suppress_recommendations,
            Relationship::AnyOf { suppress_recommendations, .. } => *suppress_recommendations,
        }
    }

    pub fn choice_help_text(&self) -> Option<&str> {
        match self {
            Relationship::One(t) => t.choice_help_text.as_deref(),
            Relationship::AnyOf { choice_help_text, .. } => choice_help_text.as_deref(),
        }
    }
}

impl fmt::Display for Relationship {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Relationship::One(t) => t.fmt(f),
            Relationship::AnyOf { any_of, .. } => {
                write!(f, "any of (")?;
                for (i, r) in any_of.iter().enumerate() {
                    if i > 0 {
                        write!(f, " | ")?;
                    }
                    r.fmt(f)?;
                }
                write!(f, ")")
            }
        }
    }
}

impl Serialize for Relationship {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Relationship::One(t) => t.serialize(s),
            Relationship::AnyOf { any_of, choice_help_text, suppress_recommendations } => {
                let mut m = s.serialize_map(None)?;
                m.serialize_entry("any_of", any_of)?;
                if let Some(c) = choice_help_text {
                    m.serialize_entry("choice_help_text", c)?;
                }
                if *suppress_recommendations {
                    m.serialize_entry("suppress_recommendations", &true)?;
                }
                m.end()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rels(j: &str) -> Result<Vec<Relationship>, String> {
        let serde_json::Value::Array(a) = serde_json::from_str(j).unwrap() else { panic!() };
        Relationship::list_from_json(a)
    }

    fn module(id: &str, ver: &str, provides: &str) -> Module {
        let j = format!(
            r#"{{"identifier":"{id}","version":"{ver}","author":"a","license":"MIT","download":"https://x/y.zip",
                "provides":[{provides}]}}"#
        );
        Module::from_json(j.as_bytes()).unwrap()
    }

    #[test]
    fn converter_rules() {
        let r = rels(r#"[{"name":"A"}, "junk", 3, {"comment":"no name"}, {"any_of":[{"name":"B"},{"name":"C"}],
            "choice_help_text":"pick"}, {"name":"D","min_version":"1.0","comment":"x"}]"#)
        .unwrap();
        assert_eq!(r.len(), 3);
        assert_eq!(r[1].names(), vec!["B", "C"]);
        assert_eq!(r[1].choice_help_text(), Some("pick"));
        assert!(rels(r#"[{"any_of":[{"name":"B"}],"name":"X"}]"#).is_err());
        assert!(rels(r#"[{"any_of":null,"version":"1"}]"#).is_err());
        assert!(rels(r#"[{"name":"A","version":"99999999999:1"}]"#).is_err());
        // Regression: a null name is kept (CKAN loads it), it matches nothing.
        let r = rels(r#"[{"name":null},{"name":"B"}]"#).unwrap();
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].names(), vec![""]);
    }

    #[test]
    fn bounds() {
        let a = module("Abc", "1.2", r#""Virtual""#);
        let t = |j: &str| rels(j).unwrap().remove(0);
        assert!(t(r#"[{"name":"Abc"}]"#).within_bounds(&a));
        assert!(t(r#"[{"name":"Abc","min_version":"1.2","max_version":"1.2"}]"#).within_bounds(&a));
        assert!(!t(r#"[{"name":"Abc","min_version":"1.3"}]"#).within_bounds(&a));
        assert!(!t(r#"[{"name":"Abc","max_version":"1.1"}]"#).within_bounds(&a));
        // Exact version wins over min/max; 1.20 != 1.2, but 1.02 == 1.2.
        assert!(!t(r#"[{"name":"Abc","version":"1.20","min_version":"0"}]"#).within_bounds(&a));
        assert!(t(r#"[{"name":"Abc","version":"1.02"}]"#).within_bounds(&a));
        // Provides never checks the version.
        assert!(t(r#"[{"name":"Virtual","version":"9.9"}]"#).within_bounds(&a));
        assert!(t(r#"[{"any_of":[{"name":"Zzz"},{"name":"Abc"}]}]"#).within_bounds(&a));
    }

    #[test]
    fn matches_any_order() {
        let a = module("Abc", "1.2", "");
        let mods = [&a];
        let no_dll = |_: &str| false;
        let no_dlc = |_: &str| None;
        let t = |j: &str| rels(j).unwrap().remove(0);
        let r = t(r#"[{"name":"Abc","min_version":"2"}]"#);
        assert!(r.matches_any(&mods, &no_dll, &no_dlc).is_none());
        assert!(matches!(r.matches_any(&mods, &|n: &str| n == "Abc", &no_dlc), Some(Match::Dll)));
        let r = t(r#"[{"name":"MakingHistory-DLC","min_version":"1.1"}]"#);
        let dlc_known = |_: &str| Some(Some(ModuleVersion::new("1.0")));
        let dlc_unknown = |_: &str| Some(None);
        assert!(r.matches_any(&mods, &no_dll, &dlc_known).is_none());
        assert!(matches!(r.matches_any(&mods, &no_dll, &dlc_unknown), Some(Match::Dlc)));
        let r = t(r#"[{"any_of":[{"name":"Nope"},{"name":"Abc"}]}]"#);
        assert!(matches!(r.matches_any(&mods, &no_dll, &no_dlc), Some(Match::Module(_))));
        // Regression (CKAN quirk): an any_of child matched only by a DLL or DLC does not count.
        let r = t(r#"[{"any_of":[{"name":"Dll"},{"name":"MakingHistory-DLC"}]}]"#);
        assert!(r.matches_any(&mods, &|n: &str| n == "Dll", &no_dlc).is_none());
        assert!(r.matches_any(&mods, &no_dll, &dlc_unknown).is_none());
        let r = t(r#"[{"any_of":[{"name":"Dll"},{"name":"Abc"}]}]"#);
        assert!(matches!(r.matches_any(&mods, &|n: &str| n == "Dll", &no_dlc), Some(Match::Module(_))));
    }

    #[test]
    fn unknown_dlc_version_in_bounds() {
        let dlc = |v: &str| {
            let j = format!(r#"{{"identifier":"MakingHistory-DLC","version":"{v}","author":"a","kind":"dlc"}}"#);
            Module::from_json(j.as_bytes()).unwrap()
        };
        let r = rels(r#"[{"name":"MakingHistory-DLC","min_version":"1.1"}]"#).unwrap().remove(0);
        assert!(r.within_bounds(&dlc(crate::registry::UNKNOWN_DLC_VERSION)));
        assert!(!r.within_bounds(&dlc("1.0.0")));
        assert!(r.within_bounds(&dlc("1.1.0")));
        // A normal module with version "0" is NOT unknown.
        let m = module("MakingHistory-DLC", "0", "");
        assert!(!r.within_bounds(&m));
    }
}
