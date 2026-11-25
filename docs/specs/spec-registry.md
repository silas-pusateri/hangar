# PORT SPEC: Registry + Config Files (on-disk CKAN compatibility)

Source root: `ckan-src/`. All refs are `file:line`. Paths: `<G>` = game root, `<C>` = `<G>/CKAN`, `<A>` = app-data dir (see section 9).

## 0. File map (KSP1)

| File | Owner | Ref |
|---|---|---|
| `<C>/registry.json` | Registry | RegistryManager.cs:61, :352 |
| `<C>/registry.locked` | lock (PID) | RegistryManager.cs:45-46 |
| `<C>/installed-<SanitizedName>.ckan` | auto-export on every Save | RegistryManager.cs:431-442 |
| `<C>/history/installed-<SanitizedName>-yyyy-MM-dd_HH-mm-ss.ckan` | history snapshot | RegistryManager.cs:435-444, GameInstance.cs:129 |
| `<C>/compatible_ksp_versions.json` (**KSP1**; KSP2 = `compatible_game_versions.json`) | GameInstance | KerbalSpaceProgram.cs:286, KerbalSpaceProgram2.cs:226 |
| `<C>/playtime.json` | TimeLog | TimeLog.cs:62 |
| `<C>/install_filters.json` | GameInstance | GameInstance.cs:222 |
| `<C>/stability_tolerance.json` | StabilityToleranceConfig | GameInstance.cs:230 |
| `<C>/suppressed_compat_warning_identifiers.json` | GameInstance | GameInstance.cs:204 |
| `<C>/GUIConfig.json` | GUI only | GUI/Model/GUIConfiguration.cs:171 |
| `<C>/downloads/` | per-instance cache path (property only) | GameInstance.cs:126 |
| `<C>/dlc/*.dlc` | test DLC stubs | RegistryManager.cs:618-629 |
| `<A>/config.json` | global config | JsonConfiguration.cs:85-87 |
| `<A>/labels.json` | module labels | Types/Labels/ModuleLabelList.cs (DefaultPath) |
| `<A>/repos/etags.json`, `<A>/repos/<HASH8>-<name>.json` | repo data cache | RepositoryDataManager.cs:338, :344-354 |
| `<A>/downloads/` | default download cache | GameInstanceManager.cs:780 |

Instance clone skips `CKAN/registry.locked`, `CKAN/playtime.json`, `CKAN/GUIConfig.json` (GameInstanceManager.cs:297).

## 1. registry.json

### 1.1 Top-level shape (Registry.cs:37-53, :1335-1343)
Newtonsoft writes `[JsonProperty]` fields in declaration order:

```
registry_version     int     (write 3; LATEST_REGISTRY_VERSION, Registry.cs:34)
sorted_repositories  { name -> Repository }   (SortedDictionary, ordinal key sort)
installed_dlls       { identifier -> relpath }
installed_modules    { identifier -> InstalledModule }
installed_files      { relpath -> identifier }
available_modules    {}   ALWAYS empty object (legacy; JsonAlwaysEmptyObjectConverter)
download_counts      {}   ALWAYS empty object (legacy)
```
- There is **no `installed_dlc` key.** DLC is kept as entries in `installed_modules` whose `source_module.kind == "dlc"` (Registry.cs:137-146, :992-1025).
- `available_modules`/`download_counts`: on read, value is discarded; on write, emit `{}` (never null: older clients crash on null; Registry.cs:1333-1343).
- Write formatting (RegistryManager.cs:378-396): `Formatting.Indented`, `Indentation = 0` (one token per line, no indent), `DateTimeZoneHandling.Utc`, trailing `Environment.NewLine` ("\n" on macOS). UTF-8 (no BOM; `Encoding.UTF8.GetBytes`). Formatting is cosmetic; any valid JSON loads.

