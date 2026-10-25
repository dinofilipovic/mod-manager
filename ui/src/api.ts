// Typed wrappers around the Tauri commands in crates/app/src/commands.rs.
// The types are generated from the Rust structs (`npm run bindings`).
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import { openUrl, revealItemInDir } from "@tauri-apps/plugin-opener";
import type { AppInfo } from "./bindings/AppInfo";
import type { DeployReport } from "./bindings/DeployReport";
import type { Download } from "./bindings/Download";
import type { GameDefinition } from "./bindings/GameDefinition";
import type { ImportResult } from "./bindings/ImportResult";
import type { ArchiveOrder } from "./bindings/ArchiveOrder";
import type { Collection } from "./bindings/Collection";
import type { FomodRequest } from "./bindings/FomodRequest";
import type { FomodState } from "./bindings/FomodState";
import type { ImportReport } from "./bindings/ImportReport";
import type { InstallOutcome } from "./bindings/InstallOutcome";
import type { SaveBackup } from "./bindings/SaveBackup";
import type { SavesInfo } from "./bindings/SavesInfo";
import type { Selections } from "./bindings/Selections";
import type { Loadout } from "./bindings/Loadout";
import type { LogReport } from "./bindings/LogReport";
import type { NexusStatus } from "./bindings/NexusStatus";
import type { NexusUser } from "./bindings/NexusUser";
import type { ScanResult } from "./bindings/ScanResult";

export const api = {
  appInfo: () => invoke<AppInfo>("app_info"),
  listGames: () => invoke<GameDefinition[]>("list_games"),
  scan: () => invoke<ScanResult>("scan"),

  loadout: (instanceId: string) => invoke<Loadout>("get_loadout", { instanceId }),
  importArchives: (instanceId: string, paths: string[]) =>
    invoke<ImportResult[]>("import_archives", { instanceId, paths }),
  setModEnabled: (instanceId: string, modId: string, enabled: boolean) =>
    invoke<Loadout>("set_mod_enabled", { instanceId, modId, enabled }),
  reorderMods: (instanceId: string, order: string[]) => invoke<Loadout>("reorder_mods", { instanceId, order }),
  removeMod: (instanceId: string, modId: string) => invoke<Loadout>("remove_mod", { instanceId, modId }),
  renameMod: (instanceId: string, modId: string, name: string) =>
    invoke<Loadout>("rename_mod", { instanceId, modId, name }),
  identifyMod: (instanceId: string, modId: string) => invoke<Loadout>("identify_mod", { instanceId, modId }),
  modFiles: (modId: string) => invoke<string[]>("mod_files", { modId }),

  createProfile: (instanceId: string, name: string, copyCurrent: boolean) =>
    invoke<Loadout>("create_profile", { instanceId, name, copyCurrent }),
  renameProfile: (instanceId: string, profileId: string, name: string) =>
    invoke<Loadout>("rename_profile", { instanceId, profileId, name }),
  deleteProfile: (instanceId: string, profileId: string) =>
    invoke<Loadout>("delete_profile", { instanceId, profileId }),
  switchProfile: (instanceId: string, profileId: string) =>
    invoke<Loadout>("switch_profile", { instanceId, profileId }),

  setArchiveOrder: (instanceId: string, order: ArchiveOrder) =>
    invoke<Loadout>("set_archive_order", { instanceId, order }),
  gameLogs: (instanceId: string) => invoke<LogReport[]>("game_logs", { instanceId }),
  readLog: (instanceId: string, relativePath: string) => invoke<string[]>("read_log", { instanceId, relativePath }),
  deploy: (instanceId: string) => invoke<DeployReport>("deploy", { instanceId }),
  purge: (instanceId: string) => invoke<DeployReport>("purge", { instanceId }),
  fixDllOverrides: (instanceId: string) => invoke<ScanResult>("fix_dll_overrides", { instanceId }),
  launchGame: (instanceId: string, modded: boolean) => invoke<void>("launch_game", { instanceId, modded }),

  nexusStatus: () => invoke<NexusStatus>("nexus_status"),
  connectNexus: (apiKey: string) => invoke<NexusUser>("connect_nexus", { apiKey }),
  disconnectNexus: () => invoke<void>("disconnect_nexus"),
  handleNxm: (url: string) => invoke<string>("handle_nxm", { url }),
  listDownloads: () => invoke<Download[]>("list_downloads"),
  clearFinishedDownloads: () => invoke<Download[]>("clear_finished_downloads"),
  checkUpdates: (instanceId: string) => invoke<Loadout>("check_updates", { instanceId }),
  updateMod: (modId: string) => invoke<string>("update_mod", { modId }),

  getFomod: (token: string) => invoke<FomodRequest>("get_fomod", { token }),
  fomodState: (token: string, selections: Selections) => invoke<FomodState>("fomod_state", { token, selections }),
  fomodImage: (token: string, path: string) => invoke<string>("fomod_image", { token, path }),
  finishFomod: (token: string, selections: Selections) => invoke<InstallOutcome>("finish_fomod", { token, selections }),
  cancelFomod: (token: string) => invoke<void>("cancel_fomod", { token }),

  savesInfo: (instanceId: string) => invoke<SavesInfo>("saves_info", { instanceId }),
  backupSaves: (instanceId: string) => invoke<SaveBackup | null>("backup_saves", { instanceId }),
  restoreSaves: (instanceId: string, backupId: string) => invoke<void>("restore_saves", { instanceId, backupId }),
  deleteSaveBackup: (instanceId: string, backupId: string) =>
    invoke<void>("delete_save_backup", { instanceId, backupId }),
  setAutoSaveBackup: (instanceId: string, on: boolean) => invoke<void>("set_auto_save_backup", { instanceId, on }),

  exportProfile: (instanceId: string, profileId: string, path: string) =>
    invoke<void>("export_profile", { instanceId, profileId, path }),
  importProfile: (instanceId: string, path: string) => invoke<ImportReport>("import_profile", { instanceId, path }),

  openCollection: (link: string) => invoke<Collection>("open_collection", { link }),
  installCollection: (gameDomain: string, files: [number, number][]) =>
    invoke<number>("install_collection", { gameDomain, files }),
};

