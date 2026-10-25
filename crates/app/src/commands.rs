//! Tauri commands: thin wrappers over `mm-core`.
//!
//! Every command runs its work on a blocking worker thread (`blocking`):
//! the core does disk, network (blocking `reqwest`) and Wine work, none of
//! which may run on the async runtime. A panic in a command comes back to the
//! UI as an error message instead of leaving the call unanswered.

use std::path::PathBuf;

use mm_core::archives::{ArchiveRetention, ModArchiveSize};
use mm_core::collections::{self, Collection};
use mm_core::deploy::{DeployReport, DeploymentCheck};
use mm_core::fomod::Selections;
use mm_core::health::LogReport;
use mm_core::library::{
    ArchiveOrder, FomodRequest, FomodState, ImportReport, InstallOutcome, Loadout, ModMeta,
    Prepared, ProfileExport,
};
use mm_core::nexus::{self, Download, NexusClient, NexusUser, RateLimits};
use mm_core::saves::{BackupReason, SaveBackup};
use mm_core::updates::{self, ChangelogVersion, UpdateSummary};
use mm_core::{GameDefinition, ScanResult};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use ts_rs::TS;

use crate::downloads::{self, LIBRARY_CHANGED, Request};
use crate::state::{AppState, CommandResult, err, lock};

#[derive(Serialize, TS)]
#[ts(export)]
pub struct ImportResult {
    pub file: String,
    pub outcome: Option<InstallOutcome>,
    /// Set when the mod has installer options to choose first.
    pub fomod: Option<FomodRequest>,
    pub error: Option<String>,
}

#[derive(Serialize, TS)]
#[ts(export)]
pub struct SavesInfo {
    /// The save folder, if the game has one here.
    pub save_dir: Option<String>,
    pub auto_backup: bool,
    pub backups: Vec<SaveBackup>,
}

#[derive(Serialize, TS)]
#[ts(export)]
pub struct NexusStatus {
    pub user: Option<NexusUser>,
    /// Set when a key is stored but couldn't be checked.
    pub error: Option<String>,
}

#[derive(Serialize, TS)]
#[ts(export)]
pub struct AppInfo {
    pub version: String,
    pub data_dir: String,
    #[ts(type = "number")]
    pub library_bytes: u64,
    /// Size of the archive store.
    #[ts(type = "number")]
    pub archives_bytes: u64,
    pub crossover_installed: bool,
    /// Problems found when the library was opened, to show once.
    pub warnings: Vec<String>,
}

#[derive(Serialize, TS)]
#[ts(export)]
pub struct ArchivesInfo {
    pub retention: ArchiveRetention,
    /// Revisions per mod whose archives are kept under `LastRevisions`.
    pub revisions_kept: u32,
    #[ts(type = "number")]
    pub total_bytes: u64,
    pub unused_count: u32,
    #[ts(type = "number")]
    pub unused_bytes: u64,
    /// Largest first.
    pub mods: Vec<ModArchiveSize>,
}

#[derive(Serialize, TS)]
#[ts(export)]
pub struct CleanResult {
    pub deleted: u32,
    #[ts(type = "number")]
    pub bytes: u64,
}

/// The result of a library change: the fresh loadout.
type LoadoutResult = CommandResult<Loadout>;

/// Runs `work` on a blocking worker thread with access to the app state.
async fn blocking<T, F>(app: AppHandle, work: F) -> CommandResult<T>
where
    T: Send + 'static,
    F: FnOnce(&AppHandle, &AppState) -> CommandResult<T> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        work(&app, &state)
    })
    .await
    .map_err(|e| format!("Something went wrong inside the app: {e}"))?
}

fn loadout(state: &AppState, instance_id: &str) -> LoadoutResult {
    let (instance, game, _) = state.resolve(instance_id)?;
    lock(&state.library).loadout(game, &instance).map_err(err)
}

fn nexus_client(api_key: &str) -> CommandResult<NexusClient> {
    NexusClient::new(api_key, env!("CARGO_PKG_VERSION")).map_err(err)
}

/// A Nexus client and the game's Nexus domain, when an account is connected
/// and the game is on Nexus.
fn nexus_lookup(game: &GameDefinition) -> Option<(NexusClient, String)> {
    let domain = game.stores.nexus_domain.clone()?;
    let key = nexus::api_key::load().ok().flatten()?;
    Some((nexus_client(&key).ok()?, domain))
}

#[tauri::command]
pub async fn app_info(app: AppHandle) -> CommandResult<AppInfo> {
    blocking(app, |_, state| {
        let library = lock(&state.library);
        Ok(AppInfo {
            version: env!("CARGO_PKG_VERSION").into(),
            data_dir: library.root().display().to_string(),
            library_bytes: library.disk_usage(),
            archives_bytes: library.archives_bytes(),
            crossover_installed: mm_core::env::runner::crossover_wine().is_some(),
            warnings: library.warnings().to_vec(),
        })
    })
    .await
}

#[tauri::command]
pub async fn list_games(app: AppHandle) -> CommandResult<Vec<GameDefinition>> {
    blocking(app, |_, state| Ok(state.games.clone())).await
}

#[tauri::command]
pub async fn scan(app: AppHandle) -> CommandResult<ScanResult> {
    blocking(app, |_, state| Ok(state.rescan())).await
}

