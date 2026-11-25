//! App state, the frame loop, menus, toolbar, status bar, startup and the self-test.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use eframe::egui::{self, Color32, Key, Modifiers, RichText};

use super::changes::Flow;
use super::dialogs::Dialogs;
use super::info::InfoState;
use super::model::{self, Col, LabelInfo, ListFilter, Pending, RowModel};
use super::worker::{self, Done, Job, Outcome, PlanMode, Poll, Reporter, SessionFacts};
use crate::config::Config;
use crate::modlist::search::{Filter, ModSearch};
use crate::ops::Session;
use crate::repo::RepoIndex;
use crate::resolve::Changeset;

pub const GREEN: Color32 = Color32::from_rgb(46, 160, 67);
pub const RED: Color32 = Color32::from_rgb(218, 54, 51);
pub const BLUE: Color32 = Color32::from_rgb(56, 132, 255);
pub const PURPLE: Color32 = Color32::from_rgb(163, 113, 247);

/// Text sizes in percent. The View menu lists them; Cmd+= and Cmd+- step through them.
pub const TEXT_SIZES: [u16; 8] = [80, 90, 100, 110, 125, 150, 175, 200];

/// A saved text size in the range of `TEXT_SIZES` (a hand-edited config can hold any number).
fn text_size_ok(pct: u16) -> u16 {
    pct.clamp(TEXT_SIZES[0], TEXT_SIZES[TEXT_SIZES.len() - 1])
}

pub const SEARCH_HINT: &str = "Search: name  @author  tag:x  label:x  is:installed  dep:x  -word";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Mods,
    Changes,
}

/// An instance that another program (a second Hangar window, CKAN) has locked. Hangar tries
/// again at `next` and opens it as soon as the lock is free.
pub struct WaitLock {
    pub name: Option<String>,
    pub message: String,
    pub next: Instant,
}

/// Time between two tries to open a locked instance.
const LOCK_RETRY: Duration = Duration::from_secs(2);

/// Game process state shared with the thread that waits on it.
#[derive(Default)]
pub struct Play {
    pub running: AtomicBool,
    /// Message and new total play time from the waiting thread.
    pub result: Mutex<Option<(String, Option<Duration>)>>,
}

pub struct App {
    pub ctx: egui::Context,
    /// The app's copy of the settings. Synced into the session after every change.
    pub config: Config,
    pub session: Option<Session>,
    /// Kept so the info panel can read versions while a job owns the session.
    pub index: Option<Arc<RepoIndex>>,
    pub model: Option<RowModel>,
    pub facts: SessionFacts,
    pub label_info: LabelInfo,
    pub view: Vec<usize>,
    pub view_dirty: bool,
    pub filter: ListFilter,
    pub search_text: String,
    pub search: ModSearch,
    pub sort: (Col, bool),
    pub selected: Option<String>,
    pub scroll_to_selected: bool,
    pub pending: Pending,
    /// Plan for the current pending changes (None = not planned yet).
    pub plan: Option<Changeset>,
    pub plan_error: Option<String>,
    pub tab: Tab,
    pub flow: Flow,
    pub job: Option<Job>,
    pub dlg: Dialogs,
    pub info: InfoState,
    pub status: String,
    pub play: Arc<Play>,
    pub focus_search: bool,
    pub(super) started: bool,
    pub(super) frames: u64,
    t0: Instant,
    pub(super) first_loaded_ms: Option<u128>,
    pub(super) selftest: Option<super::selftest::SelfTest>,
    pub(super) job_times: Vec<super::selftest::JobTime>,
    pub(super) filter_ms: u128,
    /// HANGAR_GUI_FRAMELOG=1 prints one line per frame to stderr.
    frame_log: bool,
    /// The user asked to quit while a job that changes files ran: quit when it ends.
    pub quit_after_job: bool,
    /// Compatible ids before the last repo update that changed data ("newly compatible").
    prev_compat: Option<Arc<HashSet<String>>>,
    /// `prev_compat` candidate of the running update job.
    next_prev: Option<Arc<HashSet<String>>>,
    /// Set while the chosen instance is locked by another program.
    pub wait_lock: Option<WaitLock>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> App {
        let ctx = cc.egui_ctx.clone();
        let config = Config::load();
        match config.hangar.dark_mode {
            Some(true) => ctx.set_theme(egui::Theme::Dark),
            Some(false) => ctx.set_theme(egui::Theme::Light),
            None => ctx.set_theme(egui::ThemePreference::System),
        }
        ctx.all_styles_mut(|s| {
            s.spacing.item_spacing = egui::vec2(6.0, 4.0);
            s.spacing.button_padding = egui::vec2(8.0, 3.0);
        });
        // Hangar handles the zoom keys itself, so that the text size is saved.
        ctx.options_mut(|o| o.zoom_with_keyboard = false);
        ctx.set_zoom_factor(f32::from(text_size_ok(config.hangar.text_size)) / 100.0);
        let search_text = config.hangar.last_search.clone();
        let selftest = super::selftest::SelfTest::from_env();
        App {
            ctx,
            search: ModSearch::parse(&search_text),
            search_text,
            config,
            session: None,
            index: None,
            model: None,
            facts: SessionFacts::default(),
            label_info: LabelInfo { names: Vec::new(), hide: Vec::new(), colors: Vec::new() },
            view: Vec::new(),
            view_dirty: true,
            filter: ListFilter::default(),
            sort: (Col::Name, true),
            selected: None,
            scroll_to_selected: false,
            pending: Pending::default(),
            plan: None,
            plan_error: None,
            tab: Tab::Mods,
            flow: Flow::Idle,
            job: None,
            dlg: Dialogs::default(),
            info: InfoState::default(),
            status: "Starting".into(),
            play: Arc::new(Play::default()),
            focus_search: false,
            started: false,
            frames: 0,
            t0: Instant::now(),
            first_loaded_ms: None,
            selftest,
            job_times: Vec::new(),
            filter_ms: 0,
            frame_log: std::env::var_os("HANGAR_GUI_FRAMELOG").is_some(),
            quit_after_job: false,
            prev_compat: None,
            next_prev: None,
            wait_lock: None,
        }
    }

