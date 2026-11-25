//! Regression tests for defects found in review (CKAN behavior checks).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use super::*;
use crate::meta::{GameVersion, Module, ReleaseStatus, VersionCriteria};
use crate::registry::{InstalledModule, Registry};
use crate::repo::RepoIndex;

fn m(id: &str, ver: &str, extra: &str) -> Module {
    let j = format!(
        r#"{{"spec_version":1,"identifier":"{id}","name":"{id}","abstract":"x","version":"{ver}","download":"https://example.com/{id}.zip"{extra}}}"#
    );
    match Module::from_json(j.as_bytes()) {
        Ok(m) => m,
        Err(e) => panic!("bad test module {id}: {e}"),
    }
}

struct Fx {
    index: RepoIndex,
    registry: Registry,
    crit: VersionCriteria,
    held: HashSet<String>,
}

impl Fx {
    fn new(mods: Vec<Module>, installed: Vec<(Module, bool)>) -> Fx {
        let mut modules: HashMap<String, Vec<Arc<Module>>> = HashMap::new();
        let mut providers: HashMap<String, Vec<String>> = HashMap::new();
        for md in mods {
            for p in md.provides_list() {
                let v = providers.entry(p.to_string()).or_default();
                if !v.contains(&md.identifier) {
                    v.push(md.identifier.clone());
                }
            }
            modules.entry(md.identifier.clone()).or_default().push(Arc::new(md));
        }
        for v in modules.values_mut() {
            v.sort_by(|a, b| b.version.cmp(&a.version));
        }
        let index = RepoIndex { modules, providers, ..Default::default() };
        let mut installed_modules = BTreeMap::new();
        for (md, auto) in installed {
            installed_modules.insert(
                md.identifier.clone(),
                InstalledModule {
                    install_time: String::new(),
                    source_module: md,
                    auto_installed: auto,
                    installed_files: BTreeMap::new(),
                },
            );
        }
        let registry = Registry {
            registry_version: 3,
            sorted_repositories: BTreeMap::new(),
            installed_dlls: BTreeMap::new(),
            installed_modules,
            installed_files: BTreeMap::new(),
            available_modules: serde_json::Map::new(),
            download_counts: serde_json::Map::new(),
        };
        let crit = VersionCriteria::new(Some(GameVersion::new(1, 12, 5, -1)), &[]);
        Fx { index, registry, crit, held: HashSet::new() }
    }
    fn ctx(&self) -> Context<'_> {
        Context {
            index: &self.index,
            registry: &self.registry,
            criteria: &self.crit,
            held: &self.held,
            stability: ReleaseStatus::Stable,
        }
    }
    fn plan(&self, req: &Request) -> Result<Changeset, ResolveError> {
        plan(&self.ctx(), req, &ResolverOptions::default())
    }
}

fn summary(cs: &Changeset) -> Vec<String> {
    cs.changes
        .iter()
        .map(|c| {
            let k = match c.kind {
                ChangeKind::Install => "I",
                ChangeKind::Remove => "R",
                ChangeKind::Upgrade => "U",
                ChangeKind::Replace => "X",
            };
            format!("{k}:{}:{}", c.module.identifier, c.module.version)
        })
        .collect()
}

fn ver(o: Option<Arc<Module>>) -> Option<String> {
    o.map(|m| m.version.to_string())
}

