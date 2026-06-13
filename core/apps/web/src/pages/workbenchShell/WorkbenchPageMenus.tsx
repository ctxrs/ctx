import { WorkbenchArchiveConfirm, WorkbenchConversationMenu, WorkbenchTaskMenu } from "./WorkbenchShellChrome";
import type { useWorkbenchActiveTaskController } from "./useWorkbenchActiveTaskController";
import type { useWorkbenchTaskListController } from "./useWorkbenchTaskListController";

type ActiveTaskController = ReturnType<typeof useWorkbenchActiveTaskController>;
type TaskListController = ReturnType<typeof useWorkbenchTaskListController>;

type Props = {
  activeTaskController: ActiveTaskController;
  taskListController: TaskListController;
  activeTaskId: string | null;
  activeSessionId: string | null;
};

export function WorkbenchPageMenus({
  activeTaskController,
  taskListController,
  activeTaskId,
  activeSessionId,
}: Props) {
  return (
    <>
      <WorkbenchTaskMenu
        taskMenu={taskListController.taskMenu}
        taskMenuRef={taskListController.taskMenuRef}
        archiveDisabled={taskListController.taskMenuArchiveDisabled}
        archiveLabel={taskListController.taskMenuArchiveLabel}
        markReadDisabled={taskListController.taskMenuMarkReadDisabled}
        markReadLabel={taskListController.taskMenuMarkReadLabel}
        onRename={taskListController.onTaskMenuRename}
        onToggleArchive={taskListController.onTaskMenuToggleArchive}
        onToggleRead={taskListController.onTaskMenuToggleRead}
        onDelete={taskListController.onTaskMenuDelete}
      />

      <WorkbenchArchiveConfirm
        archiveConfirm={taskListController.archiveConfirm}
        archiveConfirmStyle={taskListController.archiveConfirmStyle}
        archiveConfirmRef={taskListController.archiveConfirmRef}
        archiveConfirmDontRemind={taskListController.archiveConfirmDontRemind}
        onArchiveConfirmDontRemindChange={taskListController.setArchiveConfirmDontRemind}
        onCancel={taskListController.cancelArchiveConfirm}
        onConfirm={() => {
          void taskListController.confirmArchive();
        }}
      />

      <WorkbenchConversationMenu
        convoMenu={activeTaskController.convoMenu}
        convoMenuRef={activeTaskController.convoMenuRef}
        activeSessionId={activeSessionId}
        activeTaskId={activeTaskId}
        canCopyTaskId={!activeTaskController.activeTaskIsOptimistic}
        copyTranscriptBusy={activeTaskController.copyTranscriptBusy}
        transcriptSpinnerDelayMs={activeTaskController.transcriptSpinnerDelayMs}
        canCopyWorktree={activeTaskController.worktreeChip.canCopyWorktree}
        archiveConversationDisabled={
          !activeTaskId ||
          Boolean(activeTaskController.activeTask?.archived_at) ||
          taskListController.isArchivePending(activeTaskId)
        }
        onExportTranscript={() => {
          activeTaskController.closeConvoMenu();
          void activeTaskController.exportTranscript();
        }}
        onCopyTranscript={() => {
          activeTaskController.closeConvoMenu();
          void activeTaskController.copyTranscript();
        }}
        onExportSessionLog={() => {
          activeTaskController.closeConvoMenu();
          void activeTaskController.exportSessionLog();
        }}
        onCopySessionLog={() => {
          activeTaskController.closeConvoMenu();
          void activeTaskController.copySessionLog();
        }}
        onCopyWorktreeLocation={() => {
          activeTaskController.closeConvoMenu();
          void activeTaskController.copyWorktreeLocation();
        }}
        onCopyTaskId={() => {
          activeTaskController.closeConvoMenu();
          void activeTaskController.copyTaskId();
        }}
        onArchiveConversation={(event) => {
          if (!activeTaskId) return;
          activeTaskController.closeConvoMenu();
          const anchor = event.currentTarget.getBoundingClientRect();
          taskListController.onToggleArchive(activeTaskId, true, anchor).catch(() => {});
        }}
      />
    </>
  );
}
