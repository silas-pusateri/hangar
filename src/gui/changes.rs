//! Changes tab and the apply flow: plan -> (provider choice) -> recommendations -> confirm ->
//! apply on the worker -> report (or conflict -> move to Trash and retry).

use std::sync::Arc;
use std::sync::atomic::Ordering;

use eframe::egui::{self, Color32, RichText};

use super::app::{App, BLUE, GREEN, PURPLE, RED};
use super::model::{self, kind_text};
use super::worker::{self, Outcome, PlanMode};
use crate::error::{Error, Result};
use crate::meta::Module;
use crate::ops::ApplyReport;
use crate::resolve::{ChangeKind, Changeset, OptionalKind, OptionalMod, Request, ResolveError};

pub enum Flow {
    Idle,
    Choose {
        req: Request,
        mode: PlanMode,
        requester: String,
        requested: String,
        choices: Vec<Arc<Module>>,
        help: Option<String>,
        pick: usize,
    },
    Recommend {
        req: Request,
        cs: Changeset,
        items: Vec<(OptionalMod, bool)>,
    },
    Confirm {
        cs: Changeset,
    },
    Conflict {
        cs: Changeset,
        msg: String,
        files: Vec<String>,
    },
    Report {
        title: String,
        ok: bool,
        lines: Vec<String>,
        log: Vec<String>,
    },
}

pub fn kind_color(k: ChangeKind) -> Color32 {
    match k {
        ChangeKind::Install => GREEN,
        ChangeKind::Remove => RED,
        ChangeKind::Upgrade => BLUE,
        ChangeKind::Replace => PURPLE,
    }
}

/// Apply button: plan with the recommendations step.
pub fn begin_apply(app: &mut App) {
    if app.pending.is_empty() {
        return;
    }
    let req = app.pending.to_request();
    app.start_plan(req, PlanMode::Apply);
}

/// Plan for the Changes tab when it is opened.
pub fn preview_if_needed(app: &mut App) {
    if !app.pending.is_empty() && app.plan.is_none() && app.plan_error.is_none() && !app.busy() {
        let req = app.pending.to_request();
        app.start_plan(req, PlanMode::Preview);
    }
}

/// File > Audit recommendations: optional mods for everything installed.
pub fn audit_recommendations(app: &mut App) {
    app.start_job("Finding recommendations", |s, rep| {
        rep.progress("Checking installed mods", None);
        let cs = Changeset::default();
        let opt = crate::resolve::optional_mods(&s.context(), &cs);
        worker::done_same(
            s,
            Outcome::Planned { mode: PlanMode::Apply, req: Request::default(), result: Ok((cs, opt)) },
        )
    });
}

fn initial_check(app: &App, om: &OptionalMod) -> bool {
    match om.kind {
        OptionalKind::Recommends => om.checked && app.config.hangar.with_recommends,
        OptionalKind::Suggests => app.config.hangar.with_suggests,
        OptionalKind::Supports => false,
    }
}

/// A plan job finished.
pub fn planned(app: &mut App, mode: PlanMode, req: Request, result: Result<(Changeset, Vec<OptionalMod>)>) {
    match result {
        Ok((cs, opt)) => {
            let audit = req.install.is_empty() && req.remove.is_empty() && req.upgrade.is_empty() && req.replace.is_empty();
            // A plan with extra optional mods is not the plan of the pending list.
            if !audit && mode != PlanMode::ApplyNoRecs {
                app.plan = Some(cs.clone());
                app.plan_error = None;
            }
            match mode {
                PlanMode::Preview => app.status = format!("Planned {} change(s)", cs.changes.len()),
                PlanMode::Apply if !opt.is_empty() => {
                    app.status = "Choose optional mods".into();
                    let items = opt
                        .into_iter()
                        .map(|om| {
                            let c = initial_check(app, &om);
                            (om, c)
                        })
                        .collect();
                    app.flow = Flow::Recommend { req, cs, items };
                }
                PlanMode::Apply | PlanMode::ApplyNoRecs => {
                    if cs.is_empty() {
                        app.status = if audit { "No recommendations found".into() } else { "Nothing to do".into() };
                    } else {
                        app.status = "Review the changes".into();
                        app.flow = Flow::Confirm { cs };
                    }
                }
            }
        }
        Err(Error::Resolve(ResolveError::TooManyProvides { requester, requested, choices, help })) => {
            app.flow = Flow::Choose { req, mode, requester, requested, choices, help, pick: 0 };
        }
        Err(e) => {
            let text = e.to_string();
            app.plan_error = Some(text.clone());
            if mode == PlanMode::Preview {
                app.status = "The changes cannot be planned".into();
            } else {
                app.show_error("Cannot apply changes", &text);
            }
        }
    }
}

