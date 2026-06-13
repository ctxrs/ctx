import type { Message, Session, SessionEvent } from "../../api/client";
import { pickFirstString, readPayloadString } from "./eventNormalization";

export type AcpMeta = {
  models?: unknown;
  modes?: unknown;
  currentModelId?: string;
  commands?: unknown;
  slashCommands?: unknown;
};

export const asRecord = (value: unknown): Record<string, unknown> =>
  value && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : {};

export const readPayloadObject = (
  payload: SessionEvent["payload_json"],
  key: string,
): Record<string, unknown> | null => {
  const value = asRecord(payload)[key];
  return value && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : null;
};

export const readPayloadNumber = (payload: unknown, keys: string[]): number | null => {
  const record = asRecord(payload);
  for (const key of keys) {
    const value = record[key];
    if (typeof value === "number" && Number.isFinite(value)) return value;
    if (typeof value === "string" && value.trim()) {
      const parsed = Number.parseFloat(value);
      if (Number.isFinite(parsed)) return parsed;
    }
  }
  return null;
};

export const messageFromEvent = (
  event: SessionEvent,
  session: Session | null | undefined,
): Message | null => {
  const role =
    event.event_type === "user_message"
      ? "user"
      : event.event_type === "assistant_message_inserted"
        ? "assistant"
        : null;
  if (!role) return null;
  const payload = asRecord(event.payload_json);
  const messageId = readPayloadString(payload, ["message_id", "messageId"]);
  const content = readPayloadString(payload, ["content"]);
  if (!messageId || !content) return null;
  const delivery = readPayloadString(payload, ["delivery"]) === "queued" ? "queued" : "immediate";
  const attachments = Array.isArray(payload.attachments) ? payload.attachments : [];
  const message: Message = {
    id: messageId,
    session_id: event.session_id,
    task_id: session?.task_id ?? "",
    turn_id: event.turn_id ?? null,
    turn_sequence: readPayloadNumber(payload, ["turn_sequence", "turnSequence"]),
    role,
    content,
    attachments,
    delivery,
    created_at: event.created_at ?? new Date().toISOString(),
  };
  const orderSeq = readPayloadNumber(payload, ["order_seq", "orderSeq"]);
  if (orderSeq !== null) {
    (message as Message & { order_seq?: number }).order_seq = orderSeq;
  }
  return message;
};

export const readAcpCurrentModelId = (models: unknown): string | undefined => {
  const record = asRecord(models);
  if (!record) return;
  const modelId = record.currentModelId ?? record.current_model_id;
  return typeof modelId === "string" ? modelId : undefined;
};

export const hasModelList = (models: unknown): boolean => {
  const record = asRecord(models);
  if (!record) return false;
  const list =
    record.availableModels ??
    record.available_models ??
    record.models ??
    [];
  return Array.isArray(list) && list.length > 0;
};

export const extractAcpMetaFromEvent = (event: SessionEvent): AcpMeta | null => {
  if (event.event_type !== "init") return null;
  const payload = asRecord(event.payload_json);
  if (!payload) return null;
  const models = payload.models ?? undefined;
  const modes = payload.modes ?? undefined;
  const commands = payload.commands ?? undefined;
  const slashCommands = payload.slashCommands ?? payload.slash_commands ?? undefined;
  const currentModelId =
    pickFirstString(payload.currentModelId, payload.current_model_id) ?? readAcpCurrentModelId(models);
  if (!models && !modes && !commands && !slashCommands && !currentModelId) return null;
  return {
    models,
    modes,
    currentModelId,
    commands,
    slashCommands,
  };
};
