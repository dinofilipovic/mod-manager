//! Environments are the places a Windows game can run: CrossOver bottles,
//! plain Wine prefixes, Steam Proton prefixes, and Windows itself.
//!
//! The Wine-based ones share the prefix layout (`drive_c`, `dosdevices`,
//! `system.reg`, `user.reg`), so path and registry handling lives here and the
//! submodules only know how to find them and read their metadata.

pub mod crossover;
pub mod native;
pub mod proton;
pub mod runner;
pub mod wine;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use ts_rs::TS;

use crate::error::Result;
use crate::fs_util::resolve_case_insensitive;
use crate::registry;

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct Environment {
    /// Stable identifier, e.g. `crossover:Cyberpunk 2077`.
    pub id: String,
    pub name: String,
    pub kind: EnvironmentKind,
    /// Root of the Wine prefix (the bottle folder for CrossOver).
    pub prefix: PathBuf,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(tag = "type")]
#[ts(export)]
pub enum EnvironmentKind {
    CrossOverBottle {
        bottle_id: Option<String>,
        template: Option<String>,
        wine_arch: Option<String>,
        crossover_version: Option<String>,
    },
    WinePrefix,
    /// A Steam Proton prefix; the game lives in a Linux Steam library.
    Proton {
        app_id: String,
        steam_root: PathBuf,
    },
    /// Windows itself.
    Native,
}

/// Registry hive of a Wine prefix.
#[derive(Debug, Clone, Copy)]
pub enum Hive {
    /// `HKEY_LOCAL_MACHINE`, stored in `system.reg`.
    LocalMachine,
    /// `HKEY_CURRENT_USER`, stored in `user.reg`.
    CurrentUser,
}

impl Environment {
    pub fn c_drive(&self) -> PathBuf {
        self.prefix.join("drive_c")
    }

    /// Converts a Windows path like `C:\Program Files (x86)\Steam` to a path on
    /// this machine, using the prefix's `dosdevices` drive mappings. Components
    /// are matched without regard to case; if the path does not exist yet, the
    /// unresolved rest is appended as-is.
    pub fn windows_to_host(&self, windows_path: &str) -> Option<PathBuf> {
        if matches!(self.kind, EnvironmentKind::Native) {
            return Some(PathBuf::from(windows_path));
        }
        let (drive, rest) = windows_path.split_once(':')?;
        if drive.len() != 1 || !drive.chars().all(|c| c.is_ascii_alphabetic()) {
            return None;
        }
        let drive = drive.to_ascii_lowercase();
        // `dosdevices/c:` can't be named on Windows, where `c:` is a drive.
        let mapped = (!cfg!(windows))
            .then(|| self.prefix.join("dosdevices").join(format!("{drive}:")))
            .filter(|p| p.exists());
        let base = match mapped {
            Some(path) => path,
            None if drive == "c" => self.c_drive(),
            None => return None,
        };
        Some(resolve_case_insensitive(&base, rest).unwrap_or_else(|| {
            rest.split(['/', '\\'])
                .filter(|c| !c.is_empty())
                .fold(base, |path, c| path.join(c))
        }))
    }

    /// Reads the string values of a registry key, or `None` if it does not exist.
    pub fn registry_key(&self, hive: Hive, key: &str) -> Result<Option<BTreeMap<String, String>>> {
        if matches!(self.kind, EnvironmentKind::Native) {
            return Ok(native::registry_key(hive, key));
        }
        let file = match hive {
            Hive::LocalMachine => "system.reg",
            Hive::CurrentUser => "user.reg",
        };
        let path = self.prefix.join(file);
        if !path.is_file() {
            return Ok(None);
        }
        registry::read_key(&path, key)
    }

    /// Whether games here run through Wine (everything but Windows itself).
    pub fn is_wine(&self) -> bool {
        !matches!(self.kind, EnvironmentKind::Native)
    }

    /// Steam installs that can hold this environment's games.
    pub fn steam_roots(&self) -> Vec<PathBuf> {
        match &self.kind {
            EnvironmentKind::Proton { steam_root, .. } => vec![steam_root.clone()],
            _ => [r"C:\Program Files (x86)\Steam", r"C:\Program Files\Steam"]
                .iter()
                .filter_map(|p| self.windows_to_host(p))
                .filter(|p| p.is_dir())
                .collect(),
        }
    }

    /// Converts a path written by a launcher (Steam library folders, Epic
    /// manifests) to a host path: Windows paths inside Wine, Linux paths as-is
    /// for Proton.
    pub fn launcher_path(&self, path: &str) -> Option<PathBuf> {
        match self.kind {
            EnvironmentKind::Proton { .. } => Some(PathBuf::from(path)),
            _ => self.windows_to_host(path),
        }
    }

    /// The prefix-wide Wine DLL overrides, keyed by lowercase DLL name.
    pub fn dll_overrides(&self) -> Result<BTreeMap<String, String>> {
        if !self.is_wine() {
            return Ok(BTreeMap::new());
        }
        Ok(self
            .registry_key(Hive::CurrentUser, r"Software\Wine\DllOverrides")?
            .unwrap_or_default())
    }
}

/// Whether a folder looks like a Wine prefix.
pub fn is_wine_prefix(path: &Path) -> bool {
    path.join("drive_c").is_dir() && path.join("system.reg").is_file()
}

/// Finds every environment on this machine: CrossOver bottles, the default
/// Wine prefix, Proton prefixes of the given Steam app ids, and Windows itself.
pub fn discover(steam_app_ids: &[String]) -> Vec<Environment> {
    let mut environments: Vec<Environment> = crossover::default_bottle_dirs()
        .iter()
        .flat_map(|dir| crossover::discover_bottles(dir).unwrap_or_default())
        .collect();
    environments.extend(wine::discover_default_prefix());
    environments.extend(proton::discover(steam_app_ids));
    environments.extend(native::discover());
    environments
}
