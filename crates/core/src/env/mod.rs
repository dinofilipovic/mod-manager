//! Environments are the places a Windows game can run: CrossOver bottles and
//! plain Wine prefixes today; native Windows and Proton prefixes later.
//!
//! All of them share the Wine prefix layout (`drive_c`, `dosdevices`,
//! `system.reg`, `user.reg`), so path and registry handling lives here and the
//! submodules only know how to find them and read their metadata.

pub mod crossover;
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
        let (drive, rest) = windows_path.split_once(':')?;
        if drive.len() != 1 || !drive.chars().all(|c| c.is_ascii_alphabetic()) {
            return None;
        }
        let drive = drive.to_ascii_lowercase();
        let mapped = self.prefix.join("dosdevices").join(format!("{drive}:"));
        let base = if mapped.exists() {
            mapped
        } else if drive == "c" {
            self.c_drive()
        } else {
            return None;
        };
        Some(resolve_case_insensitive(&base, rest).unwrap_or_else(|| {
            rest.split(['/', '\\'])
                .filter(|c| !c.is_empty())
                .fold(base, |path, c| path.join(c))
        }))
    }

    /// Reads the string values of a registry key, or `None` if it does not exist.
    pub fn registry_key(&self, hive: Hive, key: &str) -> Result<Option<BTreeMap<String, String>>> {
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

    /// The prefix-wide Wine DLL overrides, keyed by lowercase DLL name.
    pub fn dll_overrides(&self) -> Result<BTreeMap<String, String>> {
        Ok(self
            .registry_key(Hive::CurrentUser, r"Software\Wine\DllOverrides")?
            .unwrap_or_default())
    }
}

/// Whether a folder looks like a Wine prefix.
pub fn is_wine_prefix(path: &Path) -> bool {
    path.join("drive_c").is_dir() && path.join("system.reg").is_file()
}

/// Finds every environment on this machine: all CrossOver bottles plus the
/// default Wine prefix, if any.
pub fn discover() -> Vec<Environment> {
    let mut environments: Vec<Environment> = crossover::default_bottle_dirs()
        .iter()
        .flat_map(|dir| crossover::discover_bottles(dir).unwrap_or_default())
        .collect();
    environments.extend(wine::discover_default_prefix());
    environments
}
