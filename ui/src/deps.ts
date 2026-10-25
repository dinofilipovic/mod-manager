// Small helpers for showing mod requirements.
import type { DependencySource } from "./bindings/DependencySource";
import type { DependencyStatus } from "./bindings/DependencyStatus";

/** Needed and not there; optional ones and ones the app can't check don't count. */
export const isUnmet = (d: DependencyStatus) =>
  !d.optional && d.state.state !== "Satisfied" && d.state.state !== "Unverifiable";

export const SOURCE_LABELS: Record<DependencySource, string> = {
  Toml: "the mod's files",
  Nexus: "Nexus page",
  User: "added by you",
};

export function stateLabel(d: DependencyStatus): string {
  switch (d.state.state) {
    case "Satisfied":
      return "OK";
    case "Missing":
      return d.optional ? "Optional" : "Missing";
    case "Disabled":
      return "Disabled";
    case "TooOld":
      return "Too old";
    case "Unverifiable":
      return "Check yourself";
  }
}

/** A short line for a badge: what the first unmet requirement needs. */
export function unmetSummary(unmet: DependencyStatus[]): string {
  const first = unmet[0];
  const more = unmet.length > 1 ? ` +${unmet.length - 1}` : "";
  if (first.state.state === "TooOld") return `${first.name} too old${more}`;
  if (first.state.state === "Disabled") return `Enable ${first.name}${more}`;
  return `Needs ${first.name}${more}`;
}
