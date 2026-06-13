import {
  idToString,
  type Message,
  type SessionTurn,
  type SessionTurnTool,
} from "../../api/client";
import {
  compareSessionTurnOrder,
  mergeSessionMessages,
} from "../sessionHeadState";
import {
  isTerminalTurnStatus,
  mergeTurnCount,
  mergeTurnStatus,
  normalizeTerminalTurnLiveCounts,
} from "./cachePolicy";
import type { InternalEntry } from "./entryState";
import type { SessionReplicaPatch } from "../sessionReplicaProtocol";
import { shouldPreserveExistingTranscriptWindow } from "../sessionHeadRepair";

export function haveSameArrayRefs<T>(previous: readonly T[], next: readonly T[]): boolean {
  if (previous === next) return true;
  if (previous.length !== next.length) return false;
  for (let index = 0; index < previous.length; index += 1) {
    if (previous[index] !== next[index]) return false;
  }
  return true;
}

export function haveSameRecordRefs<T>(previous: Record<string, T>, next: Record<string, T>): boolean {
  if (previous === next) return true;
  const previousKeys = Object.keys(previous);
  const nextKeys = Object.keys(next);
  if (previousKeys.length !== nextKeys.length) return false;
  for (const key of previousKeys) {
    if (previous[key] !== next[key]) return false;
  }
  return true;
}

const repairReplaceIsCoveredByEntry = (
  entry: Pick<InternalEntry, "turns" | "messages">,
  data: Pick<Exclude<SessionReplicaPatch, { op: "evict" }>["data"], "turns" | "messages">,
): boolean => {
  const incomingTurns = Array.isArray(data.turns) ? data.turns : [];
  const incomingMessages = Array.isArray(data.messages) ? data.messages : [];

  const entryTurnIds = new Set(entry.turns.map((turn) => idToString(turn.turn_id)).filter(Boolean));
  const entryMessageIds = new Set(entry.messages.map((message) => idToString(message.id)).filter(Boolean));

  return (
    incomingTurns.every((turn) => entryTurnIds.has(idToString(turn.turn_id))) &&
    incomingMessages.every((message) => entryMessageIds.has(idToString(message.id)))
  );
};

export const repairReplaceShouldPreserveEntryTranscript = (
  entry: Pick<InternalEntry, "turns" | "messages">,
  data: Pick<Exclude<SessionReplicaPatch, { op: "evict" }>["data"], "turns" | "messages" | "headWindow">,
): boolean => {
  if (repairReplaceIsCoveredByEntry(entry, data)) return true;
  return shouldPreserveExistingTranscriptWindow(entry, {
    turns: Array.isArray(data.turns) ? data.turns : [],
    messages: Array.isArray(data.messages) ? data.messages : [],
    head_window: data.headWindow ?? undefined,
  });
};

export const rebuildSeqAndStartState = (entry: InternalEntry) => {
  entry.seqSet = new Set(
    entry.events
      .map((event) => (typeof event.seq === "number" ? event.seq : Number.NaN))
      .filter((seq) => Number.isFinite(seq)) as number[],
  );
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
};

const summaryOnlyTool = (
  summary: InternalEntry["toolSummaries"][number],
): SessionTurnTool & { summary_only: boolean } => ({
  session_id: summary.session_id,
  tool_call_id: summary.tool_call_id,
  turn_id: summary.turn_id,
  tool_kind: summary.tool_kind ?? null,
  provider_tool_name: summary.provider_tool_name ?? null,
  title: summary.title ?? null,
  subtitle: summary.subtitle ?? null,
  status: summary.status ?? null,
  input_json: summary.input_preview ?? null,
  output_text: null,
  order_seq: summary.order_seq,
  input_truncated: summary.input_truncated ?? null,
  input_original_bytes: summary.input_original_bytes ?? null,
  output_truncated: summary.output_truncated ?? null,
  output_original_bytes: summary.output_original_bytes ?? null,
  first_event_seq: summary.first_event_seq ?? null,
  created_at: summary.created_at,
  updated_at: summary.updated_at,
  summary_only: true,
});

