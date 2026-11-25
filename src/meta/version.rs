//! CKAN ModuleVersion: `[epoch:]version`, compared with CKAN's exact chunk algorithm
//! (Core/Versioning/ModuleVersion.cs). Equality is `cmp == 0`, so do NOT hash or key maps by
//! ModuleVersion.
//!
//! The compare is a total preorder (lexicographic over (string key, number) pairs), so it is
//! safe for `sort`.

use std::cmp::Ordering;
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Clone)]
pub struct ModuleVersion {
    raw: Box<str>,
}

/// Split `[epoch:]version` like the CKAN regex `^(?:(?<epoch>[0-9]+):)?(?<version>.*)$`.
/// Returns None when CKAN throws: the epoch overflows i32, or a `\n` is not the last char.
/// The .NET `$` also matches before one final `\n`, so that `\n` is not part of the version.
fn split_epoch(s: &str) -> Option<(i32, &str)> {
    let body = s.strip_suffix('\n').unwrap_or(s);
    if body.contains('\n') {
        return None;
    }
    let digits = body.bytes().take_while(u8::is_ascii_digit).count();
    if digits > 0 && body.as_bytes().get(digits) == Some(&b':') {
        // Convert.ToInt32 throws on overflow (leading zeros are fine).
        let epoch: i32 = body.get(..digits)?.parse().ok()?;
        return Some((epoch, body.get(digits + 1..)?));
    }
    Some((0, body))
}

impl ModuleVersion {
    /// Never fails. A string that CKAN would reject (see `parse`) compares as epoch 0 with the
    /// whole text as the version.
    pub fn new(s: &str) -> Self {
        ModuleVersion { raw: s.into() }
    }

    /// Strict CKAN parse: None if the epoch overflows i32 or the text has an inner newline.
    pub fn parse(s: &str) -> Option<Self> {
        split_epoch(s).map(|_| ModuleVersion::new(s))
    }

    /// The text as written in metadata (with epoch).
    pub fn as_str(&self) -> &str {
        &self.raw
    }

    fn parts(&self) -> (i32, &str) {
        split_epoch(&self.raw).unwrap_or((0, &self.raw))
    }

    /// Epoch number (0 if none).
    #[cfg(test)]
    pub fn epoch(&self) -> u64 {
        self.parts().0.max(0) as u64
    }

    /// The part after the epoch that the compare uses.
    #[cfg(test)]
    pub fn version_part(&self) -> &str {
        self.parts().1
    }

    /// Text without the `epoch:` prefix (for display when epochs are hidden).
    /// CKAN StripEpoch: only strips when the text is `^[0-9]+:[^:]+$`.
    pub fn without_epoch(&self) -> &str {
        let s: &str = &self.raw;
        let digits = s.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 || s.as_bytes().get(digits) != Some(&b':') {
            return s;
        }
        match s.get(digits + 1..) {
            Some(rest) if !rest.is_empty() && !rest.contains(':') => rest,
            _ => s,
        }
    }

    /// CKAN `ToString(hideEpoch, hideV)`: optionally strip the epoch and a leading `v`/`version`.
    pub fn display(&self, hide_epoch: bool, hide_v: bool) -> String {
        let s = if hide_v { strip_v(&self.raw) } else { self.raw.to_string() };
        if hide_epoch { ModuleVersion::new(&s).without_epoch().to_string() } else { s }
    }
}

/// CKAN StripV: `^(?<num>\d\:)?[vV]+(ersion)?[_.]*(?<ver>\d.*)$` -> num + ver.
fn strip_v(s: &str) -> String {
    let b = s.as_bytes();
    let mut i = 0;
    let num_len = if b.len() >= 2 && b[0].is_ascii_digit() && b[1] == b':' { 2 } else { 0 };
    i += num_len;
    let v_start = i;
    while i < b.len() && (b[i] == b'v' || b[i] == b'V') {
        i += 1;
    }
    if i == v_start {
        return s.to_string();
    }
    if b.get(i..i + 6) == Some(b"ersion") {
        i += 6;
    }
    while i < b.len() && (b[i] == b'_' || b[i] == b'.') {
        i += 1;
    }
    if !b.get(i).is_some_and(u8::is_ascii_digit) {
        return s.to_string();
    }
    // `.*$` stops before one final `\n`, and the result is built from the groups only,
    // so that `\n` is dropped.
    let ver = s.get(i..).unwrap_or("");
    let ver = ver.strip_suffix('\n').unwrap_or(ver);
    if ver.contains('\n') {
        return s.to_string();
    }
    format!("{}{}", s.get(..num_len).unwrap_or(""), ver)
}

