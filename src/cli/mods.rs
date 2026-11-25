//! Session commands that read mod data or change settings: update, available, list, search,
//! show, scan, mark, compat, repo, export, play.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::sync::Arc;

use crate::meta::{GameVersion, Module, ModuleVersion};
use crate::modlist::{self, ExportFormat};
use crate::ops::Session;
use crate::registry::InstalledVersion;
use crate::repo::{Repository, UpdateResult};
use crate::resolve;

use super::args::{self, Globals, flag, value};
use super::term::{self, ProgressPrinter};
use super::{Fail, Res, fmt, open_session};

fn need<'a>(pos: &'a [String], i: usize, what: &str) -> Result<&'a str, Fail> {
    pos.get(i).map(String::as_str).ok_or_else(|| Fail(format!("Missing argument: {what}")))
}

/// Run `f` with a progress callback that prints to the terminal.
pub fn with_progress<T>(f: impl FnOnce(crate::repo::Progress) -> T) -> T {
    let pp = ProgressPrinter::new();
    let cb = |m: &str, frac: Option<f32>| pp.update(m, frac);
    let r = f(&cb);
    pp.finish();
    r
}

fn game_line(s: &Session) -> String {
    let v = s.instance.version.map(|v| v.to_string()).unwrap_or_else(|| "unknown".into());
    format!("KSP {v}")
}

// ---------------------------------------------------------------- update

/// Newest version of each identifier (to report what an update changed).
fn snapshot(s: &Session) -> HashMap<String, ModuleVersion> {
    s.index.modules.iter().filter_map(|(id, v)| v.first().map(|m| (id.clone(), m.version.clone()))).collect()
}

fn name_of(s: &Session, id: &str) -> String {
    s.index.versions(id).first().map(|m| m.display_name().to_string()).unwrap_or_else(|| id.to_string())
}

pub fn update(g: &Globals, a: &[String]) -> Res {
    let p = args::parse(a, &[flag(&["-f", "--force"]), flag(&["-l", "--list-changes"])])?;
    if let Some(x) = p.pos.first() {
        return Err(Fail(format!("Unexpected argument: {x}")));
    }
    let mut s = open_session(g)?;
    let before = p.flag("-l").then(|| snapshot(&s));
    let r = with_progress(|cb| s.update_repos(p.flag("-f"), cb))?;
    if r == UpdateResult::NoChanges {
        out!("Repositories already up to date.");
    }
    if let Some(before) = before {
        let after = snapshot(&s);
        let mut added: Vec<&String> = after.keys().filter(|k| !before.contains_key(*k)).collect();
        let mut removed: Vec<&String> = before.keys().filter(|k| !after.contains_key(*k)).collect();
        let mut changed: Vec<&String> =
            after.iter().filter(|(k, v)| before.get(*k).is_some_and(|b| b != *v)).map(|(k, _)| k).collect();
        added.sort();
        removed.sort();
        changed.sort();
        out!("Found {} new modules, {} removed modules, and {} updated modules.", added.len(), removed.len(), changed.len());
        for (head, list) in [("New", &added), ("Removed", &removed), ("Updated", &changed)] {
            if !list.is_empty() {
                out!("{head} modules [Name (CKAN identifier)]:");
                for id in list.iter() {
                    out!("  {} ({id})", name_of(&s, id));
                }
            }
        }
    }
    out!("Updated information on {} modules.", s.index.modules.len());
    Ok(())
}

// ---------------------------------------------------------------- available / search

/// (compatible latest modules, ids that are not compatible). Sorted by identifier.
fn split_compatible(s: &Session) -> (Vec<Arc<Module>>, Vec<String>) {
    let ctx = s.context();
    let ok = resolve::installable_set(&ctx);
    let mut compat = Vec::new();
    let mut other = Vec::new();
    let mut ids: Vec<&String> = s.index.modules.keys().collect();
    ids.sort();
    for id in ids {
        let latest = ok.contains(id).then(|| s.index.latest(id, Some(&s.criteria), ctx.stability)).flatten();
        match latest {
            Some(m) => compat.push(Arc::clone(m)),
            None => other.push(id.clone()),
        }
    }
    (compat, other)
}

