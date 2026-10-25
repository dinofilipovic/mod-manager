# Mod Manager

A cross-platform mod manager for Windows games, wherever they run: in
**CrossOver bottles on macOS**, in **Wine prefixes on Linux**, and (planned) on
native Windows. It installs mods from archives or straight from Nexus Mods,
keeps per-game profiles and load orders, and deploys them safely into the game
folder.

**Cyberpunk 2077** is the first supported game. New games are added as data
files, not code.

> Status: early but usable. Built and tested with Cyberpunk 2077 (Steam) in a
> CrossOver 26 bottle on macOS.

## Features

- **Finds your games.** Scans CrossOver bottles and Wine prefixes, then finds
  game installs through Steam library manifests, the GOG registry, Epic
  launcher manifests and common install paths.
- **Installs mods from archives** (`.zip`, `.7z`, `.rar`). Drag them onto the
  window or use *Add mods*. Each file goes to the right place, whatever the
  archive layout: game-root archives, extra wrapper folders, loose `.archive`
  files, Cyber Engine Tweaks folders (`init.lua`), REDmod (`info.json`),
  redscript, TweakXL and RED4ext.
- **Nexus Mods integration.** Connect with your personal API key (stored in the
  system keychain). *Mod Manager Download* on the website then downloads and
  installs into the app. Archives you add yourself are matched on Nexus by the
  mod id in their file name, so they get a proper name, author, image and update
  alerts too.
- **Update checks.** Premium accounts update in one click; free accounts get a
  link straight to the right files page.
- **Profiles** per game install, each with its own enabled mods and load order.
- **Load order and conflicts.** Drag to reorder; lower in the list wins. Every
  mod shows which mods it overrides or is overridden by.
- **Safe deployment.** Mods stay unpacked in the library and are placed in the
  game folder as copy-on-write clones (APFS, Btrfs, XFS, ReFS), else hard links
  or copies. Original game files a mod replaces are backed up and restored.
  *Remove all mods from game folder* undoes everything.
- **Missing requirements.** Knows which frameworks mods build on (Cyber Engine
  Tweaks, RED4ext, redscript, ArchiveXL, TweakXL, Codeware). A mod whose
  framework is missing or disabled is flagged with a one-click *Enable* or a
  link to get it.
- **Health tab.** Shows the game version and Steam build, warns when the game
  was updated after your script extenders were installed, and reads the logs
  of every framework and CET mod, listing errors next to the mod that caused
  them.
- **Archive load order.** By file name (the game's default), or optionally by
  your mod list: the app then writes `archive/pc/mod/modlist.txt` so mods lower
  in the list win.
- **Wine setup in one click.** Detects the `version` and `winmm` DLL overrides
  Cyber Engine Tweaks and RED4ext need, and sets them in the bottle.
- **Play.** Applies pending changes, then starts the game through the bottle's
  Steam client (or the game executable) with `-modded` for REDmod.
- **Command line.** `mm detect`, `mm mods`, `mm health`, `mm install`,
  `mm deploy`, `mm purge`, handy on Linux or a Steam Deck.

### Not yet

Nexus Collections, FOMOD installer options (default files are installed with a
warning), native Windows and Proton environments, and games other than
Cyberpunk 2077.

## Getting started

Prerequisites: [Rust](https://rustup.rs) 1.93+, [Node.js](https://nodejs.org)
22+, and the [Tauri system dependencies](https://v2.tauri.app/start/prerequisites/)
for your OS. On macOS, CrossOver must be installed for the Wine features.

```bash
git clone https://github.com/dinofilipovic/mod-manager.git
cd mod-manager
npm install
npm run dev
```

To build installers (`.dmg`/`.app`, `.AppImage`/`.deb`/`.rpm`, `.msi`/`.exe`):

```bash
npm run build
```

They are written to `target/release/bundle/`. `nxm://` links from Nexus only
open the app from an installed build: on macOS, open the built `.app` once so
the system registers it.

### First run with Cyberpunk 2077 in CrossOver

1. Install Cyberpunk 2077 in a CrossOver bottle (Steam, GOG or Epic).
2. Open Mod Manager. The game appears in the sidebar.
3. Click **Fix automatically** on the Wine banner if it appears.
4. In **Settings**, connect your Nexus Mods account (optional, but needed for
   downloads and update alerts).
5. Add mods, then press **Play**. Changes are applied first.

## How it is built

```
crates/core   mm-core: all logic (games, environments, detection, installer,
              library, deployment, Nexus). No UI dependency.
crates/cli    mm: command-line front end over the core
crates/app    Tauri 2 desktop app: a thin command layer over the core
ui/           React + TypeScript frontend (Vite)
games/        One TOML definition per supported game
```

- **Rust core, replaceable UI.** The UI talks to the core only through the
  Tauri commands in `crates/app/src/commands.rs`. Their TypeScript types in
  `ui/src/bindings` are generated from the Rust structs with
  [ts-rs](https://github.com/Aleph-Alpha/ts-rs), so both sides stay in sync.
- **Environments.** CrossOver bottles and Wine prefixes share one model (paths,
  drive mappings, registry), so adding Proton or native Windows means adding a
  discovery module, not touching the rest.
- **Storage.** App data lives in the OS data folder (for example
  `~/Library/Application Support/ModManager` on macOS): `library.db` (SQLite),
  `mods/` (unpacked mods), `backups/` (replaced game files) and `downloads/`.

### Development

```bash
npm run dev                         # desktop app with hot reload
npm run dev -w ui                   # UI only, in a browser at localhost:1420, with a fake backend
cargo test --workspace              # tests; also regenerates ui/src/bindings
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p mm-cli -- --help       # the command-line tool
```

CI builds and tests on macOS, Linux and Windows for every push and pull request.

### Adding a game

Add `games/<id>.toml` (see [`games/cyberpunk2077.toml`](games/cyberpunk2077.toml))
and list it in `BUILTIN_GAMES` in `crates/core/src/game.rs`. A definition
describes how to recognise the game, where each kind of mod file goes, which
folder names must be lowercase, and which Wine DLL overrides its mod loaders
need. Unknown keys are rejected, so a key placed under the wrong `[table]`
fails loudly instead of being ignored.

## Contributing

Issues and pull requests are welcome. Please run the tests and clippy before
opening a pull request.

## License

[MIT](LICENSE). Not affiliated with Nexus Mods, CodeWeavers or CD PROJEKT RED.
