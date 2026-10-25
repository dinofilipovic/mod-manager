import { useEffect, useState } from "react";
import { api, nexusUrls, openExternal } from "../api";
import type { LoadoutMod } from "../bindings/LoadoutMod";
import type { NexusStatus } from "../bindings/NexusStatus";
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
  onIdentify,
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
  onIdentify: () => Promise<void>;
}) {
  const m = entry.mod;
  const [files, setFiles] = useState<string[] | null>(null);
  const [showFiles, setShowFiles] = useState(false);
  const [identifying, setIdentifying] = useState(false);

  useEffect(() => {
    setFiles(null);
    setShowFiles(false);
  }, [m.id, m.revision]);

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
          <strong>Version {m.update_version ?? "newer"} is available</strong>
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
        {m.archive_name && (
          <>
            <dt>Archive</dt>
            <dd className="break">{m.archive_name}</dd>
          </>
        )}
      </dl>

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
