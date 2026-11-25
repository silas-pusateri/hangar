# PORT SPEC: Relationship Resolver + Changesets

Refs (under `ckan-src/Core/`): RR=`Relationships/RelationshipResolver.cs`, RRT=`Relationships/ResolvedRelationshipsTree.cs`, RRel=`Relationships/ResolvedRelationship.cs`, RD=`Types/RelationshipDescriptor.cs`, CM=`Types/CkanModule.cs`, IRQ=`Registry/IRegistryQuerier.cs`, REG=`Registry/Registry.cs`, MI=`IO/ModuleInstaller.cs`.

## 1. Primitives

**Module identity** (CM:567-578, 694): equal when `identifier` and `version` are equal.

**Descriptor JSON** (RD:64-91, 232):
- `{"name": S, "version"?: V, "min_version"?: V, "max_version"?: V, "choice_help_text"?: S, "suppress_recommendations"?: bool}`
- `{"any_of": [descriptor...], "choice_help_text"?: S}`. An any_of may not carry name/version/min_version/max_version.

Repo use: any_of 399 files, suppress_recommendations 185, replaced_by 16, choice_help_text 3; no metapackage.

**WithinBounds(rel, mod)** (RD:93-115):
```
Module(rel):
  if mod.identifier == rel.name: return bounds_ok(mod.version)
  else: return mod.provides.contains(rel.name)   // provides never check version
bounds_ok(v): v is None or v is Unmanaged{unknown} -> true
  elif rel.version set -> rel.version == v     // exact version wins over min/max
  else (min is None or min <= v) and (max is None or max >= v)
AnyOf(rel): any(child.within_bounds(mod))
```
**MatchesAny(rel, mods, dlls?, dlc?) -> (bool, Option<matched>)** (RD:129-150, 247-258):
- Module: `dlls ∋ name` → `(true, None)` (any version). Else first mod in `mods` within bounds → `Some`. Else `dlc[name]` passes bounds_ok → `(true, None)`.
- AnyOf: first child match that gives `Some`. Quirk: a child matched only by DLL/DLC gives None, so any_of returns **false**.

**ContainsAny(rel, idents)** (RD:181, 289): `name ∈ idents`, or any any_of child.

**ConflictsWith(a, b)** (CM:499-511): different identifiers and (a.conflicts matches [b] or b.conflicts matches [a]).

**BadRelationships(m, others)** (CM:880-906), `others'` = others minus m's identifier: `m.depends` with `BadDepends(d, others)`; `m.conflicts` matching `others'`; for o in `others'`: `o.depends` with `BadDepends(d,[m])`, `o.conflicts` matching `[m]`. `BadDepends(d, t) = d.ContainsAny(t.idents) && !d.MatchesAny(t)`: it flags only a wrong version of a present identifier, never an absent one. `DependsAndConflictsOK(others) = BadRelationships(others).is_empty()`.

**IsMetapackage / IsDLC**: `kind == "metapackage" / "dlc"` (CM:563-565).

## 2. Availability lookups

**AvailableModule.Latest(stab, crit?, rel?, installed?, toInstall?)** (`Repositories/AvailableModule.cs:106-140`): iterate versions **descending**; return the first that passes, in order:
1. `release_status <= (stab.ModStabilityTolerance(id) ?? stab.Overall)`. (stable=0 < testing=1 < development=2)
2. `rel.WithinBounds(m)`.
3. `m.IsCompatible(crit)`.
4. `m.DependsAndConflictsOK(installed)`.
5. `m.DependsAndConflictsOK(toInstall)`.

**Registry.LatestAvailable(id, ...)** (REG:682-691): max by version of `Latest` over each repo's entry; unknown `id` throws `ModuleNotFoundKraken` (REG:666-677).

**Registry.LatestAvailableWithProvides(id, stab, crit, rel, installed, toInstall)** (REG:819-841). Index `providers[name] = [AvailableModule]` from every version's `ProvidesList = [identifier] + provides` (REG:744-752).
```
for am in providers[id]: m = am.Latest(stab, crit, rel, installed, toInstall)
keep m where m.ProvidesList.contains(id)
group by m.identifier, keep max version
sort by download_count desc, then name asc
```
No provider → empty list (no exception). AnyOf = distinct union of children (RD:260-268).

