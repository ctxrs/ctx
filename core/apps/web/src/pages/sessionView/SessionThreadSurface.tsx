import {
  type CSSProperties,
  type Dispatch,
  type MutableRefObject,
  type SetStateAction,
  useMemo,
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
  type MessageAttachment,
} from "../../api/client";
import { AskUserQuestionCard } from "../../components/AskUserQuestionCard";
import { DictationOnboardingModal } from "../../components/dictation/DictationOnboardingModal";
import {
  WorkbenchComposer as UnifiedWorkbenchComposer,
  type ContextWindowInfo,
  type WorkbenchModeId,
} from "../../components/WorkbenchComposer";
import { findHarnessCatalogEntry } from "../../utils/harnessCatalog";
import type {
  DictationOnboardingCloudDraft,
  DictationOnboardingState,
} from "../../utils/useDictationController";
import type { SlashCommandDescriptor } from "../../state/useComposerAutocomplete";
import type { SessionViewVerbosity } from "../../state/uiStateStore";
import type { WorkbenchMessageListContext } from "../sessionThread";
import type {
  ThreadItem,
  WorkbenchListItem,
} from "./SessionPage.types";
import type { WorkbenchThreadProjectionOp } from "../sessionThreadProjection";
import { resolveWorkbenchMessageExpanded } from "../sessionMessageListItemIdentity";
import {
  getWorkbenchTurnHeaderLayoutState,
} from "../sessionThread/transcriptRowLayoutModel";
import { SessionThreadPane } from "../sessionThread/SessionThreadPane";
import {
  AssistantEntry,
  ThreadItemView,
  WorkbenchThoughtRow,
  WorkbenchToolGroupRow,
  WorkbenchToolRow,
  WorkbenchTurnHeaderView,
  WorkbenchTurnStatusRow,
} from "../sessionThread/SessionThreadItemViews";
import { SessionQueuePanel } from "./SessionQueuePanel";

export type SessionThreadSurfaceTranscriptProps = {
  listItems: WorkbenchListItem[];
  sourceKey?: string;
  liveTailItems: WorkbenchListItem[];
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
  isActive: boolean;
  listStyle: CSSProperties;
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
};

export type SessionThreadSurfaceQueueProps = {
  queueForPanel: Message[];
  pendingQueueMessageIdSet: Set<string>;
  queueActionBusy: boolean;
  sendBusy: boolean;
  onSendQueuedNow: (message: Message) => Promise<void>;
  onEditQueued: (message: Message) => Promise<void>;
  onRemoveQueued: (messageId: string) => Promise<void>;
};

export type SessionThreadSurfaceComposerProps = {
  input: string;
  setInput: (next: string) => void;
  slashCommands: SlashCommandDescriptor[];
  draftAttachments: MessageAttachment[];
  setDraftAttachments: Dispatch<SetStateAction<MessageAttachment[]>>;
  onAttachmentError: (message: string | null) => void;
  sendNow: () => Promise<void>;
  sendBusy: boolean;
  hasDraftContent: boolean;
  hasActiveTurn: boolean;
  interruptPending?: boolean;
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
  availableModels: Array<{ id: string; name?: string }>;
  currentModelId: string;
  currentModelDisplayLabel?: string;
  onSetModelId: (next: string) => Promise<void>;
};

