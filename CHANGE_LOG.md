# Change log

## Unreleased

### Added
- Text size setting for the window: a View menu (Larger text, Smaller text,
  Normal text size, and a list from 80% to 200%) and the keys Cmd+=, Cmd+-
  and Cmd+0. Hangar saves the size in `config.json` (`Hangar.text_size`) and
  uses it again at the next start.

### Changed
- Mod list column order: Downloads now comes after Latest version, and Max
  game version is the last column.
- The window works at large text sizes: Apply changes and Play stay at the
  right of the toolbar, and the filter and search box go to a second row when
  the toolbar is too narrow. Dialogs are never larger than the main window,
  their lists get shorter in a short window, and wide content scrolls.

### Fixed
- When another Hangar window or CKAN has the game folder open, the window no
  longer stops at an error and the Instances dialog. It shows that the
  instance is in use, tries again every 2 seconds, and opens the instance when
  the other program closes ("Try now" and "Stop waiting" buttons). The lock
  message now names the program that holds the lock, for example "another
  Hangar window (process 123)".
- A setting changed in the window while an instance opened (text size, shown
  columns) no longer goes back to the old value when the instance is open.
- A bad `text_size` value in `config.json` (for example 1.5 or "big") no
  longer resets all the other Hangar settings.

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
- Mod list export (CKAN modpack .ckan, text, markdown, BBCode, CSV, TSV) and
  import of CKAN modpacks from a file or a URL.
- Game instances (add, fake, rename, default, autodetect of Steam installs),
  compatible game versions, labels (held and hidden), install filters,
  play time, the Play button, and a scan for unmanaged DLLs and DLC.
- Window: sortable mod list, CKAN filters and search syntax, info panel with
  metadata, relationships, contents and versions, a Changes tab, dialogs for
  settings, repositories, instances, labels, compatibility, history and
  unmanaged files. The window uses no CPU when it is idle.
- `bundle.sh` builds `dist/Hangar.app` (arm64, ad-hoc signed).

### Changed
- The repo index is parsed as a stream and shares equal data between the
  versions of a mod. Memory after load went from 164 MB to 48 MB (CLI). The
  CKAN output is byte-identical.
