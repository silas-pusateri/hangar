//! Tests for extraction, removal and conflicts, on temp dirs only.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;
use crate::registry::{FileEntry, InstalledModule};

static SEQ: AtomicU64 = AtomicU64::new(0);

/// Fresh empty dir under the system temp dir.
pub(crate) fn temp_dir(tag: &str) -> PathBuf {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!("hangar-install-t-{tag}-{}-{n}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

/// Write a zip: `(name, None)` is a dir entry, `(name, Some(bytes))` a file.
pub(crate) fn make_zip(path: &Path, entries: &[(&str, Option<&[u8]>)]) {
    let f = fs::File::create(path).unwrap();
    let mut zw = zip::ZipWriter::new(f);
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, data) in entries {
        match data {
            None => zw.add_directory(*name, opts).unwrap(),
            Some(d) => {
                zw.start_file(*name, opts).unwrap();
                zw.write_all(d).unwrap();
            }
        }
    }
    zw.finish().unwrap();
}

/// A fake game dir with the stock layout.
pub(crate) fn fake_game(tag: &str) -> Instance {
    let g = temp_dir(tag);
    fs::write(g.join("buildID64.txt"), "build id = 03190\n").unwrap();
    for d in ["GameData/Squad", "Ships/VAB", "Ships/SPH", "Ships/@thumbs/VAB", "saves/scenarios", "Missions", "CKAN"] {
        fs::create_dir_all(g.join(d)).unwrap();
    }
    Instance { name: "test".into(), game_dir: g, version: None }
}

fn empty_registry() -> Registry {
    serde_json::from_str("{}").unwrap()
}

fn module(json: &str) -> Module {
    Module::from_json(json.as_bytes()).unwrap()
}

/// Bytes with CR, LF and NUL in them (GH #205: write binary, not text).
fn binary(n: usize) -> Vec<u8> {
    (0..n).map(|i| [b'\r', b'\n', 0, 0xFF, (i % 251) as u8][i % 5]).collect()
}

const DOGE: &str = r#"{"spec_version":"v1.10","identifier":"DogeCoinFlag","version":"1.01",
    "download":"https://kerbalstuff.com/mod/269/Dogecoin%20Flag/download/1.01",
    "install":[
      {"file":"DogeCoinFlag-1.01/GameData/DogeCoinFlag","install_to":"GameData",
       "filter":["Thumbs.db","README.md"],"filter_regexp":"\\.bak$"},
      {"file":"DogeCoinFlag-1.01/META.ckan","install_to":"GameData"}]}"#;

fn doge_zip(dir: &Path, png: &[u8]) -> PathBuf {
    let z = dir.join("doge.zip");
    make_zip(
        &z,
        &[
            ("DogeCoinFlag-1.01/", None),
            ("DogeCoinFlag-1.01/GameData/", None),
            ("DogeCoinFlag-1.01/GameData/DogeCoinFlag/", None),
            ("DogeCoinFlag-1.01/GameData/DogeCoinFlag/Flags/", None),
            ("DogeCoinFlag-1.01/GameData/DogeCoinFlag/Flags/dogecoin.png", Some(png)),
            ("DogeCoinFlag-1.01/META.ckan", Some(b"{}")),
            ("DogeCoinFlag-1.01/README.md", Some(b"readme")),
        ],
    );
    z
}

/// Register by hand (the registry module may not be ready in parallel builds).
fn register(reg: &mut Registry, m: &Module, files: &[String]) {
    for f in files {
        reg.installed_files.insert(f.clone(), m.identifier.clone());
    }
    reg.installed_modules.insert(
        m.identifier.clone(),
        InstalledModule {
            install_time: "2026-01-01T00:00:00.0000000Z".into(),
            source_module: m.clone(),
            auto_installed: false,
            installed_files: files.iter().map(|f| (f.clone(), FileEntry {})).collect::<BTreeMap<_, _>>(),
        },
    );
}

