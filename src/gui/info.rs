//! Mod info side panel: Metadata, Relationships, Contents, Versions.
//! Heavy data (file lists, zip entries) is read once per selected mod, not per frame.

use std::sync::Arc;

use eframe::egui::{self, RichText};

use super::app::{App, BLUE, GREEN, RED};
use super::model::{Row, RowKind};
use crate::meta::{Module, Relationship};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InfoTab {
    #[default]
    Metadata,
    Relationships,
    Contents,
    Versions,
}

#[derive(Default)]
pub struct InfoState {
    pub tab: InfoTab,
    for_id: String,
    /// Installed files of the selected mod.
    files: Option<Vec<String>>,
    /// Entries of the cached zip, or an error text.
    zip: Option<Result<Vec<String>, String>>,
}

impl InfoState {
    /// Forget cached data (the session or the rows changed).
    pub fn reset(&mut self) {
        self.for_id.clear();
        self.files = None;
        self.zip = None;
    }
}

enum Click {
    Select(String),
    Url(String),
    InstallVersion(crate::meta::ModuleVersion),
    Download,
}

pub fn game_versions_text(m: &Module) -> String {
    let any = |g: &crate::meta::GameVersion| if g.is_any() { "any".to_string() } else { g.to_string() };
    match (m.ksp_version, m.ksp_version_min, m.ksp_version_max) {
        (Some(v), _, _) => any(&v),
        (None, None, None) => "any".into(),
        (None, lo, hi) => format!(
            "{} - {}",
            lo.map(|g| any(&g)).unwrap_or_else(|| "any".into()),
            hi.map(|g| any(&g)).unwrap_or_else(|| "any".into())
        ),
    }
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let Some(row) = app.selected_row().cloned() else { return };
    if app.info.for_id != row.id {
        app.info.reset();
        app.info.for_id = row.id.clone();
    }
    let mut clicks: Vec<Click> = Vec::new();
    ui.add_space(4.0);
    ui.horizontal_top(|ui| {
        if ui.small_button("x").on_hover_text("Close").clicked() {
            app.selected = None;
        }
        ui.add(egui::Label::new(RichText::new(&row.name).heading()).wrap());
    });
    if app.selected.is_none() {
        return;
    }
    ui.label(RichText::new(&row.id).monospace().weak());
    if !row.module.abstract_.is_empty() {
        ui.add_space(2.0);
        ui.add(egui::Label::new(row.module.abstract_.as_str()).wrap());
    }
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        for (t, name) in [
            (InfoTab::Metadata, "Metadata"),
            (InfoTab::Relationships, "Relationships"),
            (InfoTab::Contents, "Contents"),
            (InfoTab::Versions, "Versions"),
        ] {
            if ui.selectable_label(app.info.tab == t, name).clicked() {
                app.info.tab = t;
            }
        }
    });
    ui.separator();
    match app.info.tab {
        InfoTab::Metadata => metadata(app, ui, &row, &mut clicks),
        InfoTab::Relationships => relationships(app, ui, &row, &mut clicks),
        InfoTab::Contents => contents(app, ui, &row, &mut clicks),
        InfoTab::Versions => versions(app, ui, &row, &mut clicks),
    }
    for c in clicks {
        match c {
            Click::Select(id) => app.select_id(&id),
            Click::Url(u) => super::table::open_url(app, &u),
            Click::InstallVersion(v) => {
                app.pending.install_version(&row, &v);
                app.pending_changed();
                app.status = format!("{} {v} marked for install", row.name);
            }
            Click::Download => {
                if let Some(ri) = app.model.as_ref().and_then(|m| m.find(&row.id)) {
                    super::table::apply(app, super::table::Action::Download(ri));
                }
            }
        }
    }
}

/// One "key: value" line that wraps inside the panel.
fn kv(ui: &mut egui::Ui, k: &str, v: impl Into<egui::WidgetText>) {
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(format!("{k}:")).weak());
        ui.add(egui::Label::new(v).wrap());
    });
}