/// AA and BB pin each other's exact version: CKAN's upgradeability search upgrades both
/// together; one-at-a-time checks against the old versions never can.
#[test]
fn lockstep_upgrade_of_pinned_pair() {
    let fx = Fx::new(
        vec![
            m("AA", "1.0", r#","depends":[{"name":"BB","version":"1.0"}]"#),
            m("AA", "2.0", r#","depends":[{"name":"BB","version":"2.0"}]"#),
            m("BB", "1.0", ""),
            m("BB", "2.0", ""),
        ],
        vec![(m("AA", "1.0", r#","depends":[{"name":"BB","version":"1.0"}]"#), false), (m("BB", "1.0", ""), true)],
    );
    let cs = fx.plan(&Request { upgrade_all: true, ..Default::default() }).unwrap();
    assert_eq!(summary(&cs), ["U:BB:2.0", "U:AA:2.0"]);
    let up = updates(&fx.ctx());
    assert_eq!(up.len(), 2);
    assert_eq!(ver(has_update(&fx.ctx(), "AA")).as_deref(), Some("2.0"));
    assert_eq!(ver(has_update(&fx.ctx(), "BB")).as_deref(), Some("2.0"));
    // Named upgrade of the depender pulls the pinned dependency along (GUI flow).
    let cs = fx.plan(&Request { upgrade: vec!["AA".into()], ..Default::default() }).unwrap();
    assert_eq!(summary(&cs), ["U:BB:2.0", "U:AA:2.0"]);
}

/// An update that needs a newer installed dependency is still listed (CKAN UpgradeableModules
/// starts from an empty partial solution).
#[test]
fn update_needing_newer_dependency_is_listed() {
    let fx = Fx::new(
        vec![
            m("AA", "1.0", ""),
            m("AA", "2.0", r#","depends":[{"name":"BB","min_version":"2.0"}]"#),
            m("BB", "1.0", ""),
            m("BB", "2.0", ""),
        ],
        vec![(m("AA", "1.0", ""), false), (m("BB", "1.0", ""), false)],
    );
    assert_eq!(ver(has_update(&fx.ctx(), "AA")).as_deref(), Some("2.0"));
    assert_eq!(updates(&fx.ctx()).len(), 2);
    // A fixed (not upgradeable) installed module still blocks: CC pins BB 1.0.
    let fx = Fx::new(
        vec![m("BB", "1.0", ""), m("BB", "2.0", "")],
        vec![(m("BB", "1.0", ""), false), (m("CC", "1.0", r#","depends":[{"name":"BB","version":"1.0"}]"#), false)],
    );
    assert!(has_update(&fx.ctx(), "BB").is_none());
    assert!(fx.plan(&Request { upgrade_all: true, ..Default::default() }).unwrap().is_empty());
}

/// AA 2.0 conflicts with the old CC; CC is upgraded in the same plan, so no conflict.
#[test]
fn upgrade_targets_do_not_clash_with_old_versions_of_other_targets() {
    let fx = Fx::new(
        vec![
            m("AA", "1.0", ""),
            m("AA", "2.0", r#","conflicts":[{"name":"CC","max_version":"1.0"}]"#),
            m("CC", "1.0", ""),
            m("CC", "2.0", ""),
        ],
        vec![(m("AA", "1.0", ""), false), (m("CC", "1.0", ""), false)],
    );
    let cs = fx.plan(&Request { upgrade_all: true, ..Default::default() }).unwrap();
    let mut s = summary(&cs);
    s.sort();
    assert_eq!(s, ["U:AA:2.0", "U:CC:2.0"]);
}

/// CKAN Replace: the replacement is not auto-installed, even if the replaced module was.
#[test]
fn replacement_is_not_auto_installed() {
    let old = m("Old", "1.0", r#","replaced_by":{"name":"New"}"#);
    let fx = Fx::new(vec![old.clone(), m("New", "2.0", "")], vec![(old, true)]);
    let cs = fx.plan(&Request { replace: vec!["Old".into()], ..Default::default() }).unwrap();
    assert_eq!(summary(&cs), ["X:New:2.0"]);
    assert!(!cs.changes[0].auto_installed);
}

/// CKAN MetadataEquals ignores choice_help_text / suppress_recommendations and compares
/// versions by value ("1.0" == "1.00"). Duplicate entries still count.
#[test]
fn metadata_compare_follows_ckan_equality() {
    let inst = m("AA", "1.0", r#","depends":[{"name":"BB","min_version":"1.0"}]"#);
    let repo = m(
        "AA",
        "1.0",
        r#","depends":[{"name":"BB","min_version":"1.00","choice_help_text":"pick","suppress_recommendations":true}]"#,
    );
    let fx = Fx::new(vec![repo, m("BB", "1.0", "")], vec![(inst, false), (m("BB", "1.0", ""), false)]);
    assert!(has_update(&fx.ctx(), "AA").is_none());

    let inst = m("AA", "1.0", r#","depends":[{"name":"BB"}]"#);
    let repo = m("AA", "1.0", r#","depends":[{"name":"BB"},{"name":"BB"}]"#);
    let fx = Fx::new(vec![repo, m("BB", "1.0", "")], vec![(inst, false), (m("BB", "1.0", ""), false)]);
    assert!(has_update(&fx.ctx(), "AA").is_some());

    // sha1 differs while sha256 matches: CKAN checks both.
    let inst = m("AA", "1.0", r#","download_hash":{"sha1":"AAAA","sha256":"BBBB"}"#);
    let repo = m("AA", "1.0", r#","download_hash":{"sha1":"CCCC","sha256":"BBBB"}"#);
    let fx = Fx::new(vec![repo], vec![(inst, false)]);
    assert!(has_update(&fx.ctx(), "AA").is_some());
}

/// A module that only had a conflict before still loses its dependency on removal and must
/// be removed as a reverse dependency, not reported as unsatisfied.
#[test]
fn preexisting_conflict_does_not_hide_reverse_dependency() {
    let fx = Fx::new(
        vec![],
        vec![
            (m("XX", "1.0", r#","depends":[{"name":"ZZ"}],"conflicts":[{"name":"YY"}]"#), false),
            (m("YY", "1.0", ""), false),
            (m("ZZ", "1.0", ""), false),
        ],
    );
    let cs = fx.plan(&Request { remove: vec!["ZZ".into()], ..Default::default() }).unwrap();
    assert_eq!(summary(&cs), ["R:ZZ:1.0", "R:XX:1.0"]);
    assert_eq!(reverse_dependencies(&fx.ctx(), &["ZZ".into()]), ["XX"]);
}

/// The final consistency check must see every conflicting pair, not only the first hit per
/// module (BB conflicts with both AA and CC).
#[test]
fn problems_report_every_conflict_pair() {
    use super::world::{MRef, ModSet, problems};
    let mods = [m("AA", "1.0", ""), m("BB", "1.0", r#","conflicts":[{"name":"AA"},{"name":"CC"}]"#), m("CC", "1.0", "")];
    let mut set = ModSet::new();
    for x in &mods {
        set.insert(MRef::Borrowed(x));
    }
    let mut got: Vec<String> = problems(&set, &[], &HashSet::new()).into_iter().map(|p| p.text).collect();
    got.sort();
    assert_eq!(got.len(), 2, "{got:?}");
}

/// Upgrade search cost on real data: 300 installed mods at their oldest compatible version.
/// Run with HANGAR_TEST_META=<master.tar.gz> (release build for real timings).
#[test]
fn real_metadata_upgrade_search_cost() {
    let Some(path) = std::env::var_os("HANGAR_TEST_META") else { return };
    let data = match crate::repo::parse_archive(std::path::Path::new(&path)) {
        Ok(d) => d,
        Err(e) => panic!("parse_archive: {e}"),
    };
    let index = RepoIndex::build(vec![(crate::repo::Repository::default_repo(), data)]);
    let mut fx = Fx::new(vec![], vec![]);
    fx.index = index;
    let ok = installable_set(&fx.ctx());
    let mut ids: Vec<&String> = ok.iter().collect();
    ids.sort();
    let mut installed = BTreeMap::new();
    for id in ids.into_iter().step_by(6).take(300) {
        let Some(old) = fx.index.versions(id).iter().rev().find(|m| m.is_compatible(&fx.crit)) else { continue };
        installed.insert(
            id.clone(),
            InstalledModule {
                install_time: String::new(),
                source_module: (**old).clone(),
                auto_installed: false,
                installed_files: BTreeMap::new(),
            },
        );
    }
    fx.registry.installed_modules = installed;
    let t = std::time::Instant::now();
    let up = updates(&fx.ctx());
    eprintln!("updates: {} of {} in {:?}", up.len(), fx.registry.installed_modules.len(), t.elapsed());
    let t = std::time::Instant::now();
    let r = fx.plan(&Request { upgrade_all: true, ..Default::default() });
    eprintln!("upgrade_all: {:?} in {:?}", r.as_ref().map(|c| c.changes.len()).map_err(|e| e.to_string()), t.elapsed());
}

/// Two installed mods with the same replaced_by target: both old mods must go.
#[test]
fn two_mods_replaced_by_one() {
    let o1 = m("OldA", "1.0", r#","replaced_by":{"name":"New"}"#);
    let o2 = m("OldB", "1.0", r#","replaced_by":{"name":"New"}"#);
    let fx = Fx::new(vec![o1.clone(), o2.clone(), m("New", "2.0", "")], vec![(o1, false), (o2, false)]);
    let req = Request { replace: vec!["OldA".into(), "OldB".into(), "OldA".into()], ..Default::default() };
    let cs = fx.plan(&req).unwrap();
    assert_eq!(summary(&cs), ["R:OldB:1.0", "X:New:2.0"]);
    assert_eq!(cs.changes[1].replaces.as_deref(), Some("OldA"));
}

/// A change set with only removals is not the audit case: no recommendations.
#[test]
fn removal_only_changeset_has_no_optional_mods() {
    let aa = m("AA", "1.0", r#","recommends":[{"name":"RR"}]"#);
    let fx = Fx::new(vec![aa.clone(), m("RR", "1.0", "")], vec![(aa, false), (m("ZZ", "1.0", ""), false)]);
    let cs = fx.plan(&Request { remove: vec!["ZZ".into()], ..Default::default() }).unwrap();
    assert_eq!(summary(&cs), ["R:ZZ:1.0"]);
    assert!(optional_mods(&fx.ctx(), &cs).is_empty());
    assert_eq!(optional_mods(&fx.ctx(), &Changeset::default()).len(), 1);
}
