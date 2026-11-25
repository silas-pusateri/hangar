# Change log

## Unreleased

### Changed
- The Play button is green with white text, in the light and the dark theme.

## 0.1.0 - 2026-11-24

First version of Hangar, a mod manager for Kerbal Space Program 1. It runs
natively on Apple silicon Macs and it uses the CKAN file formats.

### Added
- One Rust binary with two modes: a window (egui, no arguments) and a command
  line (`hangar <command>`).
- Repository support: the CKAN-meta archive and other CKAN repositories
  (tar.gz or zip), ETag checks, download counts, `repositories.json`.
- Metadata: CKAN spec parsing up to v1.36, exact CKAN version comparison,
  game version ranges, relationships with `any_of`, `provides` and
  `replaced_by`.
- Dependency resolver ported from CKAN: dependencies, conflicts, virtual
  modules with a provider choice, recommendations and suggestions, upgrades,
  replacements, removal of auto-installed mods that are no longer necessary.
- Install and remove with file transactions: a failure rolls back all file
  changes. A held signal (Ctrl-C, SIGTERM, SIGHUP) cannot cut a change in half.
- Download cache with CKAN file names, SHA1 and SHA256 checks, a size limit
  and a free-space check.
- CKAN registry format in `<game>/CKAN/` (registry.json v3, registry.locked,
  installed-*.ckan, history), so CKAN and Hangar can manage the same game.
  When another Hangar window or CKAN has the game folder open, the window
  shows that the instance is in use and opens it when the other program
  closes. The lock message names the program that holds the lock.
- Mod list export (CKAN modpack .ckan, text, markdown, BBCode, CSV, TSV) and
  import of CKAN modpacks from a file or a URL.
- Game instances (add, fake, rename, default, autodetect of Steam installs),
  compatible game versions, labels (held and hidden), install filters,
  play time, the Play button, and a scan for unmanaged DLLs and DLC.
- Window: sortable mod list, CKAN filters and search syntax, info panel with
  metadata, relationships, contents and versions, a Changes tab, dialogs for
  settings, repositories, instances, labels, compatibility, history and
  unmanaged files. The window uses no CPU when it is idle.
- Text size setting: a View menu (80% to 200%) and the keys Cmd+=, Cmd+-
  and Cmd+0. The size is saved in `config.json` (`Hangar.text_size`). The
  layout fits up to 200%: the toolbar goes to two rows, and dialogs are never
  larger than the main window.
- `bundle.sh` builds `dist/Hangar.app` (arm64, ad-hoc signed).

### Changed
- The repo index is parsed as a stream and shares equal data between the
  versions of a mod. Memory after load went from 164 MB to 48 MB (CLI). The
  CKAN output is byte-identical.
- Tests no longer use paths from the development machine. The live install
  test downloads the CKAN-meta archive when `HANGAR_TEST_META` is not set.