/// An apply job finished.
pub fn applied(app: &mut App, cs: Changeset, result: Result<ApplyReport>, log: Vec<String>) {
    match result {
        Ok(r) => {
            app.pending.clear();
            app.pending_changed();
            app.tab = super::app::Tab::Mods;
            let mut lines = Vec::new();
            let list = |what: &str, v: &[String], lines: &mut Vec<String>| {
                if !v.is_empty() {
                    lines.push(format!("{what}: {}", v.join(", ")));
                }
            };
            list("Installed", &r.installed, &mut lines);
            list("Updated", &r.upgraded, &mut lines);
            list("Removed", &r.removed, &mut lines);
            lines.extend(r.messages.iter().cloned());
            if lines.is_empty() {
                lines.push("Nothing changed.".into());
            }
            app.status = format!(
                "Applied: {} installed, {} updated, {} removed",
                r.installed.len(),
                r.upgraded.len(),
                r.removed.len()
            );
            app.flow = Flow::Report { title: "Changes applied".into(), ok: true, lines, log };
        }
        Err(e @ Error::Conflict(_)) => {
            let files = crate::ops::conflict_files(&e);
            let msg = e.to_string();
            app.status = "File conflict".into();
            app.flow = Flow::Conflict { cs, msg, files };
        }
        Err(e) => {
            app.status = "Apply failed".into();
            app.flow = Flow::Report { title: "Apply failed".into(), ok: false, lines: failure_lines(&e), log };
        }
    }
}

/// Text for a failed apply. Claim a clean undo only when the undo did not fail too.
pub fn failure_lines(e: &Error) -> Vec<String> {
    let text = e.to_string();
    let undo_failed = text.contains("Undoing the file changes also failed");
    let tail = if undo_failed {
        "Some files could not be put back. Check the game folder before you play."
    } else {
        "No files were changed; the registry was restored."
    };
    vec![text, tail.to_string()]
}

// ---------------------------------------------------------------- Changes tab

fn change_grid(ui: &mut egui::Ui, rows: &[(ChangeKind, String, String, String)]) {
    egui::Grid::new("changes_grid").num_columns(4).striped(true).spacing([14.0, 5.0]).show(ui, |ui| {
        ui.label(RichText::new("Change").strong());
        ui.label(RichText::new("Mod").strong());
        ui.label(RichText::new("Version").strong());
        ui.label(RichText::new("Reason").strong());
        ui.end_row();
        for (k, name, ver, why) in rows {
            ui.label(RichText::new(kind_text(*k)).color(kind_color(*k)).strong());
            ui.label(name);
            ui.label(RichText::new(ver).monospace());
            ui.label(RichText::new(why).weak());
            ui.end_row();
        }
    });
}

pub fn show_tab(app: &mut App, ui: &mut egui::Ui) {
    if app.pending.is_empty() && app.plan.is_none() {
        ui.vertical_centered(|ui| {
            ui.add_space(40.0);
            ui.heading("No pending changes");
            ui.weak("Tick mods in the list to install, update or remove them.");
        });
        return;
    }
    let idle = !app.busy();
    ui.horizontal(|ui| {
        if ui.add_enabled(idle, egui::Button::new("Clear")).clicked() {
            app.pending.clear();
            app.pending_changed();
        }
        if ui.add_enabled(idle && !app.pending.is_empty(), egui::Button::new("Plan again")).clicked() {
            app.plan = None;
            app.plan_error = None;
            preview_if_needed(app);
        }
        let apply = egui::Button::new(RichText::new("Apply").strong()).fill(GREEN.gamma_multiply(0.35));
        if ui.add_enabled(idle && !app.pending.is_empty(), apply).clicked() {
            begin_apply(app);
        }
        if app.play.running.load(Ordering::Relaxed) {
            ui.colored_label(RED, "Close the game before you apply changes.");
        }
    });
    ui.add_space(6.0);
    if let Some(e) = app.plan_error.as_ref() {
        ui.colored_label(RED, RichText::new("These changes cannot be made:").strong());
        ui.label(RichText::new(e).monospace());
        ui.add_space(8.0);
    }
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| match app.plan.as_ref() {
        Some(cs) => {
            let lines = model::change_lines(cs, app.hide_epochs(), app.hide_v());
            ui.weak(format!("{} change(s) after dependencies are resolved", lines.len()));
            change_grid(ui, &lines);
        }
        None => {
            if app.busy() {
                ui.weak("Planning...");
            } else {
                ui.weak("Requested changes (dependencies not resolved yet):");
            }
            let rows: Vec<(ChangeKind, String, String, String)> =
                app.pending.summary().into_iter().map(|(k, id, d)| (k, id, d, "Requested by user".into())).collect();
            change_grid(ui, &rows);
        }
    });
}

// ---------------------------------------------------------------- flow dialogs