#[tauri::command]
pub async fn get_loadout(app: AppHandle, instance_id: String) -> LoadoutResult {
    blocking(app, move |app, state| {
        let (instance, game, _) = state.resolve(&instance_id)?;
        lock(&state.current_instance).insert(instance.game_id.clone(), instance.id.clone());
        let loadout = loadout(state, &instance_id)?;
        // The game was updated since its mods were last checked (also while
        // the app was closed): mods may need updates too.
        let checked = lock(&state.library).last_update_check(&game.id);
        if loadout
            .game
            .updated_at
            .is_some_and(|updated| checked.is_none_or(|c| c < updated))
            && lock(&state.library).auto_update_check()
        {
            spawn_update_check(app, game.clone());
        }
        Ok(loadout)
    })
    .await
}

/// Sent after an automatic update check found updates: `{ game, available }`.
pub const UPDATES_FOUND: &str = "updates-found";

/// Checks a game for mod updates in the background (once at a time per game).
pub fn spawn_update_check(app: &AppHandle, game: GameDefinition) {
    let state = app.state::<AppState>();
    if !lock(&state.checking).insert(game.id.clone()) {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        let state = app.state::<AppState>();
        match run_update_check(&state, &game, true) {
            Ok(summary) => {
                let _ = app.emit(LIBRARY_CHANGED, ());
                if summary.available > 0 {
                    let _ = app.emit(
                        UPDATES_FOUND,
                        serde_json::json!({ "game": game.name, "available": summary.available }),
                    );
                }
            }
            Err(e) => eprintln!("automatic update check of {} skipped: {e}", game.id),
        }
        lock(&state.checking).remove(&game.id);
    });
}

/// At startup: checks every detected game whose last check is a day old.
pub fn check_updates_at_startup(app: &AppHandle) {
    let state = app.state::<AppState>();
    let scan = state.rescan();
    let now = mm_core::library::now();
    let mut games: Vec<GameDefinition> = Vec::new();
    for instance in &scan.instances {
        if let Ok(game) = state.game(&instance.game_id)
            && !games.iter().any(|g| g.id == game.id)
            && game.stores.nexus_domain.is_some()
            && lock(&state.library).update_check_due(&game.id, now)
        {
            games.push(game.clone());
        }
    }
    if games.is_empty() || !matches!(nexus::api_key::load(), Ok(Some(_))) {
        return;
    }
    for game in games {
        spawn_update_check(app, game);
    }
}

/// Mods with a waiting update, per game id (for the sidebar).
#[tauri::command]
pub async fn update_counts(
    app: AppHandle,
) -> CommandResult<std::collections::BTreeMap<String, u32>> {
    blocking(app, |_, state| {
        lock(&state.library).update_counts().map_err(err)
    })
    .await
}

#[tauri::command]
pub async fn auto_update_check(app: AppHandle) -> CommandResult<bool> {
    blocking(app, |_, state| Ok(lock(&state.library).auto_update_check())).await
}

#[tauri::command]
pub async fn set_auto_update_check(app: AppHandle, on: bool) -> CommandResult<()> {
    blocking(app, move |_, state| {
        lock(&state.library).set_auto_update_check(on).map_err(err)
    })
    .await
}

#[tauri::command]
pub async fn import_archives(
    app: AppHandle,
    instance_id: String,
    paths: Vec<String>,
) -> CommandResult<Vec<ImportResult>> {
    blocking(app, move |_, state| {
        let (_, game, _) = state.resolve(&instance_id)?;
        let lookup = nexus_lookup(game);
        Ok(paths
            .into_iter()
            .map(|path| {
                let file = PathBuf::from(&path)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or(path.clone());
                // Ask Nexus about the archive before taking the library lock.
                let (meta, lookup_note) = match &lookup {
                    Some((client, domain)) => match client.identify(domain, &file) {
                        Ok(Some(found)) => (ModMeta::from(found), None),
                        Ok(None) => (ModMeta::default(), None),
                        Err(e) => (
                            ModMeta::default(),
                            Some(format!("Couldn't look this mod up on Nexus Mods: {e}")),
                        ),
                    },
                    None => (ModMeta::default(), None),
                };
                // Copying, hashing and extracting happen without the library
                // lock (each `lock` guard ends with its statement).
                let job = lock(&state.library).begin_install(
                    game,
                    &PathBuf::from(&path),
                    meta,
                    Some(&instance_id),
                );
                let extracted = job.and_then(|job| job.extract(game));
                let prepared = match extracted {
                    Ok(extracted) => lock(&state.library).complete_install(game, extracted),
                    Err(e) => Err(e),
                };
                match prepared {
                    Ok(Prepared::Installed(mut outcome)) => {
                        outcome.warnings.extend(lookup_note);
                        ImportResult {
                            file,
                            outcome: Some(outcome),
                            fomod: None,
                            error: None,
                        }
                    }
                    Ok(Prepared::NeedsChoices(request)) => ImportResult {
                        file,
                        outcome: None,
                        fomod: Some(request),
                        error: None,
                    },
                    Err(e) => ImportResult {
                        file,
                        outcome: None,
                        fomod: None,
                        error: Some(e.to_string()),
                    },
                }
            })
            .collect())
    })
    .await
}

/// Fills in a mod's name, author, image and Nexus file from Nexus Mods, using
/// the mod id in its archive's file name.
#[tauri::command]
pub async fn identify_mod(app: AppHandle, instance_id: String, mod_id: String) -> LoadoutResult {
    blocking(app, move |_, state| {
        let (_, game, _) = state.resolve(&instance_id)?;
        let (client, domain) = nexus_lookup(game)
            .ok_or("Connect your Nexus Mods account in Settings first.")?;
        let record = lock(&state.library)
            .store()
            .get_mod(&mod_id)
            .map_err(err)?
            .ok_or("This mod is no longer installed.")?;
        let archive = record
            .archive_name
            .ok_or("This mod wasn't installed from an archive, so it can't be matched.")?;
        let found = client.identify(&domain, &archive).map_err(err)?.ok_or(
            "The archive's file name has no Nexus mod id. Re-download it with \"Mod Manager Download\" to link it.",
        )?;
        lock(&state.library)
            .apply_nexus_match(&record.id, &found)
            .map_err(err)?;
        loadout(state, &instance_id)
    })
    .await
}