export function SessionThreadSurface({
  sessionId,
  session,
  worktreeId,
  handleFileOpenError,
  transcript,
  queue,
  composer,
  atBottom,
}: {
  sessionId: string;
  session: Session | null;
  worktreeId: string | null;
  handleFileOpenError: (message: string | null) => void;
  transcript: SessionThreadSurfaceTranscriptProps;
  queue: SessionThreadSurfaceQueueProps;
  composer: SessionThreadSurfaceComposerProps;
  atBottom: boolean;
}) {
  const messageListContext = useMemo(
    () => ({
      ...transcript.context,
      expandedTurnHeaders: transcript.expandedTurnHeaders,
      expandedTurnDetailsById: transcript.expandedTurnDetailsById,
      expandedToolById: transcript.expandedToolById,
      expandedMessageById: transcript.expandedMessageById,
      turnToolsLoading: transcript.turnToolsLoading,
      verbosity: transcript.verbosity,
    }),
    [transcript],
  );

  const renderThreadItem = (item: ThreadItem) => {
    if (item.kind === "spacer") {
      return <div style={{ height: 1 }} />;
    }
    if (item.kind === "thought") {
      return <WorkbenchThoughtRow item={item} />;
    }
    if (item.kind === "turn_status") {
      return <WorkbenchTurnStatusRow item={item} />;
    }
    if (item.kind === "assistant") {
      if (!item.is_complete && item.content.trim().length === 0) {
        return null;
      }
      return (
        <AssistantEntry
          content={item.content}
          worktreeId={worktreeId}
          onFileOpenError={handleFileOpenError}
        />
      );
    }
    if (item.kind === "tool_group") {
      const expanded = transcript.expandedTurnDetailsById[item.turn_id] ?? false;
      const toolsLoading = transcript.turnToolsLoading.includes(item.turn_id);
      return (
        <WorkbenchToolGroupRow
          item={item}
          verbosity={transcript.verbosity}
          expanded={expanded}
          toolsLoading={toolsLoading}
          onToggle={() => {
            transcript.setExpandedTurnDetailsById((prev) => ({
              ...prev,
              [item.turn_id]: !expanded,
            }));
          }}
          onRequestTools={() => transcript.onRequestTurnTools(item.turn_id)}
          onToggleTool={(toolId) => {
            transcript.setExpandedToolById((prev) => ({ ...prev, [toolId]: !prev[toolId] }));
          }}
          expandedToolById={transcript.expandedToolById}
        />
      );
    }
    if (item.kind === "tool") {
      const toolExpanded = transcript.expandedToolById[item.id] ?? false;
      return (
        <WorkbenchToolRow
          item={item}
          verbosity={transcript.verbosity}
          expanded={toolExpanded}
          onToggle={() => {
            transcript.setExpandedToolById((prev) => ({ ...prev, [item.id]: !toolExpanded }));
          }}
        />
      );
    }
    if (item.kind === "ask_user_question") {
      const isActive = item.tool_call_id === transcript.activeAskToolCallId;
      return (
        <AskUserQuestionCard
          input={item.input}
          answers={item.answers}
          outcome={item.outcome}
          readOnly={item.answered}
          active={isActive}
          onCancel={
            item.answered
              ? undefined
              : async () => transcript.onCancelAskUserQuestion(item.tool_call_id)
          }
          onSubmit={
            item.answered
              ? undefined
              : async (answers) => transcript.onSubmitAskUserQuestion(item.tool_call_id, answers)
          }
        />
      );
    }
    return (
      <ThreadItemView
        item={item}
        worktreeId={worktreeId}
        onFileOpenError={handleFileOpenError}
        messageExpanded={
          item.kind === "message"
            ? resolveWorkbenchMessageExpanded(item, transcript.expandedMessageById)
            : undefined
        }
        onToggleMessageExpanded={
          item.kind === "message"
            ? (expanded) => {
                transcript.setExpandedMessageById((prev) => ({ ...prev, [item.id]: expanded }));
              }
            : undefined
        }
      />
    );
  };

  const workbenchItemContent = (_: number, item: WorkbenchListItem) => {
    if (!item) return <div style={{ height: 1 }} />;
    const itemId = item.id;
    if (item.kind === "turn_header") {
      const header = (item as Extract<WorkbenchListItem, { kind: "turn_header" }>).header;
      const layout = getWorkbenchTurnHeaderLayoutState(item, transcript.expandedTurnHeaders);
      return (
        <div data-thread-item-id={itemId} style={{ display: "contents" }}>
          <WorkbenchTurnHeaderView
            header={header}
            plainText={layout.displayPlainText}
            expanded={layout.expanded}
            onToggle={() => {
              transcript.setExpandedTurnHeaders((prev) => ({ ...prev, [header.id]: !layout.expanded }));
            }}
          />
        </div>
      );
    }
    const content = renderThreadItem(item as ThreadItem);
    return (
      <div className="wb-thread-indent" data-thread-item-id={itemId}>
        {content}
      </div>
    );
  };

  const harness = findHarnessCatalogEntry(session?.provider_id);

  return (
    <>
      <SessionThreadPane
        sessionId={sessionId}
        isActive={transcript.isActive}
        style={transcript.listStyle}
        initialData={transcript.initialData}
        sourceKey={transcript.sourceKey}
        itemContent={workbenchItemContent}
        itemIdentity={transcript.itemIdentity}
        itemKey={transcript.itemKey}
        increaseViewportBy={transcript.increaseViewportBy}
        initialLocation={transcript.initialLocation}
        threadProjectionOp={transcript.threadProjectionOp}
        context={messageListContext}
        onScroll={transcript.onScroll}
        onRenderedDataChange={transcript.onRenderedDataChange}
        methodsRef={transcript.methodsRef}
        licenseKey={transcript.licenseKey}
        shortSizeAlign={transcript.shortSizeAlign}
      >
        {transcript.liveTailItems.length > 0 ? (
          <div className="wb-thread-live-tail" role="list" aria-label="Live turn">
            {transcript.liveTailItems.map((item, index) => (
              <div
                key={item.id}
                role="listitem"
                className="wb-thread-live-tail-row"
                data-thread-item-id={item.id}
              >
                {workbenchItemContent(index, item)}
              </div>
            ))}
          </div>
        ) : null}
        <SessionQueuePanel
          queue={queue.queueForPanel}
          pendingQueueMessageIdSet={queue.pendingQueueMessageIdSet}
          queueActionBusy={queue.queueActionBusy}
          sendBusy={queue.sendBusy}
          onSendQueuedNow={queue.onSendQueuedNow}
          onEditQueued={queue.onEditQueued}
          onRemoveQueued={queue.onRemoveQueued}
        />
        <UnifiedWorkbenchComposer
          variant="activeSession"
          value={composer.input}
          setValue={composer.setInput}
          placeholder="@ for context, / for commands"
          inputDisabled={composer.dictationRecording}
          sessionIdForAutocomplete={sessionId}
          slashCommands={composer.slashCommands}
          attachments={composer.draftAttachments}
          setAttachments={composer.setDraftAttachments}
          onAttachmentError={composer.onAttachmentError}
          onSend={composer.sendNow}
          sendDisabled={composer.sendBusy || !composer.hasDraftContent}
          sendDisabledReason={
            composer.sendBusy ? "Sending..." : !composer.hasDraftContent ? "Enter a message." : null
          }
          onInterrupt={composer.onInterruptSession}
          isWorking={composer.hasActiveTurn}
          interruptPending={composer.interruptPending}
          verbosity={transcript.verbosity}
          onSetVerbosity={composer.setVerbosityPref}
          modeId={composer.workbenchMode}
          setModeId={composer.setWorkbenchMode}
          contextWindow={composer.contextWindow}
          recording={composer.dictationRecording}
          onToggleRecording={composer.onToggleRecording}
          providerId={session?.provider_id ?? undefined}
          harnessLabel={harness?.label ?? (session?.provider_id ?? "Provider")}
          harnessLogoSrc={harness?.logoSrc}
          harnessLogoInvert={harness?.invertInDark}
          harnessLogoInvertInLight={harness?.invertInLight}
          availableModels={composer.availableModels}
          currentModelId={composer.currentModelId}
          currentModelDisplayLabel={composer.currentModelDisplayLabel}
          onSetModelId={composer.onSetModelId}
        />
        {composer.sendError ? <div className="wb-banner">{composer.sendError}</div> : null}
        {composer.fileOpenError ? <div className="wb-banner">{composer.fileOpenError}</div> : null}
        {composer.dictationDebugText ? <div className="wb-banner">{composer.dictationDebugText}</div> : null}
        {composer.dictationError ? <div className="wb-banner">{composer.dictationError}</div> : null}
        <DictationOnboardingModal
          state={composer.dictationOnboarding}
          onClose={composer.dismissDictationOnboarding}
          onBack={composer.backDictationOnboarding}
          onChooseLocal={composer.chooseDictationOnboardingLocal}
          onChooseCloud={composer.chooseDictationOnboardingCloud}
          onCloudChange={composer.updateDictationOnboardingCloud}
          onSubmitCloud={() => {
            void composer.submitDictationOnboardingCloud();
          }}
          onSubmitLocal={() => {
            void composer.submitDictationOnboardingLocal();
          }}
        />
      </SessionThreadPane>
      <div className="sr-only" aria-live="polite">
        {session && (atBottom ? "Agent output updating." : "New agent activity.")}
      </div>
    </>
  );
}
