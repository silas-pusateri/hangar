//! Matching primitives for the resolver: a set of modules with lookup indexes, relationship
//! matching (WithinBounds / MatchesAny / ConflictsWith / BadRelationships), and repository
//! lookups (AvailableModule.Latest, LatestAvailableWithProvides).

use std::collections::{HashMap, HashSet};
use std::ops::Deref;
use std::sync::Arc;

use crate::meta::{Module, Relationship, RelationshipTarget};

use super::Context;

/// A module borrowed from the registry or shared from the index / a request.
#[derive(Clone, Debug)]
pub(crate) enum MRef<'a> {
    Borrowed(&'a Module),
    Shared(Arc<Module>),
}

impl Deref for MRef<'_> {
    type Target = Module;
    fn deref(&self) -> &Module {
        match self {
            MRef::Borrowed(m) => m,
            MRef::Shared(a) => a,
        }
    }
}

impl std::fmt::Display for MRef<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        (**self).fmt(f)
    }
}

impl MRef<'_> {
    pub fn to_arc(&self) -> Arc<Module> {
        match self {
            MRef::Shared(a) => a.clone(),
            MRef::Borrowed(m) => Arc::new((*m).clone()),
        }
    }
}

/// A DLC with an unknown version (registry writes UNKNOWN_DLC_VERSION) matches any bounds.
fn unknown_version(m: &Module) -> bool {
    let v = m.version.as_str();
    m.is_dlc() && (v.is_empty() || v == crate::registry::UNKNOWN_DLC_VERSION)
}

/// WithinBounds for one target. Provides never check the version.
pub(crate) fn within_target(t: &RelationshipTarget, m: &Module) -> bool {
    if m.identifier == t.name {
        unknown_version(m) || t.version_ok(&m.version)
    } else {
        m.provides.contains(&t.name)
    }
}

/// WithinBounds for a descriptor (any_of: any child).
pub(crate) fn within(rel: &Relationship, m: &Module) -> bool {
    match rel {
        Relationship::One(t) => within_target(t, m),
        Relationship::AnyOf { any_of, .. } => any_of.iter().any(|r| within(r, m)),
    }
}

/// True if the descriptor names `name` (ContainsAny with one identifier).
pub(crate) fn names(rel: &Relationship, name: &str) -> bool {
    rel.contains_any(&|n| n == name)
}

/// Modules keyed by identifier, with indexes for provides, depends and conflicts names.
#[derive(Clone, Default)]
pub(crate) struct ModSet<'a> {
    by_id: HashMap<String, MRef<'a>>,
    /// Identifiers in insertion order (for stable "first match" results).
    order: Vec<String>,
    /// provided name -> identifiers that provide it (not the identifier itself).
    provides: HashMap<String, Vec<String>>,
    /// name -> identifiers whose depends name it.
    dep_names: HashMap<String, Vec<String>>,
    /// name -> identifiers whose conflicts name it.
    conf_names: HashMap<String, Vec<String>>,
}

fn rel_names(rels: &[Relationship]) -> HashSet<&str> {
    rels.iter().flat_map(|r| r.names()).collect()
}

fn push_idx(map: &mut HashMap<String, Vec<String>>, key: &str, id: &str) {
    map.entry(key.to_string()).or_default().push(id.to_string());
}

fn pull_idx(map: &mut HashMap<String, Vec<String>>, key: &str, id: &str) {
    if let Some(v) = map.get_mut(key) {
        v.retain(|x| x != id);
        if v.is_empty() {
            map.remove(key);
        }
    }
}

