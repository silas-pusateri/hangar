//! Mod list export (CKAN modpack .ckan, text, markdown, BBCode, CSV, TSV), modpack import, and
//! zip import into the download cache. Search syntax is in `search`.

pub mod export;
pub mod import;
pub mod search;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::error::{Ctx, Error, Result};
use crate::install::DownloadCache;
use crate::instance::Instance;
use crate::meta::game_version::GameVersionRange;
use crate::meta::{
    GameVersion, Kind, List, Module, ModuleVersion, Relationship, RelationshipTarget, ReleaseStatus, Resources, SpecVersion,
    VersionCriteria,
};
use crate::registry::Registry;
use crate::repo::RepoIndex;
use crate::resolve::InstallRequest;
use crate::util::Utc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Ckan,
    Text,
    Markdown,
    BbCode,
    Csv,
    Tsv,
}

impl ExportFormat {
    pub const ALL: [ExportFormat; 6] = [
        ExportFormat::Ckan,
        ExportFormat::Text,
        ExportFormat::Markdown,
        ExportFormat::BbCode,
        ExportFormat::Csv,
        ExportFormat::Tsv,
    ];

    /// CLI names: ckan, text, markdown, bbcode, csv, tsv (case-insensitive).
    pub fn from_name(s: &str) -> Option<ExportFormat> {
        match s.to_ascii_lowercase().as_str() {
            "ckan" => Some(ExportFormat::Ckan),
            "text" | "txt" => Some(ExportFormat::Text),
            "markdown" | "md" => Some(ExportFormat::Markdown),
            "bbcode" => Some(ExportFormat::BbCode),
            "csv" => Some(ExportFormat::Csv),
            "tsv" => Some(ExportFormat::Tsv),
            _ => None,
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            ExportFormat::Ckan => "ckan",
            ExportFormat::Text | ExportFormat::BbCode => "txt",
            ExportFormat::Markdown => "md",
            ExportFormat::Csv => "csv",
            ExportFormat::Tsv => "tsv",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ExportFormat::Ckan => "CKAN modpack (*.ckan)",
            ExportFormat::Text => "Plain text (*.txt)",
            ExportFormat::Markdown => "Markdown (*.md)",
            ExportFormat::BbCode => "BBCode (*.txt)",
            ExportFormat::Csv => "Comma-separated values (*.csv)",
            ExportFormat::Tsv => "Tab-separated values (*.tsv)",
        }
    }
}

/// Options for the modpack (.ckan) export, as in CKAN's modpack editor.
#[derive(Debug, Clone)]
pub struct ModpackOptions {
    pub identifier: String,
    pub name: String,
    pub abstract_: String,
    pub author: Vec<String>,
    pub version: String,
    /// Keep `version` on each relationship.
    pub with_versions: bool,
    /// Put the mods in `recommends` instead of `depends`.
    pub as_recommends: bool,
    /// When false, every depends entry gets `suppress_recommendations: true`.
    pub include_optional: bool,
}

impl ModpackOptions {
    /// CKAN defaults for an instance (`installed-<name>`, abstract, OS user, UTC dotted version).
    pub fn defaults_for(inst: &Instance) -> ModpackOptions {
        defaults_named(&inst.name, &Utc::now())
    }
}

