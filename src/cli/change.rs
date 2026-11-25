//! Commands that change installed mods: install, remove, upgrade, replace, import.
//! Each one builds a Request, plans it (asking about provider choices and optional mods),
//! shows the change set, asks to continue, then applies it.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use crate::error::Error;
use crate::meta::{Module, ModuleVersion};
use crate::ops::{ApplyReport, Session};
use crate::resolve::{self, Changeset, InstallRequest, Request, ResolveError, ResolverOptions};

use super::args::{self, Globals, flag, multi};
use super::mods::{canonical_id, with_progress};
use super::interrupt::Guard;
use super::term;
use super::{Fail, Res, fmt, open_session};

/// Order provider choices: most downloads first, then the requested name itself, then by id.
pub fn order_choices(choices: &mut [Arc<Module>], requested: &str, downloads: &dyn Fn(&str) -> u64) {
    choices.sort_by(|a, b| {
        downloads(&b.identifier)
            .cmp(&downloads(&a.identifier))
            .then_with(|| (b.identifier == requested).cmp(&(a.identifier == requested)))
            .then_with(|| a.identifier.cmp(&b.identifier))
    });
}

/// Plan the request; ask the user to pick a provider when more than one mod can satisfy a need.
fn plan(s: &Session, g: &Globals, req: &mut Request, opts: &ResolverOptions) -> Result<Changeset, Fail> {
    loop {
        let err = match s.plan(req, opts) {
            Ok(cs) => return Ok(cs),
            Err(e) => e,
        };
        let Error::Resolve(ResolveError::TooManyProvides { requester, requested, mut choices, help }) = err else {
            return Err(err.into());
        };
        if g.headless || choices.is_empty() || req.choices.contains_key(&requested) {
            let e = ResolveError::TooManyProvides { requester, requested, choices, help };
            return Err(Fail(e.to_string()));
        }
        order_choices(&mut choices, &requested, &|id| s.index.download_count(id));
        out!("{requested} (needed by {requester}) is provided by more than one mod.");
        if let Some(h) = &help {
            out!("{h}");
        }
        for (i, m) in choices.iter().enumerate() {
            out!("{:>3}) {} {} - {} ({} downloads)", i + 1, m.identifier, m.version, m.display_name(), s.index.download_count(&m.identifier));
        }
        let Some(k) = term::choose_number("Choose one", choices.len()) else {
            return Err(Fail("Cancelled".into()));
        };
        let Some(pick) = choices.get(k) else {
            return Err(Fail("Cancelled".into()));
        };
        req.choices.insert(requested, pick.identifier.clone());
    }
}

/// Ask which optional mods to add. Returns their indices.
fn ask_optional(n: usize) -> Vec<usize> {
    loop {
        term::write_out("Add which? (numbers like 1 3 5-7, a = all, Enter = none): ");
        let Some(line) = term::read_line() else {
            term::write_out("\n");
            return Vec::new();
        };
        match term::parse_multi_choice(&line, n) {
            Ok(v) => return v,
            Err(e) => out!("{e}"),
        }
    }
}

/// Unowned files that stop the change set from installing: from the cached zips (exact), plus
/// any `<path> already exists;` named in the error text.
fn conflict_files(s: &Session, cs: &Changeset, msg: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let cache = s.cache.index();
    let mut filters = s.instance.install_filters();
    filters.extend(s.config.global_install_filters());
    for m in cs.downloads() {
        let Some(zip) = cache.get(&m) else { continue };
        let Ok(entries) = crate::install::zip_entry_names(zip) else { continue };
        let Ok(files) = crate::install::module_files(&m, &entries, &filters) else { continue };
        out.extend(crate::install::existing_conflicts(&s.instance, &s.registry, &files));
    }
    out.extend(paths_in_conflict_message(msg));
    let mut seen = HashSet::new();
    out.retain(|p| seen.insert(p.clone()) && s.registry.owner_of(p).is_none());
    out
}

/// `GameData/x.cfg already exists; Foo 1.0 cannot install it` gives `GameData/x.cfg`
/// (not when the file is owned by a mod).
pub fn paths_in_conflict_message(msg: &str) -> Vec<String> {
    msg.lines()
        .filter_map(|l| l.trim().split_once(" already exists;").map(|(p, _)| p.trim()))
        .filter(|p| !p.is_empty() && !p.contains(" (owned by "))
        .map(str::to_string)
        .collect()
}

