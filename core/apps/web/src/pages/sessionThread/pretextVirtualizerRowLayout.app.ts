import type { PretextVirtualizerPlannedLayout } from "@pretext-virtualizer/core";
import {
  clearPretextVirtualizerRowLayoutCache,
  getPretextVirtualizerRowLayout as getPackagePretextVirtualizerRowLayout,
  type PretextVirtualizerRowLayoutContext,
  type WorkbenchListItem,
} from "./pretextVirtualizerRowLayout";
import {
  buildSessionTranscriptMeasurementHooks,
  clearSessionTranscriptMeasurementAuthorities,
} from "./sessionTranscriptMeasurementAuthorities";

export type AppPretextVirtualizerRowLayoutContext = PretextVirtualizerRowLayoutContext & {
  sessionId?: string | null;
};

function buildAppRowLayoutContext(
  context: AppPretextVirtualizerRowLayoutContext,
): PretextVirtualizerRowLayoutContext {
  return {
    ...context,
    measurementHooks: buildSessionTranscriptMeasurementHooks({
      sessionId: context.sessionId,
    }),
  };
}

export function clearAppPretextVirtualizerRowLayoutCache(): void {
  clearPretextVirtualizerRowLayoutCache();
  clearSessionTranscriptMeasurementAuthorities();
}

export function getPretextVirtualizerRowLayout(
  item: WorkbenchListItem,
  viewportWidth: number,
  context: AppPretextVirtualizerRowLayoutContext,
): PretextVirtualizerPlannedLayout {
  return getPackagePretextVirtualizerRowLayout(item, viewportWidth, buildAppRowLayoutContext(context));
}
