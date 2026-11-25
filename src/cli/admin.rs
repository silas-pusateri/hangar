//! Commands that need no game session: compare, instance, cache, label, repo available.

use std::path::{Path, PathBuf};

use crate::config::{Config, InstanceEntry, Label, LabelList};
use crate::install::DownloadCache;
use crate::instance::Instance;
use crate::meta::GameVersion;

use super::args::{self, Globals, flag, value};
use super::{Fail, Res, fmt, term};

fn need<'a>(pos: &'a [String], i: usize, what: &str) -> Result<&'a str, Fail> {
    pos.get(i).map(String::as_str).ok_or_else(|| Fail(format!("Missing argument: {what}")))
}

fn no_extra(pos: &[String], n: usize) -> Res {
    match pos.get(n) {
        Some(x) => Err(Fail(format!("Unexpected argument: {x}"))),
        None => Ok(()),
    }
}

pub fn compare(a: &[String]) -> Res {
    let p = args::parse(a, &[flag(&["--machine-readable"])])?;
    let v1 = need(&p.pos, 0, "v1")?;
    let v2 = need(&p.pos, 1, "v2")?;
    no_extra(&p.pos, 2)?;
    out!("{}", fmt::compare(v1, v2));
    Ok(())
}

// ---------------------------------------------------------------- instance

pub fn instance(_g: &Globals, a: &[String]) -> Res {
    let sub = a.first().map(String::as_str).unwrap_or("list");
    let rest = a.get(1..).unwrap_or(&[]);
    match sub {
        "list" => instance_list(rest),
        "add" => instance_add(rest),
        "forget" => instance_forget(rest),
        "rename" => instance_rename(rest),
        "default" => instance_default(rest),
        "fake" => instance_fake(rest),
        other => Err(Fail(format!("Unknown command: instance {other}"))),
    }
}

fn instance_list(a: &[String]) -> Res {
    args::parse(a, &[])?;
    let config = Config::load();
    if config.game_instances.is_empty() {
        out!("No game instances registered. Add one with \"hangar instance add <name> <path>\".");
        return Ok(());
    }
    let builds = crate::instance::embedded_builds();
    let rows: Vec<[String; 5]> = config
        .game_instances
        .iter()
        .map(|e| {
            // Read-only: detect the version from the game files, no registry lock.
            let ver = Instance::open(&e.name, Path::new(&e.path), builds)
                .ok()
                .and_then(|i| i.version)
                .map(|v| v.to_string())
                .unwrap_or_else(|| "<NONE>".into());
            let default = if config.auto_start_instance.as_deref() == Some(e.name.as_str()) { "Yes" } else { "No" };
            [e.name.clone(), e.game.clone(), ver, default.to_string(), e.path.clone()]
        })
        .collect();
    term::write_out(&fmt::table(&["Name", "Game", "Version", "Default", "Path"], &rows));
    Ok(())
}

fn abs_path(p: &str) -> PathBuf {
    let p = Path::new(p);
    std::fs::canonicalize(p).or_else(|_| std::path::absolute(p)).unwrap_or_else(|_| p.to_path_buf())
}

fn add_entry(config: &mut Config, name: &str, path: &Path) -> Res {
    if config.instance(name).is_some() {
        return Err(Fail(format!("Install with name \"{name}\" already exists, aborting")));
    }
    config.game_instances.push(InstanceEntry {
        name: name.to_string(),
        path: path.display().to_string(),
        game: "KSP".into(),
    });
    Ok(())
}

fn instance_add(a: &[String]) -> Res {
    let p = args::parse(a, &[])?;
    let name = need(&p.pos, 0, "name")?;
    let path = abs_path(need(&p.pos, 1, "path")?);
    no_extra(&p.pos, 2)?;
    if !crate::instance::is_valid_game_dir(&path) {
        return Err(Fail(format!("Sorry, {} does not appear to be a game instance", path.display())));
    }
    let mut config = Config::load();
    if let Some(e) = config.game_instances.iter().find(|e| abs_path(&e.path) == path) {
        return Err(Fail(format!("{} is already registered as \"{}\"", path.display(), e.name)));
    }
    add_entry(&mut config, name, &path)?;
    config.save()?;
    out!("Added \"{name}\" with root \"{}\" to known installs", path.display());
    Ok(())
}

