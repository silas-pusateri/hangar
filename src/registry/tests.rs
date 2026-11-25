//! Registry unit tests. They only write inside fresh dirs under std::env::temp_dir().

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::*;

fn tmp(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let d = std::env::temp_dir().join(format!("hangar-registry-test-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(d.join("CKAN")).unwrap();
    std::fs::create_dir_all(d.join("GameData")).unwrap();
    d
}

fn cleanup(d: &Path) {
    // Only ever our own temp dir.
    if d.starts_with(std::env::temp_dir()) {
        let _ = std::fs::remove_dir_all(d);
    }
}

fn module(json: &str) -> Module {
    serde_json::from_str(json).unwrap()
}

fn simple(id: &str, ver: &str) -> Module {
    module(&format!(r#"{{"spec_version":1,"identifier":"{id}","name":"{id}","abstract":"x","author":"me","version":"{ver}","license":"MIT","download":"https://x/{id}.zip"}}"#))
}

fn inst(dir: &Path) -> Instance {
    Instance { name: "Test KSP".into(), game_dir: dir.to_path_buf(), version: None }
}

fn touch(dir: &Path, rel: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, b"x").unwrap();
}

const SPEC_SAMPLE: &str = r#"{
"registry_version": 3,
"sorted_repositories": {
"KSP-default": {"name": "KSP-default","uri": "https://github.com/KSP-CKAN/CKAN-meta/archive/master.tar.gz","priority": 0}
},
"installed_dlls": { "KSPCommunityFixes": "GameData/KSPCommunityFixes/KSPCommunityFixes.dll" },
"installed_modules": {
"ModuleManager": {
"install_time": "2025-03-01T12:34:56.1234567Z",
"source_module": { "spec_version": 1, "identifier": "ModuleManager", "name": "Module Manager",
  "abstract": "...", "author": "sarbian", "version": "4.2.3", "ksp_version_min": "1.8", "license": "CC-BY-SA",
  "download": "https://example.com/mm.zip", "download_size": 1234, "download_hash": {"sha1":"AA","sha256":"BB"},
  "download_content_type": "application/zip" },
"auto_installed": true,
"installed_files": { "GameData/ModuleManager.4.2.3.dll": {} }
},
"MakingHistory-DLC": {
"install_time": "2025-03-01T12:00:00Z",
"source_module": { "spec_version": "v1.28", "identifier": "MakingHistory-DLC", "name": "MakingHistory-DLC",
  "abstract": "An official expansion pack", "author": "SQUAD", "version": "1.9.1", "license": "restricted", "kind": "dlc" },
"installed_files": {}
}
},
"installed_files": { "GameData/ModuleManager.4.2.3.dll": "ModuleManager" },
"available_modules": {},
"download_counts": {}
}"#;

/// Shape of CKAN's own Tests/Data/registry.json (version 1): sha1_sum file entries, +11:00 times,
/// null members, legacy available_modules, no sorted_repositories.
const OLD_STYLE: &str = r#"{"registry_version":1,
"available_modules":{"AGExt":{"module_version":{"1.20":{"identifier":"AGExt","version":"1.20"}}}},
"installed_dlls":{},
"installed_modules":{
 "DogeCoinFlag":{"install_time":"2014-11-14T16:47:59.774894+11:00",
  "source_module":{"abstract":"Such flag. Very currency. To the mun! Wow!","description":null,"author":["pjf"],"comment":null,
   "conflicts":null,"depends":null,"download":"https://kerbalstuff.com/mod/269/Dogecoin%20Flag/download/1.02","download_size":53359,
   "identifier":"DogeCoinFlag","ksp_version":null,"ksp_version_max":null,"ksp_version_min":null,"license":"CC-BY","name":"Dogecoin Flag",
   "provides":null,"recommends":null,"release_status":"stable",
   "resources":{"repository":null,"homepage":"https://www.reddit.com/r/dogecoin/","bugtracker":null,"kerbalstuff":"https://kerbalstuff.com/mod/269"},
   "suggests":null,"version":"1.02","supports":null,
   "install":[{"file":"GameData/DogeCoinFlag","install_to":"GameData","filter":null,"filter_regexp":null}],"spec_version":"1"},
  "installed_files":{"GameData/DogeCoinFlag":{"sha1_sum":null},"GameData/DogeCoinFlag/Flags":{"sha1_sum":null},
   "GameData/DogeCoinFlag/Flags/dogecoin.png":{"sha1_sum":"D7-05-8C-3F-E4-88-22-E7-A2-71-B4-F8-A8-54-A2-5E-28-EA-E9-90"}}},
 "RSSTextures4096":{"install_time":"2014-11-23T11:50:45.088626+11:00",
  "source_module":{"abstract":"Textures","author":null,"depends":[{"max_version":null,"min_version":null,"name":"RealSolarSystem","version":null}],
   "download":"https://nabaal.net/4096.zip","download_size":0,"identifier":"RSSTextures4096","license":"CC-BY-NC-SA",
   "name":"RSS Textures","provides":["RSSTextures"],"version":"1.0","spec_version":1},
  "installed_files":{"GameData/RealSolarSystem/Plugins/PluginData/Pluto_NRM.png":{"sha1_sum":"42"}}}
},
"installed_files":{"GameData/DogeCoinFlag":"DogeCoinFlag","GameData/DogeCoinFlag/Flags":"DogeCoinFlag",
 "GameData/DogeCoinFlag/Flags/dogecoin.png":"DogeCoinFlag",
 "GameData/RealSolarSystem/Plugins/PluginData/Pluto_NRM.png":"RSSTextures4096"}
}"#;

fn write_registry(dir: &Path, text: &str) {
    std::fs::write(dir.join("CKAN").join(REGISTRY_FILE), text).unwrap();
}

/// Compare two registries through their JSON form.
fn as_value(r: &Registry) -> serde_json::Value {
    serde_json::from_slice(&r.to_ckan_json().unwrap()).unwrap()
}

// ---------------------------------------------------------------- lock

#[test]
fn lock_create_conflict_stale_and_drop() {
    let d = tmp("lock");
    let ckan = d.join("CKAN");
    {
        let lock = RegistryLock::acquire(&ckan).unwrap();
        let text = std::fs::read_to_string(&lock.path).unwrap();
        assert_eq!(text, std::process::id().to_string());
        // Our own PID is alive: a second lock fails.
        assert!(matches!(RegistryLock::acquire(&ckan), Err(Error::Locked(_))));
    }
    assert!(!ckan.join(LOCK_FILE).exists(), "drop removes the lock");

    // Dead PID: stale, removed.
    std::fs::write(ckan.join(LOCK_FILE), "999999").unwrap();
    let lock = RegistryLock::acquire(&ckan).unwrap();
    drop(lock);

    // Non-integer content: kept, locked.
    std::fs::write(ckan.join(LOCK_FILE), "hello").unwrap();
    assert!(matches!(RegistryLock::acquire(&ckan), Err(Error::Locked(_))));
    assert!(ckan.join(LOCK_FILE).exists(), "failed acquire must not remove a foreign lock");

    // Live foreign PID (pid 1 = launchd): kept.
    std::fs::write(ckan.join(LOCK_FILE), "1").unwrap();
    assert!(matches!(RegistryLock::acquire(&ckan), Err(Error::Locked(_))));
    cleanup(&d);
}

#[test]
fn lock_creates_missing_ckan_dir() {
    let d = tmp("lockdir");
    let ckan = d.join("Other").join("CKAN");
    let lock = RegistryLock::acquire(&ckan).unwrap();
    assert!(lock.path.exists());
    drop(lock);
    cleanup(&d);
}

// ---------------------------------------------------------------- load

#[test]
fn empty_has_default_repo() {
    let r = Registry::empty();
    assert_eq!(r.registry_version, 3);
    let repos = r.repositories();
    assert_eq!(repos.len(), 1);
    assert_eq!(repos[0].name, "KSP-default");
    assert_eq!(repos[0].uri, "https://github.com/KSP-CKAN/CKAN-meta/archive/master.tar.gz");
}

#[test]
fn missing_file_gives_empty_and_scans_dlls() {
    let d = tmp("missing");
    touch(&d, "GameData/Foo/Plugins/Foo.dll");
    let r = Registry::load(&d.join("CKAN")).unwrap();
    assert_eq!(r.sorted_repositories.len(), 1);
    assert_eq!(r.installed_dlls.get("Foo").map(String::as_str), Some("GameData/Foo/Plugins/Foo.dll"));
    cleanup(&d);
}

#[test]
fn corrupt_is_archived() {
    let d = tmp("corrupt");
    write_registry(&d, "{ this is not json");
    let r = Registry::load(&d.join("CKAN")).unwrap();
    assert!(r.installed_modules.is_empty());
    assert!(!d.join("CKAN/registry.json").exists());
    let archived: Vec<String> = std::fs::read_dir(d.join("CKAN"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("registry.json_CORRUPTED_"))
        .collect();
    assert_eq!(archived.len(), 1);
    assert_eq!(archived[0].len(), "registry.json_CORRUPTED_".len() + 14);
    cleanup(&d);
}

#[test]
fn newer_version_refused_and_untouched() {
    let d = tmp("v999");
    write_registry(&d, r#"{ "registry_version": 999 }"#);
    assert!(Registry::load(&d.join("CKAN")).is_err());
    let text = std::fs::read_to_string(d.join("CKAN/registry.json")).unwrap();
    assert!(text.contains("999"));
    cleanup(&d);
}

#[test]
fn bad_module_is_error_not_archived() {
    let d = tmp("badmod");
    write_registry(
        &d,
        r#"{"registry_version":3,"installed_modules":{"X":{"install_time":"2025-01-01T00:00:00Z","source_module":{"identifier":"X","version":"1","kind":"weird"}}}}"#,
    );
    assert!(matches!(Registry::load(&d.join("CKAN")), Err(Error::Json(_))));
    assert!(d.join("CKAN/registry.json").exists());
    cleanup(&d);
}

#[test]
fn version_1_control_lock_rekeyed() {
    // CKAN Tests/Data/registry-version-1.json
    let d = tmp("v1");
    write_registry(
        &d,
        r#"{"registry_version": 1, "installed_modules": {"001ControlLock": {"source_module": {
            "identifier": "001ControlLock", "name": "Control Lock", "abstract": "A mod that changed identifiers",
            "license": "unknown", "version": "1.0", "download": "https://github.com/controllock"},
            "installed_files": {"GameData/ControlLock/ControlLock.dll": {}}}}}"#,
    );
    let r = Registry::load(&d.join("CKAN")).unwrap();
    assert!(!r.installed_modules.contains_key("001ControlLock"));
    let cl = r.installed_modules.get("ControlLock").unwrap();
    assert_eq!(cl.source_module.identifier, "ControlLock");
    assert_eq!(r.owner_of("GameData/ControlLock/ControlLock.dll"), Some("ControlLock"));
    assert_eq!(r.registry_version, 3);
    assert_eq!(r.sorted_repositories.len(), 1);
    cleanup(&d);
}

