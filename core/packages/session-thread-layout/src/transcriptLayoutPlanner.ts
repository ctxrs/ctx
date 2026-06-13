import type { PretextVirtualizerPlannedLayout } from "@pretext-virtualizer/core";
import type { WorkbenchListItem } from "./transcriptTypes";
import {
  getSessionMarkdownInlineWrapBrowserProfile,
  type SessionMarkdownInlineWrapBrowserProfile,
} from "./sessionMarkdownBrowserProfile";
import {
  clearPretextVirtualizerRowLayoutCache,
  getPretextVirtualizerRowLayout,
  type PretextVirtualizerRowLayoutContext,
} from "./pretextVirtualizerRowLayout";
import { SESSION_THREAD_GEOMETRY_REVISION } from "./sessionThreadGeometrySpec";

export type TranscriptLayoutPlannerProfileId = SessionMarkdownInlineWrapBrowserProfile["id"];
export type TranscriptLayoutPlanner = {
  planRow: (
    item: WorkbenchListItem,
    viewportWidth: number,
    context: PretextVirtualizerRowLayoutContext,
  ) => TranscriptRowPlan;
  clearCaches: () => void;
};

export type TranscriptRowPlan = {
  item: WorkbenchListItem;
  itemId: string;
  rowKind: WorkbenchListItem["kind"];
  viewportWidth: number;
  geometryRevision: string;
  browserProfileId: TranscriptLayoutPlannerProfileId;
  plannedLayout: PretextVirtualizerPlannedLayout;
  totalHeight: number;
  trace: {
    planner: "pretext-row-layout";
    widthBucket: `w${number}`;
  };
};

function buildWidthBucket(viewportWidth: number): `w${number}` {
  return `w${Math.floor(Math.max(0, viewportWidth) / 64)}`;
}

export function planTranscriptRowLayout(
  item: WorkbenchListItem,
  viewportWidth: number,
  context: PretextVirtualizerRowLayoutContext,
): TranscriptRowPlan {
  const plannedLayout = getPretextVirtualizerRowLayout(item, viewportWidth, context);
  return {
    item,
    itemId: item.id,
    rowKind: item.kind,
    viewportWidth,
    geometryRevision: SESSION_THREAD_GEOMETRY_REVISION,
    browserProfileId: getSessionMarkdownInlineWrapBrowserProfile().id,
    plannedLayout,
    totalHeight: plannedLayout.height,
    trace: {
      planner: "pretext-row-layout",
      widthBucket: buildWidthBucket(viewportWidth),
    },
  };
}

export function planTranscriptRows(
  items: readonly WorkbenchListItem[],
  viewportWidth: number,
  context: PretextVirtualizerRowLayoutContext,
): TranscriptRowPlan[] {
  return items.map((item) => planTranscriptRowLayout(item, viewportWidth, context));
}

export function clearTranscriptLayoutPlannerCaches(): void {
  clearPretextVirtualizerRowLayoutCache();
}

export function getTranscriptRowPlannedLayout(
  item: WorkbenchListItem,
  viewportWidth: number,
  context: PretextVirtualizerRowLayoutContext,
): PretextVirtualizerPlannedLayout {
  return planTranscriptRowLayout(item, viewportWidth, context).plannedLayout;
}

export const defaultTranscriptLayoutPlanner: TranscriptLayoutPlanner = {
  planRow: planTranscriptRowLayout,
  clearCaches: clearTranscriptLayoutPlannerCaches,
};