impl<'a> ModSet<'a> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert, replacing any module with the same identifier.
    pub fn insert(&mut self, m: MRef<'a>) {
        let id = m.identifier.clone();
        self.remove(&id);
        for p in &m.provides {
            if *p != id {
                push_idx(&mut self.provides, p, &id);
            }
        }
        for n in rel_names(&m.depends) {
            push_idx(&mut self.dep_names, n, &id);
        }
        for n in rel_names(&m.conflicts) {
            push_idx(&mut self.conf_names, n, &id);
        }
        self.order.push(id.clone());
        self.by_id.insert(id, m);
    }

    pub fn remove(&mut self, id: &str) -> Option<MRef<'a>> {
        let m = self.by_id.remove(id)?;
        self.order.retain(|x| x != id);
        for p in &m.provides {
            pull_idx(&mut self.provides, p, id);
        }
        for n in rel_names(&m.depends) {
            pull_idx(&mut self.dep_names, n, id);
        }
        for n in rel_names(&m.conflicts) {
            pull_idx(&mut self.conf_names, n, id);
        }
        Some(m)
    }

    pub fn get(&self, id: &str) -> Option<&MRef<'a>> {
        self.by_id.get(id)
    }

    pub fn contains_id(&self, id: &str) -> bool {
        self.by_id.contains_key(id)
    }

    /// Identifier or provided name present.
    pub fn contains_name(&self, name: &str) -> bool {
        self.by_id.contains_key(name) || self.provides.contains_key(name)
    }

    pub fn iter(&self) -> impl Iterator<Item = &MRef<'a>> {
        self.order.iter().filter_map(|id| self.by_id.get(id))
    }

    pub fn ids(&self) -> impl Iterator<Item = &String> {
        self.order.iter()
    }

    fn find_target(&self, t: &RelationshipTarget, exclude: Option<&str>) -> Option<&MRef<'a>> {
        if exclude != Some(t.name.as_str())
            && let Some(m) = self.by_id.get(&t.name)
            && within_target(t, m)
        {
            return Some(m);
        }
        let provs = self.provides.get(&t.name)?;
        provs.iter().filter(|id| exclude != Some(id.as_str())).find_map(|id| self.by_id.get(id))
    }

    /// First module within bounds of `rel` (any_of: first child that matches), skipping the
    /// identifier `exclude`.
    pub fn find_excluding(&self, rel: &Relationship, exclude: Option<&str>) -> Option<&MRef<'a>> {
        match rel {
            Relationship::One(t) => self.find_target(t, exclude),
            Relationship::AnyOf { any_of, .. } => any_of.iter().find_map(|r| self.find_excluding(r, exclude)),
        }
    }

    pub fn find(&self, rel: &Relationship) -> Option<&MRef<'a>> {
        self.find_excluding(rel, None)
    }

    /// A module in the set (other identifier) that conflicts with `m`, in either direction.
    pub fn conflicts_with(&self, m: &Module) -> Option<&MRef<'a>> {
        for c in &m.conflicts {
            if let Some(x) = self.find_excluding(c, Some(&m.identifier)) {
                return Some(x);
            }
        }
        for name in m.provides_list() {
            for oid in self.conf_names.get(name).into_iter().flatten() {
                if *oid == m.identifier {
                    continue;
                }
                if let Some(o) = self.by_id.get(oid)
                    && o.conflicts.iter().any(|c| within(c, m))
                {
                    return Some(o);
                }
            }
        }
        None
    }

    /// Every module in the set (other identifier) that conflicts with `m`, in either direction.
    pub fn conflicts_all(&self, m: &Module) -> Vec<&MRef<'a>> {
        let me = m.identifier.as_str();
        let mut hits: Vec<&MRef<'a>> = Vec::new();
        for name in m.conflicts.iter().flat_map(|c| c.names()) {
            let ids = std::iter::once(name).chain(self.provides.get(name).into_iter().flatten().map(String::as_str));
            for id in ids {
                if let Some(x) = self.by_id.get(id)
                    && m.conflicts.iter().any(|c| within(c, x))
                {
                    hits.push(x);
                }
            }
        }
        for name in m.provides_list() {
            for oid in self.conf_names.get(name).into_iter().flatten() {
                if let Some(o) = self.by_id.get(oid)
                    && o.conflicts.iter().any(|c| within(c, m))
                {
                    hits.push(o);
                }
            }
        }
        let mut out: Vec<&MRef<'a>> = Vec::new();
        for x in hits {
            if x.identifier != me && !out.iter().any(|o| o.identifier == x.identifier) {
                out.push(x);
            }
        }
        out
    }

    /// BadRelationships(m, self) is non-empty (CKAN DependsAndConflictsOK is false).
    /// Returns a short description of the first problem.
    pub fn bad_relationship(&self, m: &Module) -> Option<String> {
        let me = m.identifier.as_str();
        // m depends on a present identifier at a wrong version.
        for d in &m.depends {
            if d.contains_any(&|n| n != me && self.by_id.contains_key(n)) && self.find_excluding(d, Some(me)).is_none() {
                return Some(format!("{m} depends on {d}"));
            }
        }
        if let Some(x) = self.conflicts_with(m) {
            return Some(format!("{m} conflicts with {}", x));
        }
        // A present module depends on m's identifier at another version.
        for oid in self.dep_names.get(me).into_iter().flatten() {
            if oid == me {
                continue;
            }
            if let Some(o) = self.by_id.get(oid) {
                for d in &o.depends {
                    if names(d, me) && !within(d, m) && self.find_excluding(d, Some(me)).is_none() {
                        return Some(format!("{} depends on {d}", o));
                    }
                }
            }
        }
        None
    }
}

/// Relationship satisfied by one of the sets, or by an unmanaged DLL.
/// (Deliberate deviation from CKAN: an any_of child matched only by a DLL or DLC counts.)
pub(crate) fn satisfied(rel: &Relationship, sets: &[&ModSet], dlls: &HashSet<String>) -> bool {
    match rel {
        Relationship::One(t) => dlls.contains(&t.name) || sets.iter().any(|s| s.find_target(t, None).is_some()),
        Relationship::AnyOf { any_of, .. } => any_of.iter().any(|r| satisfied(r, sets, dlls)),
    }
}

