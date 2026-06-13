import React, { useCallback, useMemo, type Dispatch, type SetStateAction } from "react";
import { ChevronDown } from "lucide-react";

import type {
  WorkspaceActiveSnapshotItem,
  WorkspaceActiveSnapshotState,
} from "../../state/workspaceActiveSnapshotStore";
import type { TaskListContext, TaskListItem } from "./WorkbenchPage.types";
import { useWorkbenchTaskScrollbar } from "./useWorkbenchTaskScrollbar";

type WorkspaceSnapshotStore = {
  ensureArchivedLoaded: () => void;
  loadMoreActive: () => void;
  loadMoreArchived: () => void;
};

type Params = {
  activeTaskSummaries: WorkspaceActiveSnapshotItem[];
  archivedTaskSummaries: WorkspaceActiveSnapshotItem[];
  archivedCollapsed: boolean;
  setArchivedCollapsed: Dispatch<SetStateAction<boolean>>;
  workspaceSnapshot: WorkspaceActiveSnapshotState;
  workspaceSnapshotStore: WorkspaceSnapshotStore;
  renderTaskRow: (summary: WorkspaceActiveSnapshotItem, opts?: { archived?: boolean }) => React.ReactNode;
};

export function useWorkbenchTaskListModel({
  activeTaskSummaries,
  archivedTaskSummaries,
  archivedCollapsed,
  setArchivedCollapsed,
  workspaceSnapshot,
  workspaceSnapshotStore,
  renderTaskRow,
}: Params) {
  const renderArchivedRow = useCallback(
    (summary: WorkspaceActiveSnapshotItem) => renderTaskRow(summary, { archived: true }),
    [renderTaskRow],
  );

  const taskListItems = useMemo<TaskListItem[]>(() => {
    const items: TaskListItem[] = [];
    activeTaskSummaries.forEach((summary) => items.push({ kind: "active-task", summary }));
    items.push({ kind: "archived-header" });
    if (!archivedCollapsed) {
      if (workspaceSnapshot.fetchState.archived === "error") {
        items.push({ kind: "archived-error" });
      }
      archivedTaskSummaries.forEach((summary) => items.push({ kind: "archived-task", summary }));
      if (
        archivedTaskSummaries.length === 0 &&
        workspaceSnapshot.archivedLoaded &&
        workspaceSnapshot.fetchState.archived !== "loading"
      ) {
        items.push({ kind: "archived-empty" });
      }
      if (workspaceSnapshot.fetchState.archived === "loading") {
        items.push({ kind: "archived-loading" });
      }
    }
    return items;
  }, [
    activeTaskSummaries,
    archivedCollapsed,
    archivedTaskSummaries,
    workspaceSnapshot.archivedLoaded,
    workspaceSnapshot.fetchState.archived,
  ]);

  const activeSectionLastIndex = useMemo(() => {
    if (activeTaskSummaries.length > 0) {
      return activeTaskSummaries.length - 1;
    }
    if (workspaceSnapshot.initialized && workspaceSnapshot.fetchState.active !== "loading") {
      return 0;
    }
    return -1;
  }, [activeTaskSummaries.length, workspaceSnapshot.fetchState.active, workspaceSnapshot.initialized]);

  const renderTaskListItem = useCallback(
    (item: TaskListItem) => {
      switch (item.kind) {
        case "active-task":
          return renderTaskRow(item.summary);
        case "archived-header":
          return (
            <div className="wb-section-header wb-section-header-archived">
              <button
                type="button"
                className="wb-section-toggle"
                onClick={() => {
                  const next = !archivedCollapsed;
                  setArchivedCollapsed(next);
                  if (!next) {
                    workspaceSnapshotStore.ensureArchivedLoaded();
                  }
                }}
                aria-expanded={!archivedCollapsed}
              >
                <span className="wb-section-title">Archived Tasks</span>
                <span className={`wb-section-chev ${archivedCollapsed ? "wb-section-chev-collapsed" : ""}`}>
                  <ChevronDown size={14} />
                </span>
              </button>
            </div>
          );
        case "archived-loading":
          return (
            <div className="wb-archived-loading" aria-live="polite" aria-label="Loading archived tasks">
              <span className="wb-archived-spinner" aria-hidden="true" />
            </div>
          );
        case "archived-error":
          return <div className="wb-muted">Failed to load archived tasks. Retry.</div>;
        case "archived-empty":
          return <div className="wb-muted">No archived tasks.</div>;
        case "archived-task":
          return renderArchivedRow(item.summary);
        default:
          return null;
      }
    },
    [archivedCollapsed, renderArchivedRow, renderTaskRow, setArchivedCollapsed, workspaceSnapshotStore],
  );

  const computeTaskListItemKey = useCallback((_: number, item: TaskListItem) => {
    switch (item.kind) {
      case "active-task":
        return `active-${item.summary.id}`;
      case "archived-task":
        return `archived-${item.summary.id}`;
      case "archived-header":
        return "archived-header";
      case "archived-loading":
        return "archived-loading";
      case "archived-error":
        return "archived-error";
      case "archived-empty":
        return "archived-empty";
      default:
        return "unknown";
    }
  }, []);

  const onTaskListRangeChanged = useCallback(
    (range: { startIndex: number; endIndex: number }) => {
      if (!workspaceSnapshot.hasMoreActive) return;
      if (workspaceSnapshot.fetchState.active === "loading") return;
      if (activeSectionLastIndex < 0) return;
      if (range.endIndex < activeSectionLastIndex) return;
      workspaceSnapshotStore.loadMoreActive();
    },
    [activeSectionLastIndex, workspaceSnapshot.fetchState.active, workspaceSnapshot.hasMoreActive, workspaceSnapshotStore],
  );

  const { onTaskListScroll, onTaskListScrollerChange } = useWorkbenchTaskScrollbar({
    itemCount: taskListItems.length,
  });
  const initialTaskListItemCount =
    activeTaskSummaries.length === 0 && taskListItems.length > 0 ? taskListItems.length : undefined;
  const taskListVirtuosoKey =
    activeTaskSummaries.length === 0 ? `empty-${archivedCollapsed ? "collapsed" : "expanded"}-${taskListItems.length}` : "default";

  const loadMoreArchived = useCallback(() => {
    workspaceSnapshotStore.loadMoreArchived();
  }, [workspaceSnapshotStore]);

  const taskListContext = useMemo<TaskListContext>(
    () => ({
      archivedCollapsed,
      archivedFetchState: workspaceSnapshot.fetchState.archived,
      hasMoreArchived: workspaceSnapshot.hasMoreArchived,
      onLoadMoreArchived: loadMoreArchived,
      onScroll: onTaskListScroll,
      onScrollerChange: onTaskListScrollerChange,
    }),
    [
      archivedCollapsed,
      loadMoreArchived,
      onTaskListScroll,
      onTaskListScrollerChange,
      workspaceSnapshot.fetchState.archived,
      workspaceSnapshot.hasMoreArchived,
    ],
  );

  return {
    taskListVirtuosoKey,
    taskListItems,
    initialTaskListItemCount,
    computeTaskListItemKey,
    renderTaskListItem,
    taskListContext,
    onTaskListRangeChanged,
  };
}
