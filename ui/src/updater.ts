// App self-updates from GitHub Releases (see .github/workflows/release.yml).
import { useCallback, useEffect, useState } from "react";
import { relaunch } from "@tauri-apps/plugin-process";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { errorMessage } from "./api";

export type Updater = {
  available: Update | null;
  checking: boolean;
  /** Download progress 0–1 while installing. */
  progress: number | null;
  error: string | null;
  checkNow: () => Promise<Update | null>;
  install: () => Promise<void>;
};

export function useUpdater(): Updater {
  const [available, setAvailable] = useState<Update | null>(null);
  const [checking, setChecking] = useState(false);
  const [progress, setProgress] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);

  const checkNow = useCallback(async () => {
    setChecking(true);
    setError(null);
    try {
      const update = await check();
      setAvailable(update);
      return update;
    } catch (e) {
      setError(errorMessage(e));
      return null;
    } finally {
      setChecking(false);
    }
  }, []);

  // Quietly check once at startup; errors (offline, no releases yet) only show in Settings.
  useEffect(() => {
    void checkNow();
  }, [checkNow]);

  const install = useCallback(async () => {
    if (!available) return;
    setError(null);
    let total = 0;
    let done = 0;
    try {
      await available.downloadAndInstall((event) => {
        if (event.event === "Started") total = event.data.contentLength ?? 0;
        if (event.event === "Progress") {
          done += event.data.chunkLength;
          setProgress(total ? done / total : null);
        }
      });
      await relaunch();
    } catch (e) {
      setError(errorMessage(e));
      setProgress(null);
    }
  }, [available]);

  return { available, checking, progress, error, checkNow, install };
}
