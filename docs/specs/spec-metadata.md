# PORT SPEC: Metadata model + versions

Source: CKAN v1.36.5 (commit c0650d4). Paths are relative to `ckan-src/`. Real-data counts come from 31445 `.ckan` files (3878 distinct identifiers) in CKAN-meta-master. Only `*.ckan` files are modules. The 649 `.frozen` and 1123 `.kerbalstuff` files are not modules.

## 1. Module fields (`Core/Types/CkanModule.cs:38-193`, `CKAN.schema`)

The parser ignores unknown keys (Newtonsoft default), including `x_*`, `comment` in relationships, and the typos found in the data (`suggest`, `suggested`, `Install`, `ksp_min_version`, `release-status`). **Do not use `deny_unknown_fields`.**

| key | JSON form | Rust type / rule | default | real count |
|---|---|---|---|---|
| `spec_version` | int `1` or `"vX.Y"` | `ModuleVersion` (from `"1"` or the string). Real values: `v1.4`(17256), `1`(6446, int), up to `v1.36`. Also `v1.01` and `v1.00`, which break the schema pattern | `"1"` | 31445 |
| `identifier` | string `^[A-Za-z0-9][A-Za-z0-9-]+$` | `String`, required | – | all |
| `name` | string | `String` | `""` if missing (:413) | all |
| `abstract` | string | `String` | `""` (:412) | all |
| `description` | string | `Option<String>` | | 528 |
| `comment` | string | `Option<String>` | | 1074 |
| `author` | string OR string[] | `Vec<String>`, `OneOrMany` | if absent: spec<v1.28 gives `[""]`, otherwise error (:346-358) | str 24984 / arr 6461 (up to 10 items) |
| `license` | string OR string[] (enum) | `Vec<License>` | `["unknown"]` (:411) | str 31277 / arr 168 |
| `version` | string | `ModuleVersion`, required | – | all are strings |
| `kind` | `package`/`metapackage`/`dlc` | enum | `package` | 69 (35 package, 34 dlc, 0 metapackage) |
| `download` | URI string OR URI[] (v1.34) | `Vec<String>`, `OneOrMany` | required when kind==package (:360) | str 29615 / arr 1796 (always 2 items) |
| `download_size` | integer | `u64` (max seen 5050408362, more than i32) | 0 | 31371 |
| `download_hash` | `{sha1?, sha256?}` | both `Option<String>` | | both 30999; sha256 only 309 |
| `download_content_type` | string | `String` | `"application/zip"` | zip 31235, `application/vnd.github+json` 73 |
| `install_size` | integer | `u64` | 0 | 8615 |
| `ksp_version`, `ksp_version_min`, `ksp_version_max` | string `any`, `N.N` or `N.N.N` | `Option<GameVersion>` (§4) | None (means any) | 16350 / 14505 / 12989 |
| `ksp_version_strict` | bool | `bool` (no effect on compatibility, §5) | false | 128 (127 true) |
| `release_status` | `stable`/`testing`/`development` | enum | stable | stable 3820, testing 310 |
| `resources` | object | §3 | | 31434 |
| `tags` | string[] `^[a-z0-9-]+$` | `HashSet<String>` (remove duplicates) | | 14668 |
| `localizations` | string[] | `Vec<String>`. Free-form values (`en-us`, `ru`, `PT-BR`, `RP`...) | | 4696 |
| `provides` | identifier[] | `Vec<String>` | | 3015 |
| `depends` `recommends` `suggests` `supports` `conflicts` | array of relationships | §2 | | 22688/7909/7411/1781/6455 |
| `replaced_by` | object `{name, version?, min_version?}` | `ModuleRelationshipDescriptor` (normal deserialization, not the relationship converter) | | 15 (name only) |
| `install` | array of stanzas | §3 | empty/absent gives the default stanza `{find: identifier, install_to: "GameData"}` (:143-151, ModuleInstallDescriptor.cs:137) | 23605 |
| `release_date` | ISO-8601 string | `DateTime<Utc>` (parsed as UTC, :282) | | 13408 |

