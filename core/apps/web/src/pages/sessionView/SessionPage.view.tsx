import { useCallback, useEffect, useMemo, useState } from "react";
import {
  type MessageAttachment,
  type Session,
  idToString,
} from "../../api/client";
import {
  useOpenSession,
  useSessionEntry,
  useSessionSupervisor,
} from "../../state/sessionSupervisor";
import type { SlashCommandDescriptor } from "../../state/useComposerAutocomplete";
import { type WorkbenchModeId } from "../../components/WorkbenchComposer";
import { useFeatureGate } from "../../utils/analytics";
import { useDictationController } from "../../utils/useDictationController";
import { useWorkbenchStore } from "../../workbench/store";
import { deriveProtocolSlashCommands } from "../../utils/protocolSlashCommands";
import { VIRTUOSO_MESSAGE_LIST_LICENSE_KEY } from "../../config/licenses";
import { appendSegment } from "./SessionPage.helpers";
import { SessionWorkbenchPane } from "./SessionWorkbenchPane";
import { useSessionDraftAttachments } from "./useSessionDraftAttachments";
import { useSessionViewTranscriptController } from "./useSessionViewTranscriptController";
import { useSessionViewRuntimeController } from "./useSessionViewRuntimeController";
import {
  selectSessionQueuePanelMessages,
  selectSessionThreadProjection,
} from "../../state/sessionThreadProjection/selectors";
import {
  formatMemoryMb,
  SCROLLBACK_INCREASE_VIEWPORT_BY_PX,
} from "./SessionPage.viewHelpers";
import { useSessionComposerQueueController } from "../useSessionComposerQueueController";
import { composeModelId } from "../../utils/modelEffort";