/// A link that never widens the panel (long URLs are cut with an ellipsis).
fn link(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let color = ui.visuals().hyperlink_color;
    ui.add(egui::Label::new(RichText::new(text).color(color)).truncate().sense(egui::Sense::click()))
        .on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn metadata(app: &App, ui: &mut egui::Ui, row: &Row, clicks: &mut Vec<Click>) {
    let m = &row.module;
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.vertical(|ui| {
            if row.installed {
                kv(ui, "Installed", RichText::new(&row.installed_version).monospace());
            }
            if let Some(u) = &row.update_to {
                kv(ui, "Update to", RichText::new(u.version.display(app.hide_epochs(), app.hide_v())).monospace().color(BLUE));
            }
            if !row.latest_version.is_empty() {
                let mut t = RichText::new(&row.latest_version).monospace();
                if row.upgradeable {
                    t = t.color(BLUE);
                }
                kv(ui, "Latest", t);
            } else {
                kv(ui, "Latest", RichText::new("no compatible version").color(RED));
            }
            kv(ui, "Authors", m.author.join(", "));
            kv(ui, "License", m.license.join(", "));
            kv(ui, "Status", m.release_status.as_str());
            kv(ui, "Game versions", RichText::new(game_versions_text(m)).monospace());
            if m.download_size > 0 {
                kv(ui, "Download size", crate::util::human_size(m.download_size));
            }
            if m.install_size > 0 {
                kv(ui, "Install size", crate::util::human_size(m.install_size));
            }
            if row.downloads > 0 {
                kv(ui, "Downloads", super::table::fmt_count(row.downloads));
            }
            if !row.release_date.is_empty() {
                kv(ui, "Released", RichText::new(&row.release_date).monospace());
            }
            if !row.install_date.is_empty() {
                kv(ui, "Install date", RichText::new(&row.install_date).monospace());
            }
            if row.installed && row.kind == RowKind::Module {
                kv(ui, "Installed by", if row.auto { "dependency (auto)" } else { "user" });
            }
            kv(ui, "Cached", if row.cached { "yes" } else { "no" });
            if m.kind != crate::meta::Kind::Package {
                kv(ui, "Kind", m.kind.as_str());
            }
            if !m.tags.is_empty() {
                kv(ui, "Tags", m.tags.join(", "));
            }
            let labels: Vec<&str> =
                row.labels.iter().filter_map(|&l| app.label_info.names.get(usize::from(l)).map(String::as_str)).collect();
            if !labels.is_empty() {
                kv(ui, "Labels", labels.join(", "));
            }
            if let Some(rb) = &m.replaced_by {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Replaced by:").weak());
                    if link(ui, &rb.to_string()).clicked() {
                        clicks.push(Click::Select(rb.name.clone()));
                    }
                });
            }
        });
        if !m.resources.is_empty() {
            ui.add_space(8.0);
            ui.label(RichText::new("Links").strong());
            for (k, url) in m.resources.iter() {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{k}:")).weak());
                    if link(ui, url).on_hover_text(url).clicked() {
                        clicks.push(Click::Url(url.to_string()));
                    }
                });
            }
        }
        if let Some(d) = m.description.as_deref().filter(|d| !d.trim().is_empty()) {
            ui.add_space(8.0);
            ui.label(RichText::new("Description").strong());
            ui.add(egui::Label::new(d).wrap());
        }
    });
}

/// Marker text and color for a relationship target.
fn target_state(app: &App, name: &str) -> (&'static str, egui::Color32) {
    let model = app.model.as_ref();
    let installed_row = |id: &str| model.and_then(|m| m.find(id)).and_then(|i| model.and_then(|m| m.row(i))).is_some_and(|r| r.installed);
    let providers: &[String] = app.index.as_ref().map(|i| i.providers_of(name)).unwrap_or(&[]);
    if installed_row(name) || providers.iter().any(|p| installed_row(p)) {
        ("installed", GREEN)
    } else if !providers.is_empty() {
        ("available", BLUE)
    } else {
        ("not found", RED)
    }
}

fn rel_line(app: &App, ui: &mut egui::Ui, rel: &Relationship, clicks: &mut Vec<Click>) {
    match rel {
        Relationship::One(t) => {
            ui.horizontal(|ui| {
                let (txt, col) = target_state(app, &t.name);
                ui.label(RichText::new(format!("[{txt}]")).small().color(col));
                if link(ui, &t.to_string()).clicked() {
                    let id = app
                        .model
                        .as_ref()
                        .and_then(|m| m.find(&t.name).map(|_| t.name.clone()))
                        .or_else(|| app.index.as_ref().and_then(|i| i.providers_of(&t.name).first().cloned()))
                        .unwrap_or_else(|| t.name.clone());
                    clicks.push(Click::Select(id));
                }
            });
        }
        Relationship::AnyOf { any_of, choice_help_text, .. } => {
            ui.label(RichText::new("any of:").weak());
            ui.indent("anyof", |ui| {
                for r in any_of {
                    rel_line(app, ui, r, clicks);
                }
                if let Some(h) = choice_help_text {
                    ui.weak(h);
                }
            });
        }
    }
}

fn relationships(app: &App, ui: &mut egui::Ui, row: &Row, clicks: &mut Vec<Click>) {
    let m = Arc::clone(&row.module);
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        let groups: [(&str, &[Relationship]); 5] = [
            ("Depends", &m.depends),
            ("Recommends", &m.recommends),
            ("Suggests", &m.suggests),
            ("Supports", &m.supports),
            ("Conflicts", &m.conflicts),
        ];
        let mut any = false;
        for (title, list) in groups {
            if list.is_empty() {
                continue;
            }
            any = true;
            ui.label(RichText::new(title).strong());
            ui.push_id(title, |ui| {
                ui.indent(title, |ui| {
                    for (k, r) in list.iter().enumerate() {
                        ui.push_id(k, |ui| rel_line(app, ui, r, clicks));
                    }
                });
            });
            ui.add_space(4.0);
        }
        if !m.provides.is_empty() {
            any = true;
            ui.label(RichText::new("Provides").strong());
            ui.indent("provides", |ui| {
                for p in &m.provides {
                    ui.label(RichText::new(p).monospace());
                }
            });
        }
        if let Some(rb) = &m.replaced_by {
            any = true;
            ui.label(RichText::new("Replaced by").strong());
            ui.indent("replaced", |ui| {
                if link(ui, &rb.to_string()).clicked() {
                    clicks.push(Click::Select(rb.name.clone()));
                }
            });
        }
        if !any {
            ui.weak("This mod has no relationships.");
        }
    });
}

