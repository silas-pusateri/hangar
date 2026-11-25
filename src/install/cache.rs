//! Download cache with CKAN file naming: `<HASH8(url)>-<identifier>-<version>.zip`,
//! where HASH8 = first 8 chars of uppercase SHA1 of the URL with `%XX` unescaped as .NET
//! Uri.ToString() does. Partial downloads go to `<cache>/downloading/`.

use std::collections::HashMap;
use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sha1::Digest as _;

use crate::error::{Ctx, Error, Result};
use crate::meta::Module;
use crate::repo::Progress;
use crate::util::{hex_upper, url_hash8};

#[derive(Debug, Clone)]
pub struct DownloadCache {
    pub dir: PathBuf,
}

/// One file in the cache.
#[derive(Debug, Clone)]
pub struct CachedFile {
    pub path: PathBuf,
    pub size: u64,
}

// ---------------------------------------------------------------- .NET Uri.ToString

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Bidi control chars stay escaped in .NET IRI unescaping.
fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}')
}

/// Unescape `%XX` like .NET Uri.ToString (V1ToStringUnescape): ASCII is unescaped except `%`
/// and the chars in `keep`; non-ASCII bytes are unescaped when they form valid UTF-8.
/// A `%` that does not start an escape becomes `%25` (the Uri constructor escapes it).
fn unescape_part(s: &str, keep: &[u8], out: &mut String) {
    let b = s.as_bytes();
    let esc = |i: usize| -> Option<u8> {
        if b.get(i) != Some(&b'%') {
            return None;
        }
        Some(hex_val(*b.get(i + 1)?)? * 16 + hex_val(*b.get(i + 2)?)?)
    };
    let mut i = 0;
    let mut plain_start = 0;
    while i < b.len() {
        if b[i] != b'%' {
            i += 1;
            continue;
        }
        out.push_str(s.get(plain_start..i).unwrap_or(""));
        match esc(i) {
            None => {
                out.push_str("%25");
                i += 1;
            }
            Some(v) if v < 0x80 => {
                if v == b'%' || keep.contains(&v) {
                    out.push_str(s.get(i..i + 3).unwrap_or(""));
                } else {
                    out.push(v as char);
                }
                i += 3;
            }
            Some(_) => {
                // A run of escaped high bytes: decode the valid UTF-8 parts.
                let start = i;
                let mut bytes = Vec::new();
                while let Some(v) = esc(i).filter(|v| *v >= 0x80) {
                    bytes.push(v);
                    i += 3;
                }
                let mut pos = start;
                for chunk in bytes.utf8_chunks() {
                    for c in chunk.valid().chars() {
                        let n = c.len_utf8() * 3;
                        if is_bidi_control(c) {
                            out.push_str(s.get(pos..pos + n).unwrap_or(""));
                        } else {
                            out.push(c);
                        }
                        pos += n;
                    }
                    let n = chunk.invalid().len() * 3;
                    out.push_str(s.get(pos..pos + n).unwrap_or(""));
                    pos += n;
                }
            }
        }
        plain_start = i;
    }
    out.push_str(s.get(plain_start..).unwrap_or(""));
}

/// RFC 3986 remove_dot_segments (the .NET Uri compresses `.` and `..` in http paths).
fn remove_dot_segments(path: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let segs: Vec<&str> = path.split('/').collect();
    let last = segs.len().saturating_sub(1);
    for (i, seg) in segs.iter().enumerate() {
        match *seg {
            "." => {
                if i == last {
                    out.push("");
                }
            }
            ".." => {
                if out.len() > 1 {
                    out.pop();
                }
                if i == last {
                    out.push("");
                }
            }
            s => out.push(s),
        }
    }
    let r = out.join("/");
    if r.starts_with('/') { r } else { format!("/{r}") }
}