Rules checked after load (`CkanModule.cs:302-375`, `:405-418`):
- If `ksp_version` is present together with `ksp_version_min` or `ksp_version_max`, the module is invalid (InvalidModuleAttributesKraken). Real data: 0 cases.
- `release_status` must be one of the 3 values. The converter maps `"alpha"` to development and `"beta"` to testing. A JSON `null` gives stable. `""` is an error (`Core/Converters/JsonReleaseStatusConverter.cs:14-21`). Any other string is parsed by Newtonsoft StringEnumConverter. UNVERIFIED: that converter may also accept case-insensitive names and integers.
- `kind` uses the same enum converter. If `kind==dlc`, the version becomes an `UnmanagedModuleVersion` with the same comparison but a different display (:415).
- License: if a string is not in the `definitions.license` enum, the module is rejected (`License.cs:40-48`). Real data: 0 invalid values. Redistributable = in `redistributable_license`. NOT redistributable: `AFL-3.0 AGPL-3.0 APSL-2.0 MPL-1.0 Ms-PL Ms-RL restricted unknown`.
- Spec support: the client loads a module only if `ClientSpec > spec_version` (ModuleVersion compare, `CkanModule.cs:703`). `ClientSpec` = the client release (for example `v1.36.5`). A dev build with an odd patch uses `v1.(minor+1).999` (`Meta.cs:24-31`). Otherwise the module is rejected with UnsupportedKraken. Hangar: use `"v1.36.999"`.
- Equality of two modules = `identifier` and `version` are equal (version compare == 0) (:567).

## 2. Relationships (`Core/Converters/JsonRelationshipConverter.cs`, `Core/Types/RelationshipDescriptor.cs`)

Deserialize from a raw JSON value:
```
if value is not array -> None  (silently drops an object or a string)
for each child that is an OBJECT (skip non-objects):
  if child has "any_of":
     if child also has any of [name, version, min_version, max_version] -> ERROR (whole module fails)
     -> AnyOf { any_of: recurse(child.any_of), choice_help_text?, suppress_recommendations }
  else if child has "name":
     -> Module { name, version?, min_version?, max_version?, choice_help_text?, suppress_recommendations }
  else skip silently
```
Common fields: `choice_help_text: Option<String>`, `suppress_recommendations: bool` (default false). Version fields are ModuleVersion strings. Nested `any_of` is allowed (recursive). Real data: 0 nested cases.

Key sets in the real data: `name` 125320, `min_version,name` 6905, `name,suppress_recommendations` 1402, `any_of` 869 (inner items are always `{name}`), `max_version,name` 478, `comment,name` 435, `name,version` 63, `min,max,name` 37, `choice_help_text,name` 2, `any_of,choice_help_text` 2. All version values are strings.

Matching (`RelationshipDescriptor.cs:93-150`):
```
Module.within_bounds(mod):
  if mod.identifier == name: within_bounds_ver(mod.version)
  else: mod.provides contains name            # NO version check through provides
within_bounds_ver(v):
  v is None or v is unmanaged-unknown -> true
  if version set: return version == v        # exact match (compare==0); min/max ignored
  (min is None or min <= v) and (max is None or max >= v)   # both bounds inclusive
Module.matches_any(mods, dlls, dlc):
  name in dlls -> true (any version)
  first mod in mods with within_bounds -> true
  dlc[name] exists and within_bounds_ver(dlc_ver)
AnyOf.matches_any = any child matches (C# uses unordered PLINQ; Hangar: first match in order)
AnyOf.within_bounds = any child within_bounds
ContainsAny(ids): Module: name in ids; AnyOf: any child
```
Equality: Module compares (name, version, min, max). AnyOf compares the sequences in order.

## 3. Resources and install stanzas

`resources` (`Core/Types/ResourcesDescriptor.cs`) has these keys: `homepage, spacedock, curse, repository, bugtracker, discussions, ci, license, manual, metanetkan, remote-avc, remote-swinfo, store, steamstore, gogstore, epicstore`, all `Option<Url>`.
- `homepage, repository, bugtracker, discussions, ci, license, manual`: an invalid URL becomes None, with no error (`JsonIgnoreBadUrlConverter.cs:20-35`).
- `spacedock, curse`: the value can be a string OR an object `{"url": ...}`. Use `.url` (`JsonOldResourceUrlConverter.cs:23-32`).
- Ignore other keys. Real data has `x_screenshot` 14786 and `kerbalstuff` 2353. These are not in the model.