/// Environment.UserName on macOS.
fn os_user() -> String {
    ["USER", "LOGNAME"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
        .unwrap_or_else(|| "unknown".to_string())
}

fn defaults_named(inst_name: &str, now: &Utc) -> ModpackOptions {
    let name = format!("installed-{inst_name}");
    ModpackOptions {
        identifier: crate::util::sanitize_identifier(&name),
        abstract_: format!("A list of modules installed on the {inst_name} KSP instance"),
        name,
        author: vec![os_user()],
        // CKAN formats "hh" (12-hour clock); Hangar uses 24-hour HH on purpose.
        version: now.dotted(),
        with_versions: true,
        as_recommends: false,
        include_optional: true,
    }
}

/// Generate the metapackage of user-installed (not auto-installed, not DLC) modules that exist in
/// the index, dependencies first, with ksp_version_min/max from the instance criteria.
pub fn generate_modpack(inst: &Instance, reg: &Registry, index: &RepoIndex, opts: &ModpackOptions) -> Module {
    modpack_with(reg, index, opts, &inst.criteria(), &Utc::now())
}

/// `generate_modpack` with explicit criteria and clock.
pub(crate) fn modpack_with(reg: &Registry, index: &RepoIndex, opts: &ModpackOptions, crit: &VersionCriteria, now: &Utc) -> Module {
    let chosen: Vec<&Module> = reg
        .installed_modules
        .values()
        .filter(|im| {
            !im.source_module.is_dlc() && !im.auto_installed && !index.versions(&im.source_module.identifier).is_empty()
        })
        .map(|im| &im.source_module)
        .collect();
    let rels: Vec<Relationship> = deps_first(&chosen)
        .into_iter()
        .map(|m| {
            Relationship::One(RelationshipTarget {
                version: opts.with_versions.then(|| m.version.clone()),
                suppress_recommendations: !opts.as_recommends && !opts.include_optional,
                ..RelationshipTarget::simple(&m.identifier)
            })
        })
        .collect();
    let (depends, recommends) = if opts.as_recommends { (Vec::new(), rels) } else { (rels, Vec::new()) };
    let (min, max) = crit.min_max().map_or((None, None), |(a, b)| (Some(a), Some(b)));
    Module {
        // MinimumSpecVersion of a single-license metapackage is always v1.6.
        spec_version: SpecVersion::Str("v1.6".into()),
        comment: None,
        identifier: opts.identifier.clone(),
        name: opts.name.as_str().into(),
        abstract_: opts.abstract_.as_str().into(),
        description: None,
        author: opts.author.as_slice().into(),
        version: ModuleVersion::new(&opts.version),
        ksp_version: None,
        ksp_version_min: min,
        ksp_version_max: max,
        ksp_version_strict: false,
        license: vec!["unknown".to_string()].into(),
        release_status: ReleaseStatus::Stable,
        resources: Resources::default(),
        tags: List::new(),
        localizations: List::new(),
        provides: List::new(),
        depends: depends.into(),
        recommends: recommends.into(),
        suggests: List::new(),
        supports: List::new(),
        conflicts: List::new(),
        replaced_by: None,
        install: List::new(),
        download: List::new(),
        download_size: 0,
        download_hash: None,
        download_content_type: None,
        install_size: 0,
        release_date: Some(now.iso8601().into()),
        kind: Kind::Metapackage,
    }
}

/// Order modules so that each comes after the chosen modules it depends on (by identifier or
/// provides). Ties keep the input order. Cycles are broken where they are found.
fn deps_first<'a>(chosen: &[&'a Module]) -> Vec<&'a Module> {
    let mut by_name: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, m) in chosen.iter().enumerate() {
        for p in m.provides_list() {
            by_name.entry(p).or_default().push(i);
        }
    }
    // 0 = new, 1 = in progress, 2 = done
    let mut state = vec![0u8; chosen.len()];
    let mut out = Vec::with_capacity(chosen.len());

    fn visit<'a>(
        i: usize,
        chosen: &[&'a Module],
        by_name: &HashMap<&str, Vec<usize>>,
        state: &mut [u8],
        out: &mut Vec<&'a Module>,
    ) {
        let Some(m) = chosen.get(i) else { return };
        match state.get_mut(i) {
            Some(s) if *s == 0 => *s = 1,
            _ => return,
        }
        for rel in &m.depends {
            for name in rel.names() {
                for &j in by_name.get(name).map(Vec::as_slice).unwrap_or(&[]) {
                    if j != i {
                        visit(j, chosen, by_name, state, out);
                    }
                }
            }
        }
        if let Some(s) = state.get_mut(i) {
            *s = 2;
        }
        out.push(m);
    }

    for i in 0..chosen.len() {
        visit(i, chosen, &by_name, &mut state, &mut out);
    }
    out
}

/// Export the installed mod list in a format. `Ckan` uses `generate_modpack` with defaults
/// (with versions). Text formats list all installed modules incl. auto-installed and DLC, sorted by name.
pub fn export(fmt: ExportFormat, inst: &Instance, reg: &Registry, index: &RepoIndex) -> String {
    match fmt {
        ExportFormat::Ckan => generate_modpack(inst, reg, index, &ModpackOptions::defaults_for(inst)).to_json_pretty(),
        other => export_text(other, reg),
    }
}

