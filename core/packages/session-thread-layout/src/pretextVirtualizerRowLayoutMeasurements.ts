import {
  incrementPretextPerfCounter,
} from "./pretextPerfDiagnostics";
import type { PretextVirtualizerRowLayoutContext, PretextVirtualizerMeasuredHeight, PretextVirtualizerMessageLayout, PretextVirtualizerRowMeasurementRequest, PretextVirtualizerTextMeasurementRequest } from "./pretextVirtualizerRowLayout";
import {
  resolveSessionThreadAssistantTextWidth,
  resolveSessionThreadContentWidth,
  resolveSessionThreadIndentedContentWidth,
  resolveSessionThreadMessageTextWidth,
  resolveSessionThreadTurnHeaderTextWidth,
} from "./sessionThreadLayoutTokens";
import {
  SESSION_MARKDOWN_MEASUREMENT_CONTRACT,
  SESSION_THREAD_ROW_MEASUREMENT_CONTRACT,
} from "./sessionThreadMeasurementContract";
import {
  measureAssistantMarkdownTextHeight,
  measureMessageLayoutTextHeight,
  measureTurnHeaderPreviewTextHeight,
} from "./sessionTranscriptTextMeasurement";
import { measureSessionTextHeight } from "./sessionTextMeasurement";
import {
  getWorkbenchMessageLayoutState,
  getWorkbenchTurnHeaderLayoutState,
} from "./transcriptRowLayoutModel";
import type { WorkbenchListItem, WorkbenchTurnHeader } from "./transcriptTypes";

type ThreadAttachment = WorkbenchTurnHeader["attachments"][number];

const ROW_CONTRACT = SESSION_THREAD_ROW_MEASUREMENT_CONTRACT;
const MARKDOWN_CONTRACT = SESSION_MARKDOWN_MEASUREMENT_CONTRACT;

const SMALL_LINE_HEIGHT_PX = ROW_CONTRACT.tools.loading.typography.lineHeightPx;
const THOUGHT_FONT = `${ROW_CONTRACT.thought.typography.fontStyle ?? "normal"} ${ROW_CONTRACT.thought.typography.fontSizePx}px ${ROW_CONTRACT.thought.typography.fontFamily}`;
const THOUGHT_LINE_HEIGHT_PX = ROW_CONTRACT.thought.typography.lineHeightPx;
const TOOL_THOUGHT_BODY_FONT = `${ROW_CONTRACT.tools.thoughtBody.typography.fontSizePx}px ${ROW_CONTRACT.tools.thoughtBody.typography.fontFamily}`;
const TOOL_THOUGHT_BODY_LINE_HEIGHT_PX = ROW_CONTRACT.tools.thoughtBody.typography.lineHeightPx;

export const SPACER_HEIGHT_PX = ROW_CONTRACT.fixed.spacerHeightPx;
export const TURN_STATUS_HEIGHT_PX = ROW_CONTRACT.fixed.turnStatusHeightPx;
export const TOOL_ROW_HEIGHT_PX = ROW_CONTRACT.tools.summary.rowHeightPx;
const TOOL_GROUP_GAP_PX = ROW_CONTRACT.tools.groupGapPx;
const TOOL_THOUGHT_TITLE_HEIGHT_PX = ROW_CONTRACT.tools.thoughtTitle.heightPx;
const TOOL_THOUGHT_BODY_CHROME_WIDTH_PX = ROW_CONTRACT.tools.thoughtBody.chromeWidthPx;
const TOOL_THOUGHT_BODY_CHROME_HEIGHT_PX = ROW_CONTRACT.tools.thoughtBody.chromeHeightPx;

const THOUGHT_HORIZONTAL_CHROME_PX = ROW_CONTRACT.thought.horizontalChromePx;
const THOUGHT_VERTICAL_CHROME_PX = ROW_CONTRACT.thought.verticalChromePx;

const TURN_HEADER_OUTER_VERTICAL_PX = ROW_CONTRACT.turnHeader.outerVerticalPx;
const TURN_HEADER_BUBBLE_VERTICAL_PX =
  ROW_CONTRACT.turnHeader.bubblePaddingBlockPx * 2 + ROW_CONTRACT.turnHeader.bubbleBorderWidthPx * 2;