    pub fn busy(&self) -> bool {
        self.job.is_some()
    }

    /// Session usable for a change right now (no job running).
    pub fn idle_session(&self) -> Option<&Session> {
        if self.busy() { None } else { self.session.as_ref() }
    }

    pub fn instance_name(&self) -> Option<&str> {
        match self.session.as_ref() {
            Some(s) => Some(s.instance.name.as_str()),
            None if self.busy() => self.dlg.opening.as_deref().or(self.facts.instance.as_deref()),
            None => None,
        }
    }

    pub fn hide_epochs(&self) -> bool {
        self.config.hangar.hide_epochs
    }

    pub fn hide_v(&self) -> bool {
        self.config.hangar.hide_v
    }

    /// Save the config and copy it into the session.
    pub fn commit_config(&mut self) {
        if let Err(e) = self.config.save() {
            self.show_error("Cannot save settings", &e.to_string());
        }
        if let Some(s) = self.session.as_mut() {
            s.config = self.config.clone();
        }
    }

    /// Set the text size (percent), apply it from the next frame and save it.
    pub fn set_text_size(&mut self, pct: u16) {
        let pct = text_size_ok(pct);
        self.ctx.set_zoom_factor(f32::from(pct) / 100.0);
        self.status = format!("Text size {pct}%");
        if pct != self.config.hangar.text_size {
            self.config.hangar.text_size = pct;
            self.commit_config();
        }
    }

    /// Go to the next larger (`up`) or smaller text size in `TEXT_SIZES`.
    fn step_text_size(&mut self, up: bool) {
        let cur = text_size_ok(self.config.hangar.text_size);
        let next = if up {
            TEXT_SIZES.iter().copied().find(|&p| p > cur)
        } else {
            TEXT_SIZES.iter().rev().copied().find(|&p| p < cur)
        };
        self.set_text_size(next.unwrap_or(cur));
    }

    /// Try again to open an instance that was locked by another program.
    fn retry_locked(&mut self, ctx: &egui::Context) {
        let Some(w) = self.wait_lock.as_ref() else { return };
        if self.job.is_some() {
            return;
        }
        let now = Instant::now();
        if now < w.next {
            ctx.request_repaint_after(w.next - now);
            return;
        }
        let name = w.name.clone();
        self.open_instance(name);
    }

    pub fn show_error(&mut self, title: &str, text: &str) {
        self.status = format!("{title}: {}", text.lines().next().unwrap_or(""));
        self.dlg.error = Some((title.to_string(), text.to_string()));
    }

    /// Pending changes were edited: the old plan no longer applies.
    pub fn pending_changed(&mut self) {
        self.plan = None;
        self.plan_error = None;
        if matches!(self.sort.0, Col::Installed | Col::Update | Col::Replace) {
            self.view_dirty = true;
        }
    }

    pub fn resolver_options(&self) -> crate::resolve::ResolverOptions {
        // Recommendations and suggestions are offered in a dialog instead.
        crate::resolve::ResolverOptions {
            with_recommends: false,
            with_suggests: false,
            with_all_suggests: false,
            allow_incompatible: false,
        }
    }

    // ------------------------------------------------------------ jobs

    /// Move the session into a job. False if busy or no session.
    pub fn start_job(&mut self, title: &str, f: impl FnOnce(Session, &Reporter) -> Done + Send + 'static) -> bool {
        if self.job.is_some() {
            return false;
        }
        let Some(s) = self.session.take() else { return false };
        self.status = format!("{title}...");
        self.job = Some(Job::spawn(&self.ctx, title, move |rep| f(s, rep)));
        true
    }

    /// A job that does not need the session.
    pub fn start_free_job(&mut self, title: &str, f: impl FnOnce(&Reporter) -> Done + Send + 'static) -> bool {
        if self.job.is_some() {
            return false;
        }
        self.status = format!("{title}...");
        self.job = Some(Job::spawn(&self.ctx, title, f));
        true
    }

    /// Compatible ids before the last repo update, so rebuilt rows keep their "new" marks.
    pub fn prev_compat(&self) -> Option<Arc<HashSet<String>>> {
        self.prev_compat.clone()
    }

