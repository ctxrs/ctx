import { idToString, type SessionEvent, type SessionTurn } from "../../api/client";
import type { AskUserQuestionAnswerState, ThreadItem } from "../sessionView/SessionPage.types";
import {
  humanToolKind,
  isPlaceholderToolLabel,
  normalizeDisplayToolLabel,
  toolDisplayTitleFromPayload,
} from "../sessionView/SessionPage.helpers";
import { collectThoughtBlocks, readEventOrderSeq } from "./timelineProjection";

type InvariantLogger = (reason: string, details: Record<string, unknown>) => void;

type ActivityEntry = {
  item: ThreadItem;
  created_at: string;
  kind: "tool" | "thought" | "ask_user_question" | "message";
  order_seq?: number;
};

type ToolItem = Extract<ThreadItem, { kind: "tool" }>;
type ToolLocation = ToolItem["locations"][number];

const asRecord = (value: unknown): Record<string, unknown> =>
  value && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : {};

function resolveToolUpdateRecord(payload: Record<string, unknown>): Record<string, unknown> {
  const nested = asRecord(payload.update);
  return Object.keys(nested).length > 0 ? nested : payload;
}

function readToolCallId(payload: Record<string, unknown>, update: Record<string, unknown>): string {
  const rawInputRecord = asRecord(update.rawInput);
  const rawInputLegacyRecord = asRecord(update.raw_input);
  const toolCall = asRecord(update.toolCall);
  const toolCallRawInput = asRecord(toolCall.rawInput);
  return String(
    payload.tool_call_id ??
      update.toolCallId ??
      update.tool_call_id ??
      rawInputRecord.call_id ??
      rawInputLegacyRecord.call_id ??
      toolCallRawInput.call_id ??
      "",
  ).trim();
}

function normalizeToolLocations(locations: unknown): ToolLocation[] {
  const locs = Array.isArray(locations) ? locations : [];
  const out: ToolLocation[] = [];
  for (const loc of locs) {
    const locRecord = asRecord(loc);
    const pathValue = locRecord.path;
    const rangeValue = locRecord.range;
    if (typeof pathValue !== "string" && rangeValue === undefined) continue;
    out.push({
      path: typeof pathValue === "string" ? pathValue : undefined,
      range: rangeValue,
    });
  }
  return out;
}

function ensureToolItem(
  toolById: Map<string, ToolItem>,
  turnId: string,
  toolCallId: string,
  createdAt: string,
) {
  const existing = toolById.get(toolCallId);
  if (existing) return existing;
  const tool: ToolItem = {
    kind: "tool",
    id: `tool-${turnId}-${toolCallId}`,
    tool_call_id: toolCallId,
    created_at: createdAt,
    updated_at: createdAt,
    tool_kind: "tool",
    provider_tool_name: "",
    title: "Tool",
    subtitle: "",
    status: "pending",
    locations: [],
    input: null,
    output_text: "",
    raw: null,
    updates_seen: 0,
    has_details: true,
  };
  toolById.set(toolCallId, tool);
  return tool;
}

function applyToolUpdateFromEvent(tool: ToolItem, ev: SessionEvent, update: unknown) {
  const updateRecord = asRecord(update);
  const toolCall = asRecord(updateRecord.toolCall);
  const rawInput = updateRecord.rawInput;
  const toolCallRawInput = toolCall.rawInput;
  tool.updated_at = ev.created_at;
  tool.updates_seen += 1;
  tool.raw = ev.payload_json ?? tool.raw;

  const nextKind = String(updateRecord.kind ?? toolCall?.kind ?? "").trim();
  if (nextKind) tool.tool_kind = nextKind;
  const nextProviderToolName = String(
    updateRecord.tool_name ?? updateRecord.toolName ?? updateRecord.name ?? toolCall?.name ?? "",
  ).trim();
  if (nextProviderToolName) tool.provider_tool_name = nextProviderToolName;

  const nextTitle = toolDisplayTitleFromPayload(updateRecord);
  if (nextTitle) tool.title = nextTitle;
  else if (tool.tool_kind && tool.title === "Tool") tool.title = humanToolKind(tool.tool_kind);
  const nextSubtitle = String(updateRecord.subtitle ?? "").trim();
  if (nextSubtitle) tool.subtitle = nextSubtitle;

  const nextStatus = String(updateRecord.status ?? toolCall?.status ?? "").trim();
  if (nextStatus) tool.status = normalizeToolStatus(nextStatus, ev.event_type);
  else if (ev.event_type === "tool_result") tool.status = "completed";

  tool.locations = normalizeToolLocations(updateRecord.locations);

  const input =
    rawInput ?? toolCallRawInput ?? toolCall.input ?? updateRecord.input ?? updateRecord.input_preview ?? null;
  if (input != null) tool.input = input;

  const nextOutput = extractToolOutputText(update);
  if (nextOutput) tool.output_text = mergeStreamingText(tool.output_text, nextOutput);
  if (tool.input != null || tool.output_text.trim().length > 0) {
    tool.has_details = true;
  }
}

