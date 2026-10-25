import { useCallback, useEffect, useState } from "react";
import { api, errorMessage, nexusUrls, openExternal, reveal } from "../api";
import type { ArchiveOrder } from "../bindings/ArchiveOrder";
import type { DeploymentCheck } from "../bindings/DeploymentCheck";
import type { DriftKind } from "../bindings/DriftKind";
import type { UpdateLink } from "../bindings/UpdateLink";
import type { UnownedGroup } from "../bindings/UnownedGroup";
import { isUnmet, stateLabel } from "../deps";
import type { Loadout } from "../bindings/Loadout";
import type { LogReport } from "../bindings/LogReport";
import type { FrameworkStatus } from "../bindings/FrameworkStatus";
import { Dialog } from "../components/Dialog";
import { Icon } from "../components/Icon";
import { useToast } from "../components/Toasts";
import { formatBytes, plural, timeAgo } from "../format";

/**
 * Why mods might not be working: the game version and recent updates,
 * framework status, archive load order and errors from the game's logs.
 */
export function HealthView({
  instanceId,
  loadout,
  gameDomain,
  onLoadout,
  onEnableMod,
}: {
  instanceId: string;
  loadout: Loadout;
  gameDomain: string | null;
  onLoadout: (loadout: Loadout) => void;
  onEnableMod: (modId: string) => void;
}) {
  const toast = useToast();
  const [logs, setLogs] = useState<LogReport[] | null>(null);
  const [loadingLogs, setLoadingLogs] = useState(false);
  const [onlyErrors, setOnlyErrors] = useState(true);
  const [viewing, setViewing] = useState<{ log: LogReport; lines: string[] | null } | null>(null);

  const loadLogs = useCallback(async () => {
    setLoadingLogs(true);
    try {
      setLogs(await api.gameLogs(instanceId));
    } catch (e) {
      toast("error", "Couldn't read the game's logs", errorMessage(e));
    } finally {
      setLoadingLogs(false);
    }
  }, [instanceId, toast]);

  useEffect(() => {
    void loadLogs();
  }, [loadLogs]);

  const setOrder = async (order: ArchiveOrder) => {
    try {
      onLoadout(await api.setArchiveOrder(instanceId, order));
    } catch (e) {
      toast("error", "Couldn't change the load order setting", errorMessage(e));
    }
  };

  const openLog = async (log: LogReport) => {
    setViewing({ log, lines: null });
    try {
      const lines = await api.readLog(instanceId, log.relative_path);
      setViewing({ log, lines });
    } catch (e) {
      setViewing(null);
      toast("error", "Couldn't open the log", errorMessage(e));
    }
  };

  const game = loadout.game;
  const relevant = loadout.frameworks.filter((f) => f.needed_by.length > 0 || f.state.state !== "NotInstalled");
  const others = loadout.frameworks.filter((f) => !relevant.includes(f));
  const totalErrors = logs?.reduce((n, l) => n + l.error_count, 0) ?? 0;

  return (
    <div className="health">
      <section className="card">
        <h2>Game</h2>
        <dl className="facts">
          <dt>Version</dt>
          <dd>{game.version ?? "Unknown"}</dd>
          {game.build_id && (
            <>
              <dt>Steam build</dt>
              <dd>{game.build_id}</dd>
            </>
          )}
          <dt>Last update</dt>
          <dd>{game.updated_at ? timeAgo(game.updated_at) : "None seen since this app started tracking it"}</dd>
        </dl>
        {game.too_old_frameworks.length > 0 && (
          <div className="notice notice-error">
            <Icon name="alert" />
            <div>
              <strong>Too old for game version {game.version}: {game.too_old_frameworks.join(", ")}</strong>
              <p>
                Mods that need {game.too_old_frameworks.length === 1 ? "it" : "them"} won't load. Update{" "}
                {game.too_old_frameworks.length === 1 ? "it" : "them"} (see Frameworks below), then apply.
              </p>
            </div>
          </div>
        )}
        {game.outdated_frameworks.length > 0 && (
          <div className="notice notice-warn">
            <Icon name="alert" />
            <div>
              <strong>The game was updated after {game.outdated_frameworks.join(", ")} was installed</strong>
              <p>
                Script extenders usually stop working after a game patch until their authors release an update. If mods
                stopped loading, check for updates first.
              </p>
            </div>
          </div>
        )}
      </section>

      <GameFolderCheck instanceId={instanceId} deployedAt={loadout.deployed_at} />

      <RequirementsCard instanceId={instanceId} loadout={loadout} onLoadout={onLoadout} />

      <HandInstalledCard instanceId={instanceId} onLoadout={onLoadout} />

      <section className="card">
        <h2>Frameworks</h2>
        <p className="muted small">The building blocks other mods need. Missing ones are the most common reason a mod does nothing.</p>
        <ul className="frameworks">
          {relevant.map((f) => (
            <FrameworkRow key={f.id} framework={f} gameDomain={gameDomain} onEnableMod={onEnableMod} />
          ))}
        </ul>
        {others.length > 0 && (
          <p className="muted small">Not needed by your mods: {others.map((f) => f.name).join(", ")}.</p>
        )}
      </section>

      {loadout.archive_order && (
        <section className="card">
          <h2>Archive load order</h2>
          <p className="muted small">
            When two .archive mods change the same game asset, the one the game loads first wins.
          </p>
          <label className="radio">
            <input
              type="radio"
              name="archive-order"
              checked={loadout.archive_order === "ByFileName"}
              onChange={() => setOrder("ByFileName")}
            />
            <span>
              <span className="radio-title">
                <strong>By file name</strong> (game default, recommended)
              </span>
              <small>The game loads archives alphabetically. Mod authors name their files to load in the right order.</small>
            </span>
          </label>
          <label className="radio">
            <input
              type="radio"
              name="archive-order"
              checked={loadout.archive_order === "ByList"}
              onChange={() => setOrder("ByList")}
            />
            <span>
              <span className="radio-title">
                <strong>By my mod list</strong>
              </span>
              <small>
                Writes <code>archive/pc/mod/modlist.txt</code> so mods lower in your list win. Only use this to fix a
                specific conflict; other tools and guides assume the default order.
              </small>
            </span>
          </label>
        </section>
      )}

      <section className="card">
        <div className="card-head-row">
          <h2>Logs</h2>
          <div className="row">
            <label className="check small">
              <input type="checkbox" checked={onlyErrors} onChange={(e) => setOnlyErrors(e.target.checked)} />
              Errors only
            </label>
            <button onClick={loadLogs} disabled={loadingLogs}>
              <Icon name="refresh" className={loadingLogs ? "spin" : undefined} /> Refresh
            </button>
          </div>
        </div>
        {logs === null ? (
          <p className="status-line">
            <Icon name="refresh" className="spin" /> Reading logs…
          </p>
        ) : logs.length === 0 ? (
          <p className="muted">
            No logs yet. Start the game with your mods applied, then come back: frameworks write their logs when the game
            starts.
          </p>
        ) : (
          <>
            <p className="muted small">
              {totalErrors === 0
                ? "No errors in the latest logs."
                : `${plural(totalErrors, "error")} in the latest logs. Errors in a framework's own log usually matter most.`}
            </p>
            <ul className="logs">
              {logs.map((log) => {
                const entries = log.entries.filter((e) => !onlyErrors || e.severity === "Error");
                return (
                  <li key={log.relative_path} className="log">
                    <div className="log-head">
                      <strong>{log.name}</strong>
                      <span className="muted small">{log.relative_path}</span>
                      <span className="log-counts">
                        {log.error_count > 0 && <span className="badge badge-error">{plural(log.error_count, "error")}</span>}
                        {log.warning_count > 0 && <span className="badge badge-lose">{plural(log.warning_count, "warning")}</span>}
                        {log.error_count === 0 && log.warning_count === 0 && <span className="badge badge-win">clean</span>}
                      </span>
                    </div>
                    <p className="muted small">
                      Written {timeAgo(log.modified_at)} · {formatBytes(log.size_bytes)}
                      {log.stale && " · from before your last Apply, start the game again for fresh results"}
                    </p>
                    {entries.length > 0 && (
                      <ul className="log-entries">
                        {entries.slice(-8).map((e) => (
                          <li key={`${e.line}-${e.text}`} className={e.severity === "Error" ? "sev-error" : "sev-warn"}>
                            <span className="line-no">{e.line || ""}</span>
                            {e.mod_name && <span className="mod-chip">{e.mod_name}</span>}
                            <code>{e.text}</code>
                          </li>
                        ))}
                      </ul>
                    )}
                    <div className="row">
                      <button onClick={() => openLog(log)}>View log</button>
                      <button className="link-button" onClick={() => reveal(log.path)}>
                        Show in folder
                      </button>
                    </div>
                  </li>
                );
              })}
            </ul>
          </>
        )}
      </section>

      {viewing && (
        <Dialog
          title={viewing.log.name}
          onClose={() => setViewing(null)}
          actions={<button onClick={() => setViewing(null)}>Close</button>}
        >
          <p className="muted small">{viewing.log.relative_path} · last 400 lines</p>
          <pre className="log-view">{viewing.lines ? viewing.lines.join("\n") : "Loading…"}</pre>
        </Dialog>
      )}
    </div>
  );
}