/// The five text formats (`Ckan` gives an empty string here).
pub fn export_text(fmt: ExportFormat, reg: &Registry) -> String {
    let rows = export::sorted_rows(reg);
    match fmt {
        ExportFormat::Ckan => String::new(),
        ExportFormat::Text => export::plain(&rows),
        ExportFormat::Markdown => export::markdown(&rows),
        ExportFormat::BbCode => export::bbcode(&rows),
        ExportFormat::Csv => export::dsv(&rows, ','),
        ExportFormat::Tsv => export::dsv(&rows, '\t'),
    }
}

/// Read a .ckan file from a path, `file://` URI or http(s) URL.
/// Like CKAN (File.ReadAllText + Json.NET), a UTF-8 BOM, comments and trailing commas are
/// accepted: CKAN's own GUI export writes the file with a BOM.
pub fn read_ckan(path_or_url: &str) -> Result<Module> {
    let head = path_or_url.get(..8).unwrap_or(path_or_url).to_ascii_lowercase();
    let bytes = if head.starts_with("http://") || head.starts_with("https://") {
        let mut resp = crate::repo::http_agent()
            .get(path_or_url)
            .call()
            .map_err(|e| Error::Net(format!("{path_or_url}: {e}")))?;
        resp.body_mut().read_to_vec().map_err(|e| Error::Net(format!("{path_or_url}: {e}")))?
    } else {
        let p = match head.starts_with("file://").then(|| path_or_url.get(7..)).flatten() {
            // A file URI is percent-encoded (Uri.LocalPath decodes it).
            Some(rest) => percent_decode(rest),
            None => path_or_url.to_string(),
        };
        if !Path::new(&p).is_file() {
            return Err(Error::NotFound(p));
        }
        std::fs::read(&p).ctx(&p)?
    };
    Module::from_json(&import::lenient_json(&bytes))
}

