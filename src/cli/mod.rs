//! Command line interface (CKAN cmdline style). `hangar <command> ...`.

/// Print a line to stdout; a closed pipe is ignored (no panic).
macro_rules! out {
    () => { $crate::cli::term::write_out("\n") };
    ($($t:tt)*) => { $crate::cli::term::write_out(&format!("{}\n", format_args!($($t)*))) };
}

/// Print a line to stderr; write errors are ignored.
macro_rules! warn {
    ($($t:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stderr(), $($t)*);
    }};
}

mod admin;
mod args;
mod change;
mod fmt;
mod interrupt;
mod mods;
mod term;
#[cfg(test)]
mod tests;

use std::path::Path;

use crate::config::{Config, InstanceEntry};
use crate::ops::Session;

use args::Globals;

/// A command failure: the message goes to stderr, the exit code is 1.
#[derive(Debug)]
pub struct Fail(pub String);

impl From<crate::error::Error> for Fail {
    fn from(e: crate::error::Error) -> Self {
        Fail(e.to_string())
    }
}

impl From<String> for Fail {
    fn from(s: String) -> Self {
        Fail(s)
    }
}

impl From<&str> for Fail {
    fn from(s: &str) -> Self {
        Fail(s.to_string())
    }
}

pub type Res = std::result::Result<(), Fail>;

pub const USAGE: &str = "\
Usage: hangar [--instance NAME | --gamedir PATH] [--headless] [-y] <command> [args]

Global options:
  --instance NAME     use this registered game instance
  --gamedir PATH      use this game folder (not registered)
  --headless          never ask; fail when a choice is needed
  -y, --yes           answer yes to confirmation questions

Mods:
  update [-f] [-l]                     download repository metadata
  available [--detail]                 list compatible mods
  list [--porcelain] [--export FMT]    list installed mods (FMT: ckan text markdown bbcode csv tsv)
  search <term> [--all] [--author A] [--detail]
  show <id> [--with-versions]
  install <id|id=ver>... [-c FILE|URL ...] [--no-recommends] [--with-suggests]
          [--with-all-suggests] [--allow-incompatible]
  remove <id>... [--re] [--all]
  upgrade <id|id=ver>... | --all [-c FILE]
  replace <id>... | --all
  import <zip|dir>... [--delete]       add downloaded zips to the cache and install them
  scan                                 look for unmanaged DLLs and DLC
  mark auto|user <id>...
  export <file> [--format FMT]         save the installed mod list
  play [--no-wait]                     start the game

Settings:
  compat list | add <ver> | forget <ver> | set <ver>... | clear
  repo list | available | add <name> [<uri>] | forget <name> | priority <name> <n> | default
  instance list | add <name> <path> | forget <name> | rename <old> <new> | default <name>
           | fake <name> <path> <version> [--MakingHistory VER] [--BreakingGround VER] [--set-default]
  cache list | set <path> | clear | reset | showlimit | setlimit <MiB>
  label list | add <name> [--hide] [--hold] | remove <name> | show <name> | toggle <name> <id>

Other:
  compare <v1> <v2>                    print -1, 0 or 1
  version                              print the Hangar version
  help                                 show this text
  gui                                  open the window (also with no command)
";

/// Run a CLI command. Returns the process exit code.
pub fn run(args: Vec<String>) -> i32 {
    let (g, rest) = match args::split_globals(args) {
        Ok(x) => x,
        Err(e) => {
            warn!("{e}");
            warn!("Run \"hangar help\" for usage.");
            return 1;
        }
    };
    let Some((cmd, cmd_args)) = rest.split_first() else {
        term::write_out(USAGE);
        return 0;
    };
    match dispatch(&g, cmd, cmd_args) {
        Ok(()) => 0,
        Err(Fail(msg)) => {
            if !msg.is_empty() {
                warn!("{msg}");
            }
            1
        }
    }
}

fn dispatch(g: &Globals, cmd: &str, a: &[String]) -> Res {
    match cmd {
        "help" | "-h" | "--help" => {
            term::write_out(USAGE);
            Ok(())
        }
        "version" | "--version" | "-V" => {
            out!("Hangar v{}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        "compare" => admin::compare(a),
        "instance" => admin::instance(g, a),
        "cache" => admin::cache(a),
        "label" => admin::label(a),
        "repo" if a.first().is_some_and(|s| s == "available") => admin::repo_available(),
        "repo" => mods::repo(g, a),
        "update" => mods::update(g, a),
        "available" => mods::available(g, a),
        "list" => mods::list(g, a),
        "search" => mods::search(g, a),
        "show" => mods::show(g, a),
        "scan" => mods::scan(g, a),
        "mark" => mods::mark(g, a),
        "compat" => mods::compat(g, a),
        "export" => mods::export(g, a),
        "play" => mods::play(g, a),
        "install" => change::install(g, a),
        "remove" => change::remove(g, a),
        "upgrade" => change::upgrade(g, a),
        "replace" => change::replace(g, a),
        "import" => change::import(g, a),
        other => Err(Fail(format!("Unknown command: {other}\nRun \"hangar help\" for usage."))),
    }
}

/// Open the session for the chosen instance. `--gamedir` adds an unsaved entry to the config
/// in memory (or reuses the entry already registered for that folder).
pub fn open_session(g: &Globals) -> Result<Session, Fail> {
    let mut config = Config::load();
    let name = match (&g.gamedir, &g.instance) {
        (Some(dir), _) => Some(temp_instance(&mut config, dir)?),
        (None, Some(n)) => Some(n.clone()),
        (None, None) => None,
    };
    let s = Session::open(config, name.as_deref())?;
    for w in &s.warnings {
        warn!("{w}");
    }
    Ok(s)
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

/// Name of the config entry for `dir`, adding a temporary one when it is not registered.
pub fn temp_instance(config: &mut Config, dir: &Path) -> Result<String, Fail> {
    if !crate::instance::is_valid_game_dir(dir) {
        return Err(Fail(format!("{} is not a KSP game folder", dir.display())));
    }
    if let Some(e) = config.game_instances.iter().find(|e| same_dir(Path::new(&e.path), dir)) {
        return Ok(e.name.clone());
    }
    let abs = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
    let base = abs.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "gamedir".into());
    let mut name = base.clone();
    let mut n = 2;
    while config.instance(&name).is_some() {
        name = format!("{base} ({n})");
        n += 1;
    }
    config.game_instances.push(InstanceEntry { name: name.clone(), path: abs.display().to_string(), game: "KSP".into() });
    Ok(name)
}
