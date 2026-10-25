import { useEffect, useState } from "react";
import { api, errorMessage, nexusUrls, openExternal } from "../api";
import type { LoadoutMod } from "../bindings/LoadoutMod";
import type { NexusStatus } from "../bindings/NexusStatus";
import type { ModChangelog } from "../bindings/ModChangelog";
import type { RevisionInfo } from "../bindings/RevisionInfo";
import type { DependencyKind } from "../bindings/DependencyKind";
import type { DependencyStatus } from "../bindings/DependencyStatus";
import { SOURCE_LABELS, isUnmet, stateLabel } from "../deps";
import { Icon } from "../components/Icon";
import { KIND_LABELS, formatBytes, plural, timeAgo } from "../format";

export function ModDetails({
  entry,
  index,
  total,
  gameDomain,
  nexus,
  onClose,
  onMove,
  onRemove,
  onRename,
  onUpdate,
  onPin,
  onSkipUpdate,
  onReinstall,
  onRollback,
  onAddDependency,
  onRemoveDependency,
  libraryMods,
  onIdentify,
  onEnableMod,
}: {
  entry: LoadoutMod;
  index: number;
  total: number;
  gameDomain: string | null;
  nexus: NexusStatus | null;
  onClose: () => void;
  onMove: (delta: number) => void;
  onRemove: () => void;
  onRename: () => void;
  onUpdate: () => void;
  onPin: (pinned: boolean) => void;
  onSkipUpdate: () => void;
  onReinstall: (revision: number, changeOptions: boolean) => Promise<void>;
  onRollback: () => Promise<void>;
  onAddDependency: (kind: DependencyKind, target: string, optional: boolean) => Promise<boolean>;
  onRemoveDependency: (kind: DependencyKind, target: string) => void;
  /** Other mods of the library, for declaring a requirement. */
  libraryMods: { id: string; name: string }[];
  onIdentify: () => Promise<void>;
  onEnableMod: (modId: string) => void;
}) {
  const m = entry.mod;
  const [files, setFiles] = useState<string[] | null>(null);
  const [showFiles, setShowFiles] = useState(false);
  const [identifying, setIdentifying] = useState(false);
  const [changelog, setChangelog] = useState<
    { state: "hidden" } | { state: "loading" } | { state: "shown"; log: ModChangelog } | { state: "error"; message: string }
  >({ state: "hidden" });

  const [revisions, setRevisions] = useState<RevisionInfo[] | null>(null);
  const [historyBusy, setHistoryBusy] = useState(false);

  useEffect(() => {
    setFiles(null);
    setShowFiles(false);
    setChangelog({ state: "hidden" });
    setRevisions(null);
    api.modRevisions(m.id).then(setRevisions, () => setRevisions([]));
  }, [m.id, m.revision]);

  const versionAction = async (work: () => Promise<void>) => {
    setHistoryBusy(true);
    try {
      await work();
    } finally {
      setHistoryBusy(false);
    }
  };
  const current = revisions?.find((r) => r.current);
  const canRollBack = revisions?.some((r) => !r.current && r.archive_kept) ?? false;

  const loadChangelog = async (refresh = false) => {
    setChangelog({ state: "loading" });
    try {
      setChangelog({ state: "shown", log: await api.modChangelog(m.id, refresh) });
    } catch (e) {
      setChangelog({ state: "error", message: errorMessage(e) });
    }
  };

  useEffect(() => {
    if (showFiles && !files) void api.modFiles(m.id).then(setFiles);
  }, [showFiles, files, m.id]);

  const nexusLink = gameDomain && m.nexus_mod_id ? nexusUrls.modPage(gameDomain, m.nexus_mod_id) : null;

  return (
    <aside className="details" aria-label={`${m.name} details`}>
      <div className="details-head">
        <button className="icon-button" onClick={onClose} aria-label="Close details">
          <Icon name="x" />
        </button>
      </div>
      {m.picture_url && <img className="details-image" src={m.picture_url} alt="" />}
      <h2>{m.name}</h2>
      <p className="muted">
        {[m.version && `v${m.version}`, m.author && `by ${m.author}`].filter(Boolean).join(" · ") || "Local archive"}
      </p>
      <div className="mod-meta">
        {m.kinds.map((k) => (
          <span key={k} className={`kind kind-${k}`}>
            {KIND_LABELS[k] ?? k}
          </span>
        ))}
      </div>
      {m.summary && <p className="summary">{m.summary}</p>}

      {entry.enabled && entry.requires.some(isUnmet) && (
        <div className="callout callout-bad">
          <strong>
            Won't work yet:{" "}
            {entry.requires
              .filter(isUnmet)
              .map((d) => `${d.name} ${stateLabel(d).toLowerCase()}`)
              .join(", ")}
          </strong>
          <p>See Requires below for what to do.</p>
        </div>
      )}

      {!m.nexus_file_id && m.archive_name && gameDomain && (
        <div className="callout callout-neutral">
          <strong>Not linked to Nexus Mods</strong>
          <p>
            {nexus?.user
              ? "Link it to fill in the name, author and image, and to get update alerts."
              : "Connect your Nexus Mods account in Settings to link it and get update alerts."}
          </p>
          {nexus?.user && (
            <button
              disabled={identifying}
              onClick={async () => {
                setIdentifying(true);
                await onIdentify();
                setIdentifying(false);
              }}
            >
              <Icon name="link" className={identifying ? "spin" : undefined} />
              {identifying ? "Looking it up…" : "Find on Nexus"}
            </button>
          )}
        </div>
      )}

      {m.update_file_id && gameDomain && m.nexus_mod_id && (
        <div className="callout">
          <strong>
            Version {m.update_version ?? "newer"} is available
            {m.version && <span className="muted"> (you have {m.version})</span>}
          </strong>
          {nexus?.user?.is_premium ? (
            <button className="primary" onClick={onUpdate}>
              <Icon name="download" /> Update now
            </button>
          ) : (
            <>
              <p>On the files page, click “Mod Manager Download” and the update installs over this one.</p>
              <button className="primary" onClick={() => openExternal(nexusUrls.filesPage(gameDomain, m.nexus_mod_id!, m.update_file_id))}>
                <Icon name="external" /> Open files page
              </button>
            </>
          )}
          <div className="row">
            <button
              className="link-button"
              onClick={() => (changelog.state === "shown" ? setChangelog({ state: "hidden" }) : loadChangelog())}
              disabled={changelog.state === "loading"}
            >
              {changelog.state === "loading" ? "Loading changelog…" : changelog.state === "shown" ? "Hide changes" : "What's new?"}
            </button>
            <button className="link-button" onClick={onSkipUpdate} title="Don't offer this version again">
              Skip this version
            </button>
          </div>
          {changelog.state === "error" && (
            <p className="error-text" role="alert">
              {changelog.message}
            </p>
          )}
          {changelog.state === "shown" &&
            (changelog.log.versions.length ? (
              <div className="changelog">
                {changelog.log.versions.map((v) => (
                  <div key={v.version}>
                    <strong>{v.version}</strong>
                    <ul>
                      {v.lines.map((line, i) => (
                        <li key={i}>{line}</li>
                      ))}
                    </ul>
                  </div>
                ))}
                <button className="link-button small" onClick={() => loadChangelog(true)}>
                  Refresh · fetched {timeAgo(changelog.log.fetched_at)}
                </button>
              </div>
            ) : (
              <p className="muted small">The author didn't write a changelog for this version.</p>
            ))}
        </div>
      )}

      {entry.enabled && (entry.overrides.length > 0 || entry.overridden_by.length > 0) && (
        <div className="conflicts">
          <h3>
            <Icon name="bolt" size={12} /> Shares {plural(entry.conflict_files, "file")} with other mods
          </h3>
          {entry.overrides.length > 0 && (
            <p>
              <span className="win">Wins over</span> {entry.overrides.join(", ")}
            </p>
          )}
          {entry.overridden_by.length > 0 && (
            <p>
              <span className="lose">Loses to</span> {entry.overridden_by.join(", ")}
            </p>
          )}
          <p className="muted">Move it lower in the list to make it win.</p>
        </div>
      )}

      {entry.enabled && entry.resource_conflicts.length > 0 && (
        <div className="conflicts">
          <h3>
            <Icon name="layers" size={12} /> Changes the same game resources as other archives
          </h3>
          {entry.resource_conflicts.map((c) => {
            const wins = c.winner_mod_id === m.id;
            return (
              <p key={`${c.winner_archive}-${c.loser_archive}`}>
                <span className={wins ? "win" : "lose"}>{wins ? "Wins over" : "Loses to"}</span>{" "}
                {wins ? c.loser_mod : c.winner_mod}{" "}
                <span className="muted small">
                  ({c.resources === 1 ? "1 resource" : `${c.resources} resources`},{" "}
                  {wins ? c.winner_archive : c.loser_archive} vs {wins ? c.loser_archive : c.winner_archive})
                </span>
              </p>
            );
          })}
          <p className="muted">The archive the game loads first wins; see Archive load order in Health.</p>
        </div>
      )}

      <dl className="facts">
        <dt>Load order</dt>
        <dd>
          {index + 1} of {total}
          <span className="row tight">
            <button className="icon-button" disabled={index === 0} onClick={() => onMove(-1)} aria-label="Move up">
              <Icon name="up" size={14} />
            </button>
            <button className="icon-button" disabled={index === total - 1} onClick={() => onMove(1)} aria-label="Move down">
              <Icon name="down" size={14} />
            </button>
          </span>
        </dd>
        <dt>Files</dt>
        <dd>
          {m.file_count} · {formatBytes(m.size_bytes)}
        </dd>
        <dt>Installed</dt>
        <dd>{timeAgo(m.installed_at)}</dd>
        {m.nexus_mod_id && (
          <>
            <dt>Updates</dt>
            <dd>
              <label className="toggle-inline">
                <input type="checkbox" checked={m.pinned} onChange={(e) => onPin(e.target.checked)} />
                Keep this version (pin)
              </label>
            </dd>
          </>
        )}
        {m.archive_name && (
          <>
            <dt>Archive</dt>
            <dd className="break">{m.archive_name}</dd>
          </>
        )}
      </dl>

      {entry.checks.length > 0 && (
        <div className={`callout ${entry.wont_load ? "callout-bad" : "callout-neutral"}`}>
          <strong>{entry.wont_load ? "Won't load as installed" : "Worth checking"}</strong>
          <ul className="check-list">
            {entry.checks.slice(0, 8).map((c, i) => (
              <li key={i} className={c.severity === "Error" ? "error-text" : undefined}>
                {c.message}
              </li>
            ))}
            {entry.checks.length > 8 && <li className="muted">and {entry.checks.length - 8} more</li>}
          </ul>
          {entry.wont_load && (
            <p className="small">Download it again, try another version, or ask on the mod's page; removing it keeps the game clean.</p>
          )}
        </div>
      )}

      <Requirements
        entry={entry}
        gameDomain={gameDomain}
        libraryMods={libraryMods}
        onEnableMod={onEnableMod}
        onAdd={onAddDependency}
        onRemove={onRemoveDependency}
      />

      {revisions && revisions.length > 0 && (
        <section className="history" aria-busy={historyBusy}>
          <h3>Versions</h3>
          <ul>
            {revisions.slice(0, 6).map((r) => (
              <li key={r.revision} className={r.current ? "current" : undefined}>
                <span>
                  {r.version ? `v${r.version}` : `Revision ${r.revision}`}
                  {r.current && <span className="badge">Installed</span>}
                  <span className="muted small"> · {timeAgo(r.installed_at)}</span>
                </span>
                {!r.archive_kept ? (
                  <span className="muted small" title="Its archive was deleted (Mod archives in Settings)">
                    Archive not kept
                  </span>
                ) : r.current ? null : (
                  <button className="link-button" disabled={historyBusy} onClick={() => versionAction(() => onReinstall(r.revision, false))}>
                    Install this version
                  </button>
                )}
              </li>
            ))}
          </ul>
          <div className="row">
            {canRollBack && (
              <button disabled={historyBusy} onClick={() => versionAction(onRollback)}>
                <Icon name="undo" /> Roll back
              </button>
            )}
            {current?.archive_kept && (
              <button disabled={historyBusy} onClick={() => versionAction(() => onReinstall(current.revision, false))}>
                <Icon name="refresh" /> Reinstall
              </button>
            )}
            {current?.archive_kept && current.has_choices && (
              <button disabled={historyBusy} onClick={() => versionAction(() => onReinstall(current.revision, true))}>
                Change options
              </button>
            )}
          </div>
        </section>
      )}

      <button className="link-button" onClick={() => setShowFiles(!showFiles)}>
        <Icon name="chevron" size={12} className={showFiles ? "rot90" : undefined} /> {showFiles ? "Hide" : "Show"} files
      </button>
      {showFiles && (
        <ul className="file-list">
          {files ? files.map((f) => <li key={f}>{f}</li>) : <li className="muted">Loading…</li>}
        </ul>
      )}

      <div className="details-actions">
        {nexusLink && (
          <button onClick={() => openExternal(nexusLink)}>
            <Icon name="external" /> Nexus page
          </button>
        )}
        <button onClick={onRename}>Rename</button>
        <button className="danger" onClick={onRemove}>
          <Icon name="trash" /> Remove
        </button>
      </div>
    </aside>
  );
}