export const applyCanonicalToolSummaries = (
  entry: InternalEntry,
  summaries: InternalEntry["toolSummaries"],
  opts?: { resetByTurn?: boolean },
) => {
  const support = entry.support;
  let changed = entry.toolSummaries !== summaries;
  entry.toolSummaries = summaries;
  support.toolSummariesReady = true;
  const resetByTurn = opts?.resetByTurn === true;
  const currentByTurn = support.turnToolsByTurnId;
  let nextByTurn: Record<string, SessionTurnTool[]> = resetByTurn ? {} : currentByTurn;
  let toolsByTurnChanged = resetByTurn && Object.keys(currentByTurn).length > 0;

  for (const summary of summaries) {
    const turnId = idToString(summary.turn_id);
    if (!turnId) continue;
    if (support.turnToolsHydratedByTurnId[turnId]) continue;
    const existing = nextByTurn[turnId] ?? [];
    const key = String(summary.tool_call_id ?? "").trim();
    if (!key) continue;
    if (existing.some((tool) => String(tool.tool_call_id ?? "").trim() === key)) continue;
    if (!resetByTurn && nextByTurn === currentByTurn) {
      nextByTurn = { ...currentByTurn };
    }
    nextByTurn[turnId] = [...existing, summaryOnlyTool(summary)];
    toolsByTurnChanged = true;
    if (support.turnToolsHydratedByTurnId[turnId] === undefined) {
      support.turnToolsHydratedByTurnId[turnId] = false;
    }
  }

  if (toolsByTurnChanged) {
    support.turnToolsByTurnId = nextByTurn;
    changed = true;
  }
  return changed;
};

export const preserveLocalQueuedMessages = (
  currentMessages: Message[],
  incomingMessages: Message[],
): Message[] => {
  const incomingIds = new Set(
    incomingMessages.map((message) => idToString(message.id)).filter((id): id is string => Boolean(id)),
  );
  return currentMessages.filter((message) => {
    const messageId = idToString(message.id);
    if (!messageId || incomingIds.has(messageId)) return false;
    return message.delivery === "queued";
  });
};

export const preserveLocalUserMessageAnchors = (
  previousTurns: SessionTurn[],
  previousMessages: Message[],
  nextTurns: SessionTurn[],
  nextMessages: Message[],
  opts?: { excludedMessageIds?: ReadonlySet<string> },
): { turns: SessionTurn[]; messages: Message[] } => {
  if (previousTurns.length === 0 || previousMessages.length === 0 || nextTurns.length === 0) {
    return { turns: nextTurns, messages: nextMessages };
  }

  const previousTurnById = new Map(
    previousTurns
      .map((turn) => {
        const turnId = idToString(turn.turn_id);
        return turnId ? ([turnId, turn] as const) : null;
      })
      .filter((item): item is readonly [string, SessionTurn] => item !== null),
  );
  const previousMessageById = new Map(
    previousMessages
      .map((message) => {
        const messageId = idToString(message.id);
        return messageId ? ([messageId, message] as const) : null;
      })
      .filter((item): item is readonly [string, Message] => item !== null),
  );
  const nextMessageIds = new Set(
    nextMessages.map((message) => idToString(message.id)).filter((messageId): messageId is string => Boolean(messageId)),
  );

  let repairedTurns = nextTurns;
  let repairedMessages = nextMessages;
  const preservedMessages: Message[] = [];

  nextTurns.forEach((turn, index) => {
    const turnId = idToString(turn.turn_id);
    if (!turnId) return;
    const nextUserMessageId = idToString(turn.user_message_id ?? "");
    if (nextUserMessageId && nextMessageIds.has(nextUserMessageId)) return;

    const previousTurn = previousTurnById.get(turnId);
    const previousUserMessageId = idToString(previousTurn?.user_message_id ?? "");
    if (!previousTurn || !previousUserMessageId) return;
    if (opts?.excludedMessageIds?.has(previousUserMessageId)) return;

    const previousUserMessage = previousMessageById.get(previousUserMessageId);
    if (!previousUserMessage || previousUserMessage.role !== "user") return;
    if (idToString(previousUserMessage.turn_id ?? "") !== turnId) return;

    if (repairedTurns === nextTurns) {
      repairedTurns = nextTurns.slice();
    }
    repairedTurns[index] =
      repairedTurns[index]?.user_message_id === previousTurn.user_message_id
        ? repairedTurns[index]!
        : { ...turn, user_message_id: previousTurn.user_message_id };

    if (!nextMessageIds.has(previousUserMessageId)) {
      nextMessageIds.add(previousUserMessageId);
      preservedMessages.push(previousUserMessage);
    }
  });

  if (preservedMessages.length > 0) {
    repairedMessages = mergeSessionMessages(repairedMessages, preservedMessages);
  }

  return {
    turns: repairedTurns,
    messages: repairedMessages,
  };
};