const TURN_HEADER_ATTACHMENT_SIZE_PX = ROW_CONTRACT.turnHeader.attachments.sizePx;
const TURN_HEADER_ATTACHMENT_GAP_PX = ROW_CONTRACT.turnHeader.attachments.gapPx;
const TURN_HEADER_ATTACHMENT_MARGIN_TOP_PX = ROW_CONTRACT.turnHeader.attachments.marginTopPx;

const MESSAGE_ROLE_HEIGHT_PX = ROW_CONTRACT.message.roleLineHeightPx;
const MESSAGE_BUBBLE_VERTICAL_PX =
  ROW_CONTRACT.message.bubblePaddingBlockPx * 2 + ROW_CONTRACT.message.bubbleBorderWidthPx * 2;
const MESSAGE_TOGGLE_HEIGHT_PX =
  ROW_CONTRACT.message.toggleMarginTopPx + ROW_CONTRACT.message.toggleLineHeightPx;
const MESSAGE_ROW_VERTICAL_PX = ROW_CONTRACT.message.rowPaddingBlockPx * 2;

const ASSISTANT_VERTICAL_PADDING_PX = ROW_CONTRACT.assistant.verticalPaddingPx;

export const PERF_WIDTH_BUCKET_SIZE = 64;

export const normalizeRowLayoutHeight = (value: number): number =>
  Number.isFinite(value) && value > 0 ? Math.max(1, Math.round(value * 16) / 16) : 1;

const resolveThoughtTextWidth = (viewportWidth: number): number =>
  Math.max(1, resolveSessionThreadIndentedContentWidth(viewportWidth) - THOUGHT_HORIZONTAL_CHROME_PX);

function measureTextHeight(params: {
  cacheKey: string;
  text: string;
  font: string;
  width: number;
  lineHeight: number;
  whiteSpace?: "normal" | "pre-wrap";
}): number {
  return measureSessionTextHeight(params);
}

function resolveMeasuredHeight(
  measurement: PretextVirtualizerMeasuredHeight | null | undefined,
  missCounter: string,
): number | null {
  if (measurement == null) {
    return null;
  }
  if (measurement.status === "measured") {
    return measurement.height;
  }
  incrementPretextPerfCounter(missCounter);
  return null;
}

const countImageAttachments = (attachments: readonly ThreadAttachment[]): number =>
  attachments.filter((attachment) => attachment.kind === "image" || attachment.kind === "image_ref").length;

function countAttachmentRows(attachmentCount: number, width: number): number {
  if (attachmentCount <= 0) return 0;
  const perRow = Math.max(
    1,
    Math.floor(
      (width + ROW_CONTRACT.message.attachments.gapPx) /
        (ROW_CONTRACT.message.attachments.widthPx + ROW_CONTRACT.message.attachments.gapPx),
    ),
  );
  return Math.ceil(attachmentCount / perRow);
}

export function measureTurnHeaderRowHeight(
  header: WorkbenchTurnHeader,
  viewportWidth: number,
  context: PretextVirtualizerRowLayoutContext,
): number {
  const { contentRevision, displayPlainText, expanded } = getWorkbenchTurnHeaderLayoutState(
    { kind: "turn_header", id: `turn-header-${header.id}`, header },
    context.expandedTurnHeaders ?? {},
  );
  const previewRequest: PretextVirtualizerTextMeasurementRequest = {
    kind: "turn-header-preview-text",
    cacheKey: `turn-header:${contentRevision}:${expanded ? "expanded" : "collapsed"}`,
    text: displayPlainText,
    width: resolveSessionThreadTurnHeaderTextWidth(viewportWidth),
    collapsedMaxHeightPx: ROW_CONTRACT.turnHeader.collapsedMaxHeightPx,
    expanded,
  };
  const collapsedTextHeight =
    resolveMeasuredHeight(
      context.measurementHooks?.measureTextHeight?.(previewRequest),
      "pretext_row_layout_turn_header_text_measurement_miss",
    ) ??
    measureTurnHeaderPreviewTextHeight({
      cacheKey: previewRequest.cacheKey,
      text: previewRequest.text,
      width: previewRequest.width,
      collapsedMaxHeightPx: previewRequest.collapsedMaxHeightPx,
      expanded: previewRequest.expanded,
    });
  const imageCount = expanded ? countImageAttachments(header.attachments) : 0;
  const perRow = Math.max(
    1,
    Math.floor(
      (resolveSessionThreadContentWidth(viewportWidth) -
        ROW_CONTRACT.turnHeader.bubbleBorderWidthPx * 2 -
        ROW_CONTRACT.turnHeader.bubblePaddingInlinePx * 2 +
        TURN_HEADER_ATTACHMENT_GAP_PX) /
        (TURN_HEADER_ATTACHMENT_SIZE_PX + TURN_HEADER_ATTACHMENT_GAP_PX),
    ),
  );
  const attachmentRows = imageCount > 0 ? Math.ceil(imageCount / perRow) : 0;
  const attachmentsHeight =
    attachmentRows > 0
      ? TURN_HEADER_ATTACHMENT_MARGIN_TOP_PX +
        attachmentRows * TURN_HEADER_ATTACHMENT_SIZE_PX +
        Math.max(0, attachmentRows - 1) * TURN_HEADER_ATTACHMENT_GAP_PX
      : 0;
  return normalizeRowLayoutHeight(
    TURN_HEADER_OUTER_VERTICAL_PX + TURN_HEADER_BUBBLE_VERTICAL_PX + collapsedTextHeight + attachmentsHeight,
  );
}

