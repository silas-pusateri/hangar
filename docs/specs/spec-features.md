```
+==============================================================+
|  HANGAR PORT SPEC :: USER FEATURES + EXPORT / IMPORT  v1.0   |
|  src: CKAN C# (read-only clone)   refs: path:line            |
+==============================================================+
```

Refs are relative to `ckan-src/`. UNVERIFIED = not proved from source.

## 1. Export

### 1.1 Format table

| CLI `list --export X` (Cmdline/Action/List.cs:168-178, case-insensitive) | GUI filter label / ext (GUI/Main/MainExport.cs:71-78, EditModpack.cs:440) | Writer |
|---|---|---|
| `ckan` | `CKAN modpack (*.ckan)` / `ckan` | CkanExporter |
| `text` | `Plain text (*.txt)` / `txt` | PlainTextExporter |
| `markdown` | `Markdown (*.md)` / `md` | MarkdownExporter |
| `bbcode` | `BBCode (*.txt)` / `txt` | BbCodeExporter |
| `csv` | `Comma-separated values (*.csv)` / `csv` | DSV, delim `,` |
| `tsv` | `Tab-separated values (*.tsv)` / `tsv` | DSV, delim `\t` |

- Unknown format: CLI shows an error, then prints the normal list (List.cs:36-40). With export set, no header or legend; output goes to stdout.
- GUI: "Save installed mod list (cannot be re-imported)..." gives the 5 text formats. "Export modpack (can be re-imported)..." opens the editor (§1.4). Existing files are truncated (MainExport.cs:95).
- `StreamWriter(stream, Encoding.UTF8)` probably writes a BOM (UNVERIFIED). Line end = `Environment.NewLine`. Hangar: UTF-8, no BOM, LF.

### 1.2 Text writers (Core/Exporters/*.cs)
Rows are `registry.InstalledModules` sorted by `Module.name`, using the C# default comparer, which is culture-aware. Hangar: compare case-insensitively, then ordinally. The rows include auto-installed mods and DLC. Unmanaged DLLs are not rows. `version` is `ModuleVersion.ToString()` and keeps the epoch, e.g. `1:2.0`.

- Plain (PlainTextExporter.cs:15): `{name} ({identifier} {version})`
- Markdown (MarkdownExporter.cs:14): ``- **{name}** `{identifier} {version}` ``
- BBCode (BbCodeExporter.cs:12-19): line `[LIST]`, then one line `[*][B]{name}[/B] ({identifier} {version})` for each mod, then line `[/LIST]`.
- CSV/TSV (DelimiterSeparatedValueExporter.cs:25-69). The header is in this exact order:
  `identifier,version,name,abstract,description,author,kind,download,download_size,ksp_version,ksp_version_min,ksp_version_max,license,release_status,repository,homepage,bugtracker,discussions,spacedock,curse`
  - `QuoteIfNecessary` (:93-98): null becomes empty. It adds `"…"` ONLY if the value contains the delimiter. It does NOT escape quotes or newlines (defect: a multi-line description breaks the row). Hangar decision: copy it, or use RFC 4180.
  - Quoted when needed: name, abstract, description, author (`;`-joined), kind, license (`;`-joined), URLs. Raw: identifier, version, download_size (`0` if absent), ksp_version/_min/_max (empty if null), release_status.
  - `download` = the first URL in the list. All URLs go through `Net.NormalizeUri` (Core/Net/Net.cs:239-266): replace space with `+`, percent-escape `" < > ^ \` ( ) { | } [ ]`, prepend `http://` if there is no `http(s)://`, and return empty if the result is not a well-formed URI.
  - The resource columns come from `resources.{repository,homepage,bugtracker,discussions,spacedock,curse}`.

Samples (two installed mods):
```
Kopernicus Planetary System Modifier (Kopernicus release-1.12.1-215)
Module Manager (ModuleManager 4.2.3)
```
```
- **Kopernicus Planetary System Modifier** `Kopernicus release-1.12.1-215`
- **Module Manager** `ModuleManager 4.2.3`
```
```
[LIST]
[*][B]Kopernicus Planetary System Modifier[/B] (Kopernicus release-1.12.1-215)
[*][B]Module Manager[/B] (ModuleManager 4.2.3)
[/LIST]
```
CSV row (the abstract has a comma, so it is quoted; TSV uses tabs and does not quote it):
```
ModuleManager,4.2.3,Module Manager,"Edit configs, no conflict",,sarbian;Blowfish,package,https://x.test/MM-4.2.3.zip,30000,,1.8.0,1.12.99,CC-BY-SA,stable,https://github.com/sarbian/ModuleManager,,,,,
```