export const preserveMonotonicTurns = (
  previousTurns: SessionTurn[],
  nextTurns: SessionTurn[],
): SessionTurn[] => {
  if (previousTurns.length === 0 || nextTurns.length === 0) return nextTurns;
  const previousById = new Map(
    previousTurns
      .map((turn) => {
        const turnId = idToString(turn.turn_id);
        return turnId ? ([turnId, turn] as const) : null;
      })
      .filter((item): item is readonly [string, SessionTurn] => item !== null),
  );
  let changed = false;
  const merged = nextTurns.map((turn) => {
    const turnId = idToString(turn.turn_id);
    if (!turnId) return turn;
    const previous = previousById.get(turnId);
    if (!previous) return normalizeTerminalTurnLiveCounts(turn);
    const nextStatus = mergeTurnStatus(previous.status, turn.status);
    const useAuthoritativeToolCounts =
      isTerminalTurnStatus(nextStatus) && isTerminalTurnStatus(turn.status);
    const nextTurn: SessionTurn = normalizeTerminalTurnLiveCounts({
      ...turn,
      status: nextStatus,
      end_seq: turn.end_seq ?? previous.end_seq,
      tool_total: useAuthoritativeToolCounts
        ? mergeTurnCount(previous.tool_total, turn.tool_total)
        : Math.max(previous.tool_total ?? 0, turn.tool_total ?? 0),
      // `tool_pending`/`tool_running` are live counters, not cumulative totals.
      // Authoritative replace/repair patches must be able to clear them.
      tool_pending: turn.tool_pending,
      tool_running: turn.tool_running,
      tool_completed: useAuthoritativeToolCounts
        ? mergeTurnCount(previous.tool_completed, turn.tool_completed)
        : Math.max(previous.tool_completed ?? 0, turn.tool_completed ?? 0),
      tool_failed: useAuthoritativeToolCounts
        ? mergeTurnCount(previous.tool_failed, turn.tool_failed)
        : Math.max(previous.tool_failed ?? 0, turn.tool_failed ?? 0),
      metrics_json: turn.metrics_json ?? previous.metrics_json,
    });
    changed =
      changed ||
      nextTurn.status !== turn.status ||
      nextTurn.end_seq !== turn.end_seq ||
      nextTurn.tool_total !== turn.tool_total ||
      nextTurn.tool_pending !== turn.tool_pending ||
      nextTurn.tool_running !== turn.tool_running ||
      nextTurn.tool_completed !== turn.tool_completed ||
      nextTurn.tool_failed !== turn.tool_failed ||
      nextTurn.metrics_json !== turn.metrics_json;
    return nextTurn;
  });
  return changed ? merged : nextTurns;
};

export const mergeStreamDeltaTurns = (
  previousTurns: SessionTurn[],
  deltaTurns: SessionTurn[],
): SessionTurn[] => {
  if (deltaTurns.length === 0) return previousTurns;
  const byId = new Map<string, SessionTurn>();
  for (const turn of previousTurns) {
    const turnId = idToString(turn.turn_id);
    if (turnId) byId.set(turnId, turn);
  }
  for (const turn of deltaTurns) {
    const turnId = idToString(turn.turn_id);
    if (turnId) byId.set(turnId, turn);
  }
  return Array.from(byId.values()).sort(compareSessionTurnOrder);
};

export const removeMessagesById = (
  messages: Message[],
  removedIds: ReadonlySet<string>,
): Message[] => {
  if (removedIds.size === 0) return messages;
  return messages.filter((message) => !removedIds.has(idToString(message.id)));
};
