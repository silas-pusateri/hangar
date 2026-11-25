//! Hidden self-test. `HANGAR_GUI_SELFTEST=<report.json>` waits until the instance is loaded,
//! writes a JSON report and closes the window. With `HANGAR_GUI_SHOTS=<dir>` it first walks
//! through a few screens (info tabs, apply flow, Changes tab, dialogs, dark theme) and saves a
//! BMP screenshot of each. It never applies changes, except with
//! `HANGAR_GUI_SELFTEST_INSTALL=<id>`: then it installs that mod through the normal apply flow
//! (plan, recommendations unticked, confirm, apply on the worker), reinstalls it, then removes
//! it again. While the first apply runs it asks the window to close, which must be deferred.
//! `HANGAR_GUI_SELFTEST_CONFLICT=<relative path>` first creates that file in the game folder,
//! so the install hits the conflict dialog and uses "Move to Trash and retry".

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use eframe::egui;

use super::app::{App, Tab};
use super::changes::Flow;
use super::info::InfoTab;
use super::model::{self, Col, ListFilter};
use crate::modlist::search::{Filter, ModSearch};

/// A finished job, for the report.
#[derive(serde::Serialize)]
pub struct JobTime {
    pub title: String,
    pub ms: u128,
    pub ok: bool,
}

pub struct SelfTest {
    path: PathBuf,
    shots: Option<PathBuf>,
    done: bool,
    step: usize,
    /// Screenshots asked for / written.
    asked: usize,
    written: Arc<AtomicUsize>,
    not_before: Instant,
    frames_waited: u32,
    notes: Vec<String>,
    install: Option<String>,
    install_step: usize,
    /// Unowned file to create before the install (conflict test).
    conflict: Option<String>,
    conflict_retried: bool,
    /// A close request was sent while an apply ran.
    close_sent: bool,
    close_tested: bool,
}

impl SelfTest {
    pub fn from_env() -> Option<SelfTest> {
        let path = std::env::var_os("HANGAR_GUI_SELFTEST").filter(|v| !v.is_empty())?;
        let shots = std::env::var_os("HANGAR_GUI_SHOTS").filter(|v| !v.is_empty()).map(PathBuf::from);
        Some(SelfTest {
            path: PathBuf::from(path),
            shots,
            done: false,
            step: 0,
            asked: 0,
            written: Arc::new(AtomicUsize::new(0)),
            not_before: Instant::now(),
            frames_waited: 0,
            notes: Vec::new(),
            install: std::env::var("HANGAR_GUI_SELFTEST_INSTALL").ok().filter(|v| !v.is_empty()),
            install_step: 0,
            conflict: std::env::var("HANGAR_GUI_SELFTEST_CONFLICT").ok().filter(|v| !v.is_empty()),
            conflict_retried: false,
            close_sent: false,
            close_tested: false,
        })
    }
}

/// Write an image as a 32-bit top-down BMP (no extra crates needed).
pub fn write_bmp(path: &Path, img: &egui::ColorImage) -> std::io::Result<()> {
    let [w, h] = img.size;
    let (Ok(w32), Ok(h32)) = (i32::try_from(w), i32::try_from(h)) else {
        return Err(std::io::Error::other("image too big"));
    };
    let data_len = w * h * 4;
    let mut out = Vec::with_capacity(54 + data_len);
    let file_len = u32::try_from(54 + data_len).unwrap_or(u32::MAX);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&file_len.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&w32.to_le_bytes());
    out.extend_from_slice(&(-h32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&[0u8; 24]);
    for p in &img.pixels {
        let [r, g, b, a] = p.to_srgba_unmultiplied();
        out.extend_from_slice(&[b, g, r, a]);
    }
    std::fs::write(path, out)
}

fn shot(st: &mut SelfTest, ctx: &egui::Context, name: &str) {
    let Some(dir) = st.shots.clone() else { return };
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(format!("{name}.bmp"));
    let written = Arc::clone(&st.written);
    let c = ctx.clone();
    st.asked += 1;
    ctx.request_screenshot(move |img| {
        if let Err(e) = write_bmp(&path, &img) {
            eprintln!("selftest: {}: {e}", path.display());
        }
        written.fetch_add(1, Ordering::SeqCst);
        c.request_repaint();
    });
    ctx.request_repaint();
}

/// Wait this long after a state change before the next screenshot (window fade-in).
const SETTLE: Duration = Duration::from_millis(450);