#[test]
fn old_style_registry_upgraded_and_kept() {
    let d = tmp("old");
    write_registry(&d, OLD_STYLE);
    let r = Registry::load(&d.join("CKAN")).unwrap();
    assert_eq!(r.registry_version, 3);
    assert!(r.available_modules.is_empty());
    let doge = r.installed("DogeCoinFlag").unwrap();
    assert_eq!(doge.install_time, "2014-11-14T05:47:59.774894Z");
    assert_eq!(doge.installed_files.len(), 3);
    assert_eq!(doge.source_module.author, vec!["pjf"]);
    assert_eq!(r.installed("RSSTextures4096").unwrap().install_time, "2014-11-23T00:50:45.088626Z");
    assert_eq!(r.owner_of("GameData/DogeCoinFlag/Flags/dogecoin.png"), Some("DogeCoinFlag"));
    assert_eq!(r.repositories()[0].name, "KSP-default");
    assert_eq!(
        r.installed_version("RSSTextures", true),
        Some(InstalledVersion::Module(ModuleVersion::new("1.0")))
    );

    // Save and read back: all data kept, file entries are {} now.
    r.save(&inst(&d), None).unwrap();
    let text = std::fs::read_to_string(d.join("CKAN/registry.json")).unwrap();
    assert!(!text.contains("sha1_sum"));
    assert!(text.contains("\"available_modules\": {}"));
    let r2 = Registry::load(&d.join("CKAN")).unwrap();
    assert_eq!(as_value(&r), as_value(&r2));
    cleanup(&d);
}

