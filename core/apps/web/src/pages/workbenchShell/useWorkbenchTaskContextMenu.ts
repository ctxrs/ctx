import React, { useCallback, useEffect, useRef, useState } from "react";

import type { WorkspaceActiveSnapshotItem } from "../../state/workspaceActiveSnapshotStore";
import type { AnchorRect } from "./WorkbenchPage.types";

type TaskMenuState = {
  taskId: string;
  style: React.CSSProperties;
};

type Args = {
  tasksById: Record<string, WorkspaceActiveSnapshotItem>;
  beginRenameTask: (taskId: string) => void;
  isArchivePending: (taskId: string | null | undefined) => boolean;
  isTaskUnread: (taskId: string) => boolean;
  onToggleArchive: (
    taskId: string,
    nextArchived: boolean,
    anchor?: AnchorRect | null,
  ) => Promise<void>;
  markTaskRead: (taskId: string) => Promise<void>;
  markTaskUnread: (taskId: string) => Promise<void>;
  onDeleteTask: (taskId: string) => Promise<void>;
};

export function useWorkbenchTaskContextMenu({
  tasksById,
  beginRenameTask,
  isArchivePending,
  isTaskUnread,
  onToggleArchive,
  markTaskRead,
  markTaskUnread,
  onDeleteTask,
}: Args) {
  const [taskMenu, setTaskMenu] = useState<TaskMenuState | null>(null);
  const taskMenuRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    const onPointerDown = (event: PointerEvent) => {
      if (!taskMenu) return;
      const element = event.target as HTMLElement | null;
      if (element && (element.closest(".wb-task-menu") || element.closest(".wb-task-menu-trigger"))) return;
      setTaskMenu(null);
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") setTaskMenu(null);
    };
    window.addEventListener("pointerdown", onPointerDown);
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("pointerdown", onPointerDown);
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [taskMenu]);

  const openTaskMenu = useCallback((taskId: string, opts: { triggerEl: HTMLElement } | { x: number; y: number }) => {
    const baseLeft =
      "triggerEl" in opts
        ? opts.triggerEl.getBoundingClientRect().left
        : Math.max(8, Math.min(opts.x, window.innerWidth - 8));
    const baseTop =
      "triggerEl" in opts
        ? opts.triggerEl.getBoundingClientRect().bottom + 6
        : Math.max(8, Math.min(opts.y, window.innerHeight - 8));
    const left = Math.min(baseLeft, window.innerWidth - 240);
    const top = Math.min(baseTop, window.innerHeight - 260);
    setTaskMenu((prev) => (prev?.taskId === taskId ? null : { taskId, style: { left, top } }));
  }, []);

  const taskMenuArchiveDisabled = taskMenu ? isArchivePending(taskMenu.taskId) : true;
  const taskMenuArchiveLabel =
    taskMenu && tasksById[taskMenu.taskId]?.task.archived_at ? "Unarchive" : "Archive";
  const taskMenuMarkReadDisabled = taskMenu
    ? !Boolean(tasksById[taskMenu.taskId]?.task.last_assistant_message_at)
    : true;
  const taskMenuMarkReadLabel =
    taskMenu && isTaskUnread(taskMenu.taskId) ? "Mark as Read" : "Mark as Unread";

  const onTaskMenuRename = useCallback(() => {
    if (!taskMenu) return;
    const taskId = taskMenu.taskId;
    setTaskMenu(null);
    beginRenameTask(taskId);
  }, [beginRenameTask, taskMenu]);

  const onTaskMenuToggleArchive = useCallback(
    (event: React.MouseEvent<HTMLButtonElement>) => {
      if (!taskMenu) return;
      const taskId = taskMenu.taskId;
      const summary = tasksById[taskId];
      const nextArchived = !summary?.task.archived_at;
      const anchor = event.currentTarget.getBoundingClientRect();
      void onToggleArchive(taskId, nextArchived, anchor).catch(() => {});
      setTaskMenu(null);
    },
    [onToggleArchive, taskMenu, tasksById],
  );

  const onTaskMenuToggleRead = useCallback(() => {
    if (!taskMenu) return;
    const taskId = taskMenu.taskId;
    const unread = isTaskUnread(taskId);
    setTaskMenu(null);
    if (unread) {
      void markTaskRead(taskId);
      return;
    }
    void markTaskUnread(taskId);
  }, [isTaskUnread, markTaskRead, markTaskUnread, taskMenu]);

  const onTaskMenuDelete = useCallback(() => {
    if (!taskMenu) return;
    const taskId = taskMenu.taskId;
    setTaskMenu(null);
    void onDeleteTask(taskId);
  }, [onDeleteTask, taskMenu]);

  return {
    taskMenu,
    taskMenuRef,
    openTaskMenu,
    taskMenuArchiveDisabled,
    taskMenuArchiveLabel,
    taskMenuMarkReadDisabled,
    taskMenuMarkReadLabel,
    onTaskMenuRename,
    onTaskMenuToggleArchive,
    onTaskMenuToggleRead,
    onTaskMenuDelete,
  };
}
