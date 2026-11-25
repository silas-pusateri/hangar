//! Text export writers (Core/Exporters/*.cs): plain text, Markdown, BBCode, CSV and TSV.

use crate::meta::{GameVersion, Module};
use crate::registry::Registry;

/// CSV/TSV header, in CKAN's exact order.
pub const DSV_HEADER: [&str; 20] = [
    "identifier",
    "version",
    "name",
    "abstract",
    "description",
    "author",
    "kind",
    "download",
    "download_size",
    "ksp_version",
    "ksp_version_min",
    "ksp_version_max",
    "license",
    "release_status",
    "repository",
    "homepage",
    "bugtracker",
    "discussions",
    "spacedock",
    "curse",
];

/// All installed modules (incl. auto-installed and DLC), sorted by name like CKAN's
/// culture-aware `OrderBy(name)`: case-insensitive first; on a tie lowercase comes before
/// uppercase ("alpha" < "Alpha"), then the identifier keeps the order stable.
pub fn sorted_rows(reg: &Registry) -> Vec<&Module> {
    let mut v: Vec<&Module> = reg.installed_modules.values().map(|im| &im.source_module).collect();
    v.sort_by_cached_key(|m| {
        let swapped: String = m
            .name
            .chars()
            .map(|c| if c.is_uppercase() { c.to_ascii_lowercase() } else { c.to_ascii_uppercase() })
            .collect();
        (m.name.to_lowercase(), swapped)
    });
    v
}

/// `{name} ({identifier} {version})` per line.
pub fn plain(rows: &[&Module]) -> String {
    let mut s = String::new();
    for m in rows {
        s.push_str(&format!("{} ({} {})\n", m.name, m.identifier, m.version));
    }
    s
}

/// ``- **{name}** `{identifier} {version}` `` per line.
pub fn markdown(rows: &[&Module]) -> String {
    let mut s = String::new();
    for m in rows {
        s.push_str(&format!("- **{}** `{} {}`\n", m.name, m.identifier, m.version));
    }
    s
}

/// `[LIST]`, one `[*][B]{name}[/B] ({identifier} {version})` per mod, `[/LIST]`.
pub fn bbcode(rows: &[&Module]) -> String {
    let mut s = String::from("[LIST]\n");
    for m in rows {
        s.push_str(&format!("[*][B]{}[/B] ({} {})\n", m.name, m.identifier, m.version));
    }
    s.push_str("[/LIST]\n");
    s
}

/// Delimiter-separated values (`,` or `\t`).
pub fn dsv(rows: &[&Module], delim: char) -> String {
    let mut s = DSV_HEADER.join(&delim.to_string());
    s.push('\n');
    let gv = |v: &Option<GameVersion>| v.map(|g| g.to_string()).unwrap_or_default();
    for m in rows {
        let res = |k: &str| uri_field(m.resources.get(k), delim);
        let fields: [String; 20] = [
            quote(&m.identifier, delim),
            quote(m.version.as_str(), delim),
            quote(&m.name, delim),
            quote(&m.abstract_, delim),
            quote(m.description.as_deref().unwrap_or(""), delim),
            quote(&m.author.join(";"), delim),
            quote(m.kind.as_str(), delim),
            uri_field(m.download.first().map(String::as_str), delim),
            m.download_size.to_string(),
            gv(&m.ksp_version),
            gv(&m.ksp_version_min),
            gv(&m.ksp_version_max),
            quote(&m.license.join(";"), delim),
            m.release_status.as_str().to_string(),
            res("repository"),
            res("homepage"),
            res("bugtracker"),
            res("discussions"),
            res("spacedock"),
            res("curse"),
        ];
        s.push_str(&fields.join(&delim.to_string()));
        s.push('\n');
    }
    s
}

/// CKAN QuoteIfNecessary quotes only values that contain the delimiter and never escapes.
/// Deliberate fix (RFC 4180): also quote values that contain `"`, CR or LF, and double
/// embedded quotes, so a multi-line description no longer breaks the row.
pub fn quote(v: &str, delim: char) -> String {
    if v.contains(delim) || v.contains('"') || v.contains('\n') || v.contains('\r') {
        format!("\"{}\"", v.replace('"', "\"\""))
    } else {
        v.to_string()
    }
}

fn uri_field(u: Option<&str>, delim: char) -> String {
    u.and_then(normalize_uri).map(|s| quote(&s, delim)).unwrap_or_default()
}

/// Net.NormalizeUri: space -> `+`, percent-escape `" < > ^ ` ( ) { | } [ ]`, prepend
/// `http://` when there is no http(s) scheme, None when the result is not a well-formed URI.
pub fn normalize_uri(uri: &str) -> Option<String> {
    let mut s = String::with_capacity(uri.len() + 8);
    for c in uri.chars() {
        match c {
            ' ' => s.push('+'),
            '"' | '<' | '>' | '^' | '`' | '(' | ')' | '{' | '|' | '}' | '[' | ']' => {
                s.push_str(&format!("%{:02X}", c as u32));
            }
            _ => s.push(c),
        }
    }
    let lower = s.get(..8).unwrap_or(&s).to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        s.insert_str(0, "http://");
    }
    well_formed(&s).then_some(s)
}

