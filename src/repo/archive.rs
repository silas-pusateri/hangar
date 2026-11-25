//! Archive parsing: one pass over a tar.gz (or zip) reads the side files and streams the
//! .ckan blobs in batches through a bounded queue to a few std parser threads.

use std::collections::HashMap;
use std::io::{BufReader, Read};
use std::path::Path;
use std::sync::Arc;

use serde::Deserialize;

use super::{RepoData, Repository};
use crate::error::{Ctx, Error, Result};
use crate::meta::Module;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveKind {
    TarGz,
    Zip,
}

/// Detect the archive type from the first bytes.
pub fn detect(magic: &[u8]) -> Option<ArchiveKind> {
    if magic.starts_with(&[0x1f, 0x8b]) {
        Some(ArchiveKind::TarGz)
    } else if magic.starts_with(b"PK\x03\x04") || magic.starts_with(b"PK\x05\x06") {
        Some(ArchiveKind::Zip)
    } else {
        None
    }
}

/// Bytes of .ckan text per batch sent to a parser thread. One buffer per batch, big enough to
/// be a "large" allocation that goes back to the OS when freed (no holes between modules).
const BATCH_BYTES: usize = 256 * 1024;

/// A numbered batch of raw .ckan blobs in one buffer (the number keeps the archive order).
struct Batch {
    no: usize,
    buf: Vec<u8>,
    /// End offset of each blob in `buf`.
    ends: Vec<usize>,
}

impl Batch {
    fn new(no: usize) -> Batch {
        Batch { no, buf: Vec::with_capacity(BATCH_BYTES), ends: Vec::with_capacity(256) }
    }
}

/// Side files found in one archive. The .ckan blobs go to the parser threads in batches
/// through a bounded queue, so only a few batches are in memory at a time.
struct Collected<'a> {
    tx: std::sync::mpsc::SyncSender<Batch>,
    batch: Batch,
    /// Folder of the last .ckan entry (`CKAN-meta-master/<identifier>`).
    dir: String,
    data: &'a mut RepoData,
}

impl Collected<'_> {
    /// Route one entry by its name. `read` is called only for entries we keep.
    fn entry(&mut self, name: &str, size: u64, read: &mut dyn Read) -> Result<()> {
        if name.ends_with(".ckan") {
            // Start a new batch only at a new folder, so the versions of one identifier are
            // parsed together and can share their equal fields (Module::share_with).
            let dir = name.rsplit_once('/').map_or("", |(d, _)| d);
            if dir != self.dir {
                if self.batch.buf.len() + size.min(1 << 24) as usize > BATCH_BYTES {
                    self.flush();
                }
                dir.clone_into(&mut self.dir);
            }
            read.read_to_end(&mut self.batch.buf).ctx(name)?;
            self.batch.ends.push(self.batch.buf.len());
        } else if name.ends_with("download_counts.json") {
            let mut buf = Vec::new();
            read.read_to_end(&mut buf).ctx(name)?;
            self.data.download_counts = parse_download_counts(&buf);
        } else if name.ends_with("builds.json") {
            let mut s = String::new();
            read.read_to_string(&mut s).ctx(name)?;
            self.data.builds = crate::instance::parse_builds(&s);
        } else if name.ends_with("repositories.json") {
            let mut buf = Vec::new();
            read.read_to_end(&mut buf).ctx(name)?;
            self.data.repositories = parse_repository_list(&buf);
        }
        // Everything else (*.frozen, README, LICENSE, ...) is skipped.
        Ok(())
    }

    /// Send the current batch. A send fails only when all parsers are gone (a panic, which
    /// aborts in release); the batch is then dropped.
    fn flush(&mut self) {
        if self.batch.ends.is_empty() {
            return;
        }
        let next = Batch::new(self.batch.no + 1);
        let _ = self.tx.send(std::mem::replace(&mut self.batch, next));
    }
}

