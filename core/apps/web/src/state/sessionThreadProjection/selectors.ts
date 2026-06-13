import type { SessionCacheEntry } from "../sessionSupervisor/entryState";
import { applySessionThreadProjectionOverlay, selectSessionQueuePanelMessages } from "./overlay";
import { EMPTY_SESSION_THREAD_PROJECTION, type SessionThreadProjection } from "./types";

export { EMPTY_SESSION_THREAD_PROJECTION } from "./types";
export type { SessionThreadProjection } from "./types";
export { selectSessionQueuePanelMessages } from "./overlay";

export function selectSessionThreadProjection(
  entry: SessionCacheEntry | null | undefined,
): SessionThreadProjection {
  if (!entry) return EMPTY_SESSION_THREAD_PROJECTION;
  const baseProjection = entry.threadProjection;
  if (!baseProjection) return EMPTY_SESSION_THREAD_PROJECTION;
  return applySessionThreadProjectionOverlay(baseProjection, entry);
}