/// One step of the screenshot tour. Returns true when the tour is over.
fn tour(app: &mut App, ctx: &egui::Context) -> bool {
    let busy = app.busy();
    let Some(st) = app.selftest.as_mut() else { return true };
    if st.written.load(Ordering::SeqCst) < st.asked || busy {
        return false;
    }
    // Let animations (window fade-in) finish: some time and some frames.
    st.frames_waited += 1;
    if Instant::now() < st.not_before || st.frames_waited < 12 {
        ctx.request_repaint();
        return false;
    }
    st.frames_waited = 0;
    let step = st.step;
    st.step += 1;
    st.not_before = Instant::now() + SETTLE;
    ctx.request_repaint();
    let top_id = app.model.as_ref().and_then(|m| {
        let mut v: Vec<usize> = (0..m.rows.len()).filter(|&i| m.row(i).is_some_and(|r| r.compatible)).collect();
        model::sort_rows(&m.rows, &mut v, Col::Downloads, false, &app.pending);
        v.first().and_then(|&i| m.row(i)).map(|r| r.id.clone())
    });
    match step {
        0 => {}
        1 => with_st(app, |st| shot(st, ctx, "01-list")),
        2 => {
            if let Some(id) = top_id.clone() {
                app.select_id(&id);
                app.info.tab = InfoTab::Metadata;
            }
        }
        3 => with_st(app, |st| shot(st, ctx, "02-info-metadata")),
        4 => app.info.tab = InfoTab::Relationships,
        5 => with_st(app, |st| shot(st, ctx, "03-info-relationships")),
        6 => app.info.tab = InfoTab::Versions,
        7 => with_st(app, |st| shot(st, ctx, "04-info-versions")),
        8 => {
            // Mark the selected mod and start the apply flow (plan only).
            let row = app.selected_row().cloned();
            if let Some(r) = row {
                app.pending.set_installed(&r, true);
                app.pending_changed();
                super::changes::begin_apply(app);
                note(app, format!("apply flow for {}", r.id));
            }
        }
        9 => {
            let what = match &app.flow {
                Flow::Recommend { items, .. } => format!("recommendations dialog with {} item(s)", items.len()),
                Flow::Confirm { cs } => format!("confirm dialog with {} change(s)", cs.changes.len()),
                Flow::Choose { choices, .. } => format!("provider chooser with {} choice(s)", choices.len()),
                Flow::Idle => format!("no dialog; plan error: {:?}", app.plan_error),
                _ => "other dialog".into(),
            };
            note(app, what);
            with_st(app, |st| shot(st, ctx, "05-apply-flow"));
        }
        10 => {
            if let Flow::Recommend { cs, .. } = std::mem::replace(&mut app.flow, Flow::Idle) {
                app.flow = Flow::Confirm { cs };
            }
        }
        11 => with_st(app, |st| shot(st, ctx, "06-confirm")),
        12 => {
            app.flow = Flow::Idle;
            app.tab = Tab::Changes;
        }
        13 => with_st(app, |st| shot(st, ctx, "07-changes-tab")),
        14 => {
            app.tab = Tab::Mods;
            app.dlg.open_settings(&app.config, app.session.as_ref());
        }
        15 => with_st(app, |st| shot(st, ctx, "08-settings")),
        16 => {
            app.dlg.settings = None;
            app.dlg.open_instances();
        }
        17 => with_st(app, |st| shot(st, ctx, "09-instances")),
        18 => {
            app.dlg.instances = None;
            app.dlg.open_labels(app.session.as_ref());
        }
        19 => with_st(app, |st| shot(st, ctx, "10-labels")),
        20 => {
            app.dlg.labels = None;
            ctx.set_theme(egui::Theme::Dark);
            app.set_search("@ferram".into());
        }
        21 => with_st(app, |st| shot(st, ctx, "11-dark-search")),
        22 => {
            ctx.set_theme(egui::Theme::Light);
            app.set_search(String::new());
            app.filter = ListFilter::Basic(Filter::Incompatible);
            app.view_dirty = true;
            app.selected = None;
        }
        23 => with_st(app, |st| shot(st, ctx, "12-light-incompatible")),
        _ => {
            app.pending.clear();
            app.pending_changed();
            return true;
        }
    }
    false
}

fn note(app: &mut App, s: String) {
    if let Some(st) = app.selftest.as_mut() {
        st.notes.push(s);
    }
}

fn row_state(app: &App, id: &str) -> String {
    match app.model.as_ref().and_then(|m| m.find(id)).and_then(|i| app.model.as_ref().and_then(|m| m.row(i))) {
        Some(r) => format!("{id}: installed={} version={} auto={}", r.installed, r.installed_version, r.auto),
        None => format!("{id}: no row"),
    }
}