/// `{id: count}`. Bad values are skipped; a bad file gives an empty map.
pub fn parse_download_counts(buf: &[u8]) -> HashMap<String, u64> {
    let Ok(map) = serde_json::from_slice::<HashMap<String, serde_json::Value>>(buf) else {
        return HashMap::new();
    };
    map.into_iter().filter_map(|(k, v)| v.as_u64().or_else(|| v.as_f64().map(|f| f.max(0.0) as u64)).map(|n| (k, n))).collect()
}

/// `{"repositories": [...]}`. Bad entries are skipped.
pub fn parse_repository_list(buf: &[u8]) -> Vec<Repository> {
    #[derive(Deserialize)]
    struct List {
        #[serde(default)]
        repositories: Vec<serde_json::Value>,
    }
    let Ok(l) = serde_json::from_slice::<List>(buf) else {
        return Vec::new();
    };
    l.repositories.into_iter().filter_map(|v| serde_json::from_value::<Repository>(v).ok()).collect()
}

fn collect_tar_gz(path: &Path, out: &mut Collected) -> Result<()> {
    let f = std::fs::File::open(path).ctx(path.display())?;
    let gz = flate2::read::GzDecoder::new(BufReader::with_capacity(1 << 16, f));
    let mut ar = tar::Archive::new(gz);
    let entries = ar.entries().ctx(path.display())?;
    for e in entries {
        let mut e = e.ctx(path.display())?;
        if !e.header().entry_type().is_file() {
            continue;
        }
        let name = match e.path() {
            Ok(p) => p.to_string_lossy().into_owned(),
            Err(_) => continue,
        };
        let size = e.size();
        out.entry(&name, size, &mut e)?;
    }
    Ok(())
}

fn collect_zip(path: &Path, out: &mut Collected) -> Result<()> {
    let f = std::fs::File::open(path).ctx(path.display())?;
    let mut za = zip::ZipArchive::new(BufReader::new(f)).map_err(|e| Error::Metadata(format!("{}: {e}", path.display())))?;
    for i in 0..za.len() {
        let mut zf = za.by_index(i).map_err(|e| Error::Metadata(format!("{}: {e}", path.display())))?;
        if zf.is_dir() {
            continue;
        }
        let name = match zf.name() {
            Ok(n) => n.into_owned(),
            Err(_) => continue,
        };
        let size = zf.size();
        out.entry(&name, size, &mut zf)?;
    }
    Ok(())
}

/// Parse one batch of .ckan blobs. A module shares equal fields with the module before it
/// when both have the same identifier. Returns (modules, skipped).
fn parse_batch(b: &Batch) -> (Vec<Arc<Module>>, usize) {
    let mut mods: Vec<Arc<Module>> = Vec::with_capacity(b.ends.len());
    let mut bad = 0usize;
    let mut start = 0;
    for &end in &b.ends {
        match b.buf.get(start..end).map(Module::from_json) {
            Some(Ok(mut m)) => {
                if let Some(prev) = mods.last().filter(|p| p.identifier == m.identifier) {
                    m.share_with(prev);
                }
                mods.push(Arc::new(m));
            }
            _ => bad += 1,
        }
        start = end;
    }
    (mods, bad)
}

/// What one parser thread made: (batch number, modules) and the skipped count.
#[derive(Default)]
struct Parsed {
    batches: Vec<(usize, Vec<Arc<Module>>)>,
    skipped: usize,
}

/// Parser thread: take batches from the queue until the reader closes it.
fn parse_worker(rx: &std::sync::Mutex<std::sync::mpsc::Receiver<Batch>>) -> Parsed {
    let mut out = Parsed::default();
    loop {
        // Hold the lock only for the receive, not for the parse.
        let next = match rx.lock() {
            Ok(r) => r.recv(),
            Err(_) => break,
        };
        let Ok(batch) = next else { break };
        let (mods, bad) = parse_batch(&batch);
        out.batches.push((batch.no, mods));
        out.skipped += bad;
    }
    out
}

