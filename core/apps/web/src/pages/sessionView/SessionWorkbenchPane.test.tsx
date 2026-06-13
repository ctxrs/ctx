import React, { useRef, useState } from "react";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { VirtuosoMessageListTestingContext } from "@virtuoso.dev/message-list";
import type { Session } from "../../api/client";
import type { MessageAttachment } from "../../api/client";
import type { WorkbenchMessageListContext } from "../sessionThread";
import type { WorkbenchListItem } from "./SessionPage.types";
import { SessionWorkbenchPane } from "./SessionWorkbenchPane";

const { copyTextToClipboardMock } = vi.hoisted(() => ({
  copyTextToClipboardMock: vi.fn(async () => true),
}));

vi.mock("../../utils/clipboard", () => ({
  copyTextToClipboard: copyTextToClipboardMock,
}));

vi.mock("../../components/WorkbenchComposer", () => ({
  WorkbenchComposer: () => <div data-testid="workbench-composer" />,
}));

vi.mock("./ProviderGuardBanner", () => ({
  ProviderGuardBanner: () => null,
}));

vi.mock("./SessionAuthBanner", () => ({
  SessionAuthBanner: () => null,
}));

vi.mock("./SessionDebugPanel", () => ({
  SessionDebugPanel: () => null,
}));

vi.mock("./SessionQueuePanel", () => ({
  SessionQueuePanel: () => null,
}));

vi.mock("./SessionSubagentInvocationsCard", () => ({
  SessionSubagentInvocationsCard: () => null,
}));

vi.mock("../../components/dictation/DictationOnboardingModal", () => ({
  DictationOnboardingModal: () => null,
}));

function TestPane({
  session = null,
  interruptSessionId = "",
  attachmentError = null,
}: {
  session?: Session | null;
  interruptSessionId?: string;
  attachmentError?: string | null;
}) {
  const listItems = useRef<WorkbenchListItem[]>([
    {
      kind: "turn_header",
      id: "turn-header-row-1",
      header: {
        id: "header-1",
        content: "line 1\nline 2\nline 3\nline 4\nline 5",
        plain_text: "line 1\nline 2\nline 3\nline 4\nline 5",
        attachments: [],
        created_at: "2025-01-01T00:00:00.000Z",
      },
    },
  ]);
  const [expandedTurnHeaders, setExpandedTurnHeaders] = useState<Record<string, boolean>>({});
  const [expandedTurnDetailsById, setExpandedTurnDetailsById] = useState<Record<string, boolean>>({});
  const [expandedToolById, setExpandedToolById] = useState<Record<string, boolean>>({});
  const [expandedMessageById, setExpandedMessageById] = useState<Record<string, boolean>>({});
  const [draftAttachments, setDraftAttachments] = useState<MessageAttachment[]>([]);
  const methodsRef = useRef(null);
  const dropScopeRef = useRef<HTMLDivElement | null>(null);
  const context: WorkbenchMessageListContext = { loaded: true, loadingOlder: false };

  return (
    <VirtuosoMessageListTestingContext.Provider value={{ viewportHeight: 600, itemHeight: 120 }}>
      <SessionWorkbenchPane
        id="session-1"
        entryLoadState={undefined}
        entryError={null}
        session={session}
        sessionError={null}
        sessionLoadIssues={[]}
        attachmentError={attachmentError}
        dropActive={false}
        dropScopeRef={dropScopeRef}
        listItems={listItems.current}
        liveTailItems={[]}
        events={[]}
        messages={[]}
        worktreeId={null}
        handleFileOpenError={() => {}}
        activeAskToolCallId={null}
        expandedTurnHeaders={expandedTurnHeaders}
        setExpandedTurnHeaders={setExpandedTurnHeaders}
        expandedTurnDetailsById={expandedTurnDetailsById}
        setExpandedTurnDetailsById={setExpandedTurnDetailsById}
        expandedToolById={expandedToolById}
        setExpandedToolById={setExpandedToolById}
        expandedMessageById={expandedMessageById}
        setExpandedMessageById={setExpandedMessageById}
        turnToolsLoading={[]}
        verbosity="default"
        onCancelAskUserQuestion={async () => {}}
        onSubmitAskUserQuestion={async () => {}}
        onRequestTurnTools={() => {}}
        showDebug={false}
        debugEvents={[]}
        authUi={{ status: "ok", methods: [] }}
        authMethodId=""
        onAuthMethodChange={() => {}}
        authBusy={false}
        authError={null}
        onAuthenticate={async () => {}}
        onRetrySessionLoads={() => {}}
        subagentInvocations={[]}
        onOpenChildSession={() => {}}
        isActive
        style={{ height: 400 }}
        itemIdentity={(item) => item.id}
        itemKey={(item) => item.id}
        increaseViewportBy={240}
        initialData={listItems.current}
        initialLocation={{ index: 0, align: "start" }}
        threadProjectionOp={{ kind: "noop", projectionRevision: 0, changedItemIds: [], remeasureItemIds: [] }}
        context={context}
        onScroll={() => {}}
        onRenderedDataChange={() => {}}
        methodsRef={methodsRef}
        licenseKey=""
        shortSizeAlign="top"
        queueForPanel={[]}
        pendingQueueMessageIdSet={new Set<string>()}
        queueActionBusy={false}
        sendBusy={false}
        onSendQueuedNow={async () => {}}
        onEditQueued={async () => {}}
        onRemoveQueued={async () => {}}
        input=""
        setInput={() => {}}
        slashCommands={[]}
        draftAttachments={draftAttachments}
        setDraftAttachments={setDraftAttachments}
        onAttachmentError={() => {}}
        sendNow={async () => {}}
        hasDraftContent={false}
        hasActiveTurn={false}
        atBottom={true}
        setVerbosityPref={() => {}}
        workbenchMode="default"
        setWorkbenchMode={() => {}}
        contextWindow={null}
        dictationRecording={false}
        onToggleRecording={() => {}}
        onInterruptSession={null}
        sendError={null}
        fileOpenError={null}
        dictationDebugText={null}
        dictationError={null}
        dictationOnboarding={null}
        dismissDictationOnboarding={() => {}}
        backDictationOnboarding={() => {}}
        chooseDictationOnboardingLocal={() => {}}
        chooseDictationOnboardingCloud={() => {}}
        updateDictationOnboardingCloud={() => {}}
        submitDictationOnboardingCloud={async () => {}}
        submitDictationOnboardingLocal={async () => {}}
        providerGuardNotice={null}
        providerGuardHeading=""
        providerGuardMessage=""
        providerGuardProviderLabel={undefined}
        providerGuardPidLabel={null}
        providerGuardMemoryLimitMb={null}
        providerGuardLimitLabel=""
        providerGuardActionBusy={false}
        providerGuardActionError={null}
        canRaiseProviderGuard={false}
        onRaiseProviderGuardLimit={async () => {}}
        onDisableProviderGuard={async () => {}}
        formatMemoryMb={() => "0 MB"}
        availableModels={[]}
        currentModelId=""
        onSetModelId={async () => {}}
        modelSwitchError={null}
        interruptSessionId={interruptSessionId}
      />
    </VirtuosoMessageListTestingContext.Provider>
  );
}

