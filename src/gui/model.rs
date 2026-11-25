//! Session-free view model of the mod list: rows, filtering, sorting and pending changes.
//! Rows are built once per session change (on the worker); the UI only filters and sorts
//! indices into them when the filter, search or sort changes.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use crate::config::LabelList;
use crate::install::cache::CacheIndex;
use crate::meta::{GameVersion, Module, ModuleVersion};
use crate::modlist::search::{Filter, ModSearch, RowState};
use crate::resolve::{ChangeKind, Changeset, Context, InstallRequest, Request};

/// Mod list columns, in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Col {
    Installed,
    Update,
    Replace,
    Name,
    Author,
    InstalledVersion,
    Latest,
    MaxGame,
    DownloadSize,
    ReleaseDate,
    InstallDate,
    Downloads,
}

pub const COLUMNS: [Col; 12] = [
    Col::Installed,
    Col::Update,
    Col::Replace,
    Col::Name,
    Col::Author,
    Col::InstalledVersion,
    Col::Latest,
    Col::Downloads,
    Col::DownloadSize,
    Col::ReleaseDate,
    Col::InstallDate,
    Col::MaxGame,
];

impl Col {
    /// Name stored in `config.hangar.hidden_columns`.
    pub fn key(self) -> &'static str {
        match self {
            Col::Installed => "Installed",
            Col::Update => "Update",
            Col::Replace => "Replace",
            Col::Name => "Name",
            Col::Author => "Author",
            Col::InstalledVersion => "InstalledVersion",
            Col::Latest => "LatestVersion",
            Col::MaxGame => "MaxGameVersion",
            Col::DownloadSize => "DownloadSize",
            Col::ReleaseDate => "ReleaseDate",
            Col::InstallDate => "InstallDate",
            Col::Downloads => "DownloadCount",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Col::Installed => "Installed",
            Col::Update => "Update",
            Col::Replace => "Replace",
            Col::Name => "Name",
            Col::Author => "Author",
            Col::InstalledVersion => "Installed version",
            Col::Latest => "Latest version",
            Col::MaxGame => "Max game version",
            Col::DownloadSize => "Download",
            Col::ReleaseDate => "Released",
            Col::InstallDate => "Install date",
            Col::Downloads => "Downloads",
        }
    }

    /// Name cannot be hidden.
    pub fn can_hide(self) -> bool {
        self != Col::Name
    }
}

/// What kind of thing a row is (decides if the Installed box can change).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RowKind {
    #[default]
    Module,
    /// A DLC (installed or known from the repo). Cannot be changed here.
    Dlc,
    /// An unmanaged DLL with this identifier is installed.
    Dll,
}

/// One row per identifier.
#[derive(Debug, Clone)]
pub struct Row {
    pub id: String,
    pub name: String,
    /// Lowercase name for sorting.
    pub sort_name: String,
    pub author: String,
    /// Metadata shown and searched: the installed module, else the latest compatible, else the latest.
    pub module: Arc<Module>,
    /// Latest compatible version (None if no compatible version).
    pub latest: Option<Arc<Module>>,
    pub installed_version: String,
    pub installed_key: Option<ModuleVersion>,
    pub latest_version: String,
    pub max_game: String,
    pub max_game_key: GameVersion,
    pub download_size: u64,
    pub release_date: String,
    pub install_date: String,
    pub downloads: u64,
    pub kind: RowKind,
    pub installed: bool,
    pub auto: bool,
    pub upgradeable: bool,
    /// Target of the available update.
    pub update_to: Option<Arc<Module>>,
    pub replaceable: bool,
    pub compatible: bool,
    pub cached: bool,
    pub newly_compatible: bool,
    /// Indices into `LabelList::labels`.
    pub labels: Vec<u16>,
}

