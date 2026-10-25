//! The download queue: Nexus downloads run on background threads and the UI
//! follows them through `downloads-changed` events.

use std::time::{Duration, Instant};

use mm_core::archives::{ArchiveSource, Ingest};
use mm_core::installer::sanitize_folder_name;
use mm_core::library::{ModMeta, Prepared};
use mm_core::nexus::{self, Download, DownloadStatus, NexusClient};
use tauri::{AppHandle, Emitter, Manager};

use crate::state::{AppState, CommandResult, err, lock};

pub const DOWNLOADS_CHANGED: &str = "downloads-changed";
pub const LIBRARY_CHANGED: &str = "library-changed";
/// A download has installer options to choose; carries the `FomodRequest`.
pub const FOMOD_REQUESTED: &str = "fomod-requested";

/// What to fetch: from an `nxm://` link, or a file a premium account may
/// download directly (updates).
pub struct Request {
    pub game_domain: String,
    pub mod_id: u32,
    pub file_id: u32,
    pub key: Option<String>,
    pub expires: Option<u64>,
    /// Installed mod this download updates.
    pub replaces: Option<String>,
}

pub fn start_nxm(app: &AppHandle, link: &str) -> CommandResult<String> {
    let link = nexus::parse_nxm(link).map_err(err)?;
    start(
        app,
        Request {
            game_domain: link.game_domain,
            mod_id: link.mod_id,
            file_id: link.file_id,
            key: link.key,
            expires: link.expires,
            replaces: None,
        },
    )
}

/// Queues a download and returns its id. Fails fast if the game isn't
/// supported or no Nexus account is connected.
pub fn start(app: &AppHandle, request: Request) -> CommandResult<String> {
    let state = app.state::<AppState>();
    let game = state
        .game_by_nexus_domain(&request.game_domain)
        .ok_or_else(|| format!("Mods for '{}' aren't supported yet.", request.game_domain))?
        .clone();
    let api_key = nexus::api_key::load()
        .map_err(err)?
        .ok_or("Connect your Nexus Mods account in Settings to download mods.")?;

    let download = Download {
        id: uuid::Uuid::new_v4().to_string(),
        game_id: game.id.clone(),
        instance_id: state.target_instance(&game.id),
        name: format!("Mod {}", request.mod_id),
        version: None,
        file_name: None,
        nexus_mod_id: request.mod_id,
        nexus_file_id: request.file_id,
        bytes_done: 0,
        bytes_total: None,
        status: DownloadStatus::Queued,
    };
    let id = download.id.clone();
    lock(&state.downloads).insert(0, download);
    notify(app);

    let app = app.clone();
    let download_id = id.clone();
    std::thread::spawn(move || {
        let status = match run(&app, &download_id, &game, &api_key, request) {
            Ok(status) => status,
            Err(message) => DownloadStatus::Failed { message },
        };
        update(&app, &download_id, |d| d.status = status);
        let _ = app.emit(LIBRARY_CHANGED, ());
    });
    Ok(id)
}

fn run(
    app: &AppHandle,
    id: &str,
    game: &mm_core::GameDefinition,
    api_key: &str,
    request: Request,
) -> CommandResult<DownloadStatus> {
    let client = NexusClient::new(api_key, env!("CARGO_PKG_VERSION")).map_err(err)?;
    let domain = &request.game_domain;
    let info = client.mod_info(domain, request.mod_id).map_err(err)?;
    let file = client
        .file_info(domain, request.mod_id, request.file_id)
        .map_err(err)?;
    let name = nexus::display_name(&info, Some(&file))
        .unwrap_or_else(|| format!("Mod {}", request.mod_id));
    update(app, id, |d| {
        d.name = name.clone();
        d.version = file.version.clone().or(info.version.clone());
        d.file_name = file.file_name.clone();
        d.bytes_total = file.size_kb.map(|kb| kb * 1024);
        d.status = DownloadStatus::Downloading;
    });

    let url = client
        .download_url(domain, request.mod_id, request.file_id, request.key.as_deref(), request.expires)
        .map_err(|e| {
            if request.key.is_none() {
                format!("{e} Free accounts must use the \"Mod Manager Download\" button on the mod page.")
            } else {
                e.to_string()
            }
        })?;

    let state = app.state::<AppState>();
    let dest_dir = lock(&state.library).downloads_dir();
    let file_name = file
        .file_name
        .clone()
        .unwrap_or_else(|| format!("{}-{}-{}.zip", name, request.mod_id, request.file_id));
    let dest = dest_dir.join(sanitize_folder_name(&file_name));
    let mut last_emit = Instant::now();
    let hashes = client
        .download(&url, &dest, |done, total| {
            if last_emit.elapsed() > Duration::from_millis(200) {
                last_emit = Instant::now();
                update(app, id, |d| {
                    d.bytes_done = done;
                    d.bytes_total = total.or(d.bytes_total);
                });
            }
        })
        .map_err(err)?;

    update(app, id, |d| {
        d.bytes_done = d.bytes_total.unwrap_or(d.bytes_done);
        d.status = DownloadStatus::Installing;
    });
    let instance_id = lock(&state.downloads)
        .iter()
        .find(|d| d.id == id)
        .and_then(|d| d.instance_id.clone());

    let mut library = lock(&state.library);
    // Kept in the archive store, so the mod can be reinstalled or rolled back.
    let stored = library
        .store_archive(
            &dest,
            Some(&file_name),
            ArchiveSource::Nexus,
            Ingest::Move,
            Some(hashes),
            (Some(request.mod_id), Some(request.file_id)),
        )
        .map_err(err)?;
    // A download that is the pending update of an installed mod replaces it.
    let replaces = request.replaces.or_else(|| {
        library
            .mods_for_game(&game.id)
            .ok()?
            .into_iter()
            .find(|m| {
                m.nexus_mod_id == Some(request.mod_id) && m.update_file_id == Some(request.file_id)
            })
            .map(|m| m.id)
    });
    let meta = ModMeta {
        name: Some(name),
        version: file.version.or(info.version),
        author: info.author,
        summary: info.summary,
        picture_url: info.picture_url,
        nexus_mod_id: Some(request.mod_id),
        nexus_file_id: Some(request.file_id),
        replaces,
        archive_id: Some(stored.id),
        ..Default::default()
    };
    let prepared = library
        .prepare_archive(game, &dest, meta, instance_id.as_deref())
        .map_err(err);
    match prepared? {
        Prepared::Installed(outcome) => Ok(DownloadStatus::Done {
            mod_id: outcome.record.id,
            replaced_existing: outcome.replaced_existing,
            replayed_choices: outcome.replayed_choices,
            warnings: outcome.warnings,
        }),
        Prepared::NeedsChoices(request) => {
            lock(&state.fomod_downloads).insert(request.token.clone(), id.to_string());
            let _ = app.emit(FOMOD_REQUESTED, request.clone());
            Ok(DownloadStatus::AwaitingChoices {
                token: request.token,
            })
        }
    }
}

pub fn update(app: &AppHandle, id: &str, change: impl FnOnce(&mut Download)) {
    let state = app.state::<AppState>();
    if let Some(d) = lock(&state.downloads).iter_mut().find(|d| d.id == id) {
        change(d);
    }
    notify(app);
}

fn notify(app: &AppHandle) {
    let state = app.state::<AppState>();
    let downloads = lock(&state.downloads).clone();
    let _ = app.emit(DOWNLOADS_CHANGED, downloads);
}
