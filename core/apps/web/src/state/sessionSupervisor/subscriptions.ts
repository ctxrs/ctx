import { buildSessionSubscriptionPlan } from "@ctx/session-supervisor-core";
import type { SessionHeadSnapshot } from "../../api/client";
import type { SessionSubscriptionCursor } from "../sessionSubscription";
import type { InternalEntry } from "./entryState";
import { reconcileActivityFromTurns, reconcileLatestTurnInterruptedFromActivity } from "./cachePolicy";
import { isReplicaAuthority } from "./config";

export function buildSubscribedSessions(
  subscribedSessionIds: string[],
  entries: Map<string, InternalEntry>,
  workspaceSessionHeadsById: Map<string, SessionHeadSnapshot>,
  activeTaskSessionIds: string[] = [],
  workspaceActivePrimarySessionIds: string[] = [],
  warmSessionIds: string[] = [],
): SessionSubscriptionCursor[] {
  const activeTaskSessionSet = new Set(
    mergeOrderedSessionIds(activeTaskSessionIds, workspaceActivePrimarySessionIds),
  );
  const warmSessionSet = new Set(warmSessionIds);
  return subscribedSessionIds.map((sessionId) => {
    const entry = entries.get(sessionId);
    const head = workspaceSessionHeadsById.get(sessionId);
    const headSeq = head?.last_event_seq;
    const headProjectionRev = head?.projection_rev;
    const intent =
      (entry?.refCount ?? 0) > 0 || (!activeTaskSessionSet.has(sessionId) && !warmSessionSet.has(sessionId))
        ? "replay"
        : "head";
    if (
      (entry?.freshness === "recovering" || entry?.loadState === "recovering") &&
      entry?.recoverySubscriptionPolicy !== "preserve"
    ) {
      return {
        sessionId,
        intent: "replay",
        replay: { kind: "reset" },
      };
    }
    if (
      (entry?.freshness === "recovering" || entry?.loadState === "recovering") &&
      entry?.recoverySubscriptionPolicy === "preserve"
    ) {
      return {
        sessionId,
        intent,
        replay: { kind: "auto" },
      };
    }
    const afterSeq =
      typeof entry?.lastEventSeq === "number"
        ? entry.lastEventSeq
        : typeof headSeq === "number"
          ? headSeq
          : null;
    const afterProjectionRev =
      typeof entry?.projectionRev === "number"
        ? entry.projectionRev
        : typeof headProjectionRev === "number"
          ? headProjectionRev
          : null;
    return {
      sessionId,
      intent,
      replay:
        typeof afterSeq === "number"
          ? {
              kind: "resume",
              afterSeq,
              ...(typeof afterProjectionRev === "number" ? { afterProjectionRev } : {}),
            }
          : { kind: "auto" },
    };
  });
}

const compareSessionVersionFreshness = (
  incoming: { lastEventSeq: number | null; projectionRev: number | null; stateRev: number | null },
  existing: { lastEventSeq: number | null; projectionRev: number | null; stateRev: number | null },
): number => {
  const fields: Array<keyof typeof incoming> = ["lastEventSeq", "projectionRev", "stateRev"];
  for (const field of fields) {
    const incomingValue = incoming[field];
    const existingValue = existing[field];
    if (incomingValue === existingValue) continue;
    if (incomingValue === null) return -1;
    if (existingValue === null) return 1;
    return incomingValue - existingValue;
  }
  return 0;
};

