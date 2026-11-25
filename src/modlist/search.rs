//! CKAN mod search syntax and list filters.
//!
//! Terms are split on whitespace and ANDed; a leading `-` negates a term.
//! bare words, `@author`, `desc:`, `lic:`, `lang:`, `dep:`, `rec:`, `sug:`, `conf:`, `sup:`,
//! `tag:x` (`tag:` alone = untagged), `label:x` (`label:` alone = no label),
//! `is:`/`not:` + compatible|installed|cached|newly-compatible|upgradeable|replaceable.

use crate::meta::{Module, Relationship};

/// Facts about one row that the search needs besides the module metadata.
#[derive(Debug, Clone, Copy, Default)]
pub struct RowState<'a> {
    pub installed: bool,
    pub cached: bool,
    /// An update is available (CKAN HasUpdate).
    pub upgradeable: bool,
    /// The module has a replaced_by target (CKAN HasReplacement).
    pub replaceable: bool,
    /// CKAN `!GUIMod.IsIncompatible`: a compatible version is available, or (for an installed
    /// row) the installed version itself is compatible. An installed row with neither is
    /// incompatible and shows under the Incompatible filter, as in CKAN.
    pub compatible: bool,
    pub newly_compatible: bool,
    /// Names of labels that contain this module.
    pub labels: &'a [&'a str],
}

/// Filter menu entries (GUIModFilter).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Filter {
    #[default]
    Compatible,
    Installed,
    Upgradeable,
    Replaceable,
    Cached,
    Uncached,
    NewlyCompatible,
    NotInstalled,
    Incompatible,
    All,
}

impl Filter {
    pub const ALL: [Filter; 10] = [
        Filter::Compatible,
        Filter::Installed,
        Filter::Upgradeable,
        Filter::Replaceable,
        Filter::Cached,
        Filter::Uncached,
        Filter::NewlyCompatible,
        Filter::NotInstalled,
        Filter::Incompatible,
        Filter::All,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Filter::Compatible => "Compatible",
            Filter::Installed => "Installed",
            Filter::Upgradeable => "Installed (update available)",
            Filter::Replaceable => "Replaceable",
            Filter::Cached => "Cached",
            Filter::Uncached => "Uncached",
            Filter::NewlyCompatible => "Newly compatible",
            Filter::NotInstalled => "Not installed",
            Filter::Incompatible => "Incompatible",
            Filter::All => "All",
        }
    }

    /// CKAN maps each filter to one search flag (ModSearch.cs, GUIModFilter constructor).
    pub fn matches(self, s: &RowState) -> bool {
        match self {
            Filter::Compatible => s.compatible,
            Filter::Installed => s.installed,
            Filter::Upgradeable => s.upgradeable,
            // MatchesReplaceable: only installed rows count.
            Filter::Replaceable => s.installed && s.replaceable,
            Filter::Cached => s.cached,
            Filter::Uncached => !s.cached,
            Filter::NewlyCompatible => s.newly_compatible,
            Filter::NotInstalled => !s.installed,
            Filter::Incompatible => !s.compatible,
            Filter::All => true,
        }
    }
}

/// One search term: negation flag plus the lowercased text to look for.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Term {
    neg: bool,
    text: String,
}

impl Term {
    /// CKAN ShouldNegateTerm: a leading `-` negates; the rest is lowercased for matching.
    fn new(raw: &str) -> Term {
        match raw.strip_prefix('-') {
            Some(rest) => Term { neg: true, text: rest.to_lowercase() },
            None => Term { neg: false, text: raw.to_lowercase() },
        }
    }
}

/// A parsed search string (GUI/Model/ModSearch.cs).
#[derive(Debug, Clone, Default)]
pub struct ModSearch {
    /// Bare words joined; None when CKAN's Name would be empty.
    name: Option<Term>,
    authors: Vec<Term>,
    description: Option<Term>,
    licenses: Vec<Term>,
    localizations: Vec<Term>,
    depends: Vec<Term>,
    recommends: Vec<Term>,
    suggests: Vec<Term>,
    conflicts: Vec<Term>,
    supports: Vec<Term>,
    /// Raw `tag:` remainders (may start with `-`; empty = untagged).
    tag_names: Vec<String>,
    /// Raw `label:` remainders (may start with `-`; empty = in no label).
    label_names: Vec<String>,
    compatible: Option<bool>,
    installed: Option<bool>,
    cached: Option<bool>,
    newly_compatible: Option<bool>,
    upgradeable: Option<bool>,
    replaceable: Option<bool>,
}

