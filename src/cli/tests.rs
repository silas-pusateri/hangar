use std::sync::Arc;

use super::args::{self, Globals, flag, multi, value};
use super::{admin, change, fmt, interrupt, mods, term};
use crate::meta::{GameVersion, Module, ModuleVersion};
use crate::resolve::{Change, ChangeKind, Changeset, OptionalKind, OptionalMod, Reason};

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

fn module(id: &str, ver: &str) -> Module {
    let json = format!(
        r#"{{"spec_version":1,"identifier":"{id}","name":"{id} Mod","abstract":"Does {id} things","author":["Ann","Bob Smith"],
        "version":"{ver}","license":"MIT","download":"https://example.test/{id}.zip","download_size":2048,
        "ksp_version_min":"1.8","ksp_version_max":"1.12","depends":[{{"name":"ModuleManager","min_version":"4.0"}}],
        "resources":{{"homepage":"https://example.test/home"}}}}"#
    );
    match Module::from_json(json.as_bytes()) {
        Ok(m) => m,
        Err(e) => panic!("{e}"),
    }
}

#[test]
fn globals_anywhere() {
    let (g, rest) = args::split_globals(s(&["install", "--headless", "Foo", "--instance=Main", "-y"])).unwrap();
    assert_eq!(g, Globals { instance: Some("Main".into()), gamedir: None, headless: true, yes: true });
    assert_eq!(rest, s(&["install", "Foo"]));
    let (g, rest) = args::split_globals(s(&["--gamedir", "/tmp/ksp", "list"])).unwrap();
    assert_eq!(g.gamedir.as_deref(), Some(std::path::Path::new("/tmp/ksp")));
    assert_eq!(rest, s(&["list"]));
    // After `--` nothing is a global option.
    let (g, rest) = args::split_globals(s(&["search", "--", "--headless"])).unwrap();
    assert!(!g.headless);
    assert_eq!(rest, s(&["search", "--", "--headless"]));
}

#[test]
fn globals_errors() {
    assert!(args::split_globals(s(&["--instance"])).is_err());
    assert!(args::split_globals(s(&["--instance", "a", "--gamedir", "b", "list"])).is_err());
    assert!(args::split_globals(s(&["--instance="])).is_err());
}

#[test]
fn parse_flags_values_multi() {
    let spec = [flag(&["--all"]), value(&["--author"]), multi(&["-c", "--ckanfiles"])];
    let p = args::parse(&s(&["a", "--author", "x", "-c", "f1.ckan", "f2.ckan", "--all", "b"]), &spec).unwrap();
    assert_eq!(p.pos, s(&["a", "b"]));
    assert_eq!(p.value("--author"), Some("x"));
    assert_eq!(p.values("-c"), s(&["f1.ckan", "f2.ckan"]).as_slice());
    assert!(p.flag("--all"));
    let p = args::parse(&s(&["--author=y", "--ckanfiles=a.ckan", "-c", "b.ckan"]), &spec).unwrap();
    assert_eq!(p.value("--author"), Some("y"));
    assert_eq!(p.values("-c"), s(&["a.ckan", "b.ckan"]).as_slice());
}

#[test]
fn parse_errors_and_numbers() {
    let spec = [flag(&["--all"]), value(&["--author"]), multi(&["-c"])];
    assert!(args::parse(&s(&["--nope"]), &spec).is_err());
    assert!(args::parse(&s(&["--author"]), &spec).is_err());
    assert!(args::parse(&s(&["-c", "--all"]), &spec).is_err());
    assert!(args::parse(&s(&["--all=1"]), &spec).is_err());
    // Negative numbers and a lone dash are values.
    let p = args::parse(&s(&["setlimit", "-1", "-"]), &spec).unwrap();
    assert_eq!(p.pos, s(&["setlimit", "-1", "-"]));
    let p = args::parse(&s(&["--", "--all"]), &spec).unwrap();
    assert_eq!(p.pos, s(&["--all"]));
    assert!(!p.flag("--all"));
}