function TestMessagePane() {
  const longMessage = Array.from({ length: 24 }, (_, index) => `line ${index + 1}`).join("\n");
  const listItems = useRef<WorkbenchListItem[]>([
    {
      kind: "message",
      id: "message-1",
      role: "user",
      content: longMessage,
      attachments: [],
      created_at: "2025-01-01T00:00:00.000Z",
    },
  ]);
  const [expandedTurnHeaders, setExpandedTurnHeaders] = useState<Record<string, boolean>>({});
  const [expandedTurnDetailsById, setExpandedTurnDetailsById] = useState<Record<string, boolean>>({});
  const [expandedToolById, setExpandedToolById] = useState<Record<string, boolean>>({});
  const [expandedMessageById, setExpandedMessageById] = useState<Record<string, boolean>>({});
  const [draftAttachments, setDraftAttachments] = useState<MessageAttachment[]>([]);
  const methodsRef = useRef(null);
  const dropScopeRef = useRef<HTMLDivElement | null>(null);
  const context: WorkbenchMessageListContext = { loaded: true, loadingOlder: false };

  return (
    <VirtuosoMessageListTestingContext.Provider value={{ viewportHeight: 600, itemHeight: 120 }}>
      <SessionWorkbenchPane
        id="session-1"
        entryLoadState={undefined}
        entryError={null}
        session={null}
        sessionError={null}
        sessionLoadIssues={[]}
        attachmentError={null}
        dropActive={false}
        dropScopeRef={dropScopeRef}
        listItems={listItems.current}
        liveTailItems={[]}
        events={[]}
        messages={[]}
        worktreeId={null}
        handleFileOpenError={() => {}}
        activeAskToolCallId={null}
        expandedTurnHeaders={expandedTurnHeaders}
        setExpandedTurnHeaders={setExpandedTurnHeaders}
        expandedTurnDetailsById={expandedTurnDetailsById}
        setExpandedTurnDetailsById={setExpandedTurnDetailsById}
        expandedToolById={expandedToolById}
        setExpandedToolById={setExpandedToolById}
        expandedMessageById={expandedMessageById}
        setExpandedMessageById={setExpandedMessageById}
        turnToolsLoading={[]}
        verbosity="default"
        onCancelAskUserQuestion={async () => {}}
        onSubmitAskUserQuestion={async () => {}}
        onRequestTurnTools={() => {}}
        showDebug={false}
        debugEvents={[]}
        authUi={{ status: "ok", methods: [] }}
        authMethodId=""
        onAuthMethodChange={() => {}}
        authBusy={false}
        authError={null}
        onAuthenticate={async () => {}}
        onRetrySessionLoads={() => {}}
        subagentInvocations={[]}
        onOpenChildSession={() => {}}
        isActive
        style={{ height: 400 }}
        itemIdentity={(item) => item.id}
        itemKey={(item) => item.id}
        increaseViewportBy={240}
        initialData={listItems.current}
        initialLocation={{ index: 0, align: "start" }}
        threadProjectionOp={{ kind: "noop", projectionRevision: 0, changedItemIds: [], remeasureItemIds: [] }}
        context={context}
        onScroll={() => {}}
        onRenderedDataChange={() => {}}
        methodsRef={methodsRef}
        licenseKey=""
        shortSizeAlign="top"
        queueForPanel={[]}
        pendingQueueMessageIdSet={new Set<string>()}
        queueActionBusy={false}
        sendBusy={false}
        onSendQueuedNow={async () => {}}
        onEditQueued={async () => {}}
        onRemoveQueued={async () => {}}
        input=""
        setInput={() => {}}
        slashCommands={[]}
        draftAttachments={draftAttachments}
        setDraftAttachments={setDraftAttachments}
        onAttachmentError={() => {}}
        sendNow={async () => {}}
        hasDraftContent={false}
        hasActiveTurn={false}
        atBottom={true}
        setVerbosityPref={() => {}}
        workbenchMode="default"
        setWorkbenchMode={() => {}}
        contextWindow={null}
        dictationRecording={false}
        onToggleRecording={() => {}}
        onInterruptSession={null}
        sendError={null}
        fileOpenError={null}
        dictationDebugText={null}
        dictationError={null}
        dictationOnboarding={null}
        dismissDictationOnboarding={() => {}}
        backDictationOnboarding={() => {}}
        chooseDictationOnboardingLocal={() => {}}
        chooseDictationOnboardingCloud={() => {}}
        updateDictationOnboardingCloud={() => {}}
        submitDictationOnboardingCloud={async () => {}}
        submitDictationOnboardingLocal={async () => {}}
        providerGuardNotice={null}
        providerGuardHeading=""
        providerGuardMessage=""
        providerGuardProviderLabel={undefined}
        providerGuardPidLabel={null}
        providerGuardMemoryLimitMb={null}
        providerGuardLimitLabel=""
        providerGuardActionBusy={false}
        providerGuardActionError={null}
        canRaiseProviderGuard={false}
        onRaiseProviderGuardLimit={async () => {}}
        onDisableProviderGuard={async () => {}}
        formatMemoryMb={() => "0 MB"}
        availableModels={[]}
        currentModelId=""
        onSetModelId={async () => {}}
        modelSwitchError={null}
        interruptSessionId=""
      />
    </VirtuosoMessageListTestingContext.Provider>
  );
}

