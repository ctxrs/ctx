import type {
  Message,
  Session,
  SessionActivityState,
  SessionEvent,
  SessionHeadWindow,
  SessionSummaryCheckpoint,
  SessionTurn,
  SessionTurnToolSummary,
} from "@ctx/types";
import type { AssistantStreamingState } from "./assistantStreaming";
import type {
  SessionReplicaAppendMode,
  SessionReplicaCanonicalAppendMode,
  SessionReplicaData,
  SessionReplicaFreshnessState,
  SessionReplicaReplaceMode,
} from "./sessionReplicaProtocol";

type SessionReplicaPatchEntry = {
  session?: Session;
  activity?: SessionActivityState | null;
  freshness: SessionReplicaFreshnessState;
  turns: SessionTurn[];
  turnsRev: number;
  assistantStreamingByTurnId: Record<string, AssistantStreamingState>;
  assistantStreamingRev: number;
  messages: Message[];
  messagesRev: number;
  events: SessionEvent[];
  eventsRev: number;
  toolSummaries: SessionTurnToolSummary[];
  lastEventSeq?: number;
  projectionRev?: number;
  stateRev?: number;
  hasMoreTurns: boolean;
  summaryCheckpoint?: SessionSummaryCheckpoint | null;
  headWindow?: SessionHeadWindow | null;
  hydrated: boolean;
};

export function buildCanonicalReplicaPatch(
  entry: SessionReplicaPatchEntry,
  opts: {
    appendMode: SessionReplicaCanonicalAppendMode;
    replaceMode?: SessionReplicaReplaceMode;
  },
): SessionReplicaData & { appendMode: SessionReplicaCanonicalAppendMode };
export function buildCanonicalReplicaPatch(
  entry: SessionReplicaPatchEntry,
  opts?: {
    replaceMode?: SessionReplicaReplaceMode;
    appendMode?: undefined;
  },
): SessionReplicaData;
export function buildCanonicalReplicaPatch(
  entry: SessionReplicaPatchEntry,
  opts?: {
    replaceMode?: SessionReplicaReplaceMode;
    appendMode?: SessionReplicaCanonicalAppendMode;
  },
): SessionReplicaData {
  const patch: SessionReplicaData & { appendMode?: SessionReplicaAppendMode } = {
    session: entry.session,
    activity: entry.activity ?? null,
    freshness: entry.freshness,
    turns: entry.turns,
    turnsRev: entry.turnsRev,
    assistantStreamingByTurnId: entry.assistantStreamingByTurnId,
    assistantStreamingRev: entry.assistantStreamingRev,
    messages: entry.messages,
    messagesRev: entry.messagesRev,
    events: entry.events,
    eventsRev: entry.eventsRev,
    toolSummaries: entry.toolSummaries,
    lastEventSeq: entry.lastEventSeq,
    projectionRev: entry.projectionRev,
    hasMoreTurns: entry.hasMoreTurns,
    summaryCheckpoint: entry.summaryCheckpoint ?? null,
    headWindow: entry.headWindow ?? null,
    turnsHydrated: entry.hydrated,
  };
  if (entry.stateRev !== undefined) {
    patch.stateRev = entry.stateRev;
  }
  if (opts?.replaceMode) {
    patch.replaceMode = opts.replaceMode;
  }
  if (opts?.appendMode) {
    patch.appendMode = opts.appendMode;
  }
  return patch;
}

export function buildStreamDeltaReplicaPatch(
  entry: SessionReplicaPatchEntry,
  opts: {
    turns?: SessionTurn[];
    messages?: Message[];
    removedMessageIds?: string[];
    events?: SessionEvent[];
    toolSummaries?: SessionTurnToolSummary[];
    includeSession?: boolean;
    includeActivity?: boolean;
    includeAssistantStreaming?: boolean;
  },
): SessionReplicaData & { appendMode: "stream_delta" } {
  const patch: SessionReplicaData & { appendMode: "stream_delta" } = {
    appendMode: "stream_delta",
    freshness: entry.freshness,
    lastEventSeq: entry.lastEventSeq,
    projectionRev: entry.projectionRev,
    turnsHydrated: entry.hydrated,
  };
  if (entry.stateRev !== undefined) {
    patch.stateRev = entry.stateRev;
  }
  if (opts.includeSession && entry.session) {
    patch.session = entry.session;
  }
  if (opts.includeActivity) {
    patch.activity = entry.activity ?? null;
  }
  if (opts.turns && opts.turns.length > 0) {
    patch.turns = opts.turns;
    patch.turnsRev = entry.turnsRev;
  }
  if (opts.messages && opts.messages.length > 0) {
    patch.messages = opts.messages;
    patch.messagesRev = entry.messagesRev;
  }
  if (opts.removedMessageIds && opts.removedMessageIds.length > 0) {
    patch.removedMessageIds = opts.removedMessageIds;
    patch.messagesRev = entry.messagesRev;
  }
  if (opts.events && opts.events.length > 0) {
    patch.events = opts.events;
    patch.eventsRev = entry.eventsRev;
  }
  if (opts.toolSummaries && opts.toolSummaries.length > 0) {
    patch.toolSummaries = opts.toolSummaries;
  }
  if (opts.includeAssistantStreaming) {
    patch.assistantStreamingByTurnId = entry.assistantStreamingByTurnId;
    patch.assistantStreamingRev = entry.assistantStreamingRev;
  }
  return patch;
}

export function buildStreamingOverlayReplicaPatch(
  entry: SessionReplicaPatchEntry,
): SessionReplicaData & { appendMode: "stream_delta" } {
  const patch: SessionReplicaData & { appendMode: "stream_delta" } = {
    assistantStreamingByTurnId: entry.assistantStreamingByTurnId,
    assistantStreamingRev: entry.assistantStreamingRev,
    appendMode: "stream_delta",
  };
  return patch;
}
