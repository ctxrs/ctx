import {
  idToString,
  type Message,
  type SessionEvent,
  type SessionHeadSnapshot,
  type SessionHeadWindow,
  type SessionTurn,
  type SessionTurnToolSummary,
} from "../api/client";
import {
  isPartialEvent,
  mergeTurn,
  normalizeTerminalTurnLiveCounts,
  stripPartialEvents,
  stripTurnPartials,
} from "./sessionSupervisor/cachePolicy";

const HEAD_EVENT_BUFFER_LIMIT = 800;
const ACTIVE_HEAD_TURN_LIMIT = 5;
const ACTIVE_HEAD_MESSAGE_LIMIT = 200;
const ACTIVE_HEAD_EVENT_LIMIT = 0;
const ACTIVE_HEAD_BYTE_LIMIT = 256_000;
const ACTIVE_HEAD_TOOL_SUMMARY_LIMIT = 96;

export const emptySessionHeadWindow = (): SessionHeadWindow => ({
  turn_limit: 0,
  message_limit: 0,
  event_limit: 0,
  byte_limit: 0,
  turn_count: 0,
  message_count: 0,
  event_count: 0,
  bytes: 0,
  truncated: false,
});

export const sanitizeSessionHeadSnapshot = (head: SessionHeadSnapshot): SessionHeadSnapshot => {
  const turns = Array.isArray(head.turns) ? stripTurnPartials(head.turns) : head.turns ?? [];
  const events = Array.isArray(head.events) ? stripPartialEvents(head.events) : head.events ?? [];
  return {
    ...head,
    turns,
    events,
  };
};

const clampActiveHeadWindowLimit = (value: number | null | undefined, limit: number): number => {
  if (typeof value !== "number" || !Number.isFinite(value) || value <= 0) {
    return limit;
  }
  return Math.min(value, limit);
};

const compareMessageOrder = (a: Message, b: Message): number => {
  const aOrderSeq = Number(a.order_seq ?? Number.NaN);
  const bOrderSeq = Number(b.order_seq ?? Number.NaN);
  if (Number.isFinite(aOrderSeq) && Number.isFinite(bOrderSeq) && aOrderSeq !== bOrderSeq) {
    return aOrderSeq - bOrderSeq;
  }
  if (Number.isFinite(aOrderSeq) && !Number.isFinite(bOrderSeq)) return -1;
  if (!Number.isFinite(aOrderSeq) && Number.isFinite(bOrderSeq)) return 1;
  const createdAtOrder = String(a.created_at ?? "").localeCompare(String(b.created_at ?? ""));
  if (createdAtOrder !== 0) return createdAtOrder;
  const aTurnSequence = Number(a.turn_sequence ?? Number.NaN);
  const bTurnSequence = Number(b.turn_sequence ?? Number.NaN);
  if (Number.isFinite(aTurnSequence) && Number.isFinite(bTurnSequence) && aTurnSequence !== bTurnSequence) {
    return aTurnSequence - bTurnSequence;
  }
  if (Number.isFinite(aTurnSequence) && !Number.isFinite(bTurnSequence)) return -1;
  if (!Number.isFinite(aTurnSequence) && Number.isFinite(bTurnSequence)) return 1;
  return String(idToString(a.id)).localeCompare(String(idToString(b.id)));
};

const compareEventOrder = (a: SessionEvent, b: SessionEvent): number =>
  Number(a.seq ?? 0) - Number(b.seq ?? 0);

const compareToolSummaryOrder = (
  a: SessionTurnToolSummary,
  b: SessionTurnToolSummary,
): number => {
  const aOrderSeq = Number(a.order_seq ?? Number.NaN);
  const bOrderSeq = Number(b.order_seq ?? Number.NaN);
  if (Number.isFinite(aOrderSeq) && Number.isFinite(bOrderSeq) && aOrderSeq !== bOrderSeq) {
    return aOrderSeq - bOrderSeq;
  }
  if (Number.isFinite(aOrderSeq) && !Number.isFinite(bOrderSeq)) return -1;
  if (!Number.isFinite(aOrderSeq) && Number.isFinite(bOrderSeq)) return 1;
  const updatedAtOrder = String(a.updated_at ?? "").localeCompare(String(b.updated_at ?? ""));
  if (updatedAtOrder !== 0) return updatedAtOrder;
  const createdAtOrder = String(a.created_at ?? "").localeCompare(String(b.created_at ?? ""));
  if (createdAtOrder !== 0) return createdAtOrder;
  return String(a.tool_call_id ?? "").localeCompare(String(b.tool_call_id ?? ""));
};