pub fn available(g: &Globals, a: &[String]) -> Res {
    let p = args::parse(a, &[flag(&["--detail"])])?;
    let s = open_session(g)?;
    let (compat, _) = split_compatible(&s);
    let mut text = format!("Modules compatible with {}\n\n", game_line(&s));
    for m in compat.iter().filter(|m| !m.is_dlc()) {
        if p.flag("--detail") {
            let _ = writeln!(text, "* {} ({}) - {} - {}", m.identifier, m.version, m.display_name(), m.abstract_);
        } else {
            let _ = writeln!(text, "* {} ({}) - {}", m.identifier, m.version, m.display_name());
        }
    }
    term::write_out(&text);
    Ok(())
}

fn by_name(a: &Arc<Module>, b: &Arc<Module>) -> std::cmp::Ordering {
    a.name.cmp(&b.name).then_with(|| a.identifier.cmp(&b.identifier))
}

pub fn search(g: &Globals, a: &[String]) -> Res {
    let p = args::parse(a, &[flag(&["--detail"]), flag(&["--all"]), value(&["--author"])])?;
    let term_raw = p.pos.join(" ");
    let author_raw = p.value("--author").unwrap_or("").to_string();
    if term_raw.trim().is_empty() && author_raw.trim().is_empty() {
        return Err(Fail("Missing argument: search term (or --author)".into()));
    }
    let term = fmt::searchable(&term_raw);
    let author = fmt::searchable(&author_raw);
    let s = open_session(g)?;
    let stability = s.config.hangar.stability;
    let (compat, other) = split_compatible(&s);
    let mut found: Vec<Arc<Module>> = compat.into_iter().filter(|m| fmt::search_matches(m, &term, &author)).collect();
    found.sort_by(by_name);
    let mut incompat: Vec<Arc<Module>> = Vec::new();
    if p.flag("--all") {
        for id in &other {
            let m = s.index.latest(id, None, stability).or_else(|| s.index.versions(id).first());
            if let Some(m) = m.filter(|m| fmt::search_matches(m, &term, &author)) {
                incompat.push(Arc::clone(m));
            }
        }
        incompat.sort_by(by_name);
    }
    let by = if author_raw.trim().is_empty() { String::new() } else { format!(" by \"{author_raw}\"") };
    if p.flag("--all") {
        out!("Found {} compatible and {} incompatible mods matching \"{term_raw}\"{by}", found.len(), incompat.len());
    } else {
        out!("Found {} compatible mods matching \"{term_raw}\"{by}", found.len());
    }
    let mut text = String::new();
    if p.flag("--detail") {
        if !found.is_empty() {
            text.push_str("Matching compatible mods:\n");
        }
        for m in &found {
            let _ = writeln!(text, "* {} ({}) - {} by {} - {}", m.identifier, m.version, m.display_name(), authors(m), m.abstract_);
        }
        if !incompat.is_empty() {
            text.push_str("Matching incompatible mods:\n");
        }
        for m in &incompat {
            let _ = writeln!(
                text,
                "* {} ({} - {}) - {} by {} - {}",
                m.identifier,
                m.version,
                fmt::game_span(m),
                m.display_name(),
                authors(m),
                m.abstract_
            );
        }
    } else {
        let mut ids: Vec<&str> = found.iter().chain(incompat.iter()).map(|m| m.identifier.as_str()).collect();
        ids.sort_unstable();
        for id in ids {
            let _ = writeln!(text, "{id}");
        }
    }
    term::write_out(&text);
    Ok(())
}

fn authors(m: &Module) -> String {
    if m.author.is_empty() { "N/A".into() } else { m.author.join(", ") }
}

// ---------------------------------------------------------------- list / export

