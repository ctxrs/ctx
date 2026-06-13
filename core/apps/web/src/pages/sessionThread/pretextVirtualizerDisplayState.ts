import type { PretextVirtualizerSnapshot } from "@pretext-virtualizer/core";
import type { PretextVirtualizerItemLocation } from "@pretext-virtualizer/interface";
import type { WorkbenchListItem } from "../SessionPage.types";
import type { WorkbenchMessageListUiState } from "../sessionMessageListItemIdentity";
import {
  primeSessionPretextRuntime,
  readSessionPretextRuntimePreparedState,
  type SessionPretextRuntimeRecord,
} from "./pretextSessionRuntimeCache";
import { computeBottomOffsetPx } from "./pretextFollowBottom";
import { getSessionTranscriptWarmState } from "./sessionTranscriptWarmState";

export function createInitialSessionThreadPretextSnapshot(params: {
  runtime: SessionPretextRuntimeRecord;
  sessionId: string;
  listItems: readonly WorkbenchListItem[];
  uiState: WorkbenchMessageListUiState;
  sourceKey: string;
  layoutKey: string;
}): PretextVirtualizerSnapshot<WorkbenchListItem> {
  const preparedState = readSessionPretextRuntimePreparedState(params.runtime);
  const preparedMatchesCurrentInputs =
    preparedState.sourceKey === params.sourceKey && preparedState.layoutKey === params.layoutKey;
  if (
    params.listItems.length === 0 ||
    (preparedState.snapshot.visibleItems.length > 0 && preparedMatchesCurrentInputs)
  ) {
    return preparedState.snapshot;
  }
  const warmState = getSessionTranscriptWarmState();
  if (warmState.viewportWidth <= 0 || warmState.viewportHeight <= 0) {
    return preparedState.snapshot;
  }
  const primedRuntime = primeSessionPretextRuntime({
    sessionId: params.sessionId,
    listItems: params.listItems,
    uiState: params.uiState,
    viewportWidth: warmState.viewportWidth,
    viewportHeight: warmState.viewportHeight,
    sourceKey: params.sourceKey,
    layoutKey: params.layoutKey,
  });
  return readSessionPretextRuntimePreparedState(primedRuntime).snapshot;
}

export function isBottomOpenLocation(location: PretextVirtualizerItemLocation | null | undefined): boolean {
  return location?.index === "LAST" && (location.align ?? "start") === "end";
}

export function isSnapshotReadyForDisplay(
  snapshot: PretextVirtualizerSnapshot<WorkbenchListItem>,
  itemCount: number,
  requireBottomAlignment: boolean,
  bottomThresholdPx: number,
): boolean {
  if (itemCount === 0) return false;
  if (snapshot.visibleItems.length === 0) return false;
  if (!requireBottomAlignment) return true;
  const bottomOffsetPx = computeBottomOffsetPx({
    totalHeight: snapshot.totalHeight,
    scrollTop: snapshot.scrollTop,
    viewportHeight: snapshot.viewportHeight,
  });
  return bottomOffsetPx <= bottomThresholdPx;
}
