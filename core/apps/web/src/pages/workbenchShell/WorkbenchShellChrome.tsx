import { useRef, type TouchEvent as ReactTouchEvent } from "react";
import type React from "react";
import { Virtuoso, type ListRange } from "react-virtuoso";
import { Link } from "react-router-dom";
import { ChevronsLeft, ChevronsRight, Menu, Settings, SquarePen } from "lucide-react";

import { TextInput } from "../../components/ui/text-input";
import { TASK_LIST_COMPONENTS } from "./WorkbenchPage.taskList";
import type { AnchorRect, TaskListContext, TaskListItem } from "./WorkbenchPage.types";
import {
  shouldCloseMobileSidebarSwipe,
  type MobileSidebarSwipePoint,
} from "./mobileSidebarGesture";

type TouchPointLike = {
  clientX: number;
  clientY: number;
};

type WorkbenchTopbarProps = {
  workspaceId: string;
  workspaceTitle: string;
  showDebugIds: boolean;
  debugIdLabel: string;
  onCopyDebugIds: () => void;
  settingsHref?: string;
  onToggleSidebar?: () => void;
  sidebarOpen?: boolean;
};

export function WorkbenchTopbar({
  workspaceId,
  workspaceTitle,
  showDebugIds,
  debugIdLabel,
  onCopyDebugIds,
  settingsHref,
  onToggleSidebar,
  sidebarOpen = false,
}: WorkbenchTopbarProps) {
  const taskListToggleLabel = sidebarOpen ? "Close task list" : "Open task list";

  return (
    <div className="wb-topbar">
      <div className="wb-topbar-left" data-tauri-drag-region={false}>
        {onToggleSidebar ? (
          <button
            type="button"
            className="wb-topbar-icon wb-topbar-menu-button"
            onClick={onToggleSidebar}
            aria-label={taskListToggleLabel}
            title={taskListToggleLabel}
            data-tauri-drag-region={false}
          >
            <Menu size={18} strokeWidth={2.4} aria-hidden="true" />
          </button>
        ) : null}
      </div>
      <div className="wb-topbar-center">
        {workspaceTitle ? <div className="wb-topbar-title">{workspaceTitle}</div> : null}
      </div>
      <div className="wb-topbar-right" data-tauri-drag-region={false}>
        {showDebugIds ? (
          <button
            type="button"
            className="wb-topbar-ids"
            title="Click to copy workspace/task/session IDs"
            onClick={onCopyDebugIds}
            data-tauri-drag-region={false}
          >
            {debugIdLabel}
          </button>
        ) : null}
        <Link
          className="wb-topbar-icon"
          to={settingsHref ?? `/settings?ws=${encodeURIComponent(String(workspaceId))}`}
          title="Settings"
          aria-label="Settings"
          data-tauri-drag-region={false}
        >
          <Settings size={14} />
        </Link>
      </div>
    </div>
  );
}

type WorkbenchSidebarProps = {
  collapsed: boolean;
  taskSearchRef: React.RefObject<HTMLInputElement | null>;
  taskQuery: string;
  onTaskQueryChange: (value: string) => void;
  onNewTask: () => void;
  taskListVirtuosoKey: string;
  taskListItems: TaskListItem[];
  initialTaskListItemCount: number | undefined;
  computeTaskListItemKey: (_: number, item: TaskListItem) => string;
  renderTaskListItem: (item: TaskListItem) => React.ReactNode;
  taskListContext: TaskListContext;
  onTaskListRangeChanged: (range: ListRange) => void;
  onExpandSidebar: () => void;
  onCollapseSidebar: () => void;
  onSidebarResizerMouseDown: (event: React.MouseEvent<HTMLDivElement>) => void;
  onResetSidebarWidth: () => void;
  mobileMode?: boolean;
  onSwipeClose?: () => void;
};