pub fn list(g: &Globals, a: &[String]) -> Res {
    let p = args::parse(a, &[flag(&["--porcelain"]), value(&["--export"])])?;
    let s = open_session(g)?;
    if let Some(name) = p.value("--export") {
        match ExportFormat::from_name(name) {
            Some(ExportFormat::Ckan) => {
                // CKAN `list --export ckan` writes no versions.
                let mut opts = modlist::ModpackOptions::defaults_for(&s.instance);
                opts.with_versions = false;
                let m = modlist::generate_modpack(&s.instance, &s.registry, &s.index, &opts);
                term::write_out(&m.to_json_pretty());
                return Ok(());
            }
            Some(f) => {
                term::write_out(&modlist::export_text(f, &s.registry));
                return Ok(());
            }
            None => warn!("Unknown export format: {name}"),
        }
    }
    let porcelain = p.flag("--porcelain");
    let mut text = String::new();
    if !porcelain {
        let _ = writeln!(text, "\nKSP found at: {}", s.instance.game_dir.display());
        if let Some(v) = s.instance.version {
            let _ = writeln!(text, "\nKSP version: {v}");
        }
        text.push_str("\nInstalled modules:\n\n");
    }
    for row in list_rows(&s) {
        text.push_str(&row);
        text.push('\n');
    }
    if !porcelain {
        let _ = writeln!(text, "\n{}", fmt::LIST_LEGEND);
    }
    term::write_out(&text);
    Ok(())
}

/// One line per installed module, DLC and unmanaged DLL, sorted by identifier.
pub fn list_rows(s: &Session) -> Vec<String> {
    let ctx = s.context();
    let ups = resolve::updates(&ctx);
    let mut all: BTreeMap<&str, Option<&crate::registry::InstalledModule>> = BTreeMap::new();
    for (id, im) in &s.registry.installed_modules {
        all.insert(id, Some(im));
    }
    for id in s.registry.installed_dlls.keys() {
        all.entry(id).or_insert(None);
    }
    let mut out = Vec::with_capacity(all.len());
    for (id, im) in all {
        let Some(im) = im else {
            let f = fmt::RowFacts { unmanaged: true, ..facts_default() };
            out.push(fmt::list_row(id, "", &f));
            continue;
        };
        let m = &im.source_module;
        if m.is_dlc() {
            let ver = match s.registry.installed_version(id, false) {
                Some(InstalledVersion::Dlc(Some(v))) => v.to_string(),
                _ => String::new(),
            };
            let f = fmt::RowFacts { unmanaged: true, ..facts_default() };
            out.push(fmt::list_row(id, &ver, &f));
            continue;
        }
        let rep = m.replaced_by.as_ref().and_then(|_| resolve::replacement(&ctx, id));
        let f = fmt::RowFacts {
            unmanaged: false,
            in_repo: !s.index.versions(id).is_empty(),
            has_compatible: s.index.latest(id, Some(&s.criteria), ctx.stability).is_some(),
            upgradeable: ups.contains_key(id),
            auto_installed: im.auto_installed,
            replacement: rep.as_ref().map(|r| (r.identifier.as_str(), r.version.as_str())),
        };
        out.push(fmt::list_row(id, m.version.as_str(), &f));
    }
    out
}

fn facts_default() -> fmt::RowFacts<'static> {
    fmt::RowFacts {
        unmanaged: false,
        in_repo: true,
        has_compatible: true,
        upgradeable: false,
        auto_installed: false,
        replacement: None,
    }
}

/// Export format from `--format` or the file extension.
pub fn export_format(fmt_opt: Option<&str>, file: &str) -> Result<ExportFormat, Fail> {
    if let Some(f) = fmt_opt {
        return ExportFormat::from_name(f).ok_or_else(|| Fail(format!("Unknown export format: {f}")));
    }
    let ext = std::path::Path::new(file).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "ckan" => Ok(ExportFormat::Ckan),
        "txt" => Ok(ExportFormat::Text),
        "md" => Ok(ExportFormat::Markdown),
        "csv" => Ok(ExportFormat::Csv),
        "tsv" => Ok(ExportFormat::Tsv),
        _ => Err(Fail(format!("Cannot tell the format from \"{file}\"; use --format ckan|text|markdown|bbcode|csv|tsv"))),
    }
}

pub fn export(g: &Globals, a: &[String]) -> Res {
    let p = args::parse(a, &[value(&["--format", "-f"])])?;
    let file = need(&p.pos, 0, "file")?;
    let f = export_format(p.value("--format"), file)?;
    let s = open_session(g)?;
    let text = modlist::export(f, &s.instance, &s.registry, &s.index);
    if file == "-" {
        term::write_out(&text);
    } else {
        std::fs::write(file, text.as_bytes()).map_err(|e| Fail(format!("{file}: {e}")))?;
        out!("Exported {} to {file}", f.label());
    }
    Ok(())
}

// ---------------------------------------------------------------- show

