import {
  type CSSProperties,
  type Dispatch,
  type MutableRefObject,
  type SetStateAction,
} from "react";
import type {
  PretextVirtualizerItemLocation,
  PretextVirtualizerListMethods,
  PretextVirtualizerScrollLocation,
  PretextVirtualizerShortSizeAlign,
} from "@pretext-virtualizer/interface";
import {
  Message,
  Session,
  SessionEvent,
  type MessageAttachment,
  type SubagentInvocation,
} from "../../api/client";
import {
  type ContextWindowInfo,
  type WorkbenchModeId,
} from "../../components/WorkbenchComposer";
import type {
  DictationOnboardingCloudDraft,
  DictationOnboardingState,
} from "../../utils/useDictationController";
import type { SlashCommandDescriptor } from "../../state/useComposerAutocomplete";
import type { SessionViewVerbosity } from "../../state/uiStateStore";
import type { WorkbenchMessageListContext } from "../sessionThread";
import type { WorkbenchListItem } from "./SessionPage.types";
import { SESSION_THREAD_LAYOUT_STYLE } from "../sessionThread/sessionThreadLayoutTokens";
import type { WorkbenchThreadProjectionOp } from "../sessionThreadProjection";
import { ProviderGuardBanner } from "./ProviderGuardBanner";
import { SessionAuthBanner } from "./SessionAuthBanner";
import { SessionDebugPanel } from "./SessionDebugPanel";
import { SessionSubagentInvocationsCard } from "./SessionSubagentInvocationsCard";
import { SessionThreadSurface } from "./SessionThreadSurface";

type ProviderGuardNotice = {
  kind: string;
  stage?: string | null;
  provider?: string | null;
  pid?: number | null;
  killAtMs?: number | null;
  limitHighMb?: number | null;
  limitMaxMb?: number | null;
  memoryMb?: number | null;
  systemUsedMb?: number | null;
  systemTotalMb?: number | null;
  message?: string | null;
} | null;

type SessionErrorState = {
  provider?: string | null;
  message: string;
} | null;

type AuthUiState = {
  status: string;
  provider?: string | null;
  message?: string | null;
  methods: Array<{ id: string; name: string }>;
};

