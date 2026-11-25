//! Install stanza matching (CKAN ModuleInstallDescriptor.FindInstallableFiles).

use crate::error::{Error, Result};
use crate::meta::{InstallStanza, Module};
use crate::util::normalize_path;

use super::{InstallableFile, MetaRegex};

/// `install_to` keys other than GameData and GameRoot, with their real relative path (KSP1).
pub const ALLOWED_INSTALL_TO: [(&str, &str); 10] = [
    ("Tutorial", "saves/training"),
    ("Scenarios", "saves/scenarios"),
    ("Missions", "Missions"),
    ("Ships", "Ships"),
    ("Ships/VAB", "Ships/VAB"),
    ("Ships/SPH", "Ships/SPH"),
    ("Ships/@thumbs", "Ships/@thumbs"),
    ("Ships/@thumbs/VAB", "Ships/@thumbs/VAB"),
    ("Ships/@thumbs/SPH", "Ships/@thumbs/SPH"),
    ("Ships/Script", "Ships/Script"),
];

/// `install_to` values (and their sub paths) under which directories may be created.
pub const CREATEABLE_INSTALL_TOS: [&str; 4] = ["GameData", "Tutorial", "Scenarios", "Missions"];
/// Destinations (and their sub paths) under which directories may be created.
pub const CREATEABLE_DIRS: [&str; 1] = ["Ships/Script"];
/// A leading folder with one of these names is stripped from the output name.
pub const RESERVED_PATHS: [&str; 3] = ["GameData", "Ships", "Missions"];

fn equals_or_under(path: &str, dir: &str) -> bool {
    path == dir || (path.len() > dir.len() && path.starts_with(dir) && path.as_bytes().get(dir.len()) == Some(&b'/'))
}

/// .NET `Path.GetDirectoryName` for `/` paths: text before the last `/`, or "" if none.
pub(crate) fn dirname(p: &str) -> &str {
    match p.rfind('/') {
        Some(0) => "",
        Some(i) => p.get(..i).unwrap_or(""),
        None => "",
    }
}

fn bad_location(what: &str) -> Error {
    Error::Metadata(format!("invalid install path {what:?}"))
}

/// Map a normalized `install_to` to the real relative dir.
fn install_dir(inst: &str) -> Result<String> {
    if inst.contains("/../") || inst.ends_with("/..") {
        return Err(bad_location(inst));
    }
    if inst == "GameData" || inst.starts_with("GameData/") {
        let sub = inst.get("GameData".len()..).unwrap_or("");
        let sub = sub.strip_prefix('/').unwrap_or(sub);
        return Ok(normalize_path(&format!("GameData/{sub}")));
    }
    if inst == "GameRoot" {
        return Ok(String::new());
    }
    ALLOWED_INSTALL_TO
        .iter()
        .find(|(k, _)| *k == inst)
        .map(|(_, v)| v.to_string())
        .ok_or_else(|| Error::Metadata(format!("unknown install path {inst:?}")))
}

/// One stanza with its regexes compiled.
struct Compiled<'a> {
    st: &'a InstallStanza,
    pat: MetaRegex,
    filter: Vec<String>,
    filter_re: Vec<MetaRegex>,
    include: Vec<String>,
    include_re: Vec<MetaRegex>,
}

impl<'a> Compiled<'a> {
    fn new(st: &'a InstallStanza) -> Result<Self> {
        let pat = if let Some(f) = &st.file {
            MetaRegex::new(&format!("^{}(/|$)", regex::escape(&normalize_path(f))), true)?
        } else if let Some(f) = &st.find {
            MetaRegex::new(&format!("(?:^|/){}(/|$)", regex::escape(&normalize_path(f))), true)?
        } else if let Some(f) = &st.find_regexp {
            MetaRegex::new(f, true)?
        } else {
            return Err(Error::Metadata("install stanza needs one of file/find/find_regexp".into()));
        };
        let res = |v: &[String]| v.iter().map(|r| MetaRegex::new(r, false)).collect::<Result<Vec<_>>>();
        Ok(Compiled {
            st,
            pat,
            filter: st.filter.iter().map(|s| s.to_lowercase()).collect(),
            filter_re: res(&st.filter_regexp)?,
            include: st.include_only.iter().map(|s| s.to_lowercase()).collect(),
            include_re: res(&st.include_only_regexp)?,
        })
    }