/// Identifier in the index or registry that equals `id`, ignoring case if no exact match.
pub fn canonical_id(s: &Session, id: &str) -> String {
    if s.index.modules.contains_key(id) || s.registry.installed_modules.contains_key(id) {
        return id.to_string();
    }
    let lower = id.to_lowercase();
    s.registry
        .installed_modules
        .keys()
        .chain(s.index.modules.keys())
        .find(|k| k.to_lowercase() == lower)
        .cloned()
        .unwrap_or_else(|| id.to_string())
}

pub fn show(g: &Globals, a: &[String]) -> Res {
    let p = args::parse(a, &[flag(&["--with-versions"])])?;
    if p.pos.is_empty() {
        return Err(Fail("Missing argument: identifier".into()));
    }
    let s = open_session(g)?;
    let mut failed = false;
    for name in &p.pos {
        let id = canonical_id(&s, name);
        let mut text = String::new();
        if let Some(im) = s.registry.installed(&id) {
            let files: Vec<String> = im.installed_files.keys().cloned().collect();
            text.push_str(&fmt::show_module(&im.source_module, Some(&files)));
        } else if let Some(m) = pick_available(g, &s, name, &id) {
            text.push_str(&fmt::show_module(&m, None));
        } else {
            failed = true;
            continue;
        }
        if p.flag("--with-versions") {
            let mods: Vec<&Module> = s.index.versions(&id).iter().map(|m| m.as_ref()).collect();
            if !mods.is_empty() {
                text.push('\n');
                text.push_str(&fmt::version_table(&mods));
            }
        }
        text.push('\n');
        term::write_out(&text);
    }
    if failed { Err(Fail(String::new())) } else { Ok(()) }
}

/// The module to show for a name that is not installed: the latest compatible version by
/// identifier or display name, else a close match from a search.
fn pick_available(g: &Globals, s: &Session, name: &str, id: &str) -> Option<Arc<Module>> {
    let stab = s.config.hangar.stability;
    if let Some(m) = s.index.latest(id, Some(&s.criteria), stab) {
        return Some(Arc::clone(m));
    }
    let named = s
        .index
        .modules
        .values()
        .filter_map(|v| v.first())
        .find(|m| m.name == name)
        .and_then(|m| s.index.latest(&m.identifier, Some(&s.criteria), stab));
    if let Some(m) = named {
        return Some(Arc::clone(m));
    }
    let crit: Vec<String> = s.criteria.versions.iter().map(GameVersion::to_string).collect();
    out!("{name} not installed or compatible with KSP {}", crit.join(", "));
    if let Some(m) = s.index.versions(id).first() {
        out!("Showing the newest version, which is not compatible:\n");
        return Some(Arc::clone(m));
    }
    out!("Looking for close matches in compatible mods...\n");
    let term = fmt::searchable(name);
    let (compat, _) = split_compatible(s);
    let mut found: Vec<Arc<Module>> = compat.into_iter().filter(|m| fmt::search_matches(m, &term, "")).collect();
    found.sort_by(by_name);
    match found.len() {
        0 => {
            warn!("No close matches found");
            None
        }
        1 => {
            let m = found.pop()?;
            out!("Found 1 close match: {}\n", m.display_name());
            Some(m)
        }
        n => {
            out!("Close matches:");
            for (i, m) in found.iter().enumerate() {
                out!("{:>3}) {} ({})", i + 1, m.display_name(), m.identifier);
            }
            if g.headless {
                return None;
            }
            let k = term::choose_number("Show which mod?", n)?;
            found.get(k).cloned()
        }
    }
}

// ---------------------------------------------------------------- scan / mark

pub fn scan(g: &Globals, a: &[String]) -> Res {
    args::parse(a, &[])?;
    let mut s = open_session(g)?;
    if s.rescan()? {
        out!("Found changes in unmanaged DLLs or DLC; registry saved.");
    } else {
        out!("No changes found.");
    }
    Ok(())
}