    /// Open an instance (None = auto-start / only / first) on the worker.
    pub fn open_instance(&mut self, name: Option<String>) {
        if self.job.is_some() {
            return;
        }
        if self.config.game_instances.is_empty() {
            self.dlg.open_instances();
            return;
        }
        // Drop the old session first so its lock is released.
        self.session = None;
        self.facts = SessionFacts::default();
        self.index = None;
        self.model = None;
        self.view.clear();
        self.selected = None;
        self.pending.clear();
        self.pending_changed();
        self.flow = Flow::Idle;
        self.prev_compat = None;
        let c = &self.config;
        self.dlg.opening = name
            .clone()
            .or_else(|| c.auto_start_instance.clone().filter(|n| c.instance(n).is_some()))
            .or_else(|| c.game_instances.first().map(|e| e.name.clone()));
        let cfg = self.config.clone();
        self.start_free_job("Opening instance", move |rep| {
            rep.progress("Loading registry and repository data", None);
            match Session::open(cfg, name.as_deref()) {
                Ok(s) => worker::done_with(s, None, Outcome::Opened(Ok(()))),
                Err(e) => Done { session: None, model: None, facts: None, outcome: Outcome::Opened(Err(e)) },
            }
        });
    }

    pub fn update_repos(&mut self, force: bool) {
        if self.busy() {
            return;
        }
        let before: Option<Arc<HashSet<String>>> = self.model.as_ref().map(|m| Arc::new(m.compatible_ids.clone()));
        let old = self.prev_compat.clone();
        self.next_prev = before.clone();
        self.start_job("Updating repositories", move |mut s, rep| {
            let p = |m: &str, f: Option<f32>| rep.progress(m, f);
            let r = s.update_repos(force, &p);
            // No new data: keep the marks of the last real update.
            let prev = if matches!(r, Ok(crate::repo::UpdateResult::NoChanges)) { old } else { before };
            worker::done_with(s, prev.as_deref(), Outcome::Updated(r))
        });
    }

    pub fn rebuild(&mut self) {
        let prev = self.prev_compat();
        self.start_job("Refreshing list", move |s, _| worker::done_with(s, prev.as_deref(), Outcome::Rebuilt));
    }

    pub fn start_plan(&mut self, req: crate::resolve::Request, mode: PlanMode) {
        let opts = self.resolver_options();
        self.start_job("Planning changes", move |s, rep| {
            rep.progress("Resolving dependencies", None);
            let result = s.plan(&req, &opts).map(|cs| {
                let opt = if mode == PlanMode::Apply {
                    crate::resolve::optional_mods(&s.context(), &cs)
                } else {
                    Vec::new()
                };
                (cs, opt)
            });
            worker::done_same(s, Outcome::Planned { mode, req, result })
        });
    }

    pub fn start_apply(&mut self, cs: Changeset, trash_first: Vec<String>) {
        let close_game = self.play.running.load(Ordering::Relaxed);
        if close_game {
            self.show_error("Game is running", "Close Kerbal Space Program before you change mods.");
            return;
        }
        let prev = self.prev_compat();
        let started = self.start_job("Applying changes", move |mut s, rep| {
            let p = |m: &str, f: Option<f32>| rep.progress(m, f);
            let result = if trash_first.is_empty() {
                s.apply(&cs, &p)
            } else {
                rep.progress(&format!("Moving {} file(s) to the Trash", trash_first.len()), None);
                match s.trash_files(&trash_first) {
                    Ok(()) => s.apply(&cs, &p),
                    Err(e) => Err(e),
                }
            };
            worker::done_with(s, prev.as_deref(), Outcome::Applied { cs, result })
        });
        if started {
            self.mark_no_close();
        }
    }

    /// The running job changes the game folder: closing the window waits for it.
    pub fn mark_no_close(&mut self) {
        if let Some(j) = self.job.as_mut() {
            j.no_close = true;
        }
    }

