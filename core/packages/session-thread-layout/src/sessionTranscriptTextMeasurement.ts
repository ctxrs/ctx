import {
  clearSessionMarkdownMeasurementCaches,
  measureSessionMarkdownDocument,
} from "./sessionMarkdownMeasurement";
import { measureSessionPlainTextBlockHeight } from "./sessionPlainTextMeasurement";
import { measureSessionTextHeight } from "./sessionTextMeasurement";
import { SESSION_MARKDOWN_MEASUREMENT_CONTRACT } from "./sessionThreadMeasurementContract";

const BODY_FONT = `${SESSION_MARKDOWN_MEASUREMENT_CONTRACT.typography.bodyFontSizePx}px ${SESSION_MARKDOWN_MEASUREMENT_CONTRACT.typography.bodyFontFamily}`;
const BODY_LINE_HEIGHT_PX = SESSION_MARKDOWN_MEASUREMENT_CONTRACT.typography.bodyLineHeightPx;

export type TranscriptMeasuredMessageLayout = {
  expanded: boolean;
  expandable: boolean;
  renderMode: "plain_text" | "markdown";
  shownContent: string;
};

export function clearSessionTranscriptTextMeasurementCaches(): void {
  clearSessionMarkdownMeasurementCaches();
}

export function measureTurnHeaderPreviewTextHeight(params: {
  cacheKey: string;
  text: string;
  width: number;
  collapsedMaxHeightPx: number;
  expanded: boolean;
}): number {
  const fullHeight = measureSessionPlainTextBlockHeight({
    cacheKey: params.cacheKey,
    text: params.text,
    font: BODY_FONT,
    width: params.width,
    lineHeight: BODY_LINE_HEIGHT_PX,
  });
  return params.expanded ? fullHeight : Math.min(fullHeight, params.collapsedMaxHeightPx);
}

export function measureMessageLayoutTextHeight(params: {
  cacheKey: string;
  itemId: string;
  layout: TranscriptMeasuredMessageLayout;
  width: number;
}): number {
  if (params.layout.renderMode === "plain_text") {
    return measureSessionTextHeight({
      cacheKey: `${params.cacheKey}:plain:${params.itemId}:${params.layout.expanded ? "expanded" : "collapsed"}:${params.layout.shownContent.length}`,
      text: params.layout.shownContent,
      font: BODY_FONT,
      width: params.width,
      lineHeight: BODY_LINE_HEIGHT_PX,
      whiteSpace: "pre-wrap",
    });
  }

  return measureSessionMarkdownDocument(params.layout.shownContent, params.width);
}

export function measureAssistantMarkdownTextHeight(params: {
  content: string;
  width: number;
}): number {
  return measureSessionMarkdownDocument(params.content, params.width);
}
