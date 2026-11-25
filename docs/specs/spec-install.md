# PORT SPEC: Install / Remove Files, Cache, KSP1 Detection

Source: CKAN commit c0650d4 (shallow). Paths below are relative to `ckan-src/`.
MID = `Core/Types/ModuleInstallDescriptor.cs`, MI = `Core/IO/ModuleInstaller.cs`, KSP = `Core/Games/KerbalSpaceProgram.cs`.
All relative paths use `/`. "Normalize" = `CKANPathUtils.NormalizePath`: replace `\` with `/`; if length >= 2, trim trailing `/` (`Core/IO/CKANPathUtils.cs:26`).
Path comparisons are case-SENSITIVE on macOS/Linux (`Platform.PathComparison` = Ordinal unless Windows, `Core/Platform.cs:69-75`).

## 1. Install stanza (JSON shape)

```json
{ "file"|"find"|"find_regexp": "<str>",          // exactly one (MID:91-102)
  "install_to": "GameData",                       // default "GameData" if key absent (MID:41-43)
  "as": "<name>", "find_matches_files": false,
  "filter"|"filter_regexp"|"include_only"|"include_only_regexp": "<str>" | ["<str>",...] }
```
- string-or-array for the 4 list fields (MID:49-62).
- Error if zero or >1 of file/find/find_regexp; error if any of (filter, filter_regexp) AND any of (include_only, include_only_regexp) (MID:104-111).
- `install_to` normalized on load (MID:114).
- Unknown keys ignored (OptIn serialization). Real metadata has `comment` (1023), `optional`/`description` (28 each), and typo `find_matches_file` (52): all ignored.
- **Default when `install` missing or empty array**: `[{ "find": "<identifier>", "install_to": "GameData" }]` (`Core/Types/CkanModule.cs:143-150`, MID:137-142). 7840 of 31445 real .ckan files have no `install` (34 of these are `kind: dlc`).
- **`find_matches_files` is IGNORED by matching in this CKAN version.** It only takes part in equality/hash (MID:219,254). `find` and `find_regexp` always match files AND directories (see pattern below). Netkan only uses it for spec_version validation.

## 2. Matching algorithm (MID:68-77, 267-311, 325-423)

Install pattern, compiled case-INSENSITIVE:
- `file`: `^` + escape(normalize(file)) + `(/|$)`
- `find`: `(?:^|/)` + escape(normalize(find)) + `(/|$)`
- `find_regexp`: raw string, as given (.NET regex dialect).

```
fn find_installable_files(stanza, zip, game) -> Vec<InstallableFile>:
  inst = normalize(stanza.install_to)
  if regex(r"/\.\.(/|$)").is_match(inst): Err(BadInstallLocation)        // MID:336,425
  install_dir = if inst == "GameData" || inst.starts_with("GameData/"):
                    normalize("GameData/" + inst["GameData".len..].trim_start_one('/'))
                elif inst == "GameRoot": ""
                elif let Some(p) = ALLOWED[inst]: p                     // KSP:336-348
                else Err(BadInstallLocation "unknown install path")
  // topmost rule: ONLY for `find` (not file, not find_regexp)
  shortest = if stanza.find.is_some():
       min over zip entries e of pat.find(e.name.replace('\\','/')).start()  (None if no match)
     else None
  allow_dirs = inst equals or starts_with(X + "/") for X in CREATEABLE_INSTALL_TOS
  for e in zip entries IN ZIP ORDER:
     name = e.name.replace('\\','/')
     if !is_wanted(name, shortest): continue
     dest = transform_output_name(name, install_dir, stanza.as)
     out.push({ source: e, destination: dest,
                makedir: allow_dirs || dest equals/starts_with(D+"/") for D in CREATEABLE_DIRS })
  if out.is_empty(): Err(BadMetadata "no files found for <describe>")   // MID:416-422
```
- Directory entries (name ends `/`) ARE yielded; caller decides.
- "Topmost" is by **character index of the leftmost match start** (the `/` before the name, or 0), not by path depth. Entries whose match starts at a different index are dropped (MID:280). Port exactly with leftmost-first regex search.

```
fn is_wanted(path, match_where) -> bool:                               // MID:267-311
  m = pat.find(path)?; if match_where.is_some() && m.start() != match_where: false
  segs = path.to_lowercase().split('/')
  if filter.any(|f| segs.contains(f.to_lowercase())): return false      // whole segment, case-insens
  if filter_regexp.any(|r| Regex(r).is_match(path)): return false       // case-SENSITIVE, unanchored, full zip path
  if include_only.any(|t| segs.contains(t.to_lowercase())): return true
  if include_only_regexp.any(|r| Regex(r).is_match(path)): return true
  return include_only.is_none() && include_only_regexp.is_none()