    /// CKAN IsWanted.
    fn is_wanted(&self, path: &str, match_where: Option<usize>) -> bool {
        let Some(at) = self.pat.find_start(path) else {
            return false;
        };
        if match_where.is_some_and(|w| w != at) {
            return false;
        }
        let lower = path.to_lowercase();
        let in_segments = |words: &[String]| lower.split('/').any(|seg| words.iter().any(|w| w == seg));
        if in_segments(&self.filter) {
            return false;
        }
        if self.filter_re.iter().any(|r| r.is_match(path)) {
            return false;
        }
        if in_segments(&self.include) {
            return true;
        }
        if self.include_re.iter().any(|r| r.is_match(path)) {
            return true;
        }
        self.include.is_empty() && self.include_re.is_empty()
    }

    /// CKAN ShortestMatchingPrefix.
    fn shortest_matching_prefix<'p>(&self, full: &'p str) -> &'p str {
        let mut shortest = full;
        let mut p = full.strip_suffix('/').unwrap_or(full);
        while !p.is_empty() {
            if !self.pat.is_match(p) {
                break;
            }
            shortest = p;
            let next = dirname(p);
            if next.len() >= p.len() {
                break;
            }
            p = next;
        }
        shortest
    }

    /// CKAN TransformOutputName, plus a strict check that the result stays inside the game dir.
    fn transform_output_name(&self, full: &str, install_dir: &str) -> Result<String> {
        let lead = dirname(self.shortest_matching_prefix(full));
        let mut name = full;
        if !lead.is_empty() {
            let rest = name
                .strip_prefix(lead)
                .and_then(|r| r.strip_prefix('/'))
                .ok_or_else(|| Error::Metadata(format!("{full:?} does not start with {lead:?}")))?;
            name = rest;
        }
        let replaced;
        match self.st.as_.as_deref() {
            Some(a) if !a.trim().is_empty() => {
                if a.contains('/') || a.contains('\\') {
                    return Err(Error::Metadata(format!("\"as\" must not contain path separators: {a:?}")));
                }
                replaced = match name.find('/') {
                    Some(i) => format!("{a}{}", name.get(i..).unwrap_or("")),
                    None => a.to_string(),
                };
                name = &replaced;
            }
            _ => {
                for r in RESERVED_PATHS {
                    let hit = name.get(..r.len()).is_some_and(|p| p.eq_ignore_ascii_case(r))
                        && name.as_bytes().get(r.len()) == Some(&b'/');
                    if hit {
                        name = name.get(r.len() + 1..).unwrap_or("");
                        break;
                    }
                }
            }
        }
        if name.contains("/../") || name.ends_with("/..") {
            return Err(bad_location(name));
        }
        // Path.Combine: a rooted second part wins.
        let joined = if install_dir.is_empty() || name.starts_with('/') {
            name.to_string()
        } else if install_dir.ends_with('/') {
            format!("{install_dir}{name}")
        } else {
            format!("{install_dir}/{name}")
        };
        let dest = normalize_path(&joined);
        check_relative(&dest)?;
        Ok(dest)
    }
}

/// A destination must be relative and must not climb out of the game dir.
pub(crate) fn check_relative(rel: &str) -> Result<()> {
    if rel.starts_with('/') || rel.contains('\0') || rel.split('/').any(|s| s == "..") {
        return Err(bad_location(rel));
    }
    Ok(())
}

