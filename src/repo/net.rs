//! Repo downloads: ETag checks, streamed atomic downloads, local file copies.

use std::collections::{BTreeMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use super::archive;
use super::{Progress, Repository, UpdateResult};
use crate::error::{Ctx, Error, Result};

pub const ETAGS_FILE: &str = "etags.json";

/// Cache file of a repo: `<dir>/<HASH8(uri)>-<name>.tar.gz` (name without `/`, `:` and NUL).
pub fn cache_file(repo: &Repository, dir: &Path) -> PathBuf {
    let name: String = repo.name.chars().map(|c| if matches!(c, '/' | '\0' | ':') { '_' } else { c }).collect();
    dir.join(format!("{}-{name}.tar.gz", crate::util::url_hash8(&repo.uri)))
}

/// The cache file of a repo, or the first `<HASH8>-*.tar.gz` file (the repo was renamed).
pub fn find_cache_file(repo: &Repository, dir: &Path) -> Option<PathBuf> {
    let exact = cache_file(repo, dir);
    if exact.is_file() {
        return Some(exact);
    }
    let prefix = format!("{}-", crate::util::url_hash8(&repo.uri));
    let rd = std::fs::read_dir(dir).ok()?;
    let mut found: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(&prefix) && n.ends_with(".tar.gz") && !n.contains(".part"))
        })
        .collect();
    found.sort();
    found.into_iter().next()
}