/// Drive the flow dialogs the way a user would: untick recommendations, confirm.
/// Returns false while something is still running.
fn drive_flow(app: &mut App) -> bool {
    match std::mem::replace(&mut app.flow, Flow::Idle) {
        Flow::Recommend { cs, .. } => {
            note(app, format!("recommendations shown; confirm {} change(s)", cs.changes.len()));
            app.flow = Flow::Confirm { cs };
            false
        }
        Flow::Confirm { cs } => {
            let lines = model::change_lines(&cs, true, false);
            let text: Vec<String> = lines.iter().map(|(k, n, v, _)| format!("{} {n} {v}", model::kind_text(*k))).collect();
            note(app, format!("apply: {}", text.join("; ")));
            app.start_apply(cs, Vec::new());
            // Ask to quit while the apply runs: the window must stay until it ends.
            if app.busy() && app.selftest.as_ref().is_some_and(|st| !st.close_tested && !st.close_sent) {
                app.ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                with_st(app, |st| st.close_sent = true);
            }
            false
        }
        Flow::Report { title, lines, log, .. } => {
            note(app, format!("{title}: {} ({} log lines)", lines.join(" | "), log.len()));
            true
        }
        Flow::Conflict { cs, msg, files } => {
            note(app, format!("conflict: {} files={files:?}", msg.lines().next().unwrap_or("")));
            if app.selftest.as_ref().is_some_and(|st| !st.conflict_retried) && !files.is_empty() {
                with_st(app, |st| st.conflict_retried = true);
                note(app, "Move to Trash and retry".into());
                app.start_apply(cs, files);
                return false;
            }
            true
        }
        Flow::Choose { requested, choices, .. } => {
            note(app, format!("provider choice needed for {requested} ({} choices); stopping", choices.len()));
            true
        }
        Flow::Idle => {
            if let Some(e) = app.dlg.error.take() {
                note(app, format!("error: {}: {}", e.0, e.1));
            }
            true
        }
    }
}

/// Install then remove one mod through the GUI flow. Returns true when done (or not asked).
fn install_tour(app: &mut App, ctx: &egui::Context) -> bool {
    let Some(st) = app.selftest.as_ref() else { return true };
    let Some(id) = st.install.clone() else { return true };
    let step = st.install_step;
    if app.busy() {
        return false;
    }
    ctx.request_repaint();
    let next = |app: &mut App| {
        if let Some(st) = app.selftest.as_mut() {
            st.install_step += 1;
        }
    };
    match step {
        0 => {
            let conflict = app.selftest.as_ref().and_then(|st| st.conflict.clone());
            if let (Some(rel), Some(s)) = (conflict, app.session.as_ref()) {
                let p = s.instance.game_dir.join(&rel);
                let made = p.parent().map(std::fs::create_dir_all).transpose().and_then(|_| std::fs::write(&p, b"selftest"));
                note(app, format!("made unowned file {rel}: {}", made.is_ok()));
            }
        }
        2 => {
            // Reinstall from the row menu.
            note(app, format!("before reinstall: {}", row_state(app, &id)));
            let installed = app.model.as_ref().and_then(|m| m.find(&id)).and_then(|i| app.model.as_ref().and_then(|m| m.row(i))).is_some_and(|r| r.installed);
            if !installed {
                note(app, "not installed; install tour ends".into());
                with_st(app, |st| st.install = None);
                return true;
            }
            if let Some(ri) = app.model.as_ref().and_then(|m| m.find(&id)) {
                super::table::apply(app, super::table::Action::Reinstall(ri));
            }
            next(app);
            return false;
        }
        _ => {}
    }
    match step {
        0 | 4 => {
            let row = app.model.as_ref().and_then(|m| m.find(&id)).and_then(|i| app.model.as_ref().and_then(|m| m.row(i))).cloned();
            let Some(r) = row else {
                note(app, format!("{id} is not in the list"));
                if let Some(st) = app.selftest.as_mut() {
                    st.install = None;
                }
                return true;
            };
            note(app, format!("before step {step}: {}", row_state(app, &id)));
            app.pending.set_installed(&r, step == 0);
            app.pending_changed();
            super::changes::begin_apply(app);
            next(app);
            false
        }
        1 | 3 | 5 => {
            if drive_flow(app) {
                note(app, format!("after step {step}: {}", row_state(app, &id)));
                next(app);
            }
            false
        }
        _ => true,
    }
}

fn with_st(app: &mut App, f: impl FnOnce(&mut SelfTest)) {
    if let Some(st) = app.selftest.as_mut() {
        f(st);
    }
}

/// The close request sent during an apply must have been deferred, not obeyed.
fn check_deferred_close(app: &mut App) {
    if !app.selftest.as_ref().is_some_and(|st| st.close_sent) || !app.quit_after_job {
        return;
    }
    // Take the quit back so the test can go on.
    app.quit_after_job = false;
    note(app, format!("close request deferred while busy={}: {}", app.busy(), app.status));
    with_st(app, |st| {
        st.close_sent = false;
        st.close_tested = true;
    });
}