function buildAskUserQuestionItem(
  ev: SessionEvent,
  turnId: string,
  answersByToolCallId: Map<string, AskUserQuestionAnswerState>,
): Extract<ThreadItem, { kind: "ask_user_question" }> | null {
  if (ev.event_type !== "notice") return null;
  const payload = ev.payload_json ?? {};
  if (payload.kind !== "ask_user_question") return null;
  const toolCallId = String(payload.tool_call_id ?? "").trim();
  if (!toolCallId) return null;
  const answerState = answersByToolCallId.get(toolCallId);
  return {
    kind: "ask_user_question",
    id: `askq-${turnId}-${toolCallId}`,
    turn_id: turnId,
    created_at: ev.created_at,
    tool_call_id: toolCallId,
    input: payload.input ?? payload.input_json ?? payload,
    answers: answerState?.answers,
    outcome: answerState?.outcome,
    answered: Boolean(answerState),
  };
}

function buildNoticeMessageItem(
  ev: SessionEvent,
  turnId: string,
): Extract<ThreadItem, { kind: "message" }> | null {
  if (ev.event_type !== "notice") return null;
  const payload = ev.payload_json ?? {};
  const code = String(payload?.kind ?? payload?.code ?? "").trim().toLowerCase();
  const explicitTimelineMessage = payload?.display_in_timeline === true;
  if (!explicitTimelineMessage && code !== "context.compacted" && code !== "context_compacted") {
    return null;
  }
  const unknownToolNotice = code === "crp_unknown_event" ? buildUnknownToolNoticeMessage(payload) : null;
  const message = explicitTimelineMessage
    ? (
      unknownToolNotice ??
      pickFirstString(payload?.message, payload?.text, payload?.summary, payload?.content) ??
      (() => {
        const originalType = pickFirstString(payload?.original_type, payload?.originalType);
        return originalType ? `Unknown runtime event: ${originalType}` : "Unknown runtime event.";
      })()
    )
    : (
      pickFirstString(payload?.message, payload?.text, payload?.summary, payload?.content) ??
      "Context compacted. Earlier turns were summarized."
    );
  const eventId = idToString(ev.id);
  if (!eventId) {
    return null;
  }
  return {
    kind: "message",
    id: `notice-${turnId}-${eventId}`,
    role: "system",
    content: message,
    attachments: [],
    created_at: ev.created_at,
  };
}

function buildUnknownToolNoticeMessage(payload: Record<string, unknown>): string | null {
  const raw = asRecord(payload.raw);
  const toolName = normalizeDisplayToolLabel(
    toolDisplayTitleFromPayload(payload) || toolDisplayTitleFromPayload(raw),
  );
  if (!toolName || isPlaceholderToolLabel(toolName)) return null;
  const preview = pickFirstString(
    payload.tool_preview,
    payload.toolPreview,
    raw.command,
    raw.description,
    raw.file_path,
    raw.filePath,
    raw.path,
    raw.query,
    raw.pattern,
    raw.regex,
    raw.message,
    raw.text,
    raw.summary,
    raw.title,
  );
  if (preview && preview !== toolName) {
    return `Unknown tool event: ${toolName} · ${preview}`;
  }
  return `Unknown tool event: ${toolName}`;
}