/// Local path of a `file://` URI or an absolute path; None for remote URIs.
pub fn local_path(uri: &str) -> Option<PathBuf> {
    if let Some(rest) = uri.strip_prefix("file://") {
        // file:///abs/path or file://localhost/abs/path
        let rest = rest.strip_prefix("localhost").unwrap_or(rest);
        return Some(PathBuf::from(percent_decode(rest)));
    }
    if uri.starts_with('/') {
        return Some(PathBuf::from(uri));
    }
    None
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while let Some(&c) = b.get(i) {
        if c == b'%' {
            let hex = b
                .get(i + 1..i + 3)
                .and_then(|h| std::str::from_utf8(h).ok())
                .and_then(|h| u8::from_str_radix(h, 16).ok());
            if let Some(v) = hex {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `{uri: etag}`; a missing or corrupt file gives an empty map.
pub fn load_etags(dir: &Path) -> BTreeMap<String, String> {
    std::fs::read(dir.join(ETAGS_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice::<BTreeMap<String, String>>(&b).ok())
        .unwrap_or_default()
}

fn strip_etag(s: &str) -> String {
    s.replace('"', "")
}

fn header_etag<B>(resp: &ureq::http::Response<B>) -> Option<String> {
    resp.headers().get("etag").and_then(|v| v.to_str().ok()).map(strip_etag).filter(|s| !s.is_empty())
}

/// ETag of a remote URI from a HEAD request.
pub(super) fn head_etag(agent: &ureq::Agent, uri: &str) -> Option<String> {
    let resp = agent.head(uri).call().ok()?;
    header_etag(&resp)
}

fn temp_path(dest: &Path) -> PathBuf {
    let mut s = dest.as_os_str().to_owned();
    s.push(format!(".part{}", std::process::id()));
    PathBuf::from(s)
}

/// Copy a stream into `dest` through a temp file; check the whole archive before the rename.
fn store_stream(
    src: &mut dyn Read,
    dest: &Path,
    total: Option<u64>,
    what: &str,
    progress: Progress,
) -> Result<()> {
    let tmp = temp_path(dest);
    let res = (|| -> Result<()> {
        let mut f = std::fs::File::create(&tmp).ctx(tmp.display())?;
        let mut buf = vec![0u8; 1 << 16];
        let mut done: u64 = 0;
        let mut magic: Vec<u8> = Vec::with_capacity(4);
        let mut last_report = 0u64;
        loop {
            let n = src.read(&mut buf).map_err(|e| Error::Net(format!("{what}: {e}")))?;
            if n == 0 {
                break;
            }
            let chunk = buf.get(..n).unwrap_or(&[]);
            if magic.len() < 4 {
                let need = 4 - magic.len();
                magic.extend(chunk.iter().take(need));
            }
            f.write_all(chunk).ctx(tmp.display())?;
            done += n as u64;
            if done - last_report >= 1 << 20 {
                last_report = done;
                let frac = total.filter(|t| *t > 0).map(|t| (done as f32 / t as f32).min(1.0));
                progress(&format!("Downloading {what}: {}", crate::util::human_size(done)), frac);
            }
        }
        f.sync_all().ctx(tmp.display())?;
        drop(f);
        if archive::detect(&magic).is_none() {
            return Err(Error::Metadata(format!("{what}: download is not a tar.gz or zip archive")));
        }
        // A cut or damaged download can start with good magic bytes: read it all back first.
        progress(&format!("Checking {what}"), None);
        archive::validate(&tmp)
    })();
    match res {
        Ok(()) => std::fs::rename(&tmp, dest).ctx(dest.display()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Download one remote repo. Returns the new ETag (quotes removed).
fn download(agent: &ureq::Agent, repo: &Repository, dest: &Path, progress: Progress) -> Result<Option<String>> {
    progress(&format!("Downloading {}", repo.name), None);
    let resp = agent.get(&repo.uri).call().map_err(|e| Error::Net(format!("{}: {e}", repo.uri)))?;
    let etag = header_etag(&resp);
    let body = resp.into_body();
    let total = body.content_length();
    let mut reader = body.into_reader();
    store_stream(&mut reader, dest, total, &repo.name, progress)?;
    Ok(etag)
}

/// Copy a local repo archive into the cache.
fn copy_local(repo: &Repository, src: &Path, dest: &Path, progress: Progress) -> Result<()> {
    progress(&format!("Reading {}", src.display()), None);
    let mut f = std::fs::File::open(src).ctx(src.display())?;
    let total = f.metadata().ok().map(|m| m.len());
    store_stream(&mut f, dest, total, &repo.name, progress)
}

/// Remove the caches of this repo stored under other names (the repo was renamed), so a
/// later lookup by prefix can never pick an old copy. Only `<HASH8(uri)>-*.tar.gz` files in
/// the cache dir are touched.
fn remove_other_caches(repo: &Repository, dir: &Path, keep: &Path) {
    let prefix = format!("{}-", crate::util::url_hash8(&repo.uri));
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for p in rd.flatten().map(|e| e.path()) {
        let is_cache = p
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with(&prefix) && n.ends_with(".tar.gz") && !n.contains(".part"));
        if is_cache && p != keep && p.is_file() {
            let _ = std::fs::remove_file(&p);
        }
    }
}

fn touch(path: &Path) {
    if let Ok(f) = std::fs::File::options().write(true).open(path) {
        let _ = f.set_modified(std::time::SystemTime::now());
    }
}

pub fn update(repos: &[Repository], cache_dir: &Path, force: bool, progress: Progress) -> Result<UpdateResult> {
    std::fs::create_dir_all(cache_dir).ctx(cache_dir.display())?;
    let mut etags = load_etags(cache_dir);
    let agent = super::http_agent();

    // Distinct by uri, in the given order.
    let mut seen = HashSet::new();
    let list: Vec<&Repository> = repos.iter().filter(|r| seen.insert(r.uri.as_str())).collect();

    let mut to_update: Vec<&Repository> = Vec::new();
    for r in &list {
        if force || local_path(&r.uri).is_some() {
            to_update.push(r);
            continue;
        }
        let stored = etags.get(&r.uri);
        let have_cache = find_cache_file(r, cache_dir).is_some();
        let stale = match (stored, have_cache) {
            (Some(old), true) => {
                progress(&format!("Checking {}", r.name), None);
                head_etag(&agent, &r.uri).is_none_or(|new| &new != old)
            }
            _ => true,
        };
        if stale {
            to_update.push(r);
        }
    }

    if to_update.is_empty() {
        for r in &list {
            if let Some(p) = find_cache_file(r, cache_dir) {
                touch(&p);
            }
        }
        return Ok(UpdateResult::NoChanges);
    }

    // Each repo is stored on its own; a failure keeps the old cache of that repo and its etag.
    let mut first_err: Option<Error> = None;
    let n = to_update.len();
    for (i, r) in to_update.iter().enumerate() {
        progress(&format!("Updating {} ({}/{n})", r.name, i + 1), Some(i as f32 / n as f32));
        let dest = cache_file(r, cache_dir);
        let res = match local_path(&r.uri) {
            Some(src) => copy_local(r, &src, &dest, progress).map(|()| None),
            None => download(&agent, r, &dest, progress),
        };
        match res {
            Ok(tag) => {
                remove_other_caches(r, cache_dir, &dest);
                match tag {
                    Some(t) => etags.insert(r.uri.clone(), t),
                    None => etags.remove(&r.uri),
                };
            }
            Err(e) => {
                if first_err.is_none() {
                    first_err = Some(e);
                }
            }
        }
    }
    crate::util::write_json(&cache_dir.join(ETAGS_FILE), &etags)?;
    progress("Repositories updated", Some(1.0));
    match first_err {
        Some(e) => Err(e),
        None => Ok(UpdateResult::Updated),
    }
}

pub fn fetch_repository_list() -> Result<Vec<Repository>> {
    let agent = super::http_agent();
    let resp = agent.get(super::REPO_LIST_URI).call().map_err(|e| Error::Net(format!("{}: {e}", super::REPO_LIST_URI)))?;
    let buf = resp.into_body().read_to_vec().map_err(|e| Error::Net(e.to_string()))?;
    let list = archive::parse_repository_list(&buf);
    if list.is_empty() {
        return Err(Error::Json("repositories.json has no repositories".into()));
    }
    Ok(list)
}