#[tauri::command]
pub async fn set_mod_enabled(
    app: AppHandle,
    instance_id: String,
    mod_id: String,
    enabled: bool,
) -> LoadoutResult {
    blocking(app, move |_, state| {
        let (_, game, _) = state.resolve(&instance_id)?;
        lock(&state.library)
            .set_mod_enabled(game, &instance_id, &mod_id, enabled)
            .map_err(err)?;
        loadout(state, &instance_id)
    })
    .await
}

#[tauri::command]
pub async fn reorder_mods(
    app: AppHandle,
    instance_id: String,
    order: Vec<String>,
) -> LoadoutResult {
    blocking(app, move |_, state| {
        let (_, game, _) = state.resolve(&instance_id)?;
        lock(&state.library)
            .reorder(game, &instance_id, &order)
            .map_err(err)?;
        loadout(state, &instance_id)
    })
    .await
}

#[tauri::command]
pub async fn remove_mod(app: AppHandle, instance_id: String, mod_id: String) -> LoadoutResult {
    blocking(app, move |_, state| {
        lock(&state.library).remove_mod(&mod_id).map_err(err)?;
        loadout(state, &instance_id)
    })
    .await
}

#[tauri::command]
pub async fn rename_mod(
    app: AppHandle,
    instance_id: String,
    mod_id: String,
    name: String,
) -> LoadoutResult {
    blocking(app, move |_, state| {
        lock(&state.library)
            .rename_mod(&mod_id, &name)
            .map_err(err)?;
        loadout(state, &instance_id)
    })
    .await
}

#[tauri::command]
pub async fn mod_files(app: AppHandle, mod_id: String) -> CommandResult<Vec<String>> {
    blocking(app, move |_, state| {
        lock(&state.library).mod_files(&mod_id).map_err(err)
    })
    .await
}

#[tauri::command]
pub async fn create_profile(
    app: AppHandle,
    instance_id: String,
    name: String,
    copy_current: bool,
) -> LoadoutResult {
    blocking(app, move |_, state| {
        {
            let mut library = lock(&state.library);
            let source = if copy_current {
                Some(library.ensure_active_profile(&instance_id).map_err(err)?)
            } else {
                None
            };
            library
                .create_profile(&instance_id, &name, source.as_deref())
                .map_err(err)?;
        }
        loadout(state, &instance_id)
    })
    .await
}

#[tauri::command]
pub async fn rename_profile(
    app: AppHandle,
    instance_id: String,
    profile_id: String,
    name: String,
) -> LoadoutResult {
    blocking(app, move |_, state| {
        lock(&state.library)
            .rename_profile(&profile_id, &name)
            .map_err(err)?;
        loadout(state, &instance_id)
    })
    .await
}

#[tauri::command]
pub async fn delete_profile(
    app: AppHandle,
    instance_id: String,
    profile_id: String,
) -> LoadoutResult {
    blocking(app, move |_, state| {
        lock(&state.library)
            .delete_profile(&instance_id, &profile_id)
            .map_err(err)?;
        loadout(state, &instance_id)
    })
    .await
}

#[tauri::command]
pub async fn switch_profile(
    app: AppHandle,
    instance_id: String,
    profile_id: String,
) -> LoadoutResult {
    blocking(app, move |_, state| {
        lock(&state.library)
            .set_active_profile(&instance_id, &profile_id)
            .map_err(err)?;
        loadout(state, &instance_id)
    })
    .await
}

#[tauri::command]
pub async fn set_archive_order(
    app: AppHandle,
    instance_id: String,
    order: ArchiveOrder,
) -> LoadoutResult {
    blocking(app, move |_, state| {
        lock(&state.library)
            .set_archive_order(&instance_id, order)
            .map_err(err)?;
        loadout(state, &instance_id)
    })
    .await
}

/// The game's logs with errors and warnings, read fresh each time.
#[tauri::command]
pub async fn game_logs(app: AppHandle, instance_id: String) -> CommandResult<Vec<LogReport>> {
    blocking(app, move |_, state| {
        let (instance, game, _) = state.resolve(&instance_id)?;
        lock(&state.library).logs(game, &instance).map_err(err)
    })
    .await
}

#[tauri::command]
pub async fn read_log(
    app: AppHandle,
    instance_id: String,
    relative_path: String,
) -> CommandResult<Vec<String>> {
    blocking(app, move |_, state| {
        let (instance, game, _) = state.resolve(&instance_id)?;
        lock(&state.library)
            .log_tail(game, &instance, &relative_path, 400)
            .map_err(err)
    })
    .await
}

#[tauri::command]
pub async fn deploy(app: AppHandle, instance_id: String) -> CommandResult<DeployReport> {
    blocking(app, move |_, state| {
        let (instance, game, _) = state.resolve(&instance_id)?;
        lock(&state.library).deploy(game, &instance).map_err(err)
    })
    .await
}

