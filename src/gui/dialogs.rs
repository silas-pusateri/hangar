//! Settings-style windows: instances, settings, compatible versions, labels, install filters,
//! play warning, about, shortcuts, errors. File-based dialogs are in `files.rs`.

use std::path::PathBuf;

use eframe::egui::{self, RichText};

use super::app::{App, RED};
use super::files::{CkanDlg, ExportDlg, HistoryDlg, ImportDlg, ImportResultDlg, UnmanagedDlg};
use crate::config::{Config, InstanceEntry, Label, LabelList};
use crate::meta::{GameVersion, ReleaseStatus};
use crate::ops::Session;
use crate::repo::Repository;

pub use super::files::{imported, pick_ckan_files};

#[derive(Default)]
pub struct Dialogs {
    pub error: Option<(String, String)>,
    /// Name of the instance being opened (for messages).
    pub opening: Option<String>,
    pub instances: Option<InstancesDlg>,
    pub settings: Option<SettingsDlg>,
    pub compat: Option<CompatDlg>,
    pub labels: Option<LabelsDlg>,
    pub filters: Option<FiltersDlg>,
    pub export: Option<ExportDlg>,
    pub ckan: Option<CkanDlg>,
    pub import: Option<ImportDlg>,
    pub import_result: Option<ImportResultDlg>,
    pub history: Option<HistoryDlg>,
    pub unmanaged: Option<UnmanagedDlg>,
    pub play_warning: Option<Vec<String>>,
    pub about: bool,
    pub shortcuts: bool,
}

impl Dialogs {
    pub fn any_open(&self) -> bool {
        self.error.is_some()
            || self.instances.is_some()
            || self.settings.is_some()
            || self.compat.is_some()
            || self.labels.is_some()
            || self.filters.is_some()
            || self.export.is_some()
            || self.ckan.is_some()
            || self.import.is_some()
            || self.import_result.is_some()
            || self.history.is_some()
            || self.unmanaged.is_some()
            || self.play_warning.is_some()
            || self.about
            || self.shortcuts
    }

    pub fn open_instances(&mut self) {
        if self.instances.is_none() {
            self.instances = Some(InstancesDlg { stale: true, ..Default::default() });
        }
    }

    pub fn open_settings(&mut self, cfg: &Config, s: Option<&Session>) {
        self.settings = Some(SettingsDlg::new(cfg, s));
    }

    pub fn open_compat(&mut self, s: Option<&Session>) {
        if let Some(s) = s {
            self.compat = Some(CompatDlg::new(s));
        }
    }

    pub fn open_labels(&mut self, s: Option<&Session>) {
        if let Some(s) = s {
            self.labels = Some(LabelsDlg { list: s.labels.clone(), selected: None, new_name: String::new(), error: None });
        }
    }

    pub fn open_filters(&mut self, cfg: &Config, s: Option<&Session>) {
        if let Some(s) = s {
            self.filters = Some(FiltersDlg {
                global: cfg.global_install_filters().join("\n"),
                instance: s.instance.install_filters().join("\n"),
            });
        }
    }

    pub fn open_export(&mut self, s: Option<&Session>, modpack: bool) {
        if let Some(s) = s {
            self.export = Some(ExportDlg::new(s, modpack));
        }
    }

    pub fn open_history(&mut self, s: Option<&Session>) {
        if let Some(s) = s {
            self.history = Some(HistoryDlg::new(s));
        }
    }

    pub fn open_unmanaged(&mut self, s: Option<&Session>) {
        if let Some(s) = s {
            self.unmanaged = Some(UnmanagedDlg::new(s));
        }
    }

    pub fn repo_list_arrived(&mut self, list: Vec<Repository>) {
        if let Some(d) = self.settings.as_mut() {
            d.repo_list = Some(list);
        }
    }
}

/// Draw every open dialog. Each one is taken out of `app.dlg` while it draws.
pub fn show_all(app: &mut App, ctx: &egui::Context) {
    macro_rules! run {
        ($field:ident) => {
            if let Some(mut d) = app.dlg.$field.take() {
                if d.show(app, ctx) && app.dlg.$field.is_none() {
                    app.dlg.$field = Some(d);
                }
            }
        };
    }
    run!(instances);
    run!(settings);
    run!(compat);
    run!(labels);
    run!(filters);
    run!(export);
    run!(ckan);
    run!(import);
    run!(import_result);
    run!(history);
    run!(unmanaged);
    play_warning(app, ctx);
    about(app, ctx);
    shortcuts(app, ctx);
    error(app, ctx);
}

/// A scroll height for a dialog: `h` when the window is tall, less when it is short (small
/// window or large text size), so that the buttons below the scroll area stay on screen.
pub fn fit_h(ctx: &egui::Context, h: f32) -> f32 {
    let ratio = ((ctx.content_rect().height() - 200.0) / 600.0).clamp(0.1, 1.0);
    (h * ratio).max(48.0)
}

/// A dialog width that is not wider than the window.
pub fn fit_w(ctx: &egui::Context, w: f32) -> f32 {
    w.min(ctx.content_rect().width() - 48.0).max(200.0)
}