### 1.3 Modpack generation: `RegistryManager.GenerateModpack(recommends=false, with_versions=true)` (Core/Registry/RegistryManager.cs:477-530)
```
name       = "installed-" + instance.Name                       // Core resx ModpackName
identifier = Sanitize(name)   // strip leading [^A-Za-z0-9]+, then [^A-Za-z0-9-] -> "-" (Core/Types/Identifier.cs:27-38)
abstract   = "A list of modules installed on the {instance.Name} KSP instance"
author     = [Environment.UserName]   (OS login name)
license    = ["unknown"]
version    = UtcNow formatted "yyyy.MM.dd.hh.mm.ss"   // NOTE: hh = 12-hour clock (CKAN quirk); Hangar may use HH
kind       = metapackage
spec_version = SpecVersionAnalyzer.MinimumSpecVersion(module) -> "v1.6" in practice (kind=metapackage rule, SpecVersionAnalyzer.cs)
range      = instance.VersionCriteria().MinAndMax  // union of ranges of game version + compat versions (GameVersionCriteria.cs:33-39)
ksp_version_min = range.Lower.AsInclusiveLower().WithoutBuild
ksp_version_max = range.Upper.AsInclusiveUpper().WithoutBuild   (GameVersionBound.cs:35-61)
release_date    = DateTime.Now, serialized as UTC ISO-8601 with 7 fractional digits + "Z"
mods = InstalledModules where !IsDLC && !AutoInstalled && LatestAvailable(id) does not throw   // :501-505, :533-546
        // NOTE: IsAvailable ignores compat; it drops mods that are in NO repo (custom .ckan installs)
order = RelationshipResolver(mods).ModList() ∩ mods      // dependencies before dependers
rels  = each -> {name: id, version: installed version}  (with_versions) | {name: id}
recommends ? module.recommends = rels : module.depends = rels
```
Bounds: `ToVersionRange` (GameVersion.cs:286-318) maps `1.12` to `[1.12.0.0, 1.13.0.0)`; a build version is one point. `AsInclusiveLower` drops trailing zeros. `AsInclusiveUpper` on an exclusive bound steps down one part (`1.13.0.0` to `1.12`). Test: Tests/CmdLine/ListTests.cs:254-275.

Held mods ("Don't upgrade" labels) get no special treatment in export. Labels are not exported.

Callers and differences:
| Caller | args | post-processing |
|---|---|---|
| `list --export ckan` (CkanExporter.cs:13) | (false, **false**) | none, so no versions |
| `RegistryManager.Save()` (:397-438), on every registry save | (false, true) | writes `<CkanDir>/installed-<SanitizedName>.ckan` AND `<CkanDir>/history/installed-<SanitizedName>-yyyy-MM-dd_HH-mm-ss.ckan` (local time) |
| GUI Export modpack (MainExport.cs:32) | (false, true) | the editor in §1.4 |

`SanitizedName` = instance name with the OS invalid filename chars removed (GameInstance.cs:105). On macOS these are only `/` and NUL.

JSON serialization (CkanModule.ToJson, CkanModule.cs:470-481): 4-space indent, a trailing newline, and keys in `Order`. Null values and default values are omitted: kind `package`, release_status `stable`, download_content_type `application/zip`, ksp_version_strict false, download_size 0. A single-element `author` or `license` becomes a bare string (JsonSingleOrArrayConverter.cs:30-38). Key order: spec_version, comment, identifier, name, abstract, description, author, version, ksp_version, ksp_version_min, ksp_version_max, ksp_version_strict, license, release_status, resources, tags, localizations, provides, depends, recommends, suggests, supports, conflicts, install, download, download_size, download_hash, download_content_type, install_size, release_date, kind. Relationship object keys: `name`, `version`/`min_version`/`max_version`, `choice_help_text`, `suppress_recommendations` (only if true). The exact key order inside a relationship is UNVERIFIED.

