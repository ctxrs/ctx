import type { Message, SessionTurn } from "../../api/client";
import type { AssistantStreamingState } from "../assistantStreaming";

function readFirstMessageId(items: readonly { id?: string | null }[]): string {
  return typeof items[0]?.id === "string" ? items[0]!.id! : "";
}

function readLastMessageId(items: readonly { id?: string | null }[]): string {
  return typeof items.at(-1)?.id === "string" ? items.at(-1)!.id! : "";
}

function readFirstTurnId(turns: readonly { turn_id?: string | null }[]): string {
  return typeof turns[0]?.turn_id === "string" ? turns[0]!.turn_id! : "";
}

function readLastTurnId(turns: readonly { turn_id?: string | null }[]): string {
  return typeof turns.at(-1)?.turn_id === "string" ? turns.at(-1)!.turn_id! : "";
}

export function buildAssistantStreamingStamp(
  assistantStreamingByTurnId: Record<string, AssistantStreamingState>,
  assistantStreamingRev?: number,
): string {
  return `${assistantStreamingRev ?? 0}:${Object.keys(assistantStreamingByTurnId).length}`;
}

export function buildTurnsStamp(
  turns: readonly SessionTurn[],
  turnsRev: number | undefined,
): string {
  if (turns.length === 0) return `${turnsRev ?? 0}:0`;
  return `${turnsRev ?? 0}:${turns.length}:${readFirstTurnId(turns)}:${readLastTurnId(turns)}`;
}

export function buildMessagesStamp(
  messages: readonly Message[],
  messagesRev?: number,
): string {
  return `${messagesRev ?? 0}:${messages.length}:${readFirstMessageId(messages)}:${readLastMessageId(messages)}`;
}
