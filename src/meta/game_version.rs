//! KSP game versions (`1.12.5.3190`, `1.12`, `any`), ranges, and module compatibility.
//! Port of Core/Versioning/GameVersion*.cs and Core/Types/GameComparator/StrictGameComparator.cs.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::Module;

/// Missing parts are -1. `ANY` has all parts -1.
/// The derived order is CKAN's: lexicographic on (major, minor, patch, build), -1 lowest,
/// so `ANY < 1.12 < 1.12.0 < 1.12.0.0`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GameVersion {
    pub major: i32,
    pub minor: i32,
    pub patch: i32,
    pub build: i32,
}

impl GameVersion {
    pub const ANY: GameVersion = GameVersion { major: -1, minor: -1, patch: -1, build: -1 };

    pub fn new(major: i32, minor: i32, patch: i32, build: i32) -> Self {
        GameVersion { major, minor, patch, build }
    }

    /// `any`, `1`, `1.12`, `1.12.5`, `1.12.5.3190`. Caller strips one trailing `.` for JSON input.
    /// CKAN TryParse: `any` is exact (before trim); else the trimmed text must be 1-4 dot-separated
    /// ASCII digit groups that fit in i32. A group equal to i32::MAX is undefined, and the first
    /// undefined group cuts the version there (an undefined major is an error).
    pub fn parse(s: &str) -> Option<GameVersion> {
        if s == "any" {
            return Some(GameVersion::ANY);
        }
        let s = s.trim();
        let mut parts = [-1i32; 4];
        let mut n = 0;
        for p in s.split('.') {
            if n == 4 || p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let v: i32 = p.parse().ok()?;
            parts[n] = if v == i32::MAX { -1 } else { v };
            n += 1;
        }
        if n == 0 || parts[0] < 0 {
            return None;
        }
        let [major, minor, patch, build] = parts;
        Some(if minor < 0 {
            GameVersion::new(major, -1, -1, -1)
        } else if patch < 0 {
            GameVersion::new(major, minor, -1, -1)
        } else {
            GameVersion::new(major, minor, patch, build)
        })
    }

    pub fn is_any(&self) -> bool {
        self.major < 0 && self.minor < 0 && self.patch < 0 && self.build < 0
    }


    pub fn without_build(&self) -> GameVersion {
        GameVersion { build: -1, ..*self }
    }

    /// `major.minor` only.
    pub fn major_minor(&self) -> GameVersion {
        GameVersion { patch: -1, build: -1, ..*self }
    }

    /// JSON form (GameVersionJsonConverter): drop one `.` at the end (the .NET `\.$` also
    /// matches before one final `\n`), then parse.
    pub fn parse_json(s: &str) -> Option<GameVersion> {
        if let Some(head) = s.strip_suffix(".\n") {
            return GameVersion::parse(&format!("{head}\n"));
        }
        GameVersion::parse(s.strip_suffix('.').unwrap_or(s))
    }

    /// CKAN ToVersionRange: `a.b.c.d` -> `[v, v]`; `a.b.c` -> `[a.b.c.0, a.b.(c+1).0)`;
    /// `a.b` -> `[a.b.0.0, a.(b+1).0.0)`; `a` -> `[a.0.0.0, (a+1).0.0.0)`; any -> unbounded.
    // The `&self` signature is a shared contract (other modules call it).
    #[allow(clippy::wrong_self_convention)]
    pub fn to_range(&self) -> GameVersionRange {
        let incl = |value| GameVersionBound { value, inclusive: true };
        let excl = |value| GameVersionBound { value, inclusive: false };
        let (lower, upper) = if self.build >= 0 {
            (incl(*self), incl(*self))
        } else if self.patch >= 0 {
            (
                incl(GameVersion::new(self.major, self.minor, self.patch, 0)),
                excl(GameVersion::new(self.major, self.minor, self.patch.saturating_add(1), 0)),
            )
        } else if self.minor >= 0 {
            (
                incl(GameVersion::new(self.major, self.minor, 0, 0)),
                excl(GameVersion::new(self.major, self.minor.saturating_add(1), 0, 0)),
            )
        } else if self.major >= 0 {
            (incl(GameVersion::new(self.major, 0, 0, 0)), excl(GameVersion::new(self.major.saturating_add(1), 0, 0, 0)))
        } else {
            (GameVersionBound::UNBOUNDED, GameVersionBound::UNBOUNDED)
        };
        GameVersionRange { lower, upper }
    }
}

