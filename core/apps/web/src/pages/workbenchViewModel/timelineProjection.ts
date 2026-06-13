import { idToString, type SessionEvent } from "../../api/client";

export type AssistantOrderSeqLookup = {
  byProviderId: Map<string, number>;
};

export type ThoughtBlock = {
  idKey: string;
  text: string;
  orderSeq?: number;
  createdAt?: string;
  isCrp: boolean;
};

type InvariantLogger = (reason: string, details: Record<string, unknown>) => void;

const asRecord = (value: unknown): Record<string, unknown> =>
  value && typeof value === "object" ? (value as Record<string, unknown>) : {};

const pickFirstString = (...values: unknown[]): string | null => {
  for (const value of values) {
    if (typeof value === "string" && value.trim()) return value.trim();
  }
  return null;
};

const readNonEmptyString = (value: unknown): string | null => {
  if (typeof value === "string") {
    const trimmed = value.trim();
    return trimmed ? trimmed : null;
  }
  return null;
};

function isNonToolStatus(value: string): boolean {
  const status = value.trim().toLowerCase();
  return ![
    "pending",
    "queued",
    "running",
    "in_progress",
    "completed",
    "failed",
    "error",
    "ok",
    "success",
    "succeeded",
  ].includes(status);
}

function isStatusUpdateMeta(meta: unknown): boolean {
  const metaRecord = asRecord(meta);
  const codexMeta = asRecord(metaRecord.codex);
  const reasoningKind = codexMeta.reasoning_kind ?? codexMeta.reasoningKind;
  if (reasoningKind === "status") return true;

  const statusText = pickFirstString(
    metaRecord.status_text,
    metaRecord.statusText,
    metaRecord.status_string,
    metaRecord.statusString,
    codexMeta.status_text,
    codexMeta.statusText,
    codexMeta.status_string,
    codexMeta.statusString,
  );
  if (statusText) return true;

  const statusValue =
    typeof metaRecord.status === "string"
      ? metaRecord.status
      : typeof codexMeta.status === "string"
        ? codexMeta.status
        : null;
  if (statusValue && isNonToolStatus(statusValue)) return true;

  return false;
}

function shouldRenderThoughtChunk(event: SessionEvent): boolean {
  const payload = asRecord(event.payload_json);
  const meta = payload._meta ?? payload.meta;
  const metaRecord = asRecord(meta);
  if (metaRecord.heartbeat === true) return false;
  if (isStatusUpdateMeta(metaRecord)) return false;
  const codexMeta = asRecord(metaRecord.codex);
  const reasoningKind = codexMeta.reasoning_kind ?? codexMeta.reasoningKind;
  if (reasoningKind === "summary") return false;
  return true;
}

export function readEventOrderSeq(event: SessionEvent): number | null {
  const parse = (value: unknown): number | null => {
    if (typeof value === "number" && Number.isFinite(value)) return value;
    if (typeof value === "string" && value.trim()) {
      const parsed = Number.parseFloat(value);
      return Number.isFinite(parsed) ? parsed : null;
    }
    return null;
  };

  const payload = asRecord(event.payload_json);
  return parse(payload.order_seq ?? payload.orderSeq);
}

export function collectAssistantOrderSeq(events: SessionEvent[]): AssistantOrderSeqLookup {
  const byProviderId = new Map<string, number>();
  for (const event of events) {
    const payload = asRecord(event.payload_json);
    if (event.event_type === "assistant_message_inserted") {
      const orderSeq = readEventOrderSeq(event);
      if (!Number.isFinite(orderSeq)) continue;
      const providerId = readNonEmptyString(payload.provider_message_id ?? payload.providerMessageId);
      if (providerId) byProviderId.set(providerId, orderSeq as number);
      continue;
    }
    if (event.event_type === "assistant_chunk" || event.event_type === "assistant_complete") {
      const orderSeq = readEventOrderSeq(event);
      if (!Number.isFinite(orderSeq)) continue;
      const providerId = readNonEmptyString(
        payload.message_id ?? payload.messageId ?? payload.provider_message_id ?? payload.providerMessageId,
      );
      if (providerId) byProviderId.set(providerId, orderSeq as number);
    }
  }
  return { byProviderId };
}

function appendStreamingFragment(previous: string, fragment: string): string {
  const prev = previous ?? "";
  const next = fragment ?? "";
  if (!prev) return next;
  if (!next) return prev;
  if (next.startsWith(prev)) return next;
  if (prev.endsWith(next)) return prev;
  return `${prev}${next}`;
}