### 1.2 Repository object (Repositories/Repository.cs)
```
{ "name": str, "uri": str, "priority": int,
  "x_mirror": bool   (omitted when false; IgnoreAndPopulate),
  "x_comment": str   (omitted when null) }
```
Equality of repositories = `uri` only. Default repo: name `"KSP-default"` (`$"{ShortName}-{default}"`, Repository.cs `DefaultGameRepo`), uri `https://github.com/KSP-CKAN/CKAN-meta/archive/master.tar.gz` (KerbalSpaceProgram.cs:296). The per-instance repo list IS `sorted_repositories`; there is no separate repo config file.

### 1.3 InstalledModule (Registry/InstalledModule.cs:35-49)
```
{ "install_time":   ISO-8601 UTC, e.g. "2025-03-01T12:34:56.1234567Z",
  "source_module":  full CkanModule object (same serializer as a .ckan file),
  "auto_installed": true   (key omitted when false),
  "installed_files": { relpath -> {} } }
```
- `installed_files` values are empty objects (`InstalledModuleFile` has no members, line 14-21). Old registries have `{"sha1_sum": ...}`: ignore extra keys.
- Paths: relative to `<G>`, `/` separators, no trailing `/` (CKANPathUtils.NormalizePath). Directories are listed as well as files (fixture: `"GameData/DogeCoinFlag"`).
- Constructor rejects rooted paths (line 84-87).
- `auto_installed` cannot be set on DLC (line 56-66).
- Date format: Newtonsoft ISO round-trip `yyyy-MM-ddTHH:mm:ss.FFFFFFFZ` (trailing zeros in fraction trimmed). Read must also accept offsets (fixture has `+11:00`), normalised to UTC (Registry.cs:409-414).

### 1.4 installed_files (top level) and installed_dlls
- `installed_files`: reverse index file->owner, rebuilt by `ReindexInstalled()` (Registry.cs:297-307) = union over modules of `module.installed_files` keys. Directories are "owned" by the last registrant (Registry.cs:893-899).
- `installed_dlls`: `{ident: relpath}` for unmanaged DLLs. Ident = `Identifier.Sanitize(captured)` where regex `^(?:.*/)?(?<identifier>[^.]+).*\.dll$` (case-insensitive), and only for paths starting `GameData/` (GameInstance.cs:287-313). Sanitize: strip `^[^A-Za-z0-9]+`, then replace `[^A-Za-z0-9-]` with `-` (Types/Identifier.cs:27-37).

### 1.5 Path comparison
macOS/Linux: ordinal, case-sensitive. Windows: OrdinalIgnoreCase (Platform.cs:69-75). Hangar on macOS: case-sensitive keys.

### 1.6 Load algorithm (RegistryManager.cs:304-340; Registry.cs:163-290, 382-404)
```
acquire lock (sec 2) else error "registry in use"
try text = read_utf8(<C>/registry.json)
  on FileNotFound/DirNotFound: create empty registry(initial repos), AscertainDefaultRepo, ScanUnmanagedFiles
  on JSON parse error: rename registry.json -> "registry.json_CORRUPTED_" + local yyyyMMddHHmmss; create new (as above)
parse with registry_version default 0, installed_files default null
post-fixes (DeSerialisationFixes):
  if registry_version > 3: fail "RegistryVersionNotSupported" (do NOT overwrite)
  if installed_files missing: ReindexInstalled()
  if registry_version < 1: make every installed_files key and each module's file keys relative to <G> + normalized
  if registry_version < 2 and installed_modules has "001ControlLock": re-key as "ControlLock" (also source_module.identifier), ReindexInstalled
  if sorted_repositories["default"].uri == ".../CKAN-meta/archive/master.zip": set uri to game default .tar.gz
     (note: checks literal name "default", Registry.cs:264-274)
  renumber priorities: sort repos by (priority, name) and assign 0..n-1 (Registry.cs:276-287)
  registry_version = 3
AscertainDefaultRepo: if repo count == 0, set { "KSP-default": default repo } (RegistryManager.cs:365-376)
CheckSanity (depends/conflicts consistency): log only, never blocks load (RegistryManager.cs:85-101)
```
Registry objects are cached per `<C>` path (one manager per instance, RegistryManager.cs:265-280).

