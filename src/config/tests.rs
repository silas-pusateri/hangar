use super::*;

#[test]
fn ckan_config_shapes() {
    let j = br#"{
      "AutoStartInstance": "Main", "DownloadCacheDir": "", "CacheSizeLimit": -1, "RefreshRate": 0,
      "Language": "en-US",
      "KspInstances": [ {"Name": "Main", "Path": "/g"}, {"Name": "Two", "Path": "/h", "Game": "KSP2"}, {"Name": null} ],
      "AuthTokens": { "api.github.com": "ghp_x", "bad": 1 },
      "GlobalInstallFilters": ["/Ships/"],
      "PreferredHosts": ["github.com", null],
      "Hangar": { "hide_v": true, "stability": "nonsense" }
    }"#;
    let c = parse_config(j).unwrap();
    assert_eq!(c.auto_start_instance.as_deref(), Some("Main"));
    assert_eq!(c.download_cache_dir, None);
    assert_eq!(c.cache_size_limit, None);
    assert_eq!(c.refresh_rate, None);
    assert_eq!(c.game_instances.len(), 2);
    assert_eq!(c.game_instances[0].game, "KSP");
    assert_eq!(c.game_instances[1].game, "KSP2");
    assert_eq!(c.auth_tokens.len(), 1);
    assert_eq!(c.global_install_filters(), vec!["/Ships/"]);
    // A bad Hangar block falls back to defaults, the rest still loads.
    assert_eq!(c.hangar, HangarSettings::default());
    assert!(parse_config(b"[1,2]").is_none());
    assert!(parse_config(b"{bad").is_none());

    // Our own output reads back the same.
    let mut c2 = c.clone();
    c2.hangar.hide_v = true;
    c2.refresh_rate = Some(30);
    let text = serde_json::to_string_pretty(&c2).unwrap();
    assert!(text.contains("\"GameInstances\""));
    assert!(text.contains("\"GlobalInstallFiltersByGame\""));
    assert_eq!(parse_config(text.as_bytes()).unwrap(), c2);
}

#[test]
fn labels_shapes() {
    let j = br#"{"labels":[
        {"name":"All","color":"PaleGreen","module_identifiers":["A","B"]},
        {"name":"Inst","instance_name":"Main","hide":true,"module_identifiers":{"KSP":["C"]}},
        {"name":"Held","color":"255, 255, 176","hold_version":true,"module_identifiers_by_game":{"KSP":["Kopernicus"]},"color2":null},
        {"name":"Nulls","color":null,"module_identifiers":null}
    ]}"#;
    let l = parse_labels(j).unwrap();
    assert_eq!(l.labels.len(), 4);
    // Instance-specific labels sort first.
    assert_eq!(l.labels[0].name, "Inst");
    assert!(l.labels[0].contains("C"));
    let all = l.labels.iter().find(|x| x.name == "All").unwrap();
    assert!(all.contains("A"));
    assert_eq!(all.module_identifiers_by_game.get("KSP2").map(Vec::len), Some(2));
    assert_eq!(l.held("Main"), HashSet::from(["Kopernicus".to_string()]));
    assert_eq!(l.hidden("Main"), HashSet::from(["C".to_string()]));
    assert!(l.hidden("Other").is_empty());
    assert_eq!(l.labels.iter().find(|x| x.name == "Held").unwrap().rgb(), Some([255, 255, 176]));
    // Written shape: legacy key gone, false flags omitted.
    let text = serde_json::to_string(&l).unwrap();
    assert!(!text.contains("\"module_identifiers\""));
    assert!(!text.contains("notify_on_change"));
    assert_eq!(parse_labels(text.as_bytes()).unwrap(), l);
}

#[test]
fn label_defaults() {
    let d = LabelList::defaults();
    let names: Vec<&str> = d.labels.iter().map(|l| l.name.as_str()).collect();
    assert_eq!(names, ["Favourites", "Hidden", "Held"]);
    assert!(d.labels[1].hide);
    assert!(d.labels[2].hold_version);
    assert!(d.labels.iter().all(|l| l.rgb().is_some()));
    let mut d2 = d.clone();
    d2.toggle(2, "Foo");
    assert_eq!(d2.held("any"), HashSet::from(["Foo".to_string()]));
    d2.toggle(2, "Foo");
    assert!(d2.held("any").is_empty());
}

