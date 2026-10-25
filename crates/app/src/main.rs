//! Desktop app. Only exposes `mm-core` to the UI as Tauri commands; all logic
//! lives in the core so the UI layer stays replaceable.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod downloads;
mod state;

use mm_core::game;
use mm_core::library::Library;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_deep_link::DeepLinkExt;

use crate::state::AppState;

/// Sent to the UI when an `nxm://` link couldn't be started.
const NXM_ERROR: &str = "nxm-error";
/// Sent to the UI with an `nxm://` collection link to show.
const OPEN_COLLECTION: &str = "open-collection";

fn handle_links(app: &AppHandle, urls: impl IntoIterator<Item = String>) {
    for url in urls.into_iter().filter(|u| u.starts_with("nxm:")) {
        // Collection links open the collection in the app for review.
        if mm_core::collections::parse_collection_link(&url).is_ok() {
            let _ = app.emit(OPEN_COLLECTION, url);
        } else if let Err(message) = downloads::start_nxm(app, &url) {
            let _ = app.emit(NXM_ERROR, message);
        }
    }
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn main() {
    tauri::Builder::default()
        // Must be first: a second launch (e.g. from an nxm link on Windows and
        // Linux) hands its arguments to the running app and exits.
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            handle_links(app, args);
        }))
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .setup(|app| {
            let games = game::builtin_games()?;
            let library = Library::open(&Library::default_root())?;
            // Background Nexus work respects the budget left from the last run.
            library.load_rate_limits();
            app.manage(AppState::new(library, games));
            downloads::start_workers(app.handle());
            let handle = app.handle().clone();
            // After the window is up; the keychain may ask first.
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(5));
                commands::check_updates_at_startup(&handle);
            });

            #[cfg(any(target_os = "linux", windows))]
            if let Err(e) = app.deep_link().register_all() {
                eprintln!("couldn't register the nxm:// handler: {e}");
            }
            let handle = app.handle().clone();
            app.deep_link().on_open_url(move |event| {
                handle_links(&handle, event.urls().into_iter().map(|u| u.to_string()));
            });
            if let Ok(Some(urls)) = app.deep_link().get_current() {
                let handle = app.handle().clone();
                // Let the UI subscribe to download events first.
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_secs(2));
                    handle_links(&handle, urls.into_iter().map(|u| u.to_string()));
                });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::app_info,
            commands::list_games,
            commands::scan,
            commands::get_loadout,
            commands::import_archives,
            commands::set_mod_enabled,
            commands::reorder_mods,
            commands::remove_mod,
            commands::rename_mod,
            commands::identify_mod,
            commands::mod_files,
            commands::create_profile,
            commands::rename_profile,
            commands::delete_profile,
            commands::switch_profile,
            commands::set_archive_order,
            commands::game_logs,
            commands::read_log,
            commands::deploy,
            commands::purge,
            commands::verify_deployment,
            commands::fix_dll_overrides,
            commands::launch_game,
            commands::nexus_status,
            commands::connect_nexus,
            commands::disconnect_nexus,
            commands::handle_nxm,
            commands::list_downloads,
            commands::clear_finished_downloads,
            commands::retry_download,
            commands::cancel_download,
            commands::check_updates,
            commands::mod_changelog,
            commands::set_mod_pinned,
            commands::ignore_update,
            commands::update_all,
            commands::nexus_rate_limits,
            commands::update_counts,
            commands::auto_update_check,
            commands::set_auto_update_check,
            commands::update_mod,
            commands::get_fomod,
            commands::fomod_state,
            commands::fomod_image,
            commands::finish_fomod,
            commands::cancel_fomod,
            commands::saves_info,
            commands::backup_saves,
            commands::restore_saves,
            commands::delete_save_backup,
            commands::set_auto_save_backup,
            commands::export_profile,
            commands::import_profile,
            commands::open_collection,
            commands::install_collection,
            commands::archives_info,
            commands::set_archive_retention,
            commands::clean_unused_archives,
            commands::mod_revisions,
            commands::reinstall_mod,
            commands::rollback_mod,
            commands::set_revisions_kept,
            commands::fetch_requirements,
            commands::add_dependency,
            commands::remove_dependency,
            commands::get_missing,
            commands::preflight,
            commands::refresh_mod_status,
        ])
        .run(tauri::generate_context!())
        .expect("failed to start the app");
}
