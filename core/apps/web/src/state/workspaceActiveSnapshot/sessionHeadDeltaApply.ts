import type { Message, SessionHeadDelta, SessionHeadSnapshot, SessionTurn } from "@ctx/types";
import { idToString } from "../../api/client";
import {
  compactActiveSessionHeadSnapshot,
  mergeSessionEvents,
  mergeSessionMessages,
  mergeSessionToolSummaries,
  mergeSessionTurns,
} from "../sessionHeadState";
import { createSeedHeadSnapshot } from "./sessionHeadSeed";
import type { WorkspaceActiveSnapshotItem } from "./storeTypes";
import { shouldReplaceSessionHead } from "./summaryHelpers";

export function applySessionHeadDeltaToSnapshot(params: {
  delta: SessionHeadDelta;
  tasks: Map<string, WorkspaceActiveSnapshotItem>;
  sessionHeadsById: Map<string, SessionHeadSnapshot>;
  shouldRetainSessionHead?: (sessionId: string) => boolean;
}): boolean {
  const { delta, tasks, sessionHeadsById, shouldRetainSessionHead } = params;
  const sessionId = idToString(delta?.session_id ?? "");
  if (!sessionId) return false;
  if (shouldRetainSessionHead && !shouldRetainSessionHead(sessionId)) {
    return false;
  }
  let existing = sessionHeadsById.get(sessionId);
  if (!existing) {
    const seeded = createSeedHeadSnapshot(tasks, sessionId);
    if (!seeded) return false;
    existing = seeded;
    sessionHeadsById.set(sessionId, seeded);
  }
  let changed = false;
  let turns = existing.turns ?? [];
  let toolSummaries = Array.isArray(existing.tool_summaries) ? existing.tool_summaries : [];
  let messages = existing.messages ?? [];
  let events = existing.events ?? [];
  const ensureTurnForMessage = (
    message: Message,
  ): { turn: SessionTurn | null; message: Message } => {
    const existingTurnIds = new Set(
      turns.map((turn) => idToString(turn.turn_id)).filter((turnId) => turnId.length > 0),
    );
    const existingMessageTurnId = idToString(message.turn_id ?? "");
    if (existingMessageTurnId && existingTurnIds.has(existingMessageTurnId)) {
      return { turn: null, message };
    }
    const syntheticTurnId = existingMessageTurnId || `synthetic-turn:${idToString(message.id)}`;
    const timestamp = message.created_at ?? existing.session.updated_at ?? existing.session.created_at;
    const sequence =
      typeof message.order_seq === "number"
        ? message.order_seq
        : typeof message.turn_sequence === "number"
          ? message.turn_sequence
          : typeof delta.last_event_seq === "number"
            ? delta.last_event_seq
            : 0;
    return {
      turn: {
        turn_id: syntheticTurnId,
        session_id: idToString(message.session_id ?? existing.session.id),
        run_id: null,
        user_message_id: null,
        status: "queued",
        start_seq: sequence,
        end_seq: sequence,
        started_at: timestamp,
        updated_at: timestamp,
        assistant_partial: null,
        thought_partial: null,
        metrics_json: null,
        tool_total: 0,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 0,
        tool_failed: 0,
      },
      message: {
        ...message,
        turn_id: syntheticTurnId,
      },
    };
  };
  if (delta.turn) {
    turns = mergeSessionTurns(turns, [delta.turn]);
    changed = true;
  }
  if (delta.message) {
    const normalized = ensureTurnForMessage(delta.message);
    if (normalized.turn) {
      turns = mergeSessionTurns(turns, [normalized.turn]);
    }
    messages = mergeSessionMessages(messages, [normalized.message]);
    changed = true;
  }
  if (delta.event) {
    events = mergeSessionEvents(events, [delta.event]);
    changed = true;
  }
  const incomingToolSummaries = Array.isArray(delta.tool_summaries) ? delta.tool_summaries : [];
  if (incomingToolSummaries.length > 0) {
    toolSummaries = mergeSessionToolSummaries(toolSummaries, incomingToolSummaries, turns);
    changed = true;
  }
  const next: SessionHeadSnapshot = compactActiveSessionHeadSnapshot({
    ...existing,
    turns,
    tool_summaries: toolSummaries,
    messages,
    events,
    ...(delta.session ? { session: delta.session } : {}),
    ...("activity" in delta ? { activity: delta.activity ?? undefined } : {}),
    ...(typeof delta.last_event_seq === "number" ? { last_event_seq: delta.last_event_seq } : {}),
    ...(typeof delta.projection_rev === "number" ? { projection_rev: delta.projection_rev } : {}),
    ...(typeof delta.state_rev === "number" ? { state_rev: delta.state_rev } : {}),
  });

  if (
    !changed &&
    next.last_event_seq === existing.last_event_seq &&
    (next.projection_rev ?? 0) === (existing.projection_rev ?? 0)
  ) {
    return false;
  }
  if (!shouldReplaceSessionHead(existing, next)) return false;
  sessionHeadsById.set(sessionId, next);
  return true;
}