**CompatibleModules(stab, crit)** (REG:639-642, CompatibilitySorter.cs:139-253): latest compatible version per identifier whose some version passes crit + stability **and** has each depends met by an installed mod/DLL/DLC or a Compatible non-DLC provider of a named ident (cycles count as met).

## 3. RelationshipResolverOptions (RelationshipResolverOptions.cs)

Defaults: `with_recommends=true`, `with_suggests=false`, `with_all_suggests=false`, `without_toomanyprovides_kraken=false`, `without_enforce_consistency=false`, `proceed_with_inconsistencies=false`, `allow_incompatible=false` (unused inside the resolver), `get_recommenders=false` and `stability_tolerance`.

Presets (lines 18-55):
- `DependsOnly`: rec, sug and all_sug are all false.
- `KitchenSink`: rec=true, sug=true, no_tmp=true, no_enforce=true, proceed=true, get_recommenders=true.
- `Conflicts`: no_tmp=true, proceed=true, no_enforce=true, rec=false.

Derived options:
- `OptionsFor(d) = d.suppress_recommendations ? WithoutRecommendations() : self`.
- `WithoutRecommendations()` clears rec, sug and all_sug.
- `WithoutSuggestions()` clears only `with_suggests`; `with_all_suggests` stays set.
- `OptionalHandling()` gives flags: Rec if with_recommends; Sug if with_suggests or all_sug; AllSug if all_sug (143-149).

The CLI install command (`Cmdline/Action/Install.cs:111-119`) uses `with_recommends = !--no-recommends`, `with_suggests`, `with_all_suggests`, and `without_toomanyprovides_kraken = without_enforce_consistency = user.Headless`.

## 4. Phase A: feasibility tree (RRT, RRel)

The constructor (RR:30-73) does these steps:
```
installed = registry.installed_modules.map(.Module) - modulesToRemove      // equality = id+version
reasons[m] += Installed for each installed
dlls = registry.InstalledDlls - modulesToInstall.identifiers              // DLL being upgraded to a managed module
tree = ResolvedRelationshipsTree(toInst, registry, dlls, installed, stab, crit, opts.OptionalHandling())
if !proceed_with_inconsistencies and tree.unsatisfied() nonempty:
    throw DependenciesNotSatisfiedKraken(unsatisfied)
AddModulesToInstall(toInst)     // Phase B
```
**Tree build.** There is one shared `cache: Map<Descriptor, Resolved>` (RRT:238-296). For each requested module `m`, call `resolve_module(m, definitely = toInst, all = toInst)`. It takes the depends with reason Depends; then, if the Rec flag is set, the recommends with reason Recommended(m, 0); then, if the Sug flag is set, the suggests with reason Suggested(m). Each descriptor `rel` uses `cache.get_or_insert(rel)`:
```
if rel matches (installed, dlls, InstalledDlc):  matched? ResolvedByInstalled(mod) : ResolvedByDLL
elif rel matches (all, None, None) with a module: ResolvedByInstalling(mod)
else: providers = rel.LatestAvailableWithProvides(registry, stab, crit, installed, definitely)
      ResolvedByNew{ resolved: providers.map(p -> (p, resolve_module(p, definitely, all+[source],
                       optRels', cache')))) }
```
- `optRels'` (RRel:171-176): if `rel.suppress_recommendations`, drop Rec and Sug. Otherwise, if AllSug is not set, drop Sug. This means suggestions go only one level deep unless with_all_suggests is set.
- `cache'`: when there is exactly 1 provider, use the shared cache. When there is more than 1 provider, use a **copy** per branch (RRel:179-181).
- The cache entry is keyed by descriptor, so the first source wins. The value is re-labelled with `WithSource(source, reason)` (RRT:296).
- `crit` is always applied: dependencies resolve only to compatible versions; `allow_incompatible` affects only CLI-named mods.

**Unsatisfied** (RRel:62-65, 205-260). Only chains with reason `Depends` count. Recommends and suggests never fail Phase A.
- `ResolvedByNew` is unsatisfied when it has no non-DLC provider.
- Otherwise it is unsatisfied only when **every** provider's sub-relationships have at least one unsatisfied chain. Each such chain is returned with `this` prepended.
- `EnrichRejection` (RRT:66-72): an empty ResolvedByNew gets a reason by re-querying without filters (`UnsatisfiedCandidates`, RRel:300-323).

