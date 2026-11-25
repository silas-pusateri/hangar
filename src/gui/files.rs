//! File-based dialogs: export, install from .ckan, import zips, history, unmanaged files.

use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui::{self, RichText};

use super::app::{App, GREEN, RED, Tab};
use super::dialogs::{reveal, window};
use super::worker::{self, Outcome, PlanMode};
use crate::error::Result;
use crate::meta::{GameVersion, Module, ModuleVersion, Relationship};
use crate::modlist::{self, ExportFormat, ImportResult, ModpackOptions};
use crate::ops::Session;
use crate::resolve::InstallRequest;

/// CKAN modpack identifier rule.
pub fn valid_identifier(s: &str) -> bool {
    s.len() >= 2
        && s.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Plan the pending changes and show the Changes tab. Rebuilds rows too (compat may differ).
fn plan_into_changes(app: &mut App) {
    app.pending_changed();
    app.tab = Tab::Changes;
    let req = app.pending.to_request();
    let opts = app.resolver_options();
    let prev = app.prev_compat();
    app.start_job("Planning changes", move |s, rep| {
        rep.progress("Resolving dependencies", None);
        let result = s.plan(&req, &opts).map(|cs| (cs, Vec::new()));
        worker::done_with(s, prev.as_deref(), Outcome::Planned { mode: PlanMode::Preview, req, result })
    });
}

// ---------------------------------------------------------------- export

pub struct ExportDlg {
    modpack: bool,
    format: ExportFormat,
    identifier: String,
    name: String,
    abstract_: String,
    author: String,
    version: String,
    with_versions: bool,
    as_recommends: bool,
    include_optional: bool,
    msg: Option<String>,
}

impl ExportDlg {
    pub fn new(s: &Session, modpack: bool) -> ExportDlg {
        let d = ModpackOptions::defaults_for(&s.instance);
        ExportDlg {
            modpack,
            format: if modpack { ExportFormat::Ckan } else { ExportFormat::Text },
            identifier: d.identifier,
            name: d.name,
            abstract_: d.abstract_,
            author: d.author.join(", "),
            version: d.version,
            // CKAN GUI defaults: no versions, optional relationships suppressed.
            with_versions: false,
            as_recommends: false,
            include_optional: false,
            msg: None,
        }
    }

    fn options(&self) -> std::result::Result<ModpackOptions, String> {
        if !valid_identifier(&self.identifier) {
            return Err("The identifier may use letters, digits and '-' and must start with a letter or digit.".into());
        }
        if self.name.trim().is_empty() || self.version.trim().is_empty() {
            return Err("Name and version must not be empty.".into());
        }
        Ok(ModpackOptions {
            identifier: self.identifier.clone(),
            name: self.name.trim().to_string(),
            abstract_: self.abstract_.trim().to_string(),
            author: self.author.split(',').map(|a| a.trim().to_string()).filter(|a| !a.is_empty()).collect(),
            version: self.version.trim().to_string(),
            with_versions: self.with_versions,
            as_recommends: self.as_recommends,
            include_optional: self.include_optional,
        })
    }

    pub fn show(&mut self, app: &mut App, ctx: &egui::Context) -> bool {
        let mut export = false;
        let mut cancel = false;
        let title = if self.modpack { "Export modpack" } else { "Save installed mod list" };
        let keep = window(ctx, title, [520.0, 420.0], |ui| {
            egui::ComboBox::from_label("Format").selected_text(self.format.label()).show_ui(ui, |ui| {
                for f in ExportFormat::ALL {
                    ui.selectable_value(&mut self.format, f, f.label());
                }
            });
            if self.format == ExportFormat::Ckan {
                ui.add_space(4.0);
                egui::Grid::new("modpack").num_columns(2).spacing([10.0, 4.0]).show(ui, |ui| {
                    for (k, v) in [
                        ("Identifier", &mut self.identifier),
                        ("Name", &mut self.name),
                        ("Abstract", &mut self.abstract_),
                        ("Author(s)", &mut self.author),
                        ("Version", &mut self.version),
                    ] {
                        ui.label(k);
                        ui.add(egui::TextEdit::singleline(v).desired_width(320.0));
                        ui.end_row();
                    }
                });
                ui.checkbox(&mut self.with_versions, "Save mod versions");
                ui.checkbox(&mut self.include_optional, "Include optional relationships (recommendations)");
                ui.checkbox(&mut self.as_recommends, "List mods as recommendations instead of dependencies");
            } else {
                ui.weak("Lists every installed mod, including dependencies and DLC.");
            }
            if let Some(m) = &self.msg {
                ui.colored_label(RED, m);
            }
            ui.separator();
            ui.horizontal(|ui| {
                export = ui.add_enabled(app.idle_session().is_some(), egui::Button::new(RichText::new("Export...").strong())).clicked();
                cancel = ui.button("Cancel").clicked();
            });
        });
        if export {
            return !self.export(app);
        }
        keep && !cancel
    }

    /// True when written (or the user cancelled the save dialog).
    fn export(&mut self, app: &mut App) -> bool {
        let Some(s) = app.idle_session() else { return false };
        let text = if self.format == ExportFormat::Ckan {
            match self.options() {
                Ok(o) => modlist::generate_modpack(&s.instance, &s.registry, &s.index, &o).to_json_pretty(),
                Err(e) => {
                    self.msg = Some(e);
                    return false;
                }
            }
        } else {
            modlist::export_text(self.format, &s.registry)
        };
        let base = if self.format == ExportFormat::Ckan { self.identifier.clone() } else { format!("installed-{}", s.instance.sanitized_name()) };
        let ext = self.format.extension();
        let Some(path) = rfd::FileDialog::new()
            .set_title("Export")
            .set_file_name(format!("{base}.{ext}"))
            .add_filter(self.format.label(), &[ext])
            .save_file()
        else {
            return false;
        };
        match modlist::write_export(&path, &text) {
            Ok(()) => {
                app.status = format!("Exported to {}", path.display());
                true
            }
            Err(e) => {
                self.msg = Some(e.to_string());
                false
            }
        }
    }
}

// ---------------------------------------------------------------- install from .ckan

pub struct CkanDlg {
    modules: Vec<Module>,
    errors: Vec<String>,
    missing: Vec<GameVersion>,
    add_missing: bool,
    has_pins: bool,
    ignore_pins: bool,
}

fn has_pins(m: &Module) -> bool {
    m.is_metapackage() && m.depends.iter().any(|r| matches!(r, Relationship::One(t) if t.version.is_some()))
}

/// File > Install from .ckan: pick files, read them, then show the options dialog.
pub fn pick_ckan_files(app: &mut App) {
    let Some(files) = rfd::FileDialog::new().set_title("Install from .ckan").add_filter("CKAN metadata", &["ckan"]).pick_files()
    else {
        return;
    };
    let Some(s) = app.idle_session() else { return };
    let mut modules = Vec::new();
    let mut errors = Vec::new();
    for f in files {
        match modlist::read_ckan(&f.to_string_lossy()) {
            Ok(m) if m.is_dlc() => errors.push(format!("{}: {} is a DLC and cannot be installed", f.display(), m.identifier)),
            Ok(m) => modules.push(m),
            Err(e) => errors.push(format!("{}: {e}", f.display())),
        }
    }
    let mut missing: Vec<GameVersion> = Vec::new();
    for m in modules.iter().filter(|m| m.is_metapackage()) {
        for v in modlist::missing_compat_versions(m, &s.criteria, &s.index) {
            if !missing.contains(&v) {
                missing.push(v);
            }
        }
    }
    let pins = modules.iter().any(has_pins);
    app.dlg.ckan = Some(CkanDlg { modules, errors, add_missing: !missing.is_empty(), missing, has_pins: pins, ignore_pins: false });
}

/// Install requests for modules read from .ckan files. A modpack (metapackage) adds one request
/// per dependency that is not installed yet, plus the modpack itself; any other module is
/// installed as it is.
pub fn ckan_requests(modules: Vec<Module>, reg: &crate::registry::Registry, ignore_pins: bool) -> Vec<(String, InstallRequest)> {
    let mut extra: Vec<(String, InstallRequest)> = Vec::new();
    for m in modules {
        if m.is_metapackage() {
            let mp = if ignore_pins { modlist::modpack_without_versions(&m) } else { m };
            for r in modlist::modpack_requests(&mp, reg, ignore_pins) {
                let label = match &r {
                    InstallRequest::Latest(id) => id.clone(),
                    InstallRequest::Exact(id, v) => format!("{id} {v}"),
                    InstallRequest::Module(x) => x.to_string(),
                };
                extra.push((label, r));
            }
            extra.push((format!("{} (modpack)", mp.display_name()), InstallRequest::Module(Arc::new(mp))));
        } else {
            extra.push((m.to_string(), InstallRequest::Module(Arc::new(m))));
        }
    }
    extra
}

impl CkanDlg {
    pub fn show(&mut self, app: &mut App, ctx: &egui::Context) -> bool {
        let mut go = false;
        let mut cancel = false;
        let keep = window(ctx, "Install from .ckan", [560.0, 380.0], |ui| {
            for m in &self.modules {
                let kind = if m.is_metapackage() { format!("modpack, {} mods", m.depends.len()) } else { "mod".into() };
                ui.label(format!("{} {} ({kind})", m.display_name(), m.version));
            }
            for e in &self.errors {
                ui.colored_label(RED, e);
            }
            if !self.missing.is_empty() {
                let list: Vec<String> = self.missing.iter().map(|v| v.to_string()).collect();
                ui.checkbox(
                    &mut self.add_missing,
                    format!("Add game versions {} to the compatible list (the modpack targets them)", list.join(", ")),
                );
            }
            if self.has_pins {
                ui.checkbox(&mut self.ignore_pins, "Ignore the pinned versions and install the latest compatible ones");
            }
            ui.separator();
            ui.horizontal(|ui| {
                let can = !self.modules.is_empty() && app.idle_session().is_some();
                go = ui.add_enabled(can, egui::Button::new(RichText::new("Continue").strong())).clicked();
                cancel = ui.button("Cancel").clicked();
            });
        });
        if go {
            self.go(app);
            return false;
        }
        keep && !cancel
    }

    fn go(&mut self, app: &mut App) {
        if self.add_missing
            && !self.missing.is_empty()
            && !app.busy()
            && let Some(s) = app.session.as_mut()
        {
            let mut v = s.instance.compatible_versions();
            v.extend(self.missing.iter().copied());
            if let Err(e) = s.set_compatible_versions(&v) {
                app.show_error("Cannot save compatible versions", &e.to_string());
                return;
            }
        }
        let Some(s) = app.idle_session() else { return };
        let extra = ckan_requests(std::mem::take(&mut self.modules), &s.registry, self.ignore_pins);
        app.pending.extra.extend(extra);
        plan_into_changes(app);
    }
}

// ---------------------------------------------------------------- import zips

#[derive(Default)]
pub struct ImportDlg {
    files: Vec<PathBuf>,
    delete_after: bool,
}

fn downloads_dir() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    ["Downloads", "Desktop"].iter().map(|d| home.join(d)).find(|p| p.is_dir())
}

