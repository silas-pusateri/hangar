//! Background jobs on std threads. A job owns the Session while it runs and sends it back,
//! with a freshly built row model, in one `Done` message. Progress messages arrive on the
//! same channel; the job requests a repaint when it sends (throttled).

use std::collections::HashSet;
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant, SystemTime};

use eframe::egui;

use super::model::{BuildInput, RowModel};
use crate::error::{Error, Result};
use crate::modlist::ImportResult;
use crate::ops::{ApplyReport, Session};
use crate::repo::{Repository, UpdateResult};
use crate::resolve::{Changeset, OptionalMod, Request};

/// Why a plan was made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanMode {
    /// Show it in the Changes tab only.
    Preview,
    /// Continue to the recommendations and confirm dialogs.
    Apply,
    /// Like Apply, but recommendations were already offered.
    ApplyNoRecs,
}

pub enum Outcome {
    Opened(Result<()>),
    Updated(Result<UpdateResult>),
    Planned { mode: PlanMode, req: Request, result: Result<(Changeset, Vec<OptionalMod>)> },
    Applied { cs: Changeset, result: Result<ApplyReport> },
    Downloaded(Result<()>),
    Imported(Result<ImportResult>),
    RepoList(Result<Vec<Repository>>),
    Rebuilt,
}

/// Facts read with the session on the worker, so the UI never touches the disk for them.
#[derive(Debug, Clone, Default)]
pub struct SessionFacts {
    /// Instance name and game version, shown while a job owns the session.
    pub instance: Option<String>,
    pub game_version: Option<String>,
    pub playtime: Duration,
    /// Modification time of the oldest repo cache (None = some repo has no cache).
    pub repo_time: Option<SystemTime>,
}

pub struct Done {
    pub session: Option<Session>,
    pub model: Option<RowModel>,
    pub facts: Option<SessionFacts>,
    pub outcome: Outcome,
}

pub enum Msg {
    Progress(String, Option<f32>),
    Done(Box<Done>),
}

/// Sends progress from a job thread.
pub struct Reporter {
    tx: Sender<Msg>,
    ctx: egui::Context,
    last: Mutex<Instant>,
}

impl Reporter {
    pub fn progress(&self, msg: &str, frac: Option<f32>) {
        let _ = self.tx.send(Msg::Progress(msg.to_string(), frac));
        // At most ~30 repaints per second; a later repaint is always scheduled.
        let now = Instant::now();
        let due = match self.last.lock() {
            Ok(mut t) if now.duration_since(*t) >= Duration::from_millis(33) => {
                *t = now;
                true
            }
            _ => false,
        };
        if due {
            self.ctx.request_repaint();
        } else {
            self.ctx.request_repaint_after(Duration::from_millis(40));
        }
    }
}

/// A running job as the UI sees it.
pub struct Job {
    rx: Receiver<Msg>,
    pub title: String,
    pub message: String,
    pub fraction: Option<f32>,
    pub log: Vec<String>,
    pub started: Instant,
    /// The job changes files in the game folder: the window must not close while it runs.
    pub no_close: bool,
}

pub enum Poll {
    Running,
    Done(Box<Done>),
    /// The thread ended without a result (should not happen).
    Lost,
}

const LOG_MAX: usize = 400;

impl Job {
    /// Run `f` on a new thread.
    pub fn spawn(ctx: &egui::Context, title: &str, f: impl FnOnce(&Reporter) -> Done + Send + 'static) -> Job {
        let (tx, rx) = mpsc::channel();
        let rep = Reporter { tx: tx.clone(), ctx: ctx.clone(), last: Mutex::new(Instant::now()) };
        let spawned = std::thread::Builder::new().name(format!("hangar-{title}")).spawn(move || {
            let done = f(&rep);
            let _ = rep.tx.send(Msg::Done(Box::new(done)));
            rep.ctx.request_repaint();
        });
        if let Err(e) = spawned {
            // No thread: report the failure as a finished job.
            let _ = tx.send(Msg::Done(Box::new(Done {
                session: None,
                model: None,
                facts: None,
                outcome: Outcome::Opened(Err(Error::Other(format!("cannot start a thread: {e}")))),
            })));
        }
        Job {
            rx,
            title: title.to_string(),
            message: format!("{title}..."),
            fraction: None,
            log: Vec::new(),
            started: Instant::now(),
            no_close: false,
        }
    }