## 5. Phase B: building the changeset (RR:79-400)

State: `modlist: Map<String, Module>` (keys are identifiers **and** provides aliases), `reasons: Map<Module, Vec<Reason>>`, `user_requested`, `conflicts: Vec<(Module, Module)>`, `alreadyResolved: Set<Module>`, `suppressedRecommenders: Set<Descriptor>`.

```
fn add_modules_to_install(mods):
  for m in mods (in order):                                     // RR:89-115
    for x in (modlist.values ∪ installed).distinct() where x.ConflictsWith(m):
      if proceed: conflicts += (x,m),(m,x) else throw Inconsistent("{m} conflicts with {x}")
    user_requested.push(m); add(m, UserRequested)
  for m in user_requested: resolve(m, opts, None)
  // RR:126-139
  check_consistency(modlist.values ∪ installed, dlls, InstalledDlc)
     -> on failure: if without_enforce_consistency { conflicts += new (a, b) pairs } else throw BadRelationshipsKraken

fn resolve(m, opts, old_stanza):                                 // RR:146-187 (depth-first)
  if !alreadyResolved.insert(m): return
  resolve_stanza(m.depends,    Depends(m),       opts, soft=false, old_stanza)
  if opts.with_recommends:
    resolve_stanza(m.recommends, Recommended(m,0), child(opts), soft=true, old_stanza)
  if opts.with_suggests || opts.with_all_suggests:
    resolve_stanza(m.suggests,   Suggested(m),     child(opts), soft=true, old_stanza)
  where child(o) = o.get_recommenders ? o.WithoutRecommendations() : o.WithoutSuggestions()

fn resolve_stanza(stanza, reason, orig_opts, soft, old_stanza):  // RR:202-400
  for d in stanza:
    if orig_opts.get_recommenders && d.suppress_recommendations: suppressedRecommenders += d; continue
    opts = orig_opts.OptionsFor(d)
    // 1. satisfied by something already in the changeset
    if let Some(c) = d.matches_module(modlist.values.distinct()):
        reasons[c] += reason; resolve(c, opts, Some(stanza)); continue
    // 2. changeset holds the named ident but at a wrong version
    if d.ContainsAny(modlist.keys):
        if opts.proceed { if reason has parent P: conflicts += (mod,P),(P,mod) ; continue }
        else throw Inconsistent("{d} required, but an incompatible version is in the resolver")
    // 3. satisfied by installed module / DLL / DLC
    if d matches (installed, dlls, InstalledDlc) -> (_, Some(i)): reasons[i] += reason; continue  (None: continue)
    // 4. choose candidates (tree-filtered), §6
    candidates = match tree.candidates(d, opts.get_recommenders ? dependencies() : modlist.values.distinct()):
        Err(DependenciesNotSatisfied k) if opts.proceed =>
            for u in k.unsatisfied where u.rejection is RejectedByConflict r:
                reasons[r.provider] += (u.depends.last().reason or reason)
                conflicts += (r.provider, r.blockingMod), (r.blockingMod, r.provider)
            continue
        Err(_) if soft => continue           // recommendation/suggestion not available: silently skip
        Err(e) => throw e
    // 5. too many providers
    if candidates.len() > 1:
        if opts.without_toomanyprovides_kraken:
            if opts.get_recommenders && reason !is Depends:
                for (i,c) in candidates: add(c, reason is Recommended ? Recommended(P,i) : reason)
            continue                         // NOTE: a Depends stays unresolved (headless mode)
        if let Some(old) = old_stanza:
            pick = candidates.filter(c => old.any(r => r.within_bounds(c)))
            if pick.len() != 1: throw TooManyModsProvide(P, d.to_string(), candidates, d.choice_help_text)
            candidates = [pick[0]]
        else: throw TooManyModsProvide(P, d.to_string(), candidates, d.choice_help_text)
    c = candidates.single()
    // 6. conflict check against the fixed set
    fixed = (opts.get_recommenders ? dependencies() : modlist.values) ∪ installed
    match fixed.find(x => x.ConflictsWith(c)):
      None => add(c, (d is Module && d.name != c.identifier && reason is Depends(P))
                       ? VirtualDepends(P, d.to_string()) : reason);
              resolve(c, opts, Some(stanza))
      Some(x) if opts.proceed => add(c, reason); conflicts += (x,c),(c,x)
      Some(_) if soft => skip
      Some(x) => throw Inconsistent("{x} conflicts with {c} (needed by user-requested ...)")
```
`old_stanza` = the stanza that pulled in the current module; its relationships break provider ties.