impl ImportDlg {
    pub fn show(&mut self, app: &mut App, ctx: &egui::Context) -> bool {
        let mut go = false;
        let mut cancel = false;
        let keep = window(ctx, "Import downloaded mods", [560.0, 360.0], |ui| {
            ui.label("Add mod zips you downloaded yourself. Hangar finds them by their hash and puts them in the download cache.");
            if ui.button("Choose zip files...").clicked() {
                let mut d = rfd::FileDialog::new().set_title("Import mods").add_filter("Mods", &["zip"]);
                if let Some(dir) = downloads_dir() {
                    d = d.set_directory(dir);
                }
                if let Some(f) = d.pick_files() {
                    for p in f {
                        if !self.files.contains(&p) {
                            self.files.push(p);
                        }
                    }
                }
            }
            egui::ScrollArea::vertical().max_height(super::dialogs::fit_h(ui.ctx(), 180.0)).show(ui, |ui| {
                let mut drop: Option<usize> = None;
                for (i, f) in self.files.iter().enumerate() {
                    ui.horizontal(|ui| {
                        if ui.small_button("x").clicked() {
                            drop = Some(i);
                        }
                        ui.label(RichText::new(f.display().to_string()).small());
                    });
                }
                if let Some(i) = drop.filter(|&i| i < self.files.len()) {
                    self.files.remove(i);
                }
            });
            ui.checkbox(&mut self.delete_after, "Move imported zips to the Trash afterwards");
            ui.separator();
            ui.horizontal(|ui| {
                let can = !self.files.is_empty() && app.idle_session().is_some();
                go = ui.add_enabled(can, egui::Button::new(RichText::new("Import").strong())).clicked();
                cancel = ui.button("Cancel").clicked();
            });
        });
        if go {
            let files = std::mem::take(&mut self.files);
            let del = self.delete_after;
            let prev = app.prev_compat();
            let started = app.start_job("Importing mods", move |s, rep| {
                rep.progress(&format!("Hashing {} file(s)", files.len()), None);
                let r = modlist::import_zips(&files, &s.index, &s.cache, &s.criteria, del);
                worker::done_with(s, prev.as_deref(), Outcome::Imported(r))
            });
            if started {
                // It moves zips into the cache (and maybe to the Trash).
                app.mark_no_close();
            }
            return false;
        }
        keep && !cancel
    }
}