/// Deployed files that no longer match what was placed.
#[tauri::command]
pub async fn verify_deployment(
    app: AppHandle,
    instance_id: String,
) -> CommandResult<DeploymentCheck> {
    blocking(app, move |_, state| {
        let (instance, game, _) = state.resolve(&instance_id)?;
        lock(&state.library)
            .verify_deployment(game, &instance)
            .map_err(err)
    })
    .await
}

#[tauri::command]
pub async fn purge(app: AppHandle, instance_id: String) -> CommandResult<DeployReport> {
    blocking(app, move |_, state| {
        let (instance, game, _) = state.resolve(&instance_id)?;
        lock(&state.library).purge(game, &instance).map_err(err)
    })
    .await
}

#[tauri::command]
pub async fn fix_dll_overrides(app: AppHandle, instance_id: String) -> CommandResult<ScanResult> {
    blocking(app, move |_, state| {
        let (instance, _, env) = state.resolve(&instance_id)?;
        let missing: Vec<(String, String)> = instance
            .dll_overrides
            .iter()
            .filter(|d| !d.satisfied)
            .map(|d| (d.dll.clone(), d.required.clone()))
            .collect();
        env.set_dll_overrides(&missing).map_err(err)?;
        Ok(state.rescan())
    })
    .await
}

#[tauri::command]
pub async fn launch_game(app: AppHandle, instance_id: String, modded: bool) -> CommandResult<()> {
    blocking(app, move |_, state| {
        let (instance, game, env) = state.resolve(&instance_id)?;
        let library = lock(&state.library);
        if library.auto_save_backup(&instance.id) {
            // A failed backup shouldn't stop the game; the Saves tab shows backups.
            if let Err(e) =
                library.backup_saves(game, &env, &instance.id, BackupReason::BeforeLaunch)
            {
                eprintln!("save backup before launch failed: {e}");
            }
        }
        drop(library);
        env.launch_game(game, &instance, modded).map_err(err)
    })
    .await
}

// Nexus Mods

#[tauri::command]
pub async fn nexus_status(app: AppHandle) -> CommandResult<NexusStatus> {
    blocking(app, |_, _| {
        let status = match nexus::api_key::load() {
            Ok(Some(key)) => match nexus_client(&key).and_then(|c| c.validate().map_err(err)) {
                Ok(user) => NexusStatus {
                    user: Some(user),
                    error: None,
                },
                Err(e) => NexusStatus {
                    user: None,
                    error: Some(e),
                },
            },
            Ok(None) => NexusStatus {
                user: None,
                error: None,
            },
            Err(e) => NexusStatus {
                user: None,
                error: Some(e.to_string()),
            },
        };
        Ok(status)
    })
    .await
}

/// Checks the key with Nexus before saving it.
#[tauri::command]
pub async fn connect_nexus(app: AppHandle, api_key: String) -> CommandResult<NexusUser> {
    blocking(app, move |_, _| {
        let api_key = api_key.trim();
        if api_key.is_empty() {
            return Err("Paste your API key first.".into());
        }
        if api_key.chars().any(char::is_whitespace) {
            return Err(
                "The key contains spaces or line breaks. Copy it again from Nexus Mods.".into(),
            );
        }
        let user = nexus_client(api_key)?.validate().map_err(err)?;
        nexus::api_key::save(api_key).map_err(err)?;
        Ok(user)
    })
    .await
}

#[tauri::command]
pub async fn disconnect_nexus(app: AppHandle) -> CommandResult<()> {
    blocking(app, |_, _| nexus::api_key::delete().map_err(err)).await
}

#[tauri::command]
pub async fn handle_nxm(app: AppHandle, url: String) -> CommandResult<String> {
    blocking(app, move |app, _| downloads::start_nxm(app, &url)).await
}

#[tauri::command]
pub async fn list_downloads(app: AppHandle) -> CommandResult<Vec<Download>> {
    blocking(app, |_, state| Ok(lock(&state.downloads).clone())).await
}

#[tauri::command]
pub async fn clear_finished_downloads(app: AppHandle) -> CommandResult<Vec<Download>> {
    blocking(app, |app, _| Ok(downloads::clear_finished(app))).await
}

#[tauri::command]
pub async fn retry_download(app: AppHandle, id: String) -> CommandResult<()> {
    blocking(app, move |app, _| downloads::retry(app, &id)).await
}

#[tauri::command]
pub async fn cancel_download(app: AppHandle, id: String) -> CommandResult<()> {
    blocking(app, move |app, _| downloads::cancel(app, &id)).await
}

#[derive(Serialize, TS)]
#[ts(export)]
pub struct UpdateCheckResult {
    pub loadout: Loadout,
    pub summary: UpdateSummary,
}

#[derive(Serialize, TS)]
#[ts(export)]
pub struct ModChangelog {
    /// Newest first.
    pub versions: Vec<ChangelogVersion>,
    #[ts(type = "number")]
    pub fetched_at: i64,
}

/// A mod whose update needs the "Mod Manager Download" button (free accounts).
#[derive(Serialize, TS)]
#[ts(export)]
pub struct UpdateLink {
    pub mod_name: String,
    pub version: Option<String>,
    pub url: String,
}

#[derive(Serialize, TS)]
#[ts(export)]
pub struct UpdateAllResult {
    /// Downloads queued (Premium).
    pub queued: u32,
    /// Files pages to open instead (free accounts).
    pub links: Vec<UpdateLink>,
}

