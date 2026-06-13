import type { WorkbenchListItem } from "./SessionPage.types";

type RenderedItemContractViolation = {
  kind: string;
  reason: string;
  id: string;
  details?: Record<string, unknown>;
};

const asRecord = (value: unknown): Record<string, unknown> => {
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  return value as Record<string, unknown>;
};

export function debugStableKey(item: WorkbenchListItem): string {
  // Best-effort "identity" key independent of `item.id` to detect id churn.
  // This is DEV-only diagnostics; collisions are possible but still useful.
  const rec = asRecord(item);
  const kind = String(rec.kind ?? "unknown");
  const header = asRecord(rec.header);
  switch (kind) {
    case "turn_header":
      return `turn_header:${String(header.id ?? "")}`;
    case "tool":
      return `tool:${String(rec.tool_call_id ?? "")}`;
    case "ask_user_question":
      return `askq:${String(rec.tool_call_id ?? "")}`;
    case "turn_status":
      return `turn_status:${String(rec.turn_id ?? "")}`;
    case "thought":
      return `thought:${String(rec.turn_id ?? "")}:${String(rec.created_at ?? "")}`;
    case "assistant":
      // Prefer turn_id + created_at, but also include the item id prefix if it encodes a domain id.
      return `assistant:${String(rec.turn_id ?? "")}:${String(rec.created_at ?? "")}:${String(
        rec.is_complete ?? "",
      )}:${String(rec.id ?? "").slice(0, 40)}`;
    case "message":
      return `message:${String(rec.role ?? "")}:${String(rec.created_at ?? "")}`;
    case "spacer":
      return `spacer:${String(rec.created_at ?? "")}`;
    default:
      return `${kind}:${String(rec.created_at ?? "")}`;
  }
}

export function debugItemSummary(item: WorkbenchListItem | { id: string }): Record<string, unknown> {
  const rec = asRecord(item);
  const header = asRecord(rec.header);
  const kind = String(rec.kind ?? "unknown");
  const base: Record<string, unknown> = {
    id: String(rec.id ?? ""),
    kind,
    created_at: rec.created_at ?? header.created_at ?? null,
  };

  if (kind === "turn_header") {
    base.turn_id = header.id ?? null;
    return base;
  }
  if (typeof rec.turn_id === "string") base.turn_id = rec.turn_id;
  if (typeof rec.tool_call_id === "string") base.tool_call_id = rec.tool_call_id;
  if (typeof rec.event_id === "string") base.event_id = rec.event_id;
  if (typeof rec.status === "string") base.status = rec.status;
  if (typeof rec.role === "string") base.role = rec.role;
  if (typeof rec.is_complete === "boolean") base.is_complete = rec.is_complete;
  if (typeof rec.content === "string") base.content_len = rec.content.length;
  return base;
}

export function findFirstRenderedItemContractViolation(
  items: WorkbenchListItem[],
): RenderedItemContractViolation | null {
  // The whole point is to avoid "fallback ids" like `ts:` and `idx:` which cause identity churn.
  // These invariants are intentionally strict; if they fire, it's a bug we should fix upstream.
  for (const it of items) {
    const anyIt = asRecord(it);
    const kind = String(anyIt.kind ?? "unknown");
    const id = String(anyIt.id ?? "");
    if (!id) return { kind, reason: "missing id", id: "" };

    if (kind === "thought" && (id.includes("ts:") || id.includes("idx:") || id.includes("unknown-"))) {
      return { kind, reason: "fallback thought id (ts/idx/unknown)", id };
    }
    if (kind === "turn_header") {
      const turnId = String(asRecord(anyIt.header).id ?? "");
      if (!turnId) return { kind, reason: "turn_header missing header.id", id };
    }
    if (kind === "tool" || kind === "ask_user_question") {
      const toolCallId = String(anyIt.tool_call_id ?? "");
      if (!toolCallId) return { kind, reason: "missing tool_call_id", id };
    }
    if (kind === "assistant") {
      const turnId = String(anyIt.turn_id ?? "");
      if (!turnId) return { kind, reason: "assistant missing turn_id", id };
      const isPending = id.endsWith("-pending");
      if (!isPending && !id.startsWith("assistant-msg-")) {
        return { kind, reason: "assistant id does not encode message id", id, details: { turn_id: turnId } };
      }
    }
  }
  return null;
}
