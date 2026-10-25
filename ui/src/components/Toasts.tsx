import { createContext, useCallback, useContext, useState, type ReactNode } from "react";
import { Icon } from "./Icon";

type Tone = "info" | "success" | "error";
type Toast = { id: number; tone: Tone; title: string; detail?: string };

const ToastContext = createContext<(tone: Tone, title: string, detail?: string) => void>(() => {});

export const useToast = () => useContext(ToastContext);

let nextId = 1;

export function ToastProvider({ children }: { children: ReactNode }) {
  const [toasts, setToasts] = useState<Toast[]>([]);

  const dismiss = useCallback((id: number) => setToasts((t) => t.filter((x) => x.id !== id)), []);

  const push = useCallback(
    (tone: Tone, title: string, detail?: string) => {
      const id = nextId++;
      setToasts((t) => [...t.slice(-3), { id, tone, title, detail }]);
      setTimeout(() => dismiss(id), tone === "error" ? 9000 : 4500);
    },
    [dismiss],
  );

  return (
    <ToastContext.Provider value={push}>
      {children}
      <div className="toasts" role="status" aria-live="polite">
        {toasts.map((t) => (
          <div key={t.id} className={`toast toast-${t.tone}`}>
            <Icon name={t.tone === "error" ? "alert" : t.tone === "success" ? "check" : "sparkle"} />
            <div>
              <strong>{t.title}</strong>
              {t.detail && <p>{t.detail}</p>}
            </div>
            <button className="icon-button" onClick={() => dismiss(t.id)} aria-label="Dismiss">
              <Icon name="x" size={14} />
            </button>
          </div>
        ))}
      </div>
    </ToastContext.Provider>
  );
}
