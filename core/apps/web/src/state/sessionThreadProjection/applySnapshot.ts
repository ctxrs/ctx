import type { Message, SessionEvent, SessionTurn, SessionTurnTool } from "../../api/client";
import { deriveSessionThreadEventsStamp } from "./applyEvents";
import {
  buildAssistantStreamingStamp,
  buildMessagesStamp,
  buildTurnsStamp,
} from "./stamps";
import type { SessionThreadProjection } from "./types";
import type { AssistantStreamingState } from "../assistantStreaming";

type SessionThreadProjectionSource = {
  stateLoaded?: boolean;
  turns?: SessionTurn[];
  turnsRev?: number;
  assistantStreamingByTurnId?: Record<string, AssistantStreamingState>;
  assistantStreamingRev?: number;
  messages?: Message[];
  messagesRev?: number;
  events?: SessionEvent[];
  eventsRev?: number;
  turnToolsByTurnId?: Record<string, SessionTurnTool[]>;
  toolSummariesReady?: boolean;
  projectionRev?: number;
};

export function buildSessionThreadProjectionFromSnapshot(
  source: SessionThreadProjectionSource,
): SessionThreadProjection {
  const turns = source.turns ?? [];
  const assistantStreamingByTurnId = source.assistantStreamingByTurnId ?? {};
  const assistantStreamingStamp = buildAssistantStreamingStamp(
    assistantStreamingByTurnId,
    source.assistantStreamingRev,
  );
  const messages = source.messages ?? [];
  const events = source.events ?? [];
  return {
    loaded: Boolean(source.stateLoaded),
    turns,
    turnsStamp: buildTurnsStamp(turns, source.turnsRev),
    assistantStreamingByTurnId,
    assistantStreamingStamp,
    messages,
    messagesStamp: buildMessagesStamp(messages, source.messagesRev),
    events,
    eventsStamp: deriveSessionThreadEventsStamp(events, source.eventsRev),
    toolsByTurnId: source.turnToolsByTurnId ?? {},
    toolSummariesReady: Boolean(source.toolSummariesReady),
    projectionRev: source.projectionRev ?? 0,
  };
}
