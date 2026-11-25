//! Zip import helpers (Core/IO/ModuleImporter.cs): file list expansion, hash matching and
//! internal .ckan files inside a zip (JSON only; YAML .ckan files are skipped).

use std::borrow::Cow;
use std::collections::HashSet;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Map, Value};

use crate::error::{Error, Result};
use crate::meta::{GameVersion, Module};

/// Largest internal .ckan or .version entry we read (guards against zip bombs).
const MAX_ENTRY: u64 = 16 << 20;

/// Expand dirs to their top-level files, drop duplicates. Missing paths go to the second list.
/// Duplicates are found by (device, inode), so `d/../d/a.zip`, a symlink or a different letter
/// case still count as one file (a second store of a moved file would fail the import).
pub fn expand_paths(paths: &[PathBuf]) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut files = Vec::new();
    let mut missing = Vec::new();
    let mut seen_ids: HashSet<(u64, u64)> = HashSet::new();
    let mut seen_paths: HashSet<PathBuf> = HashSet::new();
    let mut push = |p: PathBuf, files: &mut Vec<PathBuf>| {
        let new = match std::fs::metadata(&p) {
            Ok(md) => seen_ids.insert((md.dev(), md.ino())),
            Err(_) => seen_paths.insert(p.clone()),
        };
        if new {
            files.push(p);
        }
    };
    for p in paths {
        if p.is_dir() {
            let mut inner: Vec<PathBuf> = match std::fs::read_dir(p) {
                Ok(rd) => rd.flatten().map(|e| e.path()).filter(|q| q.is_file()).collect(),
                Err(_) => Vec::new(),
            };
            inner.sort();
            for q in inner {
                push(q, &mut files);
            }
        } else if p.is_file() {
            push(p.clone(), &mut files);
        } else {
            missing.push(p.clone());
        }
    }
    (files, missing)
}

/// Result of matching files to modules.
#[derive(Debug, Default)]
pub struct Scan {
    pub matched: Vec<(PathBuf, Vec<Arc<Module>>)>,
    pub not_found: Vec<PathBuf>,
}

/// Match each file by hash (`lookup(sha1, sha256)`), else by internal .ckan files.
pub fn scan_files(files: &[PathBuf], lookup: &dyn Fn(&str, &str) -> Vec<Arc<Module>>) -> Scan {
    let mut scan = Scan::default();
    for f in files {
        let by_hash = match crate::util::file_hashes(f) {
            Ok((sha1, sha256)) => lookup(&sha1, &sha256),
            Err(_) => Vec::new(),
        };
        if !by_hash.is_empty() {
            scan.matched.push((f.clone(), by_hash));
            continue;
        }
        match internal_modules(f) {
            Ok(mods) if !mods.is_empty() => {
                scan.matched.push((f.clone(), mods.into_iter().map(Arc::new).collect()));
            }
            _ => scan.not_found.push(f.clone()),
        }
    }
    scan
}

fn zip_err(p: &Path, e: impl std::fmt::Display) -> Error {
    Error::Other(format!("{}: {e}", p.display()))
}

fn read_entry(zip: &mut zip::ZipArchive<std::fs::File>, name: &str, p: &Path) -> Result<Vec<u8>> {
    let f = zip.by_name(name).map_err(|e| zip_err(p, e))?;
    let mut buf = Vec::new();
    f.take(MAX_ENTRY).read_to_end(&mut buf).map_err(|e| zip_err(p, e))?;
    Ok(buf)
}

/// JSON as Newtonsoft reads it: a UTF-8 BOM is skipped, and on a parse error the text is
/// retried without `//` and `/* */` comments and trailing commas (Json.NET accepts both).
pub fn lenient_json(bytes: &[u8]) -> Cow<'_, [u8]> {
    let b = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    if serde_json::from_slice::<serde::de::IgnoredAny>(b).is_ok() {
        return Cow::Borrowed(b);
    }
    Cow::Owned(strip_json_extras(b))
}

