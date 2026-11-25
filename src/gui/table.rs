//! The mod list: a virtual TableBuilder over the cached, filtered, sorted row indices.
//! Rendering only collects `Action`s; they are applied after the table is drawn.

use std::sync::Arc;

use eframe::egui::{self, Align, Color32, Layout, RichText, Sense};
use egui_extras::{Column, TableBuilder};

use super::app::{self, App, BLUE, GREEN, PURPLE, RED};
use super::changes::Flow;
use super::model::{COLUMNS, Col, Mark, Pending, Row, RowKind};
use super::worker::{self, Outcome};
use crate::meta::Module;
use crate::resolve::{Change, ChangeKind, Changeset, Reason};

pub const ROW_HEIGHT: f32 = 22.0;

#[derive(Debug, Clone)]
pub enum Action {
    Select(usize),
    SetInstalled(usize, bool),
    SetUpgrade(usize, bool),
    SetReplace(usize, bool),
    Sort(Col),
    ToggleColumn(Col),
    ToggleLabel(usize, usize),
    Download(usize),
    Purge(usize),
    Reinstall(usize),
    SetAuto(usize, bool),
    OpenUrl(String),
}

/// Starting width of a column (the user can resize; egui keeps the new width).
fn base_width(c: Col) -> f32 {
    match c {
        Col::Installed => 64.0,
        Col::Update => 56.0,
        Col::Replace => 60.0,
        Col::Name => 280.0,
        Col::Author => 130.0,
        Col::InstalledVersion | Col::Latest => 100.0,
        Col::MaxGame => 90.0,
        Col::DownloadSize => 78.0,
        Col::ReleaseDate => 88.0,
        Col::InstallDate => 125.0,
        Col::Downloads => 80.0,
    }
}

fn column_for(c: Col, name_width: f32) -> Column {
    match c {
        Col::Installed | Col::Update | Col::Replace => Column::exact(base_width(c)),
        Col::Name => Column::initial(name_width).at_least(120.0).resizable(true).clip(true),
        _ => Column::initial(base_width(c)).at_least(40.0).resizable(true).clip(true),
    }
}

/// `12345` -> `12,345`.
pub fn fmt_count(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn mark_color(m: Mark) -> Option<Color32> {
    match m {
        Mark::None => None,
        Mark::Install => Some(GREEN),
        Mark::Remove => Some(RED),
        Mark::Upgrade => Some(BLUE),
        Mark::Replace => Some(PURPLE),
    }
}

fn fill(ui: &egui::Ui, c: Color32) {
    ui.painter().rect_filled(ui.max_rect(), 0.0, c);
}

fn text_cell(ui: &mut egui::Ui, text: RichText) {
    ui.add_space(6.0);
    ui.add(egui::Label::new(text).truncate().selectable(false));
}

fn check_cell(ui: &mut egui::Ui, value: bool, enabled: bool) -> Option<bool> {
    let mut v = value;
    let mut out = None;
    ui.with_layout(Layout::centered_and_justified(egui::Direction::LeftToRight), |ui| {
        if ui.add_enabled(enabled, egui::Checkbox::without_text(&mut v)).changed() {
            out = Some(v);
        }
    });
    out
}

/// Per-frame data the cells need besides the row.
struct Ctx<'a> {
    pending: &'a Pending,
    tints: &'a [Option<Color32>],
    weak: Color32,
    can_change: bool,
}

