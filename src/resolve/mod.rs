//! Relationship resolver: turns user requests (install / remove / upgrade / replace) into an
//! ordered change set, following CKAN's RelationshipResolver rules (depends, provides, any_of,
//! conflicts, recommends/suggests, held versions, auto-installed orphans).

mod compat;
mod optional;
mod planner;
mod world;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod review_tests;

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::Arc;

use crate::meta::{Module, ModuleVersion, ReleaseStatus, VersionCriteria};
use crate::registry::Registry;
use crate::repo::RepoIndex;

/// Read-only view of everything the resolver needs.
#[derive(Clone, Copy)]
pub struct Context<'a> {
    pub index: &'a RepoIndex,
    pub registry: &'a Registry,
    pub criteria: &'a VersionCriteria,
    /// Identifiers that must stay at their installed version (Held label).
    pub held: &'a HashSet<String>,
    pub stability: ReleaseStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolverOptions {
    pub with_recommends: bool,
    pub with_suggests: bool,
    pub with_all_suggests: bool,
    /// Allow user-requested modules that are not compatible with the game version.
    pub allow_incompatible: bool,
}

impl Default for ResolverOptions {
    fn default() -> Self {
        ResolverOptions { with_recommends: true, with_suggests: false, with_all_suggests: false, allow_incompatible: false }
    }
}

#[derive(Debug, Clone)]
pub enum InstallRequest {
    /// Latest compatible version of an identifier (or of a provider of a virtual name).
    Latest(String),
    /// `id=version`.
    Exact(String, ModuleVersion),
    /// A module from a local .ckan file (may not be in the index).
    Module(Arc<Module>),
}