impl fmt::Display for GameVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_any() {
            return f.write_str("any");
        }
        write!(f, "{}", self.major)?;
        for p in [self.minor, self.patch, self.build] {
            if p < 0 {
                break;
            }
            write!(f, ".{p}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for GameVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "GameVersion({self})")
    }
}

impl Serialize for GameVersion {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for GameVersion {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        GameVersion::parse_json(&s).ok_or_else(|| serde::de::Error::custom(format!("bad game version {s:?}")))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct GameVersionBound {
    /// `GameVersion::ANY` means unbounded.
    pub value: GameVersion,
    pub inclusive: bool,
}

impl GameVersionBound {
    /// CKAN `GameVersionBound.Unbounded` = (Any, inclusive).
    pub const UNBOUNDED: GameVersionBound = GameVersionBound { value: GameVersion::ANY, inclusive: true };

    pub fn is_unbounded(&self) -> bool {
        *self == GameVersionBound::UNBOUNDED
    }

    /// CKAN Highest: sort by (unbounded last, value descending, exclusive first), take the first.
    /// A bounded value always wins over Unbounded.
    pub fn highest(a: GameVersionBound, b: GameVersionBound) -> GameVersionBound {
        let key = |x: &GameVersionBound| (x.is_unbounded(), std::cmp::Reverse(x.value), x.inclusive);
        if key(&b) < key(&a) { b } else { a }
    }

    /// CKAN Lowest: sort by (unbounded last, value ascending, exclusive first), take the first.
    pub fn lowest(a: GameVersionBound, b: GameVersionBound) -> GameVersionBound {
        let key = |x: &GameVersionBound| (x.is_unbounded(), x.value, x.inclusive);
        if key(&b) < key(&a) { b } else { a }
    }

    /// CKAN AsInclusiveLower: the lowest version (fewest parts) inside the bound.
    pub fn as_inclusive_lower(&self) -> GameVersion {
        let v = self.value;
        let gv = GameVersion::new;
        if self.inclusive {
            // Drop trailing zero parts.
            if v.patch > 0 {
                gv(v.major, v.minor, v.patch, -1)
            } else if v.minor > 0 {
                gv(v.major, v.minor, -1, -1)
            } else if v.major > 0 {
                gv(v.major, -1, -1, -1)
            } else {
                GameVersion::ANY
            }
        } else if v.patch >= 0 {
            gv(v.major, v.minor, v.patch.saturating_add(1), -1)
        } else if v.minor >= 0 {
            gv(v.major, v.minor.saturating_add(1), -1, -1)
        } else if v.major >= 0 {
            gv(v.major.saturating_add(1), -1, -1, -1)
        } else {
            GameVersion::ANY
        }
    }