#[test]
fn spec_sample_round_trip() {
    let d = tmp("sample");
    write_registry(&d, SPEC_SAMPLE);
    let r = Registry::load(&d.join("CKAN")).unwrap();
    assert_eq!(r.installed_dlls.len(), 1);
    let mm = r.installed("ModuleManager").unwrap();
    assert!(mm.auto_installed);
    assert_eq!(mm.install_time, "2025-03-01T12:34:56.1234567Z");
    assert_eq!(r.installed_mods().count(), 1);
    assert_eq!(
        r.installed_version("MakingHistory-DLC", false),
        Some(InstalledVersion::Dlc(Some(ModuleVersion::new("1.9.1"))))
    );

    r.save(&inst(&d), None).unwrap();
    let text = std::fs::read_to_string(d.join("CKAN/registry.json")).unwrap();
    assert!(text.ends_with("}\n"));
    // Same data as the input, field by field.
    let a: serde_json::Value = serde_json::from_str(SPEC_SAMPLE).unwrap();
    let b: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(a, b, "saved:\n{text}");
    // CKAN field order and no indent.
    let order: Vec<usize> = [
        "\"registry_version\"",
        "\"sorted_repositories\"",
        "\"installed_dlls\"",
        "\"installed_modules\"",
        "\"installed_files\": {\n\"GameData",
        "\"available_modules\"",
        "\"download_counts\"",
    ]
    .iter()
    .map(|k| text.find(k).unwrap())
    .collect();
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?}");
    assert!(text.lines().all(|l| !l.starts_with(' ')));
    // auto_installed omitted when false
    assert_eq!(text.matches("auto_installed").count(), 1);
    // nothing else written without an index
    assert!(!d.join("CKAN/history").exists());
    cleanup(&d);
}