pub struct ImportResultDlg {
    lines: Vec<String>,
    installable: Vec<(Arc<Module>, bool)>,
}

/// An import job finished.
pub fn imported(app: &mut App, r: Result<ImportResult>) {
    match r {
        Err(e) => app.show_error("Import failed", &e.to_string()),
        Ok(res) => {
            let name = |p: &PathBuf| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let mut lines = Vec::new();
            for (f, m) in &res.stored {
                lines.push(format!("Stored {} as {m}", name(f)));
            }
            for (f, m) in &res.already_cached {
                lines.push(format!("Already cached: {m} ({})", name(f)));
            }
            if !res.not_found.is_empty() {
                let n: Vec<String> = res.not_found.iter().map(name).collect();
                lines.push(format!("Not found in the repositories: {}", n.join(", ")));
            }
            let installed = |m: &Module| {
                app.model.as_ref().and_then(|x| x.find(&m.identifier)).and_then(|i| app.model.as_ref().and_then(|x| x.row(i))).is_some_and(|r| {
                    r.installed_key.as_ref() == Some(&m.version)
                })
            };
            let installable: Vec<(Arc<Module>, bool)> =
                res.installable.iter().filter(|m| !installed(m)).map(|m| (Arc::clone(m), true)).collect();
            app.status = format!("Imported {} file(s)", res.stored.len() + res.already_cached.len());
            app.dlg.import_result = Some(ImportResultDlg { lines, installable });
        }
    }
}