/// Asks Nexus which mods of the install's game have newer files, with as
/// few requests as the last check allows. Nexus is asked without holding the
/// library, so the app stays usable meanwhile. `automatic` checks pause when
/// few Nexus requests are left.
#[tauri::command]
pub async fn check_updates(
    app: AppHandle,
    instance_id: String,
    automatic: bool,
) -> CommandResult<UpdateCheckResult> {
    blocking(app, move |app, state| {
        let (_, game, _) = state.resolve(&instance_id)?;
        let summary = run_update_check(state, game, automatic)?;
        let _ = app.emit(LIBRARY_CHANGED, ());
        Ok(UpdateCheckResult {
            loadout: loadout(state, &instance_id)?,
            summary,
        })
    })
    .await
}

pub fn run_update_check(
    state: &AppState,
    game: &GameDefinition,
    automatic: bool,
) -> CommandResult<UpdateSummary> {
    let domain = game
        .stores
        .nexus_domain
        .clone()
        .ok_or("This game isn't on Nexus Mods.")?;
    let key = nexus::api_key::load()
        .map_err(err)?
        .ok_or("Connect your Nexus Mods account in Settings to check for updates.")?;
    // Automatic checks stop before the remaining requests run low.
    let budget = || {
        if automatic {
            nexus::check_budget(mm_core::library::now())
        } else {
            Ok(())
        }
    };
    budget().map_err(err)?;
    let client = nexus_client(&key)?;
    // Link mods installed from archives first, so they get checked too: by
    // the archive's MD5, then by the mod id in its file name.
    let unlinked = lock(&state.library)
        .unlinked_archives(&game.id)
        .map_err(err)?;
    let mut lookups = 0;
    for (mod_id, md5, archive_name) in unlinked {
        budget().map_err(err)?;
        lookups += 1;
        let found = match client.identify_by_md5(&domain, &md5) {
            Ok(Some(found)) => Some(found),
            Ok(None) => {
                // The file name lookup costs two more requests.
                lookups += 2;
                archive_name
                    .as_deref()
                    .and_then(|name| client.identify(&domain, name).ok().flatten())
            }
            // Offline: the check below would fail the same way.
            Err(e @ mm_core::Error::Network(_)) => return Err(err(e)),
            Err(_) => None,
        };
        let library = lock(&state.library);
        if let Some(found) = &found {
            library.apply_nexus_match(&mod_id, found).map_err(err)?;
        }
        // Without the exact file it still can't be checked: don't ask again for a week.
        if found.is_none_or(|f| f.file_id.is_none()) {
            library.mark_lookup_failed(&mod_id).map_err(err)?;
        }
    }
    let Some(check) = lock(&state.library).update_check(game).map_err(err)? else {
        return Err("This game isn't on Nexus Mods.".into());
    };
    let now = mm_core::library::now();
    let period = updates::period_since(check.last_checked_at, now);
    let result = updates::run(&client, &check, now, &budget);
    let library = lock(&state.library);
    let _ = library.save_rate_limits();
    let mut summary = library
        .apply_update_check(&result.map_err(err)?, period)
        .map_err(err)?;
    summary.requests += lookups;
    Ok(summary)
}

/// The changelog of a mod's update (or its latest entries), from the cache
/// unless `refresh` or it's older than a day.
#[tauri::command]
pub async fn mod_changelog(
    app: AppHandle,
    mod_id: String,
    refresh: bool,
) -> CommandResult<ModChangelog> {
    blocking(app, move |_, state| {
        let record = lock(&state.library)
            .store()
            .get_mod(&mod_id)
            .map_err(err)?
            .ok_or("This mod is no longer installed.")?;
        let game = state.game(&record.game_id)?;
        let (Some(domain), Some(nexus_mod)) =
            (game.stores.nexus_domain.clone(), record.nexus_mod_id)
        else {
            return Err("This mod isn't linked to Nexus Mods, so it has no changelog here.".into());
        };
        let now = mm_core::library::now();
        let cached = lock(&state.library)
            .cached_changelog(&domain, nexus_mod)
            .map_err(err)?
            .filter(|(at, _)| !refresh && now - at < 86_400);
        let (fetched_at, entries) = match cached {
            Some(cached) => cached,
            None => {
                let key = nexus::api_key::load()
                    .map_err(err)?
                    .ok_or("Connect your Nexus Mods account in Settings to read changelogs.")?;
                let entries = nexus_client(&key)?
                    .changelogs(&domain, nexus_mod)
                    .map_err(err)?;
                let library = lock(&state.library);
                library
                    .cache_changelog(&domain, nexus_mod, &entries)
                    .map_err(err)?;
                let _ = library.save_rate_limits();
                (now, entries)
            }
        };
        Ok(ModChangelog {
            versions: updates::relevant_changelog(
                &entries,
                record.version.as_deref(),
                record.update_version.as_deref(),
            ),
            fetched_at,
        })
    })
    .await
}

#[tauri::command]
pub async fn set_mod_pinned(
    app: AppHandle,
    instance_id: String,
    mod_id: String,
    pinned: bool,
) -> LoadoutResult {
    blocking(app, move |_, state| {
        lock(&state.library)
            .set_mod_pinned(&mod_id, pinned)
            .map_err(err)?;
        loadout(state, &instance_id)
    })
    .await
}

/// Skips a mod's pending update file (`ignore`), or stops skipping it.
#[tauri::command]
pub async fn ignore_update(
    app: AppHandle,
    instance_id: String,
    mod_id: String,
    ignore: bool,
) -> LoadoutResult {
    blocking(app, move |_, state| {
        lock(&state.library)
            .ignore_update(&mod_id, ignore)
            .map_err(err)?;
        loadout(state, &instance_id)
    })
    .await
}

