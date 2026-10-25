import type { Store } from "./bindings/Store";

export const STORE_LABELS: Record<Store, string> = {
  Steam: "Steam",
  Gog: "GOG",
  Epic: "Epic",
  KnownPath: "Manual",
};

/** Short labels for the game's layout kinds (from the game definition). */
export const KIND_LABELS: Record<string, string> = {
  archive: "Archive",
  redscript: "redscript",
  tweak: "TweakXL",
  cet: "CET",
  red4ext: "RED4ext",
  redmod: "REDmod",
};

export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit++;
  }
  return `${value.toFixed(value < 10 ? 1 : 0)} ${units[unit]}`;
}

export function timeAgo(unixSeconds: number): string {
  const seconds = Math.max(0, Date.now() / 1000 - unixSeconds);
  if (seconds < 60) return "just now";
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes} min ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours} h ago`;
  const days = Math.floor(hours / 24);
  if (days < 30) return `${days} d ago`;
  return new Date(unixSeconds * 1000).toLocaleDateString();
}

export function plural(n: number, one: string, many = `${one}s`): string {
  return `${n} ${n === 1 ? one : many}`;
}