function FrameworkRow({
  framework: f,
  gameDomain,
  onEnableMod,
}: {
  framework: FrameworkStatus;
  gameDomain: string | null;
  onEnableMod: (modId: string) => void;
}) {
  const needed = f.needed_by.length > 0;
  const state = f.state;
  const page = gameDomain && f.nexus_mod_id ? nexusUrls.modPage(gameDomain, f.nexus_mod_id) : null;
  return (
    <li className="framework">
      <span
        className={`status-pill ${
          f.too_old
            ? "bad"
            : state.state === "Installed" || state.state === "InstalledManually"
              ? "ok"
              : needed
                ? "bad"
                : "warn"
        }`}
      >
        {f.too_old
          ? "Too old"
          : state.state === "Installed"
            ? "Installed"
            : state.state === "InstalledManually"
              ? "Installed manually"
              : state.state === "Disabled"
                ? "Disabled"
                : "Missing"}
      </span>
      <div className="framework-text">
        <strong>
          {f.name}
          {f.version && <span className="muted small"> {f.version}</span>}
        </strong>
        <small className="muted">
          {f.too_old && `This game version needs ${f.min_version} or newer. `}
          {needed ? `Needed by ${f.needed_by.join(", ")}` : "Not needed by your enabled mods"}
          {state.state === "Installed" && state.mod_name !== f.name && ` · from “${state.mod_name}”`}
        </small>
      </div>
      {f.too_old && page && (
        <button className="primary" onClick={() => openExternal(page)}>
          <Icon name="external" /> Get {f.min_version}
        </button>
      )}
      {state.state === "Disabled" && (
        <button className="primary" onClick={() => onEnableMod(state.mod_id)}>
          Enable
        </button>
      )}
      {state.state === "NotInstalled" && page && (
        <button className={needed ? "primary" : undefined} onClick={() => openExternal(page)}>
          <Icon name="external" /> Get it
        </button>
      )}
    </li>
  );
}

