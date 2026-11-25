//! Output lock for the real CKAN-meta data: a hash of every written form of every module.
//! The first run writes the golden file; later runs compare with it line by line.
//!
//! Run: HANGAR_TEST_META=<master.tar.gz> HANGAR_GOLDEN=<file> cargo test --release golden -- --ignored

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Read;

use sha2::Digest;

use super::*;
use crate::registry::{InstalledModule, Registry};
use crate::repo::RepoIndex;

fn h(bytes: &[u8]) -> String {
    let d = sha2::Sha256::digest(bytes);
    d.iter().take(12).fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

fn golden_lines(meta: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    // 1. Every .ckan file on its own (from_json + to_json_pretty).
    let f = std::fs::File::open(meta).unwrap();
    let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(std::io::BufReader::new(f)));
    let mut buf = Vec::new();
    for e in ar.entries().unwrap() {
        let mut e = e.unwrap();
        let name = e.path().unwrap().to_string_lossy().into_owned();
        if !name.ends_with(".ckan") {
            continue;
        }
        buf.clear();
        e.read_to_end(&mut buf).unwrap();
        match Module::from_json(&buf) {
            Ok(m) => out.push(format!("F {name} {}", h(m.to_json_pretty().as_bytes()))),
            Err(e) => out.push(format!("F {name} ERR {e}")),
        }
    }
    // 2. The index path (parse_archive + RepoIndex::build), as the CLI and GUI load it.
    let data = crate::repo::parse_archive(meta).unwrap();
    out.push(format!("S skipped {} counts {} builds {}", data.skipped, data.download_counts.len(), data.builds.len()));
    let idx = RepoIndex::build(vec![(crate::repo::Repository::default_repo(), data)]);
    let ids: BTreeMap<&String, _> = idx.modules.iter().collect();
    for (id, list) in &ids {
        let all: String = list.iter().map(|m| m.to_json_pretty()).collect();
        out.push(format!("I {id} {} {}", list.len(), h(all.as_bytes())));
    }
    let provs: BTreeMap<&String, &Vec<String>> = idx.providers.iter().collect();
    out.push(format!("P providers {}", h(format!("{provs:?}").as_bytes())));
    // 3. registry.json with real modules (every 5th identifier, newest version), and a reload.
    let mut reg = Registry::empty();
    for (i, (id, list)) in ids.iter().enumerate().filter(|(i, _)| i % 5 == 0) {
        let Some(m) = list.first() else { continue };
        let files = vec![format!("GameData/{id}/a{i}.dll"), format!("GameData/{id}")];
        reg.register((**m).clone(), files, i % 2 == 0).unwrap();
    }
    for im in reg.installed_modules.values_mut() {
        im.install_time = "2026-11-24T00:00:00Z".into();
    }
    let json = reg.to_ckan_json().unwrap();
    out.push(format!("R registry {} {}", reg.installed_modules.len(), h(&json)));
    let dir = std::env::temp_dir().join(format!("hangar-golden-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("CKAN")).unwrap();
    std::fs::write(dir.join("CKAN").join("registry.json"), &json).unwrap();
    let back = Registry::load(&dir.join("CKAN")).unwrap();
    let json2 = back.to_ckan_json().unwrap();
    out.push(format!("R reload {}", h(&json2)));
    let _ = std::fs::remove_dir_all(&dir);
    let one: Vec<&InstalledModule> = reg.installed_modules.values().take(50).collect();
    for im in one {
        out.push(format!("M {} {}", im.source_module.identifier, h(im.source_module.to_json_pretty().as_bytes())));
    }
    // 4. Modpack (.ckan export) and text exports of that registry.
    let now = crate::util::Utc { year: 2026, month: 11, day: 24, hour: 18, min: 30, sec: 12, nanos: 0 };
    let mut opts = crate::modlist::ModpackOptions {
        identifier: "installed-golden".into(),
        name: "installed-golden".into(),
        abstract_: "golden".into(),
        author: vec!["me".into()],
        version: "2026.11.24".into(),
        with_versions: true,
        as_recommends: false,
        include_optional: true,
    };
    let crit = VersionCriteria { versions: vec![GameVersion::parse("1.12.5.3190").unwrap()] };
    let pack = crate::modlist::modpack_with(&reg, &idx, &opts, &crit, &now);
    out.push(format!("X modpack {}", h(pack.to_json_pretty().as_bytes())));
    opts.as_recommends = true;
    opts.with_versions = false;
    let pack = crate::modlist::modpack_with(&reg, &idx, &opts, &crit, &now);
    out.push(format!("X modpack2 {}", h(pack.to_json_pretty().as_bytes())));
    for fmt in ["text", "markdown", "bbcode", "csv", "tsv"] {
        let f = crate::modlist::ExportFormat::from_name(fmt).unwrap();
        out.push(format!("X {fmt} {}", h(crate::modlist::export_text(f, &reg).as_bytes())));
    }
    out
}

#[test]
#[ignore]
fn golden_real_meta_output() {
    let (Some(meta), Some(gold)) = (std::env::var_os("HANGAR_TEST_META"), std::env::var_os("HANGAR_GOLDEN")) else {
        eprintln!("HANGAR_TEST_META or HANGAR_GOLDEN not set, skipping");
        return;
    };
    let lines = golden_lines(std::path::Path::new(&meta));
    let text = lines.join("\n") + "\n";
    let gold = std::path::Path::new(&gold);
    let Ok(want) = std::fs::read_to_string(gold) else {
        std::fs::write(gold, &text).unwrap();
        eprintln!("golden file written: {} lines", lines.len());
        return;
    };
    let want: Vec<&str> = want.lines().collect();
    let bad: Vec<String> = want
        .iter()
        .zip(lines.iter())
        .filter(|(a, b)| **a != b.as_str())
        .take(10)
        .map(|(a, b)| format!("want {a}\n got {b}"))
        .collect();
    assert!(bad.is_empty() && want.len() == lines.len(), "{} vs {} lines\n{}", want.len(), lines.len(), bad.join("\n"));
    eprintln!("golden output identical: {} lines", lines.len());
}
