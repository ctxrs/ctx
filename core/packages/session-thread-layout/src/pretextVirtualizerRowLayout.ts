import type { PretextVirtualizerPlannedLayout } from "@pretext-virtualizer/core";
import {
  addPretextPerfBucket,
  incrementPretextPerfCounter,
} from "./pretextPerfDiagnostics";
import {
  clearSessionTranscriptTextMeasurementCaches,
} from "./sessionTranscriptTextMeasurement";
import type { WorkbenchListItem } from "./transcriptTypes";
import {
  measureAskUserQuestionRowHeight,
  measureAssistantRowHeight,
  measureMessageRowHeight,
  measureThoughtRowHeight,
  measureToolGroupRowHeight,
  measureTurnHeaderRowHeight,
  PERF_WIDTH_BUCKET_SIZE,
  SPACER_HEIGHT_PX,
  TOOL_ROW_HEIGHT_PX,
  TURN_STATUS_HEIGHT_PX,
} from "./pretextVirtualizerRowLayoutMeasurements";

export type PretextVirtualizerRowLayoutContext = {
  expandedTurnHeaders?: Readonly<Record<string, boolean>>;
  expandedTurnDetailsById?: Readonly<Record<string, boolean>>;
  expandedMessageById?: Readonly<Record<string, boolean>>;
  turnToolsLoading?: readonly string[];
  measurementHooks?: PretextVirtualizerMeasurementHooks;
};

export type PretextVirtualizerMessageLayout = {
  expanded: boolean;
  expandable: boolean;
  renderMode: "plain_text" | "markdown";
  shownContent: string;
};

export type PretextVirtualizerRowMeasurementRequest =
  | {
      kind: "assistant-row";
      item: Extract<WorkbenchListItem, { kind: "assistant" }>;
      viewportWidth: number;
    }
  | {
      kind: "message-row";
      item: Extract<WorkbenchListItem, { kind: "message" }>;
      viewportWidth: number;
      layout: PretextVirtualizerMessageLayout;
    };

export type PretextVirtualizerTextMeasurementRequest =
  | {
      kind: "assistant-markdown-text";
      content: string;
      width: number;
    }
  | {
      kind: "message-text";
      itemId: string;
      width: number;
      layout: PretextVirtualizerMessageLayout;
    }
  | {
      kind: "turn-header-preview-text";
      cacheKey: string;
      text: string;
      width: number;
      collapsedMaxHeightPx: number;
      expanded: boolean;
    };

export type PretextVirtualizerMeasuredHeight =
  | {
      status: "measured";
      height: number;
    }
  | {
      status: "miss";
    };

export type PretextVirtualizerMeasurementHooks = {
  resolveRowHeightOverride?: (request: PretextVirtualizerRowMeasurementRequest) => number | null;
  measureRowHeight?: (
    request: PretextVirtualizerRowMeasurementRequest,
  ) => PretextVirtualizerMeasuredHeight;
  measureTextHeight?: (
    request: PretextVirtualizerTextMeasurementRequest,
  ) => PretextVirtualizerMeasuredHeight;
};

export const clearPretextVirtualizerRowLayoutCache = (): void => {
  clearSessionTranscriptTextMeasurementCaches();
};

export const getPretextVirtualizerRowLayout = (
  item: WorkbenchListItem,
  viewportWidth: number,
  context: PretextVirtualizerRowLayoutContext,
): PretextVirtualizerPlannedLayout => {
  const widthBucket = `w${Math.floor(Math.max(0, viewportWidth) / PERF_WIDTH_BUCKET_SIZE)}`;
  incrementPretextPerfCounter("pretext_row_layout_calls");
  addPretextPerfBucket("pretext_row_layout_kind", item.kind);
  addPretextPerfBucket("pretext_row_layout_item", `${item.kind}:${item.id}:${widthBucket}`);
  switch (item.kind) {
    case "spacer":
      return { height: SPACER_HEIGHT_PX };
    case "turn_status":
      return { height: TURN_STATUS_HEIGHT_PX };
    case "thought":
      return { height: measureThoughtRowHeight(item, viewportWidth) };
    case "turn_header":
      return { height: measureTurnHeaderRowHeight(item.header, viewportWidth, context) };
    case "message":
      return { height: measureMessageRowHeight(item, viewportWidth, context) };
    case "assistant":
      return { height: measureAssistantRowHeight(item, viewportWidth, context) };
    case "tool":
      return { height: TOOL_ROW_HEIGHT_PX };
    case "tool_group":
      return { height: measureToolGroupRowHeight(item, viewportWidth, context) };
    case "ask_user_question":
      return { height: measureAskUserQuestionRowHeight(item, viewportWidth) };
    default:
      return { height: SPACER_HEIGHT_PX };
  }
};