function readThoughtFullContent(payload: Record<string, unknown>): string | null {
  const full =
    payload.full_content ??
    payload.fullContent ??
    payload.full ??
    payload.content ??
    payload.content_fragment ??
    payload.contentFragment;
  return typeof full === "string" && full.trim() ? full : null;
}

function isFinalThoughtPayload(payload: Record<string, unknown>): boolean {
  return (
    payload.is_final === true ||
    payload.isFinal === true ||
    typeof payload.full_content === "string" ||
    typeof payload.fullContent === "string"
  );
}

function isCrpThoughtEvent(event: SessionEvent): boolean {
  if (event.event_type !== "thought_chunk") return false;
  const payload = asRecord(event.payload_json);
  return (
    payload.crp_seq != null ||
    payload.crpSeq != null ||
    payload.crp_channel != null ||
    payload.crpChannel != null
  );
}

function readThoughtBlockKey(event: SessionEvent, onInvariant?: InvariantLogger): string | null {
  const turnId = idToString(event.turn_id);
  if (!turnId) {
    onInvariant?.("thought_chunk missing turn_id", {
      event_type: event.event_type,
      event_id: idToString(event.id),
      created_at: event.created_at,
    });
    return null;
  }

  const payload = asRecord(event.payload_json);
  const rawItemId = readNonEmptyString(payload.item_id ?? payload.itemId);
  if (!rawItemId) {
    onInvariant?.("CRP thought_chunk missing payload.item_id", {
      turn_id: turnId,
      event_id: idToString(event.id),
      created_at: event.created_at,
      crp_seq: payload.crp_seq ?? payload.crpSeq ?? null,
    });
    return null;
  }

  const rawSummary = payload.summary_index ?? payload.summaryIndex;
  const parsedSummary = typeof rawSummary === "number" ? rawSummary : Number(rawSummary);
  if (!Number.isFinite(parsedSummary)) {
    onInvariant?.("CRP thought_chunk missing/invalid payload.summary_index", {
      turn_id: turnId,
      event_id: idToString(event.id),
      created_at: event.created_at,
      item_id: rawItemId,
      summary_index: rawSummary ?? null,
    });
    return null;
  }
  return `${turnId}|${rawItemId}|${parsedSummary}`;
}

function collectThoughtStream(events: SessionEvent[]): {
  text: string;
  orderSeq?: number;
  createdAt?: string;
  isCrp: boolean;
} | null {
  const thoughtEvents = events
    .filter((event) => event.event_type === "thought_chunk" && shouldRenderThoughtChunk(event))
    .map((event) => ({ event, orderSeq: readEventOrderSeq(event) }))
    .filter((entry) => Number.isFinite(entry.orderSeq));
  if (thoughtEvents.length === 0) return null;

  const sorted = thoughtEvents.slice().sort((a, b) => {
    const left = a.orderSeq as number;
    const right = b.orderSeq as number;
    if (left !== right) return left - right;
    return String(a.event.created_at).localeCompare(String(b.event.created_at));
  });

  let text = "";
  let createdAt: string | undefined;
  let orderSeq: number | undefined;
  let isCrp = false;
  let hasFinal = false;

  for (const { event, orderSeq: seq } of sorted) {
    const payload = asRecord(event.payload_json);
    const finalText = readThoughtFullContent(payload);
    if (isFinalThoughtPayload(payload) && finalText) {
      text = finalText;
      hasFinal = true;
    } else if (!hasFinal) {
      const fragment = String(payload.content_fragment ?? payload.contentFragment ?? "");
      if (fragment) {
        text = appendStreamingFragment(text, fragment);
      }
    }
    createdAt = createdAt ?? event.created_at;
    if (orderSeq === undefined) orderSeq = seq as number;
    if (isCrpThoughtEvent(event)) isCrp = true;
  }

  if (!text) return null;
  return { text, orderSeq, createdAt, isCrp };
}