    /// Keep the window open while a job that changes files runs; quit when it ends.
    fn guard_close(&mut self, ctx: &egui::Context) {
        if !ctx.input(|i| i.viewport().close_requested()) {
            return;
        }
        if let Some(job) = self.job.as_ref().filter(|j| j.no_close) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.status = format!("Hangar quits when \"{}\" is done", job.title);
            self.quit_after_job = true;
        }
    }

    fn poll_job(&mut self) {
        let Some(job) = self.job.as_mut() else { return };
        match job.poll() {
            Poll::Running => {}
            Poll::Lost => {
                let title = job.title.clone();
                self.job = None;
                self.show_error(&title, "The background job ended without a result. Reopen the instance.");
            }
            Poll::Done(done) => {
                let title = job.title.clone();
                let ms = job.started.elapsed().as_millis();
                let log = std::mem::take(&mut job.log);
                self.job = None;
                self.finish(*done, &title, ms, log);
            }
        }
        if self.quit_after_job && self.job.is_none() {
            self.quit_after_job = false;
            self.ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn finish(&mut self, done: Done, title: &str, ms: u128, log: Vec<String>) {
        let Done { session, model, facts, outcome } = done;
        if let Some(s) = session {
            self.index = Some(Arc::clone(&s.index));
            self.session = Some(s);
        }
        if let Some(f) = facts {
            self.facts = f;
        }
        if let Some(m) = model {
            self.set_model(m);
        }
        let ok = !matches!(
            &outcome,
            Outcome::Opened(Err(_))
                | Outcome::Updated(Err(_))
                | Outcome::Applied { result: Err(_), .. }
                | Outcome::Downloaded(Err(_))
                | Outcome::Imported(Err(_))
                | Outcome::RepoList(Err(_))
        );
        self.job_times.push(super::selftest::JobTime { title: title.to_string(), ms, ok });
        match outcome {
            Outcome::Opened(Ok(())) => {
                self.dlg.opening = None;
                self.wait_lock = None;
                if let Some(s) = self.session.as_mut() {
                    // Keep the Hangar settings changed while the instance opened (text size, columns).
                    let hangar = std::mem::take(&mut self.config.hangar);
                    self.config = s.config.clone();
                    self.config.hangar = hangar;
                    s.config.hangar = self.config.hangar.clone();
                    let n = self.model.as_ref().map_or(0, |m| m.rows.len());
                    self.status = format!("Opened {} ({n} mods) in {ms} ms", s.instance.name);
                    if !s.warnings.is_empty() {
                        let w = s.warnings.join("\n");
                        self.dlg.error = Some(("Opened with warnings".into(), w));
                    }
                    // CKAN asks again after the game version changed.
                    if s.instance.version.is_some() && s.instance.compat_from_other_game_version() && self.selftest.is_none() {
                        self.dlg.open_compat(Some(s));
                    }
                }
                let stale = match self.facts.repo_time {
                    None => true,
                    Some(t) => SystemTime::now().duration_since(t).map_or(true, |d| d > Duration::from_secs(3600)),
                };
                if self.config.hangar.update_repos_on_launch && stale {
                    self.update_repos(false);
                }
            }
            Outcome::Opened(Err(crate::error::Error::Locked(m))) => {
                // Another Hangar window or CKAN uses this game folder: wait for it to close.
                let name = self.dlg.opening.take();
                self.status = format!("Waiting for {} to be free", name.as_deref().unwrap_or("the instance"));
                self.wait_lock = Some(WaitLock { name, message: m, next: Instant::now() + LOCK_RETRY });
            }
            Outcome::Opened(Err(e)) => {
                self.wait_lock = None;
                let name = self.dlg.opening.take().unwrap_or_else(|| "the instance".into());
                self.show_error(&format!("Cannot open {name}"), &e.to_string());
                self.dlg.open_instances();
            }
            Outcome::Updated(Ok(r)) => {
                let next = self.next_prev.take();
                if r == crate::repo::UpdateResult::Updated {
                    self.prev_compat = next;
                }
                self.status = match r {
                    crate::repo::UpdateResult::Updated => format!("Repositories updated in {ms} ms"),
                    crate::repo::UpdateResult::NoChanges => "Repositories are up to date".into(),
                };
            }
            Outcome::Updated(Err(e)) => {
                // Some repos may have new data: the rows were built against the old list.
                self.prev_compat = self.next_prev.take();
                self.show_error("Cannot update repositories", &e.to_string());
            }
            Outcome::Planned { mode, req, result } => super::changes::planned(self, mode, req, result),
            Outcome::Applied { cs, result } => super::changes::applied(self, cs, result, log),
            Outcome::Downloaded(Ok(())) => self.status = "Downloaded to cache".into(),
            Outcome::Downloaded(Err(e)) => self.show_error("Download failed", &e.to_string()),
            Outcome::Imported(r) => super::dialogs::imported(self, r),
            Outcome::RepoList(r) => match r {
                Ok(list) => self.dlg.repo_list_arrived(list),
                Err(e) => self.show_error("Cannot fetch the repository list", &e.to_string()),
            },
            Outcome::Rebuilt => self.status = "List refreshed".into(),
        }
    }

    pub fn set_model(&mut self, m: RowModel) {
        self.pending.retain_valid(&m);
        // The session changed (versions, compat, installed mods): an old plan may be wrong.
        self.plan = None;
        self.plan_error = None;
        if let Some(s) = self.session.as_ref() {
            self.label_info = LabelInfo::of(&s.labels);
        }
        self.model = Some(m);
        self.view_dirty = true;
        self.info.reset();
        if self.first_loaded_ms.is_none() {
            self.first_loaded_ms = Some(self.t0.elapsed().as_millis());
        }
    }

    pub fn refresh_view(&mut self) {
        let Some(m) = self.model.as_ref() else {
            self.view.clear();
            self.view_dirty = false;
            return;
        };
        let t = Instant::now();
        let mut v = model::filter_rows(m, &self.filter, &self.search, &self.label_info);
        model::sort_rows(&m.rows, &mut v, self.sort.0, self.sort.1, &self.pending);
        self.view = v;
        self.view_dirty = false;
        self.filter_ms = t.elapsed().as_millis();
    }

    pub fn set_search(&mut self, text: String) {
        self.search = ModSearch::parse(&text);
        self.search_text = text;
        self.view_dirty = true;
    }

    pub fn selected_row(&self) -> Option<&model::Row> {
        let m = self.model.as_ref()?;
        m.row(m.find(self.selected.as_deref()?)?)
    }

    pub fn select_id(&mut self, id: &str) {
        let Some(m) = self.model.as_ref() else { return };
        if m.find(id).is_none() {
            self.status = format!("{id} is not in the list");
            return;
        }
        self.selected = Some(id.to_string());
        self.scroll_to_selected = true;
        // Make sure it is visible.
        if !self.view.iter().any(|&i| m.row(i).is_some_and(|r| r.id == id)) {
            self.filter = ListFilter::Basic(Filter::All);
            self.set_search(String::new());
        }
    }

    // ------------------------------------------------------------ play

    pub fn launch_game(&mut self, ignore_incompatible: bool) {
        let Some(s) = self.idle_session() else { return };
        if !ignore_incompatible {
            let bad: Vec<String> = self
                .model
                .as_ref()
                .map(|m| {
                    m.rows
                        .iter()
                        .filter(|r| r.installed && !r.compatible && r.kind == model::RowKind::Module)
                        .map(|r| format!("{} {}", r.name, r.installed_version))
                        .collect()
                })
                .unwrap_or_default();
            if !bad.is_empty() {
                self.dlg.play_warning = Some(bad);
                return;
            }
        }
        let inst = s.instance.clone();
        match inst.launch() {
            Err(e) => self.show_error("Cannot start the game", &e.to_string()),
            Ok(mut child) => {
                self.play.running.store(true, Ordering::Relaxed);
                self.status = "Game started".into();
                let play = Arc::clone(&self.play);
                let ctx = self.ctx.clone();
                let spawned = std::thread::Builder::new().name("hangar-play".into()).spawn(move || {
                    let t = Instant::now();
                    let status = child.wait();
                    let d = t.elapsed();
                    let msg = match (status, inst.add_playtime(d)) {
                        (Ok(_), Ok(())) => format!("Game closed after {}", model::hours_text(d)),
                        (Ok(_), Err(e)) => format!("Game closed; cannot save play time: {e}"),
                        (Err(e), _) => format!("Lost track of the game: {e}"),
                    };
                    if let Ok(mut r) = play.result.lock() {
                        *r = Some((msg, Some(inst.playtime())));
                    }
                    play.running.store(false, Ordering::Relaxed);
                    ctx.request_repaint();
                });
                if spawned.is_err() {
                    self.play.running.store(false, Ordering::Relaxed);
                }
            }
        }
    }

    fn poll_play(&mut self) {
        let msg = self.play.result.lock().ok().and_then(|mut r| r.take());
        if let Some((m, t)) = msg {
            self.status = m;
            if let Some(t) = t {
                self.facts.playtime = t;
            }
        }
    }

    // ------------------------------------------------------------ frame parts

    fn shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::F)) {
            self.tab = Tab::Mods;
            self.focus_search = true;
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::R)) && !self.busy() {
            self.update_repos(false);
        }
        use egui::gui_zoom::kb_shortcuts as zoom;
        let (larger, smaller, normal) = ctx.input_mut(|i| {
            // `|` and not `||`: consume both forms of the key.
            let larger = i.consume_shortcut(&zoom::ZOOM_IN) | i.consume_shortcut(&zoom::ZOOM_IN_SECONDARY);
            (larger, i.consume_shortcut(&zoom::ZOOM_OUT), i.consume_shortcut(&zoom::ZOOM_RESET))
        });
        if normal {
            self.set_text_size(100);
        } else if larger {
            self.step_text_size(true);
        } else if smaller {
            self.step_text_size(false);
        }
        let no_window = !self.dlg.any_open() && matches!(self.flow, Flow::Idle);
        if no_window && ctx.input(|i| i.key_pressed(Key::Escape)) && !self.search_text.is_empty() {
            self.set_search(String::new());
        }
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        let idle = !self.busy();
        let has = self.session.is_some() && idle;
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.add_enabled(has, egui::Button::new("Install from .ckan...")).clicked() {
                    super::dialogs::pick_ckan_files(self);
                }
                if ui.add_enabled(has, egui::Button::new("Import downloaded mods...")).clicked() {
                    self.dlg.import = Some(Default::default());
                }
                ui.separator();
                if ui.add_enabled(has, egui::Button::new("Export modpack...")).clicked() {
                    self.dlg.open_export(self.session.as_ref(), true);
                }
                if ui.add_enabled(has, egui::Button::new("Save installed mod list...")).clicked() {
                    self.dlg.open_export(self.session.as_ref(), false);
                }
                ui.separator();
                if ui.add_enabled(has, egui::Button::new("Installation history...")).clicked() {
                    self.dlg.open_history(self.session.as_ref());
                }
                if ui.add_enabled(has, egui::Button::new("Unmanaged files...")).clicked() {
                    self.dlg.open_unmanaged(self.session.as_ref());
                }
                if ui.add_enabled(has, egui::Button::new("Audit recommendations...")).clicked() {
                    super::changes::audit_recommendations(self);
                }
                ui.separator();
                if ui.add_enabled(idle, egui::Button::new("Instances...")).clicked() {
                    self.dlg.open_instances();
                }
                ui.separator();
                if ui.add(egui::Button::new("Quit").shortcut_text("Cmd+Q")).clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button("Settings", |ui| {
                if ui.add_enabled(idle, egui::Button::new("Settings...")).clicked() {
                    self.dlg.open_settings(&self.config, self.session.as_ref());
                }
                if ui.add_enabled(has, egui::Button::new("Compatible game versions...")).clicked() {
                    self.dlg.open_compat(self.session.as_ref());
                }
                if ui.add_enabled(has, egui::Button::new("Labels...")).clicked() {
                    self.dlg.open_labels(self.session.as_ref());
                }
                if ui.add_enabled(has, egui::Button::new("Install filters...")).clicked() {
                    self.dlg.open_filters(&self.config, self.session.as_ref());
                }
                ui.separator();
                ui.menu_button("Columns", |ui| {
                    for c in model::COLUMNS.iter().copied().filter(|c| c.can_hide()) {
                        let mut shown = !self.config.hangar.hidden_columns.iter().any(|h| h == c.key());
                        if ui.checkbox(&mut shown, c.title()).changed() {
                            toggle_column(&mut self.config, c);
                            self.commit_config();
                        }
                    }
                });
            });
            ui.menu_button("View", |ui| {
                let cur = text_size_ok(self.config.hangar.text_size);
                let max = TEXT_SIZES[TEXT_SIZES.len() - 1];
                if ui.add_enabled(cur < max, egui::Button::new("Larger text").shortcut_text("Cmd+=")).clicked() {
                    self.step_text_size(true);
                }
                if ui.add_enabled(cur > TEXT_SIZES[0], egui::Button::new("Smaller text").shortcut_text("Cmd+-")).clicked() {
                    self.step_text_size(false);
                }
                if ui.add_enabled(cur != 100, egui::Button::new("Normal text size").shortcut_text("Cmd+0")).clicked() {
                    self.set_text_size(100);
                }
                ui.separator();
                ui.menu_button(format!("Text size: {cur}%"), |ui| {
                    for p in TEXT_SIZES {
                        if ui.radio(cur == p, format!("{p}%")).clicked() {
                            self.set_text_size(p);
                        }
                    }
                });
            });
            ui.menu_button("Help", |ui| {
                if ui.button("Keyboard shortcuts").clicked() {
                    self.dlg.shortcuts = true;
                }
                if ui.button("About Hangar").clicked() {
                    self.dlg.about = true;
                }
            });
        });
    }

    /// The toolbar. Apply and Play are placed first (at the right), so they stay visible at
    /// large text sizes. When the rest of the row has too little width, the filter and the
    /// search box go on a second row.
    fn toolbar(&mut self, ui: &mut egui::Ui) {
        let mut two_rows = false;
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                self.toolbar_actions(ui);
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    // Width for instance, Refresh, Update all, Filter and a 160 pt search box.
                    two_rows = ui.available_width() < 700.0;
                    self.toolbar_main(ui);
                    if !two_rows {
                        ui.separator();
                        self.filter_and_search(ui, 320.0);
                    }
                });
            });
        });
        if two_rows {
            ui.horizontal(|ui| self.filter_and_search(ui, 600.0));
        }
    }

    /// Play and Apply changes, added right to left.
    fn toolbar_actions(&mut self, ui: &mut egui::Ui) {
        let has = self.session.is_some() && !self.busy();
        let playing = self.play.running.load(Ordering::Relaxed);
        let play_text = if playing { "Playing" } else { "Play" };
        // Green with white text, in the light and the dark theme.
        let play = egui::Button::new(RichText::new(play_text).strong().color(Color32::WHITE)).fill(GREEN);
        if ui.add_enabled(has && !playing, play).on_hover_text("Launch KSP").clicked() {
            self.launch_game(false);
        }
        let n = self.pending.len();
        let apply = egui::Button::new(RichText::new(format!("Apply changes ({n})")).strong())
            .fill(if n > 0 { GREEN.gamma_multiply(0.35) } else { Color32::TRANSPARENT });
        if ui.add_enabled(has && n > 0, apply).clicked() {
            super::changes::begin_apply(self);
        }
    }

    /// Instance picker, Refresh and Update all.
    fn toolbar_main(&mut self, ui: &mut egui::Ui) {
        let idle = !self.busy();
        let current = self.instance_name().unwrap_or("No instance").to_string();
        let mut pick: Option<String> = None;
        let combo_w = (ui.available_width() * 0.3).clamp(90.0, 170.0);
        ui.add_enabled_ui(idle, |ui| {
            egui::ComboBox::from_id_salt("instance_pick")
                .selected_text(RichText::new(&current).strong())
                .width(combo_w)
                .truncate()
                .show_ui(ui, |ui| {
                    for e in &self.config.game_instances {
                        if ui.selectable_label(e.name == current, &e.name).clicked() && e.name != current {
                            pick = Some(e.name.clone());
                        }
                    }
                    ui.separator();
                    if ui.button("Manage instances...").clicked() {
                        self.dlg.open_instances();
                    }
                });
        });
        if let Some(name) = pick {
            self.open_instance(Some(name));
        }
        ui.separator();
        let has = self.session.is_some() && idle;
        if ui.add_enabled(has, egui::Button::new("Refresh")).on_hover_text("Update repositories (Cmd+R)").clicked() {
            self.update_repos(false);
        }
        let ups = self.model.as_ref().map_or(0, |m| m.counts.upgradeable);
        if ui
            .add_enabled(has && ups > 0, egui::Button::new(format!("Update all ({ups})")))
            .on_hover_text("Mark every mod with an update; held mods are skipped")
            .clicked()
        {
            if let Some(m) = self.model.as_ref() {
                let n = self.pending.mark_all_upgrades(&m.rows);
                self.status = format!("{n} update(s) marked");
            }
            self.pending_changed();
        }
    }

    /// The filter menu and the search box (at most `max_w` points wide).
    fn filter_and_search(&mut self, ui: &mut egui::Ui, max_w: f32) {
        self.filter_menu(ui);
        // Keep room for the clear button.
        let w = (ui.available_width() - 32.0).clamp(120.0, max_w);
        let resp = ui.add(
            egui::TextEdit::singleline(&mut self.search_text)
                .id(egui::Id::unique("search_box"))
                .hint_text(SEARCH_HINT)
                .desired_width(w),
        );
        if self.focus_search {
            resp.request_focus();
            self.focus_search = false;
        }
        if resp.changed() {
            let t = self.search_text.clone();
            self.set_search(t);
        }
        if !self.search_text.is_empty() && ui.small_button("x").on_hover_text("Clear search (Esc)").clicked() {
            self.set_search(String::new());
        }
    }

    fn filter_menu(&mut self, ui: &mut egui::Ui) {
        let title = self.filter.label(&self.label_info.names);
        let mut new_filter: Option<ListFilter> = None;
        ui.menu_button(format!("Filter: {title}"), |ui| {
            for f in Filter::ALL {
                let sel = self.filter == ListFilter::Basic(f);
                if ui.selectable_label(sel, f.label()).clicked() {
                    new_filter = Some(ListFilter::Basic(f));
                }
            }
            ui.separator();
            ui.menu_button("Labels", |ui| {
                if self.label_info.names.is_empty() {
                    ui.weak("No labels");
                }
                for (i, name) in self.label_info.names.iter().enumerate() {
                    if ui.selectable_label(self.filter == ListFilter::Label(i), name).clicked() {
                        new_filter = Some(ListFilter::Label(i));
                    }
                }
            });
            ui.menu_button("Tags", |ui| {
                egui::ScrollArea::vertical().max_height(super::dialogs::fit_h(ui.ctx(), 420.0)).show(ui, |ui| {
                    if let Some(m) = self.model.as_ref() {
                        let sel = self.filter == ListFilter::Untagged;
                        if ui.selectable_label(sel, format!("(untagged) ({})", m.untagged)).clicked() {
                            new_filter = Some(ListFilter::Untagged);
                        }
                        for (t, n) in &m.tags {
                            let sel = matches!(&self.filter, ListFilter::Tag(x) if x == t);
                            if ui.selectable_label(sel, format!("{t} ({n})")).clicked() {
                                new_filter = Some(ListFilter::Tag(t.clone()));
                            }
                        }
                    } else {
                        ui.weak("No data");
                    }
                });
            });
        });
        if let Some(f) = new_filter {
            self.filter = f;
            self.view_dirty = true;
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if let Some(job) = self.job.as_ref() {
                let bar = match job.fraction {
                    Some(f) => egui::ProgressBar::new(f).show_percentage(),
                    // No animation: it would repaint every frame.
                    None => egui::ProgressBar::new(0.0),
                };
                ui.add(bar.desired_width(180.0).desired_height(14.0));
                ui.label(RichText::new(&job.message).small());
            } else {
                ui.label(RichText::new(&self.status).small());
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let small = |t: String| RichText::new(t).small();
                if self.facts.instance.is_some() && (self.session.is_some() || self.busy()) {
                    let age = match self.facts.repo_time {
                        Some(t) => {
                            let secs = SystemTime::now().duration_since(t).map(|d| d.as_secs()).unwrap_or(0);
                            format!("Repo data {}", model::age_text(secs))
                        }
                        None => "No repo cache".into(),
                    };
                    ui.label(small(age));
                    ui.separator();
                    if let Some(m) = self.model.as_ref() {
                        let c = m.counts;
                        ui.label(small(format!(
                            "{} installed, {} upgradeable, {} available",
                            c.installed, c.upgradeable, c.compatible
                        )));
                        ui.separator();
                    }
                    let playing = if self.play.running.load(Ordering::Relaxed) { " (playing)" } else { "" };
                    ui.label(small(format!("Played {}{playing}", model::hours_text(self.facts.playtime))));
                    ui.separator();
                    let gv = self.facts.game_version.clone().unwrap_or_else(|| "unknown".into());
                    ui.label(small(format!("KSP {gv}")));
                }
            });
        });
    }

    fn central(&mut self, ui: &mut egui::Ui) {
        if self.model.is_none() {
            empty_state(ui, self);
            return;
        }
        ui.horizontal(|ui| {
            let n = self.pending.len();
            if ui.selectable_label(self.tab == Tab::Mods, "Mods").clicked() {
                self.tab = Tab::Mods;
            }
            let changes = if n > 0 { format!("Changes ({n})") } else { "Changes".into() };
            if ui.selectable_label(self.tab == Tab::Changes, changes).clicked() && self.tab != Tab::Changes {
                self.tab = Tab::Changes;
                super::changes::preview_if_needed(self);
            }
            if self.tab == Tab::Mods {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let total = self.model.as_ref().map_or(0, |m| m.rows.len());
                    ui.weak(format!("{} of {} mods shown", self.view.len(), total));
                });
            }
        });
        ui.separator();
        match self.tab {
            Tab::Mods => super::table::show(self, ui),
            Tab::Changes => super::changes::show_tab(self, ui),
        }
    }
}

