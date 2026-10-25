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
    ...extra,
  };
}

let mods: LoadoutMod[] = [
  { mod: mod("cet", "Cyber Engine Tweaks", ["cet"], { version: "1.35.0", author: "yamashi", nexus_mod_id: 107, nexus_file_id: 1, size_bytes: 12_500_000 }), enabled: true, overrides: [], overridden_by: [], conflict_files: 0 },
  { mod: mod("red4ext", "RED4ext", ["red4ext"], { version: "1.27.0", author: "WopsS", nexus_mod_id: 2380, nexus_file_id: 1, update_file_id: 2, update_version: "1.28.0" }), enabled: true, overrides: [], overridden_by: [], conflict_files: 0 },
  { mod: mod("archivexl", "ArchiveXL", ["archive", "red4ext"], { version: "1.21.1", author: "psiberx" }), enabled: true, overrides: [], overridden_by: [], conflict_files: 0 },
  { mod: mod("hd", "HD Reworked Project", ["archive"], { version: "2.0", author: "Halk", size_bytes: 4_200_000_000 }), enabled: true, overrides: [], overridden_by: ["Better Neon Lights"], conflict_files: 3 },
  { mod: mod("neon", "Better Neon Lights", ["archive"], { author: "Glow" }), enabled: true, overrides: ["HD Reworked Project"], overridden_by: [], conflict_files: 3 },
  { mod: mod("hud", "Immersive HUD", ["cet", "redscript"], { version: "0.9" }), enabled: false, overrides: [], overridden_by: [], conflict_files: 0 },
  { mod: mod("redmod", "Night City Radio Expanded", ["redmod"]), enabled: true, overrides: [], overridden_by: [], conflict_files: 0 },
];
let pending = true;

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
          return { added: 12, removed: 1, replaced: 0, unchanged: 30, backed_up: 0, restored: 0, cloned: 12, linked: 0, copied: 0 };
        case "nexus_status":
          return { user: { user_id: 1, name: "demo", is_premium: false, profile_url: null }, error: null };
        case "list_downloads":
          return downloads;
        case "app_info":
          return { version: "0.1.0", data_dir: "/Users/demo/Library/Application Support/ModManager", library_bytes: 4_300_000_000, crossover_installed: true };
        case "launch_game":
          return null;
        default:
          return null;
      }
    },
    { shouldMockEvents: true },
  );
}
