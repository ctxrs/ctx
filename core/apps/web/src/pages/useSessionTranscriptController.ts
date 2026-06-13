import { useCallback, useEffect, useMemo, useRef } from "react";
import type { WorkbenchListItem } from "./SessionPage.types";
import type { WorkbenchMessageListUiState } from "./sessionMessageListItemIdentity";
import {
  createWorkbenchLayoutProjectionOp,
  createWorkbenchThreadProjectionOp,
  mergeWorkbenchThreadProjectionOps,
  type WorkbenchThreadProjectionOp,
} from "./sessionThreadProjection";
import { usePretextVirtualizerSessionController } from "./usePretextVirtualizerSessionController";

export function useSessionTranscriptController({
  sessionId,
  isActive,
  loaded,
  listItems,
  canLoadOlder,
  loadOlder,
  showDebug,
  onAtBottomChange,
  onInitialContentRendered,
  uiState,
  workbenchThreadOp,
  projectionRevision,
}: {
  sessionId: string;
  isActive: boolean;
  loaded: boolean;
  listItems: WorkbenchListItem[];
  canLoadOlder: boolean;
  loadOlder: () => Promise<void>;
  showDebug: boolean;
  onAtBottomChange: (atBottom: boolean) => void;
  onInitialContentRendered?: () => void;
  uiState: WorkbenchMessageListUiState;
  workbenchThreadOp: WorkbenchThreadProjectionOp | null | undefined;
  projectionRevision: number;
}) {
  const previousUiStateRef = useRef<WorkbenchMessageListUiState | null>(null);
  const effectiveWorkbenchThreadOp = useMemo(
    () => workbenchThreadOp ?? createWorkbenchThreadProjectionOp("noop", projectionRevision),
    [projectionRevision, workbenchThreadOp],
  );

  const layoutThreadOp = useMemo(
    () =>
      createWorkbenchLayoutProjectionOp({
        listItems,
        previousUiState: previousUiStateRef.current,
        nextUiState: uiState,
        projectionRevision,
      }),
    [listItems, projectionRevision, uiState],
  );

  useEffect(() => {
    previousUiStateRef.current = uiState;
  }, [uiState]);

  const threadProjectionOp = useMemo(
    () => mergeWorkbenchThreadProjectionOps(effectiveWorkbenchThreadOp, layoutThreadOp),
    [effectiveWorkbenchThreadOp, layoutThreadOp],
  );

  const itemIdentity = useCallback((item: WorkbenchListItem) => item.id, []);
  const itemKey = useCallback((item: WorkbenchListItem) => item.id, []);

  const messageListController = usePretextVirtualizerSessionController({
    sessionId,
    isActive,
    loaded,
    listItems,
    canLoadOlder,
    loadOlder,
    showDebug,
    onAtBottomChange,
    onInitialContentRendered,
  });

  return {
    threadProjectionOp,
    itemIdentity,
    itemKey,
    ...messageListController,
  };
}