export function applySessionActivityUpdate(
  entries: Map<string, InternalEntry>,
  sessionId: string,
  activity: InternalEntry["activity"],
  version?: { lastEventSeq?: number | null; projectionRev?: number | null; stateRev?: number | null },
): { changed: boolean; subscriptionCursorChanged: boolean } {
  const entry = entries.get(sessionId);
  if (!entry) return { changed: false, subscriptionCursorChanged: false };
  if (
    entry.refCount > 0 ||
    entry.subscribed ||
    entry.loadState === "recovering" ||
    isReplicaAuthority(entry.freshness)
  ) {
    return { changed: false, subscriptionCursorChanged: false };
  }
  const incomingProjectionRev =
    typeof version?.projectionRev === "number" ? version.projectionRev : null;
  const incomingStateRev = typeof version?.stateRev === "number" ? version.stateRev : null;
  const incomingLastEventSeq =
    typeof version?.lastEventSeq === "number" ? version.lastEventSeq : null;
  const incomingVersion = {
    lastEventSeq: incomingLastEventSeq,
    projectionRev: incomingProjectionRev,
    stateRev: incomingStateRev,
  };
  const existingVersion = {
    lastEventSeq: typeof entry.lastEventSeq === "number" ? entry.lastEventSeq : null,
    projectionRev: typeof entry.projectionRev === "number" ? entry.projectionRev : null,
    stateRev: typeof entry.stateRev === "number" ? entry.stateRev : null,
  };
  if (compareSessionVersionFreshness(incomingVersion, existingVersion) < 0) {
    return { changed: false, subscriptionCursorChanged: false };
  }

  let changed = false;
  let subscriptionCursorChanged = false;
  if (activity !== undefined) {
    if (
      (entry.activity?.is_working ?? false) !== (activity?.is_working ?? false) ||
      (entry.activity?.last_turn_status ?? null) !== (activity?.last_turn_status ?? null)
    ) {
      entry.activity = activity ?? null;
      changed = true;
    }
    if (reconcileLatestTurnInterruptedFromActivity(entry.turns, entry.activity)) {
      entry.turnsRev += 1;
      changed = true;
    }
    const reconciledActivity = reconcileActivityFromTurns(entry.activity, entry.turns);
    if (reconciledActivity !== entry.activity) {
      entry.activity = reconciledActivity;
      changed = true;
    }
  }
  if (incomingLastEventSeq !== null && entry.lastEventSeq !== incomingLastEventSeq) {
    entry.lastEventSeq = incomingLastEventSeq;
    changed = true;
    subscriptionCursorChanged = true;
  }
  if (incomingProjectionRev !== null && entry.projectionRev !== incomingProjectionRev) {
    entry.projectionRev = incomingProjectionRev;
    changed = true;
    subscriptionCursorChanged = true;
  }
  if (incomingStateRev !== null && entry.stateRev !== incomingStateRev) {
    entry.stateRev = incomingStateRev;
    changed = true;
  }
  if (changed && entry.freshness === "bootstrap") {
    entry.freshness = "replica";
  }
  if (!changed) {
    return { changed: false, subscriptionCursorChanged: false };
  }
  entry.updatedAtMs = Date.now();
  return {
    changed: true,
    subscriptionCursorChanged: subscriptionCursorChanged && entry.subscribed,
  };
}

export type SessionSupervisorSubscriptionHost = {
  entries: Map<string, InternalEntry>;
  activeTaskSessionIds: string[];
  workspaceActivePrimarySessionIds: string[];
  warmSessionIds: string[];
  subscribedSessionIds: string[];
  setSubscribedSessionIds(next: string[]): void;
  emitSubscribedSessions(): void;
  ensureEntry(sessionId: string): InternalEntry;
  publish(): void;
};

const mergeOrderedSessionIds = (...groups: readonly (readonly string[])[]): string[] => {
  const next: string[] = [];
  const seen = new Set<string>();
  for (const group of groups) {
    for (const rawSessionId of group) {
      const sessionId = String(rawSessionId ?? "").trim();
      if (!sessionId || seen.has(sessionId)) continue;
      seen.add(sessionId);
      next.push(sessionId);
    }
  }
  return next;
};

export function refreshSubscriptions(
  host: SessionSupervisorSubscriptionHost,
  opts?: { emitIfUnchanged?: boolean },
) {
  const openSessionIds = Array.from(host.entries.values())
    .filter((entry) => entry.refCount > 0)
    .map((entry) => entry.sessionId);
  const activeHeadSessionIds = mergeOrderedSessionIds(
    host.activeTaskSessionIds,
    host.workspaceActivePrimarySessionIds,
  );
  const plan = buildSessionSubscriptionPlan({
    openSessionIds,
    activeTaskSessionIds: activeHeadSessionIds,
    warmSessionIds: host.warmSessionIds,
    previousSubscribedSessionIds: host.subscribedSessionIds,
  });
  if (!plan.changed) {
    if (opts?.emitIfUnchanged) {
      host.emitSubscribedSessions();
    }
    return;
  }
  const nextSet = new Set(plan.nextSubscribedSessionIds);
  host.setSubscribedSessionIds(plan.nextSubscribedSessionIds);
  for (const entry of host.entries.values()) {
    entry.subscribed = nextSet.has(entry.sessionId);
  }
  for (const sessionId of plan.addedSessionIds) {
    const entry = host.ensureEntry(sessionId);
    entry.subscribed = true;
  }
  host.emitSubscribedSessions();
  host.publish();
}

export type SessionSupervisorRecoveryHost = {
  entries: Map<string, InternalEntry>;
  emitSubscribedSessions(): void;
  publish(): void;
};

export function markOpenSessionsRecovering(host: SessionSupervisorRecoveryHost) {
  let changed = false;
  for (const entry of host.entries.values()) {
    if (entry.refCount <= 0) continue;
    if (entry.loadState === "fatal") continue;
    if (entry.recoverySubscriptionPolicy !== "reset") {
      entry.recoverySubscriptionPolicy = "reset";
      changed = true;
    }
    if (entry.loadState !== "recovering") {
      entry.loadState = "recovering";
      changed = true;
    }
    if (entry.freshness !== "recovering") {
      entry.freshness = "recovering";
      changed = true;
    }
    entry.updatedAtMs = Date.now();
  }
  if (changed) {
    host.publish();
    host.emitSubscribedSessions();
  }
}

export function emitSubscribedSessions(
  sink: ((sessions: SessionSubscriptionCursor[]) => void) | null,
  sessions: SessionSubscriptionCursor[],
) {
  sink?.(sessions);
}
