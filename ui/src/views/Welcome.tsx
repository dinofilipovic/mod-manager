import type { ScanResult } from "../bindings/ScanResult";
import { Icon } from "../components/Icon";

export function Welcome({ scanning, scan, onRescan }: { scanning: boolean; scan: ScanResult | null; onRescan: () => void }) {
  if (scanning && !scan) {
    return (
      <div className="welcome">
        <Icon name="refresh" size={28} className="spin" />
        <p className="muted">Looking for CrossOver bottles and games…</p>
      </div>
    );
  }
  return (
    <div className="welcome">
      <img src="/logo.svg" alt="" width={64} height={64} />
      <h1>No supported games found</h1>
      <p>
        Install Cyberpunk 2077 with Steam, GOG or Epic inside a CrossOver bottle (or a Wine prefix on Linux), then look
        again.
      </p>
      {scan && scan.environments.length > 0 && (
        <p className="muted">
          Checked {scan.environments.length} environment{scan.environments.length === 1 ? "" : "s"}:{" "}
          {scan.environments.map((e) => e.name).join(", ")}
        </p>
      )}
      <button className="primary" onClick={onRescan} disabled={scanning}>
        <Icon name="refresh" className={scanning ? "spin" : undefined} /> Look again
      </button>
    </div>
  );
}
