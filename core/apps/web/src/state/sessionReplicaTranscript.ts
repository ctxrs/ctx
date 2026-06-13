import {
  applyAssistantChunkToStreaming,
  applyAssistantCompleteToStreaming,
  clearAssistantStreaming,
  reconcileAssistantStreamingWithMessages,
  type AssistantStreamingState,
  type AssistantStreamingStore,
} from "./assistantStreaming";
import {
  idToString,
  type Message,
  type Session,
  type SessionEvent,
  type SessionTurn,
  type SessionTurnToolSummary,
} from "../api/client";
import { compareSessionTurnOrder, mergeSessionMessages } from "./sessionHeadState";
import { asRecord, messageFromEvent, readPayloadObject } from "./sessionSupervisor/eventHydration";
import { appendFragment, isTerminalTurnStatus, mergeTurn } from "./sessionSupervisor/cachePolicy";
import { readPayloadString } from "./sessionSupervisor/eventNormalization";
import {
  resolveTurnFailureFromLifecycleEvent,
  resolveTurnStatusFromLifecycleEvent,
} from "./sessionSupervisor/turnLifecycleProjection";
import {
  applyToolBucketDelta,
  deriveTurnStatusFromEvent,
  extractToolCallId,
  extractToolStatus,
  shouldRenderAssistantChunk,
  shouldRenderThoughtChunk,
  toolStatusBucket,
} from "./sessionSupervisor/toolStateProjection";
import { isFinalThoughtEvent } from "./sessionSupervisor/thoughtProjection";

export type SessionReplicaTranscriptEntry = {
  sessionId: string;
  session?: Session;
  turns: SessionTurn[];
  turnsRev: number;
  assistantStreamingByTurnId: Record<string, AssistantStreamingState>;
  assistantStreamingRev: number;
  messages: Message[];
  messagesRev: number;
  events: SessionEvent[];
  eventsRev: number;
  toolSummaries: SessionTurnToolSummary[];
  nextTransientSeq: number;
  startedTurnIds: Set<string>;
  toolStatusByKey: Map<string, string>;
  toolIdsByTurn: Map<string, Set<string>>;
};

const mergeTurns = (base: SessionTurn[], incoming: SessionTurn[]): SessionTurn[] => {
  if (incoming.length === 0) return base;
  const byId = new Map<string, SessionTurn>();
  for (const turn of base) {
    const turnId = idToString(turn.turn_id);
    if (turnId) byId.set(turnId, turn);
  }
  for (const turn of incoming) {
    const turnId = idToString(turn.turn_id);
    if (!turnId) continue;
    const previous = byId.get(turnId);
    byId.set(turnId, previous ? mergeTurn(previous, turn) : turn);
  }
  return Array.from(byId.values()).sort(compareSessionTurnOrder);
};

const upsertTurns = (base: SessionTurn[], incoming: SessionTurn[]): SessionTurn[] => {
  if (incoming.length === 0) return base;
  const byId = new Map<string, SessionTurn>();
  for (const turn of base) {
    const turnId = idToString(turn.turn_id);
    if (turnId) byId.set(turnId, turn);
  }
  for (const turn of incoming) {
    const turnId = idToString(turn.turn_id);
    if (turnId) byId.set(turnId, turn);
  }
  return Array.from(byId.values()).sort(compareSessionTurnOrder);
};

const mergeEvents = (base: SessionEvent[], incoming: SessionEvent[]): SessionEvent[] => {
  if (incoming.length === 0) return base;
  const bySeq = new Map<number, SessionEvent>();
  for (const event of base) {
    if (typeof event.seq === "number") bySeq.set(event.seq, event);
  }
  for (const event of incoming) {
    if (typeof event.seq === "number") bySeq.set(event.seq, event);
  }
  return Array.from(bySeq.values()).sort((a, b) => Number(a.seq ?? 0) - Number(b.seq ?? 0));
};

const bumpTurnsRev = (entry: Pick<SessionReplicaTranscriptEntry, "turnsRev">) => {
  entry.turnsRev += 1;
};

const bumpMessagesRev = (entry: Pick<SessionReplicaTranscriptEntry, "messagesRev">) => {
  entry.messagesRev += 1;
};

const bumpEventsRev = (entry: Pick<SessionReplicaTranscriptEntry, "eventsRev">) => {
  entry.eventsRev += 1;
};

const readPayloadNumber = (payload: unknown, keys: string[]): number | null => {
  const record = asRecord(payload);
  for (const key of keys) {
    const raw = record[key];
    if (typeof raw === "number" && Number.isFinite(raw)) return raw;
    if (typeof raw === "string" && raw.trim()) {
      const parsed = Number(raw);
      if (Number.isFinite(parsed)) return parsed;
    }
  }
  return null;
};