fn modal<R>(ctx: &egui::Context, id: &str, width: f32, f: impl FnOnce(&mut egui::Ui) -> R) -> (R, bool) {
    let resp = egui::Modal::new(egui::Id::unique(id)).show(ctx, |ui| {
        // A fixed width: wide content (a long mod name) scrolls instead of widening the dialog.
        let w = super::dialogs::fit_w(ui.ctx(), width);
        ui.set_width(w);
        ui.set_max_width(w);
        f(ui)
    });
    let esc = resp.should_close();
    (resp.inner, esc)
}

pub fn show_flow(app: &mut App, ctx: &egui::Context) {
    let flow = std::mem::replace(&mut app.flow, Flow::Idle);
    let next = match flow {
        Flow::Idle => Flow::Idle,
        Flow::Choose { req, mode, requester, requested, choices, help, mut pick } => {
            let index = app.index.clone();
            let (act, esc) = modal(ctx, "choose", 560.0, |ui| {
                ui.heading("Choose a mod");
                ui.label(format!("{requested} is needed by {requester}. More than one mod provides it; pick one."));
                if let Some(h) = &help {
                    ui.label(RichText::new(h).italics());
                }
                ui.add_space(6.0);
                egui::ScrollArea::vertical().max_height(super::dialogs::fit_h(ui.ctx(), 320.0)).show(ui, |ui| {
                    for (i, m) in choices.iter().enumerate() {
                        let dl = index.as_ref().map_or(0, |x| x.download_count(&m.identifier));
                        ui.radio_value(
                            &mut pick,
                            i,
                            format!("{} ({}) {}, {} downloads", m.display_name(), m.identifier, m.version, super::table::fmt_count(dl)),
                        );
                        if !m.abstract_.is_empty() {
                            ui.indent(i, |ui| ui.weak(m.abstract_.as_str()));
                        }
                    }
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let ok = ui.button("Continue").clicked();
                    let cancel = ui.button("Cancel").clicked();
                    (ok, cancel)
                })
                .inner
            });
            let (ok, cancel) = act;
            if ok {
                if let Some(m) = choices.get(pick) {
                    let mut req = req;
                    req.choices.insert(requested.clone(), m.identifier.clone());
                    app.pending.choices.insert(requested, m.identifier.clone());
                    app.start_plan(req, mode);
                }
                Flow::Idle
            } else if cancel || esc {
                app.status = "Cancelled".into();
                Flow::Idle
            } else {
                Flow::Choose { req, mode, requester, requested, choices, help, pick }
            }
        }
        Flow::Recommend { req, cs, mut items } => {
            let mut result: Option<bool> = None;
            let (_, esc) = modal(ctx, "recommend", 680.0, |ui| {
                ui.heading("Recommended mods");
                ui.add(
                    egui::Label::new("The mods you install recommend, suggest or support these mods. Tick the ones to add.")
                        .wrap(),
                );
                ui.horizontal(|ui| {
                    if ui.button("Check all").clicked() {
                        items.iter_mut().for_each(|(_, c)| *c = true);
                    }
                    if ui.button("Uncheck all").clicked() {
                        items.iter_mut().for_each(|(_, c)| *c = false);
                    }
                    if ui.button("Check recommendations").clicked() {
                        items.iter_mut().for_each(|(m, c)| *c = m.kind == OptionalKind::Recommends);
                    }
                });
                ui.separator();
                egui::ScrollArea::vertical().max_height(super::dialogs::fit_h(ui.ctx(), 360.0)).auto_shrink([false, true]).show(ui, |ui| {
                    for (k, (om, on)) in items.iter_mut().enumerate() {
                        ui.push_id(k, |ui| {
                            ui.checkbox(on, RichText::new(format!("{} {}", om.module.display_name(), om.module.version)).strong());
                            let what = match om.kind {
                                OptionalKind::Recommends => "Recommended by",
                                OptionalKind::Suggests => "Suggested by",
                                OptionalKind::Supports => "Supports",
                            };
                            ui.indent("why", |ui| {
                                ui.add(egui::Label::new(RichText::new(format!("{what} {}", om.sources.join(", "))).small().weak()).truncate());
                                if !om.module.abstract_.is_empty() {
                                    ui.add(egui::Label::new(RichText::new(om.module.abstract_.as_str()).small()).truncate());
                                }
                            });
                        });
                        ui.add_space(2.0);
                    }
                });
                ui.separator();
                ui.horizontal(|ui| {
                    let n = items.iter().filter(|(_, c)| *c).count();
                    if ui.button(RichText::new(format!("Continue ({n} added)")).strong()).clicked() {
                        result = Some(true);
                    }
                    if ui.button("Cancel").clicked() {
                        result = Some(false);
                    }
                });
            });
            if esc && result.is_none() {
                result = Some(false);
            }
            match result {
                Some(true) => {
                    let picks: Vec<Arc<Module>> = items.iter().filter(|(_, c)| *c).map(|(m, _)| Arc::clone(&m.module)).collect();
                    if picks.is_empty() {
                        if cs.is_empty() { Flow::Idle } else { Flow::Confirm { cs } }
                    } else {
                        let mut req = req;
                        model::add_optional(&mut req, &picks);
                        app.start_plan(req, PlanMode::ApplyNoRecs);
                        Flow::Idle
                    }
                }
                Some(false) => Flow::Idle,
                None => Flow::Recommend { req, cs, items },
            }
        }
        Flow::Confirm { cs } => {
            let lines = model::change_lines(&cs, app.hide_epochs(), app.hide_v());
            let size: u64 = cs.downloads().iter().map(|m| m.download_size).sum();
            let playing = app.play.running.load(Ordering::Relaxed);
            let ((ok, cancel), esc) = modal(ctx, "confirm", 640.0, |ui| {
                ui.heading("Apply these changes?");
                let count = |k: ChangeKind| lines.iter().filter(|l| l.0 == k).count();
                ui.label(format!(
                    "{} to install, {} to update, {} to replace, {} to remove. Download about {}.",
                    count(ChangeKind::Install),
                    count(ChangeKind::Upgrade),
                    count(ChangeKind::Replace),
                    count(ChangeKind::Remove),
                    crate::util::human_size(size)
                ));
                if playing {
                    ui.colored_label(RED, "The game is running. Close it first.");
                }
                ui.add_space(6.0);
                egui::ScrollArea::both().max_height(super::dialogs::fit_h(ui.ctx(), 340.0)).show(ui, |ui| change_grid(ui, &lines));
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let ok = ui.add_enabled(!playing, egui::Button::new(RichText::new("Apply").strong())).clicked();
                    let cancel = ui.button("Cancel").clicked();
                    (ok, cancel)
                })
                .inner
            });
            if ok {
                app.start_apply(cs, Vec::new());
                Flow::Idle
            } else if cancel || esc {
                Flow::Idle
            } else {
                Flow::Confirm { cs }
            }
        }
        Flow::Conflict { cs, msg, files } => {
            let ((retry, cancel), esc) = modal(ctx, "conflict", 620.0, |ui| {
                ui.heading("File conflict");
                if files.is_empty() {
                    // The file list below repeats the message, so it is shown only here.
                    ui.label(RichText::new(&msg).monospace());
                    ui.label("Hangar cannot move these files for you. Remove them by hand, then apply again.");
                } else {
                    ui.add_space(4.0);
                    ui.label("These files are in the way and no mod owns them:");
                    egui::ScrollArea::vertical().max_height(super::dialogs::fit_h(ui.ctx(), 200.0)).show(ui, |ui| {
                        for f in &files {
                            ui.label(RichText::new(f).monospace().small());
                        }
                    });
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let retry = ui.add_enabled(!files.is_empty(), egui::Button::new("Move to Trash and retry")).clicked();
                    let cancel = ui.button("Cancel").clicked();
                    (retry, cancel)
                })
                .inner
            });
            if retry {
                app.start_apply(cs, files);
                Flow::Idle
            } else if cancel || esc {
                Flow::Idle
            } else {
                Flow::Conflict { cs, msg, files }
            }
        }
        Flow::Report { title, ok, lines, log } => {
            let (close, esc) = modal(ctx, "report", 600.0, |ui| {
                let t = RichText::new(&title).heading();
                ui.label(if ok { t } else { t.color(RED) });
                for l in &lines {
                    ui.label(l);
                }
                if !log.is_empty() {
                    egui::CollapsingHeader::new(format!("Log ({} lines)", log.len())).show(ui, |ui| {
                        egui::ScrollArea::vertical().max_height(super::dialogs::fit_h(ui.ctx(), 260.0)).stick_to_bottom(true).show(ui, |ui| {
                            for l in &log {
                                ui.label(RichText::new(l).monospace().small());
                            }
                        });
                    });
                }
                ui.add_space(6.0);
                ui.button("OK").clicked()
            });
            if close || esc { Flow::Idle } else { Flow::Report { title, ok, lines, log } }
        }
    };
    // A branch may have started a new flow through app methods; keep the newer one.
    if matches!(app.flow, Flow::Idle) {
        app.flow = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_text_matches_undo_result() {
        let clean = failure_lines(&Error::Io("disk full".into()));
        assert_eq!(clean.get(1).map(String::as_str), Some("No files were changed; the registry was restored."));
        let bad = failure_lines(&Error::Io("disk full\nUndoing the file changes also failed: busy".into()));
        assert!(bad.get(1).is_some_and(|l| l.starts_with("Some files could not be put back")));
    }
}