#[test]
fn id_version_split() {
    assert_eq!(args::split_id_version("Foo"), ("Foo", None));
    assert_eq!(args::split_id_version("Foo=1:2.0"), ("Foo", Some("1:2.0")));
    assert_eq!(args::split_id_version("Foo=a=b"), ("Foo", Some("a=b")));
}

#[test]
fn multi_choice_answers() {
    assert_eq!(term::parse_multi_choice("", 5).unwrap(), Vec::<usize>::new());
    assert_eq!(term::parse_multi_choice("a", 3).unwrap(), vec![0, 1, 2]);
    assert_eq!(term::parse_multi_choice("3, 1 2-3", 5).unwrap(), vec![0, 1, 2]);
    assert!(term::parse_multi_choice("0", 5).is_err());
    assert!(term::parse_multi_choice("6", 5).is_err());
    assert!(term::parse_multi_choice("x", 5).is_err());
    assert!(term::parse_multi_choice("3-2", 5).is_err());
}

#[test]
fn list_bullets() {
    let base = fmt::RowFacts {
        unmanaged: false,
        in_repo: true,
        has_compatible: true,
        upgradeable: false,
        auto_installed: false,
        replacement: None,
    };
    assert_eq!(fmt::list_row("A", "1.0", &base), "- A 1.0");
    assert_eq!(fmt::list_row("A", "1.0", &fmt::RowFacts { auto_installed: true, ..base }), "+ A 1.0");
    assert_eq!(fmt::list_row("A", "1.0", &fmt::RowFacts { upgradeable: true, ..base }), "^ A 1.0");
    assert_eq!(fmt::list_row("A", "1.0", &fmt::RowFacts { has_compatible: false, ..base }), "X A 1.0");
    assert_eq!(
        fmt::list_row("A", "1.0", &fmt::RowFacts { has_compatible: false, replacement: Some(("B", "2.0")), ..base }),
        "> A 1.0 > B 2.0"
    );
    assert_eq!(fmt::list_row("A", "1.0", &fmt::RowFacts { in_repo: false, ..base }), "? A 1.0");
    assert_eq!(fmt::list_row("kOS", "", &fmt::RowFacts { unmanaged: true, ..base }), "A kOS");
}

#[test]
fn changeset_text() {
    let mm = Arc::new(module("ModuleManager", "4.2.3"));
    let foo = Arc::new(module("Foo", "1.1"));
    let cs = Changeset {
        changes: vec![
            Change {
                kind: ChangeKind::Install,
                module: mm,
                old_version: None,
                replaces: None,
                reasons: vec![Reason::Depends("Foo".into())],
                auto_installed: true,
            },
            Change {
                kind: ChangeKind::Upgrade,
                module: foo,
                old_version: Some(ModuleVersion::new("1.0")),
                replaces: None,
                reasons: vec![Reason::UserRequested],
                auto_installed: false,
            },
        ],
    };
    let t = fmt::changeset(&cs, &|_| false);
    assert!(t.starts_with("Changes:\n"), "{t}");
    assert!(t.contains("  Install  ModuleManager 4.2.3 (Dependency of Foo)\n"), "{t}");
    assert!(t.contains("  Upgrade  Foo 1.0 -> 1.1 (Requested by user)\n"), "{t}");
    assert!(t.contains("Download size: 4.0 KiB"), "{t}");
    // Cached downloads do not count.
    let t = fmt::changeset(&cs, &|m| m.identifier == "Foo");
    assert!(t.contains("Download size: 2.0 KiB"), "{t}");
    let t = fmt::changeset(&cs, &|_| true);
    assert!(!t.contains("Download size"), "{t}");
}

