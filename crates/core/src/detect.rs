//! Finds installs of a game inside an environment by asking each store where
//! it put the game, then falling back to the game's known install paths.
//! A folder only counts if the game's executable is in it.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::PathBuf;

use serde::Serialize;
use ts_rs::TS;

use crate::env::{Environment, EnvironmentKind, Hive};
use crate::fs_util::{canonical_or_self, resolve_case_insensitive};
use crate::game::GameDefinition;
use crate::vdf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[ts(export)]
pub enum Store {
    Steam,
    Gog,
    Epic,
    /// Found at one of the game's known install paths, store unknown.
    KnownPath,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct GameInstance {
    /// Stable across runs: derived from the game, environment and folder.
    pub id: String,
    pub game_id: String,
    pub game_name: String,
    pub environment_id: String,
    pub store: Store,
    pub install_path: PathBuf,
    /// The executable's product version, when it has one.
    pub version: Option<String>,
    /// Steam's build id for Steam installs; changes with every game update.
    pub build_id: Option<String>,
    /// Changes whenever the game's executable changes (size, date, build).
    pub fingerprint: String,
    /// The launcher's id for the game, e.g. Epic's app name.
    pub launch_id: Option<String>,
    /// Steam launch options to set by hand (Proton), when the game needs any.
    pub launch_options_hint: Option<String>,
    /// State of the Wine DLL overrides the game's mod loaders need.
    pub dll_overrides: Vec<DllOverrideStatus>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct DllOverrideStatus {
    pub dll: String,
    pub required: String,
    /// The value currently set in the prefix, if any.
    pub current: Option<String>,
    /// True when Wine will load the game's own copy of the DLL first.
    pub satisfied: bool,
    pub reason: String,
}

/// Detects every game in every environment.
pub fn detect_all(games: &[GameDefinition], environments: &[Environment]) -> Vec<GameInstance> {
    environments
        .iter()
        .flat_map(|env| games.iter().flat_map(move |game| detect_game(game, env)))
        .collect()
}

/// Detects installs of one game in one environment.
pub fn detect_game(game: &GameDefinition, env: &Environment) -> Vec<GameInstance> {
    // A Proton prefix belongs to exactly one Steam game.
    if let EnvironmentKind::Proton { app_id, .. } = &env.kind
        && game.stores.steam_app_id.as_ref() != Some(app_id)
    {
        return Vec::new();
    }
    let is_proton = matches!(env.kind, EnvironmentKind::Proton { .. });
    let steam = steam_candidates(game, env)
        .into_iter()
        .map(|p| (Store::Steam, p, None));
    let others: Vec<(Store, PathBuf, Option<String>)> = if is_proton {
        Vec::new()
    } else {
        gog_candidates(game, env)
            .into_iter()
            .map(|p| (Store::Gog, p, None))
            .chain(
                epic_candidates(env)
                    .into_iter()
                    .map(|(p, app)| (Store::Epic, p, Some(app))),
            )
            .chain(
                game.known_install_paths
                    .iter()
                    .filter_map(|p| env.windows_to_host(p))
                    .map(|p| (Store::KnownPath, p, None)),
            )
            .collect()
    };
    let candidates = steam.chain(others);

    let overrides = env.dll_overrides().unwrap_or_default();
    let mut seen = HashSet::new();
    candidates
        .filter(|(_, path, _)| {
            resolve_case_insensitive(path, &game.executable).is_some_and(|exe| exe.is_file())
        })
        .map(|(store, path, launch_id)| (store, canonical_or_self(path), launch_id))
        .filter(|(_, path, _)| seen.insert(path.clone()))
        .map(|(store, install_path, launch_id)| {
            let exe = resolve_case_insensitive(&install_path, &game.executable);
            let build_id = (store == Store::Steam)
                .then(|| steam_build_id(game, &install_path))
                .flatten();
            let fingerprint = exe
                .as_deref()
                .and_then(|e| fs::metadata(e).ok())
                .map(|m| {
                    let modified = m
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map_or(0, |d| d.as_secs());
                    format!(
                        "{}:{modified}:{}",
                        m.len(),
                        build_id.as_deref().unwrap_or("")
                    )
                })
                .unwrap_or_default();
            GameInstance {
                id: instance_id(&game.id, &env.id, &install_path),
                game_id: game.id.clone(),
                game_name: game.name.clone(),
                environment_id: env.id.clone(),
                store,
                version: exe.as_deref().and_then(exe_version),
                build_id,
                fingerprint,
                launch_id,
                launch_options_hint: is_proton.then(|| proton_launch_options(game)).flatten(),
                install_path,
                // Proton takes overrides from launch options, Windows needs none.
                dll_overrides: if env.is_wine() && !is_proton {
                    dll_override_status(game, &overrides)
                } else {
                    Vec::new()
                },
            }
        })
        .collect()
}

/// Steam launch options a Proton game needs: Wine DLL overrides go in
/// `WINEDLLOVERRIDES`, and the game's modded arguments follow `%command%`.
fn proton_launch_options(game: &GameDefinition) -> Option<String> {
    let dlls: Vec<&str> = game
        .wine
        .dll_overrides
        .iter()
        .map(|d| d.dll.as_str())
        .collect();
    if dlls.is_empty() && game.launch.modded_args.is_empty() {
        return None;
    }
    let mut options = String::new();
    if !dlls.is_empty() {
        options.push_str(&format!("WINEDLLOVERRIDES=\"{}=n,b\" ", dlls.join(",")));
    }
    options.push_str("%command%");
    for arg in &game.launch.modded_args {
        options.push(' ');
        options.push_str(arg);
    }
    Some(options)
}

/// The product version in a Windows executable's version resource.
pub fn exe_version(exe: &std::path::Path) -> Option<String> {
    use pelite::pe64::Pe;
    let map = pelite::FileMap::open(exe).ok()?;
    let file = pelite::pe64::PeFile::from_bytes(map.as_ref()).ok()?;
    let info = file.resources().ok()?.version_info().ok()?;
    let from_strings = info
        .translation()
        .first()
        .and_then(|lang| info.value(*lang, "ProductVersion"))
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    from_strings.or_else(|| info.fixed().map(|f| f.dwProductVersion.to_string()))
}

/// A DLL's or executable's own version, for telling framework versions apart:
/// the `FileVersion` string (`v1.38.0 [HEAD]` becomes `1.38.0`), else the
/// fixed file version.
pub fn file_version(path: &std::path::Path) -> Option<String> {
    use pelite::pe64::Pe;
    let map = pelite::FileMap::open(path).ok()?;
    let file = pelite::pe64::PeFile::from_bytes(map.as_ref()).ok()?;
    let info = file.resources().ok()?.version_info().ok()?;
    let from_strings = info
        .translation()
        .first()
        .and_then(|lang| info.value(*lang, "FileVersion"))
        .map(|v| {
            v.trim()
                .trim_start_matches(['v', 'V'])
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .to_string()
        })
        .filter(|v| v.chars().next().is_some_and(|c| c.is_ascii_digit()));
    from_strings.or_else(|| info.fixed().map(|f| f.dwFileVersion.to_string()))
}

/// Steam's build id from the app manifest next to a Steam library install
/// (`<library>/steamapps/common/<game>` -> `<library>/steamapps/appmanifest_<id>.acf`).
fn steam_build_id(game: &GameDefinition, install_path: &std::path::Path) -> Option<String> {
    let app_id = game.stores.steam_app_id.as_ref()?;
    let steamapps = install_path.parent()?.parent()?;
    let manifest = read_vdf(steamapps, &format!("appmanifest_{app_id}.acf"))?;
    manifest
        .get("AppState")?
        .get_str("buildid")
        .map(String::from)
}

/// `<game>-<hash>`, using FNV-1a so the id never changes between builds.
fn instance_id(game_id: &str, environment_id: &str, path: &std::path::Path) -> String {
    let key = format!("{environment_id}\0{}", path.display());
    let hash = key.bytes().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
    });
    format!("{game_id}-{hash:016x}")
}

