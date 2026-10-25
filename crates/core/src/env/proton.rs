//! Steam Proton prefixes on Linux (including the Steam Deck). Each game gets
//! its own Wine prefix at `steamapps/compatdata/<app id>/pfx`, while the game
//! itself is installed natively in a Steam library.

use std::path::{Path, PathBuf};

use super::{Environment, EnvironmentKind, is_wine_prefix};
use crate::fs_util::canonical_or_self;

/// Steam installs on this machine: native, the `~/.steam` link, and Flatpak.
pub fn steam_roots() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    let mut roots: Vec<PathBuf> = Vec::new();
    for candidate in [
        home.join(".steam/steam"),
        home.join(".local/share/Steam"),
        home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam"),
    ] {
        let canonical = canonical_or_self(candidate);
        if canonical.join("steamapps").is_dir() && !roots.contains(&canonical) {
            roots.push(canonical);
        }
    }
    roots
}

/// Proton prefixes for the given Steam app ids, on Linux only.
pub fn discover(app_ids: &[String]) -> Vec<Environment> {
    if !cfg!(target_os = "linux") {
        return Vec::new();
    }
    discover_in(&steam_roots(), app_ids)
}

pub fn discover_in(steam_roots: &[PathBuf], app_ids: &[String]) -> Vec<Environment> {
    let mut found: Vec<Environment> = Vec::new();
    for root in steam_roots {
        for library in crate::detect::steam_libraries(root, |p| Some(PathBuf::from(p))) {
            for app_id in app_ids {
                let prefix = library
                    .join("steamapps/compatdata")
                    .join(app_id)
                    .join("pfx");
                let id = format!("proton:{app_id}");
                if is_wine_prefix(&prefix) && !found.iter().any(|e| e.id == id) {
                    found.push(environment(app_id, root, prefix));
                }
            }
        }
    }
    found
}

fn environment(app_id: &str, steam_root: &Path, prefix: PathBuf) -> Environment {
    Environment {
        id: format!("proton:{app_id}"),
        name: "Proton (Steam)".into(),
        kind: EnvironmentKind::Proton {
            app_id: app_id.to_string(),
            steam_root: steam_root.to_path_buf(),
        },
        prefix,
    }
}
