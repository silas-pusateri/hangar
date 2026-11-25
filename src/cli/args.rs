//! Hand-written argument parser: global options, then per-command flags and options.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// Options that apply to every command. They may appear anywhere before `--`.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Globals {
    pub instance: Option<String>,
    pub gamedir: Option<PathBuf>,
    /// Never ask: y/n questions answer yes, choices fail, optional mods are not added.
    pub headless: bool,
    /// Answer yes to y/n questions.
    pub yes: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// No value.
    Flag,
    /// One value (`--x v` or `--x=v`); a repeat replaces it.
    Value,
    /// One or more values: all following args up to the next option.
    Multi,
}

/// One option. `names[0]` is the key used to look it up.
#[derive(Debug, Clone, Copy)]
pub struct Opt {
    pub names: &'static [&'static str],
    pub kind: Kind,
}

pub const fn flag(names: &'static [&'static str]) -> Opt {
    Opt { names, kind: Kind::Flag }
}

pub const fn value(names: &'static [&'static str]) -> Opt {
    Opt { names, kind: Kind::Value }
}

pub const fn multi(names: &'static [&'static str]) -> Opt {
    Opt { names, kind: Kind::Multi }
}

#[derive(Debug, Default)]
pub struct Parsed {
    pub pos: Vec<String>,
    flags: HashSet<&'static str>,
    vals: HashMap<&'static str, Vec<String>>,
}

impl Parsed {
    pub fn flag(&self, key: &str) -> bool {
        self.flags.contains(key)
    }

    pub fn value(&self, key: &str) -> Option<&str> {
        self.vals.get(key).and_then(|v| v.last()).map(String::as_str)
    }

    pub fn values(&self, key: &str) -> &[String] {
        self.vals.get(key).map(Vec::as_slice).unwrap_or(&[])
    }
}

/// True for `-x`, `--xyz`; false for `-`, `-1`, `-0.5` (negative numbers are values).
fn is_option(a: &str) -> bool {
    let Some(rest) = a.strip_prefix('-') else { return false };
    !rest.is_empty() && !rest.starts_with(|c: char| c.is_ascii_digit() || c == '.')
}

/// Take the global options out of `args`. Returns the globals and the rest (command first).
pub fn split_globals(args: Vec<String>) -> Result<(Globals, Vec<String>), String> {
    let mut g = Globals::default();
    let mut rest = Vec::with_capacity(args.len());
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        if a == "--" {
            rest.push(a);
            rest.extend(it.by_ref());
            break;
        }
        let (name, inline) = match a.split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n.to_string(), Some(v.to_string())),
            _ => (a.clone(), None),
        };
        match name.as_str() {
            "--headless" if inline.is_none() => g.headless = true,
            "-y" | "--yes" if inline.is_none() => g.yes = true,
            "--instance" | "--gamedir" => {
                let v = match inline {
                    Some(v) => v,
                    None => it.next().ok_or_else(|| format!("{name} needs a value"))?,
                };
                if v.is_empty() {
                    return Err(format!("{name} needs a value"));
                }
                if name == "--instance" {
                    g.instance = Some(v);
                } else {
                    g.gamedir = Some(PathBuf::from(v));
                }
            }
            _ => rest.push(a),
        }
    }
    if g.instance.is_some() && g.gamedir.is_some() {
        return Err("use --instance or --gamedir, not both".into());
    }
    Ok((g, rest))
}

/// Parse command arguments against `spec`. Unknown options are errors.
pub fn parse(args: &[String], spec: &[Opt]) -> Result<Parsed, String> {
    let find = |n: &str| spec.iter().find(|o| o.names.contains(&n));
    let mut p = Parsed::default();
    let mut i = 0;
    while let Some(a) = args.get(i) {
        i += 1;
        if a == "--" {
            p.pos.extend(args.iter().skip(i).cloned());
            break;
        }
        if !is_option(a) {
            p.pos.push(a.clone());
            continue;
        }
        let (name, inline) = match a.split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n, Some(v.to_string())),
            _ => (a.as_str(), None),
        };
        let Some(o) = find(name) else {
            return Err(format!("unknown option {name}"));
        };
        let key = o.names.first().copied().unwrap_or("");
        match o.kind {
            Kind::Flag => {
                if inline.is_some() {
                    return Err(format!("{name} takes no value"));
                }
                p.flags.insert(key);
            }
            Kind::Value => {
                let v = match inline {
                    Some(v) => v,
                    None => {
                        let v = args.get(i).ok_or_else(|| format!("{name} needs a value"))?;
                        i += 1;
                        v.clone()
                    }
                };
                p.vals.insert(key, vec![v]);
            }
            Kind::Multi => {
                let list = p.vals.entry(key).or_default();
                let before = list.len();
                if let Some(v) = inline {
                    list.push(v);
                }
                while let Some(v) = args.get(i).filter(|v| !is_option(v) && *v != "--") {
                    list.push(v.clone());
                    i += 1;
                }
                if list.len() == before {
                    return Err(format!("{name} needs at least one value"));
                }
            }
        }
    }
    Ok(p)
}

/// `id=version` (CKAN regex `^(?<mod>[^=]*)=(?<version>.*)$`).
pub fn split_id_version(s: &str) -> (&str, Option<&str>) {
    match s.split_once('=') {
        Some((id, v)) => (id, Some(v)),
        None => (s, None),
    }
}