Sample (`installed-<instance>.ckan`, instance "My KSP"):
```json
{
    "spec_version": "v1.6",
    "identifier": "installed-My-KSP",
    "name": "installed-My KSP",
    "abstract": "A list of modules installed on the My KSP KSP instance",
    "author": "jdoe",
    "version": "2026.11.24.06.30.12",
    "ksp_version_min": "1.12",
    "ksp_version_max": "1.12.5",
    "license": "unknown",
    "depends": [
        {
            "name": "ModuleManager",
            "version": "4.2.3"
        },
        {
            "name": "Kopernicus",
            "version": "release-1.12.1-215"
        }
    ],
    "release_date": "2026-11-24T18:30:12.1234567Z",
    "kind": "metapackage"
}
```

### 1.4 GUI modpack editor (GUI/Controls/EditModpack.cs)
- Fields (:44-73): Identifier, Name, Abstract, Author (`", "`-joined), Version, game min/max (`""` + KnownVersions as X.Y.Z and X.Y, descending), License (sorted valid licenses).
- Groups: Depends, Recommendations, Suggestions, Ignored. Ignored = `registry.Installed(withProvides:false, withDLLs:false)` ids that are in no rel list. This includes auto-installed mods, DLC and non-repo mods, sorted by id, with the installed version (:94-115). Each row shows: name, version, the installed abstract. Radio buttons move the selected rows between groups. Ctrl+A selects all rows. Esc clears the selection.
- Validation (:150-195): the identifier must match `^[A-Za-z0-9][A-Za-z0-9-]+$`. Name and version must not be empty. If both are set, min must be <= max. Author is split on `,` and each part is trimmed. License is one value.
- On Export (:316-345): set empty rel lists to null. Recompute spec_version. Then apply:
  - "Save mod versions" checkbox (default **unchecked**): when unchecked, clear `version/min_version/max_version` on all rels (:351-376).
  - "Include optional relationships" checkbox (default **unchecked**): when unchecked, set `suppress_recommendations: true` on every **depends** rel (:378-397).
  - Write the file, then open the file browser at it.
  - So the GUI default output has depends with no versions and with `suppress_recommendations: true`.

## 2. Import

### 2.1 Install from .ckan (GUI File menu, Main.cs:709-834; filter `CKAN metadata (*.ckan)|*.ckan`, multiselect)
```
installed = InstalledModules
for path in files:
  m = parse .ckan (on error: show the error, skip this file)
  if m.kind==metapackage && m.depends:
     vrels = depends where version != null
     if vrels: ask ONCE per batch "override specific versions?" (resx MetapackageRelationshipVersionsPrompt) -> yes: rel.version = null
     for rel in depends not satisfied by installed:
        add rel.ExactMatch(compatible, considering installed+toInstall) ?? rel.ExactMatch(ignoring compat)
        // no provides-chooser for modpacks
  toInstall += m
  if m.IsDLC: error "can't install DLC" (it is added before this check; UNVERIFIED whether it is then removed)
if any metapackages:
  fileRange = union of their ksp_version min/max
  missing = KnownVersions in fileRange not covered by instance criteria, mapped to Major.Minor, distinct
  if missing && yes(MetapackageAddCompatibilityPrompt): instance compat versions += missing
incompat = toInstall ∩ registry.IncompatibleModules(crit)
if incompat empty || yes(ModpackInstallIncompatiblePrompt): build the changeset (Install for each) and show the Changeset tab
```
CLI: `install -c/--ckanfiles <path|URL>...` (Install.cs:44-68, :209-214). An absolute well-formed URI is downloaded. An existing file becomes a `file://` URI. Otherwise the CLI shows `FileNotFoundKraken`. The CLI then parses each target and calls InstallList with the normal resolver options. It has no metapackage version prompt. `upgrade -c` and `replace -c` take one ckan file.