/// The text .NET `new Uri(url).ToString()` returns, for absolute URLs: scheme and host in
/// lower case, default port dropped, empty path becomes `/`, `\` becomes `/` and dot segments
/// are removed in http(s) paths, then `%XX` is unescaped (see `unescape_part`).
/// UNVERIFIED against a real .NET runtime beyond `%20` and the empty path (see tests).
pub fn dotnet_uri_string(url: &str) -> String {
    let Some(colon) = url.find("://") else {
        let mut out = String::with_capacity(url.len());
        unescape_part(url, b"#?", &mut out);
        return out;
    };
    let scheme = url.get(..colon).unwrap_or("").to_ascii_lowercase();
    let rest = url.get(colon + 3..).unwrap_or("");
    let auth_end = rest.find(['/', '?', '#', '\\']).unwrap_or(rest.len());
    let authority = rest.get(..auth_end).unwrap_or("");
    let tail = rest.get(auth_end..).unwrap_or("");

    // Authority: keep userinfo, lower-case the host, drop a default port.
    let (userinfo, hostport) = match authority.rfind('@') {
        Some(i) => (authority.get(..=i).unwrap_or(""), authority.get(i + 1..).unwrap_or("")),
        None => ("", authority),
    };
    let mut host = hostport.to_ascii_lowercase();
    if let Some(i) = host.rfind(':').filter(|i| !host.get(*i..).unwrap_or("").contains(']')) {
        let port = host.get(i + 1..).unwrap_or("").to_string();
        let default = matches!((scheme.as_str(), port.as_str()), ("http", "80") | ("https", "443") | ("ftp", "21"));
        if default || port.is_empty() {
            host.truncate(i);
        }
    }

    let q = tail.find(['?', '#']).unwrap_or(tail.len());
    let web = matches!(scheme.as_str(), "http" | "https" | "ftp" | "file");
    let mut path = tail.get(..q).unwrap_or("").to_string();
    let after = tail.get(q..).unwrap_or("");
    if web {
        path = remove_dot_segments(&path.replace('\\', "/"));
    }
    let (query, fragment) = match after.find('#') {
        Some(i) => (after.get(..i).unwrap_or(""), after.get(i..).unwrap_or("")),
        None => (after, ""),
    };

    let mut out = String::with_capacity(url.len());
    out.push_str(&scheme);
    out.push_str("://");
    out.push_str(userinfo);
    out.push_str(&host);
    unescape_part(&path, b"#?", &mut out);
    unescape_part(query, b"#", &mut out);
    unescape_part(fragment, b"", &mut out);
    out
}

/// CKAN cache key of a download URL.
pub fn url_hash(url: &str) -> String {
    url_hash8(&dotnet_uri_string(url))
}

// ---------------------------------------------------------------- time helpers

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Parse an ISO 8601 date-time (`2023-05-01T12:00:00Z`, `...12:00:00.123+02:00`, or a date)
/// to nanoseconds since 1970 UTC. No zone means UTC.
pub fn parse_release_date(s: &str) -> Option<i128> {
    let s = s.trim();
    let num = |a: usize, b: usize| -> Option<i64> {
        let t = s.get(a..b)?;
        if t.bytes().all(|c| c.is_ascii_digit()) && !t.is_empty() { t.parse().ok() } else { None }
    };
    let (y, mo, d) = (num(0, 4)?, num(5, 7)? as u32, num(8, 10)? as u32);
    if s.get(4..5) != Some("-") || s.get(7..8) != Some("-") || !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    let mut secs = days_from_civil(y, mo, d) * 86_400;
    let mut nanos: i128 = 0;
    if s.len() > 10 {
        if !matches!(s.get(10..11), Some("T" | "t" | " ")) {
            return None;
        }
        secs += num(11, 13)? * 3600 + num(14, 16)? * 60;
        let mut i = 16;
        if s.get(16..17) == Some(":") {
            secs += num(17, 19)?;
            i = 19;
        }
        if s.get(i..i + 1) == Some(".") {
            let frac_end = s.get(i + 1..)?.find(|c: char| !c.is_ascii_digit()).map_or(s.len(), |k| i + 1 + k);
            let frac = s.get(i + 1..frac_end)?;
            let digits: String = frac.chars().chain(std::iter::repeat('0')).take(9).collect();
            nanos = digits.parse::<i128>().ok()?;
            i = frac_end;
        }
        match s.get(i..) {
            None | Some("") | Some("Z") | Some("z") => {}
            Some(z) => {
                let sign = match z.get(..1) {
                    Some("+") => 1,
                    Some("-") => -1,
                    _ => return None,
                };
                let zz = z.get(1..)?.replace(':', "");
                let h: i64 = zz.get(..2)?.parse().ok()?;
                let m: i64 = zz.get(2..4).unwrap_or("0").parse().ok()?;
                secs -= sign * (h * 3600 + m * 60);
            }
        }
    }
    Some(secs as i128 * 1_000_000_000 + nanos)
}