fn deregister(reg: &mut Registry, id: &str) {
    reg.installed_modules.remove(id);
    reg.installed_files.retain(|_, o| o != id);
}

fn uninstall(inst: &Instance, reg: &mut Registry, id: &str) -> Vec<String> {
    let mut tx = FileTx::begin(&inst.game_dir).unwrap();
    let del = delete_module_files(inst, reg, id, &mut tx).unwrap();
    deregister(reg, id);
    let cfg = remove_empty_dirs(inst, reg, &del, &mut tx);
    tx.commit().unwrap();
    cfg
}

#[test]
fn zip_names_in_order() {
    let d = temp_dir("names");
    let z = doge_zip(&d, b"x");
    let names = zip_entry_names(&z).unwrap();
    assert_eq!(names.len(), 7);
    assert_eq!(names[0], "DogeCoinFlag-1.01/");
    assert_eq!(names[6], "DogeCoinFlag-1.01/README.md");
    fs::write(d.join("bad.zip"), b"not a zip").unwrap();
    assert!(zip_entry_names(&d.join("bad.zip")).is_err());
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn extract_doge_binary_and_dirs() {
    let inst = fake_game("doge");
    let png = binary(52043);
    let z = doge_zip(&inst.game_dir, &png);
    let reg = empty_registry();
    let m = module(DOGE);
    let mut tx = FileTx::begin(&inst.game_dir).unwrap();
    let written = extract_module(&inst, &reg, &m, &z, &[], &mut tx).unwrap();
    tx.commit().unwrap();
    assert_eq!(
        written,
        vec!["GameData/DogeCoinFlag", "GameData/DogeCoinFlag/Flags", "GameData/DogeCoinFlag/Flags/dogecoin.png"]
    );
    let out = fs::read(inst.game_dir.join("GameData/DogeCoinFlag/Flags/dogecoin.png")).unwrap();
    assert_eq!(out.len(), 52043);
    assert_eq!(out, png);
    assert!(!inst.game_dir.join("GameData/META.ckan").exists());
    assert!(!inst.game_dir.join("GameData/README.md").exists());
    fs::remove_dir_all(&inst.game_dir).unwrap();
}

#[test]
fn dont_overwrite_208_and_rollback() {
    let inst = fake_game("ow");
    let z = doge_zip(&inst.game_dir, b"new");
    let flags = inst.game_dir.join("GameData/DogeCoinFlag/Flags");
    fs::create_dir_all(&flags).unwrap();
    fs::write(flags.join("dogecoin.png"), b"mine").unwrap();
    let reg = empty_registry();
    let m = module(DOGE);
    let mut tx = FileTx::begin(&inst.game_dir).unwrap();
    let err = extract_module(&inst, &reg, &m, &z, &[], &mut tx).unwrap_err();
    assert!(matches!(err, Error::Conflict(_)), "{err}");
    assert!(err.to_string().contains("GameData/DogeCoinFlag/Flags/dogecoin.png"), "{err}");
    tx.rollback().unwrap();
    assert_eq!(fs::read(flags.join("dogecoin.png")).unwrap(), b"mine");

    // Conflicts list: unowned existing file only.
    let files = module_files(&m, &zip_entry_names(&z).unwrap(), &[]).unwrap();
    assert_eq!(existing_conflicts(&inst, &reg, &files), vec!["GameData/DogeCoinFlag/Flags/dogecoin.png"]);
    let mut reg2 = empty_registry();
    register(&mut reg2, &module(r#"{"identifier":"Other","version":"1","download":"https://x/"}"#), &[
        "GameData/DogeCoinFlag/Flags/dogecoin.png".into(),
    ]);
    assert!(existing_conflicts(&inst, &reg2, &files).is_empty());
    fs::remove_dir_all(&inst.game_dir).unwrap();
}

#[test]
fn no_dir_entries_creates_parents_and_filters() {
    let inst = fake_game("nodir");
    let z = inst.game_dir.join("m.zip");
    make_zip(&z, &[("Foo/Plugins/Foo.dll", Some(b"dll")), ("Foo/MiniAVC.dll", Some(b"avc"))]);
    let m = module(r#"{"identifier":"Foo","version":"1","download":"https://x/"}"#);
    let mut tx = FileTx::begin(&inst.game_dir).unwrap();
    let written = extract_module(&inst, &empty_registry(), &m, &z, &["/MiniAVC.dll".into()], &mut tx).unwrap();
    tx.commit().unwrap();
    // Implicit parent dirs are created but not recorded.
    assert_eq!(written, vec!["GameData/Foo/Plugins/Foo.dll"]);
    assert!(!inst.game_dir.join("GameData/Foo/MiniAVC.dll").exists());
    fs::remove_dir_all(&inst.game_dir).unwrap();
}

#[test]
fn ships_no_makedir() {
    let inst = fake_game("ships");
    let z = inst.game_dir.join("s.zip");
    make_zip(&z, &[("Craft/", None), ("Craft/a.craft", Some(b"c"))]);
    let m = module(
        r#"{"identifier":"Crafts","version":"1","download":"https://x/",
        "install":[{"find":"Craft","install_to":"Ships/VAB"}]}"#,
    );
    let mut tx = FileTx::begin(&inst.game_dir).unwrap();
    // The dir entry "Craft/" -> Ships/VAB/Craft is skipped (no makedir), so its file fails.
    let err = extract_module(&inst, &empty_registry(), &m, &z, &[], &mut tx).unwrap_err();
    assert!(matches!(err, Error::NotFound(_)), "{err}");
    tx.rollback().unwrap();
    assert!(!inst.game_dir.join("Ships/VAB/Craft").exists());

    let m = module(
        r#"{"identifier":"Crafts","version":"1","download":"https://x/",
        "install":[{"file":"Craft/a.craft","install_to":"Ships/VAB"}]}"#,
    );
    let mut tx = FileTx::begin(&inst.game_dir).unwrap();
    let written = extract_module(&inst, &empty_registry(), &m, &z, &[], &mut tx).unwrap();
    tx.commit().unwrap();
    assert_eq!(written, vec!["Ships/VAB/a.craft"]);
    fs::remove_dir_all(&inst.game_dir).unwrap();
}