/// Remove comments and trailing commas outside strings. Works on bytes: every byte that
/// matters here is ASCII, so multi-byte UTF-8 sequences pass through unchanged.
fn strip_json_extras(b: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    let mut in_str = false;
    // Index in `out` of a comma that may turn out to be trailing.
    let mut pending_comma: Option<usize> = None;
    while let Some(&c) = b.get(i) {
        if in_str {
            out.push(c);
            if c == b'\\' {
                if let Some(&n) = b.get(i + 1) {
                    out.push(n);
                    i += 1;
                }
            } else if c == b'"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        match (c, b.get(i + 1)) {
            (b'/', Some(b'/')) => {
                while b.get(i).is_some_and(|&x| x != b'\n') {
                    i += 1;
                }
                continue;
            }
            (b'/', Some(b'*')) => {
                i += 2;
                while b.get(i).is_some() && !(b.get(i) == Some(&b'*') && b.get(i + 1) == Some(&b'/')) {
                    i += 1;
                }
                i += 2;
                continue;
            }
            _ => {}
        }
        if c.is_ascii_whitespace() {
            out.push(c);
        } else if c == b'}' || c == b']' {
            if let Some(at) = pending_comma.take() {
                out.remove(at);
            }
            out.push(c);
        } else {
            pending_comma = None;
            if c == b',' {
                pending_comma = Some(out.len());
            } else if c == b'"' {
                in_str = true;
            }
            out.push(c);
        }
        i += 1;
    }
    out
}

fn ends_with_ci(s: &str, suffix: &str) -> bool {
    s.len() >= suffix.len() && s.get(s.len() - suffix.len()..).is_some_and(|t| t.eq_ignore_ascii_case(suffix))
}

/// Modules from `*.ckan` entries in a zip, with the CKAN import fixes: a default
/// `download` URL and `$vref: #/ckan/ksp-avc[/filter]` applied from the single .version file.
pub fn internal_modules(zip_path: &Path) -> Result<Vec<Module>> {
    let file = std::fs::File::open(zip_path).map_err(|e| zip_err(zip_path, e))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| zip_err(zip_path, e))?;
    let names: Vec<String> = zip.file_names().filter_map(|n| n.ok().map(|c| c.into_owned())).collect();
    let zip_name = zip_path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();

    let mut ckans: Vec<Map<String, Value>> = Vec::new();
    for n in names.iter().filter(|n| ends_with_ci(n, ".ckan")) {
        let buf = read_entry(&mut zip, n, zip_path)?;
        // JSON only. CKAN also reads YAML here.
        if let Ok(Value::Object(mut obj)) = serde_json::from_slice::<Value>(&lenient_json(&buf)) {
            if obj.get("download").is_none_or(Value::is_null) {
                let id = match obj.get("identifier") {
                    Some(Value::String(s)) => s.clone(),
                    Some(v) => v.to_string(),
                    None => String::new(),
                };
                obj.insert("download".into(), Value::String(format!("https://ckan/imported-from-zip/{id}/{zip_name}")));
            }
            ckans.push(obj);
        }
    }

    // Group by $vref and apply AVC data where asked.
    let vrefs: Vec<String> = {
        let mut v: Vec<String> = ckans.iter().map(vref_of).collect();
        v.sort();
        v.dedup();
        v
    };
    for vref in vrefs {
        let Some(filter) = avc_filter(&vref) else { continue };
        let mut avcs = Vec::new();
        for n in names.iter().filter(|n| ends_with_ci(n, ".version")) {
            if !filter_ok(n, filter.as_deref()) {
                continue;
            }
            let buf = read_entry(&mut zip, n, zip_path)?;
            if let Ok(Value::Object(o)) = serde_json::from_slice::<Value>(&lenient_json(&buf)) {
                avcs.push(Avc::from_json(&o));
            }
        }
        if avcs.len() > 1 {
            return Err(Error::Metadata(format!("{}: Too many .version files found!", zip_path.display())));
        }
        if let Some(avc) = avcs.first() {
            for obj in ckans.iter_mut().filter(|o| vref_of(o) == vref) {
                obj.remove("$vref");
                if let Some(v) = &avc.version
                    && obj.get("version").is_none_or(Value::is_null)
                {
                    obj.insert("version".into(), Value::String(v.clone()));
                }
                set_json_compatibility(obj, avc.ksp_version, avc.ksp_version_min, avc.ksp_version_max);
            }
        }
    }

    Ok(ckans
        .into_iter()
        .filter_map(|o| serde_json::to_vec(&Value::Object(o)).ok())
        .filter_map(|b| Module::from_json(&b).ok())
        .collect())
}