fn system_nanos(t: SystemTime) -> i128 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_nanos() as i128,
        Err(e) => -(e.duration().as_nanos() as i128),
    }
}

// ---------------------------------------------------------------- zip check

/// Read every entry fully (checks CRCs). Error text says what is wrong.
pub fn zip_valid(path: &Path) -> Result<()> {
    let bad = |e: String| Error::Hash(format!("{} is not a valid zip: {e}", path.display()));
    let f = fs::File::open(path).ctx(path.display())?;
    let mut za = zip::ZipArchive::new(std::io::BufReader::new(f)).map_err(|e| bad(e.to_string()))?;
    for i in 0..za.len() {
        let mut zf = za.by_index(i).map_err(|e| bad(e.to_string()))?;
        if zf.is_dir() {
            continue;
        }
        std::io::copy(&mut zf, &mut std::io::sink()).map_err(|e| bad(e.to_string()))?;
    }
    Ok(())
}

// ---------------------------------------------------------------- the cache

fn is_cache_name(name: &str) -> bool {
    let b = name.as_bytes();
    b.len() > 9
        && b.get(..8).is_some_and(|h| h.iter().all(|c| c.is_ascii_digit() || (b'A'..=b'F').contains(c)))
        && b.get(8) == Some(&b'-')
        && !name.ends_with(".sha1")
        && !name.ends_with(".sha256")
}

fn remove_with_sidecars(path: &Path) {
    let _ = fs::remove_file(path);
    for ext in ["sha1", "sha256"] {
        let mut s = path.as_os_str().to_owned();
        s.push(format!(".{ext}"));
        let _ = fs::remove_file(PathBuf::from(s));
    }
}

/// Set a file's modification time to now (errors are ignored).
fn touch(path: &Path) {
    if let Ok(f) = fs::OpenOptions::new().write(true).open(path) {
        let _ = f.set_modified(SystemTime::now());
    }
}

/// True when both paths name the same existing file (same device and inode).
fn same_file(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (fs::metadata(a), fs::metadata(b)) {
        (Ok(x), Ok(y)) => x.dev() == y.dev() && x.ino() == y.ino(),
        _ => false,
    }
}

/// True when the module's release date is at or after the file's modification time.
fn is_stale(m: &Module, path: &Path) -> bool {
    let Some(rel) = m.release_date.as_deref().and_then(parse_release_date) else {
        return false;
    };
    match fs::metadata(path).and_then(|md| md.modified()) {
        Ok(t) => rel >= system_nanos(t),
        Err(_) => false,
    }
}

/// A snapshot of the cache dir: hash prefix -> file. Build once with `DownloadCache::index`
/// for bulk "is cached" checks (one dir scan instead of one per module).
#[derive(Debug, Clone, Default)]
pub struct CacheIndex {
    files: HashMap<String, PathBuf>,
}

impl CacheIndex {
    /// Cached zip for this module (any download URL), without the staleness check.
    pub fn get(&self, m: &Module) -> Option<&Path> {
        m.download.iter().find_map(|u| self.files.get(&url_hash(u)).map(PathBuf::as_path))
    }


}

impl DownloadCache {
    pub fn new(dir: PathBuf) -> DownloadCache {
        DownloadCache { dir }
    }

    /// Cache file name for this module (uses `download[0]`).
    pub fn file_name(m: &Module) -> String {
        let url = m.download.first().map(String::as_str).unwrap_or("");
        format!("{}-{}", url_hash(url), m.standard_name())
    }

