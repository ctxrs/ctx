import type { Message, SessionTurn } from "../../api/client";
import type { AssistantStreamingState } from "../../state/assistantStreaming";

// Runtime thread invalidation now uses explicit supervisor/view stamps.
// These hash helpers remain as a narrow test/debug seam.
const HASH_SEED = 5381;

function hashString(hash: number, value: string | null | undefined): number {
  const text = value ?? "";
  for (let i = 0; i < text.length; i += 1) {
    hash = ((hash << 5) + hash) ^ text.charCodeAt(i);
  }
  return hash;
}

function hashNumber(hash: number, value: number | null | undefined): number {
  if (!Number.isFinite(value ?? Number.NaN)) return hashString(hash, "");
  return hashString(hash, String(value));
}

function hashUnknownRecord(hash: number, value: Record<string, unknown> | null | undefined): number {
  if (!value) return hashString(hash, "");
  const keys = Object.keys(value).sort();
  for (const key of keys) {
    hash = hashString(hash, key);
    const inner = value[key];
    if (inner == null) {
      hash = hashString(hash, "");
      continue;
    }
    if (typeof inner === "string") {
      hash = hashString(hash, inner);
      continue;
    }
    if (typeof inner === "number") {
      hash = hashNumber(hash, inner);
      continue;
    }
    if (typeof inner === "boolean") {
      hash = hashString(hash, inner ? "1" : "0");
      continue;
    }
    hash = hashString(hash, JSON.stringify(inner));
  }
  return hash;
}

function finalizeHash(length: number, hash: number): string {
  return `${length}:${(hash >>> 0).toString(36)}`;
}

export function deriveMessagesKey(messages: Message[]): string {
  if (messages.length === 0) return "0";
  let hash = HASH_SEED;
  for (const message of messages) {
    hash = hashString(hash, String(message.id ?? ""));
    hash = hashString(hash, String(message.turn_id ?? ""));
    hash = hashNumber(hash, Number(message.turn_sequence ?? Number.NaN));
    hash = hashString(hash, String(message.role ?? ""));
    hash = hashString(hash, String(message.delivery ?? ""));
    hash = hashString(hash, String(message.created_at ?? ""));
    hash = hashString(hash, String(message.content ?? ""));
    const attachments = Array.isArray(message.attachments) ? message.attachments : [];
    hash = hashNumber(hash, attachments.length);
    for (const attachment of attachments) {
      const record = (attachment ?? {}) as Record<string, unknown>;
      hash = hashString(hash, typeof record.kind === "string" ? record.kind : "");
      hash = hashString(hash, typeof record.mime_type === "string" ? record.mime_type : "");
      hash = hashString(hash, typeof record.name === "string" ? record.name : "");
      hash = hashString(hash, typeof record.blob_id === "string" ? record.blob_id : "");
      hash = hashString(hash, typeof record.data_base64 === "string" ? record.data_base64 : "");
    }
  }
  return finalizeHash(messages.length, hash);
}

export function deriveTurnsKey(turns: SessionTurn[]): string {
  if (turns.length === 0) return "0";
  let hash = HASH_SEED;
  for (const turn of turns) {
    hash = hashString(hash, String(turn.turn_id ?? ""));
    hash = hashString(hash, String(turn.user_message_id ?? ""));
    hash = hashString(hash, String(turn.status ?? ""));
    hash = hashNumber(hash, Number(turn.start_seq ?? Number.NaN));
    hash = hashNumber(hash, Number(turn.end_seq ?? Number.NaN));
    hash = hashString(hash, String(turn.started_at ?? ""));
    hash = hashString(hash, String(turn.updated_at ?? ""));
    hash = hashString(hash, String(turn.thought_partial ?? ""));
    hash = hashNumber(hash, turn.tool_total);
    hash = hashNumber(hash, turn.tool_pending);
    hash = hashNumber(hash, turn.tool_running);
    hash = hashNumber(hash, turn.tool_completed);
    hash = hashNumber(hash, turn.tool_failed);
    hash = hashUnknownRecord(hash, turn.metrics_json ?? null);
  }
  return finalizeHash(turns.length, hash);
}

export function deriveAssistantStreamingKey(
  assistantStreamingByTurnId: Record<string, AssistantStreamingState>,
): string {
  const entries = Object.entries(assistantStreamingByTurnId);
  if (entries.length === 0) return "0";
  let hash = HASH_SEED;
  entries.sort(([a], [b]) => a.localeCompare(b));
  for (const [turnId, state] of entries) {
    hash = hashString(hash, turnId);
    hash = hashString(hash, state.content);
    hash = hashString(hash, state.providerMessageId ?? "");
    hash = hashNumber(hash, state.orderSeq ?? -1);
  }
  return finalizeHash(entries.length, hash);
}
