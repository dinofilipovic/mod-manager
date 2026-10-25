import { createContext, useCallback, useContext, useEffect, useState, type ReactNode } from "react";
import { events } from "../api";
import type { FomodRequest } from "../bindings/FomodRequest";
import { FomodWizard } from "./FomodWizard";
import { useToast } from "./Toasts";

const QueueContext = createContext<(request: FomodRequest) => void>(() => {});

/** Opens installer wizards one at a time, from imports and from downloads. */
export const useFomodQueue = () => useContext(QueueContext);

export function FomodQueueProvider({ children }: { children: ReactNode }) {
  const toast = useToast();
  const [queue, setQueue] = useState<FomodRequest[]>([]);

  const enqueue = useCallback(
    (request: FomodRequest) =>
      setQueue((q) => (q.some((r) => r.token === request.token) ? q : [...q, request])),
    [],
  );

  useEffect(() => {
    const unlisten = events.fomodRequested(enqueue);
    return () => void unlisten.then((f) => f());
  }, [enqueue]);

  const next = () => setQueue((q) => q.slice(1));
  const current = queue[0];

  return (
    <QueueContext.Provider value={enqueue}>
      {children}
      {current && (
        <FomodWizard
          key={current.token}
          request={current}
          onDone={(outcome) => {
            toast(
              "success",
              outcome.replaced_existing ? `${outcome.mod.name} updated` : `${outcome.mod.name} installed`,
              outcome.warnings[0] ?? "Apply changes to put it in the game.",
            );
            next();
          }}
          onCancel={() => {
            toast("info", `${current.mod_name} wasn't installed`);
            next();
          }}
        />
      )}
    </QueueContext.Provider>
  );
}
