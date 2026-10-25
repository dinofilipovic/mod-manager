//! Finds installs of a game inside an environment by asking each store where
//! it put the game, then falling back to the game's known install paths.
//! A folder only counts if the game's executable is in it.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::PathBuf;

use serde::Serialize;
use ts_rs::TS;

use crate::env::{Environment, Hive};
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
    let candidates = steam_candidates(game, env)
        .into_iter()
        .map(|p| (Store::Steam, p))
        .chain(
            gog_candidates(game, env)
                .into_iter()
                .map(|p| (Store::Gog, p)),
        )
        .chain(epic_candidates(env).into_iter().map(|p| (Store::Epic, p)))
        .chain(
            game.known_install_paths
                .iter()
                .filter_map(|p| env.windows_to_host(p))
                .map(|p| (Store::KnownPath, p)),
        );

    let overrides = env.dll_overrides().unwrap_or_default();
    let mut seen = HashSet::new();
    candidates
        .filter(|(_, path)| {
            resolve_case_insensitive(path, &game.executable).is_some_and(|exe| exe.is_file())
        })
        .map(|(store, path)| (store, canonical_or_self(path)))
        .filter(|(_, path)| seen.insert(path.clone()))
        .map(|(store, install_path)| {
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
                install_path,
                dll_overrides: dll_override_status(game, &overrides),
            }
        })
        .collect()
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

const STEAM_ROOTS: &[&str] = &[r"C:\Program Files (x86)\Steam", r"C:\Program Files\Steam"];

/// Reads Steam's library list and the game's app manifest in each library.
fn steam_candidates(game: &GameDefinition, env: &Environment) -> Vec<PathBuf> {
    let Some(app_id) = &game.stores.steam_app_id else {
        return Vec::new();
    };
    let mut libraries = Vec::new();
    for root in STEAM_ROOTS.iter().filter_map(|r| env.windows_to_host(r)) {
        if !root.is_dir() {
            continue;
        }
        if let Some(doc) = read_vdf(&root, "steamapps/libraryfolders.vdf")
            && let Some(folders) = doc.get("libraryfolders")
        {
            libraries.extend(
                folders
                    .entries()
                    .iter()
                    .filter_map(|(_, folder)| folder.get_str("path"))
                    .filter_map(|path| env.windows_to_host(path)),
            );
        }
        libraries.push(root);
    }

    let mut seen = HashSet::new();
    libraries
        .into_iter()
        .filter(|lib| seen.insert(canonical_or_self(lib.clone())))
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
/// identify games by opaque IDs, so every install location is returned and
/// the executable check in `detect_game` picks the right one.
fn epic_candidates(env: &Environment) -> Vec<PathBuf> {
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
            json.get("InstallLocation")?
                .as_str()
                .and_then(|p| env.windows_to_host(p))
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