fn find_entry(config: &Config, name: &str) -> Result<usize, Fail> {
    config
        .game_instances
        .iter()
        .position(|e| e.name == name)
        .ok_or_else(|| Fail(format!("Couldn't find install with name \"{name}\", aborting")))
}

fn instance_forget(a: &[String]) -> Res {
    let p = args::parse(a, &[])?;
    let name = need(&p.pos, 0, "name")?;
    no_extra(&p.pos, 1)?;
    let mut config = Config::load();
    let i = find_entry(&config, name)?;
    config.game_instances.remove(i);
    if config.auto_start_instance.as_deref() == Some(name) {
        config.auto_start_instance = None;
    }
    config.save()?;
    out!("Successfully removed \"{name}\"");
    Ok(())
}

fn instance_rename(a: &[String]) -> Res {
    let p = args::parse(a, &[])?;
    let old = need(&p.pos, 0, "old name")?;
    let new = need(&p.pos, 1, "new name")?;
    no_extra(&p.pos, 2)?;
    let mut config = Config::load();
    let i = find_entry(&config, old)?;
    if old != new && config.instance(new).is_some() {
        return Err(Fail(format!("This instance name is already taken: {new}")));
    }
    if let Some(e) = config.game_instances.get_mut(i) {
        e.name = new.to_string();
    }
    if config.auto_start_instance.as_deref() == Some(old) {
        config.auto_start_instance = Some(new.to_string());
    }
    config.save()?;
    out!("Successfully renamed \"{old}\" to \"{new}\"");
    Ok(())
}

fn instance_default(a: &[String]) -> Res {
    let p = args::parse(a, &[])?;
    let Some(name) = p.pos.first() else {
        instance_list(&[])?;
        return Err(Fail("Argument missing, please select from the list above".into()));
    };
    no_extra(&p.pos, 1)?;
    let mut config = Config::load();
    find_entry(&config, name)?;
    config.auto_start_instance = Some(name.clone());
    config.save()?;
    out!("Successfully set \"{name}\" as the default game instance");
    Ok(())
}

/// A game version for a fake instance. Without a build number, the newest known build of
/// that release is used (so the fake dir has a real build id).
pub fn fake_version(s: &str) -> Option<GameVersion> {
    let v = GameVersion::parse(s)?;
    if v.is_any() || v.minor < 0 {
        return None;
    }
    if v.build >= 0 {
        return Some(v);
    }
    let best = crate::instance::embedded_builds()
        .values()
        .filter(|b| b.major == v.major && b.minor == v.minor && (v.patch < 0 || b.patch == v.patch))
        .max()
        .copied();
    Some(best.unwrap_or(v))
}