```
Filters test the full path inside the ZIP (including the leading part above the match), not the destination.

```
fn transform_output_name(name, install_dir, as) -> String:             // MID:444-499
  prefix = shortest_matching_prefix(name)
  lead = dirname(prefix)
  if !lead.is_empty():
     if !name.starts_with(lead + "/"): Err(BadMetadata)
     name = name[lead.len()+1..]
  if as is non-blank:
     if as contains '/' or '\\': Err(BadMetadata)
     name = replace_first_piece(name, as)  // text before first '/' -> as; no '/' -> whole = as
  elif some r in RESERVED_PATHS where name.starts_with_ignore_case(r + "/"):
     name = name[r.len()+1..]              // strip "GameData/", "Ships/", "Missions/"
  if name.contains("/../") || name.ends_with("/.."): Err(BadInstallLocation)
  normalize(path_join(install_dir, name))

fn shortest_matching_prefix(full):                                      // MID:501-520
  shortest = full; p = full.trim_end_matches_one('/')
  while !p.is_empty():
     if pat.is_match(p) { shortest = p; p = dirname(p) } else break
  shortest
```
Example: entry `kOS-1.1/GameData/kOS/Plugins/kOS.dll`, `find: kOS` -> prefix `kOS-1.1/GameData/kOS` -> lead `kOS-1.1/GameData` -> `kOS/Plugins/kOS.dll` -> `GameData/kOS/Plugins/kOS.dll`.
Note: reserved-prefix strip is case-insensitive and applies even for `install_to: GameRoot` (e.g. `file: GameData` + GameRoot installs to root without `GameData/`). Port as is.

Regex dialect WARNING: 143 distinct regexes in real metadata; 13 use .NET lookarounds, including variable-length lookbehind (`(?<!KA_.+\.cfg)$`). Rust `regex` cannot run these; `fancy-regex` lacks variable-length lookbehind. Needs a .NET-compatible engine or special-casing. UNVERIFIED which crate covers all 13.

## 3. KSP1 install targets (KSP:51-114, 336-348)

| install_to | real relative path | makedir |
|---|---|---|
| `GameData`, `GameData/<sub>` | `GameData[/<sub>]` | yes |
| `GameRoot` | `` (game root) | no |
| `Ships` / `Ships/VAB` / `Ships/SPH` | same | no |
| `Ships/@thumbs`, `Ships/@thumbs/VAB`, `Ships/@thumbs/SPH` | same | no |
| `Ships/Script` | same | yes (CreateableDirs) |
| `Tutorial` | `saves/training` | yes |
| `Scenarios` | `saves/scenarios` | yes |
| `Missions` | `Missions` | yes |

- CREATEABLE_INSTALL_TOS = `GameData, Tutorial, Scenarios, Missions` (tested against the stanza's install_to string). CREATEABLE_DIRS = `Ships/Script` (tested against dest). RESERVED_PATHS = `GameData, Ships, Missions`.
- Sub-paths are only allowed under GameData; `Ships/X` other than the listed keys is rejected.
- makedir=false: directory entries are skipped, and parent dirs are NOT created; a missing parent gives `DirectoryNotFoundKraken` (MI:707-781).
- Real usage: GameData 19976, GameData/sub 7374, Ships 1743, GameRoot 125, Ships/SPH 49, Ships/VAB 40, Scenarios 29, Missions 5, Tutorial 2, Ships/Script 1.
- StockFolders: `GameData/Squad, GameData/SquadExpansion, KSP_Data, KSP_x64_Data, KSPLauncher_Data, Launcher_Data, MonoBleedingEdge, PDLauncher` (KSP:58-68).
- **There is NO install-time rule that refuses writes into GameData/Squad or stock dirs.** Stock folders are only protected on REMOVE (§6) and skipped by the DLL scan and unmanaged-file listing (`Core/GameInstance.cs:320-327`). No client-side default filters exist (no built-in Thumbs.db/.DS_Store/__MACOSX); metadata authors add them (`filter` counts: .DS_Store 490, Thumbs.db 188, __MACOSX 70).
- Netkan (not client) rejects nested `GameData` and duplicate destinations (`Netkan/Validators/InstallsFilesValidator.cs:42-65`).

## 4. Install of one module (MI:269-484)

1. `kind: dlc` -> error ModuleIsDLC (MI:80-84, 350-356). DLC is never installed or removed.
2. If installed version exists and is not "unmanaged" (autodetected DLL) -> skip with message (MI:277-285).
3. `kind: metapackage` -> no ZIP, no files; register with empty file list (MI:288,375-381). (0 metapackages in current CKAN-meta.)
4. Else ZIP path = cache lookup (§7); missing -> FileNotFound.
5. files = for each stanza in order: `find_installable_files` (concatenated); drop entries whose name ends `.ckan` (case-insens) (MI:486-487).
6. Install filters = config `GlobalInstallFiltersByGame["KSP"]` (in `config.json`, `Core/Configuration/JsonConfiguration.cs:34,227`) + instance `<GameDir>/CKAN/install_filters.json` (JSON string array, `Core/GameInstance.cs:206-222`). A file is skipped if `destination.contains(filter)` (case-SENSITIVE substring). Count skipped for a message. Presets: `MiniAVC` = `/MiniAVC.dll,/MiniAVC.xml,/LICENSE-MiniAVC.txt`; `MiniAVC2-V2` = `/MiniAVC-V2.dll,/MiniAVC-V2.dll.mdb` (KSP:179-199).
7. Autodetected-DLL replacement (MI:397-425): if `installed_dlls[identifier]` = `dll` exists and some new dest maps (via §8 dll mapping) to this identifier and no mod owns `dll`: if `dirname(dll)` not among those dests' dirnames -> error DllLocationMismatch; else delete `dll` (transactional).
8. Conflicts, interactive only (MI:427-447, 501-560): candidates = non-dir files with no registry owner whose abs path exists. For each, `same` = equal size and equal bytes. Prompt listing all; yes -> delete them; no -> cancel. Headless: no prompt.
9. Write each file (MI:699-786):
   - dir entry: if makedir, create dir (record it); else return None (not recorded).
   - file: if makedir, `create_dir_all(parent)`. If target exists -> **error FileExists** (never overwrite; owned-by-other-mod files reach this). Try hard links from dedup candidates (other instances, same rel path and size), else stream copy.
   - record the returned abs path.
10. Register (`Core/Registry/Registry.cs:844-897`): rel paths of recorded files+dirs; for each that is not an existing directory and is already in `installed_files` -> InconsistentKraken. Then `installed_files[rel] = identifier` (directories get overwritten owner), remove any `installed_dlls` entry with this identifier or with a path in the set, add `installed_modules[identifier]`.
11. ZIP read error -> purge cached ZIP, error "corrupt in cache" (MI:334-340).

Whole operation is transactional (TxFileManager under `<GameDir>/CKAN`): on failure, created files are deleted and snapshotted/deleted files restored. Rust: keep an undo journal.

Install order: modules whose deps are done, sorted by identifier, repeated passes (MI:233-254). Free-space check before each mod (MI:152).

Registry JSON (`<GameDir>/CKAN/registry.json`): `installed_files: {relPath: identifier}`, `installed_dlls: {identifier: relPath}`, `installed_modules: {identifier: {install_time, source_module, auto_installed, installed_files: {relPath: {}}}}` (`Core/Registry/InstalledModule.cs:35-49`). Lock file `CKAN/registry.locked`.

## 5. Recorded paths

- Files: always recorded. Directories: recorded only if the ZIP has an explicit dir entry and makedir is true. Implicit parent dirs are NOT recorded.
- Paths stored relative to GameDir, normalized.

## 6. Removal (MI:799-1176)

UninstallList: every name must be installed, no DLC; set = requested + reverse dependencies + removable auto-installed; sorted by identifier, removed in that order (MI:823-835).

```
fn uninstall(ident):
  files = installed_modules[ident].files
  dirs = set()
  for rel in files:
    abs = game_dir/rel
    if abs is dir: dirs.add(abs)
    else: dirs.add(dirname(abs)); delete file (tx)
    NotFound -> ignore; IOError/PermissionDenied -> undeletable.push(rel); other -> ignore
  if undeletable: Err(FailedToDeleteFiles)
  deregister(ident)   // errors if any listed FILE still exists; removes installed_files keys
  dirs = add_parent_directories(dirs)
  for d in dirs sorted by length DESC:
    rel = relative(d)
    if is_reserved(d) || STOCK.contains(rel): continue
    entries = all files+dirs under d recursively (relative)
    removable = entries where owner==None && rel not in files
                && any path piece AFTER d is in AUTO_REMOVABLE_DIRS ("@thumbs")
    delete removable files; try rmdir removable dirs (ignore errors)
    if not_removable.is_empty(): rmdir(d)          // non-transactional
    elif not_removable (minus possibleConfigOnlyDirs) has any owned / own file: keep d
    else: keep d; add d to possibleConfigOnlyDirs (UI asks user later)
