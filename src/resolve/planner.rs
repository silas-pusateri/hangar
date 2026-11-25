//! `plan()`: CKAN RelationshipResolver (phase B, depth-first) plus removal (reverse
//! dependencies, auto-installed orphans), upgrade, replace, and install ordering.
//!
//! Phase A (the feasibility tree) is replaced by a candidate filter: a provider is kept only if
//! it does not clash with the planned or installed modules and its depends can be met
//! (CompatibleModules rule).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::meta::{Module, ModuleVersion, Relationship, RelationshipTarget};

use super::compat::Compat;
use super::world::{
    MRef, ModSet, dll_set, installed_set, latest_ok, problems, provider_ids, providers_for, satisfied, why_none,
    within,
};
use super::{
    Change, ChangeKind, Changeset, Context, InstallRequest, Reason, Request, ResolveError, ResolverOptions,
};

/// Optional-relationship flags carried down the resolution (RelationshipResolverOptions).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Opts {
    pub rec: bool,
    pub sug: bool,
    pub all_sug: bool,
}

impl Opts {
    pub fn of(o: &ResolverOptions) -> Self {
        Opts { rec: o.with_recommends, sug: o.with_suggests, all_sug: o.with_all_suggests }
    }
    pub fn depends_only() -> Self {
        Opts { rec: false, sug: false, all_sug: false }
    }
    fn without_suggestions(self) -> Self {
        Opts { sug: false, ..self }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum IReason {
    User,
    Depends(String),
    Recommended(String),
    Suggested(String),
    Replaces(String),
}

impl IReason {
    fn public(&self) -> Reason {
        match self {
            IReason::User => Reason::UserRequested,
            IReason::Depends(p) => Reason::Depends(p.clone()),
            IReason::Recommended(p) => Reason::Recommended(p.clone()),
            IReason::Suggested(p) => Reason::Suggested(p.clone()),
            IReason::Replaces(p) => Reason::Replaces(p.clone()),
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Stanza {
    Depends,
    Recommends,
    Suggests,
}

struct Target {
    module: Arc<Module>,
    reason: IReason,
    opts: Opts,
}

/// The installed record that a planned module takes over.
struct Old {
    version: ModuleVersion,
    auto: bool,
    replaces: Option<String>,
}

struct Planner<'a> {
    ctx: Context<'a>,
    choices: &'a HashMap<String, String>,
    installed: ModSet<'a>,
    dlls: HashSet<String>,
    planned: ModSet<'a>,
    reasons: HashMap<String, Vec<IReason>>,
    resolved: HashSet<String>,
    old: HashMap<String, Old>,
    unsatisfied: Vec<String>,
    conflicts: Vec<String>,
    stack: Vec<String>,
    compat: Compat<'a>,
}

pub(crate) fn plan(ctx: Context, req: &Request, opts: &ResolverOptions) -> Result<Changeset, ResolveError> {
    let p = Planner {
        ctx,
        choices: &req.choices,
        installed: ModSet::new(),
        dlls: HashSet::new(),
        planned: ModSet::new(),
        reasons: HashMap::new(),
        resolved: HashSet::new(),
        old: HashMap::new(),
        unsatisfied: Vec::new(),
        conflicts: Vec::new(),
        stack: Vec::new(),
        compat: Compat::new(ctx),
    };
    p.run(req, opts)
}

fn dedup(v: &mut Vec<String>) {
    let mut seen = HashSet::new();
    v.retain(|s| seen.insert(s.clone()));
}

/// Order TooManyProvides choices like CKAN's CLI prompt: downloads desc, the requested name
/// itself first, then identifier.
fn sort_choices(ctx: &Context, requested: &str, v: &mut [Arc<Module>]) {
    v.sort_by(|a, b| {
        ctx.index
            .download_count(&b.identifier)
            .cmp(&ctx.index.download_count(&a.identifier))
            .then_with(|| (b.identifier == requested).cmp(&(a.identifier == requested)))
            .then_with(|| a.identifier.cmp(&b.identifier))
    });
}

fn tmp(ctx: &Context, requester: &str, requested: &str, mut choices: Vec<Arc<Module>>, help: Option<&str>) -> ResolveError {
    sort_choices(ctx, requested, &mut choices);
    ResolveError::TooManyProvides {
        requester: requester.to_string(),
        requested: requested.to_string(),
        choices,
        help: help.map(String::from),
    }
}

impl<'a> Planner<'a> {
    fn run(mut self, req: &Request, opts: &ResolverOptions) -> Result<Changeset, ResolveError> {
        let ctx = self.ctx;
        let orig = installed_set(&ctx);
        let all_dlls = dll_set(&ctx);
        let user_opts = Opts::of(opts);

        // Requested removals.
        let mut remove0: Vec<String> = Vec::new();
        for id in &req.remove {
            match ctx.registry.installed(id) {
                None => return Err(ResolveError::NotInstalled(id.clone())),
                Some(im) if im.source_module.is_dlc() => return Err(ResolveError::IsDlc(id.clone())),
                Some(_) => remove0.push(id.clone()),
            }
        }
        dedup(&mut remove0);

        // Targets: upgrades, replacements, installs (in this order).
        let mut targets: Vec<Target> = Vec::new();
        let mut up_ids: Vec<String> = Vec::new();
        for id in &req.upgrade {
            match ctx.registry.installed(id) {
                None => return Err(ResolveError::NotInstalled(id.clone())),
                Some(im) if im.source_module.is_dlc() => return Err(ResolveError::IsDlc(id.clone())),
                Some(_) => up_ids.push(id.clone()),
            }
        }
        if req.upgrade_all {
            up_ids.extend(ctx.registry.installed_mods().map(|im| im.source_module.identifier.clone()));
        }
        dedup(&mut up_ids);
        up_ids.retain(|id| !ctx.held.contains(id));
        let mut ups = upgrade_targets(&ctx, &orig, &up_ids);
        if !req.upgrade_all && ups.len() < up_ids.len() {
            // A named mod whose update needs other upgrades (a newer dependency, a pinned
            // partner): take its target from the full solution, as the GUI list shows it.
            // The resolver then pulls the needed dependency upgrades in.
            let all: Vec<String> = ctx
                .registry
                .installed_mods()
                .map(|im| im.source_module.identifier.clone())
                .filter(|id| !ctx.held.contains(id))
                .collect();
            let have: HashSet<String> = ups.iter().map(|m| m.identifier.clone()).collect();
            for m in upgrade_targets(&ctx, &orig, &all) {
                if up_ids.contains(&m.identifier) && !have.contains(&m.identifier) {
                    ups.push(m);
                }
            }
        }
        for m in ups {
            targets.push(Target { module: m, reason: IReason::User, opts: Opts::depends_only() });
        }

        let mut replaced: Vec<(String, String)> = Vec::new();
        for id in &req.replace {
            if replaced.iter().any(|(o, _)| o == id) {
                continue;
            }
            match ctx.registry.installed(id) {
                None => return Err(ResolveError::NotInstalled(id.clone())),
                Some(im) if im.source_module.is_dlc() => return Err(ResolveError::IsDlc(id.clone())),
                Some(_) => {}
            }
            let r = super::replacement(&ctx, id)
                .ok_or_else(|| ResolveError::NotFound(format!("compatible replacement for {id}")))?;
            replaced.push((id.clone(), r.identifier.clone()));
            targets.push(Target { module: r, reason: IReason::Replaces(id.clone()), opts: user_opts });
        }

        for r in &req.install {
            if let Some(m) = self.install_target(r, opts, &orig, &all_dlls)? {
                targets.push(Target { module: m, reason: IReason::User, opts: user_opts });
            }
        }
        {
            let mut seen = HashSet::new();
            targets.retain(|t| seen.insert(t.module.identifier.clone()));
        }
        let target_ids: HashSet<String> = targets.iter().map(|t| t.module.identifier.clone()).collect();

        // DLLs that a managed module is about to replace no longer count.
        let mut dlls = all_dlls.clone();
        for id in &target_ids {
            dlls.remove(id);
        }

        // Reverse dependencies of the removals, against the state after the requested changes.
        let prebroken = unmet_ids(&orig, &all_dlls);
        let mut hyp = orig.clone();
        for (old, _) in &replaced {
            hyp.remove(old);
        }
        for t in &targets {
            hyp.insert(MRef::Shared(t.module.clone()));
        }
        let mut removals = removal_closure(&mut hyp, &remove0, &dlls, &prebroken, &target_ids);

        // Installed state the resolver works against.
        self.installed = orig.clone();
        self.dlls = dlls;
        for (id, _) in &removals {
            self.installed.remove(id);
        }
        for (old_id, new_id) in &replaced {
            if let Some(im) = ctx.registry.installed(old_id) {
                self.installed.remove(old_id);
                // Several installed mods can share one replacement (real data: 7 mods are
                // replaced by NehemiahEngineeringOrbitalScience). The Replace change names one;
                // the others become plain removals so none is left behind.
                if self.old.contains_key(new_id) {
                    removals.push((old_id.clone(), Reason::UserRequested));
                    continue;
                }
                // CKAN keeps only the flag of a removed record with the same identifier, so a
                // replacement is user-installed unless it was already installed as auto.
                let auto = ctx.registry.installed(new_id).is_some_and(|r| r.auto_installed);
                self.old.insert(
                    new_id.clone(),
                    Old { version: im.source_module.version.clone(), auto, replaces: Some(old_id.clone()) },
                );
            }
            // An installed copy of the replacement is removed and installed fresh.
            if new_id != old_id && self.installed.remove(new_id).is_some() {
                removals.push((new_id.clone(), Reason::UserRequested));
            }
        }

        // Add all user-requested modules first, then resolve each (RR:89-139).
        // All targets go in first, so the old installed versions of other targets (upgrades)
        // do not count as conflicts (CKAN passes them in toRemove).
        for t in &targets {
            self.add(t.module.clone(), t.reason.clone());
        }
        let mut earlier = ModSet::new();
        for t in &targets {
            let hit = earlier.conflicts_with(&t.module).or_else(|| self.installed.conflicts_with(&t.module));
            if let Some(x) = hit {
                self.conflicts.push(format!("{} conflicts with {}", t.module, x));
            }
            earlier.insert(MRef::Shared(t.module.clone()));
        }
        for t in &targets {
            self.resolve(&t.module, t.opts, None)?;
        }

        if !self.unsatisfied.is_empty() {
            dedup(&mut self.unsatisfied);
            return Err(ResolveError::Unsatisfied(self.unsatisfied));
        }
        if !self.conflicts.is_empty() {
            dedup(&mut self.conflicts);
            return Err(ResolveError::Conflicts(self.conflicts));
        }

        // Auto-installed modules nobody needs any more.
        let sweep = !remove0.is_empty() || !replaced.is_empty() || !self.old.is_empty();
        if sweep {
            let before: HashSet<String> = find_orphans(&ctx, &ModSet::new(), &orig).into_iter().collect();
            for id in find_orphans(&ctx, &self.planned, &self.installed) {
                if before.contains(&id) {
                    continue; // already unneeded before this change: leave it alone
                }
                self.installed.remove(&id);
                removals.push((id, Reason::NoLongerNeeded));
            }
        }

        // Final consistency check (SanityChecker), ignoring problems that existed before.
        let before: HashSet<String> = problems(&orig, &[], &all_dlls).into_iter().map(|p| p.text).collect();
        let mut unmet = Vec::new();
        let mut clash = Vec::new();
        for p in problems(&self.installed, &[&self.planned], &self.dlls) {
            if !before.contains(&p.text) {
                if p.conflict { clash.push(p.text) } else { unmet.push(p.text) }
            }
        }
        if !unmet.is_empty() {
            dedup(&mut unmet);
            return Err(ResolveError::Unsatisfied(unmet));
        }
        if !clash.is_empty() {
            dedup(&mut clash);
            return Err(ResolveError::Conflicts(clash));
        }

        Ok(self.changeset(removals))
    }

    /// Pick the module for one install request. None = nothing to do (already installed).
    fn install_target(
        &self,
        r: &InstallRequest,
        opts: &ResolverOptions,
        orig: &ModSet,
        dlls: &HashSet<String>,
    ) -> Result<Option<Arc<Module>>, ResolveError> {
        let ctx = &self.ctx;
        let check = |m: &Arc<Module>| -> Result<Option<Arc<Module>>, ResolveError> {
            if m.is_dlc() {
                return Err(ResolveError::IsDlc(m.identifier.clone()));
            }
            if !opts.allow_incompatible && !m.is_compatible(ctx.criteria) {
                return Err(ResolveError::Incompatible(m.to_string()));
            }
            if let Some(im) = ctx.registry.installed(&m.identifier) {
                if im.source_module.is_dlc() {
                    return Err(ResolveError::IsDlc(m.identifier.clone()));
                }
                if im.source_module.version == m.version {
                    return Ok(None);
                }
            }
            Ok(Some(m.clone()))
        };
        match r {
            InstallRequest::Exact(id, v) => {
                let m = ctx.index.get(id, v).ok_or_else(|| ResolveError::NotFound(format!("{id}={v}")))?;
                check(m)
            }
            InstallRequest::Module(m) => check(m),
            InstallRequest::Latest(name) => {
                if let Some(im) = ctx.registry.installed(name) {
                    if im.source_module.is_dlc() {
                        return Err(ResolveError::IsDlc(name.clone()));
                    }
                    return Ok(None);
                }
                let versions = ctx.index.versions(name);
                if !versions.is_empty() {
                    if versions.iter().all(|m| m.is_dlc()) {
                        return Err(ResolveError::IsDlc(name.clone()));
                    }
                    let accept = |m: &Module| !m.is_dlc();
                    let pick = latest_ok(ctx, name, &accept, false, &[orig])
                        .or_else(|| latest_ok(ctx, name, &accept, false, &[]))
                        .or_else(|| if opts.allow_incompatible { latest_ok(ctx, name, &accept, true, &[]) } else { None });
                    return pick.map(|m| Some(m.clone())).ok_or_else(|| ResolveError::Incompatible(name.clone()));
                }
                // A virtual name: pick one of its providers.
                let rel = Relationship::One(RelationshipTarget::simple(name));
                if satisfied(&rel, &[orig], dlls) {
                    return Ok(None);
                }
                if provider_ids(ctx, name).is_empty() {
                    return Err(ResolveError::NotFound(name.clone()));
                }
                let mut c = providers_for(ctx, &rel, &[orig]);
                if c.is_empty() {
                    c = providers_for(ctx, &rel, &[]);
                }
                if c.is_empty() {
                    return Err(ResolveError::Incompatible(name.clone()));
                }
                if c.len() == 1 {
                    return Ok(c.pop());
                }
                if let Some(id) = self.choices.get(name) {
                    return match c.iter().find(|m| m.identifier == *id) {
                        Some(m) => Ok(Some(m.clone())),
                        None => Err(ResolveError::NotFound(format!("{id} (chosen for {name})"))),
                    };
                }
                Err(tmp(ctx, "user request", name, c, None))
            }
        }
    }

    fn add_reason(&mut self, id: &str, r: IReason) {
        let v = self.reasons.entry(id.to_string()).or_default();
        if !v.contains(&r) {
            v.push(r);
        }
    }

    /// RR.Add: put a module into the change set. An installed copy with the same identifier
    /// becomes an upgrade.
    fn add(&mut self, m: Arc<Module>, r: IReason) {
        let id = m.identifier.clone();
        if let Some(prev) = self.installed.get(&id)
            && !prev.is_dlc()
        {
            let auto = self.ctx.registry.installed(&id).is_some_and(|im| im.auto_installed);
            let version = prev.version.clone();
            self.old.entry(id.clone()).or_insert(Old { version, auto, replaces: None });
            self.installed.remove(&id);
        }
        self.planned.insert(MRef::Shared(m));
        self.add_reason(&id, r);
    }

    fn resolve(&mut self, m: &Arc<Module>, opts: Opts, old: Option<&[Relationship]>) -> Result<(), ResolveError> {
        if !self.resolved.insert(format!("{}\u{0}{}", m.identifier, m.version)) {
            return Ok(());
        }
        self.stack.push(m.to_string());
        let r = self.resolve_inner(m, opts, old);
        self.stack.pop();
        r
    }

    fn resolve_inner(&mut self, m: &Arc<Module>, opts: Opts, old: Option<&[Relationship]>) -> Result<(), ResolveError> {
        self.stanza(m, &m.depends, Stanza::Depends, opts, old)?;
        if opts.rec {
            self.stanza(m, &m.recommends, Stanza::Recommends, opts.without_suggestions(), old)?;
        }
        if opts.sug || opts.all_sug {
            self.stanza(m, &m.suggests, Stanza::Suggests, opts.without_suggestions(), old)?;
        }
        Ok(())
    }

    fn unsat(&mut self, d: &Relationship, why: &str) {
        let chain = self.stack.join(" -> ");
        self.unsatisfied.push(format!("{chain} depends on {d} ({why})"));
    }

    fn stanza(
        &mut self,
        parent: &Arc<Module>,
        rels: &[Relationship],
        kind: Stanza,
        opts: Opts,
        old: Option<&[Relationship]>,
    ) -> Result<(), ResolveError> {
        let soft = kind != Stanza::Depends;
        for d in rels {
            let pid = parent.identifier.clone();
            let reason = match kind {
                Stanza::Depends => IReason::Depends(pid),
                Stanza::Recommends => IReason::Recommended(pid),
                Stanza::Suggests => IReason::Suggested(pid),
            };
            let o = if d.suppress_recommendations() { Opts::depends_only() } else { opts };

            // 1. Already in the change set.
            if let Some(c) = self.planned.find(d).map(MRef::to_arc) {
                self.add_reason(&c.identifier, reason);
                self.resolve(&c, o, Some(rels))?;
                continue;
            }
            // 2. The change set has the named module at another version.
            if d.contains_any(&|n| self.planned.contains_name(n)) {
                if !soft {
                    self.conflicts.push(format!("{parent} depends on {d}, but another version is in the change set"));
                }
                continue;
            }
            // 3. Installed module, DLL or DLC.
            if satisfied(d, &[&self.installed], &self.dlls) {
                continue;
            }
            // 4. Candidates from the repositories.
            let mut cands = match self.candidates(d) {
                Ok(c) => c,
                Err(why) => {
                    if !soft {
                        self.unsat(d, &why);
                    }
                    continue;
                }
            };
            // 5. More than one provider.
            let c = if cands.len() > 1 {
                match self.pick(parent, d, cands, old)? {
                    Some(c) => c,
                    None => continue,
                }
            } else {
                match cands.pop() {
                    Some(c) => c,
                    None => continue,
                }
            };
            // 6. Conflict check against the fixed set.
            let hit = self.planned.conflicts_with(&c).or_else(|| self.installed.conflicts_with(&c));
            if let Some(x) = hit {
                if !soft {
                    self.conflicts.push(format!("{} conflicts with {c} (needed by {parent})", x));
                }
                continue;
            }
            self.add(c.clone(), reason);
            self.resolve(&c, o, Some(rels))?;
        }
        Ok(())
    }

    /// Providers that can be used for `d` now (tree.candidates). Err = why there is none.
    fn candidates(&mut self, d: &Relationship) -> Result<Vec<Arc<Module>>, String> {
        let ctx = self.ctx;
        let provs = providers_for(&ctx, d, &[&self.installed, &self.planned]);
        if provs.is_empty() {
            let loose = providers_for(&ctx, d, &[]);
            if loose.is_empty() {
                return Err(why_none(&ctx, d));
            }
            let why: Vec<String> = loose
                .iter()
                .filter_map(|m| self.installed.bad_relationship(m).or_else(|| self.planned.bad_relationship(m)))
                .collect();
            return Err(if why.is_empty() { "no usable version".into() } else { why.join("; ") });
        }
        let mut ok = Vec::new();
        let mut rejected = Vec::new();
        for p in provs {
            if let Some(x) = self.planned.get(&p.identifier)
                && x.version != p.version
            {
                rejected.push(format!("{x} is already in the change set"));
                continue;
            }
            if let Some(x) = self.installed.get(&p.identifier)
                && ctx.held.contains(&p.identifier)
            {
                rejected.push(format!("{x} is held"));
                continue;
            }
            let hit = self.planned.conflicts_with(&p).or_else(|| self.installed.conflicts_with(&p));
            if let Some(x) = hit {
                rejected.push(format!("{p} conflicts with {}", x));
                continue;
            }
            let sets = [&self.planned, &self.installed];
            if !self.compat.module_ok(&p, &sets, &self.dlls) {
                let what = self.compat.first_unmet(&p, &sets, &self.dlls).unwrap_or_default();
                rejected.push(format!("{p} depends on {what}, which cannot be installed"));
                continue;
            }
            ok.push(p);
        }
        if ok.is_empty() { Err(rejected.join("; ")) } else { Ok(ok) }
    }

    /// Choose among several providers: the user's answer, else the parent's stanza, else ask.
    fn pick(
        &mut self,
        parent: &Arc<Module>,
        d: &Relationship,
        cands: Vec<Arc<Module>>,
        old: Option<&[Relationship]>,
    ) -> Result<Option<Arc<Module>>, ResolveError> {
        let key = d.to_string();
        let chosen = self.choices.get(&key).or_else(|| match d {
            Relationship::One(t) => self.choices.get(&t.name),
            Relationship::AnyOf { .. } => None,
        });
        if let Some(id) = chosen {
            if let Some(c) = cands.iter().find(|c| c.identifier == *id) {
                return Ok(Some(c.clone()));
            }
            let accept = |m: &Module| within(d, m) && !m.is_dlc();
            if let Some(m) = latest_ok(&self.ctx, id, &accept, false, &[&self.installed, &self.planned]) {
                return Ok(Some(m.clone()));
            }
            let why = format!("chosen module {id} cannot satisfy it");
            self.unsat(d, &why);
            return Ok(None);
        }
        if let Some(old) = old {
            let mut hits = cands.iter().filter(|c| old.iter().any(|r| within(r, c)));
            if let (Some(one), None) = (hits.next(), hits.next()) {
                return Ok(Some(one.clone()));
            }
        }
        let requested = match d {
            Relationship::One(t) => t.name.clone(),
            Relationship::AnyOf { .. } => key,
        };
        Err(tmp(&self.ctx, &parent.identifier, &requested, cands, d.choice_help_text()))
    }

    /// CKAN IsAutoInstalled: every reason is Depends with a non-metapackage parent.
    fn is_auto(&self, id: &str) -> bool {
        let Some(rs) = self.reasons.get(id) else { return false };
        !rs.is_empty()
            && rs.iter().all(|r| match r {
                IReason::Depends(p) => {
                    let pm = self.planned.get(p).or_else(|| self.installed.get(p));
                    pm.is_none_or(|m| !m.is_metapackage())
                }
                _ => false,
            })
    }

    /// Planned identifiers with dependencies before dependers. Depends edges are hard, except
    /// inside a depends cycle (strongly connected component), where they are ignored.
    /// Recommends/suggests edges are soft (they often form cycles): among the ready modules,
    /// one with no pending soft edges goes first, then by identifier.
    fn install_order(&self) -> Vec<String> {
        let ids: Vec<&str> = self.planned.ids().map(String::as_str).collect();
        let pos: HashMap<&str, usize> = ids.iter().enumerate().map(|(i, id)| (*id, i)).collect();
        let n = ids.len();
        let mut hard: Vec<Vec<usize>> = vec![Vec::new(); n];
        let mut soft: Vec<Vec<usize>> = vec![Vec::new(); n];
        for (ci, &c) in ids.iter().enumerate() {
            for r in self.reasons.get(c).into_iter().flatten() {
                let (edges, p) = match r {
                    IReason::Depends(p) => (&mut hard, p),
                    IReason::Recommended(p) | IReason::Suggested(p) => (&mut soft, p),
                    _ => continue,
                };
                if let Some(&pi) = pos.get(p.as_str())
                    && pi != ci
                    && let Some(e) = edges.get_mut(pi)
                {
                    e.push(ci);
                }
            }
        }
        let comp = scc(&hard);
        let comp_of = |i: usize| comp.get(i).copied().unwrap_or(i);
        let mut done = vec![false; n];
        let blocked = |edges: &[Vec<usize>], i: usize, done: &[bool], same_comp_ok: bool| -> bool {
            edges.get(i).is_some_and(|e| {
                e.iter().any(|&d| !done.get(d).copied().unwrap_or(true) && !(same_comp_ok && comp_of(d) == comp_of(i)))
            })
        };
        let mut out: Vec<String> = Vec::with_capacity(n);
        while out.len() < n {
            let next = (0..n)
                .filter(|&i| !done.get(i).copied().unwrap_or(true) && !blocked(&hard, i, &done, true))
                .min_by(|&a, &b| {
                    blocked(&soft, a, &done, false)
                        .cmp(&blocked(&soft, b, &done, false))
                        .then_with(|| ids.get(a).cmp(&ids.get(b)))
                });
            let Some(i) = next else { break };
            if let Some(d) = done.get_mut(i) {
                *d = true;
            }
            if let Some(id) = ids.get(i) {
                out.push(id.to_string());
            }
        }
        // Never drop a planned module, even if the order above got stuck.
        for (i, id) in ids.iter().enumerate() {
            if !done.get(i).copied().unwrap_or(true) {
                out.push(id.to_string());
            }
        }
        out
    }

    fn changeset(&self, removals: Vec<(String, Reason)>) -> Changeset {
        let reg = self.ctx.registry;
        let mut changes = Vec::new();
        let mut seen = HashSet::new();
        for (id, reason) in removals {
            if !seen.insert(id.clone()) {
                continue;
            }
            if let Some(im) = reg.installed(&id) {
                changes.push(Change {
                    kind: ChangeKind::Remove,
                    module: Arc::new(im.source_module.clone()),
                    old_version: None,
                    replaces: None,
                    reasons: vec![reason],
                    auto_installed: false,
                });
            }
        }
        for id in self.install_order() {
            let Some(m) = self.planned.get(&id) else { continue };
            let reasons: Vec<Reason> = self.reasons.get(&id).into_iter().flatten().map(IReason::public).collect();
            let (kind, old_version, replaces, auto) = match self.old.get(&id) {
                Some(Old { version, auto, replaces: Some(r) }) => {
                    (ChangeKind::Replace, Some(version.clone()), Some(r.clone()), *auto)
                }
                Some(Old { version, auto, replaces: None }) => (ChangeKind::Upgrade, Some(version.clone()), None, *auto),
                None => (ChangeKind::Install, None, None, self.is_auto(&id)),
            };
            changes.push(Change { kind, module: m.to_arc(), old_version, replaces, reasons, auto_installed: auto });
        }
        Changeset { changes }
    }
}

/// Strongly connected components (Tarjan, iterative): component id per node.
fn scc(edges: &[Vec<usize>]) -> Vec<usize> {
    let n = edges.len();
    let mut index = vec![usize::MAX; n];
    let mut low = vec![0usize; n];
    let mut on_stack = vec![false; n];
    let mut comp = vec![usize::MAX; n];
    let mut stack: Vec<usize> = Vec::new();
    let mut next = 0usize;
    for root in 0..n {
        if index[root] != usize::MAX {
            continue;
        }
        // (node, next edge position)
        let mut work: Vec<(usize, usize)> = vec![(root, 0)];
        index[root] = next;
        low[root] = next;
        next += 1;
        stack.push(root);
        on_stack[root] = true;
        while let Some(&mut (v, ref mut ei)) = work.last_mut() {
            let succ = edges.get(v).and_then(|e| e.get(*ei)).copied();
            if let Some(w) = succ {
                *ei += 1;
                if index[w] == usize::MAX {
                    index[w] = next;
                    low[w] = next;
                    next += 1;
                    stack.push(w);
                    on_stack[w] = true;
                    work.push((w, 0));
                } else if on_stack[w] {
                    low[v] = low[v].min(index[w]);
                }
                continue;
            }
            work.pop();
            if let Some(&(u, _)) = work.last() {
                low[u] = low[u].min(low[v]);
            }
            if low[v] == index[v] {
                while let Some(w) = stack.pop() {
                    on_stack[w] = false;
                    comp[w] = v;
                    if w == v {
                        break;
                    }
                }
            }
        }
    }
    comp
}

/// FindRemovableAutoInstalled: auto-installed modules in `installed` that nothing in
/// `planned` or the non-auto installed modules needs (only depends count; DLLs are not
/// considered, as in CKAN).
fn find_orphans(ctx: &Context, planned: &ModSet, installed: &ModSet) -> Vec<String> {
    let reg = ctx.registry;
    let mut keeping = planned.clone();
    let mut removable: Vec<MRef> = Vec::new();
    for m in installed.iter() {
        let auto = !m.is_dlc() && reg.installed(&m.identifier).is_some_and(|im| im.auto_installed);
        if auto { removable.push(m.clone()) } else { keeping.insert(m.clone()) }
    }
    if removable.is_empty() {
        return Vec::new();
    }
    let mut deps: Vec<Relationship> = Vec::new();
    for m in keeping.iter() {
        for d in &m.depends {
            if keeping.find(d).is_none() {
                deps.push(d.clone());
            }
        }
    }
    loop {
        let (needed, rest): (Vec<MRef>, Vec<MRef>) =
            removable.into_iter().partition(|r| deps.iter().any(|d| within(d, r)));
        removable = rest;
        if needed.is_empty() {
            break;
        }
        deps.retain(|d| !needed.iter().any(|n| within(d, n)));
        for n in &needed {
            keeping.insert(n.clone());
        }
        for n in &needed {
            for d in &n.depends {
                if keeping.find(d).is_none() {
                    deps.push(d.clone());
                }
            }
        }
    }
    let mut ids: Vec<String> = removable.iter().map(|m| m.identifier.clone()).collect();
    ids.sort();
    ids
}

/// FindReverseDependencies: remove `start` from `hyp`, then keep removing installed modules
/// whose depends are no longer met. Modules that were already broken before, and modules in
/// `skip` (being installed), are left alone. Returns (identifier, reason) in removal order.
pub(crate) fn removal_closure(
    hyp: &mut ModSet,
    start: &[String],
    dlls: &HashSet<String>,
    prebroken: &HashSet<String>,
    skip: &HashSet<String>,
) -> Vec<(String, Reason)> {
    let mut out: Vec<(String, Reason)> = Vec::new();
    let mut gone: HashSet<String> = HashSet::new();
    for id in start {
        hyp.remove(id);
        gone.insert(id.clone());
        out.push((id.clone(), Reason::UserRequested));
    }
    if start.is_empty() {
        return out;
    }
    loop {
        let mut newly: Vec<(String, String)> = Vec::new();
        for m in hyp.iter() {
            if m.is_dlc() || skip.contains(&m.identifier) || prebroken.contains(&m.identifier) {
                continue;
            }
            if let Some(d) = m.depends.iter().find(|d| !satisfied(d, &[hyp], dlls)) {
                let by = d
                    .names()
                    .into_iter()
                    .find(|n| gone.contains(*n))
                    .map(String::from)
                    .unwrap_or_else(|| d.to_string());
                newly.push((m.identifier.clone(), by));
            }
        }
        if newly.is_empty() {
            break;
        }
        for (id, by) in newly {
            hyp.remove(&id);
            gone.insert(id.clone());
            out.push((id, Reason::ReverseDepends(by)));
        }
    }
    out
}

/// Newest acceptable version of an installed module given the other modules in `set`
/// (HasUpdate with LatestAvailable's DependsAndConflictsOK check).
pub(crate) fn update_for(ctx: &Context, inst: &Module, set: &ModSet) -> Option<Arc<Module>> {
    if inst.is_dlc() {
        return None;
    }
    let latest = latest_ok(ctx, &inst.identifier, &|_| true, false, &[set])?;
    if latest.is_dlc() {
        return None;
    }
    match latest.version.cmp(&inst.version) {
        std::cmp::Ordering::Greater => Some(latest.clone()),
        std::cmp::Ordering::Equal if metadata_changed(inst, latest) => Some(latest.clone()),
        _ => None,
    }
}

/// Identifiers in `set` with a `depends` that is not met (pre-existing breakage). Conflicts do
/// not count here: a module that only clashes must still be removed when it loses a dependency.
pub(crate) fn unmet_ids(set: &ModSet, dlls: &HashSet<String>) -> HashSet<String> {
    problems(set, &[], dlls).into_iter().filter(|p| !p.conflict).map(|p| p.id).collect()
}

/// Upgrade targets for `ids` (held ones already removed), like the first solution of CKAN's
/// FindUpgradeabilitySolutions:
/// - candidates are the ids with an update when no other module is considered;
/// - each candidate's update is checked against the fixed modules (installed, not candidates)
///   plus the updates chosen so far, so pinned pairs can upgrade together;
/// - a candidate left without an update keeps its installed version; chosen updates that clash
///   with it are dropped and the search runs again (a stand-in for CKAN's backtracking).
pub(crate) fn upgrade_targets(ctx: &Context, orig: &ModSet, ids: &[String]) -> Vec<Arc<Module>> {
    let empty = ModSet::new();
    let cands: Vec<&str> = ids
        .iter()
        .map(String::as_str)
        .filter(|id| orig.get(id).is_some_and(|m| update_for(ctx, m, &empty).is_some()))
        .collect();
    let mut banned: HashSet<&str> = HashSet::new();
    // Each round bans at least one candidate, so this ends.
    for _round in 0..=cands.len() {
        let live: Vec<&str> = cands.iter().copied().filter(|id| !banned.contains(id)).collect();
        let mut partial = orig.clone();
        for id in &live {
            partial.remove(id);
        }
        let mut chosen: HashMap<&str, Arc<Module>> = HashMap::new();
        // Repeat while something changes: an update may only fit after another one.
        for _pass in 0..=live.len() {
            let mut changed = false;
            for id in &live {
                if chosen.contains_key(id) {
                    continue;
                }
                let Some(inst) = orig.get(id) else { continue };
                if let Some(m) = update_for(ctx, inst, &partial) {
                    partial.insert(MRef::Shared(m.clone()));
                    chosen.insert(id, m);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        // Candidates without an update stay at their installed version.
        let mut clash = false;
        for id in &live {
            if chosen.contains_key(id) {
                continue;
            }
            let Some(inst) = orig.get(id) else { continue };
            let mut one = ModSet::new();
            one.insert(inst.clone());
            for (cid, u) in &chosen {
                if one.bad_relationship(u).is_some() && banned.insert(cid) {
                    clash = true;
                }
            }
        }
        if !clash {
            return ids.iter().filter_map(|id| chosen.get(id.as_str()).cloned()).collect();
        }
    }
    Vec::new()
}

/// Relationship equality as in CKAN: name and version bounds only (choice_help_text and
/// suppress_recommendations are ignored); versions compare by value.
fn rel_eq(a: &Relationship, b: &Relationship) -> bool {
    match (a, b) {
        (Relationship::One(x), Relationship::One(y)) => target_eq(x, y),
        (Relationship::AnyOf { any_of: x, .. }, Relationship::AnyOf { any_of: y, .. }) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| rel_eq(p, q))
        }
        _ => false,
    }
}

fn target_eq(a: &RelationshipTarget, b: &RelationshipTarget) -> bool {
    a.name == b.name && a.version == b.version && a.min_version == b.min_version && a.max_version == b.max_version
}

/// CKAN RelationshipsAreEquivalent: same count, then pairwise equal after sorting by text.
fn rels_equivalent(a: &[Relationship], b: &[Relationship]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut x: Vec<(String, &Relationship)> = a.iter().map(|r| (r.to_string(), r)).collect();
    let mut y: Vec<(String, &Relationship)> = b.iter().map(|r| (r.to_string(), r)).collect();
    x.sort_by(|p, q| p.0.cmp(&q.0));
    y.sort_by(|p, q| p.0.cmp(&q.0));
    x.iter().zip(&y).all(|(p, q)| rel_eq(p.1, q.1))
}

/// CKAN MetadataChanged: the repo copy of the installed version differs in install stanzas,
/// install_size, download hash (sha256 and sha1 each checked when both sides have it),
/// conflicts/depends/recommends (as sets), replaced_by or provides.
pub(crate) fn metadata_changed(inst: &Module, repo: &Module) -> bool {
    if inst.install_stanzas() != repo.install_stanzas() || inst.install_size != repo.install_size {
        return true;
    }
    if let (Some(a), Some(b)) = (&inst.download_hash, &repo.download_hash) {
        let differs = |x: &Option<Box<str>>, y: &Option<Box<str>>| match (x, y) {
            (Some(x), Some(y)) => !x.eq_ignore_ascii_case(y),
            _ => false,
        };
        if differs(&a.sha256, &b.sha256) || differs(&a.sha1, &b.sha1) {
            return true;
        }
    }
    let rb_same = match (&inst.replaced_by, &repo.replaced_by) {
        (None, None) => true,
        (Some(x), Some(y)) => target_eq(x, y),
        _ => false,
    };
    if !rels_equivalent(&inst.conflicts, &repo.conflicts)
        || !rels_equivalent(&inst.depends, &repo.depends)
        || !rels_equivalent(&inst.recommends, &repo.recommends)
        || !rb_same
    {
        return true;
    }
    let mut a = inst.provides.to_vec();
    let mut b = repo.provides.to_vec();
    a.sort();
    b.sort();
    a != b
}
