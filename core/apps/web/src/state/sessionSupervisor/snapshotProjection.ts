import type {
  ConnectionStatus,
  InternalEntry,
  SessionCacheEntry,
  SessionSupervisorSnapshot,
} from "./entryState";
import { buildSessionThreadProjectionFromSnapshot } from "../sessionThreadProjection/applySnapshot";
import { deriveSessionThreadEventsStamp } from "../sessionThreadProjection/applyEvents";
import {
  buildAssistantStreamingStamp,
  buildMessagesStamp,
  buildTurnsStamp,
} from "../sessionThreadProjection/stamps";

export type SessionSupervisorSnapshotProjectionHost = {
  maxCachedSessions: number;
  listeners: Set<() => void>;
  snapshot: SessionSupervisorSnapshot;
  entries: Map<string, InternalEntry>;
  onEvictSession?: (sessionId: string) => void;
};

export function mapConnection(
  connection: "idle" | "connecting" | "connected" | "disconnected",
): ConnectionStatus {
  if (connection === "connected") return "connected";
  if (connection === "disconnected") return "disconnected";
  if (connection === "connecting") return "connecting";
  return "idle";
}

export function setConnection(
  this: SessionSupervisorSnapshotProjectionHost,
  next: ConnectionStatus,
) {
  const prev = this.snapshot.connection;
  if (prev === next) return;
  this.snapshot = { ...this.snapshot, connection: next };
  for (const listener of this.listeners) listener();
}

export function evictIfNeeded(this: SessionSupervisorSnapshotProjectionHost) {
  if (this.entries.size <= this.maxCachedSessions) return;
  const now = Date.now();
  const candidates = [...this.entries.values()]
    .filter((entry) => entry.refCount === 0 && now > entry.warmUntilMs)
    .sort((a, b) => a.updatedAtMs - b.updatedAtMs);
  for (const entry of candidates) {
    if (this.entries.size <= this.maxCachedSessions) break;
    this.entries.delete(entry.sessionId);
    this.onEvictSession?.(entry.sessionId);
  }
}

function haveSameStringMembers(previous: readonly string[], next: ReadonlySet<string>): boolean {
  if (previous.length !== next.size) return false;
  let index = 0;
  for (const value of next) {
    if (previous[index] !== value) return false;
    index += 1;
  }
  return true;
}

function haveSameRecordEntries(
  previous: Record<string, unknown> | undefined,
  next: Record<string, unknown> | undefined,
): boolean {
  if (previous === next) return true;
  if (!previous || !next) return false;
  const previousKeys = Object.keys(previous);
  const nextKeys = Object.keys(next);
  if (previousKeys.length !== nextKeys.length) return false;
  for (const key of previousKeys) {
    if (previous[key] !== next[key]) return false;
  }
  return true;
}

function threadProjectionStillMatchesEntry(
  entry: InternalEntry,
  previous: SessionCacheEntry,
): boolean {
  const projection = previous.threadProjection;
  if (!projection) {
    return false;
  }
  const assistantStreamingStamp = buildAssistantStreamingStamp(
    entry.assistantStreamingByTurnId,
    entry.assistantStreamingRev,
  );
  if (projection.assistantStreamingStamp !== assistantStreamingStamp) {
    return false;
  }
  const turnsStamp = buildTurnsStamp(entry.turns, entry.turnsRev);
  if (projection.turnsStamp !== turnsStamp) {
    return false;
  }
  const messagesStamp = buildMessagesStamp(entry.messages, entry.messagesRev);
  if (projection.messagesStamp !== messagesStamp) {
    return false;
  }
  const eventsStamp = deriveSessionThreadEventsStamp(entry.events, entry.eventsRev);
  return projection.eventsStamp === eventsStamp;
}