function Requirements({
  entry,
  gameDomain,
  libraryMods,
  onEnableMod,
  onAdd,
  onRemove,
}: {
  entry: LoadoutMod;
  gameDomain: string | null;
  libraryMods: { id: string; name: string }[];
  onEnableMod: (modId: string) => void;
  onAdd: (kind: DependencyKind, target: string, optional: boolean) => Promise<boolean>;
  onRemove: (kind: DependencyKind, target: string) => void;
}) {
  const [adding, setAdding] = useState(false);
  const [pick, setPick] = useState("");
  const [nexusId, setNexusId] = useState("");
  const [optional, setOptional] = useState(false);
  const [saving, setSaving] = useState(false);

  const action = (d: DependencyStatus) => {
    if (d.state.state === "Disabled") {
      const modId = d.state.mod_id;
      return (
        <button className="primary small" onClick={() => onEnableMod(modId)}>
          Enable
        </button>
      );
    }
    if ((d.state.state === "Missing" || d.state.state === "TooOld") && gameDomain && d.nexus_mod_id) {
      return (
        <button className="small" onClick={() => openExternal(nexusUrls.modPage(gameDomain, d.nexus_mod_id!))}>
          <Icon name="external" /> Get
        </button>
      );
    }
    if (d.kind === "External" && d.url) {
      return (
        <button className="small" onClick={() => openExternal(d.url!)}>
          <Icon name="external" /> Open
        </button>
      );
    }
    return null;
  };

  const add = async () => {
    const target = pick || nexusId.trim();
    if (!target) return;
    setSaving(true);
    const ok = await onAdd(pick ? "Mod" : "NexusMod", target, optional);
    setSaving(false);
    if (ok) {
      setAdding(false);
      setPick("");
      setNexusId("");
      setOptional(false);
    }
  };

  return (
    <section className="requires">
      <h3>Requires</h3>
      {entry.requires.length === 0 ? (
        <p className="muted small">Nothing known. Nexus requirements are looked up after a download, or from Health.</p>
      ) : (
        <ul>
          {entry.requires.map((d) => (
            <li key={`${d.source}-${d.kind}-${d.target}`} className={isUnmet(d) ? "unmet" : undefined}>
              <span className={`status-pill ${isUnmet(d) ? "bad" : d.state.state === "Satisfied" ? "ok" : "warn"}`}>
                {stateLabel(d)}
              </span>
              <span className="requires-text">
                <strong>{d.name}</strong>
                {d.optional && <span className="muted"> (optional)</span>}
                <small className="muted">
                  {" "}
                  · {SOURCE_LABELS[d.source]}
                  {d.state.state === "TooOld" && ` · has ${d.state.have}, needs ${d.state.need}`}
                  {d.state.state === "Satisfied" && d.state.by !== d.name && ` · from ${d.state.by}`}
                  {d.notes && ` · ${d.notes}`}
                </small>
              </span>
              {action(d)}
              {d.source === "User" && (
                <button className="icon-button" aria-label={`Remove requirement ${d.name}`} onClick={() => onRemove(d.kind, d.target)}>
                  <Icon name="x" size={12} />
                </button>
              )}
            </li>
          ))}
        </ul>
      )}
      {entry.required_by.length > 0 && (
        <p className="small">
          <span className="muted">Required by</span> {entry.required_by.join(", ")}
        </p>
      )}
      {adding ? (
        <div className="add-requirement">
          <select value={pick} onChange={(e) => setPick(e.target.value)} aria-label="Mod it requires">
            <option value="">A mod in your library…</option>
            {libraryMods.map((m) => (
              <option key={m.id} value={m.id}>
                {m.name}
              </option>
            ))}
          </select>
          <input
            placeholder="or a Nexus mod id"
            value={nexusId}
            disabled={!!pick}
            inputMode="numeric"
            onChange={(e) => setNexusId(e.target.value)}
            aria-label="Nexus mod id"
          />
          <label className="toggle-inline small">
            <input type="checkbox" checked={optional} onChange={(e) => setOptional(e.target.checked)} /> Optional
          </label>
          <div className="row">
            <button className="primary" disabled={saving || (!pick && !nexusId.trim())} onClick={add}>
              {saving ? "Adding…" : "Add"}
            </button>
            <button onClick={() => setAdding(false)}>Cancel</button>
          </div>
        </div>
      ) : (
        <button className="link-button small" onClick={() => setAdding(true)}>
          + Add a requirement
        </button>
      )}
    </section>
  );
}
