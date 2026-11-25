//! Parse every .ckan file of the real CKAN-meta tarball (HANGAR_TEST_META) and check that the
//! only failures are ones CKAN also rejects. Prints parse time and a rough memory size.

use std::collections::BTreeMap;
use std::io::Read;
use std::time::{Duration, Instant};

use super::*;

fn s(x: &str) -> usize {
    x.len()
}
fn vs(v: &[String]) -> usize {
    v.iter().map(|x| 24 + x.len()).sum()
}
fn os<T: AsRef<str>>(o: &Option<T>) -> usize {
    o.as_ref().map_or(0, |x| x.as_ref().len())
}
fn mv(v: &ModuleVersion) -> usize {
    v.as_str().len()
}
fn rel(r: &Relationship) -> usize {
    std::mem::size_of::<Relationship>()
        + match r {
            Relationship::One(t) => target(t),
            Relationship::AnyOf { any_of, choice_help_text, .. } => any_of.iter().map(rel).sum::<usize>() + os(choice_help_text),
        }
}
fn target(t: &RelationshipTarget) -> usize {
    s(&t.name)
        + t.version.as_ref().map_or(0, mv)
        + t.min_version.as_ref().map_or(0, mv)
        + t.max_version.as_ref().map_or(0, mv)
        + os(&t.choice_help_text)
}

/// Rough heap bytes owned by a module (string bytes + element headers; ignores allocator slack).
fn heap(m: &Module) -> usize {
    let rels = [&m.depends, &m.recommends, &m.suggests, &m.supports, &m.conflicts];
    s(&m.identifier)
        + s(&m.name)
        + s(&m.abstract_)
        + os(&m.description)
        + os(&m.comment)
        + vs(&m.author)
        + mv(&m.version)
        + vs(&m.license)
        + m.resources.iter().map(|(k, v)| 64 + k.len() + v.len()).sum::<usize>()
        + vs(&m.tags)
        + vs(&m.localizations)
        + vs(&m.provides)
        + rels.iter().flat_map(|v| v.iter()).map(rel).sum::<usize>()
        + m.replaced_by.as_ref().map_or(0, |t| target(t))
        + m.install.iter().map(|i| {
            std::mem::size_of::<InstallStanza>()
                + os(&i.file)
                + os(&i.find)
                + os(&i.find_regexp)
                + s(&i.install_to)
                + os(&i.as_)
                + vs(&i.filter)
                + vs(&i.filter_regexp)
                + vs(&i.include_only)
                + vs(&i.include_only_regexp)
        }).sum::<usize>()
        + vs(&m.download)
        + m.download_hash.as_ref().map_or(0, |h| os(&h.sha1) + os(&h.sha256))
        + os(&m.download_content_type)
        + os(&m.release_date)
}

/// Map an error message to a short failure class.
fn class(e: &Error) -> String {
    let t = e.to_string();
    for k in [
        "unsupported spec_version",
        "bad version",
        "bad game version",
        "invalid license",
        "missing author",
        "package without download",
        "ksp_version together",
        "invalid identifier",
        "install stanza",
        "any_of combined",
        "bad release_status",
        "bad kind",
    ] {
        if t.contains(k) {
            return k.to_string();
        }
    }
    t
}

#[test]
fn parse_all_real_ckan_files() {
    let Some(path) = std::env::var_os("HANGAR_TEST_META") else {
        eprintln!("HANGAR_TEST_META not set, skipping");
        return;
    };
    let f = std::fs::File::open(&path).unwrap();
    let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(std::io::BufReader::new(f)));
    let mut ok: Vec<Module> = Vec::new();
    let mut fails: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut parse_time = Duration::ZERO;
    let mut json_bytes = 0usize;
    let t0 = Instant::now();
    let mut buf = Vec::new();
    for entry in ar.entries().unwrap() {
        let mut entry = entry.unwrap();
        let name = entry.path().unwrap().to_string_lossy().into_owned();
        if !name.ends_with(".ckan") {
            continue; // .frozen, .kerbalstuff, dirs, other files
        }
        buf.clear();
        entry.read_to_end(&mut buf).unwrap();
        json_bytes += buf.len();
        let t = Instant::now();
        let r = Module::from_json(&buf);
        parse_time += t.elapsed();
        match r {
            Ok(m) => ok.push(m),
            Err(e) => fails.entry(class(&e)).or_default().push(format!("{name}: {e}")),
        }
    }
    let total = ok.len() + fails.values().map(Vec::len).sum::<usize>();
    let heap_total: usize = ok.iter().map(heap).sum();
    let n = ok.len().max(1);
    eprintln!("files: {total}, ok: {}, failed: {}", ok.len(), total - ok.len());
    eprintln!("wall (gunzip+untar+parse): {:?}, parse only: {:?}, {:.1} us/module", t0.elapsed(), parse_time,
        parse_time.as_secs_f64() * 1e6 / total.max(1) as f64);
    eprintln!("json bytes: {} ({} avg), size_of::<Module>() = {}, heap ~{} avg, total ~{:.1} MiB",
        json_bytes, json_bytes / total.max(1), std::mem::size_of::<Module>(), heap_total / n,
        (heap_total + n * std::mem::size_of::<Module>()) as f64 / 1048576.0);
    for (k, v) in &fails {
        eprintln!("FAIL class {k:?}: {}", v.len());
        for x in v.iter().take(5) {
            eprintln!("    {x}");
        }
    }
    assert!(total > 30000, "expected the full CKAN-meta tarball");
    // Only failure classes CKAN also rejects.
    let allowed = ["unsupported spec_version", "bad game version", "bad version"];
    for k in fails.keys() {
        assert!(allowed.contains(&k.as_str()), "unexpected failure class {k}");
    }
}