const retainMessagesForTurns = (messages: Message[], turns: SessionTurn[]): Message[] => {
  if (turns.length === 0) {
    return [];
  }
  const allowedTurnIds = new Set(
    turns.map((turn) => idToString(turn.turn_id)).filter((turnId) => turnId.length > 0),
  );
  return messages
    .filter((message) => {
      const turnId = idToString(message.turn_id);
      return !turnId || allowedTurnIds.has(turnId);
    })
    .sort(compareMessageOrder);
};

const retainToolSummariesForTurns = (
  toolSummaries: SessionTurnToolSummary[],
  turns: SessionTurn[],
): SessionTurnToolSummary[] => {
  if (turns.length === 0) return [];
  const allowedTurnIds = new Set(
    turns.map((turn) => idToString(turn.turn_id)).filter((turnId) => turnId.length > 0),
  );
  return toolSummaries.filter((summary) => allowedTurnIds.has(idToString(summary.turn_id)));
};

const estimateHeadWindowBytes = (params: {
  turns: SessionTurn[];
  toolSummaries: SessionTurnToolSummary[];
  events: SessionEvent[];
  messages: Message[];
}): number => {
  try {
    return new TextEncoder().encode(
      JSON.stringify({
        turns: params.turns,
        tool_summaries: params.toolSummaries,
        events: params.events,
        messages: params.messages,
      }),
    ).length;
  } catch {
    return 0;
  }
};

export const compactActiveSessionHeadSnapshot = (head: SessionHeadSnapshot): SessionHeadSnapshot => {
  const sanitized = sanitizeSessionHeadSnapshot(head);
  const turnLimit = clampActiveHeadWindowLimit(
    sanitized.head_window?.turn_limit,
    ACTIVE_HEAD_TURN_LIMIT,
  );
  const messageLimit = clampActiveHeadWindowLimit(
    sanitized.head_window?.message_limit,
    ACTIVE_HEAD_MESSAGE_LIMIT,
  );
  const eventLimit = clampActiveHeadWindowLimit(
    sanitized.head_window?.event_limit,
    ACTIVE_HEAD_EVENT_LIMIT,
  );
  const byteLimit = clampActiveHeadWindowLimit(
    sanitized.head_window?.byte_limit,
    ACTIVE_HEAD_BYTE_LIMIT,
  );

  let turns = Array.isArray(sanitized.turns) ? [...sanitized.turns] : [];
  let toolSummaries = Array.isArray(sanitized.tool_summaries) ? [...sanitized.tool_summaries] : [];
  let messages = Array.isArray(sanitized.messages) ? [...sanitized.messages] : [];
  let events = Array.isArray(sanitized.events) ? [...sanitized.events] : [];
  let truncated = Boolean(sanitized.head_window?.truncated);
  let hasMoreTurns = Boolean(sanitized.has_more_turns);

  turns.sort(compareSessionTurnOrder);
  messages.sort(compareMessageOrder);
  events.sort(compareEventOrder);
  toolSummaries.sort(compareToolSummaryOrder);

  const keepTurns = Math.min(turnLimit, turns.length);
  if (keepTurns === 0) {
    turns = [];
    messages = [];
    toolSummaries = [];
    events = [];
  } else {
    if (turns.length > keepTurns) {
      turns = turns.slice(-keepTurns);
      truncated = true;
      hasMoreTurns = true;
    }
    messages = retainMessagesForTurns(messages, turns);
    toolSummaries = retainToolSummariesForTurns(toolSummaries, turns);
    events = [];
  }

  if (toolSummaries.length > ACTIVE_HEAD_TOOL_SUMMARY_LIMIT) {
    toolSummaries = toolSummaries.slice(-ACTIVE_HEAD_TOOL_SUMMARY_LIMIT);
    truncated = true;
  }

  if (messages.length > messageLimit) {
    messages = messages.slice(-messageLimit);
    truncated = true;
  }

  if (events.length > eventLimit) truncated = true;
  events = [];

  let bytes = estimateHeadWindowBytes({ turns, toolSummaries, events, messages });
  while (bytes > byteLimit) {
    if (messages.length > 0) {
      messages.shift();
      truncated = true;
      bytes = estimateHeadWindowBytes({ turns, toolSummaries, events, messages });
      continue;
    }
    if (turns.length > 0) {
      turns.shift();
      truncated = true;
      hasMoreTurns = true;
      messages = retainMessagesForTurns(messages, turns);
      toolSummaries = retainToolSummariesForTurns(toolSummaries, turns);
      bytes = estimateHeadWindowBytes({ turns, toolSummaries, events, messages });
      continue;
    }
    if (events.length > 0) {
      events.shift();
      truncated = true;
      bytes = estimateHeadWindowBytes({ turns, toolSummaries, events, messages });
      continue;
    }
    break;
  }
  bytes = estimateHeadWindowBytes({ turns, toolSummaries, events, messages });

  const droppedTurnOrMessageRows =
    (sanitized.turns?.length ?? 0) > turns.length ||
    (sanitized.messages?.length ?? 0) > messages.length;
  const droppedRows =
    droppedTurnOrMessageRows ||
    (sanitized.tool_summaries?.length ?? 0) > toolSummaries.length ||
    (sanitized.events?.length ?? 0) > events.length;

  return {
    ...sanitized,
    turns,
    tool_summaries: toolSummaries,
    messages,
    events,
    has_more_turns: hasMoreTurns || droppedTurnOrMessageRows,
    head_window: {
      turn_limit: turnLimit,
      message_limit: messageLimit,
      event_limit: eventLimit,
      byte_limit: byteLimit,
      turn_count: turns.length,
      message_count: messages.length,
      event_count: events.length,
      bytes,
      truncated: truncated || droppedRows,
    },
  };
};