#[test]
fn optional_text() {
    let o = OptionalMod {
        module: Arc::new(module("Bar", "2.0")),
        kind: OptionalKind::Recommends,
        sources: vec!["Foo".into()],
        checked: true,
    };
    assert_eq!(fmt::optional_list(&[o]), "  1) Bar 2.0 - Does Bar things (recommended by Foo)\n");
}

#[test]
fn spans_and_tables() {
    let any = GameVersion::ANY;
    let v = |s: &str| GameVersion::parse(s).unwrap();
    assert_eq!(fmt::span(&any, &any), "KSP all versions");
    assert_eq!(fmt::span(&v("1.8"), &v("1.12")), "KSP 1.8-1.12");
    assert_eq!(fmt::span(&v("1.12"), &v("1.12")), "KSP 1.12");
    assert_eq!(fmt::span(&any, &v("1.12")), "KSP 1.12 and earlier");
    assert_eq!(fmt::span(&v("1.8"), &any), "KSP 1.8 and later");
    let t = fmt::table(&["Name", "Path"], &[["a".to_string(), "/x".to_string()], ["long name".into(), "/y".into()]]);
    assert_eq!(t, "Name       Path\n---------  ----\na          /x\nlong name  /y\n");
}

#[test]
fn search_matching() {
    let m = module("KerbalEngineerRedux", "1.1.9");
    let t = |x: &str| fmt::searchable(x);
    assert!(fmt::search_matches(&m, &t("kerbal engineer"), ""));
    assert!(fmt::search_matches(&m, &t("Kerbal-Engineer"), &t("bob smith")));
    assert!(fmt::search_matches(&m, &t("does"), ""));
    assert!(!fmt::search_matches(&m, &t("mechjeb"), ""));
    assert!(!fmt::search_matches(&m, &t("kerbal"), &t("carol")));
    assert!(fmt::search_matches(&m, "", &t("ann")));
}

#[test]
fn show_text() {
    let m = module("Foo", "1.0");
    let files = vec!["GameData/Foo".to_string(), "GameData/Foo/a.cfg".to_string()];
    let t = fmt::show_module(&m, Some(&files));
    assert!(t.starts_with("Foo Mod: Does Foo things\n"), "{t}");
    assert!(t.contains("  Version:\t1.0\n"), "{t}");
    assert!(t.contains("  Authors:\tAnn, Bob Smith\n"), "{t}");
    assert!(t.contains("  Game:\t\tKSP 1.8-1.12\n"), "{t}");
    assert!(t.contains("\nDepends:\n  - ModuleManager >= 4.0\n"), "{t}");
    assert!(t.contains("  Home page:\thttps://example.test/home\n"), "{t}");
    assert!(t.contains("Showing 2 installed files:\n  - GameData/Foo\n"), "{t}");
    assert!(t.contains("\nFilename: "), "{t}");
    assert!(t.trim_end().ends_with("-Foo-1.0.zip"), "{t}");
}

#[test]
fn compare_versions() {
    assert_eq!(fmt::compare("1.0", "1.1"), -1);
    assert_eq!(fmt::compare("1.1", "1.1"), 0);
    assert_eq!(fmt::compare("1:0.1", "2.0"), 1);
    assert_eq!(fmt::compare("v1.10", "v1.9"), 1);
}

#[test]
fn provider_order() {
    let mut c = vec![Arc::new(module("Zed", "1")), Arc::new(module("Alpha", "1")), Arc::new(module("Want", "1"))];
    let dl = |id: &str| if id == "Zed" { 50 } else { 10 };
    change::order_choices(&mut c, "Want", &dl);
    let ids: Vec<&str> = c.iter().map(|m| m.identifier.as_str()).collect();
    assert_eq!(ids, ["Zed", "Want", "Alpha"]);
}

#[test]
fn conflict_message_paths() {
    let msg = "GameData/x.cfg already exists; Foo 1.0 cannot install it";
    assert_eq!(change::paths_in_conflict_message(msg), s(&["GameData/x.cfg"]));
    let owned = "GameData/y.cfg already exists (owned by Bar); Foo 1.0 cannot install it";
    assert!(change::paths_in_conflict_message(owned).is_empty());
    assert!(change::paths_in_conflict_message("something else").is_empty());
}