impl ImportResultDlg {
    pub fn show(&mut self, app: &mut App, ctx: &egui::Context) -> bool {
        let mut go = false;
        let mut close = false;
        let keep = window(ctx, "Import finished", [520.0, 360.0], |ui| {
            egui::ScrollArea::vertical().max_height(super::dialogs::fit_h(ui.ctx(), 140.0)).id_salt("imp_lines").show(ui, |ui| {
                for l in &self.lines {
                    ui.label(l);
                }
            });
            if !self.installable.is_empty() {
                ui.separator();
                ui.label(RichText::new("Install these compatible mods?").strong());
                egui::ScrollArea::vertical().max_height(super::dialogs::fit_h(ui.ctx(), 160.0)).id_salt("imp_mods").show(ui, |ui| {
                    for (m, on) in self.installable.iter_mut() {
                        ui.checkbox(on, format!("{} {}", m.display_name(), m.version));
                    }
                });
            }
            ui.separator();
            ui.horizontal(|ui| {
                let n = self.installable.iter().filter(|(_, c)| *c).count();
                if n > 0 {
                    go = ui.add_enabled(app.idle_session().is_some(), egui::Button::new(format!("Mark {n} for install"))).clicked();
                }
                close = ui.button("Close").clicked();
            });
        });
        if go {
            for (m, _) in self.installable.iter().filter(|(_, c)| *c) {
                app.pending.install.insert(m.identifier.clone(), Some(m.version.clone()));
                app.pending.remove.remove(&m.identifier);
            }
            plan_into_changes(app);
            return false;
        }
        keep && !close
    }
}

// ---------------------------------------------------------------- history

pub struct HistoryDlg {
    files: Vec<PathBuf>,
    selected: Option<usize>,
    /// (identifier, version pin, checked) of the selected snapshot.
    entries: std::result::Result<Vec<(String, Option<ModuleVersion>, bool)>, String>,
}