/// A standard dialog window. Returns false when the user closed it.
/// The window is never larger than the main window; wide content scrolls sideways.
pub fn window(ctx: &egui::Context, title: &str, size: [f32; 2], add: impl FnOnce(&mut egui::Ui)) -> bool {
    let mut open = true;
    let screen = ctx.content_rect().size();
    let w = fit_w(ctx, size[0]);
    egui::Window::new(title)
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .hscroll(true)
        .default_size([w, size[1].min(screen.y - 48.0)])
        .max_width(fit_w(ctx, size[0] * 1.5))
        .max_height(screen.y - 24.0)
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(ctx.content_rect().center())
        .show(ctx, add);
    open
}

/// Open a folder or file in Finder.
pub fn reveal(path: &std::path::Path) -> String {
    match std::process::Command::new("/usr/bin/open").arg(path).spawn() {
        Ok(_) => format!("Opened {}", path.display()),
        Err(e) => format!("Cannot open {}: {e}", path.display()),
    }
}

// ---------------------------------------------------------------- instances

struct InstRow {
    name: String,
    path: String,
    version: String,
    playtime: String,
    valid: bool,
}

#[derive(Default)]
pub struct InstancesDlg {
    stale: bool,
    rows: Vec<InstRow>,
    selected: Option<usize>,
    renaming: Option<(usize, String)>,
    found: Option<Vec<PathBuf>>,
    msg: Option<String>,
}

fn unique_name(cfg: &Config, base: &str) -> String {
    let base = if base.trim().is_empty() { "KSP" } else { base.trim() };
    if cfg.instance(base).is_none() {
        return base.to_string();
    }
    (2..1000).map(|n| format!("{base} ({n})")).find(|n| cfg.instance(n).is_none()).unwrap_or_else(|| format!("{base} (new)"))
}

/// Register a game folder. Error text if it is not valid or already registered.
pub fn add_instance(cfg: &mut Config, dir: &std::path::Path) -> Result<String, String> {
    if !crate::instance::is_valid_game_dir(dir) {
        return Err(format!("{} is not a KSP game folder (needs buildID64.txt or buildID.txt and GameData).", dir.display()));
    }
    let path = dir.to_string_lossy().into_owned();
    if let Some(e) = cfg.game_instances.iter().find(|e| std::path::Path::new(&e.path) == dir) {
        return Err(format!("This folder is already registered as {}.", e.name));
    }
    let base = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let name = unique_name(cfg, &base);
    cfg.game_instances.push(InstanceEntry { name: name.clone(), path, game: "KSP".into() });
    Ok(name)
}

impl InstancesDlg {
    fn refresh(&mut self, cfg: &Config) {
        let builds = crate::instance::embedded_builds();
        self.rows = cfg
            .game_instances
            .iter()
            .map(|e| {
                let dir = std::path::Path::new(&e.path);
                match crate::instance::Instance::open(&e.name, dir, builds) {
                    Ok(i) => InstRow {
                        name: e.name.clone(),
                        path: e.path.clone(),
                        version: i.version.map(|v| v.to_string()).unwrap_or_else(|| "unknown".into()),
                        playtime: super::model::hours_text(i.playtime()),
                        valid: true,
                    },
                    Err(_) => InstRow {
                        name: e.name.clone(),
                        path: e.path.clone(),
                        version: "missing".into(),
                        playtime: String::new(),
                        valid: false,
                    },
                }
            })
            .collect();
        self.stale = false;
    }

