//! The download queue: Nexus downloads run on a few worker threads and the
//! UI follows them through `downloads-changed` events. The queue is kept in
//! the library, so it continues after a restart; the rules (what runs next,
//! retries) are in `mm_core::queue`.

use std::time::{Duration, Instant};

use mm_core::archives::{ArchiveSource, Ingest};
use mm_core::error::Error;
use mm_core::installer::sanitize_folder_name;
use mm_core::library::{ModMeta, Prepared, now};
use mm_core::nexus::{self, Download, DownloadStatus, NexusClient};
use mm_core::queue;
use tauri::{AppHandle, Emitter, Manager};

use crate::state::{AppState, CommandResult, err, lock};

pub const DOWNLOADS_CHANGED: &str = "downloads-changed";
pub const LIBRARY_CHANGED: &str = "library-changed";
/// A download has installer options to choose; carries the `FomodRequest`.
pub const FOMOD_REQUESTED: &str = "fomod-requested";

/// How often a running download's progress is saved to the library.
const SAVE_EVERY: Duration = Duration::from_secs(2);

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
    nexus::api_key::load()
        .map_err(err)?
        .ok_or("Connect your Nexus Mods account in Settings to download mods.")?;

    let download = Download {
        id: uuid::Uuid::new_v4().to_string(),
        game_id: game.id.clone(),
        game_domain: request.game_domain,
        instance_id: state.target_instance(&game.id),
        name: format!("Mod {}", request.mod_id),
        nexus_mod_id: request.mod_id,
        nexus_file_id: request.file_id,
        status: DownloadStatus::Queued,
        replaces: request.replaces,
        key: request.key,
        expires: request.expires,
        created_at: now(),
        ..Default::default()
    };
    let id = download.id.clone();
    save(&state, &download);
    lock(&state.downloads).insert(0, download);
    state.queue_wake.notify_one();
    notify(app);
    Ok(id)
}

/// Loads the queue left by the last run and starts the workers.
pub fn start_workers(app: &AppHandle) {
    let state = app.state::<AppState>();
    let saved = lock(&state.library).store().downloads().unwrap_or_default();
    let mut restored = queue::restore(saved, now());
    for d in &restored {
        save(&state, d);
    }
    // Newest first, as the UI lists them.
    restored.reverse();
    *lock(&state.downloads) = restored;
    for _ in 0..queue::WORKERS {
        let app = app.clone();
        std::thread::spawn(move || worker(&app));
    }
    notify(app);
}

fn worker(app: &AppHandle) {
    let state = app.state::<AppState>();
    loop {
        let job = {
            let mut downloads = lock(&state.downloads);
            match queue::next_ready(&downloads, now()) {
                Some(i) => {
                    let d = &mut downloads[i];
                    d.status = if d.archive_id.is_some() {
                        DownloadStatus::Installing
                    } else {
                        DownloadStatus::Downloading
                    };
                    d.retry_at = None;
                    Some(d.clone())
                }
                None => {
                    // Woken by a new download, or each second for retries.
                    let _ = state
                        .queue_wake
                        .wait_timeout(downloads, Duration::from_secs(1));
                    None
                }
            }
        };
        let Some(download) = job else { continue };
        save(&state, &download);
        notify(app);
        let id = download.id.clone();
        match run(app, download) {
            Ok(status) => update(app, &id, |d| d.status = status),
            Err(e) => update(app, &id, |d| {
                if e.to_string() == nexus::CANCELLED {
                    d.status = DownloadStatus::Failed {
                        message: nexus::CANCELLED.into(),
                    };
                } else {
                    queue::record_failure(d, &e, now());
                }
            }),
        }
        let _ = app.emit(LIBRARY_CHANGED, ());
    }
}

fn nexus_error(message: impl Into<String>) -> Error {
    Error::Nexus(message.into())
}