pub fn toggle_column(cfg: &mut Config, c: Col) {
    let hidden = &mut cfg.hangar.hidden_columns;
    if let Some(p) = hidden.iter().position(|h| h == c.key()) {
        hidden.remove(p);
    } else if c.can_hide() {
        hidden.push(c.key().to_string());
    }
}

fn empty_state(ui: &mut egui::Ui, app: &mut App) {
    ui.vertical_centered(|ui| {
        ui.add_space(ui.available_height() * 0.3);
        if app.busy() {
            ui.heading("Loading...");
            if let Some(j) = app.job.as_ref() {
                ui.weak(&j.message);
            }
        } else if let Some(w) = app.wait_lock.as_ref() {
            let name = w.name.clone().unwrap_or_else(|| "The instance".into());
            ui.heading(format!("{name} is in use"));
            ui.weak("Another Hangar window or CKAN has this game folder open.");
            ui.weak(format!("Hangar opens {name} as soon as that program closes."));
            ui.add_space(6.0);
            ui.label(RichText::new(w.message.lines().next().unwrap_or("")).small().weak());
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                // Center the two buttons.
                let w = 2.0 * 110.0 + ui.spacing().item_spacing.x;
                ui.add_space(((ui.available_width() - w) / 2.0).max(0.0));
                if ui.add_sized([110.0, 24.0], egui::Button::new("Try now")).clicked()
                    && let Some(wl) = app.wait_lock.as_mut()
                {
                    wl.next = Instant::now();
                }
                if ui.add_sized([110.0, 24.0], egui::Button::new("Stop waiting")).clicked() {
                    app.wait_lock = None;
                    app.status = "Stopped waiting".into();
                    app.dlg.open_instances();
                }
            });
        } else if app.session.is_none() {
            ui.heading("No instance selected");
            ui.weak("Add or pick a KSP game folder to manage its mods.");
            ui.add_space(8.0);
            if ui.button("Manage instances...").clicked() {
                app.dlg.open_instances();
            }
        } else {
            ui.heading("No repository data");
            ui.weak("Press Refresh to download the mod list.");
            ui.add_space(8.0);
            if ui.button("Refresh").clicked() {
                app.update_repos(false);
            }
        }
    });
}

