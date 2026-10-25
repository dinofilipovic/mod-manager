import type { View } from "../App";
import type { NexusStatus } from "../bindings/NexusStatus";
import type { ScanResult } from "../bindings/ScanResult";
import { STORE_LABELS } from "../format";
import { Icon } from "./Icon";

export function Sidebar({
  scan,
  scanning,
  view,
  onNavigate,
  onRescan,
  activeDownloads,
  nexus,
  updateAvailable,
  updateCounts,
}: {
  scan: ScanResult | null;
  scanning: boolean;
  view: View | null;
  onNavigate: (view: View) => void;
  onRescan: () => void;
  activeDownloads: number;
  nexus: NexusStatus | null;
  updateAvailable: string | null;
  /** Mods with an update waiting, per game id. */
  updateCounts: Record<string, number>;
}) {
  const envName = (id: string) => scan?.environments.find((e) => e.id === id)?.name ?? id;
  return (
    <nav className="sidebar" aria-label="Main">
      <div className="brand">
        <img src="/logo.svg" alt="" width={26} height={26} />
        <span>Mod Manager</span>
      </div>

      <div className="nav-section">
        <div className="nav-heading">
          <span>Games</span>
          <button className="icon-button" onClick={onRescan} disabled={scanning} title="Look for games again">
            <Icon name="refresh" size={14} className={scanning ? "spin" : undefined} />
          </button>
        </div>
        {scan?.instances.length === 0 && <p className="nav-empty">No games found</p>}
        {scan?.instances.map((instance) => {
          const needsSetup = instance.dll_overrides.some((d) => !d.satisfied);
          const active = view?.kind === "game" && view.instanceId === instance.id;
          return (
            <button
              key={instance.id}
              className={`nav-item game-item${active ? " active" : ""}`}
              onClick={() => onNavigate({ kind: "game", instanceId: instance.id })}
            >
              <span className="game-tile" aria-hidden="true">
                {instance.game_name.slice(0, 2).toUpperCase()}
              </span>
              <span className="nav-text">
                <span>{instance.game_name}</span>
                <small>
                  {envName(instance.environment_id)} · {STORE_LABELS[instance.store]}
                </small>
              </span>
              {needsSetup && <span className="dot warn" title="Setup needed" />}
              {!needsSetup && (updateCounts[instance.game_id] ?? 0) > 0 && (
                <span className="count" title="Mod updates available">
                  {updateCounts[instance.game_id]}
                </span>
              )}
            </button>
          );
        })}
      </div>

      <div className="nav-section">
        <button
          className={`nav-item${view?.kind === "downloads" ? " active" : ""}`}
          onClick={() => onNavigate({ kind: "downloads" })}
        >
          <Icon name="download" />
          <span className="nav-text">Downloads</span>
          {activeDownloads > 0 && <span className="count">{activeDownloads}</span>}
        </button>
        <button
          className={`nav-item${view?.kind === "collections" ? " active" : ""}`}
          onClick={() => onNavigate({ kind: "collections" })}
        >
          <Icon name="layers" />
          <span className="nav-text">Collections</span>
        </button>
        <button
          className={`nav-item${view?.kind === "settings" ? " active" : ""}`}
          onClick={() => onNavigate({ kind: "settings" })}
        >
          <Icon name="settings" />
          <span className="nav-text">Settings</span>
        </button>
      </div>

      {updateAvailable && (
        <button className="update-pill" onClick={() => onNavigate({ kind: "settings" })}>
          <Icon name="sparkle" size={14} /> Update to {updateAvailable}
        </button>
      )}
      <button className="account" onClick={() => onNavigate({ kind: "settings" })}>
        <span className={`avatar${nexus?.user ? " connected" : ""}`}>
          {nexus?.user ? nexus.user.name.slice(0, 1).toUpperCase() : <Icon name="user" size={14} />}
        </span>
        <span className="nav-text">
          <span>{nexus?.user ? nexus.user.name : "Nexus Mods"}</span>
          <small>
            {!nexus
              ? "Checking account…"
              : nexus.user
                ? nexus.user.is_premium
                  ? "Premium"
                  : "Connected"
                : nexus.error
                  ? "Needs attention"
                  : "Not connected"}
          </small>
        </span>
      </button>
    </nav>
  );
}