const cloneSessionEntry = (entry: InternalEntry, previous?: SessionCacheEntry): SessionCacheEntry => {
  const overlay = entry.overlay;
  const support = entry.support;
  const canReuseThreadProjection =
    previous?.threadProjection != null &&
    previous.turns === entry.turns &&
    previous.turnsRev === entry.turnsRev &&
    previous.assistantStreamingByTurnId === entry.assistantStreamingByTurnId &&
    previous.assistantStreamingRev === entry.assistantStreamingRev &&
    previous.messages === entry.messages &&
    previous.messagesRev === entry.messagesRev &&
    previous.events === entry.events &&
    previous.eventsRev === entry.eventsRev &&
    previous.turnToolsByTurnId === support.turnToolsByTurnId &&
    previous.toolSummariesReady === support.toolSummariesReady &&
    previous.projectionRev === (entry.projectionRev ?? 0) &&
    previous.stateLoaded === support.stateLoaded &&
    threadProjectionStillMatchesEntry(entry, previous);
  const baseThreadProjection = canReuseThreadProjection
    ? previous.threadProjection!
    : buildSessionThreadProjectionFromSnapshot({
        stateLoaded: support.stateLoaded,
        turns: entry.turns,
        turnsRev: entry.turnsRev,
        assistantStreamingByTurnId: entry.assistantStreamingByTurnId,
        assistantStreamingRev: entry.assistantStreamingRev,
        messages: entry.messages,
        messagesRev: entry.messagesRev,
        events: entry.events,
        eventsRev: entry.eventsRev,
        turnToolsByTurnId: support.turnToolsByTurnId,
        toolSummariesReady: support.toolSummariesReady,
        projectionRev: entry.projectionRev,
      });
  const turnToolsLoading =
    previous && haveSameStringMembers(previous.turnToolsLoading, support.turnToolsLoadingSet)
      ? previous.turnToolsLoading
      : [...support.turnToolsLoadingSet];
  const loadErrors =
    previous && haveSameRecordEntries(previous.loadErrors, support.loadErrors)
      ? (previous.loadErrors ?? {})
      : { ...support.loadErrors };
  const fetching =
    previous && previous.fetching && previous.fetching.head === support.fetching.head && previous.fetching.history === support.fetching.history
      ? previous.fetching
      : { ...support.fetching };

  const nextEntry: SessionCacheEntry = {
    sessionId: entry.sessionId,
    mode: entry.mode,
    loadState: entry.loadState,
    freshness: entry.freshness,
    session: entry.session,
    activity: entry.activity ?? null,
    acpModels: entry.acpModels,
    acpModes: entry.acpModes,
    acpCurrentModelId: entry.acpCurrentModelId,
    acpCommands: entry.acpCommands,
    acpSlashCommands: entry.acpSlashCommands,
    turns: entry.turns,
    turnToolsByTurnId: support.turnToolsByTurnId,
    turnToolsLoading,
    toolSummaries: entry.toolSummaries,
    toolSummariesReady: support.toolSummariesReady,
    hasMoreTurns: entry.hasMoreTurns,
    events: entry.events,
    eventsRev: entry.eventsRev,
    messages: entry.messages,
    messagesRev: entry.messagesRev,
    turnsRev: entry.turnsRev,
    assistantStreamingByTurnId: entry.assistantStreamingByTurnId,
    assistantStreamingRev: entry.assistantStreamingRev,
    artifacts: support.artifacts,
    artifactsLoading: support.stateLoading && !support.stateLoaded && support.artifacts.length === 0,
    subagentInvocations: support.subagentInvocations,
    subagentInvocationsLoaded: support.subagentInvocationsLoaded,
    subagentInvocationsLoading: support.subagentInvocationsLoading,
    stateLoaded: support.stateLoaded,
    stateLoading: support.stateLoading,
    stateRev: entry.stateRev,
    loadErrors,
    queue: entry.queue,
    optimisticThreadMessages: overlay.optimisticThreadMessages,
    optimisticQueuedMessages: overlay.optimisticQueuedMessages,
    optimisticQueueRemovalIds: overlay.optimisticQueueRemovalIds,
    overlayRev: overlay.overlayRev,
    diff: support.diff,
    gitStatusSummary: support.gitStatusSummary ?? null,
    summaryCheckpoint: entry.summaryCheckpoint ?? null,
    headWindow: entry.headWindow ?? null,
    projectionRev: baseThreadProjection.projectionRev,
    threadProjection: baseThreadProjection,
    lastEventSeq: entry.lastEventSeq,
    loading: entry.loading,
    error: entry.error,
    subscribed: entry.subscribed,
    oldestTurnSeq: entry.oldestTurnSeq,
    fetching,
    updatedAtMs: entry.updatedAtMs,
  };
  if (
    previous &&
    previous.mode === nextEntry.mode &&
    previous.loadState === nextEntry.loadState &&
    previous.freshness === nextEntry.freshness &&
    previous.session === nextEntry.session &&
    previous.activity === nextEntry.activity &&
    previous.acpModels === nextEntry.acpModels &&
    previous.acpModes === nextEntry.acpModes &&
    previous.acpCurrentModelId === nextEntry.acpCurrentModelId &&
    previous.acpCommands === nextEntry.acpCommands &&
    previous.acpSlashCommands === nextEntry.acpSlashCommands &&
    previous.turns === nextEntry.turns &&
    previous.turnToolsByTurnId === nextEntry.turnToolsByTurnId &&
    previous.turnToolsLoading === nextEntry.turnToolsLoading &&
    previous.toolSummaries === nextEntry.toolSummaries &&
    previous.toolSummariesReady === nextEntry.toolSummariesReady &&
    previous.hasMoreTurns === nextEntry.hasMoreTurns &&
    previous.events === nextEntry.events &&
    previous.eventsRev === nextEntry.eventsRev &&
    previous.messages === nextEntry.messages &&
    previous.messagesRev === nextEntry.messagesRev &&
    previous.turnsRev === nextEntry.turnsRev &&
    previous.assistantStreamingByTurnId === nextEntry.assistantStreamingByTurnId &&
    previous.assistantStreamingRev === nextEntry.assistantStreamingRev &&
    previous.artifacts === nextEntry.artifacts &&
    previous.artifactsLoading === nextEntry.artifactsLoading &&
    previous.subagentInvocations === nextEntry.subagentInvocations &&
    previous.subagentInvocationsLoaded === nextEntry.subagentInvocationsLoaded &&
    previous.subagentInvocationsLoading === nextEntry.subagentInvocationsLoading &&
    previous.stateLoaded === nextEntry.stateLoaded &&
    previous.stateLoading === nextEntry.stateLoading &&
    previous.stateRev === nextEntry.stateRev &&
    previous.loadErrors === nextEntry.loadErrors &&
    previous.queue === nextEntry.queue &&
    previous.optimisticThreadMessages === nextEntry.optimisticThreadMessages &&
    previous.optimisticQueuedMessages === nextEntry.optimisticQueuedMessages &&
    previous.optimisticQueueRemovalIds === nextEntry.optimisticQueueRemovalIds &&
    previous.overlayRev === nextEntry.overlayRev &&
    previous.diff === nextEntry.diff &&
    previous.gitStatusSummary === nextEntry.gitStatusSummary &&
    previous.summaryCheckpoint === nextEntry.summaryCheckpoint &&
    previous.headWindow === nextEntry.headWindow &&
    previous.projectionRev === nextEntry.projectionRev &&
    previous.threadProjection === nextEntry.threadProjection &&
    previous.lastEventSeq === nextEntry.lastEventSeq &&
    previous.loading === nextEntry.loading &&
    previous.error === nextEntry.error &&
    previous.subscribed === nextEntry.subscribed &&
    previous.oldestTurnSeq === nextEntry.oldestTurnSeq &&
    previous.fetching === nextEntry.fetching &&
    previous.updatedAtMs === nextEntry.updatedAtMs
  ) {
    return previous;
  }
  return nextEntry;
};

export function publish(this: SessionSupervisorSnapshotProjectionHost) {
  evictIfNeeded.call(this);
  const sessions: Record<string, SessionCacheEntry> = {};
  const previousSessions = this.snapshot.sessions;
  let changed = false;
  for (const [id, entry] of this.entries) {
    const nextEntry = cloneSessionEntry(entry, previousSessions[id]);
    sessions[id] = nextEntry;
    if (nextEntry !== previousSessions[id]) {
      changed = true;
    }
  }
  if (!changed) {
    const previousIds = Object.keys(previousSessions);
    if (previousIds.length !== this.entries.size) {
      changed = true;
    } else {
      for (const id of previousIds) {
        if (!(id in sessions)) {
          changed = true;
          break;
        }
      }
    }
  }
  if (!changed) {
    return;
  }
  this.snapshot = { connection: this.snapshot.connection, sessions };
  for (const listener of this.listeners) listener();
}