export const ensureReplicaEventSeq = (
  entry: Pick<SessionReplicaTranscriptEntry, "nextTransientSeq">,
  event: SessionEvent,
): SessionEvent => {
  if (typeof event.seq === "number") return event;
  const nextSeq = entry.nextTransientSeq;
  entry.nextTransientSeq = nextSeq + 1;
  return { ...event, seq: nextSeq };
};

const mergeMessagesIntoEntry = (
  entry: Pick<
    SessionReplicaTranscriptEntry,
    "messages" | "messagesRev" | "assistantStreamingByTurnId" | "assistantStreamingRev"
  >,
  incoming: Message[],
) => {
  if (incoming.length === 0) return;
  entry.messages = mergeSessionMessages(entry.messages, incoming);
  reconcileAssistantStreamingWithMessages(entry as AssistantStreamingStore, incoming);
  bumpMessagesRev(entry);
};

const ensureTurnFromEvent = (
  entry: Pick<SessionReplicaTranscriptEntry, "turns" | "turnsRev" | "startedTurnIds">,
  event: SessionEvent,
): SessionTurn | null => {
  const turnId = idToString(event.turn_id);
  if (!turnId) return null;
  const existing = entry.turns.find((turn) => idToString(turn.turn_id) === turnId);
  if (existing) return existing;
  const createdAt = event.created_at ?? new Date().toISOString();
  const turn: SessionTurn = {
    turn_id: event.turn_id ?? turnId,
    session_id: event.session_id,
    run_id: event.run_id ?? null,
    user_message_id: readPayloadString(asRecord(event.payload_json), ["user_message_id", "message_id"]) ?? null,
    status: deriveTurnStatusFromEvent(event),
    start_seq: event.seq ?? null,
    end_seq: null,
    started_at: createdAt,
    updated_at: createdAt,
    assistant_partial: null,
    thought_partial: "",
    metrics_json: null,
    tool_total: 0,
    tool_pending: 0,
    tool_running: 0,
    tool_completed: 0,
    tool_failed: 0,
  };
  if (typeof turn.start_seq === "number" && turn.start_seq >= 0) {
    entry.startedTurnIds.add(turnId);
  }
  entry.turns = [...entry.turns, turn].sort(compareSessionTurnOrder);
  bumpTurnsRev(entry);
  return turn;
};

const applyToolEventToTurn = (
  entry: Pick<SessionReplicaTranscriptEntry, "toolIdsByTurn" | "toolStatusByKey">,
  turnId: string,
  turn: SessionTurn,
  event: SessionEvent,
): boolean => {
  if (turn.status !== "running" && turn.status !== "starting" && turn.status !== "queued") {
    return false;
  }
  const toolCallId = extractToolCallId(event);
  if (!toolCallId) return false;
  const nextStatus = extractToolStatus(event);
  if (!nextStatus) return false;

  const key = `${turnId}:${toolCallId}`;
  const previousStatus = entry.toolStatusByKey.get(key);
  if (!entry.toolIdsByTurn.has(turnId)) {
    if ((turn.tool_total ?? 0) > 0) {
      return false;
    }
    entry.toolIdsByTurn.set(turnId, new Set());
  }
  const turnToolIds = entry.toolIdsByTurn.get(turnId);
  if (!turnToolIds) return false;
  const isNew = !turnToolIds.has(toolCallId);
  if (isNew) {
    turnToolIds.add(toolCallId);
    turn.tool_total = (turn.tool_total ?? 0) + 1;
  }

  const previousBucket = toolStatusBucket(previousStatus);
  const nextBucket = toolStatusBucket(nextStatus);
  if (previousBucket !== nextBucket) {
    applyToolBucketDelta(turn, previousBucket, -1);
    applyToolBucketDelta(turn, nextBucket, 1);
  }
  entry.toolStatusByKey.set(key, nextStatus);
  return true;
};

