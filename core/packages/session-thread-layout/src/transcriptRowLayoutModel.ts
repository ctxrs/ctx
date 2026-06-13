import type { WorkbenchListItem, WorkbenchTurnHeader } from "./transcriptTypes";
import { markdownToPlainText } from "./markdownPlainText";
import { normalizeTurnHeaderPlainText } from "./transcriptPlainText";

const MESSAGE_COLLAPSE_LINE_THRESHOLD = 20;
const MESSAGE_COLLAPSE_CHAR_THRESHOLD = 1500;
const MESSAGE_EXPANDED_PLAIN_TEXT_CHAR_THRESHOLD = 24_000;
const TURN_HEADER_COLLAPSE_LINE_THRESHOLD = 1;
const TURN_HEADER_COLLAPSE_CHAR_THRESHOLD = 140;
const MESSAGE_COLLAPSE_CACHE_LIMIT = 1000;
const TURN_HEADER_TEXT_CACHE_LIMIT = 1000;
const TURN_HEADER_PREVIEW_SOURCE_CHAR_LIMIT = 2048;
const TURN_HEADER_COLLAPSED_PREVIEW_LINE_LIMIT = 4;

export type WorkbenchMessageCollapseState = {
  collapsedContent: string;
  canCollapse: boolean;
  isExpandable: boolean;
};

export type WorkbenchMessageRenderMode = "markdown" | "plain_text";

export type WorkbenchTurnHeaderTextState = {
  contentRevision: string;
  collapsedPlainText: string;
  expandable: boolean;
  expandedPlainText: string | null;
};

export type WorkbenchListItemExpansionState = {
  expandedTurnHeaders: Record<string, boolean>;
  expandedMessageById: Record<string, boolean>;
};

const messageCollapseStateCache = new Map<string, WorkbenchMessageCollapseState>();
const turnHeaderTextStateCache = new Map<string, WorkbenchTurnHeaderTextState>();

function pruneMessageCollapseStateCache(): void {
  while (messageCollapseStateCache.size > MESSAGE_COLLAPSE_CACHE_LIMIT) {
    const oldestKey = messageCollapseStateCache.keys().next().value;
    if (typeof oldestKey !== "string") break;
    messageCollapseStateCache.delete(oldestKey);
  }
}

function pruneTurnHeaderTextStateCache(): void {
  while (turnHeaderTextStateCache.size > TURN_HEADER_TEXT_CACHE_LIMIT) {
    const oldestKey = turnHeaderTextStateCache.keys().next().value;
    if (typeof oldestKey !== "string") break;
    turnHeaderTextStateCache.delete(oldestKey);
  }
}

export function getWorkbenchMessageCollapseState(content: string): WorkbenchMessageCollapseState {
  const normalized = String(content ?? "");
  const cached = messageCollapseStateCache.get(normalized);
  if (cached) {
    return cached;
  }

  let newlineCount = 0;
  let collapseEnd = normalized.length;
  for (let index = 0; index < normalized.length; index += 1) {
    if (normalized.charCodeAt(index) !== 10) continue;
    newlineCount += 1;
    if (newlineCount === MESSAGE_COLLAPSE_LINE_THRESHOLD) {
      collapseEnd = index;
      break;
    }
  }

  const canCollapse = collapseEnd < normalized.length;
  const nextState: WorkbenchMessageCollapseState = {
    collapsedContent: canCollapse ? normalized.slice(0, collapseEnd) : normalized,
    canCollapse,
    isExpandable: canCollapse || normalized.length > MESSAGE_COLLAPSE_CHAR_THRESHOLD,
  };
  messageCollapseStateCache.set(normalized, nextState);
  pruneMessageCollapseStateCache();
  return nextState;
}

export function getCollapsedMessageContent(content: string): string {
  return getWorkbenchMessageCollapseState(content).collapsedContent;
}

export function isExpandableMessageContent(content: string): boolean {
  return getWorkbenchMessageCollapseState(content).isExpandable;
}

export function canCollapseMessageContent(content: string): boolean {
  return getWorkbenchMessageCollapseState(content).canCollapse;
}

export function isExpandableTurnHeaderPlainText(plainText: string): boolean {
  const normalized = String(plainText ?? "");
  if (normalized.length > TURN_HEADER_COLLAPSE_CHAR_THRESHOLD) {
    return true;
  }
  for (let index = 0; index < normalized.length; index += 1) {
    if (normalized.charCodeAt(index) === 10) {
      return true;
    }
  }
  return false;
}

function getTurnHeaderTextCacheKey(header: WorkbenchTurnHeader): string {
  if (typeof header.content_revision === "string" && header.content_revision.length > 0) {
    return `revision:${header.content_revision}`;
  }
  const explicitPlainText = typeof header.plain_text === "string" ? header.plain_text : "";
  if (explicitPlainText.length > 0) {
    return `plain:${explicitPlainText}`;
  }
  return `markdown:${header.content ?? ""}`;
}

function buildFullTurnHeaderPlainText(header: WorkbenchTurnHeader): string {
  const explicitPlainText = typeof header.plain_text === "string" ? header.plain_text : "";
  if (explicitPlainText.length > 0) {
    return normalizeTurnHeaderPlainText(explicitPlainText);
  }
  return markdownToPlainText(header.content ?? "");
}