### 1.7 Save algorithm (RegistryManager.cs:398-439)
```
if enforce_consistency (default true): CheckSanity -> error aborts save
mkdir -p <C>
write <C>/registry.json  (transactional write; Hangar: write temp + fsync + rename)
mkdir -p <C>/history
modpack = GenerateModpack(recommends=false, with_versions=true).ToJson()
write <C>/installed-<SanitizedName>.ckan          = modpack
write <C>/history/installed-<SanitizedName>-<local yyyy-MM-dd_HH-mm-ss>.ckan = modpack
```
`SanitizedName` = instance name with `Path.GetInvalidFileNameChars()` removed (GameInstance.cs:105; on Unix only `\0` and `/`). History list = `*.ckan` in history dir, newest CreationTime first (GameInstance.cs:136-138).

### 1.8 Mutation rules
- RegisterModule (Registry.cs:849-913): rel = files made relative (set). For each rel that is NOT an existing directory: if already in installed_files -> collect conflict, abort all (InconsistentKraken). Else set `installed_files[f]=ident` for all; remove from installed_dlls any entry whose key == ident or whose value in rel; add `installed_modules[ident]` (install_time = now UTC). Duplicate ident = error (Dictionary.Add).
- DeregisterModule (922-959): fail if any listed file still exists as a FILE on disk (dirs allowed); else remove its keys from installed_files and drop the module.
- ReregisterModule: replace source_module, keep files + auto_installed, new install_time.
- SetDlls (975-990): keep only entries whose ident is not an installed module and whose path is not in installed_files; replace if different.
- SetDlcs (992-1025): remove DLC entries no longer detected; for each detected `(ident, ver)` write `installed_modules[ident]` = source_module from repo for that version, else synthetic module: `spec_version "v1.28"`, identifier=name=ident, abstract "An official expansion pack", author ["SQUAD"], license "restricted", version=ver, kind "dlc"; files empty; auto_installed false.

