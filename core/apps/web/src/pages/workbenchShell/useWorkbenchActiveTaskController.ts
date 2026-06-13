import { useCallback, useEffect, useMemo, useRef, useState, type MutableRefObject } from "react";

import {
  getSessionDiff,
  idToString,
  type SessionTurn,
} from "../../api/client";
import { artifactPrefetcher } from "../../state/artifactPrefetch";
import { type SessionSupervisor, useOpenSession, useSessionCacheSnapshot } from "../../state/sessionSupervisor";
import type { WorkspaceActiveSnapshotItem, WorkspaceActiveSnapshotState } from "../../state/workspaceActiveSnapshotStore";
import { useWorkspaceVcsSnapshot, useWorkspaceVcsStore } from "../../state/workspaceVcsStore";
import { findHarnessCatalogEntry } from "../../utils/harnessCatalog";
import { errorMessage } from "../../utils/errorMessage";
import { composeModelId, parseModelId } from "../../utils/modelEffort";
import { hasSessionActiveTurn } from "../../utils/sessionActivity";
import type { OptimisticTaskSummary } from "./WorkbenchPage.types";
import {
  formatWorktreeChipLabel,
  isOptimisticTask,
  parseMs,
} from "./WorkbenchPage.utils";
import { useWorkbenchActiveWorktree } from "./useWorkbenchActiveWorktree";
import { canRenderWorkbenchActiveSession } from "./workbenchTaskActivity";
import { getDiffSummaryStats, isDiffSummaryTooLarge } from "./useWorkbenchDiffPane";
import { useWorkbenchSessionActions } from "./useWorkbenchSessionActions";
import {
  resolveMeasuredSessionSwitchId,
  useWorkbenchSessionSwitchMetrics,
} from "./useWorkbenchSessionSwitchMetrics";
import { useWorkbenchConversationMenu } from "./useWorkbenchConversationMenu";
import { useWorkbenchPanelState } from "./useWorkbenchPanelState";
import { useWorkbenchWebSessions } from "./useWorkbenchWebSessions";
import { buildGitPaneModel } from "./worktreeGitPaneModel";

const compareSessionTurnOrder = (left: SessionTurn, right: SessionTurn): number => {
  const leftSeq = Number(left.start_seq ?? Number.NaN);
  const rightSeq = Number(right.start_seq ?? Number.NaN);
  if (Number.isFinite(leftSeq) && Number.isFinite(rightSeq) && leftSeq !== rightSeq) {
    return leftSeq - rightSeq;
  }
  if (Number.isFinite(leftSeq) && !Number.isFinite(rightSeq)) return -1;
  if (!Number.isFinite(leftSeq) && Number.isFinite(rightSeq)) return 1;
  const leftStartedAt = String(left.started_at ?? "");
  const rightStartedAt = String(right.started_at ?? "");
  if (leftStartedAt !== rightStartedAt) {
    return leftStartedAt.localeCompare(rightStartedAt);
  }
  return String(left.turn_id ?? "").localeCompare(String(right.turn_id ?? ""));
};

const getLatestTurnStatus = (turns: SessionTurn[] | null | undefined): SessionTurn["status"] | null => {
  let latestTurn: SessionTurn | null = null;
  for (const turn of turns ?? []) {
    if (!latestTurn || compareSessionTurnOrder(turn, latestTurn) > 0) {
      latestTurn = turn;
    }
  }
  return latestTurn?.status ?? null;
};

export const resolveWorkspaceVcsDetailDemand = ({
  diffOpen,
  activeWorktreeId,
  inventoryDemandAllowed,
}: {
  diffOpen: boolean;
  activeWorktreeId: string;
  inventoryDemandAllowed: boolean;
}): string[] => (diffOpen && activeWorktreeId && inventoryDemandAllowed ? [activeWorktreeId] : []);

type WorkspaceSnapshotStore = {
  getWorktreeRoot: (worktreeId: string) => string | null | undefined;
};