/// .NET `char.IsNumber` on one UTF-16 unit: Unicode Nd/Nl/No, never a surrogate.
fn is_num(c: char) -> bool {
    (c as u32) < 0x1_0000 && c.is_numeric()
}

/// .NET `string.CompareOrdinal`: UTF-16 code unit order.
fn ordinal(a: &str, b: &str) -> Ordering {
    if a.is_ascii() && b.is_ascii() { a.cmp(b) } else { a.encode_utf16().cmp(b.encode_utf16()) }
}

/// Split at the first char where `pred` is true: (head, rest). No match gives (s, "").
fn split_at_first(s: &str, pred: impl Fn(char) -> bool) -> (&str, &str) {
    match s.char_indices().find(|&(_, c)| pred(c)) {
        Some((i, _)) => s.split_at(i),
        None => (s, ""),
    }
}

/// int.TryParse on a run of numeric chars: ASCII digits that fit in i32, else 0.
fn num_value(n: &str) -> i32 {
    n.parse::<i32>().unwrap_or(0)
}

/// The CKAN compare of two version parts with equal epochs.
fn compare_parts(v1: &str, v2: &str) -> Ordering {
    let (mut r1, mut r2) = (v1, v2);
    while !r1.is_empty() && !r2.is_empty() {
        // String step.
        let (s1, rest1) = split_at_first(r1, is_num);
        let (s2, rest2) = split_at_first(r2, is_num);
        let c = match (s1.chars().next(), s2.chars().next()) {
            (Some(f1), Some(f2)) => match (f1 == '.', f2 == '.') {
                (false, true) => Ordering::Less,
                (true, false) => Ordering::Greater,
                // QUIRK: ".a" vs ".b" is equal; only "." alone beats a longer dot segment.
                (true, true) => match (s1.len() == 1, s2.len() == 1) {
                    (true, false) => Ordering::Greater,
                    (false, true) => Ordering::Less,
                    _ => Ordering::Equal,
                },
                (false, false) => ordinal(s1, s2),
            },
            _ => ordinal(s1, s2),
        };
        if c != Ordering::Equal {
            return c;
        }
        // Number step.
        let (n1, rest1) = split_at_first(rest1, |c| !is_num(c));
        let (n2, rest2) = split_at_first(rest2, |c| !is_num(c));
        let c = num_value(n1).cmp(&num_value(n2));
        if c != Ordering::Equal {
            return c;
        }
        r1 = rest1;
        r2 = rest2;
    }
    match (r1.is_empty(), r2.is_empty()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Less,
        _ => Ordering::Greater,
    }
}

impl Ord for ModuleVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        let (e1, v1) = self.parts();
        let (e2, v2) = other.parts();
        if e1 != e2 {
            return e1.cmp(&e2);
        }
        if v1 == v2 {
            return Ordering::Equal;
        }
        compare_parts(v1, v2)
    }
}

impl PartialOrd for ModuleVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for ModuleVersion {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for ModuleVersion {}

impl fmt::Display for ModuleVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

impl fmt::Debug for ModuleVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", &*self.raw)
    }
}

impl Serialize for ModuleVersion {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.raw)
    }
}

