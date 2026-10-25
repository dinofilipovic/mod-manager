import { useEffect, useState } from "react";
import { api, errorMessage, nexusUrls, openExternal, reveal, withTimeout } from "../api";
import type { AppInfo } from "../bindings/AppInfo";
import type { NexusStatus } from "../bindings/NexusStatus";
import { Icon } from "../components/Icon";
import { useToast } from "../components/Toasts";
import { formatBytes } from "../format";

/** Longest we wait for Nexus before telling the user something is wrong. */
const CONNECT_TIMEOUT_MS = 45_000;

type Connect = { phase: "idle" } | { phase: "checking"; startedAt: number } | { phase: "error"; message: string };

export function SettingsView({
  nexus,
  onNexusChange,
  onNexusStatus,
}: {
  nexus: NexusStatus | null;
  onNexusChange: () => Promise<void>;
  onNexusStatus: (status: NexusStatus) => void;
}) {
  const toast = useToast();
  const [key, setKey] = useState("");
  const [connect, setConnect] = useState<Connect>({ phase: "idle" });
  const [justConnected, setJustConnected] = useState(false);
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [, tick] = useState(0);

  useEffect(() => {
    void api.appInfo().then(setInfo);
  }, []);

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
          <dt>Deployment</dt>
          <dd>
            Copy-on-write clones where the disk supports it (APFS, Btrfs, XFS, ReFS), so deployed mods take no extra
            space. Hard links or copies otherwise.
          </dd>
        </dl>
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