export function WorkbenchSidebar({
  collapsed,
  taskSearchRef,
  taskQuery,
  onTaskQueryChange,
  onNewTask,
  taskListVirtuosoKey,
  taskListItems,
  initialTaskListItemCount,
  computeTaskListItemKey,
  renderTaskListItem,
  taskListContext,
  onTaskListRangeChanged,
  onExpandSidebar,
  onCollapseSidebar,
  onSidebarResizerMouseDown,
  onResetSidebarWidth,
  mobileMode = false,
  onSwipeClose,
}: WorkbenchSidebarProps) {
  const swipeStartRef = useRef<MobileSidebarSwipePoint | null>(null);
  const swipeLatestRef = useRef<MobileSidebarSwipePoint | null>(null);

  const resetSwipe = () => {
    swipeStartRef.current = null;
    swipeLatestRef.current = null;
  };

  const recordSwipePoint = (touch: TouchPointLike): MobileSidebarSwipePoint => ({
    clientX: touch.clientX,
    clientY: touch.clientY,
  });

  const onTouchStart = (event: ReactTouchEvent<HTMLDivElement>) => {
    if (!mobileMode || collapsed || !onSwipeClose) return;
    if (event.touches.length !== 1) {
      resetSwipe();
      return;
    }
    const point = recordSwipePoint(event.touches[0]);
    swipeStartRef.current = point;
    swipeLatestRef.current = point;
  };

  const onTouchMove = (event: ReactTouchEvent<HTMLDivElement>) => {
    if (!mobileMode || collapsed || !onSwipeClose || !swipeStartRef.current) return;
    if (event.touches.length !== 1) {
      resetSwipe();
      return;
    }
    swipeLatestRef.current = recordSwipePoint(event.touches[0]);
  };

  const onTouchEnd = (event: ReactTouchEvent<HTMLDivElement>) => {
    if (!mobileMode || collapsed || !onSwipeClose || !swipeStartRef.current) {
      resetSwipe();
      return;
    }
    const endTouch = event.changedTouches[0] ?? null;
    const endPoint = endTouch ? recordSwipePoint(endTouch) : swipeLatestRef.current;
    const shouldClose = endPoint
      ? shouldCloseMobileSidebarSwipe(swipeStartRef.current, endPoint)
      : false;
    resetSwipe();
    if (shouldClose) onSwipeClose();
  };

  return (
    <>
      {collapsed ? (
        <button
          type="button"
          className="wb-sidebar-tab wb-sidebar-tab-collapsed"
          aria-label="Show sidebar"
          title="Show sidebar"
          onClick={onExpandSidebar}
        >
          <ChevronsRight size={16} />
        </button>
      ) : (
        <button
          type="button"
          className="wb-sidebar-tab wb-sidebar-tab-open"
          aria-label="Collapse sidebar"
          title="Collapse"
          onClick={onCollapseSidebar}
        >
          <ChevronsLeft size={16} />
        </button>
      )}

      <div
        className="wb-sidebar"
        aria-hidden={collapsed}
        onTouchStart={onTouchStart}
        onTouchMove={onTouchMove}
        onTouchEnd={onTouchEnd}
        onTouchCancel={resetSwipe}
      >
        <div className="wb-sidebar-top">
          <div className="wb-sidebar-header">
            <TextInput
              ref={taskSearchRef}
              className="wb-search"
              data-testid="workbench-task-search"
              placeholder="Search Tasks"
              value={taskQuery}
              onChange={(event) => onTaskQueryChange(event.target.value)}
            />
            <button
              type="button"
              className="wb-sidebar-action"
              aria-label="New task"
              title="New Task"
              onClick={onNewTask}
            >
              <SquarePen size={16} />
            </button>
          </div>
        </div>

        <div className="wb-sidebar-section wb-sidebar-grow" style={{ minHeight: 0, display: "flex" }}>
          <Virtuoso
            key={taskListVirtuosoKey}
            style={{ height: "100%" }}
            data={taskListItems}
            initialItemCount={initialTaskListItemCount}
            overscan={8}
            computeItemKey={computeTaskListItemKey}
            itemContent={(_, item) => renderTaskListItem(item)}
            context={taskListContext}
            rangeChanged={onTaskListRangeChanged}
            components={TASK_LIST_COMPONENTS}
          />
        </div>
      </div>

      {!collapsed ? (
        <div
          className="wb-sidebar-resizer"
          role="separator"
          aria-orientation="vertical"
          aria-label="Resize sidebar"
          onMouseDown={onSidebarResizerMouseDown}
          onDoubleClick={onResetSidebarWidth}
        />
      ) : null}
    </>
  );
}