export function collectThoughtBlocks(events: SessionEvent[], opts?: { onInvariant?: InvariantLogger }): ThoughtBlock[] {
  const onInvariant = opts?.onInvariant;
  const thoughtEvents = events.filter((event) => event.event_type === "thought_chunk" && shouldRenderThoughtChunk(event));
  if (thoughtEvents.length === 0) return [];

  const crpThoughtEvents = thoughtEvents.filter(isCrpThoughtEvent);
  const nonCrpThoughtEvents = thoughtEvents.filter((event) => !isCrpThoughtEvent(event));
  const blocks: ThoughtBlock[] = [];

  if (crpThoughtEvents.length > 0) {
    const groups = new Map<string, SessionEvent[]>();
    for (const event of crpThoughtEvents) {
      const orderSeq = readEventOrderSeq(event);
      if (!Number.isFinite(orderSeq)) continue;
      const key = readThoughtBlockKey(event, onInvariant);
      if (!key) continue;
      const list = groups.get(key) ?? [];
      list.push(event);
      groups.set(key, list);
    }

    for (const [key, list] of groups.entries()) {
      const sorted = list.slice().sort((a, b) => {
        const left = readEventOrderSeq(a) as number;
        const right = readEventOrderSeq(b) as number;
        if (left !== right) return left - right;
        return String(a.created_at).localeCompare(String(b.created_at));
      });

      let text = "";
      let createdAt: string | undefined;
      let orderSeq: number | undefined;
      let hasFinal = false;
      for (const event of sorted) {
        const payload = asRecord(event.payload_json);
        const finalText = readThoughtFullContent(payload);
        if (isFinalThoughtPayload(payload) && finalText) {
          text = finalText;
          hasFinal = true;
        } else if (!hasFinal) {
          const fragment = String(payload.content_fragment ?? payload.contentFragment ?? "");
          if (fragment) {
            text = appendStreamingFragment(text, fragment);
          }
        }
        createdAt = createdAt ?? event.created_at;
        if (orderSeq === undefined) orderSeq = readEventOrderSeq(event) as number;
      }
      if (text.trim() && Number.isFinite(orderSeq)) {
        blocks.push({ idKey: `crp:${key}`, text, orderSeq, createdAt, isCrp: true });
      }
    }
  }

  if (nonCrpThoughtEvents.length > 0) {
    const stream = collectThoughtStream(nonCrpThoughtEvents);
    if (stream) {
      if (!Number.isFinite(stream.orderSeq)) {
        onInvariant?.("non-CRP thought stream missing order_seq", {
          count: nonCrpThoughtEvents.length,
        });
      } else {
        blocks.push({
          idKey: `stream:seq:${stream.orderSeq as number}`,
          text: stream.text,
          orderSeq: stream.orderSeq,
          createdAt: stream.createdAt,
          isCrp: stream.isCrp,
        });
      }
    }
  }

  return blocks
    .slice()
    .sort((a, b) => {
      const left = a.orderSeq;
      const right = b.orderSeq;
      if (Number.isFinite(left) && Number.isFinite(right)) {
        if (left !== right) return (left as number) - (right as number);
        return String(a.createdAt ?? "").localeCompare(String(b.createdAt ?? ""));
      }
      if (Number.isFinite(left) && !Number.isFinite(right)) return -1;
      if (!Number.isFinite(left) && Number.isFinite(right)) return 1;
      return 0;
    });
}

export function buildCustomStatusByTurnId(events: SessionEvent[]): Map<string, string> {
  const normalize = (value: unknown): string | null => {
    const text = String(value ?? "").trim();
    return text ? text : null;
  };

  const extractReasoningSummaryText = (event: SessionEvent): string | null => {
    if (event.event_type !== "notice") return null;
    const payload = asRecord(event.payload_json);
    if (payload.kind !== "reasoning_summary") return null;
    return normalize(payload.text) ?? normalize(payload.summary) ?? normalize(payload.content);
  };

  const sorted = events
    .slice()
    .sort((a, b) => {
      const left = readEventOrderSeq(a) ?? Number.NaN;
      const right = readEventOrderSeq(b) ?? Number.NaN;
      if (Number.isFinite(left) && Number.isFinite(right) && left !== right) return left - right;
      if (Number.isFinite(left) && !Number.isFinite(right)) return -1;
      if (!Number.isFinite(left) && Number.isFinite(right)) return 1;
      return String(a.created_at).localeCompare(String(b.created_at));
    });

  const summaryByTurn = new Map<string, { order: number; text: string }>();

  let order = 0;
  for (const event of sorted) {
    order += 1;
    const turnId = idToString(event.turn_id);
    if (!turnId) continue;

    const summaryText = extractReasoningSummaryText(event);
    if (summaryText) {
      summaryByTurn.set(turnId, { order, text: summaryText });
    }
  }

  const out = new Map<string, string>();
  const allTurnIds = new Set<string>([...summaryByTurn.keys()]);
  for (const turnId of allTurnIds) {
    const summary = summaryByTurn.get(turnId);
    if (summary?.text) {
      out.set(turnId, summary.text);
    }
  }

  return out;
}
