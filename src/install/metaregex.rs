//! Metadata regexes (.NET dialect) on top of the Rust `regex` crate.
//!
//! Rust `regex` has no lookaround. Real metadata uses only these .NET forms:
//! - `(?<!X)$`  = "the text does not end with a match of X"
//! - `^(?!X)`   = "the text does not start with a match of X"
//! - a top-level `|` of those forms and plain regexes.
//!
//! A pattern that compiles in Rust as is, is used as is. Other lookaround forms give an error.

use regex::{Regex, RegexBuilder};

use crate::error::{Error, Result};

#[derive(Debug, Clone)]
enum Part {
    /// Normal regex: matches if it matches anywhere.
    Plain(Regex),
    /// `(?<!X)$`: holds `(?:X)$`; the part matches when this does NOT match.
    NotEndsWith(Regex),
    /// `^(?!X)`: holds `^(?:X)`; the part matches when this does NOT match.
    NotStartsWith(Regex),
}

/// A regex as used in metadata. Handles the .NET-only lookaround forms found in real metadata
/// (`(?<!X)$` = "does not end with X", `^(?!X)` = "does not start with X", top-level `|`).
#[derive(Debug, Clone)]
pub struct MetaRegex {
    parts: Vec<Part>,
}

fn build(pattern: &str, ci: bool) -> std::result::Result<Regex, regex::Error> {
    RegexBuilder::new(pattern).case_insensitive(ci).build()
}

fn has_lookaround(p: &str) -> bool {
    p.contains("(?=") || p.contains("(?!") || p.contains("(?<=") || p.contains("(?<!")
}

/// Scan a pattern and call `f(index, byte, depth)` for every byte that is not escaped and not
/// inside a character class. Returns an error for unbalanced parentheses.
fn scan(p: &str, mut f: impl FnMut(usize, u8, usize)) -> std::result::Result<(), String> {
    let b = p.as_bytes();
    let mut i = 0;
    let mut depth = 0usize;
    let mut in_class = false;
    while i < b.len() {
        let c = b[i];
        if c == b'\\' {
            i += 2;
            continue;
        }
        if in_class {
            if c == b']' {
                in_class = false;
            }
            i += 1;
            continue;
        }
        match c {
            b'[' => {
                in_class = true;
                i += 1;
                // `[^]...]` and `[]...]`: a `]` right after the opening is a literal.
                if b.get(i) == Some(&b'^') {
                    i += 1;
                }
                if b.get(i) == Some(&b']') {
                    i += 1;
                }
                continue;
            }
            b'(' => {
                f(i, c, depth);
                depth += 1;
            }
            b')' => {
                if depth == 0 {
                    return Err("unbalanced ')'".into());
                }
                depth -= 1;
                f(i, c, depth);
            }
            _ => f(i, c, depth),
        }
        i += 1;
    }
    if depth != 0 || in_class {
        return Err("unbalanced '(' or '['".into());
    }
    Ok(())
}

/// Split at top-level `|`.
fn split_top(p: &str) -> std::result::Result<Vec<&str>, String> {
    let mut cuts = Vec::new();
    scan(p, |i, c, depth| {
        if c == b'|' && depth == 0 {
            cuts.push(i);
        }
    })?;
    let mut out = Vec::with_capacity(cuts.len() + 1);
    let mut start = 0;
    for i in cuts {
        out.push(p.get(start..i).ok_or("bad split")?);
        start = i + 1;
    }
    out.push(p.get(start..).ok_or("bad split")?);
    Ok(out)
}

/// Index of the `)` that closes the `(` at `open`.
fn closing_paren(p: &str, open: usize) -> Option<usize> {
    let mut open_depth = None;
    let mut close = None;
    let r = scan(p, |i, c, depth| {
        if close.is_some() {
            return;
        }
        if c == b'(' && i == open {
            open_depth = Some(depth);
        } else if c == b')' && open_depth == Some(depth) {
            close = Some(i);
        }
    });
    r.ok()?;
    close
}

