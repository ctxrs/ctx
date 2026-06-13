import {
  type SessionEvent,
} from "../../api/client";
import { hasModelList } from "./eventHydration";
import { resolveTurnAnalyticsMetadata } from "./turnAnalyticsMetadata";
import { trackSessionEventVolumeBurst, trackUnknownEventBurst } from "../../utils/analytics";
import {
  hasSessionReplicaRecoveryData,
  resolveReplicaReadyLoadState,
  shouldReplayReplicaReplace,
} from "./authorityPolicy";
import type { InternalEntry } from "./entryState";
import type { SessionReplicaPatch } from "../sessionReplicaProtocol";
import {
  applyCanonicalTranscriptPatch,
  rebuildSeqAndStartState,
  type SessionSupervisorReplicaPatchHost,
} from "./replicaPatchApplyTranscript";

export type { SessionSupervisorReplicaPatchHost } from "./replicaPatchApplyTranscript";

const SESSION_EVENT_BURST_WINDOW_MS = 5_000;
const SESSION_EVENT_BURST_THRESHOLD = 250;
const UNKNOWN_EVENT_BURST_THRESHOLD = 10;

type BurstWindow = {
  startedAtMs: number;
  count: number;
  emitted: boolean;
};

const sessionEventBurstBySession = new Map<string, BurstWindow>();
const unknownEventBurstByKey = new Map<string, BurstWindow>();

const advanceBurstWindow = (
  map: Map<string, BurstWindow>,
  key: string,
  increment: number,
  nowMs: number,
): BurstWindow => {
  const current = map.get(key);
  if (!current || nowMs - current.startedAtMs > SESSION_EVENT_BURST_WINDOW_MS) {
    const next = {
      startedAtMs: nowMs,
      count: increment,
      emitted: false,
    };
    map.set(key, next);
    return next;
  }
  current.count += increment;
  return current;
};

const noteLiveEventBursts = (
  sessionId: string,
  entry: InternalEntry,
  events: SessionEvent[],
) => {
  const normalizedSessionId = sessionId.trim();
  if (!normalizedSessionId || events.length === 0) return;
  const nowMs = Date.now();
  const analytics = resolveTurnAnalyticsMetadata(entry.session, normalizedSessionId);
  const sessionBurst = advanceBurstWindow(
    sessionEventBurstBySession,
    normalizedSessionId,
    events.length,
    nowMs,
  );
  if (!sessionBurst.emitted && sessionBurst.count >= SESSION_EVENT_BURST_THRESHOLD) {
    sessionBurst.emitted = true;
    trackSessionEventVolumeBurst({
      source: "session_replica_ingest",
      sessionId: analytics.sessionId,
      taskId: analytics.taskId,
      workspaceId: analytics.workspaceId,
      count: sessionBurst.count,
      windowMs: SESSION_EVENT_BURST_WINDOW_MS,
    });
  }

  const unknownCounts = new Map<string, number>();
  for (const event of events) {
    if (event.event_type !== "notice") continue;
    const payload = event.payload_json;
    if (!payload || typeof payload !== "object" || Array.isArray(payload)) continue;
    const kind = String(payload.kind ?? payload.code ?? "").trim().toLowerCase();
    if (kind !== "crp_unknown_event") continue;
    const originalType = String(payload.original_type ?? payload.originalType ?? "unknown").trim() || "unknown";
    unknownCounts.set(originalType, (unknownCounts.get(originalType) ?? 0) + 1);
  }
  for (const [originalType, count] of unknownCounts) {
    const burstKey = `${normalizedSessionId}:${originalType}`;
    const burst = advanceBurstWindow(unknownEventBurstByKey, burstKey, count, nowMs);
    if (!burst.emitted && burst.count >= UNKNOWN_EVENT_BURST_THRESHOLD) {
      burst.emitted = true;
      trackUnknownEventBurst({
        source: "session_replica_ingest",
        sessionId: analytics.sessionId,
        taskId: analytics.taskId,
        workspaceId: analytics.workspaceId,
        originalType,
        count: burst.count,
        windowMs: SESSION_EVENT_BURST_WINDOW_MS,
      });
    }
  }
};

const isOverlayOnlyStreamPatch = (patch: SessionReplicaPatch): boolean => {
  if (patch.op !== "append" || patch.data.appendMode !== "stream_delta") return false;
  const data = patch.data;
  return (
    data.assistantStreamingByTurnId !== undefined &&
    data.turns === undefined &&
    data.messages === undefined &&
    data.events === undefined &&
    data.toolSummaries === undefined &&
    data.session === undefined &&
    data.activity === undefined &&
    data.freshness === undefined &&
    data.lastEventSeq === undefined &&
    data.projectionRev === undefined &&
    data.stateRev === undefined
  );
};

const hasDurableSeq = (value: number | undefined): value is number =>
  typeof value === "number" && value >= 0;

