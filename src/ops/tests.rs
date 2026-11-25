//! Session tests on temp dirs: a fake game, a local file:// repo, and zips put into the cache
//! by hand, so no network is used (except the ignored live test).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;
use crate::meta::{GameVersion, ModuleVersion};
use crate::resolve::InstallRequest;

static SEQ: AtomicU64 = AtomicU64::new(0);

fn tmp(tag: &str) -> PathBuf {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!("hangar-ops-t-{tag}-{}-{n}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn no_progress(_: &str, _: Option<f32>) {}

fn make_zip(path: &Path, files: &[(&str, &[u8])]) {
    let f = fs::File::create(path).unwrap();
    let mut zw = zip::ZipWriter::new(f);
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, data) in files {
        zw.start_file(*name, opts).unwrap();
        zw.write_all(data).unwrap();
    }
    zw.finish().unwrap();
}

fn write_tar_gz(path: &Path, files: &[(String, Vec<u8>)]) {
    let f = fs::File::create(path).unwrap();
    let gz = flate2::write::GzEncoder::new(f, flate2::Compression::fast());
    let mut b = tar::Builder::new(gz);
    for (name, data) in files {
        let mut h = tar::Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        b.append_data(&mut h, name, &data[..]).unwrap();
    }
    b.into_inner().unwrap().finish().unwrap();
}

/// One test module: its zip holds `files` (paths inside the zip).
struct TMod {
    id: &'static str,
    ver: &'static str,
    extra: &'static str,
    files: Vec<(&'static str, &'static [u8])>,
    /// Download URL; None = a fake https URL (the zip is put into the cache).
    url: Option<&'static str>,
}

fn tmod(id: &'static str, ver: &'static str, extra: &'static str, files: Vec<(&'static str, &'static [u8])>) -> TMod {
    TMod { id, ver, extra, files, url: None }
}

struct Env {
    root: PathBuf,
    game: PathBuf,
    zips: Vec<(String, String, PathBuf)>,
    repo: Repository,
}

impl Env {
    fn new(tag: &str, mods: &[TMod]) -> Env {
        let root = tmp(tag);
        let game = root.join("game");
        crate::instance::create_fake(&game, &GameVersion::new(1, 12, 5, 3190), &[]).unwrap();
        let zdir = root.join("zips");
        fs::create_dir_all(&zdir).unwrap();
        let mut zips = Vec::new();
        let mut tar_files = Vec::new();
        for m in mods {
            let z = zdir.join(format!("{}-{}.zip", m.id, m.ver));
            make_zip(&z, &m.files);
            let size = fs::metadata(&z).unwrap().len();
            let url = m.url.map(str::to_string).unwrap_or_else(|| format!("https://example.invalid/{}-{}.zip", m.id, m.ver));
            let ckan = format!(
                r#"{{"spec_version":1,"identifier":"{id}","name":"{id}","abstract":"a","author":"me","version":"{ver}",
                "license":"MIT","download":"{url}","download_size":{size}{extra}}}"#,
                id = m.id,
                ver = m.ver,
                extra = m.extra
            );
            tar_files.push((format!("CKAN-meta-master/{}/{}-{}.ckan", m.id, m.id, m.ver), ckan.into_bytes()));
            zips.push((m.id.to_string(), m.ver.to_string(), z));
        }
        let meta = root.join("meta.tar.gz");
        write_tar_gz(&meta, &tar_files);
        let repo = Repository {
            name: "local".into(),
            uri: format!("file://{}", meta.display()),
            priority: 0,
            x_mirror: false,
            x_comment: None,
        };
        Env { root, game, zips, repo }
    }

    fn config(&self) -> Config {
        let mut c = Config::default();
        c.game_instances.push(InstanceEntry { name: "t".into(), path: self.game.display().to_string(), game: "KSP".into() });
        c.download_cache_dir = Some(self.root.join("cache").display().to_string());
        c
    }

    /// Open, point the instance at the local repo, and put every zip into the cache.
    fn session(&self) -> Session {
        let mut s =
            Session::open_with(self.config(), None, LabelList::defaults(), self.root.join("repos"), false).unwrap();
        s.set_repositories(vec![self.repo.clone()]).unwrap();
        assert!(s.warnings.is_empty(), "{:?}", s.warnings);
        for (id, ver, z) in &self.zips {
            let Some(m) = s.index.get(id, &ModuleVersion::new(ver)).cloned() else { continue };
            if m.download.first().is_some_and(|u| u.starts_with("https://example.invalid/")) {
                s.cache.store(&m, z, false).unwrap();
            }
        }
        s
    }

    fn has(&self, rel: &str) -> bool {
        self.game.join(rel).exists()
    }
}

