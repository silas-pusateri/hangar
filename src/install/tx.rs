//! Undo journal for file changes in a game dir.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::{Ctx, Error, Result};

static TX_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
enum Op {
    CreatedFile(PathBuf),
    CreatedDir(PathBuf),
    Removed { orig: PathBuf, aside: PathBuf },
}

/// Undo journal for file changes. `rollback` deletes created files/dirs and restores removed
/// files (removed files are moved aside under `<game>/CKAN/tx-<pid>-<n>/` until commit).
/// Dropping a FileTx without `commit` rolls it back.
#[derive(Debug)]
pub struct FileTx {
    ops: Vec<Op>,
    /// First name to try for the aside dir. The dir actually used is in `aside`.
    aside_base: PathBuf,
    /// The aside dir this tx created (only this one is ever deleted).
    aside: Option<PathBuf>,
    next_aside: u64,
    done: bool,
}

/// Move a file; copy and delete when a rename is not possible (other volume).
fn move_file(from: &Path, to: &Path) -> std::io::Result<()> {
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == ErrorKind::NotFound => Err(e),
        Err(_) => {
            fs::copy(from, to)?;
            fs::remove_file(from)
        }
    }
}

impl FileTx {
    pub fn begin(game_dir: &Path) -> Result<FileTx> {
        if !game_dir.is_dir() {
            return Err(Error::NotFound(format!("game dir {}", game_dir.display())));
        }
        let n = TX_SEQ.fetch_add(1, Ordering::Relaxed);
        Ok(FileTx {
            ops: Vec::new(),
            aside_base: game_dir.join("CKAN").join(format!("tx-{}-{n}", std::process::id())),
            aside: None,
            next_aside: 0,
            done: false,
        })
    }



    /// Create `dir` and missing parents; each dir created is recorded.
    pub fn create_dir_all(&mut self, dir: &Path) -> Result<()> {
        let mut missing = Vec::new();
        let mut cur = Some(dir);
        while let Some(p) = cur {
            match fs::symlink_metadata(p) {
                Ok(md) if md.is_dir() => break,
                Ok(md) if md.file_type().is_symlink() && p.is_dir() => break,
                Ok(_) => return Err(Error::Conflict(format!("{} exists and is not a directory", p.display()))),
                Err(_) => missing.push(p.to_path_buf()),
            }
            cur = p.parent();
        }
        for p in missing.into_iter().rev() {
            match fs::create_dir(&p) {
                Ok(()) => self.ops.push(Op::CreatedDir(p)),
                Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
                Err(e) => return Err(Error::Io(format!("{}: {e}", p.display()))),
            }
        }
        Ok(())
    }

    /// Create a new file (never overwrites). The file is recorded.
    pub fn create_file(&mut self, path: &Path) -> Result<fs::File> {
        match fs::OpenOptions::new().write(true).create_new(true).open(path) {
            Ok(f) => {
                self.ops.push(Op::CreatedFile(path.to_path_buf()));
                Ok(f)
            }
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                Err(Error::Conflict(format!("{} already exists", path.display())))
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {
                Err(Error::NotFound(format!("directory for {} does not exist", path.display())))
            }
            Err(e) => Err(Error::Io(format!("{}: {e}", path.display()))),
        }
    }

    /// Remove a file (moved aside until commit). A missing file is not an error.
    pub fn remove_file(&mut self, path: &Path) -> Result<()> {
        match fs::symlink_metadata(path) {
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(Error::Io(format!("{}: {e}", path.display()))),
            Ok(md) if md.is_dir() => return Err(Error::Conflict(format!("{} is a directory", path.display()))),
            Ok(_) => {}
        }
        let aside = self.aside_dir()?.join(self.next_aside.to_string());
        self.next_aside += 1;
        match move_file(path, &aside) {
            Ok(()) => {
                self.ops.push(Op::Removed { orig: path.to_path_buf(), aside });
                Ok(())
            }
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
            Err(e) => Err(Error::Io(format!("{}: {e}", path.display()))),
        }
    }

    pub fn commit(mut self) -> Result<()> {
        self.done = true;
        self.ops.clear();
        self.remove_aside()
    }

    pub fn rollback(mut self) -> Result<()> {
        self.done = true;
        self.undo()
    }

    /// Create this tx's own aside dir on first use. A dir with the same name can be left from a
    /// crashed run with the same pid; it may hold the only copy of a user's file, so it is never
    /// reused or deleted: a new name is picked instead.
    fn aside_dir(&mut self) -> Result<PathBuf> {
        if let Some(d) = &self.aside {
            return Ok(d.clone());
        }
        if let Some(parent) = self.aside_base.parent() {
            fs::create_dir_all(parent).ctx(parent.display())?;
        }
        let base = self.aside_base.as_os_str().to_owned();
        for k in 0u32..1000 {
            let mut name = base.clone();
            if k > 0 {
                name.push(format!("-{k}"));
            }
            let d = PathBuf::from(name);
            match fs::create_dir(&d) {
                Ok(()) => {
                    self.aside = Some(d.clone());
                    return Ok(d);
                }
                Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(Error::Io(format!("{}: {e}", d.display()))),
            }
        }
        Err(Error::Io(format!("{}: no free name for the undo dir", self.aside_base.display())))
    }

