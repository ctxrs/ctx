import type {
  Message,
  Session,
  SessionActivityState,
  SessionEvent,
  SessionHead,
  SessionHeadDelta,
  SessionHeadSnapshot,
  SessionHeadWindow,
  SessionSnapshot,
  SessionState,
  SessionSummaryCheckpoint,
  SessionTurn,
  SessionTurnToolSummary,
} from "@ctx/types";
import type { AssistantStreamingState } from "./assistantStreaming";
import {
  isTerminalTurnStatus,
  mergeTurnCount,
  mergeTurnStatus,
  normalizeTerminalTurnLiveCounts,
} from "./sessionSupervisor/cachePolicy";
import type {
  SessionReplicaCanonicalAppendMode,
  SessionReplicaData,
  SessionReplicaFreshnessState,
  SessionReplicaReplaceMode,
} from "./sessionReplicaProtocol";

export type SessionReplicaHeadRequestOptions = {
  minEventSeq?: number;
};

export type SessionReplicaApi = {
  getSessionHead: (
    sessionId: string,
    limit?: number,
    includeEvents?: boolean,
    opts?: SessionReplicaHeadRequestOptions,
  ) => Promise<SessionHeadSnapshot | null>;
  getSessionState?: (sessionId: string) => Promise<SessionState | null>;
  getSessionSnapshot?: (sessionId: string, limit?: number, includeEvents?: boolean) => Promise<SessionSnapshot | null>;
  setAuth?: (baseUrl?: string | null, authToken?: string | null, runId?: string | null) => void;
};

export type SessionReplicaEntry = {
  sessionId: string;
  session?: Session;
  activity?: SessionActivityState | null;
  activityLastEventSeq?: number;
  activityProjectionRev?: number;
  freshness: SessionReplicaFreshnessState;
  summaryCheckpoint?: SessionSummaryCheckpoint | null;
  headWindow?: SessionHeadWindow | null;
  projectionRev?: number;
  stateRev?: number;
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
  hasMoreTurns: boolean;
  loading: boolean;
  requestToken: number;
  hydrated: boolean;
  nextTransientSeq: number;
  startedTurnIds: Set<string>;
  toolStatusByKey: Map<string, string>;
  toolIdsByTurn: Map<string, Set<string>>;
};

export type SessionReplicaApplyHeadOptions = {
  appendMode?: SessionReplicaCanonicalAppendMode;
  replaceMode?: SessionReplicaReplaceMode;
  freshness?: SessionReplicaFreshnessState;
  gapRepairEpoch?: number;
};

export type SessionReplicaGapRepairBaseline = {
  epoch: number;
  lastEventSeq: number | null;
  seedFollows: boolean;
  httpRepairStarted: boolean;
  seedFallbackTimer: ReturnType<typeof globalThis.setTimeout> | null;
};

export const clearSessionReplicaGapRepairBaseline = (
  baselines: Map<string, SessionReplicaGapRepairBaseline>,
  sessionId: string,
): void => {
  const baseline = baselines.get(sessionId);
  if (baseline?.seedFallbackTimer != null) {
    globalThis.clearTimeout(baseline.seedFallbackTimer);
  }
  baselines.delete(sessionId);
};

export const replaceSessionReplicaGapRepairBaseline = (
  baselines: Map<string, SessionReplicaGapRepairBaseline>,
  sessionId: string,
  baseline: SessionReplicaGapRepairBaseline,
): void => {
  clearSessionReplicaGapRepairBaseline(baselines, sessionId);
  baselines.set(sessionId, baseline);
};

export const normalizeReplicaId = (value: unknown): string =>
  typeof value === "string" ? value.trim() : "";

export const SHOULD_EMIT_REPLICA_DEV_DIAGNOSTICS =
  import.meta.env.DEV && import.meta.env.MODE !== "test";

const PARTIAL_EVENT_TYPES = new Set(["assistant_chunk", "assistant_complete", "context_window_update"]);
const FINAL_DELTA_EVENT_TYPES = new Set(["assistant_complete", "assistant_message_inserted"]);

const isFinalThoughtEvent = (event: SessionEvent | null | undefined): boolean => {
  if (!event || String(event.event_type ?? "") !== "thought_chunk") return false;
  const payload = event.payload_json ?? {};
  return payload?.is_final === true
    || payload?.isFinal === true
    || typeof payload?.full_content === "string"
    || typeof payload?.fullContent === "string";
};

const isPartialEvent = (event: SessionEvent | null | undefined): boolean => {
  if (!event) return false;
  const type = String(event.event_type ?? "");
  return PARTIAL_EVENT_TYPES.has(type) || (type === "thought_chunk" && !isFinalThoughtEvent(event));
};