#[derive(Debug, Clone, Default)]
pub struct Request {
    pub install: Vec<InstallRequest>,
    pub remove: Vec<String>,
    pub upgrade: Vec<String>,
    /// Upgrade every installed module that has an update (held ones excepted).
    pub upgrade_all: bool,
    /// Installed identifiers to replace with their `replaced_by` target.
    pub replace: Vec<String>,
    /// Answers to earlier TooManyProvides questions: virtual/any_of text -> chosen identifier.
    pub choices: HashMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChangeKind {
    Install,
    Remove,
    Upgrade,
    Replace,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reason {
    UserRequested,
    /// Needed by this identifier.
    Depends(String),
    Recommended(String),
    Suggested(String),
    /// Removed because it depends on a removed module.
    ReverseDepends(String),
    /// Auto-installed and no longer needed.
    NoLongerNeeded,
    /// Replaces this identifier.
    Replaces(String),
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Reason::UserRequested => write!(f, "Requested by user"),
            Reason::Depends(p) => write!(f, "Dependency of {p}"),
            Reason::Recommended(p) => write!(f, "Recommended by {p}"),
            Reason::Suggested(p) => write!(f, "Suggested by {p}"),
            Reason::ReverseDepends(p) => write!(f, "Depends on {p}, which is being removed"),
            Reason::NoLongerNeeded => write!(f, "Auto-installed and no longer needed"),
            Reason::Replaces(p) => write!(f, "Replaces {p}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Change {
    pub kind: ChangeKind,
    /// The new module (Install/Upgrade/Replace) or the installed module (Remove).
    pub module: Arc<Module>,
    /// For Upgrade: the installed version. For Replace: the replaced identifier's version.
    pub old_version: Option<ModuleVersion>,
    /// For Replace: the identifier being replaced.
    pub replaces: Option<String>,
    pub reasons: Vec<Reason>,
    /// Install as auto-installed (only dependencies).
    pub auto_installed: bool,
}

/// Ordered: all Remove changes first, then Upgrade/Replace/Install in dependency order
/// (dependencies before the modules that need them).
#[derive(Debug, Clone, Default)]
pub struct Changeset {
    pub changes: Vec<Change>,
}

impl Changeset {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// Modules that must be downloaded (Install/Upgrade/Replace of non-metapackages).
    pub fn downloads(&self) -> Vec<Arc<Module>> {
        self.changes
            .iter()
            .filter(|c| c.kind != ChangeKind::Remove && !c.module.is_metapackage() && !c.module.is_dlc())
            .map(|c| c.module.clone())
            .collect()
    }
}

#[derive(Debug, Clone)]
pub enum ResolveError {
    NotFound(String),
    NotInstalled(String),
    IsDlc(String),
    /// Requested module not compatible with the game (and allow_incompatible is off).
    Incompatible(String),
    /// Dependency chains that cannot be met, e.g. "A 1.0 depends on B >= 2 (no compatible version)".
    Unsatisfied(Vec<String>),
    /// Conflict descriptions, e.g. "A 1.0 conflicts with B 2.0".
    Conflicts(Vec<String>),
    /// More than one module can satisfy `requested`; the user must pick one. Put the answer
    /// in `Request::choices[requested]` and plan again.
    TooManyProvides { requester: String, requested: String, choices: Vec<Arc<Module>>, help: Option<String> },
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResolveError::NotFound(s) => write!(f, "Module not found: {s}"),
            ResolveError::NotInstalled(s) => write!(f, "Module not installed: {s}"),
            ResolveError::IsDlc(s) => write!(f, "{s} is a DLC and cannot be installed or removed here"),
            ResolveError::Incompatible(s) => write!(f, "{s} is not compatible with this game version"),
            ResolveError::Unsatisfied(v) => write!(f, "Unsatisfied dependencies:\n  {}", v.join("\n  ")),
            ResolveError::Conflicts(v) => write!(f, "Conflicts:\n  {}", v.join("\n  ")),
            ResolveError::TooManyProvides { requester, requested, choices, help } => {
                write!(f, "{requested} (needed by {requester}) is provided by more than one mod: ")?;
                let names: Vec<&str> = choices.iter().map(|m| m.identifier.as_str()).collect();
                write!(f, "{}", names.join(", "))?;
                if let Some(h) = help {
                    write!(f, "\n{h}")?;
                }
                Ok(())
            }
        }
    }
}

/// Build the change set for a request.
pub fn plan(ctx: &Context, req: &Request, opts: &ResolverOptions) -> Result<Changeset, ResolveError> {
    planner::plan(*ctx, req, opts)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionalKind {
    Recommends,
    Suggests,
    Supports,
}

/// An optional module offered in the recommendations dialog.
#[derive(Debug, Clone)]
pub struct OptionalMod {
    pub module: Arc<Module>,
    pub kind: OptionalKind,
    /// Identifiers that recommend / suggest / are supported by it.
    pub sources: Vec<String>,
    /// Pre-checked (recommendations that can be installed; never suggestions or supporters).
    pub checked: bool,
}

/// Recommendations, suggestions and supporters for the modules a change set installs
/// (or, with an empty change set, for all installed modules = "audit recommendations").
/// Excludes installed and already-planned modules and those that cannot be installed.
pub fn optional_mods(ctx: &Context, cs: &Changeset) -> Vec<OptionalMod> {
    optional::optional_mods(*ctx, cs)
}

/// The update CKAN's UpgradeableModules would offer for `id` (None if held, DLC, or up to date).
/// The update must fit the installed modules that stay as they are (held ones and those without
/// an update) plus the other updates, so a pair that pins each other upgrades together. Same
/// version with changed metadata (install stanzas, size, hash, relationships, provides,
/// replaced_by) also counts, as in CKAN. Unmanaged DLLs and virtual names give None.
/// This solves for all installed modules; for a list, call `updates` once instead.
#[cfg(test)]
pub fn has_update(ctx: &Context, id: &str) -> Option<Arc<Module>> {
    if ctx.held.contains(id) || ctx.registry.installed(id).is_none() {
        return None;
    }
    updates(ctx).remove(id)
}

/// `has_update` for every installed module at once (one solve; use it for lists).
pub fn updates(ctx: &Context) -> HashMap<String, Arc<Module>> {
    let set = world::installed_set(ctx);
    let ids: Vec<String> = ctx
        .registry
        .installed_mods()
        .map(|im| im.source_module.identifier.clone())
        .filter(|id| !ctx.held.contains(id))
        .collect();
    planner::upgrade_targets(ctx, &set, &ids).into_iter().map(|m| (m.identifier.clone(), m)).collect()
}

/// The compatible `replaced_by` target of an installed module, if any.
pub fn replacement(ctx: &Context, id: &str) -> Option<Arc<Module>> {
    let im = ctx.registry.installed(id)?;
    let rb = im.source_module.replaced_by.as_ref()?;
    let m = match &rb.version {
        Some(v) => ctx.index.get(&rb.name, v)?,
        None => world::latest_ok(ctx, &rb.name, &|m| world::within_target(rb, m) && !m.is_dlc(), false, &[])?,
    };
    m.is_compatible(ctx.criteria).then(|| m.clone())
}

/// Installed identifiers that would break if `ids` were removed (transitively), not including `ids`.
/// Modules whose depends were already unmet before are not counted.
#[cfg(test)]
pub fn reverse_dependencies(ctx: &Context, ids: &[String]) -> Vec<String> {
    let mut hyp = world::installed_set(ctx);
    let dlls = world::dll_set(ctx);
    let prebroken = planner::unmet_ids(&hyp, &dlls);
    let start: Vec<String> = ids.iter().filter(|id| hyp.contains_id(id)).cloned().collect();
    planner::removal_closure(&mut hyp, &start, &dlls, &prebroken, &HashSet::new())
        .into_iter()
        .map(|(id, _)| id)
        .filter(|id| !ids.contains(id))
        .collect()
}

/// Compatible and all `depends` can be met by installed or compatible modules (CKAN
/// CompatibleModules rule; cycles count as met). Used for the "Compatible" filter.
/// Also checks `release_status <= ctx.stability`; DLC is never installable. For a whole list,
/// call `installable_set` once instead (it shares one memo).
#[cfg(test)]
pub fn is_installable(ctx: &Context, m: &Module) -> bool {
    let set = world::installed_set(ctx);
    let dlls = world::dll_set(ctx);
    compat::Compat::new(*ctx).module_ok(m, &[&set], &dlls)
}

/// Identifiers that pass the CompatibleModules rule (some compatible version whose depends can
/// be met), computed for the whole index in one pass.
pub fn installable_set(ctx: &Context) -> HashSet<String> {
    let set = world::installed_set(ctx);
    let dlls = world::dll_set(ctx);
    let mut c = compat::Compat::new(*ctx);
    let mut ids: Vec<&String> = ctx.index.modules.keys().collect();
    ids.sort();
    ids.into_iter().filter(|id| c.ident_ok(id, &[&set], &dlls)).cloned().collect()
}