/// One test touches HANGAR_HOME so parallel tests in this module do not race on it.
#[test]
fn load_save_in_hangar_home() {
    let home = std::env::temp_dir().join(format!("hangar-config-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    // SAFETY: test-only; HANGAR_HOME points at a fresh temp dir.
    unsafe { std::env::set_var("HANGAR_HOME", &home) };

    // Missing file: import from CKAN's config (CKAN_CONFIG_FILE points at a temp copy, so
    // the real one is never read) and write once.
    let ckan_cfg = home.join("ckan-config.json");
    std::fs::write(
        &ckan_cfg,
        r#"{"KspInstances":[{"Name":"Main","Path":"/g"}],"AuthTokens":{"h":"t"},"CacheSizeLimit":5}"#,
    )
    .unwrap();
    // SAFETY: test-only, as above.
    unsafe { std::env::set_var("CKAN_CONFIG_FILE", &ckan_cfg) };
    let c = Config::load();
    unsafe { std::env::remove_var("CKAN_CONFIG_FILE") };
    assert_eq!(c.game_instances.len(), 1);
    assert_eq!(c.auth_tokens.get("h").map(String::as_str), Some("t"));
    // Cache settings are not shared with CKAN.
    assert_eq!(c.cache_size_limit, None);
    assert!(home.join(CONFIG_FILE).is_file());
    assert_eq!(c.cache_dir(), home.join("downloads"));
    assert_eq!(c.repo_cache_dir(), home.join("repos"));

    let mut c = Config::default();
    c.game_instances.push(InstanceEntry { name: "Main".into(), path: "/g".into(), game: "KSP".into() });
    c.download_cache_dir = Some("cache2".into());
    c.save().unwrap();
    let back = Config::load();
    assert_eq!(back, c);
    assert_eq!(back.cache_dir(), home.join("cache2"));

    // Corrupt file: renamed aside, defaults used.
    std::fs::write(home.join(CONFIG_FILE), "{broken").unwrap();
    assert_eq!(Config::load(), Config::default());
    let aside = std::fs::read_dir(&home)
        .unwrap()
        .flatten()
        .any(|e| e.file_name().to_string_lossy().starts_with("config.json_CORRUPTED_"));
    assert!(aside);

    // Unreadable file (here a dir): kept aside, so a later save cannot replace it.
    assert!(!home.join(CONFIG_FILE).exists());
    std::fs::create_dir_all(home.join(CONFIG_FILE).join("keep")).unwrap();
    assert_eq!(Config::load(), Config::default());
    assert!(!home.join(CONFIG_FILE).exists());
    Config::default().save().unwrap();
    let kept = std::fs::read_dir(&home).unwrap().flatten().any(|e| e.path().join("keep").is_dir());
    assert!(kept);
    // Both corrupt copies survive, even within the same second.
    let asides = std::fs::read_dir(&home)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("config.json_CORRUPTED_"))
        .count();
    assert_eq!(asides, 2);
    std::fs::create_dir_all(home.join(LABELS_FILE)).unwrap();
    assert_eq!(LabelList::load(), LabelList::defaults());
    assert!(!home.join(LABELS_FILE).exists());

    let mut l = LabelList::defaults();
    l.toggle(0, "Foo");
    l.save().unwrap();
    assert_eq!(LabelList::load(), l);
    std::fs::write(home.join(LABELS_FILE), "nope").unwrap();
    assert_eq!(LabelList::load(), LabelList::defaults());
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn bad_text_size_keeps_other_hangar_settings() {
    for (raw, want) in [("150.4", 150), ("-5", 0), ("70000", 65535), ("\"big\"", 100), ("null", 100)] {
        let text = format!(r#"{{"Hangar": {{"dark_mode": true, "hidden_columns": ["author"], "text_size": {raw}}}}}"#);
        let c = parse_config(text.as_bytes()).expect("config parses");
        assert_eq!(c.hangar.text_size, want, "{raw}");
        assert_eq!(c.hangar.dark_mode, Some(true), "{raw}");
        assert_eq!(c.hangar.hidden_columns, vec!["author".to_string()], "{raw}");
    }
    // An old config without the field gets the normal size.
    let c = parse_config(br#"{"Hangar": {"hide_v": true}}"#).expect("config parses");
    assert_eq!(c.hangar.text_size, 100);
}