    fn show(&mut self, app: &mut App, ctx: &egui::Context) -> bool {
        if self.stale {
            self.refresh(&app.config);
        }
        let idle = !app.busy();
        let current = app.session.as_ref().map(|s| s.instance.name.clone());
        let mut changed = false;
        let mut open_name: Option<String> = None;
        let keep = window(ctx, "Game instances", [820.0, 420.0], |ui| {
            if app.config.game_instances.is_empty() {
                ui.label(RichText::new("No game instance yet. Add your KSP folder to start.").strong());
                ui.add_space(4.0);
            }
            egui::Grid::new("inst_grid").num_columns(5).striped(true).spacing([14.0, 6.0]).show(ui, |ui| {
                for h in ["Name", "Version", "Play time", "Path", ""] {
                    ui.label(RichText::new(h).strong());
                }
                ui.end_row();
                for (i, r) in self.rows.iter().enumerate() {
                    let mut name = RichText::new(&r.name);
                    if current.as_deref() == Some(r.name.as_str()) {
                        name = name.strong();
                    }
                    if ui.selectable_label(self.selected == Some(i), name).clicked() {
                        self.selected = Some(i);
                        self.renaming = None;
                    }
                    let v = RichText::new(&r.version).monospace();
                    ui.label(if r.valid { v } else { v.color(RED) });
                    ui.label(&r.playtime);
                    ui.label(RichText::new(&r.path).small());
                    let mut tags = Vec::new();
                    if app.config.auto_start_instance.as_deref() == Some(r.name.as_str()) {
                        tags.push("default");
                    }
                    if current.as_deref() == Some(r.name.as_str()) {
                        tags.push("open");
                    }
                    ui.weak(tags.join(", "));
                    ui.end_row();
                }
            });
            ui.separator();
            let sel = self.selected.and_then(|i| self.rows.get(i)).map(|r| (r.name.clone(), r.path.clone(), r.valid));
            ui.horizontal_wrapped(|ui| {
                if let Some((name, path, valid)) = sel.clone() {
                    if ui.add_enabled(idle && valid, egui::Button::new(RichText::new("Open").strong())).clicked() {
                        open_name = Some(name.clone());
                    }
                    if ui.add_enabled(idle, egui::Button::new("Rename")).clicked() {
                        self.renaming = self.selected.map(|i| (i, name.clone()));
                    }
                    if ui.add_enabled(idle, egui::Button::new("Set as default")).clicked() {
                        app.config.auto_start_instance = Some(name.clone());
                        changed = true;
                    }
                    if ui.add_enabled(idle, egui::Button::new("Forget")).on_hover_text("Remove from the list; files stay").clicked() {
                        app.config.game_instances.retain(|e| e.name != name);
                        if app.config.auto_start_instance.as_deref() == Some(name.as_str()) {
                            app.config.auto_start_instance = None;
                        }
                        if current.as_deref() == Some(name.as_str()) {
                            app.session = None;
                            app.model = None;
                            app.index = None;
                            app.view.clear();
                            app.selected = None;
                            app.pending.clear();
                        }
                        self.selected = None;
                        changed = true;
                    }
                    if ui.button("Open folder").clicked() {
                        self.msg = Some(reveal(std::path::Path::new(&path)));
                    }
                    ui.separator();
                }
                if ui.add_enabled(idle, egui::Button::new("Add folder...")).clicked()
                    && let Some(dir) = rfd::FileDialog::new().set_title("Choose your KSP folder").pick_folder()
                {
                    match add_instance(&mut app.config, &dir) {
                        Ok(n) => {
                            self.msg = Some(format!("Added {n}"));
                            changed = true;
                        }
                        Err(e) => self.msg = Some(e),
                    }
                }
                if ui.add_enabled(idle, egui::Button::new("Find Steam installs")).clicked() {
                    if std::env::var_os("HANGAR_NO_AUTODETECT").is_some() {
                        self.msg = Some("Detection is off (HANGAR_NO_AUTODETECT is set).".into());
                        self.found = Some(Vec::new());
                    } else {
                        let found: Vec<PathBuf> = crate::instance::find_installs()
                            .into_iter()
                            .filter(|p| !app.config.game_instances.iter().any(|e| std::path::Path::new(&e.path) == p))
                            .collect();
                        self.msg = Some(format!("Found {} new install(s)", found.len()));
                        self.found = Some(found);
                    }
                }
            });
            if let Some((i, mut text)) = self.renaming.take() {
                let mut done = false;
                let mut cancel = false;
                ui.horizontal(|ui| {
                    ui.label("New name:");
                    let r = ui.text_edit_singleline(&mut text);
                    done = ui.button("Save").clicked() || (r.lost_focus() && ui.input(|x| x.key_pressed(egui::Key::Enter)));
                    cancel = ui.button("Cancel").clicked();
                });
                if done {
                    let new = text.trim().to_string();
                    let old = self.rows.get(i).map(|r| r.name.clone()).unwrap_or_default();
                    if new.is_empty() || (new != old && app.config.instance(&new).is_some()) {
                        self.msg = Some("The name must be new and not empty.".into());
                        self.renaming = Some((i, text));
                    } else {
                        for e in app.config.game_instances.iter_mut().filter(|e| e.name == old) {
                            e.name = new.clone();
                        }
                        if app.config.auto_start_instance.as_deref() == Some(old.as_str()) {
                            app.config.auto_start_instance = Some(new.clone());
                        }
                        if let Some(s) = app.session.as_mut().filter(|s| s.instance.name == old) {
                            s.instance.name = new.clone();
                            app.facts.instance = Some(new.clone());
                        }
                        changed = true;
                    }
                } else if !cancel {
                    self.renaming = Some((i, text));
                }
            }
            if let Some(found) = self.found.as_ref() {
                let mut add: Option<PathBuf> = None;
                for p in found {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(p.display().to_string()).small());
                        if ui.add_enabled(idle, egui::Button::new("Add").small()).clicked() {
                            add = Some(p.clone());
                        }
                    });
                }
                if let Some(p) = add {
                    match add_instance(&mut app.config, &p) {
                        Ok(n) => {
                            self.msg = Some(format!("Added {n}"));
                            changed = true;
                        }
                        Err(e) => self.msg = Some(e),
                    }
                    if let Some(f) = self.found.as_mut() {
                        f.retain(|x| *x != p);
                    }
                }
            }
            if let Some(m) = &self.msg {
                ui.add_space(4.0);
                ui.label(RichText::new(m).italics());
            }
        });
        if changed {
            app.commit_config();
            self.refresh(&app.config);
        }
        if let Some(n) = open_name {
            app.open_instance(Some(n));
            return false;
        }
        keep
    }
}

// ---------------------------------------------------------------- settings

pub struct SettingsDlg {
    repos: Vec<Repository>,
    repos_dirty: bool,
    has_session: bool,
    new_name: String,
    new_uri: String,
    repo_list: Option<Vec<Repository>>,
    cache_dir: String,
    cache_size: Option<u64>,
    limit_mib: String,
    stability: ReleaseStatus,
    hide_epochs: bool,
    hide_v: bool,
    update_on_launch: bool,
    with_recommends: bool,
    with_suggests: bool,
    theme: Option<bool>,
    purge_confirm: bool,
    msg: Option<String>,
}

