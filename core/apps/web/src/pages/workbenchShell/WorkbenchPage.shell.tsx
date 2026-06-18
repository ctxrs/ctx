import React, { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useNavigate } from "react-router-dom";
import {
  MessageAttachment,
  interruptSession,
} from "../../api/client";
import type { WorkspaceActiveSnapshotEvent } from "@ctx/types";
import { useSessionCacheSnapshot, useSessionSupervisor } from "../../state/sessionSupervisor";
import { useDaemonConnection } from "../../api/useDaemonConnection";
import { type DraftHarness, type WorkbenchModeId } from "../../components/WorkbenchComposer";
import type { SlashCommandDescriptor } from "../../state/useComposerAutocomplete";
import { isDesktopApp } from "../../utils/desktop";
import { isMobileShellApp } from "../../utils/runtime";
import { useDictationController } from "../../utils/useDictationController";
import { NEW_TASK_DRAFT_KEY, useActiveWorkbenchIds, useNewTaskDraft, useWorkbenchShellSnapshot, useWorkbenchStore } from "../../workbench/store";
import { useWorkspaceActiveSnapshotEvents, useWorkspaceActiveSnapshotSnapshot, useWorkspaceActiveSnapshotStore } from "../../state/workspaceActiveSnapshotStore";
import { useWorkspaceAgentWorkGraph, useWorkspaceAgentWorkStore } from "../../state/workspaceAgentWorkStore";
import { usePluginRegistry } from "../../state/pluginRegistryStore";
import { useHarnessAuthenticationController } from "../settings/hooks/useHarnessAuthenticationController";
import { HarnessAuthenticationSectionView } from "../settings/sections/HarnessAuthenticationSection";
import { useWorkbenchDragDropAttachments } from "./useWorkbenchDragDropAttachments";
import { useWorkbenchOptimisticTasks } from "./useWorkbenchOptimisticTasks";
import { useWorkbenchProviders } from "./useWorkbenchProviders";
import { WorkbenchPageShellView } from "./WorkbenchPageShellView";
import { useWorkbenchChromeIntegration } from "./useWorkbenchChromeIntegration";
import { useWorkbenchShellLayout } from "./useWorkbenchShellLayout";
import { useWorkbenchSessionBridge } from "./useWorkbenchSessionBridge";
import { useWorkbenchDesktopAttention } from "./useWorkbenchDesktopAttention";
import { useWorkbenchDesktopTaskRouting } from "./useWorkbenchDesktopTaskRouting";
import { useWorkbenchTaskCreation } from "./useWorkbenchTaskCreation";
import { useWorkbenchTaskListController } from "./useWorkbenchTaskListController";
import { useWorkbenchActiveTaskController } from "./useWorkbenchActiveTaskController";
import { useWorkbenchComposerHarnessAuth } from "./useWorkbenchComposerHarnessAuth";
import { useWorkbenchDebugIds } from "./useWorkbenchDebugIds";
import { useWorkbenchDraftHarnessSelection } from "./useWorkbenchDraftHarnessSelection";
import { useWorkbenchE2EFocusBridge } from "./useWorkbenchE2EFocusBridge";
import { useWorkbenchNavigationTarget } from "./useWorkbenchNavigationTarget";
import { useWorkbenchShellIntegrations } from "./useWorkbenchShellIntegrations";
import type { OptimisticFocus } from "./WorkbenchPage.types";
import { appendSegment } from "./WorkbenchPage.utils";
import { useWorkbenchTaskReadActions } from "./useWorkbenchTaskReadActions";
import { useWorkbenchWorkspaceMetadata } from "./useWorkbenchWorkspaceMetadata";
import { resolveWorkspaceBootstrapGateState } from "../workspaceBootstrapGate";
import { getProviderOwnerScopeKeyOrNull } from "../../state/providerScopeAdapters";
import { projectPluginSlashCommands } from "./pluginCommandProjection";
import { resolvePluginCommandMessage } from "./pluginCommandInvocation";