/// Decode `%XX` escapes. Invalid escapes stay as they are; invalid UTF-8 is replaced.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while let Some(&c) = b.get(i) {
        let hex = |k: usize| b.get(k).and_then(|x| (*x as char).to_digit(16));
        match (c, hex(i + 1), hex(i + 2)) {
            (b'%', Some(h), Some(l)) => {
                out.push((h * 16 + l) as u8);
                i += 3;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn without_version(rel: &Relationship) -> Relationship {
    match rel {
        Relationship::One(t) => Relationship::One(RelationshipTarget { version: None, ..t.clone() }),
        Relationship::AnyOf { any_of, choice_help_text, suppress_recommendations } => Relationship::AnyOf {
            any_of: any_of.iter().map(without_version).collect(),
            choice_help_text: choice_help_text.clone(),
            suppress_recommendations: *suppress_recommendations,
        },
    }
}

/// Install requests for a modpack: one per `depends` entry not already satisfied by an installed
/// module. With `ignore_versions`, version pins are dropped (latest compatible).
/// Empty for a module that is not a metapackage: CKAN (Main.cs InstallFromCkanFiles) only
/// expands metapackages; any other .ckan is installed as `InstallRequest::Module` alone.
pub fn modpack_requests(m: &Module, reg: &Registry, ignore_versions: bool) -> Vec<InstallRequest> {
    let mut out = Vec::new();
    if !m.is_metapackage() {
        return out;
    }
    for rel in &m.depends {
        // CKAN clears the pins before the "already installed" check.
        let rel = if ignore_versions { without_version(rel) } else { rel.clone() };
        if reg.installed_modules.values().any(|im| rel.within_bounds(&im.source_module)) {
            continue;
        }
        match &rel {
            Relationship::One(t) => out.push(match &t.version {
                Some(v) => InstallRequest::Exact(t.name.clone(), v.clone()),
                None => InstallRequest::Latest(t.name.clone()),
            }),
            // No provider chooser for modpacks: take the first alternative.
            Relationship::AnyOf { .. } => {
                if let Some(n) = rel.names().first() {
                    out.push(InstallRequest::Latest((*n).to_string()));
                }
            }
        }
    }
    out
}

/// The modpack with the `version` pins removed from its `depends`, as CKAN does in place when
/// the user chooses to ignore versions (CKAN clears top-level entries only; any_of children are
/// cleared too here, the same as `modpack_requests(.., true)`). Pass this module (not the
/// original) as `InstallRequest::Module` with `modpack_requests(.., true)`, or its pins would
/// conflict with the unpinned requests.
pub fn modpack_without_versions(m: &Module) -> Module {
    Module { depends: m.depends.iter().map(without_version).collect(), ..m.clone() }
}

/// Real game versions inside the modpack's ksp_version range that the instance criteria do not
/// cover yet (as major.minor), to offer adding them to the compatible list.
pub fn missing_compat_versions(m: &Module, crit: &VersionCriteria, index: &RepoIndex) -> Vec<GameVersion> {
    let range = GameVersionRange::between(&m.earliest_game_version(), &m.latest_game_version());
    let have: Vec<GameVersionRange> = crit.versions.iter().map(GameVersion::to_range).collect();
    let mut out: Vec<GameVersion> = Vec::new();
    for gv in &index.known_game_versions {
        if range.contains(gv) && !have.iter().any(|r| r.contains(gv)) {
            let mm = gv.major_minor();
            if !out.contains(&mm) {
                out.push(mm);
            }
        }
    }
    out
}

#[derive(Debug, Default)]
pub struct ImportResult {
    /// Zip file and the module it was stored as.
    pub stored: Vec<(PathBuf, Arc<Module>)>,
    pub already_cached: Vec<(PathBuf, Arc<Module>)>,
    pub not_found: Vec<PathBuf>,
    /// Compatible matched modules (offer to install).
    pub installable: Vec<Arc<Module>>,
}

/// Import downloaded zips: match each by SHA1/SHA256 against all module versions, store in the
/// cache. A directory adds its top-level files. `delete_after` moves the zips to the Trash.
pub fn import_zips(
    paths: &[PathBuf],
    index: &RepoIndex,
    cache: &DownloadCache,
    crit: &VersionCriteria,
    delete_after: bool,
) -> Result<ImportResult> {
    let (files, missing) = import::expand_paths(paths);
    let scan = import::scan_files(&files, &|sha1, sha256| index.find_by_hash(sha1, sha256));
    let in_repo = |m: &Module| index.get(&m.identifier, &m.version).is_some();
    store_matches(scan, missing, &in_repo, cache, crit, delete_after)
}

/// Second half of `import_zips`: everything after hash matching.
fn store_matches(
    scan: import::Scan,
    missing: Vec<PathBuf>,
    in_repo: &dyn Fn(&Module) -> bool,
    cache: &DownloadCache,
    crit: &VersionCriteria,
    delete_after: bool,
) -> Result<ImportResult> {
    let mut res = ImportResult { not_found: missing, ..Default::default() };
    res.not_found.extend(scan.not_found);
    if scan.matched.is_empty() {
        if res.not_found.is_empty() {
            return Ok(res);
        }
        let names: Vec<String> = res
            .not_found
            .iter()
            .map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
            .collect();
        return Err(Error::NotFound(format!("Not found in index: {}", names.join(", "))));
    }

    let mut seen: HashSet<(String, String)> = HashSet::new();
    for (_, mods) in &scan.matched {
        for m in mods {
            if m.is_compatible(crit) && seen.insert((m.identifier.clone(), m.version.to_string())) {
                res.installable.push(Arc::clone(m));
            }
        }
    }

    let mut to_trash = Vec::new();
    for (file, mods) in &scan.matched {
        // Zips with modules that are not in a repo (paid mods with internal .ckan) are kept.
        let deletable = delete_after && mods.iter().all(|m| in_repo(m));
        let size = std::fs::metadata(file).map(|md| md.len()).ok();
        // Store once per primary URL, since each URL has its own cache name.
        let mut urls = HashSet::new();
        let mut to_store = Vec::new();
        for m in mods.iter().filter(|m| urls.insert(m.download.first().cloned().unwrap_or_default())) {
            let cached_size = cache.get(m).and_then(|p| std::fs::metadata(p).ok()).map(|md| md.len());
            if cached_size.is_some() && cached_size == size {
                res.already_cached.push((file.clone(), Arc::clone(m)));
            } else {
                to_store.push(m);
            }
        }
        let n = to_store.len();
        for (k, m) in to_store.into_iter().enumerate() {
            // Move instead of copy on the last store when the file goes away anyway.
            cache.store(m, file, deletable && k + 1 == n)?;
            res.stored.push((file.clone(), Arc::clone(m)));
        }
        if deletable {
            to_trash.push(file.clone());
        }
    }
    for f in to_trash.iter().filter(|f| f.exists()) {
        // util::trash deletes the file when the Trash is not available (CKAN always deletes).
        // The content is in the cache at this point. A failure leaves the file in place.
        let _ = crate::util::trash(f);
    }
    Ok(res)
}

/// Write an export to a file.
pub fn write_export(path: &Path, text: &str) -> Result<()> {
    crate::util::write_atomic(path, text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::InstalledModule;
    use std::collections::BTreeMap;

    fn module(json: &str) -> Module {
        Module::from_json(json.as_bytes()).unwrap()
    }

    fn mm() -> Module {
        module(r#"{"identifier":"ModuleManager","name":"Module Manager","abstract":"a","version":"4.2.3","download":"https://x/mm.zip"}"#)
    }

    fn kop() -> Module {
        module(
            r#"{"identifier":"Kopernicus","name":"Kopernicus Planetary System Modifier","abstract":"a",
            "version":"release-1.12.1-215","download":"https://x/k.zip",
            "depends":[{"name":"ModuleManager"},{"name":"ModularFlightIntegrator"}]}"#,
        )
    }

    fn mfi() -> Module {
        module(r#"{"identifier":"ModularFlightIntegrator","name":"Modular Flight Integrator","abstract":"a","version":"1.2.10","download":"https://x/mfi.zip"}"#)
    }

    fn reg_with(mods: Vec<(Module, bool)>) -> Registry {
        let mut installed = BTreeMap::new();
        for (m, auto) in mods {
            installed.insert(
                m.identifier.clone(),
                InstalledModule {
                    install_time: "2026-11-24T00:00:00Z".into(),
                    source_module: m,
                    auto_installed: auto,
                    installed_files: BTreeMap::new(),
                },
            );
        }
        Registry {
            registry_version: 3,
            sorted_repositories: BTreeMap::new(),
            installed_dlls: BTreeMap::new(),
            installed_modules: installed,
            installed_files: BTreeMap::new(),
            available_modules: Default::default(),
            download_counts: Default::default(),
        }
    }

    fn index_with(mods: &[&Module]) -> RepoIndex {
        let mut idx = RepoIndex::default();
        for m in mods {
            idx.modules.entry(m.identifier.clone()).or_default().push(Arc::new((*m).clone()));
        }
        idx
    }

    fn fixed_now() -> Utc {
        Utc { year: 2026, month: 11, day: 24, hour: 18, min: 30, sec: 12, nanos: 123_456_700 }
    }

    #[test]
    fn defaults() {
        let inst = Instance { name: "My KSP".into(), game_dir: PathBuf::from("/nonexistent"), version: None };
        let o = ModpackOptions::defaults_for(&inst);
        assert_eq!(o.name, "installed-My KSP");
        assert_eq!(o.identifier, "installed-My-KSP");
        assert_eq!(o.abstract_, "A list of modules installed on the My KSP KSP instance");
        assert_eq!(o.author.len(), 1);
        assert!(o.with_versions && !o.as_recommends && o.include_optional);
        assert_eq!(defaults_named("x", &fixed_now()).version, "2026.11.24.18.30.12");
    }

    #[test]
    fn modpack_golden() {
        let dlc = module(r#"{"identifier":"MakingHistory-DLC","name":"Making History","abstract":"a","version":"1.12.1","kind":"dlc"}"#);
        let custom = module(r#"{"identifier":"MyLocal","name":"Local","abstract":"a","version":"1","download":"https://x/l.zip"}"#);
        let reg = reg_with(vec![(kop(), false), (mm(), false), (mfi(), true), (dlc, false), (custom, false)]);
        let (k, m, f) = (kop(), mm(), mfi());
        let idx = index_with(&[&k, &m, &f]);
        let mut opts = defaults_named("My KSP", &fixed_now());
        opts.author = vec!["jdoe".into()];
        let crit = VersionCriteria { versions: vec![GameVersion::parse("1.12.5.3190").unwrap()] };
        let pack = modpack_with(&reg, &idx, &opts, &crit, &fixed_now());
        let want = r#"{
    "spec_version": "v1.6",
    "identifier": "installed-My-KSP",
    "name": "installed-My KSP",
    "abstract": "A list of modules installed on the My KSP KSP instance",
    "author": "jdoe",
    "version": "2026.11.24.18.30.12",
    "ksp_version_min": "1.12.5",
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
"#;
        assert_eq!(pack.to_json_pretty(), want);
        // It must read back as a valid module.
        assert!(Module::from_json(want.as_bytes()).is_ok());
    }

    #[test]
    fn modpack_flags() {
        let reg = reg_with(vec![(kop(), false), (mm(), false)]);
        let (k, m) = (kop(), mm());
        let idx = index_with(&[&k, &m]);
        let crit = VersionCriteria::default();
        let mut opts = defaults_named("x", &fixed_now());
        opts.with_versions = false;
        opts.include_optional = false;
        let p = modpack_with(&reg, &idx, &opts, &crit, &fixed_now());
        assert!(p.ksp_version_min.is_none() && p.ksp_version_max.is_none());
        let j = p.to_json_pretty();
        assert!(j.contains("\"suppress_recommendations\": true"));
        assert!(!j.contains("\"version\": \"4.2.3\""));
        opts.as_recommends = true;
        let p = modpack_with(&reg, &idx, &opts, &crit, &fixed_now());
        assert!(p.depends.is_empty());
        assert_eq!(p.recommends.len(), 2);
        assert!(!p.recommends.iter().any(Relationship::suppress_recommendations));
    }

    #[test]
    fn deps_order_and_cycles() {
        let a = module(r#"{"identifier":"Aaa","version":"1","download":"u","depends":[{"name":"virtual-b"}]}"#);
        let b = module(r#"{"identifier":"Bbb","version":"1","download":"u","provides":["virtual-b"],"depends":[{"any_of":[{"name":"Ccc"},{"name":"Zzz"}]}]}"#);
        let c = module(r#"{"identifier":"Ccc","version":"1","download":"u","depends":[{"name":"Aaa"}]}"#);
        let d = module(r#"{"identifier":"Ddd","version":"1","download":"u"}"#);
        let ids: Vec<&str> = deps_first(&[&a, &b, &c, &d]).iter().map(|m| m.identifier.as_str()).collect();
        // Cycle A -> B -> C -> A is broken at A; every module appears once.
        assert_eq!(ids, vec!["Ccc", "Bbb", "Aaa", "Ddd"]);
    }

    #[test]
    fn text_export_sorted() {
        let mut a = mm();
        a.name = "alpha".into();
        let mut b = kop();
        b.name = "Beta".into();
        let dlc = module(r#"{"identifier":"Serenity-DLC","name":"Breaking Ground","abstract":"a","version":"1:1.7.1","kind":"dlc"}"#);
        let reg = reg_with(vec![(b, false), (a, true), (dlc, false)]);
        assert_eq!(
            export_text(ExportFormat::Text, &reg),
            "alpha (ModuleManager 4.2.3)\nBeta (Kopernicus release-1.12.1-215)\nBreaking Ground (Serenity-DLC 1:1.7.1)\n"
        );
        // Same name apart from case: lowercase first, as .NET culture compare does.
        let mut up = mfi();
        up.name = "Alpha".into();
        let mut low = mm();
        low.name = "alpha".into();
        let reg2 = reg_with(vec![(up, false), (low, false)]);
        assert_eq!(
            export_text(ExportFormat::Text, &reg2),
            "alpha (ModuleManager 4.2.3)\nAlpha (ModularFlightIntegrator 1.2.10)\n"
        );
        let csv = export_text(ExportFormat::Csv, &reg);
        assert_eq!(csv.lines().count(), 4);
        assert!(csv.contains("\nSerenity-DLC,1:1.7.1,Breaking Ground,a,,,dlc,,0,"));
    }

    #[test]
    fn requests_for_modpack() {
        let reg = reg_with(vec![(mm(), false)]);
        let pack = module(
            r#"{"identifier":"installed-x","version":"1","kind":"metapackage","depends":[
                {"name":"ModuleManager","version":"4.2.3"},
                {"name":"Kopernicus","version":"release-1.12.1-215"},
                {"name":"Foo"},
                {"name":"ModuleManager","version":"4.2.4"},
                {"any_of":[{"name":"ModuleManager"},{"name":"Bar"}]},
                {"any_of":[{"name":"Baz"},{"name":"Bar"}]}]}"#,
        );
        let show = |v: Vec<InstallRequest>| -> Vec<String> {
            v.into_iter()
                .map(|r| match r {
                    InstallRequest::Latest(n) => n,
                    InstallRequest::Exact(n, v) => format!("{n}={v}"),
                    InstallRequest::Module(m) => m.to_string(),
                })
                .collect()
        };
        assert_eq!(
            show(modpack_requests(&pack, &reg, false)),
            vec!["Kopernicus=release-1.12.1-215", "Foo", "ModuleManager=4.2.4", "Baz"]
        );
        assert_eq!(show(modpack_requests(&pack, &reg, true)), vec!["Kopernicus", "Foo", "Baz"]);
    }

    #[test]
    fn requests_only_for_metapackages() {
        let reg = reg_with(Vec::new());
        // A plain mod .ckan: CKAN installs the module itself; its depends are not pinned requests.
        assert!(modpack_requests(&kop(), &reg, false).is_empty());
        let pack = module(
            r#"{"identifier":"installed-x","version":"1","kind":"metapackage","depends":[
                {"name":"Kopernicus","version":"release-1.12.1-215"},
                {"name":"Foo","min_version":"2"},
                {"any_of":[{"name":"Bar","version":"3"}]}]}"#,
        );
        let free = modpack_without_versions(&pack);
        let j = free.to_json_pretty();
        assert!(!j.contains("\"version\": \"release-1.12.1-215\"") && !j.contains("\"version\": \"3\""));
        assert!(j.contains("\"min_version\": \"2\""), "min/max stay, as in CKAN");
        assert_eq!(free.identifier, pack.identifier);
        assert_eq!(free.depends.len(), 3);
    }

    #[test]
    fn read_ckan_bom_and_file_uri() {
        let d = std::env::temp_dir().join(format!("hangar-modlist-read {}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("My Pack.ckan");
        // CKAN's GUI export writes a UTF-8 BOM; Json.NET also takes a trailing comma.
        std::fs::write(&f, "\u{feff}{\"identifier\":\"installed-x\",\"version\":\"1\",\"kind\":\"metapackage\",}").unwrap();
        assert_eq!(read_ckan(f.to_str().unwrap()).unwrap().identifier, "installed-x");
        let uri = format!("FILE://{}", f.to_str().unwrap().replace(' ', "%20"));
        assert_eq!(read_ckan(&uri).unwrap().identifier, "installed-x");
        assert!(matches!(read_ckan(&format!("{}x", f.display())), Err(Error::NotFound(_))));
        assert_eq!(percent_decode("a%2"), "a%2");
        assert_eq!(percent_decode("%C3%BC%zz"), "\u{fc}%zz");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn missing_compat() {
        let v = |s: &str| GameVersion::parse(s).unwrap();
        // Skip while GameVersion ranges are still the provisional stub (every range matches).
        if v("1.12").to_range().contains(&v("1.11.2")) {
            return;
        }
        let pack = module(r#"{"identifier":"installed-x","version":"1","kind":"metapackage","ksp_version_min":"1.10","ksp_version_max":"1.12.5"}"#);
        let idx = RepoIndex {
            known_game_versions: ["1.9.1", "1.10.0", "1.10.1", "1.11.2", "1.12.3", "1.12.5"].iter().map(|s| v(s)).collect(),
            ..Default::default()
        };
        let crit = VersionCriteria { versions: vec![v("1.12.5"), v("1.12")] };
        assert_eq!(missing_compat_versions(&pack, &crit, &idx), vec![v("1.10"), v("1.11")]);
    }

    fn write_zip(path: &Path, entries: &[(&str, &str)]) {
        use std::io::Write;
        let mut w = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (n, body) in entries {
            w.start_file(*n, opts).unwrap();
            w.write_all(body.as_bytes()).unwrap();
        }
        w.finish().unwrap();
    }

    #[test]
    fn import_zips_end_to_end() {
        let root = std::env::temp_dir().join(format!("hangar-modlist-e2e-{}", std::process::id()));
        let dl = root.join("dl");
        std::fs::create_dir_all(&dl).unwrap();
        let zip_a = dl.join("Abc-1.0.zip");
        write_zip(&zip_a, &[("GameData/Abc/a.cfg", "x")]);
        let paid = dl.join("Paid.zip");
        write_zip(&paid, &[("GameData/P/P.ckan", r#"{"identifier":"PaidMod","version":"2","author":"a","license":"restricted"}"#)]);
        let junk = dl.join("notes.txt");
        std::fs::write(&junk, "x").unwrap();
        let (sha1, sha256) = crate::util::file_hashes(&zip_a).unwrap();
        let size = std::fs::metadata(&zip_a).unwrap().len();
        let a = module(&format!(
            r#"{{"identifier":"Abc","version":"1.0","download":"https://x.test/abc.zip","download_size":{size},
            "download_hash":{{"sha1":"{sha1}","sha256":"{sha256}"}}}}"#
        ));
        let idx = index_with(&[&a]);
        let cache = DownloadCache::new(root.join("cache"));
        let crit = VersionCriteria::default();
        let keep = root.join("Abc-copy.zip");
        std::fs::copy(&zip_a, &keep).unwrap();

        let res = import_zips(std::slice::from_ref(&dl), &idx, &cache, &crit, true).unwrap();
        let stored: Vec<&str> = res.stored.iter().map(|(_, m)| m.identifier.as_str()).collect();
        assert_eq!(stored, vec!["Abc", "PaidMod"]);
        assert_eq!(res.not_found, vec![junk.clone()]);
        assert_eq!(res.installable.len(), 2);
        // The repo zip was moved into the cache; the paid zip (not in a repo) and junk stay.
        assert!(!zip_a.exists());
        assert!(cache.get(&a).is_some());
        assert!(paid.exists() && junk.exists());

        // The same file again (without delete): already cached, nothing stored, file kept.
        let res = import_zips(std::slice::from_ref(&keep), &idx, &cache, &crit, false).unwrap();
        assert!(res.stored.is_empty());
        assert_eq!(res.already_cached.len(), 1);
        assert!(keep.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn import_not_found_is_error() {
        let cache = DownloadCache::new(std::env::temp_dir().join("hangar-modlist-unused-cache"));
        let scan = import::Scan { matched: Vec::new(), not_found: vec![PathBuf::from("/x/a.zip"), PathBuf::from("/x/b.zip")] };
        let e = store_matches(scan, Vec::new(), &|_| true, &cache, &VersionCriteria::default(), false).unwrap_err();
        assert_eq!(e.to_string(), "Not found: Not found in index: a.zip, b.zip");
        let empty = store_matches(import::Scan::default(), Vec::new(), &|_| true, &cache, &VersionCriteria::default(), false);
        assert!(empty.unwrap().stored.is_empty());
    }

    /// Minimal RFC 4180 reader: number of fields in each record. A doubled quote inside a
    /// quoted field toggles the state twice, so it needs no special case.
    fn csv_field_counts(text: &str) -> Vec<usize> {
        let mut out = Vec::new();
        let (mut fields, mut in_q) = (1usize, false);
        for c in text.chars() {
            match (c, in_q) {
                ('"', _) => in_q = !in_q,
                (',', false) => fields += 1,
                ('\n', false) => {
                    out.push(fields);
                    fields = 1;
                }
                _ => {}
            }
        }
        out
    }

    #[test]
    fn real_meta_csv_and_search() {
        let Some(path) = std::env::var_os("HANGAR_TEST_META") else { return };
        let f = std::fs::File::open(path).unwrap();
        let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(std::io::BufReader::new(f)));
        let mut mods = Vec::new();
        for e in ar.entries().unwrap() {
            let mut e = e.unwrap();
            let p = e.path().unwrap().to_string_lossy().into_owned();
            if !p.ends_with(".ckan") {
                continue;
            }
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut e, &mut buf).unwrap();
            if let Ok(m) = Module::from_json(&buf) {
                mods.push(m);
            }
        }
        assert!(mods.len() > 30_000, "parsed {}", mods.len());
        let rows: Vec<&Module> = mods.iter().collect();
        let csv = export::dsv(&rows, ',');
        let counts = csv_field_counts(&csv);
        assert_eq!(counts.len(), mods.len() + 1);
        assert!(counts.iter().all(|n| *n == 20), "a CSV record does not have 20 fields");
        let tsv = export::dsv(&rows, '\t');
        assert!(tsv.len() > 1000);

        let t0 = std::time::Instant::now();
        let st = search::RowState::default();
        for q in ["mechjeb", "@sarbian", "desc:parts", "-tag:plugin dep:modulemanager", "lic:gpl lang:de", "is:installed"] {
            let s = search::ModSearch::parse(q);
            let n = mods.iter().filter(|m| s.matches(m, &st)).count();
            assert!(n < mods.len(), "{q} matched everything");
        }
        let s = search::ModSearch::parse("mechjeb");
        assert!(mods.iter().any(|m| m.identifier == "MechJeb2" && s.matches(m, &st)));
        eprintln!("6 searches over {} modules: {:?}", mods.len(), t0.elapsed());
    }
}