impl Row {
    pub fn can_toggle_install(&self) -> bool {
        match self.kind {
            RowKind::Module => self.installed || self.latest.is_some(),
            RowKind::Dlc | RowKind::Dll => false,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct Counts {
    pub installed: usize,
    pub upgradeable: usize,
    pub compatible: usize,
    pub total: usize,
}

#[derive(Debug, Default)]
pub struct RowModel {
    pub rows: Vec<Row>,
    pub by_id: HashMap<String, usize>,
    /// (tag, count), sorted by tag.
    pub tags: Vec<(String, usize)>,
    pub untagged: usize,
    /// Identifiers that pass the compatible rule (kept to find newly compatible ones later).
    pub compatible_ids: HashSet<String>,
    pub counts: Counts,
    /// Time spent building (ms).
    pub build_ms: u128,
}

impl RowModel {
    pub fn row(&self, i: usize) -> Option<&Row> {
        self.rows.get(i)
    }

    pub fn find(&self, id: &str) -> Option<usize> {
        self.by_id.get(id).copied()
    }

    /// Recount after an in-place change.
    pub fn recount(&mut self) {
        self.counts = Counts {
            installed: self.rows.iter().filter(|r| r.installed && r.kind != RowKind::Dll).count(),
            upgradeable: self.rows.iter().filter(|r| r.upgradeable).count(),
            compatible: self.rows.iter().filter(|r| r.compatible).count(),
            total: self.rows.len(),
        };
    }
}

/// Everything the row builder needs (no Session, so tests can build one).
pub struct BuildInput<'a> {
    pub ctx: Context<'a>,
    pub cache: &'a CacheIndex,
    pub labels: &'a LabelList,
    pub instance: &'a str,
    /// Compatible ids before the last repo update (for "newly compatible").
    pub prev_compatible: Option<&'a HashSet<String>>,
    pub hide_epochs: bool,
    pub hide_v: bool,
}

fn date_part(s: &str) -> String {
    s.get(..10).unwrap_or(s).to_string()
}

fn install_date(s: &str) -> String {
    s.get(..16).unwrap_or(s).replace('T', " ")
}

fn game_version_text(g: &GameVersion) -> String {
    if g.is_any() { "any".to_string() } else { g.to_string() }
}

/// Build all rows. Cost: one update solve, one installable pass, one pass over identifiers.
pub fn build_rows(inp: &BuildInput) -> RowModel {
    let t0 = std::time::Instant::now();
    let ctx = &inp.ctx;
    let index = ctx.index;
    let reg = ctx.registry;
    let updates = crate::resolve::updates(ctx);
    let installable = crate::resolve::installable_set(ctx);

    // Label membership: id -> label indices (labels for this instance only).
    let mut label_map: HashMap<&str, Vec<u16>> = HashMap::new();
    for (i, l) in inp.labels.labels.iter().enumerate() {
        if l.instance_name.as_deref().is_some_and(|n| n != inp.instance) {
            continue;
        }
        let Ok(i16) = u16::try_from(i) else { continue };
        for id in l.module_identifiers_by_game.get("KSP").into_iter().flatten() {
            label_map.entry(id.as_str()).or_default().push(i16);
        }
    }

    let mut ids: BTreeSet<&str> = index.modules.keys().map(String::as_str).collect();
    ids.extend(reg.installed_modules.keys().map(String::as_str));

    let mut rows = Vec::with_capacity(ids.len());
    let mut tag_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut untagged = 0usize;
    for id in ids {
        let im = reg.installed(id);
        let versions = index.versions(id);
        let latest = index.latest(id, Some(ctx.criteria), ctx.stability).cloned();
        let module: Arc<Module> = match im {
            Some(im) => match index.get(id, &im.source_module.version) {
                Some(m) => Arc::clone(m),
                None => Arc::new(im.source_module.clone()),
            },
            None => match latest.as_ref().or_else(|| versions.first()) {
                Some(m) => Arc::clone(m),
                None => continue,
            },
        };
        let is_dll = im.is_none() && reg.installed_dlls.contains_key(id);
        let kind = if module.is_dlc() {
            RowKind::Dlc
        } else if is_dll {
            RowKind::Dll
        } else {
            RowKind::Module
        };
        let installed = im.is_some() || is_dll;
        let hide = |v: &ModuleVersion| v.display(inp.hide_epochs, inp.hide_v);
        let installed_version = match (im, is_dll) {
            (Some(im), _) => {
                if crate::meta::relationship::is_unknown_dlc_version(&im.source_module) {
                    "(unknown)".to_string()
                } else {
                    hide(&im.source_module.version)
                }
            }
            (None, true) => "(DLL)".to_string(),
            _ => String::new(),
        };
        let newest_any = versions.first().unwrap_or(&module);
        let max_key = newest_any.latest_compatible_real_game_version(&index.known_game_versions);
        let compatible = installable.contains(id) || im.is_some_and(|im| im.source_module.is_compatible(ctx.criteria));
        let newly = inp.prev_compatible.is_some_and(|p| !p.contains(id)) && installable.contains(id);
        let update_to = updates.get(id).cloned();
        let replaceable = im.is_some_and(|im| im.source_module.replaced_by.is_some())
            && crate::resolve::replacement(ctx, id).is_some();
        let cached = !module.download.is_empty() && inp.cache.get(&module).is_some();
        if module.tags.is_empty() {
            untagged += 1;
        }
        for t in &module.tags {
            *tag_counts.entry(t.clone()).or_default() += 1;
        }
        let name = module.display_name().to_string();
        rows.push(Row {
            id: id.to_string(),
            sort_name: name.to_lowercase(),
            name,
            author: module.author.join(", "),
            installed_version,
            installed_key: im.map(|im| im.source_module.version.clone()),
            latest_version: latest.as_ref().map(|m| hide(&m.version)).unwrap_or_default(),
            max_game: game_version_text(&max_key.without_build()),
            max_game_key: max_key,
            download_size: module.download_size,
            release_date: module.release_date.as_deref().map(date_part).unwrap_or_default(),
            install_date: im.map(|im| install_date(&im.install_time)).unwrap_or_default(),
            downloads: index.download_count(id),
            kind,
            installed,
            auto: im.is_some_and(|im| im.auto_installed),
            upgradeable: update_to.is_some(),
            update_to,
            replaceable,
            compatible,
            cached,
            newly_compatible: newly,
            labels: label_map.get(id).cloned().unwrap_or_default(),
            latest,
            module,
        });
    }
    let by_id = rows.iter().enumerate().map(|(i, r)| (r.id.clone(), i)).collect();
    let mut model = RowModel {
        rows,
        by_id,
        tags: tag_counts.into_iter().collect(),
        untagged,
        compatible_ids: installable,
        counts: Counts::default(),
        build_ms: t0.elapsed().as_millis(),
    };
    model.recount();
    model
}

/// Update label indices of all rows in place (after a label edit).
pub fn relabel(model: &mut RowModel, labels: &LabelList, instance: &str) {
    for r in &mut model.rows {
        r.labels.clear();
    }
    for (i, l) in labels.labels.iter().enumerate() {
        if l.instance_name.as_deref().is_some_and(|n| n != instance) {
            continue;
        }
        let Ok(i16) = u16::try_from(i) else { continue };
        for id in l.module_identifiers_by_game.get("KSP").into_iter().flatten() {
            if let Some(r) = model.by_id.get(id).and_then(|&k| model.rows.get_mut(k)) {
                r.labels.push(i16);
            }
        }
    }
}

// ---------------------------------------------------------------- filter

/// The Filter menu: a basic filter, a label, or a tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListFilter {
    Basic(Filter),
    Label(usize),
    Tag(String),
    Untagged,
}

impl Default for ListFilter {
    fn default() -> Self {
        ListFilter::Basic(Filter::Compatible)
    }
}

impl ListFilter {
    pub fn label(&self, label_names: &[String]) -> String {
        match self {
            ListFilter::Basic(f) => f.label().to_string(),
            ListFilter::Label(i) => format!("Label: {}", label_names.get(*i).map(String::as_str).unwrap_or("?")),
            ListFilter::Tag(t) => format!("Tag: {t}"),
            ListFilter::Untagged => "Untagged".to_string(),
        }
    }
}

/// Facts about labels the filter needs: names (by label index) and which ones hide rows.
pub struct LabelInfo {
    pub names: Vec<String>,
    pub hide: Vec<bool>,
    pub colors: Vec<Option<[u8; 3]>>,
}

impl LabelInfo {
    pub fn of(labels: &LabelList) -> LabelInfo {
        LabelInfo {
            names: labels.labels.iter().map(|l| l.name.clone()).collect(),
            hide: labels.labels.iter().map(|l| l.hide).collect(),
            colors: labels.labels.iter().map(|l| l.rgb()).collect(),
        }
    }
}

fn row_state<'a>(r: &Row, names: &'a [&'a str]) -> RowState<'a> {
    RowState {
        installed: r.installed,
        cached: r.cached,
        upgradeable: r.upgradeable,
        replaceable: r.replaceable,
        compatible: r.compatible,
        newly_compatible: r.newly_compatible,
        labels: names,
    }
}

/// True if the row passes the list filter and the search.
pub fn row_visible(r: &Row, filter: &ListFilter, search: &ModSearch, li: &LabelInfo) -> bool {
    let names: Vec<&str> = r.labels.iter().filter_map(|&i| li.names.get(usize::from(i)).map(String::as_str)).collect();
    // Rows in a hiding label stay out unless the filter or search names that label.
    for &i in &r.labels {
        let i = usize::from(i);
        if li.hide.get(i).copied().unwrap_or(false) {
            let named = *filter == ListFilter::Label(i)
                || li.names.get(i).is_some_and(|n| search.mentions_label(n));
            if !named {
                return false;
            }
        }
    }
    let st = row_state(r, &names);
    let pass = match filter {
        ListFilter::Basic(f) => f.matches(&st),
        ListFilter::Label(i) => r.labels.iter().any(|&l| usize::from(l) == *i),
        ListFilter::Tag(t) => r.module.tags.iter().any(|x| x == t),
        ListFilter::Untagged => r.module.tags.is_empty(),
    };
    pass && search.matches(&r.module, &st)
}

/// Indices of the visible rows (in model order).
pub fn filter_rows(model: &RowModel, filter: &ListFilter, search: &ModSearch, li: &LabelInfo) -> Vec<usize> {
    model
        .rows
        .iter()
        .enumerate()
        .filter(|(_, r)| row_visible(r, filter, search, li))
        .map(|(i, _)| i)
        .collect()
}

/// Sort visible indices by a column. Ties fall back to the name.
pub fn sort_rows(rows: &[Row], view: &mut [usize], col: Col, asc: bool, pending: &Pending) {
    use std::cmp::Ordering;
    let key = |i: &usize| rows.get(*i);
    view.sort_by(|a, b| {
        let (Some(x), Some(y)) = (key(a), key(b)) else { return Ordering::Equal };
        let o = match col {
            Col::Installed => pending.install_checked(x).cmp(&pending.install_checked(y)),
            Col::Update => (x.upgradeable, pending.upgrade.contains(&x.id))
                .cmp(&(y.upgradeable, pending.upgrade.contains(&y.id))),
            Col::Replace => (x.replaceable, pending.replace.contains(&x.id))
                .cmp(&(y.replaceable, pending.replace.contains(&y.id))),
            Col::Name => Ordering::Equal,
            Col::Author => x.author.to_lowercase().cmp(&y.author.to_lowercase()),
            Col::InstalledVersion => x.installed_key.cmp(&y.installed_key),
            Col::Latest => {
                x.latest.as_ref().map(|m| &m.version).cmp(&y.latest.as_ref().map(|m| &m.version))
            }
            Col::MaxGame => x.max_game_key.cmp(&y.max_game_key),
            Col::DownloadSize => x.download_size.cmp(&y.download_size),
            Col::ReleaseDate => x.release_date.cmp(&y.release_date),
            Col::InstallDate => x.install_date.cmp(&y.install_date),
            Col::Downloads => x.downloads.cmp(&y.downloads),
        };
        let o = if asc { o } else { o.reverse() };
        o.then_with(|| x.sort_name.cmp(&y.sort_name)).then_with(|| x.id.cmp(&y.id))
    });
    if col == Col::Name && !asc {
        view.reverse();
    }
}

// ---------------------------------------------------------------- pending changes

/// Changes the user ticked in the list, not yet planned.
#[derive(Debug, Clone, Default)]
pub struct Pending {
    /// id -> exact version (None = latest compatible).
    pub install: BTreeMap<String, Option<ModuleVersion>>,
    pub remove: BTreeSet<String>,
    pub upgrade: BTreeSet<String>,
    pub replace: BTreeSet<String>,
    /// Extra requests from .ckan files: (display text, request).
    pub extra: Vec<(String, InstallRequest)>,
    /// Provider choices the user made (virtual name -> identifier). Not counted as changes.
    pub choices: HashMap<String, String>,
}

/// Accent state of a row for coloring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    None,
    Install,
    Remove,
    Upgrade,
    Replace,
}

impl Pending {
    pub fn len(&self) -> usize {
        self.install.len() + self.remove.len() + self.upgrade.len() + self.replace.len() + self.extra.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn clear(&mut self) {
        *self = Pending::default();
    }

    /// Effective value of the Installed checkbox.
    pub fn install_checked(&self, r: &Row) -> bool {
        if r.installed { !self.remove.contains(&r.id) } else { self.install.contains_key(&r.id) }
    }

    pub fn mark(&self, r: &Row) -> Mark {
        if self.remove.contains(&r.id) {
            Mark::Remove
        } else if self.install.contains_key(&r.id) {
            if r.installed { Mark::Upgrade } else { Mark::Install }
        } else if self.upgrade.contains(&r.id) {
            Mark::Upgrade
        } else if self.replace.contains(&r.id) {
            Mark::Replace
        } else {
            Mark::None
        }
    }

    /// Tick or untick the Installed box.
    pub fn set_installed(&mut self, r: &Row, want: bool) {
        if !r.can_toggle_install() {
            return;
        }
        if r.installed {
            if want {
                self.remove.remove(&r.id);
            } else {
                self.remove.insert(r.id.clone());
                self.upgrade.remove(&r.id);
                self.replace.remove(&r.id);
                self.install.remove(&r.id);
            }
        } else if want {
            self.install.insert(r.id.clone(), None);
        } else {
            self.install.remove(&r.id);
        }
    }

    pub fn set_upgrade(&mut self, r: &Row, want: bool) {
        if want && r.upgradeable {
            self.upgrade.insert(r.id.clone());
            self.remove.remove(&r.id);
            self.replace.remove(&r.id);
        } else {
            self.upgrade.remove(&r.id);
        }
    }

    pub fn set_replace(&mut self, r: &Row, want: bool) {
        if want && r.replaceable {
            self.replace.insert(r.id.clone());
            self.remove.remove(&r.id);
            self.upgrade.remove(&r.id);
        } else {
            self.replace.remove(&r.id);
        }
    }

    /// Install a specific version (Versions tab).
    pub fn install_version(&mut self, r: &Row, v: &ModuleVersion) {
        if r.kind != RowKind::Module {
            return;
        }
        if r.installed_key.as_ref() == Some(v) {
            self.install.remove(&r.id);
            return;
        }
        self.remove.remove(&r.id);
        self.upgrade.remove(&r.id);
        self.install.insert(r.id.clone(), Some(v.clone()));
    }

    /// "Update all": tick every upgradeable row (held mods have no update). Returns the count.
    pub fn mark_all_upgrades(&mut self, rows: &[Row]) -> usize {
        let mut n = 0;
        for r in rows.iter().filter(|r| r.upgradeable && !self.remove.contains(&r.id)) {
            if self.upgrade.insert(r.id.clone()) {
                n += 1;
            }
        }
        n
    }

    /// Drop entries that no longer make sense after the rows were rebuilt.
    pub fn retain_valid(&mut self, model: &RowModel) {
        let row = |id: &str| model.find(id).and_then(|i| model.row(i));
        self.install.retain(|id, v| row(id).is_some_and(|r| !r.installed || v.is_some()));
        self.remove.retain(|id| row(id).is_some_and(|r| r.installed));
        self.upgrade.retain(|id| row(id).is_some_and(|r| r.upgradeable));
        self.replace.retain(|id| row(id).is_some_and(|r| r.replaceable));
    }

    pub fn to_request(&self) -> Request {
        let mut install: Vec<InstallRequest> = self
            .install
            .iter()
            .map(|(id, v)| match v {
                Some(v) => InstallRequest::Exact(id.clone(), v.clone()),
                None => InstallRequest::Latest(id.clone()),
            })
            .collect();
        install.extend(self.extra.iter().map(|(_, r)| r.clone()));
        Request {
            install,
            remove: self.remove.iter().cloned().collect(),
            upgrade: self.upgrade.iter().cloned().collect(),
            replace: self.replace.iter().cloned().collect(),
            choices: self.choices.clone(),
            ..Request::default()
        }
    }

    /// Unplanned list for the Changes tab: (kind, id, detail).
    pub fn summary(&self) -> Vec<(ChangeKind, String, String)> {
        let mut v = Vec::new();
        for (id, ver) in &self.install {
            v.push((ChangeKind::Install, id.clone(), ver.as_ref().map(|x| x.to_string()).unwrap_or_else(|| "latest".into())));
        }
        for (label, _) in &self.extra {
            v.push((ChangeKind::Install, label.clone(), "from .ckan file".into()));
        }
        for id in &self.remove {
            v.push((ChangeKind::Remove, id.clone(), String::new()));
        }
        for id in &self.upgrade {
            v.push((ChangeKind::Upgrade, id.clone(), String::new()));
        }
        for id in &self.replace {
            v.push((ChangeKind::Replace, id.clone(), String::new()));
        }
        v
    }
}

pub fn kind_text(k: ChangeKind) -> &'static str {
    match k {
        ChangeKind::Install => "Install",
        ChangeKind::Remove => "Remove",
        ChangeKind::Upgrade => "Update",
        ChangeKind::Replace => "Replace",
    }
}

/// One line of a planned change for display: (kind, name, version text, reasons).
pub fn change_lines(cs: &Changeset, hide_epochs: bool, hide_v: bool) -> Vec<(ChangeKind, String, String, String)> {
    cs.changes
        .iter()
        .map(|c| {
            let new = c.module.version.display(hide_epochs, hide_v);
            let ver = match (&c.kind, &c.old_version) {
                (ChangeKind::Upgrade, Some(old)) => format!("{} -> {new}", old.display(hide_epochs, hide_v)),
                _ => new,
            };
            let mut name = format!("{} ({})", c.module.display_name(), c.module.identifier);
            if let Some(r) = &c.replaces {
                name.push_str(&format!(", replaces {r}"));
            }
            if c.auto_installed {
                name.push_str(" [auto]");
            }
            let reasons: Vec<String> = c.reasons.iter().map(|r| r.to_string()).collect();
            (c.kind, name, ver, reasons.join("; "))
        })
        .collect()
}

/// Add ticked optional mods to a request as exact installs.
pub fn add_optional(req: &mut Request, picks: &[Arc<Module>]) {
    for m in picks {
        let present = req.install.iter().any(|r| match r {
            InstallRequest::Latest(id) | InstallRequest::Exact(id, _) => *id == m.identifier,
            InstallRequest::Module(x) => x.identifier == m.identifier,
        });
        if !present {
            req.install.push(InstallRequest::Exact(m.identifier.clone(), m.version.clone()));
        }
    }
}

/// Readable age: "12 min ago", "3 h ago", "2 days ago".
pub fn age_text(secs: u64) -> String {
    match secs {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", secs / 60),
        3600..=172_799 => format!("{} h ago", secs / 3600),
        _ => format!("{} days ago", secs / 86_400),
    }
}

/// Hours with one decimal, as CKAN shows play time.
pub fn hours_text(d: std::time::Duration) -> String {
    format!("{:.1} h", d.as_secs_f64() / 3600.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Label;
    use crate::registry::Registry;
    use crate::repo::{RepoData, RepoIndex, Repository};
    use crate::resolve::Context;

    fn module(id: &str, ver: &str, extra: &str) -> Module {
        let j = format!(
            r#"{{"spec_version":1,"identifier":"{id}","name":"{id} mod","abstract":"about {id}","author":"me",
            "version":"{ver}","download":"https://example.com/{id}-{ver}.zip","download_size":1000{extra}}}"#
        );
        Module::from_json(j.as_bytes()).expect("module")
    }

