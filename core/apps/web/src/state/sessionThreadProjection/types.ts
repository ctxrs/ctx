import type { Message, SessionEvent, SessionTurn, SessionTurnTool } from "../../api/client";
import type { AssistantStreamingState } from "../assistantStreaming";

export type SessionThreadProjection = {
  loaded: boolean;
  turns: SessionTurn[];
  turnsStamp: string;
  assistantStreamingByTurnId: Record<string, AssistantStreamingState>;
  assistantStreamingStamp: string;
  messages: Message[];
  messagesStamp: string;
  events: SessionEvent[];
  eventsStamp: string;
  toolsByTurnId: Record<string, SessionTurnTool[]>;
  toolSummariesReady: boolean;
  projectionRev: number;
};

export const EMPTY_SESSION_THREAD_PROJECTION: SessionThreadProjection = {
  loaded: false,
  turns: [],
  turnsStamp: "0:0",
  assistantStreamingByTurnId: {},
  assistantStreamingStamp: "0:0",
  messages: [],
  messagesStamp: "0:0",
  events: [],
  eventsStamp: "0:0:0",
  toolsByTurnId: {},
  toolSummariesReady: false,
  projectionRev: 0,
};
