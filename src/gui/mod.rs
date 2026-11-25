//! eframe/egui desktop interface.
//!
//! Files: `app` (state, frame loop, menus, toolbar, status bar), `table` (mod list),
//! `info` (mod info side panel), `changes` (pending changes and the apply flow),
//! `dialogs` (settings-style windows), `files` (file dialogs), `worker` (background jobs), `model` (Session-free view model).

mod app;
mod changes;
mod dialogs;
mod files;
mod info;
mod model;
mod selftest;
mod table;
mod worker;

/// Open the main window. Blocks until it closes.
pub fn run() -> crate::error::Result<()> {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Hangar")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([900.0, 560.0]),
        ..Default::default()
    };
    eframe::run_native("Hangar", options, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
        .map_err(|e| crate::error::Error::Other(format!("window: {e}")))
}
