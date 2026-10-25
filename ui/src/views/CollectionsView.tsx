import { useEffect, useState } from "react";
import type { View } from "../App";
import { api, errorMessage, nexusUrls, openExternal } from "../api";
import type { Collection } from "../bindings/Collection";
import type { GameDefinition } from "../bindings/GameDefinition";
import type { NexusStatus } from "../bindings/NexusStatus";
import { Icon } from "../components/Icon";
import { useToast } from "../components/Toasts";
import { formatBytes, plural } from "../format";

/**
 * Review a Nexus collection and install its mods. Premium accounts download
 * everything at once; free accounts get each mod's files page.
 */
export function CollectionsView({
  initialLink,
  games,
  nexus,
  loadInstalled,
  onNavigate,
}: {
  initialLink: string | null;
  games: GameDefinition[];
  nexus: NexusStatus | null;
  /** Nexus file ids already in the library for a game, to mark what's installed. */
  loadInstalled: (gameDomain: string) => Promise<Set<number>>;
  onNavigate: (view: View) => void;
}) {
  const toast = useToast();
  const [link, setLink] = useState(initialLink ?? "");
  const [collection, setCollection] = useState<Collection | null>(null);
  const [loading, setLoading] = useState(false);
  const [chosen, setChosen] = useState<Set<number>>(new Set());
  const [installing, setInstalling] = useState(false);
  const [installedFileIds, setInstalled] = useState<Set<number>>(new Set());

  const load = async (target: string) => {
    if (!target.trim()) return;
    setLoading(true);
    try {
      const c = await api.openCollection(target.trim());
      const installed = await loadInstalled(c.game_domain).catch(() => new Set<number>());
      setInstalled(installed);
      setCollection(c);
      setChosen(new Set(c.mods.filter((m) => !m.optional && !installed.has(m.file_id)).map((m) => m.file_id)));
    } catch (e) {
      toast("error", "Couldn't open the collection", errorMessage(e));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    if (initialLink) {
      setLink(initialLink);
      void load(initialLink);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [initialLink]);

  const premium = !!nexus?.user?.is_premium;
  const selected = collection?.mods.filter((m) => chosen.has(m.file_id)) ?? [];
  const totalBytes = selected.reduce((n, m) => n + (m.size_bytes ?? 0), 0);

  const installAll = async () => {
    if (!collection) return;
    setInstalling(true);
    try {
      const queued = await api.installCollection(
        collection.game_domain,
        selected.map((m) => [m.mod_id, m.file_id]),
      );
      toast("success", `${plural(queued, "download")} started`, "Mods with installer options will ask for your choices.");
      onNavigate({ kind: "downloads" });
    } catch (e) {
      toast("error", "Couldn't start the downloads", errorMessage(e));
    } finally {
      setInstalling(false);
    }
  };

  const domains = games.map((g) => g.stores.nexus_domain).filter((d): d is string => !!d);

  return (
    <div className="page">
      <header className="page-header">
        <h1>Collections</h1>
      </header>

      <div className="how-to">
        <p>
          Collections are curated mod lists on Nexus Mods. Paste a collection's link, or click <strong>Add to Vortex</strong>{" "}
          / <strong>Download</strong> on its page and it opens here.
        </p>
        <div className="row">
          <input
            className="grow"
            value={link}
            placeholder="https://www.nexusmods.com/games/cyberpunk2077/collections/…"
            onChange={(e) => setLink(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && load(link)}
          />
          <button className="primary" onClick={() => load(link)} disabled={loading || !link.trim()}>
            {loading ? "Opening…" : "Open"}
          </button>
        </div>
        <div className="row">
          {domains.map((d) => (
            <button key={d} className="link-button" onClick={() => openExternal(nexusUrls.collections(d))}>
              <Icon name="external" size={12} /> Browse {games.find((g) => g.stores.nexus_domain === d)?.name} collections
            </button>
          ))}
        </div>
      </div>

      {collection && (
        <section className="card">
          <div className="card-head-row">
            <div>
              <h2>{collection.name}</h2>
              <p className="muted small">
                by {collection.author} · revision {collection.revision} · {plural(collection.mods.length, "mod")}
              </p>
            </div>
            {premium ? (
              <button className="primary" onClick={installAll} disabled={installing || selected.length === 0}>
                <Icon name="download" /> Install {plural(selected.length, "mod")} ({formatBytes(totalBytes)})
              </button>
            ) : (
              <span className="muted small">
                {nexus?.user ? "Free account: get each mod from its page." : "Connect Nexus Mods in Settings to download."}
              </span>
            )}
          </div>
          {collection.summary && <p className="muted">{collection.summary}</p>}
          {collection.unavailable_file_ids.length > 0 && (
            <div className="notice notice-warn">
              <Icon name="alert" />
              <div>
                <strong>{plural(collection.unavailable_file_ids.length, "file")} in this collection no longer exist on Nexus</strong>
                <p>They're skipped. The collection may still work without them.</p>
              </div>
            </div>
          )}
          <ul className="collection-mods">
            {collection.mods.map((m) => {
              const installed = installedFileIds.has(m.file_id);
              return (
                <li key={m.file_id} className={installed ? "installed" : ""}>
                  {premium && !installed ? (
                    <input
                      type="checkbox"
                      checked={chosen.has(m.file_id)}
                      aria-label={`Include ${m.name}`}
                      onChange={(e) =>
                        setChosen((s) => {
                          const next = new Set(s);
                          if (e.target.checked) next.add(m.file_id);
                          else next.delete(m.file_id);
                          return next;
                        })
                      }
                    />
                  ) : (
                    <span className="check-spacer" />
                  )}
                  <div className="collection-mod-text">
                    <strong>{m.name}</strong>
                    <span className="muted small">
                      {m.file_title !== m.name ? `${m.file_title} · ` : ""}
                      {m.version ?? ""}
                      {m.size_bytes ? ` · ${formatBytes(m.size_bytes)}` : ""}
                    </span>
                  </div>
                  {m.optional && <span className="badge badge-update">Optional</span>}
                  {installed ? (
                    <span className="badge badge-win">Installed</span>
                  ) : (
                    !premium && (
                      <button onClick={() => openExternal(nexusUrls.filesPage(m.game_domain, m.mod_id, m.file_id))}>
                        <Icon name="external" /> Get
                      </button>
                    )
                  )}
                </li>
              );
            })}
          </ul>
          <p className="muted small">
            Installing mods one by one doesn't reproduce the curator's installer choices or file overrides; check each
            mod's options when its installer asks.
          </p>
        </section>
      )}
    </div>
  );
}