#[test]
fn export_formats_from_names() {
    use crate::modlist::ExportFormat;
    assert_eq!(mods::export_format(None, "mods.ckan").ok(), Some(ExportFormat::Ckan));
    assert_eq!(mods::export_format(None, "a/b.MD").ok(), Some(ExportFormat::Markdown));
    assert_eq!(mods::export_format(Some("bbcode"), "x.txt").ok(), Some(ExportFormat::BbCode));
    assert!(mods::export_format(None, "noext").is_err());
    assert!(mods::export_format(Some("pdf"), "x.pdf").is_err());
}

#[test]
fn fake_versions_and_limits() {
    let v = admin::fake_version("1.12.5").unwrap();
    assert_eq!((v.major, v.minor, v.patch), (1, 12, 5));
    assert!(v.build > 0, "known build id expected, got {v}");
    assert_eq!(admin::fake_version("1.2.2.1622").unwrap().build, 1622);
    assert!(admin::fake_version("1").is_none());
    assert!(admin::fake_version("any").is_none());
    assert_eq!(admin::limit_text(None), "Unlimited");
    assert_eq!(admin::limit_text(Some(5 * 1024 * 1024)), "5 MiB");
}

#[test]
fn temp_instance_entries() {
    let tmp = std::env::temp_dir().join(format!("hangar-cli-test-{}", std::process::id()));
    let dir = tmp.join("KSP Fake");
    let _ = std::fs::remove_dir_all(&tmp);
    crate::instance::create_fake(&dir, &GameVersion::new(1, 12, 5, 3190), &[]).unwrap();
    let mut c = crate::config::Config::default();
    let n = super::temp_instance(&mut c, &dir).unwrap();
    assert_eq!(n, "KSP Fake");
    assert_eq!(c.game_instances.len(), 1);
    // The same folder again reuses the entry.
    assert_eq!(super::temp_instance(&mut c, &dir).unwrap(), "KSP Fake");
    assert_eq!(c.game_instances.len(), 1);
    assert!(super::temp_instance(&mut c, &tmp).is_err());
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn run_without_session() {
    assert_eq!(super::run(s(&["compare", "1.0", "2.0"])), 0);
    assert_eq!(super::run(s(&["compare", "1.0"])), 1);
    assert_eq!(super::run(s(&["version"])), 0);
    assert_eq!(super::run(s(&["nosuchcommand"])), 1);
    assert_eq!(super::run(s(&["--instance"])), 1);
}

#[test]
fn progress_message_shapes() {
    assert_eq!(term::message_shape("Downloading local: 2.0 MiB"), term::message_shape("Downloading local: 13.5 MiB"));
    assert_ne!(term::message_shape("Downloading A"), term::message_shape("Installing A"));
}

#[test]
fn interrupt_is_held_while_critical() {
    let g = interrupt::Guard::new();
    interrupt::Guard::note_progress("Installing Foo 1.0");
    // SAFETY: the guard's handler is installed and the phase is critical, so it only sets a flag.
    unsafe { libc::raise(libc::SIGINT) };
    assert!(g.interrupted());
    // Download messages leave the critical phase; others enter it again.
    interrupt::Guard::note_progress("Downloading Foo 1.0");
    interrupt::Guard::note_progress("Saving the registry");
    drop(g);
    // A new guard starts clean.
    let g = interrupt::Guard::new();
    assert!(!g.interrupted());
}

#[test]
fn progress_lines_fit_the_terminal() {
    assert_eq!(term::fit_width("Saving", 10), "Saving");
    assert_eq!(term::fit_width("Reading /a/very/long/path", 10), "Reading...");
    assert_eq!(term::fit_width("abcdef", 2), "..");
    assert_eq!(term::fit_width("abc", 0), "");
}