fn install_req(ids: &[(&str, Option<&str>)]) -> Request {
    let install = ids
        .iter()
        .map(|(id, v)| match v {
            Some(v) => InstallRequest::Exact(id.to_string(), ModuleVersion::new(v)),
            None => InstallRequest::Latest(id.to_string()),
        })
        .collect();
    Request { install, ..Default::default() }
}

fn run(s: &mut Session, req: &Request) -> Result<ApplyReport> {
    let cs = s.plan(req, &ResolverOptions::default())?;
    s.apply(&cs, &no_progress)
}

fn basic_mods() -> Vec<TMod> {
    vec![
        tmod("Dep", "1.0", "", vec![("GameData/Dep/dep.cfg", b"dep")]),
        tmod(
            "Main",
            "1.0",
            r#","depends":[{"name":"Dep"}]"#,
            vec![("GameData/Main/main.cfg", b"one"), ("GameData/Main/old-only.cfg", b"x")],
        ),
        tmod("Main", "2.0", r#","depends":[{"name":"Dep"}]"#, vec![("GameData/Main/main.cfg", b"two")]),
    ]
}

#[test]
fn install_upgrade_remove() {
    let env = Env::new("flow", &basic_mods());
    let mut s = env.session();
    let ckan = env.game.join("CKAN");

    let r = run(&mut s, &install_req(&[("Main", Some("1.0"))])).unwrap();
    assert_eq!(r.installed, vec!["Dep 1.0".to_string(), "Main 1.0".to_string()]);
    assert!(env.has("GameData/Dep/dep.cfg") && env.has("GameData/Main/old-only.cfg"));
    assert!(s.registry.installed("Dep").unwrap().auto_installed);
    assert!(!s.registry.installed("Main").unwrap().auto_installed);
    // Registry files on disk.
    let saved = Registry::load(&ckan).unwrap();
    assert!(saved.installed("Main").is_some() && saved.installed("Dep").is_some());
    let modpack = fs::read_to_string(ckan.join("installed-t.ckan")).unwrap();
    assert!(modpack.contains("Main"));
    assert!(!s.instance.history().is_empty());
    // No undo dir left behind.
    assert!(!fs::read_dir(&ckan).unwrap().flatten().any(|e| e.file_name().to_string_lossy().starts_with("tx-")));

    let r = run(&mut s, &Request { upgrade: vec!["Main".into()], ..Default::default() }).unwrap();
    assert_eq!(r.upgraded, vec!["Main 1.0 -> 2.0".to_string()]);
    assert_eq!(fs::read(env.game.join("GameData/Main/main.cfg")).unwrap(), b"two");
    assert!(!env.has("GameData/Main/old-only.cfg"));
    assert_eq!(s.registry.installed("Main").unwrap().source_module.version, ModuleVersion::new("2.0"));
    assert!(!s.registry.installed("Main").unwrap().auto_installed);

    // Removing Main also removes the auto-installed Dep.
    let r = run(&mut s, &Request { remove: vec!["Main".into()], ..Default::default() }).unwrap();
    assert!(r.removed.iter().any(|x| x.starts_with("Main ")));
    assert!(r.removed.iter().any(|x| x.starts_with("Dep ")));
    assert!(!env.has("GameData/Main") && !env.has("GameData/Dep"));
    assert!(env.has("GameData/Squad"));
    assert!(s.registry.installed_mods().next().is_none());
    assert!(Registry::load(&ckan).unwrap().installed_mods().next().is_none());

    // set_auto_installed on a missing module is an error.
    assert!(s.set_auto_installed("Main", true).is_err());
}

#[test]
fn conflict_then_trash_and_retry() {
    let env = Env::new("conflict", &basic_mods());
    let mut s = env.session();
    fs::create_dir_all(env.game.join("GameData/Dep")).unwrap();
    fs::write(env.game.join("GameData/Dep/dep.cfg"), b"mine").unwrap();
    let before = fs::read(env.game.join("CKAN/registry.json")).unwrap();

    let e = run(&mut s, &install_req(&[("Main", Some("1.0"))])).unwrap_err();
    assert!(matches!(e, Error::Conflict(_)), "{e}");
    assert_eq!(conflict_files(&e), vec!["GameData/Dep/dep.cfg".to_string()]);
    assert!(s.registry.installed("Dep").is_none());
    assert!(!env.has("GameData/Main"));
    assert_eq!(fs::read(env.game.join("CKAN/registry.json")).unwrap(), before);
    assert_eq!(fs::read(env.game.join("GameData/Dep/dep.cfg")).unwrap(), b"mine");

    // Refused: escapes, absolute paths, the CKAN dir, stock folders, missing files.
    for bad in ["../x", "/etc/hosts", "GameData/../buildID.txt", "CKAN/registry.json", "ckan", "GameData/Squad", "GameData", ""] {
        assert!(s.trash_files(&[bad.to_string()]).is_err(), "{bad}");
    }
    assert!(s.trash_files(&["GameData/none.cfg".to_string()]).is_err());
    assert!(env.has("buildID.txt") && env.has("CKAN/registry.json"));

    s.trash_files(&conflict_files(&e)).unwrap();
    assert!(!env.has("GameData/Dep/dep.cfg"));
    run(&mut s, &install_req(&[("Main", Some("1.0"))])).unwrap();
    assert_eq!(fs::read(env.game.join("GameData/Dep/dep.cfg")).unwrap(), b"dep");

    // Owned files are refused, also with other case.
    assert!(s.trash_files(&["GameData/Dep/dep.cfg".to_string()]).is_err());
    assert!(s.trash_files(&["gamedata/dep/DEP.cfg".to_string()]).is_err());
    assert!(s.trash_files(&["GameData/Dep".to_string()]).is_err());
    assert!(env.has("GameData/Dep/dep.cfg"));
}

const SHARED_A: &str = r#","install":[{"find":"Alpha","install_to":"GameData"},{"find":"Shared","install_to":"GameData"}]"#;
const SHARED_B: &str = r#","install":[{"find":"Beta","install_to":"GameData"},{"find":"Shared","install_to":"GameData"}]"#;

/// Two mods that write the same file: the second install fails after the first one wrote its
/// files; everything is undone.
#[test]
fn rollback_on_failing_second_install() {
    let mods = vec![
        tmod("Alpha", "1.0", SHARED_A, vec![("GameData/Alpha/a.cfg", b"a"), ("GameData/Shared/s.cfg", b"a")]),
        tmod("Beta", "1.0", SHARED_B, vec![("GameData/Beta/b.cfg", b"b"), ("GameData/Shared/s.cfg", b"b")]),
    ];
    let env = Env::new("rollback", &mods);
    let mut s = env.session();
    let reg_before = s.registry.to_ckan_json().unwrap();
    let file_before = fs::read(env.game.join("CKAN/registry.json")).unwrap();

    let e = run(&mut s, &install_req(&[("Alpha", None), ("Beta", None)])).unwrap_err();
    assert!(matches!(e, Error::Conflict(_)), "{e}");
    assert!(conflict_files(&e).is_empty(), "owned-by-other-mod is not an unowned conflict");
    assert!(!env.has("GameData/Alpha") && !env.has("GameData/Shared") && !env.has("GameData/Beta"));
    assert_eq!(s.registry.to_ckan_json().unwrap(), reg_before);
    assert_eq!(fs::read(env.game.join("CKAN/registry.json")).unwrap(), file_before);
}

/// A cached zip with a bad CRC: the install fails, earlier installs are undone, the zip is purged.
#[test]
fn corrupt_cached_zip_is_purged() {
    let mods = vec![
        tmod("Alpha", "1.0", "", vec![("GameData/Alpha/a.cfg", b"a")]),
        tmod("Beta", "1.0", "", vec![("GameData/Beta/b.cfg", b"BBBBBBBBBBBBBBBBBBBB")]),
    ];
    let env = Env::new("corrupt", &mods);
    let mut s = env.session();
    let beta = s.index.get("Beta", &ModuleVersion::new("1.0")).cloned().unwrap();
    let cached = s.cache.get(&beta).unwrap();
    let mut bytes = fs::read(&cached).unwrap();
    let pos = bytes.windows(20).position(|w| w == b"BBBBBBBBBBBBBBBBBBBB").unwrap();
    bytes[pos] = b'X';
    fs::write(&cached, &bytes).unwrap();

    let e = run(&mut s, &install_req(&[("Alpha", None), ("Beta", None)])).unwrap_err();
    assert!(e.to_string().contains("corrupt zip in cache"), "{e}");
    assert!(e.to_string().contains("deleted"), "{e}");
    assert!(!cached.exists());
    assert!(!env.has("GameData/Alpha") && !env.has("GameData/Beta"));
    assert!(s.registry.installed("Alpha").is_none());
}

/// A download that fails: nothing is installed, not even the mods that were cached.
#[test]
fn failed_download_installs_nothing() {
    let mut mods = vec![tmod("Alpha", "1.0", "", vec![("GameData/Alpha/a.cfg", b"a")])];
    mods.push(TMod { url: Some("http://127.0.0.1:9/Gamma-1.0.zip"), ..tmod("Gamma", "1.0", "", vec![("GameData/Gamma/g.cfg", b"g")]) });
    let env = Env::new("dlfail", &mods);
    let mut s = env.session();
    let e = run(&mut s, &install_req(&[("Alpha", None), ("Gamma", None)])).unwrap_err();
    assert!(matches!(e, Error::Net(_)), "{e}");
    assert!(!env.has("GameData/Alpha"));
    assert!(s.registry.installed("Alpha").is_none());
}

#[test]
fn open_rules_lock_and_rescan() {
    let env = Env::new("open", &basic_mods());
    let labels = LabelList::defaults;
    let repos = env.root.join("repos");
    // Unknown name, no instances.
    assert!(matches!(
        Session::open_with(env.config(), Some("nope"), labels(), repos.clone(), false),
        Err(Error::NotFound(_))
    ));
    assert!(Session::open_with(Config::default(), None, labels(), repos.clone(), false).is_err());

    // Auto-start wins over the first entry.
    let other = env.root.join("other");
    crate::instance::create_fake(&other, &GameVersion::new(1, 11, 2, 2785), &[]).unwrap();
    let mut c = env.config();
    c.game_instances.push(InstanceEntry { name: "o".into(), path: other.display().to_string(), game: "KSP".into() });
    c.auto_start_instance = Some("o".into());
    let s = Session::open_with(c.clone(), None, labels(), repos.clone(), false).unwrap();
    assert_eq!(s.instance.name, "o");
    assert!(s.instance.version.is_some_and(|v| v.without_build() == GameVersion::new(1, 11, 2, -1).without_build()));
    // Second open of the same instance: locked.
    assert!(matches!(Session::open_with(c.clone(), Some("o"), labels(), repos.clone(), false), Err(Error::Locked(_))));
    drop(s);
    assert!(Session::open_with(c, Some("o"), labels(), repos.clone(), false).is_ok());

    // No version can be detected: refused.
    let bad = env.root.join("bad");
    fs::create_dir_all(bad.join("GameData")).unwrap();
    fs::write(bad.join("buildID64.txt"), "build id = 99999").unwrap();
    let mut c = env.config();
    c.game_instances[0].path = bad.display().to_string();
    let e = Session::open_with(c, None, labels(), repos.clone(), false).err().unwrap();
    assert!(e.to_string().contains("game version"), "{e}");

    // Not a game dir: refused, and no CKAN dir is made there.
    let empty = env.root.join("empty");
    fs::create_dir_all(&empty).unwrap();
    let mut c = env.config();
    c.game_instances[0].path = empty.display().to_string();
    assert!(Session::open_with(c, None, labels(), repos.clone(), false).is_err());
    assert!(!empty.join("CKAN").exists());

    // DLC stub and a manual DLL are found by rescan.
    let mut s = env.session();
    fs::create_dir_all(env.game.join("CKAN/dlc")).unwrap();
    fs::write(env.game.join("CKAN/dlc/MakingHistory.dlc"), "1.9.1").unwrap();
    fs::create_dir_all(env.game.join("GameData/Manual")).unwrap();
    fs::write(env.game.join("GameData/Manual/Manual.dll"), "x").unwrap();
    assert!(s.rescan().unwrap());
    assert!(s.registry.installed("MakingHistory-DLC").is_some());
    assert!(s.registry.installed_dlls.contains_key("Manual"));
    assert!(!s.rescan().unwrap());
    let saved = Registry::load(&env.game.join("CKAN")).unwrap();
    assert!(saved.installed("MakingHistory-DLC").is_some());
}

#[test]
fn update_repos_from_file_uri() {
    let env = Env::new("update", &basic_mods());
    let mut s = Session::open_with(env.config(), None, LabelList::defaults(), env.root.join("repos"), false).unwrap();
    // Default repo, never updated: empty index.
    assert!(s.index.modules.is_empty());
    s.registry.sorted_repositories.clear();
    s.registry.sorted_repositories.insert("local".into(), env.repo.clone());
    assert_eq!(s.update_repos(false, &no_progress).unwrap(), UpdateResult::Updated);
    assert_eq!(s.index.versions("Main").len(), 2);
    assert!(fs::read_dir(env.root.join("repos")).unwrap().flatten().any(|e| e.file_name().to_string_lossy().ends_with(".tar.gz")));
    // A corrupt cache gives an empty index and a warning, not an error.
    for e in fs::read_dir(env.root.join("repos")).unwrap().flatten() {
        if e.file_name().to_string_lossy().ends_with(".tar.gz") {
            fs::write(e.path(), b"not an archive").unwrap();
        }
    }
    s.reload_index();
    assert!(s.index.modules.is_empty());
    assert!(!s.warnings.is_empty());

    // Repository list rules.
    assert!(s.set_repositories(vec![env.repo.clone(), env.repo.clone()]).is_err());
    let mut r2 = env.repo.clone();
    r2.name = "second".into();
    r2.priority = 7;
    s.set_repositories(vec![r2, env.repo.clone()]).unwrap();
    let repos = s.registry.repositories();
    assert_eq!(repos.iter().map(|r| (r.name.as_str(), r.priority)).collect::<Vec<_>>(), vec![("second", 0), ("local", 1)]);
}

#[test]
fn conflict_text_round_trip() {
    let e = conflict_error(&["GameData/a b.cfg".into(), "Ships/VAB/x.craft".into()]);
    assert_eq!(conflict_files(&e), vec!["GameData/a b.cfg".to_string(), "Ships/VAB/x.craft".to_string()]);
    assert!(conflict_files(&Error::Conflict("other".into())).is_empty());
    assert!(conflict_files(&Error::Other(UNOWNED_FILES_HEADER.into())).is_empty());
    let mut c = Config::default();
    let found = vec![PathBuf::from("/x/steamapps/common/Kerbal Space Program"), PathBuf::from("/Applications/Kerbal Space Program")];
    assert!(add_detected(&mut c, &found));
    let names: Vec<&str> = c.game_instances.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, vec!["Steam", "Kerbal Space Program"]);
    assert!(!add_detected(&mut c, &found));
}