pub fn tick(app: &mut App, ctx: &egui::Context) {
    let Some(st) = app.selftest.as_ref() else { return };
    if st.done {
        return;
    }
    check_deferred_close(app);
    let Some(st) = app.selftest.as_ref() else { return };
    let has_shots = st.shots.is_some();
    let no_instance = app.config.game_instances.is_empty() && app.started;
    // Waiting for a locked instance is not a failure: the app opens it when the lock is free.
    let failed = app.session.is_none() && app.job.is_none() && app.started && app.wait_lock.is_none();
    let loaded = app.session.is_some() && app.model.is_some() && app.job.is_none();
    if !(loaded || no_instance || failed) {
        return;
    }
    if loaded && !install_tour(app, ctx) {
        return;
    }
    if loaded && has_shots && !tour(app, ctx) {
        return;
    }
    let report = report(app, loaded);
    let text = serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".into());
    if let Some(st) = app.selftest.as_mut() {
        if let Err(e) = std::fs::write(&st.path, text) {
            eprintln!("selftest: cannot write {}: {e}", st.path.display());
        }
        st.done = true;
    }
    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
}

fn report(app: &mut App, loaded: bool) -> serde_json::Value {
    let mut visible = serde_json::Map::new();
    let mut filter_total_ms = 0u128;
    let mut search_hits = 0usize;
    let mut top: Vec<String> = Vec::new();
    let mut sort_ms = 0u128;
    if let Some(m) = app.model.as_ref() {
        let empty = ModSearch::default();
        for f in Filter::ALL {
            let t = Instant::now();
            let n = model::filter_rows(m, &ListFilter::Basic(f), &empty, &app.label_info).len();
            filter_total_ms += t.elapsed().as_millis();
            visible.insert(f.label().to_string(), n.into());
        }
        let s = ModSearch::parse("kerbal");
        search_hits = model::filter_rows(m, &ListFilter::Basic(Filter::All), &s, &app.label_info).len();
        let mut v: Vec<usize> = (0..m.rows.len()).collect();
        let t = Instant::now();
        model::sort_rows(&m.rows, &mut v, Col::Downloads, false, &app.pending);
        sort_ms = t.elapsed().as_millis();
        top = v.iter().take(5).filter_map(|&i| m.row(i)).map(|r| r.name.clone()).collect();
    }
    app.filter = ListFilter::default();
    app.set_search(String::new());
    app.refresh_view();
    let sample: Vec<String> = app
        .view
        .iter()
        .take(12)
        .filter_map(|&i| app.model.as_ref().and_then(|m| m.row(i)))
        .map(|r| r.name.clone())
        .collect();
    let error = app.dlg.error.as_ref().map(|(t, e)| format!("{t}: {e}"));
    let notes = app.selftest.as_ref().map(|s| s.notes.clone()).unwrap_or_default();
    let shots = app.selftest.as_ref().map_or(0, |s| s.written.load(Ordering::SeqCst));
    serde_json::json!({
        "ok": loaded && error.is_none(),
        "error": error,
        "instance": app.session.as_ref().map(|s| s.instance.name.clone()),
        "game_version": app.session.as_ref().and_then(|s| s.instance.version).map(|v| v.to_string()),
        "repo_module_versions": app.index.as_ref().map_or(0, |i| i.module_count()),
        "rows": app.model.as_ref().map_or(0, |m| m.rows.len()),
        "counts": app.model.as_ref().map(|m| m.counts),
        "default_view_rows": app.view.len(),
        "visible_per_filter": visible,
        "all_filters_ms": filter_total_ms,
        "last_view_refresh_ms": app.filter_ms,
        "search_kerbal_rows": search_hits,
        "sort_downloads_ms": sort_ms,
        "top_downloads": top,
        "sample": sample,
        "tags": app.model.as_ref().map_or(0, |m| m.tags.len()),
        "frames": app.frames,
        "first_loaded_ms": app.first_loaded_ms,
        "row_build_ms": app.model.as_ref().map(|m| m.build_ms),
        "jobs": app.job_times,
        "screenshots": shots,
        "notes": notes,
        "status": app.status,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bmp_header() {
        let img = egui::ColorImage::new([2, 1], vec![egui::Color32::RED, egui::Color32::BLUE]);
        let p = std::env::temp_dir().join(format!("hangar-bmp-{}.bmp", std::process::id()));
        assert!(write_bmp(&p, &img).is_ok());
        let b = std::fs::read(&p).unwrap_or_default();
        assert_eq!(b.len(), 54 + 8);
        assert_eq!(b.get(0..2), Some(&b"BM"[..]));
        assert_eq!(b.get(54..58), Some(&[0u8, 0, 255, 255][..]));
        let _ = std::fs::remove_file(&p);
    }
}