#[test]
fn write_then_read_same() {
    let d = tmp("rt");
    let mut r = Registry::empty();
    r.register(simple("Alpha", "1.0"), vec!["GameData/Alpha".into(), "GameData/Alpha/a.cfg".into()], false).unwrap();
    r.register(simple("Beta", "2:0.5"), vec!["GameData/Beta/b.dll".into()], true).unwrap();
    r.installed_dlls.insert("Gamma".into(), "GameData/Gamma/Gamma.dll".into());
    r.set_dlc(&[("BreakingGround-DLC".into(), Some("1.7.1".into()))], None);
    r.available_modules.insert("junk".into(), serde_json::json!(1));
    r.save(&inst(&d), None).unwrap();
    let r2 = Registry::load(&d.join("CKAN")).unwrap();
    r.available_modules.clear();
    assert_eq!(as_value(&r), as_value(&r2));
    assert!(r2.installed("Beta").unwrap().auto_installed);
    cleanup(&d);
}

#[test]
fn missing_installed_files_reindexed_and_priorities_renumbered() {
    let d = tmp("reindex");
    write_registry(
        &d,
        r#"{"registry_version":3,
        "sorted_repositories":{
          "b":{"name":"b","uri":"https://b","priority":5},
          "a":{"name":"a","uri":"https://a","priority":5},
          "default":{"name":"default","uri":"https://github.com/KSP-CKAN/CKAN-meta/archive/master.zip","priority":0}},
        "installed_modules":{"Foo":{"install_time":"2025-01-01T00:00:00Z",
          "source_module":{"identifier":"Foo","version":"1","download":"https://x"},
          "installed_files":{"GameData/Foo/Foo.cfg":{}}}}}"#,
    );
    let r = Registry::load(&d.join("CKAN")).unwrap();
    assert_eq!(r.owner_of("GameData/Foo/Foo.cfg"), Some("Foo"));
    let repos = r.repositories();
    let names: Vec<(&str, i32)> = repos.iter().map(|x| (x.name.as_str(), x.priority)).collect();
    assert_eq!(names, vec![("default", 0), ("a", 1), ("b", 2)]);
    assert_eq!(repos[0].uri, crate::repo::DEFAULT_REPO_URI);
    cleanup(&d);
}

#[test]
fn version_0_paths_made_relative() {
    let d = tmp("v0");
    let abs = d.join("GameData/Foo/Foo.cfg");
    let json = format!(
        r#"{{"installed_modules":{{"Foo":{{"install_time":"2025-01-01T00:00:00Z",
          "source_module":{{"identifier":"Foo","version":"1","download":"https://x"}},
          "installed_files":{{"{0}":{{}}, "GameData\\Foo\\":{{}}}}}}}},
          "installed_files":{{"{0}":"Foo"}}}}"#,
        abs.display()
    );
    write_registry(&d, &json);
    let r = Registry::load(&d.join("CKAN")).unwrap();
    let files: Vec<&String> = r.installed("Foo").unwrap().installed_files.keys().collect();
    assert_eq!(files, vec!["GameData/Foo", "GameData/Foo/Foo.cfg"]);
    assert_eq!(r.owner_of("GameData/Foo/Foo.cfg"), Some("Foo"));
    cleanup(&d);
}

// ---------------------------------------------------------------- mutation

#[test]
fn register_rules() {
    let mut r = Registry::empty();
    r.installed_dlls.insert("Alpha".into(), "GameData/Alpha/Alpha.dll".into());
    r.installed_dlls.insert("Other".into(), "GameData/Shared/Other.dll".into());
    r.installed_dlls.insert("Keep".into(), "GameData/Keep/Keep.dll".into());
    r.register(
        simple("Alpha", "1.0"),
        vec!["GameData/Shared".into(), "GameData/Shared/a.cfg".into(), "GameData\\Shared\\Other.dll".into()],
        false,
    )
    .unwrap();
    // DLL entries for the identifier and for registered paths are removed.
    assert_eq!(r.installed_dlls.keys().collect::<Vec<_>>(), vec!["Keep"]);
    assert_eq!(r.owner_of("GameData/Shared/Other.dll"), Some("Alpha"));

    // A shared directory is fine; the last registrant owns it.
    r.register(simple("Beta", "1.0"), vec!["GameData/Shared".into(), "GameData/Shared/b.cfg".into()], false).unwrap();
    assert_eq!(r.owner_of("GameData/Shared"), Some("Beta"));

    // An owned file is a conflict, and nothing changes.
    let before = r.installed_files.clone();
    let e = r.register(simple("Gamma", "1.0"), vec!["GameData/Shared/a.cfg".into(), "GameData/G.cfg".into()], false);
    assert!(matches!(e, Err(Error::Conflict(_))));
    assert_eq!(r.installed_files, before);
    assert!(r.installed("Gamma").is_none());

    // Duplicate identifier and rooted path are errors.
    assert!(r.register(simple("Alpha", "2.0"), vec![], false).is_err());
    assert!(r.register(simple("Delta", "1.0"), vec!["/etc/passwd".into()], false).is_err());
}