fn dll_override_status(
    game: &GameDefinition,
    current: &BTreeMap<String, String>,
) -> Vec<DllOverrideStatus> {
    game.wine
        .dll_overrides
        .iter()
        .map(|required| {
            let dll = required.dll.to_ascii_lowercase();
            // winecfg writes `version`, other tools sometimes `*version`.
            let current = current
                .get(&dll)
                .or_else(|| current.get(&format!("*{dll}")))
                .cloned();
            let satisfied = current
                .as_deref()
                .and_then(|v| v.split(',').next())
                .is_some_and(|first| matches!(first.trim(), "native" | "n"));
            DllOverrideStatus {
                dll: required.dll.clone(),
                required: required.mode.clone(),
                current,
                satisfied,
                reason: required.reason.clone(),
            }
        })
        .collect()
}

/// All library folders of a Steam install (the install itself first).
/// `to_host` converts paths written in `libraryfolders.vdf`.
pub fn steam_libraries(
    root: &std::path::Path,
    to_host: impl Fn(&str) -> Option<PathBuf>,
) -> Vec<PathBuf> {
    let mut libraries = vec![root.to_path_buf()];
    if let Some(doc) = read_vdf(root, "steamapps/libraryfolders.vdf")
        && let Some(folders) = doc.get("libraryfolders")
    {
        libraries.extend(
            folders
                .entries()
                .iter()
                .filter_map(|(_, folder)| folder.get_str("path"))
                .filter_map(&to_host),
        );
    }
    let mut seen = HashSet::new();
    libraries
        .into_iter()
        .filter(|lib| seen.insert(canonical_or_self(lib.clone())))
        .collect()
}

