import { useRef, useState, type PointerEvent } from "react";
import type { LoadoutMod } from "../bindings/LoadoutMod";
import { Icon } from "../components/Icon";
import { KIND_LABELS, formatBytes } from "../format";

type Drag = { id: string; from: number; over: number };

/**
 * The load-ordered mod list. Reordering uses pointer events rather than HTML
 * drag-and-drop, which the webview reserves for dropping files on the window.
 */
export function ModList({
  mods,
  reorderable,
  selectedId,
  onSelect,
  onToggle,
  onReorder,
}: {
  mods: LoadoutMod[];
  reorderable: boolean;
  selectedId: string | null;
  onSelect: (id: string) => void;
  onToggle: (id: string, enabled: boolean) => void;
  onReorder: (order: string[]) => void;
}) {
  const rows = useRef<(HTMLLIElement | null)[]>([]);
  const [drag, setDrag] = useState<Drag | null>(null);

  const indexAt = (y: number) => {
    let index = 0;
    rows.current.forEach((row, i) => {
      if (!row) return;
      const rect = row.getBoundingClientRect();
      if (y > rect.top + rect.height / 2) index = i + 1;
    });
    return index;
  };

  const start = (e: PointerEvent, id: string, from: number) => {
    e.preventDefault();
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    setDrag({ id, from, over: from });
  };
  const move = (e: PointerEvent) => {
    if (drag) setDrag({ ...drag, over: indexAt(e.clientY) });
  };
  const end = () => {
    if (!drag) return;
    const to = drag.over > drag.from ? drag.over - 1 : drag.over;
    if (to !== drag.from) {
      const order = mods.map((m) => m.mod.id);
      order.splice(to, 0, ...order.splice(drag.from, 1));
      onReorder(order);
    }
    setDrag(null);
  };

  return (
    <ol className={`mod-list${drag ? " dragging" : ""}`} onPointerMove={move} onPointerUp={end} onPointerCancel={() => setDrag(null)}>
      {mods.map((entry, i) => {
        const m = entry.mod;
        const dropBefore = drag && drag.over === i && i !== drag.from && i !== drag.from + 1;
        const dropAfter = drag && i === mods.length - 1 && drag.over === mods.length && drag.from !== i;
        return (
          <li
            key={m.id}
            ref={(el) => {
              rows.current[i] = el;
            }}
            className={[
              "mod-row",
              entry.enabled ? "" : "disabled",
              selectedId === m.id ? "selected" : "",
              drag?.id === m.id ? "lifted" : "",
              dropBefore ? "drop-before" : "",
              dropAfter ? "drop-after" : "",
            ].join(" ")}
          >
            <span
              className={`grip${reorderable ? "" : " off"}`}
              title={reorderable ? "Drag to change load order" : "Clear the search and filter to reorder"}
              onPointerDown={reorderable ? (e) => start(e, m.id, i) : undefined}
            >
              <Icon name="grip" />
            </span>
            <span className="order">{i + 1}</span>
            <label className="switch" title={entry.enabled ? "Disable" : "Enable"}>
              <input type="checkbox" checked={entry.enabled} onChange={(e) => onToggle(m.id, e.target.checked)} />
              <span />
            </label>
            <button className="mod-main" onClick={() => onSelect(m.id)}>
              {m.picture_url ? (
                <img className="thumb" src={m.picture_url} alt="" loading="lazy" />
              ) : (
                <span className="thumb thumb-fallback">{(KIND_LABELS[m.kinds[0]] ?? "Mod").slice(0, 3)}</span>
              )}
              <span className="mod-text">
                <span className="mod-name">
                  {m.name}
                  {m.version && <span className="version">{m.version}</span>}
                </span>
                <span className="mod-meta">
                  {m.kinds.map((k) => (
                    <span key={k} className={`kind kind-${k}`}>
                      {KIND_LABELS[k] ?? k}
                    </span>
                  ))}
                  {m.author && <span className="muted">by {m.author}</span>}
                </span>
              </span>
              <span className="mod-badges">
                {entry.enabled && entry.missing_requirements.length > 0 && (
                  <span
                    className="badge badge-error"
                    title={`Needs ${entry.missing_requirements.map((r) => r.name).join(", ")}`}
                  >
                    <Icon name="alert" size={11} /> Needs {entry.missing_requirements[0].name}
                    {entry.missing_requirements.length > 1 ? ` +${entry.missing_requirements.length - 1}` : ""}
                  </span>
                )}
                {m.update_file_id && <span className="badge badge-update">Update {m.update_version ?? ""}</span>}
                {entry.enabled && entry.overridden_by.length > 0 && (
                  <span className="badge badge-lose" title={`Overridden by ${entry.overridden_by.join(", ")}`}>
                    <Icon name="bolt" size={11} /> loses {entry.conflict_files}
                  </span>
                )}
                {entry.enabled && entry.overridden_by.length === 0 && entry.overrides.length > 0 && (
                  <span className="badge badge-win" title={`Overrides ${entry.overrides.join(", ")}`}>
                    <Icon name="bolt" size={11} /> wins {entry.conflict_files}
                  </span>
                )}
                <span className="size muted">{formatBytes(m.size_bytes)}</span>
              </span>
            </button>
          </li>
        );
      })}
    </ol>
  );
}