/// Updates every mod with a known update: queued downloads for Premium,
/// files pages to open for free accounts.
#[tauri::command]
pub async fn update_all(app: AppHandle, instance_id: String) -> CommandResult<UpdateAllResult> {
    blocking(app, move |app, state| {
        let (_, game, _) = state.resolve(&instance_id)?;
        let domain = game
            .stores
            .nexus_domain
            .clone()
            .ok_or("This game isn't on Nexus Mods.")?;
        let key = nexus::api_key::load()
            .map_err(err)?
            .ok_or("Connect your Nexus Mods account in Settings first.")?;
        let premium = nexus_client(&key)?.validate().map_err(err)?.is_premium;
        let pending: Vec<_> = lock(&state.library)
            .mods_for_game(&game.id)
            .map_err(err)?
            .into_iter()
            .filter(|m| !m.pinned && m.update_file_id.is_some() && m.nexus_mod_id.is_some())
            .collect();
        let mut result = UpdateAllResult {
            queued: 0,
            links: Vec::new(),
        };
        for m in pending {
            let (Some(nexus_mod), Some(file)) = (m.nexus_mod_id, m.update_file_id) else {
                continue;
            };
            if premium {
                downloads::start(
                    app,
                    Request {
                        game_domain: domain.clone(),
                        mod_id: nexus_mod,
                        file_id: file,
                        key: None,
                        expires: None,
                        replaces: Some(m.id.clone()),
                    },
                )?;
                result.queued += 1;
            } else {
                result.links.push(UpdateLink {
                    mod_name: m.name,
                    version: m.update_version,
                    url: format!(
                        "{}?tab=files&file_id={file}",
                        nexus::mod_page_url(&domain, nexus_mod)
                    ),
                });
            }
        }
        Ok(result)
    })
    .await
}

/// The latest Nexus rate limits seen (also from an earlier run).
#[tauri::command]
pub async fn nexus_rate_limits(app: AppHandle) -> CommandResult<Option<RateLimits>> {
    blocking(app, |_, state| Ok(lock(&state.library).load_rate_limits())).await
}

/// Downloads a mod's pending update directly. Premium accounts only; free
/// accounts get the files page to click "Mod Manager Download" there.
#[tauri::command]
pub async fn update_mod(app: AppHandle, mod_id: String) -> CommandResult<String> {
    blocking(app, move |app, state| {
        let record = lock(&state.library)
            .store()
            .get_mod(&mod_id)
            .map_err(err)?
            .ok_or("This mod is no longer installed.")?;
        let game = state.game(&record.game_id)?;
        let (Some(nexus_mod_id), Some(update_file_id), Some(domain)) = (
            record.nexus_mod_id,
            record.update_file_id,
            game.stores.nexus_domain.clone(),
        ) else {
            return Err("No update is known for this mod. Check for updates first.".into());
        };
        downloads::start(
            app,
            Request {
                game_domain: domain,
                mod_id: nexus_mod_id,
                file_id: update_file_id,
                key: None,
                expires: None,
                replaces: Some(record.id),
            },
        )
    })
    .await
}

// Installer options (FOMOD)

#[tauri::command]
pub async fn get_fomod(app: AppHandle, token: String) -> CommandResult<FomodRequest> {
    blocking(app, move |_, state| {
        lock(&state.library).fomod_request(&token).map_err(err)
    })
    .await
}

#[tauri::command]
pub async fn fomod_state(
    app: AppHandle,
    token: String,
    selections: Selections,
) -> CommandResult<FomodState> {
    blocking(app, move |_, state| {
        lock(&state.library)
            .fomod_state(&token, &selections)
            .map_err(err)
    })
    .await
}