/// Approximation of .NET Uri.IsWellFormedUriString(Absolute) for http(s) URLs: a host is
/// present, no whitespace, control chars or backslashes, and every `%` starts a valid escape.
fn well_formed(s: &str) -> bool {
    let rest = match s.find("://") {
        Some(i) => s.get(i + 3..).unwrap_or(""),
        None => return false,
    };
    let host_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = rest.get(..host_end).unwrap_or("");
    let host = authority.rsplit('@').next().unwrap_or("");
    if host.is_empty() || host.starts_with(':') {
        return false;
    }
    if s.chars().any(|c| c.is_whitespace() || c.is_control() || c == '\\') {
        return false;
    }
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let ok = b.get(i + 1).is_some_and(u8::is_ascii_hexdigit) && b.get(i + 2).is_some_and(u8::is_ascii_hexdigit);
            if !ok {
                return false;
            }
        }
        i += 1;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kop() -> Module {
        Module::from_json(
            br#"{"identifier":"Kopernicus","name":"Kopernicus Planetary System Modifier","abstract":"Planets",
            "version":"release-1.12.1-215","download":"https://x.test/k.zip","license":"LGPL-3.0",
            "author":["Thomas P.","NathanKell"],"ksp_version":"1.12.5"}"#,
        )
        .unwrap()
    }

    fn mm() -> Module {
        Module::from_json(
            br#"{"identifier":"ModuleManager","name":"Module Manager","abstract":"Edit configs, no conflict",
            "version":"4.2.3","download":"https://x.test/MM-4.2.3.zip","download_size":30000,
            "license":"CC-BY-SA","author":["sarbian","Blowfish"],"ksp_version_min":"1.8.0",
            "ksp_version_max":"1.12.99","resources":{"repository":"https://github.com/sarbian/ModuleManager"}}"#,
        )
        .unwrap()
    }

    #[test]
    fn text_formats_golden() {
        let (k, m) = (kop(), mm());
        let rows = vec![&k, &m];
        assert_eq!(
            plain(&rows),
            "Kopernicus Planetary System Modifier (Kopernicus release-1.12.1-215)\nModule Manager (ModuleManager 4.2.3)\n"
        );
        assert_eq!(
            markdown(&rows),
            "- **Kopernicus Planetary System Modifier** `Kopernicus release-1.12.1-215`\n- **Module Manager** `ModuleManager 4.2.3`\n"
        );
        assert_eq!(
            bbcode(&rows),
            "[LIST]\n[*][B]Kopernicus Planetary System Modifier[/B] (Kopernicus release-1.12.1-215)\n[*][B]Module Manager[/B] (ModuleManager 4.2.3)\n[/LIST]\n"
        );
    }

    #[test]
    fn csv_tsv_golden() {
        let m = mm();
        let rows = vec![&m];
        let csv = dsv(&rows, ',');
        let mut lines = csv.lines();
        assert_eq!(
            lines.next().unwrap(),
            "identifier,version,name,abstract,description,author,kind,download,download_size,ksp_version,ksp_version_min,ksp_version_max,license,release_status,repository,homepage,bugtracker,discussions,spacedock,curse"
        );
        assert_eq!(
            lines.next().unwrap(),
            "ModuleManager,4.2.3,Module Manager,\"Edit configs, no conflict\",,sarbian;Blowfish,package,https://x.test/MM-4.2.3.zip,30000,,1.8.0,1.12.99,CC-BY-SA,stable,https://github.com/sarbian/ModuleManager,,,,,"
        );
        let tsv = dsv(&rows, '\t');
        assert_eq!(
            tsv.lines().nth(1).unwrap(),
            "ModuleManager\t4.2.3\tModule Manager\tEdit configs, no conflict\t\tsarbian;Blowfish\tpackage\thttps://x.test/MM-4.2.3.zip\t30000\t\t1.8.0\t1.12.99\tCC-BY-SA\tstable\thttps://github.com/sarbian/ModuleManager\t\t\t\t\t"
        );
    }

    #[test]
    fn rfc4180_quoting() {
        assert_eq!(quote("a \"b\"", ','), "\"a \"\"b\"\"\"");
        assert_eq!(quote("line1\nline2", '\t'), "\"line1\nline2\"");
        assert_eq!(quote("a,b", '\t'), "a,b");
        assert_eq!(quote("a\tb", '\t'), "\"a\tb\"");
    }

    #[test]
    fn uri_normalization() {
        assert_eq!(normalize_uri("https://x.test/a b[1].zip").as_deref(), Some("https://x.test/a+b%5B1%5D.zip"));
        assert_eq!(normalize_uri("forum.kerbalspaceprogram.com/t/1").as_deref(), Some("http://forum.kerbalspaceprogram.com/t/1"));
        assert_eq!(normalize_uri("HTTPS://X.test/").as_deref(), Some("HTTPS://X.test/"));
        assert_eq!(normalize_uri("https:///nohost"), None);
        assert_eq!(normalize_uri("https://x.test/%zz"), None);
        assert_eq!(normalize_uri("https://x.test/a\"b").as_deref(), Some("https://x.test/a%22b"));
    }
}