type WorkbenchActiveTaskControllerArgs = {
  workspaceId: string;
  daemonDataRoot: string | null;
  sidebarCollapsed: boolean;
  sidebarWidth: number;
  activeTaskId: string | null;
  activeTaskSummary: WorkspaceActiveSnapshotItem | OptimisticTaskSummary | null;
  activeSessionId: string | null;
  optimisticSessionIdSet: Set<string>;
  optimisticStartingTaskRef: MutableRefObject<{ primarySessionId?: string | null } | null>;
  workspaceSnapshot: WorkspaceActiveSnapshotState;
  workspaceSnapshotStore: WorkspaceSnapshotStore;
  supervisor: Pick<
    SessionSupervisor,
    | "getSnapshot"
    | "loadMoreTurns"
    | "loadSessionState"
    | "loadSubagentInvocations"
    | "setDiff"
  >;
};

export function useWorkbenchActiveTaskController({
  workspaceId,
  daemonDataRoot,
  sidebarCollapsed,
  sidebarWidth,
  activeTaskId,
  activeTaskSummary,
  activeSessionId,
  optimisticSessionIdSet,
  optimisticStartingTaskRef,
  workspaceSnapshot,
  workspaceSnapshotStore,
  supervisor,
}: WorkbenchActiveTaskControllerArgs) {
  const sessionCache = useSessionCacheSnapshot();
  const workspaceVcsStore = useWorkspaceVcsStore();
  const workspaceVcsSnapshot = useWorkspaceVcsSnapshot();
  const activeEntry = activeSessionId ? sessionCache.sessions[activeSessionId] ?? null : null;
  const activeSessionRenderable = canRenderWorkbenchActiveSession(activeEntry);
  const activeLoadErrors = activeEntry?.loadErrors;
  const activeSessionDiff = activeEntry?.diff ?? "";
  const activeTask = activeTaskSummary?.task ?? null;
  const activeTaskIsOptimistic = activeTaskSummary ? isOptimisticTask(activeTaskSummary) : false;
  const activeTaskHasAssistantMessage = Boolean(activeTask?.last_assistant_message_at);
  const activeTaskArchived = Boolean(activeTask?.archived_at);

  const {
    diffOpen,
    artifactsOpen,
    sessionsOpen,
    diffWidth,
    diffResizing,
    onSplitterMouseDown,
    terminalOpen,
    setTerminalOpen,
    terminalHeight,
    terminalResizing,
    terminalPanelRef,
    closeTerminalPanel,
    onTerminalResizerMouseDown,
    toggleDiffPane,
    toggleArtifactsPane,
    toggleSessionsPane,
    toggleTerminalPanel,
  } = useWorkbenchPanelState({
    workspaceId,
    sidebarCollapsed,
    sidebarWidth,
    activeSessionId,
  });
  const [diffContentLoading, setDiffContentLoading] = useState(false);
  const [diffContentErrorBySessionId, setDiffContentErrorBySessionId] = useState<Record<string, string | undefined>>(
    {},
  );

  const optimisticStartingSessionId = String(optimisticStartingTaskRef.current?.primarySessionId ?? "");
  const activeSessionIdValue = activeSessionId ?? "";
  const isOptimisticSessionId =
    !!activeSessionIdValue &&
    (optimisticSessionIdSet.has(activeSessionIdValue) ||
      activeSessionIdValue.startsWith("optimistic-") ||
      optimisticStartingSessionId === activeSessionIdValue);
  const openSessionId = activeSessionId && !isOptimisticSessionId ? activeSessionId : "";
  useOpenSession(openSessionId, { watchDiff: diffOpen });

  const activeDiffContentError = activeSessionId ? diffContentErrorBySessionId[activeSessionId] ?? null : null;
  const activeWorktreeId = activeEntry?.session ? idToString(activeEntry.session.worktree_id) : "";
  const activeWorktreeVcsSnapshot = activeWorktreeId
    ? workspaceVcsSnapshot.snapshotsByWorktreeId[activeWorktreeId] ?? null
    : null;
  const activeWorktreeVcsSummary: Record<string, unknown> | null = useMemo(() => {
    if (!activeWorktreeVcsSnapshot) return null;
    return { ...activeWorktreeVcsSnapshot.summary };
  }, [activeWorktreeVcsSnapshot]);
  const activeWorktreeVcsComputeState = activeWorktreeVcsSnapshot?.compute_state ?? null;
  const activeWorktreeDiffAvailable = activeWorktreeVcsSnapshot?.available !== false;
  const gitPaneModel = useMemo(() => buildGitPaneModel(activeWorktreeVcsSnapshot), [activeWorktreeVcsSnapshot]);
  const summaryWorktreeIds = useMemo(() => {
    const ids = new Set<string>();
    for (const taskId of workspaceSnapshot.activeIds) {
      const item = workspaceSnapshot.tasksById[taskId];
      const primaryWorktreeId = idToString(item?.task.primary_worktree_id);
      if (primaryWorktreeId) ids.add(primaryWorktreeId);
      for (const session of item?.sessions ?? []) {
        const worktreeId = idToString(session.session.worktree_id);
        if (worktreeId) ids.add(worktreeId);
      }
    }
    if (activeWorktreeId) ids.add(activeWorktreeId);
    return Array.from(ids).sort();
  }, [activeWorktreeId, workspaceSnapshot.activeIds, workspaceSnapshot.tasksById]);
  const detailWorktreeIds = useMemo(
    () =>
      resolveWorkspaceVcsDetailDemand({
        diffOpen,
        activeWorktreeId,
        inventoryDemandAllowed: gitPaneModel.inventoryDemandAllowed,
      }),
    [activeWorktreeId, diffOpen, gitPaneModel.inventoryDemandAllowed],
  );
  useEffect(() => {
    workspaceVcsStore.setDemand({
      summaryWorktreeIds,
      detailWorktreeIds,
    });
  }, [detailWorktreeIds, summaryWorktreeIds, workspaceVcsStore]);
  const snapshotSummaryStats = useMemo(
    () => getDiffSummaryStats(activeWorktreeVcsSummary),
    [activeWorktreeVcsSummary],
  );
  const snapshotHasCounts = snapshotSummaryStats.fileCount !== null || snapshotSummaryStats.lineCount !== null;
  const diffSummary = activeWorktreeDiffAvailable && snapshotHasCounts ? activeWorktreeVcsSummary : null;
  const diffSummaryError =
    activeWorktreeDiffAvailable && activeWorktreeVcsComputeState === "error"
      ? "Failed to compute diff summary."
      : null;
  const diffSummaryLoading =
    !diffSummaryError && activeWorktreeDiffAvailable && (!activeWorktreeVcsSnapshot || !snapshotHasCounts);
  const diffLoading = diffSummaryLoading || diffContentLoading;

  const activeWorktree = useWorkbenchActiveWorktree({
    activeTaskArchived,
    activeWorktreeId,
    daemonDataRoot,
    workspaceId,
    workspaceSnapshotStore,
  });
  const {
    activeWebSessionId,
    setActiveWebSessionId,
    activeSessionKind,
    setActiveSessionKind,
    daemonBaseUrl,
    webSessionsEnabled,
    webSessionsLoading,
    sessionSections,
  } = useWorkbenchWebSessions(activeSessionId);

  const diffSummaryStats = useMemo(() => getDiffSummaryStats(diffSummary), [diffSummary]);
  const diffSummaryCount = diffSummaryStats.fileCount;
  const diffTooLarge = useMemo(() => isDiffSummaryTooLarge(diffSummary), [diffSummary]);
  const diffTooLargeLabel = useMemo(() => {
    if (!diffTooLarge) return null;
    const details: string[] = [];
    if (diffSummaryStats.fileCount !== null) details.push(`${diffSummaryStats.fileCount} files`);
    if (diffSummaryStats.lineCount !== null) details.push(`${diffSummaryStats.lineCount} lines`);
    const suffix = details.length > 0 ? ` (${details.join(", ")})` : "";
    return `Diff too large to display${suffix}.`;
  }, [diffSummaryStats, diffTooLarge]);

  const diffSummaryReady = snapshotHasCounts || diffSummary !== null || diffSummaryError !== null;
  const diffUnavailableLabel = gitPaneModel.unavailableLabel;
  const hasDiff =
    !!diffSummaryError ||
    gitPaneModel.loading ||
    !gitPaneModel.listReady ||
    gitPaneModel.totalCount > 0 ||
    !!diffUnavailableLabel;
  const diffEmptyLabel = diffUnavailableLabel ?? (diffLoading || !diffSummaryReady ? "Loading changes..." : "No changed files.");
  const diffBadgeCount = useMemo(() => {
    return gitPaneModel.badgeCount;
  }, [gitPaneModel.badgeCount]);
  const gitStatusSignature = useMemo(() => {
    if (activeWorktreeVcsSnapshot) {
      return [
        `rev:${String(activeWorktreeVcsSnapshot.rev ?? "")}`,
        `base:${String(activeWorktreeVcsSnapshot.base_commit_sha ?? "")}`,
        `head:${String(activeWorktreeVcsSnapshot.head_commit_sha ?? "")}`,
        `available:${activeWorktreeVcsSnapshot.available === false ? "0" : "1"}`,
        `reason:${String(activeWorktreeVcsSnapshot.unavailable_reason ?? "")}`,
        `files:${String(snapshotSummaryStats.fileCount ?? "")}`,
        `adds:${String(snapshotSummaryStats.additions ?? "")}`,
        `dels:${String(snapshotSummaryStats.deletions ?? "")}`,
      ].join("|");
    }
    return "";
  }, [activeWorktreeVcsSnapshot, snapshotSummaryStats]);
  const showReviewPane = diffOpen;
  const showArtifactsPane = artifactsOpen;
  const showSessionsPane = webSessionsEnabled && sessionsOpen;
  const rightPaneOpen = showReviewPane || showArtifactsPane || showSessionsPane;
  const activeSessionCacheEntry = activeSessionId ? sessionCache.sessions[activeSessionId] : undefined;
  const artifacts = useMemo(() => {
    return activeSessionCacheEntry?.artifacts ?? [];
  }, [activeSessionCacheEntry]);
  const artifactsLoading = activeSessionCacheEntry?.artifactsLoading ?? false;
  const artifactsError =
    activeSessionCacheEntry?.stateLoaded || artifacts.length > 0 ? null : (activeLoadErrors?.state ?? null);
  const sessionLoadIssues = useMemo(() => {
    const issues: Array<{ key: "state" | "subagentInvocations"; message: string }> = [];
    if (activeLoadErrors?.state) {
      issues.push({ key: "state", message: activeLoadErrors.state });
    }
    if (activeLoadErrors?.subagentInvocations) {
      issues.push({
        key: "subagentInvocations",
        message: activeLoadErrors.subagentInvocations,
      });
    }
    return issues;
  }, [activeLoadErrors?.state, activeLoadErrors?.subagentInvocations]);

  useEffect(() => {
    if (!artifactsOpen || !activeSessionId) return;
    supervisor.loadSessionState(activeSessionId);
  }, [activeSessionId, artifactsOpen, supervisor]);

  useEffect(() => {
    artifactPrefetcher.prefetch(activeSessionId ?? null, artifacts, !activeTaskArchived);
  }, [activeSessionId, activeTaskArchived, artifacts]);

  const retryActiveSessionLoads = useCallback(() => {
    if (!activeSessionId) return;
    supervisor.loadSessionState(activeSessionId, { force: true });
    supervisor.loadSubagentInvocations(activeSessionId, { force: true });
  }, [activeSessionId, supervisor]);

  const retryArtifactsLoad = useCallback(() => {
    if (!activeSessionId) return;
    supervisor.loadSessionState(activeSessionId, { force: true });
  }, [activeSessionId, supervisor]);

  const diffContentInFlightRef = useRef<Map<string, Promise<void>>>(new Map());
  const diffRefreshTimerRef = useRef<number | null>(null);
  const diffRefreshSignatureRef = useRef<Map<string, string>>(new Map());

  const refreshDiff = useCallback(
    async (sessionId: string) => {
      if (!sessionId) return;
      const currentOptimisticSessionId = String(optimisticStartingTaskRef.current?.primarySessionId ?? "");
      if (
        optimisticSessionIdSet.has(sessionId) ||
        (currentOptimisticSessionId && currentOptimisticSessionId === sessionId)
      ) {
        return;
      }
      if (sessionId !== activeSessionId) return;
      if (!activeWorktreeDiffAvailable) {
        setDiffContentErrorBySessionId((prev) => ({ ...prev, [sessionId]: undefined }));
        supervisor.setDiff(sessionId, "");
        return;
      }
      if (gitPaneModel.totalCount <= 0 || !activeWorktreeVcsSummary) {
        setDiffContentErrorBySessionId((prev) => ({ ...prev, [sessionId]: undefined }));
        supervisor.setDiff(sessionId, "");
        return;
      }
      if (isDiffSummaryTooLarge(activeWorktreeVcsSummary)) {
        setDiffContentErrorBySessionId((prev) => ({ ...prev, [sessionId]: undefined }));
        supervisor.setDiff(sessionId, "");
        return;
      }
      const existing = diffContentInFlightRef.current.get(sessionId);
      if (existing) return existing;
      setDiffContentLoading(true);
      setDiffContentErrorBySessionId((prev) => ({ ...prev, [sessionId]: undefined }));
      const request = (async () => {
        try {
          const response = await getSessionDiff(sessionId);
          if (response.available === false) {
            supervisor.setDiff(sessionId, "");
            setDiffContentErrorBySessionId((prev) => ({ ...prev, [sessionId]: undefined }));
            return;
          }
          supervisor.setDiff(sessionId, response.diff ?? "");
          setDiffContentErrorBySessionId((prev) => ({ ...prev, [sessionId]: undefined }));
        } catch (error: unknown) {
          supervisor.setDiff(sessionId, "");
          const detail = errorMessage(error);
          const message = detail ? `Failed to load diff content: ${detail}` : "Failed to load diff content.";
          setDiffContentErrorBySessionId((prev) => ({ ...prev, [sessionId]: message }));
        }
      })().finally(() => {
        diffContentInFlightRef.current.delete(sessionId);
        setDiffContentLoading(false);
      });
      diffContentInFlightRef.current.set(sessionId, request);
      return request;
    },
    [
      activeSessionId,
      activeWorktreeDiffAvailable,
      activeWorktreeVcsSummary,
      gitPaneModel.totalCount,
      optimisticSessionIdSet,
      optimisticStartingTaskRef,
      supervisor,
    ],
  );

  useEffect(() => {
    if (!diffOpen || !activeSessionId) return;
    void refreshDiff(activeSessionId);
  }, [activeSessionId, diffOpen, refreshDiff]);

  useEffect(() => {
    if (!activeSessionId) return;
    if (!gitStatusSignature) return;
    const prevSignature = diffRefreshSignatureRef.current.get(activeSessionId) ?? "";
    if (gitStatusSignature === prevSignature) return;
    diffRefreshSignatureRef.current.set(activeSessionId, gitStatusSignature);
    if (diffRefreshTimerRef.current) {
      window.clearTimeout(diffRefreshTimerRef.current);
    }
    diffRefreshTimerRef.current = window.setTimeout(() => {
      if (diffOpen) {
        void refreshDiff(activeSessionId);
      }
    }, 400);
    return () => {
      if (diffRefreshTimerRef.current) {
        window.clearTimeout(diffRefreshTimerRef.current);
        diffRefreshTimerRef.current = null;
      }
    };
  }, [activeSessionId, diffOpen, gitStatusSignature, refreshDiff]);

  const worktreeChip = useMemo(() => {
    const session = activeEntry?.session ?? null;
    const worktreeRoot = String(activeWorktree?.root_path ?? "");
    const executionEnvironment = String(session?.execution_environment ?? "").trim();
    const worktreePath = worktreeRoot;
    const worktreeLabel = formatWorktreeChipLabel({
      worktreePath,
      worktreeId: activeWorktreeId,
      executionEnvironment,
    });

    return {
      worktreeLabel,
      worktreePath,
      canCopyWorktree: Boolean(worktreePath),
      canOpenTerminal: Boolean(worktreePath),
      copyPath: worktreePath,
    };
  }, [activeEntry, activeWorktree?.root_path, activeWorktreeId]);

  const singleSessionHeader = useMemo(() => {
    const session = activeEntry?.session ?? null;
    if (!session) return null;
    const parsedModel = parseModelId(
      composeModelId(session?.model_id ?? "", session?.reasoning_effort ?? null),
    );
    const harness =
      findHarnessCatalogEntry(session?.provider_id)?.label ??
      (session?.provider_id ?? "Provider");

    const lastIso = (() => {
      if (!activeEntry) return null;
      let bestIso: string | null = activeEntry.session?.updated_at ?? null;
      let bestMs = bestIso ? parseMs(bestIso) ?? -1 : -1;
      for (const message of activeEntry.messages ?? []) {
        const ms = parseMs(message.created_at);
        if (ms !== null && ms >= bestMs) {
          bestMs = ms;
          bestIso = message.created_at;
        }
      }
      for (const event of activeEntry.events ?? []) {
        const ms = parseMs(event.created_at);
        if (ms !== null && ms >= bestMs) {
          bestMs = ms;
          bestIso = event.created_at;
        }
      }
      return bestIso;
    })();

    return {
      title: activeTask?.title ?? "Conversation",
      lastIso,
      harness,
      modelBase: parsedModel.base || String(session?.model_id ?? ""),
      effort: parsedModel.effort,
    };
  }, [activeEntry, activeTask?.title]);

  const singleSessionHeaderForRender = useMemo(() => {
    if (singleSessionHeader) return singleSessionHeader;
    if (!activeTaskId) return null;
    return {
      title: activeTask?.title ?? "Conversation",
      lastIso: null,
      harness: "",
      modelBase: "",
      effort: "",
    };
  }, [activeTask?.title, activeTaskId, singleSessionHeader]);

  const showSingleSessionHeader = Boolean(activeTaskId && singleSessionHeaderForRender);
  const {
    copyTranscriptBusy,
    transcriptNotice,
    setTranscriptNotice,
    transcriptSpinnerDelayMs,
    worktreeCopied,
    copyWorktreeLocation,
    copyTaskId,
    openWorktreeTerminal,
    exportSessionLog,
    copySessionLog,
    exportTranscript,
    copyTranscript,
  } = useWorkbenchSessionActions({
    activeEntry,
    activeSessionId,
    activeTaskId,
    activeWorktreeId,
    singleSessionTitle: singleSessionHeader?.title ?? null,
    worktreePath: worktreeChip.worktreePath,
    canCopyWorktree: worktreeChip.canCopyWorktree,
    canCopyTaskId: Boolean(activeTaskId) && !activeTaskIsOptimistic,
    canOpenTerminal: worktreeChip.canOpenTerminal,
    terminalPanelRef,
    setTerminalOpen,
    getSupervisorSnapshot: () => supervisor.getSnapshot(),
    loadMoreTurns: async (sessionId) => {
      await supervisor.loadMoreTurns(sessionId);
    },
  });

  const dismissTranscriptNotice = useCallback(() => {
    setTranscriptNotice(null);
  }, [setTranscriptNotice]);

  const canInterruptSession =
    Boolean(activeSessionId) &&
    hasSessionActiveTurn(activeEntry?.activity, getLatestTurnStatus(activeEntry?.turns ?? null));

  const {
    convoMenu,
    convoMenuRef,
    openConvoMenu,
    closeConvoMenu,
  } = useWorkbenchConversationMenu();

  return {
    activeTask,
    activeTaskArchived,
    activeTaskIsOptimistic,
    activeTaskHasAssistantMessage,
    activeSessionRenderable,
    showSingleSessionHeader,
    singleSessionHeaderForRender,
    worktreeChip,
    worktreeCopied,
    copyTranscriptBusy,
    transcriptNotice,
    dismissTranscriptNotice,
    transcriptSpinnerDelayMs,
    copyWorktreeLocation,
    copyTaskId,
    openWorktreeTerminal,
    exportSessionLog,
    copySessionLog,
    exportTranscript,
    copyTranscript,
    canInterruptSession,
    diffOpen,
    artifactsOpen,
    sessionsOpen,
    showReviewPane,
    showArtifactsPane,
    showSessionsPane,
    rightPaneOpen,
    diffWidth,
    diffResizing,
    onSplitterMouseDown,
    terminalOpen,
    terminalHeight,
    terminalResizing,
    terminalPanelRef,
    closeTerminalPanel,
    onTerminalResizerMouseDown,
    toggleDiffPane,
    toggleArtifactsPane,
    toggleSessionsPane,
    toggleTerminalPanel,
    sessionLoadIssues,
    retryActiveSessionLoads,
    activeSessionDiff,
    activeDiffContentError,
    activeWebSessionId,
    setActiveWebSessionId,
    activeSessionKind,
    setActiveSessionKind,
    daemonBaseUrl,
    webSessionsEnabled,
    webSessionsLoading,
    sessionSections,
    hasDiff,
    diffSummaryError,
    diffTooLarge,
    diffTooLargeLabel,
    diffEmptyLabel,
    artifacts,
    artifactsLoading,
    artifactsError,
    retryArtifactsLoad,
    convoMenu,
    convoMenuRef,
    openConvoMenu,
    closeConvoMenu,
    gitPaneModel,
    diffLoading,
    diffBadgeCount,
  };
}