    /// CKAN AsInclusiveUpper: the highest version inside the bound.
    pub fn as_inclusive_upper(&self) -> GameVersion {
        let v = self.value;
        let gv = GameVersion::new;
        if self.inclusive {
            v
        } else if v.patch > 0 {
            gv(v.major, v.minor, v.patch - 1, -1)
        } else if v.minor > 0 {
            gv(v.major, v.minor - 1, -1, -1)
        } else if v.major > 0 {
            gv(v.major - 1, -1, -1, -1)
        } else {
            GameVersion::ANY
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct GameVersionRange {
    pub lower: GameVersionBound,
    pub upper: GameVersionBound,
}

impl GameVersionRange {
    #[cfg(test)]
    pub const ANY: GameVersionRange =
        GameVersionRange { lower: GameVersionBound::UNBOUNDED, upper: GameVersionBound::UNBOUNDED };

    /// Range(lower.to_range().lower, upper.to_range().upper).
    pub fn between(lower: &GameVersion, upper: &GameVersion) -> Self {
        GameVersionRange { lower: lower.to_range().lower, upper: upper.to_range().upper }
    }

    /// CKAN IsEmpty: upper < lower, or equal values with an exclusive end.
    /// QUIRK: an Unbounded upper (value ANY) under a bounded lower counts as empty.
    pub fn is_empty(&self) -> bool {
        let (lo, hi) = (self.lower, self.upper);
        hi.value < lo.value || (lo.value == hi.value && (!lo.inclusive || !hi.inclusive))
    }

    pub fn intersect(&self, other: &GameVersionRange) -> Option<GameVersionRange> {
        let r = GameVersionRange {
            lower: GameVersionBound::highest(self.lower, other.lower),
            upper: GameVersionBound::lowest(self.upper, other.upper),
        };
        if r.is_empty() { None } else { Some(r) }
    }

    pub fn contains(&self, v: &GameVersion) -> bool {
        self.intersect(&v.to_range()).is_some()
    }
}

impl fmt::Display for GameVersionRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let side = |b: &GameVersionBound| if b.value.is_any() { String::new() } else { b.value.to_string() };
        write!(
            f,
            "{}{},{}{}",
            if self.lower.inclusive { '[' } else { '(' },
            side(&self.lower),
            side(&self.upper),
            if self.upper.inclusive { ']' } else { ')' }
        )
    }
}

/// The set of game versions a module must match one of: the installed game version plus
/// the user's extra compatible versions.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VersionCriteria {
    pub versions: Vec<GameVersion>,
}

impl VersionCriteria {
    pub fn new(game: Option<GameVersion>, compatible: &[GameVersion]) -> Self {
        let mut versions: Vec<GameVersion> = Vec::new();
        for v in game.iter().chain(compatible.iter()) {
            if !versions.contains(v) {
                versions.push(*v);
            }
        }
        VersionCriteria { versions }
    }

    /// CKAN MinAndMax: fold the ranges of all versions with Lowest(lower) and Highest(upper).
    /// None when there are no versions.
    pub fn range(&self) -> Option<GameVersionRange> {
        let (first, rest) = self.versions.split_first()?;
        Some(rest.iter().fold(first.to_range(), |acc, v| {
            let r = v.to_range();
            GameVersionRange {
                lower: GameVersionBound::lowest(acc.lower, r.lower),
                upper: GameVersionBound::highest(acc.upper, r.upper),
            }
        }))
    }

    /// Inclusive (lowest, highest) versions covered, without build (for modpack ksp_version_min/max).
    /// CKAN: `MinAndMax.Lower.AsInclusiveLower().WithoutBuild`, `...Upper.AsInclusiveUpper().WithoutBuild`.
    pub fn min_max(&self) -> Option<(GameVersion, GameVersion)> {
        let r = self.range()?;
        Some((r.lower.as_inclusive_lower().without_build(), r.upper.as_inclusive_upper().without_build()))
    }
}

/// CKAN default compatible versions for a KSP install with no compatible_ksp_versions.json
/// (KerbalSpaceProgram.DefaultCompatibleVersions).
pub fn default_compatible_versions(installed: &GameVersion) -> Vec<GameVersion> {
    // 1.2.9 was a prerelease that broke compatibility with earlier 1.2 releases.
    if installed.major == 1 && installed.minor == 2 && installed.patch == 9 {
        return Vec::new();
    }
    if installed.is_any() {
        return Vec::new(); // CKAN would throw here.
    }
    // Unity break versions, newest first.
    let brk = [8, 4, 2, 1].iter().map(|&m| GameVersion::new(1, m, -1, -1)).find(|b| b <= installed);
    match brk {
        Some(b) if installed.minor >= b.minor => {
            (b.minor..=installed.minor).map(|m| GameVersion::new(b.major, m, -1, -1)).collect()
        }
        Some(_) => Vec::new(), // CKAN: Enumerable.Range with a negative count throws
        None if installed.minor >= 0 => vec![installed.major_minor()],
        None => Vec::new(), // CKAN: GameVersion(major, -1) throws
    }
}

impl Module {
    /// CKAN StrictGameComparator against the criteria (empty criteria = compatible).
    pub fn is_compatible(&self, crit: &VersionCriteria) -> bool {
        crit.versions.is_empty() || crit.versions.iter().any(|gv| self.compatible_with(gv))
    }