    fn remove_aside(&self) -> Result<()> {
        let Some(d) = &self.aside else { return Ok(()) };
        match fs::remove_dir_all(d) {
            Err(e) if e.kind() != ErrorKind::NotFound => Err(Error::Io(format!("{}: {e}", d.display()))),
            _ => Ok(()),
        }
    }

    fn undo(&mut self) -> Result<()> {
        let mut first_err: Option<Error> = None;
        for op in std::mem::take(&mut self.ops).into_iter().rev() {
            let r = match op {
                Op::CreatedFile(p) => match fs::remove_file(&p) {
                    Err(e) if e.kind() != ErrorKind::NotFound => Err(Error::Io(format!("{}: {e}", p.display()))),
                    _ => Ok(()),
                },
                // A dir may hold files we did not create; leave it then.
                Op::CreatedDir(p) => {
                    let _ = fs::remove_dir(&p);
                    Ok(())
                }
                Op::Removed { orig, aside } => {
                    if let Some(parent) = orig.parent() {
                        let _ = fs::create_dir_all(parent);
                    }
                    move_file(&aside, &orig).map_err(|e| Error::Io(format!("restore {}: {e}", orig.display())))
                }
            };
            if let Err(e) = r {
                first_err.get_or_insert(e);
            }
        }
        // Keep moved-aside files if a restore failed, so nothing is lost.
        if first_err.is_none()
            && let Err(e) = self.remove_aside()
        {
            first_err = Some(e);
        }
        first_err.map_or(Ok(()), Err)
    }
}

impl Drop for FileTx {
    fn drop(&mut self) {
        if !self.done {
            let _ = self.undo();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn temp_dir(tag: &str) -> PathBuf {
        let n = TX_SEQ.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("hangar-install-{tag}-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn rollback_restores_everything() {
        let g = temp_dir("tx");
        fs::create_dir_all(g.join("GameData/Old")).unwrap();
        fs::write(g.join("GameData/Old/keep.cfg"), b"old").unwrap();

        let mut tx = FileTx::begin(&g).unwrap();
        tx.create_dir_all(&g.join("GameData/New/Sub")).unwrap();
        use std::io::Write;
        tx.create_file(&g.join("GameData/New/Sub/a.cfg")).unwrap().write_all(b"new").unwrap();
        tx.remove_file(&g.join("GameData/Old/keep.cfg")).unwrap();
        assert!(!g.join("GameData/Old/keep.cfg").exists());
        // Never overwrite.
        assert!(tx.create_file(&g.join("GameData/New/Sub/a.cfg")).is_err());
        tx.rollback().unwrap();

        assert!(!g.join("GameData/New").exists());
        assert_eq!(fs::read(g.join("GameData/Old/keep.cfg")).unwrap(), b"old");
        assert!(fs::read_dir(g.join("CKAN")).map(|d| d.count()).unwrap_or(0) == 0);
        fs::remove_dir_all(&g).unwrap();
    }

    #[test]
    fn commit_and_drop() {
        let g = temp_dir("tx2");
        fs::write(g.join("x.txt"), b"x").unwrap();
        let mut tx = FileTx::begin(&g).unwrap();
        tx.remove_file(&g.join("x.txt")).unwrap();
        tx.create_file(&g.join("y.txt")).unwrap();
        tx.commit().unwrap();
        assert!(!g.join("x.txt").exists());
        assert!(g.join("y.txt").exists());
        assert!(fs::read_dir(g.join("CKAN")).unwrap().count() == 0);

        {
            let mut tx = FileTx::begin(&g).unwrap();
            tx.create_file(&g.join("z.txt")).unwrap();
            tx.remove_file(&g.join("y.txt")).unwrap();
            tx.remove_file(&g.join("missing.txt")).unwrap();
        } // dropped: rolled back
        assert!(!g.join("z.txt").exists());
        assert!(g.join("y.txt").exists());
        assert!(FileTx::begin(&g.join("nope")).is_err());
        fs::remove_dir_all(&g).unwrap();
    }

    /// A leftover undo dir with this tx's name (crash, then pid reuse) holds the only copy of a
    /// user file. Commit and rollback must not delete or overwrite it.
    #[test]
    fn stale_aside_dir_is_never_touched() {
        let g = temp_dir("tx3");
        for (tag, commit) in [("c", true), ("r", false), ("n", true)] {
            let mut tx = FileTx::begin(&g).unwrap();
            let stale = tx.aside_base.clone();
            fs::create_dir_all(&stale).unwrap();
            fs::write(stale.join("0"), b"only copy").unwrap();
            if tag != "n" {
                fs::write(g.join("x.txt"), b"x").unwrap();
                tx.remove_file(&g.join("x.txt")).unwrap();
                assert_ne!(tx.aside.as_deref(), Some(stale.as_path()));
            }
            if commit { tx.commit().unwrap() } else { tx.rollback().unwrap() }
            assert_eq!(fs::read(stale.join("0")).unwrap(), b"only copy", "{tag}");
            assert_eq!(g.join("x.txt").exists(), tag == "r", "{tag}");
            // Only the stale dir is left in CKAN/.
            assert_eq!(fs::read_dir(g.join("CKAN")).unwrap().count(), 1, "{tag}");
            fs::remove_dir_all(&stale).unwrap();
            let _ = fs::remove_file(g.join("x.txt"));
        }
        fs::remove_dir_all(&g).unwrap();
    }
}
