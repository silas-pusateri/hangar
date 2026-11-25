//! Recommendations dialog (CKAN ModuleInstaller.FindRecommendations, spec section 12).

use std::collections::{HashMap, HashSet};

use crate::meta::Relationship;

use super::compat::Compat;
use super::world::{MRef, ModSet, dll_set, installed_set, latest_ok, providers_for, satisfied, within};
use super::{ChangeKind, Changeset, Context, OptionalKind, OptionalMod};

fn kind_rank(k: OptionalKind) -> u8 {
    match k {
        OptionalKind::Recommends => 0,
        OptionalKind::Suggests => 1,
        OptionalKind::Supports => 2,
    }
}

pub(crate) fn optional_mods(ctx: Context, cs: &Changeset) -> Vec<OptionalMod> {
    let mut installed = installed_set(&ctx);
    let dlls = dll_set(&ctx);
    let mut planned = ModSet::new();
    for c in &cs.changes {
        installed.remove(&c.module.identifier);
        if c.kind != ChangeKind::Remove {
            if let Some(r) = &c.replaces {
                installed.remove(r);
            }
            if !c.module.is_dlc() {
                planned.insert(MRef::Shared(c.module.clone()));
            }
        }
    }

    // Sources: what the change set installs, or every installed mod (audit).
    // A change set with only removals has no sources (it is not the audit case).
    let sources: Vec<MRef> = if !cs.is_empty() {
        planned.iter().cloned().collect()
    } else {
        installed.iter().filter(|m| !m.is_dlc()).cloned().collect()
    };

    // A depends with suppress_recommendations silences the target's recommendations.
    let suppress: Vec<&Relationship> = planned
        .iter()
        .chain(installed.iter())
        .flat_map(|m| m.depends.iter())
        .filter(|d| d.suppress_recommendations())
        .collect();
    let recommenders: Vec<&MRef> = sources.iter().filter(|m| !suppress.iter().any(|d| within(d, m))).collect();

    let mut compat = Compat::new(ctx);
    let mut out: Vec<OptionalMod> = Vec::new();
    let mut pos: HashMap<String, usize> = HashMap::new();
    let mut first_choice: HashSet<String> = HashSet::new();
    let sets = [&planned, &installed];

    for src in &recommenders {
        let stanzas = [(OptionalKind::Recommends, &src.recommends), (OptionalKind::Suggests, &src.suggests)];
        for (kind, rels) in stanzas {
            for d in rels.iter() {
                if satisfied(d, &sets, &dlls) {
                    continue;
                }
                // No clash filter here: a conflicting recommendation is shown, just not checked.
                let cands = providers_for(&ctx, d, &[]);
                for (i, c) in cands.iter().enumerate() {
                    let id = &c.identifier;
                    if planned.contains_id(id) || installed.contains_id(id) {
                        continue;
                    }
                    if kind == OptionalKind::Recommends && i == 0 {
                        first_choice.insert(id.clone());
                    }
                    if let Some(&p) = pos.get(id) {
                        let e = &mut out[p];
                        if !e.sources.contains(&src.identifier) {
                            e.sources.push(src.identifier.clone());
                        }
                        if kind == OptionalKind::Recommends {
                            e.kind = OptionalKind::Recommends;
                        }
                        continue;
                    }
                    if !compat.module_ok(c, &sets, &dlls) {
                        continue;
                    }
                    pos.insert(id.clone(), out.len());
                    out.push(OptionalMod {
                        module: c.clone(),
                        kind,
                        sources: vec![src.identifier.clone()],
                        checked: false,
                    });
                }
            }
        }
    }

    // Supporters: compatible modules whose `supports` names one of the sources.
    for (id, versions) in &ctx.index.modules {
        if versions.iter().all(|m| m.supports.is_empty()) || pos.contains_key(id) {
            continue;
        }
        if planned.contains_id(id) || installed.contains_id(id) {
            continue;
        }
        let Some(m) = latest_ok(&ctx, id, &|m| !m.is_dlc(), false, &[&installed]) else { continue };
        let srcs: Vec<String> = recommenders
            .iter()
            .filter(|s| m.supports.iter().any(|d| within(d, s)))
            .map(|s| s.identifier.clone())
            .collect();
        if srcs.is_empty() {
            continue;
        }
        if planned.conflicts_with(m).is_some() || installed.conflicts_with(m).is_some() {
            continue;
        }
        if !compat.module_ok(m, &sets, &dlls) {
            continue;
        }
        out.push(OptionalMod { module: m.clone(), kind: OptionalKind::Supports, sources: srcs, checked: false });
    }

    out.sort_by(|a, b| {
        kind_rank(a.kind)
            .cmp(&kind_rank(b.kind))
            .then_with(|| b.sources.len().cmp(&a.sources.len()))
            .then_with(|| a.module.identifier.cmp(&b.module.identifier))
    });

    // Pre-check first-choice recommendations that clash with nothing.
    let mut checked = ModSet::new();
    for e in &mut out {
        if e.kind != OptionalKind::Recommends || !first_choice.contains(&e.module.identifier) {
            continue;
        }
        let m = &e.module;
        if planned.conflicts_with(m).is_some()
            || installed.conflicts_with(m).is_some()
            || checked.conflicts_with(m).is_some()
        {
            continue;
        }
        e.checked = true;
        checked.insert(MRef::Shared(m.clone()));
    }
    out
}