    /// The module's game version range, or None when it has no game version fields (any).
    /// `Err(())` for min/max whose bounds are reversed (CKAN: never compatible).
    fn game_range(&self) -> Option<Result<GameVersionRange, ()>> {
        Some(Ok(match (self.ksp_version, self.ksp_version_min, self.ksp_version_max) {
            (Some(v), _, _) => v.to_range(),
            (None, Some(lo), Some(hi)) => {
                // Compare the BOUND values, not the raw versions (min 1.12.3 / max 1.12 is valid).
                let r = GameVersionRange::between(&lo, &hi);
                if r.lower.value > r.upper.value {
                    return Some(Err(()));
                }
                r
            }
            (None, Some(lo), None) => GameVersionRange::between(&lo, &GameVersion::ANY),
            (None, None, Some(hi)) => GameVersionRange::between(&GameVersion::ANY, &hi),
            (None, None, None) => return None,
        }))
    }

    /// CKAN StrictGameComparator.SingleVersionsCompatible.
    pub fn compatible_with(&self, gv: &GameVersion) -> bool {
        match self.game_range() {
            None => true,
            Some(Err(())) => false,
            Some(Ok(mr)) => gv.to_range().intersect(&mr).is_some(),
        }
    }

    /// `ksp_version_max ?? ksp_version ?? ANY`.
    pub fn latest_game_version(&self) -> GameVersion {
        self.ksp_version_max.or(self.ksp_version).unwrap_or(GameVersion::ANY)
    }

    /// `ksp_version_min ?? ksp_version ?? ANY`.
    pub fn earliest_game_version(&self) -> GameVersion {
        self.ksp_version_min.or(self.ksp_version).unwrap_or(GameVersion::ANY)
    }

    /// CKAN LatestCompatibleRealGameVersion: the last of `real` (sorted oldest first) inside
    /// `between(earliest, latest)`, else `latest_game_version()`.
    pub fn latest_compatible_real_game_version(&self, real: &[GameVersion]) -> GameVersion {
        let r = GameVersionRange::between(&self.earliest_game_version(), &self.latest_game_version());
        real.iter().rev().find(|v| r.contains(v)).copied().unwrap_or_else(|| self.latest_game_version())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(s: &str) -> GameVersion {
        GameVersion::parse(s).unwrap()
    }

    fn incl(s: &str) -> GameVersionBound {
        GameVersionBound { value: g(s), inclusive: true }
    }

    fn excl(s: &str) -> GameVersionBound {
        GameVersionBound { value: g(s), inclusive: false }
    }

    fn module(extra: &str) -> Module {
        let j = format!(
            r#"{{"spec_version":1,"identifier":"Mod","name":"Mod","abstract":"a","author":"x",
                "version":"1.0","license":"MIT","download":"https://x/y.zip"{extra}}}"#
        );
        Module::from_json(j.as_bytes()).unwrap()
    }

    fn crit_1125() -> VersionCriteria {
        let inst = g("1.12.5.3190");
        VersionCriteria::new(Some(inst), &default_compatible_versions(&inst))
    }

    #[test]
    fn parse_rules() {
        assert_eq!(g("any"), GameVersion::ANY);
        assert!(GameVersion::parse(" any").is_none());
        assert_eq!(g(" 1.12 "), GameVersion::new(1, 12, -1, -1));
        assert_eq!(g("1.12.5.3190"), GameVersion::new(1, 12, 5, 3190));
        assert!(GameVersion::parse("1.2.3.4.5").is_none());
        assert!(GameVersion::parse("1..2").is_none());
        assert!(GameVersion::parse("1.x").is_none());
        assert!(GameVersion::parse("").is_none());
        assert!(GameVersion::parse("1.99999999999").is_none());
        // i32::MAX is undefined and cuts the version.
        assert_eq!(g("1.2147483647.5"), GameVersion::new(1, -1, -1, -1));
        assert!(GameVersion::parse("2147483647.1").is_none());
        // JSON form strips one trailing dot.
        let j: GameVersion = serde_json::from_str("\"1.1.\"").unwrap();
        assert_eq!(j, g("1.1"));
        assert!(serde_json::from_str::<GameVersion>("\"1.1..\"").is_err());
        // Regression: `\.$` also matches before one final newline.
        assert_eq!(GameVersion::parse_json("1.1.\n"), Some(g("1.1")));
        assert_eq!(GameVersion::parse_json("1.1\n"), Some(g("1.1")));
        assert_eq!(GameVersion::parse_json("1.1.\n\n"), None);
    }