/// One attempt: download (continuing a partial file) into the archive
/// store, then install. A download whose archive is already stored only
/// installs.
fn run(app: &AppHandle, download: Download) -> Result<DownloadStatus, Error> {
    let state = app.state::<AppState>();
    let game = state.game(&download.game_id).map_err(nexus_error)?.clone();
    let api_key = nexus::api_key::load()?.ok_or_else(|| {
        nexus_error("Connect your Nexus Mods account in Settings to download mods.")
    })?;
    let client = NexusClient::new(&api_key, env!("CARGO_PKG_VERSION"))?;
    let domain = &download.game_domain;
    let (mod_id, file_id) = (download.nexus_mod_id, download.nexus_file_id);
    let info = client.mod_info(domain, mod_id)?;
    let file = client.file_info(domain, mod_id, file_id)?;
    let name = nexus::display_name(&info, Some(&file)).unwrap_or_else(|| format!("Mod {mod_id}"));
    let file_name = file
        .file_name
        .clone()
        .unwrap_or_else(|| format!("{name}-{mod_id}-{file_id}.zip"));

    let archive_id = match download.archive_id {
        Some(id) => id,
        None => {
            if queue::link_expired(&download, now()) {
                return Err(nexus_error(queue::EXPIRED_MESSAGE));
            }
            update(app, &download.id, |d| {
                d.name = name.clone();
                d.version = file.version.clone().or(info.version.clone());
                d.file_name = Some(file_name.clone());
                d.bytes_total = file.size_kb.map(|kb| kb * 1024);
            });
            let url = client
                .download_url(domain, mod_id, file_id, download.key.as_deref(), download.expires)
                .map_err(|e| match e {
                    Error::Nexus(m) if download.key.is_none() => Error::Nexus(format!(
                        "{m} Free accounts must use the \"Mod Manager Download\" button on the mod page."
                    )),
                    other => other,
                })?;
            let dest = lock(&state.library).downloads_dir().join(format!(
                "{}-{}",
                download.id,
                sanitize_folder_name(&file_name)
            ));
            let (mut last_emit, mut last_save) = (Instant::now(), Instant::now());
            let hashes = client.download(&url, &dest, |done, total| {
                if last_emit.elapsed() > Duration::from_millis(200) {
                    last_emit = Instant::now();
                    let persist = last_save.elapsed() > SAVE_EVERY;
                    if persist {
                        last_save = Instant::now();
                    }
                    update_with(app, &download.id, persist, |d| {
                        d.bytes_done = done;
                        d.bytes_total = total.or(d.bytes_total);
                    });
                }
                !lock(&state.cancelled).contains(&download.id)
            })?;
            // Kept in the archive store, so the mod can be reinstalled or rolled back.
            let stored = lock(&state.library).store_archive(
                &dest,
                Some(&file_name),
                ArchiveSource::Nexus,
                Ingest::Move,
                Some(hashes),
                (Some(mod_id), Some(file_id)),
            )?;
            update(app, &download.id, |d| {
                d.archive_id = Some(stored.id);
                d.bytes_done = d.bytes_total.unwrap_or(d.bytes_done);
                d.status = DownloadStatus::Installing;
            });
            stored.id
        }
    };

    let mut library = lock(&state.library);
    // A download that is the pending update of an installed mod replaces it.
    let replaces = download.replaces.clone().or_else(|| {
        library
            .mods_for_game(&game.id)
            .ok()?
            .into_iter()
            .find(|m| m.nexus_mod_id == Some(mod_id) && m.update_file_id == Some(file_id))
            .map(|m| m.id)
    });
    let meta = ModMeta {
        name: Some(name),
        version: file.version.or(info.version),
        author: info.author,
        summary: info.summary,
        picture_url: info.picture_url,
        nexus_mod_id: Some(mod_id),
        nexus_file_id: Some(file_id),
        replaces,
        archive_id: Some(archive_id),
        ..Default::default()
    };
    let archive_path = library.downloads_dir().join(&file_name);
    match library.prepare_archive(&game, &archive_path, meta, download.instance_id.as_deref())? {
        Prepared::Installed(outcome) => Ok(DownloadStatus::Done {
            mod_id: outcome.record.id,
            replaced_existing: outcome.replaced_existing,
            replayed_choices: outcome.replayed_choices,
            warnings: outcome.warnings,
        }),
        Prepared::NeedsChoices(request) => {
            lock(&state.fomod_downloads).insert(request.token.clone(), download.id.clone());
            let _ = app.emit(FOMOD_REQUESTED, request.clone());
            Ok(DownloadStatus::AwaitingChoices {
                token: request.token,
            })
        }
    }
}