### 2.2 Import downloaded mods (Core/IO/ModuleImporter.cs:31-178)
GUI: File > "Import downloaded mods...". The open dialog filter is `Mods (*.zip)|*.zip`, multiselect. The first folder that exists of `~/Downloads`, Desktop, GameDir is the start folder (MainImport.cs:100-110). CLI: `import <files|dirs>...`. A dir adds its top-level files only. A missing path prints `ImportNotFound` (Import.cs:63-92). The CLI asks no delete question when `--headless`.
```
index = for every available module version in all repos: download_hash.sha256 -> [mods], sha1 -> [mods]   (Registry.cs:1254-1278)
        hashes are UPPERCASE hex, no separators (NetFileCache.cs:659; meta repo confirms)
for fi in files:
  if index[SHA256(fi)] or index[SHA1(fi)]: matched[fi] = mods
  elif zip has internal *.ckan (YAML/JSON, name ends ".ckan", case-insensitive):
       for each: download ??= "https://ckan/imported-from-zip/{identifier}/{zipFileName}"
       "$vref" "#/ckan/ksp-avc[/filter]": read the single *.version in the zip (filter = exact name or regex); >1 is an error
            version ??= avc.version; set ksp_version/min/max from avc
       "#/ckan/space-warp[/filter]": the same, from swinfo.json (KSP2 only; Hangar may skip)
       matched[fi] = those modules
  else notFound += fi
if matched empty: error "Not found in index: a.zip, b.zip"; return false
if notFound: warn with the same message
installable = all matched mods that are compatible with instance criteria
deletable   = matched files whose every mod exists in repo by (id, version)  // keeps paid or internal-ckan zips
delete      = allowDelete && deletable && yes("Scanning complete. Delete {n} old files after import?")
for (file, mod) distinct by mod.download[0]:
   if cache has mod AND cached size == file size: report "Already cached: {mod} ({file})"
   else Cache.Store(mod, file, move = delete && this is the last store of this file, validate=false)
if installable && yes("Install {n} compatible imported mods in game instance {name} ({dir})?"): mark them for install
if delete: delete each deletable file that still exists
```
GUI opens the Changeset tab (MainImport.cs:55-82). CLI installs directly (Import.cs:38-51). Hangar: trash the zips, do not hard-delete.

## 3. Feature inventory (ranked for a first release)

### 3.1 Tier 1, must have
1. **Mod list grid** (ManageMods.resx:141-155): Installed (checkbox), Auto-installed, Update (checkbox), Replace, Name, Author, Installed version, Latest version, Max game version, Download, Install size, Release date, Install date, Downloads, Description. Columns can be hidden; multi-column sort.
2. **Toolbar**: Refresh, Update all (skips held mods, ModList.cs:104), Apply, Filters, Play, Back/Forward.
3. **Filter menu** (GUIModFilter.cs, mapped to search flags in ModSearch.cs:102-115): Compatible, Installed, Installed (update available), Replaceable, Cached, Uncached, Newly compatible, Not installed, Incompatible, All, Tags › (each tag, plus "Untagged"), Labels › (each label). Default is Compatible.
4. **Search box syntax** (ModSearch.cs:346-532). Split on whitespace. Every term ANDs, and a `-` prefix negates the term:
   - bare word: all bare words are joined with NO separator after the `[^\p{L}0-9]` chars are stripped. The result is a substring match (case-insensitive) against the name abbreviation (first letter of each word in the name), SearchableName, or SearchableIdentifier (:560-565).
   - `@author`: prefix match on the stripped author. `desc:`: substring match on the abstract or description. `lic:`: prefix match. `lang:`: prefix match on the localizations.
   - `dep:` `rec:` `sug:` `conf:` `sup:`: a relationship name prefix (`r.StartsWith`).
   - `tag:x`: exact tag. `tag:` alone means untagged. `label:x`: label name, with spaces removed, case-insensitive. `label:` alone means "in no label".
   - `is:`/`not:` + `compatible|installed|cached|newly-compatible|upgradeable|replaceable`.
   - Several saved searches combine with OR (ModList.cs:311). Labels with `hide` and hidden tags take rows out of view unless the search names them (:313-323).
5. **Changeset tab**: a list of {mod, change type None/Install/Remove/Update/Replace, reason}. Buttons: Back, Clear, Apply. It warns the user to close the game.
6. **Mod info tabs**: Metadata (id, replaced by, max game ver, status, author, downloads, licence, links, tags), Relationships (tree), Contents (files), Versions (all versions, pick one, per-mod stability).
7. **Install from .ckan** (§2.1) and **Export modpack and mod list** (§1).
8. **Recommendations dialog** (ChooseRecommendedMods), shown before Apply: Mod, Related mod, description. Check/Uncheck all, Always uncheck (`SuppressRecommendations`), Check recs, Check suggestions. Recs start checked, suggestions not (UNVERIFIED). Held or hidden mods start unchecked (:57).
9. **Instance manager**: Name, Game, Version, Hours Played, Path. Select, Add, Import from Steam, Clone, Rename, Set default, Forget, Open dir.
10. **Launch game** (GameInstance.cs:359-401). The macOS default command is `./KSP.app/Contents/MacOS/KSP` (KerbalSpaceProgram.cs:142-153), plus Steam URLs. The process runs with cwd = GameDir. Before launch it warns about incompatible installed mods (the warning can be turned off for each id). Play time counts only for non-Steam launches.