    #[test]
    fn order() {
        assert!(GameVersion::ANY < g("1.12"));
        assert!(g("1.12") < g("1.12.0"));
        assert!(g("1.12.0") < g("1.12.0.0"));
        assert!(g("1.9") < g("1.10"));
    }

    #[test]
    fn ranges() {
        let r = |lo, hi| GameVersionRange { lower: lo, upper: hi };
        assert_eq!(g("1.2.3.4").to_range(), r(incl("1.2.3.4"), incl("1.2.3.4")));
        assert_eq!(g("1.2.3").to_range(), r(incl("1.2.3.0"), excl("1.2.4.0")));
        assert_eq!(g("1.2").to_range(), r(incl("1.2.0.0"), excl("1.3.0.0")));
        assert_eq!(g("1").to_range(), r(incl("1.0.0.0"), excl("2.0.0.0")));
        assert_eq!(GameVersion::ANY.to_range(), GameVersionRange::ANY);
        assert_eq!(g("1.2").to_range().to_string(), "[1.2.0.0,1.3.0.0)");
    }

    #[test]
    fn bounds_highest_lowest() {
        let u = GameVersionBound::UNBOUNDED;
        assert_eq!(GameVersionBound::highest(u, incl("1.0.0.0")), incl("1.0.0.0"));
        assert_eq!(GameVersionBound::lowest(incl("1.0.0.0"), u), incl("1.0.0.0"));
        assert_eq!(GameVersionBound::highest(incl("1.2.0.0"), incl("1.3.0.0")), incl("1.3.0.0"));
        assert_eq!(GameVersionBound::lowest(incl("1.2.0.0"), incl("1.3.0.0")), incl("1.2.0.0"));
        // Equal values: the exclusive bound wins both ways.
        assert_eq!(GameVersionBound::highest(incl("1.2.0.0"), excl("1.2.0.0")), excl("1.2.0.0"));
        assert_eq!(GameVersionBound::lowest(incl("1.2.0.0"), excl("1.2.0.0")), excl("1.2.0.0"));
        assert_eq!(GameVersionBound::highest(u, u), u);
    }

    #[test]
    fn intersect_and_contains() {
        let a = g("1.12").to_range();
        assert!(a.contains(&g("1.12.5.3190")));
        assert!(a.contains(&g("1.12.5")));
        assert!(!a.contains(&g("1.13")));
        assert!(!a.contains(&g("1.11.9.9")));
        // Touching ranges: [1.7.3.0, 1.7.4.0) and [1.7.4.0, 1.7.5.0) do not meet.
        assert!(g("1.7.3").to_range().intersect(&g("1.7.4").to_range()).is_none());
        assert!(GameVersionRange::ANY.contains(&g("1.0")));
        assert!(GameVersionRange::ANY.intersect(&GameVersionRange::ANY).is_some());
        // QUIRK: bounded lower and both uppers unbounded -> empty.
        let min_only = GameVersionRange::between(&g("1.8"), &GameVersion::ANY);
        assert!(min_only.intersect(&GameVersionRange::ANY).is_none());
        assert!(min_only.contains(&g("1.9")));
    }