const isStaleStreamPatch = (entry: InternalEntry, patch: SessionReplicaPatch): boolean => {
  if (patch.op !== "append" || patch.data.appendMode !== "stream_delta") return false;
  const incomingSeq = hasDurableSeq(patch.data.lastEventSeq) ? patch.data.lastEventSeq : undefined;
  const existingSeq = hasDurableSeq(entry.lastEventSeq) ? entry.lastEventSeq : undefined;
  if (incomingSeq !== undefined && existingSeq !== undefined && incomingSeq < existingSeq) {
    return true;
  }

  const incomingProjectionRev = hasDurableSeq(patch.data.projectionRev) ? patch.data.projectionRev : undefined;
  const existingProjectionRev = hasDurableSeq(entry.projectionRev) ? entry.projectionRev : undefined;
  return (
    incomingProjectionRev !== undefined &&
    existingProjectionRev !== undefined &&
    incomingProjectionRev < existingProjectionRev &&
    (incomingSeq === undefined || existingSeq === undefined)
  );
};

export const applyReplicaPatches = (
  host: SessionSupervisorReplicaPatchHost,
  patches: SessionReplicaPatch[],
): { changed: boolean; subscriptionCursorsChanged: boolean } => {
  if (!patches || patches.length === 0) {
    return { changed: false, subscriptionCursorsChanged: false };
  }

  let changed = false;
  let subscriptionCursorsChanged = false;
  for (const patch of patches) {
    const sessionId = String(patch.sessionId || "").trim();
    if (!sessionId) continue;
    const existingEntry = host.getEntry?.(sessionId);
    if (!existingEntry && isOverlayOnlyStreamPatch(patch)) continue;
    const entry = existingEntry ?? host.ensureEntry(sessionId);
    const priorHistoryExtended = entry.historyExtended;

    if (isStaleStreamPatch(entry, patch)) continue;

    if (patch.op === "evict") {
      const beforeSeq = patch.data.eventsBeforeSeq;
      if (typeof beforeSeq === "number") {
        entry.events = entry.events.filter(
          (event) => typeof event.seq === "number" && event.seq >= beforeSeq,
        );
        rebuildSeqAndStartState(entry);
        entry.eventsRev += 1;
        entry.updatedAtMs = Date.now();
        changed = true;
      }
      continue;
    }

    const normalizedFreshness =
      patch.data.freshness === undefined ? undefined : patch.data.freshness === "authoritative"
        ? "replica"
        : patch.data.freshness;
    if (patch.op === "replace" && !shouldReplayReplicaReplace({
      entry,
      patch,
      normalizedFreshness,
    })) {
      continue;
    }

    let entryChanged = applyCanonicalTranscriptPatch(host, entry, patch, normalizedFreshness);

    const data = patch.data;
    if (Array.isArray(data.events) && data.events.length > 0) {
      if (patch.op === "append" && patch.data.appendMode === "stream_delta") {
        noteLiveEventBursts(sessionId, entry, data.events);
      }
      host.applyAcpMetaFromEvents(entry, data.events);
      host.applyGitStatusSnapshotFromEvents(entry, data.events);
    }
    if (data.gitStatusSummary !== undefined) {
      if (entry.support.gitStatusSummary !== (data.gitStatusSummary ?? null)) {
        entry.support.gitStatusSummary = data.gitStatusSummary ?? null;
        entryChanged = true;
      }
      host.syncStateCache(entry);
    }
    if (data.stateLoaded !== undefined) {
      entry.support.stateLoaded = data.stateLoaded;
      if (data.stateLoaded) {
        host.clearSupportLoadError(entry, "state");
      }
    }
    if (data.stateLoading !== undefined) {
      if (entry.support.stateLoading !== data.stateLoading) {
        entry.support.stateLoading = data.stateLoading;
        entryChanged = true;
      }
    }
    if (data.loading !== undefined) {
      const prevLoading = entry.loading;
      const prevLoadState = entry.loadState;
      entry.loading = data.loading;
      if (data.loading && entry.loadState !== "live") {
        host.setSessionLoadState(entry, "pending_hydration");
      }
      if (entry.loading !== prevLoading || entry.loadState !== prevLoadState) {
        entryChanged = true;
      }
    }
    if (data.error !== undefined) {
      const prevError = entry.error;
      const prevLoadState = entry.loadState;
      if (data.error) {
        host.setFatalError(entry, data.error);
      } else {
        entry.error = undefined;
        if (entry.loadState === "fatal") {
          host.setSessionLoadState(entry, "pending_hydration");
        }
      }
      if (entry.error !== prevError || entry.loadState !== prevLoadState) {
        entryChanged = true;
      }
    } else if (hasSessionReplicaRecoveryData(data)) {
      const prevLoadState = entry.loadState;
      entry.error = undefined;
      host.setSessionLoadState(entry, resolveReplicaReadyLoadState(entry));
      if (entry.loadState !== prevLoadState) {
        entryChanged = true;
      }
    }
    if (data.subagentNotice) {
      void host.ensureSubagentInvocations(entry, { force: true });
    }
    if (!entry.acpModels || !hasModelList(entry.acpModels)) {
      void host.ensureProviderOptions(entry);
    }

    if (patch.op === "replace" && priorHistoryExtended && data.hasMoreTurns === false) {
      entry.hasMoreTurns = true;
      entry.historyExtended = true;
      entryChanged = true;
    }

    if (data.lastEventSeq !== undefined && entry.subscribed) {
      subscriptionCursorsChanged = true;
    }
    host.syncSupportLoadsForOpenSession(entry);
    if (entryChanged) {
      entry.updatedAtMs = Date.now();
      changed = true;
    }
  }

  return { changed, subscriptionCursorsChanged };
};