type SessionWorkbenchPaneProps = {
  id: string;
  entryLoadState: string | undefined;
  entryError: string | null | undefined;
  session: Session | null;
  sessionError: SessionErrorState;
  sessionLoadIssues: Array<{ key: "state" | "subagentInvocations"; message: string }>;
  attachmentError: string | null;
  dropActive: boolean;
  dropScopeRef: MutableRefObject<HTMLDivElement | null>;
  listItems: WorkbenchListItem[];
  threadListSourceKey?: string;
  liveTailItems: WorkbenchListItem[];
  events: SessionEvent[];
  messages: Message[];
  worktreeId: string | null;
  handleFileOpenError: (message: string | null) => void;
  activeAskToolCallId: string | null;
  expandedTurnHeaders: Record<string, boolean>;
  setExpandedTurnHeaders: Dispatch<SetStateAction<Record<string, boolean>>>;
  expandedTurnDetailsById: Record<string, boolean>;
  setExpandedTurnDetailsById: Dispatch<SetStateAction<Record<string, boolean>>>;
  expandedToolById: Record<string, boolean>;
  setExpandedToolById: Dispatch<SetStateAction<Record<string, boolean>>>;
  expandedMessageById: Record<string, boolean>;
  setExpandedMessageById: Dispatch<SetStateAction<Record<string, boolean>>>;
  turnToolsLoading: string[];
  verbosity: SessionViewVerbosity;
  onCancelAskUserQuestion: (toolCallId: string) => Promise<void>;
  onSubmitAskUserQuestion: (
    toolCallId: string,
    answers: Record<string, string>,
  ) => Promise<void>;
  onRequestTurnTools: (turnId: string) => void;
  showDebug: boolean;
  debugEvents: SessionEvent[];
  authUi: AuthUiState;
  authMethodId: string;
  onAuthMethodChange: (value: string) => void;
  authBusy: boolean;
  authError: string | null;
  onAuthenticate: () => Promise<void>;
  onRetrySessionLoads: () => void;
  subagentInvocations: SubagentInvocation[];
  onOpenChildSession: (childSessionId: string) => void;
  isActive: boolean;
  style: CSSProperties;
  itemIdentity: (item: WorkbenchListItem) => unknown;
  itemKey: (item: WorkbenchListItem) => string;
  increaseViewportBy: number;
  initialData: WorkbenchListItem[];
  initialLocation: PretextVirtualizerItemLocation | null;
  threadProjectionOp: WorkbenchThreadProjectionOp;
  context: WorkbenchMessageListContext;
  onScroll: (location: PretextVirtualizerScrollLocation) => void;
  onRenderedDataChange: (range: readonly WorkbenchListItem[]) => void;
  methodsRef: MutableRefObject<
    PretextVirtualizerListMethods<WorkbenchListItem, WorkbenchMessageListContext> | null
  >;
  licenseKey: string;
  shortSizeAlign: PretextVirtualizerShortSizeAlign;
  queueForPanel: Message[];
  pendingQueueMessageIdSet: Set<string>;
  queueActionBusy: boolean;
  sendBusy: boolean;
  onSendQueuedNow: (message: Message) => Promise<void>;
  onEditQueued: (message: Message) => Promise<void>;
  onRemoveQueued: (messageId: string) => Promise<void>;
  input: string;
  setInput: (next: string) => void;
  slashCommands: SlashCommandDescriptor[];
  draftAttachments: MessageAttachment[];
  setDraftAttachments: Dispatch<SetStateAction<MessageAttachment[]>>;
  onAttachmentError: (message: string | null) => void;
  sendNow: () => Promise<void>;
  hasDraftContent: boolean;
  hasActiveTurn: boolean;
  interruptPending?: boolean;
  atBottom: boolean;
  setVerbosityPref: (next: SessionViewVerbosity) => void;
  workbenchMode: WorkbenchModeId;
  setWorkbenchMode: (next: WorkbenchModeId) => void;
  contextWindow: ContextWindowInfo | null;
  dictationRecording: boolean;
  onToggleRecording: () => void;
  onInterruptSession: (() => Promise<void>) | null;
  sendError: string | null;
  fileOpenError: string | null;
  dictationDebugText: string | null;
  dictationError: string | null;
  dictationOnboarding: DictationOnboardingState | null;
  dismissDictationOnboarding: () => void;
  backDictationOnboarding: () => void;
  chooseDictationOnboardingLocal: () => void;
  chooseDictationOnboardingCloud: () => void;
  updateDictationOnboardingCloud: (
    patch: Partial<DictationOnboardingCloudDraft>,
  ) => void;
  submitDictationOnboardingCloud: () => Promise<void>;
  submitDictationOnboardingLocal: () => Promise<void>;
  providerGuardNotice: ProviderGuardNotice;
  providerGuardHeading: string;
  providerGuardMessage: string;
  providerGuardProviderLabel?: string;
  providerGuardPidLabel: string | null;
  providerGuardMemoryLimitMb?: number | null;
  providerGuardLimitLabel: string;
  providerGuardActionBusy: boolean;
  providerGuardActionError: string | null;
  canRaiseProviderGuard: boolean;
  onRaiseProviderGuardLimit: () => Promise<void>;
  onDisableProviderGuard: () => Promise<void>;
  formatMemoryMb: (value?: number | null) => string;
  availableModels: Array<{ id: string; name?: string }>;
  currentModelId: string;
  currentModelDisplayLabel?: string;
  onSetModelId: (next: string) => Promise<void>;
  modelSwitchError: string | null;
  interruptSessionId: string;
};