const DRIFT_TEXT: Record<DriftKind, { title: string; detail: string }> = {
  Missing: {
    title: "Missing from the game folder",
    detail: "Something deleted these. Apply puts them back.",
  },
  ReplacedByGame: {
    title: "Replaced by the game",
    detail:
      "A game update or file check put its own version here. Apply keeps the game's file as the new original, then places the mod's file over it again.",
  },
  Modified: {
    title: "Edited in the game folder",
    detail:
      "Changed since they were placed, probably by hand or by the mod itself. They stay as they are; if the mod is updated or removed, a copy is kept in the preserved folder first.",
  },
};

/** Compares the files this app placed with what is in the game folder now. */
function GameFolderCheck({ instanceId, deployedAt }: { instanceId: string; deployedAt: number | null }) {
  const [check, setCheck] = useState<DeploymentCheck | null>(null);
  const [checking, setChecking] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const run = useCallback(async () => {
    setChecking(true);
    setError(null);
    try {
      setCheck(await api.verifyDeployment(instanceId));
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setChecking(false);
    }
  }, [instanceId]);

  useEffect(() => {
    void run();
  }, [run, deployedAt]);

  const kinds: DriftKind[] = ["Missing", "ReplacedByGame", "Modified"];
  return (
    <section className="card" aria-busy={checking}>
      <div className="card-head-row">
        <h2>Game folder</h2>
        <button onClick={run} disabled={checking}>
          <Icon name="refresh" className={checking ? "spin" : undefined} /> {checking ? "Checking…" : "Check again"}
        </button>
      </div>
      {error && (
        <div className="notice notice-error" role="alert">
          <Icon name="alert" />
          <div>
            <strong>Couldn't check the game folder</strong>
            <p>{error} Rescan from the sidebar if the game was moved, then check again.</p>
          </div>
        </div>
      )}
      {check && check.drifted.length === 0 && (
        <p className="status-line">
          <Icon name="check" /> All {plural(check.checked, "placed file")} are as this app left them.
        </p>
      )}
      {check &&
        kinds.map((kind) => {
          const files = check.drifted.filter((d) => d.kind === kind);
          if (!files.length) return null;
          return (
            <div key={kind} className={`notice ${kind === "Modified" ? "notice-info" : "notice-warn"}`}>
              <Icon name="alert" />
              <div>
                <strong>
                  {DRIFT_TEXT[kind].title}: {plural(files.length, "file")}
                </strong>
                <p>{DRIFT_TEXT[kind].detail}</p>
                <ul className="drift-list">
                  {files.slice(0, 8).map((f) => (
                    <li key={f.target}>
                      <code>{f.target}</code>
                      {f.mod_name && <span className="muted"> · {f.mod_name}</span>}
                    </li>
                  ))}
                  {files.length > 8 && <li className="muted">and {files.length - 8} more</li>}
                </ul>
              </div>
            </div>
          );
        })}
    </section>
  );
}