type WorkbenchTaskMenuProps = {
  taskMenu: { taskId: string; style: React.CSSProperties } | null;
  taskMenuRef: React.RefObject<HTMLDivElement | null>;
  archiveDisabled: boolean;
  archiveLabel: string;
  markReadDisabled: boolean;
  markReadLabel: string;
  onRename: () => void;
  onToggleArchive: (event: React.MouseEvent<HTMLButtonElement>) => void;
  onToggleRead: () => void;
  onDelete: () => void;
};

export function WorkbenchTaskMenu({
  taskMenu,
  taskMenuRef,
  archiveDisabled,
  archiveLabel,
  markReadDisabled,
  markReadLabel,
  onRename,
  onToggleArchive,
  onToggleRead,
  onDelete,
}: WorkbenchTaskMenuProps) {
  if (!taskMenu) {
    return null;
  }

  return (
    <div className="wb-menu wb-task-menu" role="menu" ref={taskMenuRef} style={taskMenu.style}>
      <button type="button" className="wb-menu-item" onClick={onRename} role="menuitem">
        Rename Task
      </button>
      <button
        type="button"
        className="wb-menu-item wb-archive-confirm-trigger"
        disabled={archiveDisabled}
        onClick={onToggleArchive}
        role="menuitem"
      >
        {archiveLabel}
      </button>
      <button
        type="button"
        className="wb-menu-item"
        disabled={markReadDisabled}
        onClick={onToggleRead}
        role="menuitem"
      >
        {markReadLabel}
      </button>
      <button
        type="button"
        className="wb-menu-item wb-menu-item-danger"
        onClick={onDelete}
        role="menuitem"
      >
        Delete Task
      </button>
    </div>
  );
}

type WorkbenchArchiveConfirmProps = {
  archiveConfirm: { taskId: string; anchor: AnchorRect } | null;
  archiveConfirmStyle: React.CSSProperties | null;
  archiveConfirmRef: React.RefObject<HTMLDivElement | null>;
  archiveConfirmDontRemind: boolean;
  onArchiveConfirmDontRemindChange: (checked: boolean) => void;
  onCancel: () => void;
  onConfirm: () => void;
};

export function WorkbenchArchiveConfirm({
  archiveConfirm,
  archiveConfirmStyle,
  archiveConfirmRef,
  archiveConfirmDontRemind,
  onArchiveConfirmDontRemindChange,
  onCancel,
  onConfirm,
}: WorkbenchArchiveConfirmProps) {
  if (!archiveConfirm || !archiveConfirmStyle) {
    return null;
  }

  return (
    <div
      className="wb-archive-confirm wb-menu-tooltip"
      data-open="true"
      role="dialog"
      aria-label="Archive confirmation"
      ref={archiveConfirmRef}
      style={archiveConfirmStyle}
    >
      <div className="wb-archive-confirm-title">Archive conversation?</div>
      <div className="wb-archive-confirm-body">
        Archiving deletes the ctx-managed worktrees and branches associated with this task, including its
        live dedicated subagents. Later, you can unarchive to recreate the live task worktrees, but
        unmerged changes will be lost. Archived subagents stay archived.
        <br />
        <br />
        If you want to keep changes made here, consider instructing the primary agent to use the Merge Queue
        to bring the changes into your main branch. Otherwise, tell it to stash the changes into another
        local or remote branch for later use.
        <br />
        <br />
        In general, we recommend aggressively archiving tasks as you complete work for performance and
        organization. You can always unarchive any task later, which will restore all conversation history,
        including subagents.
      </div>
      <label className="wb-archive-confirm-toggle">
        <input
          type="checkbox"
          checked={archiveConfirmDontRemind}
          onChange={(event) => onArchiveConfirmDontRemindChange(event.target.checked)}
        />
        Don&apos;t ask me again
      </label>
      <div className="wb-archive-confirm-actions">
        <button type="button" className="wb-snackbar-btn wb-snackbar-btn-secondary" onClick={onCancel}>
          Cancel
        </button>
        <button type="button" className="wb-snackbar-btn wb-archive-confirm-danger" onClick={onConfirm}>
          Archive
        </button>
      </div>
    </div>
  );
}

