import type { WorkspaceActiveSnapshotState } from "../../state/workspaceActiveSnapshotStore";
import type { SessionSupervisorSnapshot } from "../../state/sessionSupervisor";
import { useWorkspaceVcsStore } from "../../state/workspaceVcsStore";
import { useWarmSessionTranscriptRuntimes } from "./useWarmSessionTranscriptRuntimes";
import { useWorkbenchE2EBridge } from "./useWorkbenchE2EBridge";

export function useWorkbenchShellIntegrations({
  workspaceSnapshot,
  sessionSnap,
  activeTaskId,
  activeSessionId,
  foregroundTaskWorking,
  focusNewTask,
  clearDraftHarness,
  focusTask,
  toggleDiffPane,
  toggleArtifactsPane,
}: {
  workspaceSnapshot: WorkspaceActiveSnapshotState;
  sessionSnap: SessionSupervisorSnapshot;
  activeTaskId: string | null;
  activeSessionId: string | null;
  foregroundTaskWorking: boolean;
  focusNewTask: () => void;
  clearDraftHarness: () => void;
  focusTask: (taskId: string, sessionId?: string | null) => boolean;
  toggleDiffPane: () => void;
  toggleArtifactsPane: () => void;
}) {
  const workspaceVcsStore = useWorkspaceVcsStore();

  useWarmSessionTranscriptRuntimes({
    workspaceSnapshot,
    sessionSnap,
    activeSessionId,
    suppressWarmSessions: foregroundTaskWorking,
  });

  useWorkbenchE2EBridge({
    focusNewTask,
    clearDraftHarness,
    focusTask,
    getActiveTask: () => ({ taskId: activeTaskId, sessionId: activeSessionId }),
    getVcsSnapshot: (worktreeId) => workspaceVcsStore.getWorktreeVcsSnapshot(worktreeId),
    refreshVcsDetails: (worktreeId) => {
      workspaceVcsStore.ensureDetailsDemand([worktreeId]);
      workspaceVcsStore.refresh([worktreeId], "details");
      return true;
    },
    toggleDiffPane,
    toggleArtifactsPane,
  });
}