/// `Session::apply` with progress. Ctrl-C is held while files change (see `interrupt`).
fn apply_once(s: &mut Session, cs: &Changeset) -> (crate::error::Result<ApplyReport>, bool) {
    let guard = Guard::new();
    let r = with_progress(|cb| {
        let watch = |m: &str, f: Option<f32>| {
            Guard::note_progress(m);
            cb(m, f);
        };
        s.apply(cs, &watch)
    });
    let stopped = guard.interrupted();
    drop(guard);
    (r, stopped)
}

/// Apply with progress. On a file conflict, offer to move the files to the Trash and retry.
fn apply(s: &mut Session, g: &Globals, cs: &Changeset) -> Result<ApplyReport, Fail> {
    let mut tries = 0;
    loop {
        tries += 1;
        let err = match apply_once(s, cs) {
            (Ok(r), false) => return Ok(r),
            (Ok(r), true) => {
                print_report(&r);
                return Err(Fail("Stopped by the interrupt after the changes above were done.".into()));
            }
            (Err(e), true) => return Err(Fail(format!("{e}\nStopped by the interrupt; the changes were undone."))),
            (Err(e), false) => e,
        };
        let Error::Conflict(msg) = &err else {
            return Err(err.into());
        };
        let mut files = crate::ops::conflict_files(&err);
        if files.is_empty() {
            files = conflict_files(s, cs, msg);
        }
        if files.is_empty() || tries > 3 {
            return Err(err.into());
        }
        out!("These files are in the way and no mod owns them:");
        for f in &files {
            out!("  - {f}");
        }
        if g.headless {
            return Err(Fail(NOT_TRASHED.into()));
        }
        if !term::confirm_always_ask(g, "Move them to the Trash and try again?") {
            return Err(Fail(NOT_TRASHED.into()));
        }
        s.trash_files(&files)?;
    }
}

/// The user's files in the way were kept; nothing was installed.
const NOT_TRASHED: &str =
    "Nothing was changed. Move or remove these files (or answer y to move them to the Trash), then try again.";

fn print_report(r: &ApplyReport) {
    for (head, list) in [("Installed", &r.installed), ("Upgraded", &r.upgraded), ("Removed", &r.removed)] {
        if !list.is_empty() {
            out!("{head}: {}", list.join(", "));
        }
    }
    for m in &r.messages {
        out!("{m}");
    }
}

/// Plan, offer optional mods, show, confirm, apply.
fn run_changes(s: &mut Session, g: &Globals, mut req: Request, opts: ResolverOptions, offer_optional: bool) -> Res {
    let mut cs = plan(s, g, &mut req, &opts)?;
    if offer_optional && !g.headless && !g.yes && !cs.is_empty() {
        let optional = resolve::optional_mods(&s.context(), &cs);
        if !optional.is_empty() {
            out!("Optional mods for this change:");
            term::write_out(&fmt::optional_list(&optional));
            let picked = ask_optional(optional.len());
            if !picked.is_empty() {
                for i in picked {
                    if let Some(o) = optional.get(i) {
                        req.install.push(InstallRequest::Exact(o.module.identifier.clone(), o.module.version.clone()));
                    }
                }
                cs = plan(s, g, &mut req, &opts)?;
            }
        }
    }
    if cs.is_empty() {
        out!("Nothing to do.");
        return Ok(());
    }
    let cached = s.cache.index();
    term::write_out(&fmt::changeset(&cs, &|m| cached.get(m).is_some()));
    if !term::confirm(g, "Continue?") {
        return Err(Fail("Cancelled at user request.".into()));
    }
    let rep = apply(s, g, &cs)?;
    print_report(&rep);
    out!("Done!");
    Ok(())
}

fn resolver_options(s: &Session, p: &args::Parsed) -> ResolverOptions {
    ResolverOptions {
        with_recommends: s.config.hangar.with_recommends && !p.flag("--no-recommends"),
        with_suggests: s.config.hangar.with_suggests || p.flag("--with-suggests"),
        with_all_suggests: p.flag("--with-all-suggests"),
        allow_incompatible: p.flag("--allow-incompatible"),
    }
}

const INSTALL_OPTS: [args::Opt; 5] = [
    multi(&["-c", "--ckanfiles"]),
    flag(&["--no-recommends"]),
    flag(&["--with-suggests"]),
    flag(&["--with-all-suggests"]),
    flag(&["--allow-incompatible"]),
];