#[test]
fn deregister_rules() {
    let d = tmp("dereg");
    let mut r = Registry::empty();
    r.register(simple("Alpha", "1.0"), vec!["GameData/Shared".into(), "GameData/Shared/a.cfg".into()], false).unwrap();
    r.register(simple("Beta", "1.0"), vec!["GameData/Shared".into(), "GameData/Shared/b.cfg".into()], false).unwrap();
    touch(&d, "GameData/Shared/b.cfg");
    // A listed file still on disk blocks it; the directory does not.
    assert!(matches!(r.deregister("Beta", &d), Err(Error::Conflict(_))));
    std::fs::remove_file(d.join("GameData/Shared/b.cfg")).unwrap();
    r.deregister("Beta", &d).unwrap();
    assert!(r.installed("Beta").is_none());
    assert_eq!(r.owner_of("GameData/Shared/b.cfg"), None);
    // The shared directory goes back to Alpha.
    assert_eq!(r.owner_of("GameData/Shared"), Some("Alpha"));
    assert!(matches!(r.deregister("Nope", &d), Err(Error::NotFound(_))));
    cleanup(&d);
}

#[test]
fn reindex_and_missing_files() {
    let d = tmp("missingfiles");
    let mut r = Registry::empty();
    r.register(simple("Alpha", "1.0"), vec!["GameData/Alpha".into(), "GameData/Alpha/a.cfg".into(), "GameData/Alpha/b.cfg".into()], false)
        .unwrap();
    r.installed_files.clear();
    r.reindex();
    assert_eq!(r.installed_files.len(), 3);
    touch(&d, "GameData/Alpha/a.cfg");
    assert_eq!(r.missing_files("Alpha", &d), vec!["GameData/Alpha/b.cfg"]);
    assert!(r.missing_files("Nope", &d).is_empty());
    cleanup(&d);
}

#[test]
fn installed_version_kinds() {
    let mut r = Registry::empty();
    let mut m = simple("Kopernicus", "1.0");
    m.provides = vec!["Planets".into()].into();
    r.register(m, vec![], false).unwrap();
    r.installed_dlls.insert("kOS".into(), "GameData/kOS/Plugins/kOS.dll".into());
    r.set_dlc(&[("MakingHistory-DLC".into(), None)], None);

    assert_eq!(r.installed_version("Kopernicus", false), Some(InstalledVersion::Module(ModuleVersion::new("1.0"))));
    assert_eq!(r.installed_version("kOS", false), Some(InstalledVersion::Dll));
    assert_eq!(r.installed_version("Planets", false), None);
    assert_eq!(r.installed_version("Planets", true), Some(InstalledVersion::Module(ModuleVersion::new("1.0"))));
    assert_eq!(r.installed_version("MakingHistory-DLC", true), Some(InstalledVersion::Dlc(None)));
    assert_eq!(r.installed_version("Nope", true), None);
    // DLC never counts as auto-installed, and is not in installed_mods.
    r.set_auto_installed("MakingHistory-DLC", true);
    assert!(!r.installed("MakingHistory-DLC").unwrap().auto_installed);
    assert_eq!(r.installed_mods().count(), 1);
}

// ---------------------------------------------------------------- DLLs and DLC

#[test]
fn dll_identifiers() {
    assert_eq!(Registry::dll_identifier("GameData/kOS/Plugins/kOS.Safe.dll").as_deref(), Some("kOS"));
    assert_eq!(Registry::dll_identifier("GameData/X/Kopernicus_Parser.dll").as_deref(), Some("Kopernicus-Parser"));
    assert_eq!(Registry::dll_identifier("GameData/ModuleManager.4.2.3.dll").as_deref(), Some("ModuleManager"));
    assert_eq!(Registry::dll_identifier("GameData/Foo/_Bar.DLL").as_deref(), Some("Bar"));
    assert_eq!(Registry::dll_identifier("GameData/Foo/bar.dll.bak"), None);
    assert_eq!(Registry::dll_identifier("Ships/Foo.dll"), None);
    assert_eq!(Registry::dll_identifier("gamedata/Foo.dll"), None);
}

#[test]
fn scan_dlls_rules() {
    let d = tmp("scan");
    touch(&d, "GameData/Squad/Plugins/Squad.dll");
    touch(&d, "GameData/SquadExpansion/MakingHistory/Plugins/MH.dll");
    touch(&d, "GameData/A/Plugins/Foo.dll");
    touch(&d, "GameData/B/Foo.Extra.DLL");
    touch(&d, "GameData/Managed/Managed.dll");
    touch(&d, "GameData/Owned/Owned.dll");
    touch(&d, "GameData/Readme.txt");
    let mut r = Registry::empty();
    r.register(simple("Managed", "1.0"), vec![], false).unwrap();
    r.register(simple("Other", "1.0"), vec!["GameData/Owned/Owned.dll".into()], false).unwrap();
    assert!(r.scan_dlls(&d));
    let got: Vec<(&str, &str)> = r.installed_dlls.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    // First path wins for Foo (breadth first like .NET EnumerateFiles, so the shallower file);
    // stock folders, installed identifiers and owned paths are dropped.
    assert_eq!(got, vec![("Foo", "GameData/B/Foo.Extra.DLL")]);
    assert!(!r.scan_dlls(&d), "second scan: no change");
    std::fs::remove_file(d.join("GameData/B/Foo.Extra.DLL")).unwrap();
    assert!(r.scan_dlls(&d));
    assert_eq!(r.installed_dlls.get("Foo").map(String::as_str), Some("GameData/A/Plugins/Foo.dll"));
    cleanup(&d);
}

