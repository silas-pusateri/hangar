//! Resolver unit tests on small synthetic indexes and registries.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use super::*;
use crate::meta::{GameVersion, Module, ModuleVersion, ReleaseStatus, VersionCriteria};
use crate::registry::{InstalledModule, Registry};
use crate::repo::RepoIndex;

/// Module from a JSON fragment; `extra` starts with a comma or is empty.
fn m(id: &str, ver: &str, extra: &str) -> Module {
    let j = format!(
        r#"{{"spec_version":1,"identifier":"{id}","name":"{id}","abstract":"x","version":"{ver}","download":"https://example.com/{id}.zip"{extra}}}"#
    );
    match Module::from_json(j.as_bytes()) {
        Ok(m) => m,
        Err(e) => panic!("bad test module {id}: {e}"),
    }
}

fn dlc(id: &str, ver: &str) -> Module {
    m(id, ver, r#","kind":"dlc""#)
}

fn index(mods: Vec<Module>, downloads: &[(&str, u64)]) -> RepoIndex {
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
    let download_counts = downloads.iter().map(|(k, n)| (k.to_string(), *n)).collect();
    RepoIndex { modules, providers, download_counts, ..Default::default() }
}

fn registry(installed: Vec<(Module, bool)>, dlls: &[&str]) -> Registry {
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
    Registry {
        registry_version: 3,
        sorted_repositories: BTreeMap::new(),
        installed_dlls: dlls.iter().map(|d| (d.to_string(), format!("GameData/{d}.dll"))).collect(),
        installed_modules,
        installed_files: BTreeMap::new(),
        available_modules: serde_json::Map::new(),
        download_counts: serde_json::Map::new(),
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
        Fx::with(index(mods, &[]), registry(installed, &[]))
    }
    fn with(index: RepoIndex, registry: Registry) -> Fx {
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
    fn plan_with(&self, req: &Request, o: ResolverOptions) -> Result<Changeset, ResolveError> {
        plan(&self.ctx(), req, &o)
    }
}

fn install(ids: &[&str]) -> Request {
    Request { install: ids.iter().map(|s| InstallRequest::Latest(s.to_string())).collect(), ..Default::default() }
}

fn depends_only() -> ResolverOptions {
    ResolverOptions { with_recommends: false, ..Default::default() }
}

/// "Kind:id:version" per change, in order.
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

fn change<'c>(cs: &'c Changeset, id: &str) -> &'c Change {
    match cs.changes.iter().find(|c| c.module.identifier == id) {
        Some(c) => c,
        None => panic!("no change for {id}"),
    }
}

// ---------------------------------------------------------------- install basics

