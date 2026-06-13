import {
  idToString,
  recordClientCounterMetric,
  type Message,
  type SessionEvent,
  type SessionTurn,
  type SessionTurnTool,
} from "../../api/client";
import type {
  AskUserQuestionAnswerState,
  ThreadItem,
  WorkbenchThreadView,
  WorkbenchTurnHeader,
} from "../sessionView/SessionPage.types";
import type { AssistantStreamingState } from "../../state/assistantStreaming";
import { humanToolKind, isPlaceholderToolLabel, normalizeDisplayToolLabel, toolDisplayTitleFromPayload } from "../sessionView/SessionPage.helpers";
import {
  buildPendingTurns,
  filterQueuedMessagesForPanel,
  filterTurnsForQueuedMessages,
  mergeMessagesForView,
  mergeQueuedMessagesForPanel,
} from "./messageMerge";
import {
  deriveAuthUi,
  deriveProviderGuardNotice,
  deriveSessionError,
  extractErrorMessage,
  type AuthUi,
  type ProviderGuardNotice,
  type SessionErrorInfo,
} from "./authDerivations";
import {
  buildCustomStatusByTurnId,
  collectAssistantOrderSeq,
  readEventOrderSeq,
} from "./timelineProjection";
import type { SessionViewVerbosity } from "../../state/uiStateStore";
import { collectAskUserQuestionAnswers } from "./askUserQuestions";
import { mergeGroupsWithSystemMessages } from "./systemMessageGroups";
import { buildTurnActivityTimeline } from "./toolTimeline";

export {
  buildPendingTurns,
  filterQueuedMessagesForPanel,
  filterTurnsForQueuedMessages,
  mergeMessagesForView,
  mergeQueuedMessagesForPanel,
};
export { deriveAuthUi, deriveProviderGuardNotice, deriveSessionError };
export { collectAskUserQuestionAnswers } from "./askUserQuestions";
export { normalizeContextWindowMetrics } from "./contextWindow";
export { deriveMessagesKey, deriveTurnsKey } from "./messageKeys";

const devInvariantLogKeys = new Set<string>();
const telemetryInvariantKeys = new Set<string>();

const asRecord = (value: unknown): Record<string, unknown> =>
  value && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : {};

function recordThreadInvariantCounter(reason: string, details: Record<string, string> = {}): void {
  const key = `${reason}:${JSON.stringify(details)}`;
  if (telemetryInvariantKeys.has(key)) return;
  if (telemetryInvariantKeys.size > 500) telemetryInvariantKeys.clear();
  telemetryInvariantKeys.add(key);
  recordClientCounterMetric("workbench.thread.contract_violation_count", {
    reason,
    ...details,
  });
}

function logViewModelInvariant(reason: string, details: Record<string, unknown>): void {
  if (!import.meta.env.DEV) return;
  const key = `${reason}:${JSON.stringify(details)}`;
  if (devInvariantLogKeys.has(key)) return;
  if (devInvariantLogKeys.size > 500) devInvariantLogKeys.clear();
  devInvariantLogKeys.add(key);
  // eslint-disable-next-line no-console
  console.error("[WorkbenchThreadViewModel][contract-violation]", { reason, ...details });
}

function isTerminalTurnStatus(
  status: SessionTurn["status"] | null | undefined,
): status is Extract<SessionTurn["status"], "completed" | "failed" | "interrupted"> {
  return status === "completed" || status === "failed" || status === "interrupted";
}

function readMessageOrderSeq(message: unknown): number {
  const record = asRecord(message);
  const raw = record.order_seq ?? record.turn_sequence;
  return Number(raw ?? Number.NaN);
}

export function filterThreadItemsForVerbosity(items: ThreadItem[], verbosity: SessionViewVerbosity): ThreadItem[] {
  if (verbosity === "terse") {
    return items.filter((item) => item.kind !== "tool" && item.kind !== "tool_group" && item.kind !== "thought");
  }
  return items;
}

type SortableThreadGroup = {
  sort_seq: number;
  group: WorkbenchThreadView["groups"][number];
};

export function buildWorkbenchThreadViewModel(
  turns: SessionTurn[],
  messages: Message[],
  toolsByTurnId: Record<string, SessionTurnTool[]>,
  events: SessionEvent[],
  assistantStreamingOrAnswers:
    | Record<string, AssistantStreamingState>
    | Map<string, AskUserQuestionAnswerState> = {},
  askUserQuestionAnswers?: Map<string, AskUserQuestionAnswerState>,
): WorkbenchThreadView {
  const assistantStreamingByTurnId =
    assistantStreamingOrAnswers instanceof Map ? {} : assistantStreamingOrAnswers;
  const answers =
    assistantStreamingOrAnswers instanceof Map
      ? assistantStreamingOrAnswers
      : askUserQuestionAnswers ??
        collectAskUserQuestionAnswers(events, {});
  if (turns.length > 0) {
    return buildWorkbenchThreadViewModelFromTurns(
      turns,
      messages,
      toolsByTurnId,
      events,
      assistantStreamingByTurnId,
      answers,
    );
  }
  recordThreadInvariantCounter("managed_no_turns");
  return { groups: mergeGroupsWithSystemMessages([], messages), debugEvents: [] };
}