impl SettingsDlg {
    fn new(cfg: &Config, s: Option<&Session>) -> SettingsDlg {
        let h = &cfg.hangar;
        SettingsDlg {
            repos: s.map(|s| s.registry.repositories()).unwrap_or_default(),
            repos_dirty: false,
            has_session: s.is_some(),
            new_name: String::new(),
            new_uri: String::new(),
            repo_list: None,
            cache_dir: cfg.cache_dir().display().to_string(),
            cache_size: s.map(|s| s.cache.total_size()),
            limit_mib: cfg.cache_size_limit.map(|b| (b / (1024 * 1024)).to_string()).unwrap_or_default(),
            stability: h.stability,
            hide_epochs: h.hide_epochs,
            hide_v: h.hide_v,
            update_on_launch: h.update_repos_on_launch,
            with_recommends: h.with_recommends,
            with_suggests: h.with_suggests,
            theme: h.dark_mode,
            purge_confirm: false,
            msg: None,
        }
    }

    fn add_repo(&mut self, r: Repository) {
        if self.repos.iter().any(|x| x.uri == r.uri || x.name == r.name) {
            self.msg = Some(format!("{} is already in the list", r.name));
            return;
        }
        self.repos.push(r);
        self.repos_dirty = true;
    }

    fn repos_ui(&mut self, app: &mut App, ui: &mut egui::Ui) {
        ui.label(RichText::new("Repositories").strong());
        if !self.has_session {
            ui.weak("Open an instance to edit its repositories.");
            return;
        }
        let mut action: Option<(usize, i32)> = None;
        egui::Grid::new("repo_grid").num_columns(3).striped(true).spacing([10.0, 4.0]).show(ui, |ui| {
            for (i, r) in self.repos.iter().enumerate() {
                ui.label(&r.name);
                ui.label(RichText::new(&r.uri).small().monospace());
                ui.horizontal(|ui| {
                    if ui.small_button("Up").clicked() {
                        action = Some((i, -1));
                    }
                    if ui.small_button("Down").clicked() {
                        action = Some((i, 1));
                    }
                    if ui.small_button("Remove").clicked() {
                        action = Some((i, 0));
                    }
                });
                ui.end_row();
            }
        });
        if let Some((i, d)) = action {
            match d {
                0 => {
                    if i < self.repos.len() {
                        self.repos.remove(i);
                    }
                }
                -1 if i > 0 => self.repos.swap(i, i - 1),
                1 if i + 1 < self.repos.len() => self.repos.swap(i, i + 1),
                _ => {}
            }
            self.repos_dirty = true;
        }
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.new_name).hint_text("Name").desired_width(120.0));
            ui.add(egui::TextEdit::singleline(&mut self.new_uri).hint_text("URL of a .tar.gz or .zip").desired_width(300.0));
            let ok = !self.new_name.trim().is_empty() && !self.new_uri.trim().is_empty();
            if ui.add_enabled(ok, egui::Button::new("Add")).clicked() {
                let r = Repository {
                    name: self.new_name.trim().to_string(),
                    uri: self.new_uri.trim().to_string(),
                    priority: 0,
                    x_mirror: false,
                    x_comment: None,
                };
                self.add_repo(r);
                self.new_name.clear();
                self.new_uri.clear();
            }
        });
        ui.horizontal(|ui| {
            if ui.add_enabled(!app.busy(), egui::Button::new("Add from CKAN list...")).clicked() {
                app.start_free_job("Fetching repository list", |rep| {
                    rep.progress("Downloading repositories.json", None);
                    super::worker::Done {
                        session: None,
                        model: None,
                        facts: None,
                        outcome: super::worker::Outcome::RepoList(crate::repo::fetch_repository_list()),
                    }
                });
            }
            if ui.button("Reset to default").clicked() {
                self.repos = vec![Repository::default_repo()];
                self.repos_dirty = true;
            }
        });
        if let Some(list) = self.repo_list.clone() {
            ui.add_space(4.0);
            ui.label(RichText::new("Known repositories").weak());
            egui::ScrollArea::vertical().id_salt("known_repos").max_height(fit_h(ui.ctx(), 120.0)).show(ui, |ui| {
                for r in list {
                    ui.horizontal(|ui| {
                        let have = self.repos.iter().any(|x| x.uri == r.uri);
                        if ui.add_enabled(!have, egui::Button::new("Add").small()).clicked() {
                            self.add_repo(r.clone());
                        }
                        ui.label(&r.name);
                        ui.label(RichText::new(&r.uri).small().weak());
                    });
                }
            });
        }
    }

    fn show(&mut self, app: &mut App, ctx: &egui::Context) -> bool {
        let mut save = false;
        let mut cancel = false;
        let keep = window(ctx, "Settings", [720.0, 560.0], |ui| {
            egui::ScrollArea::vertical().max_height(fit_h(ctx, 560.0)).show(ui, |ui| {
                self.repos_ui(app, ui);
                ui.separator();
                ui.label(RichText::new("Download cache").strong());
                ui.horizontal(|ui| {
                    ui.label("Folder:");
                    ui.add(egui::TextEdit::singleline(&mut self.cache_dir).desired_width(360.0));
                    if ui.button("Choose...").clicked()
                        && let Some(d) = rfd::FileDialog::new().set_title("Download cache folder").pick_folder()
                    {
                        self.cache_dir = d.display().to_string();
                    }
                    if ui.button("Show").clicked() {
                        self.msg = Some(reveal(std::path::Path::new(&self.cache_dir)));
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Size limit (MiB, empty = none):");
                    ui.add(egui::TextEdit::singleline(&mut self.limit_mib).desired_width(80.0));
                    if let Some(n) = self.cache_size {
                        ui.weak(format!("In use: {}", crate::util::human_size(n)));
                    }
                });
                ui.horizontal(|ui| {
                    let can = self.has_session && !app.busy();
                    if !self.purge_confirm {
                        if ui.add_enabled(can, egui::Button::new("Purge all...")).clicked() {
                            self.purge_confirm = true;
                        }
                    } else {
                        ui.colored_label(RED, "Delete every cached download?");
                        if ui.button("Yes, purge").clicked() {
                            self.purge_confirm = false;
                            if let Some(s) = app.idle_session() {
                                self.msg = Some(match s.cache.trim(0) {
                                    Ok(()) => "Cache purged".into(),
                                    Err(e) => format!("Purge failed: {e}"),
                                });
                                self.cache_size = Some(s.cache.total_size());
                                app.rebuild();
                            }
                        }
                        if ui.button("No").clicked() {
                            self.purge_confirm = false;
                        }
                    }
                });
                ui.separator();
                ui.label(RichText::new("Behavior").strong());
                egui::ComboBox::from_label("Stability tolerance")
                    .selected_text(self.stability.as_str())
                    .show_ui(ui, |ui| {
                        for s in [ReleaseStatus::Stable, ReleaseStatus::Testing, ReleaseStatus::Development] {
                            ui.selectable_value(&mut self.stability, s, s.as_str());
                        }
                    });
                ui.checkbox(&mut self.update_on_launch, "Update repositories on launch (when older than 1 hour)");
                ui.checkbox(&mut self.with_recommends, "Pre-check recommended mods");
                ui.checkbox(&mut self.with_suggests, "Pre-check suggested mods");
                ui.checkbox(&mut self.hide_epochs, "Hide epochs in versions (2:1.0 shows as 1.0)");
                ui.checkbox(&mut self.hide_v, "Hide a leading v in versions");
                ui.horizontal(|ui| {
                    ui.label("Theme:");
                    ui.selectable_value(&mut self.theme, None, "System");
                    ui.selectable_value(&mut self.theme, Some(false), "Light");
                    ui.selectable_value(&mut self.theme, Some(true), "Dark");
                });
                if let Some(m) = &self.msg {
                    ui.add_space(4.0);
                    ui.label(RichText::new(m).italics());
                }
            });
            ui.separator();
            ui.horizontal(|ui| {
                save = ui.add_enabled(!app.busy(), egui::Button::new(RichText::new("Save").strong())).clicked();
                cancel = ui.button("Cancel").clicked();
            });
        });
        if save {
            return !self.save(app);
        }
        keep && !cancel
    }

    /// Apply the settings. True when done.
    fn save(&mut self, app: &mut App) -> bool {
        let limit = match self.limit_mib.trim() {
            "" => None,
            t => match t.parse::<u64>() {
                Ok(n) => Some(n.saturating_mul(1024 * 1024)),
                Err(_) => {
                    self.msg = Some("The size limit must be a whole number of MiB.".into());
                    return false;
                }
            },
        };
        let h = &app.config.hangar;
        let needs_rebuild = h.stability != self.stability || h.hide_epochs != self.hide_epochs || h.hide_v != self.hide_v;
        let old_dir = app.config.cache_dir();
        let c = &mut app.config;
        c.cache_size_limit = limit;
        let dir = self.cache_dir.trim();
        c.download_cache_dir = if dir.is_empty() || std::path::Path::new(dir) == crate::util::hangar_dir().join("downloads") {
            None
        } else {
            Some(dir.to_string())
        };
        c.hangar.stability = self.stability;
        c.hangar.hide_epochs = self.hide_epochs;
        c.hangar.hide_v = self.hide_v;
        c.hangar.update_repos_on_launch = self.update_on_launch;
        c.hangar.with_recommends = self.with_recommends;
        c.hangar.with_suggests = self.with_suggests;
        c.hangar.dark_mode = self.theme;
        match self.theme {
            Some(true) => app.ctx.set_theme(egui::Theme::Dark),
            Some(false) => app.ctx.set_theme(egui::Theme::Light),
            None => app.ctx.set_theme(egui::ThemePreference::System),
        }
        app.commit_config();
        let new_dir = app.config.cache_dir();
        let mut repo_added = false;
        if let Some(s) = app.session.as_mut() {
            if new_dir != old_dir {
                s.cache = crate::install::DownloadCache::new(new_dir.clone());
            }
            if self.repos_dirty {
                let old = s.registry.repositories();
                repo_added = has_new_repo(&old, &self.repos);
                let mut repos = self.repos.clone();
                for (i, r) in repos.iter_mut().enumerate() {
                    r.priority = i32::try_from(i).unwrap_or(i32::MAX);
                }
                if let Err(e) = s.set_repositories(repos) {
                    self.msg = Some(format!("Cannot save repositories: {e}"));
                    return false;
                }
            }
        }
        // As CKAN: a new repo is downloaded at once; a removed or moved one changes the list.
        // Both jobs rebuild the rows (the session index was reloaded with the new list).
        if repo_added {
            app.update_repos(false);
        } else if needs_rebuild || new_dir != old_dir || self.repos_dirty {
            app.rebuild();
        }
        true
    }
}