#[test]
fn corrupt_zip_is_an_error() {
    let inst = fake_game("corrupt");
    let z = inst.game_dir.join("c.zip");
    fs::write(&z, b"PK\x03\x04garbage").unwrap();
    let mut tx = FileTx::begin(&inst.game_dir).unwrap();
    let err = extract_module(&inst, &empty_registry(), &module(DOGE), &z, &[], &mut tx).unwrap_err();
    assert!(err.to_string().contains("corrupt"), "{err}");
    fs::remove_dir_all(&inst.game_dir).unwrap();
}

#[test]
fn install_module_dlc_and_already_installed() {
    let inst = fake_game("dlc");
    let mut reg = empty_registry();
    let mut tx = FileTx::begin(&inst.game_dir).unwrap();
    let dlc = module(r#"{"identifier":"MakingHistory-DLC","version":"1.0","kind":"dlc"}"#);
    assert!(install_module(&inst, &mut reg, &dlc, None, false, &[], &mut tx).is_err());
    let m = module(DOGE);
    register(&mut reg, &m, &[]);
    assert!(install_module(&inst, &mut reg, &m, None, false, &[], &mut tx).unwrap().is_empty());
    let other = module(r#"{"identifier":"Other","version":"1","download":"https://x/"}"#);
    let err = install_module(&inst, &mut reg, &other, None, false, &[], &mut tx).unwrap_err();
    assert!(matches!(err, Error::NotFound(_)));
    tx.commit().unwrap();
    fs::remove_dir_all(&inst.game_dir).unwrap();
}

#[test]
fn uninstall_removes_files_and_empty_dirs() {
    let inst = fake_game("un");
    let g = inst.game_dir.clone();
    let z = doge_zip(&g, b"png");
    let mut reg = empty_registry();
    let m = module(DOGE);
    let mut tx = FileTx::begin(&g).unwrap();
    let written = extract_module(&inst, &reg, &m, &z, &[], &mut tx).unwrap();
    tx.commit().unwrap();
    register(&mut reg, &m, &written);

    let cfg = uninstall(&inst, &mut reg, "DogeCoinFlag");
    assert!(cfg.is_empty());
    assert!(!g.join("GameData/DogeCoinFlag").exists());
    assert!(g.join("GameData/Squad").is_dir());
    assert!(g.join("GameData").is_dir());
    fs::remove_dir_all(&g).unwrap();
}

#[test]
fn uninstall_keeps_dirs_with_other_files() {
    let inst = fake_game("keep");
    let g = inst.game_dir.clone();
    let mut reg = empty_registry();
    let a = module(r#"{"identifier":"ModA","version":"1","download":"https://x/"}"#);
    let b = module(r#"{"identifier":"ModB","version":"1","download":"https://x/"}"#);
    for (rel, data) in [
        ("GameData/Shared/a.cfg", "a"),
        ("GameData/Shared/b.cfg", "b"),
        ("GameData/ModA/Parts/p.cfg", "p"),
        ("GameData/ModA/Parts/user.cfg", "user"),
        ("GameData/ModA/Parts/@thumbs/t.png", "t"),
        ("GameData/ModA/Only/a2.cfg", "x"),
        ("GameData/ModA/Only/@thumbs/t.png", "t"),
    ] {
        fs::create_dir_all(g.join(rel).parent().unwrap()).unwrap();
        fs::write(g.join(rel), data).unwrap();
    }
    register(
        &mut reg,
        &a,
        &[
            "GameData/Shared/a.cfg".into(),
            "GameData/ModA/Parts/p.cfg".into(),
            "GameData/ModA/Only/a2.cfg".into(),
            "GameData/Missing/gone.cfg".into(),
        ],
    );
    register(&mut reg, &b, &["GameData/Shared/b.cfg".into()]);

    let cfg = uninstall(&inst, &mut reg, "ModA");
    // Other mod's file keeps its dir, not offered for deletion.
    assert!(g.join("GameData/Shared/b.cfg").exists());
    assert!(!g.join("GameData/Shared/a.cfg").exists());
    // Unmanaged file keeps the dir, which is offered as config-only.
    assert!(g.join("GameData/ModA/Parts/user.cfg").exists());
    assert!(!g.join("GameData/ModA/Parts/@thumbs").exists(), "@thumbs auto-removed");
    assert!(!g.join("GameData/ModA/Only").exists(), "only @thumbs left: removed");
    // CKAN also offers the parent: its only other content is the offered dir and user files.
    assert_eq!(cfg, vec!["GameData/ModA/Parts".to_string(), "GameData/ModA".to_string()]);
    assert!(g.join("GameData/ModA").is_dir());
    assert!(reg.installed("ModA").is_none());
    fs::remove_dir_all(&g).unwrap();
}

#[test]
fn uninstall_craft_files_keeps_ships_dirs() {
    let inst = fake_game("craft");
    let g = inst.game_dir.clone();
    let z = doge_zip(&g, b"png");
    let flag = "DogeCoinFlag-1.01/GameData/DogeCoinFlag/Flags/dogecoin.png";
    let m = module(&format!(
        r#"{{"identifier":"CraftsMod","version":"1.0","download":"https://github.com/",
        "install":[{{"file":"{flag}","install_to":"Ships/VAB","as":"mycraft.craft"}},
                   {{"file":"{flag}","install_to":"Ships/SPH","as":"mycraft.craft"}}]}}"#
    ));
    let mut reg = empty_registry();
    let mut tx = FileTx::begin(&g).unwrap();
    let written = extract_module(&inst, &reg, &m, &z, &[], &mut tx).unwrap();
    tx.commit().unwrap();
    assert_eq!(written, vec!["Ships/VAB/mycraft.craft", "Ships/SPH/mycraft.craft"]);
    register(&mut reg, &m, &written);
    let cfg = uninstall(&inst, &mut reg, "CraftsMod");
    assert!(cfg.is_empty());
    assert!(g.join("Ships/VAB").is_dir() && g.join("Ships/SPH").is_dir());
    assert!(!g.join("Ships/VAB/mycraft.craft").exists());
    fs::remove_dir_all(&g).unwrap();
}

#[test]
fn mod_in_stock_folder_not_removed() {
    let inst = fake_game("stock");
    let g = inst.game_dir.clone();
    fs::write(g.join("GameData/Squad/patch.cfg"), "x").unwrap();
    let mut reg = empty_registry();
    let m = module(r#"{"identifier":"SquadPatch","version":"1","download":"https://x/"}"#);
    register(&mut reg, &m, &["GameData/Squad/patch.cfg".into()]);
    let cfg = uninstall(&inst, &mut reg, "SquadPatch");
    assert!(cfg.is_empty());
    assert!(g.join("GameData/Squad").is_dir());
    fs::remove_dir_all(&g).unwrap();
}

#[test]
fn parent_dirs_rules() {
    // ModuleInstallerDirTest: game root and GameData give nothing; slash variants dedupe.
    assert!(with_parents(&[String::new()]).is_empty());
    assert!(with_parents(&["GameData".into()]).is_empty());
    let v = with_parents(&["GameData/DogeCoinFlag".into(), "GameData\\DogeCoinFlag".into(), "GameData/DogeCoinFlag/".into()]);
    assert_eq!(v, vec!["GameData/DogeCoinFlag"]);
    let v = with_parents(&["GameData/Squad/Parts/X".into(), "saves/scenarios/A".into()]);
    assert_eq!(v, vec!["GameData/Squad/Parts/X", "GameData/Squad/Parts", "saves/scenarios/A", "saves"]);
}

/// A case-insensitive `find` can register `Ships/vab/...`; on a case-insensitive volume that is
/// the stock Ships/VAB dir, which must survive the uninstall even when empty.
#[test]
fn reserved_and_stock_dirs_ignore_case() {
    assert_eq!(with_parents(&["gamedata/squad/Foo".into()]), vec!["gamedata/squad/Foo"]);
    assert!(with_parents(&["ships/vab".into(), "SHIPS/@THUMBS/sph".into()]).is_empty());

    let inst = fake_game("case");
    let g = inst.game_dir.clone();
    let z = g.join("c.zip");
    make_zip(&z, &[("Pack/vab/a.craft", Some(b"c"))]);
    let m = module(
        r#"{"identifier":"Crafts","version":"1","download":"https://x/",
        "install":[{"find":"VAB","install_to":"Ships"}]}"#,
    );
    let mut reg = empty_registry();
    let mut tx = FileTx::begin(&g).unwrap();
    let written = extract_module(&inst, &reg, &m, &z, &[], &mut tx).unwrap();
    tx.commit().unwrap();
    assert_eq!(written, vec!["Ships/vab/a.craft"]);
    register(&mut reg, &m, &written);
    uninstall(&inst, &mut reg, "Crafts");
    assert!(!g.join("Ships/vab/a.craft").exists());
    assert!(g.join("Ships/VAB").is_dir(), "stock dir removed");
    fs::remove_dir_all(&g).unwrap();
}

// ModuleInstallerTests.GroupFilesByRemovable_WithFiles_CorrectOutput
#[test]
fn group_files_by_removable_cases() {
    let root = "GameData/SomeMod/Parts";
    let p = |s: &str| format!("{root}/{s}");
    let all = |v: &[&str]| v.iter().map(|s| p(s)).collect::<Vec<_>>();
    type Case<'a> = (&'a [&'a str], &'a [&'a str], &'a [&'a str], &'a [&'a str]);
    let cases: [Case; 5] = [
        (&[], &[], &[], &[]),
        (
            &[],
            &["userfile.cfg", "userfile2.cfg", "@thumbs", "@thumbs/part1.png", "@thumbs/part3.png", "@thumbs/part4.png"],
            &["@thumbs/part1.png", "@thumbs/part3.png", "@thumbs/part4.png", "@thumbs"],
            &["userfile2.cfg", "userfile.cfg"],
        ),
        (&[], &["userfile.cfg", "userfile2.cfg"], &[], &["userfile2.cfg", "userfile.cfg"]),
        (
            &[],
            &["@thumbs", "@thumbs/part1.png", "@thumbs/part3.png", "@thumbs/part4.png"],
            &["@thumbs/part1.png", "@thumbs/part3.png", "@thumbs/part4.png", "@thumbs"],
            &[],
        ),
        (
            &["userfile2.cfg", "@thumbs/part1.png"],
            &["userfile.cfg", "userfile2.cfg", "@thumbs", "@thumbs/part1.png", "@thumbs/part3.png", "@thumbs/part4.png"],
            &["@thumbs/part3.png", "@thumbs/part4.png", "@thumbs"],
            &["@thumbs/part1.png", "userfile2.cfg", "userfile.cfg"],
        ),
    ];
    for (owned, paths, want_rm, want_keep) in cases {
        let mut reg = empty_registry();
        for o in owned {
            reg.installed_files.insert(p(o), "otherMod".into());
        }
        let (rm, keep) = group_by_removable(root, &reg, &HashSet::new(), &all(paths));
        assert_eq!(rm, all(want_rm));
        assert_eq!(keep, all(want_keep));
    }
}

// Live check: download ModuleManager from the real metadata and install it into a fake game.
// Run with: HANGAR_TEST_META=<master.tar.gz> cargo test install::tests::live -- --ignored
#[test]
#[ignore]
fn live_download_and_install_module_manager() {
    let Some(meta) = std::env::var_os("HANGAR_TEST_META") else { return };
    let gz = flate2::read::GzDecoder::new(fs::File::open(meta).unwrap());
    let mut ar = tar::Archive::new(gz);
    let mut best: Option<Module> = None;
    for e in ar.entries().unwrap() {
        let mut e = e.unwrap();
        let path = e.path().unwrap().to_string_lossy().into_owned();
        if !path.contains("/ModuleManager/") || !path.ends_with(".ckan") {
            continue;
        }
        let mut s = String::new();
        std::io::Read::read_to_string(&mut e, &mut s).unwrap();
        let Ok(m) = Module::from_json(s.as_bytes()) else { continue };
        // Newest by release_date (ModuleVersion compare may still be provisional).
        if best.as_ref().is_none_or(|b| m.release_date > b.release_date) {
            best = Some(m);
        }
    }
    let m = best.expect("ModuleManager in metadata");
    let work = temp_dir("live");
    let cache = DownloadCache::new(work.join("cache"));
    let agent = crate::repo::http_agent();
    let progress = |_: &str, _: Option<f32>| {};
    let path = cache.download(&m, &agent, &|_| None, &progress).unwrap();
    assert_eq!(cache.get(&m), Some(path.clone()));
    assert!(path.file_name().unwrap().to_string_lossy().starts_with(&cache::url_hash(&m.download[0])));

    let inst = fake_game("live-game");
    let mut tx = FileTx::begin(&inst.game_dir).unwrap();
    let written = extract_module(&inst, &empty_registry(), &m, &path, &[], &mut tx).unwrap();
    tx.commit().unwrap();
    eprintln!("installed {m}: {written:?}");
    assert!(written.iter().any(|w| w.starts_with("GameData/ModuleManager") && w.ends_with(".dll")));
    for w in &written {
        assert!(inst.game_dir.join(w).exists());
    }
    fs::remove_dir_all(&work).unwrap();
    fs::remove_dir_all(&inst.game_dir).unwrap();
}