fn file_label(p: &std::path::Path) -> String {
    p.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

impl HistoryDlg {
    pub fn new(s: &Session) -> HistoryDlg {
        HistoryDlg { files: s.instance.history(), selected: None, entries: Ok(Vec::new()) }
    }

    fn load(&mut self, i: usize) {
        self.selected = Some(i);
        let Some(p) = self.files.get(i) else { return };
        self.entries = modlist::read_ckan(&p.to_string_lossy()).map_err(|e| e.to_string()).map(|m| {
            m.depends
                .iter()
                .filter_map(|r| match r {
                    Relationship::One(t) => Some((t.name.clone(), t.version.clone(), true)),
                    Relationship::AnyOf { .. } => r.names().first().map(|n| ((*n).to_string(), None, true)),
                })
                .collect()
        });
    }

    pub fn show(&mut self, app: &mut App, ctx: &egui::Context) -> bool {
        let mut install = false;
        let mut close = false;
        let mut pick: Option<usize> = None;
        let keep = window(ctx, "Installation history", [720.0, 440.0], |ui| {
            if self.files.is_empty() {
                ui.weak("No snapshots yet. Hangar saves one after every change.");
            }
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(260.0);
                    egui::ScrollArea::vertical().id_salt("hist_files").max_height(super::dialogs::fit_h(ui.ctx(), 340.0)).show(ui, |ui| {
                        for (i, f) in self.files.iter().enumerate() {
                            if ui.selectable_label(self.selected == Some(i), file_label(f)).clicked() {
                                pick = Some(i);
                            }
                        }
                    });
                });
                // No vertical separator: it would stretch the window to the screen height.
                ui.add_space(16.0);
                ui.vertical(|ui| match &mut self.entries {
                    Err(e) => {
                        ui.colored_label(RED, e.as_str());
                    }
                    Ok(list) if list.is_empty() => {
                        ui.weak("Select a snapshot to see its mods.");
                    }
                    Ok(list) => {
                        ui.horizontal(|ui| {
                            if ui.small_button("Check all").clicked() {
                                list.iter_mut().for_each(|e| e.2 = true);
                            }
                            if ui.small_button("Uncheck all").clicked() {
                                list.iter_mut().for_each(|e| e.2 = false);
                            }
                        });
                        egui::ScrollArea::vertical().id_salt("hist_mods").max_height(super::dialogs::fit_h(ui.ctx(), 300.0)).show(ui, |ui| {
                            for (id, v, on) in list.iter_mut() {
                                let row = app.model.as_ref().and_then(|m| m.find(id)).and_then(|i| app.model.as_ref().and_then(|m| m.row(i)));
                                let state = match row {
                                    Some(r) if r.installed && (v.is_none() || r.installed_key == *v) => ("installed", GREEN),
                                    Some(r) if r.installed => ("other version installed", RED),
                                    Some(_) => ("not installed", ui.visuals().text_color()),
                                    None => ("not in the repositories", RED),
                                };
                                ui.horizontal(|ui| {
                                    let ver = v.as_ref().map(|x| x.to_string()).unwrap_or_default();
                                    ui.checkbox(on, format!("{id} {ver}"));
                                    ui.label(RichText::new(state.0).small().color(state.1));
                                });
                            }
                        });
                    }
                });
            });
            ui.separator();
            ui.horizontal(|ui| {
                let any = self.entries.as_ref().is_ok_and(|l| l.iter().any(|e| e.2));
                install = ui.add_enabled(any && app.idle_session().is_some(), egui::Button::new("Install selected")).clicked();
                close = ui.button("Close").clicked();
            });
        });
        if let Some(i) = pick {
            self.load(i);
        }
        if install {
            let Ok(list) = &self.entries else { return true };
            let mut n = 0;
            for (id, v, on) in list.iter().filter(|e| e.2) {
                let _ = on;
                let Some(r) = app.model.as_ref().and_then(|m| m.find(id)).and_then(|i| app.model.as_ref().and_then(|m| m.row(i))).cloned() else {
                    continue;
                };
                match v {
                    Some(v) => {
                        if r.installed_key.as_ref() != Some(v) {
                            app.pending.install_version(&r, v);
                            n += 1;
                        }
                    }
                    None if !r.installed => {
                        app.pending.set_installed(&r, true);
                        n += 1;
                    }
                    None => {}
                }
            }
            app.status = format!("{n} mod(s) marked from history");
            if n > 0 {
                plan_into_changes(app);
            }
            return false;
        }
        keep && !close
    }
}

// ---------------------------------------------------------------- unmanaged files

pub struct UnmanagedDlg {
    files: Vec<String>,
    checked: Vec<bool>,
    game_dir: PathBuf,
    confirm: bool,
    msg: Option<String>,
}

impl UnmanagedDlg {
    pub fn new(s: &Session) -> UnmanagedDlg {
        let files = s.instance.unmanaged_files(&|p| s.registry.owner_of(p).is_some());
        UnmanagedDlg { checked: vec![false; files.len()], files, game_dir: s.instance.game_dir.clone(), confirm: false, msg: None }
    }