/// A tiny HTTP server that sends `body` slowly to every request and counts the requests.
fn slow_server(body: Vec<u8>) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use std::io::Read;
    use std::net::TcpListener;
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/Shared-1.0.zip", l.local_addr().unwrap());
    let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let h = hits.clone();
    std::thread::spawn(move || {
        for conn in l.incoming().flatten() {
            h.fetch_add(1, Ordering::SeqCst);
            let body = body.clone();
            std::thread::spawn(move || {
                let mut conn = conn;
                let mut req = Vec::new();
                let mut b = [0u8; 1024];
                while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                    match conn.read(&mut b) {
                        Ok(0) | Err(_) => return,
                        Ok(n) => req.extend_from_slice(&b[..n]),
                    }
                }
                let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                let _ = conn.write_all(head.as_bytes());
                for chunk in body.chunks(body.len().div_ceil(6).max(1)) {
                    let _ = conn.write_all(chunk);
                    let _ = conn.flush();
                    std::thread::sleep(std::time::Duration::from_millis(40));
                }
            });
        }
    });
    (url, hits)
}

/// Two mods from one zip (like RasterPropMonitor and RasterPropMonitor-Core): one download,
/// and both install. Before the fix both threads wrote the same partial and cache file.
#[test]
fn shared_download_url_fetched_once() {
    let files: Vec<(&'static str, &'static [u8])> =
        vec![("GameData/CoreDir/c.cfg", b"core"), ("GameData/MainDir/m.cfg", b"main")];
    let mut zip = tmp("sharedzip");
    zip.push("s.zip");
    make_zip(&zip, &files);
    let (url, hits) = slow_server(fs::read(&zip).unwrap());
    let url: &'static str = Box::leak(url.into_boxed_str());
    let mods = vec![
        TMod { url: Some(url), ..tmod("Core", "1.0", r#","install":[{"find":"CoreDir","install_to":"GameData"}]"#, files.clone()) },
        TMod {
            url: Some(url),
            ..tmod(
                "Main",
                "1.0",
                r#","depends":[{"name":"Core"}],"install":[{"find":"MainDir","install_to":"GameData"}]"#,
                files.clone(),
            )
        },
    ];
    let env = Env::new("shared", &mods);
    let mut s = env.session();
    let r = run(&mut s, &install_req(&[("Main", None)])).unwrap();
    assert_eq!(r.installed.len(), 2, "{r:?}");
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    assert!(env.has("GameData/CoreDir/c.cfg") && env.has("GameData/MainDir/m.cfg"));
    let core = s.index.get("Core", &ModuleVersion::new("1.0")).cloned().unwrap();
    let main = s.index.get("Main", &ModuleVersion::new("1.0")).cloned().unwrap();
    assert!(s.cache.get(&core).is_some() && s.cache.get(&main).is_some());
}

/// Mods that do not fit on the disk: refused before any file changes (CKAN CheckFreeSpace).
#[test]
fn no_free_space_installs_nothing() {
    let mods = vec![
        tmod("Alpha", "1.0", "", vec![("GameData/Alpha/a.cfg", b"a")]),
        tmod("Huge", "1.0", r#","install_size":9000000000000000000"#, vec![("GameData/Huge/h.cfg", b"h")]),
    ];
    let env = Env::new("space", &mods);
    let mut s = env.session();
    let e = run(&mut s, &install_req(&[("Alpha", None), ("Huge", None)])).unwrap_err();
    assert!(e.to_string().contains("Not enough free space"), "{e}");
    assert!(!env.has("GameData/Alpha") && s.registry.installed("Alpha").is_none());
    run(&mut s, &install_req(&[("Alpha", None)])).unwrap();
    let free = super::apply::free_bytes(&env.root.join("not/yet")).unwrap();
    assert!(free > 1 << 20);
}

/// Over the cache limit: a zip that no repo module uses goes before the ones just installed.
#[test]
fn cache_trim_keeps_repo_files() {
    let env = Env::new("trim", &basic_mods());
    let mut s = env.session();
    let stray = env.root.join("zips/stray.zip");
    make_zip(&stray, &[("x.cfg", &[7u8; 4000][..])]);
    let size = fs::metadata(&stray).unwrap().len();
    let cache_dir = env.root.join("cache");
    fs::copy(&stray, cache_dir.join("0BADBEEF-Stray-1.0.zip")).unwrap();
    let before = s.cache.total_size();
    s.config.cache_size_limit = Some(before - size);
    run(&mut s, &install_req(&[("Main", Some("1.0"))])).unwrap();
    assert!(!cache_dir.join("0BADBEEF-Stray-1.0.zip").exists());
    let main = s.index.get("Main", &ModuleVersion::new("1.0")).cloned().unwrap();
    assert!(s.cache.get(&main).is_some());
}

/// A run killed in the middle leaves CKAN/tx-*; the next open says so and names broken mods.
#[test]
fn interrupted_run_is_reported_on_open() {
    let env = Env::new("interrupted", &basic_mods());
    let mut s = env.session();
    run(&mut s, &install_req(&[("Main", Some("1.0"))])).unwrap();
    drop(s);
    let open = || Session::open_with(env.config(), None, LabelList::defaults(), env.root.join("repos"), false).unwrap();
    assert!(open().warnings.is_empty());
    let undo = env.game.join("CKAN/tx-999-0");
    fs::create_dir_all(&undo).unwrap();
    fs::rename(env.game.join("GameData/Main/main.cfg"), undo.join("0")).unwrap();
    let w = open().warnings.join("\n");
    assert!(w.contains("CKAN/tx-999-0") && w.contains("install them again: Main."), "{w}");
    fs::rename(undo.join("0"), env.game.join("GameData/Main/main.cfg")).unwrap();
    let w = open().warnings.join("\n");
    assert!(w.contains("No installed mod is missing files"), "{w}");
}

/// Ctrl-C during the file phase is held back, not lost and not acted on at once.
#[test]
fn signal_guard_defers_ctrl_c() {
    let g = super::guard::SignalGuard::new();
    // SAFETY: SIGINT goes to the guard's handler while `g` lives.
    unsafe { libc::raise(libc::SIGINT) };
    assert_eq!(super::guard::take_pending(), libc::SIGINT);
    drop(g);
}

/// Live: install ModuleManager from the real metadata over the network.
/// Run with `cargo test live_module_manager -- --ignored`. `HANGAR_TEST_META` can name a local
/// copy of the CKAN-meta archive; without it the archive is downloaded.
#[test]
#[ignore]
fn live_module_manager() {
    let uri = std::env::var("HANGAR_TEST_META").map_or_else(|_| Repository::default_repo().uri, |m| format!("file://{m}"));
    let env = Env::new("live", &[]);
    let mut s = Session::open_with(env.config(), None, LabelList::defaults(), env.root.join("repos"), false).unwrap();
    let repo = Repository { name: "KSP-default".into(), uri, priority: 0, x_mirror: false, x_comment: None };
    s.set_repositories(vec![repo]).unwrap();
    assert!(s.index.module_count() > 1000);
    let msgs = std::sync::Mutex::new(0usize);
    let progress = |_: &str, f: Option<f32>| {
        assert!(f.is_none_or(|f| (0.0..=1.0).contains(&f)));
        *msgs.lock().unwrap() += 1;
    };
    let cs = s.plan(&install_req(&[("ModuleManager", None)]), &ResolverOptions::default()).unwrap();
    let r = s.apply(&cs, &progress).unwrap();
    assert!(r.installed.iter().any(|x| x.starts_with("ModuleManager ")), "{r:?}");
    let im = s.registry.installed("ModuleManager").unwrap();
    assert!(im.installed_files.keys().any(|f| f.starts_with("GameData/ModuleManager") && f.ends_with(".dll")));
    for f in im.installed_files.keys() {
        assert!(env.game.join(f).exists(), "{f}");
    }
    assert!(*msgs.lock().unwrap() > 0);
    // Remove it again.
    run(&mut s, &Request { remove: vec!["ModuleManager".into()], ..Default::default() }).unwrap();
    assert!(s.registry.installed("ModuleManager").is_none());
}