impl eframe::App for App {
    /// Runs before `ui`, also while the window is hidden: jobs finish and quit is guarded.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_job();
        self.poll_play();
        self.guard_close(ctx);
        self.retry_locked(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.frames += 1;
        let ctx = ui.ctx().clone();
        if self.frame_log {
            eprintln!("frame {} t={}ms job={}", self.frames, self.t0.elapsed().as_millis(), self.job.is_some());
        }
        if !self.started {
            self.started = true;
            if self.config.game_instances.is_empty() {
                self.status = "No instance configured".into();
                if self.selftest.is_none() {
                    self.dlg.open_instances();
                }
            } else {
                self.open_instance(None);
            }
        }
        self.shortcuts(&ctx);
        if self.view_dirty {
            self.refresh_view();
        }

        egui::Panel::top("menu").show(ui, |ui| self.menu_bar(ui));
        egui::Panel::top("toolbar")
            .frame(egui::Frame::side_top_panel(ui.style()).inner_margin(egui::Margin::symmetric(8, 6)))
            .show(ui, |ui| self.toolbar(ui));
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        if self.tab == Tab::Mods && self.selected_row().is_some() {
            egui::Panel::right("info")
                .resizable(true)
                .default_size(400.0f32.min(ui.available_width() * 0.4))
                .size_range((ui.available_width() * 0.25).min(300.0)..=(ui.available_width() * 0.5).min(600.0))
                .show(ui, |ui| super::info::show(self, ui));
        }
        egui::CentralPanel::default().show(ui, |ui| self.central(ui));

        super::dialogs::show_all(self, &ctx);
        super::changes::show_flow(self, &ctx);
        if self.view_dirty {
            // A dialog or action changed the view; draw again with the new rows.
            ctx.request_repaint();
        }
        super::selftest::tick(self, &ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // Cmd+Q ends the app without a close request: let an apply finish its transaction.
        if let Some(job) = self.job.as_mut().filter(|j| j.no_close) {
            job.wait();
        }
        if self.config.hangar.last_search != self.search_text && self.selftest.is_none() {
            self.config.hangar.last_search = self.search_text.clone();
            let _ = self.config.save();
        }
    }
}
