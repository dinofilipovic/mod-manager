# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

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
