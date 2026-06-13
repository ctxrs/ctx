import type { WorkbenchListItem } from "./SessionPage.types";
import { humanTurnStatus } from "./sessionView/SessionPage.helpers";
import { SESSION_TRANSCRIPT_LAYOUT_ENGINE_REVISION } from "./sessionThread/sessionMarkdownMeasurement";
import {
  getWorkbenchMessageLayoutState,
  getWorkbenchTurnHeaderLayoutState,
  resolveWorkbenchMessageExpandedFromContent,
} from "./sessionThread/transcriptRowLayoutModel";

export type WorkbenchMessageListUiState = {
  expandedTurnHeaders: Record<string, boolean>;
  expandedTurnDetailsById: Record<string, boolean>;
  expandedToolById: Record<string, boolean>;
  expandedMessageById: Record<string, boolean>;
  turnToolsLoading: readonly string[];
  verbosity?: string;
};

type HeightRevisionOptions = {
  verbosity?: string;
};

type LayoutRevisionOptions = {
  verbosity?: string;
  toolExpansionIds?: readonly string[];
};

function fingerprintAttachmentLayout(
  attachments: ReadonlyArray<{
    kind?: string;
    name?: string | null;
    mime_type?: string | null;
  }>,
): string {
  return fingerprintUnknown(
    attachments.map((attachment) => ({
      kind: attachment.kind ?? "",
      name: attachment.name ?? "",
      mimeType: attachment.mime_type ?? "",
    })),
  );
}

function stableTrueKeys(record: Record<string, boolean>): string[] {
  return Object.entries(record)
    .filter(([, value]) => value)
    .map(([key]) => key)
    .sort();
}

function fingerprintString(value: string): string {
  const normalized = String(value ?? "");
  let hash = 2166136261;
  for (let index = 0; index < normalized.length; index += 1) {
    hash ^= normalized.charCodeAt(index);
    hash = Math.imul(hash, 16777619);
  }
  return `${normalized.length}:${(hash >>> 0).toString(36)}`;
}

function fingerprintUnknown(value: unknown): string {
  try {
    return fingerprintString(JSON.stringify(value) ?? "");
  } catch {
    return fingerprintString(String(value ?? ""));
  }
}

function withTranscriptLayoutEngineRevision(revision: string): string {
  return `engine:${SESSION_TRANSCRIPT_LAYOUT_ENGINE_REVISION}:${revision}`;
}

export function getWorkbenchMessageListLayoutRevision(
  uiState: WorkbenchMessageListUiState,
  options?: LayoutRevisionOptions,
): string {
  const relevantExpandedTools =
    options?.toolExpansionIds == null
      ? stableTrueKeys(uiState.expandedToolById)
      : stableTrueKeys(uiState.expandedToolById).filter((toolId) =>
          options.toolExpansionIds?.includes(toolId),
        );
  return JSON.stringify({
    layoutEngineRevision: SESSION_TRANSCRIPT_LAYOUT_ENGINE_REVISION,
    verbosity: options?.verbosity ?? uiState.verbosity ?? null,
    turnHeaders: stableTrueKeys(uiState.expandedTurnHeaders),
    turnDetails: stableTrueKeys(uiState.expandedTurnDetailsById),
    tools: relevantExpandedTools,
    messages: stableTrueKeys(uiState.expandedMessageById),
    turnToolsLoading: [...uiState.turnToolsLoading].sort(),
  });
}

export function collectWorkbenchToolGroupExpansionIds(
  listItems: readonly WorkbenchListItem[],
): string[] {
  return listItems
    .flatMap((item) => (item.kind === "tool_group" ? item.tools.map((tool) => tool.id) : []))
    .sort();
}

export function resolveWorkbenchMessageExpanded(
  item: Extract<WorkbenchListItem, { kind: "message" }>,
  expandedMessageById: Record<string, boolean>,
): boolean {
  return resolveWorkbenchMessageExpandedFromContent(item, expandedMessageById);
}

export function resolveWorkbenchTurnHeaderExpanded(
  item: Extract<WorkbenchListItem, { kind: "turn_header" }>,
  expandedTurnHeaders: Record<string, boolean>,
): boolean {
  return getWorkbenchTurnHeaderLayoutState(item, expandedTurnHeaders).expanded;
}

function toolGroupChildExpansionKey(
  item: Extract<WorkbenchListItem, { kind: "tool_group" }>,
  expandedToolById: Record<string, boolean>,
): string {
  if (item.tools.length === 0) return "none";
  return item.tools
    .map((tool) => `${tool.id}:${expandedToolById[tool.id] ? "open" : "closed"}`)
    .join("|");
}

function isMutableToolStatus(status: string): boolean {
  const normalized = String(status ?? "").trim().toLowerCase();
  return (
    normalized.includes("running") ||
    normalized.includes("pending") ||
    normalized.includes("queued") ||
    normalized.includes("starting")
  );
}

function isMutableTurnStatus(status: Extract<WorkbenchListItem, { kind: "turn_status" }>["status"]): boolean {
  return status === "running" || status === "starting" || status === "queued";
}