export function SessionWorkbenchPane({
  id,
  entryLoadState,
  entryError,
  session,
  sessionError,
  sessionLoadIssues,
  attachmentError,
  dropActive,
  dropScopeRef,
  listItems,
  threadListSourceKey,
  liveTailItems,
  events,
  messages,
  worktreeId,
  handleFileOpenError,
  activeAskToolCallId,
  expandedTurnHeaders,
  setExpandedTurnHeaders,
  expandedTurnDetailsById,
  setExpandedTurnDetailsById,
  expandedToolById,
  setExpandedToolById,
  expandedMessageById,
  setExpandedMessageById,
  turnToolsLoading,
  verbosity,
  onCancelAskUserQuestion,
  onSubmitAskUserQuestion,
  onRequestTurnTools,
  showDebug,
  debugEvents,
  authUi,
  authMethodId,
  onAuthMethodChange,
  authBusy,
  authError,
  onAuthenticate,
  onRetrySessionLoads,
  subagentInvocations,
  onOpenChildSession,
  isActive,
  style,
  itemIdentity,
  itemKey,
  increaseViewportBy,
  initialData,
  initialLocation,
  threadProjectionOp,
  context,
  onScroll,
  onRenderedDataChange,
  methodsRef,
  licenseKey,
  shortSizeAlign,
  queueForPanel,
  pendingQueueMessageIdSet,
  queueActionBusy,
  sendBusy,
  onSendQueuedNow,
  onEditQueued,
  onRemoveQueued,
  input,
  setInput,
  slashCommands,
  draftAttachments,
  setDraftAttachments,
  onAttachmentError,
  sendNow,
  hasDraftContent,
  hasActiveTurn,
  interruptPending = false,
  atBottom,
  setVerbosityPref,
  workbenchMode,
  setWorkbenchMode,
  contextWindow,
  dictationRecording,
  onToggleRecording,
  onInterruptSession,
  sendError,
  fileOpenError,
  dictationDebugText,
  dictationError,
  dictationOnboarding,
  dismissDictationOnboarding,
  backDictationOnboarding,
  chooseDictationOnboardingLocal,
  chooseDictationOnboardingCloud,
  updateDictationOnboardingCloud,
  submitDictationOnboardingCloud,
  submitDictationOnboardingLocal,
  providerGuardNotice,
  providerGuardHeading,
  providerGuardMessage,
  providerGuardProviderLabel,
  providerGuardPidLabel,
  providerGuardMemoryLimitMb,
  providerGuardLimitLabel,
  providerGuardActionBusy,
  providerGuardActionError,
  canRaiseProviderGuard,
  onRaiseProviderGuardLimit,
  onDisableProviderGuard,
  formatMemoryMb,
  availableModels,
  currentModelId,
  currentModelDisplayLabel,
  onSetModelId,
  modelSwitchError,
  interruptSessionId,
}: SessionWorkbenchPaneProps) {
  const liveTailCount = liveTailItems.length;
  const totalVisibleThreadItems = listItems.length + liveTailCount;

  return (
    <div
      className="wb-session-view ctx-drop-scope"
      ref={dropScopeRef}
      style={SESSION_THREAD_LAYOUT_STYLE}
      data-testid="session-view"
      data-session-id={id}
      data-thread-count={totalVisibleThreadItems}
    >
      {dropActive ? (
        <div className="ctx-drop-overlay" aria-hidden="true">
          <div className="ctx-drop-overlay-text">Drop image to attach</div>
        </div>
      ) : null}
      <div className="wb-session-left">
        {entryLoadState === "fatal" && entryError ? (
          <div className="banner">
            <span className="error">{entryError}</span>
          </div>
        ) : null}
        {sessionError ? (
          <div className="banner" role="alert">
            <div className="row" style={{ justifyContent: "space-between" }}>
              <strong>Error</strong>
              {sessionError.provider ? <span className="muted">{sessionError.provider}</span> : null}
            </div>
            <div className="error" style={{ whiteSpace: "pre-wrap" }}>
              {sessionError.message}
            </div>
          </div>
        ) : null}
        {modelSwitchError ? (
          <div className="banner" role="alert">
            <div className="row" style={{ justifyContent: "space-between" }}>
              <strong>Model Switch Failed</strong>
            </div>
            <div className="error" style={{ whiteSpace: "pre-wrap" }}>
              {modelSwitchError}
            </div>
          </div>
        ) : null}
        {sessionLoadIssues.length > 0 ? (
          <div className="banner wb-session-load-issues" role="alert" data-testid="workbench-session-load-issues">
            <div className="wb-session-load-issues-title">Some session details failed to load.</div>
            {sessionLoadIssues.map((issue) => (
              <div key={issue.key}>{issue.message}</div>
            ))}
            <div>
              <button type="button" onClick={onRetrySessionLoads}>Retry</button>
            </div>
          </div>
        ) : null}
        {attachmentError ? (
          <div className="banner" role="alert" data-testid="session-attachment-error">
            <div className="row" style={{ justifyContent: "space-between" }}>
              <strong>Attachment Failed</strong>
            </div>
            <div className="error" style={{ whiteSpace: "pre-wrap" }}>
              {attachmentError}
            </div>
          </div>
        ) : null}
        <ProviderGuardBanner
          heading={providerGuardHeading}
          message={providerGuardMessage}
          providerLabel={providerGuardProviderLabel}
          pidLabel={providerGuardPidLabel}
          memoryLabel={
            providerGuardNotice?.memoryMb != null
              ? `Memory ${formatMemoryMb(providerGuardNotice.memoryMb)}${
                  providerGuardMemoryLimitMb != null
                    ? ` / ${formatMemoryMb(providerGuardMemoryLimitMb)} (${providerGuardLimitLabel})`
                    : ""
                }`
              : null
          }
          systemLabel={
            providerGuardNotice?.systemUsedMb != null &&
            providerGuardNotice.systemTotalMb != null
              ? `System ${formatMemoryMb(providerGuardNotice.systemUsedMb)} / ${formatMemoryMb(providerGuardNotice.systemTotalMb)}`
              : null
          }
          notice={providerGuardNotice}
          actionBusy={providerGuardActionBusy}
          actionError={providerGuardActionError}
          canRaiseLimit={canRaiseProviderGuard}
          onRaiseLimit={onRaiseProviderGuardLimit}
          onDisableGuard={onDisableProviderGuard}
        />
        {showDebug ? (
          <div className="wb-muted" style={{ fontFamily: "var(--mono)" }}>
            debug: events={events.length} messages={messages.length} userMessages=
            {messages.filter((message) => message.role === "user").length} historyItems={listItems.length} liveItems=
            {liveTailCount}
          </div>
        ) : null}
        <SessionAuthBanner
          visible={authUi.status === "required" || authUi.status === "failed"}
          status={authUi.status}
          provider={authUi.provider ?? session?.provider_id}
          message={authUi.message}
          methods={authUi.methods}
          authMethodId={authMethodId}
          onAuthMethodChange={onAuthMethodChange}
          authBusy={authBusy}
          authError={authError}
          onAuthenticate={onAuthenticate}
        />
        <SessionSubagentInvocationsCard
          subagentInvocations={subagentInvocations}
          onOpenChildSession={onOpenChildSession}
        />
        {showDebug && debugEvents.length > 0 ? <SessionDebugPanel events={debugEvents} /> : null}

        <SessionThreadSurface
          sessionId={id}
          session={session}
          worktreeId={worktreeId}
          handleFileOpenError={handleFileOpenError}
          transcript={{
            listItems,
            sourceKey: threadListSourceKey,
            liveTailItems,
            activeAskToolCallId,
            expandedTurnHeaders,
            setExpandedTurnHeaders,
            expandedTurnDetailsById,
            setExpandedTurnDetailsById,
            expandedToolById,
            setExpandedToolById,
            expandedMessageById,
            setExpandedMessageById,
            turnToolsLoading,
            verbosity,
            onCancelAskUserQuestion,
            onSubmitAskUserQuestion,
            onRequestTurnTools,
            isActive,
            listStyle: style,
            itemIdentity,
            itemKey,
            increaseViewportBy,
            initialData,
            initialLocation,
            threadProjectionOp,
            context,
            onScroll,
            onRenderedDataChange,
            methodsRef,
            licenseKey,
            shortSizeAlign,
          }}
          queue={{
            queueForPanel,
            pendingQueueMessageIdSet,
            queueActionBusy,
            sendBusy,
            onSendQueuedNow,
            onEditQueued,
            onRemoveQueued,
          }}
          composer={{
            input,
            setInput,
            slashCommands,
            draftAttachments,
            setDraftAttachments,
            onAttachmentError,
            sendNow,
            sendBusy,
            hasDraftContent,
            hasActiveTurn,
            interruptPending,
            setVerbosityPref,
            workbenchMode,
            setWorkbenchMode,
            contextWindow,
            dictationRecording,
            onToggleRecording,
            onInterruptSession,
            sendError,
            fileOpenError,
            dictationDebugText,
            dictationError,
            dictationOnboarding,
            dismissDictationOnboarding,
            backDictationOnboarding,
            chooseDictationOnboardingLocal,
            chooseDictationOnboardingCloud,
            updateDictationOnboardingCloud,
            submitDictationOnboardingCloud,
            submitDictationOnboardingLocal,
            availableModels,
            currentModelId,
            currentModelDisplayLabel,
            onSetModelId,
          }}
          atBottom={atBottom}
        />
      </div>
    </div>
  );
}