/** Requirements across the enabled mods: what's unmet, cycles, and getting them. */
function RequirementsCard({
  instanceId,
  loadout,
  onLoadout,
}: {
  instanceId: string;
  loadout: Loadout;
  onLoadout: (loadout: Loadout) => void;
}) {
  const toast = useToast();
  const [busy, setBusy] = useState<string | null>(null);
  const [links, setLinks] = useState<UpdateLink[] | null>(null);
  const unmet = loadout.mods
    .filter((m) => m.enabled)
    .map((m) => ({ mod: m.mod.name, deps: m.requires.filter(isUnmet) }))
    .filter((m) => m.deps.length > 0);
  const gettable = unmet.some((m) => m.deps.some((d) => d.state.state !== "Disabled" && d.nexus_mod_id));

  const act = async (label: string, work: () => Promise<void>) => {
    setBusy(label);
    try {
      await work();
    } catch (e) {
      toast("error", `Couldn't ${label.toLowerCase()}`, `${errorMessage(e)} Check your connection and try again.`);
    } finally {
      setBusy(null);
    }
  };

  return (
    <section className="card" aria-busy={busy !== null}>
      <div className="card-head-row">
        <h2>Requirements</h2>
        <div className="row">
          <button
            disabled={busy !== null}
            onClick={() =>
              act("Look up requirements", async () => {
                const result = await api.fetchRequirements(instanceId, true);
                onLoadout(result.loadout);
                toast("success", `Looked up ${result.updated === 1 ? "1 mod" : `${result.updated} mods`} on Nexus Mods`);
              })
            }
            title="Read the requirements listed on each mod's Nexus page"
          >
            <Icon name="refresh" className={busy === "Look up requirements" ? "spin" : undefined} /> Look up on Nexus
          </button>
          {gettable && (
            <button
              className="primary"
              disabled={busy !== null}
              onClick={() =>
                act("Get missing mods", async () => {
                  const result = await api.getMissing(instanceId);
                  if (result.queued) toast("info", `Downloading ${result.queued === 1 ? "1 mod" : `${result.queued} mods`}`, "Follow progress in Downloads.");
                  if (result.links.length) setLinks(result.links);
                })
              }
            >
              <Icon name="download" /> Get all missing
            </button>
          )}
        </div>
      </div>
      {unmet.length === 0 ? (
        <p className="status-line">
          <Icon name="check" /> Every enabled mod has what it needs, as far as the app can tell.
        </p>
      ) : (
        <ul className="drift-list">
          {unmet.map((m) => (
            <li key={m.mod}>
              <strong>{m.mod}</strong>: {m.deps.map((d) => `${d.name} (${stateLabel(d).toLowerCase()})`).join(", ")}
            </li>
          ))}
        </ul>
      )}
      {loadout.dependency_cycles.length > 0 && (
        <div className="notice notice-warn">
          <Icon name="alert" />
          <div>
            <strong>Mods that need each other in a circle</strong>
            <p>
              {loadout.dependency_cycles.map((c) => c.join(" → ") + " → " + c[0]).join("; ")}. Usually one of these
              requirements is wrong; remove the one you added or check the mods' pages.
            </p>
          </div>
        </div>
      )}
      {links && (
        <Dialog
          title="Download from Nexus Mods"
          onClose={() => setLinks(null)}
          actions={
            <>
              <button onClick={() => setLinks(null)}>Close</button>
              <button
                className="primary"
                onClick={() => {
                  links.forEach((l) => void openExternal(l.url));
                  setLinks(null);
                }}
              >
                <Icon name="external" /> Open all pages
              </button>
            </>
          }
        >
          <p>On each page, click “Mod Manager Download” on the main file; it installs here.</p>
          <ul className="missing-mods">
            {links.map((l) => (
              <li key={l.url}>
                <span>{l.mod_name}</span>
                <button onClick={() => openExternal(l.url)}>
                  <Icon name="external" /> Files page
                </button>
              </li>
            ))}
          </ul>
        </Dialog>
      )}
    </section>
  );
}