impl<'de> Deserialize<'de> for ModuleVersion {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // CKAN JsonSimpleStringConverter: the JSON scalar's ToString().
        let s = match serde_json::Value::deserialize(d)? {
            serde_json::Value::String(s) => s,
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::Bool(b) => if b { "True" } else { "False" }.to_string(),
            o => return Err(serde::de::Error::custom(format!("bad version {o}"))),
        };
        ModuleVersion::parse(&s).ok_or_else(|| serde::de::Error::custom(format!("bad version {s:?}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> ModuleVersion {
        ModuleVersion::new(s)
    }

    fn cmp(a: &str, b: &str) -> Ordering {
        v(a).cmp(&v(b))
    }

    #[test]
    fn spec_worked_examples() {
        use Ordering::*;
        let cases = [
            ("1.01", "1.1", Equal),
            ("1.0", "1.0.0", Less),
            ("1.0", "1.0-beta", Less),
            ("1.0_beta", "1.0.1_beta", Less),
            ("1.0.repackaged", "1.0.1", Less),
            ("1.a2", "1.b2", Equal),
            ("v1.2", "1.10", Greater),
            ("V1.0", "v0.1", Less),
            ("v6a12", "v6a5", Greater),
            ("1:0.1", "9.9", Greater),
            ("0:1.0", "1.0", Equal),
            ("v1.1.0.202206081400", "v1.1.0.202206081500", Equal),
            ("1.0", "1.0.", Less),
        ];
        for (a, b, want) in cases {
            assert_eq!(cmp(a, b), want, "{a} vs {b}");
            assert_eq!(cmp(b, a), want.reverse(), "{b} vs {a}");
        }
    }

    #[test]
    fn ckan_unit_tests() {
        // From CKAN Tests/Core/Versioning/ModuleVersionTests.cs.
        assert!(v("1.1") > v("1.0"));
        assert!(v("1.0.10") > v("1.0.9"));
        assert!(v("1.2.0") > v("1.1.20"));
        assert!(v("2:1.0") > v("1:2.0"));
        assert!(v("1.0-alpha") < v("1.0-beta"));
        assert!(v("v1.0") > v("1.0"));
        assert_eq!(v("1.0"), v("1.00"));
        assert!(v("") < v("0"));
        assert_eq!(cmp("", ""), Ordering::Equal);
    }

    #[test]
    fn epoch_parsing() {
        assert_eq!(v("1:2:3").epoch(), 1);
        assert_eq!(v("1:2:3").version_part(), "2:3");
        assert_eq!(v("12:v1.0").epoch(), 12);
        assert_eq!(v("v1.0").epoch(), 0);
        assert_eq!(v(":1.0").version_part(), ":1.0");
        assert_eq!(v("007:1").epoch(), 7);
        // Overflow: CKAN throws.
        assert!(ModuleVersion::parse("2147483648:1.0").is_none());
        assert!(ModuleVersion::parse("2147483647:1.0").is_some());
        // new() keeps it and compares the whole text with epoch 0.
        assert_eq!(v("2147483648:1.0").epoch(), 0);
        // Newlines: only one final newline is allowed and it is not part of the version.
        assert!(ModuleVersion::parse("1.0\nx").is_none());
        assert!(ModuleVersion::parse("1.0\n\n").is_none());
        let nl = ModuleVersion::parse("1.0\n").unwrap();
        assert_eq!(nl, v("1.0"));
        assert_eq!(nl.as_str(), "1.0\n");
        // Not trimmed.
        assert_eq!(v("2.1 ").version_part(), "2.1 ");
        assert!(v("2.1 ") > v("2.1"));
    }

    #[test]
    fn epoch_display() {
        assert_eq!(v("1:2.0").without_epoch(), "2.0");
        assert_eq!(v("2.0").without_epoch(), "2.0");
        assert_eq!(v("1:2:3").without_epoch(), "1:2:3");
        assert_eq!(v("1:").without_epoch(), "1:");
        assert_eq!(v("1:v2.0").display(true, true), "2.0");
        assert_eq!(v("1:v2.0").display(false, true), "1:2.0");
        assert_eq!(v("Version_1.2").display(false, true), "1.2");
        assert_eq!(v("vbeta").display(false, true), "vbeta");
        assert_eq!(v("10:v1.0").display(false, true), "10:v1.0");
        // Regression: CKAN StripV drops one final newline (it is outside the groups).
        assert_eq!(v("v1.0\n").display(false, true), "1.0");
        assert_eq!(v("v1.0\nx").display(false, true), "v1.0\nx");
    }

    #[test]
    fn unicode_and_overflow_numbers() {
        // Non-ASCII numeric chars are numeric for the split but parse as 0.
        assert_eq!(cmp("1.\u{0663}", "1.0"), Ordering::Equal);
        // Ordinal is UTF-16: U+FF21 (fullwidth A) sorts after a surrogate pair (U+1F600).
        assert_eq!(cmp("a\u{FF21}", "a\u{1F600}"), Ordering::Greater);
        assert_eq!(cmp("1.99999999999", "1.0"), Ordering::Equal);
    }

    #[test]
    fn json_forms() {
        let x: ModuleVersion = serde_json::from_str("\"1:2.0\"").unwrap();
        assert_eq!(x.epoch(), 1);
        let n: ModuleVersion = serde_json::from_str("3").unwrap();
        assert_eq!(n.as_str(), "3");
        assert!(serde_json::from_str::<ModuleVersion>("\"99999999999:1\"").is_err());
        assert!(serde_json::from_str::<ModuleVersion>("null").is_err());
    }

    #[test]
    fn sort_is_total() {
        let mut vs: Vec<ModuleVersion> =
            ["1.a", ".b5", ".a1", "1.0", "1.00", "v1", "V1", "", "0", "1:0", "1.0.", "1.x", "1."]
                .iter()
                .map(|s| v(s))
                .collect();
        vs.sort();
        for w in vs.windows(2) {
            assert!(w[0] <= w[1]);
        }
    }
}