fn vref_of(o: &Map<String, Value>) -> String {
    o.get("$vref").and_then(Value::as_str).unwrap_or("").to_string()
}

/// `^#/ckan/ksp-avc(/(?<filter>.*))?$` -> Some(filter).
fn avc_filter(vref: &str) -> Option<Option<String>> {
    let rest = vref.strip_prefix("#/ckan/ksp-avc")?;
    if rest.is_empty() {
        return Some(None);
    }
    rest.strip_prefix('/').map(|f| Some(f.to_string()))
}

fn filter_ok(name: &str, filter: Option<&str>) -> bool {
    match filter {
        None => true,
        Some(f) => name == f || regex::Regex::new(f).is_ok_and(|r| r.is_match(name)),
    }
}

/// The parts of a KSP-AVC .version file that import uses.
#[derive(Debug, Default, PartialEq)]
pub struct Avc {
    pub version: Option<String>,
    pub ksp_version: Option<GameVersion>,
    pub ksp_version_min: Option<GameVersion>,
    pub ksp_version_max: Option<GameVersion>,
}

fn json_str(v: Option<&Value>) -> Option<String> {
    match v {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    }
}

/// Json.NET ToObject: an exact property name first, else a case-insensitive match.
fn get_ci<'a>(o: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    o.get(key).or_else(|| o.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v))
}

impl Avc {
    pub fn from_json(o: &Map<String, Value>) -> Avc {
        Avc {
            version: get_ci(o, "VERSION").and_then(avc_module_version),
            ksp_version: get_ci(o, "KSP_VERSION").and_then(avc_game_version),
            ksp_version_min: get_ci(o, "KSP_VERSION_MIN").and_then(avc_game_version),
            ksp_version_max: get_ci(o, "KSP_VERSION_MAX").and_then(avc_game_version),
        }
    }
}

/// JsonAvcToVersion: `MAJOR.MINOR.PATCH[.BUILD]`, missing parts default to 0.
fn avc_module_version(v: &Value) -> Option<String> {
    let (mut major, mut minor, mut patch) = ("0".to_string(), "0".to_string(), "0".to_string());
    let build: Option<String>;
    match v {
        Value::String(s) => {
            let mut it = s.split('.');
            if let Some(x) = it.next() {
                major = x.to_string();
            }
            if let Some(x) = it.next() {
                minor = x.to_string();
            }
            if let Some(x) = it.next() {
                patch = x.to_string();
            }
            build = it.next().map(str::to_string);
        }
        Value::Object(o) => {
            major = json_str(o.get("MAJOR")).unwrap_or(major);
            minor = json_str(o.get("MINOR")).unwrap_or(minor);
            patch = json_str(o.get("PATCH")).unwrap_or(patch);
            build = json_str(o.get("BUILD"));
        }
        _ => return None,
    }
    let mut s = format!("{major}.{minor}.{patch}");
    if let Some(b) = build.filter(|b| !b.trim().is_empty()) {
        s.push('.');
        s.push_str(&b);
    }
    Some(s)
}

/// JsonAvcToGameVersion: a missing or `-1` part ends the version; no major = any.
fn avc_game_version(v: &Value) -> Option<GameVersion> {
    let (major, minor, patch) = match v {
        Value::String(s) => {
            let mut it = s.split('.').map(str::to_string);
            (it.next(), it.next(), it.next())
        }
        Value::Object(o) => (json_str(o.get("MAJOR")), json_str(o.get("MINOR")), json_str(o.get("PATCH"))),
        _ => return None,
    };
    let wild = |p: &Option<String>| p.as_ref().is_none_or(|s| s.trim().parse::<i64>() == Ok(-1));
    let text = if wild(&major) {
        return Some(GameVersion::ANY);
    } else if wild(&minor) {
        major.unwrap_or_default()
    } else if wild(&patch) {
        format!("{}.{}", major.unwrap_or_default(), minor.unwrap_or_default())
    } else {
        format!("{}.{}.{}", major.unwrap_or_default(), minor.unwrap_or_default(), patch.unwrap_or_default())
    };
    GameVersion::parse(&text)
}