```
- add_parent_directories (MI:1134-1175): for each dir, walk up to (not including) game root; stop (exclusive) at a StockFolder; then drop reserved dirs.
- is_reserved (KSP:106-114), exact abs string match: GameDir, `GameDir/CKAN`, `GameData`, `Missions`, `saves/scenarios`, `saves/training`, `Ships`, `Ships/@thumbs`, `Ships/VAB`, `Ships/@thumbs/VAB`, `Ships/SPH`, `Ships/@thumbs/SPH`, `Ships/Script`.
- Net effect: a directory survives if it has any file of another mod or any unmanaged file (other than under `@thumbs`).

## 7. Download cache

- Default dir: `<AppData>/CKAN/downloads` (`Core/GameInstanceManager.cs:779-780`); override `DownloadCacheDir` in `<AppData>/CKAN/config.json` (env `CKAN_CONFIG_FILE` overrides config path, `JsonConfiguration.cs:85-87`). macOS AppData: `~/Library/Application Support` (.NET 8+); old Mono builds used `~/.local/share/CKAN`, and `macosx/CKAN` launcher moves it and leaves a symlink. Legacy per-instance `<GameDir>/CKAN/downloads` is also searched (`NetFileCache.cs:277-282`).
- In-progress downloads: `<cache>/downloading/<hash>-<StandardName>` (resume by prefix match) (`NetFileCache.cs:111-131`).
- URL hash (`NetFileCache.cs:582-589`): `SHA1(UTF8(uri.ToString()))`, uppercase hex, first 8 chars. **`System.Uri.ToString()` returns the UNESCAPED form**: `%20` becomes a space before hashing. 11296 real download URLs contain `%` (e.g. SpaceDock `Its%20the%20little%20things`). Hash of the unescaped string to share caches with CKAN. Exact unescape set (which reserved `%XX` stay escaped, e.g. `%25`, `%23`, `%3F`) is UNVERIFIED; also .NET host lowercasing / dot-segment normalization. Test against a real CKAN cache.
- File name: `<HASH8>-<StandardName>`; StandardName = `<identifier>-<version with [^A-Za-z0-9_.-] -> "-">.zip` (`CkanModule.cs:725-729,769`). Stored under hash of `download[0]` (first URL) (`NetModuleCache.cs:188-195`).
- Lookup: list cache files matching `^[0-9A-F]{8}-` (not `.sha1`/`.sha256`), key = first 8 chars; for each URL in `download` (string or array), first hit wins. If module `release_date` is set and is >= file mtime (UTC), the cached file is deleted as stale (`NetFileCache.cs:213-243`, `NetModuleCache.cs:64-69`).
- Validation on store (`NetModuleCache.cs:148-200`, `NetAsyncModulesDownloader.cs:200-216`): (1) if `download_hash.sha256` set, compare to SHA-256 of the downloaded bytes as UPPERCASE hex (exact string compare; metadata is uppercase); mismatch -> delete, error. (2) size == `download_size` if > 0. (3) full ZIP CRC test. `sha1` is NOT checked on download (only used by import/display). sha256 present in 31308, sha1 in 30999 modules.
- Hash sidecars: `<file>.sha1`, `<file>.sha256` (uppercase hex, written only for files directly in cache dir) (`NetFileCache.cs:603-630`).
- Size limit (`CacheSizeLimit`): delete un-indexed first, then incompatible, then oldest creation time, until under limit (`NetFileCache.cs:284-368`).

## 8. Autodetected DLLs and DLC

- Scan (`Core/Registry/RegistryManager.cs:567-600`): recursively list all files under `<GameDir>/GameData`, keep names ending `.dll` (case-insens), skip `GameData/Squad/` and `GameData/SquadExpansion/` prefixes, map with dll_to_identifier, group by identifier, keep the first path (filesystem enumeration order; UNVERIFIED stable). Then `SetDlls`: drop identifiers that are installed modules and paths already in `installed_files` (`Registry.cs:975-990`). Run at registry load and before install/upgrade/replace.
- dll_to_identifier (`Core/GameInstance.cs:287-313`): rel path must start with `GameData/` (case-sensitive on mac); regex (case-insens) `^(?:.*/)?(?<identifier>[^.]+).*\.dll$`; then Sanitize: remove leading `^[^A-Za-z0-9]+`, replace `[^A-Za-z0-9-]` with `-` (`Core/Types/Identifier.cs:27-37`). E.g. `GameData/kOS/Plugins/kOS.Safe.dll` -> `kOS`; `GameData/X/Kopernicus_Parser.dll` -> `Kopernicus-Parser`.
- Autodetected mods report `UnmanagedModuleVersion` (satisfy deps, can be "installed over").
- DLC detection: `GameData/SquadExpansion/MakingHistory/readme.txt` -> `MakingHistory-DLC` (requires KSP >= 1.4.1; version "1.0" mapped to "1.0.0"); `GameData/SquadExpansion/Serenity/readme.txt` -> `BreakingGround-DLC` (>= 1.7.1). Dir AND readme must exist. Version = first line matching `^Version\s+(\S+)` (case-insens), else unknown (`Core/Games/KerbalSpaceProgram/DLC/*.cs`). Test hook: `<GameDir>/CKAN/dlc/<Name>.dlc` -> `<Name>-DLC`, version = trimmed file text. DLC rows go in `installed_modules` with no files; module from repo (identifier+version) or a synthetic `kind: dlc` stub (`Registry.cs:992-1025`).

## 9. KSP1 game detection (macOS)

- Valid game dir (KSP:26-28, 288-294): on macOS, `buildID64.txt` OR `buildID.txt` exists AND dir `GameData` exists; plus version detectable (`GameInstance.cs:59`).
- Version (KSP:253-262): (1) buildID provider: for each of `buildID64.txt`, `buildID.txt`, first line matching `^build id\s+=\s+0*(?<buildid>\d+)` (case-insens), look up `builds[buildid]`; take max of distinct results (`KspBuildIdVersionProvider.cs`). (2) Fallback `readme.txt`: first line `^Version\s+(\d+\.\d+\.\d+)` (case-insens).
- Build map JSON: `{"builds": {"<buildid>": "<maj.min.patch.build>"}}`, e.g. `"3190": "1.12.5.3190"`. Source order: cached `<AppData>/CKAN/builds-ksp.json`, else embedded `Core/builds-ksp.json`. Refresh from `https://raw.githubusercontent.com/KSP-CKAN/CKAN-meta/master/builds.json` (also inside the CKAN-meta archive as `builds.json`) and write the cache (`KspBuildMap.cs:35-158`).
- Default compatible versions (KSP:264-284): 1.2.9 -> none; else latest Unity break in {1.8,1.4,1.2,1.1} <= installed -> all minors from break to installed; else `[maj.min]`. Stored in `<GameDir>/CKAN/compatible_ksp_versions.json`.
- Instance search (`Core/GameInstanceManager.cs:75-195`): (1) portable: exe dir / cwd that is a valid game dir; (2) the single registered instance; (3) `AutoStartInstance`; (4) if none registered: Steam libraries + `MacPath()` = `/Applications/Kerbal Space Program` (KSP:36-49).
- Steam on macOS (`Core/IO/SteamLibrary.cs:148-170`): root `~/Library/Application Support/Steam`; read `config/libraryfolders.vdf` (KeyValues text, `path` keys); each library `SteamApps|steamapps/*.acf` gives `installdir`; game dir = `<lib>/steamapps/common/<installdir>`; non-Steam shortcuts from `userdata/*/config/shortcuts.vdf` (binary KV, `StartDir`). Each candidate is kept if it passes GameInFolder and Valid. On a Steam Mac install, the KSP dir with `buildID64.txt` is usually `.../common/Kerbal Space Program` (UNVERIFIED on real hardware).
- Per-instance dirs: `<GameDir>/CKAN/` (`registry.json`, `history/`, `install_filters.json`, `dlc/`).
- Launch on mac: `./KSP.app/Contents/MacOS/KSP` (KSP:142-153).