/// True if `new` has a repository URL that `old` does not have.
pub fn has_new_repo(old: &[Repository], new: &[Repository]) -> bool {
    new.iter().any(|r| !old.iter().any(|o| o.uri == r.uri))
}

// ---------------------------------------------------------------- compatible versions

pub struct CompatDlg {
    /// Why the dialog opened by itself (first start, or the game version changed).
    reason: Option<String>,
    game: String,
    items: Vec<(GameVersion, bool)>,
    extra: String,
    error: Option<String>,
}

impl CompatDlg {
    fn new(s: &Session) -> CompatDlg {
        let current = s.instance.compatible_versions();
        let mut known: Vec<GameVersion> = s.index.known_game_versions.iter().map(GameVersion::major_minor).collect();
        known.extend(current.iter().copied());
        known.sort_by(|a, b| b.cmp(a));
        known.dedup();
        let game_mm = s.instance.version.map(|v| v.major_minor());
        let items = known
            .into_iter()
            .filter(|v| Some(*v) != game_mm || current.contains(v))
            .map(|v| (v, current.contains(&v)))
            .collect();
        let reason = s.instance.compat_from_other_game_version().then(|| {
            "The compatible versions were never saved for this game version. Check them once and press Save.".to_string()
        });
        CompatDlg {
            reason,
            game: s.instance.version.map(|v| v.to_string()).unwrap_or_else(|| "unknown".into()),
            items,
            extra: String::new(),
            error: None,
        }
    }