fn existing_gv(o: &Map<String, Value>, key: &str) -> Option<GameVersion> {
    let s = json_str(o.get(key)).or_else(|| json_str(o.get("ksp_version")))?;
    GameVersion::parse(s.strip_suffix('.').unwrap_or(&s))
}

/// GameVersion.SetJsonCompatibility: widen the module's game range with the AVC range.
pub fn set_json_compatibility(
    o: &mut Map<String, Value>,
    ver: Option<GameVersion>,
    min: Option<GameVersion>,
    max: Option<GameVersion>,
) {
    let real = |g: &Option<GameVersion>| g.filter(|g| !g.is_any());
    let (avc_min, avc_max) = if min.is_none() && max.is_none() { (ver, ver) } else { (min, max) };
    let mins: Vec<GameVersion> = [real(&existing_gv(o, "ksp_version_min")), real(&avc_min)].into_iter().flatten().collect();
    let maxes: Vec<GameVersion> = [real(&existing_gv(o, "ksp_version_max")), real(&avc_max)].into_iter().flatten().collect();
    let lo = mins.into_iter().min();
    let hi = maxes.into_iter().max();
    if lo.is_none() && hi.is_none() {
        return;
    }
    o.remove("ksp_version");
    o.remove("ksp_version_min");
    o.remove("ksp_version_max");
    match (lo, hi) {
        (Some(a), Some(b)) if a == b => {
            o.insert("ksp_version".into(), Value::String(a.to_string()));
        }
        (a, b) => {
            if let Some(a) = a {
                o.insert("ksp_version_min".into(), Value::String(a.to_string()));
            }
            if let Some(b) = b {
                o.insert("ksp_version_max".into(), Value::String(b.to_string()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hangar-modlist-{tag}-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        d
    }

    fn write_zip(path: &Path, entries: &[(&str, &str)]) {
        let f = std::fs::File::create(path).unwrap();
        let mut w = zip::ZipWriter::new(f);
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (n, body) in entries {
            w.start_file(*n, opts).unwrap();
            w.write_all(body.as_bytes()).unwrap();
        }
        w.finish().unwrap();
    }

    #[test]
    fn internal_ckan_with_avc() {
        let d = temp_dir("avc");
        let z = d.join("Paid Mod.zip");
        write_zip(
            &z,
            &[
                (
                    "GameData/Paid/Paid.ckan",
                    r##"{"spec_version":"v1.4","identifier":"PaidMod","name":"Paid","abstract":"x","license":"restricted",
                        "$vref":"#/ckan/ksp-avc","ksp_version_min":"1.10"}"##,
                ),
                (
                    "GameData/Paid/Paid.version",
                    r#"{"NAME":"Paid","VERSION":{"MAJOR":2,"MINOR":1,"PATCH":0},"KSP_VERSION_MIN":"1.11.0","KSP_VERSION_MAX":{"MAJOR":1,"MINOR":12,"PATCH":-1}}"#,
                ),
                ("GameData/Paid/readme.txt", "hi"),
            ],
        );
        let mods = internal_modules(&z).unwrap();
        assert_eq!(mods.len(), 1);
        let m = &mods[0];
        assert_eq!(m.version.as_str(), "2.1.0");
        assert_eq!(m.download, vec!["https://ckan/imported-from-zip/PaidMod/Paid Mod.zip"]);
        assert_eq!(m.ksp_version_min.map(|g| g.to_string()).as_deref(), Some("1.10"));
        assert_eq!(m.ksp_version_max.map(|g| g.to_string()).as_deref(), Some("1.12"));

        // Hash lookup miss falls back to the internal .ckan; a plain file is not found.
        let other = d.join("notes.txt");
        std::fs::write(&other, "x").unwrap();
        let scan = scan_files(&[z.clone(), other.clone()], &|_, _| Vec::new());
        assert_eq!(scan.matched.len(), 1);
        assert_eq!(scan.not_found, vec![other]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn hash_match_and_expand() {
        let d = temp_dir("hash");
        let a = d.join("a.zip");
        std::fs::write(&a, b"abc").unwrap();
        let m = Arc::new(Module::from_json(br#"{"identifier":"Abc","version":"1","download":"https://x/a.zip"}"#).unwrap());
        let mm = m.clone();
        let scan = scan_files(std::slice::from_ref(&a), &move |sha1, _| {
            if sha1 == "A9993E364706816ABA3E25717850C26C9CD0D89D" { vec![mm.clone()] } else { Vec::new() }
        });
        assert_eq!(scan.matched.len(), 1);
        assert!(Arc::ptr_eq(&scan.matched[0].1[0], &m));

        let (files, missing) = expand_paths(&[d.clone(), a.clone(), d.join("nope.zip")]);
        assert_eq!(files, vec![a]);
        assert_eq!(missing.len(), 1);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn bom_comments_and_trailing_commas() {
        // Windows-made files often start with a BOM; Json.NET also skips comments and trailing commas.
        let d = temp_dir("bom");
        let z = d.join("bom.zip");
        write_zip(
            &z,
            &[
                (
                    "GameData/B/B.ckan",
                    "\u{feff}{\"spec_version\":\"v1.4\",\"identifier\":\"BomMod\",\"author\":\"a\",\"license\":\"MIT\",\"$vref\":\"#/ckan/ksp-avc\",}",
                ),
                (
                    "GameData/B/B.version",
                    "\u{feff}{ // AVC file\n \"VERSION\": \"1.2.3\", /* x, ] */ \"URL\": \"http://a//b\",\n \"KSP_VERSION\": \"1.12.5\",\n}",
                ),
            ],
        );
        let mods = internal_modules(&z).unwrap();
        assert_eq!(mods.len(), 1);
        assert_eq!(mods[0].version.as_str(), "1.2.3");
        assert_eq!(mods[0].ksp_version.map(|g| g.to_string()).as_deref(), Some("1.12.5"));
        // Strings keep their slashes and commas; arrays lose trailing commas too.
        let v: Value = serde_json::from_slice(&lenient_json(br#"{"a":"x//y,]","b":[1,2,],}"#)).unwrap();
        assert_eq!(v, serde_json::json!({"a":"x//y,]","b":[1,2]}));
        // Valid JSON is not copied.
        assert!(matches!(lenient_json(b"{}"), Cow::Borrowed(_)));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn expand_dedups_same_file() {
        let d = temp_dir("dedup");
        let a = d.join("a.zip");
        std::fs::write(&a, b"x").unwrap();
        let name = d.file_name().unwrap().to_owned();
        let other_spelling = d.join("..").join(name).join("a.zip");
        let (files, missing) = expand_paths(&[d.clone(), other_spelling, a.clone()]);
        assert_eq!(files, vec![a]);
        assert!(missing.is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn avc_versions() {
        assert_eq!(avc_module_version(&Value::String("1.2".into())).as_deref(), Some("1.2.0"));
        assert_eq!(avc_module_version(&serde_json::json!({"MAJOR":1,"MINOR":2,"PATCH":3,"BUILD":4})).as_deref(), Some("1.2.3.4"));
        assert_eq!(avc_game_version(&serde_json::json!({"MAJOR":1,"MINOR":12,"PATCH":5})), GameVersion::parse("1.12.5"));
        assert_eq!(avc_game_version(&serde_json::json!({"MAJOR":-1})), Some(GameVersion::ANY));
        // Property names match case-insensitively, as in Json.NET.
        let lower = serde_json::json!({"version":"1.0.4","ksp_version_min":"1.8","Ksp_Version_Max":"1.12"});
        let avc = Avc::from_json(lower.as_object().unwrap());
        assert_eq!(avc.version.as_deref(), Some("1.0.4"));
        assert_eq!(avc.ksp_version_min, GameVersion::parse("1.8"));
        assert_eq!(avc.ksp_version_max, GameVersion::parse("1.12"));
        let mut o = Map::new();
        o.insert("ksp_version".into(), Value::String("1.12".into()));
        set_json_compatibility(&mut o, GameVersion::parse("1.12"), None, None);
        assert_eq!(o.get("ksp_version").and_then(Value::as_str), Some("1.12"));
        assert!(o.get("ksp_version_min").is_none());
    }
}