export function SessionView({
  sessionId,
  isActive = true,
  autoOpenSession = true,
  sessionMode = "active",
  hideSessionLoadIssuesBanner = false,
  draft,
  onDraftChange,
  onDraftAttachmentsChange,
  onDraftPersistNow,
  onModeChange,
}: {
  sessionId: string;
  isActive?: boolean;
  sessionMode?: "active" | "archived";
  draft?: { text: string; modeId: WorkbenchModeId; attachments?: MessageAttachment[] } | null;
  onDraftChange?: ((text: string) => void) | null;
  onDraftAttachmentsChange?: ((attachments: MessageAttachment[]) => void) | null;
  onDraftPersistNow?: (() => void | Promise<void>) | null;
  onModeChange?: ((modeId: WorkbenchModeId) => void) | null;
  autoOpenSession?: boolean;
  hideSessionLoadIssuesBanner?: boolean;
}) {
  const id = sessionId;
  const supervisor = useSessionSupervisor();
  const workbenchStore = useWorkbenchStore();
  const showDebug = useMemo(() => {
    try {
      return new URLSearchParams(window.location.search).get("debug") === "1";
    } catch {
      return false;
    }
  }, [id]);
  const perfEnabled = useMemo(() => {
    try {
      return new URLSearchParams(window.location.search).get("perf") === "1";
    } catch {
      return false;
    }
  }, [id]);
  const [inputInternal, setInputInternal] = useState("");
  const [workbenchModeInternal, setWorkbenchModeInternal] = useState<WorkbenchModeId>("default");

  const input = draft?.text ?? inputInternal;
  const setInput = useCallback(
    (next: string) => {
      if (draft) {
        onDraftChange?.(next);
        return;
      }
      setInputInternal(next);
    },
    [draft, onDraftChange],
  );
  const workbenchMode = draft?.modeId ?? workbenchModeInternal;
  const setWorkbenchMode = useCallback(
    (next: WorkbenchModeId) => {
      if (draft) {
        onModeChange?.(next);
        return;
      }
      setWorkbenchModeInternal(next);
    },
    [draft, onModeChange],
  );

  useOpenSession(autoOpenSession ? id : "", { watchDiff: true, mode: sessionMode });

  const entry = useSessionEntry(id);
  const session: Session | null = entry?.session ?? null;

  const baseThreadProjection = useMemo(
    () => selectSessionThreadProjection(entry),
    [entry],
  );

  const queuedMessagesEnabled = useFeatureGate("queued_messages_enabled", false);
  const composerAnalyticsModelId = useMemo(
    () => composeModelId(String(session?.model_id ?? ""), session?.reasoning_effort ?? null),
    [session?.model_id, session?.reasoning_effort],
  );
  const listStyle = useMemo(() => ({ flex: 1, minHeight: 0 } as const), []);

  const runtimeController = useSessionViewRuntimeController({
    sessionId: id,
    entry,
    session,
    threadProjection: baseThreadProjection,
    supervisor,
  });
  const [attachmentError, setAttachmentError] = useState<string | null>(null);
  const handleAttachmentError = useCallback((message: string | null) => {
    setAttachmentError(message);
  }, []);
  const handleComposerSendStarted = useCallback(() => {
    setAttachmentError(null);
  }, []);

  const {
    setDraftAttachmentsInternal,
    draftAttachments,
    setDraftAttachments,
    dropScopeRef,
    dropActive,
  } = useSessionDraftAttachments({
    draft,
    onDraftAttachmentsChange,
    onError: handleAttachmentError,
  });

  useEffect(() => {
    setDraftAttachmentsInternal([]);
    setAttachmentError(null);
  }, [id, setDraftAttachmentsInternal]);

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
    text: input,
    setText: setInput,
    appendSegment,
  });

  const resolveSendText = useCallback(async () => {
    return (dictationRecording ? await stopDictation({ awaitFinal: true }) : input).trim();
  }, [dictationRecording, input, stopDictation]);

  const transcriptController = useSessionViewTranscriptController({
    sessionId: id,
    entry,
    threadProjection: baseThreadProjection,
    supervisor,
    isActive,
    showDebug,
    perfEnabled,
    listStyle,
    increaseViewportBy: SCROLLBACK_INCREASE_VIEWPORT_BY_PX,
    licenseKey: VIRTUOSO_MESSAGE_LIST_LICENSE_KEY,
  });

  const composerState = useSessionComposerQueueController({
    sessionId: id,
    session,
    supervisor,
    input,
    setInput,
    draftAttachments,
    setDraftAttachments,
    optimisticThreadMessages: entry?.optimisticThreadMessages ?? [],
    optimisticQueuedMessages: entry?.optimisticQueuedMessages ?? [],
    messageCount: baseThreadProjection.messages.length,
    turnCount: baseThreadProjection.turns.length,
    hasActiveTurn: runtimeController.hasActiveTurn,
    queuedMessagesEnabled,
    currentModelId: composerAnalyticsModelId,
    interruptSessionId: runtimeController.interruptSessionId,
    resolveSendText,
    setAtBottom: transcriptController.setAtBottom,
    onDraftPersistNow,
    onSendStarted: handleComposerSendStarted,
  });

  const handleRetrySessionLoads = useCallback(() => {
    if (!id) return;
    supervisor.loadSessionState(id, { force: true });
    supervisor.loadSubagentInvocations(id, { force: true });
  }, [id, supervisor]);

  const openChildSession = useCallback(
    (childSessionId: string) => {
      if (!session) return;
      const taskId = idToString(session.task_id);
      if (!taskId) return;
      workbenchStore.focusTask(taskId, childSessionId || null);
    },
    [session, workbenchStore],
  );

  const slashCommands = useMemo<SlashCommandDescriptor[]>(
    () =>
      deriveProtocolSlashCommands({
        providerId: entry?.session?.provider_id,
        commands: entry?.acpCommands,
        slashCommands: entry?.acpSlashCommands,
      }),
    [entry?.acpCommands, entry?.acpSlashCommands, entry?.session?.provider_id],
  );

  const queueForPanel = useMemo(
    () =>
      queuedMessagesEnabled
        ? selectSessionQueuePanelMessages(entry, baseThreadProjection.turns)
        : [],
    [baseThreadProjection.turns, entry, queuedMessagesEnabled],
  );
  const hasDraftContent = input.trim().length > 0 || draftAttachments.length > 0;
  const handleToggleRecording = useCallback(() => {
    if (dictationRecording) {
      stopDictation().catch(() => {});
      return;
    }
    startDictation().catch(() => {});
  }, [dictationRecording, startDictation, stopDictation]);

  return (
    <SessionWorkbenchPane
      id={id}
      entryLoadState={entry?.loadState}
      entryError={entry?.error}
      session={session}
      sessionError={transcriptController.sessionError}
      sessionLoadIssues={hideSessionLoadIssuesBanner ? [] : runtimeController.sessionLoadIssues}
      attachmentError={attachmentError}
      dropActive={dropActive}
      dropScopeRef={dropScopeRef}
      listItems={transcriptController.transcript.listItems}
      threadListSourceKey={transcriptController.threadListSourceKey}
      liveTailItems={transcriptController.transcript.liveTailItems}
      events={transcriptController.events}
      messages={transcriptController.messages}
      worktreeId={runtimeController.worktreeId}
      handleFileOpenError={transcriptController.handleFileOpenError}
      activeAskToolCallId={transcriptController.transcript.activeAskToolCallId}
      expandedTurnHeaders={transcriptController.transcript.expandedTurnHeaders}
      setExpandedTurnHeaders={transcriptController.transcript.setExpandedTurnHeaders}
      expandedTurnDetailsById={transcriptController.transcript.expandedTurnDetailsById}
      setExpandedTurnDetailsById={transcriptController.transcript.setExpandedTurnDetailsById}
      expandedToolById={transcriptController.transcript.expandedToolById}
      setExpandedToolById={transcriptController.transcript.setExpandedToolById}
      expandedMessageById={transcriptController.transcript.expandedMessageById}
      setExpandedMessageById={transcriptController.transcript.setExpandedMessageById}
      turnToolsLoading={transcriptController.transcript.turnToolsLoading}
      verbosity={transcriptController.verbosity}
      onCancelAskUserQuestion={transcriptController.transcript.onCancelAskUserQuestion}
      onSubmitAskUserQuestion={transcriptController.transcript.onSubmitAskUserQuestion}
      onRequestTurnTools={transcriptController.transcript.onRequestTurnTools}
      showDebug={showDebug}
      debugEvents={transcriptController.debugEvents}
      authUi={runtimeController.authUi}
      authMethodId={runtimeController.authMethodId}
      onAuthMethodChange={runtimeController.setAuthMethodId}
      authBusy={runtimeController.authBusy}
      authError={runtimeController.authError}
      onAuthenticate={runtimeController.onAuthenticate}
      onRetrySessionLoads={handleRetrySessionLoads}
      subagentInvocations={runtimeController.subagentInvocations}
      onOpenChildSession={openChildSession}
      isActive={isActive}
      style={transcriptController.transcript.listStyle}
      itemIdentity={transcriptController.transcript.itemIdentity}
      itemKey={transcriptController.transcript.itemKey}
      increaseViewportBy={transcriptController.transcript.increaseViewportBy}
      initialData={transcriptController.transcript.initialData}
      initialLocation={transcriptController.transcript.initialLocation}
      threadProjectionOp={transcriptController.transcript.threadProjectionOp}
      context={transcriptController.transcript.context}
      onScroll={transcriptController.transcript.onScroll}
      onRenderedDataChange={transcriptController.transcript.onRenderedDataChange}
      methodsRef={transcriptController.transcript.methodsRef}
      licenseKey={transcriptController.transcript.licenseKey}
      shortSizeAlign={transcriptController.transcript.shortSizeAlign}
      queueForPanel={queueForPanel}
      pendingQueueMessageIdSet={composerState.pendingQueueMessageIdSet}
      queueActionBusy={composerState.queueActionBusy}
      sendBusy={composerState.sendBusy}
      onSendQueuedNow={composerState.onSendQueuedNow}
      onEditQueued={composerState.onEditQueued}
      onRemoveQueued={composerState.onRemoveQueued}
      input={input}
      setInput={setInput}
      slashCommands={slashCommands}
      draftAttachments={draftAttachments}
      setDraftAttachments={setDraftAttachments}
      onAttachmentError={handleAttachmentError}
      sendNow={composerState.sendNow}
      hasDraftContent={hasDraftContent}
      hasActiveTurn={runtimeController.hasActiveTurn}
      interruptPending={composerState.interruptPending}
      atBottom={transcriptController.atBottom}
      setVerbosityPref={transcriptController.setVerbosityPref}
      workbenchMode={workbenchMode}
      setWorkbenchMode={setWorkbenchMode}
      contextWindow={runtimeController.contextWindow}
      dictationRecording={dictationRecording}
      onToggleRecording={handleToggleRecording}
      onInterruptSession={composerState.onInterruptSession}
      sendError={composerState.sendError}
      fileOpenError={transcriptController.fileOpenError}
      dictationDebugText={dictationDebugText}
      dictationError={dictationError}
      dictationOnboarding={dictationOnboarding}
      dismissDictationOnboarding={dismissDictationOnboarding}
      backDictationOnboarding={backDictationOnboarding}
      chooseDictationOnboardingLocal={chooseDictationOnboardingLocal}
      chooseDictationOnboardingCloud={chooseDictationOnboardingCloud}
      updateDictationOnboardingCloud={updateDictationOnboardingCloud}
      submitDictationOnboardingCloud={submitDictationOnboardingCloud}
      submitDictationOnboardingLocal={submitDictationOnboardingLocal}
      providerGuardNotice={runtimeController.providerGuardNotice}
      providerGuardHeading={runtimeController.providerGuardHeading}
      providerGuardMessage={runtimeController.providerGuardMessage}
      providerGuardProviderLabel={runtimeController.providerGuardProviderLabel}
      providerGuardPidLabel={runtimeController.providerGuardPidLabel}
      providerGuardMemoryLimitMb={runtimeController.providerGuardMemoryLimitMb}
      providerGuardLimitLabel={runtimeController.providerGuardLimitLabel}
      providerGuardActionBusy={runtimeController.providerGuardActionBusy}
      providerGuardActionError={runtimeController.providerGuardActionError}
      canRaiseProviderGuard={runtimeController.canRaiseProviderGuard}
      onRaiseProviderGuardLimit={runtimeController.onRaiseProviderGuardLimit}
      onDisableProviderGuard={runtimeController.onDisableProviderGuard}
      formatMemoryMb={formatMemoryMb}
      availableModels={runtimeController.availableModels}
      currentModelId={runtimeController.currentModelId}
      currentModelDisplayLabel={runtimeController.currentModelDisplayLabel}
      onSetModelId={runtimeController.onSetModelId}
      modelSwitchError={runtimeController.modelSwitchError}
      interruptSessionId={runtimeController.interruptSessionId}
    />
  );
}
