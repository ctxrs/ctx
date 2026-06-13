import type { WorkspaceActiveSnapshotEvent } from "@ctx/types";
import { idToString } from "../../api/client";
import type { SessionSubscriptionCursor } from "../sessionSubscription";

const normalizeId = (value: string | null | undefined): string =>
  typeof value === "string" ? value.trim() : "";

export const workspaceEventSessionId = (event: WorkspaceActiveSnapshotEvent): string => {
  switch (event.type) {
    case "session_gap":
      return idToString(event.session_id);
    case "session_head_seed":
      return idToString(event.head.session.id);
    case "session_summary":
      return idToString(event.summary.session.id);
    case "session_summary_delta":
      return idToString(event.delta.session_id);
    case "session_head_delta":
      return idToString(event.delta.session_id);
    default:
      return "";
  }
};

export const resolveForegroundPrioritySessionIds = (
  foregroundSessionId: string | null | undefined,
  subscribedSessions?: readonly SessionSubscriptionCursor[],
): string[] => {
  const explicit = normalizeId(foregroundSessionId);
  const firstSubscribed = normalizeId(subscribedSessions?.[0]?.sessionId);
  if (!explicit) return firstSubscribed ? [firstSubscribed] : [];
  if (!firstSubscribed || firstSubscribed === explicit) return [explicit];
  const explicitStillSubscribed = (subscribedSessions ?? []).some(
    (subscription) => normalizeId(subscription.sessionId) === explicit,
  );
  return explicitStillSubscribed ? [explicit, firstSubscribed] : [firstSubscribed];
};

export const isForegroundPrioritySessionEvent = (
  foregroundSessionId: string | null | undefined,
  subscribedSessions: readonly SessionSubscriptionCursor[] | undefined,
  event: WorkspaceActiveSnapshotEvent,
): boolean => {
  const sessionId = workspaceEventSessionId(event);
  if (!sessionId) return false;
  return resolveForegroundPrioritySessionIds(foregroundSessionId, subscribedSessions).includes(
    sessionId,
  );
};

export const prioritizeForegroundPriorityEvents = (
  foregroundSessionId: string | null | undefined,
  subscribedSessions: readonly SessionSubscriptionCursor[] | undefined,
  events: readonly WorkspaceActiveSnapshotEvent[],
): WorkspaceActiveSnapshotEvent[] => {
  const prioritySessionIds = new Set(
    resolveForegroundPrioritySessionIds(foregroundSessionId, subscribedSessions),
  );
  if (prioritySessionIds.size === 0 || events.length < 2) return events.slice();
  const priorityEvents: WorkspaceActiveSnapshotEvent[] = [];
  const otherEvents: WorkspaceActiveSnapshotEvent[] = [];
  let sawOtherBeforePriority = false;
  let otherSeen = false;
  for (const event of events) {
    if (prioritySessionIds.has(workspaceEventSessionId(event))) {
      priorityEvents.push(event);
      if (otherSeen) sawOtherBeforePriority = true;
    } else {
      otherSeen = true;
      otherEvents.push(event);
    }
  }
  if (!sawOtherBeforePriority || priorityEvents.length === 0) return events.slice();
  return [...priorityEvents, ...otherEvents];
};