### 1.9 ScanUnmanagedFiles (RegistryManager.cs:567-609)
Walk `<G>/GameData/**` (+ alternate mod dirs; none for KSP1), take files ending `.dll` (case-insensitive), make relative, drop those under stock folders (e.g. `GameData/Squad/`, `GameData/SquadExpansion/`; UNVERIFIED exact list, see KerbalSpaceProgram.cs `StockFolders`), group by DLL ident, first path wins -> SetDlls. Then ScanDlc:
- Test stubs: `<C>/dlc/<Name>.dlc` -> ident `<Name>-DLC`, version = trimmed file text.
- Real: `GameData/SquadExpansion/MakingHistory/readme.txt` -> `MakingHistory-DLC`; `GameData/SquadExpansion/Serenity/readme.txt` -> `BreakingGround-DLC`. Version = first line matching `^Version\s+(\S+)` (case-insens); MakingHistory maps "1.0" -> "1.0.0"; no match -> unknown version (JSON `null`). Directory without readme = not installed (Games/KerbalSpaceProgram/DLC/*.cs).
DLC version JSON is the original string (JsonUnmanagedModuleVersionConverter writes `OriginalString`).

### 1.10 Transactions (Registry.cs:418-520)
Registry snapshots itself as JSON on first mutation inside a transaction, restores it on rollback. Hangar: clone registry struct before a change set, restore on failure; write files only after success.

### 1.11 Sample registry.json
```json
{
"registry_version": 3,
"sorted_repositories": {
"KSP-default": {"name": "KSP-default","uri": "https://github.com/KSP-CKAN/CKAN-meta/archive/master.tar.gz","priority": 0}
},
"installed_dlls": { "KSPCommunityFixes": "GameData/KSPCommunityFixes/KSPCommunityFixes.dll" },
"installed_modules": {
"ModuleManager": {
"install_time": "2025-03-01T12:34:56.1234567Z",
"source_module": { "spec_version": 1, "identifier": "ModuleManager", "name": "Module Manager",
  "abstract": "...", "author": "sarbian", "version": "4.2.3", "ksp_version_min": "1.8", "license": "CC-BY-SA",
  "download": "https://...", "download_size": 1234, "download_hash": {"sha1":"...","sha256":"..."},
  "download_content_type": "application/zip" },
"auto_installed": true,
"installed_files": { "GameData/ModuleManager.4.2.3.dll": {} }
},
"MakingHistory-DLC": {
"install_time": "2025-03-01T12:00:00Z",
"source_module": { "spec_version": "v1.28", "identifier": "MakingHistory-DLC", "name": "MakingHistory-DLC",
  "abstract": "An official expansion pack", "author": "SQUAD", "version": "1.9.1", "license": "restricted", "kind": "dlc" },
"installed_files": {}
}
},
"installed_files": { "GameData/ModuleManager.4.2.3.dll": "ModuleManager" },
"available_modules": {},
"download_counts": {}
}
```
`source_module` field order/omission rules = CkanModule `[JsonProperty(Order=N)]` (Types/CkanModule.cs:38-192): single-element author/license/download lists are written as scalars (JsonSingleOrArrayConverter.cs:30-38).

## 2. registry.locked (RegistryManager.cs:157-259)
```
CheckStaleLock: if file exists:
   read text; if unreadable -> keep (locked)
   if int.parse(text) ok and no process with that pid -> delete file
   (non-integer content -> keep)
GetLock: open with CREATE_NEW (O_CREAT|O_EXCL), write decimal PID (no newline), flush; keep handle open
         IOException (exists) -> RegistryInUse
Release: close handle; file opened DeleteOnClose -> deleted on close
```
Hangar: `OpenOptions::create_new`, write `std::process::id()`, hold the File; delete explicitly on drop (no DeleteOnClose on macOS). Use `kill(pid,0)` for liveness. A crash leaves a stale file with a dead PID, which the next run removes. `IsMaybeLocked` = file exists (RegistryManager.cs:48-49).

## 3. installed-*.ckan (modpack) (RegistryManager.cs:477-531)
```
name        = "installed-" + instance.Name
identifier  = Identifier.Sanitize(name)
spec_version= MinimumSpecVersion(module)        (initial "v1.6")
abstract    = "A list of modules installed on the <Name> KSP instance"
author      = [OS user name]; license = ["unknown"]
version     = UTC now as "yyyy.MM.dd.hh.mm.ss"   (NOTE: hh = 12-hour clock, a CKAN quirk)
kind        = "metapackage"
ksp_version_min/max = instance compat range lower/upper, inclusive, without build
download_content_type = "application/zip"; release_date = local now
depends     = installed modules where !DLC && !auto_installed && available in repos,
              sorted deps-before-dependers by resolver, each {"name": id, "version": installed version}
```
Written with 4-space indent (CkanModule.cs:470-482). UNVERIFIED: exact `MinimumSpecVersion` result; treat as the lowest spec covering the used fields (likely "v1.6" or higher if `release_date`/`kind` require it).

## 4. compatible_ksp_versions.json (GameInstance.cs:152-185; Configuration/CompatibleGameVersions.cs)
Shape (default Newtonsoft, no indent):
```json
{"GameVersionWhenWritten":"1.12.5.3190","Versions":["1.12","1.11","1.10","1.9","1.8"]}
```
- Legacy names on read: `VersionOfKspWhenWritten` -> `GameVersionWhenWritten`, `CompatibleKspVersions` -> `Versions`.
- GameVersion JSON = its string; on read, strip one trailing `.` before parse (GameVersion.cs:958-990).
- Write: `Versions` = distinct, sorted descending; `GameVersionWhenWritten` = detected game version.
- Missing file: `Versions = DefaultCompatibleVersions(gameVer)` (KerbalSpaceProgram.cs:264-284): if 1.2.9 -> []; else latest Unity break B in [1.8,1.4,1.2,1.1] with B <= ver -> [1.B.minor .. 1.ver.minor] (major.minor only); else [major.minor]. `GameVersionWhenWritten = null`.
- `CompatibleVersionsAreFromDifferentGameVersion` = stored version != detected (UI prompt after game update).

## 5. playtime.json (Types/TimeLog.cs)
`{"time": "1.02:03:04.5000000"}` (.NET TimeSpan "c" format `[d.]hh:mm:ss[.fffffff]`), Indented. Missing file -> zero. Saved when a game launched by CKAN exits (Stop adds elapsed).

## 6. Other per-instance files
- `install_filters.json`: JSON array of strings, e.g. `["/Ships/","Thumbs.db"]` (GameInstance.cs:208-222). Missing -> [].
- `stability_tolerance.json` (Configuration/StabilityToleranceConfig.cs): `{"overall_stability_tolerance":"testing","mod_stability_tolerance":{"Kopernicus":"development"}}`. `overall_*` omitted when `stable`. Values: `stable|testing|development` (JsonReleaseStatusConverter; string form UNVERIFIED but matches .ckan `release_status`). Indented.
- `suppressed_compat_warning_identifiers.json` (SuppressedCompatWarningIdentifiers.cs): `{"GameVersionWhenWritten":"1.12.5.3190","Identifiers":["Foo"]}`; discarded when stored version != current game version.

## 7. Labels: `<A>/labels.json` (Types/Labels/ModuleLabel.cs, ModuleLabelList.cs)
Global (not per instance). Missing file -> defaults (not written until Save). Indented.
```json
{
  "labels": [
    { "name": "Favourites", "color": "PaleGreen" },
    { "name": "Hidden", "color": "PaleVioletRed", "hide": true },
    { "name": "Held", "color": "255, 255, 176", "hold_version": true,
      "module_identifiers_by_game": { "KSP": ["Kopernicus"] } }
  ]
}
```
Fields: `name` (str), `color` (System.Drawing.Color, omitted if null), `instance_name` (str|omit = all instances), bool flags omitted when false: `hide`, `notify_on_change`, `remove_on_change`, `alert_on_install`, `remove_on_install`, `hold_version`, `ignore_missing_files`; `module_identifiers_by_game` `{gameShortName: [ident]}`.
- Read legacy `module_identifiers` (rename converter). If its value is an array (not object), copy it to every known game (`KSP`, `KSP2`); null/empty array -> none (JsonToGamesDictionaryConverter.cs).
- After load, sort labels: instance-specific first (instance_name != null), then by instance_name (ModuleLabelList.cs OnDeserialized).
- `LabelsFor(inst)` = instance_name null or == inst.Name. Held identifiers = labels with hold_version; ignore-missing = ignore_missing_files.
- Color string: UNVERIFIED (.NET ColorConverter behavior, not in repo): known colors by name, else `"R, G, B"` (alpha 255) or `"A, R, G, B"`. Accept `#RRGGBB` too, defensively.

## 8. Global config `<A>/config.json` (Configuration/JsonConfiguration.cs)
Path override: env `CKAN_CONFIG_FILE` (line 85-87). Null members omitted (`ItemNullValueHandling.Ignore`, line 22-23; semantics UNVERIFIED for property-level nulls). Indented, written on every setter.
```json
{
  "AutoStartInstance": "Main",
  "DownloadCacheDir": "/Volumes/Big/ckan-cache",
  "CacheSizeLimit": 10737418240,
  "RefreshRate": 0,
  "Language": "en-US",
  "GameInstances": [ { "Name": "Main", "Path": "/Users/me/Library/Application Support/Steam/steamapps/common/Kerbal Space Program", "Game": "KSP" } ],
  "AuthTokens": { "api.github.com": "ghp_xxx" },
  "GlobalInstallFiltersByGame": { "KSP": ["/Ships/"] },
  "PreferredHosts": ["github.com", null, "spacedock.info"],
  "DevBuilds": false
}
```
Rules: legacy keys `KspInstances` -> `GameInstances`, `GlobalInstallFilters` -> `GlobalInstallFiltersByGame` (array form applied to all games). Entry missing `Game` -> `"KSP"`. `CacheSizeLimit` bytes, <0 -> null. `RefreshRate` minutes, <=0 -> null. `DownloadCacheDir` empty -> null (default `<A>/downloads`), relative -> absolute. AuthTokens keyed by host; removing last token sets dict null (omitted). `PreferredHosts` null element = "all other hosts" placeholder (UNVERIFIED semantics). Missing file -> create dir + write `{}`-equivalent defaults (line 326-336). Instances order = SortedList by name (line 176-183).

## 9. `<A>` on macOS
`<A>` = `SpecialFolder.LocalApplicationData` + `/CKAN` (IO/CKANPathUtils.cs:15-17). .NET (current macOS builds): `~/Library/Application Support/CKAN`. Old Mono builds: `~/.local/share/CKAN`; the macOS launcher moves Mono dir to the .NET dir and symlinks back (macosx/CKAN:3-10). Hangar: use `~/Library/Application Support/CKAN`; if missing and `~/.local/share/CKAN` exists, read it.

## 10. Repo data cache `<A>/repos/` (Repositories/RepositoryDataManager.cs, RepositoryData.cs)
- Shared across instances. Raw tarballs are NOT kept; the parsed result is saved.
- File name: `<HASH8>-<repo.name>.json`, `HASH8` = first 8 chars of uppercase hex SHA1 of `uri.ToString()` UTF-8 (Net/NetFileCache.cs:590-598). Default KSP repo -> `4A85405A-KSP-default.json`. Lookup = first file in dir whose name starts with HASH8 (rename-safe; line 347-351).
- Content (`SaveTo`, line 117-134; Indentation 0):
```json
{
"available_modules": { "AGExt": { "module_version": { "1.20": { ...CkanModule... }, "1.21a": {...} } } },
"download_counts": { "AGExt": 12345 },
"known_game_versions": ["0.23.0.464", "1.12.5.3190"],
"repositories": [ { "name": "KSP-default", "uri": "...", "priority": 0, "x_mirror": true, "x_comment": "..." } ]
}
```
  null members omitted. `module_version` keys = version strings, read into a version-sorted map; a bad entry is skipped, not fatal (AvailableModule.cs:66-79).
- Archive parse (line 182-335): detect tar.gz vs zip by magic; skip `*.frozen`; `*.ckan` -> CkanModule (Unsupported/BadMetadata errors -> null module and set `UnsupportedSpec`); `*download_counts.json` -> `{id:int}`; `*builds.json` -> `game.ParseBuildsJson` (`{"builds":{id:"ver"}}` values); `*repositories.json` -> its `repositories` array. Suffix match (`EndsWith`), any directory depth.
- `etags.json`: `{ "<uri>": "<etag>" }`, Indented; ETag stored with all `"` removed (Net.cs:42, DownloadPart.cs:119). Corrupt/empty file -> {}.
- Update (line 144-247):
```
refresh builds map
toUpdate = repos distinct by uri where uri.is_file || force || stale(r)
stale(r) = no etag stored || cache file missing || HEAD(uri).ETag(stripped) != stored
if none: touch mtime of each repo cache file to now; return NoChanges
GET all; per success set/remove etags[url]; parse; save cache file
on all OK: write etags.json; emit Updated
on any error: reload etags.json from disk (discard in-memory changes); propagate
return OutdatedClient if any repo UnsupportedSpec else Updated
```
- Age: `LastUpdate` = min(now - mtime) over stale repos; `TimeTillStale` 3 days, `TimeTillVeryStale` 14 days (line 111-120).
- Merge order across repos: priority asc, then name (line 324-329). Download count = first repo that has one.

## 11. Download cache (summary, see cache spec)
Dir = config `DownloadCacheDir` or `<A>/downloads`. File `<HASH8(url)>-<filename>`; sidecars `<file>.sha1`, `<file>.sha256` (NetFileCache.cs:413-441, :318-319).