function TestAssistantPane() {
  const longAssistant = Array.from({ length: 24 }, (_, index) => `assistant line ${index + 1}`).join("\n");
  const listItems = useRef<WorkbenchListItem[]>([
    {
      kind: "assistant",
      id: "assistant-1",
      turn_id: "turn-1",
      created_at: "2025-01-01T00:00:00.000Z",
      content: longAssistant,
      thought: "",
      is_complete: true,
    },
  ]);
  const [expandedTurnHeaders, setExpandedTurnHeaders] = useState<Record<string, boolean>>({});
  const [expandedTurnDetailsById, setExpandedTurnDetailsById] = useState<Record<string, boolean>>({});
  const [expandedToolById, setExpandedToolById] = useState<Record<string, boolean>>({});
  const [expandedMessageById, setExpandedMessageById] = useState<Record<string, boolean>>({});
  const [draftAttachments, setDraftAttachments] = useState<MessageAttachment[]>([]);
  const methodsRef = useRef(null);
  const dropScopeRef = useRef<HTMLDivElement | null>(null);
  const context: WorkbenchMessageListContext = { loaded: true, loadingOlder: false };

  return (
    <VirtuosoMessageListTestingContext.Provider value={{ viewportHeight: 600, itemHeight: 120 }}>
      <SessionWorkbenchPane
        id="session-1"
        entryLoadState={undefined}
        entryError={null}
        session={null}
        sessionError={null}
        sessionLoadIssues={[]}
        attachmentError={null}
        dropActive={false}
        dropScopeRef={dropScopeRef}
        listItems={listItems.current}
        liveTailItems={[]}
        events={[]}
        messages={[]}
        worktreeId={null}
        handleFileOpenError={() => {}}
        activeAskToolCallId={null}
        expandedTurnHeaders={expandedTurnHeaders}
        setExpandedTurnHeaders={setExpandedTurnHeaders}
        expandedTurnDetailsById={expandedTurnDetailsById}
        setExpandedTurnDetailsById={setExpandedTurnDetailsById}
        expandedToolById={expandedToolById}
        setExpandedToolById={setExpandedToolById}
        expandedMessageById={expandedMessageById}
        setExpandedMessageById={setExpandedMessageById}
        turnToolsLoading={[]}
        verbosity="default"
        onCancelAskUserQuestion={async () => {}}
        onSubmitAskUserQuestion={async () => {}}
        onRequestTurnTools={() => {}}
        showDebug={false}
        debugEvents={[]}
        authUi={{ status: "ok", methods: [] }}
        authMethodId=""
        onAuthMethodChange={() => {}}
        authBusy={false}
        authError={null}
        onAuthenticate={async () => {}}
        onRetrySessionLoads={() => {}}
        subagentInvocations={[]}
        onOpenChildSession={() => {}}
        isActive
        style={{ height: 400 }}
        itemIdentity={(item) => item.id}
        itemKey={(item) => item.id}
        increaseViewportBy={240}
        initialData={listItems.current}
        initialLocation={{ index: 0, align: "start" }}
        threadProjectionOp={{ kind: "noop", projectionRevision: 0, changedItemIds: [], remeasureItemIds: [] }}
        context={context}
        onScroll={() => {}}
        onRenderedDataChange={() => {}}
        methodsRef={methodsRef}
        licenseKey=""
        shortSizeAlign="top"
        queueForPanel={[]}
        pendingQueueMessageIdSet={new Set<string>()}
        queueActionBusy={false}
        sendBusy={false}
        onSendQueuedNow={async () => {}}
        onEditQueued={async () => {}}
        onRemoveQueued={async () => {}}
        input=""
        setInput={() => {}}
        slashCommands={[]}
        draftAttachments={draftAttachments}
        setDraftAttachments={setDraftAttachments}
        onAttachmentError={() => {}}
        sendNow={async () => {}}
        hasDraftContent={false}
        hasActiveTurn={false}
        atBottom={true}
        setVerbosityPref={() => {}}
        workbenchMode="default"
        setWorkbenchMode={() => {}}
        contextWindow={null}
        dictationRecording={false}
        onToggleRecording={() => {}}
        onInterruptSession={null}
        sendError={null}
        fileOpenError={null}
        dictationDebugText={null}
        dictationError={null}
        dictationOnboarding={null}
        dismissDictationOnboarding={() => {}}
        backDictationOnboarding={() => {}}
        chooseDictationOnboardingLocal={() => {}}
        chooseDictationOnboardingCloud={() => {}}
        updateDictationOnboardingCloud={() => {}}
        submitDictationOnboardingCloud={async () => {}}
        submitDictationOnboardingLocal={async () => {}}
        providerGuardNotice={null}
        providerGuardHeading=""
        providerGuardMessage=""
        providerGuardProviderLabel={undefined}
        providerGuardPidLabel={null}
        providerGuardMemoryLimitMb={null}
        providerGuardLimitLabel=""
        providerGuardActionBusy={false}
        providerGuardActionError={null}
        canRaiseProviderGuard={false}
        onRaiseProviderGuardLimit={async () => {}}
        onDisableProviderGuard={async () => {}}
        formatMemoryMb={() => "0 MB"}
        availableModels={[]}
        currentModelId=""
        onSetModelId={async () => {}}
        modelSwitchError={null}
        interruptSessionId=""
      />
    </VirtuosoMessageListTestingContext.Provider>
  );
}