#[test]
fn set_dlc_rules() {
    let mut r = Registry::empty();
    assert!(r.set_dlc(&[("MakingHistory-DLC".into(), Some("1.9.1".into()))], None));
    let im = r.installed("MakingHistory-DLC").unwrap();
    assert!(im.source_module.is_dlc());
    assert_eq!(im.source_module.author, vec!["SQUAD"]);
    assert_eq!(im.source_module.license, vec!["restricted"]);
    assert_eq!(im.source_module.abstract_, "An official expansion pack");
    assert!(im.installed_files.is_empty());
    let json = serde_json::to_value(&im.source_module).unwrap();
    assert_eq!(json["spec_version"], "v1.28");
    assert_eq!(json["kind"], "dlc");

    assert!(!r.set_dlc(&[("MakingHistory-DLC".into(), Some("1.9.1".into()))], None), "same: no change");

    // Repo metadata is used when the index has that version.
    let repo_mod = module(
        r#"{"spec_version":"v1.28","identifier":"BreakingGround-DLC","name":"Breaking Ground","abstract":"Robotics",
        "author":"SQUAD","version":"1.7.1","license":"restricted","kind":"dlc","resources":{"store":"https://store"}}"#,
    );
    let mut modules: HashMap<String, Vec<Arc<Module>>> = HashMap::new();
    modules.insert("BreakingGround-DLC".into(), vec![Arc::new(repo_mod)]);
    let index = RepoIndex { modules, ..Default::default() };
    assert!(r.set_dlc(&[("BreakingGround-DLC".into(), Some("1.7.1".into()))], Some(&index)));
    assert!(r.installed("MakingHistory-DLC").is_none(), "undetected DLC removed");
    assert_eq!(r.installed("BreakingGround-DLC").unwrap().source_module.name, "Breaking Ground");
    assert!(r.set_dlc(&[], None));
    assert!(r.installed_modules.is_empty());
}

// ---------------------------------------------------------------- real data

/// Optional: CKAN's own Tests/Data/registry.json (HANGAR_TEST_CKAN_REGISTRY = path).
#[test]
fn ckan_test_data_registry() {
    let Ok(p) = std::env::var("HANGAR_TEST_CKAN_REGISTRY") else { return };
    let d = tmp("ckandata");
    std::fs::copy(&p, d.join("CKAN/registry.json")).unwrap();
    let r = Registry::load(&d.join("CKAN")).unwrap();
    assert!(r.installed_modules.len() > 3);
    assert!(r.installed("DogeCoinFlag").unwrap().install_time.ends_with('Z'));
    r.save(&inst(&d), None).unwrap();
    let r2 = Registry::load(&d.join("CKAN")).unwrap();
    assert_eq!(as_value(&r), as_value(&r2));
    cleanup(&d);
}

/// Real metadata (HANGAR_TEST_META = master.tar.gz): every 25th module and the DLC modules go
/// through register -> save -> load unchanged.
#[test]
fn real_meta_round_trip() {
    use std::io::Read as _;
    let Ok(p) = std::env::var("HANGAR_TEST_META") else { return };
    let f = std::fs::File::open(p).unwrap();
    let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(f));
    let mut n = 0usize;
    let mut r = Registry::empty();
    let mut dlc: BTreeMap<String, Arc<Module>> = BTreeMap::new();
    for e in ar.entries().unwrap() {
        let mut e = e.unwrap();
        let path = e.path().unwrap().to_string_lossy().into_owned();
        if !path.ends_with(".ckan") {
            continue;
        }
        n += 1;
        let is_dlc = path.contains("-DLC/");
        if !n.is_multiple_of(25) && !is_dlc {
            continue;
        }
        let mut buf = Vec::new();
        e.read_to_end(&mut buf).unwrap();
        let Ok(m) = Module::from_json(&buf) else { continue };
        if m.is_dlc() {
            dlc.insert(format!("{} {}", m.identifier, m.version), Arc::new(m));
            continue;
        }
        if r.installed(&m.identifier).is_none() {
            let files = vec![format!("GameData/{}/{}.cfg", m.identifier, m.version)];
            r.register(m, files, n.is_multiple_of(2)).unwrap();
        }
    }
    assert!(r.installed_modules.len() > 500, "{}", r.installed_modules.len());
    assert!(!dlc.is_empty());
    let mut modules: HashMap<String, Vec<Arc<Module>>> = HashMap::new();
    for m in dlc.values() {
        modules.entry(m.identifier.clone()).or_default().push(m.clone());
    }
    let (id, ver) = {
        let m = dlc.values().next().unwrap();
        (m.identifier.clone(), m.version.to_string())
    };
    let index = RepoIndex { modules, ..Default::default() };
    assert!(r.set_dlc(&[(id.clone(), Some(ver))], Some(&index)));
    assert_ne!(r.installed(&id).unwrap().source_module.abstract_, "An official expansion pack");

    let d = tmp("realmeta");
    r.save(&inst(&d), None).unwrap();
    let r2 = Registry::load(&d.join("CKAN")).unwrap();
    assert_eq!(as_value(&r), as_value(&r2));
    cleanup(&d);
}

