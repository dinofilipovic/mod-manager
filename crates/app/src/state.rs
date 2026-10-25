//! Shared app state: the mod library, the last scan and the download queue.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

use mm_core::library::Library;
use mm_core::nexus::Download;
use mm_core::{Environment, GameDefinition, GameInstance, ScanResult};

pub struct AppState {
    pub library: Mutex<Library>,
    pub games: Vec<GameDefinition>,
    pub scan: Mutex<Option<ScanResult>>,
    pub downloads: Mutex<Vec<Download>>,
    /// The install the user is looking at, per game; `nxm://` downloads go there.
    pub current_instance: Mutex<HashMap<String, String>>,
}

/// Errors cross the IPC boundary as plain messages.
pub type CommandResult<T> = Result<T, String>;

pub fn err(e: impl ToString) -> String {
    e.to_string()
}

pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // A panic while holding the lock leaves the data usable; keep going.
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl AppState {
    pub fn new(library: Library, games: Vec<GameDefinition>) -> Self {
        Self {
            library: Mutex::new(library),
            games,
            scan: Mutex::new(None),
            downloads: Mutex::new(Vec::new()),
            current_instance: Mutex::new(HashMap::new()),
        }
    }

    pub fn rescan(&self) -> ScanResult {
        let result = mm_core::scan(&self.games);
        *lock(&self.scan) = Some(result.clone());
        result
    }

    fn scan_result(&self) -> ScanResult {
        if let Some(result) = lock(&self.scan).clone() {
            return result;
        }
        self.rescan()
    }

    pub fn game(&self, id: &str) -> CommandResult<&GameDefinition> {
        self.games
            .iter()
            .find(|g| g.id == id)
            .ok_or_else(|| format!("unknown game {id}"))
    }

    pub fn game_by_nexus_domain(&self, domain: &str) -> Option<&GameDefinition> {
        self.games
            .iter()
            .find(|g| g.stores.nexus_domain.as_deref() == Some(domain))
    }

    /// The instance, its game and its environment.
    pub fn resolve(
        &self,
        instance_id: &str,
    ) -> CommandResult<(GameInstance, &GameDefinition, Environment)> {
        let scan = self.scan_result();
        let instance = scan
            .instances
            .iter()
            .find(|i| i.id == instance_id)
            .cloned()
            .ok_or("This game install is no longer found. Rescan to refresh.")?;
        let env = scan
            .environments
            .iter()
            .find(|e| e.id == instance.environment_id)
            .cloned()
            .ok_or("The bottle for this install is no longer found.")?;
        let game = self.game(&instance.game_id)?;
        Ok((instance, game, env))
    }

    /// Where a download for `game_id` should be installed.
    pub fn target_instance(&self, game_id: &str) -> Option<String> {
        if let Some(id) = lock(&self.current_instance).get(game_id) {
            return Some(id.clone());
        }
        self.scan_result()
            .instances
            .iter()
            .find(|i| i.game_id == game_id)
            .map(|i| i.id.clone())
    }
}
