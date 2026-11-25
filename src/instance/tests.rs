use super::*;

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("hangar-inst-test-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn fake(tag: &str, dlc: &[(&str, &str)]) -> Instance {
    let d = tmp(tag);
    create_fake(&d, &GameVersion::new(1, 12, 5, 3190), dlc).unwrap();
    Instance::open("Test KSP", &d, &HashMap::new()).unwrap()
}

#[test]
fn fake_open_and_version() {
    let i = fake("open", &[]);
    assert_eq!(i.version, Some(GameVersion::new(1, 12, 5, 3190)));
    assert!(i.game_dir.join("Ships/@thumbs/VAB").is_dir());
    assert!(i.game_dir.join("saves/training").is_dir());
    assert!(i.game_dir.join("Missions").is_dir());
    // Not empty: a second fake must fail.
    assert!(create_fake(&i.game_dir, &GameVersion::new(1, 12, 5, 3190), &[]).is_err());
    assert!(Instance::open("x", &i.game_dir.join("GameData"), &HashMap::new()).is_err());
    // No binary in a fake: launch fails without starting anything.
    assert!(matches!(i.launch(), Err(Error::NotFound(_))));
    let _ = std::fs::remove_dir_all(&i.game_dir);
}

#[test]
fn version_detection_rules() {
    let d = tmp("detect");
    std::fs::create_dir_all(d.join("GameData")).unwrap();
    // Leading zeros and a header line, like the real Steam file.
    std::fs::write(d.join("buildID.txt"), "[config]\r\nbuild id = 03190\r\n").unwrap();
    assert!(is_valid_game_dir(&d));
    assert_eq!(detect_version(&d, &HashMap::new()), Some(GameVersion::new(1, 12, 5, 3190)));
    // The given map wins over the embedded one.
    let mut b = HashMap::new();
    b.insert("3190".to_string(), GameVersion::new(9, 9, 9, 3190));
    assert_eq!(detect_version(&d, &b), Some(GameVersion::new(9, 9, 9, 3190)));
    // Two files: the max wins.
    std::fs::write(d.join("buildID64.txt"), "build id = 1028").unwrap();
    assert_eq!(detect_version(&d, &HashMap::new()), Some(GameVersion::new(1, 12, 5, 3190)));
    // Unknown build: readme fallback.
    std::fs::write(d.join("buildID.txt"), "build id = 999999").unwrap();
    std::fs::write(d.join("buildID64.txt"), "build id = 999998").unwrap();
    std::fs::write(d.join("readme.txt"), "\u{feff}KSP\nversion 1.11.2\n").unwrap();
    assert_eq!(detect_version(&d, &HashMap::new()), Some(GameVersion::new(1, 11, 2, -1)));
    std::fs::remove_file(d.join("readme.txt")).unwrap();
    assert_eq!(detect_version(&d, &HashMap::new()), None);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn builds_parsing() {
    let b = parse_builds(r#"{"builds":{"3190":"1.12.5.3190","bad":"x.y","1":"1.0."}}"#);
    assert_eq!(b.len(), 2);
    assert_eq!(b.get("1"), Some(&GameVersion::new(1, 0, -1, -1)));
    assert!(parse_builds("nope").is_empty());
    assert!(embedded_builds().len() > 100);
    assert_eq!(embedded_builds().get("3190"), Some(&GameVersion::new(1, 12, 5, 3190)));
}

#[test]
fn real_install_read_only() {
    let d = Path::new("/Users/silaspusateri/Library/Application Support/Steam/steamapps/common/Kerbal Space Program");
    if !d.is_dir() {
        return;
    }
    assert!(is_valid_game_dir(d));
    assert_eq!(detect_version(d, &HashMap::new()).map(|v| v.to_string()), Some("1.12.5.3190".into()));
    assert!(find_installs().iter().any(|p| p == d));
}

#[test]
fn compat_file() {
    let i = fake("compat", &[]);
    assert!(i.read_compat_file().is_none());
    // CKAN: no file means "stored version unknown", which differs from the detected one.
    assert!(i.compat_from_other_game_version());
    let v = [GameVersion::new(1, 11, -1, -1), GameVersion::new(1, 12, -1, -1), GameVersion::new(1, 11, -1, -1)];
    i.set_compatible_versions(&v).unwrap();
    let text = std::fs::read_to_string(i.ckan_dir().join(COMPAT_FILE)).unwrap();
    assert_eq!(text, r#"{"GameVersionWhenWritten":"1.12.5.3190","Versions":["1.12","1.11"]}"#);
    assert_eq!(i.compatible_versions(), vec![GameVersion::new(1, 12, -1, -1), GameVersion::new(1, 11, -1, -1)]);
    assert!(!i.compat_from_other_game_version());
    // Legacy keys, trailing dot.
    std::fs::write(
        i.ckan_dir().join(COMPAT_FILE),
        r#"{"VersionOfKspWhenWritten":"1.10.1","CompatibleKspVersions":["1.9.","1.8"]}"#,
    )
    .unwrap();
    assert_eq!(i.compatible_versions(), vec![GameVersion::new(1, 9, -1, -1), GameVersion::new(1, 8, -1, -1)]);
    assert!(i.compat_from_other_game_version());
    // A file written with a null game version also asks again.
    std::fs::write(i.ckan_dir().join(COMPAT_FILE), r#"{"GameVersionWhenWritten":null,"Versions":[]}"#).unwrap();
    assert!(i.compat_from_other_game_version());
    assert!(i.compatible_versions().is_empty());
    // No detected version and no file: nothing to compare, no prompt.
    let none = Instance { version: None, ..i.clone() };
    std::fs::remove_file(i.ckan_dir().join(COMPAT_FILE)).unwrap();
    assert!(!none.compat_from_other_game_version());
    let _ = std::fs::remove_dir_all(&i.game_dir);
}

#[test]
fn filters_and_playtime() {
    let i = fake("files", &[]);
    assert!(i.install_filters().is_empty());
    i.set_install_filters(&["/Ships/".into(), "Thumbs.db".into()]).unwrap();
    assert_eq!(i.install_filters(), vec!["/Ships/", "Thumbs.db"]);
    assert_eq!(i.playtime(), Duration::ZERO);
    std::fs::write(i.ckan_dir().join(PLAYTIME_FILE), r#"{"time": "1.02:03:04.5000000"}"#).unwrap();
    i.add_playtime(Duration::from_secs(3600)).unwrap();
    let text = std::fs::read_to_string(i.ckan_dir().join(PLAYTIME_FILE)).unwrap();
    assert!(text.contains(r#""time": "1.03:03:04.5000000""#), "{text}");
    let _ = std::fs::remove_dir_all(&i.game_dir);
}

#[test]
fn dlc_detection() {
    let i = fake("dlc", &[("MakingHistory-DLC", "1.0"), ("BreakingGround", "1.7.1")]);
    let mut d = i.detect_dlc();
    d.sort();
    assert_eq!(
        d,
        vec![
            ("BreakingGround-DLC".to_string(), Some("1.7.1".to_string())),
            ("MakingHistory-DLC".to_string(), Some("1.0.0".to_string())),
        ]
    );
    // A dir without readme is not installed; a stub overrides; an empty stub has no version.
    std::fs::remove_file(i.game_dir.join("GameData/SquadExpansion/Serenity/readme.txt")).unwrap();
    std::fs::create_dir_all(i.ckan_dir().join("dlc")).unwrap();
    std::fs::write(i.ckan_dir().join("dlc/MakingHistory.dlc"), " 1.9.1\n").unwrap();
    std::fs::write(i.ckan_dir().join("dlc/Other.dlc"), "").unwrap();
    let mut d = i.detect_dlc();
    d.sort();
    assert_eq!(
        d,
        vec![("MakingHistory-DLC".to_string(), Some("1.9.1".to_string())), ("Other-DLC".to_string(), None)]
    );
    assert!(create_fake(&tmp("dlcbad"), &GameVersion::new(1, 12, 5, 3190), &[("Nope", "1")]).is_err());
    let _ = std::fs::remove_dir_all(&i.game_dir);
    let _ = std::fs::remove_dir_all(tmp("dlcbad"));
}

#[test]
fn history_and_unmanaged() {
    let i = fake("hist", &[]);
    let h = i.ckan_dir().join("history");
    std::fs::create_dir_all(&h).unwrap();
    for n in ["installed-a-2026-01-01_00-00-00.ckan", "installed-a-2026-02-01_00-00-00.ckan"] {
        std::fs::write(h.join(n), "{}").unwrap();
        std::thread::sleep(Duration::from_millis(20));
    }
    std::fs::write(h.join("notes.txt"), "x").unwrap();
    let names: Vec<String> =
        i.history().iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
    assert_eq!(names, ["installed-a-2026-02-01_00-00-00.ckan", "installed-a-2026-01-01_00-00-00.ckan"]);

    std::fs::create_dir_all(i.game_dir.join("GameData/Mod")).unwrap();
    std::fs::write(i.game_dir.join("GameData/Mod/a.dll"), "").unwrap();
    std::fs::write(i.game_dir.join("GameData/Mod/b.cfg"), "").unwrap();
    std::fs::write(i.game_dir.join("GameData/Squad/stock.cfg"), "").unwrap();
    std::fs::write(i.game_dir.join("CKAN/registry.json"), "").unwrap();
    let un = i.unmanaged_files(&|p| p == "GameData/Mod/b.cfg");
    assert_eq!(un, ["GameData/Mod/a.dll", "buildID.txt", "buildID64.txt", "readme.txt"]);
    let _ = std::fs::remove_dir_all(&i.game_dir);
}

#[test]
fn steam_discovery() {
    let root = tmp("steam");
    let lib2 = root.join("lib2");
    std::fs::create_dir_all(root.join("config")).unwrap();
    std::fs::create_dir_all(lib2.join("steamapps")).unwrap();
    std::fs::write(
        root.join("config/libraryfolders.vdf"),
        format!("\"libraryfolders\"\n{{\n\t\"1\"\n\t{{\n\t\t\"path\"\t\t\"{}\"\n\t}}\n}}\n", lib2.display()),
    )
    .unwrap();
    std::fs::write(lib2.join("steamapps/appmanifest_220200.acf"), "\"AppState\"\n{\n\t\"installdir\"\t\t\"KSP Game\"\n}\n").unwrap();
    let game = lib2.join("steamapps/common/KSP Game");
    create_fake(&game, &GameVersion::new(1, 12, 5, 3190), &[]).unwrap();
    let extra = root.join("not-a-game");
    std::fs::create_dir_all(&extra).unwrap();
    assert_eq!(find_installs_in(&root, &[extra]), vec![game]);
    let _ = std::fs::remove_dir_all(&root);
}
