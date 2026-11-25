//! Small shared helpers: time stamps, hashes, paths, atomic writes.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use sha1::Digest as _;

use crate::error::{Ctx, Result};

// ---------------------------------------------------------------- time

/// Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Broken-down UTC time.
#[derive(Debug, Clone, Copy)]
pub struct Utc {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub min: u32,
    pub sec: u32,
    pub nanos: u32,
}

impl Utc {
    pub fn now() -> Self {
        Self::from_system(SystemTime::now())
    }

    pub fn from_system(t: SystemTime) -> Self {
        let d = t.duration_since(UNIX_EPOCH).unwrap_or_default();
        let secs = d.as_secs() as i64;
        let (year, month, day) = civil_from_days(secs.div_euclid(86_400));
        let rem = secs.rem_euclid(86_400) as u32;
        Utc { year, month, day, hour: rem / 3600, min: rem % 3600 / 60, sec: rem % 60, nanos: d.subsec_nanos() }
    }

    /// `2026-11-24T18:30:12.1234567Z` (CKAN / Newtonsoft round-trip form, 7 fraction digits).
    pub fn iso8601(&self) -> String {
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:07}Z",
            self.year,
            self.month,
            self.day,
            self.hour,
            self.min,
            self.sec,
            self.nanos / 100
        )
    }

    /// `yyyy.MM.dd.HH.mm.ss` (modpack version).
    pub fn dotted(&self) -> String {
        format!("{:04}.{:02}.{:02}.{:02}.{:02}.{:02}", self.year, self.month, self.day, self.hour, self.min, self.sec)
    }

    /// `yyyy-MM-dd_HH-mm-ss` (history file names).
    pub fn file_stamp(&self) -> String {
        format!("{:04}-{:02}-{:02}_{:02}-{:02}-{:02}", self.year, self.month, self.day, self.hour, self.min, self.sec)
    }

}

// ---------------------------------------------------------------- hashes

pub fn hex_upper(bytes: &[u8]) -> String {
    const H: &[u8; 16] = b"0123456789ABCDEF";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(H[(b >> 4) as usize] as char);
        s.push(H[(b & 15) as usize] as char);
    }
    s
}

pub fn sha1_upper(data: &[u8]) -> String {
    hex_upper(&sha1::Sha1::digest(data))
}

/// SHA1 and SHA256 of a file, uppercase hex, streamed.
pub fn file_hashes(path: &Path) -> Result<(String, String)> {
    let mut f = std::fs::File::open(path).ctx(path.display())?;
    let mut s1 = sha1::Sha1::new();
    let mut s2 = sha2::Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf).ctx(path.display())?;
        if n == 0 {
            break;
        }
        s1.update(&buf[..n]);
        s2.update(&buf[..n]);
    }
    Ok((hex_upper(&s1.finalize()), hex_upper(&s2.finalize())))
}

/// CKAN URL hash prefix: first 8 chars of uppercase SHA1 of the URL text.
pub fn url_hash8(url: &str) -> String {
    sha1_upper(url.as_bytes())[..8].to_string()
}

// ---------------------------------------------------------------- identifiers and paths

/// CKAN Identifier.Sanitize: strip leading non-alphanumerics, then map `[^A-Za-z0-9-]` to `-`.
pub fn sanitize_identifier(s: &str) -> String {
    s.trim_start_matches(|c: char| !c.is_ascii_alphanumeric())
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' })
        .collect()
}

/// CKANPathUtils.NormalizePath: `\` to `/`, and trim all trailing `/` when len >= 2.
pub fn normalize_path(p: &str) -> String {
    let s = p.replace('\\', "/");
    if p.len() < 2 { s } else { s.trim_end_matches('/').to_string() }
}

/// Path of `abs` relative to `root`, with `/` separators. None if not inside root.
pub fn rel_path(root: &Path, abs: &Path) -> Option<String> {
    let r = abs.strip_prefix(root).ok()?;
    Some(normalize_path(&r.to_string_lossy()))
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"))
}

/// Hangar's own data dir. `HANGAR_HOME` overrides it (tests must set it).
pub fn hangar_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("HANGAR_HOME") {
        return PathBuf::from(p);
    }
    home().join("Library/Application Support/Hangar")
}

/// Real CKAN's data dir on macOS (read-only use: import of config and labels).
pub fn ckan_app_dir() -> PathBuf {
    let new = home().join("Library/Application Support/CKAN");
    if new.exists() {
        return new;
    }
    let old = home().join(".local/share/CKAN");
    if old.exists() { old } else { new }
}

// ---------------------------------------------------------------- files

/// Write a file through a temp file and rename, so a crash never leaves half a file.
pub fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ctx(dir.display())?;
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    {
        let mut f = std::fs::File::create(&tmp).ctx(tmp.display())?;
        f.write_all(data).ctx(tmp.display())?;
        f.sync_all().ctx(tmp.display())?;
    }
    std::fs::rename(&tmp, path).ctx(path.display())
}

/// Write pretty JSON (2-space indent from serde_json; CKAN reads any indent) atomically.
pub fn write_json<T: serde::Serialize>(path: &Path, v: &T) -> Result<()> {
    let mut s = serde_json::to_string_pretty(v)?;
    s.push('\n');
    write_atomic(path, s.as_bytes())
}

/// Move a file or dir to the macOS Trash (uses /usr/bin/trash when present), else delete it.
pub fn trash(path: &Path) -> Result<()> {
    if Path::new("/usr/bin/trash").exists() {
        let st = std::process::Command::new("/usr/bin/trash").arg(path).status().ctx("trash")?;
        if st.success() {
            return Ok(());
        }
    }
    if path.is_dir() {
        std::fs::remove_dir_all(path).ctx(path.display())
    } else {
        std::fs::remove_file(path).ctx(path.display())
    }
}

/// Human size: `1.2 MiB`.
pub fn human_size(n: u64) -> String {
    const U: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{n} B") } else { format!("{v:.1} {}", U[i]) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        let t = UNIX_EPOCH + std::time::Duration::from_secs(1_795_544_171);
        let u = Utc::from_system(t);
        assert_eq!((u.year, u.month, u.day), (2026, 11, 24));
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(11_017), (2000, 3, 1));
    }

    #[test]
    fn sanitize() {
        assert_eq!(sanitize_identifier("installed-My KSP"), "installed-My-KSP");
        assert_eq!(sanitize_identifier("_Kopernicus_Parser"), "Kopernicus-Parser");
        assert_eq!(normalize_path("GameData\\Foo/"), "GameData/Foo");
        assert_eq!(normalize_path("/"), "/");
        assert_eq!(normalize_path("GameData//"), "GameData");
    }

    #[test]
    fn hashes() {
        assert_eq!(sha1_upper(b"abc"), "A9993E364706816ABA3E25717850C26C9CD0D89D");
        assert_eq!(url_hash8("https://github.com/KSP-CKAN/CKAN-meta/archive/master.tar.gz"), "4A85405A");
    }
}
