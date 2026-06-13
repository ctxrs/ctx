import type { WorkbenchListItem } from "../sessionView/SessionPage.types";
import {
  collectWorkbenchToolGroupExpansionIds,
  getWorkbenchMessageListLayoutRevision,
  getWorkbenchListItemHeightRevision,
  type WorkbenchMessageListUiState,
} from "../sessionMessageListItemIdentity";

export function getSessionTranscriptUiStateRevision(
  uiState: WorkbenchMessageListUiState,
  listItems: readonly WorkbenchListItem[] = [],
): string {
  return getWorkbenchMessageListLayoutRevision(uiState, {
    toolExpansionIds: collectWorkbenchToolGroupExpansionIds(listItems),
    verbosity: uiState.verbosity,
  });
}

function fingerprintString(value: string): string {
  const normalized = String(value ?? "");
  let hash = 2166136261;
  for (let index = 0; index < normalized.length; index += 1) {
    hash ^= normalized.charCodeAt(index);
    hash = Math.imul(hash, 16777619);
  }
  return `${normalized.length}:${(hash >>> 0).toString(36)}`;
}

export function normalizeViewportDimension(value: number | undefined): number {
  return Number.isFinite(value) && (value ?? 0) > 0 ? Math.round(value ?? 0) : 0;
}

export function createDefaultSessionTranscriptUiState(
  verbosity?: string,
  turnToolsLoading: readonly string[] = [],
): WorkbenchMessageListUiState {
  return {
    expandedTurnHeaders: {},
    expandedTurnDetailsById: {},
    expandedToolById: {},
    expandedMessageById: {},
    turnToolsLoading,
    verbosity,
  };
}

export function buildSessionPretextRuntimeSourceKey(
  listItems: readonly WorkbenchListItem[],
  uiState: WorkbenchMessageListUiState,
): string {
  const itemRevision = listItems
    .map((item) =>
      `${item.id}:${getWorkbenchListItemHeightRevision(item, uiState, {
        verbosity: uiState.verbosity,
      })}`,
    )
    .join("|");
  return `items:${fingerprintString(itemRevision)}`;
}

export function buildSessionPretextRuntimeLayoutKey(
  params: { uiState: WorkbenchMessageListUiState; listItems: readonly WorkbenchListItem[] },
): string {
  return `ui:${fingerprintString(getSessionTranscriptUiStateRevision(params.uiState, params.listItems))}`;
}