/// Read .ckan files or URLs into install requests (DLC is refused).
fn ckan_requests(files: &[String]) -> Result<Vec<Arc<Module>>, Fail> {
    let mut out = Vec::new();
    for f in files {
        let m = crate::modlist::read_ckan(f).map_err(|e| match e {
            Error::NotFound(p) if !f.contains(['/', '.']) => {
                Fail(format!("File not found, exiting: {p}\n-c takes every name after it; put mod identifiers before -c."))
            }
            Error::NotFound(p) => Fail(format!("File not found, exiting: {p}")),
            other => Fail(format!("{f}: {other}")),
        })?;
        if m.is_dlc() {
            return Err(Fail(format!("{} is a DLC and cannot be installed here", m.identifier)));
        }
        out.push(Arc::new(m));
    }
    Ok(out)
}

/// `id` or `id=version` to an install request, with the identifier case fixed.
fn id_request(s: &Session, arg: &str) -> InstallRequest {
    let (id, ver) = args::split_id_version(arg);
    let id = canonical_id(s, id);
    match ver {
        Some(v) => InstallRequest::Exact(id, ModuleVersion::new(v)),
        None => InstallRequest::Latest(id),
    }
}

pub fn install(g: &Globals, a: &[String]) -> Res {
    let p = args::parse(a, &INSTALL_OPTS)?;
    if p.pos.is_empty() && p.values("-c").is_empty() {
        return Err(Fail("Missing argument: mods to install (identifiers or -c files)".into()));
    }
    let ckans = ckan_requests(p.values("-c"))?;
    let mut s = open_session(g)?;
    let mut req = Request::default();
    for arg in &p.pos {
        let r = id_request(&s, arg);
        if let InstallRequest::Latest(id) = &r
            && s.registry.installed(id).is_some()
        {
            out!("{id} is already installed, skipping");
            continue;
        }
        req.install.push(r);
    }
    for m in ckans {
        if s.registry.installed(&m.identifier).is_some_and(|im| im.source_module.version == m.version) {
            out!("{m} is already installed, skipping");
            continue;
        }
        req.install.push(InstallRequest::Module(m));
    }
    if req.install.is_empty() {
        out!("Nothing to install.");
        return Ok(());
    }
    let opts = resolver_options(&s, &p);
    run_changes(&mut s, g, req, opts, true)
}

pub fn remove(g: &Globals, a: &[String]) -> Res {
    let p = args::parse(a, &[flag(&["--all"]), flag(&["--re", "--regex"])])?;
    let mut s = open_session(g)?;
    let installed: Vec<String> = s.registry.installed_mods().map(|im| im.source_module.identifier.clone()).collect();
    let ids: Vec<String> = if p.flag("--all") {
        if installed.is_empty() {
            out!("No modules are installed, so there is nothing to remove.");
            return Ok(());
        }
        installed
    } else if p.flag("--re") {
        let mut v = Vec::new();
        for pat in &p.pos {
            let re = regex::Regex::new(pat).map_err(|e| Fail(format!("Bad pattern {pat}: {e}")))?;
            v.extend(installed.iter().filter(|id| re.is_match(id)).cloned());
        }
        v.sort();
        v.dedup();
        v
    } else {
        let mut v = Vec::new();
        for name in &p.pos {
            let id = canonical_id(&s, name);
            match s.registry.installed(&id) {
                Some(im) if im.source_module.is_dlc() => {
                    return Err(Fail(format!("{id} is a DLC and cannot be removed here")));
                }
                Some(_) => v.push(id),
                None => return Err(Fail(format!("Module {name} is not installed"))),
            }
        }
        v
    };
    if ids.is_empty() {
        return Err(Fail("Missing argument: mods to remove".into()));
    }
    let req = Request { remove: ids, ..Request::default() };
    run_changes(&mut s, g, req, ResolverOptions::default(), false)
}

