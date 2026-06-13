import type { SessionEvent, SessionTurn, SessionTurnTool } from "../../api/client";
import { pickFirstString } from "./eventNormalization";

const asRecord = (value: unknown): Record<string, unknown> =>
  value && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : {};

export const isNonToolStatus = (value: string): boolean => {
  const s = value.trim().toLowerCase();
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
  ].includes(s);
};

export function isStatusUpdateMeta(meta: unknown): boolean {
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

export function shouldRenderThoughtChunk(ev: SessionEvent): boolean {
  const payload = asRecord(ev.payload_json);
  const acpUpdate = asRecord(payload.acp_update);
  const meta = asRecord(acpUpdate._meta ?? acpUpdate.meta ?? payload._meta ?? payload.meta);
  if (meta.heartbeat === true) return false;
  if (isStatusUpdateMeta(meta)) return false;
  const codexMeta = asRecord(meta.codex);
  const reasoningKind = codexMeta.reasoning_kind ?? codexMeta.reasoningKind;
  if (reasoningKind === "summary") return false;
  return true;
}

export function shouldRenderAssistantChunk(ev: SessionEvent): boolean {
  const payload = asRecord(ev.payload_json);
  const acpUpdate = asRecord(payload.acp_update);
  const meta = asRecord(acpUpdate._meta ?? acpUpdate.meta ?? payload._meta ?? payload.meta);
  if (meta.heartbeat === true) return false;
  if (isStatusUpdateMeta(meta)) return false;
  return true;
}

export const extractToolCallId = (event: SessionEvent): string | null => {
  const payload = asRecord(event.payload_json);
  const toolCall = asRecord(payload.tool_call);
  const tool = asRecord(payload.tool);
  const direct = payload.tool_call_id ?? toolCall.id ?? tool.id;
  if (typeof direct === "string" && direct.trim()) return String(direct);
  const acpUpdate = asRecord(payload.acp_update);
  const updateToolCall = asRecord(acpUpdate.tool_call);
  const fromUpdate = acpUpdate.tool_call_id ?? updateToolCall.id;
  if (typeof fromUpdate === "string" && fromUpdate.trim()) return String(fromUpdate);
  return null;
};

export const normalizeToolStatus = (raw: string, eventType: string): string => {
  const s = String(raw ?? "").toLowerCase();
  if (s === "inprogress" || s === "in_progress" || s === "running") return "in_progress";
  if (s === "pending" || s === "queued") return "pending";
  if (s === "completed" || s === "complete" || s === "ok" || s === "succeeded") return "completed";
  if (s === "failed" || s === "error") return "failed";
  if (eventType === "tool_result") return "completed";
  return s || "pending";
};

export const extractToolStatus = (event: SessionEvent): string | null => {
  const payload = asRecord(event.payload_json);
  const tool = asRecord(payload.tool);
  const direct = payload.tool_status ?? tool.status ?? payload.status;
  if (typeof direct === "string" && direct.trim()) return normalizeToolStatus(direct, String(event.event_type ?? ""));
  const acpUpdate = asRecord(payload.acp_update);
  const updateTool = asRecord(acpUpdate.tool);
  const fromUpdate = acpUpdate.tool_status ?? updateTool.status;
  if (typeof fromUpdate === "string" && fromUpdate.trim()) return normalizeToolStatus(fromUpdate, String(event.event_type ?? ""));
  if (event.event_type === "tool_result") return "completed";
  if (event.event_type === "tool_call") return "pending";
  return null;
};

export const toolStatusBucket = (status?: string | null): string | null => {
  const s = String(status ?? "").toLowerCase();
  if (s === "pending" || s === "queued") return "pending";
  if (s === "in_progress" || s === "inprogress" || s === "running") return "in_progress";
  if (s === "completed" || s === "complete" || s === "ok" || s === "succeeded") return "completed";
  if (s === "failed" || s === "error") return "failed";
  if (!s) return "pending";
  return "pending";
};

export const applyToolBucketDelta = (turn: SessionTurn, bucket: string | null, delta: number) => {
  if (!bucket || delta === 0) return;
  switch (bucket) {
    case "pending":
      turn.tool_pending = Math.max(0, (turn.tool_pending ?? 0) + delta);
      break;
    case "in_progress":
      turn.tool_running = Math.max(0, (turn.tool_running ?? 0) + delta);
      break;
    case "completed":
      turn.tool_completed = Math.max(0, (turn.tool_completed ?? 0) + delta);
      break;
    case "failed":
      turn.tool_failed = Math.max(0, (turn.tool_failed ?? 0) + delta);
      break;
    default:
      break;
  }
};

export const readTurnStatusFromPayload = (event: SessionEvent): SessionTurn["status"] | null => {
  const payload = asRecord(event.payload_json);
  const raw = payload.status;
  if (typeof raw !== "string") return null;
  const status = raw.trim();
  switch (status) {
    case "queued":
    case "starting":
    case "running":
    case "completed":
    case "interrupted":
    case "failed":
      return status;
    case "error":
      return "failed";
    default:
      return null;
  }
};

export const deriveTurnStatusFromEvent = (event: SessionEvent): SessionTurn["status"] => {
  const payloadStatus = readTurnStatusFromPayload(event);
  if (payloadStatus) return payloadStatus;
  const eventType = String(event.event_type ?? "");
  switch (eventType) {
    case "done":
      return "completed";
    case "turn_finished":
      return "completed";
    case "turn_queued":
      return "queued";
    case "turn_started":
      return "running";
    case "turn_interrupted":
      return "interrupted";
    case "error":
      return "failed";
    default:
      return "running";
  }
};

const TOOL_INPUT_PREVIEW_KEYS = [
  "command",
  "query",
  "pattern",
  "regex",
  "text",
  "path",
  "file",
  "filename",
  "file_path",
  "filePath",
  "filepath",
  "target",
  "paths",
  "paths_total",
  "files",
  "file_paths",
  "filePaths",
  "glob",
  "parsed_cmd",
  "diff_stats",
  "url",
  "uri",
  "href",
  "method",
  "server",
  "tool",
  "tool_name",
  "toolName",
  "cwd",
  "root",
];

export const toolInputPreview = (input: unknown): Record<string, unknown> | null => {
  if (!input || typeof input !== "object" || Array.isArray(input)) return null;
  const obj = input as Record<string, unknown>;
  const out: Record<string, unknown> = {};
  for (const key of TOOL_INPUT_PREVIEW_KEYS) {
    if (obj[key] !== undefined) out[key] = obj[key];
  }
  return Object.keys(out).length > 0 ? out : null;
};

export const summarizeToolPayload = (
  tool: SessionTurnTool,
): SessionTurnTool & { summary_only: boolean } => ({
  ...tool,
  input_json: toolInputPreview(tool.input_json) ?? null,
  output_text: null,
  input_truncated: tool.input_truncated ?? null,
  input_original_bytes: tool.input_original_bytes ?? null,
  output_truncated: tool.output_truncated ?? null,
  output_original_bytes: tool.output_original_bytes ?? null,
  summary_only: true,
});