// ---------------------------------------------------------------- review regressions

#[test]
fn lock_drop_keeps_a_foreign_lock() {
    let d = tmp("lockforeign");
    let ckan = d.join("CKAN");
    let lock = RegistryLock::acquire(&ckan).unwrap();
    // Someone deleted our lock by hand and another program locked the folder.
    std::fs::write(ckan.join(LOCK_FILE), "1").unwrap();
    drop(lock);
    assert!(ckan.join(LOCK_FILE).exists(), "drop must not remove another process's lock");
    cleanup(&d);
}

#[test]
fn null_members_and_missing_install_time() {
    let d = tmp("nulls");
    write_registry(
        &d,
        r#"{"registry_version":3,"installed_dlls":{"Gone":null,"Kept":"GameData/Kept/Kept.dll"},
        "installed_modules":{
          "A":{"install_time":null,"auto_installed":null,"installed_files":null,
               "source_module":{"identifier":"A","version":"1","download":"https://x"}},
          "B":{"source_module":{"identifier":"B","version":"1","download":"https://x"},
               "installed_files":{"GameData/B/b.cfg":null}}},
        "installed_files":{"GameData/B/b.cfg":"B","GameData/Null":null}}"#,
    );
    let r = Registry::load(&d.join("CKAN")).unwrap();
    assert_eq!(r.installed_dlls.keys().collect::<Vec<_>>(), vec!["Kept"]);
    let a = r.installed("A").unwrap();
    assert!(!a.auto_installed);
    assert_eq!(a.install_time, "0001-01-01T00:00:00Z");
    // A null file entry keeps its path.
    assert!(r.installed("B").unwrap().installed_files.contains_key("GameData/B/b.cfg"));
    assert_eq!(r.installed("B").unwrap().install_time, "0001-01-01T00:00:00Z");
    r.save(&inst(&d), None).unwrap();
    let text = std::fs::read_to_string(d.join("CKAN/registry.json")).unwrap();
    assert!(!text.contains("\"install_time\": \"\""), "CKAN cannot parse an empty DateTime");
    let r2 = Registry::load(&d.join("CKAN")).unwrap();
    assert_eq!(as_value(&r), as_value(&r2));
    cleanup(&d);
}

#[test]
fn non_utf8_registry_is_loaded_not_archived() {
    let d = tmp("latin1");
    let mut bytes = br#"{"registry_version":3,"installed_modules":{"A":{"install_time":"2025-01-01T00:00:00Z",
        "source_module":{"identifier":"A","version":"1","download":"https://x","abstract":"caf"#
        .to_vec();
    bytes.push(0xE9); // Latin-1 e-acute: not UTF-8
    bytes.extend_from_slice(br#""}}}}"#);
    std::fs::write(d.join("CKAN").join(REGISTRY_FILE), &bytes).unwrap();
    let r = Registry::load(&d.join("CKAN")).unwrap();
    assert_eq!(r.installed("A").unwrap().source_module.abstract_, "caf\u{fffd}");
    assert!(d.join("CKAN/registry.json").exists());
    cleanup(&d);
}

#[test]
fn corrupt_archives_never_overwrite_each_other() {
    let d = tmp("corrupt2");
    for text in ["{ first", "{ second"] {
        write_registry(&d, text);
        Registry::load(&d.join("CKAN")).unwrap();
    }
    let mut kept: Vec<String> = std::fs::read_dir(d.join("CKAN"))
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("registry.json_CORRUPTED_"))
        .map(|e| std::fs::read_to_string(e.path()).unwrap())
        .collect();
    kept.sort();
    assert_eq!(kept, vec!["{ first", "{ second"]);
    cleanup(&d);
}

#[test]
fn version_0_path_outside_game_dir_is_refused() {
    let d = tmp("v0out");
    write_registry(
        &d,
        r#"{"installed_modules":{"Foo":{"install_time":"2025-01-01T00:00:00Z",
          "source_module":{"identifier":"Foo","version":"1","download":"https://x"},
          "installed_files":{"/etc/hosts":{}}}}}"#,
    );
    assert!(matches!(Registry::load(&d.join("CKAN")), Err(Error::Metadata(_))));
    assert!(d.join("CKAN/registry.json").exists(), "left as is");
    cleanup(&d);
}