**add(m, reason)** (RR:418-468):
```
if let Some(dup) = modlist[m.identifier]:
   if dup.identifier == m.identifier:
      if dup.version == m.version && !dup.MetadataEquals(m): remove dup from modlist and reasons   // reinstall
      else: panic "Already added"         // programming error
   else: modlist.remove(m.identifier)      // key was a provides alias of another mod; real mod replaces it
modlist[m.identifier] = m; reasons[m] += reason
for alias in m.provides where !modlist.contains_key(alias): modlist[alias] = m
```

**Consistency** (`SanityChecker.cs:44-103`) fails if any `m.depends` does not match `(mods, dlls, dlc)`, or any `m.conflicts` matches `mods` minus m's identifier.

## 6. tree.candidates(d, installing) (RRT:74-183)

```
match cache.get(d):
  ResolvedByInstalling(x) => [x]; ResolvedByInstalled(x) => [x]
  ResolvedByNew(rn) => for (p, subs) in rn.resolved:
      if installing has same id but other version than p -> reject RejectedByVersionMismatch(p, clash)
      elif FindConflict(p, installing, rn.context.installed) -> reject RejectedByConflict
           (installed blockers are checked first, then installing; same identifier skipped; RRel:329-355)
      elif p.BadRelationships(installing) has type Depends -> reject RejectedByRelationship
      elif subs.flat_map(bad_relationships(installing)) nonempty -> reject (chain-prefixed)
      else candidates.push(p)
  ResolvedByDLL | None => no candidates
if candidates nonempty: return candidates      // keep provider order from §2 (download count desc, name)
if unresolved empty and cache[d] is ResolvedByNew: unresolved = rn.UnsatisfiedCandidates()
if unresolved empty: throw InconsistentKraken("{d} unsatisfied; installing: ...")
throw DependenciesNotSatisfiedKraken(unresolved with ancestor chains prepended)
```
`ResolvedByNew.bad_relationships` (RRel:262-293), Depends only: empty if any provider is clean, else all providers' problems.

## 7. Changeset outputs (RR:474-628)
- **ModList()**: `modlist.values.distinct()`, sorted by (UserRequested reason? false first), `allDependers(m).count()` desc, `name` asc. It includes user-requested mods that are already installed. `allDependers(m)` = breadth-first search over the RelationshipReason parents in `reasons`, including m.
- **ReadyToInstall(m, done)**: no pending mod in `modlist - m - done - allDependers(m)` has m among its dependers. The installer repeatedly installs the ready set, sorted by identifier (MI:233-254).
- **IsAutoInstalled(m)**: every reason is Depends (including VirtualDepends) with a non-metapackage parent.
- **Dependencies()**: breadth-first search from user_requested (minus those a suppressed recommender is within bounds of) along `Depends(parent)` reasons.
- **Recommendations(deps)** / **Suggestions(deps, recs)**: mods not in deps (and not in recs) with a Recommended (or Suggested) reason whose parent ∈ deps and no suppressed recommender within bounds of that parent; sorted by depender count desc.
- **Supporters(supported, exclude)**: `CompatibleModules - exclude`, not installed, any `supports` entry matching `supported`.
- **ConflictList** groups conflicts by user-requested root. **ConflictDescriptions** keeps the pair whose key has depender count <= its value's. `IsConsistent = conflicts.is_empty()`.

## 8. Install flow (MI:60-183, Cmdline/Action/Install.cs:84-200)
1. CLI picks each requested module: exact `id=ver` (with crit, or none if `--allow-incompatible`), else `LatestAvailable(id, stab, crit, None, installed)`, else the installed module.
2. `RR(mods, None, opts).ModList()`. Any DLC throws `ModuleIsDLCKraken`. Metapackages are never downloaded.
3. Install in dependency order with auto flag `autoInstalled.contains(m) || IsAutoInstalled(m)`. `Install()` skips ("already installed") when `InstalledVersion(id)` is non-null and not Unmanaged. That check **includes provides** (MI:277-285).
4. **TMP loop** (Install.cs:138-171): sort `ex.modules` by download count desc, then `identifier == ex.requested` first, then identifier. Prompt with `"{identifier} ({name})"` and the message `choice_help_text ?? "{requested} provided by more than one available module, required by {requester.name}"`. Add the choice to `modules` **and** `autoInstalled`, then retry InstallList. A choice < 0 aborts.