Real download URLs can contain raw spaces (forgecdn). .NET `Uri` accepts them. Store `download` as `String` and percent-encode when you fetch. UNVERIFIED: Newtonsoft may also accept relative URIs.

`install[]` stanza (`ModuleInstallDescriptor.cs:28-115`):
- `file | find | find_regexp`: exactly one is required. If 0 or more than 1 is present, the module is rejected.
- `install_to`: string, default `"GameData"`. It is normalized (path separators).
- `as`: Option<String>.
- `find_matches_files`: bool, default false. Real data: 52 stanzas use the typo `find_matches_file`. It is IGNORED, so the value stays false.
- `filter, filter_regexp, include_only, include_only_regexp`: `OneOrMany<String>`. Real data: `filter` 1812 str / 1450 arr, `filter_regexp` 429 str / 252 arr.
- Using both a `filter*` and an `include_only*` key is an error.
- Ignored keys: `comment`, `optional`, `description`.
- Real `install_to` values: `GameData`, `GameData/...`, `Ships`, `GameRoot`, `Ships/SPH`, `Ships/VAB`, `Scenarios`, `Missions`, `Tutorial`, `Ships/Script`.

## 4. ModuleVersion (`Core/Versioning/ModuleVersion.cs`)

Parse (:28-62):
- Regex `^(?:(?<epoch>[0-9]+):)?(?<version>.*)$`. The epoch is ASCII digits. `Convert.ToInt32` throws if the epoch overflows. `.` does not match `\n`, so a version with an embedded newline does not parse (error).
- The epoch match is greedy: `"1:2:3"` gives epoch=1, version=`"2:3"`.
- No epoch gives 0. The string is not trimmed.
- JSON input: the value is converted with `.ToString()`. A JSON number would become a string (real data: all strings).
- Real data: 4153 versions have an epoch (1: 2727, 2: 1113, 3: 276, 4: 36, 5: 1). 7893 start with `v`/`V` after the epoch. 76 contain spaces (`"2.1 "`, `"1.0.7 - Hotfix"`).

Compare (:227-415), exact algorithm:
```
cmp(a, b):
  if a.epoch == b.epoch && a.ver == b.ver (ordinal string eq): return 0
  if a.epoch != b.epoch: return sign(a.epoch - b.epoch)
  r1, r2 = a.ver, b.ver; c = 0
  while r1 != "" && r2 != "":
     # string step
     s1, r1 = split at first char where is_numeric(ch)   # s1 = whole string if no digit, r1 = ""
     s2, r2 = same for r2
     if s1 != "" && s2 != "":
        f1, f2 = s1[0], s2[0]
        if   f1 != '.' && f2 == '.': c = -1
        elif f1 == '.' && f2 != '.': c = +1
        elif f1 == '.' && f2 == '.':
             c = (len(s1)==1 && len(s2)>1) ? +1 : (len(s1)>1 && len(s2)==1) ? -1 : 0   # QUIRK: ".a" vs ".b" gives 0
        else: c = ordinal_utf16(s1, s2)
     else: c = ordinal_utf16(s1, s2)          # "" < anything
     if c != 0: return sign(c)
     # number step
     n1, r1 = split at first char where !is_numeric(ch)
     n2, r2 = same for r2
     i1 = parse_i32_ascii(n1) or 0   # empty, OVERFLOW (>2147483647) and non-ASCII numerics all give 0
     i2 = parse_i32_ascii(n2) or 0
     if i1 != i2: return sign(i1 - i2)
  if r1 == "": return (r2 == "") ? 0 : -1
  return +1
```
- `is_numeric` = .NET `char.IsNumber` (Unicode Nd, Nl, No). This is the same as Rust `char::is_numeric`. Apply it per UTF-16 unit. Real data: all versions are ASCII, so `is_ascii_digit` behaves the same today.
- `ordinal_utf16` = `string.CompareOrdinal`. It compares UTF-16 code units, so uppercase sorts before lowercase.
- Spec.md:244-273 says "letters before non-letters". The code does NOT do that. Copy the code.
- Equality is `cmp == 0`, so equal values can have different strings. **Do not derive `Hash`/`Eq` from the string.** C# `GetHashCode` hashes (epoch, ver), which does not agree with Equals. Do not key maps by ModuleVersion.