    /// Block until the job ends (used at quit, so a half-done apply is never cut off).
    pub fn wait(&mut self) {
        loop {
            match self.rx.recv() {
                Ok(Msg::Progress(..)) => {}
                Ok(Msg::Done(_)) | Err(_) => return,
            }
        }
    }

    /// Drain messages. Call once per frame while a job exists.
    pub fn poll(&mut self) -> Poll {
        loop {
            match self.rx.try_recv() {
                Ok(Msg::Progress(m, f)) => {
                    if f.is_some() {
                        self.fraction = f;
                    }
                    if self.log.last() != Some(&m) && !m.is_empty() {
                        if self.log.len() >= LOG_MAX {
                            self.log.remove(0);
                        }
                        self.log.push(m.clone());
                    }
                    self.message = m;
                }
                Ok(Msg::Done(d)) => return Poll::Done(d),
                Err(TryRecvError::Empty) => return Poll::Running,
                Err(TryRecvError::Disconnected) => return Poll::Lost,
            }
        }
    }
}

/// Oldest modification time of the repo caches; None if any repo has no cache file.
pub fn repo_time(s: &Session) -> Option<SystemTime> {
    let dir = s.config.repo_cache_dir();
    let mut oldest: Option<SystemTime> = None;
    for r in s.registry.repositories() {
        let p = crate::repo::net::find_cache_file(&r, &dir)?;
        let t = std::fs::metadata(&p).and_then(|m| m.modified()).ok()?;
        oldest = Some(oldest.map_or(t, |o| o.min(t)));
    }
    oldest
}

/// Build the row model for a session.
pub fn build_model(s: &Session, prev_compatible: Option<&HashSet<String>>) -> RowModel {
    let cache = s.cache.index();
    super::model::build_rows(&BuildInput {
        ctx: s.context(),
        cache: &cache,
        labels: &s.labels,
        instance: &s.instance.name,
        prev_compatible,
        hide_epochs: s.config.hangar.hide_epochs,
        hide_v: s.config.hangar.hide_v,
    })
}

pub fn facts(s: &Session) -> SessionFacts {
    SessionFacts {
        instance: Some(s.instance.name.clone()),
        game_version: s.instance.version.map(|v| v.to_string()),
        playtime: s.instance.playtime(),
        repo_time: repo_time(s),
    }
}

/// Finish a job that changed the session: rebuild rows and facts.
pub fn done_with(s: Session, prev: Option<&HashSet<String>>, outcome: Outcome) -> Done {
    let model = build_model(&s, prev);
    let f = facts(&s);
    Done { session: Some(s), model: Some(model), facts: Some(f), outcome }
}

/// Finish a job that did not change what the rows show.
pub fn done_same(s: Session, outcome: Outcome) -> Done {
    Done { session: Some(s), model: None, facts: None, outcome }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Quit waits on this: it must return only after the job body ran to the end.
    #[test]
    fn wait_blocks_until_the_job_ends() {
        let ctx = egui::Context::default();
        let ended = Arc::new(AtomicBool::new(false));
        let e = Arc::clone(&ended);
        let mut job = Job::spawn(&ctx, "test", move |rep| {
            rep.progress("working", Some(0.5));
            std::thread::sleep(Duration::from_millis(150));
            e.store(true, Ordering::SeqCst);
            Done { session: None, model: None, facts: None, outcome: Outcome::Rebuilt }
        });
        assert!(!job.no_close);
        job.wait();
        assert!(ended.load(Ordering::SeqCst));
    }
}
