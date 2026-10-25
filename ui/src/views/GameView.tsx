import { useCallback, useEffect, useMemo, useState } from "react";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import type { View } from "../App";
import {
  ARCHIVE_EXTENSIONS,
  api,
  errorMessage,
  events,
  nexusUrls,
  openExternal,
  pickArchives,
  pickProfileExportPath,
  pickProfileToImport,
  reveal,
} from "../api";
import type { ImportReport } from "../bindings/ImportReport";
import { useFomodQueue } from "../components/FomodQueue";
import type { DeployReport } from "../bindings/DeployReport";
import type { Environment } from "../bindings/Environment";
import type { GameDefinition } from "../bindings/GameDefinition";
import type { GameInstance } from "../bindings/GameInstance";
import type { Loadout } from "../bindings/Loadout";
import type { NexusStatus } from "../bindings/NexusStatus";
import type { ScanResult } from "../bindings/ScanResult";
import type { UpdateLink } from "../bindings/UpdateLink";
import { ConfirmDialog, Dialog, PromptDialog } from "../components/Dialog";
import { Icon } from "../components/Icon";
import { useToast } from "../components/Toasts";
import { KIND_LABELS, STORE_LABELS, plural, timeAgo } from "../format";
import { ModDetails } from "./ModDetails";
import { HealthView } from "./HealthView";
import { SavesView } from "./SavesView";
import { ModList } from "./ModList";

type DialogState =
  | { kind: "new-profile" }
  | { kind: "rename-profile" }
  | { kind: "delete-profile" }
  | { kind: "purge" }
  | { kind: "remove-mod"; modId: string; name: string }
  | { kind: "rename-mod"; modId: string; name: string }
  | { kind: "update-links"; links: UpdateLink[] }
  | null;

function describeReport(r: DeployReport): string {
  const parts = [
    r.added && `${r.added} added`,
    r.replaced && `${r.replaced} replaced`,
    r.removed && `${r.removed} removed`,
    r.restored && `${r.restored} originals restored`,
    r.adopted && `${r.adopted} kept as updated by the game`,
  ].filter(Boolean);
  const summary = parts.length ? `${parts.join(", ")}.` : "Nothing to change.";
  const preserved = r.preserved.length
    ? ` ${r.preserved.length === 1 ? "1 edited file was" : `${r.preserved.length} edited files were`} copied to the preserved folder first.`
    : "";
  return summary + preserved;
}