fn instance_fake(a: &[String]) -> Res {
    let spec = [value(&["--MakingHistory"]), value(&["--BreakingGround"]), flag(&["--set-default"]), value(&["--game"])];
    let p = args::parse(a, &spec)?;
    let name = need(&p.pos, 0, "name")?;
    let path = need(&p.pos, 1, "path")?;
    let ver_text = need(&p.pos, 2, "version")?;
    no_extra(&p.pos, 3)?;
    if let Some(game) = p.value("--game").filter(|g| !g.eq_ignore_ascii_case("KSP")) {
        return Err(Fail(format!("No such game {game}!")));
    }
    let version = fake_version(ver_text).ok_or_else(|| {
        Fail("Please check the version argument - Format it like Maj.Min.Patch[.Build] - e.g. 1.6.0 or 1.2.2.1622".into())
    })?;
    let mut dlc: Vec<(&str, &str)> = Vec::new();
    for (key, dlc_name) in [("--MakingHistory", "MakingHistory"), ("--BreakingGround", "BreakingGround")] {
        if let Some(v) = p.value(key) {
            if GameVersion::parse(v).is_none_or(|g| g.is_any()) {
                return Err(Fail(format!("Please check the {dlc_name} DLC version argument - e.g. 1.1.0")));
            }
            dlc.push((dlc_name, v));
        }
    }
    let mut config = Config::load();
    if config.instance(name).is_some() {
        return Err(Fail(format!("This instance name is already taken: {name}")));
    }
    let dir = std::path::absolute(path).unwrap_or_else(|_| PathBuf::from(path));
    out!("Creating new fake game instance {name} at {} with version {version}", dir.display());
    crate::instance::create_fake(&dir, &version, &dlc)?;
    add_entry(&mut config, name, &abs_path(path))?;
    if p.flag("--set-default") {
        out!("Setting new instance to default...");
        config.auto_start_instance = Some(name.to_string());
    }
    config.save()?;
    out!("--Done--");
    Ok(())
}

// ---------------------------------------------------------------- cache

const MIB: u64 = 1024 * 1024;

pub fn cache(a: &[String]) -> Res {
    let sub = a.first().map(String::as_str).unwrap_or("list");
    let p = args::parse(a.get(1..).unwrap_or(&[]), &[])?;
    let mut config = Config::load();
    match sub {
        "list" => {
            no_extra(&p.pos, 0)?;
            let dir = config.cache_dir();
            let cache = DownloadCache::new(dir.clone());
            let files = cache.list();
            let total: u64 = files.iter().map(|f| f.size).sum();
            out!("Download cache: {}", dir.display());
            out!("{} files, {}", files.len(), crate::util::human_size(total));
            Ok(())
        }
        "set" => {
            let path = abs_path(need(&p.pos, 0, "path")?);
            no_extra(&p.pos, 1)?;
            if !path.is_dir() {
                return Err(Fail(format!("Invalid path: {}", path.display())));
            }
            config.download_cache_dir = Some(path.display().to_string());
            config.save()?;
            out!("Download cache set to {}", path.display());
            Ok(())
        }
        "clear" => {
            no_extra(&p.pos, 0)?;
            DownloadCache::new(config.cache_dir()).trim(0)?;
            out!("Download cache cleared");
            Ok(())
        }
        "reset" => {
            no_extra(&p.pos, 0)?;
            config.download_cache_dir = None;
            config.save()?;
            out!("Download cache reset to {}", config.cache_dir().display());
            Ok(())
        }
        "showlimit" => {
            no_extra(&p.pos, 0)?;
            out!("{}", limit_text(config.cache_size_limit));
            Ok(())
        }
        "setlimit" => {
            let n: i64 = need(&p.pos, 0, "MiB")?.parse().map_err(|_| Fail("The limit must be a whole number of MiB".into()))?;
            no_extra(&p.pos, 1)?;
            config.cache_size_limit = u64::try_from(n).ok().map(|n| n.saturating_mul(MIB));
            config.save()?;
            out!("{}", limit_text(config.cache_size_limit));
            Ok(())
        }
        other => Err(Fail(format!("Unknown command: cache {other}"))),
    }
}

pub fn limit_text(limit: Option<u64>) -> String {
    match limit {
        Some(b) => format!("{} MiB", b / MIB),
        None => "Unlimited".into(),
    }
}

// ---------------------------------------------------------------- labels

fn label_index(list: &LabelList, name: &str) -> Result<usize, Fail> {
    list.labels
        .iter()
        .position(|l| l.name == name)
        .or_else(|| list.labels.iter().position(|l| l.name.eq_ignore_ascii_case(name)))
        .ok_or_else(|| Fail(format!("No label named \"{name}\"")))
}