    fn show(&mut self, app: &mut App, ctx: &egui::Context) -> bool {
        let mut save = false;
        let mut cancel = false;
        let keep = window(ctx, "Compatible game versions", [420.0, 480.0], |ui| {
            if let Some(r) = &self.reason {
                ui.label(RichText::new(r).strong());
            }
            ui.label(format!("Installed game version: {}", self.game));
            ui.label("Mods for these game versions also count as compatible. 1.12 covers every 1.12.x.");
            ui.horizontal(|ui| {
                if ui.small_button("Check all").clicked() {
                    self.items.iter_mut().for_each(|(_, c)| *c = true);
                }
                if ui.small_button("Uncheck all").clicked() {
                    self.items.iter_mut().for_each(|(_, c)| *c = false);
                }
            });
            egui::ScrollArea::vertical().max_height(fit_h(ui.ctx(), 300.0)).show(ui, |ui| {
                for (v, c) in self.items.iter_mut() {
                    ui.checkbox(c, RichText::new(v.to_string()).monospace());
                }
            });
            ui.horizontal(|ui| {
                ui.label("Other versions:");
                ui.add(egui::TextEdit::singleline(&mut self.extra).hint_text("1.11, 1.10.1").desired_width(180.0));
            });
            if let Some(e) = &self.error {
                ui.colored_label(RED, e);
            }
            ui.separator();
            ui.horizontal(|ui| {
                save = ui.add_enabled(app.idle_session().is_some(), egui::Button::new(RichText::new("Save").strong())).clicked();
                cancel = ui.button("Cancel").clicked();
            });
        });
        if save {
            let mut list: Vec<GameVersion> = self.items.iter().filter(|(_, c)| *c).map(|(v, _)| *v).collect();
            for t in self.extra.split([',', ' ']).map(str::trim).filter(|t| !t.is_empty()) {
                match GameVersion::parse(t) {
                    Some(v) if !v.is_any() => list.push(v),
                    _ => {
                        self.error = Some(format!("{t} is not a game version"));
                        return true;
                    }
                }
            }
            if app.busy() {
                return true;
            }
            let Some(s) = app.session.as_mut() else { return false };
            match s.set_compatible_versions(&list) {
                Ok(()) => {
                    app.status = "Compatible game versions saved".into();
                    app.rebuild();
                    return false;
                }
                Err(e) => {
                    self.error = Some(e.to_string());
                    return true;
                }
            }
        }
        keep && !cancel
    }
}

// ---------------------------------------------------------------- labels

pub struct LabelsDlg {
    list: LabelList,
    selected: Option<usize>,
    new_name: String,
    error: Option<String>,
}