/// Puts a failed download back in the queue. An expired `nxm://` key is
/// dropped: Premium accounts download without one.
pub fn retry(app: &AppHandle, id: &str) -> CommandResult<()> {
    let state = app.state::<AppState>();
    lock(&state.cancelled).remove(id);
    let mut found = false;
    update(app, id, |d| {
        if matches!(d.status, DownloadStatus::Failed { .. }) {
            found = true;
            if queue::link_expired(d, now()) {
                d.key = None;
                d.expires = None;
            }
            d.status = DownloadStatus::Queued;
            d.attempts = 0;
            d.retry_at = None;
        }
    });
    if !found {
        return Err("Only failed downloads can be retried.".into());
    }
    state.queue_wake.notify_one();
    Ok(())
}

/// Stops a waiting or running download; a partial file is kept for a retry.
pub fn cancel(app: &AppHandle, id: &str) -> CommandResult<()> {
    let state = app.state::<AppState>();
    let mut running = false;
    update(app, id, |d| match d.status {
        DownloadStatus::Queued => {
            d.status = DownloadStatus::Failed {
                message: nexus::CANCELLED.into(),
            }
        }
        DownloadStatus::Downloading => running = true,
        _ => {}
    });
    if running {
        lock(&state.cancelled).insert(id.to_string());
    }
    Ok(())
}

/// Forgets finished and failed downloads, also in the library.
pub fn clear_finished(app: &AppHandle) -> Vec<Download> {
    let state = app.state::<AppState>();
    let mut downloads = lock(&state.downloads);
    let library = lock(&state.library);
    downloads.retain(|d| {
        let finished = matches!(
            d.status,
            DownloadStatus::Done { .. } | DownloadStatus::Failed { .. }
        );
        if finished {
            let _ = library.store().delete_download(&d.id);
            // A partial file of a failed download isn't needed anymore.
            if let Some(name) = &d.file_name {
                let dest = library.downloads_dir().join(format!(
                    "{}-{}",
                    d.id,
                    sanitize_folder_name(name)
                ));
                let _ = std::fs::remove_file(nexus::part_path(&dest));
            }
        }
        !finished
    });
    downloads.clone()
}

pub fn update(app: &AppHandle, id: &str, change: impl FnOnce(&mut Download)) {
    update_with(app, id, true, change)
}

fn update_with(app: &AppHandle, id: &str, persist: bool, change: impl FnOnce(&mut Download)) {
    let state = app.state::<AppState>();
    let changed = {
        let mut downloads = lock(&state.downloads);
        downloads.iter_mut().find(|d| d.id == id).map(|d| {
            change(d);
            d.clone()
        })
    };
    if let Some(d) = changed
        && persist
    {
        save(&state, &d);
    }
    notify(app);
}

fn save(state: &AppState, download: &Download) {
    if let Err(e) = lock(&state.library).store().save_download(download) {
        eprintln!("couldn't save the download queue: {e}");
    }
}

fn notify(app: &AppHandle) {
    let state = app.state::<AppState>();
    let downloads = lock(&state.downloads).clone();
    let _ = app.emit(DOWNLOADS_CHANGED, downloads);
}
