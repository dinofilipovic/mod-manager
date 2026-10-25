import { useEffect, useState } from "react";
import { api, errorMessage, nexusUrls, openExternal, reveal, withTimeout } from "../api";
import type { AppInfo } from "../bindings/AppInfo";
import type { ArchiveRetention } from "../bindings/ArchiveRetention";
import type { ArchivesInfo } from "../bindings/ArchivesInfo";
import type { NexusStatus } from "../bindings/NexusStatus";
import { Icon } from "../components/Icon";
import { useToast } from "../components/Toasts";
import { formatBytes } from "../format";
import type { Updater } from "../updater";

/** Longest we wait for Nexus before telling the user something is wrong. */
const CONNECT_TIMEOUT_MS = 45_000;

type Connect = { phase: "idle" } | { phase: "checking"; startedAt: number } | { phase: "error"; message: string };

export function SettingsView({
  nexus,
  onNexusChange,
  onNexusStatus,
  updater,
}: {
  nexus: NexusStatus | null;
  onNexusChange: () => Promise<void>;
  onNexusStatus: (status: NexusStatus) => void;
  updater: Updater;
}) {
  const toast = useToast();
  const [key, setKey] = useState("");
  const [connect, setConnect] = useState<Connect>({ phase: "idle" });
  const [justConnected, setJustConnected] = useState(false);
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [archives, setArchives] = useState<ArchivesInfo | null>(null);
  const [archiveBusy, setArchiveBusy] = useState<string | null>(null);
  const [archiveError, setArchiveError] = useState<string | null>(null);
  const [, tick] = useState(0);

  useEffect(() => {
    void api.appInfo().then(setInfo);
    api.archivesInfo().then(setArchives, (e) => setArchiveError(errorMessage(e)));
  }, []);

  const archiveAction = async (label: string, work: () => Promise<void>) => {
    setArchiveBusy(label);
    setArchiveError(null);
    try {
      await work();
    } catch (e) {
      setArchiveError(errorMessage(e));
    } finally {
      setArchiveBusy(null);
    }
  };

  const setRetention = (retention: ArchiveRetention) =>
    archiveAction("retention", async () => {
      setArchives(await api.setArchiveRetention(retention));
      setInfo(await api.appInfo());
    });

  const cleanArchives = () =>
    archiveAction("clean", async () => {
      const result = await api.cleanUnusedArchives();
      setArchives(await api.archivesInfo());
      setInfo(await api.appInfo());
      toast(
        "success",
        result.deleted ? `Deleted ${result.deleted} unused archives` : "No unused archives",
        result.deleted ? `${formatBytes(result.bytes)} freed.` : undefined,
      );
    });

  // Re-render every second while checking, to show how long it has taken.
  useEffect(() => {
    if (connect.phase !== "checking") return;
    const timer = setInterval(() => tick((n) => n + 1), 1000);
    return () => clearInterval(timer);
  }, [connect.phase]);

  const submit = async () => {
    setConnect({ phase: "checking", startedAt: Date.now() });
    try {
      const user = await withTimeout(
        api.connectNexus(key),
        CONNECT_TIMEOUT_MS,
        "Nexus Mods didn't answer within 45 seconds. Check your internet connection and try again.",
      );
      onNexusStatus({ user, error: null });
      setKey("");
      setConnect({ phase: "idle" });
      setJustConnected(true);
      toast("success", `Connected as ${user.name}`);
    } catch (e) {
      setConnect({ phase: "error", message: errorMessage(e) });
      // A slow check may still have finished in the background.
      void onNexusChange();
    }
  };

  const checking = connect.phase === "checking";
  const seconds = checking ? Math.floor((Date.now() - connect.startedAt) / 1000) : 0;

  return (
    <div className="page">
      <header className="page-header">
        <h1>Settings</h1>
      </header>

      <section className="card">
        <h2>Nexus Mods account</h2>
        {!nexus ? (
          <p className="status-line">
            <Icon name="refresh" className="spin" /> Checking your saved Nexus Mods account…
          </p>
        ) : nexus.user ? (
          <>
            {justConnected && (
              <div className="notice notice-success" role="status">
                <Icon name="check" />
                <div>
                  <strong>You're connected.</strong>
                  <p>
                    “Mod Manager Download” buttons on Nexus Mods now install here.
                    {nexus.user.is_premium
                      ? " As a Premium member, updates download in one click."
                      : " Updates link to the mod's files page."}
                  </p>
                </div>
              </div>
            )}
            <div className="account-row">
              <span className="avatar connected large">{nexus.user.name.slice(0, 1).toUpperCase()}</span>
              <div>
                <strong>{nexus.user.name}</strong>
                <p className="muted">{nexus.user.is_premium ? "Premium member" : "Free member"}</p>
              </div>
              <button
                onClick={async () => {
                  await api.disconnectNexus();
                  setJustConnected(false);
                  await onNexusChange();
                }}
              >
                Disconnect
              </button>
            </div>
          </>
        ) : (
          <>
            <p>
              Paste your personal API key from your Nexus Mods account. It's checked with Nexus and kept in the system
              keychain, never in a plain file.
            </p>
            {nexus.error && connect.phase === "idle" && (
              <div className="notice notice-error" role="alert">
                <Icon name="alert" />
                <div>
                  <strong>Your saved key couldn't be used</strong>
                  <p>{nexus.error}</p>
                </div>
              </div>
            )}
            <div className="row">
              <input
                type="password"
                placeholder="API key"
                value={key}
                disabled={checking}
                onChange={(e) => {
                  setKey(e.target.value);
                  if (connect.phase === "error") setConnect({ phase: "idle" });
                }}
                onKeyDown={(e) => e.key === "Enter" && key.trim() && !checking && submit()}
                aria-label="Nexus Mods API key"
                aria-describedby="nexus-connect-status"
              />
              <button className="primary" disabled={!key.trim() || checking} onClick={submit}>
                {checking ? "Checking…" : "Connect"}
              </button>
            </div>

            <div id="nexus-connect-status" aria-live="polite">
              {checking && (
                <div className="notice notice-info">
                  <Icon name="refresh" className="spin" />
                  <div>
                    <strong>Checking your key with Nexus Mods…</strong>
                    <p>
                      {seconds < 8
                        ? "This usually takes a second or two."
                        : `Still waiting for Nexus Mods (${seconds}s). It gives up after 45 seconds.`}
                    </p>
                  </div>
                </div>
              )}
              {connect.phase === "error" && (
                <div className="notice notice-error" role="alert">
                  <Icon name="alert" />
                  <div>
                    <strong>Couldn't connect</strong>
                    <p>{connect.message}</p>
                  </div>
                  <button onClick={submit} disabled={!key.trim()}>
                    Try again
                  </button>
                </div>
              )}
            </div>

            <ol className="steps muted">
              <li>
                Open{" "}
                <button className="link-button inline" onClick={() => openExternal(nexusUrls.apiKey)}>
                  your Nexus Mods API page <Icon name="external" size={11} />
                </button>{" "}
                and sign in.
              </li>
              <li>Scroll to the bottom, to “Personal API Key”, and copy the key.</li>
              <li>Paste it above and press Connect.</li>
            </ol>
          </>
        )}
      </section>

      <section className="card">
        <h2>Storage</h2>
        <dl className="facts">
          <dt>Library</dt>
          <dd className="break">
            {info?.data_dir}
            {info && (
              <button className="link-button" onClick={() => reveal(info.data_dir)}>
                Show
              </button>
            )}
          </dd>
          <dt>Mods size</dt>
          <dd>{info ? formatBytes(info.library_bytes) : "…"}</dd>
          <dt>Archives size</dt>
          <dd>{info ? formatBytes(info.archives_bytes) : "…"}</dd>
          <dt>Deployment</dt>
          <dd>
            Copy-on-write clones where the disk supports it (APFS, Btrfs, XFS, ReFS), so deployed mods take no extra
            space. Hard links or copies otherwise.
          </dd>
        </dl>
      </section>

      <section className="card" aria-busy={archiveBusy !== null}>
        <div className="card-head-row">
          <h2>Mod archives</h2>
          <button
            onClick={cleanArchives}
            disabled={archiveBusy !== null || !archives || archives.unused_count === 0}
            title="Delete archives that no installed mod was installed from"
          >
            <Icon name="trash" />{" "}
            {archiveBusy === "clean"
              ? "Cleaning…"
              : archives?.unused_count
                ? `Clean ${archives.unused_count} unused (${formatBytes(archives.unused_bytes)})`
                : "Clean unused"}
          </button>
        </div>
        <p className="muted small">
          The archives mods were installed from, kept so a mod can be reinstalled, rolled back or installed with other
          options without downloading it again.
        </p>
        <label className="field-row">
          <span>Keep</span>
          <select
            value={archives?.retention ?? "LastTwo"}
            disabled={!archives || archiveBusy !== null}
            onChange={(e) => void setRetention(e.target.value as ArchiveRetention)}
          >
            <option value="KeepAll">All archives</option>
            <option value="LastTwo">The last 2 per mod (enough to roll back)</option>
            <option value="DeleteAfterInstall">None: delete after installing</option>
          </select>
        </label>
        {archiveError && (
          <div className="notice notice-error" role="alert">
            <Icon name="alert" />
            <div>
              <strong>Couldn't change the archives</strong>
              <p>{archiveError} Check that the library folder is writable, then try again.</p>
            </div>
          </div>
        )}
        {archives && archives.mods.length > 0 && (
          <table className="size-table">
            <thead>
              <tr>
                <th>Mod</th>
                <th>Archives</th>
                <th>Size</th>
              </tr>
            </thead>
            <tbody>
              {archives.mods.slice(0, 12).map((m) => (
                <tr key={m.mod_id}>
                  <td>{m.mod_name}</td>
                  <td>{m.archives}</td>
                  <td>{formatBytes(m.bytes)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
        {archives && archives.mods.length === 0 && <p className="muted small">No archives stored yet.</p>}
      </section>

      <section className="card">
        <div className="card-head-row">
          <h2>App updates</h2>
          <button
            onClick={async () => {
              const found = await updater.checkNow();
              if (!found && !updater.error) toast("success", "You have the latest version");
            }}
            disabled={updater.checking || updater.progress !== null}
          >
            <Icon name="refresh" className={updater.checking ? "spin" : undefined} /> Check now
          </button>
        </div>
        {updater.available ? (
          <div className="notice notice-info">
            <Icon name="sparkle" />
            <div>
              <strong>Version {updater.available.version} is available</strong>
              <p>{updater.available.body?.trim() || "Installs in the background, then the app restarts."}</p>
              {updater.progress !== null && (
                <div className="progress">
                  <span style={{ width: `${Math.round(updater.progress * 100)}%` }} />
                </div>
              )}
            </div>
            <button className="primary" onClick={updater.install} disabled={updater.progress !== null}>
              {updater.progress !== null ? "Installing…" : "Install and restart"}
            </button>
          </div>
        ) : (
          <p className="muted small">
            {updater.error
              ? `Couldn't check for updates: ${updater.error}`
              : "The app checks for a new version each time it starts."}
          </p>
        )}
      </section>

      <section className="card">
        <h2>About</h2>
        <dl className="facts">
          <dt>Version</dt>
          <dd>{info?.version}</dd>
          <dt>CrossOver</dt>
          <dd>{info ? (info.crossover_installed ? "Found" : "Not found in Applications") : "…"}</dd>
        </dl>
      </section>
    </div>
  );
}