pub fn upgrade(g: &Globals, a: &[String]) -> Res {
    let p = args::parse(
        a,
        &[flag(&["--all"]), multi(&["-c", "--ckanfiles"]), flag(&["--allow-incompatible"]), flag(&["--no-recommends"])],
    )?;
    if p.pos.is_empty() && !p.flag("--all") && p.values("-c").is_empty() {
        return Err(Fail("Missing argument: mods to upgrade, or --all".into()));
    }
    let ckans = ckan_requests(p.values("-c"))?;
    let mut s = open_session(g)?;
    let ups = resolve::updates(&s.context());
    let mut req = Request::default();
    if p.flag("--all") {
        if ups.is_empty() && ckans.is_empty() {
            out!("All modules are up to date");
            return Ok(());
        }
        req.upgrade_all = !ups.is_empty();
    }
    for arg in &p.pos {
        let (name, ver) = args::split_id_version(arg);
        let id = canonical_id(&s, name);
        if s.registry.installed(&id).is_none() {
            return Err(Fail(format!("Module {name} is not installed")));
        }
        match ver {
            Some(v) => req.install.push(InstallRequest::Exact(id, ModuleVersion::new(v))),
            None if ups.contains_key(&id) => req.upgrade.push(id),
            None if s.held.contains(&id) => out!("Module {id} is held at its installed version"),
            None => out!("Module {id} is already up to date"),
        }
    }
    req.install.extend(ckans.into_iter().map(InstallRequest::Module));
    if !req.upgrade_all && req.upgrade.is_empty() && req.install.is_empty() {
        return Ok(());
    }
    let opts = ResolverOptions {
        with_recommends: false,
        allow_incompatible: p.flag("--allow-incompatible"),
        ..ResolverOptions::default()
    };
    run_changes(&mut s, g, req, opts, false)
}

pub fn replace(g: &Globals, a: &[String]) -> Res {
    let p = args::parse(a, &[flag(&["--all"]), flag(&["--allow-incompatible"])])?;
    if p.pos.is_empty() && !p.flag("--all") {
        return Err(Fail("Missing argument: mods to replace, or --all".into()));
    }
    let mut s = open_session(g)?;
    let ids: Vec<String> = if p.flag("--all") {
        s.registry.installed_mods().filter(|im| im.source_module.replaced_by.is_some()).map(|im| im.source_module.identifier.clone()).collect()
    } else {
        let mut v = Vec::new();
        for name in &p.pos {
            let id = canonical_id(&s, name);
            if s.registry.installed(&id).is_none() {
                return Err(Fail(format!("Module {name} not found")));
            }
            v.push(id);
        }
        v
    };
    let mut req = Request::default();
    {
        let ctx = s.context();
        for id in ids {
            let old = s.registry.installed(&id).map(|im| im.source_module.version.to_string()).unwrap_or_default();
            match resolve::replacement(&ctx, &id) {
                Some(r) => {
                    out!("Replacement {} {} found for {id} {old}", r.identifier, r.version);
                    req.replace.push(id);
                }
                None => out!("No replacement found for {id}"),
            }
        }
    }
    if req.replace.is_empty() {
        out!("No replacements found");
        return Ok(());
    }
    let opts = ResolverOptions { allow_incompatible: p.flag("--allow-incompatible"), ..ResolverOptions::default() };
    run_changes(&mut s, g, req, opts, false)
}

pub fn import(g: &Globals, a: &[String]) -> Res {
    let p = args::parse(a, &[flag(&["--delete"])])?;
    if p.pos.is_empty() {
        return Err(Fail("Missing argument: zip files or folders to import".into()));
    }
    let paths: Vec<PathBuf> = p.pos.iter().map(PathBuf::from).collect();
    for path in paths.iter().filter(|p| !p.exists()) {
        warn!("File not found: {}", path.display());
    }
    let mut s = open_session(g)?;
    let delete = p.flag("--delete") && term::confirm(g, "Move the imported files to the Trash after import?");
    let res = crate::modlist::import_zips(&paths, &s.index, &s.cache, &s.criteria, delete)?;
    for (f, m) in &res.stored {
        out!("Imported {m} from {}", f.display());
    }
    for (f, m) in &res.already_cached {
        out!("Already cached: {m} ({})", f.display());
    }
    for f in res.not_found.iter().filter(|f| f.exists()) {
        warn!("Not found in index: {}", f.display());
    }
    let todo: Vec<&Arc<Module>> = res
        .installable
        .iter()
        .filter(|m| s.registry.installed(&m.identifier).is_none_or(|im| im.source_module.version != m.version))
        .collect();
    if todo.is_empty() {
        return Ok(());
    }
    let q = format!(
        "Install {} compatible imported mods in game instance {} ({})?",
        todo.len(),
        s.instance.name,
        s.instance.game_dir.display()
    );
    if !term::confirm(g, &q) {
        return Ok(());
    }
    let req = Request {
        install: todo.iter().map(|m| InstallRequest::Exact(m.identifier.clone(), m.version.clone())).collect(),
        ..Request::default()
    };
    run_changes(&mut s, g, req, ResolverOptions::default(), false)
}