#[test]
fn depends_chain_order_and_auto_flags() {
    let fx = Fx::new(
        vec![
            m("AA", "1.0", r#","depends":[{"name":"BB"}]"#),
            m("BB", "1.0", r#","depends":[{"name":"CC"}]"#),
            m("CC", "1.0", ""),
            m("CC", "2.0", ""),
        ],
        vec![],
    );
    let cs = fx.plan(&install(&["AA"])).unwrap();
    assert_eq!(summary(&cs), ["I:CC:2.0", "I:BB:1.0", "I:AA:1.0"]);
    assert!(!change(&cs, "AA").auto_installed);
    assert!(change(&cs, "BB").auto_installed && change(&cs, "CC").auto_installed);
    assert_eq!(change(&cs, "AA").reasons, [Reason::UserRequested]);
    assert_eq!(change(&cs, "CC").reasons, [Reason::Depends("BB".into())]);
    assert_eq!(cs.downloads().len(), 3);
}

#[test]
fn depends_cycle_installs_before_its_dependers() {
    // AA needs ZZ; ZZ and MM need each other. Both cycle members come before AA.
    let fx = Fx::new(
        vec![
            m("AA", "1.0", r#","depends":[{"name":"ZZ"}]"#),
            m("ZZ", "1.0", r#","depends":[{"name":"MM"}]"#),
            m("MM", "1.0", r#","depends":[{"name":"ZZ"}]"#),
        ],
        vec![],
    );
    assert_eq!(summary(&fx.plan(&install(&["AA"])).unwrap()), ["I:MM:1.0", "I:ZZ:1.0", "I:AA:1.0"]);
}

#[test]
fn version_bounds_and_exact_requests() {
    let fx = Fx::new(
        vec![
            m("AA", "1.0", r#","depends":[{"name":"BB","max_version":"1.5"}]"#),
            m("BB", "1.0", ""),
            m("BB", "1.5", ""),
            m("BB", "2.0", ""),
        ],
        vec![],
    );
    let cs = fx.plan(&install(&["AA"])).unwrap();
    assert_eq!(summary(&cs), ["I:BB:1.5", "I:AA:1.0"]);
    let req = Request { install: vec![InstallRequest::Exact("BB".into(), ModuleVersion::new("1.0"))], ..Default::default() };
    assert_eq!(summary(&fx.plan(&req).unwrap()), ["I:BB:1.0"]);
    let req = Request { install: vec![InstallRequest::Exact("BB".into(), ModuleVersion::new("9"))], ..Default::default() };
    assert!(matches!(fx.plan(&req), Err(ResolveError::NotFound(_))));
}

#[test]
fn module_from_file_pulls_deps_from_index() {
    let fx = Fx::new(vec![m("BB", "1.0", "")], vec![]);
    let local = Arc::new(m("Local", "0.1", r#","depends":[{"name":"BB"}]"#));
    let req = Request { install: vec![InstallRequest::Module(local)], ..Default::default() };
    assert_eq!(summary(&fx.plan(&req).unwrap()), ["I:BB:1.0", "I:Local:0.1"]);
}

#[test]
fn not_found_and_unsatisfied_chain() {
    let fx = Fx::new(
        vec![m("AA", "1.0", r#","depends":[{"name":"BB"}]"#), m("BB", "1.0", r#","depends":[{"name":"Gone"}]"#)],
        vec![],
    );
    assert!(matches!(fx.plan(&install(&["Nope"])), Err(ResolveError::NotFound(_))));
    match fx.plan(&install(&["AA"])) {
        Err(ResolveError::Unsatisfied(v)) => {
            assert_eq!(v.len(), 1);
            assert!(v[0].starts_with("AA 1.0 depends on BB"), "{v:?}");
            assert!(v[0].contains("Gone"), "{v:?}");
        }
        o => panic!("{o:?}"),
    }
}

#[test]
fn already_installed_request_is_empty() {
    let fx = Fx::new(vec![m("AA", "1.0", ""), m("AA", "2.0", "")], vec![(m("AA", "1.0", ""), false)]);
    assert!(fx.plan(&install(&["AA"])).unwrap().is_empty());
}

// ---------------------------------------------------------------- compatibility

#[test]
fn dependencies_use_compatible_versions_only() {
    let fx = Fx::new(
        vec![
            m("AA", "1.0", r#","depends":[{"name":"BB"}]"#),
            m("BB", "1.0", r#","ksp_version":"1.12""#),
            m("BB", "2.0", r#","ksp_version":"1.3""#),
            m("Old", "1.0", r#","ksp_version":"1.3""#),
            m("Needy", "1.0", r#","depends":[{"name":"Old"}]"#),
        ],
        vec![],
    );
    assert_eq!(summary(&fx.plan(&install(&["AA"])).unwrap()), ["I:BB:1.0", "I:AA:1.0"]);
    // User-requested incompatible module: only with allow_incompatible.
    assert!(matches!(fx.plan(&install(&["Old"])), Err(ResolveError::Incompatible(_))));
    let o = ResolverOptions { allow_incompatible: true, ..Default::default() };
    assert_eq!(summary(&fx.plan_with(&install(&["Old"]), o).unwrap()), ["I:Old:1.0"]);
    // But never for dependencies.
    assert!(matches!(fx.plan_with(&install(&["Needy"]), o), Err(ResolveError::Unsatisfied(_))));
}

#[test]
fn stability_filter() {
    let fx = Fx::new(vec![m("AA", "1.0", ""), m("AA", "2.0", r#","release_status":"testing""#)], vec![]);
    assert_eq!(summary(&fx.plan(&install(&["AA"])).unwrap()), ["I:AA:1.0"]);
}

// ---------------------------------------------------------------- provides and any_of

#[test]
fn virtual_dependency_too_many_provides_and_choice() {
    let fx = Fx::with(
        index(
            vec![
                m("AA", "1.0", r#","depends":[{"name":"VV","choice_help_text":"Pick one"}]"#),
                m("P1", "1.0", r#","provides":["VV"]"#),
                m("P2", "1.0", r#","provides":["VV"]"#),
            ],
            &[("P2", 50), ("P1", 10)],
        ),
        registry(vec![], &[]),
    );
    match fx.plan(&install(&["AA"])) {
        Err(ResolveError::TooManyProvides { requester, requested, choices, help }) => {
            assert_eq!((requester.as_str(), requested.as_str()), ("AA", "VV"));
            let ids: Vec<&str> = choices.iter().map(|m| m.identifier.as_str()).collect();
            assert_eq!(ids, ["P2", "P1"]); // download count first
            assert_eq!(help.as_deref(), Some("Pick one"));
        }
        o => panic!("{o:?}"),
    }
    let mut req = install(&["AA"]);
    req.choices.insert("VV".into(), "P1".into());
    let cs = fx.plan(&req).unwrap();
    assert_eq!(summary(&cs), ["I:P1:1.0", "I:AA:1.0"]);
    assert!(change(&cs, "P1").auto_installed);
}

#[test]
fn any_of_choice_keyed_by_descriptor_text() {
    let fx = Fx::new(
        vec![
            m("AA", "1.0", r#","depends":[{"any_of":[{"name":"XX"},{"name":"YY"}]}]"#),
            m("XX", "1.0", ""),
            m("YY", "1.0", ""),
        ],
        vec![],
    );
    let key = match fx.plan(&install(&["AA"])) {
        Err(ResolveError::TooManyProvides { requested, .. }) => requested,
        o => panic!("{o:?}"),
    };
    assert_eq!(key, "any of (XX | YY)");
    let mut req = install(&["AA"]);
    req.choices.insert(key, "YY".into());
    assert_eq!(summary(&fx.plan(&req).unwrap()), ["I:YY:1.0", "I:AA:1.0"]);
    // An installed choice satisfies the any_of.
    let fx2 = Fx::new(fx.index.modules.values().flatten().map(|m| (**m).clone()).collect(), vec![(m("XX", "1.0", ""), false)]);
    assert_eq!(summary(&fx2.plan(&install(&["AA"])).unwrap()), ["I:AA:1.0"]);
}

#[test]
fn old_stanza_breaks_provider_tie() {
    // B (needed by A) depends on V; A's own stanza names X, so X wins.
    let fx = Fx::new(
        vec![
            m("AA", "1.0", r#","depends":[{"name":"BB"},{"name":"XX"}]"#),
            m("BB", "1.0", r#","depends":[{"name":"VV"}]"#),
            m("XX", "1.0", r#","provides":["VV"]"#),
            m("YY", "1.0", r#","provides":["VV"]"#),
        ],
        vec![],
    );
    let cs = fx.plan(&install(&["AA"])).unwrap();
    assert_eq!(summary(&cs), ["I:XX:1.0", "I:BB:1.0", "I:AA:1.0"]);
}

#[test]
fn user_requested_virtual_name() {
    let fx = Fx::new(vec![m("P1", "1.0", r#","provides":["VV"]"#), m("P2", "1.0", r#","provides":["VV"]"#)], vec![]);
    match fx.plan(&install(&["VV"])) {
        Err(ResolveError::TooManyProvides { requested, .. }) => assert_eq!(requested, "VV"),
        o => panic!("{o:?}"),
    }
    let mut req = install(&["VV"]);
    req.choices.insert("VV".into(), "P2".into());
    let cs = fx.plan(&req).unwrap();
    assert_eq!(summary(&cs), ["I:P2:1.0"]);
    assert!(!cs.changes[0].auto_installed);
}

#[test]
fn provider_with_unmet_depends_is_skipped() {
    let fx = Fx::new(
        vec![
            m("AA", "1.0", r#","depends":[{"name":"VV"}]"#),
            m("Good", "1.0", r#","provides":["VV"]"#),
            m("Bad", "1.0", r#","provides":["VV"],"depends":[{"name":"Missing"}]"#),
        ],
        vec![],
    );
    assert_eq!(summary(&fx.plan(&install(&["AA"])).unwrap()), ["I:Good:1.0", "I:AA:1.0"]);
}

// ---------------------------------------------------------------- installed, DLL, DLC

#[test]
fn installed_dll_and_dlc_satisfy_depends() {
    let mods = vec![
        m("AA", "1.0", r#","depends":[{"name":"BB"},{"name":"Dll"},{"name":"MakingHistory-DLC","min_version":"1.1"}]"#),
        m("BB", "1.0", ""),
        m("Dll", "1.0", ""),
    ];
    let reg = registry(vec![(m("BB", "1.0", ""), false), (dlc("MakingHistory-DLC", "1.9.1"), false)], &["Dll"]);
    let fx = Fx::with(index(mods.clone(), &[]), reg);
    assert_eq!(summary(&fx.plan(&install(&["AA"])).unwrap()), ["I:AA:1.0"]);
    // Missing DLC cannot be installed.
    let fx = Fx::with(
        index(mods.into_iter().chain([dlc("MakingHistory-DLC", "1.9.1")]).collect(), &[]),
        registry(vec![], &["Dll"]),
    );
    match fx.plan(&install(&["AA"])) {
        Err(ResolveError::Unsatisfied(v)) => assert!(v.iter().any(|s| s.contains("DLC")), "{v:?}"),
        o => panic!("{o:?}"),
    }
    assert!(matches!(fx.plan(&install(&["MakingHistory-DLC"])), Err(ResolveError::IsDlc(_))));
}

#[test]
fn unknown_dlc_version_matches_any_bounds() {
    let mods = vec![m("AA", "1.0", r#","depends":[{"name":"BreakingGround-DLC","min_version":"1.5"}]"#)];
    let reg = registry(vec![(dlc("BreakingGround-DLC", crate::registry::UNKNOWN_DLC_VERSION), false)], &[]);
    let fx = Fx::with(index(mods, &[]), reg);
    assert_eq!(summary(&fx.plan(&install(&["AA"])).unwrap()), ["I:AA:1.0"]);
}

#[test]
fn preexisting_broken_install_does_not_block() {
    let fx = Fx::new(vec![m("AA", "1.0", "")], vec![(m("Broken", "1.0", r#","depends":[{"name":"Missing"}]"#), false)]);
    assert_eq!(summary(&fx.plan(&install(&["AA"])).unwrap()), ["I:AA:1.0"]);
}

// ---------------------------------------------------------------- conflicts

#[test]
fn conflicts_both_directions() {
    let a = m("AA", "1.0", r#","conflicts":[{"name":"BB"}]"#);
    let fx = Fx::new(vec![a.clone(), m("BB", "1.0", "")], vec![(m("BB", "1.0", ""), false)]);
    assert!(matches!(fx.plan(&install(&["AA"])), Err(ResolveError::Conflicts(_))));
    let fx = Fx::new(vec![m("AA", "1.0", ""), m("BB", "1.0", "")], vec![(m("BB", "1.0", r#","conflicts":[{"name":"AA"}]"#), false)]);
    assert!(matches!(fx.plan(&install(&["AA"])), Err(ResolveError::Conflicts(_))));
    // Two requested modules that conflict.
    let fx = Fx::new(vec![a, m("BB", "1.0", "")], vec![]);
    assert!(matches!(fx.plan(&install(&["AA", "BB"])), Err(ResolveError::Conflicts(_))));
}

#[test]
fn dependency_conflicting_with_installed_is_rejected() {
    let fx = Fx::new(
        vec![m("AA", "1.0", r#","depends":[{"name":"CC"}]"#), m("CC", "1.0", r#","conflicts":[{"name":"DD"}]"#)],
        vec![(m("DD", "1.0", ""), false)],
    );
    match fx.plan(&install(&["AA"])) {
        Err(ResolveError::Unsatisfied(v)) => assert!(v[0].contains("conflicts with DD"), "{v:?}"),
        o => panic!("{o:?}"),
    }
}

#[test]
fn conflict_picks_older_version_that_fits() {
    // C 2.0 conflicts with installed D; C 1.0 does not.
    let fx = Fx::new(
        vec![
            m("AA", "1.0", r#","depends":[{"name":"CC"}]"#),
            m("CC", "1.0", ""),
            m("CC", "2.0", r#","conflicts":[{"name":"DD"}]"#),
        ],
        vec![(m("DD", "1.0", ""), false)],
    );
    assert_eq!(summary(&fx.plan(&install(&["AA"])).unwrap()), ["I:CC:1.0", "I:AA:1.0"]);
}

// ---------------------------------------------------------------- recommends / suggests

#[test]
fn recommends_and_suggests_follow_options() {
    let fx = Fx::new(
        vec![
            m("AA", "1.0", r#","recommends":[{"name":"RR"}],"suggests":[{"name":"SS"}]"#),
            m("RR", "1.0", ""),
            m("SS", "1.0", r#","suggests":[{"name":"TT"}]"#),
            m("TT", "1.0", ""),
        ],
        vec![],
    );
    let cs = fx.plan(&install(&["AA"])).unwrap();
    assert_eq!(summary(&cs), ["I:RR:1.0", "I:AA:1.0"]);
    assert!(!change(&cs, "RR").auto_installed);
    assert_eq!(change(&cs, "RR").reasons, [Reason::Recommended("AA".into())]);
    assert_eq!(summary(&fx.plan_with(&install(&["AA"]), depends_only()).unwrap()), ["I:AA:1.0"]);
    let o = ResolverOptions { with_suggests: true, ..Default::default() };
    assert_eq!(summary(&fx.plan_with(&install(&["AA"]), o).unwrap()), ["I:RR:1.0", "I:SS:1.0", "I:AA:1.0"]);
    let o = ResolverOptions { with_all_suggests: true, ..Default::default() };
    assert_eq!(summary(&fx.plan_with(&install(&["AA"]), o).unwrap()), ["I:RR:1.0", "I:TT:1.0", "I:SS:1.0", "I:AA:1.0"]);
}

#[test]
fn missing_recommendation_is_skipped_and_suppression_works() {
    let fx = Fx::new(
        vec![
            m("AA", "1.0", r#","recommends":[{"name":"Gone"}],"depends":[{"name":"BB","suppress_recommendations":true}]"#),
            m("BB", "1.0", r#","recommends":[{"name":"RR"}]"#),
            m("RR", "1.0", ""),
        ],
        vec![],
    );
    assert_eq!(summary(&fx.plan(&install(&["AA"])).unwrap()), ["I:BB:1.0", "I:AA:1.0"]);
}

#[test]
fn metapackage_parent_is_not_auto() {
    let fx = Fx::new(
        vec![m("Pack", "1.0", r#","kind":"metapackage","depends":[{"name":"BB"}]"#), m("BB", "1.0", "")],
        vec![],
    );
    let cs = fx.plan(&install(&["Pack"])).unwrap();
    assert_eq!(summary(&cs), ["I:BB:1.0", "I:Pack:1.0"]);
    assert!(!change(&cs, "BB").auto_installed);
    assert_eq!(cs.downloads().len(), 1);
}

// ---------------------------------------------------------------- removal

#[test]
fn remove_with_reverse_deps_and_orphans() {
    let fx = Fx::new(
        vec![],
        vec![
            (m("AA", "1.0", r#","depends":[{"name":"BB"}]"#), false),
            (m("BB", "1.0", r#","depends":[{"name":"CC"}]"#), true),
            (m("CC", "1.0", ""), true),
            (m("Keep", "1.0", ""), true),
            (m("User", "1.0", r#","depends":[{"name":"Keep"}]"#), false),
        ],
    );
    // Removing A orphans auto-installed B and C.
    let req = Request { remove: vec!["AA".into()], ..Default::default() };
    let cs = fx.plan(&req).unwrap();
    assert_eq!(summary(&cs), ["R:AA:1.0", "R:BB:1.0", "R:CC:1.0"]);
    assert_eq!(change(&cs, "BB").reasons, [Reason::NoLongerNeeded]);
    // Removing C breaks B, then A.
    let req = Request { remove: vec!["CC".into()], ..Default::default() };
    let cs = fx.plan(&req).unwrap();
    assert_eq!(summary(&cs), ["R:CC:1.0", "R:BB:1.0", "R:AA:1.0"]);
    assert_eq!(change(&cs, "BB").reasons, [Reason::ReverseDepends("CC".into())]);
    assert_eq!(reverse_dependencies(&fx.ctx(), &["CC".into()]), ["BB", "AA"]);
    assert!(matches!(
        fx.plan(&Request { remove: vec!["Nope".into()], ..Default::default() }),
        Err(ResolveError::NotInstalled(_))
    ));
}

#[test]
fn remove_dlc_is_refused() {
    let fx = Fx::new(vec![], vec![(dlc("MakingHistory-DLC", "1.9.1"), false)]);
    let req = Request { remove: vec!["MakingHistory-DLC".into()], ..Default::default() };
    assert!(matches!(fx.plan(&req), Err(ResolveError::IsDlc(_))));
}

#[test]
fn combined_remove_and_install() {
    let fx = Fx::new(
        vec![m("New", "1.0", r#","depends":[{"name":"Dep"}]"#), m("Dep", "1.0", "")],
        vec![(m("Old", "1.0", ""), false)],
    );
    let req = Request { remove: vec!["Old".into()], ..install(&["New"]) };
    assert_eq!(summary(&fx.plan(&req).unwrap()), ["R:Old:1.0", "I:Dep:1.0", "I:New:1.0"]);
}

// ---------------------------------------------------------------- upgrade and replace

#[test]
fn upgrade_named_held_and_all() {
    let mut fx = Fx::new(
        vec![m("AA", "1.0", ""), m("AA", "2.0", r#","depends":[{"name":"NN"}]"#), m("BB", "1.0", ""), m("BB", "1.1", ""), m("NN", "1.0", "")],
        vec![(m("AA", "1.0", ""), true), (m("BB", "1.0", ""), false)],
    );
    let req = Request { upgrade: vec!["AA".into()], ..Default::default() };
    let cs = fx.plan(&req).unwrap();
    assert_eq!(summary(&cs), ["I:NN:1.0", "U:AA:2.0"]);
    let a = change(&cs, "AA");
    assert_eq!(a.old_version.as_ref().map(|v| v.as_str()), Some("1.0"));
    assert!(a.auto_installed); // keeps the installed record's flag
    assert!(change(&cs, "NN").auto_installed);
    assert_eq!(has_update(&fx.ctx(), "AA").map(|m| m.version.to_string()).as_deref(), Some("2.0"));
    assert_eq!(updates(&fx.ctx()).len(), 2);
    fx.held.insert("AA".into());
    assert!(has_update(&fx.ctx(), "AA").is_none());
    let cs = fx.plan(&Request { upgrade_all: true, ..Default::default() }).unwrap();
    assert_eq!(summary(&cs), ["U:BB:1.1"]);
    assert!(matches!(
        fx.plan(&Request { upgrade: vec!["Zed".into()], ..Default::default() }),
        Err(ResolveError::NotInstalled(_))
    ));
}

#[test]
fn upgrade_respects_installed_dependers() {
    let fx = Fx::new(
        vec![m("AA", "1.0", ""), m("AA", "1.5", ""), m("AA", "2.0", "")],
        vec![(m("AA", "1.0", ""), false), (m("XX", "1.0", r#","depends":[{"name":"AA","max_version":"1.5"}]"#), false)],
    );
    assert_eq!(has_update(&fx.ctx(), "AA").map(|m| m.version.to_string()).as_deref(), Some("1.5"));
}

#[test]
fn upgrade_all_needs_second_pass() {
    // A 2.0 needs B >= 2; A sorts first, so B is upgraded on pass one and A on pass two.
    let fx = Fx::new(
        vec![m("AA", "1.0", ""), m("AA", "2.0", r#","depends":[{"name":"BB","min_version":"2.0"}]"#), m("BB", "1.0", ""), m("BB", "2.0", "")],
        vec![(m("AA", "1.0", ""), false), (m("BB", "1.0", ""), false)],
    );
    let cs = fx.plan(&Request { upgrade_all: true, ..Default::default() }).unwrap();
    assert_eq!(summary(&cs), ["U:BB:2.0", "U:AA:2.0"]);
}

#[test]
fn upgrade_drops_orphaned_dependency() {
    let fx = Fx::new(
        vec![m("AA", "1.0", r#","depends":[{"name":"BB"}]"#), m("AA", "2.0", ""), m("BB", "1.0", "")],
        vec![(m("AA", "1.0", r#","depends":[{"name":"BB"}]"#), false), (m("BB", "1.0", ""), true)],
    );
    let cs = fx.plan(&Request { upgrade: vec!["AA".into()], ..Default::default() }).unwrap();
    assert_eq!(summary(&cs), ["R:BB:1.0", "U:AA:2.0"]);
}

#[test]
fn dependency_driven_upgrade_and_held_block() {
    let mut fx = Fx::new(
        vec![m("AA", "1.0", r#","depends":[{"name":"BB","min_version":"2.0"}]"#), m("BB", "1.0", ""), m("BB", "2.0", "")],
        vec![(m("BB", "1.0", ""), true)],
    );
    let cs = fx.plan(&install(&["AA"])).unwrap();
    assert_eq!(summary(&cs), ["U:BB:2.0", "I:AA:1.0"]);
    fx.held.insert("BB".into());
    assert!(matches!(fx.plan(&install(&["AA"])), Err(ResolveError::Unsatisfied(_))));
}

#[test]
fn metadata_change_counts_as_update() {
    let fx = Fx::new(
        vec![m("AA", "1.0", r#","depends":[{"name":"BB"}]"#), m("BB", "1.0", "")],
        vec![(m("AA", "1.0", ""), false)],
    );
    let u = has_update(&fx.ctx(), "AA");
    assert_eq!(u.map(|m| m.version.to_string()).as_deref(), Some("1.0"));
    let same = Fx::new(vec![m("AA", "1.0", "")], vec![(m("AA", "1.0", ""), false)]);
    assert!(has_update(&same.ctx(), "AA").is_none());
}

#[test]
fn replace_with_replaced_by() {
    let fx = Fx::new(
        vec![m("Old", "1.0", r#","replaced_by":{"name":"New"}"#), m("New", "2.0", r#","depends":[{"name":"DD"}]"#), m("DD", "1.0", "")],
        vec![(m("Old", "1.0", r#","replaced_by":{"name":"New"}"#), false)],
    );
    assert_eq!(replacement(&fx.ctx(), "Old").map(|m| m.identifier.clone()).as_deref(), Some("New"));
    let cs = fx.plan(&Request { replace: vec!["Old".into()], ..Default::default() }).unwrap();
    assert_eq!(summary(&cs), ["I:DD:1.0", "X:New:2.0"]);
    let x = change(&cs, "New");
    assert_eq!(x.replaces.as_deref(), Some("Old"));
    assert_eq!(x.reasons, [Reason::Replaces("Old".into())]);
    assert!(replacement(&fx.ctx(), "DD").is_none());
}

// ---------------------------------------------------------------- installable / optional

#[test]
fn installable_rule_with_cycles() {
    let fx = Fx::new(
        vec![
            m("AA", "1.0", r#","depends":[{"name":"BB"}]"#),
            m("BB", "1.0", r#","depends":[{"name":"AA"}]"#),
            m("CC", "1.0", r#","depends":[{"name":"Missing"}]"#),
            m("DD", "1.0", r#","depends":[{"name":"CC"}]"#),
            m("EE", "1.0", r#","depends":[{"name":"VV"}]"#),
            m("PP", "1.0", r#","provides":["VV"]"#),
            m("Inc", "1.0", r#","ksp_version":"1.3""#),
        ],
        vec![],
    );
    let set = installable_set(&fx.ctx());
    let mut v: Vec<&str> = set.iter().map(String::as_str).collect();
    v.sort();
    assert_eq!(v, ["AA", "BB", "EE", "PP"]);
    let d = fx.index.versions("DD")[0].clone();
    assert!(!is_installable(&fx.ctx(), &d));
    assert!(is_installable(&fx.ctx(), &fx.index.versions("EE")[0]));
}

#[test]
fn optional_mods_dialog_rules() {
    let fx = Fx::with(
        index(
            vec![
                m(
                    "AA",
                    "1.0",
                    r#","recommends":[{"name":"R1"},{"any_of":[{"name":"R2"},{"name":"R3"}]},{"name":"Clash"},{"name":"Have"}],"suggests":[{"name":"SS"}]"#,
                ),
                m("R1", "1.0", ""),
                m("R2", "1.0", ""),
                m("R3", "1.0", ""),
                m("Clash", "1.0", r#","conflicts":[{"name":"Have"}]"#),
                m("Have", "1.0", ""),
                m("SS", "1.0", ""),
                m("Sup", "1.0", r#","supports":[{"name":"AA"}]"#),
                m("Broken", "1.0", r#","depends":[{"name":"Missing"}]"#),
            ],
            &[("R2", 100), ("R3", 5)],
        ),
        registry(vec![(m("Have", "1.0", ""), false)], &[]),
    );
    let cs = fx.plan_with(&install(&["AA"]), depends_only()).unwrap();
    let opt = optional_mods(&fx.ctx(), &cs);
    let got: Vec<(String, OptionalKind, bool)> =
        opt.iter().map(|o| (o.module.identifier.clone(), o.kind, o.checked)).collect();
    let want = [
        ("Clash", OptionalKind::Recommends, false),
        ("R1", OptionalKind::Recommends, true),
        ("R2", OptionalKind::Recommends, true),
        ("R3", OptionalKind::Recommends, false),
        ("SS", OptionalKind::Suggests, false),
        ("Sup", OptionalKind::Supports, false),
    ];
    let want: Vec<(String, OptionalKind, bool)> = want.iter().map(|(a, k, c)| (a.to_string(), *k, *c)).collect();
    assert_eq!(got, want);
    assert!(opt.iter().all(|o| o.sources == ["AA"]));
}

#[test]
fn audit_recommendations_with_empty_changeset() {
    let fx = Fx::new(
        vec![m("AA", "1.0", r#","recommends":[{"name":"RR"}]"#), m("RR", "1.0", "")],
        vec![(m("AA", "1.0", r#","recommends":[{"name":"RR"}]"#), false)],
    );
    let opt = optional_mods(&fx.ctx(), &Changeset::default());
    assert_eq!(opt.len(), 1);
    assert_eq!(opt[0].module.identifier, "RR");
}

// ---------------------------------------------------------------- real metadata

/// True if `from` (transitively) depends on `to` inside the change set (via Depends reasons).
fn needs_path(cs: &Changeset, from: &str, to: &str) -> bool {
    let mut seen: HashSet<String> = HashSet::new();
    let mut todo = vec![to.to_string()];
    // Walk up from `to` through its dependers (the parents in its Depends reasons).
    while let Some(x) = todo.pop() {
        if x == from {
            return true;
        }
        if !seen.insert(x.clone()) {
            continue;
        }
        for c in &cs.changes {
            if c.module.identifier == x {
                for r in &c.reasons {
                    if let Reason::Depends(p) = r {
                        todo.push(p.clone());
                    }
                }
            }
        }
    }
    false
}

/// Plans a few large real mods against KSP 1.12.5. Run with HANGAR_TEST_META=<master.tar.gz>.
#[test]
fn real_metadata_big_mods() {
    let Some(path) = std::env::var_os("HANGAR_TEST_META") else { return };
    let t0 = std::time::Instant::now();
    let data = match crate::repo::parse_archive(std::path::Path::new(&path)) {
        Ok(d) => d,
        Err(e) => panic!("parse_archive: {e}"),
    };
    let idx = RepoIndex::build(vec![(crate::repo::Repository::default_repo(), data)]);
    eprintln!("index: {} identifiers in {:?}", idx.modules.len(), t0.elapsed());
    let fx = Fx::with(idx, registry(vec![], &[]));

    let t = std::time::Instant::now();
    let set = installable_set(&fx.ctx());
    eprintln!("installable_set: {} of {} in {:?}", set.len(), fx.index.modules.len(), t.elapsed());
    assert!(set.contains("MechJeb2"));

    for id in ["Kopernicus", "RealSolarSystem", "MechJeb2", "KerbalEngineerRedux", "RP-1"] {
        let mut req = install(&[id]);
        for round in 0..20 {
            let t = std::time::Instant::now();
            match fx.plan(&req) {
                Ok(cs) => {
                    eprintln!("{id}: {} changes in {:?} (after {round} choices)", cs.changes.len(), t.elapsed());
                    for c in &cs.changes {
                        eprintln!("    {:?} {} auto={} {:?}", c.kind, c.module, c.auto_installed, c.reasons);
                    }
                    // Count depends edges whose dependency comes after its depender.
                    let at: HashMap<&str, usize> =
                        cs.changes.iter().enumerate().map(|(i, c)| (c.module.identifier.as_str(), i)).collect();
                    let late = cs
                        .changes
                        .iter()
                        .enumerate()
                        .flat_map(|(i, c)| c.reasons.iter().map(move |r| (i, r)))
                        .filter(|(i, r)| matches!(r, Reason::Depends(p) if at.get(p.as_str()).is_some_and(|j| j < i)))
                        .filter(|(i, r)| {
                            // Not a cycle if the depender cannot reach this module by depends.
                            let Reason::Depends(p) = r else { return false };
                            !needs_path(&cs, &cs.changes[*i].module.identifier, p)
                        })
                        .count();
                    assert_eq!(late, 0, "{id}: a dependency comes after its depender outside a cycle");
                    let opt = optional_mods(&fx.ctx(), &cs);
                    eprintln!("    optional: {}", opt.len());
                    break;
                }
                Err(ResolveError::TooManyProvides { requested, choices, .. }) => {
                    let pick = choices[0].identifier.clone();
                    eprintln!("{id}: choose {pick} for {requested}");
                    req.choices.insert(requested, pick);
                }
                Err(e) => {
                    eprintln!("{id}: {e}");
                    break;
                }
            }
        }
    }
    // Sanity: the basic ones must resolve.
    for id in ["MechJeb2", "KerbalEngineerRedux"] {
        assert!(fx.plan(&install(&[id])).is_ok(), "{id}");
    }
}
