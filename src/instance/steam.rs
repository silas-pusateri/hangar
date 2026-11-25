//! Steam library discovery from Valve KeyValues text files (libraryfolders.vdf, *.acf).

use std::path::{Path, PathBuf};

/// Quoted string tokens of a KeyValues text file (braces and comments are skipped).
fn tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                let mut s = String::new();
                while let Some(c) = chars.next() {
                    match c {
                        '\\' => {
                            if let Some(n) = chars.next() {
                                match n {
                                    'n' => s.push('\n'),
                                    't' => s.push('\t'),
                                    o => s.push(o),
                                }
                            }
                        }
                        '"' => break,
                        o => s.push(o),
                    }
                }
                out.push(s);
            }
            '/' if chars.peek() == Some(&'/') => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Values of every `key` (case-insensitive) in a KeyValues text file.
pub fn values_of(text: &str, key: &str) -> Vec<String> {
    let t = tokens(text);
    t.windows(2).filter(|w| w.first().is_some_and(|k| k.eq_ignore_ascii_case(key))).filter_map(|w| w.get(1).cloned()).collect()
}

/// Game dirs listed by Steam under `steam_root` (root library + libraryfolders.vdf paths).
pub fn steam_game_dirs(steam_root: &Path) -> Vec<PathBuf> {
    let mut libs: Vec<PathBuf> = vec![steam_root.to_path_buf()];
    for vdf in ["config/libraryfolders.vdf", "steamapps/libraryfolders.vdf", "SteamApps/libraryfolders.vdf"] {
        if let Ok(text) = std::fs::read_to_string(steam_root.join(vdf)) {
            for p in values_of(&text, "path") {
                let p = PathBuf::from(p);
                if !libs.contains(&p) {
                    libs.push(p);
                }
            }
        }
    }
    let mut out = Vec::new();
    // macOS file systems are usually case-insensitive: steamapps and SteamApps can be one dir.
    let mut seen_dirs: Vec<PathBuf> = Vec::new();
    for lib in libs {
        for apps in ["steamapps", "SteamApps"] {
            let dir = lib.join(apps);
            let Ok(canon) = std::fs::canonicalize(&dir) else { continue };
            if seen_dirs.contains(&canon) {
                continue;
            }
            seen_dirs.push(canon);
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) != Some("acf") {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(&p) else { continue };
                for inst in values_of(&text, "installdir") {
                    let g = dir.join("common").join(inst);
                    if !out.contains(&g) {
                        out.push(g);
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vdf_paths() {
        let vdf = "\"libraryfolders\"\n{\n\t\"0\"\n\t{\n\t\t\"path\"\t\t\"/Users/me/Library/Application Support/Steam\"\n\t\t\"label\"\t\t\"\"\n\t\t\"apps\"\n\t\t{\n\t\t\t\"220200\"\t\t\"0\"\n\t\t}\n\t}\n\t\"1\"\n\t{\n\t\t\"path\"\t\t\"/Volumes/Big\\\\Lib\"\n\t}\n}\n";
        assert_eq!(values_of(vdf, "path"), vec!["/Users/me/Library/Application Support/Steam", "/Volumes/Big\\Lib"]);
        let acf = "\"AppState\"\n{\n\t\"appid\"\t\t\"220200\"\n\t\"installdir\"\t\t\"Kerbal Space Program\"\n}\n";
        assert_eq!(values_of(acf, "installdir"), vec!["Kerbal Space Program"]);
    }
}