#[test]
fn register_refuses_paths_that_leave_the_game_dir() {
    let mut r = Registry::empty();
    for bad in ["GameData/../../x.cfg", "../x.cfg", "GameData/a\0b"] {
        assert!(matches!(r.register(simple("Bad", "1.0"), vec![bad.into()], false), Err(Error::Conflict(_))), "{bad:?}");
    }
    assert!(r.installed_files.is_empty());
    // Dots inside a name are fine.
    r.register(simple("Ok", "1.0"), vec!["GameData/Ok/..hidden".into()], false).unwrap();
}

#[test]
fn deregister_counts_a_broken_symlink_as_a_file() {
    let d = tmp("brokenlink");
    let mut r = Registry::empty();
    r.register(simple("A", "1.0"), vec!["GameData/A/link.dll".into()], false).unwrap();
    std::fs::create_dir_all(d.join("GameData/A")).unwrap();
    std::os::unix::fs::symlink(d.join("nowhere"), d.join("GameData/A/link.dll")).unwrap();
    assert!(matches!(r.deregister("A", &d), Err(Error::Conflict(_))));
    std::fs::remove_file(d.join("GameData/A/link.dll")).unwrap();
    r.deregister("A", &d).unwrap();
    cleanup(&d);
}

#[test]
fn scan_dlls_survives_symlink_loops() {
    let d = tmp("linkloop");
    touch(&d, "GameData/A/Foo.dll");
    // Two links back to GameData: without a loop guard this is 2^40 folders.
    std::os::unix::fs::symlink(d.join("GameData"), d.join("GameData/A/up1")).unwrap();
    std::os::unix::fs::symlink(d.join("GameData"), d.join("GameData/A/up2")).unwrap();
    let start = std::time::Instant::now();
    let found = find_dlls(&d);
    assert!(start.elapsed().as_secs() < 5);
    assert_eq!(found, vec!["GameData/A/Foo.dll"]);
    cleanup(&d);
}

#[test]
fn save_with_index_writes_modpack_and_history() {
    let d = tmp("saveidx");
    let mut r = Registry::empty();
    let m = simple("Alpha", "1.0");
    let mut modules: HashMap<String, Vec<Arc<Module>>> = HashMap::new();
    modules.insert("Alpha".into(), vec![Arc::new(m.clone())]);
    let index = RepoIndex { modules, ..Default::default() };
    r.register(m, vec!["GameData/Alpha/a.cfg".into()], false).unwrap();
    r.register(simple("Auto", "1.0"), vec![], true).unwrap();
    r.save(&inst(&d), Some(&index)).unwrap();
    let latest = std::fs::read(d.join("CKAN/installed-Test KSP.ckan")).unwrap();
    let pack = Module::from_json(&latest).unwrap();
    assert!(pack.is_metapackage());
    let deps: Vec<String> = pack.depends.iter().map(|x| format!("{x:?}")).collect();
    assert_eq!(deps.len(), 1, "{deps:?}");
    assert!(deps[0].contains("Alpha"));
    let hist: Vec<String> = std::fs::read_dir(d.join("CKAN/history"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(hist.len(), 1);
    assert!(hist[0].starts_with("installed-Test KSP-") && hist[0].ends_with(".ckan"), "{hist:?}");
    assert_eq!(std::fs::read(d.join("CKAN/history").join(&hist[0])).unwrap(), latest);
    cleanup(&d);
}

/// Every real module survives the registry path: raw JSON -> source_module -> registry.json
/// text -> back, with no change (HANGAR_TEST_META).
#[test]
fn real_meta_every_module_as_source_module() {
    use std::io::Read as _;
    let Ok(p) = std::env::var("HANGAR_TEST_META") else { return };
    let f = std::fs::File::open(p).unwrap();
    let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(f));
    let (mut ok, mut bad) = (0usize, Vec::new());
    for e in ar.entries().unwrap() {
        let mut e = e.unwrap();
        let path = e.path().unwrap().to_string_lossy().into_owned();
        if !path.ends_with(".ckan") {
            continue;
        }
        let mut buf = Vec::new();
        e.read_to_end(&mut buf).unwrap();
        // Only modules CKAN itself accepts can be installed.
        if Module::from_json(&buf).is_err() {
            continue;
        }
        let raw: Value = serde_json::from_slice(&buf).unwrap();
        let mut im = serde_json::json!({ "install_time": "2025-01-01T00:00:00Z", "source_module": raw });
        prepare_installed_module(&mut im);
        match serde_json::from_value::<InstalledModule>(im) {
            Ok(m) => {
                let once = serde_json::to_value(&m).unwrap();
                let mut again = once.clone();
                prepare_installed_module(&mut again);
                let back: InstalledModule = serde_json::from_value(again).unwrap();
                assert_eq!(serde_json::to_value(&back).unwrap(), once, "{path}");
                ok += 1;
            }
            Err(e) => bad.push(format!("{path}: {e}")),
        }
    }
    assert!(ok > 30_000, "{ok}");
    assert!(bad.is_empty(), "{} failed, first: {:?}", bad.len(), bad.first());
}
