use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use super::net::{cache_file, load_etags, local_path};
use super::*;

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("hangar-repo-test-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn ckan(id: &str, ver: &str, extra: &str) -> String {
    format!(
        r#"{{"spec_version":1,"identifier":"{id}","name":"{id}","abstract":"a","author":"me","version":"{ver}",
        "license":"MIT","download":"https://x.test/{id}-{ver}.zip"{extra}}}"#
    )
}

fn sample_files() -> Vec<(String, Vec<u8>)> {
    vec![
        ("CKAN-meta-master/Foo/Foo-1.0.ckan".into(), ckan("Foo", "1.0", "").into_bytes()),
        (
            "CKAN-meta-master/Foo/Foo-2.0.ckan".into(),
            ckan("Foo", "2.0", r#","download_hash":{"sha1":"AB12","sha256":"CD34"}"#).into_bytes(),
        ),
        ("CKAN-meta-master/Bar/Bar-1.0.ckan".into(), ckan("Bar", "1.0", r#","provides":["Virt"]"#).into_bytes()),
        ("CKAN-meta-master/Bad/Bad-1.0.ckan".into(), b"{not json".to_vec()),
        ("CKAN-meta-master/Bad/Bad-2.0.ckan".into(), br#"{"identifier":"x","version":"1"}"#.to_vec()),
        ("CKAN-meta-master/Old/Old-1.0.frozen".into(), ckan("Old", "1.0", "").into_bytes()),
        ("CKAN-meta-master/download_counts.json".into(), br#"{"Foo": 10, "Bar": 3}"#.to_vec()),
        (
            "CKAN-meta-master/builds.json".into(),
            br#"{"builds":{"3190":"1.12.5.3190","1028":"1.0.5.1028","2047":"1.4.1.2047"}}"#.to_vec(),
        ),
        (
            "CKAN-meta-master/repositories.json".into(),
            br#"{"repositories":[{"name":"KSP-default","uri":"https://x.test/m.tar.gz","x_mirror":true}]}"#.to_vec(),
        ),
        ("CKAN-meta-master/README.md".into(), b"hi".to_vec()),
    ]
}

fn write_tar_gz(path: &Path, files: &[(String, Vec<u8>)]) {
    let f = std::fs::File::create(path).unwrap();
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

fn write_zip(path: &Path, files: &[(String, Vec<u8>)]) {
    let f = std::fs::File::create(path).unwrap();
    let mut z = zip::ZipWriter::new(f);
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, data) in files {
        z.start_file(name.as_str(), opts).unwrap();
        z.write_all(data).unwrap();
    }
    z.finish().unwrap();
}

fn check_sample(d: &RepoData) {
    assert_eq!(d.modules.len(), 3);
    assert_eq!(d.skipped, 2);
    assert_eq!(d.download_counts.get("Foo"), Some(&10));
    assert_eq!(d.builds.get("3190").map(|v| v.to_string()), Some("1.12.5.3190".into()));
    assert_eq!(d.repositories.len(), 1);
    assert!(d.repositories[0].x_mirror);
    assert_eq!(d.repositories[0].priority, 0);
}

#[test]
fn parse_tar_gz_and_zip() {
    let dir = tmp("parse");
    let t = dir.join("a.tar.gz");
    write_tar_gz(&t, &sample_files());
    check_sample(&parse_archive(&t).unwrap());
    let z = dir.join("a.zip");
    write_zip(&z, &sample_files());
    check_sample(&parse_archive(&z).unwrap());
    let bad = dir.join("bad.tar.gz");
    std::fs::write(&bad, b"<html>").unwrap();
    assert!(parse_archive(&bad).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

fn module(id: &str, ver: &str, abs: &str) -> Module {
    let j = format!(
        r#"{{"identifier":"{id}","version":"{ver}","abstract":"{abs}","download":"https://x/{id}.zip"}}"#
    );
    Module::from_json(j.as_bytes()).unwrap()
}

fn repo(name: &str, prio: i32) -> Repository {
    Repository { name: name.into(), uri: format!("https://{name}.test/m.tar.gz"), priority: prio, x_mirror: false, x_comment: None }
}

#[test]
fn build_merges_by_priority() {
    let arcs = |v: Vec<Module>| v.into_iter().map(Arc::new).collect::<Vec<_>>();
    let mut low = RepoData { modules: arcs(vec![module("Foo", "1.0", "low"), module("Foo", "3.0", "low")]), ..Default::default() };
    low.download_counts.insert("Foo".into(), 1);
    low.builds.insert("3190".into(), GameVersion::new(1, 12, 5, 3190));
    let mut high = RepoData {
        modules: arcs(vec![module("Foo", "1.0", "high"), module("Foo", "2.0", "high"), module("Baz", "1", "high")]),
        ..Default::default()
    };
    high.download_counts.insert("Foo".into(), 99);
    high.builds.insert("1028".into(), GameVersion::new(1, 0, 5, 1028));
    // Given in the wrong order on purpose: priority decides.
    let idx = RepoIndex::build(vec![(repo("b", 1), low), (repo("a", 0), high)]);
    let vs: Vec<String> = idx.versions("Foo").iter().map(|m| m.version.to_string()).collect();
    assert_eq!(vs, ["3.0", "2.0", "1.0"]);
    assert_eq!(idx.versions("Foo")[2].abstract_, "high");
    assert_eq!(idx.download_count("Foo"), 99);
    assert_eq!(idx.providers_of("Foo"), ["Foo"]);
    // Repo builds win over the embedded map; the embedded map fills the rest.
    let mut b2 = RepoData::default();
    b2.builds.insert("3190".into(), GameVersion::new(9, 9, 9, 3190));
    let idx2 = RepoIndex::build(vec![(repo("c", 0), b2)]);
    assert_eq!(idx2.build_version("3190"), Some(GameVersion::new(9, 9, 9, 3190)));
    assert_eq!(idx2.build_version("1028"), Some(GameVersion::new(1, 0, 5, 1028)));
    assert!(idx.known_game_versions.windows(2).all(|w| w[0] < w[1]));
    assert!(idx.known_game_versions.contains(&GameVersion::new(1, 12, 5, 3190)));
    assert!(RepoIndex::build(Vec::new()).known_game_versions.len() > 100);
    assert_eq!(idx.module_count(), 4);
}

#[test]
fn providers_and_hash() {
    let mut d = RepoData::default();
    let mut a = module("AA", "1", "");
    a.provides = vec!["Virt".into()].into();
    let mut a2 = module("AA", "2", "");
    a2.provides = vec!["Virt".into()].into();
    let mut b = module("BB", "1", "");
    b.provides = vec!["Virt".into()].into();
    b.download_hash = Some(Box::new(crate::meta::DownloadHash { sha1: Some("ABCD".into()), sha256: Some("EF01".into()) }));
    d.modules = vec![Arc::new(a), Arc::new(a2), Arc::new(b)];
    let idx = RepoIndex::build(vec![(repo("x", 0), d)]);
    assert_eq!(idx.providers_of("Virt"), ["AA", "BB"]);
    assert_eq!(idx.providers_of("BB"), ["BB"]);
    assert!(idx.providers_of("Nope").is_empty());
    assert_eq!(idx.find_by_hash("abcd", "").len(), 1);
    assert_eq!(idx.find_by_hash("", "ef01").len(), 1);
    assert_eq!(idx.find_by_hash("0000", "EF01")[0].identifier, "BB");
    assert!(idx.find_by_hash("", "").is_empty());
}

#[test]
fn local_update_and_load() {
    let dir = tmp("local");
    let src = dir.join("my repo.tar.gz");
    write_tar_gz(&src, &sample_files());
    let cache = dir.join("cache");
    let uri = format!("file://{}", src.display()).replace(' ', "%20");
    assert_eq!(local_path(&uri), Some(src.clone()));
    let r = Repository { name: "local".into(), uri, priority: 0, x_mirror: false, x_comment: None };
    // Not updated yet: load_index reads the local file directly.
    let idx = load_index(std::slice::from_ref(&r), &cache).unwrap();
    assert_eq!(idx.versions("Foo").len(), 2);
    let res = update(std::slice::from_ref(&r), &cache, false, &|_, _| {}).unwrap();
    assert_eq!(res, UpdateResult::Updated);
    let cf = cache_file(&r, &cache);
    assert!(cf.is_file());
    assert!(cf.file_name().unwrap().to_str().unwrap().ends_with("-local.tar.gz"));
    let idx = load_index(&[r], &cache).unwrap();
    assert_eq!(idx.versions("Foo")[0].version.to_string(), "2.0");
    assert_eq!(idx.repositories.len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Minimal HTTP/1.1 server: HEAD and GET with an ETag. Counts GETs.
fn serve(body: Vec<u8>, etag: &'static str) -> (String, Arc<AtomicUsize>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let gets = Arc::new(AtomicUsize::new(0));
    let g2 = Arc::clone(&gets);
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut line = String::new();
            if r.read_line(&mut line).is_err() {
                continue;
            }
            loop {
                let mut h = String::new();
                if r.read_line(&mut h).is_err() || h == "\r\n" || h.is_empty() {
                    break;
                }
            }
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nETag: \"{etag}\"\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = s.write_all(head.as_bytes());
            if line.starts_with("GET") {
                g2.fetch_add(1, Ordering::SeqCst);
                let _ = s.write_all(&body);
            }
        }
    });
    (format!("http://{addr}/master.tar.gz"), gets)
}

#[test]
fn remote_etag_flow() {
    let dir = tmp("remote");
    let src = dir.join("m.tar.gz");
    write_tar_gz(&src, &sample_files());
    let (uri, gets) = serve(std::fs::read(&src).unwrap(), "v1");
    let cache = dir.join("cache");
    let r = Repository { name: "web".into(), uri: uri.clone(), priority: 0, x_mirror: false, x_comment: None };
    let repos = [r];
    assert_eq!(update(&repos, &cache, false, &|_, _| {}).unwrap(), UpdateResult::Updated);
    assert_eq!(load_etags(&cache).get(&uri).map(String::as_str), Some("v1"));
    // Same ETag: no download.
    assert_eq!(update(&repos, &cache, false, &|_, _| {}).unwrap(), UpdateResult::NoChanges);
    assert_eq!(gets.load(Ordering::SeqCst), 1);
    // Force: download again.
    assert_eq!(update(&repos, &cache, true, &|_, _| {}).unwrap(), UpdateResult::Updated);
    assert_eq!(gets.load(Ordering::SeqCst), 2);
    check_sample(&parse_archive(&cache_file(&repos[0], &cache)).unwrap());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bad_download_keeps_old_cache() {
    let dir = tmp("baddl");
    let (uri, _) = serve(b"<html>rate limited</html>".to_vec(), "x");
    let cache = dir.join("cache");
    let r = Repository { name: "web".into(), uri, priority: 0, x_mirror: false, x_comment: None };
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::write(cache_file(&r, &cache), b"\x1f\x8bold").unwrap();
    assert!(update(std::slice::from_ref(&r), &cache, true, &|_, _| {}).is_err());
    assert_eq!(std::fs::read(cache_file(&r, &cache)).unwrap(), b"\x1f\x8bold");
    let left: Vec<_> = std::fs::read_dir(&cache).unwrap().flatten().map(|e| e.file_name()).collect();
    assert!(left.iter().all(|n| !n.to_string_lossy().contains(".part")));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn real_metadata_load_time() {
    let Some(meta) = std::env::var_os("HANGAR_TEST_META") else { return };
    let meta = PathBuf::from(meta);
    let r = Repository { name: "real".into(), uri: format!("file://{}", meta.display()), priority: 0, x_mirror: false, x_comment: None };
    let cache = tmp("real");
    // Warm the file cache, then time the real load.
    let _ = parse_archive(&meta).unwrap();
    let t = std::time::Instant::now();
    let idx = load_index(std::slice::from_ref(&r), &cache).unwrap();
    let el = t.elapsed();
    let data_t = std::time::Instant::now();
    let data = parse_archive(&meta).unwrap();
    let parse_el = data_t.elapsed();
    eprintln!(
        "load_index: {:.1} ms (parse_archive alone {:.1} ms), {} ids, {} versions, {} skipped, {} builds, {} counts",
        el.as_secs_f64() * 1000.0,
        parse_el.as_secs_f64() * 1000.0,
        idx.modules.len(),
        idx.module_count(),
        data.skipped,
        idx.builds.len(),
        idx.download_counts.len()
    );
    // The real GitHub tarball (pax headers, 50 MB of tar) passes the full download check.
    let vt = std::time::Instant::now();
    archive::validate(&meta).unwrap();
    eprintln!("validate: {:.1} ms", vt.elapsed().as_secs_f64() * 1000.0);
    assert!(idx.modules.len() > 2000);
    assert!(idx.module_count() + data.skipped > 30_000);
    assert!(!idx.repositories.is_empty());
    assert_eq!(idx.build_version("3190").map(|v| v.to_string()), Some("1.12.5.3190".into()));
    assert!(idx.versions("ModuleManager").len() > 10);
    assert!(idx.providers_of("ModuleManager").contains(&"ModuleManager".to_string()));
    // A real hash: the first ModuleManager version that has one.
    let mm = idx.versions("ModuleManager").iter().find(|m| m.download_hash.as_ref().is_some_and(|h| h.sha256.is_some()));
    if let Some(mm) = mm {
        let h = mm.download_hash.as_ref().unwrap();
        let found = idx.find_by_hash("", h.sha256.as_deref().unwrap());
        assert!(found.iter().any(|m| m.same_release(mm)));
    }
    let _ = std::fs::remove_dir_all(&cache);
}

/// A cut download (right magic bytes, broken gzip stream) must not replace a good cache.
#[test]
fn truncated_archive_keeps_old_cache() {
    let dir = tmp("trunc");
    let good = dir.join("good.tar.gz");
    write_tar_gz(&good, &sample_files());
    let bytes = std::fs::read(&good).unwrap();
    let src = dir.join("repo.tar.gz");
    let cache = dir.join("cache");
    let r = Repository {
        name: "local".into(),
        uri: format!("file://{}", src.display()),
        priority: 0,
        x_mirror: false,
        x_comment: None,
    };
    std::fs::write(&src, &bytes).unwrap();
    update(std::slice::from_ref(&r), &cache, false, &|_, _| {}).unwrap();
    // Now the source is cut in half: it still starts with the gzip magic.
    std::fs::write(&src, &bytes[..bytes.len() / 2]).unwrap();
    assert!(archive::detect(&bytes[..4]).is_some());
    assert!(archive::validate(&src).is_err());
    assert!(update(std::slice::from_ref(&r), &cache, false, &|_, _| {}).is_err());
    assert_eq!(std::fs::read(cache_file(&r, &cache)).unwrap(), bytes);
    check_sample(&parse_archive(&cache_file(&r, &cache)).unwrap());
    // Only the gzip trailer missing: the CRC check still catches it.
    std::fs::write(&src, &bytes[..bytes.len() - 4]).unwrap();
    assert!(archive::validate(&src).is_err());
    // A cut zip fails too; a whole one passes.
    let z = dir.join("a.zip");
    write_zip(&z, &sample_files());
    assert!(archive::validate(&z).is_ok());
    let zb = std::fs::read(&z).unwrap();
    std::fs::write(&z, &zb[..zb.len() - 10]).unwrap();
    assert!(archive::validate(&z).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A renamed repo leaves no old cache behind, so renaming it back cannot load stale data.
#[test]
fn renamed_repo_drops_old_cache() {
    let dir = tmp("rename");
    let src = dir.join("repo.tar.gz");
    write_tar_gz(&src, &sample_files());
    let cache = dir.join("cache");
    let uri = format!("file://{}", src.display());
    let a = Repository { name: "A".into(), uri: uri.clone(), priority: 0, x_mirror: false, x_comment: None };
    let b = Repository { name: "B".into(), ..a.clone() };
    update(std::slice::from_ref(&a), &cache, false, &|_, _| {}).unwrap();
    assert!(cache_file(&a, &cache).is_file());
    update(std::slice::from_ref(&b), &cache, false, &|_, _| {}).unwrap();
    assert!(cache_file(&b, &cache).is_file());
    assert!(!cache_file(&a, &cache).exists());
    // New data, then back to the old name: the index must show the new data.
    let mut files = sample_files();
    files.push(("CKAN-meta-master/Foo/Foo-3.0.ckan".into(), ckan("Foo", "3.0", "").into_bytes()));
    write_tar_gz(&src, &files);
    update(std::slice::from_ref(&b), &cache, false, &|_, _| {}).unwrap();
    let idx = load_index(std::slice::from_ref(&a), &cache).unwrap();
    assert_eq!(idx.versions("Foo")[0].version.to_string(), "3.0");
    update(std::slice::from_ref(&a), &cache, false, &|_, _| {}).unwrap();
    assert!(!cache_file(&b, &cache).exists());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Live: GitHub answers a HEAD on the default repo (after redirects) with an ETag.
/// Run by hand: `cargo test repo::tests::live_github_etag -- --ignored`.
#[test]
#[ignore]
fn live_github_etag() {
    let agent = http_agent();
    let tag = net::head_etag(&agent, DEFAULT_REPO_URI);
    eprintln!("ETag: {tag:?}");
    assert!(tag.is_some_and(|t| !t.is_empty() && !t.contains('"')));
}

/// Load time of the real index (parse_archive + RepoIndex::build), best of 7.
/// Run: `HANGAR_TEST_META=<master.tar.gz> cargo test --release repo::tests::real_load_time -- --ignored --nocapture`.
#[test]
#[ignore]
fn real_load_time() {
    let Some(path) = std::env::var_os("HANGAR_TEST_META") else { return };
    let path = std::path::Path::new(&path);
    let mut best_parse = std::time::Duration::MAX;
    let mut best_all = std::time::Duration::MAX;
    for _ in 0..7 {
        let t = std::time::Instant::now();
        let data = parse_archive(path).unwrap();
        let tp = t.elapsed();
        let idx = RepoIndex::build(vec![(Repository::default_repo(), data)]);
        best_parse = best_parse.min(tp);
        best_all = best_all.min(t.elapsed());
        assert!(idx.module_count() > 30_000);
    }
    eprintln!("parse_archive: {best_parse:?}, parse + build: {best_all:?}");
}