/// First 4 bytes of a file (fewer if the file is shorter).
fn read_magic(path: &Path) -> Result<[u8; 4]> {
    let mut magic = [0u8; 4];
    let mut f = std::fs::File::open(path).ctx(path.display())?;
    let mut n = 0;
    while let Some(rest) = magic.get_mut(n..).filter(|r| !r.is_empty()) {
        let k = f.read(rest).ctx(path.display())?;
        if k == 0 {
            break;
        }
        n += k;
    }
    Ok(magic)
}

/// Check that a whole archive reads back without errors (gzip CRC and length, tar headers,
/// zip directory and entry CRCs). A download cut short can still start with the right magic
/// bytes; it must not replace a good cache. Nothing is parsed as JSON here.
pub fn validate(path: &Path) -> Result<()> {
    let bad = |e: &dyn std::fmt::Display| Error::Metadata(format!("{}: damaged archive: {e}", path.display()));
    match detect(&read_magic(path)?) {
        Some(ArchiveKind::TarGz) => {
            let f = std::fs::File::open(path).ctx(path.display())?;
            let gz = flate2::read::GzDecoder::new(BufReader::with_capacity(1 << 16, f));
            let mut ar = tar::Archive::new(gz);
            for e in ar.entries().map_err(|e| bad(&e))? {
                let mut e = e.map_err(|e| bad(&e))?;
                std::io::copy(&mut e, &mut std::io::sink()).map_err(|e| bad(&e))?;
            }
            // Read past the tar end blocks to the gzip trailer, so its CRC is checked.
            let mut gz = ar.into_inner();
            std::io::copy(&mut gz, &mut std::io::sink()).map_err(|e| bad(&e))?;
            Ok(())
        }
        Some(ArchiveKind::Zip) => {
            let f = std::fs::File::open(path).ctx(path.display())?;
            let mut za = zip::ZipArchive::new(BufReader::new(f)).map_err(|e| bad(&e))?;
            for i in 0..za.len() {
                let mut zf = za.by_index(i).map_err(|e| bad(&e))?;
                std::io::copy(&mut zf, &mut std::io::sink()).map_err(|e| bad(&e))?;
            }
            Ok(())
        }
        None => Err(Error::Metadata(format!("{}: not a tar.gz or zip archive", path.display()))),
    }
}

/// Parse one archive file (tar.gz or zip by magic bytes). The calling thread reads the archive
/// and sends .ckan blobs in batches through a bounded queue to min(available_parallelism, 8)
/// parser threads, so the raw blobs are never all in memory at once. Modules keep the archive order.
pub fn parse(path: &Path) -> Result<RepoData> {
    let magic = read_magic(path)?;
    let kind = detect(&magic)
        .ok_or_else(|| Error::Metadata(format!("{}: not a tar.gz or zip archive", path.display())))?;
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, 8);
    let (tx, rx) = std::sync::mpsc::sync_channel::<Batch>(threads * 2);
    let rx = std::sync::Mutex::new(rx);
    let mut data = RepoData::default();
    let (read, parts) = std::thread::scope(|s| {
        let workers: Vec<_> = (0..threads).map(|_| s.spawn(|| parse_worker(&rx))).collect();
        let mut col = Collected { tx, batch: Batch::new(0), dir: String::new(), data: &mut data };
        let read = match kind {
            ArchiveKind::TarGz => collect_tar_gz(path, &mut col),
            ArchiveKind::Zip => collect_zip(path, &mut col),
        };
        col.flush();
        drop(col); // closes the queue: the workers stop after the last batch
        // A worker can only fail by panic (abort in release); its batches are lost.
        let parts: Vec<_> = workers.into_iter().filter_map(|h| h.join().ok()).collect();
        (read, parts)
    });
    read?;
    let mut batches = Vec::new();
    for p in parts {
        batches.extend(p.batches);
        data.skipped += p.skipped;
    }
    batches.sort_unstable_by_key(|b| b.0);
    data.modules = Vec::with_capacity(batches.iter().map(|b| b.1.len()).sum());
    for (_, mods) in batches {
        data.modules.extend(mods);
    }
    Ok(data)
}