/// CkanModule.nonAlphaNumsAnyLanguage `[^\p{L}0-9]`: keep letters (any language) and ASCII
/// digits. `char::is_alphabetic` is wider than `\p{L}` (it keeps marks and letter numbers),
/// so non-ASCII text goes through the same regex class as CKAN.
fn strip_any_language(s: &str) -> String {
    if s.is_ascii() {
        return s.chars().filter(char::is_ascii_alphanumeric).collect();
    }
    static NON_ALNUM: std::sync::OnceLock<Option<regex::Regex>> = std::sync::OnceLock::new();
    match NON_ALNUM.get_or_init(|| regex::Regex::new(r"[^\p{L}0-9]").ok()) {
        Some(re) => re.replace_all(s, "").into_owned(),
        None => s.chars().filter(|c| c.is_alphabetic() || c.is_ascii_digit()).collect(),
    }
}

/// CKAN TryPrefix: `prefix` or `-prefix`; the remainder keeps the `-`.
fn try_prefix(s: &str, prefix: &str) -> Option<String> {
    if let Some(r) = s.strip_prefix(prefix) {
        return Some(r.to_string());
    }
    s.strip_prefix('-').and_then(|t| t.strip_prefix(prefix)).map(|r| format!("-{r}"))
}

/// CKAN keeps the sign of the joined text and strips everything else that is not a letter or digit.
fn joined_term(joined: &str) -> Option<Term> {
    let (neg, rest) = match joined.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, joined),
    };
    let stripped = strip_any_language(rest);
    if !neg && stripped.trim().is_empty() {
        return None;
    }
    Some(Term { neg, text: stripped.to_lowercase() })
}

/// Case-insensitive prefix test; `needle` is already lowercase.
fn starts_with_ci(hay: &str, needle: &str) -> bool {
    let mut h = hay.chars().flat_map(char::to_lowercase);
    needle.chars().all(|c| h.next() == Some(c))
}

fn contains_ci(hay: &str, needle: &str) -> bool {
    needle.is_empty() || hay.to_lowercase().contains(needle)
}

fn rel_starts_with(r: &Relationship, prefix: &str) -> bool {
    match r {
        Relationship::One(t) => starts_with_ci(&t.name, prefix),
        Relationship::AnyOf { any_of, .. } => any_of.iter().any(|c| rel_starts_with(c, prefix)),
    }
}

/// RelationshipMatch. An empty list counts as CKAN's null list: no match, even when negated.
fn rel_match(rels: &[Relationship], terms: &[Term]) -> bool {
    terms.is_empty() || (!rels.is_empty() && terms.iter().all(|t| t.neg ^ rels.iter().any(|r| rel_starts_with(r, &t.text))))
}

fn spaceless_eq(a: &str, b: &str) -> bool {
    a.replace(' ', "").to_lowercase() == b.replace(' ', "").to_lowercase()
}

impl ModSearch {
    pub fn parse(text: &str) -> ModSearch {
        let mut s = ModSearch::default();
        let mut by_name = String::new();
        let mut by_desc = String::new();
        for piece in text.split_whitespace() {
            if let Some(r) = try_prefix(piece, "@") {
                s.authors.push(Term::new(&r));
            } else if let Some(r) = try_prefix(piece, "desc:") {
                // CKAN joins all desc: terms the same way as bare words.
                if r.starts_with('-') {
                    by_desc.push('-');
                }
                by_desc.push_str(&strip_any_language(r.strip_prefix('-').unwrap_or(&r)));
            } else if let Some(r) = try_prefix(piece, "lic:") {
                s.licenses.push(Term::new(&r));
            } else if let Some(r) = try_prefix(piece, "lang:") {
                s.localizations.push(Term::new(&r));
            } else if let Some(r) = try_prefix(piece, "dep:") {
                s.depends.push(Term::new(&r));
            } else if let Some(r) = try_prefix(piece, "rec:") {
                s.recommends.push(Term::new(&r));
            } else if let Some(r) = try_prefix(piece, "sug:") {
                s.suggests.push(Term::new(&r));
            } else if let Some(r) = try_prefix(piece, "conf:") {
                s.conflicts.push(Term::new(&r));
            } else if let Some(r) = try_prefix(piece, "sup:") {
                s.supports.push(Term::new(&r));
            } else if let Some(r) = try_prefix(piece, "tag:") {
                s.tag_names.push(r);
            } else if let Some(r) = try_prefix(piece, "label:") {
                s.label_names.push(r);
            } else if let Some(r) = try_prefix(piece, "is:") {
                // "-is:x" gives "-x", which matches no suffix: CKAN ignores it.
                s.set_state(&r, true);
            } else if let Some(r) = try_prefix(piece, "not:") {
                s.set_state(&r, false);
            } else {
                let neg = piece.starts_with('-');
                if neg {
                    by_name.push('-');
                }
                by_name.push_str(&strip_any_language(piece.strip_prefix('-').unwrap_or(piece)));
            }
        }
        s.name = joined_term(&by_name);
        s.description = joined_term(&by_desc);
        s
    }