export const events = {
  downloadsChanged: (fn: (downloads: Download[]) => void): Promise<UnlistenFn> =>
    listen<Download[]>("downloads-changed", (e) => fn(e.payload)),
  libraryChanged: (fn: () => void): Promise<UnlistenFn> => listen("library-changed", () => fn()),
  nxmError: (fn: (message: string) => void): Promise<UnlistenFn> =>
    listen<string>("nxm-error", (e) => fn(e.payload)),
  fomodRequested: (fn: (request: FomodRequest) => void): Promise<UnlistenFn> =>
    listen<FomodRequest>("fomod-requested", (e) => fn(e.payload)),
  openCollection: (fn: (link: string) => void): Promise<UnlistenFn> =>
    listen<string>("open-collection", (e) => fn(e.payload)),
};

export const ARCHIVE_EXTENSIONS = ["zip", "7z", "rar"];

export async function pickArchives(): Promise<string[]> {
  const picked = await openDialog({
    multiple: true,
    directory: false,
    title: "Add mod archives",
    filters: [{ name: "Mod archives", extensions: ARCHIVE_EXTENSIONS }],
  });
  if (!picked) return [];
  return Array.isArray(picked) ? picked : [picked];
}

export const reveal = (path: string) => revealItemInDir(path);

export async function pickProfileToImport(): Promise<string | null> {
  const picked = await openDialog({
    multiple: false,
    directory: false,
    title: "Import a profile",
    filters: [{ name: "Mod Manager profile", extensions: ["json"] }],
  });
  return typeof picked === "string" ? picked : null;
}

export function pickProfileExportPath(name: string): Promise<string | null> {
  return saveDialog({
    title: "Export profile",
    defaultPath: `${name.replace(/[^\w .-]+/g, "_")}.modprofile.json`,
    filters: [{ name: "Mod Manager profile", extensions: ["json"] }],
  });
}

export const nexusUrls = {
  modPage: (domain: string, modId: number) => `https://www.nexusmods.com/${domain}/mods/${modId}`,
  filesPage: (domain: string, modId: number, fileId?: number | null) =>
    `https://www.nexusmods.com/${domain}/mods/${modId}?tab=files${fileId ? `&file_id=${fileId}` : ""}`,
  gameMods: (domain: string) => `https://www.nexusmods.com/${domain}/mods/`,
  apiKey: "https://www.nexusmods.com/users/myaccount?tab=api",
  collections: (domain: string) => `https://www.nexusmods.com/games/${domain}/collections`,
};

export const openExternal = (url: string) => openUrl(url);

/** Rejects with `message` if `promise` hasn't settled after `ms` milliseconds. */
export function withTimeout<T>(promise: Promise<T>, ms: number, message: string): Promise<T> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(message)), ms);
    promise.then(
      (value) => {
        clearTimeout(timer);
        resolve(value);
      },
      (error) => {
        clearTimeout(timer);
        reject(error);
      },
    );
  });
}

export function errorMessage(e: unknown): string {
  return typeof e === "string" ? e : e instanceof Error ? e.message : String(e);
}