fn contents(app: &mut App, ui: &mut egui::Ui, row: &Row, clicks: &mut Vec<Click>) {
    // Load once per selected mod.
    if row.installed
        && row.kind == RowKind::Module
        && app.info.files.is_none()
        && let Some(s) = app.session.as_ref()
    {
        app.info.files = Some(
            s.registry
                .installed(&row.id)
                .map(|im| im.installed_files.keys().cloned().collect())
                .unwrap_or_default(),
        );
    }
    if app.info.files.is_none()
        && row.cached
        && app.info.zip.is_none()
        && let Some(s) = app.session.as_ref()
    {
        let m = if row.installed { Arc::clone(&row.module) } else { row.latest.clone().unwrap_or_else(|| Arc::clone(&row.module)) };
        app.info.zip = Some(match s.cache.get(&m) {
            Some(p) => crate::install::zip_entry_names(&p).map_err(|e| e.to_string()),
            None => Err("The cached file is gone.".into()),
        });
    }
    let list: Option<&Vec<String>> = match (&app.info.files, &app.info.zip) {
        (Some(f), _) => {
            ui.label(RichText::new(format!("{} installed files and folders", f.len())).weak());
            Some(f)
        }
        (None, Some(Ok(z))) => {
            ui.label(RichText::new(format!("{} entries in the cached download", z.len())).weak());
            Some(z)
        }
        (None, Some(Err(e))) => {
            ui.colored_label(RED, e);
            None
        }
        (None, None) => {
            if app.session.is_none() {
                ui.weak("Busy, try again when the job ends.");
            } else {
                ui.weak("Not installed and not in the download cache.");
                let can = row.kind == RowKind::Module && !row.module.download.is_empty() && app.idle_session().is_some();
                if ui.add_enabled(can, egui::Button::new("Download to cache")).clicked() {
                    clicks.push(Click::Download);
                }
            }
            None
        }
    };
    if let Some(list) = list {
        let h = ui.text_style_height(&egui::TextStyle::Monospace);
        egui::ScrollArea::both().auto_shrink([false, false]).show_rows(ui, h, list.len(), |ui, range| {
            for f in list.iter().skip(range.start).take(range.len()) {
                ui.label(RichText::new(f).monospace().small());
            }
        });
    }
}

fn versions(app: &App, ui: &mut egui::Ui, row: &Row, clicks: &mut Vec<Click>) {
    let Some(index) = app.index.as_ref() else {
        ui.weak("No repository data.");
        return;
    };
    let list = index.versions(&row.id);
    if list.is_empty() {
        ui.weak("This mod is not in any repository.");
        return;
    }
    let crit = app.session.as_ref().map(|s| s.criteria.clone());
    let can = row.kind == RowKind::Module && app.idle_session().is_some();
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        egui::Grid::new("versions_grid").num_columns(4).striped(true).spacing([10.0, 4.0]).show(ui, |ui| {
            ui.label(RichText::new("Version").strong());
            ui.label(RichText::new("Game").strong());
            ui.label(RichText::new("Released").strong());
            ui.label("");
            ui.end_row();
            for m in list {
                let compatible = crit.as_ref().is_none_or(|c| m.is_compatible(c));
                let installed = row.installed_key.as_ref() == Some(&m.version);
                let mut v = RichText::new(m.version.display(app.hide_epochs(), app.hide_v())).monospace();
                if installed {
                    v = v.strong().color(GREEN);
                } else if !compatible {
                    v = v.weak();
                }
                ui.label(v);
                let mut g = RichText::new(game_versions_text(m)).monospace();
                if !compatible {
                    g = g.color(RED);
                }
                ui.label(g);
                let date = m.release_date.as_deref().and_then(|d| d.get(..10)).unwrap_or("");
                ui.label(RichText::new(date).monospace());
                if installed {
                    ui.weak("installed");
                } else {
                    let pending = app.pending.install.get(&row.id).is_some_and(|x| x.as_ref() == Some(&m.version));
                    let label = if pending { "Marked" } else { "Install" };
                    if ui
                        .add_enabled(can && compatible && !pending, egui::Button::new(label).small())
                        .on_disabled_hover_text(if compatible { "Busy or already marked" } else { "Not compatible with this game version" })
                        .clicked()
                    {
                        clicks.push(Click::InstallVersion(m.version.clone()));
                    }
                }
                ui.end_row();
            }
        });
    });
}