impl LabelsDlg {
    fn show(&mut self, app: &mut App, ctx: &egui::Context) -> bool {
        let mut save = false;
        let mut cancel = false;
        let inst = app.session.as_ref().map(|s| s.instance.name.clone()).unwrap_or_default();
        let keep = window(ctx, "Labels", [640.0, 420.0], |ui| {
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(200.0);
                    egui::ScrollArea::vertical().id_salt("label_list").max_height(fit_h(ui.ctx(), 300.0)).show(ui, |ui| {
                        for (i, l) in self.list.labels.iter().enumerate() {
                            let mut t = RichText::new(&l.name);
                            if let Some([r, g, b]) = l.rgb() {
                                t = t.background_color(egui::Color32::from_rgb(r, g, b).gamma_multiply(0.5));
                            }
                            if ui.selectable_label(self.selected == Some(i), t).clicked() {
                                self.selected = Some(i);
                            }
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut self.new_name).hint_text("New label").desired_width(120.0));
                        if ui.add_enabled(!self.new_name.trim().is_empty(), egui::Button::new("Add")).clicked() {
                            let name = self.new_name.trim().to_string();
                            if self.list.labels.iter().any(|l| l.name == name) {
                                self.error = Some("A label with this name exists.".into());
                            } else {
                                self.list.labels.push(Label { name, color: Some("PaleGreen".into()), ..Label::default() });
                                self.selected = Some(self.list.labels.len() - 1);
                                self.new_name.clear();
                            }
                        }
                    });
                });
                // No vertical separator: it would stretch the window to the screen height.
                ui.add_space(16.0);
                ui.vertical(|ui| {
                    let mut delete = false;
                    if let Some(l) = self.selected.and_then(|i| self.list.labels.get_mut(i)) {
                        ui.horizontal(|ui| {
                            ui.label("Name:");
                            ui.text_edit_singleline(&mut l.name);
                        });
                        ui.horizontal(|ui| {
                            ui.label("Color:");
                            let mut rgb = l.rgb().unwrap_or([200, 200, 200]);
                            if ui.color_edit_button_srgb(&mut rgb).changed() {
                                l.color = Some(format!("{}, {}, {}", rgb[0], rgb[1], rgb[2]));
                            }
                            if ui.small_button("None").clicked() {
                                l.color = None;
                            }
                        });
                        let mut this_only = l.instance_name.is_some();
                        if ui.checkbox(&mut this_only, format!("Only for instance {inst}")).changed() {
                            l.instance_name = this_only.then(|| inst.clone());
                        }
                        ui.checkbox(&mut l.hide, "Hide these mods from the list");
                        ui.checkbox(&mut l.hold_version, "Hold version (no updates)");
                        ui.checkbox(&mut l.notify_on_change, "Notify when they change");
                        ui.checkbox(&mut l.remove_on_change, "Remove from label when they change");
                        ui.checkbox(&mut l.alert_on_install, "Alert when one is installed");
                        ui.checkbox(&mut l.remove_on_install, "Remove from label when installed");
                        ui.checkbox(&mut l.ignore_missing_files, "Ignore missing files");
                        let n = l.module_identifiers_by_game.get("KSP").map_or(0, Vec::len);
                        ui.weak(format!("{n} mod(s) in this label"));
                        delete = ui.button("Delete label").clicked();
                    } else {
                        ui.weak("Select a label to edit it.");
                    }
                    if delete
                        && let Some(i) = self.selected.take()
                        && i < self.list.labels.len()
                    {
                        self.list.labels.remove(i);
                    }
                });
            });
            if let Some(e) = &self.error {
                ui.colored_label(RED, e);
            }
            ui.separator();
            ui.horizontal(|ui| {
                save = ui.add_enabled(app.idle_session().is_some(), egui::Button::new(RichText::new("Save").strong())).clicked();
                cancel = ui.button("Cancel").clicked();
            });
        });
        if save {
            if self.list.labels.iter().any(|l| l.name.trim().is_empty()) {
                self.error = Some("Every label needs a name.".into());
                return true;
            }
            if let Err(e) = self.list.save() {
                self.error = Some(format!("Cannot save: {e}"));
                return true;
            }
            if let Some(s) = app.session.as_mut() {
                s.reload_labels();
                app.label_info = super::model::LabelInfo::of(&s.labels);
            }
            app.filter = super::model::ListFilter::default();
            app.status = "Labels saved".into();
            app.rebuild();
            return false;
        }
        keep && !cancel
    }
}

// ---------------------------------------------------------------- install filters

pub struct FiltersDlg {
    global: String,
    instance: String,
}

const PRESETS: [(&str, &[&str]); 2] = [
    ("MiniAVC", &["/MiniAVC.dll", "/MiniAVC.xml", "/LICENSE-MiniAVC.txt"]),
    ("MiniAVC2-V2", &["/MiniAVC-V2.dll", "/MiniAVC-V2.dll.mdb"]),
];

fn lines(s: &str) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for l in s.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if !v.iter().any(|x| x == l) {
            v.push(l.to_string());
        }
    }
    v
}

impl FiltersDlg {
    fn show(&mut self, app: &mut App, ctx: &egui::Context) -> bool {
        let mut save = false;
        let mut cancel = false;
        let keep = window(ctx, "Install filters", [560.0, 440.0], |ui| {
            ui.label("Files whose path contains one of these texts are not installed. One per line.");
            ui.columns(2, |cols| {
                if let Some(ui) = cols.get_mut(0) {
                    ui.label(RichText::new("All instances").strong());
                    ui.add(egui::TextEdit::multiline(&mut self.global).code_editor().desired_rows(12));
                    ui.horizontal_wrapped(|ui| {
                        for (name, items) in PRESETS {
                            if ui.button(format!("Add {name}")).clicked() {
                                let mut v = lines(&self.global);
                                for i in items {
                                    if !v.iter().any(|x| x == i) {
                                        v.push((*i).to_string());
                                    }
                                }
                                self.global = v.join("\n");
                            }
                        }
                    });
                }
                if let Some(ui) = cols.get_mut(1) {
                    ui.label(RichText::new("This instance").strong());
                    ui.add(egui::TextEdit::multiline(&mut self.instance).code_editor().desired_rows(12));
                }
            });
            ui.separator();
            ui.horizontal(|ui| {
                save = ui.add_enabled(app.idle_session().is_some(), egui::Button::new(RichText::new("Save").strong())).clicked();
                cancel = ui.button("Cancel").clicked();
            });
        });
        if save {
            app.config.global_install_filters_by_game.insert("KSP".into(), lines(&self.global));
            app.commit_config();
            if let Some(s) = app.idle_session()
                && let Err(e) = s.instance.set_install_filters(&lines(&self.instance))
            {
                app.show_error("Cannot save instance filters", &e.to_string());
                return true;
            }
            app.status = "Install filters saved".into();
            return false;
        }
        keep && !cancel
    }
}