    fn index(mods: Vec<Module>) -> RepoIndex {
        RepoIndex::build(vec![(Repository::default_repo(), RepoData { modules: mods.into_iter().map(std::sync::Arc::new).collect(), ..Default::default() })])
    }

    struct World {
        index: RepoIndex,
        reg: Registry,
        crit: crate::meta::VersionCriteria,
        held: HashSet<String>,
    }

    fn world() -> World {
        let mods = vec![
            module("Alpha", "1.0", r#","ksp_version":"1.12","tags":["parts"]"#),
            module("Alpha", "2.0", r#","ksp_version":"1.12","tags":["parts"]"#),
            module("Beta", "1.0", r#","ksp_version":"1.12","depends":[{"name":"Alpha"}]"#),
            module("Gamma", "1.0", r#","ksp_version":"1.3""#),
            module("Delta", "0.5", r#","ksp_version":"1.12","tags":["physics"]"#),
        ];
        let index = index(mods);
        let mut reg = Registry::empty();
        let a1 = module("Alpha", "1.0", r#","ksp_version":"1.12","tags":["parts"]"#);
        reg.register(a1, vec!["GameData/Alpha/a.dll".into()], false).expect("register");
        let crit = crate::meta::VersionCriteria::new(crate::meta::GameVersion::parse("1.12.5"), &[]);
        World { index, reg, crit, held: HashSet::new() }
    }

    fn build(w: &World, labels: &LabelList) -> RowModel {
        let ctx = Context {
            index: &w.index,
            registry: &w.reg,
            criteria: &w.crit,
            held: &w.held,
            stability: crate::meta::ReleaseStatus::Stable,
        };
        let cache = CacheIndex::default();
        build_rows(&BuildInput {
            ctx,
            cache: &cache,
            labels,
            instance: "test",
            prev_compatible: None,
            hide_epochs: true,
            hide_v: false,
        })
    }

    fn labels() -> LabelList {
        let mut l = LabelList::defaults();
        l.labels.push(Label { name: "Mine".into(), ..Label::default() });
        // Hidden (index 1) holds Delta, Mine (index 3) holds Beta.
        l.toggle(1, "Delta");
        l.toggle(3, "Beta");
        l
    }

    fn r<'a>(m: &'a RowModel, id: &str) -> &'a Row {
        m.find(id).and_then(|i| m.row(i)).expect("row")
    }

    #[test]
    fn rows_have_flags() {
        let w = world();
        let m = build(&w, &labels());
        assert_eq!(m.rows.len(), 4);
        let a = r(&m, "Alpha");
        assert!(a.installed && a.upgradeable && a.compatible);
        assert_eq!(a.installed_version, "1.0");
        assert_eq!(a.latest_version, "2.0");
        assert_eq!(a.update_to.as_ref().map(|u| u.version.to_string()), Some("2.0".into()));
        let g = r(&m, "Gamma");
        assert!(!g.installed && !g.compatible && g.latest.is_none());
        assert!(!g.can_toggle_install());
        assert_eq!(r(&m, "Beta").labels, vec![3]);
        assert_eq!(m.counts, Counts { installed: 1, upgradeable: 1, compatible: 3, total: 4 });
        assert_eq!(m.tags, vec![("parts".to_string(), 1), ("physics".to_string(), 1)]);
        assert_eq!(m.untagged, 2);
    }

    #[test]
    fn filters_and_hidden_labels() {
        let w = world();
        let l = labels();
        let m = build(&w, &l);
        let li = LabelInfo::of(&l);
        let ids = |f: &ListFilter, s: &str| -> Vec<String> {
            let mut v: Vec<String> = filter_rows(&m, f, &ModSearch::parse(s), &li)
                .into_iter()
                .filter_map(|i| m.row(i).map(|r| r.id.clone()))
                .collect();
            v.sort();
            v
        };
        // Delta is in the Hidden label: out of the default view.
        assert_eq!(ids(&ListFilter::default(), ""), vec!["Alpha", "Beta"]);
        assert_eq!(ids(&ListFilter::Basic(Filter::Incompatible), ""), vec!["Gamma"]);
        assert_eq!(ids(&ListFilter::Basic(Filter::Installed), ""), vec!["Alpha"]);
        assert_eq!(ids(&ListFilter::Basic(Filter::Upgradeable), ""), vec!["Alpha"]);
        // Naming the hidden label (filter or search) shows it.
        assert_eq!(ids(&ListFilter::Label(1), ""), vec!["Delta"]);
        assert_eq!(ids(&ListFilter::Basic(Filter::All), "label:Hidden"), vec!["Delta"]);
        assert_eq!(ids(&ListFilter::Tag("parts".into()), ""), vec!["Alpha"]);
        assert_eq!(ids(&ListFilter::Untagged, ""), vec!["Beta", "Gamma"]);
        assert_eq!(ids(&ListFilter::Basic(Filter::All), "bet"), vec!["Beta"]);
        assert_eq!(ids(&ListFilter::Basic(Filter::All), "label:Mine"), vec!["Beta"]);
        assert_eq!(ids(&ListFilter::Basic(Filter::All), "is:installed"), vec!["Alpha"]);
    }

    #[test]
    fn sorting() {
        let w = world();
        let m = build(&w, &LabelList::default());
        let p = Pending::default();
        let mut v: Vec<usize> = (0..m.rows.len()).collect();
        let names = |v: &[usize]| -> Vec<String> { v.iter().filter_map(|&i| m.row(i)).map(|r| r.id.clone()).collect() };
        sort_rows(&m.rows, &mut v, Col::Name, true, &p);
        assert_eq!(names(&v), vec!["Alpha", "Beta", "Delta", "Gamma"]);
        sort_rows(&m.rows, &mut v, Col::Name, false, &p);
        assert_eq!(names(&v), vec!["Gamma", "Delta", "Beta", "Alpha"]);
        sort_rows(&m.rows, &mut v, Col::Installed, false, &p);
        assert_eq!(names(&v).first().map(String::as_str), Some("Alpha"));
        sort_rows(&m.rows, &mut v, Col::MaxGame, true, &p);
        assert_eq!(names(&v).first().map(String::as_str), Some("Gamma"));
    }

    #[test]
    fn pending_bookkeeping() {
        let w = world();
        let m = build(&w, &LabelList::default());
        let mut p = Pending::default();
        let a = r(&m, "Alpha").clone();
        let b = r(&m, "Beta").clone();
        let g = r(&m, "Gamma").clone();
        p.set_installed(&b, true);
        p.set_installed(&g, true); // incompatible: ignored
        assert_eq!(p.len(), 1);
        assert_eq!(p.mark(&b), Mark::Install);
        p.set_upgrade(&a, true);
        assert_eq!(p.mark(&a), Mark::Upgrade);
        p.set_installed(&a, false);
        assert_eq!(p.mark(&a), Mark::Remove);
        assert!(!p.upgrade.contains("Alpha"));
        assert!(!p.install_checked(&a));
        p.set_installed(&a, true);
        assert_eq!(p.mark(&a), Mark::None);
        assert_eq!(p.mark_all_upgrades(&m.rows), 1);
        let req = p.to_request();
        assert!(matches!(req.install.as_slice(), [InstallRequest::Latest(id)] if id == "Beta"));
        assert_eq!(req.upgrade, vec!["Alpha"]);
        p.install_version(&a, &ModuleVersion::new("1.0"));
        assert!(!p.install.contains_key("Alpha"));
        p.install_version(&b, &ModuleVersion::new("1.0"));
        assert!(matches!(p.to_request().install.as_slice(), [InstallRequest::Exact(id, _)] if id == "Beta"));
        assert_eq!(p.summary().len(), 2);
        // A rebuild where Alpha is no longer upgradeable drops the stale upgrade.
        let mut m2 = build(&w, &LabelList::default());
        if let Some(row) = m2.find("Alpha").and_then(|i| m2.rows.get_mut(i)) {
            row.upgradeable = false;
        }
        p.retain_valid(&m2);
        assert!(p.upgrade.is_empty());
        p.clear();
        assert!(p.is_empty());
    }

    #[test]
    fn plan_lines_and_optional() {
        let w = world();
        let ctx = Context {
            index: &w.index,
            registry: &w.reg,
            criteria: &w.crit,
            held: &w.held,
            stability: crate::meta::ReleaseStatus::Stable,
        };
        let mut req = Request { install: vec![InstallRequest::Latest("Beta".into())], ..Request::default() };
        let cs = crate::resolve::plan(&ctx, &req, &crate::resolve::ResolverOptions::default()).expect("plan");
        let lines = change_lines(&cs, true, false);
        assert!(lines.iter().any(|(k, n, _, _)| *k == ChangeKind::Install && n.contains("Beta")));
        let d = w.index.versions("Delta").first().cloned().expect("delta");
        add_optional(&mut req, &[d.clone(), d]);
        assert_eq!(req.install.len(), 2);
    }

    #[test]
    fn text_helpers() {
        assert_eq!(age_text(30), "just now");
        assert_eq!(age_text(7200), "2 h ago");
        assert_eq!(hours_text(std::time::Duration::from_secs(5400)), "1.5 h");
    }

    #[test]
    fn relabel_in_place() {
        let w = world();
        let mut l = LabelList::defaults();
        let mut m = build(&w, &l);
        assert!(r(&m, "Beta").labels.is_empty());
        l.toggle(0, "Beta");
        relabel(&mut m, &l, "test");
        assert_eq!(r(&m, "Beta").labels, vec![0]);
    }
}
