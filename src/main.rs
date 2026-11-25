mod cli;
mod config;
mod error;
mod gui;
mod install;
mod instance;
mod meta;
mod modlist;
mod ops;
mod registry;
mod repo;
mod resolve;
mod util;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Finder may pass -psn_* on old macOS; no args or "gui" opens the window.
    let gui = args.is_empty() || args[0] == "gui" || args[0].starts_with("-psn");
    if gui {
        if let Err(e) = gui::run() {
            eprintln!("{e}");
            std::process::exit(1);
        }
    } else {
        std::process::exit(cli::run(args));
    }
}
