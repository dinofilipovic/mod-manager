import { useCallback, useEffect, useMemo, useState } from "react";
import { api, errorMessage, events, withTimeout } from "./api";
import type { Download } from "./bindings/Download";
import type { NexusStatus } from "./bindings/NexusStatus";
import type { ScanResult } from "./bindings/ScanResult";
import type { GameDefinition } from "./bindings/GameDefinition";
import { Sidebar } from "./components/Sidebar";
import { useToast } from "./components/Toasts";
import { DownloadsView } from "./views/DownloadsView";
import { GameView } from "./views/GameView";
import { SettingsView } from "./views/SettingsView";
import { Welcome } from "./views/Welcome";

export type View = { kind: "game"; instanceId: string } | { kind: "downloads" } | { kind: "settings" };

export function App() {
  const toast = useToast();
  const [scan, setScan] = useState<ScanResult | null>(null);
  const [games, setGames] = useState<GameDefinition[]>([]);
  const [scanning, setScanning] = useState(true);
  const [view, setView] = useState<View | null>(null);
  const [downloads, setDownloads] = useState<Download[]>([]);
  const [nexus, setNexus] = useState<NexusStatus | null>(null);

  const rescan = useCallback(async () => {
    setScanning(true);
    try {
      setScan(await api.scan());
    } catch (e) {
      toast("error", "Scan failed", errorMessage(e));
    } finally {
      setScanning(false);
    }
  }, [toast]);

  const refreshNexus = useCallback(async () => {
    try {
      setNexus(await withTimeout(api.nexusStatus(), 45_000, "Nexus Mods didn't answer. Check your internet connection."));
    } catch (e) {
      setNexus({ user: null, error: errorMessage(e) });
    }
  }, []);

  useEffect(() => {
    void rescan();
    void refreshNexus();
    void api.listGames().then(setGames);
    void api.listDownloads().then(setDownloads);
    const unlisten = [
      events.downloadsChanged(setDownloads),
      events.nxmError((message) => toast("error", "Couldn't start the download", message)),
    ];
    return () => unlisten.forEach((u) => void u.then((f) => f()));
  }, [rescan, refreshNexus, toast]);

  // Notify when downloads finish or fail.
  const [seen, setSeen] = useState<Record<string, string>>({});
  useEffect(() => {
    const next: Record<string, string> = {};
    for (const d of downloads) {
      next[d.id] = d.status.state;
      if (seen[d.id] && seen[d.id] !== d.status.state) {
        if (d.status.state === "Done")
          toast(
            "success",
            d.status.replaced_existing ? `${d.name} updated` : `${d.name} installed`,
            d.status.warnings[0] ?? "Apply changes to put it in the game.",
          );
        if (d.status.state === "Failed") toast("error", `${d.name} failed`, d.status.message);
      }
    }
    setSeen(next);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [downloads]);

  // Open the first game once found.
  useEffect(() => {
    if (!view && scan?.instances[0]) setView({ kind: "game", instanceId: scan.instances[0].id });
  }, [scan, view]);

  const current = useMemo(() => {
    if (view?.kind !== "game" || !scan) return null;
    const instance = scan.instances.find((i) => i.id === view.instanceId);
    if (!instance) return null;
    const env = scan.environments.find((e) => e.id === instance.environment_id);
    const game = games.find((g) => g.id === instance.game_id);
    return instance && env && game ? { instance, env, game } : null;
  }, [scan, view, games]);

  const activeDownloads = downloads.filter((d) => ["Queued", "Downloading", "Installing"].includes(d.status.state)).length;

  return (
    <div className="shell">
      <Sidebar
        scan={scan}
        scanning={scanning}
        view={view}
        onNavigate={setView}
        onRescan={rescan}
        activeDownloads={activeDownloads}
        nexus={nexus}
      />
      <main className="content">
        {view?.kind === "downloads" ? (
          <DownloadsView downloads={downloads} games={games} nexus={nexus} onDownloads={setDownloads} onNavigate={setView} />
        ) : view?.kind === "settings" ? (
          <SettingsView nexus={nexus} onNexusChange={refreshNexus} onNexusStatus={setNexus} />
        ) : current ? (
          <GameView
            key={current.instance.id}
            instance={current.instance}
            env={current.env}
            game={current.game}
            nexus={nexus}
            onScan={setScan}
            onNavigate={setView}
          />
        ) : (
          <Welcome scanning={scanning} scan={scan} onRescan={rescan} />
        )}
      </main>
    </div>
  );
}
