import {
  idToString,
  type Message,
  type SessionEvent,
  type SessionTurn,
} from "../../api/client";
import {
  mergeSessionEvents,
  mergeSessionMessages,
  mergeSessionToolSummaries,
  mergeSessionTurns,
} from "../sessionHeadState";
import {
  reconcileActivityFromTurns,
  reconcileLatestTurnInterruptedFromActivity,
} from "./cachePolicy";
import { resolveTurnAnalyticsMetadata } from "./turnAnalyticsMetadata";
import { replayTurnStartEffectsFromTurns } from "./turnStartEffects";
import { replayTurnOutcomeEffectsFromTurns } from "./turnOutcomeEffects";
import { shouldReplayReplicaReplace } from "./authorityPolicy";
import type { InternalEntry } from "./entryState";
import type { SessionReplicaPatch } from "../sessionReplicaProtocol";
import { adoptLoadedStateRevision } from "./supportLoads";
import type { SessionSupervisorWorkspaceSnapshotState } from "./workspaceInputs";
import {
  applyCanonicalToolSummaries,
  haveSameArrayRefs,
  haveSameRecordRefs,
  mergeStreamDeltaTurns,
  preserveLocalQueuedMessages,
  preserveLocalUserMessageAnchors,
  preserveMonotonicTurns,
  rebuildSeqAndStartState,
  removeMessagesById,
  repairReplaceShouldPreserveEntryTranscript,
} from "./replicaPatchTranscriptMerge";
export { rebuildSeqAndStartState } from "./replicaPatchTranscriptMerge";

export type SessionSupervisorReplicaPatchHost = {
  workspaceSnapshotState: SessionSupervisorWorkspaceSnapshotState;
  getEntry?(sessionId: string): InternalEntry | undefined;
  ensureEntry(sessionId: string): InternalEntry;
  resolveSessionMode(
    sessionId: string,
    entry?: InternalEntry,
    explicitMode?: InternalEntry["mode"],
  ): InternalEntry["mode"] | null;
  resetEntryProjectionForReplace(entry: InternalEntry, opts?: { skipPublish?: boolean }): void;
  setSessionLoadState(entry: InternalEntry, next: InternalEntry["loadState"]): void;
  setFatalError(entry: InternalEntry, message: string): void;
  applyAcpMetaFromEvents(entry: InternalEntry, events: SessionEvent[]): boolean;
  applyGitStatusSnapshotFromEvents(entry: InternalEntry, events: SessionEvent[]): boolean;
  syncStateCache(entry: InternalEntry): void;
  clearSupportLoadError(entry: InternalEntry, key: "state" | "subagentInvocations"): void;
  adoptLoadedSubagentInvocationsRevision(entry: InternalEntry, stateRev: number): void;
  ensureProviderOptions(entry: InternalEntry): Promise<void>;
  ensureSubagentInvocations(entry: InternalEntry, opts?: { force?: boolean }): Promise<void>;
  syncSupportLoadsForOpenSession(entry: InternalEntry): void;
  bumpTurnsRev(entry: InternalEntry): void;
};