### 3.2 Tier 2, should have
11. **Import downloaded mods** (§2.2).
12. **Labels** (`<AppData>/labels.json`, ModuleLabel.cs:27-64): name, color, instance_name, hide, notify_on_change, remove_on_change, alert_on_install, remove_on_install, hold_version, ignore_missing_files, module_identifiers_by_game. Defaults: Favourites, Hidden (hide), Held (hold_version) (ModuleLabelList.cs:29-49).
13. **Compatible game versions dialog**: add extra versions. "1.12" is a wildcard for 1.12.x. The dialog shows again after a game update.
14. **Settings**: repos (add, reorder, delete), auth tokens, cache (path, MiB limit, purge), refresh interval, update repos on launch, hide epochs (default on), hide "v", auto-sort by Update, stability tolerance.
15. **Installation history**: lists `<CkanDir>/history/*.ckan`, newest first (GameInstance.cs:136-139). Selecting one shows its depends. Install button re-installs the selected rows.
16. **Play time** (TimeLog.cs): `<CkanDir>/playtime.json` = `{"time": "d.hh:mm:ss.fffffff"}`, shown as hours `N1`, editable.
17. **Audit recommendations**: runs the recommendation finder on all installed mods (MainRecommendations.cs:43-75).
18. **Unmanaged files** (GameInstance.cs:320-327). A tree of files under GameDir that are not in CkanDir, not in stock folders, and have no registry owner. Delete asks first. Hangar: use trash.
19. Context menu: Reinstall, Download to cache, Purge from cache, Labels.

### 3.3 Tier 3, later
Install filters, preferred hosts, game command lines editor, dedup (hard links), download statistics, tray icon, plugins (skip), client auto-update, help links.

### 3.4 CLI commands (Cmdline/Options.cs:48-133, Cmdline/Action/*)
Global options: `--instance`, `--gamedir`, `--headless`, `--net-useragent`, `-v`, `-d`.
1. `update [-l] [-f] [-u urls] [-g game]`: download repo metadata. `-l` lists new and removed mods.
2. `install ids|id=ver ... [-c files|URLs] [--no-recommends --with-suggests --with-all-suggests --allow-incompatible]`. `id=ver` is parsed by `^(?<mod>[^=]*)=(?<version>.*)$` (CkanModule.cs:457-459). Ids are matched without regard to case. If several mods provide a needed id, the user picks one, ordered by download count descending, then by "is requested id", then by id.
3. `remove ids [--re] [--all]`.
4. `upgrade ids|--all [-c file] [--dev-build|--stable-release]`.
5. `list [--porcelain] [--export fmt]`. Row bullets (List.cs:61-147): `^` upgradeable, `-` up to date and user-installed, `+` up to date and auto-installed, `X` incompatible, `>` replaceable (`id ver > newId newVer`), `A` unmanaged DLL, `?` not in repo. Virtual provides rows are skipped. Rows are sorted by id (ordinal SortedDictionary).
6. `search term [--detail] [--all] [--author a]`. Remove the `[^\p{L}0-9]` chars from the term. The term must be a substring (case-insensitive) of name, identifier, abstract, or description, AND the author must be a substring. Sort by name, then id (Search.cs:136-163).
7. `show id [--without-description|--without-module-info|--without-relationships|--without-resources|--without-files] [--with-versions]`.
8. `available [--detail]`. 9. `import paths` (§2.2). 10. `scan`: find unmanaged DLLs and DLC. 11. `replace ids|--all`.
12. Subcommand groups: `instance` (list, add, clone, rename, forget, default, fake), `compat` (list, add, forget, set, clear), `repo` (available, list, add, priority, forget, default), `cache` (list, set, clear, reset, showlimit, setlimit), `mark auto|user` (AutoInstalled flag), `filter`, `stability`, `authtoken`, `repair registry`.
13. `clean`, `dedup`, `compare a b [--machine-readable]` (-1/0/1), `prompt` (REPL), `version`. `gui` and `consoleui` are out of scope.