export function buildTurnActivityTimeline(opts: {
  turnId: string;
  turn: SessionTurn;
  tools: ToolItem[];
  events: SessionEvent[];
  askUserQuestionAnswers: Map<string, AskUserQuestionAnswerState>;
  onInvariant?: InvariantLogger;
}): { activity: ActivityEntry[] } {
  const toolById = new Map<string, ToolItem>();
  for (const tool of opts.tools) {
    toolById.set(tool.tool_call_id, tool);
  }

  const thoughtBlocks = collectThoughtBlocks(opts.events, { onInvariant: opts.onInvariant });

  const activity: ActivityEntry[] = [];
  const toolInserted = new Set<string>();
  const askInserted = new Set<string>();

  for (const ev of opts.events) {
    if (ev.event_type === "notice") {
      const orderSeq = readEventOrderSeq(ev);
      if (!Number.isFinite(orderSeq)) continue;
      const askItem = buildAskUserQuestionItem(ev, opts.turnId, opts.askUserQuestionAnswers);
      if (askItem && !askInserted.has(askItem.tool_call_id)) {
        activity.push({
          item: askItem,
          created_at: ev.created_at,
          kind: "ask_user_question",
          order_seq: orderSeq as number,
        });
        askInserted.add(askItem.tool_call_id);
      }
      const noticeItem = buildNoticeMessageItem(ev, opts.turnId);
      if (noticeItem) {
        activity.push({
          item: noticeItem,
          created_at: noticeItem.created_at,
          kind: "message",
          order_seq: orderSeq as number,
        });
      }
    }

    if (ev.event_type === "tool_call" || ev.event_type === "tool_call_update" || ev.event_type === "tool_result") {
      const orderSeq = readEventOrderSeq(ev);
      if (!Number.isFinite(orderSeq)) continue;
      const payload = asRecord(ev.payload_json);
      const update = resolveToolUpdateRecord(payload);
      const toolCallId = readToolCallId(payload, update);
      if (!toolCallId) continue;
      const tool = ensureToolItem(toolById, opts.turnId, toolCallId, ev.created_at);
      applyToolUpdateFromEvent(tool, ev, update);
      if (!toolInserted.has(toolCallId)) {
        activity.push({
          item: tool,
          created_at: tool.created_at,
          kind: "tool",
          order_seq: orderSeq as number,
        });
        toolInserted.add(toolCallId);
      }
    }
  }

  if (thoughtBlocks.length > 0) {
    thoughtBlocks.forEach((block) => {
      if (!Number.isFinite(block.orderSeq)) return;
      const thoughtText = block.text ?? "";
      if (!thoughtText.trim()) return;
      if (!block.idKey) {
        opts.onInvariant?.("thought block missing idKey", {
          turn_id: opts.turnId,
          order_seq: block.orderSeq ?? null,
        });
        return;
      }
      const thoughtItem: Extract<ThreadItem, { kind: "thought" }> = {
        kind: "thought",
        id: `thought-${opts.turnId}-${block.idKey}`,
        turn_id: opts.turnId,
        created_at: block.createdAt ?? opts.turn.updated_at ?? opts.turn.started_at,
        content: thoughtText,
      };
      activity.push({
        item: thoughtItem,
        created_at: thoughtItem.created_at,
        kind: "thought",
        order_seq: block.orderSeq,
      });
    });
  }

  for (const tool of toolById.values()) {
    if (toolInserted.has(tool.tool_call_id)) continue;
    const raw = asRecord(tool.raw);
    const orderSeq = Number(raw.order_seq ?? Number.NaN);
    if (!Number.isFinite(orderSeq)) continue;
    activity.push({
      item: tool,
      created_at: tool.created_at,
      kind: "tool",
      order_seq: orderSeq as number,
    });
  }

  return { activity };
}

function extractToolOutputText(update: unknown): string {
  const updateRecord = asRecord(update);
  const rawOutput = asRecord(updateRecord.rawOutput);
  const toolCall = asRecord(updateRecord.toolCall);
  const toolCallRawOutput = asRecord(toolCall?.rawOutput);
  const direct =
    updateRecord.outputText ??
    updateRecord.output_text ??
    updateRecord.output_preview ??
    updateRecord.result ??
    rawOutput?.aggregated_output ??
    rawOutput?.output ??
    toolCall?.outputText ??
    toolCall?.output_text ??
    toolCallRawOutput?.aggregated_output ??
    toolCallRawOutput?.output ??
    null;
  if (typeof direct === "string" && direct.trim()) return direct.trim();

  const blocks = Array.isArray(updateRecord.content) ? updateRecord.content : [];
  const parts: string[] = [];
  for (const block of blocks) {
    const blockRecord = asRecord(block);
    const contentRecord = asRecord(blockRecord?.content ?? block);
    const text = contentRecord?.text;
    if (typeof text === "string") parts.push(text);
  }
  return parts.join("").trim();
}

function mergeStreamingText(prev: string, next: string): string {
  const previous = prev ?? "";
  const incoming = next ?? "";
  if (!previous) return incoming;
  if (!incoming) return previous;
  if (incoming.startsWith(previous)) return incoming;
  if (previous.startsWith(incoming)) return previous;
  return incoming.length >= previous.length ? incoming : previous;
}

function pickFirstString(...values: unknown[]): string | null {
  for (const value of values) {
    if (typeof value === "string" && value.trim()) return value.trim();
  }
  return null;
}

function normalizeToolStatus(status: string, eventType: string): string {
  const value = status.toLowerCase();
  if (value === "inprogress" || value === "in_progress" || value === "running") return "in_progress";
  if (value === "pending" || value === "queued") return "pending";
  if (value === "completed" || value === "complete" || value === "ok" || value === "succeeded") {
    return "completed";
  }
  if (value === "failed" || value === "error") return "failed";
  if (eventType === "tool_result") return "completed";
  return value || "pending";
}