pub fn mark(g: &Globals, a: &[String]) -> Res {
    let p = args::parse(a, &[])?;
    let auto = match p.pos.first().map(String::as_str) {
        Some("auto") => true,
        Some("user") => false,
        Some(other) => return Err(Fail(format!("Unknown command: mark {other}"))),
        None => return Err(Fail("Missing argument: auto or user".into())),
    };
    let ids = p.pos.get(1..).unwrap_or(&[]);
    if ids.is_empty() {
        return Err(Fail("Missing argument: identifier".into()));
    }
    let word = if auto { "auto-installed" } else { "user-selected" };
    let mut s = open_session(g)?;
    let mut changed = false;
    for name in ids {
        let id = canonical_id(&s, name);
        let Some(im) = s.registry.installed(&id) else {
            return Err(Fail(format!("{name} is not installed")));
        };
        if im.source_module.is_dlc() {
            return Err(Fail(format!("Can't mark expansion '{id}' as {word}")));
        }
        if im.auto_installed == auto {
            out!("{id} is already marked as {word}");
            continue;
        }
        out!("Marking {id} as {word}...");
        s.set_auto_installed(&id, auto)?;
        changed = true;
    }
    if changed {
        out!("Changes made!");
    }
    Ok(())
}

// ---------------------------------------------------------------- compat

fn parse_versions(list: &[String]) -> Result<Vec<GameVersion>, Fail> {
    let bad: Vec<&str> = list.iter().filter(|v| GameVersion::parse(v).is_none_or(|g| g.is_any())).map(String::as_str).collect();
    if !bad.is_empty() {
        return Err(Fail(format!("Invalid game versions: {}", bad.join(", "))));
    }
    Ok(list.iter().filter_map(|v| GameVersion::parse(v)).collect())
}

pub fn compat(g: &Globals, a: &[String]) -> Res {
    let sub = a.first().map(String::as_str).unwrap_or("list");
    let p = args::parse(a.get(1..).unwrap_or(&[]), &[])?;
    let mut s = open_session(g)?;
    let actual = s.instance.version;
    let mut list = s.instance.compatible_versions();
    match sub {
        "list" => {
            let mut rows: Vec<[String; 2]> = Vec::new();
            if let Some(v) = actual {
                rows.push([v.to_string(), "Actual".into()]);
            }
            let mut extra = list.clone();
            extra.sort_by(|a, b| b.cmp(a));
            for v in extra.iter().filter(|v| Some(**v) != actual) {
                rows.push([v.to_string(), String::new()]);
            }
            term::write_out(&fmt::table(&["Version", "Actual"], &rows));
            return Ok(());
        }
        "add" => {
            if p.pos.is_empty() {
                return Err(Fail("No game versions specified!".into()));
            }
            for v in parse_versions(&p.pos)? {
                if !list.contains(&v) {
                    list.push(v);
                }
            }
        }
        "forget" => {
            if p.pos.is_empty() {
                return Err(Fail("No game versions specified!".into()));
            }
            for v in parse_versions(&p.pos)? {
                if Some(v) == actual {
                    return Err(Fail(format!("Cannot forget actual game version: {v}")));
                }
                list.retain(|x| *x != v);
            }
        }
        "set" => {
            if p.pos.is_empty() {
                return Err(Fail("No game versions specified!".into()));
            }
            list = parse_versions(&p.pos)?;
        }
        "clear" => list.clear(),
        other => return Err(Fail(format!("Unknown command: compat {other}"))),
    }
    s.set_compatible_versions(&list)?;
    list.sort();
    let shown: Vec<String> = list.iter().map(GameVersion::to_string).collect();
    out!("Compatible game versions: {}", if shown.is_empty() { "(none)".into() } else { shown.join(", ") });
    Ok(())
}

// ---------------------------------------------------------------- repo

