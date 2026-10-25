//! Core logic for the mod manager: game definitions, environment discovery,
//! game detection, the mod library and deployment. Has no UI or Tauri dependency so it can be tested on its own
//! and shared by the CLI and the desktop app.

pub mod archive;
pub mod deploy;
pub mod detect;
pub mod env;
pub mod error;
pub mod fs_util;
pub mod game;
pub mod installer;
pub mod library;
pub mod nexus;
mod registry;
pub mod store;
mod vdf;

use serde::Serialize;
use ts_rs::TS;

pub use detect::{DllOverrideStatus, GameInstance, Store};
pub use env::{Environment, EnvironmentKind};
pub use error::{Error, Result};
pub use game::GameDefinition;

/// Everything found in one pass: the environments and the game installs in them.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct ScanResult {
    pub environments: Vec<Environment>,
    pub instances: Vec<GameInstance>,
}

/// Discovers all environments on this machine and detects the given games in them.
pub fn scan(games: &[GameDefinition]) -> ScanResult {
    scan_environments(games, env::discover())
}

/// Detects the given games in an already discovered set of environments.
pub fn scan_environments(games: &[GameDefinition], environments: Vec<Environment>) -> ScanResult {
    let instances = detect::detect_all(games, &environments);
    ScanResult {
        environments,
        instances,
    }
}