## 9. Upgrade

**HasUpdate(id, ..., installed?)** (IRQ:192-235):
```
instVer = InstalledVersion(id)   // includes DLLs (Unmanaged) and provides
if None: false
latest = try LatestAvailable(id, stab, crit, None, installed) else None
if latest is None or latest.IsDLC: false
cmp(latest.version, instVer): <0 false; >0 true;
   ==0 MetadataChanged(id) || (checkMissingFiles && installed files missing)
```
`MetadataChanged` (IRQ:414-430; CM:581-650): repo copy of the installed version differs in install stanzas, install_size, sha256/sha1 (if both set), conflicts/depends/recommends (as sorted sets), replaced_by or provides.

**Upgrade all** (CLI `--all`): `UpgradeableModules(instance, held={})` → `FindUpgradeabilitySolutions` (IRQ:287-384).
```
fn solve(idents, partial):
  fixed = idents where held.contains(id) || !HasUpdate(id, installed=partial.mods)   -> installed version (DLC excluded from partial)
  partial += fixed as (false, m); if any fixed m fails DependsAndConflictsOK(partial.mods): yield nothing
  rest = idents - fixed
  if rest empty: yield partial
  for id in rest:
     if !held(id) && HasUpdate(id, partial.mods) -> latest:
         yield* solve(rest - id, partial + (true, latest)); if installed == latest: continue
     if installed is non-DLC and !installed.DependsAndConflictsOK(partial.mods): continue
     yield* solve(rest - id, partial + (false, installed) unless DLC)
take first solution; upgradeable = entries with flag true
```
`held` = GUI "Held" label idents (`Types/Labels/ModuleLabelList.cs:86-89`); CLI passes {}.

**Upgrade named mods** (Cmdline/Action/Upgrade.cs:207-250): `held` = installed ids that are not named. `limiters` = the requests (`id=ver`, else LatestAvailable, else installed) plus the held installed modules. Each non-held limiter is upgradeable if `HasUpdate(id, installed = limiters)` (IRQ:386-412). Otherwise print "already up to date".

**ModuleInstaller.Upgrade(modules)** (MI:1300-1441):
```
removingIdents = installed ∩ modules.ids
autoRemoving   = FindRemovableAutoInstalled(installing=modules, removing=removingIdents)
resolver = RR(modules, toRemove = installed versions of modules + autoRemoving, DependsOnly)
full = resolver.ModList() by id
keep = full.ids ∩ autoRemoving.ids; autoRemoving -= keep; full -= keep
toInstall = full - (installed modules not in `modules`) - skipFiles
autoInstalled += toInstall.filter(resolver.IsAutoInstalled)
toRemove = installed records of toInstall ids + autoRemoving
```
Then confirm. AddRemove uninstalls all of `toRemove`, then installs in dependency order. A module keeps the auto flag of the removed record with the same id, else `autoInstalled.contains` (MI:1266-1273).

## 10. Removal

**FindReverseDependencies(toRemove, toInstall, installed, dlls, dlc, filter?)** (REG:1161-1235):
```
if toRemove empty: return []
yield each toRemove
loop:
  hyp = installed.filter(!toRemove.contains(id)) ∪ toInstall
  broken = FindUnsatisfiedDepends(hyp, dlls, dlc).filter(m ∉ toInstall)  minus filter(desc)
  ids = broken.idents ∩ installed.idents
  yield ids - toRemove
  if toRemove ⊇ ids: break
  toRemove = toRemove ∪ ids
```
**FindRemovableAutoInstalled(installedRecs, installing)** (IRQ:560-651):
```
opts = DependsOnly + no_tmp + no_enforce + proceed
resolver = RR(installing, toRemove = all installed - non-auto recs of installedRecs, opts)
keeping  = non-auto mods of installedRecs ∪ resolver.ModList()
removable = auto recs whose module ∉ keeping
deps = keeping.flat(depends).distinct().filter(!matches(keeping))     // DLL/DLC not considered here
while deps nonempty:
  needed = removable where any dep in deps matches [module]
  if none: break
  removable -= needed; keeping += needed
  deps = (deps - deps satisfied by needed) ∪ needed.depends.filter(!matches(keeping))
return removable
```
Overload `(installing, removingIdents)`: installedRecs = records not being removed (IRQ:662-672).