fn cell(ui: &mut egui::Ui, r: &Row, ri: usize, c: Col, cx: &Ctx, actions: &mut Vec<Action>) {
    let mark = cx.pending.mark(r);
    // Row tint from the first label that has a color.
    if let Some(t) = r.labels.iter().find_map(|&l| cx.tints.get(usize::from(l)).copied().flatten()) {
        fill(ui, t);
    }
    let dim = !r.compatible && !r.installed;
    let color = |rt: RichText| if dim { rt.color(cx.weak) } else { rt };
    match c {
        Col::Installed => {
            if matches!(mark, Mark::Install | Mark::Remove)
                && let Some(mc) = mark_color(mark)
            {
                fill(ui, mc.gamma_multiply(0.35));
            }
            let on = cx.pending.install_checked(r);
            if let Some(v) = check_cell(ui, on, cx.can_change && r.can_toggle_install()) {
                actions.push(Action::SetInstalled(ri, v));
            }
        }
        Col::Update => {
            if r.upgradeable {
                let on = cx.pending.upgrade.contains(&r.id);
                if on {
                    fill(ui, BLUE.gamma_multiply(0.35));
                }
                if let Some(v) = check_cell(ui, on, cx.can_change) {
                    actions.push(Action::SetUpgrade(ri, v));
                }
            }
        }
        Col::Replace => {
            if r.replaceable {
                let on = cx.pending.replace.contains(&r.id);
                if on {
                    fill(ui, PURPLE.gamma_multiply(0.35));
                }
                if let Some(v) = check_cell(ui, on, cx.can_change) {
                    actions.push(Action::SetReplace(ri, v));
                }
            }
        }
        Col::Name => {
            ui.add_space(6.0);
            let mut t = RichText::new(&r.name);
            if let Some(mc) = mark_color(mark) {
                t = t.color(mc).strong();
            } else {
                t = color(t);
            }
            ui.add(egui::Label::new(t).truncate().selectable(false));
            if r.auto {
                ui.add_space(4.0);
                ui.add(egui::Label::new(RichText::new("auto").small().color(cx.weak)).selectable(false))
                    .on_hover_text("Installed automatically as a dependency");
            }
            if r.newly_compatible {
                ui.add_space(4.0);
                ui.add(egui::Label::new(RichText::new("new").small().color(GREEN)).selectable(false));
            }
            if r.kind == RowKind::Dlc {
                ui.add_space(4.0);
                ui.add(egui::Label::new(RichText::new("DLC").small().color(cx.weak)).selectable(false));
            }
        }
        Col::Author => text_cell(ui, color(RichText::new(&r.author))),
        Col::InstalledVersion => text_cell(ui, color(RichText::new(&r.installed_version).monospace())),
        Col::Latest => {
            let mut t = RichText::new(&r.latest_version).monospace();
            if r.upgradeable {
                t = t.color(BLUE);
            }
            text_cell(ui, color(t));
        }
        Col::MaxGame => text_cell(ui, color(RichText::new(&r.max_game).monospace())),
        Col::DownloadSize => {
            let s = if r.download_size > 0 { crate::util::human_size(r.download_size) } else { String::new() };
            text_cell(ui, color(RichText::new(s)));
        }
        Col::ReleaseDate => text_cell(ui, color(RichText::new(&r.release_date).monospace())),
        Col::InstallDate => text_cell(ui, color(RichText::new(&r.install_date).monospace())),
        Col::Downloads => {
            let s = if r.downloads > 0 { fmt_count(r.downloads) } else { String::new() };
            text_cell(ui, color(RichText::new(s)));
        }
    }
}

fn header_cell(ui: &mut egui::Ui, c: Col, sort: (Col, bool), hidden: &[String], actions: &mut Vec<Action>) {
    let arrow = if sort.0 == c { if sort.1 { " ^" } else { " v" } } else { "" };
    ui.add_space(6.0);
    let resp = ui.add(egui::Button::new(RichText::new(format!("{}{arrow}", c.title())).strong()).frame(false).truncate());
    if resp.clicked() {
        actions.push(Action::Sort(c));
    }
    resp.context_menu(|ui| column_menu(ui, hidden, actions));
}

fn column_menu(ui: &mut egui::Ui, hidden: &[String], actions: &mut Vec<Action>) {
    ui.label(RichText::new("Columns").strong());
    for c in COLUMNS.iter().copied().filter(|c| c.can_hide()) {
        let mut shown = !hidden.iter().any(|h| h == c.key());
        if ui.checkbox(&mut shown, c.title()).changed() {
            actions.push(Action::ToggleColumn(c));
        }
    }
}

fn homepage(m: &Module) -> Option<&str> {
    ["homepage", "spacedock", "repository", "curse"].iter().find_map(|k| m.resources.get(k))
}