/// Files selected by one stanza for this module, CKAN rules (topmost `find`, filters, `as`,
/// reserved prefix strip, install_to mapping). Error if nothing matches or the path is bad.
pub fn find_installable_files(stanza: &InstallStanza, entries: &[String]) -> Result<Vec<InstallableFile>> {
    let inst = normalize_path(&stanza.install_to);
    let dir = install_dir(&inst)?;
    let c = Compiled::new(stanza)?;
    let names: Vec<String> = entries.iter().map(|e| e.replace('\\', "/")).collect();

    // `find` installs only the topmost match: the smallest match index over all entries.
    let shortest = if stanza.find.is_some() { names.iter().filter_map(|n| c.pat.find_start(n)).min() } else { None };
    let allow_dirs = CREATEABLE_INSTALL_TOS.iter().any(|d| equals_or_under(&inst, d));

    let mut out = Vec::new();
    for (i, name) in names.iter().enumerate() {
        if !c.is_wanted(name, shortest) {
            continue;
        }
        let destination = c.transform_output_name(name, &dir)?;
        let makedir = allow_dirs || CREATEABLE_DIRS.iter().any(|d| equals_or_under(&destination, d));
        out.push(InstallableFile {
            zip_index: i,
            source: name.clone(),
            destination,
            is_dir: name.ends_with('/'),
            makedir,
        });
    }
    if out.is_empty() {
        return Err(Error::Metadata(format!("no files found for {}", stanza.describe())));
    }
    Ok(out)
}

/// Like `module_files`, plus the number of files skipped by install filters.
pub fn module_files_counted(
    m: &Module,
    entries: &[String],
    filters: &[String],
) -> Result<(Vec<InstallableFile>, usize)> {
    let mut all = Vec::new();
    for st in m.install_stanzas().iter() {
        let files = find_installable_files(st, entries).map_err(|e| match e {
            Error::Metadata(s) => Error::Metadata(format!("{}: {s}", m)),
            o => o,
        })?;
        all.extend(files);
    }
    // .ckan files inside the zip are never installed.
    all.retain(|f| !f.source.to_ascii_lowercase().ends_with(".ckan"));
    let before = all.len();
    all.retain(|f| !filters.iter().any(|flt| !flt.is_empty() && f.destination.contains(flt.as_str())));
    let skipped = before - all.len();
    Ok((all, skipped))
}