    #[test]
    fn criteria_min_max() {
        // The default 1.12 entry gives upper (1.13.0.0 exclusive), which beats 1.12.5.3190
        // inclusive, and AsInclusiveUpper turns it into 1.12.
        let c = crit_1125();
        assert_eq!(c.min_max(), Some((g("1.8"), g("1.12"))));
        let c = VersionCriteria::new(Some(g("1.12.5.3190")), &[]);
        assert_eq!(c.min_max(), Some((g("1.12.5"), g("1.12.5"))));
        let c = VersionCriteria::new(Some(g("1.12")), &[]);
        assert_eq!(c.min_max(), Some((g("1.12"), g("1.12"))));
        let c = VersionCriteria::new(Some(g("1.10.1")), &[g("1.9")]);
        assert_eq!(c.min_max(), Some((g("1.9"), g("1.10.1"))));
        assert_eq!(VersionCriteria::default().min_max(), None);
        assert_eq!(excl("2.0.0.0").as_inclusive_upper(), g("1"));
        assert_eq!(excl("1.3.1.0").as_inclusive_lower(), g("1.3.2"));
        assert_eq!(incl("0.0.0.0").as_inclusive_lower(), GameVersion::ANY);
    }

    #[test]
    fn defaults() {
        let d = default_compatible_versions(&g("1.12.5.3190"));
        assert_eq!(d, ["1.8", "1.9", "1.10", "1.11", "1.12"].map(g).to_vec());
        assert!(default_compatible_versions(&g("1.2.9")).is_empty());
        assert_eq!(default_compatible_versions(&g("1.3.1")), vec![g("1.2"), g("1.3")]);
        assert_eq!(default_compatible_versions(&g("1.8")), vec![g("1.8")]);
        assert_eq!(default_compatible_versions(&g("1.0.5")), vec![g("1.0")]);
        assert_eq!(default_compatible_versions(&g("0.90")), vec![g("0.90")]);
    }

    #[test]
    fn spec_compat_examples() {
        let c = crit_1125();
        // ksp_version 1.7.3: incompatible.
        assert!(!module(r#","ksp_version":"1.7.3""#).is_compatible(&c));
        // ksp_version_max 1.9: compatible only through the extra list.
        let m = module(r#","ksp_version_max":"1.9""#);
        assert!(m.is_compatible(&c));
        assert!(!m.is_compatible(&VersionCriteria::new(Some(g("1.12.5.3190")), &[])));
        // min 1.12.3, max 1.12: valid (bound values compared), compatible.
        assert!(module(r#","ksp_version_min":"1.12.3","ksp_version_max":"1.12""#).is_compatible(&c));
        // min 1.12, max 1.8: never.
        assert!(!module(r#","ksp_version_min":"1.12","ksp_version_max":"1.8""#).is_compatible(&c));
        // No fields, or "any": compatible.
        assert!(module("").is_compatible(&c));
        assert!(module(r#","ksp_version":"any""#).is_compatible(&c));
        // Empty criteria: compatible.
        assert!(module(r#","ksp_version":"1.7.3""#).is_compatible(&VersionCriteria::default()));
        // QUIRK: min only, with Any in the criteria -> that criterion does not match.
        let m = module(r#","ksp_version_min":"1.8""#);
        assert!(!m.is_compatible(&VersionCriteria::new(Some(GameVersion::ANY), &[])));
        assert!(m.is_compatible(&c));
        // min + max "any": lower > Any upper -> never.
        assert!(!module(r#","ksp_version_min":"1.8","ksp_version_max":"any""#).is_compatible(&c));
        // Exact build match.
        let m = module(r#","ksp_version":"1.12.5""#);
        assert!(m.is_compatible(&VersionCriteria::new(Some(g("1.12.5.3190")), &[])));
        assert!(!m.is_compatible(&VersionCriteria::new(Some(g("1.12.4.3190")), &[])));
    }

    #[test]
    fn latest_real() {
        let real = ["1.8.1", "1.9.1", "1.10.1", "1.11.2", "1.12.5"].map(g);
        let m = module(r#","ksp_version_min":"1.8","ksp_version_max":"1.10""#);
        assert_eq!(m.latest_compatible_real_game_version(&real), g("1.10.1"));
        let m = module(r#","ksp_version":"1.3""#);
        assert_eq!(m.latest_compatible_real_game_version(&real), g("1.3"));
        assert_eq!(module("").latest_compatible_real_game_version(&real), g("1.12.5"));
    }
}
