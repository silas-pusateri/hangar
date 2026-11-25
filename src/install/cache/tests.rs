//! Cache tests (temp dirs only).

use std::fs;

use super::*;
use crate::install::tests::{make_zip, temp_dir};

fn module(json: &str) -> Module {
    Module::from_json(json.as_bytes()).unwrap()
}

#[test]
fn url_hash_matches_ckan() {
    // CKAN Tests/CmdLine/ImportTests.cs: 3108C916-DogeCoinPlugin-1.01.zip
    assert_eq!(url_hash("https://kerbalstuff.com/mod/269/Dogecoin%20Plugin/download/1.01"), "3108C916");
    // CKAN Tests/CmdLine/ShowTests.cs: D7B3438D for "https://github.com/"
    assert_eq!(url_hash("https://github.com/"), "D7B3438D");
    // .NET adds the "/" path.
    assert_eq!(url_hash("https://github.com"), "D7B3438D");
    assert_eq!(url_hash("https://github.com/KSP-CKAN/CKAN-meta/archive/master.tar.gz"), "4A85405A");
    let m = module(
        r#"{"identifier":"DogeCoinPlugin","version":"1.01",
        "download":"https://kerbalstuff.com/mod/269/Dogecoin%20Plugin/download/1.01"}"#,
    );
    assert_eq!(DownloadCache::file_name(&m), "3108C916-DogeCoinPlugin-1.01.zip");
}

#[test]
fn dotnet_uri_to_string() {
    let t = dotnet_uri_string;
    assert_eq!(
        t("https://spacedock.info/mod/2666/Approved%20Popular%20Pre-Launch%20Experiment%20%28A.P.P.L.E.%29/download/v1.1.8"),
        "https://spacedock.info/mod/2666/Approved Popular Pre-Launch Experiment (A.P.P.L.E.)/download/v1.1.8"
    );
    // `?`, `#` and `%` stay escaped in the path.
    assert_eq!(
        t("https://spacedock.info/mod/3806/What%20Do%20I%20Need%3F/download/0.0.4.3"),
        "https://spacedock.info/mod/3806/What Do I Need%3F/download/0.0.4.3"
    );
    assert_eq!(t("https://x/a%25b%23c"), "https://x/a%25b%23c");
    assert_eq!(t("https://x/100%"), "https://x/100%25");
    // UTF-8 escapes become text.
    assert_eq!(
        t("https://spacedock.info/mod/1979/Scott%20M%C3%BCnley%20-%20A/download/1.1"),
        "https://spacedock.info/mod/1979/Scott Münley - A/download/1.1"
    );
    assert_eq!(t("https://x/Auto-Strut%EF%BC%88AAS%EF%BC%89"), "https://x/Auto-Strut（AAS）");
    // Invalid UTF-8 stays escaped.
    assert_eq!(t("https://x/a%FFb"), "https://x/a%FFb");
    // Host and scheme lower case, default port dropped, dot segments removed.
    assert_eq!(t("HTTPS://GitHub.COM:443/a/./b/../c"), "https://github.com/a/c");
    assert_eq!(t("http://magico13.net:8080/job/Kerbal%20Construction%20Time/43"), "http://magico13.net:8080/job/Kerbal Construction Time/43");
    // Query keeps `#` escaped only; literal spaces pass through.
    assert_eq!(t("https://x/f.zip?a=%3F%26"), "https://x/f.zip?a=?&");
    assert_eq!(t("https://media.forgecdn.net/files/2277/410/AUC aeronautic part.zip"), "https://media.forgecdn.net/files/2277/410/AUC aeronautic part.zip");
}