export function WorkbenchPageInner({ workspaceId }: { workspaceId: string }) {
  const navigate = useNavigate();
  const supervisor = useSessionSupervisor();
  const sessionSnap = useSessionCacheSnapshot();
  const workbenchStore = useWorkbenchStore();
  const daemonConnection = useDaemonConnection();
  const workspaceSnapshotStore = useWorkspaceActiveSnapshotStore();
  const workspaceSnapshot = useWorkspaceActiveSnapshotSnapshot();
  const agentWorkStore = useWorkspaceAgentWorkStore();
  const agentWorkGraph = useWorkspaceAgentWorkGraph();
  const pluginRegistryState = usePluginRegistry();
  const agentWorkRefreshTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const tasksById = workspaceSnapshot.tasksById;
  const workbenchSnap = useWorkbenchShellSnapshot();
  const mobileShell = isMobileShellApp();

  const [optimisticFocus, setOptimisticFocus] = useState<OptimisticFocus | null>(null);
  const { taskId: activeTaskIdFromTab, sessionId: activeSessionIdFromTab } = useActiveWorkbenchIds();
  const navToken = workbenchStore.getNavToken();
  const optimisticFocusActive = Boolean(optimisticFocus && navToken === optimisticFocus.navToken);
  const activeTaskId =
    activeTaskIdFromTab ?? (optimisticFocusActive && optimisticFocus ? optimisticFocus.taskId : null);
  const activeSessionIdFromTabResolved =
    activeSessionIdFromTab ?? (optimisticFocusActive && optimisticFocus ? optimisticFocus.sessionId : null);
  const { value: newTaskDraft, setValue: setNewTaskDraft } = useNewTaskDraft();
  const draftPrompt = newTaskDraft.text;
  const draftMode = newTaskDraft.modeId;
  const { workspace, daemonDataRoot } = useWorkbenchWorkspaceMetadata({ navigate, workspaceId });
  const manualDemoHarnessSelection = useMemo(() => {
    const params = new URLSearchParams(window.location.search);
    return params.get("ctxDemoManualHarness") === "1";
  }, []);

  useEffect(
    () => () => {
      if (agentWorkRefreshTimerRef.current) {
        clearTimeout(agentWorkRefreshTimerRef.current);
        agentWorkRefreshTimerRef.current = null;
      }
    },
    [agentWorkStore],
  );

  const refreshAgentWorkForWorkspaceEvent = useCallback(
    (event: WorkspaceActiveSnapshotEvent) => {
      switch (event.type) {
        case "active_task_upsert":
        case "session_summary":
        case "session_summary_delta":
        case "session_head_seed":
          void agentWorkStore.refresh().catch(() => {});
          if (agentWorkRefreshTimerRef.current) {
            clearTimeout(agentWorkRefreshTimerRef.current);
          }
          agentWorkRefreshTimerRef.current = setTimeout(() => {
            agentWorkRefreshTimerRef.current = null;
            void agentWorkStore.refresh().catch(() => {});
          }, 750);
          break;
        default:
          break;
      }
    },
    [agentWorkStore],
  );

  useWorkspaceActiveSnapshotEvents(refreshAgentWorkForWorkspaceEvent);

  useEffect(() => {
    if (!optimisticFocus) return;
    if (activeTaskIdFromTab) {
      setOptimisticFocus(null);
      return;
    }
    if (!optimisticFocusActive) {
      setOptimisticFocus(null);
    }
  }, [activeTaskIdFromTab, optimisticFocus, optimisticFocusActive]);

  const setDraftPrompt = useCallback(
    (text: string) => setNewTaskDraft({ text, modeId: newTaskDraft.modeId }),
    [newTaskDraft.modeId, setNewTaskDraft],
  );
  const setDraftMode = useCallback(
    (modeId: WorkbenchModeId) => setNewTaskDraft({ text: newTaskDraft.text, modeId }),
    [newTaskDraft.text, setNewTaskDraft],
  );

  const [newComposerElement, setNewComposerElement] = useState<HTMLDivElement | null>(null);
  const [draftHarness, setDraftHarness] = useState<DraftHarness | null>(null);
  const [startError, setStartError] = useState<string | null>(null);
  const [draftAttachments, setDraftAttachments] = useState<MessageAttachment[]>([]);
  const composerHarnessAuth = useHarnessAuthenticationController({
    workspaceId,
    enabled: true,
  });

  const focusNewTask = useCallback(() => {
    workbenchStore.focusNewTask();
  }, [workbenchStore]);

  const {
    sidebarCollapsed,
    setSidebarCollapsed,
    sidebarWidth,
    setSidebarWidth,
    sidebarResizing,
    onSidebarResizerMouseDown,
  } = useWorkbenchShellLayout({
    workspaceId,
    focusNewTask,
    mobileMode: mobileShell,
  });

  const clearDraftHarness = useCallback(() => {
    setDraftHarness(null);
  }, []);

  const focusTask = useCallback(
    (taskId: string, sessionId?: string | null) => {
      workbenchStore.focusTask(taskId, sessionId);
      if (mobileShell) {
        setSidebarCollapsed(true);
      }
      return true;
    },
    [mobileShell, setSidebarCollapsed, workbenchStore],
  );

  useWorkbenchNavigationTarget();

  const {
    optimisticTasks,
    setOptimisticTasks,
    optimisticStartingTaskRef,
    optimisticTasksById,
    optimisticSessionIdSet,
    optimisticFailureBySessionId,
    activeTaskSummary,
  } = useWorkbenchOptimisticTasks({
    activeTaskId,
    activeTaskIdFromTab,
    tasksById,
  });

  const {
    dictationRecording,
    dictationError,
    dictationDebugText,
    dictationOnboarding,
    dismissDictationOnboarding,
    backDictationOnboarding,
    chooseDictationOnboardingLocal,
    chooseDictationOnboardingCloud,
    updateDictationOnboardingCloud,
    submitDictationOnboardingLocal,
    submitDictationOnboardingCloud,
    startDictation,
    stopDictation,
  } = useDictationController({
    text: draftPrompt,
    setText: setDraftPrompt,
    appendSegment,
  });

  const {
    providersById,
    defaultProviderId,
    providerInstallsById,
    providerOptions,
    bootstrapState: providerBootstrapState,
    bootstrapError: providerBootstrapError,
    installAllBusy,
    installProviderFromMenu,
    cancelProviderInstallFromMenu,
    installAllProvidersFromMenu,
    updateProvidersFromMenu,
    ensureProviderAuthSummary,
    refreshBootstrap,
  } = useWorkbenchProviders({
    workspaceId,
    setDraftHarness,
    onStartError: setStartError,
  });

  const { setSingleDraftHarness } = useWorkbenchDraftHarnessSelection({
    activeTaskId,
    workspaceId,
    draftHarness,
    setDraftHarness,
    providersById,
    providerOptions,
    ensureProviderAuthSummary,
    manualDemoHarnessSelection,
  });

  const { requestHarnessAuthFromComposer } = useWorkbenchComposerHarnessAuth({
    activeTaskId,
    controller: composerHarnessAuth,
    ensureProviderAuthSummary,
    providerOptions,
    setSingleDraftHarness,
  });

  const { dropActive } = useWorkbenchDragDropAttachments({
    scopeElement: newComposerElement,
    activeTaskId,
    setDraftAttachments,
    onError: setStartError,
  });
  const resolveInitialPluginPrompt = useCallback(
    (text: string) =>
      resolvePluginCommandMessage({
        text,
        registry: pluginRegistryState.registry,
        workspaceId,
      }),
    [pluginRegistryState.registry, workspaceId],
  );

  const { startBlockedReason, startNewTask } = useWorkbenchTaskCreation({
    workspaceId,
    draftPrompt,
    setNewTaskDraft,
    draftAttachments,
    setDraftAttachments,
    draftHarness,
    providersById,
    ensureProviderAuthSummary,
    dictationRecording,
    stopDictation,
    resolveInitialPrompt: resolveInitialPluginPrompt,
    focusTask,
    workbenchStore,
    optimisticStartingTaskRef,
    setOptimisticTasks,
    setOptimisticFocus,
    supervisor,
    newTaskDraftKey: NEW_TASK_DRAFT_KEY,
    onStartError: setStartError,
  });

  const { markTaskRead, markTaskUnread } = useWorkbenchTaskReadActions(workspaceSnapshotStore);

  const {
    sessions,
    activeSessionId,
    taskLiveInfo,
    providerIdsByTaskFromSessions,
    isTaskUnread,
  } = useWorkbenchSessionBridge({
    activeTaskId,
    activeSessionIdFromTab: activeSessionIdFromTabResolved,
    activeTaskSummary,
    tasksById,
    workspaceSnapshot,
    sessionSnap,
    optimisticTasks,
    optimisticTasksById,
    supervisor,
    workbenchStore,
    workspaceSnapshotStore,
    markTaskRead,
  });

  useWorkbenchDesktopAttention({
    workspaceId,
    activeTaskIds: workspaceSnapshot.activeIds,
    tasksById,
    taskLiveInfo,
  });

  useWorkbenchDesktopTaskRouting({
    activeSessionId,
    activeTaskId,
    connection: daemonConnection,
    windowState: workbenchSnap.window,
    workbenchStore,
    workspaceId,
    workspaceName: workspace?.name ?? null,
  });

  const taskListController = useWorkbenchTaskListController({
    workspaceId,
    activeTaskId,
    activeSessionId,
    tasksById,
    workspaceSnapshot,
    workspaceSnapshotStore,
    optimisticTasks,
    setOptimisticTasks,
    optimisticTasksById,
    taskLiveInfo,
    providerIdsByTaskFromSessions,
    agentWorkGraph,
    sessionEntries: sessionSnap.sessions,
    isTaskUnread,
    focusTask,
    focusNewTask,
    markTaskRead,
    markTaskUnread,
    supervisor,
    workbenchStore,
  });

  const activeTaskController = useWorkbenchActiveTaskController({
    workspaceId,
    daemonDataRoot,
    sidebarCollapsed,
    sidebarWidth,
    activeTaskId,
    activeTaskSummary,
    activeSessionId,
    optimisticSessionIdSet,
    optimisticStartingTaskRef,
    agentWorkGraph,
    workspaceSnapshot,
    workspaceSnapshotStore,
    supervisor,
  });

  useWorkbenchShellIntegrations({
    workspaceSnapshot,
    sessionSnap,
    activeTaskId,
    activeSessionId,
    foregroundTaskWorking: Boolean(activeTaskId && taskLiveInfo.workingByTask.has(activeTaskId)),
    focusNewTask,
    clearDraftHarness,
    focusTask,
    toggleDiffPane: () => activeTaskController.toggleDiffPane("unknown"),
    toggleArtifactsPane: () => activeTaskController.toggleArtifactsPane("unknown"),
  });

  const { showDebugIds, debugIdLabel, copyDebugIds } = useWorkbenchDebugIds({
    activeSessionId,
    activeTaskId,
    workspaceId,
  });

  const optimisticFailure = activeSessionId
    ? optimisticFailureBySessionId[activeSessionId] ?? null
    : null;
  const canToggleArchive =
    Boolean(activeTaskId) && !taskListController.isArchivePending(activeTaskId);

  const focusTaskSearch = useCallback(() => {
    if (!taskListController.taskSearchRef.current) return false;
    taskListController.taskSearchRef.current.focus();
    taskListController.taskSearchRef.current.select();
    return true;
  }, [taskListController.taskSearchRef]);

  const toggleSidebar = useCallback(() => {
    setSidebarCollapsed((prev) => !prev);
  }, []);

  useWorkbenchE2EFocusBridge(workbenchStore);

  const {
    desktopUi,
    desktopStorageNotice,
    setDesktopStorageNotice,
    useHtmlTopbar,
  } = useWorkbenchChromeIntegration({
    enabled: isDesktopApp(),
    workspaceId,
    workspaceName: workspace?.name ?? null,
    state: {
      activeSessionId,
      activeTaskId,
      activeTaskArchived: activeTaskController.activeTaskArchived,
      activeTaskHasAssistantMessage: activeTaskController.activeTaskHasAssistantMessage,
      activeTaskIsOptimistic: activeTaskController.activeTaskIsOptimistic,
      canToggleArchive,
      canInterruptSession: activeTaskController.canInterruptSession,
      copyTranscriptBusy: activeTaskController.copyTranscriptBusy,
      sidebarCollapsed,
      diffOpen: activeTaskController.diffOpen,
      artifactsOpen: activeTaskController.artifactsOpen,
      sessionsOpen: activeTaskController.sessionsOpen,
      terminalOpen: activeTaskController.terminalOpen,
      webSessionsEnabled: activeTaskController.webSessionsEnabled,
      worktreeCanCopy: activeTaskController.worktreeChip.canCopyWorktree,
      worktreeCanOpenTerminal: activeTaskController.worktreeChip.canOpenTerminal,
      isTaskUnread,
    },
    handlers: {
      exportTranscript: activeTaskController.exportTranscript,
      exportSessionLog: activeTaskController.exportSessionLog,
      focusTaskSearch,
      toggleSidebar,
      toggleDiffPane: () => activeTaskController.toggleDiffPane("menu_command"),
      toggleArtifactsPane: () => activeTaskController.toggleArtifactsPane("menu_command"),
      toggleSessionsPane: () => activeTaskController.toggleSessionsPane("menu_command"),
      toggleTerminalPanel: () => activeTaskController.toggleTerminalPanel("menu_command"),
      focusNewTask,
      beginRenameTask: taskListController.beginRenameTask,
      toggleArchiveTask: (taskId, nextArchived) => {
        void taskListController.onToggleArchive(taskId, nextArchived, null).catch(() => {});
      },
      toggleTaskRead: (taskId, unread) => {
        if (unread) {
          void taskListController.markTaskRead(taskId);
          return;
        }
        void taskListController.markTaskUnread(taskId);
      },
      deleteTask: taskListController.onDeleteTask,
      copyTranscript: activeTaskController.copyTranscript,
      copySessionLog: activeTaskController.copySessionLog,
      copyWorktreeLocation: activeTaskController.copyWorktreeLocation,
      copyTaskId: activeTaskController.copyTaskId,
      openWorktreeTerminal: activeTaskController.openWorktreeTerminal,
      interruptSession: (sessionId) => {
        void interruptSession(sessionId).catch(() => {});
      },
    },
  });

  const dismissDesktopStorageNotice = useCallback(() => {
    setDesktopStorageNotice(null);
  }, [setDesktopStorageNotice]);

  const openProviderSettings = useCallback(() => {
    navigate(`/settings?ws=${encodeURIComponent(workspaceId)}#agent_harnesses`);
  }, [navigate, workspaceId]);

  const workspaceTitle = workspace?.name ?? "";
  const slashCommands = useMemo<SlashCommandDescriptor[]>(
    () => projectPluginSlashCommands(pluginRegistryState.registry),
    [pluginRegistryState.registry],
  );
  const composerHarnessAuthModal = (
    <HarnessAuthenticationSectionView
      controller={composerHarnessAuth}
      modalOnly
    />
  );
  const workspaceBootstrapGateState = resolveWorkspaceBootstrapGateState({
    workbenchHydrated: workbenchSnap.hydrated,
    providerBootstrapState,
  });

  return (
    <WorkbenchPageShellView
      workspaceId={workspaceId}
      activeTaskId={activeTaskId}
      activeSessionId={activeSessionId}
      sidebarCollapsed={sidebarCollapsed}
      sidebarResizing={sidebarResizing}
      sidebarWidth={sidebarWidth}
      mobileShell={mobileShell}
      desktopUi={desktopUi}
      useHtmlTopbar={useHtmlTopbar}
      desktopStorageNoticeReason={desktopStorageNotice?.reason ?? null}
      onDismissDesktopStorageNotice={dismissDesktopStorageNotice}
      composerHarnessAuthModal={composerHarnessAuthModal}
      workspaceBootstrapGateState={workspaceBootstrapGateState}
      providerBootstrapError={providerBootstrapError}
      onRefreshBootstrap={() => {
        void refreshBootstrap();
      }}
      workbenchWarnings={workbenchSnap.warnings}
      activeTaskController={activeTaskController}
      taskListController={taskListController}
      topbarProps={{
        workspaceId,
        workspaceTitle,
        showDebugIds,
        debugIdLabel,
        onCopyDebugIds: copyDebugIds,
        settingsHref: mobileShell ? "/mobile/connect" : undefined,
        onToggleSidebar: mobileShell ? () => setSidebarCollapsed((prev) => !prev) : undefined,
        sidebarOpen: mobileShell ? !sidebarCollapsed : false,
      }}
      providerWarningProps={{
        acknowledgementScopeId: getProviderOwnerScopeKeyOrNull(workspaceId) ?? workspaceId,
        providersById,
        mobileShell,
        updateAllBusy: installAllBusy,
        onUpdateProviders: updateProvidersFromMenu,
        onOpenSettings: openProviderSettings,
      }}
      sidebarProps={{
        collapsed: sidebarCollapsed,
        taskSearchRef: taskListController.taskSearchRef,
        taskQuery: taskListController.taskQuery,
        onTaskQueryChange: taskListController.setTaskQuery,
        onNewTask: mobileShell
          ? () => {
              focusNewTask();
              setSidebarCollapsed(true);
            }
          : focusNewTask,
        taskListVirtuosoKey: taskListController.taskListVirtuosoKey,
        taskListItems: taskListController.taskListItems,
        initialTaskListItemCount: taskListController.initialTaskListItemCount,
        computeTaskListItemKey: taskListController.computeTaskListItemKey,
        renderTaskListItem: taskListController.renderTaskListItem,
        taskListContext: taskListController.taskListContext,
        onTaskListRangeChanged: taskListController.onTaskListRangeChanged,
        onExpandSidebar: () => setSidebarCollapsed(false),
        onCollapseSidebar: () => setSidebarCollapsed(true),
        onSidebarResizerMouseDown,
        onResetSidebarWidth: () => setSidebarWidth(260),
        mobileMode: mobileShell,
        onSwipeClose: mobileShell ? () => setSidebarCollapsed(true) : undefined,
      }}
      emptyStateProps={{
        newComposerRef: setNewComposerElement,
        dropActive,
        draftPrompt,
        setDraftPrompt,
        dictationRecording,
        onToggleRecording: () => {
          if (dictationRecording) stopDictation().catch(() => {});
          else startDictation().catch(() => {});
        },
        workspaceId,
        slashCommands,
        draftAttachments,
        setDraftAttachments,
        onAttachmentError: setStartError,
        onSend: startNewTask,
        sendDisabled: Boolean(startBlockedReason),
        sendDisabledReason: startBlockedReason,
        draftMode,
        setDraftMode,
        providersById,
        providerInstallsById,
        onInstallProvider: installProviderFromMenu,
        onCancelInstallProvider: cancelProviderInstallFromMenu,
        onInstallAllProviders: installAllProvidersFromMenu,
        installAllBusy,
        providerOptions,
        ensureProviderAuthSummary,
        onRequestHarnessAuth: requestHarnessAuthFromComposer,
        draftHarness,
        setDraftHarness,
        defaultProviderId,
        dictationDebugText,
        dictationError,
        startError,
        dictationOnboarding,
        onCloseDictationOnboarding: dismissDictationOnboarding,
        onBackDictationOnboarding: backDictationOnboarding,
        onChooseDictationOnboardingLocal: chooseDictationOnboardingLocal,
        onChooseDictationOnboardingCloud: chooseDictationOnboardingCloud,
        onCloudChangeDictationOnboarding: updateDictationOnboardingCloud,
        onSubmitCloudDictationOnboarding: () => {
          void submitDictationOnboardingCloud();
        },
        onSubmitLocalDictationOnboarding: () => {
          void submitDictationOnboardingLocal();
        },
      }}
      activeTaskViewProps={
        activeTaskId
          ? {
              sessionsCount: sessions.length,
              showSingleSessionHeader: activeTaskController.showSingleSessionHeader,
              singleSessionTitle: activeTaskController.singleSessionHeaderForRender?.title ?? "Conversation",
              worktreeChip: activeTaskController.worktreeChip,
              worktreeCopied: activeTaskController.worktreeCopied,
              showArtifactsPane: activeTaskController.showArtifactsPane,
              showReviewPane: activeTaskController.showReviewPane,
              terminalOpen: activeTaskController.terminalOpen,
              artifactsCount: activeTaskController.artifacts.length,
              diffBadgeCount: activeTaskController.diffBadgeCount,
              agentWorkSummary: activeTaskController.activeAgentWorkSummary,
              onCopyWorktreeLocation: () => void activeTaskController.copyWorktreeLocation(),
              onOpenWorktreeTerminal: () => void activeTaskController.openWorktreeTerminal(),
              onToggleArtifactsPane: () => activeTaskController.toggleArtifactsPane("header_button"),
              onToggleDiffPane: () => activeTaskController.toggleDiffPane("header_button"),
              onToggleTerminalPanel: () => activeTaskController.toggleTerminalPanel("header_button"),
              onOpenConvoMenu: activeTaskController.openConvoMenu,
              sessionLoadIssues: activeTaskController.sessionLoadIssues,
              onRetrySessionLoads: activeTaskController.retryActiveSessionLoads,
              activeSessionId,
              activeSessionRenderable: activeTaskController.activeSessionRenderable,
              optimisticFailure,
              rightPaneOpen: activeTaskController.rightPaneOpen,
              onSplitterMouseDown: activeTaskController.onSplitterMouseDown,
              diffWidth: activeTaskController.diffWidth,
              showSessionsPane: activeTaskController.showSessionsPane,
              sessionSections: activeTaskController.sessionSections,
              activeSessionKind: activeTaskController.activeSessionKind,
              onSectionChange: activeTaskController.setActiveSessionKind,
              activeWebSessionId: activeTaskController.activeWebSessionId,
              onSelectWebSession: activeTaskController.setActiveWebSessionId,
              daemonBaseUrl: activeTaskController.daemonBaseUrl,
              webSessionsLoading: activeTaskController.webSessionsLoading,
              hasDiff: activeTaskController.hasDiff,
              gitPaneModel: activeTaskController.gitPaneModel,
              diffLoading: activeTaskController.diffLoading,
              diffSummaryError: activeTaskController.diffSummaryError,
              diffTooLarge: activeTaskController.diffTooLarge,
              diffTooLargeLabel: activeTaskController.diffTooLargeLabel,
              activeSessionDiff: activeTaskController.activeSessionDiff,
              activeDiffContentError: activeTaskController.activeDiffContentError,
              diffEmptyLabel: activeTaskController.diffEmptyLabel,
              artifacts: activeTaskController.artifacts,
              artifactsLoading: activeTaskController.artifactsLoading,
              artifactsError: activeTaskController.artifactsError,
              onRetryArtifactsLoad: activeTaskController.retryArtifactsLoad,
              mobileMode: mobileShell,
            }
          : null
      }
    />
  );
}