    /// Hash-named files in the cache dir, in directory order.
    fn entries(&self) -> Vec<(String, PathBuf, u64)> {
        let Ok(rd) = fs::read_dir(&self.dir) else { return Vec::new() };
        rd.flatten()
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                let md = e.metadata().ok()?;
                (md.is_file() && is_cache_name(&name)).then(|| (name, e.path(), md.len()))
            })
            .collect()
    }

    /// Scan the cache dir once (first file per hash prefix wins, as in CKAN).
    pub fn index(&self) -> CacheIndex {
        let mut files = HashMap::new();
        for (name, path, _) in self.entries() {
            if let Some(h) = name.get(..8) {
                files.entry(h.to_string()).or_insert(path);
            }
        }
        CacheIndex { files }
    }

    /// Cached zip for this module (any of its download URLs), if present.
    /// A file older than the module's `release_date` is stale: it is deleted and skipped.
    pub fn get(&self, m: &Module) -> Option<PathBuf> {
        if m.download.is_empty() {
            return None;
        }
        let idx = self.index();
        for u in &m.download {
            let Some(p) = idx.files.get(&url_hash(u)) else { continue };
            if is_stale(m, p) {
                remove_with_sidecars(p);
                continue;
            }
            return Some(p.clone());
        }
        None
    }

    #[cfg(test)]
    pub fn is_cached(&self, m: &Module) -> bool {
        self.get(m).is_some()
    }

    /// Delete any cached file under this module's URL hashes, except the files in `keep`.
    fn remove_cached(&self, m: &Module, keep: &[&Path]) {
        let hashes: Vec<String> = m.download.iter().map(|u| url_hash(u)).collect();
        for (name, path, _) in self.entries() {
            if name.get(..8).is_some_and(|h| hashes.iter().any(|x| x == h))
                && !keep.iter().any(|k| same_file(k, &path))
            {
                remove_with_sidecars(&path);
            }
        }
    }

    /// Check size (if known) and that the zip opens and all CRCs match.
    fn validate(m: &Module, path: &Path) -> Result<()> {
        let len = fs::metadata(path).ctx(path.display())?.len();
        if m.download_size > 0 && len != m.download_size {
            return Err(Error::Hash(format!("{m}: size is {len} bytes, metadata says {}", m.download_size)));
        }
        zip_valid(path)
    }

    /// Put a checked file into the cache (rename, or copy) and set its time to now.
    /// A copy goes to `downloading/` first and is renamed into place, so a cut-off copy never
    /// leaves a broken zip under a cache name. The old cached file is removed only after the
    /// copy worked, and never when it is `src` itself.
    fn put(&self, m: &Module, src: &Path, move_file: bool) -> Result<PathBuf> {
        fs::create_dir_all(&self.dir).ctx(self.dir.display())?;
        let target = self.dir.join(Self::file_name(m));
        if same_file(src, &target) {
            touch(&target);
            self.remove_cached(m, &[&target]);
            return Ok(target);
        }
        let moved = move_file && fs::rename(src, &target).is_ok();
        if !moved {
            let tmp_dir = self.dir.join("downloading");
            fs::create_dir_all(&tmp_dir).ctx(tmp_dir.display())?;
            // Not "<hash>-...": partial_path must not take this for a partial download.
            let tmp = tmp_dir.join(format!("copy-{}-{}", std::process::id(), Self::file_name(m)));
            if let Err(e) = fs::copy(src, &tmp) {
                let _ = fs::remove_file(&tmp);
                return Err(Error::Io(format!("{}: {e}", tmp.display())));
            }
            touch(&tmp);
            remove_with_sidecars(&target);
            if let Err(e) = fs::rename(&tmp, &target) {
                let _ = fs::remove_file(&tmp);
                return Err(Error::Io(format!("{}: {e}", target.display())));
            }
            if move_file {
                let _ = fs::remove_file(src);
            }
        } else {
            // A moved file keeps its old time; a fresh time keeps the release_date rule from
            // deleting an imported file.
            touch(&target);
        }
        for ext in ["sha1", "sha256"] {
            let mut s = target.as_os_str().to_owned();
            s.push(format!(".{ext}"));
            let _ = fs::remove_file(PathBuf::from(s));
        }
        // Other cached copies of this module go; a copied source stays.
        let keep: &[&Path] = if move_file { &[&target] } else { &[&target, src] };
        self.remove_cached(m, keep);
        Ok(target)
    }

    fn write_sidecars(target: &Path, sha1: &str, sha256: &str) {
        for (ext, v) in [("sha1", sha1), ("sha256", sha256)] {
            let mut s = target.as_os_str().to_owned();
            s.push(format!(".{ext}"));
            let _ = fs::write(PathBuf::from(s), v);
        }
    }

    /// Partial download file for this URL (reuse one that starts with the hash).
    fn partial_path(&self, m: &Module, url: &str) -> PathBuf {
        let dl = self.dir.join("downloading");
        let hash = url_hash(url);
        if let Ok(rd) = fs::read_dir(&dl) {
            for e in rd.flatten() {
                if e.file_name().to_string_lossy().starts_with(&format!("{hash}-")) {
                    return e.path();
                }
            }
        }
        dl.join(format!("{hash}-{}", m.standard_name()))
    }

    /// One download attempt from one URL into the partial file. Returns (sha1, sha256).
    fn fetch(
        m: &Module,
        url: &str,
        part: &Path,
        agent: &ureq::Agent,
        auth: &dyn Fn(&str) -> Option<String>,
        progress: Progress,
        resume: bool,
    ) -> Result<(String, String)> {
        let have = if resume { fs::metadata(part).map(|md| md.len()).unwrap_or(0) } else { 0 };
        let mut h1 = sha1::Sha1::new();
        let mut h2 = sha2::Sha256::new();
        let label = format!("Downloading {} {}", m.display_name(), m.version);

        // A complete partial file from an earlier run: just hash it.
        let complete = have > 0 && m.download_size > 0 && have == m.download_size;
        if !complete {
            let mut req = agent
                .get(url)
                .config()
                .timeout_per_read(Some(Duration::from_secs(60)))
                .redirect_auth_headers(ureq::config::RedirectAuthHeaders::SameHost)
                .build();
            let host = url.split("://").nth(1).and_then(|r| r.split(['/', ':', '?']).next()).unwrap_or("");
            if let Some(tok) = auth(host).filter(|t| !t.is_empty()) {
                req = req.header("Authorization", format!("token {tok}"));
            }
            if have > 0 {
                req = req.header("Range", format!("bytes={have}-"));
            }
            let resp = req.call().map_err(|e| Error::Net(format!("{url}: {e}")))?;
            let append = have > 0 && resp.status().as_u16() == 206;
            let body_len: Option<u64> =
                resp.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok());
            let start = if append { have } else { 0 };
            let total = body_len.map(|n| n.saturating_add(start)).or((m.download_size > 0).then_some(m.download_size));

            if let Some(dir) = part.parent() {
                fs::create_dir_all(dir).ctx(dir.display())?;
            }
            if append {
                hash_file(part, &mut h1, &mut h2)?;
            }
            let mut out = fs::OpenOptions::new()
                .create(true)
                .write(true)
                .append(append)
                .truncate(!append)
                .open(part)
                .ctx(part.display())?;
            let mut reader = resp.into_body().into_reader();
            let mut buf = vec![0u8; 1 << 16];
            let mut done = start;
            let mut last = Instant::now();
            progress(&label, total.map(|t| done as f32 / t.max(1) as f32));
            loop {
                let n = match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                    Err(e) => return Err(Error::Net(format!("{url}: {e}"))),
                };
                let chunk = buf.get(..n).unwrap_or(&[]);
                out.write_all(chunk).ctx(part.display())?;
                h1.update(chunk);
                h2.update(chunk);
                done = done.saturating_add(n as u64);
                if last.elapsed() >= Duration::from_millis(100) {
                    last = Instant::now();
                    progress(&label, total.map(|t| (done as f32 / t.max(1) as f32).min(1.0)));
                }
            }
            out.flush().ctx(part.display())?;
            progress(&label, Some(1.0));
        } else {
            hash_file(part, &mut h1, &mut h2)?;
        }
        Ok((hex_upper(&h1.finalize()), hex_upper(&h2.finalize())))
    }

    /// Download the module (try each URL in order), stream to `downloading/`, check size and
    /// sha256 (uppercase hex), check the zip opens, then move into the cache. Returns the path.
    /// `auth` returns an auth token for a host.
    pub fn download(
        &self,
        m: &Module,
        agent: &ureq::Agent,
        auth: &dyn Fn(&str) -> Option<String>,
        progress: Progress,
    ) -> Result<PathBuf> {
        if m.download.is_empty() {
            return Err(Error::Metadata(format!("{m} has no download URL")));
        }
        let mut last_err = Error::Net(format!("{m}: no download URL worked"));
        let want = m.download_hash.as_ref().and_then(|h| h.sha256.as_deref());
        for url in &m.download {
            let part = self.partial_path(m, url);
            // Resume a partial file first. If that fails in any way (network error, or a bad
            // hash, size or zip from old partial data), delete it and start over once.
            let mut resume = fs::metadata(&part).is_ok_and(|md| md.len() > 0);
            loop {
                let (sha1, sha256) = match Self::fetch(m, url, &part, agent, auth, progress, resume) {
                    Ok(h) => h,
                    Err(e) => {
                        last_err = e;
                        if resume {
                            let _ = fs::remove_file(&part);
                            resume = false;
                            continue;
                        }
                        // Keep what was fetched for a later resume.
                        break;
                    }
                };
                let checked = match want {
                    Some(w) if !w.eq_ignore_ascii_case(&sha256) => {
                        Err(Error::Hash(format!("{m}: SHA256 is {sha256}, metadata says {w}")))
                    }
                    _ => Self::validate(m, &part),
                };
                match checked {
                    Ok(()) => {
                        let target = self.put(m, &part, true)?;
                        Self::write_sidecars(&target, &sha1, &sha256);
                        return Ok(target);
                    }
                    Err(e) => {
                        let _ = fs::remove_file(&part);
                        last_err = e;
                        if resume {
                            resume = false;
                            continue;
                        }
                        break;
                    }
                }
            }
        }
        Err(last_err)
    }

    /// Copy (or move) an existing zip into the cache under this module's name (zip import).
    /// Checks size (if known) and the zip, like CKAN's NetModuleCache.Store.
    pub fn store(&self, m: &Module, src: &Path, move_file: bool) -> Result<PathBuf> {
        if !src.is_file() {
            return Err(Error::NotFound(src.display().to_string()));
        }
        Self::validate(m, src)?;
        self.put(m, src, move_file)
    }

    /// Delete this module's cached zip.
    pub fn purge(&self, m: &Module) -> Result<()> {
        self.remove_cached(m, &[]);
        Ok(())
    }

    /// All cache files (hash-named zips only).
    pub fn list(&self) -> Vec<CachedFile> {
        let mut v: Vec<CachedFile> =
            self.entries().into_iter().map(|(_, path, size)| CachedFile { path, size }).collect();
        v.sort_by(|a, b| a.path.cmp(&b.path));
        v
    }

    /// Total bytes in the cache.
    pub fn total_size(&self) -> u64 {
        self.list().iter().map(|f| f.size).sum()
    }

    /// Delete oldest files until the total is at most `limit` bytes.
    pub fn trim(&self, limit: u64) -> Result<()> {
        self.trim_ranked(limit, &|_| 0)
    }

    /// Like `trim`, but files with a lower `rank` go first (CKAN: 0 = not in any repo,
    /// 1 = no compatible module, 2 = the rest); oldest first within a rank. `rank` gets the
    /// 8-char hash prefix.
    pub fn trim_ranked(&self, limit: u64, rank: &dyn Fn(&str) -> u8) -> Result<()> {
        let files = self.list();
        let mut total: u64 = files.iter().map(|f| f.size).sum();
        if total <= limit {
            return Ok(());
        }
        let mut keyed: Vec<(u8, SystemTime, CachedFile)> = files
            .into_iter()
            .map(|f| {
                let name = f.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let t = fs::metadata(&f.path)
                    .and_then(|md| md.created().or_else(|_| md.modified()))
                    .unwrap_or(UNIX_EPOCH);
                (rank(name.get(..8).unwrap_or("")), t, f)
            })
            .collect();
        keyed.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        for (_, _, f) in keyed {
            if total <= limit {
                break;
            }
            remove_with_sidecars(&f.path);
            total = total.saturating_sub(f.size);
        }
        Ok(())
    }
}

fn hash_file(path: &Path, h1: &mut sha1::Sha1, h2: &mut sha2::Sha256) -> Result<()> {
    let mut f = fs::File::open(path).ctx(path.display())?;
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf).ctx(path.display())?;
        if n == 0 {
            return Ok(());
        }
        let chunk = buf.get(..n).unwrap_or(&[]);
        h1.update(chunk);
        h2.update(chunk);
    }
}

#[cfg(test)]
mod tests;