/// Reads Steam's library list and the game's app manifest in each library.
fn steam_candidates(game: &GameDefinition, env: &Environment) -> Vec<PathBuf> {
    let Some(app_id) = &game.stores.steam_app_id else {
        return Vec::new();
    };
    env.steam_roots()
        .iter()
        .flat_map(|root| steam_libraries(root, |p| env.launcher_path(p)))
        .filter_map(|lib| {
            let manifest = read_vdf(&lib, &format!("steamapps/appmanifest_{app_id}.acf"))?;
            let install_dir = manifest.get("AppState")?.get_str("installdir")?;
            resolve_case_insensitive(&lib, &format!("steamapps/common/{install_dir}"))
        })
        .collect()
}

fn read_vdf(base: &std::path::Path, relative: &str) -> Option<vdf::Value> {
    let path = resolve_case_insensitive(base, relative)?;
    vdf::parse(&fs::read_to_string(path).ok()?).ok()
}

/// GOG Galaxy and the GOG offline installers record the install path in the registry.
fn gog_candidates(game: &GameDefinition, env: &Environment) -> Vec<PathBuf> {
    let Some(gog_id) = &game.stores.gog_id else {
        return Vec::new();
    };
    [
        format!(r"Software\GOG.com\Games\{gog_id}"),
        format!(r"Software\Wow6432Node\GOG.com\Games\{gog_id}"),
    ]
    .iter()
    .filter_map(|key| env.registry_key(Hive::LocalMachine, key).ok().flatten())
    .filter_map(|values| values.get("path").and_then(|p| env.windows_to_host(p)))
    .collect()
}