/// AvailableModule.Latest: newest version of `id` that passes stability, `accept`, the game
/// version criteria (unless `crit_off`), and DependsAndConflictsOK against each set in `checks`.
pub(crate) fn latest_ok<'i>(
    ctx: &Context<'i>,
    id: &str,
    accept: &dyn Fn(&Module) -> bool,
    crit_off: bool,
    checks: &[&ModSet],
) -> Option<&'i Arc<Module>> {
    ctx.index.versions(id).iter().find(|m| {
        m.release_status <= ctx.stability
            && accept(m)
            && (crit_off || m.is_compatible(ctx.criteria))
            && checks.iter().all(|s| s.bad_relationship(m).is_none())
    })
}

/// Identifiers that may provide `name`: the index providers plus `name` itself if it is a module.
pub(crate) fn provider_ids<'i>(ctx: &Context<'i>, name: &str) -> Vec<&'i str> {
    let mut v: Vec<&str> = ctx.index.providers_of(name).iter().map(String::as_str).collect();
    if let Some((k, _)) = ctx.index.modules.get_key_value(name)
        && !v.contains(&k.as_str())
    {
        v.insert(0, k.as_str());
    }
    v
}

/// LatestAvailableWithProvides for a descriptor: the latest acceptable version of each provider,
/// sorted by download count (desc) then name; any_of gives the distinct union of its children.
/// DLC are never candidates.
pub(crate) fn providers_for(ctx: &Context, rel: &Relationship, checks: &[&ModSet]) -> Vec<Arc<Module>> {
    let mut out: Vec<Arc<Module>> = Vec::new();
    collect_providers(ctx, rel, checks, &mut out);
    out
}

fn collect_providers(ctx: &Context, rel: &Relationship, checks: &[&ModSet], out: &mut Vec<Arc<Module>>) {
    match rel {
        Relationship::One(t) => {
            let mut list: Vec<Arc<Module>> = Vec::new();
            for pid in provider_ids(ctx, &t.name) {
                let accept = |m: &Module| within_target(t, m) && !m.is_dlc();
                if let Some(m) = latest_ok(ctx, pid, &accept, false, checks) {
                    list.push(m.clone());
                }
            }
            list.sort_by(|a, b| {
                ctx.index
                    .download_count(&b.identifier)
                    .cmp(&ctx.index.download_count(&a.identifier))
                    .then_with(|| a.display_name().cmp(b.display_name()))
            });
            for m in list {
                if !out.iter().any(|o| o.identifier == m.identifier) {
                    out.push(m);
                }
            }
        }
        Relationship::AnyOf { any_of, .. } => {
            for r in any_of {
                collect_providers(ctx, r, checks, out);
            }
        }
    }
}

/// Why a descriptor has no candidate at all (for error text).
pub(crate) fn why_none(ctx: &Context, rel: &Relationship) -> String {
    let all = rel.names();
    let mut known = false;
    for n in &all {
        for pid in provider_ids(ctx, n) {
            known = true;
            if ctx.index.versions(pid).iter().any(|m| m.is_dlc()) {
                return format!("{pid} is a DLC that is not installed");
            }
        }
    }
    if !known {
        return "not found in any repository".to_string();
    }
    "no compatible version".to_string()
}

/// Installed modules (including DLC entries) as a set borrowed from the registry.
pub(crate) fn installed_set<'a>(ctx: &Context<'a>) -> ModSet<'a> {
    let mut s = ModSet::new();
    for im in ctx.registry.installed_modules.values() {
        s.insert(MRef::Borrowed(&im.source_module));
    }
    s
}

pub(crate) fn dll_set(ctx: &Context) -> HashSet<String> {
    ctx.registry.installed_dlls.keys().cloned().collect()
}

/// One unmet depends or conflict found by `problems`.
pub(crate) struct Problem {
    pub id: String,
    pub text: String,
    pub conflict: bool,
}

/// Unmet depends and conflicts inside `set` plus `extra` (SanityChecker).
pub(crate) fn problems(set: &ModSet, extra: &[&ModSet], dlls: &HashSet<String>) -> Vec<Problem> {
    let mut sets: Vec<&ModSet> = vec![set];
    sets.extend_from_slice(extra);
    let mut out = Vec::new();
    for m in set.iter().chain(extra.iter().flat_map(|s| s.iter())) {
        for d in &m.depends {
            if !satisfied(d, &sets, dlls) {
                let text = format!("{} depends on {d}, which is not installed", m);
                out.push(Problem { id: m.identifier.clone(), text, conflict: false });
            }
        }
        for s in &sets {
            for x in s.conflicts_all(m) {
                // Report each pair once (each side finds the other).
                if m.identifier < x.identifier {
                    let text = format!("{} conflicts with {}", m, x);
                    out.push(Problem { id: m.identifier.clone(), text, conflict: true });
                }
            }
        }
    }
    out
}