/// An installer image as a data URL, so the page can show it.
#[tauri::command]
pub async fn fomod_image(app: AppHandle, token: String, path: String) -> CommandResult<String> {
    blocking(app, move |_, state| {
        use base64::Engine;
        let bytes = lock(&state.library)
            .fomod_image(&token, &path)
            .map_err(err)?;
        let mime = match path
            .rsplit('.')
            .next()
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("png") => "image/png",
            Some("gif") => "image/gif",
            Some("bmp") => "image/bmp",
            Some("webp") => "image/webp",
            _ => "image/jpeg",
        };
        Ok(format!(
            "data:{mime};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        ))
    })
    .await
}

#[tauri::command]
pub async fn finish_fomod(
    app: AppHandle,
    token: String,
    selections: Selections,
) -> CommandResult<InstallOutcome> {
    blocking(app, move |app, state| {
        let game_id = lock(&state.library)
            .fomod_request(&token)
            .map_err(err)?
            .game_id;
        let game = state.game(&game_id)?;
        let job = lock(&state.library)
            .take_fomod(&token, &selections)
            .map_err(err)?;
        let installed = job.stage(game).and_then(|staged| {
            lock(&state.library).complete_install(game, mm_core::library::Extracted::Files(staged))
        });
        let outcome = match installed {
            Ok(Prepared::Installed(outcome)) => outcome,
            other => {
                let message = match other {
                    Err(e) => e.to_string(),
                    _ => "The installer asked again unexpectedly.".into(),
                };
                // The installer is gone; its download can start over from the kept archive.
                if let Some(download) = lock(&state.fomod_downloads).remove(&token) {
                    downloads::update(app, &download, |d| {
                        d.status = mm_core::nexus::DownloadStatus::Failed {
                            message: format!(
                                "Installing failed: {message} Press Retry to choose the options again."
                            ),
                        }
                    });
                }
                return Err(format!("{message} Try installing it again."));
            }
        };
        if let Some(download) = lock(&state.fomod_downloads).remove(&token) {
            let done = mm_core::nexus::DownloadStatus::Done {
                mod_id: outcome.record.id.clone(),
                replaced_existing: outcome.replaced_existing,
                replayed_choices: false,
                warnings: outcome.warnings.clone(),
            };
            downloads::update(app, &download, |d| d.status = done);
        }
        let _ = app.emit(LIBRARY_CHANGED, ());
        Ok(outcome)
    })
    .await
}

#[tauri::command]
pub async fn cancel_fomod(app: AppHandle, token: String) -> CommandResult<()> {
    blocking(app, move |app, state| {
        lock(&state.library).cancel_fomod(&token);
        if let Some(download) = lock(&state.fomod_downloads).remove(&token) {
            downloads::update(app, &download, |d| {
                d.status = mm_core::nexus::DownloadStatus::Failed {
                    message: "Installation cancelled.".into(),
                }
            });
        }
        Ok(())
    })
    .await
}

// Versions

/// One revision of a mod, for its history.
#[derive(Serialize, TS)]
#[ts(export)]
pub struct RevisionInfo {
    pub revision: u32,
    pub version: Option<String>,
    #[ts(type = "number")]
    pub installed_at: i64,
    /// Its archive is kept, so it can be installed again.
    pub archive_kept: bool,
    /// It was installed with installer (FOMOD) choices.
    pub has_choices: bool,
    pub current: bool,
}

/// Either the mod was installed, or its installer waits for choices.
#[derive(Serialize, TS)]
#[ts(export)]
pub struct ReinstallResult {
    pub outcome: Option<InstallOutcome>,
    pub fomod: Option<FomodRequest>,
}

impl From<Prepared> for ReinstallResult {
    fn from(p: Prepared) -> Self {
        match p {
            Prepared::Installed(outcome) => Self {
                outcome: Some(outcome),
                fomod: None,
            },
            Prepared::NeedsChoices(request) => Self {
                outcome: None,
                fomod: Some(request),
            },
        }
    }
}

#[tauri::command]
pub async fn mod_revisions(app: AppHandle, mod_id: String) -> CommandResult<Vec<RevisionInfo>> {
    blocking(app, move |_, state| {
        let library = lock(&state.library);
        let archives: std::collections::HashSet<i64> = library
            .archive_usage()
            .map_err(err)?
            .into_iter()
            .map(|a| a.archive.id)
            .collect();
        let revisions = library.revisions(&mod_id).map_err(err)?;
        let current = revisions.first().map(|r| r.revision);
        Ok(revisions
            .into_iter()
            .map(|r| RevisionInfo {
                current: Some(r.revision) == current,
                revision: r.revision,
                version: r.version,
                installed_at: r.installed_at,
                archive_kept: r.archive_id.is_some_and(|id| archives.contains(&id)),
                has_choices: r.fomod_choices.is_some(),
            })
            .collect())
    })
    .await
}

/// Installs a revision again from its archive with the same choices, or with
/// the installer shown (`change_options`).
#[tauri::command]
pub async fn reinstall_mod(
    app: AppHandle,
    instance_id: String,
    mod_id: String,
    revision: u32,
    change_options: bool,
) -> CommandResult<ReinstallResult> {
    blocking(app, move |app, state| {
        let (_, game, _) = state.resolve(&instance_id)?;
        let job = lock(&state.library)
            .reinstall_job(game, &mod_id, revision, Some(&instance_id), change_options)
            .map_err(err)?;
        let extracted = job.extract(game).map_err(err)?;
        let prepared = lock(&state.library)
            .complete_install(game, extracted)
            .map_err(err)?;
        let _ = app.emit(LIBRARY_CHANGED, ());
        Ok(prepared.into())
    })
    .await
}

#[tauri::command]
pub async fn rollback_mod(
    app: AppHandle,
    instance_id: String,
    mod_id: String,
) -> CommandResult<ReinstallResult> {
    blocking(app, move |app, state| {
        let (_, game, _) = state.resolve(&instance_id)?;
        let job = lock(&state.library)
            .rollback_job(game, &mod_id, Some(&instance_id))
            .map_err(err)?;
        let extracted = job.extract(game).map_err(err)?;
        let prepared = lock(&state.library)
            .complete_install(game, extracted)
            .map_err(err)?;
        let _ = app.emit(LIBRARY_CHANGED, ());
        Ok(prepared.into())
    })
    .await
}

#[tauri::command]
pub async fn set_revisions_kept(app: AppHandle, count: u32) -> CommandResult<ArchivesInfo> {
    blocking(app, move |_, state| {
        let library = lock(&state.library);
        library.set_revisions_kept(count).map_err(err)?;
        archives_info_of(&library)
    })
    .await
}

// Archive store

fn archives_info_of(library: &mm_core::library::Library) -> CommandResult<ArchivesInfo> {
    let usage = library.archive_usage().map_err(err)?;
    Ok(ArchivesInfo {
        retention: library.archive_retention(),
        revisions_kept: library.revisions_kept(),
        total_bytes: usage.iter().map(|u| u.archive.size).sum(),
        unused_count: usage.iter().filter(|u| u.unused).count() as u32,
        unused_bytes: usage
            .iter()
            .filter(|u| u.unused)
            .map(|u| u.archive.size)
            .sum(),
        mods: library.mod_archive_sizes().map_err(err)?,
    })
}

#[tauri::command]
pub async fn archives_info(app: AppHandle) -> CommandResult<ArchivesInfo> {
    blocking(app, |_, state| archives_info_of(&lock(&state.library))).await
}

#[tauri::command]
pub async fn set_archive_retention(
    app: AppHandle,
    retention: ArchiveRetention,
) -> CommandResult<ArchivesInfo> {
    blocking(app, move |_, state| {
        let library = lock(&state.library);
        library.set_archive_retention(retention).map_err(err)?;
        archives_info_of(&library)
    })
    .await
}

#[tauri::command]
pub async fn clean_unused_archives(app: AppHandle) -> CommandResult<CleanResult> {
    blocking(app, |_, state| {
        let (deleted, bytes) = lock(&state.library).clean_unused_archives().map_err(err)?;
        Ok(CleanResult { deleted, bytes })
    })
    .await
}

// Save backups

#[tauri::command]
pub async fn saves_info(app: AppHandle, instance_id: String) -> CommandResult<SavesInfo> {
    blocking(app, move |_, state| {
        let (instance, game, env) = state.resolve(&instance_id)?;
        let library = lock(&state.library);
        Ok(SavesInfo {
            save_dir: library
                .save_dir(game, &env)
                .map(|p| p.display().to_string()),
            auto_backup: library.auto_save_backup(&instance.id),
            backups: library.save_backups(&instance.id),
        })
    })
    .await
}

#[tauri::command]
pub async fn backup_saves(
    app: AppHandle,
    instance_id: String,
) -> CommandResult<Option<SaveBackup>> {
    blocking(app, move |_, state| {
        let (instance, game, env) = state.resolve(&instance_id)?;
        lock(&state.library)
            .backup_saves(game, &env, &instance.id, BackupReason::Manual)
            .map_err(err)
    })
    .await
}

#[tauri::command]
pub async fn restore_saves(
    app: AppHandle,
    instance_id: String,
    backup_id: String,
) -> CommandResult<()> {
    blocking(app, move |_, state| {
        let (instance, game, env) = state.resolve(&instance_id)?;
        lock(&state.library)
            .restore_saves(game, &env, &instance.id, &backup_id)
            .map_err(err)
    })
    .await
}

#[tauri::command]
pub async fn delete_save_backup(
    app: AppHandle,
    instance_id: String,
    backup_id: String,
) -> CommandResult<()> {
    blocking(app, move |_, state| {
        lock(&state.library)
            .delete_save_backup(&instance_id, &backup_id)
            .map_err(err)
    })
    .await
}

#[tauri::command]
pub async fn set_auto_save_backup(
    app: AppHandle,
    instance_id: String,
    on: bool,
) -> CommandResult<()> {
    blocking(app, move |_, state| {
        lock(&state.library)
            .set_auto_save_backup(&instance_id, on)
            .map_err(err)
    })
    .await
}

// Profile sharing

#[tauri::command]
pub async fn export_profile(
    app: AppHandle,
    instance_id: String,
    profile_id: String,
    path: String,
) -> CommandResult<()> {
    blocking(app, move |_, state| {
        let (_, game, _) = state.resolve(&instance_id)?;
        let export = lock(&state.library)
            .export_profile(game, &instance_id, &profile_id)
            .map_err(err)?;
        let json = serde_json::to_string_pretty(&export).map_err(err)?;
        std::fs::write(&path, json).map_err(|e| format!("Couldn't save the file: {e}"))
    })
    .await
}

#[tauri::command]
pub async fn import_profile(
    app: AppHandle,
    instance_id: String,
    path: String,
) -> CommandResult<ImportReport> {
    blocking(app, move |_, state| {
        let (_, game, _) = state.resolve(&instance_id)?;
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("Couldn't read the file: {e}"))?;
        let export: ProfileExport = serde_json::from_str(&text)
            .map_err(|_| "This file isn't a Mod Manager profile.".to_string())?;
        lock(&state.library)
            .import_profile(game, &instance_id, &export)
            .map_err(err)
    })
    .await
}