export function measureThoughtRowHeight(
  item: Extract<WorkbenchListItem, { kind: "thought" }>,
  viewportWidth: number,
): number {
  return normalizeRowLayoutHeight(
    THOUGHT_VERTICAL_CHROME_PX +
      measureTextHeight({
        cacheKey: `thought:${item.id}:${item.content}`,
        text: item.content ?? "",
        font: THOUGHT_FONT,
        width: resolveThoughtTextWidth(viewportWidth),
        lineHeight: THOUGHT_LINE_HEIGHT_PX,
        whiteSpace: "pre-wrap",
      }),
  );
}

function measureMessageAttachmentsHeight(
  item: Extract<WorkbenchListItem, { kind: "message" }>,
  viewportWidth: number,
): number {
  const imageCount = countImageAttachments(item.attachments ?? []);
  if (imageCount === 0) return 0;
  const rows = countAttachmentRows(imageCount, resolveSessionThreadMessageTextWidth(viewportWidth));
  return (
    ROW_CONTRACT.message.attachments.marginTopPx +
    rows * ROW_CONTRACT.message.attachments.heightPx +
    Math.max(0, rows - 1) * ROW_CONTRACT.message.attachments.gapPx
  );
}

export function measureMessageRowHeight(
  item: Extract<WorkbenchListItem, { kind: "message" }>,
  viewportWidth: number,
  context: PretextVirtualizerRowLayoutContext,
): number {
  const layout = getWorkbenchMessageLayoutState(item, context.expandedMessageById ?? {});
  const rowRequest: PretextVirtualizerRowMeasurementRequest = {
    kind: "message-row",
    item,
    viewportWidth,
    layout,
  };
  const overriddenHeight = context.measurementHooks?.resolveRowHeightOverride?.(rowRequest) ?? null;
  if (overriddenHeight != null) {
    incrementPretextPerfCounter("pretext_row_layout_message_override_hit");
    return normalizeRowLayoutHeight(overriddenHeight);
  }
  const textWidth = resolveSessionThreadMessageTextWidth(viewportWidth);
  const textRequest: PretextVirtualizerTextMeasurementRequest = {
    kind: "message-text",
    itemId: item.id,
    width: textWidth,
    layout,
  };
  const textHeight =
    resolveMeasuredHeight(
      context.measurementHooks?.measureTextHeight?.(textRequest),
      "pretext_row_layout_message_text_measurement_miss",
    ) ??
    measureMessageLayoutTextHeight({
      cacheKey: "message-layout",
      itemId: item.id,
      layout,
      width: textWidth,
    });
  const attachmentsHeight = measureMessageAttachmentsHeight(item, viewportWidth);
  const toggleHeight = layout.expandable ? MESSAGE_TOGGLE_HEIGHT_PX : 0;
  return normalizeRowLayoutHeight(
    MESSAGE_ROW_VERTICAL_PX +
      MESSAGE_ROLE_HEIGHT_PX +
      MESSAGE_BUBBLE_VERTICAL_PX +
      textHeight +
      attachmentsHeight +
      toggleHeight,
  );
}