fn row_menu(ui: &mut egui::Ui, r: &Row, ri: usize, app: &App, actions: &mut Vec<Action>) {
    let idle = app.idle_session().is_some();
    ui.label(RichText::new(&r.name).strong());
    ui.separator();
    ui.menu_button("Labels", |ui| {
        if app.label_info.names.is_empty() {
            ui.weak("No labels");
        }
        for (li, name) in app.label_info.names.iter().enumerate() {
            let mut on = r.labels.iter().any(|&l| usize::from(l) == li);
            if ui.add_enabled(idle, egui::Checkbox::new(&mut on, name.as_str())).changed() {
                actions.push(Action::ToggleLabel(ri, li));
            }
        }
    });
    ui.separator();
    let downloadable = r.kind == RowKind::Module && !r.module.download.is_empty() && !r.module.is_metapackage();
    if ui.add_enabled(idle && downloadable && !r.cached, egui::Button::new("Download to cache")).clicked() {
        actions.push(Action::Download(ri));
    }
    if ui.add_enabled(idle && r.cached, egui::Button::new("Purge from cache")).clicked() {
        actions.push(Action::Purge(ri));
    }
    let managed = r.installed && r.kind == RowKind::Module;
    if ui.add_enabled(idle && managed, egui::Button::new("Reinstall")).clicked() {
        actions.push(Action::Reinstall(ri));
    }
    if managed {
        let (label, auto) = if r.auto { ("Mark as user-installed", false) } else { ("Mark as auto-installed", true) };
        if ui.add_enabled(idle, egui::Button::new(label)).clicked() {
            actions.push(Action::SetAuto(ri, auto));
        }
    }
    ui.separator();
    let hp = homepage(&r.module);
    if ui.add_enabled(hp.is_some(), egui::Button::new("Open homepage")).clicked()
        && let Some(u) = hp
    {
        actions.push(Action::OpenUrl(u.to_string()));
    }
    if ui.button("Copy identifier").clicked() {
        ui.ctx().copy_text(r.id.clone());
    }
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let Some(model) = app.model.as_ref() else { return };
    if app.view.is_empty() {
        ui.vertical_centered(|ui| {
            ui.add_space(40.0);
            ui.heading("No mods match");
            ui.weak("Change the filter or the search text.");
        });
        return;
    }
    let hidden = app.config.hangar.hidden_columns.clone();
    let cols: Vec<Col> = COLUMNS.iter().copied().filter(|c| !c.can_hide() || !hidden.iter().any(|h| h == c.key())).collect();
    let dark = ui.visuals().dark_mode;
    let tints: Vec<Option<Color32>> = app
        .label_info
        .colors
        .iter()
        .map(|c| c.map(|[r, g, b]| Color32::from_rgb(r, g, b).gamma_multiply(if dark { 0.22 } else { 0.40 })))
        .collect();
    let cx = Ctx { pending: &app.pending, tints: &tints, weak: ui.visuals().weak_text_color(), can_change: !app.busy() };
    let selected = app.selected.as_deref().and_then(|id| model.find(id));
    let scroll_to = if app.scroll_to_selected { selected.and_then(|s| app.view.iter().position(|&i| i == s)) } else { None };
    let sort = app.sort;
    let mut actions: Vec<Action> = Vec::new();
    // Name takes the width the other columns leave (first layout only).
    let others: f32 = cols.iter().filter(|c| **c != Col::Name).map(|c| base_width(*c)).sum();
    let name_width = (ui.available_width() - others - 16.0).max(base_width(Col::Name));
    // Wide tables scroll sideways instead of being cut off.
    egui::ScrollArea::horizontal().id_salt("modlist_h").auto_shrink([false, false]).show(ui, |ui| {
        ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
        let mut tb = TableBuilder::new(ui)
            // A new text size starts a new layout, so the Name column fits the new width.
            .id_salt(("modlist", app.config.hangar.text_size))
            .striped(true)
            .resizable(true)
            .sense(Sense::click())
            .auto_shrink([true, false])
            .cell_layout(Layout::left_to_right(Align::Center));
        for c in &cols {
            tb = tb.column(column_for(*c, name_width));
        }
        if let Some(r) = scroll_to {
            tb = tb.scroll_to_row(r, Some(Align::Center));
        }
        tb.header(24.0, |mut h| {
            for c in &cols {
                h.col(|ui| header_cell(ui, *c, sort, &hidden, &mut actions));
            }
        })
        .body(|body| {
            body.rows(ROW_HEIGHT, app.view.len(), |mut row| {
                let Some(&ri) = app.view.get(row.index()) else { return };
                let Some(r) = model.row(ri) else { return };
                row.set_selected(Some(ri) == selected);
                for c in &cols {
                    row.col(|ui| cell(ui, r, ri, *c, &cx, &mut actions));
                }
                let resp = row.response();
                if resp.clicked() || resp.secondary_clicked() {
                    actions.push(Action::Select(ri));
                }
                resp.context_menu(|ui| row_menu(ui, r, ri, app, &mut actions));
            });
        });
    });
    app.scroll_to_selected = false;
    for a in actions {
        apply(app, a);
    }
}

pub fn open_url(app: &mut App, url: &str) {
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        app.status = format!("Not a web link: {url}");
        return;
    }
    if let Err(e) = std::process::Command::new("/usr/bin/open").arg(url).spawn() {
        app.show_error("Cannot open the link", &e.to_string());
    }
}

/// Reinstall = remove and install the same version, as one change set.
fn reinstall_changeset(r: &Row) -> Changeset {
    let m: Arc<Module> = Arc::clone(&r.module);
    Changeset {
        changes: vec![
            Change {
                kind: ChangeKind::Remove,
                module: Arc::clone(&m),
                old_version: None,
                replaces: None,
                reasons: vec![Reason::UserRequested],
                auto_installed: false,
            },
            Change {
                kind: ChangeKind::Install,
                module: m,
                old_version: None,
                replaces: None,
                reasons: vec![Reason::UserRequested],
                auto_installed: r.auto,
            },
        ],
    }
}

