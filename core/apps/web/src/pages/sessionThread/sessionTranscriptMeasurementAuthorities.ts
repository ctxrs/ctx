import type { WorkbenchListItem } from "../SessionPage.types";
import {
  SESSION_MARKDOWN_MEASUREMENT_CONTRACT,
  clearSessionMarkdownMeasurementCaches,
  measureSessionMarkdownDocument,
  shouldUseRenderedAssistantRowMeasurement,
  shouldUseRenderedMarkdownMeasurement,
} from "./sessionMarkdownMeasurement";
import { clearSessionPlainTextMeasurementCaches } from "./sessionPlainTextMeasurement";
import { measureSessionTextHeight } from "./sessionTextMeasurement";
import { shouldUseExactRenderedPlainTextMeasurement } from "./sessionMeasurementPolicy";
import {
  clearPretextRowMeasurementOverrides,
  readPretextAssistantHeightOverride,
  readPretextMessageHeightOverride,
} from "./pretextRowMeasurementOverrides";
import {
  clearSessionThreadExactMeasurementCaches,
  measureRenderedSessionAssistantHeight,
  measureRenderedSessionMarkdownHeight,
  measureRenderedSessionPlainTextBlockHeight,
  measureRenderedSessionTurnHeaderPreviewTextHeight,
} from "./sessionThreadDomMeasurement";
import type {
  PretextVirtualizerMeasuredHeight,
  PretextVirtualizerMeasurementHooks,
  PretextVirtualizerMessageLayout,
  PretextVirtualizerRowMeasurementRequest,
  PretextVirtualizerTextMeasurementRequest,
} from "./pretextVirtualizerRowLayout";

const BODY_FONT =
  `${SESSION_MARKDOWN_MEASUREMENT_CONTRACT.typography.bodyFontSizePx}px ${SESSION_MARKDOWN_MEASUREMENT_CONTRACT.typography.bodyFontFamily}`;
const BODY_LINE_HEIGHT_PX = SESSION_MARKDOWN_MEASUREMENT_CONTRACT.typography.bodyLineHeightPx;

function measured(height: number | null): PretextVirtualizerMeasuredHeight {
  if (height == null) {
    return { status: "miss" };
  }
  return { status: "measured", height };
}

export function clearSessionTranscriptMeasurementAuthorities(): void {
  clearPretextRowMeasurementOverrides();
  clearSessionThreadExactMeasurementCaches();
  clearSessionMarkdownMeasurementCaches();
  clearSessionPlainTextMeasurementCaches();
}

export function measureSessionMarkdownDocumentWithAuthorities(markdown: string, width: number): number {
  if (shouldUseRenderedMarkdownMeasurement(markdown)) {
    const renderedHeight = measureRenderedSessionMarkdownHeight(markdown, width);
    if (renderedHeight != null) {
      return renderedHeight;
    }
  }
  return measureSessionMarkdownDocument(markdown, width);
}

function measurePlainTextContentHeightWithAuthorities(text: string, width: number): number {
  if (shouldUseExactRenderedPlainTextMeasurement(text)) {
    const renderedHeight = measureRenderedSessionPlainTextBlockHeight(text, width);
    if (renderedHeight != null) {
      return renderedHeight;
    }
  }
  return measureSessionTextHeight({
    cacheKey: `plain-text-authority:${width}:${text.length}`,
    text,
    font: BODY_FONT,
    width,
    lineHeight: BODY_LINE_HEIGHT_PX,
    whiteSpace: "pre-wrap",
  });
}

function measureMessageLayoutTextHeightWithAuthorities(params: {
  itemId: string;
  layout: PretextVirtualizerMessageLayout;
  width: number;
}): number {
  if (params.layout.renderMode === "markdown") {
    return measureSessionMarkdownDocumentWithAuthorities(params.layout.shownContent, params.width);
  }
  return measurePlainTextContentHeightWithAuthorities(params.layout.shownContent, params.width);
}

function measureAssistantMarkdownTextHeightWithAuthorities(params: {
  content: string;
  width: number;
}): number {
  return measureSessionMarkdownDocumentWithAuthorities(params.content, params.width);
}

function resolveRowHeightOverride(
  request: PretextVirtualizerRowMeasurementRequest,
  sessionId: string | null | undefined,
): number | null {
  if (request.kind === "assistant-row") {
    return readPretextAssistantHeightOverride({
      sessionId,
      item: request.item,
      viewportWidth: request.viewportWidth,
    });
  }
  return readPretextMessageHeightOverride({
    sessionId,
    item: request.item,
    viewportWidth: request.viewportWidth,
    layout: request.layout,
  });
}

function measureExactRowHeight(request: PretextVirtualizerRowMeasurementRequest): PretextVirtualizerMeasuredHeight {
  if (
    request.kind === "assistant-row" &&
    shouldUseRenderedAssistantRowMeasurement(request.item.content)
  ) {
    return measured(measureRenderedSessionAssistantHeight(request.item, request.viewportWidth));
  }
  return { status: "miss" };
}

function measureExactTextHeight(request: PretextVirtualizerTextMeasurementRequest): PretextVirtualizerMeasuredHeight {
  switch (request.kind) {
    case "assistant-markdown-text":
      if (!shouldUseRenderedMarkdownMeasurement(request.content)) {
        return { status: "miss" };
      }
      return measured(measureRenderedSessionMarkdownHeight(request.content, request.width));
    case "message-text":
      if (request.layout.renderMode === "markdown") {
        if (!shouldUseRenderedMarkdownMeasurement(request.layout.shownContent)) {
          return { status: "miss" };
        }
        return measured(
          measureRenderedSessionMarkdownHeight(request.layout.shownContent, request.width),
        );
      }
      if (!shouldUseExactRenderedPlainTextMeasurement(request.layout.shownContent)) {
        return { status: "miss" };
      }
      return measured(
        measureRenderedSessionPlainTextBlockHeight(request.layout.shownContent, request.width),
      );
    case "turn-header-preview-text":
      if (!shouldUseExactRenderedPlainTextMeasurement(request.text)) {
        return { status: "miss" };
      }
      return measured(
        measureRenderedSessionTurnHeaderPreviewTextHeight({
          text: request.text,
          width: request.width,
          collapsedMaxHeightPx: request.collapsedMaxHeightPx,
          expanded: request.expanded,
        }),
      );
    default:
      return { status: "miss" };
  }
}

export function buildSessionTranscriptMeasurementHooks(params: {
  sessionId?: string | null;
}): PretextVirtualizerMeasurementHooks {
  return {
    resolveRowHeightOverride: (request) => resolveRowHeightOverride(request, params.sessionId),
    measureRowHeight: measureExactRowHeight,
    measureTextHeight: measureExactTextHeight,
  };
}

export function measureMessageLayoutTextHeightForApp(params: {
  itemId: string;
  layout: PretextVirtualizerMessageLayout;
  width: number;
}): number {
  return measureMessageLayoutTextHeightWithAuthorities(params);
}

export function measureAssistantMarkdownTextHeightForApp(params: {
  content: string;
  width: number;
}): number {
  return measureAssistantMarkdownTextHeightWithAuthorities(params);
}

export function readAssistantRowHeightOverrideForApp(params: {
  sessionId: string | null | undefined;
  item: Extract<WorkbenchListItem, { kind: "assistant" }>;
  viewportWidth: number;
}): number | null {
  return readPretextAssistantHeightOverride(params);
}