// ---------------------------------------------------------------- small windows

fn play_warning(app: &mut App, ctx: &egui::Context) {
    let Some(list) = app.dlg.play_warning.take() else { return };
    let mut launch = false;
    let mut cancel = false;
    let resp = egui::Modal::new(egui::Id::unique("play_warn")).show(ctx, |ui| {
        ui.set_width(fit_w(ctx, 480.0));
        ui.heading("Incompatible mods installed");
        ui.label("These installed mods are not marked compatible with this game version:");
        egui::ScrollArea::vertical().max_height(fit_h(ui.ctx(), 200.0)).show(ui, |ui| {
            for l in &list {
                ui.label(RichText::new(l).monospace());
            }
        });
        ui.horizontal(|ui| {
            launch = ui.button("Launch anyway").clicked();
            cancel = ui.button("Cancel").clicked();
        });
    });
    if launch {
        app.launch_game(true);
    } else if !(cancel || resp.should_close()) {
        app.dlg.play_warning = Some(list);
    }
}

fn about(app: &mut App, ctx: &egui::Context) {
    if !app.dlg.about {
        return;
    }
    let mut open = true;
    egui::Window::new("About Hangar").open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
        ui.heading(format!("Hangar {}", env!("CARGO_PKG_VERSION")));
        ui.label("A fast native mod manager for Kerbal Space Program 1.");
        ui.add_space(6.0);
        ui.label("Hangar reads and writes CKAN's metadata, registry and configuration formats.");
        ui.label("Credits: the CKAN project and its metadata team, who maintain the .ckan format and the CKAN-meta repository that Hangar uses.");
        ui.add_space(6.0);
        if ui.link("https://github.com/KSP-CKAN/CKAN").clicked() {
            super::table::open_url(app, "https://github.com/KSP-CKAN/CKAN");
        }
    });
    app.dlg.about = open;
}

fn shortcuts(app: &mut App, ctx: &egui::Context) {
    if !app.dlg.shortcuts {
        return;
    }
    let mut open = true;
    egui::Window::new("Keyboard shortcuts").open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
        egui::Grid::new("keys").num_columns(2).spacing([20.0, 4.0]).show(ui, |ui| {
            for (k, v) in [
                ("Cmd+F", "Focus the search box"),
                ("Esc", "Clear the search / close a dialog"),
                ("Cmd+R", "Refresh repositories"),
                ("Cmd+= / Cmd+-", "Larger / smaller text"),
                ("Cmd+0", "Normal text size"),
                ("Cmd+Q", "Quit"),
                ("Right-click a row", "Labels, cache, reinstall, homepage"),
                ("Right-click a header", "Show or hide columns"),
            ] {
                ui.label(RichText::new(k).monospace().strong());
                ui.label(v);
                ui.end_row();
            }
        });
    });
    app.dlg.shortcuts = open;
}

fn error(app: &mut App, ctx: &egui::Context) {
    let Some((title, text)) = app.dlg.error.take() else { return };
    let mut ok = false;
    let resp = egui::Modal::new(egui::Id::unique("error_dlg")).show(ctx, |ui| {
        ui.set_width(fit_w(ctx, 520.0));
        ui.label(RichText::new(&title).heading().color(RED));
        ui.add_space(4.0);
        egui::ScrollArea::vertical().max_height(fit_h(ui.ctx(), 320.0)).show(ui, |ui| {
            ui.label(RichText::new(&text).monospace());
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ok = ui.button("OK").clicked();
            if ui.button("Copy").clicked() {
                ui.ctx().copy_text(format!("{title}\n{text}"));
            }
        });
    });
    if !(ok || resp.should_close()) && app.dlg.error.is_none() {
        app.dlg.error = Some((title, text));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_names_and_filters() {
        let mut cfg = Config::default();
        cfg.game_instances.push(InstanceEntry { name: "KSP".into(), path: "/x".into(), game: "KSP".into() });
        assert_eq!(unique_name(&cfg, "KSP"), "KSP (2)");
        assert_eq!(unique_name(&cfg, "Other"), "Other");
        assert_eq!(unique_name(&cfg, " "), "KSP (2)");
        assert_eq!(lines("a\n\n b \na\n"), vec!["a", "b"]);
        let dir = std::env::temp_dir().join(format!("hangar-gui-inst-{}", std::process::id()));
        assert!(add_instance(&mut cfg, &dir).is_err());
        let gv = GameVersion::parse("1.12.5.3190").unwrap_or(GameVersion::ANY);
        if crate::instance::create_fake(&dir, &gv, &[]).is_ok() {
            let name = add_instance(&mut cfg, &dir);
            assert!(name.is_ok());
            assert!(add_instance(&mut cfg, &dir).is_err());
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn new_repo_triggers_update() {
        let r = |n: &str, u: &str| Repository { name: n.into(), uri: u.into(), priority: 0, x_mirror: false, x_comment: None };
        let old = vec![r("a", "file:///a"), r("b", "file:///b")];
        // Moved or removed: no download needed.
        assert!(!has_new_repo(&old, &[r("b", "file:///b"), r("a", "file:///a")]));
        assert!(!has_new_repo(&old, &[r("a", "file:///a")]));
        // Added (also under an old name): download it.
        assert!(has_new_repo(&old, &[r("a", "file:///a"), r("c", "file:///c")]));
        assert!(has_new_repo(&old, &[r("a", "file:///a2")]));
    }
}