pub fn apply(app: &mut App, a: Action) {
    let row = |app: &App, ri: usize| app.model.as_ref().and_then(|m| m.row(ri)).cloned();
    match a {
        Action::Select(ri) => {
            if let Some(r) = row(app, ri) {
                app.selected = Some(r.id);
            }
        }
        Action::SetInstalled(ri, v) => {
            if let Some(r) = row(app, ri) {
                app.pending.set_installed(&r, v);
                app.pending_changed();
            }
        }
        Action::SetUpgrade(ri, v) => {
            if let Some(r) = row(app, ri) {
                app.pending.set_upgrade(&r, v);
                app.pending_changed();
            }
        }
        Action::SetReplace(ri, v) => {
            if let Some(r) = row(app, ri) {
                app.pending.set_replace(&r, v);
                app.pending_changed();
            }
        }
        Action::Sort(c) => {
            app.sort = if app.sort.0 == c {
                (c, !app.sort.1)
            } else {
                // Numbers and flags start with the biggest first.
                let asc = matches!(c, Col::Name | Col::Author);
                (c, asc)
            };
            app.view_dirty = true;
        }
        Action::ToggleColumn(c) => {
            app::toggle_column(&mut app.config, c);
            app.commit_config();
        }
        Action::ToggleLabel(ri, li) => toggle_label(app, ri, li),
        Action::Download(ri) => {
            let Some(r) = row(app, ri) else { return };
            let m = r.latest.clone().unwrap_or_else(|| Arc::clone(&r.module));
            let prev = app.prev_compat();
            app.start_job("Downloading", move |s, rep| {
                let p = |msg: &str, f: Option<f32>| rep.progress(msg, f);
                let res = s.download(&[m], &p);
                worker::done_with(s, prev.as_deref(), Outcome::Downloaded(res))
            });
        }
        Action::Purge(ri) => {
            let Some(r) = row(app, ri) else { return };
            let Some(s) = app.idle_session() else { return };
            match s.cache.purge(&r.module) {
                Ok(()) => {
                    if let Some(x) = app.model.as_mut().and_then(|m| m.rows.get_mut(ri)) {
                        x.cached = false;
                    }
                    app.view_dirty = true;
                    app.status = format!("Purged {} from the cache", r.name);
                }
                Err(e) => app.show_error("Cannot purge", &e.to_string()),
            }
        }
        Action::Reinstall(ri) => {
            if let Some(r) = row(app, ri) {
                app.flow = Flow::Confirm { cs: reinstall_changeset(&r) };
            }
        }
        Action::SetAuto(ri, auto) => {
            let Some(r) = row(app, ri) else { return };
            if app.busy() {
                return;
            }
            let Some(s) = app.session.as_mut() else { return };
            match s.set_auto_installed(&r.id, auto) {
                Ok(()) => {
                    if let Some(x) = app.model.as_mut().and_then(|m| m.rows.get_mut(ri)) {
                        x.auto = auto;
                    }
                    app.status = format!("{} marked as {}", r.name, if auto { "auto-installed" } else { "user-installed" });
                }
                Err(e) => app.show_error("Cannot change the auto-installed flag", &e.to_string()),
            }
        }
        Action::OpenUrl(u) => open_url(app, &u),
    }
}

fn toggle_label(app: &mut App, ri: usize, li: usize) {
    let Some(id) = app.model.as_ref().and_then(|m| m.row(ri)).map(|r| r.id.clone()) else { return };
    if app.busy() {
        return;
    }
    let Some(s) = app.session.as_mut() else { return };
    s.labels.toggle(li, &id);
    let holds = s.labels.labels.get(li).is_some_and(|l| l.hold_version);
    if let Err(e) = s.labels.save() {
        app.show_error("Cannot save labels", &e.to_string());
        return;
    }
    s.reload_labels();
    let name = s.instance.name.clone();
    let labels = s.labels.clone();
    if let Some(m) = app.model.as_mut() {
        super::model::relabel(m, &labels, &name);
    }
    app.label_info = super::model::LabelInfo::of(&labels);
    app.view_dirty = true;
    if holds {
        // Held mods change the update list.
        app.rebuild();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_format() {
        assert_eq!(fmt_count(0), "0");
        assert_eq!(fmt_count(999), "999");
        assert_eq!(fmt_count(1000), "1,000");
        assert_eq!(fmt_count(1234567), "1,234,567");
    }
}
