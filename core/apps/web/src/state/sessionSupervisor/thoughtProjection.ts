import type { SessionEvent } from "../../api/client";
import { idToString } from "../../api/client";
import { pickFirstString } from "./eventNormalization";

const asRecord = (value: unknown): Record<string, unknown> | null => {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  return value as Record<string, unknown>;
};

export const readThoughtFullContent = (payload: unknown): string | null => {
  const record = asRecord(payload);
  if (!record) return null;
  return pickFirstString(
    record.full_content,
    record.fullContent,
    record.full,
    record.content,
    record.content_fragment,
    record.contentFragment,
  );
};

export const isFinalThoughtPayload = (payload: unknown): boolean => {
  const record = asRecord(payload);
  if (!record) return false;
  return (
    record.is_final === true ||
    record.isFinal === true ||
    typeof record.full_content === "string" ||
    typeof record.fullContent === "string"
  );
};

export const isFinalThoughtEvent = (event: SessionEvent | null | undefined): boolean => {
  if (!event) return false;
  if (String(event.event_type ?? "") !== "thought_chunk") return false;
  const payload = event.payload_json ?? {};
  return isFinalThoughtPayload(payload);
};

export const normalizeFinalThoughtPayload = (payload: unknown): Record<string, unknown> => {
  const record = asRecord(payload) ?? {};
  const full = readThoughtFullContent(payload);
  if (!full) return record;
  return {
    ...record,
    full_content: full,
    is_final: true,
  };
};

export const buildThoughtCacheKey = (event: SessionEvent): string | null => {
  const turnId = idToString(event.turn_id);
  if (!turnId) return null;
  const payload = asRecord(event.payload_json) ?? {};
  const itemId = pickFirstString(payload.item_id, payload.itemId);
  const rawSummary = payload.summary_index ?? payload.summaryIndex;
  const parsedSummary = typeof rawSummary === "number" ? rawSummary : Number(rawSummary);
  const summaryIndex = Number.isFinite(parsedSummary) ? parsedSummary : 0;
  const fallback = `unknown-${payload.order_seq ?? payload.orderSeq ?? idToString(event.id) ?? "missing"}`;
  return `${turnId}|${itemId ?? fallback}|${summaryIndex}`;
};