function collapseTurnHeaderPreviewPlainText(plainText: string): string {
  const normalized = String(plainText ?? "");
  let newlineCount = 0;
  let collapseEnd = normalized.length;
  for (let index = 0; index < normalized.length; index += 1) {
    if (normalized.charCodeAt(index) !== 10) continue;
    newlineCount += 1;
    if (newlineCount === TURN_HEADER_COLLAPSED_PREVIEW_LINE_LIMIT) {
      collapseEnd = index;
      break;
    }
  }
  return normalized.slice(0, collapseEnd);
}

export function getWorkbenchTurnHeaderTextState(header: WorkbenchTurnHeader): WorkbenchTurnHeaderTextState {
  const cacheKey = getTurnHeaderTextCacheKey(header);
  const cached = turnHeaderTextStateCache.get(cacheKey);
  if (cached) {
    return cached;
  }

  const explicitPlainText = typeof header.plain_text === "string" ? header.plain_text : "";
  const sourceText = explicitPlainText.length > 0 ? explicitPlainText : header.content ?? "";
  const previewSource =
    sourceText.length > TURN_HEADER_PREVIEW_SOURCE_CHAR_LIMIT
      ? sourceText.slice(0, TURN_HEADER_PREVIEW_SOURCE_CHAR_LIMIT)
      : sourceText;
  const previewPlainText =
    explicitPlainText.length > 0
      ? normalizeTurnHeaderPlainText(previewSource)
      : markdownToPlainText(previewSource);
  const expandable =
    sourceText.length > TURN_HEADER_PREVIEW_SOURCE_CHAR_LIMIT ||
    isExpandableTurnHeaderPlainText(previewPlainText);
  const nextState: WorkbenchTurnHeaderTextState = {
    contentRevision: cacheKey,
    collapsedPlainText: collapseTurnHeaderPreviewPlainText(previewPlainText),
    expandable,
    expandedPlainText: expandable ? null : buildFullTurnHeaderPlainText(header),
  };
  turnHeaderTextStateCache.set(cacheKey, nextState);
  pruneTurnHeaderTextStateCache();
  return nextState;
}

function getExpandedTurnHeaderPlainText(
  header: WorkbenchTurnHeader,
  state: WorkbenchTurnHeaderTextState,
): string {
  if (state.expandedPlainText != null) {
    return state.expandedPlainText;
  }
  const fullPlainText = buildFullTurnHeaderPlainText(header);
  state.expandedPlainText = fullPlainText;
  return fullPlainText;
}

export function getWorkbenchTurnHeaderDisplayPlainText(header: WorkbenchTurnHeader): string {
  return buildFullTurnHeaderPlainText(header);
}

export function resolveWorkbenchTurnHeaderExpandedFromPlainText(
  header: WorkbenchTurnHeader,
  displayPlainText: string,
  expandedTurnHeaders: Record<string, boolean>,
): boolean {
  if (!isExpandableTurnHeaderPlainText(displayPlainText)) return true;
  return expandedTurnHeaders[header.id] ?? false;
}

export function getWorkbenchTurnHeaderLayoutState(
  item: Extract<WorkbenchListItem, { kind: "turn_header" }>,
  expandedTurnHeaders: Record<string, boolean>,
) {
  const textState = getWorkbenchTurnHeaderTextState(item.header);
  const expanded = textState.expandable ? (expandedTurnHeaders[item.header.id] ?? false) : true;
  const displayPlainText = expanded
    ? getExpandedTurnHeaderPlainText(item.header, textState)
    : textState.collapsedPlainText;
  return {
    contentRevision: textState.contentRevision,
    displayPlainText,
    expanded,
    expandable: textState.expandable,
  };
}

export function resolveWorkbenchMessageExpandedFromContent(
  item: Extract<WorkbenchListItem, { kind: "message" }>,
  expandedMessageById: Record<string, boolean>,
): boolean {
  if (!canCollapseMessageContent(item.content)) return true;
  return expandedMessageById[item.id] ?? false;
}

export function getWorkbenchMessageLayoutState(
  item: Extract<WorkbenchListItem, { kind: "message" }>,
  expandedMessageById: Record<string, boolean>,
) {
  const collapseState = getWorkbenchMessageCollapseState(item.content);
  const collapsedContent = collapseState.collapsedContent;
  const expandable = collapseState.canCollapse;
  const expanded = resolveWorkbenchMessageExpandedFromContent(item, expandedMessageById);
  const shownContent = expanded ? item.content : collapsedContent;
  const renderMode: WorkbenchMessageRenderMode =
    expanded && item.role === "user" && shownContent.length >= MESSAGE_EXPANDED_PLAIN_TEXT_CHAR_THRESHOLD
      ? "plain_text"
      : "markdown";
  return {
    expanded,
    expandable,
    shownContent,
    renderMode,
  };
}

export function getWorkbenchListItemLayoutState(
  item: WorkbenchListItem,
  uiState: WorkbenchListItemExpansionState,
) {
  switch (item.kind) {
    case "turn_header":
      return getWorkbenchTurnHeaderLayoutState(item, uiState.expandedTurnHeaders);
    case "message":
      return getWorkbenchMessageLayoutState(item, uiState.expandedMessageById);
    default:
      return null;
  }
}