fn label_flags(l: &Label) -> String {
    let mut f = Vec::new();
    if l.hide {
        f.push("hide");
    }
    if l.hold_version {
        f.push("hold version");
    }
    if l.notify_on_change {
        f.push("notify on change");
    }
    if l.remove_on_change {
        f.push("remove on change");
    }
    if l.alert_on_install {
        f.push("alert on install");
    }
    if l.remove_on_install {
        f.push("remove on install");
    }
    if l.ignore_missing_files {
        f.push("ignore missing files");
    }
    f.join(", ")
}

pub fn label(a: &[String]) -> Res {
    let sub = a.first().map(String::as_str).unwrap_or("list");
    let spec = [flag(&["--hide"]), flag(&["--hold"]), value(&["--instance-name"]), value(&["--color"])];
    let p = args::parse(a.get(1..).unwrap_or(&[]), &spec)?;
    let mut labels = LabelList::load();
    match sub {
        "list" => {
            no_extra(&p.pos, 0)?;
            let rows: Vec<[String; 4]> = labels
                .labels
                .iter()
                .map(|l| {
                    let n = l.module_identifiers_by_game.get("KSP").map_or(0, Vec::len);
                    [l.name.clone(), l.instance_name.clone().unwrap_or_else(|| "(all)".into()), n.to_string(), label_flags(l)]
                })
                .collect();
            term::write_out(&fmt::table(&["Name", "Instance", "Mods", "Flags"], &rows));
            Ok(())
        }
        "add" => {
            let name = need(&p.pos, 0, "name")?;
            no_extra(&p.pos, 1)?;
            if labels.labels.iter().any(|l| l.name == name) {
                return Err(Fail(format!("A label named \"{name}\" already exists")));
            }
            labels.labels.push(Label {
                name: name.to_string(),
                color: p.value("--color").map(str::to_string),
                instance_name: p.value("--instance-name").map(str::to_string),
                hide: p.flag("--hide"),
                hold_version: p.flag("--hold"),
                ..Label::default()
            });
            labels.save()?;
            out!("Added label \"{name}\"");
            Ok(())
        }
        "remove" => {
            let name = need(&p.pos, 0, "name")?;
            no_extra(&p.pos, 1)?;
            let i = label_index(&labels, name)?;
            let l = labels.labels.remove(i);
            labels.save()?;
            out!("Removed label \"{}\"", l.name);
            Ok(())
        }
        "show" => {
            let name = need(&p.pos, 0, "name")?;
            no_extra(&p.pos, 1)?;
            let i = label_index(&labels, name)?;
            if let Some(l) = labels.labels.get(i) {
                let ids = l.module_identifiers_by_game.get("KSP").map(Vec::as_slice).unwrap_or(&[]);
                out!("{} ({} mods){}", l.name, ids.len(), Some(label_flags(l)).filter(|f| !f.is_empty()).map(|f| format!(" [{f}]")).unwrap_or_default());
                for id in ids {
                    out!("  - {id}");
                }
            }
            Ok(())
        }
        "toggle" => {
            let name = need(&p.pos, 0, "name")?;
            let id = need(&p.pos, 1, "identifier")?;
            no_extra(&p.pos, 2)?;
            let i = label_index(&labels, name)?;
            labels.toggle(i, id);
            let now_in = labels.labels.get(i).is_some_and(|l| l.contains(id));
            labels.save()?;
            let lname = labels.labels.get(i).map(|l| l.name.as_str()).unwrap_or(name);
            if now_in {
                out!("Added {id} to label \"{lname}\"");
            } else {
                out!("Removed {id} from label \"{lname}\"");
            }
            Ok(())
        }
        other => Err(Fail(format!("Unknown command: label {other}"))),
    }
}

// ---------------------------------------------------------------- repo available

pub fn repo_available() -> Res {
    let list = crate::repo::fetch_repository_list()?;
    out!("Listing all (canonical) available CKAN repositories:");
    let w = list.iter().map(|r| r.name.chars().count()).max().unwrap_or(0);
    for r in &list {
        out!("  {:<w$}: {}", r.name, r.uri);
    }
    Ok(())
}