const applyEventToTurns = (
  entry: Pick<
    SessionReplicaTranscriptEntry,
    | "turns"
    | "turnsRev"
    | "toolIdsByTurn"
    | "toolStatusByKey"
    | "assistantStreamingByTurnId"
    | "assistantStreamingRev"
>,
  event: SessionEvent,
): boolean => {
  const turnId = idToString(event.turn_id);
  if (!turnId) return false;
  if (String(event.event_type) === "assistant_chunk") {
    if (!shouldRenderAssistantChunk(event)) return false;
    const fragment = String(event.payload_json?.content_fragment ?? "");
    if (!fragment) return false;
    const providerMessageId = readPayloadString(event.payload_json, ["message_id", "messageId"]);
    applyAssistantChunkToStreaming(
      entry as AssistantStreamingStore,
      turnId,
      fragment,
      providerMessageId,
      readPayloadNumber(event.payload_json, ["order_seq", "orderSeq"]),
    );
    return false;
  }
  const turnIndex = entry.turns.findIndex((turn) => idToString(turn.turn_id) === turnId);
  if (turnIndex < 0) return false;

  const turn = { ...entry.turns[turnIndex] };
  let changed = false;
  switch (String(event.event_type)) {
    case "thought_chunk": {
      if (!shouldRenderThoughtChunk(event) || isFinalThoughtEvent(event)) break;
      const fragment = String(event.payload_json?.content_fragment ?? "");
      if (fragment) {
        turn.thought_partial = appendFragment(turn.thought_partial, fragment);
        changed = true;
      }
      break;
    }
    case "assistant_message_inserted": {
      changed = clearAssistantStreaming(entry as AssistantStreamingStore, turnId) || changed;
      break;
    }
    case "assistant_complete": {
      const full =
        event.payload_json?.full_content ??
        event.payload_json?.content ??
        entry.assistantStreamingByTurnId[turnId]?.content;
      const providerMessageId = readPayloadString(event.payload_json, ["message_id", "messageId"]);
      changed =
        applyAssistantCompleteToStreaming(
          entry as AssistantStreamingStore,
          turnId,
          String(full ?? ""),
          providerMessageId,
          readPayloadNumber(event.payload_json, ["order_seq", "orderSeq"]),
        ) || changed;
      break;
    }
    case "turn_queued":
    case "turn_started":
    case "turn_finished":
    case "turn_interrupted":
    case "done": {
      const nextStatus = resolveTurnStatusFromLifecycleEvent(turn.status, event);
      if (nextStatus) {
        turn.status = nextStatus;
        if (isTerminalTurnStatus(nextStatus)) {
          turn.tool_pending = 0;
          turn.tool_running = 0;
        }
      }
      if (event.event_type === "turn_finished") {
        turn.failure = resolveTurnFailureFromLifecycleEvent(event);
      }
      const contextWindow = readPayloadObject(event.payload_json, "context_window");
      if (contextWindow) {
        turn.metrics_json = contextWindow;
      }
      changed = true;
      break;
    }
    case "tool_call":
    case "tool_call_update":
    case "tool_result":
      if (applyToolEventToTurn(entry, turnId, turn, event)) {
        changed = true;
      }
      break;
    default:
      break;
  }

  const contextWindow = readPayloadObject(event.payload_json, "context_window");
  if (contextWindow) {
    turn.metrics_json = contextWindow;
    changed = true;
  }

  if (!changed) return false;
  turn.updated_at = event.created_at ?? turn.updated_at;
  entry.turns[turnIndex] = turn;
  bumpTurnsRev(entry);
  return true;
};

export const isStreamOnlyAssistantChunk = (event: SessionEvent): boolean =>
  String(event.event_type) === "assistant_chunk";

const applyQueueEvent = (
  entry: Pick<SessionReplicaTranscriptEntry, "messages" | "messagesRev">,
  event: SessionEvent,
): boolean => {
  const messageId = idToString(readPayloadString(event.payload_json, ["message_id"]) ?? "");
  if (!messageId) return false;

  switch (String(event.event_type)) {
    case "message_queue_added": {
      const messageIndex = entry.messages.findIndex((message) => idToString(message.id) === messageId);
      if (messageIndex < 0) return false;
      const message = entry.messages[messageIndex];
      if (message.delivery === "queued") return false;
      entry.messages = entry.messages.slice();
      entry.messages[messageIndex] = { ...message, delivery: "queued" };
      bumpMessagesRev(entry);
      return true;
    }
    case "message_queue_removed": {
      const previousLength = entry.messages.length;
      entry.messages = entry.messages.filter((message) => idToString(message.id) !== messageId);
      if (entry.messages.length !== previousLength) {
        bumpMessagesRev(entry);
        return true;
      }
      return false;
    }
    case "message_queue_promoted": {
      const messageIndex = entry.messages.findIndex((message) => idToString(message.id) === messageId);
      if (messageIndex < 0) return false;
      const message = entry.messages[messageIndex];
      if (message.delivery !== "queued") return false;
      entry.messages = entry.messages.slice();
      entry.messages[messageIndex] = { ...message, delivery: "immediate" };
      bumpMessagesRev(entry);
      return true;
    }
    case "message_queue_updated":
      return false;
    default:
      return false;
  }
};