    pub fn show(&mut self, app: &mut App, ctx: &egui::Context) -> bool {
        let mut close = false;
        let mut trash = false;
        let keep = window(ctx, "Unmanaged files", [700.0, 480.0], |ui| {
            ui.label(format!(
                "{} file(s) in the game folder belong to no mod and are not stock files.",
                self.files.len()
            ));
            ui.horizontal(|ui| {
                if ui.small_button("Check all").clicked() {
                    self.checked.iter_mut().for_each(|c| *c = true);
                }
                if ui.small_button("Uncheck all").clicked() {
                    self.checked.iter_mut().for_each(|c| *c = false);
                }
                if ui.small_button("Open game folder").clicked() {
                    self.msg = Some(reveal(&self.game_dir));
                }
            });
            let h = ui.spacing().interact_size.y;
            egui::ScrollArea::vertical().max_height(super::dialogs::fit_h(ui.ctx(), 320.0)).auto_shrink([false, true]).show_rows(ui, h, self.files.len(), |ui, range| {
                for i in range {
                    let (Some(f), Some(c)) = (self.files.get(i), self.checked.get_mut(i)) else { continue };
                    ui.checkbox(c, RichText::new(f).monospace().small());
                }
            });
            if let Some(m) = &self.msg {
                ui.label(RichText::new(m).italics());
            }
            ui.separator();
            let n = self.checked.iter().filter(|c| **c).count();
            ui.horizontal(|ui| {
                if self.confirm {
                    ui.colored_label(RED, format!("Move {n} file(s) to the Trash?"));
                    trash = ui.button("Yes, move to Trash").clicked();
                    if ui.button("No").clicked() {
                        self.confirm = false;
                    }
                } else {
                    let can = n > 0 && app.idle_session().is_some();
                    if ui.add_enabled(can, egui::Button::new(format!("Move {n} selected to Trash..."))).clicked() {
                        self.confirm = true;
                    }
                    close = ui.button("Close").clicked();
                }
            });
        });
        if trash {
            self.confirm = false;
            let sel: Vec<String> =
                self.files.iter().zip(&self.checked).filter(|(_, c)| **c).map(|(f, _)| f.clone()).collect();
            if let Some(s) = app.idle_session() {
                match s.trash_files(&sel) {
                    Ok(()) => {
                        *self = UnmanagedDlg::new(s);
                        self.msg = Some(format!("Moved {} file(s) to the Trash", sel.len()));
                    }
                    Err(e) => self.msg = Some(format!("Failed: {e}")),
                }
            }
        }
        keep && !close
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modpack_requests_from_ckan() {
        let pack = Module::from_json(
            br#"{"spec_version":1,"identifier":"MyPack","name":"My pack","version":"1","kind":"metapackage",
            "author":"me","depends":[{"name":"A","version":"1.0"},{"name":"B"}]}"#,
        )
        .expect("pack");
        assert!(has_pins(&pack));
        let reg = crate::registry::Registry::empty();
        let r = ckan_requests(vec![pack.clone()], &reg, false);
        assert_eq!(r.len(), 3);
        assert!(matches!(&r[0].1, InstallRequest::Exact(id, v) if id == "A" && v.as_str() == "1.0"));
        assert!(matches!(&r[1].1, InstallRequest::Latest(id) if id == "B"));
        assert!(matches!(&r[2].1, InstallRequest::Module(m) if m.identifier == "MyPack"));
        let r = ckan_requests(vec![pack], &reg, true);
        assert!(matches!(&r[0].1, InstallRequest::Latest(id) if id == "A"));
        assert!(matches!(&r[2].1, InstallRequest::Module(m) if !has_pins(m)));
        let one = Module::from_json(br#"{"identifier":"Solo","version":"2","author":"x","download":"https://x/y.zip"}"#)
            .expect("solo");
        let r = ckan_requests(vec![one], &reg, false);
        assert!(matches!(r.as_slice(), [(_, InstallRequest::Module(m))] if m.identifier == "Solo"));
    }

    #[test]
    fn identifiers() {
        assert!(valid_identifier("installed-My-KSP"));
        assert!(!valid_identifier("-x"));
        assert!(!valid_identifier("a"));
        assert!(!valid_identifier("a b"));
    }
}