/// The Epic launcher writes one JSON manifest per installed game. Manifests
/// identify games by opaque IDs, so every install location is returned (with
/// its app name, used to launch it) and the executable check in
/// `detect_game` picks the right one.
fn epic_candidates(env: &Environment) -> Vec<(PathBuf, String)> {
    let Some(dir) = env.windows_to_host(r"C:\ProgramData\Epic\EpicGamesLauncher\Data\Manifests")
    else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("item"))
        })
        .filter_map(|p| fs::read_to_string(p).ok())
        .filter_map(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .filter_map(|json| {
            let path = json
                .get("InstallLocation")?
                .as_str()
                .and_then(|p| env.windows_to_host(p))?;
            let app = json.get("AppName")?.as_str()?.to_string();
            Some((path, app))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::env::EnvironmentKind;
    use crate::game::find_builtin;

    fn fake_prefix(root: &Path) -> Environment {
        fs::create_dir_all(root.join("drive_c")).unwrap();
        fs::write(root.join("system.reg"), "WINE REGISTRY Version 2\n").unwrap();
        Environment {
            id: "test".into(),
            name: "test".into(),
            kind: EnvironmentKind::WinePrefix,
            prefix: root.to_path_buf(),
        }
    }

    fn install_game(dir: &Path) {
        fs::create_dir_all(dir.join("bin/x64")).unwrap();
        fs::write(dir.join("bin/x64/Cyberpunk2077.exe"), b"").unwrap();
    }

    #[test]
    fn finds_steam_install_through_library_folders() {
        let tmp = tempfile::tempdir().unwrap();
        let env = fake_prefix(tmp.path());
        let steamapps = tmp
            .path()
            .join("drive_c/Program Files (x86)/Steam/steamapps");
        let library = tmp.path().join("drive_c/Games/SteamLibrary/steamapps");
        fs::create_dir_all(&steamapps).unwrap();
        fs::create_dir_all(&library).unwrap();
        fs::write(
            steamapps.join("libraryfolders.vdf"),
            r#""libraryfolders" { "0" { "path" "C:\\Program Files (x86)\\Steam" } "1" { "path" "C:\\Games\\SteamLibrary" } }"#,
        )
        .unwrap();
        fs::write(
            library.join("appmanifest_1091500.acf"),
            r#""AppState" { "appid" "1091500" "installdir" "Cyberpunk 2077" }"#,
        )
        .unwrap();
        install_game(&library.join("common/Cyberpunk 2077"));

        let found = detect_game(&find_builtin("cyberpunk2077").unwrap(), &env);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].store, Store::Steam);
        assert!(
            found[0]
                .install_path
                .ends_with("SteamLibrary/steamapps/common/Cyberpunk 2077")
        );
    }

    #[test]
    fn finds_gog_install_from_registry_and_skips_duplicate_known_path() {
        let tmp = tempfile::tempdir().unwrap();
        let env = fake_prefix(tmp.path());
        fs::write(
            tmp.path().join("system.reg"),
            "[Software\\\\Wow6432Node\\\\GOG.com\\\\Games\\\\1423049311] 1\n\"path\"=\"C:\\\\GOG Games\\\\Cyberpunk 2077\"\n",
        )
        .unwrap();
        install_game(&tmp.path().join("drive_c/GOG Games/Cyberpunk 2077"));

        let found = detect_game(&find_builtin("cyberpunk2077").unwrap(), &env);
        assert_eq!(
            found.len(),
            1,
            "GOG registry hit and known path are the same folder"
        );
        assert_eq!(found[0].store, Store::Gog);
    }

    #[test]
    fn finds_proton_game_in_linux_steam_library() {
        let tmp = tempfile::tempdir().unwrap();
        let steam = tmp.path().join("Steam");
        let steamapps = steam.join("steamapps");
        fs::create_dir_all(steamapps.join("compatdata/1091500/pfx/drive_c")).unwrap();
        fs::write(steamapps.join("compatdata/1091500/pfx/system.reg"), "").unwrap();
        fs::write(
            steamapps.join("appmanifest_1091500.acf"),
            r#""AppState" { "appid" "1091500" "installdir" "Cyberpunk 2077" "buildid" "42" }"#,
        )
        .unwrap();
        install_game(&steamapps.join("common/Cyberpunk 2077"));

        let envs =
            crate::env::proton::discover_in(std::slice::from_ref(&steam), &["1091500".into()]);
        assert_eq!(envs.len(), 1);
        let found = detect_game(&find_builtin("cyberpunk2077").unwrap(), &envs[0]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].build_id.as_deref(), Some("42"));
        assert!(found[0].dll_overrides.is_empty());
        let hint = found[0].launch_options_hint.as_deref().unwrap();
        assert_eq!(
            hint,
            r#"WINEDLLOVERRIDES="version,winmm=n,b" %command% -modded"#
        );
    }

    #[test]
    fn folder_without_executable_is_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let env = fake_prefix(tmp.path());
        fs::create_dir_all(tmp.path().join("drive_c/GOG Games/Cyberpunk 2077/bin")).unwrap();
        assert!(detect_game(&find_builtin("cyberpunk2077").unwrap(), &env).is_empty());
    }

    #[test]
    fn reports_dll_override_state() {
        let tmp = tempfile::tempdir().unwrap();
        let env = fake_prefix(tmp.path());
        fs::write(
            tmp.path().join("user.reg"),
            "[Software\\\\Wine\\\\DllOverrides] 1\n\"*version\"=\"native,builtin\"\n\"winmm\"=\"builtin\"\n",
        )
        .unwrap();
        install_game(&tmp.path().join("drive_c/GOG Games/Cyberpunk 2077"));

        let found = detect_game(&find_builtin("cyberpunk2077").unwrap(), &env);
        let status: Vec<_> = found[0]
            .dll_overrides
            .iter()
            .map(|s| (s.dll.as_str(), s.satisfied))
            .collect();
        assert_eq!(status, [("version", true), ("winmm", false)]);
    }
}
