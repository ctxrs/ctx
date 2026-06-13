import type { PretextVirtualizerDiagnosticEvent } from "@pretext-virtualizer/core";
import type { WorkbenchListItem } from "../sessionView/SessionPage.types";
import { getWorkbenchListItemHeightRevision, type WorkbenchMessageListUiState } from "../sessionMessageListItemIdentity";
import type { AppPretextVirtualizerRowLayoutContext } from "./pretextVirtualizerRowLayout.app";
import { defaultTranscriptLayoutPlanner } from "./transcriptLayoutPlanner.app";
import { getSessionTranscriptUiStateRevision } from "./pretextSessionRuntimeInputs";
import type { SessionPretextRuntimeRecord } from "./pretextSessionRuntimeCache";

export type SessionPretextRuntimeBindings = {
  uiState: WorkbenchMessageListUiState;
  listItems?: readonly WorkbenchListItem[];
  uiStateRevision?: string;
  onDiagnosticEvent?: ((event: PretextVirtualizerDiagnosticEvent<WorkbenchListItem>) => void) | null;
};

export function bindSessionPretextRuntime(
  record: SessionPretextRuntimeRecord,
  bindings: SessionPretextRuntimeBindings,
): void {
  record.uiState = bindings.uiState;
  record.uiStateRevision =
    bindings.uiStateRevision ?? getSessionTranscriptUiStateRevision(bindings.uiState, bindings.listItems);
  const getLayoutRevision = (item: WorkbenchListItem) =>
    getWorkbenchListItemHeightRevision(item, bindings.uiState, {
      verbosity: bindings.uiState.verbosity,
    });
  record.callbacks.getLayoutRevision = getLayoutRevision;
  record.callbacks.getPlannedLayout = (item, viewport) => {
    const planContext: AppPretextVirtualizerRowLayoutContext = {
      sessionId: record.sessionId,
      expandedTurnHeaders: bindings.uiState.expandedTurnHeaders,
      expandedTurnDetailsById: bindings.uiState.expandedTurnDetailsById,
      expandedMessageById: bindings.uiState.expandedMessageById,
      turnToolsLoading: bindings.uiState.turnToolsLoading,
    };
    const plan = defaultTranscriptLayoutPlanner.planRow(item, viewport.width, planContext);
    return plan.plannedLayout;
  };
  record.callbacks.onDiagnosticEvent = bindings.onDiagnosticEvent ?? null;
}