Worked examples:
| a | b | result | why |
|---|---|---|---|
| `1.01` | `1.1` | == | numbers 01 and 1 are equal (test ModuleVersionTests.cs:35) |
| `1.0` | `1.0.0` | < | a runs out first |
| `1.0` | `1.0-beta` | < | runs out first: pre-release suffixes sort AFTER the release |
| `1.0_beta` | `1.0.1_beta` | < | `_` vs `.`: a non-dot sorts lower |
| `1.0.repackaged` | `1.0.1` | < | `.repackaged` (len>1) vs `.` (len 1) gives -1 |
| `1.a2` | `1.b2` | == | QUIRK: both segments start with `.`, both longer than 1 |
| `v1.2` | `1.10` | > | `"v"` vs `""` |
| `V1.0` | `v0.1` | < | 'V' 0x56 < 'v' 0x76 |
| `v6a12` | `v6a5` | > | 12 > 5 |
| `1:0.1` | `9.9` | > | epoch |
| `0:1.0` | `1.0` | == | epoch 0 is implicit |
| `v1.1.0.202206081400` | `v1.1.0.202206081500` | == | both numbers overflow i32, so both are 0 (real mod: CParts) |
| `1.0` | `1.0.` | < | runs out first |

## 5. GameVersion (`Core/Versioning/GameVersion.cs`)

Model: `major, minor, patch, build: i32`. Undefined = -1. `Any` = all four are -1. Its `ToString` is null.

Parse (:354-459):
```
if s == "any" (exact, before trim): Any
m = ^(\d+)(?:\.(\d+)(?:\.(\d+)(?:\.(\d+))?)?)?$ on s.trim()   else fail
each group: i32 parse (overflow -> fail); value == i32::MAX -> Undefined
build from the first undefined component (1, 2, 3 or 4 parts)
```
JSON read (:965-990): null gives None. Remove ONE trailing `.` (`1.1.` becomes `1.1`), then parse. Failure is a hard error for the module. Real data: `N.N.N` 40388, `N.N` 3204, `any` 252 (always `ksp_version`). No 4-part values.

Compare (:874-907): lexicographic on (major, minor, patch, build) with -1 for undefined. So `Any < 1.12 < 1.12.0 < 1.12.0.0`. Equality is component-wise.

`ToVersionRange` (:286-318):
- 4 parts: `[v, v]`
- `a.b.c`: `[a.b.c.0, a.b.(c+1).0)`
- `a.b`: `[a.b.0.0, a.(b+1).0.0)`
- `a`: `[a.0.0.0, (a+1).0.0.0)`
- Any: `(Unbounded, Unbounded)`. Unbounded = `{value: Any, inclusive: true}`.

### Range ops (`GameVersionRange.cs`, `GameVersionBound.cs`)
```
Range(lowerGV, upperGV) = (lowerGV.ToVersionRange().lower, upperGV.ToVersionRange().upper)
Highest(bounds): sort by (is_unbounded asc, value DESC, inclusive asc [false first]), take first
Lowest(bounds):  sort by (is_unbounded asc, value ASC,  inclusive asc), take first
   -> a bounded value always wins over Unbounded
is_empty(lo, hi) = hi.value < lo.value || (lo.value == hi.value && (!lo.incl || !hi.incl))
intersect(a, b) = lo = Highest(a.lo, b.lo); hi = Lowest(a.hi, b.hi); is_empty ? None : (lo, hi)
contains(range, gv) = intersect(range, gv.ToVersionRange()).is_some()
```
Edge case: if both uppers are Unbounded and the lower is bounded, then `Any(-1) < lower` and the range counts as EMPTY. This happens only when the criteria contain `Any`. Copy it as it is.