/// All files for a module (all stanzas, `.ckan` entries dropped), with install filters applied
/// (a destination containing any filter substring is skipped).
pub fn module_files(m: &Module, entries: &[String], filters: &[String]) -> Result<Vec<InstallableFile>> {
    module_files_counted(m, entries, filters).map(|(f, _)| f)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stanza(json: &str) -> InstallStanza {
        serde_json::from_str(json).unwrap()
    }

    fn entries(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn dests(st: &InstallStanza, e: &[String]) -> Vec<String> {
        find_installable_files(st, e).unwrap().into_iter().map(|f| f.destination).collect()
    }

    fn module(json: &str) -> Module {
        Module::from_json(json.as_bytes()).unwrap()
    }

    fn doge_zip() -> Vec<String> {
        entries(&[
            "DogeCoinFlag-1.01/",
            "DogeCoinFlag-1.01/GameData/",
            "DogeCoinFlag-1.01/GameData/DogeCoinFlag/",
            "DogeCoinFlag-1.01/GameData/DogeCoinFlag/Flags/",
            "DogeCoinFlag-1.01/GameData/DogeCoinFlag/Flags/dogecoin.png",
            "DogeCoinFlag-1.01/META.ckan",
            "DogeCoinFlag-1.01/README.md",
        ])
    }

    fn doge_extra_zip() -> Vec<String> {
        entries(&[
            "DogeCoinFlag-1.01/",
            "DogeCoinFlag-1.01/GameData/",
            "DogeCoinFlag-1.01/GameData/DogeCoinFlag/",
            "DogeCoinFlag-1.01/GameData/DogeCoinFlag/Flags/",
            "DogeCoinFlag-1.01/GameData/DogeCoinFlag/Flags/dogecoin.png",
            "DogeCoinFlag-1.01/GameData/DogeCoinFlag/Flags/README.md",
            "DogeCoinFlag-1.01/GameData/DogeCoinFlag/README.md",
            "DogeCoinFlag-1.01/GameData/DogeCoinFlag/notes.txt.bak",
            "DogeCoinFlag-1.01/META.ckan",
            "DogeCoinFlag-1.01/README.md",
        ])
    }

    const DOGE: &str = r#"{"spec_version":"v1.10","identifier":"DogeCoinFlag","version":"1.01",
        "download":"https://kerbalstuff.com/mod/269/Dogecoin%20Flag/download/1.01",
        "install":[
          {"file":"DogeCoinFlag-1.01/GameData/DogeCoinFlag","install_to":"GameData",
           "filter":["Thumbs.db","README.md"],"filter_regexp":"\\.bak$"},
          {"file":"DogeCoinFlag-1.01/META.ckan","install_to":"GameData"}]}"#;

    fn doge_find() -> Module {
        let mut m = module(DOGE);
        let mut install = m.install.to_vec();
        install[0].file = None;
        install[0].find = Some("DogeCoinFlag".into());
        m.install = install.into();
        m
    }

    // ModuleInstallDescriptorTests.TransformOutputName
    #[test]
    fn transform_output_name_cases() {
        let cases: [(&str, &str, &str, Option<&str>, &str); 9] = [
            ("GameData/kOS", "GameData/kOS/Plugins/kOS.dll", "GameData", None, "GameData/kOS/Plugins/kOS.dll"),
            ("kOS-1.1/GameData/kOS", "kOS-1.1/GameData/kOS/Plugins/kOS.dll", "GameData", None, "GameData/kOS/Plugins/kOS.dll"),
            ("ModuleManager.2.5.1.dll", "ModuleManager.2.5.1.dll", "GameData", None, "GameData/ModuleManager.2.5.1.dll"),
            ("Ships", "Ships/SPH/FAR Firehound.craft", "SomeDir/Ships", None, "SomeDir/Ships/SPH/FAR Firehound.craft"),
            ("GameData/kOS", "GameData/kOS/Plugins/kOS.dll", "GameData", Some("kOS-Renamed"), "GameData/kOS-Renamed/Plugins/kOS.dll"),
            ("kOS-1.1/GameData/kOS", "kOS-1.1/GameData/kOS/Plugins/kOS.dll", "GameData", Some("kOS-Renamed"), "GameData/kOS-Renamed/Plugins/kOS.dll"),
            ("ModuleManager.2.5.1.dll", "ModuleManager.2.5.1.dll", "GameData", Some("ModuleManager-Renamed.dll"), "GameData/ModuleManager-Renamed.dll"),
            ("GameData", "GameData/kOS/Plugins/kOS.dll", "GameData", Some("GameData-Renamed"), "GameData/GameData-Renamed/kOS/Plugins/kOS.dll"),
            ("Ships", "Ships/SPH/FAR Firehound.craft", "SomeDir/Ships", Some("Ships-Renamed"), "SomeDir/Ships/Ships-Renamed/SPH/FAR Firehound.craft"),
        ];
        for (file, out, dir, as_, want) in cases {
            let mut st = stanza(&format!(r#"{{"file":"{file}"}}"#));
            st.as_ = as_.map(String::from);
            let c = Compiled::new(&st).unwrap();
            assert_eq!(c.transform_output_name(out, dir).unwrap(), want, "{file} {out} {as_:?}");
        }
    }

    // ModuleInstallDescriptorTests.TransformOutputNameThrowsOnInvalidParameters
    #[test]
    fn transform_output_name_bad_as() {
        let cases = [
            ("GameData/kOS", "GameData/kOS/Plugins/kOS.dll", "kOS/Renamed"),
            ("kOS-1.1/GameData/kOS", "kOS-1.1/GameData/kOS/Plugins/kOS.dll", "kOS/Renamed"),
            ("ModuleManager.2.5.1.dll", "ModuleManager.2.5.1.dll", "Renamed/ModuleManager.dll"),
        ];
        for (file, out, as_) in cases {
            let mut st = stanza(&format!(r#"{{"file":"{file}"}}"#));
            st.as_ = Some(as_.into());
            assert!(Compiled::new(&st).unwrap().transform_output_name(out, "GameData").is_err());
        }
    }

    #[test]
    fn kos_example_topmost_find() {
        let st = stanza(r#"{"find":"kOS","install_to":"GameData"}"#);
        let e = entries(&[
            "kOS-1.1/",
            "kOS-1.1/GameData/",
            "kOS-1.1/GameData/kOS/",
            "kOS-1.1/GameData/kOS/Plugins/",
            "kOS-1.1/GameData/kOS/Plugins/kOS.dll",
            "kOS-1.1/GameData/kOS/Sub/kOS/x.cfg",
            "kOS-1.1/Extras/kOS/ignored.txt",
        ]);
        // "kOS-1.1/Extras/kOS" matches at char 14, before "kOS-1.1/GameData/kOS" (16), so it wins.
        assert_eq!(dests(&st, &e), vec!["GameData/kOS/ignored.txt"]);
        // Without the Extras entry the GameData folder is installed.
        let e2: Vec<String> = e.iter().filter(|s| !s.contains("Extras")).cloned().collect();
        assert_eq!(
            dests(&st, &e2),
            vec![
                "GameData/kOS",
                "GameData/kOS/Plugins",
                "GameData/kOS/Plugins/kOS.dll",
                "GameData/kOS/Sub/kOS/x.cfg"
            ]
        );
        assert!(find_installable_files(&st, &e2).unwrap().iter().all(|f| f.makedir));
    }

    #[test]
    fn topmost_is_by_char_index_not_depth() {
        // "Extras/kOS" matches at index 6 and wins over "kOS-1.1/GameData/kOS" (index 16).
        let st = stanza(r#"{"find":"kOS"}"#);
        let e = entries(&["kOS-1.1/GameData/kOS/a.dll", "Extras/kOS/b.txt"]);
        assert_eq!(dests(&st, &e), vec!["GameData/kOS/b.txt"]);
    }

    // ModuleInstallerTests.FindInstallableFiles / _WithKSP / Sanity
    #[test]
    fn doge_file_and_find() {
        for m in [module(DOGE), doge_find()] {
            let files = module_files(&m, &doge_zip(), &[]).unwrap();
            let src: Vec<_> = files.iter().map(|f| f.source.as_str()).collect();
            assert!(src.contains(&"DogeCoinFlag-1.01/GameData/DogeCoinFlag/Flags/dogecoin.png"));
            assert!(!src.iter().any(|s| s.ends_with(".ckan")), "META.ckan dropped");
            let d: Vec<_> = files.iter().map(|f| f.destination.as_str()).collect();
            assert!(d.contains(&"GameData/DogeCoinFlag/Flags/dogecoin.png"));
        }
    }

    // GH #315: FindInstallableFiles_WithBonusPath
    #[test]
    fn bonus_paths() {
        for p in [
            "GameData/SuchTest",
            "GameData/SuchTest/",
            "GameData\\SuchTest",
            "GameData\\SuchTest\\",
            "GameData\\SuchTest/",
            "GameData/SuchTest\\",
        ] {
            let mut st = module(DOGE).install[0].clone();
            st.install_to = p.into();
            let d = dests(&st, &doge_zip());
            assert!(d.contains(&"GameData/SuchTest/DogeCoinFlag/Flags/dogecoin.png".to_string()), "{p}");
        }
    }

    #[test]
    fn module_manager_and_mission() {
        let st = stanza(r#"{"file":"ModuleManager.2.5.1.dll","install_to":"GameData"}"#);
        assert_eq!(dests(&st, &entries(&["ModuleManager.2.5.1.dll"])), vec!["GameData/ModuleManager.2.5.1.dll"]);

        let st = stanza(r#"{"file":"AwesomeMission","install_to":"Missions"}"#);
        let e = entries(&[
            "AwesomeMission/",
            "AwesomeMission/persistent.loadmeta",
            "AwesomeMission/persistent.mission",
            "AwesomeMission/Banners/",
            "AwesomeMission/Banners/Fail/",
            "AwesomeMission/Banners/Fail/default.png",
            "AwesomeMission/Banners/Menu/default.png",
            "AwesomeMission/Banners/Success/default.png",
            "AwesomeMission/Ships/SPH/",
        ]);
        let d = dests(&st, &e);
        for want in [
            "Missions/AwesomeMission/Banners/Fail/default.png",
            "Missions/AwesomeMission/Banners/Menu/default.png",
            "Missions/AwesomeMission/Banners/Success/default.png",
            "Missions/AwesomeMission/persistent.loadmeta",
            "Missions/AwesomeMission/persistent.mission",
        ] {
            assert!(d.contains(&want.to_string()), "{want}");
        }
    }

    // FindInstallableFiles_WithFilter and FindInstallableFilesWithInclude
    #[test]
    fn filters_and_include() {
        for m in [module(DOGE), doge_find()] {
            let files = find_installable_files(&m.install[0], &doge_extra_zip()).unwrap();
            let src: Vec<_> = files.iter().map(|f| f.source.as_str()).collect();
            assert!(src.contains(&"DogeCoinFlag-1.01/GameData/DogeCoinFlag/Flags/dogecoin.png"));
            assert!(!src.contains(&"DogeCoinFlag-1.01/GameData/DogeCoinFlag/README.md"));
            assert!(!src.contains(&"DogeCoinFlag-1.01/GameData/DogeCoinFlag/Flags/README.md"));
            assert!(!src.contains(&"DogeCoinFlag-1.01/GameData/DogeCoinFlag/notes.txt.bak"));
        }
        let mut st = module(DOGE).install[0].clone();
        st.filter.clear();
        st.filter_regexp.clear();
        st.include_only = vec!["dogecoin.png".into()];
        st.include_only_regexp = vec!["\\.bak$".into()];
        let files = find_installable_files(&st, &doge_extra_zip()).unwrap();
        let src: Vec<_> = files.iter().map(|f| f.source.as_str()).collect();
        assert!(src.contains(&"DogeCoinFlag-1.01/GameData/DogeCoinFlag/Flags/dogecoin.png"));
        assert!(!src.contains(&"DogeCoinFlag-1.01/GameData/DogeCoinFlag/README.md"));
        assert!(!src.contains(&"DogeCoinFlag-1.01/GameData/DogeCoinFlag/Flags/README.md"));
        assert!(src.contains(&"DogeCoinFlag-1.01/GameData/DogeCoinFlag/notes.txt.bak"));
        // Filter words are whole segments, case-insensitive.
        let st = stanza(r#"{"find":"Foo","filter":"thumbs.DB"}"#);
        let d = dests(&st, &entries(&["Foo/Thumbs.db", "Foo/Thumbs.db.txt", "Foo/a.cfg"]));
        assert_eq!(d, vec!["GameData/Foo/Thumbs.db.txt", "GameData/Foo/a.cfg"]);
    }

    // FindInstallableFiles_NoInstallableFiles (GH #93)
    #[test]
    fn no_files_is_error() {
        let st = stanza(r#"{"file":"GameData/DogeCoinFlag","install_to":"GameData"}"#);
        let e = find_installable_files(&st, &doge_zip()).unwrap_err();
        assert!(e.to_string().contains("no files found"), "{e}");
    }

    // FindInstallableFiles_WithBadTarget
    #[test]
    fn bad_targets() {
        for loc in [
            "GameDataIsTheBestData",
            "Shups",
            "GameData/../../../../etc/pwned",
            "Ships/Foo",
            "GameRoot/saves",
            "GameRoot/CKAN",
            "GameData/..",
            "GameData\\..\\..\\etc\\pwned",
            "GameData\\..",
        ] {
            let mut st = module(DOGE).install[0].clone();
            st.install_to = loc.into();
            assert!(find_installable_files(&st, &doge_zip()).is_err(), "{loc}");
        }
    }

    // FindInstallableFiles_ZipSlip_Throws
    #[test]
    fn zip_slip() {
        let m = module(r#"{"identifier":"AwesomeMod","version":"1.0.0","download":"https://x/y.zip"}"#);
        let e = entries(&["AwesomeMod/", "AwesomeMod/../../../outside.txt"]);
        assert!(module_files(&m, &e, &[]).is_err());
        // GameRoot with a leading "../" is caught by the extra relative-path check.
        let st = stanza(r#"{"find_regexp":"^\\.\\.","install_to":"GameRoot"}"#);
        assert!(find_installable_files(&st, &entries(&["../x"])).is_err());
    }

    // AllowsInstallsToShipsDirectories
    #[test]
    fn ships_dirs() {
        let e = entries(&["ExampleShips/", "ExampleShips/AwesomeShip.craft"]);
        for dir in ["Ships", "Ships/VAB", "Ships/SPH", "Ships/@thumbs", "Ships/@thumbs/VAB", "Ships/@thumbs/SPH", "Ships/Script"] {
            let st = stanza(&format!(r#"{{"file":"ExampleShips/AwesomeShip.craft","install_to":"{dir}"}}"#));
            let files = find_installable_files(&st, &e).unwrap();
            assert_eq!(files.len(), 1);
            assert_eq!(files[0].destination, format!("{dir}/AwesomeShip.craft"));
            assert_eq!(files[0].makedir, dir == "Ships/Script", "{dir}");
        }
    }

    // AllowInstallsToScenarios, plus Tutorial
    #[test]
    fn scenarios_and_tutorial() {
        let e = entries(&[
            "saves/",
            "saves/scenarios/",
            "saves/scenarios/AwesomeRace/",
            "saves/scenarios/AwesomeRace.sfs",
            "saves/scenarios/AwesomeRace/persistent.sfs",
        ]);
        let m = module(
            r#"{"spec_version":"v1.14","identifier":"AwesomeMod","version":"1.0.0","download":"https://x/y.zip",
            "install":[{"file":"saves/scenarios/AwesomeRace.sfs","install_to":"Scenarios"},
                       {"file":"saves/scenarios/AwesomeRace","install_to":"Scenarios"}]}"#,
        );
        let files = module_files(&m, &e, &[]).unwrap();
        let mut d: Vec<_> = files.iter().map(|f| f.destination.clone()).collect();
        d.sort();
        assert_eq!(
            d,
            vec!["saves/scenarios/AwesomeRace", "saves/scenarios/AwesomeRace.sfs", "saves/scenarios/AwesomeRace/persistent.sfs"]
        );
        assert!(files.iter().all(|f| f.makedir));
        let st = stanza(r#"{"file":"Training","install_to":"Tutorial"}"#);
        assert_eq!(dests(&st, &entries(&["Training/t.sfs"])), vec!["saves/training/Training/t.sfs"]);
    }

    #[test]
    fn reserved_prefix_and_gameroot() {
        // `file: GameData` + GameRoot strips the reserved prefix.
        let st = stanza(r#"{"file":"GameData","install_to":"GameRoot"}"#);
        let files = find_installable_files(&st, &entries(&["GameData/", "GameData/Foo/a.cfg"])).unwrap();
        let d: Vec<_> = files.iter().map(|f| f.destination.as_str()).collect();
        assert_eq!(d, vec!["", "Foo/a.cfg"]);
        assert!(files.iter().all(|f| !f.makedir));
        // Case-insensitive strip.
        let st = stanza(r#"{"find":"gamedata"}"#);
        assert_eq!(dests(&st, &entries(&["Mod/gamedata/X/a.cfg"])), vec!["GameData/X/a.cfg"]);
        // Ships folder at the top of a craft pack is stripped.
        let st = stanza(r#"{"find":"Ships","install_to":"Ships"}"#);
        assert_eq!(dests(&st, &entries(&["Pack/Ships/VAB/a.craft"])), vec!["Ships/VAB/a.craft"]);
    }

    #[test]
    fn backslash_entries_and_case() {
        let st = stanza(r#"{"find":"foo"}"#);
        let files = find_installable_files(&st, &entries(&["Pack\\Foo\\a.cfg"])).unwrap();
        assert_eq!(files[0].source, "Pack/Foo/a.cfg");
        assert_eq!(files[0].destination, "GameData/Foo/a.cfg");
        assert_eq!(files[0].zip_index, 0);
    }

    #[test]
    fn find_regexp_and_lookbehind_filter() {
        let st = stanza(
            r#"{"find":"Kopernicus","install_to":"GameData","filter_regexp":["(?<!Stock_.+\\.cfg)$"]}"#,
        );
        let e = entries(&["Kopernicus/Configs/Stock_Kerbin.cfg", "Kopernicus/Configs/Other.cfg", "Kopernicus/"]);
        // Everything that does not end in Stock_*.cfg is filtered.
        assert_eq!(dests(&st, &e), vec!["GameData/Kopernicus/Configs/Stock_Kerbin.cfg"]);
        let st = stanza(r#"{"find_regexp":"^GameData/SC-[^/]+(/|$)","install_to":"GameData"}"#);
        assert_eq!(dests(&st, &entries(&["GameData/SC-A/x.cfg", "Other/x"])), vec!["GameData/SC-A/x.cfg"]);
    }

    #[test]
    fn install_filters_and_count() {
        let m = module(r#"{"identifier":"Foo","version":"1","download":"https://x/y.zip"}"#);
        let e = entries(&["Foo/MiniAVC.dll", "Foo/a.cfg", "Foo/LICENSE-MiniAVC.txt"]);
        let (files, skipped) =
            module_files_counted(&m, &e, &["/MiniAVC.dll".into(), "/LICENSE-MiniAVC.txt".into(), String::new()]).unwrap();
        assert_eq!(skipped, 2);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].destination, "GameData/Foo/a.cfg");
    }

    #[test]
    fn default_stanza_find_identifier() {
        let m = module(r#"{"identifier":"DogeCoinFlag","version":"1","download":"https://x/y.zip"}"#);
        let d: Vec<_> = module_files(&m, &doge_zip(), &[]).unwrap().into_iter().map(|f| f.destination).collect();
        assert_eq!(d, vec!["GameData/DogeCoinFlag", "GameData/DogeCoinFlag/Flags", "GameData/DogeCoinFlag/Flags/dogecoin.png"]);
    }

    /// Every stanza in the real metadata: regexes compile, install_to maps, URL hashes work.
    /// Run with HANGAR_TEST_META=<master.tar.gz>.
    #[test]
    fn real_metadata_stanzas() {
        let Some(meta) = std::env::var_os("HANGAR_TEST_META") else { return };
        let gz = flate2::read::GzDecoder::new(std::fs::File::open(meta).unwrap());
        let mut ar = tar::Archive::new(gz);
        let (mut mods, mut stanzas, mut lookaround) = (0, 0, 0);
        let mut bad_regex = Vec::new();
        let mut bad_target = Vec::new();
        for e in ar.entries().unwrap() {
            let mut e = e.unwrap();
            let path = e.path().unwrap().to_string_lossy().into_owned();
            if !path.ends_with(".ckan") {
                continue;
            }
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut e, &mut buf).unwrap();
            let Ok(m) = Module::from_json(&buf) else { continue };
            mods += 1;
            for u in &m.download {
                assert_eq!(crate::install::cache::url_hash(u).len(), 8);
            }
            for st in m.install_stanzas().iter() {
                stanzas += 1;
                let all = st.find_regexp.iter().chain(&st.filter_regexp).chain(&st.include_only_regexp);
                lookaround += all.filter(|r| r.contains("(?<") || r.contains("(?!")).count();
                if let Err(e) = Compiled::new(st) {
                    bad_regex.push(format!("{path}: {e}"));
                }
                if let Err(e) = install_dir(&normalize_path(&st.install_to)) {
                    bad_target.push(format!("{path}: {e}"));
                }
            }
        }
        eprintln!("modules {mods}, stanzas {stanzas}, lookaround uses {lookaround}, bad targets {}", bad_target.len());
        for b in bad_target.iter().take(10) {
            eprintln!("  {b}");
        }
        assert!(mods > 30_000, "{mods}");
        assert!(lookaround >= 13);
        assert!(bad_regex.is_empty(), "{bad_regex:?}");
    }
}