/// `(?<!X)$` -> Some(X)
fn as_not_ends_with(alt: &str) -> Option<&str> {
    if alt.starts_with("(?<!") && alt.ends_with(")$") && closing_paren(alt, 0) == Some(alt.len() - 2) {
        alt.get(4..alt.len() - 2)
    } else {
        None
    }
}

/// `^(?!X)` -> Some(X)
fn as_not_starts_with(alt: &str) -> Option<&str> {
    if alt.starts_with("^(?!") && alt.ends_with(')') && closing_paren(alt, 1) == Some(alt.len() - 1) {
        alt.get(4..alt.len() - 1)
    } else {
        None
    }
}

impl MetaRegex {
    pub fn new(pattern: &str, case_insensitive: bool) -> Result<MetaRegex> {
        let bad = |why: String| Error::Metadata(format!("bad regex {pattern:?}: {why}"));
        match build(pattern, case_insensitive) {
            Ok(r) => return Ok(MetaRegex { parts: vec![Part::Plain(r)] }),
            Err(e) if !has_lookaround(pattern) => return Err(bad(e.to_string())),
            Err(_) => {}
        }
        let alts = split_top(pattern).map_err(bad)?;
        let mut plain: Vec<&str> = Vec::new();
        let mut parts = Vec::new();
        for alt in alts {
            if let Some(x) = as_not_ends_with(alt) {
                let r = build(&format!("(?:{x})$"), case_insensitive).map_err(|e| bad(e.to_string()))?;
                parts.push(Part::NotEndsWith(r));
            } else if let Some(x) = as_not_starts_with(alt) {
                let r = build(&format!("^(?:{x})"), case_insensitive).map_err(|e| bad(e.to_string()))?;
                parts.push(Part::NotStartsWith(r));
            } else if has_lookaround(alt) {
                return Err(bad(format!(
                    "unsupported lookaround in {alt:?} (only (?<!X)$ and ^(?!X) are supported)"
                )));
            } else {
                plain.push(alt);
            }
        }
        if !plain.is_empty() {
            let joined: Vec<String> = plain.iter().map(|a| format!("(?:{a})")).collect();
            let r = build(&joined.join("|"), case_insensitive).map_err(|e| bad(e.to_string()))?;
            parts.insert(0, Part::Plain(r));
        }
        Ok(MetaRegex { parts })
    }

    pub fn is_match(&self, s: &str) -> bool {
        self.parts.iter().any(|p| match p {
            Part::Plain(r) => r.is_match(s),
            Part::NotEndsWith(r) | Part::NotStartsWith(r) => !r.is_match(s),
        })
    }