export const mergeReplicaTurnsIntoEntry = (
  entry: Pick<SessionReplicaTranscriptEntry, "turns" | "turnsRev" | "startedTurnIds">,
  incoming: SessionTurn[],
  opts?: { authoritative?: boolean },
) => {
  if (incoming.length === 0) return;
  for (const turn of incoming) {
    const turnId = idToString(turn.turn_id);
    const startSeq = typeof turn.start_seq === "number" ? turn.start_seq : Number.NaN;
    if (turnId && Number.isFinite(startSeq) && startSeq >= 0) {
      entry.startedTurnIds.add(turnId);
    }
  }
  entry.turns = opts?.authoritative ? upsertTurns(entry.turns, incoming) : mergeTurns(entry.turns, incoming);
  bumpTurnsRev(entry);
};

export const mergeReplicaMessagesIntoEntry = (
  entry: Pick<
    SessionReplicaTranscriptEntry,
    "messages" | "messagesRev" | "assistantStreamingByTurnId" | "assistantStreamingRev"
  >,
  incoming: Message[],
) => {
  mergeMessagesIntoEntry(entry, incoming);
};

export const mergeReplicaEventsIntoEntry = (
  entry: SessionReplicaTranscriptEntry,
  incoming: SessionEvent[],
  eventBufferLimit: number,
): { newEvents: SessionEvent[]; evictedBeforeSeq?: number } => {
  if (incoming.length === 0) return { newEvents: [] };
  const normalizedIncoming = incoming.map((event) => ensureReplicaEventSeq(entry, event));
  const existingSeqs = new Set(
    entry.events
      .map((event) => (typeof event.seq === "number" ? event.seq : Number.NaN))
      .filter((seq) => Number.isFinite(seq)) as number[],
  );
  const newEvents = normalizedIncoming.filter(
    (event) => typeof event.seq !== "number" || !existingSeqs.has(event.seq),
  );

  entry.events = mergeEvents(entry.events, normalizedIncoming);
  let evictedBeforeSeq: number | undefined;
  if (entry.events.length > eventBufferLimit) {
    entry.events = entry.events.slice(-eventBufferLimit);
    const firstSeq = entry.events[0]?.seq;
    if (typeof firstSeq === "number") {
      evictedBeforeSeq = firstSeq;
    }
  }
  bumpEventsRev(entry);
  return { newEvents, evictedBeforeSeq };
};

export const applyReplicaTranscriptEvent = (
  entry: SessionReplicaTranscriptEntry,
  event: SessionEvent,
) => {
  const turnId = idToString(event.turn_id);
  const streamOnlyAssistantChunk = isStreamOnlyAssistantChunk(event);
  if (!streamOnlyAssistantChunk && turnId && typeof event.seq === "number" && event.seq >= 0) {
    entry.startedTurnIds.add(turnId);
  }

  const derivedMessage = messageFromEvent(event, entry.session);
  if (derivedMessage) {
    mergeMessagesIntoEntry(entry, [derivedMessage]);
  }
  if (!streamOnlyAssistantChunk) {
    ensureTurnFromEvent(entry, event);
  }
  applyEventToTurns(entry, event);
  applyQueueEvent(entry, event);
};

export const rebuildReplicaTranscriptAuxState = (
  entry: Pick<
    SessionReplicaTranscriptEntry,
    "turns" | "events" | "toolSummaries" | "startedTurnIds" | "toolStatusByKey" | "toolIdsByTurn"
  >,
) => {
  entry.startedTurnIds = new Set(
    entry.turns
      .map((turn) => {
        const turnId = idToString(turn.turn_id);
        const startSeq = typeof turn.start_seq === "number" ? turn.start_seq : Number.NaN;
        return turnId && Number.isFinite(startSeq) && startSeq >= 0 ? turnId : "";
      })
      .filter(Boolean),
  );
  for (const event of entry.events) {
    const turnId = idToString(event.turn_id);
    const seq = typeof event.seq === "number" ? event.seq : Number.NaN;
    if (turnId && Number.isFinite(seq) && seq >= 0) {
      entry.startedTurnIds.add(turnId);
    }
  }

  entry.toolStatusByKey = new Map();
  entry.toolIdsByTurn = new Map();
  for (const summary of entry.toolSummaries) {
    const turnId = idToString(summary.turn_id);
    const toolCallId = String(summary.tool_call_id ?? "").trim();
    if (!turnId || !toolCallId) continue;
    const ids = entry.toolIdsByTurn.get(turnId) ?? new Set<string>();
    ids.add(toolCallId);
    entry.toolIdsByTurn.set(turnId, ids);
    if (typeof summary.status === "string" && summary.status.trim()) {
      entry.toolStatusByKey.set(`${turnId}:${toolCallId}`, summary.status);
    }
  }
};
