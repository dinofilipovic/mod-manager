# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Safety

- Applying mods is journaled file by file: a failed or interrupted apply is
  finished or undone the next time, and an original game file is never
  overwritten or lost. Fixes a bug where a failed apply followed by another
  apply could replace the backup of an original game file with a mod's file.
- Deployed files are checked against what was placed: missing files come back
  on the next apply, files a game update replaced become the new originals
  (the old one is kept in a preserved folder unless identical), and files
  edited by hand are copied to a preserved folder before being replaced or
  removed. Text and config files are never hard-linked. Health shows it all.
- `library.db` is copied before database upgrades and before each apply (the
  last five of each are kept).

### Versions and updates

- Every archive a mod was installed from is kept in an archive store
  (deduplicated); Settings chooses how many versions to keep, shows sizes per
  mod and cleans unused archives.
- Roll back a mod, reinstall it, install an older version or change its
  installer options. Installer choices are remembered and used again for
  updates when the installer still has the same options.
- Update checks ask Nexus Mods only about mods that changed since the last
  check, find newer main files when the author didn't link them, and identify
  archives you added by their MD5. Checks run on their own once a day and
  after a game update (can be turned off); the sidebar counts waiting updates.
- Pin a mod's version, skip an update, Update all, and read the changelog of
  an update.
- Nexus Mods request limits are shown in Settings; background requests pause
  before they run out.
- Downloads are a queue that survives restarts, runs three at a time, retries
  network problems and continues interrupted files; downloads can be cancelled
  and retried.
- Installing no longer blocks the app while archives are extracted.

### Requirements

- Mods' requirements come from the game's framework rules, their Nexus Mods
  pages and your own additions. Mod details list what a mod requires and what
  requires it; disabling or removing a needed mod asks first; Health lists
  unmet requirements with Get all missing.
- Framework versions are read from their DLLs and compared with the version
  the game needs (Cyberpunk 2077 2.5: CET 1.38.0, redscript 0.5.32, ArchiveXL
  1.28.0, TweakXL 1.12.0, Codeware 1.21.0, from their release notes).
- Mod Settings, Native Settings UI and Input Loader are known frameworks.

### Checks and status

- Mod files are checked at install (.archive files, JSON, YAML, CET entry
  points, 64-bit DLLs, Windows path rules); mods that won't load are marked.
- Play checks first: missing requirements, frameworks too old for the game,
  Wine DLL overrides, mods that won't load and installers waiting for choices
  open a checklist with Launch anyway.
- The mod list shows whether each mod loaded, read from the RED4ext, CET and
  redscript logs.
- Mods whose `.archive` files change the same game resources are shown with
  the winner by the archive load order.
- Mods installed by hand can be adopted without touching the game folder.
- The RED4ext log is found under its current name (`red4ext-<date>.log`).

### Command line

- `mm enable`, `disable`, `profiles`, `check-updates`, `update`, `rollback`,
  `reinstall`, `verify`, `preflight`, `deps`, `archives` and `adopt`, all with
  `--json`. Every command takes `--bottles-dir`, and `MM_DATA_DIR` moves the
  library (for scripts and tests).

## [0.1.0] - 2026-10-25

First release.

### Games and environments

- Finds games in CrossOver bottles (macOS, Linux), Wine prefixes, Steam Proton
  prefixes (Linux, Steam Deck) and natively on Windows, through Steam library
  manifests, the GOG registry, Epic launcher manifests and known install paths.
- Supported games: Cyberpunk 2077, The Witcher 3: Wild Hunt, Stardew Valley and
  Mount & Blade II: Bannerlord, each defined in a data file under `games/`.
- Reads the game version and Steam build, and warns when the game was updated
  after its script extenders were installed.

### Mods

- Installs mods from `.zip`, `.7z` and `.rar` archives by drag and drop or file
  picker, placing files by the game's layout rules (game-root archives, wrapper
  folders, loose files, marker files and folders).
- FOMOD installer support: a step-by-step options wizard with images,
  descriptions, conditional steps and validation.
- Profiles per game install, with their own enabled mods and load order;
  export a profile to a file and import it elsewhere.
- Drag-to-reorder load order with per-mod conflict information.
- Missing-requirement detection for frameworks (CET, RED4ext, redscript,
  ArchiveXL, TweakXL, Codeware and others), with one-click enabling or a link
  to get them.
- Optional archive load order for Cyberpunk 2077 via `modlist.txt`.

### Deployment and safety

- Mods stay unpacked in a library and are deployed as copy-on-write clones,
  hard links or copies; replaced game files are backed up and restored, and
  every deployed file can be removed again.
- Save backups: automatic before each launch (when saves changed, last 10
  kept) and manual, with restore that backs up the current saves first.
- Health tab: framework status and errors from the game's and frameworks' log
  files, attributed to the mod they mention.

### Nexus Mods

- Connect with a personal API key, stored in the system keychain.
- *Mod Manager Download* links (`nxm://`) download and install into the app.
- Archives added by hand are matched on Nexus by the mod id in their file name.
- Update checks for Nexus mods; Premium accounts update in one click.
- Collections: open a collection by link; Premium installs it at once, free
  accounts get each mod's page.

### Wine and launching

- Detects and sets the Wine DLL overrides that script extenders need in a
  bottle; shows the Steam launch options to use under Proton.
- Play applies pending changes and starts the game through Steam, the Epic
  launcher, or directly for GOG.

### App

- Desktop app for macOS, Linux and Windows with light and dark themes.
- Updates itself from GitHub Releases.
- `mm` command-line tool: `detect`, `mods`, `health`, `install`, `deploy`,
  `purge`.

[Unreleased]: https://github.com/dinofilipovic/mod-manager/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/dinofilipovic/mod-manager/releases/tag/v0.1.0
