import {
  SESSION_THREAD_GEOMETRY_REVISION,
  type PretextVirtualizerMessageLayout,
  type PretextVirtualizerRowLayoutContext,
  type TranscriptLayoutPlanner,
  type TranscriptLayoutPlannerProfileId,
  type TranscriptRowPlan,
  type WorkbenchListItem,
  getSessionMarkdownInlineWrapBrowserProfile,
} from "./transcriptLayoutPlanner";
import {
  clearAppPretextVirtualizerRowLayoutCache,
  getPretextVirtualizerRowLayout,
  type AppPretextVirtualizerRowLayoutContext,
} from "./pretextVirtualizerRowLayout.app";

export type { PretextVirtualizerMessageLayout, TranscriptLayoutPlannerProfileId, TranscriptRowPlan };

function buildWidthBucket(viewportWidth: number): `w${number}` {
  return `w${Math.floor(Math.max(0, viewportWidth) / 64)}`;
}

export function planTranscriptRowLayout(
  item: WorkbenchListItem,
  viewportWidth: number,
  context: AppPretextVirtualizerRowLayoutContext,
): TranscriptRowPlan {
  const plannedLayout = getPretextVirtualizerRowLayout(item, viewportWidth, context);
  const browserProfileId: TranscriptLayoutPlannerProfileId =
    getSessionMarkdownInlineWrapBrowserProfile().id;
  return {
    item,
    itemId: item.id,
    rowKind: item.kind,
    viewportWidth,
    geometryRevision: SESSION_THREAD_GEOMETRY_REVISION,
    browserProfileId,
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
  context: AppPretextVirtualizerRowLayoutContext,
): TranscriptRowPlan[] {
  return items.map((item) => planTranscriptRowLayout(item, viewportWidth, context));
}

export function clearTranscriptLayoutPlannerCaches(): void {
  clearAppPretextVirtualizerRowLayoutCache();
}

export function getTranscriptRowPlannedLayout(
  item: WorkbenchListItem,
  viewportWidth: number,
  context: AppPretextVirtualizerRowLayoutContext,
) {
  return planTranscriptRowLayout(item, viewportWidth, context).plannedLayout;
}

export const defaultTranscriptLayoutPlanner: TranscriptLayoutPlanner = {
  planRow: (
    item: WorkbenchListItem,
    viewportWidth: number,
    context: PretextVirtualizerRowLayoutContext,
  ) => planTranscriptRowLayout(item, viewportWidth, context as AppPretextVirtualizerRowLayoutContext),
  clearCaches: clearTranscriptLayoutPlannerCaches,
};