export function buildWorkbenchThreadViewModelFromTurns(
  turns: SessionTurn[],
  messages: Message[],
  toolsByTurnId: Record<string, SessionTurnTool[]>,
  events: SessionEvent[],
  assistantStreamingOrAnswers:
    | Record<string, AssistantStreamingState>
    | Map<string, AskUserQuestionAnswerState> = {},
  askUserQuestionAnswers: Map<string, AskUserQuestionAnswerState> = new Map(),
): WorkbenchThreadView {
  const assistantStreamingByTurnId =
    assistantStreamingOrAnswers instanceof Map ? {} : assistantStreamingOrAnswers;
  const answers =
    assistantStreamingOrAnswers instanceof Map
      ? assistantStreamingOrAnswers
      : askUserQuestionAnswers;
  const debugEvents: SessionEvent[] = [];
  const groups: SortableThreadGroup[] = [];
  const customStatusByTurnId = buildCustomStatusByTurnId(events);
  const sortedTurns = turns.slice().sort((a, b) => {
    const aSeq = Number(a.start_seq ?? Number.NaN);
    const bSeq = Number(b.start_seq ?? Number.NaN);
    if (Number.isFinite(aSeq) && Number.isFinite(bSeq) && aSeq !== bSeq) return aSeq - bSeq;
    if (Number.isFinite(aSeq) && !Number.isFinite(bSeq)) return -1;
    if (!Number.isFinite(aSeq) && Number.isFinite(bSeq)) return 1;
    const aEnd = Number(a.end_seq ?? Number.NaN);
    const bEnd = Number(b.end_seq ?? Number.NaN);
    if (Number.isFinite(aEnd) && Number.isFinite(bEnd) && aEnd !== bEnd) return aEnd - bEnd;
    if (Number.isFinite(aEnd) && !Number.isFinite(bEnd)) return -1;
    if (!Number.isFinite(aEnd) && Number.isFinite(bEnd)) return 1;
    const aStart = String(a.started_at ?? "");
    const bStart = String(b.started_at ?? "");
    if (aStart !== bStart) return aStart.localeCompare(bStart);
    const aId = idToString(a.turn_id) ?? "";
    const bId = idToString(b.turn_id) ?? "";
    return aId.localeCompare(bId);
  });

  const messageById = new Map<string, Message>();
  const messagesByTurnId = new Map<string, Message[]>();
  for (const m of messages) {
    const mid = idToString(m.id);
    if (mid) messageById.set(mid, m);
    const turnId = idToString(m.turn_id);
    if (turnId) {
      const list = messagesByTurnId.get(turnId) ?? [];
      list.push(m);
      messagesByTurnId.set(turnId, list);
    }
  }

  const eventsByTurnId = new Map<string, SessionEvent[]>();
  for (const ev of events) {
    const turnId = idToString(ev.turn_id);
    if (!turnId) continue;
    const list = eventsByTurnId.get(turnId) ?? [];
    list.push(ev);
    eventsByTurnId.set(turnId, list);
  }

  for (const turn of sortedTurns) {
    const turnId = idToString(turn.turn_id);
    if (!turnId) {
      if (import.meta.env.DEV) {
        // eslint-disable-next-line no-console
        console.error("[WorkbenchThreadViewModel] turn missing turn_id", {
          started_at: turn.started_at ?? null,
          updated_at: turn.updated_at ?? null,
        });
      }
      continue;
    }
    const userMessageId = turn.user_message_id ? idToString(turn.user_message_id) : "";

    const userMessage = userMessageId ? messageById.get(userMessageId) : undefined;
    if (userMessageId && !userMessage) {
      recordThreadInvariantCounter("missing_user_message_anchor", { turn_id: turnId });
    }
    const headerId = turnId;

    const header: WorkbenchTurnHeader | null = userMessage
      ? {
        id: headerId,
        content: userMessage.content ?? "",
        content_revision: userMessage.id,
        attachments: Array.isArray(userMessage.attachments)
          ? userMessage.attachments
          : [],
        created_at: userMessage.created_at,
      }
      : null;
    const headerOrderSeq = readMessageOrderSeq(userMessage);

    let tools = (toolsByTurnId[turnId] ?? []).map((tool) => {
      const toolKind = String(tool.tool_kind ?? "tool");
      const title = String(tool.title ?? humanToolKind(toolKind));
      const summaryOnly = asRecord(tool).summary_only === true;
      const hasDetails =
        !summaryOnly && (tool.input_json != null || String(tool.output_text ?? "").trim().length > 0);
      return {
        kind: "tool",
        id: `tool-${turnId}-${tool.tool_call_id}`,
        tool_call_id: tool.tool_call_id,
        created_at: tool.created_at,
        updated_at: tool.updated_at ?? tool.created_at,
        tool_kind: toolKind,
        provider_tool_name: String(tool.provider_tool_name ?? ""),
        title,
        subtitle: String(tool.subtitle ?? ""),
        status: String(tool.status ?? "pending"),
        locations: [],
        input: tool.input_json ?? null,
        output_text: String(tool.output_text ?? ""),
        raw: tool,
        updates_seen: 1,
        has_details: hasDetails,
      } satisfies Extract<ThreadItem, { kind: "tool" }>;
    });

    const eventsForTurn = eventsByTurnId.get(turnId) ?? [];
    const { activity } = buildTurnActivityTimeline({
      turnId,
      turn,
      tools,
      events: eventsForTurn,
      askUserQuestionAnswers: answers,
      onInvariant: logViewModelInvariant,
    });
    const assistantOrderSeq = collectAssistantOrderSeq(eventsForTurn);

    const assistantMessages = (messagesByTurnId.get(turnId) ?? [])
      .filter((m) => m.role === "assistant")
      .slice()
      .sort((a, b) => {
        const sa = readMessageOrderSeq(a);
        const sb = readMessageOrderSeq(b);
        if (Number.isFinite(sa) && Number.isFinite(sb) && sa !== sb) return sa - sb;
        if (Number.isFinite(sa) && !Number.isFinite(sb)) return -1;
        if (!Number.isFinite(sa) && Number.isFinite(sb)) return 1;
        return String(a.created_at).localeCompare(String(b.created_at));
      });

    type TimelineEntry = {
      item: ThreadItem;
      created_at: string;
      kind: "assistant" | "tool" | "thought" | "ask_user_question" | "message";
      order_seq?: number;
      turn_sequence?: number;
    };

    const timeline: TimelineEntry[] = [];
    for (const m of assistantMessages) {
      const orderSeq = readMessageOrderSeq(m);
      if (!Number.isFinite(orderSeq)) continue;
      const messageId = idToString(m.id);
      if (!messageId) {
        // Missing message ids break MessageList identity invariants; treat this as a bug.
        if (import.meta.env.DEV) {
          // eslint-disable-next-line no-console
          console.error("[WorkbenchThreadViewModel] assistant message missing id", {
            turnId,
            created_at: m.created_at,
            order_seq: asRecord(m).order_seq ?? null,
            turn_sequence: m.turn_sequence ?? null,
          });
        }
        continue;
      }
      timeline.push({
        item: {
          kind: "assistant",
          // Use the message id so item identity is stable even if sequencing metadata is backfilled later.
          id: `assistant-msg-${messageId}`,
          turn_id: turnId,
          created_at: m.created_at,
          content: m.content ?? "",
          thought: "",
          is_complete: true,
        },
        created_at: m.created_at,
        kind: "assistant",
        turn_sequence: orderSeq as number,
        order_seq: orderSeq as number,
      });
    }

    const statusText =
      turn.status === "running" || turn.status === "starting" || turn.status === "queued"
        ? customStatusByTurnId.get(turnId) ?? null
        : null;
    const pendingState = assistantStreamingByTurnId[turnId] ?? null;
    const pendingContent = String(pendingState?.content ?? "");
    const pendingTrimmed = pendingContent.trim();
    const statusTrimmed = statusText?.trim() ?? "";
    const pendingProviderId = pendingState?.providerMessageId ?? null;
    const persistedAssistantDuplicate =
      pendingTrimmed.length > 0
        ? assistantMessages.find((message) => String(message.content ?? "").trim() === pendingTrimmed) ?? null
        : null;
    if (persistedAssistantDuplicate) {
      const reason = isTerminalTurnStatus(turn.status)
        ? "stale_pending_after_terminal_assistant_message"
        : "stale_pending_duplicate_assistant_message";
      recordThreadInvariantCounter(reason, { turn_status: String(turn.status ?? "") || "unknown" });
      logViewModelInvariant(reason, {
        turnId,
        turnStatus: turn.status ?? null,
        pendingProviderId,
        pendingLength: pendingTrimmed.length,
        messageId: idToString(persistedAssistantDuplicate.id),
      });
    }
    if (
      pendingTrimmed.length > 0 &&
      pendingTrimmed !== statusTrimmed &&
      !persistedAssistantDuplicate
    ) {
      const pendingOrderSeq = Number.isFinite(pendingState?.orderSeq)
        ? pendingState?.orderSeq
        : pendingProviderId
          ? assistantOrderSeq.byProviderId.get(pendingProviderId)
          : undefined;
      if (!Number.isFinite(pendingOrderSeq)) {
        // Skip rendering partials without order_seq to avoid fallback ordering.
      } else {
      const pendingCreatedAt = turn.started_at ?? turn.updated_at;
      timeline.push({
        item: {
          kind: "assistant",
          id: `assistant-${turnId}-pending`,
          turn_id: turnId,
          created_at: pendingCreatedAt,
          content: pendingContent,
          thought: "",
          is_complete: false,
        },
        created_at: pendingCreatedAt,
        kind: "assistant",
        turn_sequence: Number.MAX_SAFE_INTEGER,
        order_seq: pendingOrderSeq as number,
      });
      }
    }

    for (const entry of activity) {
      timeline.push({
        item: entry.item,
        created_at: entry.created_at,
        kind: entry.kind,
        order_seq: entry.order_seq,
      });
    }

    timeline.sort((a, b) => {
      const aSeq = a.order_seq;
      const bSeq = b.order_seq;
      if (Number.isFinite(aSeq) && Number.isFinite(bSeq)) {
        if (aSeq !== bSeq) return (aSeq as number) - (bSeq as number);
        const tcmp = String(a.created_at).localeCompare(String(b.created_at));
        if (tcmp !== 0) return tcmp;
      } else if (Number.isFinite(aSeq) && !Number.isFinite(bSeq)) {
        return -1;
      } else if (!Number.isFinite(aSeq) && Number.isFinite(bSeq)) {
        return 1;
      }
      if (a.kind === "assistant" && b.kind === "assistant") {
        const sa = Number(a.turn_sequence ?? Number.NaN);
        const sb = Number(b.turn_sequence ?? Number.NaN);
        if (Number.isFinite(sa) && Number.isFinite(sb) && sa !== sb) return sa - sb;
      }
      const aRank = a.kind === "assistant" ? 2 : 1;
      const bRank = b.kind === "assistant" ? 2 : 1;
      if (aRank !== bRank) return aRank - bRank;
      return String(a.item.id).localeCompare(String(b.item.id));
    });

    const items: ThreadItem[] = timeline.map((entry) => entry.item);

    if (items.length === 0) {
      items.push({ kind: "spacer", id: `spacer-${turnId}`, created_at: turn.started_at });
    }

    const assistantMessagesContent = assistantMessages
      .map((m) => m.content ?? "")
      .filter((c) => c.trim().length > 0)
      .join("\n\n");
    items.push({
      kind: "turn_status",
      id: `turn-status-${turnId}`,
      turn_id: turnId,
      created_at: turn.updated_at ?? turn.started_at,
      status: turn.status,
      started_at: turn.started_at,
      updated_at: turn.updated_at ?? turn.started_at,
      custom_status: statusText ?? undefined,
      assistant_messages_content: assistantMessagesContent,
    });

    const timelineOrderSeq = timeline
      .map((entry) => entry.order_seq)
      .filter((seq): seq is number => Number.isFinite(seq));
    const turnStartOrderSeq = Number(turn.start_seq ?? Number.NaN);
    const turnEndOrderSeq = Number(turn.end_seq ?? Number.NaN);
    const groupOrderSeq = Number.isFinite(headerOrderSeq)
      ? (headerOrderSeq as number)
      : Number.isFinite(turnStartOrderSeq)
        ? turnStartOrderSeq
      : timelineOrderSeq.length > 0
        ? Math.min(...timelineOrderSeq)
        : Number.isFinite(turnEndOrderSeq)
          ? turnEndOrderSeq
        : Number.NaN;
    if (!Number.isFinite(groupOrderSeq)) {
      recordThreadInvariantCounter("missing_order_seq_anchor", { turn_id: turnId });
      if (import.meta.env.DEV) {
        // eslint-disable-next-line no-console
        console.error("[WorkbenchThreadViewModel] turn missing order_seq anchor", {
          turn_id: turnId,
          user_message_id: userMessageId || null,
          status: turn.status ?? null,
        });
      }
      continue;
    }
    groups.push({
      sort_seq: groupOrderSeq as number,
      group: { key: `turn-${turnId}`, header, items },
    });
  }

  return { groups: mergeGroupsWithSystemMessages(groups, messages), debugEvents };
}
