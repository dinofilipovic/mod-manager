import { useState } from "react";
import type { View } from "../App";
import { api, errorMessage, nexusUrls, openExternal } from "../api";
import type { Download } from "../bindings/Download";
import type { GameDefinition } from "../bindings/GameDefinition";
import type { NexusStatus } from "../bindings/NexusStatus";
import { Icon } from "../components/Icon";
import { useFomodQueue } from "../components/FomodQueue";
import { useToast } from "../components/Toasts";
import { formatBytes } from "../format";

const STATUS_LABEL: Record<Download["status"]["state"], string> = {
  Queued: "Waiting",
  Downloading: "Downloading",
  Installing: "Installing",
  AwaitingChoices: "Needs your choices",
  Done: "Installed",
  Failed: "Failed",
};

export function DownloadsView({
  downloads,
  games,
  nexus,
  onDownloads,
  onNavigate,
}: {
  downloads: Download[];
  games: GameDefinition[];
  nexus: NexusStatus | null;
  onDownloads: (d: Download[]) => void;
  onNavigate: (view: View) => void;
}) {
  const toast = useToast();
  const enqueueFomod = useFomodQueue();
  const [link, setLink] = useState("");
  const finished = downloads.some((d) => d.status.state === "Done" || d.status.state === "Failed");

  const submit = async () => {
    if (link.includes("/collections/")) {
      onNavigate({ kind: "collections", link: link.trim() });
      setLink("");
      return;
    }
    try {
      await api.handleNxm(link.trim());
      setLink("");
    } catch (e) {
      toast("error", "Couldn't start the download", errorMessage(e));
    }
  };

  return (
    <div className="page">
      <header className="page-header">
        <h1>Downloads</h1>
        {finished && (
          <button onClick={async () => onDownloads(await api.clearFinishedDownloads())}>Clear finished</button>
        )}
      </header>

      {!nexus?.user ? (
        <div className="callout">
          <strong>Connect your Nexus Mods account first</strong>
          <p>Downloads use your account's API key, stored in the system keychain.</p>
          <button className="primary" onClick={() => onNavigate({ kind: "settings" })}>
            Connect Nexus Mods
          </button>
        </div>
      ) : (
        <div className="how-to">
          <p>
            On any mod page on Nexus Mods, open <strong>Files</strong> and click <strong>Mod Manager Download</strong>.
            The mod downloads and installs here automatically.
          </p>
          <div className="row">
            {games
              .filter((g) => g.stores.nexus_domain)
              .map((g) => (
                <button key={g.id} onClick={() => openExternal(nexusUrls.gameMods(g.stores.nexus_domain!))}>
                  <Icon name="external" /> {g.name} mods
                </button>
              ))}
          </div>
          <label className="paste">
            <span className="muted">Or paste an nxm:// link</span>
            <span className="row">
              <input
                value={link}
                placeholder="nxm://cyberpunk2077/mods/…"
                onChange={(e) => setLink(e.target.value)}
                onKeyDown={(e) => e.key === "Enter" && link.trim() && submit()}
              />
              <button className="primary" disabled={!link.trim()} onClick={submit}>
                Download
              </button>
            </span>
          </label>
        </div>
      )}

      {downloads.length === 0 ? (
        <p className="muted empty-note">Nothing downloaded in this session yet.</p>
      ) : (
        <ul className="downloads">
          {downloads.map((d) => {
            const state = d.status.state;
            const progress = d.bytes_total ? Math.min(100, (d.bytes_done / d.bytes_total) * 100) : null;
            return (
              <li key={d.id} className={`download download-${state.toLowerCase()}`}>
                <div className="download-head">
                  <strong>{d.name}</strong>
                  {d.version && <span className="version">{d.version}</span>}
                  <span className={`status status-${state.toLowerCase()}`}>{STATUS_LABEL[state]}</span>
                </div>
                {(state === "Downloading" || state === "Installing" || state === "Queued") && (
                  <div className={`progress${progress === null || state !== "Downloading" ? " indeterminate" : ""}`}>
                    <span style={progress !== null && state === "Downloading" ? { width: `${progress}%` } : undefined} />
                  </div>
                )}
                <div className="download-meta muted">
                  {d.file_name ?? `File ${d.nexus_file_id}`}
                  {d.bytes_total ? ` · ${formatBytes(state === "Downloading" ? d.bytes_done : d.bytes_total)}${state === "Downloading" ? ` of ${formatBytes(d.bytes_total)}` : ""}` : ""}
                </div>
                {d.status.state === "Failed" && <p className="error">{d.status.message}</p>}
                {d.status.state === "AwaitingChoices" && (
                  <div className="row">
                    <span className="muted small">This mod has installer options.</span>
                    <button
                      className="primary"
                      onClick={async () => {
                        const token = (d.status as { token: string }).token;
                        try {
                          enqueueFomod(await api.getFomod(token));
                        } catch (e) {
                          toast("error", "Couldn't open the installer", errorMessage(e));
                        }
                      }}
                    >
                      Choose options
                    </button>
                  </div>
                )}
                {d.status.state === "Done" && d.status.warnings.map((w) => <p key={w} className="warn-text">{w}</p>)}
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}