type WorkbenchConversationMenuProps = {
  convoMenu: { style: React.CSSProperties } | null;
  convoMenuRef: React.RefObject<HTMLDivElement | null>;
  activeSessionId: string | null;
  activeTaskId: string | null;
  canCopyTaskId: boolean;
  copyTranscriptBusy: boolean;
  transcriptSpinnerDelayMs: number;
  canCopyWorktree: boolean;
  archiveConversationDisabled: boolean;
  onExportTranscript: () => void;
  onCopyTranscript: () => void;
  onExportSessionLog: () => void;
  onCopySessionLog: () => void;
  onCopyWorktreeLocation: () => void;
  onCopyTaskId: () => void;
  onArchiveConversation: (event: React.MouseEvent<HTMLButtonElement>) => void;
};

export function WorkbenchConversationMenu({
  convoMenu,
  convoMenuRef,
  activeSessionId,
  activeTaskId,
  canCopyTaskId,
  copyTranscriptBusy,
  transcriptSpinnerDelayMs,
  canCopyWorktree,
  archiveConversationDisabled,
  onExportTranscript,
  onCopyTranscript,
  onExportSessionLog,
  onCopySessionLog,
  onCopyWorktreeLocation,
  onCopyTaskId,
  onArchiveConversation,
}: WorkbenchConversationMenuProps) {
  if (!convoMenu) {
    return null;
  }

  return (
    <div className="wb-menu wb-convo-menu" role="menu" ref={convoMenuRef} style={convoMenu.style}>
      <button
        type="button"
        className="wb-menu-item"
        disabled={!activeSessionId}
        onClick={onExportTranscript}
        role="menuitem"
      >
        Export Transcript
      </button>
      <button
        type="button"
        className="wb-menu-item"
        disabled={!activeSessionId || copyTranscriptBusy}
        onClick={onCopyTranscript}
        role="menuitem"
      >
        <span className="wb-menu-item-row">
          <span>Copy Transcript</span>
          {copyTranscriptBusy ? (
            <span
              className="wb-task-spinner wb-menu-item-spinner"
              style={{ animationDelay: `${transcriptSpinnerDelayMs}ms` }}
              aria-hidden="true"
            />
          ) : null}
        </span>
      </button>
      <button
        type="button"
        className="wb-menu-item"
        disabled={!activeSessionId}
        onClick={onExportSessionLog}
        role="menuitem"
      >
        Export Session Log
      </button>
      <button
        type="button"
        className="wb-menu-item"
        disabled={!activeSessionId}
        onClick={onCopySessionLog}
        role="menuitem"
      >
        Copy Session Log
      </button>
      <button
        type="button"
        className="wb-menu-item"
        disabled={!canCopyWorktree}
        onClick={onCopyWorktreeLocation}
        role="menuitem"
      >
        Copy Worktree Location
      </button>
      <button
        type="button"
        className="wb-menu-item"
        disabled={!activeTaskId || !canCopyTaskId}
        onClick={onCopyTaskId}
        role="menuitem"
      >
        Copy Task ID
      </button>
      <button
        type="button"
        className="wb-menu-item wb-archive-confirm-trigger"
        disabled={archiveConversationDisabled}
        onClick={onArchiveConversation}
        role="menuitem"
      >
        Archive Conversation
      </button>
    </div>
  );
}
