//! Pure text formatting for command output (unit tested).

use std::fmt::Write as _;

use crate::meta::{GameVersion, Module};
use crate::resolve::{ChangeKind, Changeset, OptionalKind, OptionalMod};

/// State of one installed row for `list` (CKAN List.cs bullets).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowFacts<'a> {
    /// Unmanaged DLL or DLC.
    pub unmanaged: bool,
    /// Some version of the identifier is in a repository.
    pub in_repo: bool,
    /// A compatible version is available.
    pub has_compatible: bool,
    pub upgradeable: bool,
    pub auto_installed: bool,
    /// `replaced_by` target (identifier, version) that is compatible.
    pub replacement: Option<(&'a str, &'a str)>,
}

/// Bullet and row text: `^ - + X > A ?`.
pub fn list_row(id: &str, version: &str, f: &RowFacts) -> String {
    let info = if version.is_empty() { id.to_string() } else { format!("{id} {version}") };
    let (bullet, info) = if f.unmanaged {
        ('A', info)
    } else if !f.in_repo {
        ('?', info)
    } else if f.upgradeable && f.has_compatible {
        ('^', info)
    } else if let Some((rid, rver)) = f.replacement {
        ('>', format!("{info} > {rid} {rver}"))
    } else if !f.has_compatible {
        ('X', info)
    } else if f.auto_installed {
        ('+', info)
    } else {
        ('-', info)
    };
    format!("{bullet} {info}")
}

pub const LIST_LEGEND: &str =
    "Legend: -: Up to date. +: Auto-installed. X: Incompatible. ^: Upgradable. >: Replaceable.\n        A: Autodetected. ?: Unknown.";

fn kind_word(k: ChangeKind) -> &'static str {
    match k {
        ChangeKind::Install => "Install",
        ChangeKind::Remove => "Remove",
        ChangeKind::Upgrade => "Upgrade",
        ChangeKind::Replace => "Replace",
    }
}

/// The change set as shown before apply. `cached` tells which downloads are already in the
/// cache (they do not count toward the download size).
pub fn changeset(cs: &Changeset, cached: &dyn Fn(&Module) -> bool) -> String {
    let mut s = String::from("Changes:\n");
    for c in &cs.changes {
        let m = &c.module;
        let what = match c.kind {
            ChangeKind::Upgrade => match &c.old_version {
                Some(old) => format!("{} {old} -> {}", m.identifier, m.version),
                None => format!("{} {}", m.identifier, m.version),
            },
            ChangeKind::Replace => {
                let old = match (&c.replaces, &c.old_version) {
                    (Some(r), Some(v)) => format!("{r} {v}"),
                    (Some(r), None) => r.clone(),
                    _ => String::from("?"),
                };
                format!("{old} with {} {}", m.identifier, m.version)
            }
            _ => format!("{} {}", m.identifier, m.version),
        };
        let reasons: Vec<String> = c.reasons.iter().map(ToString::to_string).collect();
        let _ = write!(s, "  {:<8} {what}", kind_word(c.kind));
        if !reasons.is_empty() {
            let _ = write!(s, " ({})", reasons.join("; "));
        }
        s.push('\n');
    }
    let size: u64 = cs.downloads().iter().filter(|m| !cached(m)).map(|m| m.download_size).sum();
    if size > 0 {
        let _ = writeln!(s, "Download size: {}", crate::util::human_size(size));
    }
    s
}

/// Numbered list of optional mods for the recommendations question.
pub fn optional_list(opts: &[OptionalMod]) -> String {
    let mut s = String::new();
    for (i, o) in opts.iter().enumerate() {
        let kind = match o.kind {
            OptionalKind::Recommends => "recommended by",
            OptionalKind::Suggests => "suggested by",
            OptionalKind::Supports => "supports",
        };
        let _ = writeln!(
            s,
            "{:>3}) {} {} - {} ({kind} {})",
            i + 1,
            o.module.identifier,
            o.module.version,
            o.module.abstract_,
            o.sources.join(", ")
        );
    }
    s
}

/// CKAN GameVersionRange.VersionSpan text for one module's game versions.
pub fn game_span(m: &Module) -> String {
    let lo = m.earliest_game_version();
    let hi = m.latest_game_version();
    span(&lo, &hi)
}

pub fn span(lo: &GameVersion, hi: &GameVersion) -> String {
    match (lo.is_any(), hi.is_any()) {
        (true, true) => "KSP all versions".into(),
        (true, false) => format!("KSP {hi} and earlier"),
        (false, true) => format!("KSP {lo} and later"),
        (false, false) if lo == hi => format!("KSP {lo}"),
        (false, false) => format!("KSP {lo}-{hi}"),
    }
}

/// `Version  Game Versions` table (show --with-versions).
pub fn version_table(mods: &[&Module]) -> String {
    let rows: Vec<[String; 2]> = mods.iter().map(|m| [m.version.to_string(), game_span(m)]).collect();
    table(&["Version", "Game Versions"], &rows)
}