export const resolveFinalReplicaDeltaTurnId = (delta: SessionHeadDelta): string => {
  const messageTurnId =
    delta.message?.role === "assistant" ? normalizeReplicaId(delta.message.turn_id ?? "") : "";
  const eventType = String(delta.event?.event_type ?? "");
  if (FINAL_DELTA_EVENT_TYPES.has(eventType)) {
    return normalizeReplicaId(delta.event?.turn_id ?? messageTurnId);
  }
  return messageTurnId;
};

const stripTurnPartials = (turns: SessionTurn[]): SessionTurn[] =>
  turns.map((turn) => ({ ...turn, assistant_partial: null, thought_partial: null }));

const stripPartialEvents = (events: SessionEvent[]): SessionEvent[] =>
  events.filter((event) => !isPartialEvent(event));

export const sanitizeReplicaHeadForCache = (head: SessionHead): SessionHead => ({
  ...head,
  turns: stripTurnPartials(head.turns ?? []),
  events: stripPartialEvents(head.events ?? []),
});

export const mergeReplicaToolSummaries = (
  previous: SessionTurnToolSummary[],
  next: SessionTurnToolSummary[],
): SessionTurnToolSummary[] => {
  const byId = new Map<string, SessionTurnToolSummary>();
  for (const summary of previous) byId.set(String(summary.tool_call_id), summary);
  for (const summary of next) byId.set(String(summary.tool_call_id), summary);
  return Array.from(byId.values());
};

export const isOlderReplicaVersion = (
  incomingLastEventSeq: number | null,
  incomingProjectionRev: number | null,
  existingLastEventSeq: number | null,
  existingProjectionRev: number | null,
): boolean => {
  if (incomingLastEventSeq !== null && existingLastEventSeq !== null) {
    return incomingLastEventSeq < existingLastEventSeq;
  }
  if (existingLastEventSeq !== null && incomingLastEventSeq === null) return true;
  if (incomingLastEventSeq !== null && existingLastEventSeq === null) return false;
  return incomingProjectionRev !== null
    && existingProjectionRev !== null
    && incomingProjectionRev < existingProjectionRev;
};

const mergePartial = (previous: string, next: string): string => {
  if (!previous) return next;
  if (!next) return previous;
  if (next.startsWith(previous)) return next;
  if (previous.startsWith(next)) return previous;
  return next.length >= previous.length ? next : previous;
};

const mergeReplicaTurn = (previous: SessionTurn, next: SessionTurn): SessionTurn => {
  const status = mergeTurnStatus(previous.status, next.status);
  const useNextToolCounts = !isTerminalTurnStatus(status) || isTerminalTurnStatus(next.status);
  const countBase = useNextToolCounts ? previous : next;
  const countIncoming = useNextToolCounts ? next : previous;
  return normalizeTerminalTurnLiveCounts({
    ...previous,
    ...next,
    status,
    assistant_partial: null,
    thought_partial: mergePartial(previous.thought_partial ?? "", next.thought_partial ?? ""),
    end_seq: next.end_seq ?? previous.end_seq,
    updated_at:
      String(next.updated_at ?? "").localeCompare(String(previous.updated_at ?? "")) >= 0
        ? next.updated_at
        : previous.updated_at,
    tool_total: mergeTurnCount(countBase.tool_total, countIncoming.tool_total),
    tool_pending: mergeTurnCount(countBase.tool_pending, countIncoming.tool_pending),
    tool_running: mergeTurnCount(countBase.tool_running, countIncoming.tool_running),
    tool_completed: mergeTurnCount(countBase.tool_completed, countIncoming.tool_completed),
    tool_failed: mergeTurnCount(countBase.tool_failed, countIncoming.tool_failed),
    metrics_json: next.metrics_json ?? previous.metrics_json,
  });
};

const compareTurnOrder = (left: SessionTurn, right: SessionTurn): number => {
  const leftSeq = Number(left.start_seq ?? Number.NaN);
  const rightSeq = Number(right.start_seq ?? Number.NaN);
  if (Number.isFinite(leftSeq) && Number.isFinite(rightSeq) && leftSeq !== rightSeq) {
    return leftSeq - rightSeq;
  }
  return String(left.started_at).localeCompare(String(right.started_at));
};

export const mergeReplicaTurns = (base: SessionTurn[], incoming: SessionTurn[]): SessionTurn[] => {
  if (incoming.length === 0) return base;
  const byId = new Map<string, SessionTurn>();
  for (const turn of base) {
    const id = normalizeReplicaId(turn.turn_id);
    if (id) byId.set(id, turn);
  }
  for (const turn of incoming) {
    const id = normalizeReplicaId(turn.turn_id);
    if (!id) continue;
    byId.set(id, byId.has(id) ? mergeReplicaTurn(byId.get(id)!, turn) : normalizeTerminalTurnLiveCounts(turn));
  }
  return Array.from(byId.values()).sort(compareTurnOrder);
};

