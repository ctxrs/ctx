import type React from "react";
import type { WorkspaceActiveSnapshotItem } from "../../state/workspaceActiveSnapshotStore";

export type AnchorRect = {
  left: number;
  right: number;
  top: number;
  bottom: number;
  width: number;
  height: number;
};

export type ArchiveConfirmState = {
  taskId: string;
  anchor: AnchorRect;
};

export type TaskListContext = {
  archivedCollapsed: boolean;
  archivedFetchState: "idle" | "loading" | "error";
  hasMoreArchived: boolean;
  onLoadMoreArchived: () => void;
  onScroll?: (event: React.UIEvent<HTMLDivElement>) => void;
  onScrollerChange?: (node: HTMLDivElement | null) => void;
};

export type TaskListItem =
  | { kind: "active-task"; summary: WorkspaceActiveSnapshotItem }
  | { kind: "archived-header" }
  | { kind: "archived-loading" }
  | { kind: "archived-error" }
  | { kind: "archived-empty" }
  | { kind: "archived-task"; summary: WorkspaceActiveSnapshotItem };

export type OptimisticTaskSummary = WorkspaceActiveSnapshotItem & {
  localStatus: "starting" | "synced" | "failed";
  localPrompt: string;
  localError?: string | null;
  localMessageId: string;
};

export type OptimisticFocus = {
  taskId: string;
  sessionId: string;
  navToken: number;
};