// Collections

#[tauri::command]
pub async fn open_collection(app: AppHandle, link: String) -> CommandResult<Collection> {
    blocking(app, move |_, state| {
        let link = collections::parse_collection_link(&link).map_err(err)?;
        if state.game_by_nexus_domain(&link.game_domain).is_none() {
            return Err(format!(
                "Collections for '{}' aren't supported yet.",
                link.game_domain
            ));
        }
        let key = nexus::api_key::load().ok().flatten();
        collections::fetch_collection(key.as_deref(), &link, env!("CARGO_PKG_VERSION")).map_err(err)
    })
    .await
}

/// Queues downloads for the chosen files of a collection. Downloading
/// without visiting each mod's page needs a Premium account.
#[tauri::command]
pub async fn install_collection(
    app: AppHandle,
    game_domain: String,
    files: Vec<(u32, u32)>,
) -> CommandResult<u32> {
    blocking(app, move |app, _| {
        let key = nexus::api_key::load()
            .map_err(err)?
            .ok_or("Connect your Nexus Mods account in Settings first.")?;
        let user = nexus_client(&key)?.validate().map_err(err)?;
        if !user.is_premium {
            return Err("Installing a whole collection at once needs Nexus Mods Premium. Use the Get buttons to download each mod from its page instead.".into());
        }
        let mut queued = 0;
        for (mod_id, file_id) in files {
            downloads::start(
                app,
                Request {
                    game_domain: game_domain.clone(),
                    mod_id,
                    file_id,
                    key: None,
                    expires: None,
                    replaces: None,
                },
            )?;
            queued += 1;
        }
        Ok(queued)
    })
    .await
}