export function measureAssistantRowHeight(
  item: Extract<WorkbenchListItem, { kind: "assistant" }>,
  viewportWidth: number,
  context: PretextVirtualizerRowLayoutContext,
): number {
  if (!item.is_complete && item.content.trim().length === 0) {
    return SPACER_HEIGHT_PX;
  }
  const rowRequest: PretextVirtualizerRowMeasurementRequest = {
    kind: "assistant-row",
    item,
    viewportWidth,
  };
  const overriddenHeight = context.measurementHooks?.resolveRowHeightOverride?.(rowRequest) ?? null;
  if (overriddenHeight != null) {
    incrementPretextPerfCounter("pretext_row_layout_assistant_override_hit");
    return normalizeRowLayoutHeight(overriddenHeight);
  }
  const measuredRowHeight = resolveMeasuredHeight(
    context.measurementHooks?.measureRowHeight?.(rowRequest),
    "pretext_row_layout_assistant_row_measurement_miss",
  );
  if (measuredRowHeight != null) {
    incrementPretextPerfCounter("pretext_row_layout_assistant_row_measurement_hit");
    return normalizeRowLayoutHeight(measuredRowHeight);
  }
  const textWidth = resolveSessionThreadAssistantTextWidth(viewportWidth);
  const textRequest: PretextVirtualizerTextMeasurementRequest = {
    kind: "assistant-markdown-text",
    content: item.content,
    width: textWidth,
  };
  const textHeight =
    resolveMeasuredHeight(
      context.measurementHooks?.measureTextHeight?.(textRequest),
      "pretext_row_layout_assistant_text_measurement_miss",
    ) ??
    measureAssistantMarkdownTextHeight({ content: textRequest.content, width: textRequest.width });
  return normalizeRowLayoutHeight(ASSISTANT_VERTICAL_PADDING_PX + textHeight);
}

function measureToolThoughtHeight(thought: string, width: number): number {
  if (!thought.trim()) return 0;
  const thoughtTextHeight = measureTextHeight({
    cacheKey: `tool-thought:${thought}`,
    text: thought,
    font: TOOL_THOUGHT_BODY_FONT,
    width: Math.max(1, width - TOOL_THOUGHT_BODY_CHROME_WIDTH_PX),
    lineHeight: TOOL_THOUGHT_BODY_LINE_HEIGHT_PX,
    whiteSpace: "pre-wrap",
  });
  return TOOL_THOUGHT_TITLE_HEIGHT_PX + TOOL_THOUGHT_BODY_CHROME_HEIGHT_PX + thoughtTextHeight;
}

export function measureToolGroupRowHeight(
  item: Extract<WorkbenchListItem, { kind: "tool_group" }>,
  viewportWidth: number,
  context: PretextVirtualizerRowLayoutContext,
): number {
  const expanded = context.expandedTurnDetailsById?.[item.turn_id] ?? false;
  const summaryHeight = TOOL_ROW_HEIGHT_PX;
  if (!expanded) return summaryHeight;
  const loadingTools = item.tools.length === 0 && Boolean(context.turnToolsLoading?.includes(item.turn_id));
  let detailsHeight = 0;
  if (loadingTools) {
    detailsHeight += SMALL_LINE_HEIGHT_PX;
  } else if (item.tools.length > 0) {
    detailsHeight +=
      item.tools.length * TOOL_ROW_HEIGHT_PX +
      Math.max(0, item.tools.length - 1) * ROW_CONTRACT.tools.itemGapPx;
  }
  const thoughtHeight = measureToolThoughtHeight(
    item.thought,
    resolveSessionThreadContentWidth(viewportWidth) - ROW_CONTRACT.viewport.indentLeftPx,
  );
  if (thoughtHeight > 0) {
    if (detailsHeight > 0) detailsHeight += TOOL_GROUP_GAP_PX;
    detailsHeight += thoughtHeight;
  }
  return normalizeRowLayoutHeight(summaryHeight + (detailsHeight > 0 ? TOOL_GROUP_GAP_PX + detailsHeight : 0));
}

export function measureAskUserQuestionRowHeight(
  item: Extract<WorkbenchListItem, { kind: "ask_user_question" }>,
  viewportWidth: number,
): number {
  void item;
  void viewportWidth;
  return normalizeRowLayoutHeight(ROW_CONTRACT.askUser.outerHeightPx);
}