/** Mods put in the game folder by hand, offered for adoption. */
function HandInstalledCard({ instanceId, onLoadout }: { instanceId: string; onLoadout: (loadout: Loadout) => void }) {
  const toast = useToast();
  const [groups, setGroups] = useState<UnownedGroup[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [adopting, setAdopting] = useState<string | null>(null);

  const scan = useCallback(async () => {
    setError(null);
    try {
      setGroups(await api.unownedMods(instanceId));
    } catch (e) {
      setError(errorMessage(e));
    }
  }, [instanceId]);

  useEffect(() => {
    void scan();
  }, [scan]);

  const adopt = async (g: UnownedGroup) => {
    setAdopting(g.name);
    try {
      onLoadout(await api.adoptMod(instanceId, g.name, g.files));
      toast("success", `${g.name} is now a mod in your list`, "Its files stayed where they were. Removing it takes them out of the game.");
      await scan();
    } catch (e) {
      toast("error", `Couldn't adopt ${g.name}`, `${errorMessage(e)} Scan again and retry.`);
    } finally {
      setAdopting(null);
    }
  };

  if (groups && groups.length === 0 && !error) return null;
  return (
    <section className="card">
      <div className="card-head-row">
        <h2>Installed by hand</h2>
        <button onClick={scan} disabled={adopting !== null}>
          <Icon name="refresh" /> Scan again
        </button>
      </div>
      <p className="muted small">
        Files in the game's mod folders that this app didn't put there. Adopting one copies it into the library and
        manages it like any other mod; the game folder stays as it is.
      </p>
      {error && (
        <p className="error-text" role="alert">
          {error} Check that the game folder is reachable, then scan again.
        </p>
      )}
      <ul className="missing-mods">
        {groups?.map((g) => (
          <li key={`${g.kind}-${g.name}`}>
            <span>
              <strong>{g.name}</strong>{" "}
              <span className="muted small">
                {g.kind} · {g.files.length === 1 ? "1 file" : `${g.files.length} files`} · {formatBytes(g.size_bytes)}
              </span>
            </span>
            <button disabled={adopting !== null} onClick={() => adopt(g)}>
              {adopting === g.name ? "Adopting…" : "Adopt as mod"}
            </button>
          </li>
        ))}
      </ul>
    </section>
  );
}