pub fn repo(g: &Globals, a: &[String]) -> Res {
    let sub = a.first().map(String::as_str).unwrap_or("list");
    let p = args::parse(a.get(1..).unwrap_or(&[]), &[])?;
    let mut s = open_session(g)?;
    let mut repos = s.registry.repositories();
    match sub {
        "list" => {
            let rows: Vec<[String; 3]> = repos.iter().map(|r| [r.priority.to_string(), r.name.clone(), r.uri.clone()]).collect();
            term::write_out(&fmt::table(&["Priority", "Name", "URL"], &rows));
            return Ok(());
        }
        "add" => {
            let name = need(&p.pos, 0, "name")?;
            let uri = match p.pos.get(1) {
                Some(u) => u.clone(),
                None => crate::repo::fetch_repository_list()?
                    .into_iter()
                    .find(|r| r.name == name)
                    .map(|r| r.uri)
                    .ok_or_else(|| Fail(format!("Name {name} not found in master list, please provide name and uri")))?,
            };
            if repos.iter().any(|r| r.name == name) {
                return Err(Fail(format!("Repository with name \"{name}\" already exists, aborting")));
            }
            if repos.iter().any(|r| r.uri == uri) {
                return Err(Fail(format!("Repository with URL \"{uri}\" already exists, aborting")));
            }
            let priority = repos.iter().map(|r| r.priority + 1).max().unwrap_or(0);
            repos.push(Repository { name: name.to_string(), uri: uri.clone(), priority, x_mirror: false, x_comment: None });
            s.set_repositories(repos)?;
            out!("Added repository '{name}' - '{uri}'");
        }
        "forget" => {
            let name = need(&p.pos, 0, "name")?;
            let i = repos
                .iter()
                .position(|r| r.name == name)
                .or_else(|| {
                    let i = repos.iter().position(|r| r.name.eq_ignore_ascii_case(name))?;
                    out!("Removing insensitive match \"{}\"", repos.get(i).map(|r| r.name.as_str()).unwrap_or(""));
                    Some(i)
                })
                .ok_or_else(|| Fail(format!("Couldn't find repository with name \"{name}\", aborting")))?;
            let r = repos.remove(i);
            s.set_repositories(repos)?;
            out!("Successfully removed \"{}\"", r.name);
        }
        "priority" => {
            let name = need(&p.pos, 0, "name")?;
            let n: usize = need(&p.pos, 1, "priority")?
                .parse()
                .map_err(|_| Fail(format!("Invalid priority, allowed values are 0 to {}", repos.len().saturating_sub(1))))?;
            if n >= repos.len() {
                return Err(Fail(format!("Invalid priority: {n}, allowed values are 0 to {}", repos.len().saturating_sub(1))));
            }
            let i = repos
                .iter()
                .position(|r| r.name == name)
                .ok_or_else(|| Fail(format!("Couldn't find repository with name \"{name}\", aborting")))?;
            let r = repos.remove(i);
            repos.insert(n.min(repos.len()), r);
            for (k, r) in repos.iter_mut().enumerate() {
                r.priority = i32::try_from(k).unwrap_or(i32::MAX);
            }
            s.set_repositories(repos)?;
            out!("Set priority of {name} to {n}");
        }
        "default" => {
            let def = Repository::default_repo();
            match repos.iter_mut().find(|r| r.name == def.name) {
                Some(r) => r.uri = def.uri.clone(),
                None => {
                    for r in repos.iter_mut() {
                        r.priority += 1;
                    }
                    repos.insert(0, def.clone());
                }
            }
            s.set_repositories(repos)?;
            out!("Set {} repository to '{}'", def.name, def.uri);
        }
        other => return Err(Fail(format!("Unknown command: repo {other}"))),
    }
    out!("Run \"hangar update\" to load the repository data.");
    Ok(())
}

// ---------------------------------------------------------------- play

pub fn play(g: &Globals, a: &[String]) -> Res {
    let p = args::parse(a, &[flag(&["--no-wait"])])?;
    let s = open_session(g)?;
    let mut bad: Vec<String> = s
        .registry
        .installed_mods()
        .filter(|im| !im.source_module.is_compatible(&s.criteria))
        .map(|im| format!("{} {}", im.source_module.identifier, im.source_module.version))
        .collect();
    bad.sort();
    if !bad.is_empty() {
        out!("These installed mods are not compatible with {}:", game_line(&s));
        for b in &bad {
            out!("  - {b}");
        }
        if !term::confirm(g, "Launch anyway?") {
            return Err(Fail("Launch cancelled".into()));
        }
    }
    let inst = s.instance.clone();
    // Release the registry lock while the game runs.
    drop(s);
    let mut child = inst.launch()?;
    out!("Started {} (pid {})", inst.launch_binary().display(), child.id());
    if p.flag("--no-wait") {
        return Ok(());
    }
    let start = std::time::Instant::now();
    let _ = child.wait();
    let played = start.elapsed();
    if let Err(e) = inst.add_playtime(played) {
        warn!("Could not save play time: {e}");
    }
    out!("Played {:.1} hours.", played.as_secs_f64() / 3600.0);
    Ok(())
}
