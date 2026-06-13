import type { SessionViewVerbosity } from "../state/uiStateStore";

export function defaultSessionVerbosityForProvider(
  providerId: string | null | undefined,
): SessionViewVerbosity {
  const normalized = String(providerId ?? "")
    .trim()
    .toLowerCase();
  if (normalized === "claude" || normalized === "claude-crp") {
    return "verbose";
  }
  return "default";
}