## 6. Compatibility (`Core/Types/GameComparator/*.cs`, `GameVersionCriteria.cs`, `ServiceLocator.cs:24`)

Only `StrictGameComparator` is registered. **`ksp_version_strict` is read but never used for compatibility.** The leniency comes from the user's list of extra compatible versions.
```
GameVersionCriteria = distinct([installed_game_version] + compatible_versions)
module.is_compatible(crit) = crit.versions.is_empty() || crit.versions.any(gv -> single(gv, module))
single(gv, m):
  if m.ksp_version:                       mr = m.ksp_version.ToVersionRange()
  elif m.ksp_version_min && m.ksp_version_max:
        mr = Range(min, max); if mr.lo.value > mr.hi.value: return false
  elif min only:                          mr = Range(min, Any)    # upper Unbounded
  elif max only:                          mr = Range(Any, max)    # lower Unbounded
  else: return true                       # no game version fields means "any"
  return intersect(gv.ToVersionRange(), mr).is_some()
```
(`ksp_version: "any"` gives the full range, so the module is compatible.)

The criteria come from `GameInstance.cs:144`: `new GameVersionCriteria(Version(), compatibleVersions)`.
- The compatible list is read from `<game>/CKAN/compatible_ksp_versions.json`, in the form `{"GameVersionWhenWritten": "1.12.5.3190", "Versions": ["1.12","1.11",...]}`. The legacy key names `VersionOfKspWhenWritten` and `CompatibleKspVersions` are also accepted (`Configuration/CompatibleGameVersions.cs`).
- When saved, the list is distinct and sorted descending.
- If the file is missing, use the default from `KerbalSpaceProgram.cs:264-282`:
  - installed `1.2.9`: no defaults.
  - otherwise take the first of [1.8, 1.4, 1.2, 1.1] that is <= installed, and return every `1.m` from that version's minor up to the installed minor. Example: 1.12.5 gives [1.8, 1.9, 1.10, 1.11, 1.12].
  - otherwise (pre-1.1): `[major.minor]`.

Worked examples (installed `1.12.5.3190` plus the defaults):
- `ksp_version 1.7.3` gives `[1.7.3.0, 1.7.4.0)`. Against `1.8`: the lower bound 1.8.0.0 is above the upper bound 1.7.4.0, so the result is empty. The same is true for every criterion. **Incompatible.**
- `ksp_version_max 1.9` gives `(Unb, 1.10.0.0)`. Against `1.9` = `[1.9.0.0, 1.10.0.0)`, the intersection is not empty. **Compatible** (only through the extra list).
- `min 1.12.3, max 1.12` gives `[1.12.3.0, 1.13.0.0)`. This is valid even though the raw strings look reversed (real cases: TUFX, RocketSoundEnhancement). Against 1.12.5.3190: **compatible**. You must compare the BOUND values, not the raw GameVersions (raw 1.12.3 > 1.12).
- `min 1.12, max 1.8` gives lower 1.12.0.0 > upper 1.9.0.0, so the result is false.

Helpers:
- `LatestCompatibleGameVersion` = max ?? ksp_version ?? Any.
- `Earliest...` = min ?? ksp_version ?? Any.
- `LatestCompatibleRealGameVersion(realVers)` = the last element of `realVers` that is in `Range(earliest, latest)`, or else `Latest...` (`CkanModule.cs:531-561`).

## 7. Serialization notes (registry write-back)

- `ToJson` uses a 4-space indent and the `Order` attributes from §1 (`CkanModule.cs:470-484`).
- A single-item list is written as a scalar (`JsonSingleOrArrayConverter.cs:30-37`).
- Default values are omitted on write.

## 8. Rust serde checklist

- `OneOrMany<T>` for author, license, download and the install filter keys. JSON null gives None.
- `ModuleVersion`: implement `Ord` by hand (§4). Do not derive Hash.
- `release_date`: RFC 3339 with 0 or 2-7 fraction digits, `Z` or `+hh:mm` (real data has 10 shapes).
- Skip and log the module on any rejection rule in §1 and §3.