/// Text table: columns padded to the widest cell, two spaces apart, dashes under the header.
pub fn table<const N: usize>(headers: &[&str; N], rows: &[[String; N]]) -> String {
    let mut w: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();
    for r in rows {
        for (k, cell) in r.iter().enumerate() {
            if let Some(x) = w.get_mut(k) {
                *x = (*x).max(cell.chars().count());
            }
        }
    }
    let line = |cells: Vec<&str>| -> String {
        let n = cells.len();
        let mut s = String::new();
        for (k, c) in cells.into_iter().enumerate() {
            if k + 1 == n {
                s.push_str(c);
            } else {
                let width = w.get(k).copied().unwrap_or(0);
                let _ = write!(s, "{c:<width$}  ");
            }
        }
        s.trim_end().to_string() + "\n"
    };
    let mut out = line(headers.to_vec());
    out.push_str(&line(w.iter().map(|n| "-".repeat(*n)).collect::<Vec<_>>().iter().map(String::as_str).collect()));
    for r in rows {
        out.push_str(&line(r.iter().map(String::as_str).collect()));
    }
    out
}

/// `[^\p{L}0-9]` removed, lowercased (CKAN Searchable* fields).
pub fn searchable(s: &str) -> String {
    s.chars().filter(|c| c.is_alphabetic() || c.is_ascii_digit()).flat_map(char::to_lowercase).collect()
}

/// CKAN Search.TermMatchesModule. `term` and `author` must already be `searchable`.
pub fn search_matches(m: &Module, term: &str, author: &str) -> bool {
    let text_ok = term.is_empty()
        || searchable(&m.name).contains(term)
        || searchable(&m.identifier).contains(term)
        || searchable(&m.abstract_).contains(term)
        || m.description.as_deref().is_some_and(|d| searchable(d).contains(term));
    text_ok && (author.is_empty() || m.author.iter().any(|a| searchable(a).contains(author)))
}

/// Resources shown by `show`, in CKAN order: (label, resource key).
pub const SHOW_RESOURCES: [(&str, &str); 13] = [
    ("Home page", "homepage"),
    ("Manual", "manual"),
    ("SpaceDock", "spacedock"),
    ("Repository", "repository"),
    ("Bug tracker", "bugtracker"),
    ("Discussions", "discussions"),
    ("Curse", "curse"),
    ("Store", "store"),
    ("Steam store", "steamstore"),
    ("GOG store", "gogstore"),
    ("Epic Games Store", "epicstore"),
    ("Version file", "remote-avc"),
    ("SWInfo file", "remote-swinfo"),
];

/// CKAN Show output for one module. `files` is the installed file list (installed modules only).
pub fn show_module(m: &Module, files: Option<&[String]>) -> String {
    let mut s = String::new();
    if m.abstract_.is_empty() {
        let _ = writeln!(s, "{}", m.display_name());
    } else {
        let _ = writeln!(s, "{}: {}", m.display_name(), m.abstract_);
    }
    if let Some(d) = m.description.as_deref().filter(|d| !d.is_empty()) {
        let _ = writeln!(s, "\n{d}");
    }
    let _ = writeln!(s, "\nModule info:");
    let _ = writeln!(s, "  Version:\t{}", m.version);
    if m.author.is_empty() {
        let _ = writeln!(s, "  Authors:\tUNKNOWN");
    } else {
        let _ = writeln!(s, "  Authors:\t{}", m.author.join(", "));
    }
    let _ = writeln!(s, "  Status:\t{}", m.release_status.as_str());
    let _ = writeln!(s, "  Licence:\t{}", m.license.join(", "));
    let _ = writeln!(s, "  Game:\t\t{}", game_span(m));
    if !m.tags.is_empty() {
        let _ = writeln!(s, "  Tags: \t{}", m.tags.join(", "));
    }
    if !m.localizations.is_empty() {
        let mut l = m.localizations.to_vec();
        l.sort();
        let _ = writeln!(s, "  Languages:\t{}", l.join(", "));
    }
    let rels = [
        ("Depends", &m.depends),
        ("Recommends", &m.recommends),
        ("Suggests", &m.suggests),
        ("Supports", &m.supports),
        ("Conflicts", &m.conflicts),
    ];
    for (head, list) in rels {
        if !list.is_empty() {
            let _ = writeln!(s, "\n{head}:");
            for r in list {
                let _ = writeln!(s, "  - {r}");
            }
        }
    }
    if !m.provides.is_empty() {
        let _ = writeln!(s, "\nProvides:");
        for p in &m.provides {
            let _ = writeln!(s, "  - {p}");
        }
    }
    if let Some(rb) = &m.replaced_by {
        let _ = writeln!(s, "\nReplaced by: {rb}");
    }
    let res: Vec<(&str, String)> = SHOW_RESOURCES
        .iter()
        .filter_map(|(label, key)| {
            let u = m.resources.get(key)?;
            Some((*label, crate::modlist::export::normalize_uri(u)?))
        })
        .collect();
    if !res.is_empty() {
        let _ = writeln!(s, "\nResources:");
        for (label, u) in res {
            let _ = writeln!(s, "  {label}:\t{u}");
        }
    }
    if let Some(files) = files.filter(|_| !m.is_dlc()) {
        let _ = writeln!(s, "\nShowing {} installed files:", files.len());
        for f in files {
            let _ = writeln!(s, "  - {f}");
        }
    }
    if !m.is_dlc()
        && let Some(url) = m.download.first()
    {
        let _ = writeln!(s, "\nFilename: {}-{}", crate::install::cache::url_hash(url), m.standard_name());
    }
    s
}

/// `compare` result: -1, 0 or 1.
pub fn compare(a: &str, b: &str) -> i32 {
    use crate::meta::ModuleVersion;
    match ModuleVersion::new(a).cmp(&ModuleVersion::new(b)) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}