#[test]
fn release_dates() {
    assert_eq!(parse_release_date("2023-05-01T00:00:00Z"), Some(1_682_899_200 * 1_000_000_000));
    assert_eq!(parse_release_date("2023-05-01T02:00:00+02:00"), Some(1_682_899_200 * 1_000_000_000));
    assert_eq!(parse_release_date("2023-05-01T00:00:00.5Z"), Some(1_682_899_200 * 1_000_000_000 + 500_000_000));
    assert_eq!(parse_release_date("2023-05-01T00:00:00.123456-01:30"), Some((1_682_899_200 + 5400) * 1_000_000_000 + 123_456_000));
    assert_eq!(parse_release_date("2023-05-01"), Some(1_682_899_200 * 1_000_000_000));
    assert_eq!(parse_release_date("1970-01-01T00:00:00Z"), Some(0));
    assert_eq!(parse_release_date("garbage"), None);
    assert_eq!(parse_release_date("2023-13-01"), None);
}

fn good_zip(dir: &Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    make_zip(&p, &[("Foo/", None), ("Foo/a.cfg", Some(b"hello"))]);
    p
}

#[test]
fn store_get_list_purge() {
    let d = temp_dir("cache");
    let cache = DownloadCache::new(d.join("downloads"));
    let src = good_zip(&d, "src.zip");
    let size = fs::metadata(&src).unwrap().len();
    let m = module(&format!(
        r#"{{"identifier":"Foo","version":"1:1.0 beta","download":["https://a.example/Foo%201.zip","https://b.example/f.zip"],
        "download_size":{size},"release_date":"2001-01-01T00:00:00Z"}}"#
    ));
    assert!(cache.get(&m).is_none());
    let p = cache.store(&m, &src, false).unwrap();
    assert!(src.exists());
    assert_eq!(p.file_name().unwrap().to_string_lossy(), format!("{}-Foo-1-1.0-beta.zip", url_hash("https://a.example/Foo 1.zip")));
    assert_eq!(cache.get(&m), Some(p.clone()));
    assert!(cache.is_cached(&m));
    assert_eq!(cache.index().get(&m), Some(p.as_path()));

    // Lookup by the second URL.
    let fallback = cache.dir.join(format!("{}-Bar-2.zip", url_hash("https://c.example/bar.zip")));
    fs::copy(&src, &fallback).unwrap();
    let m2 = module(r#"{"identifier":"Bar","version":"2","download":["https://x.example/none.zip","https://c.example/bar.zip"]}"#);
    assert_eq!(cache.get(&m2), Some(fallback.clone()));

    // Only hash-named files are listed; sidecars and junk are not.
    fs::write(cache.dir.join("notes.txt"), "x").unwrap();
    fs::write(cache.dir.join(format!("{}.sha1", p.file_name().unwrap().to_string_lossy())), "AB").unwrap();
    fs::write(cache.dir.join("abcdef12-lower.zip"), "x").unwrap();
    let list = cache.list();
    assert_eq!(list.len(), 2);
    assert_eq!(cache.total_size(), 2 * size);

    cache.purge(&m).unwrap();
    assert!(cache.get(&m).is_none());
    assert!(!cache.dir.join(format!("{}.sha1", p.file_name().unwrap().to_string_lossy())).exists());
    assert_eq!(cache.list().len(), 1);
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn store_checks_size_and_zip() {
    let d = temp_dir("cache-bad");
    let cache = DownloadCache::new(d.join("downloads"));
    let src = good_zip(&d, "src.zip");
    let m = module(r#"{"identifier":"Foo","version":"1","download":"https://a/f.zip","download_size":1}"#);
    assert!(matches!(cache.store(&m, &src, false), Err(Error::Hash(_))));
    let bad = d.join("bad.zip");
    fs::write(&bad, b"PK\x03\x04 not really").unwrap();
    let m = module(r#"{"identifier":"Foo","version":"1","download":"https://a/f.zip"}"#);
    assert!(cache.store(&m, &bad, false).is_err());
    assert!(cache.list().is_empty());
    // Move mode removes the source.
    let p = cache.store(&m, &src, true).unwrap();
    assert!(!src.exists() && p.exists());
    fs::remove_dir_all(&d).unwrap();
}

/// Storing a file that is already the cache file (or another cached copy of the module) must
/// not delete it before the copy; a copy never leaves a partial file under a cache name.
#[test]
fn store_from_inside_the_cache() {
    let d = temp_dir("cache-self");
    let cache = DownloadCache::new(d.join("downloads"));
    let src = good_zip(&d, "src.zip");
    let bytes = fs::read(&src).unwrap();
    let m = module(r#"{"identifier":"Foo","version":"1","download":["https://a/f.zip","https://b/f.zip"]}"#);
    let p = cache.store(&m, &src, false).unwrap();
    // Same file again, copy and move mode.
    assert_eq!(cache.store(&m, &p, false).unwrap(), p);
    assert_eq!(cache.store(&m, &p, true).unwrap(), p);
    assert_eq!(fs::read(&p).unwrap(), bytes);
    // A cached copy under the mirror URL hash, stored in copy mode: both stay.
    cache.purge(&m).unwrap();
    let mirror = cache.dir.join(format!("{}-Foo-1.zip", url_hash("https://b/f.zip")));
    fs::copy(&src, &mirror).unwrap();
    let p = cache.store(&m, &mirror, false).unwrap();
    assert_eq!(fs::read(&p).unwrap(), bytes);
    assert!(mirror.exists());
    // Move mode: the mirror copy is the source and goes away.
    cache.purge(&m).unwrap();
    fs::copy(&src, &mirror).unwrap();
    let p = cache.store(&m, &mirror, true).unwrap();
    assert_eq!(fs::read(&p).unwrap(), bytes);
    assert!(!mirror.exists());
    assert_eq!(cache.list().len(), 1);
    // No temp copies are left behind.
    let left: Vec<_> = fs::read_dir(cache.dir.join("downloading")).map(|r| r.flatten().count()).into_iter().collect();
    assert!(left.iter().all(|n| *n == 0), "{left:?}");
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn stale_file_is_deleted() {
    let d = temp_dir("cache-stale");
    let cache = DownloadCache::new(d.join("downloads"));
    let src = good_zip(&d, "src.zip");
    let m = module(r#"{"identifier":"Foo","version":"1","download":"https://a/f.zip","release_date":"2999-01-01T00:00:00Z"}"#);
    let p = cache.store(&m, &src, false).unwrap();
    assert!(p.exists());
    assert!(cache.get(&m).is_none());
    assert!(!p.exists());
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn trim_to_limit() {
    let d = temp_dir("cache-trim");
    let cache = DownloadCache::new(d.join("downloads"));
    fs::create_dir_all(&cache.dir).unwrap();
    for (h, n) in [("AAAAAAAA", 100), ("BBBBBBBB", 100), ("CCCCCCCC", 100)] {
        fs::write(cache.dir.join(format!("{h}-m-1.zip")), vec![0u8; n]).unwrap();
    }
    cache.trim(1000).unwrap();
    assert_eq!(cache.total_size(), 300);
    // Rank: B first, then A; C kept.
    cache.trim_ranked(100, &|h| match h { "BBBBBBBB" => 0, "AAAAAAAA" => 1, _ => 2 }).unwrap();
    let names: Vec<_> = cache.list().iter().map(|f| f.path.file_name().unwrap().to_string_lossy().into_owned()).collect();
    assert_eq!(names, vec!["CCCCCCCC-m-1.zip"]);
    cache.trim(0).unwrap();
    assert!(cache.list().is_empty());
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn download_errors_without_network_use() {
    let d = temp_dir("cache-dl");
    let cache = DownloadCache::new(d.join("downloads"));
    let agent = crate::repo::http_agent();
    let progress = |_: &str, _: Option<f32>| {};
    let m = module(r#"{"identifier":"Meta","version":"1","kind":"metapackage"}"#);
    assert!(cache.download(&m, &agent, &|_| None, &progress).is_err());
    // A closed local port fails fast.
    let m = module(r#"{"identifier":"Foo","version":"1","download":"http://127.0.0.1:9/f.zip"}"#);
    assert!(matches!(cache.download(&m, &agent, &|_| None, &progress), Err(Error::Net(_))));
    assert!(cache.list().is_empty());
    fs::remove_dir_all(&d).unwrap();
}

/// Serve one fixed response on a local port (no network use).
fn serve_once(body: Vec<u8>) -> String {
    use std::io::{Read, Write};
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    std::thread::spawn(move || {
        if let Ok((mut s, _)) = l.accept() {
            let mut buf = [0u8; 4096];
            let _ = s.read(&mut buf);
            let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            let _ = s.write_all(head.as_bytes());
            let _ = s.write_all(&body);
        }
    });
    format!("http://{addr}/files/My%20Mod.zip")
}

#[test]
fn download_from_local_server() {
    let d = temp_dir("cache-local");
    let cache = DownloadCache::new(d.join("downloads"));
    let src = good_zip(&d, "src.zip");
    let bytes = fs::read(&src).unwrap();
    let (_, sha256) = crate::util::file_hashes(&src).unwrap();
    let agent = crate::repo::http_agent();
    let progress = |_: &str, _: Option<f32>| {};

    // Wrong hash: rejected, nothing cached, partial removed.
    let url = serve_once(bytes.clone());
    let m = module(&format!(
        r#"{{"identifier":"Foo","version":"1","download":"{url}","download_hash":{{"sha256":"{}"}}}}"#,
        "0".repeat(64)
    ));
    assert!(matches!(cache.download(&m, &agent, &|_| None, &progress), Err(Error::Hash(_))));
    assert!(cache.list().is_empty());
    assert_eq!(fs::read_dir(cache.dir.join("downloading")).unwrap().count(), 0);

    // Right hash and size: cached under the URL hash, with sidecars.
    let url = serve_once(bytes.clone());
    let m = module(&format!(
        r#"{{"identifier":"Foo","version":"1","download":"{url}","download_size":{},"download_hash":{{"sha256":"{sha256}"}}}}"#,
        bytes.len()
    ));
    let p = cache.download(&m, &agent, &|_| None, &progress).unwrap();
    assert_eq!(fs::read(&p).unwrap(), bytes);
    assert_eq!(cache.get(&m), Some(p.clone()));
    assert!(p.file_name().unwrap().to_string_lossy().starts_with(&url_hash(&url)));
    let side = fs::read_to_string(format!("{}.sha256", p.display())).unwrap();
    assert_eq!(side, sha256);
    fs::remove_dir_all(&d).unwrap();
}

/// A complete-looking but bad partial file from an earlier run must not fail the download:
/// it is deleted and the same URL is fetched again from the start.
#[test]
fn bad_partial_is_refetched() {
    let d = temp_dir("cache-partial");
    let cache = DownloadCache::new(d.join("downloads"));
    let src = good_zip(&d, "src.zip");
    let bytes = fs::read(&src).unwrap();
    let (_, sha256) = crate::util::file_hashes(&src).unwrap();
    let agent = crate::repo::http_agent();
    let progress = |_: &str, _: Option<f32>| {};
    let url = serve_once(bytes.clone());
    let m = module(&format!(
        r#"{{"identifier":"Foo","version":"1","download":"{url}","download_size":{},"download_hash":{{"sha256":"{sha256}"}}}}"#,
        bytes.len()
    ));
    // Same size as the real file, wrong bytes: looks complete, so no request is made for it.
    let part = cache.partial_path(&m, &url);
    fs::create_dir_all(part.parent().unwrap()).unwrap();
    fs::write(&part, vec![0u8; bytes.len()]).unwrap();
    let p = cache.download(&m, &agent, &|_| None, &progress).unwrap();
    assert_eq!(fs::read(&p).unwrap(), bytes);
    assert!(!part.exists());
    fs::remove_dir_all(&d).unwrap();
}