describe("SessionWorkbenchPane", () => {
  beforeEach(() => {
    document.body.innerHTML = "";
    copyTextToClipboardMock.mockClear();
    class ResizeObserverStub {
      observe() {}
      unobserve() {}
      disconnect() {}
    }
    Object.defineProperty(globalThis, "ResizeObserver", {
      configurable: true,
      writable: true,
      value: ResizeObserverStub,
    });
  });

  it("always exposes the mounted session id", () => {
    render(
      <TestPane
        session={{
          id: "session-1",
          task_id: "task-1",
          workspace_id: "workspace-1",
          worktree_id: "worktree-1",
          provider_id: "fake",
          model_id: "fake-model",
          title: "Session 1",
          agent_role: "assistant",
          status: "starting",
          execution_environment: "sandbox",
          created_at: "2025-01-01T00:00:00.000Z",
          updated_at: "2025-01-01T00:00:00.000Z",
        }}
        interruptSessionId=""
      />,
    );

    expect(screen.getByTestId("session-view")).toHaveAttribute("data-session-id", "session-1");
  });

  it("does not let interrupt readiness change the mounted session id", () => {
    render(
      <TestPane
        session={{
          id: "session-1",
          task_id: "task-1",
          workspace_id: "workspace-1",
          worktree_id: "worktree-1",
          provider_id: "fake",
          model_id: "fake-model",
          title: "Session 1",
          agent_role: "assistant",
          status: "starting",
          execution_environment: "sandbox",
          created_at: "2025-01-01T00:00:00.000Z",
          updated_at: "2025-01-01T00:00:00.000Z",
        }}
        interruptSessionId="session-1"
      />,
    );

    expect(screen.getByTestId("session-view")).toHaveAttribute("data-session-id", "session-1");
  });

  it("renders attachment failures in the top session banner area", () => {
    const { container } = render(
      <TestPane attachmentError="Image attachments must be 25 MiB or smaller." />,
    );

    const banner = screen.getByTestId("session-attachment-error");
    expect(banner).toHaveTextContent("Attachment Failed");
    expect(banner).toHaveTextContent("Image attachments must be 25 MiB or smaller.");
    expect(
      container.querySelector(".wb-session-left > [data-testid='session-attachment-error']"),
    ).toBe(banner);
  });

  it("expands a turn header when clicked inside the virtualized workbench list", async () => {
    const { container } = render(<TestPane />);
    const initialHeader = container.querySelector(".wb-turn-header");
    if (!(initialHeader instanceof HTMLDivElement)) {
      throw new Error("Expected .wb-turn-header to be rendered");
    }

    expect(initialHeader).toHaveAttribute("aria-expanded", "false");

    fireEvent.mouseDown(initialHeader);
    fireEvent.click(initialHeader);

    await waitFor(() => {
      const updatedHeader = container.querySelector(".wb-turn-header");
      expect(updatedHeader).toHaveAttribute("aria-expanded", "true");
    });
  });

  it("copies without expanding when the copy icon is clicked inside the virtualized workbench list", async () => {
    const { container } = render(<TestPane />);
    const header = container.querySelector(".wb-turn-header");
    if (!(header instanceof HTMLDivElement)) {
      throw new Error("Expected .wb-turn-header to be rendered");
    }
    const copyButton = container.querySelector(".wb-turn-header-copy");
    if (!(copyButton instanceof HTMLButtonElement)) {
      throw new Error("Expected .wb-turn-header-copy to be rendered");
    }

    expect(header).toHaveAttribute("aria-expanded", "false");
    expect(copyButton).toHaveAttribute("aria-label", "Copy message");

    fireEvent.mouseDown(copyButton);
    fireEvent.click(copyButton);

    expect(copyTextToClipboardMock).toHaveBeenCalledWith("line 1\nline 2\nline 3\nline 4\nline 5");
    expect(header).toHaveAttribute("aria-expanded", "false");
    await waitFor(() => {
      expect(copyButton).toHaveAttribute("aria-label", "Copied");
    });
  });

  it("stores long message expansion outside the row-local component state", () => {
    const { container } = render(<TestMessagePane />);
    const toggle = container.querySelector(".link");
    if (!(toggle instanceof HTMLButtonElement)) {
      throw new Error("Expected long message toggle button to render");
    }

    expect(toggle.textContent).toBe("Show more");
    expect(container.textContent).not.toContain("line 24");

    fireEvent.click(toggle);

    expect(toggle.textContent).toBe("Show less");
    expect(container.textContent).toContain("line 24");
  });

  it("renders long completed assistant content without a collapse toggle", () => {
    const { container } = render(<TestAssistantPane />);
    expect(container.querySelector('button[aria-controls^="assistant-"]')).toBeNull();
    expect(container.textContent).toContain("assistant line 24");
  });
});
