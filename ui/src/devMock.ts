// Fake backend for working on the UI in a plain browser (`npm run dev -w ui`,
// then open http://localhost:1420). Only loaded outside Tauri in dev builds.
import { mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import type { Download } from "./bindings/Download";
import type { GameDefinition } from "./bindings/GameDefinition";
import type { Loadout } from "./bindings/Loadout";
import type { LoadoutMod } from "./bindings/LoadoutMod";
import type { ModRecord } from "./bindings/ModRecord";
import type { ScanResult } from "./bindings/ScanResult";

const INSTANCE = "cyberpunk2077-demo";
const now = Math.floor(Date.now() / 1000);

const scan: ScanResult = {
  environments: [
    {
      id: "crossover:Cyberpunk 2077",
      name: "Cyberpunk 2077",
      kind: { type: "CrossOverBottle", bottle_id: null, template: "win10_64", wine_arch: "win64", crossover_version: "26.3" },
      prefix: "/Users/demo/Library/Application Support/CrossOver/Bottles/Cyberpunk 2077",
    },
  ],
  instances: [
    {
      id: INSTANCE,
      game_id: "cyberpunk2077",
      game_name: "Cyberpunk 2077",
      environment_id: "crossover:Cyberpunk 2077",
      store: "Steam",
      install_path: "/Users/demo/.../steamapps/common/Cyberpunk 2077",
      version: "2.5",
      build_id: "26161058",
      fingerprint: "demo",
      launch_id: null,
      launch_options_hint: null,
      dll_overrides: [
        { dll: "version", required: "native,builtin", current: null, satisfied: false, reason: "Cyber Engine Tweaks loads through version.dll" },
        { dll: "winmm", required: "native,builtin", current: null, satisfied: false, reason: "RED4ext loads through winmm.dll" },
      ],
    },
  ],
};

const game: GameDefinition = {
  id: "cyberpunk2077",
  name: "Cyberpunk 2077",
  executable: "bin/x64/Cyberpunk2077.exe",
  stores: { steam_app_id: "1091500", gog_id: "1423049311", nexus_domain: "cyberpunk2077" },
  known_install_paths: [],
  layout: [],
  canonical_dirs: [],
  root_markers: [],
  wine: { dll_overrides: [] },
  launch: { modded_args: ["-modded"] },
  frameworks: [],
  requirements: [],
  logs: [],
  load_order: { file: "archive/pc/mod/modlist.txt", extensions: ["archive"] },
  saves: "C:\\users\\*\\Saved Games\\CD Projekt Red\\Cyberpunk 2077",
};

const fomodRequest = {
  token: "demo-fomod",
  game_id: "cyberpunk2077",
  mod_name: "Better Lighting",
  defaults: [[[0], []]],
  installer: {
    module_name: "Better Lighting",
    image: null,
    info: { name: "Better Lighting", author: "Lumen", version: "2.1", website: null, description: "Nicer lights in Night City." },
    steps: [
      {
        name: "Preset",
        groups: [
          {
            name: "Brightness",
            kind: "SelectExactlyOne" as const,
            options: [
              { name: "Balanced", description: "Close to the original look, slightly brighter nights.", image: null, kind: "Recommended" as const },
              { name: "Vivid", description: "Stronger neon and more contrast.", image: null, kind: "Optional" as const },
            ],
          },
          {
            name: "Extras",
            kind: "SelectAny" as const,
            options: [
              { name: "Brighter interiors", description: "Raises indoor light levels.", image: null, kind: "Optional" as const },
              { name: "Ray tracing tweaks", description: "Needs RT enabled.", image: null, kind: "NotUsable" as const },
            ],
          },
        ],
      },
      {
        name: "Compatibility",
        groups: [
          {
            name: "Weather mods",
            kind: "SelectAtMostOne" as const,
            options: [{ name: "Nova LUT patch", description: "Only if you use Nova LUT.", image: null, kind: "Optional" as const }],
          },
        ],
      },
    ],
  },
};

function mod(id: string, name: string, kinds: string[], extra: Partial<ModRecord> = {}): ModRecord {
  return {
    id,
    game_id: "cyberpunk2077",
    name,
    version: "1.0.0",
    author: "Modder",
    summary: "A demo mod used to preview the interface.",
    picture_url: null,
    nexus_mod_id: null,
    nexus_file_id: null,
    archive_name: `${name}.zip`,
    kinds,
    file_count: 4,
    size_bytes: 3_400_000,
    installed_at: now - 86400 * 3,
    revision: 1,
    update_file_id: null,
    update_version: null,
    nexus_updated_at: null,
    pinned: false,
    ignored_update_file_id: null,
    ...extra,
  };
}

let mods: LoadoutMod[] = [
  { mod: mod("cet", "Cyber Engine Tweaks", ["cet"], { version: "1.35.0", author: "yamashi", nexus_mod_id: 107, nexus_file_id: 1, size_bytes: 12_500_000 }), enabled: true, overrides: [], overridden_by: [], conflict_files: 0, missing_requirements: [] },
  { mod: mod("red4ext", "RED4ext", ["red4ext"], { version: "1.27.0", author: "WopsS", nexus_mod_id: 2380, nexus_file_id: 1, update_file_id: 2, update_version: "1.28.0" }), enabled: true, overrides: [], overridden_by: [], conflict_files: 0, missing_requirements: [] },
  { mod: mod("archivexl", "ArchiveXL", ["archive", "red4ext"], { version: "1.21.1", author: "psiberx" }), enabled: true, overrides: [], overridden_by: [], conflict_files: 0, missing_requirements: [] },
  { mod: mod("hd", "HD Reworked Project", ["archive"], { version: "2.0", author: "Halk", size_bytes: 4_200_000_000 }), enabled: true, overrides: [], overridden_by: ["Better Neon Lights"], conflict_files: 3, missing_requirements: [] },
  { mod: mod("neon", "Better Neon Lights", ["archive"], { author: "Glow" }), enabled: true, overrides: ["HD Reworked Project"], overridden_by: [], conflict_files: 3, missing_requirements: [] },
  { mod: mod("hud", "Immersive HUD", ["cet", "redscript"], { version: "0.9" }), enabled: true, overrides: [], overridden_by: [], conflict_files: 0, missing_requirements: [{ framework_id: "redscript", name: "redscript", nexus_mod_id: 1511, disabled_mod_id: null }] },
  { mod: mod("redmod", "Night City Radio Expanded", ["redmod"]), enabled: true, overrides: [], overridden_by: [], conflict_files: 0, missing_requirements: [] },
];
let retention: "KeepAll" | "LastRevisions" | "DeleteAfterInstall" = "LastRevisions";
let revisionsKept = 2;
let unusedArchiveBytes = 412_000_000;
const archivesInfo = () => ({
  retention,
  revisions_kept: revisionsKept,
  total_bytes: 2_150_000_000,
  unused_count: unusedArchiveBytes ? 3 : 0,
  unused_bytes: unusedArchiveBytes,
  mods: [
    { mod_id: "m3", mod_name: "Cyberpunk HD Reworked", archives: 2, bytes: 1_380_000_000 },
    { mod_id: "m1", mod_name: "Cyber Engine Tweaks", archives: 2, bytes: 48_000_000 },
    { mod_id: "m2", mod_name: "Immersive HUD", archives: 1, bytes: 1_200_000 },
  ],
});
let pending = true;
let archiveOrder: "ByFileName" | "ByList" = "ByFileName";

const loadout = (): Loadout => ({
  instance_id: INSTANCE,
  profiles: [
    { id: "p1", instance_id: INSTANCE, name: "Default" },
    { id: "p2", instance_id: INSTANCE, name: "Vanilla" },
  ],
  active_profile_id: "p1",
  mods,
  pending_changes: pending,
  deployed_at: now - 3600,
  frameworks: [
    { id: "cet", name: "Cyber Engine Tweaks", nexus_mod_id: 107, state: { state: "Installed", mod_name: "Cyber Engine Tweaks" }, needed_by: ["Immersive HUD"] },
    { id: "red4ext", name: "RED4ext", nexus_mod_id: 2380, state: { state: "Installed", mod_name: "RED4ext" }, needed_by: ["ArchiveXL"] },
    { id: "redscript", name: "redscript", nexus_mod_id: 1511, state: { state: "NotInstalled" }, needed_by: ["Immersive HUD"] },
    { id: "archivexl", name: "ArchiveXL", nexus_mod_id: 4198, state: { state: "Installed", mod_name: "ArchiveXL" }, needed_by: [] },
    { id: "tweakxl", name: "TweakXL", nexus_mod_id: 4197, state: { state: "NotInstalled" }, needed_by: [] },
  ],
  game: { version: "2.5", build_id: "26161058", updated_at: now - 7200, outdated_frameworks: ["Cyber Engine Tweaks"] },
  archive_order: archiveOrder,
});

const downloads: Download[] = [
  {
    id: "d1",
    game_id: "cyberpunk2077",
    instance_id: INSTANCE,
    name: "Virtual Atelier",
    version: "1.4",
    file_name: "VirtualAtelier-2987-1-4.zip",
    nexus_mod_id: 2987,
    nexus_file_id: 1,
    bytes_done: 6_000_000,
    bytes_total: 14_000_000,
    status: { state: "Downloading" },
    game_domain: "cyberpunk2077",
    attempts: 0,
    retry_at: null,
    replaces: null,
    created_at: now - 60,
  },
  {
    id: "d2",
    game_id: "cyberpunk2077",
    instance_id: INSTANCE,
    name: "Better Vehicle Handling",
    version: "2.1",
    file_name: "BetterVehicleHandling-2.1.zip",
    nexus_mod_id: 3004,
    nexus_file_id: 9,
    bytes_done: 2_000_000,
    bytes_total: 8_000_000,
    status: { state: "Queued" },
    game_domain: "cyberpunk2077",
    attempts: 1,
    retry_at: now + 25,
    replaces: null,
    created_at: now - 50,
  },
  {
    id: "d3",
    game_id: "cyberpunk2077",
    instance_id: INSTANCE,
    name: "Mod 4575",
    version: null,
    file_name: null,
    nexus_mod_id: 4575,
    nexus_file_id: 114235,
    bytes_done: 0,
    bytes_total: null,
    status: {
      state: "Failed",
      message:
        'This download link expired while the app was closed. Click "Mod Manager Download" on the mod page again (Premium members can press Retry).',
    },
    game_domain: "cyberpunk2077",
    attempts: 0,
    retry_at: null,
    replaces: null,
    created_at: now - 40,
  },
];

export function installDevMock() {
  mockWindows("main");
  mockIPC(
    (cmd, args) => {
      const a = (args ?? {}) as Record<string, unknown>;
      switch (cmd) {
        case "scan":
          return scan;
        case "list_games":
          return [game];
        case "get_loadout":
        case "switch_profile":
          return loadout();
        case "set_mod_enabled":
          mods = mods.map((m) => (m.mod.id === a.modId ? { ...m, enabled: a.enabled as boolean } : m));
          pending = true;
          return loadout();
        case "reorder_mods":
          mods = (a.order as string[]).map((id) => mods.find((m) => m.mod.id === id)!);
          pending = true;
          return loadout();
        case "remove_mod":
          mods = mods.filter((m) => m.mod.id !== a.modId);
          return loadout();
        case "mod_files":
          return ["archive/pc/mod/example.archive", "r6/scripts/Example/main.reds"];
        case "deploy":
          pending = false;
          return {
            added: 12, removed: 1, replaced: 0, unchanged: 30, backed_up: 0, restored: 0, cloned: 12, linked: 0, copied: 0,
            adopted: 0, preserved: ["bin/x64/plugins/cyber_engine_tweaks/mods/ImmersiveHUD/settings.json"], warnings: [],
          };
        case "nexus_status":
          return { user: { user_id: 1, name: "demo", is_premium: false, profile_url: null }, error: null };
        case "list_downloads":
          return downloads;
        case "retry_download":
        case "cancel_download":
          return null;
        case "app_info":
          return {
            version: "0.1.0",
            data_dir: "/Users/demo/Library/Application Support/ModManager",
            library_bytes: 4_300_000_000,
            archives_bytes: 2_150_000_000,
            crossover_installed: true,
            warnings: [],
          };
        case "verify_deployment":
          return {
            checked: 214,
            drifted: [
              { target: "bin/x64/plugins/cyber_engine_tweaks/mods/ImmersiveHUD/settings.json", mod_id: "m2", mod_name: "Immersive HUD", kind: "Modified" },
              { target: "engine/config/platform/pc/user.ini", mod_id: "m4", mod_name: "Better Vehicle Handling", kind: "ReplacedByGame" },
            ],
          };
        case "check_updates":
          return { loadout: loadout(), summary: { checked: 3, available: 1, requests: 4, period: "1w" } };
        case "mod_changelog":
          return {
            versions: [
              { version: "1.38.0", lines: ["Supports Cyberpunk 2077 patch 2.5.", "ImGui docking for CET and mod windows."] },
              { version: "1.37.1", lines: ["Fixed NewProxy crashes."] },
            ],
            fetched_at: now - 120,
          };
        case "set_mod_pinned":
          mods = mods.map((m) => (m.mod.id === a.modId ? { ...m, mod: { ...m.mod, pinned: a.pinned as boolean } } : m));
          return loadout();
        case "ignore_update":
          mods = mods.map((m) =>
            m.mod.id === a.modId ? { ...m, mod: { ...m.mod, update_file_id: null, update_version: null } } : m,
          );
          return loadout();
        case "update_all":
          return {
            queued: 0,
            links: [
              {
                mod_name: "Cyber Engine Tweaks",
                version: "1.38.0",
                url: "https://www.nexusmods.com/cyberpunk2077/mods/107?tab=files&file_id=130002",
              },
            ],
          };
        case "update_counts":
          return { cyberpunk2077: mods.filter((m) => m.mod.update_file_id && !m.mod.pinned).length };
        case "auto_update_check":
          return true;
        case "set_auto_update_check":
          return null;
        case "nexus_rate_limits":
          return {
            hourly_limit: 100,
            hourly_remaining: 100,
            hourly_reset: now + 1800,
            daily_limit: 2500,
            daily_remaining: 2431,
            daily_reset: now + 40_000,
            observed_at: now - 60,
          };
        case "set_revisions_kept":
          revisionsKept = a.count as number;
          return archivesInfo();
        case "mod_revisions": {
          const m = mods.find((x) => x.mod.id === a.modId)?.mod;
          const v = m?.version ?? "1.0";
          return [
            { revision: 3, version: v, installed_at: m?.installed_at ?? now, archive_kept: true, has_choices: true, current: true },
            { revision: 2, version: `${v}-beta`, installed_at: now - 86400 * 20, archive_kept: true, has_choices: true, current: false },
            { revision: 1, version: null, installed_at: now - 86400 * 45, archive_kept: false, has_choices: false, current: false },
          ];
        }
        case "reinstall_mod":
        case "rollback_mod":
          return { outcome: { mod: mods[0].mod, skipped: [], warnings: [], replaced_existing: true, replayed_choices: true }, fomod: null };
        case "archives_info":
          return archivesInfo();
        case "set_archive_retention":
          retention = a.retention as typeof retention;
          return archivesInfo();
        case "clean_unused_archives": {
          const freed = unusedArchiveBytes;
          unusedArchiveBytes = 0;
          return { deleted: freed ? 3 : 0, bytes: freed };
        }
        case "set_archive_order":
          archiveOrder = a.order as typeof archiveOrder;
          pending = true;
          return loadout();
        case "game_logs":
          return [
            {
              name: "Cyber Engine Tweaks",
              framework: "cet",
              relative_path: "bin/x64/plugins/cyber_engine_tweaks/cyber_engine_tweaks.log",
              path: "/demo/cyber_engine_tweaks.log",
              modified_at: now - 600,
              size_bytes: 18_000,
              stale: false,
              error_count: 1,
              warning_count: 2,
              entries: [
                { line: 40, severity: "Warning", text: "[warning] Font not found, using fallback", mod_name: null },
                { line: 88, severity: "Error", text: "[error] [ImmersiveHUD] init.lua:12: attempt to index a nil value (global 'Game')", mod_name: "Immersive HUD" },
              ],
            },
            {
              name: "RED4ext",
              framework: "red4ext",
              relative_path: "red4ext/logs/red4ext.log",
              path: "/demo/red4ext.log",
              modified_at: now - 600,
              size_bytes: 4_000,
              stale: false,
              error_count: 0,
              warning_count: 0,
              entries: [],
            },
          ];
        case "plugin:dialog|open":
          return ["/demo/Better Lighting.zip"];
        case "import_archives":
          return [{ file: "Better Lighting.zip", outcome: null, fomod: fomodRequest, error: null }];
        case "get_fomod":
          return fomodRequest;
        case "fomod_state":
          return { visible_steps: [0, 1], option_kinds: [[["Recommended", "Optional"], ["Optional", "NotUsable"]], [["Optional"]]], error: null };
        case "finish_fomod":
          return { mod: mods[0].mod, skipped: [], warnings: [], replaced_existing: false };
        case "saves_info":
          return {
            save_dir: "/demo/Saved Games/CD Projekt Red/Cyberpunk 2077",
            auto_backup: true,
            backups: [
              { id: "b2", created_at: now - 1800, reason: "BeforeLaunch", size_bytes: 120_000_000 },
              { id: "b1", created_at: now - 86400 * 2, reason: "Manual", size_bytes: 98_000_000 },
            ],
          };
        case "open_collection":
          return {
            slug: "demo", name: "Night City Essentials", author: "Curator", summary: "A small, stable starter set.", revision: 12,
            game_domain: "cyberpunk2077", unavailable_file_ids: [],
            mods: [
              { mod_id: 107, file_id: 1, name: "Cyber Engine Tweaks", file_title: "CET 1.38.0", file_name: null, version: "1.38.0", size_bytes: 12_000_000, optional: false, game_domain: "cyberpunk2077" },
              { mod_id: 1511, file_id: 77, name: "redscript", file_title: "redscript", file_name: null, version: "0.5.27", size_bytes: 3_000_000, optional: false, game_domain: "cyberpunk2077" },
              { mod_id: 4198, file_id: 88, name: "ArchiveXL", file_title: "ArchiveXL", file_name: null, version: "1.21", size_bytes: 1_000_000, optional: true, game_domain: "cyberpunk2077" },
            ],
          };
        case "read_log":
          return ["[info] CET version 1.38.0", "[info] Game version 2.5", "[error] [ImmersiveHUD] init.lua:12: attempt to index a nil value"];
        case "launch_game":
          return null;
        default:
          return null;
      }
    },
    { shouldMockEvents: true },
  );
}