function getTurnStatusHeightRevision(item: Extract<WorkbenchListItem, { kind: "turn_status" }>): string {
  const customStatus = item.custom_status?.trim() ?? "";
  const statusLabel = isMutableTurnStatus(item.status) && customStatus ? customStatus : humanTurnStatus(item.status);
  const showCopyButton =
    item.status === "completed" && Boolean(item.assistant_messages_content?.trim());
  return `turn-status:${fingerprintString(statusLabel)}:${showCopyButton ? "copy" : "nocopy"}`;
}

export function getWorkbenchListItemHeightRevision(
  item: WorkbenchListItem,
  uiState: WorkbenchMessageListUiState,
  options?: HeightRevisionOptions,
): string {
  void options;
  switch (item.kind) {
    case "message":
      {
        const layout = getWorkbenchMessageLayoutState(item, uiState.expandedMessageById);
        const contentRevision = fingerprintString(layout.shownContent);
        const attachmentRevision = fingerprintAttachmentLayout(item.attachments);
        if (!layout.expandable) {
          return withTranscriptLayoutEngineRevision(
            `message:fixed:${layout.renderMode}:${contentRevision}:${attachmentRevision}`,
          );
        }
        return withTranscriptLayoutEngineRevision(
          layout.expanded
            ? `message:expanded:${layout.renderMode}:${contentRevision}:${attachmentRevision}`
            : `message:collapsed:${layout.renderMode}:${contentRevision}:${attachmentRevision}`,
        );
      }
    case "turn_header":
      {
        const layout = getWorkbenchTurnHeaderLayoutState(item, uiState.expandedTurnHeaders);
        const attachmentRevision = fingerprintAttachmentLayout(item.header.attachments);
        if (!layout.expandable) {
          return withTranscriptLayoutEngineRevision(
            `turn-header:fixed:${layout.contentRevision}:${attachmentRevision}`,
          );
        }
        return withTranscriptLayoutEngineRevision(
          layout.expanded
            ? `turn-header:expanded:${layout.contentRevision}:${attachmentRevision}`
            : `turn-header:collapsed:${layout.contentRevision}:attachments:hidden`,
        );
      }
    case "tool":
      return withTranscriptLayoutEngineRevision(
        [
          "tool:summary",
          item.status,
          fingerprintString(item.title),
          fingerprintString(item.subtitle ?? ""),
          fingerprintUnknown(item.locations),
          fingerprintUnknown(item.input),
        ].join(":"),
      );
    case "tool_group": {
      const expanded = uiState.expandedTurnDetailsById[item.turn_id] ?? false;
      if (!expanded) return withTranscriptLayoutEngineRevision("tool-group:collapsed");
      const loading =
        item.tools.length === 0 && uiState.turnToolsLoading.includes(item.turn_id) ? "loading" : "ready";
      return withTranscriptLayoutEngineRevision(
        `tool-group:expanded:${loading}:${fingerprintString(item.thought)}:${toolGroupChildExpansionKey(item, uiState.expandedToolById)}`,
      );
    }
    case "assistant":
      return withTranscriptLayoutEngineRevision(`assistant:fixed:${fingerprintString(item.content)}`);
    case "thought":
      return withTranscriptLayoutEngineRevision(`thought:${fingerprintString(item.content)}`);
    case "turn_status":
      return withTranscriptLayoutEngineRevision(getTurnStatusHeightRevision(item));
    case "ask_user_question":
      return withTranscriptLayoutEngineRevision(
        [
          "ask-user-question",
          item.answered ? "answered" : "pending",
          item.outcome ?? "none",
          fingerprintUnknown(item.input),
          fingerprintUnknown(item.answers ?? null),
        ].join(":"),
      );
    default:
      return withTranscriptLayoutEngineRevision("fixed");
  }
}

export function getWorkbenchListItemSizeCacheKey(
  item: WorkbenchListItem,
  uiState: WorkbenchMessageListUiState,
  options?: HeightRevisionOptions,
): string | null {
  switch (item.kind) {
    case "assistant":
      return item.is_complete ? getWorkbenchListItemHeightRevision(item, uiState, options) : null;
    case "turn_status":
      return isMutableTurnStatus(item.status) ? null : getWorkbenchListItemHeightRevision(item, uiState, options);
    case "tool":
      return isMutableToolStatus(item.status) ? null : getWorkbenchListItemHeightRevision(item, uiState, options);
    case "tool_group":
      return item.tool_pending === 0 && item.tool_running === 0
        ? getWorkbenchListItemHeightRevision(item, uiState, options)
        : null;
    case "ask_user_question":
      return item.answered ? getWorkbenchListItemHeightRevision(item, uiState, options) : null;
    case "message":
    case "turn_header":
    case "thought":
    case "spacer":
      return getWorkbenchListItemHeightRevision(item, uiState, options);
    default:
      return null;
  }
}

export function getWorkbenchListItemKey(
  item: WorkbenchListItem,
  uiState: WorkbenchMessageListUiState,
  options?: HeightRevisionOptions,
): string {
  return `${item.id}:${getWorkbenchListItemHeightRevision(item, uiState, options)}`;
}