export function GameView({
  instance,
  env,
  game,
  nexus,
  onScan,
  onNavigate,
}: {
  instance: GameInstance;
  env: Environment;
  game: GameDefinition;
  nexus: NexusStatus | null;
  onScan: (scan: ScanResult) => void;
  onNavigate: (view: View) => void;
}) {
  const toast = useToast();
  const enqueueFomod = useFomodQueue();
  const [loadout, setLoadout] = useState<Loadout | null>(null);
  const [importReport, setImportReport] = useState<ImportReport | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [kindFilter, setKindFilter] = useState<string | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [dialog, setDialog] = useState<DialogState>(null);
  const [dropping, setDropping] = useState(false);
  const [copyProfile, setCopyProfile] = useState(true);
  const [tab, setTab] = useState<"mods" | "health" | "saves">("mods");
  const [dismissedUpdate, setDismissedUpdate] = useState<string | null>(() => {
    try {
      return localStorage.getItem(`dismissed-update:${instance.id}`);
    } catch {
      return null;
    }
  });

  const id = instance.id;
  const reload = useCallback(async () => {
    try {
      setLoadout(await api.loadout(id));
    } catch (e) {
      toast("error", "Couldn't load mods", errorMessage(e));
    }
  }, [id, toast]);

  useEffect(() => {
    void reload();
    const unlisten = events.libraryChanged(() => void reload());
    return () => void unlisten.then((f) => f());
  }, [reload]);

  /** Runs an action that returns a new loadout, with a busy state and error toast. */
  const run = useCallback(
    async <T,>(label: string, action: () => Promise<T>): Promise<T | undefined> => {
      setBusy(label);
      try {
        return await action();
      } catch (e) {
        toast("error", `${label} failed`, errorMessage(e));
      } finally {
        setBusy(null);
      }
    },
    [toast],
  );

  const install = useCallback(
    async (paths: string[]) => {
      const archives = paths.filter((p) => ARCHIVE_EXTENSIONS.some((ext) => p.toLowerCase().endsWith(`.${ext}`)));
      if (archives.length === 0) {
        if (paths.length) toast("error", "Not a mod archive", "Drop .zip, .7z or .rar files.");
        return;
      }
      const results = await run("Installing", () => api.importArchives(id, archives));
      if (!results) return;
      const ok = results.filter((r) => r.outcome);
      const failed = results.filter((r) => r.error);
      if (ok.length)
        toast(
          "success",
          ok.length === 1 ? `${ok[0].outcome!.mod.name} installed` : `${ok.length} mods installed`,
          ok.flatMap((r) => r.outcome!.warnings)[0] ??
            (ok.some((r) => r.outcome!.mod.nexus_file_id)
              ? "Matched on Nexus Mods, so you'll get update alerts. Apply changes to put it in the game."
              : "Apply changes to put them in the game."),
        );
      for (const f of failed) toast("error", `Couldn't install ${f.file}`, f.error ?? undefined);
      for (const r of results) if (r.fomod) enqueueFomod(r.fomod);
      await reload();
    },
    [id, reload, run, toast, enqueueFomod],
  );

  // Files dragged onto the window.
  useEffect(() => {
    const unlisten = getCurrentWebview().onDragDropEvent((event) => {
      const p = event.payload;
      if (p.type === "enter" || p.type === "over") setDropping(true);
      else if (p.type === "leave") setDropping(false);
      else if (p.type === "drop") {
        setDropping(false);
        void install(p.paths);
      }
    });
    return () => void unlisten.then((f) => f());
  }, [install]);

  const enableMod = async (modId: string) => {
    const next = await run("Enabling", () => api.setModEnabled(id, modId, true));
    if (next) setLoadout(next);
  };

  const apply = async () => {
    const report = await run("Applying", () => api.deploy(id));
    if (report) {
      toast("success", "Game folder updated", describeReport(report));
      if (report.warnings.length) toast("info", "Check these files", report.warnings.join("\n"));
      await reload();
    }
    return report;
  };

  const play = async () => {
    if (loadout?.pending_changes && !(await apply())) return;
    const launched = await run("Launching", async () => {
      await api.launchGame(id, true);
      return true;
    });
    if (launched) toast("info", `Starting ${game.name}`, "This can take a moment in CrossOver.");
  };

  const fixDlls = async () => {
    const scan = await run("Fixing Wine settings", () => api.fixDllOverrides(id));
    if (scan) {
      onScan(scan);
      toast("success", "Wine DLL overrides set", "Script extenders will load next time the game starts.");
    }
  };

  const checkUpdates = async () => {
    const result = await run("Checking for updates", () => api.checkUpdates(id));
    if (result) {
      setLoadout(result.loadout);
      const { available, checked, requests } = result.summary;
      toast(
        available ? "info" : "success",
        available ? `${plural(available, "update")} available` : "All mods are up to date",
        `Looked at ${plural(checked, "mod")} with ${plural(requests, "Nexus request")}.`,
      );
    }
  };

  const updateAll = async () => {
    const result = await run("Updating all", () => api.updateAll(id));
    if (!result) return;
    if (result.queued) toast("info", `Downloading ${plural(result.queued, "update")}`, "Follow progress in Downloads.");
    if (result.links.length) setDialog({ kind: "update-links", links: result.links });
  };

  const mods = useMemo(() => loadout?.mods ?? [], [loadout]);
  const updatable = mods.filter((m) => m.mod.update_file_id && !m.mod.pinned).length;
  const kinds = useMemo(() => [...new Set(mods.flatMap((m) => m.mod.kinds))].sort(), [mods]);
  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    return mods.filter(
      (m) =>
        (!kindFilter || m.mod.kinds.includes(kindFilter)) &&
        (!q || m.mod.name.toLowerCase().includes(q) || m.mod.author?.toLowerCase().includes(q)),
    );
  }, [mods, query, kindFilter]);

  const missingDlls = instance.dll_overrides.filter((d) => !d.satisfied);
  const modsMissing = mods.filter((m) => m.enabled && m.missing_requirements.length > 0);
  const missingNames = [...new Set(modsMissing.flatMap((m) => m.missing_requirements.map((r) => r.name)))];
  const healthIssues = missingNames.length + (loadout?.game.outdated_frameworks.length ? 1 : 0);
  const enabledCount = mods.filter((m) => m.enabled).length;
  const activeProfile = loadout?.profiles.find((p) => p.id === loadout.active_profile_id);
  const selectedMod = mods.find((m) => m.mod.id === selected) ?? null;
  const domain = game.stores.nexus_domain;
  const envLabel =
    env.kind.type === "CrossOverBottle"
      ? `CrossOver · ${env.name}`
      : env.kind.type === "Proton"
        ? "Steam Proton"
        : env.kind.type === "Native"
          ? "Windows"
          : `Wine · ${env.name}`;

  const exportProfile = async () => {
    if (!loadout || !activeProfile) return;
    const path = await pickProfileExportPath(`${game.name} - ${activeProfile.name}`);
    if (!path) return;
    const ok = await run("Exporting profile", async () => {
      await api.exportProfile(id, activeProfile.id, path);
      return true;
    });
    if (ok) toast("success", "Profile exported", "Share the file or import it on another computer.");
  };

  const importProfile = async () => {
    const path = await pickProfileToImport();
    if (!path) return;
    const report = await run("Importing profile", () => api.importProfile(id, path));
    if (report) {
      await reload();
      if (report.missing.length) setImportReport(report);
      else toast("success", `Imported “${report.profile_name}”`, `${report.matched} mods matched. Apply to use it.`);
    }
  };

  return (
    <div className={`game-view${selectedMod ? " with-details" : ""}`}>
      <div className="game-main">
        <header className="game-header">
          <div className="game-title">
            <h1>{game.name}</h1>
            <div className="chips">
              <span className="chip">
                <Icon name="wine" size={12} /> {envLabel}
              </span>
              <span className="chip">{STORE_LABELS[instance.store]}</span>
              {instance.version && <span className="chip">Game {instance.version}</span>}
              <button className="chip chip-button" onClick={() => reveal(instance.install_path)} title={instance.install_path}>
                <Icon name="folder" size={12} /> Game folder
              </button>
            </div>
          </div>
          <div className="header-actions">
            <details className="menu">
              <summary className="icon-button" aria-label="More actions">
                <Icon name="more" />
              </summary>
              <div className="menu-items">
                <button onClick={() => reveal(instance.install_path)}>
                  <Icon name="folder" /> Open game folder
                </button>
                {domain && (
                  <button onClick={() => openExternal(nexusUrls.gameMods(domain))}>
                    <Icon name="external" /> Browse mods on Nexus
                  </button>
                )}
                <button onClick={checkUpdates} disabled={!nexus?.user}>
                  <Icon name="refresh" /> Check for updates
                </button>
                <hr />
                <button className="danger-text" onClick={() => setDialog({ kind: "purge" })}>
                  <Icon name="trash" /> Remove all mods from game folder
                </button>
              </div>
            </details>
            <button className="play" onClick={play} disabled={!!busy}>
              <Icon name="play" size={18} />
              {busy === "Launching" ? "Starting…" : loadout?.pending_changes ? "Apply & Play" : "Play"}
            </button>
          </div>
        </header>

        {(missingDlls.length > 0 || !nexus?.user) && (
          <section className="setup" aria-label="Setup">
            {missingDlls.length > 0 && (
              <div className="setup-item">
                <Icon name="alert" className="warn-icon" />
                <div>
                  <strong>Wine needs two settings for script extenders</strong>
                  <p>
                    {missingDlls.map((d) => d.reason).join(". ")}. Without{" "}
                    {missingDlls.map((d) => (
                      <code key={d.dll}>{d.dll}</code>
                    ))}{" "}
                    set to <code>native,builtin</code> in this bottle, those mods silently don't load.
                  </p>
                </div>
                <button className="primary" onClick={fixDlls} disabled={!!busy}>
                  {busy === "Fixing Wine settings" ? "Fixing…" : "Fix automatically"}
                </button>
              </div>
            )}
            {!nexus?.user && (
              <div className="setup-item">
                <Icon name="link" className="info-icon" />
                <div>
                  <strong>Connect Nexus Mods</strong>
                  <p>Install mods straight from the website's "Mod Manager Download" button and get update alerts.</p>
                </div>
                <button onClick={() => onNavigate({ kind: "settings" })}>Connect</button>
              </div>
            )}
          </section>
        )}

        {instance.launch_options_hint && (
          <section className="setup" aria-label="Steam launch options">
            <div className="setup-item">
              <Icon name="alert" className="warn-icon" />
              <div>
                <strong>Set these Steam launch options once</strong>
                <p>
                  Proton reads Wine settings from the game's launch options. In Steam: right-click {game.name} →
                  Properties → Launch Options, and paste:
                </p>
                <code className="copyable">{instance.launch_options_hint}</code>
              </div>
              <button
                onClick={async () => {
                  await navigator.clipboard.writeText(instance.launch_options_hint!);
                  toast("success", "Copied", "Paste it into the game's launch options in Steam.");
                }}
              >
                Copy
              </button>
            </div>
          </section>
        )}

        {modsMissing.length > 0 && (
          <section className="setup" aria-label="Missing requirements">
            <div className="setup-item setup-bad">
              <Icon name="alert" className="bad-icon" />
              <div>
                <strong>
                  {modsMissing.length === 1
                    ? `${modsMissing[0].mod.name} needs ${missingNames.join(", ")}`
                    : `${modsMissing.length} mods need ${missingNames.join(", ")}`}
                </strong>
                <p>Mods whose framework isn't installed and enabled silently do nothing in the game.</p>
              </div>
              <button className="primary" onClick={() => setTab("health")}>
                Review
              </button>
            </div>
          </section>
        )}

        {loadout && loadout.game.outdated_frameworks.length > 0 && dismissedUpdate !== String(loadout.game.updated_at) && (
          <section className="setup" aria-label="Game updated">
            <div className="setup-item">
              <Icon name="alert" className="warn-icon" />
              <div>
                <strong>{game.name} was updated {timeAgo(loadout.game.updated_at ?? 0)}</strong>
                <p>
                  {loadout.game.outdated_frameworks.join(", ")} {loadout.game.outdated_frameworks.length === 1 ? "was" : "were"}{" "}
                  installed before that. Script extenders often need an update after a game patch; if mods stopped working,
                  update them first.
                </p>
              </div>
              <div className="row">
                {nexus?.user && <button onClick={checkUpdates}>Check updates</button>}
                <button
                  className="icon-button"
                  aria-label="Dismiss"
                  onClick={() => {
                    const key = String(loadout.game.updated_at);
                    setDismissedUpdate(key);
                    try {
                      localStorage.setItem(`dismissed-update:${instance.id}`, key);
                    } catch {
                      // Remembering the dismissal is optional.
                    }
                  }}
                >
                  <Icon name="x" />
                </button>
              </div>
            </div>
          </section>
        )}

        <div className="tabs" role="tablist" aria-label="View">
          <button role="tab" aria-selected={tab === "mods"} className={tab === "mods" ? "active" : ""} onClick={() => setTab("mods")}>
            Mods <span className="tab-count">{mods.length}</span>
          </button>
          <button role="tab" aria-selected={tab === "health"} className={tab === "health" ? "active" : ""} onClick={() => setTab("health")}>
            Health
            {healthIssues > 0 && <span className="tab-count bad">{healthIssues}</span>}
          </button>
          <button role="tab" aria-selected={tab === "saves"} className={tab === "saves" ? "active" : ""} onClick={() => setTab("saves")}>
            Saves
          </button>
        </div>

        {tab === "saves" ? (
          <SavesView instanceId={id} gameName={game.name} />
        ) : tab === "health" ? (
          loadout && (
            <HealthView
              instanceId={id}
              loadout={loadout}
              gameDomain={domain ?? null}
              onLoadout={setLoadout}
              onEnableMod={enableMod}
            />
          )
        ) : (
          <>
        <div className="toolbar">
          <div className="profile-picker">
            <Icon name="layers" />
            <select
              aria-label="Profile"
              value={loadout?.active_profile_id ?? ""}
              onChange={async (e) => {
                if (e.target.value === "__new") return setDialog({ kind: "new-profile" });
                const next = await run("Switching profile", () => api.switchProfile(id, e.target.value));
                if (next) setLoadout(next);
              }}
            >
              {loadout?.profiles.map((p) => (
                <option key={p.id} value={p.id}>
                  {p.name}
                </option>
              ))}
              <option value="__new">+ New profile…</option>
            </select>
            <details className="menu">
              <summary className="icon-button" aria-label="Profile actions">
                <Icon name="more" size={14} />
              </summary>
              <div className="menu-items">
                <button onClick={() => setDialog({ kind: "new-profile" })}>
                  <Icon name="plus" /> New profile
                </button>
                <button onClick={() => setDialog({ kind: "rename-profile" })}>Rename “{activeProfile?.name}”</button>
                <button onClick={exportProfile}>Export to a file…</button>
                <button onClick={importProfile}>Import from a file…</button>
                <button
                  className="danger-text"
                  disabled={(loadout?.profiles.length ?? 0) <= 1}
                  onClick={() => setDialog({ kind: "delete-profile" })}
                >
                  <Icon name="trash" /> Delete profile
                </button>
              </div>
            </details>
          </div>

          <label className="search">
            <Icon name="search" />
            <input placeholder="Search mods" value={query} onChange={(e) => setQuery(e.target.value)} />
          </label>

          <div className="toolbar-actions">
            {nexus?.user && mods.length > 0 && (
              <button onClick={checkUpdates} disabled={!!busy}>
                <Icon name="refresh" className={busy === "Checking for updates" ? "spin" : undefined} /> Updates
              </button>
            )}
            {nexus?.user && updatable > 0 && (
              <button onClick={updateAll} disabled={!!busy}>
                <Icon name="download" /> Update all ({updatable})
              </button>
            )}
            <button className="primary" onClick={async () => install(await pickArchives())} disabled={!!busy}>
              <Icon name="plus" /> Add mods
            </button>
          </div>
        </div>

        {kinds.length > 1 && (
          <div className="filters" role="group" aria-label="Filter by type">
            <button className={!kindFilter ? "pill active" : "pill"} onClick={() => setKindFilter(null)}>
              All <span>{mods.length}</span>
            </button>
            {kinds.map((k) => (
              <button
                key={k}
                className={kindFilter === k ? "pill active" : "pill"}
                onClick={() => setKindFilter(kindFilter === k ? null : k)}
              >
                {KIND_LABELS[k] ?? k} <span>{mods.filter((m) => m.mod.kinds.includes(k)).length}</span>
              </button>
            ))}
          </div>
        )}

        {loadout && mods.length === 0 ? (
          <div className="empty-mods">
            <div className="drop-art">
              <Icon name="download" size={32} />
            </div>
            <h2>No mods yet</h2>
            <p>
              Drag mod archives (.zip, .7z, .rar) here, or click <strong>Mod Manager Download</strong> on a Nexus mod
              page and it lands here automatically.
            </p>
            <div className="row">
              <button className="primary" onClick={async () => install(await pickArchives())}>
                <Icon name="plus" /> Add mods
              </button>
              {domain && (
                <button onClick={() => openExternal(nexusUrls.gameMods(domain))}>
                  <Icon name="external" /> Browse Nexus Mods
                </button>
              )}
            </div>
          </div>
        ) : (
          <>
            <div className="list-caption">
              <span>
                {plural(enabledCount, "mod")} enabled of {mods.length}
                {query || kindFilter ? ` · showing ${filtered.length}` : ""}
              </span>
              <span className="muted">
                {loadout?.archive_order === "ByList"
                  ? "Lower in the list wins, including .archive load order. Drag to reorder."
                  : "Lower in the list wins when mods contain the same file. Drag to reorder."}
              </span>
            </div>
            <ModList
              mods={filtered}
              reorderable={!query && !kindFilter}
              selectedId={selected}
              onSelect={(modId) => setSelected(modId === selected ? null : modId)}
              onToggle={async (modId, enabled) => {
                const next = await run(enabled ? "Enabling" : "Disabling", () => api.setModEnabled(id, modId, enabled));
                if (next) setLoadout(next);
              }}
              onReorder={async (order) => {
                // Optimistic: show the new order immediately.
                setLoadout((l) => l && { ...l, mods: order.map((oid) => l.mods.find((m) => m.mod.id === oid)!) });
                const next = await run("Reordering", () => api.reorderMods(id, order));
                if (next) setLoadout(next);
                else void reload();
              }}
            />
          </>
        )}

          </>
        )}

        {loadout?.pending_changes && (
          <div className="apply-bar" role="status">
            <Icon name="sparkle" />
            <span>
              <strong>Changes aren't in the game yet.</strong>{" "}
              <span className="muted">Apply now, or they're applied when you press Play.</span>
            </span>
            <button className="primary" onClick={apply} disabled={!!busy}>
              {busy === "Applying" ? "Applying…" : "Apply changes"}
            </button>
          </div>
        )}
        {loadout && !loadout.pending_changes && loadout.deployed_at && (
          <p className="synced muted">
            <Icon name="check" size={12} /> Game folder matches “{activeProfile?.name}” · applied {timeAgo(loadout.deployed_at)}
          </p>
        )}
      </div>

      {selectedMod && (
        <ModDetails
          entry={selectedMod}
          index={mods.indexOf(selectedMod)}
          total={mods.length}
          gameDomain={domain ?? null}
          nexus={nexus}
          onClose={() => setSelected(null)}
          onMove={async (delta) => {
            const order = mods.map((m) => m.mod.id);
            const from = order.indexOf(selectedMod.mod.id);
            const to = Math.max(0, Math.min(order.length - 1, from + delta));
            order.splice(to, 0, ...order.splice(from, 1));
            const next = await run("Reordering", () => api.reorderMods(id, order));
            if (next) setLoadout(next);
          }}
          onRemove={() => setDialog({ kind: "remove-mod", modId: selectedMod.mod.id, name: selectedMod.mod.name })}
          onRename={() => setDialog({ kind: "rename-mod", modId: selectedMod.mod.id, name: selectedMod.mod.name })}
          onEnableMod={enableMod}
          onIdentify={async () => {
            const next = await run("Looking up on Nexus", () => api.identifyMod(id, selectedMod.mod.id));
            if (next) {
              setLoadout(next);
              const linked = next.mods.find((m) => m.mod.id === selectedMod.mod.id)?.mod;
              if (linked)
                toast(
                  "success",
                  `Linked to ${linked.name}`,
                  linked.nexus_file_id
                    ? "Update checks now cover this mod."
                    : "Found the mod, but not the exact file; update checks need a re-download via Mod Manager Download.",
                );
            }
          }}
          onUpdate={async () => {
            const started = await run("Starting update", () => api.updateMod(selectedMod.mod.id));
            if (started) toast("info", `Updating ${selectedMod.mod.name}`, "Follow progress in Downloads.");
          }}
          onPin={async (pinned) => {
            const next = await run(pinned ? "Pinning" : "Unpinning", () => api.setModPinned(id, selectedMod.mod.id, pinned));
            if (next) setLoadout(next);
          }}
          onReinstall={async (revision, changeOptions) => {
            const result = await run(changeOptions ? "Opening installer" : "Reinstalling", () =>
              api.reinstallMod(id, selectedMod.mod.id, revision, changeOptions),
            );
            if (result?.fomod) enqueueFomod(result.fomod);
            if (result?.outcome) {
              toast(
                "success",
                `${result.outcome.mod.name} reinstalled`,
                result.outcome.replayed_choices
                  ? "Same installer options as before. Apply changes to put it in the game."
                  : "Apply changes to put it in the game.",
              );
              await reload();
            }
          }}
          onRollback={async () => {
            const result = await run("Rolling back", () => api.rollbackMod(id, selectedMod.mod.id));
            if (result?.fomod) enqueueFomod(result.fomod);
            if (result?.outcome) {
              toast(
                "success",
                `${result.outcome.mod.name} rolled back${result.outcome.mod.version ? ` to ${result.outcome.mod.version}` : ""}`,
                "Apply changes to put it in the game. Pin it to stop update alerts for this version.",
              );
              await reload();
            }
          }}
          onSkipUpdate={async () => {
            const next = await run("Skipping update", () => api.ignoreUpdate(id, selectedMod.mod.id, true));
            if (next) {
              setLoadout(next);
              toast("info", "Update skipped", "You'll hear about the next version.");
            }
          }}
        />
      )}

      {dropping && (
        <div className="drop-overlay">
          <div>
            <Icon name="download" size={40} />
            <h2>Drop to install</h2>
            <p>Mods are added to “{activeProfile?.name}” and enabled.</p>
          </div>
        </div>
      )}

      {busy === "Installing" && <div className="busy-overlay">Installing…</div>}

      {importReport && (
        <Dialog
          title={`Imported “${importReport.profile_name}”`}
          onClose={() => setImportReport(null)}
          actions={
            <button className="primary" onClick={() => setImportReport(null)}>
              Done
            </button>
          }
        >
          <p>
            {importReport.matched} mods matched your library. These {importReport.missing.length} aren't installed yet:
          </p>
          <ul className="missing-mods">
            {importReport.missing.map((m) => (
              <li key={`${m.name}-${m.nexus_file_id}`}>
                <span>
                  {m.name} {m.version && <span className="muted small">{m.version}</span>}
                </span>
                {domain && m.nexus_mod_id ? (
                  <button onClick={() => openExternal(nexusUrls.filesPage(domain, m.nexus_mod_id!, m.nexus_file_id))}>
                    <Icon name="external" /> Get
                  </button>
                ) : (
                  <span className="muted small">Not from Nexus</span>
                )}
              </li>
            ))}
          </ul>
          <p className="muted small">Install them, then apply the profile.</p>
        </Dialog>
      )}

      {dialog?.kind === "new-profile" && (
        <PromptDialog
          title="New profile"
          label="Name"
          confirmLabel="Create"
          extra={
            <label className="check">
              <input type="checkbox" checked={copyProfile} onChange={(e) => setCopyProfile(e.target.checked)} />
              Start with the mods enabled in “{activeProfile?.name}”
            </label>
          }
          onSubmit={async (name) => {
            const next = await run("Creating profile", () => api.createProfile(id, name, copyProfile));
            if (next) setLoadout(next);
          }}
          onClose={() => setDialog(null)}
        />
      )}
      {dialog?.kind === "rename-profile" && activeProfile && (
        <PromptDialog
          title="Rename profile"
          label="Name"
          initial={activeProfile.name}
          confirmLabel="Rename"
          onSubmit={async (name) => {
            const next = await run("Renaming profile", () => api.renameProfile(id, activeProfile.id, name));
            if (next) setLoadout(next);
          }}
          onClose={() => setDialog(null)}
        />
      )}
      {dialog?.kind === "delete-profile" && activeProfile && (
        <ConfirmDialog
          title={`Delete “${activeProfile.name}”?`}
          message="The profile's mod choices and order are deleted. The mods themselves stay installed."
          confirmLabel="Delete profile"
          danger
          onConfirm={async () => {
            const next = await run("Deleting profile", () => api.deleteProfile(id, activeProfile.id));
            if (next) setLoadout(next);
          }}
          onClose={() => setDialog(null)}
        />
      )}
      {dialog?.kind === "update-links" && (
        <Dialog
          title={`${plural(dialog.links.length, "update")} to download`}
          onClose={() => setDialog(null)}
          actions={
            <>
              <button onClick={() => setDialog(null)}>Close</button>
              <button
                className="primary"
                onClick={() => {
                  dialog.links.forEach((l) => void openExternal(l.url));
                  setDialog(null);
                }}
              >
                <Icon name="external" /> Open all files pages
              </button>
            </>
          }
        >
          <p>
            Free Nexus Mods accounts download through the website. On each files page, click “Mod Manager Download”; the
            update installs over the old version here.
          </p>
          <ul className="missing-mods">
            {dialog.links.map((l) => (
              <li key={l.url}>
                <span>
                  {l.mod_name} {l.version && <span className="muted small">{l.version}</span>}
                </span>
                <button onClick={() => openExternal(l.url)}>
                  <Icon name="external" /> Files page
                </button>
              </li>
            ))}
          </ul>
        </Dialog>
      )}
      {dialog?.kind === "purge" && (
        <ConfirmDialog
          title="Remove all mods from the game folder?"
          message="Every file this app placed is removed and replaced game files are restored. Your mods stay in the library; apply again to bring them back."
          confirmLabel="Remove from game"
          danger
          onConfirm={async () => {
            const report = await run("Removing mods", () => api.purge(id));
            if (report) {
              toast("success", "Game folder restored", describeReport(report));
              if (report.warnings.length) toast("info", "Check these files", report.warnings.join("\n"));
              await reload();
            }
          }}
          onClose={() => setDialog(null)}
        />
      )}
      {dialog?.kind === "rename-mod" && (
        <PromptDialog
          title="Rename mod"
          label="Name"
          initial={dialog.name}
          confirmLabel="Rename"
          onSubmit={async (name) => {
            const next = await run("Renaming mod", () => api.renameMod(id, dialog.modId, name));
            if (next) setLoadout(next);
          }}
          onClose={() => setDialog(null)}
        />
      )}
      {dialog?.kind === "remove-mod" && (
        <ConfirmDialog
          title={`Remove ${dialog.name}?`}
          message="It's deleted from the library and every profile. Its files leave the game folder when you next apply."
          confirmLabel="Remove mod"
          danger
          onConfirm={async () => {
            const next = await run("Removing mod", () => api.removeMod(id, dialog.modId));
            if (next) {
              setLoadout(next);
              setSelected(null);
            }
          }}
          onClose={() => setDialog(null)}
        />
      )}
    </div>
  );
}