    /// Start of the leftmost match, counted in UTF-16 code units like .NET `Match.Index`.
    /// Lookaround forms match at 0 (they match empty text at the start or end; only the
    /// relative order matters for `find`, which never uses them).
    pub fn find_start(&self, s: &str) -> Option<usize> {
        let mut best: Option<usize> = None;
        for p in &self.parts {
            let at = match p {
                Part::Plain(r) => r.find(s).map(|m| m.start()),
                Part::NotStartsWith(r) => (!r.is_match(s)).then_some(0),
                Part::NotEndsWith(r) => (!r.is_match(s)).then_some(s.len()),
            };
            if let Some(a) = at {
                best = Some(best.map_or(a, |b| b.min(a)));
            }
        }
        best.map(|b| s.get(..b).map_or(b, |pre| pre.encode_utf16().count()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn re(p: &str) -> MetaRegex {
        MetaRegex::new(p, false).unwrap()
    }

    #[test]
    fn not_ends_with_forms() {
        // All 10 lookbehind patterns in real metadata (filter_regexp).
        let cases = [
            ("(?<!Basic Asteroids\\.cfg)$", "Foo/Basic Asteroids.cfg", "Foo/Other.cfg"),
            ("(?<!Generic_.+\\.cfg)$", "X/Generic_Thing.cfg", "X/Generic_.cfg"),
            ("(?<!KA_.+\\.cfg)$", "GameData/KA_Moho.cfg", "GameData/KA_Moho.txt"),
            ("(?<!NFP_.+\\.cfg)$", "a/NFP_x.cfg", "a/NFP.cfg"),
            ("(?<!NFS_.+\\.cfg)$", "a/NFS_x.cfg", "a/nfs_x.cfg"),
            ("(?<!OPM-Reconfig\\.cfg)$", "a/OPM-Reconfig.cfg", "a/OPM-Reconfig.cfg.bak"),
            ("(?<!Stock_.+\\.cfg)$", "Stock_A.cfg", "Stock.cfg"),
            ("(?<!Stockalike\\.cfg)$", "x/Stockalike.cfg", "x/Stockalike.cfgx"),
            ("(?<!Trans-Jool\\.cfg)$", "y/Trans-Jool.cfg", "y/Trans-Joolcfg"),
            ("(?<!Trans-Neidon\\.cfg)$", "Trans-Neidon.cfg", "z/Trans-Neidon"),
        ];
        for (p, keep, filtered) in cases {
            let r = re(p);
            // As a filter: a match means "skip". The named file must NOT match (it is kept).
            assert!(!r.is_match(keep), "{p} vs {keep}");
            assert!(r.is_match(filtered), "{p} vs {filtered}");
        }
        // Case-sensitive (filter_regexp): a different case is not the excluded name.
        assert!(re("(?<!Stockalike\\.cfg)$").is_match("x/stockalike.cfg"));
        assert!(!MetaRegex::new("(?<!Stockalike\\.cfg)$", true).unwrap().is_match("x/stockalike.cfg"));
    }

    #[test]
    fn not_starts_with_forms() {
        let r = re("^(?!CustomBiomes/PluginData/)|config.xml");
        assert!(!r.is_match("CustomBiomes/PluginData/a.cfg"));
        assert!(r.is_match("CustomBiomes/PluginData/config.xml"));
        assert!(r.is_match("CustomBiomes/Plugins/x.dll"));

        let r = re("^(?!FinePrint/FinePrint.cfg)");
        assert!(!r.is_match("FinePrint/FinePrint.cfg"));
        assert!(r.is_match("FinePrint/Plugins/FinePrint.dll"));

        let r = re("^(?!GameData/ThunderAerospace/TacLifeSupport/PluginData/TacLifeSupport/LifeSupport.cfg)");
        assert!(!r.is_match("GameData/ThunderAerospace/TacLifeSupport/PluginData/TacLifeSupport/LifeSupport.cfg"));
        assert!(r.is_match("GameData/ThunderAerospace/TacLifeSupport/Plugins/x.dll"));
    }

    #[test]
    fn plain_and_inline_flags() {
        // The one find_regexp with `(?` that is not a lookaround.
        let r = MetaRegex::new("(?i)game\\s*data/it'?s\\s*the\\s*little\\s*things", true).unwrap();
        assert!(r.is_match("x/Game Data/Its the little things/a.cfg"));
        assert_eq!(r.find_start("x/GameData/ItsTheLittleThings"), Some(2));
        // Escaped slash from real metadata.
        assert!(re("TacLifeSupport\\/LifeSupport\\.cfg$").is_match("a/TacLifeSupport/LifeSupport.cfg"));
        assert!(re("RoboBrakes.v[\\d+.]{3,5}").is_match("RoboBrakes_v1.2.3"));
        // Case rules.
        assert!(!re("\\.craft$").is_match("A.CRAFT"));
        assert!(MetaRegex::new("\\.craft$", true).unwrap().is_match("A.CRAFT"));
    }

    #[test]
    fn unsupported_and_bad() {
        assert!(MetaRegex::new("a(?=b)", false).is_err());
        assert!(MetaRegex::new("(?<!a)b", false).is_err());
        assert!(MetaRegex::new("(", false).is_err());
        assert!(MetaRegex::new("^(?!a)(", false).is_err());
    }

    #[test]
    fn utf16_index() {
        let r = re("/X");
        assert_eq!(r.find_start("Ünï/X"), Some(3));
    }
}