export const mergeReplicaMessages = (base: Message[], incoming: Message[]): Message[] => {
  if (incoming.length === 0) return base;
  const byId = new Map<string, Message>();
  for (const message of base) {
    const id = normalizeReplicaId(message.id);
    if (id) byId.set(id, message);
  }
  for (const message of incoming) {
    const id = normalizeReplicaId(message.id);
    if (id) byId.set(id, message);
  }
  return Array.from(byId.values()).sort((left, right) => {
    const createdAt = String(left.created_at).localeCompare(String(right.created_at));
    if (createdAt !== 0) return createdAt;
    const leftSeq = Number(left.turn_sequence ?? Number.NaN);
    const rightSeq = Number(right.turn_sequence ?? Number.NaN);
    if (Number.isFinite(leftSeq) && Number.isFinite(rightSeq) && leftSeq !== rightSeq) {
      return leftSeq - rightSeq;
    }
    if (Number.isFinite(leftSeq) && !Number.isFinite(rightSeq)) return -1;
    if (!Number.isFinite(leftSeq) && Number.isFinite(rightSeq)) return 1;
    return String(normalizeReplicaId(left.id)).localeCompare(String(normalizeReplicaId(right.id)));
  });
};

export const mergeReplicaEvents = (base: SessionEvent[], incoming: SessionEvent[]): SessionEvent[] => {
  if (incoming.length === 0) return base;
  const bySeq = new Map<number, SessionEvent>();
  for (const event of base) {
    if (typeof event.seq === "number") bySeq.set(event.seq, event);
  }
  for (const event of incoming) {
    if (typeof event.seq === "number") bySeq.set(event.seq, event);
  }
  return Array.from(bySeq.values()).sort((left, right) => Number(left.seq ?? 0) - Number(right.seq ?? 0));
};

export const TRANSIENT_REPLICA_SEQ_START = -4503599627370496; // -(2 ** 52)

export const createSessionReplicaEntry = (sessionId: string): SessionReplicaEntry => ({
  sessionId,
  session: undefined,
  activity: null,
  activityLastEventSeq: undefined,
  activityProjectionRev: undefined,
  freshness: "bootstrap",
  summaryCheckpoint: undefined,
  headWindow: undefined,
  projectionRev: undefined,
  stateRev: undefined,
  turns: [],
  turnsRev: 0,
  assistantStreamingByTurnId: {},
  assistantStreamingRev: 0,
  messages: [],
  messagesRev: 0,
  events: [],
  eventsRev: 0,
  toolSummaries: [],
  lastEventSeq: undefined,
  hasMoreTurns: true,
  loading: false,
  requestToken: 0,
  hydrated: false,
  nextTransientSeq: TRANSIENT_REPLICA_SEQ_START,
  startedTurnIds: new Set<string>(),
  toolStatusByKey: new Map<string, string>(),
  toolIdsByTurn: new Map<string, Set<string>>(),
});

export const headToReplicaData = (
  head: SessionHead | SessionHeadSnapshot,
): SessionReplicaData => {
  const data: SessionReplicaData = {
    turns: head.turns ?? [],
    messages: head.messages ?? [],
    events: head.events ?? [],
    toolSummaries: head.tool_summaries ?? [],
    lastEventSeq: head.last_event_seq,
    hasMoreTurns: head.has_more_turns,
  };
  if (head.session) data.session = head.session;
  if ("activity" in head) data.activity = head.activity ?? null;
  if ("summary_checkpoint" in head) data.summaryCheckpoint = head.summary_checkpoint ?? null;
  if ("head_window" in head) data.headWindow = head.head_window ?? null;
  if ("projection_rev" in head && typeof head.projection_rev === "number") {
    data.projectionRev = head.projection_rev;
  }
  if ("state_rev" in head && typeof head.state_rev === "number") {
    data.stateRev = head.state_rev;
  }
  return data;
};

export const snapshotToSessionHead = (head: SessionHeadSnapshot): SessionHead => ({
  session: head.session,
  turns: head.turns,
  tool_summaries: head.tool_summaries,
  events: head.events,
  messages: head.messages,
  last_event_seq: head.last_event_seq,
  projection_rev: head.projection_rev,
  has_more_turns: head.has_more_turns,
  activity: head.activity,
  summary_checkpoint: head.summary_checkpoint ?? null,
  head_window: head.head_window,
});