export const compareSessionTurnOrder = (a: SessionTurn, b: SessionTurn): number => {
  const sa = Number(a.start_seq ?? Number.NaN);
  const sb = Number(b.start_seq ?? Number.NaN);
  if (Number.isFinite(sa) && Number.isFinite(sb) && sa !== sb) {
    return sa - sb;
  }
  const startedAtOrder = String(a.started_at ?? "").localeCompare(String(b.started_at ?? ""));
  if (startedAtOrder !== 0) return startedAtOrder;
  return String(a.turn_id ?? "").localeCompare(String(b.turn_id ?? ""));
};

export const mergeSessionTurns = (prev: SessionTurn[], incoming: SessionTurn[]): SessionTurn[] => {
  if (incoming.length === 0) return prev;
  const byId = new Map<string, SessionTurn>();
  for (const turn of prev) {
    const id = idToString(turn.turn_id);
    if (id) byId.set(id, turn);
  }
  for (const turn of incoming) {
    const id = idToString(turn.turn_id);
    if (!id) continue;
    const existing = byId.get(id);
    byId.set(id, existing ? mergeTurn(existing, turn) : normalizeTerminalTurnLiveCounts(turn));
  }
  return Array.from(byId.values()).sort(compareSessionTurnOrder);
};

export const mergeSessionMessages = (prev: Message[], incoming: Message[]): Message[] => {
  if (incoming.length === 0) return prev;
  const byId = new Map<string, Message>();
  for (const message of prev) {
    const id = idToString(message.id);
    if (id) byId.set(id, message);
  }
  for (const message of incoming) {
    const id = idToString(message.id);
    if (!id) continue;
    byId.set(id, message);
  }
  return Array.from(byId.values()).sort(compareMessageOrder);
};

export const mergeSessionEvents = (prev: SessionEvent[], incoming: SessionEvent[]): SessionEvent[] => {
  if (incoming.length === 0) return prev;
  const bySeq = new Map<number, SessionEvent>();
  for (const event of prev) {
    if (typeof event.seq === "number") bySeq.set(event.seq, event);
  }
  for (const event of incoming) {
    if (typeof event.seq === "number" && !isPartialEvent(event)) {
      bySeq.set(event.seq, event);
    }
  }
  const next = Array.from(bySeq.values()).sort((a, b) => Number(a.seq ?? 0) - Number(b.seq ?? 0));
  return next.length > HEAD_EVENT_BUFFER_LIMIT ? next.slice(-HEAD_EVENT_BUFFER_LIMIT) : next;
};

export const mergeSessionToolSummaries = (
  prev: SessionTurnToolSummary[],
  incoming: SessionTurnToolSummary[],
  turns: SessionTurn[],
): SessionTurnToolSummary[] => {
  if (incoming.length === 0 && prev.length === 0) return prev;
  const byId = new Map(prev.map((summary) => [String(summary?.tool_call_id ?? ""), summary]));
  for (const summary of incoming) {
    const id = String(summary?.tool_call_id ?? "").trim();
    if (!id) continue;
    byId.set(id, summary);
  }
  const allowedTurnIds = new Set(turns.map((turn) => idToString(turn?.turn_id ?? "")));
  return Array.from(byId.values()).filter((summary) => {
    const toolId = String(summary?.tool_call_id ?? "").trim();
    if (!toolId) return false;
    return allowedTurnIds.has(idToString(summary?.turn_id ?? ""));
  });
};