    fn set_state(&mut self, suffix: &str, val: bool) {
        let slot = match suffix {
            "compatible" => &mut self.compatible,
            "installed" => &mut self.installed,
            "cached" => &mut self.cached,
            "newly-compatible" => &mut self.newly_compatible,
            "upgradeable" => &mut self.upgradeable,
            "replaceable" => &mut self.replaceable,
            _ => return,
        };
        *slot = Some(val);
    }

    /// True when the search has no criteria (matches every row).
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.authors.is_empty()
            && self.description.is_none()
            && self.licenses.is_empty()
            && self.localizations.is_empty()
            && self.depends.is_empty()
            && self.recommends.is_empty()
            && self.suggests.is_empty()
            && self.conflicts.is_empty()
            && self.supports.is_empty()
            && self.tag_names.is_empty()
            && self.label_names.is_empty()
            && self.compatible.is_none()
            && self.installed.is_none()
            && self.cached.is_none()
            && self.newly_compatible.is_none()
            && self.upgradeable.is_none()
            && self.replaceable.is_none()
    }

    /// True if the search names this label (`label:x`), so hidden-label rows stay visible.
    /// CKAN compares the raw name exactly; this also accepts the spaceless,
    /// case-insensitive form that `label:` matching uses. A negated `-label:x` does not count.
    pub fn mentions_label(&self, label: &str) -> bool {
        self.label_names.iter().any(|n| !n.starts_with('-') && (n == label || (!n.is_empty() && spaceless_eq(n, label))))
    }

    /// True if the search names this tag (`tag:x`, exact), so hidden-tag rows stay visible.
    #[cfg(test)]
    pub fn mentions_tag(&self, tag: &str) -> bool {
        self.tag_names.iter().any(|n| n == tag)
    }

    pub fn matches(&self, m: &Module, s: &RowState) -> bool {
        self.matches_name(m)
            && self.matches_authors(m)
            && self.matches_description(m)
            && self.matches_licenses(m)
            && self.matches_localizations(m)
            && rel_match(&m.depends, &self.depends)
            && rel_match(&m.recommends, &self.recommends)
            && rel_match(&m.suggests, &self.suggests)
            && rel_match(&m.conflicts, &self.conflicts)
            && rel_match(&m.supports, &self.supports)
            && self.matches_tags(m)
            && self.matches_labels(s)
            && self.compatible.is_none_or(|v| v == s.compatible)
            && self.installed.is_none_or(|v| v == s.installed)
            && self.cached.is_none_or(|v| v == s.cached)
            && self.newly_compatible.is_none_or(|v| v == s.newly_compatible)
            && self.upgradeable.is_none_or(|v| v == s.upgradeable)
            && self.replaceable.is_none_or(|v| v == (s.installed && s.replaceable))
    }

    /// Substring of the name abbreviation, SearchableName or SearchableIdentifier.
    fn matches_name(&self, m: &Module) -> bool {
        let Some(t) = &self.name else { return true };
        let name = m.name.trim();
        let abbrev: String = name.split(' ').filter_map(|w| w.chars().next()).collect();
        let found = contains_ci(&abbrev, &t.text)
            || contains_ci(&strip_any_language(&m.name), &t.text)
            || contains_ci(&m.identifier.chars().filter(char::is_ascii_alphanumeric).collect::<String>(), &t.text);
        t.neg ^ found
    }

    /// Prefix of a stripped author name. The term itself is not stripped (as in CKAN).
    fn matches_authors(&self, m: &Module) -> bool {
        if self.authors.is_empty() {
            return true;
        }
        let authors: Vec<String> = m.author.iter().map(|a| strip_any_language(a)).collect();
        self.authors.iter().all(|t| t.neg ^ authors.iter().any(|a| starts_with_ci(a, &t.text)))
    }

    fn matches_description(&self, m: &Module) -> bool {
        let Some(t) = &self.description else { return true };
        let found = contains_ci(&strip_any_language(&m.abstract_), &t.text)
            || contains_ci(&strip_any_language(m.description.as_deref().unwrap_or("")), &t.text);
        t.neg ^ found
    }

    fn matches_licenses(&self, m: &Module) -> bool {
        self.licenses.iter().all(|t| t.neg ^ m.license.iter().any(|l| starts_with_ci(l, &t.text)))
    }

    /// An empty list counts as CKAN's null list: no match, even when negated.
    fn matches_localizations(&self, m: &Module) -> bool {
        self.localizations.is_empty()
            || (!m.localizations.is_empty()
                && self
                    .localizations
                    .iter()
                    .all(|t| t.neg ^ m.localizations.iter().any(|l| starts_with_ci(l, &t.text))))
    }

    /// `tag:x` exact (case-sensitive); `tag:` alone = no tags.
    fn matches_tags(&self, m: &Module) -> bool {
        self.tag_names.iter().all(|tn| {
            let (neg, sub) = match tn.strip_prefix('-') {
                Some(r) => (true, r),
                None => (false, tn.as_str()),
            };
            neg ^ if sub.is_empty() { m.tags.is_empty() } else { m.tags.iter().any(|t| t == sub) }
        })
    }

    /// CKAN MatchesLabels, grouped by (negate, exclude). Only the labels that contain this row
    /// are known here, so a named label must be one of them (CKAN also requires every other
    /// label with the same spaceless name to contain the row; that only differs for duplicate names).
    fn matches_labels(&self, s: &RowState) -> bool {
        if self.label_names.is_empty() {
            return true;
        }
        // Group index = negate * 2 + exclude.
        let mut used = [false; 4];
        let mut named_ok = [true; 4];
        for n in &self.label_names {
            let n = n.replace(' ', "");
            let (neg, sub) = match n.strip_prefix('-') {
                Some(r) => (true, r.to_string()),
                None => (false, n.clone()),
            };
            let exclude = sub.is_empty();
            let gi = (neg as usize) * 2 + exclude as usize;
            used[gi] = true;
            if !exclude && !s.labels.iter().any(|l| spaceless_eq(l, &sub)) {
                named_ok[gi] = false;
            }
        }
        (0..4).filter(|gi| used[*gi]).all(|gi| {
            let neg = gi >= 2;
            let exclude = gi % 2 == 1;
            let val = if exclude { s.labels.is_empty() } else { named_ok[gi] };
            neg ^ val
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn module() -> Module {
        Module::from_json(
            r#"{"identifier":"MechJeb2","name":"MechJeb 2","abstract":"Autopilot, info, and more",
            "description":"Line one.\nThe Ascent Guidance tool","author":["sarbian","r4m0n"],"version":"2.14",
            "license":"GPL-3.0","download":"https://x/m.zip","tags":["plugin","control"],
            "localizations":["en-us","de-de"],
            "depends":[{"name":"ModuleManager"}],"recommends":[{"any_of":[{"name":"KerbalEngineerRedux"},{"name":"VOID"}]}],
            "conflicts":[{"name":"MechJebForAll"}]}"#
                .as_bytes(),
        )
        .unwrap()
    }

    fn bare() -> Module {
        Module::from_json(br#"{"identifier":"Bare-Mod","name":"Bare","abstract":"","version":"1","download":"https://x/b.zip"}"#)
            .unwrap()
    }

    #[test]
    fn search_table() {
        let m = module();
        let b = bare();
        let labels: [&str; 1] = ["Don't Upgrade"];
        let st_m = RowState { installed: true, compatible: true, cached: false, labels: &labels, ..Default::default() };
        let st_b = RowState { compatible: false, ..Default::default() };
        // (query, matches MechJeb, matches Bare)
        let cases: &[(&str, bool, bool)] = &[
            ("", true, true),
            ("   ", true, true),
            ("mechjeb", true, false),
            ("MECH JEB", true, false),       // bare words are joined
            ("m2", true, false),             // abbreviation of "MechJeb 2"
            ("mj", false, false),
            ("-mechjeb", false, true),
            ("mech -jeb", true, false),      // CKAN joins to "mech-jeb" -> "mechjeb", not negated
            ("-", false, false),             // negated empty matches nothing
            ("@sar", true, false),
            ("-@sar", false, true),
            ("@", true, true),               // a missing author loads as [""], which "" prefixes
            ("desc:ascent", true, false),
            ("desc:autopilot,", true, false),
            ("-desc:ascent", false, true),
            ("lic:gpl", true, false),
            ("-lic:gpl", false, true),
            ("lang:de", true, false),
            ("-lang:fr", true, false),       // null localizations never match
            ("dep:module", true, false),
            ("-dep:module", false, false),   // null depends never match
            ("rec:void", true, false),       // any_of children
            ("conf:mechjebfor", true, false),
            ("sug:x", false, false),
            ("tag:plugin", true, false),
            ("tag:Plugin", false, false),    // tags are case-sensitive
            ("tag:", false, true),
            ("-tag:", true, false),
            ("label:dontupgrade", false, false),
            ("label:Don'tUpgrade", true, false),
            ("label:", false, true),
            ("-label:", true, false),
            ("label:nosuch", false, false),
            ("-label:nosuch", true, true),
            ("is:installed", true, false),
            ("not:installed", false, true),
            ("is:compatible", true, false),
            ("-is:installed", true, true),   // ignored, as in CKAN
            ("is:bogus", true, true),
            ("is:cached", false, false),
            ("is:replaceable", false, false),
            ("mech is:installed tag:control", true, false),
        ];
        for (q, want_m, want_b) in cases {
            let s = ModSearch::parse(q);
            assert_eq!(s.matches(&m, &st_m), *want_m, "query {q:?} on MechJeb");
            assert_eq!(s.matches(&b, &st_b), *want_b, "query {q:?} on Bare");
        }
    }

    #[test]
    fn empty_and_mentions() {
        assert!(ModSearch::parse("  ").is_empty());
        assert!(!ModSearch::parse("is:installed").is_empty());
        let s = ModSearch::parse("label:Hidden -label:Held label:DontUpgrade tag:x");
        assert!(s.mentions_label("Hidden"));
        assert!(s.mentions_label("Dont Upgrade"));
        assert!(!s.mentions_label("Held"));
        assert!(!s.mentions_label("Favourites"));
        assert!(s.mentions_tag("x"));
    }

    #[test]
    fn filters_follow_ckan_flags() {
        // Installed but incompatible (no compatible version, installed one too old): CKAN shows
        // it under Incompatible, not Compatible.
        let inst_incompat = RowState { installed: true, compatible: false, ..Default::default() };
        assert!(!Filter::Compatible.matches(&inst_incompat));
        assert!(Filter::Incompatible.matches(&inst_incompat));
        let ok = RowState { compatible: true, ..Default::default() };
        assert!(Filter::Compatible.matches(&ok) && !Filter::Incompatible.matches(&ok));
        // Replaceable needs the row to be installed, the same as "is:replaceable".
        let repl = RowState { replaceable: true, ..Default::default() };
        assert!(!Filter::Replaceable.matches(&repl));
        assert!(Filter::Replaceable.matches(&RowState { installed: true, ..repl }));
        assert_eq!(ModSearch::parse("is:replaceable").matches(&bare(), &repl), Filter::Replaceable.matches(&repl));
        // The filter and "is:compatible" agree.
        let s = ModSearch::parse("is:compatible");
        for st in [inst_incompat, ok] {
            assert_eq!(s.matches(&bare(), &st), Filter::Compatible.matches(&st));
        }
    }

    #[test]
    fn letter_class_is_unicode_l() {
        // Letter numbers (Nl) and spacing marks (Mc) are alphabetic in Rust but not \p{L}.
        assert_eq!(strip_any_language("a\u{216B}b"), "ab");
        assert_eq!(strip_any_language("\u{915}\u{93F}"), "\u{915}");
        assert_eq!(strip_any_language("Ünï-cødé 2"), "Ünïcødé2");
        assert_eq!(strip_any_language("Mech-Jeb 2!"), "MechJeb2");
        // Non-ASCII digits are not kept (the class is 0-9).
        assert_eq!(strip_any_language("x\u{663}"), "x");
    }

    #[test]
    fn negation_only_on_first_bare_word() {
        // "-foo bar" joins to "-foobar": one negated term.
        let s = ModSearch::parse("-mech jeb");
        assert!(!s.matches(&module(), &RowState::default()));
        assert!(s.matches(&bare(), &RowState::default()));
    }
}