export const applyCanonicalTranscriptPatch = (
  host: SessionSupervisorReplicaPatchHost,
  entry: InternalEntry,
  patch: Exclude<SessionReplicaPatch, { op: "evict" }>,
  normalizedFreshness: InternalEntry["freshness"] | undefined,
): boolean => {
  const data = patch.data;
  const support = entry.support;
  const replaceMode = patch.op === "replace" ? data.replaceMode ?? null : null;
  const shouldApplyReplace = patch.op !== "replace" || shouldReplayReplicaReplace({
    entry,
    patch,
    normalizedFreshness,
  });
  if (!shouldApplyReplace) {
    return false;
  }
  const preserveCoveredHistoryOnReplace =
    patch.op === "replace" &&
    repairReplaceShouldPreserveEntryTranscript(entry, data) &&
    (entry.historyExtended || replaceMode === "repair_replace");
  const mergeAppendStreamDelta = patch.op === "append" && data.appendMode === "stream_delta";
  const removedMessageIds = mergeAppendStreamDelta
    ? new Set((data.removedMessageIds ?? []).map((id) => idToString(id)).filter(Boolean))
    : new Set<string>();
  const localQueuedMessages =
    Array.isArray(data.messages)
      ? preserveLocalQueuedMessages(removeMessagesById(entry.messages, removedMessageIds), data.messages)
      : [];
  const previousTurns = entry.turns;
  const previousMessages = entry.messages;
  let nextTurnsForAnalytics: SessionTurn[] | null = null;
  let changed = false;

  if (patch.op === "replace" && shouldApplyReplace && !preserveCoveredHistoryOnReplace) {
    host.resetEntryProjectionForReplace(entry, { skipPublish: true });
    changed = true;
  }

  if (!preserveCoveredHistoryOnReplace) {
    let nextTurns = entry.turns;
    if (Array.isArray(data.turns)) {
      nextTurns = preserveMonotonicTurns(
        previousTurns,
        mergeAppendStreamDelta ? mergeStreamDeltaTurns(entry.turns, data.turns) : data.turns,
      );
    }
    let nextMessages = removeMessagesById(entry.messages, removedMessageIds);
    if (Array.isArray(data.messages)) {
      const incomingMessages = mergeSessionMessages(data.messages, localQueuedMessages);
      nextMessages = mergeAppendStreamDelta
        ? mergeSessionMessages(entry.messages, incomingMessages)
        : incomingMessages;
      nextMessages = removeMessagesById(nextMessages, removedMessageIds);
    }
    if (Array.isArray(data.turns) || Array.isArray(data.messages)) {
      const repaired = preserveLocalUserMessageAnchors(previousTurns, previousMessages, nextTurns, nextMessages, {
        excludedMessageIds: removedMessageIds,
      });
      nextTurns = repaired.turns;
      nextMessages = repaired.messages;
    }
    if (!haveSameArrayRefs(entry.turns, nextTurns)) {
      entry.turns = nextTurns;
      entry.turnsRev = data.turnsRev ?? (entry.turnsRev + 1);
      nextTurnsForAnalytics = entry.turns;
      changed = true;
    }
    if (!haveSameArrayRefs(entry.messages, nextMessages)) {
      entry.messages = nextMessages;
      entry.messagesRev = data.messagesRev ?? (entry.messagesRev + 1);
      entry.queue = entry.messages.filter((message) => message.delivery === "queued");
      changed = true;
    }
    if (Array.isArray(data.events)) {
      const nextEvents = mergeAppendStreamDelta
        ? mergeSessionEvents(entry.events, data.events)
        : data.events;
      if (!haveSameArrayRefs(entry.events, nextEvents)) {
        entry.events = nextEvents;
        entry.eventsRev = data.eventsRev ?? (entry.eventsRev + 1);
        changed = true;
      }
    }
    if (changed) {
      rebuildSeqAndStartState(entry);
    }
    if (data.turnsHydrated !== undefined) {
      if (entry.turnsHydrated !== data.turnsHydrated) {
        entry.turnsHydrated = data.turnsHydrated;
        changed = true;
      }
    } else if ((Array.isArray(data.turns) || Array.isArray(data.messages) || Array.isArray(data.events)) && !entry.turnsHydrated) {
      entry.turnsHydrated = true;
      changed = true;
    }
    if (Array.isArray(data.toolSummaries)) {
      const nextSummaries = mergeAppendStreamDelta
        ? mergeSessionToolSummaries(entry.toolSummaries, data.toolSummaries, entry.turns)
        : data.toolSummaries;
      changed =
        applyCanonicalToolSummaries(entry, nextSummaries, {
          resetByTurn: patch.op === "replace" && !mergeAppendStreamDelta,
        }) || changed;
    }
  } else {
    let nextTurns = entry.turns;
    if (Array.isArray(data.turns)) {
      nextTurns = preserveMonotonicTurns(entry.turns, mergeSessionTurns(entry.turns, data.turns));
    }
    let nextMessages = entry.messages;
    if (Array.isArray(data.messages)) {
      nextMessages = mergeSessionMessages(entry.messages, data.messages);
    }
    if (Array.isArray(data.turns) || Array.isArray(data.messages)) {
      const repaired = preserveLocalUserMessageAnchors(previousTurns, previousMessages, nextTurns, nextMessages);
      nextTurns = repaired.turns;
      nextMessages = repaired.messages;
    }
    if (!haveSameArrayRefs(entry.turns, nextTurns)) {
      entry.turns = nextTurns;
      entry.turnsRev = data.turnsRev ?? (entry.turnsRev + 1);
      nextTurnsForAnalytics = entry.turns;
      changed = true;
    }
    if (!haveSameArrayRefs(entry.messages, nextMessages)) {
      entry.messages = nextMessages;
      entry.messagesRev = data.messagesRev ?? (entry.messagesRev + 1);
      entry.queue = entry.messages.filter((message) => message.delivery === "queued");
      changed = true;
    }
    if (Array.isArray(data.events)) {
      const nextEvents = mergeSessionEvents(entry.events, data.events);
      if (!haveSameArrayRefs(entry.events, nextEvents)) {
        entry.events = nextEvents;
        entry.eventsRev = data.eventsRev ?? (entry.eventsRev + 1);
        changed = true;
      }
    }
    if (changed) {
      rebuildSeqAndStartState(entry);
    }
    if (data.turnsHydrated !== undefined) {
      if (entry.turnsHydrated !== data.turnsHydrated) {
        entry.turnsHydrated = data.turnsHydrated;
        changed = true;
      }
    } else if ((Array.isArray(data.turns) || Array.isArray(data.messages) || Array.isArray(data.events)) && !entry.turnsHydrated) {
      entry.turnsHydrated = true;
      changed = true;
    }
    if (Array.isArray(data.toolSummaries)) {
      const nextSummaries = mergeSessionToolSummaries(entry.toolSummaries, data.toolSummaries, entry.turns);
      changed = applyCanonicalToolSummaries(entry, nextSummaries) || changed;
    }
  }
  if (data.assistantStreamingByTurnId) {
    if (!haveSameRecordRefs(entry.assistantStreamingByTurnId, data.assistantStreamingByTurnId)) {
      entry.assistantStreamingByTurnId = data.assistantStreamingByTurnId;
      entry.assistantStreamingRev = data.assistantStreamingRev ?? (entry.assistantStreamingRev + 1);
      changed = true;
    }
  }

  if (data.session) {
    if (entry.session !== data.session) {
      entry.session = data.session;
      changed = true;
    }
    if (!entry.mode) {
      const resolvedMode = host.resolveSessionMode(entry.sessionId, entry);
      if (resolvedMode) {
        entry.mode = resolvedMode;
        changed = true;
      }
    }
  }
  if (data.activity !== undefined) {
    const nextActivity = data.activity ?? null;
    if (entry.activity !== nextActivity) {
      entry.activity = nextActivity;
      changed = true;
    }
    if (reconcileLatestTurnInterruptedFromActivity(entry.turns, nextActivity)) {
      host.bumpTurnsRev(entry);
      changed = true;
    }
  }
  const reconciledActivity = reconcileActivityFromTurns(entry.activity, entry.turns);
  if (reconciledActivity !== entry.activity) {
    entry.activity = reconciledActivity;
    changed = true;
  }
  if (normalizedFreshness !== undefined) {
    if (entry.freshness !== normalizedFreshness) {
      entry.freshness = normalizedFreshness;
      changed = true;
    }
    if (normalizedFreshness !== "recovering") {
      entry.recoverySubscriptionPolicy = undefined;
    }
  }
  if (data.projectionRev !== undefined) {
    const nextProjectionRev =
      typeof entry.projectionRev === "number"
        ? Math.max(entry.projectionRev, data.projectionRev)
        : data.projectionRev;
    if (entry.projectionRev !== nextProjectionRev) {
      entry.projectionRev = nextProjectionRev;
      changed = true;
    }
  }
  if (data.stateRev !== undefined) {
    entry.stateRev = data.stateRev;
    support.stateAppliedRev = adoptLoadedStateRevision(
      support.stateLoaded,
      support.stateAppliedRev,
      data.stateRev,
    );
    host.adoptLoadedSubagentInvocationsRevision(entry, data.stateRev);
  }
  if (data.summaryCheckpoint !== undefined) {
    if (entry.summaryCheckpoint !== data.summaryCheckpoint) {
      entry.summaryCheckpoint = data.summaryCheckpoint;
      changed = true;
    }
  }
  if (data.headWindow !== undefined) {
    if (entry.headWindow !== data.headWindow) {
      entry.headWindow = data.headWindow;
      changed = true;
    }
  }
  if (data.lastEventSeq !== undefined) {
    const nextLastEventSeq =
      typeof entry.lastEventSeq === "number"
        ? Math.max(entry.lastEventSeq, data.lastEventSeq)
        : data.lastEventSeq;
    if (entry.lastEventSeq !== nextLastEventSeq) {
      entry.lastEventSeq = nextLastEventSeq;
      changed = true;
    }
  }
  if (data.hasMoreTurns !== undefined) {
    const preserveHasMoreHistory =
      patch.op === "replace" && data.hasMoreTurns === false && entry.historyExtended;
    const nextHasMoreTurns = preserveHasMoreHistory ? true : data.hasMoreTurns;
    if (entry.hasMoreTurns !== nextHasMoreTurns) {
      entry.hasMoreTurns = nextHasMoreTurns;
      changed = true;
    }
    if (preserveHasMoreHistory) {
      entry.historyExtended = true;
    }
  }
  if (Array.isArray(data.turns) && entry.turns.length > 0) {
    const nextOldestTurnSeq = entry.turns[0]?.start_seq;
    if (typeof nextOldestTurnSeq === "number" && entry.oldestTurnSeq !== nextOldestTurnSeq) {
      entry.oldestTurnSeq = nextOldestTurnSeq;
      changed = true;
    }
  }
  const liveTurnsForSideEffects =
    patch.op === "append" && patch.data.appendMode === "stream_delta"
      ? nextTurnsForAnalytics
      : null;
  // Only emit analytics/notifications for live incoming stream deltas.
  // Hydration/backfill/refresh paths must stay silent.
  if (liveTurnsForSideEffects) {
    const analytics = resolveTurnAnalyticsMetadata(entry.session, entry.sessionId);
    replayTurnStartEffectsFromTurns({
      ...analytics,
      previousTurns,
      nextTurns: liveTurnsForSideEffects,
    });
    replayTurnOutcomeEffectsFromTurns({
      ...analytics,
      notify: true,
      session: entry.session,
      workspaceSnapshotState: host.workspaceSnapshotState,
      events: entry.events,
      messages: entry.messages,
      previousTurns,
      nextTurns: liveTurnsForSideEffects,
    });
  }
  return changed;
};