**UninstallList(mods, installing)** (MI:799-855):
1. An identifier that is not installed throws `ModNotInstalledKraken`. A DLC throws `ModuleIsDLCKraken`.
2. `revdep = mods ∪ FindReverseDependencies(mods - installing.ids, installing)`.
3. `goners = (revdep ∪ FindRemovableAutoInstalled(installing, revdep).ids)`, sorted by ordinal.
4. List them as `" * {name} {version}"` and ask yes/no. No throws `CancelledActionKraken`.

## 11. replaced_by

`replaced_by` is a ModuleRelationshipDescriptor (`{"name", "version"?, "min_version"?, "max_version"?}`) (CM:68-69). **GetReplacement(installedMod)** (IRQ:509-558):
- If `replaced_by.version` is set, use `GetModuleByVersion(name, version)`.
- Otherwise use `LatestAvailable(name, stab, crit, replaced_by)`.
- Return the result only if `IsCompatible(crit)`. `ModuleNotFoundKraken` gives None.
- Only the *installed* version's `replaced_by` is used (IRQ:500-507).

**Replace()** (MI:1449-1544): install every `ReplaceWith`, remove `ToReplace` (if not installed, throw `ModuleNotFoundKraken`), and also remove an installed `ReplaceWith`. Then `RR(modsToInstall, None, options)` and AddRemove.

## 12. Recommendations screen (MI:1572-1700)
RR(source - DLC, toRemove + FindRemovableAutoInstalled, KitchenSink); `recommenders = Dependencies()`. Pre-check only recs with `Recommended{ProvidesIndex: 0}`, minus `RR(toInstall + checked, removing, ConflictsOpts).ConflictList.keys`. Suggestions are never pre-checked. Supporters are kept if `CanInstall(toInstall + s)` with DependsOnly. **CanInstall** = `ModList(false).len() >= toInstall.non_metapackage.count()`; on TMP it is `any(p => CanInstall(toInstall + p))`; any other error gives false.

## 13. Errors (Core/Types/Kraken.cs)
- `DependenciesNotSatisfiedKraken` (113): Phase A unsatisfied chain, or §6 has no candidate. Payload `[UnsatisfiedRelation{depends: chain, rejection?}]`.
- `TooManyModsProvideKraken` (273): requester, requested, modules, choice_help_text.
- `InconsistentKraken` (301): a list of strings. `BadRelationshipsKraken` (342) adds unmet `(mod, desc)` and conflicts `(mod, desc, other?)`.
- `ModuleNotFoundKraken` (88), `ModuleIsDLCKraken` (705), `ModNotInstalledKraken` (493), `CancelledActionKraken`.
- Rejections (`RejectedProvider.cs`): `ByConflict{provider, sharedProvidesId?, blockingMod, blockerIsInstalled}`, `ByRelationship{provider, violation{Source, Type, Descriptor}}`, `ByVersionMismatch{provider, blockingMod, blockerChain}`.

## 14. Rust port notes
- `ModRef = Arc<CkanModule>`, Hash/Eq on (identifier, version). Use `IndexMap` for `modlist`/`reasons`: first-match results depend on insertion order.
- `enum Reason { Installed, UserRequested, Depends(M), VirtualDepends(M,String), Recommended(M,usize), Suggested(M) }`; `enum Resolved { Installed(M), Installing(M), Dll, New{providers: Vec<(M, Vec<Resolved>)>, ctx} }`.
- Errors as `Result<_, ResolveError>` mirroring §13; TMP must be a matchable variant (CLI loop, CanInstall). Clone the tree cache per branch when providers > 1.

## UNVERIFIED / notes
- UNVERIFIED: GUI TMP prompt and Held handling (`GUI/` not read); assume CLI behavior with `held` = HeldIdentifiers.
- ModuleVersion / GameVersionCriteria comparison is in another spec.
- UNVERIFIED: `GetDownloadCount` source not traced; use 0 when missing.
- UNVERIFIED: any_of `MatchesAny` uses PLINQ `FirstOrDefault` (normally order-preserving); use the first child in order.
