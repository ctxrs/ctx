import React from "react";
import { act, render, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { postMessage, type MessageAttachment, type Message, type SessionEvent, type SessionTurn } from "../api/client";
import { buildSessionThreadProjectionFromSnapshot } from "../state/sessionThreadProjection/applySnapshot";
import { SessionView } from "./SessionPage";

const paneSpy = vi.hoisted(() => vi.fn());
const sessionEntries = vi.hoisted(() => ({ map: {} as Record<string, unknown> }));

vi.mock("../api/client", () => ({
  deleteMessage: vi.fn(async () => ({})),
  postMessage: vi.fn(async () => ({})),
  setSessionModel: vi.fn(async () => ({})),
  authenticateSession: vi.fn(async () => ({})),
  idToString: (id: string | null | undefined) => {
    if (id == null) return "";
    return String(id);
  },
  interruptSession: vi.fn(async () => ({})),
  submitAskUserQuestion: vi.fn(async () => ({})),
  uploadBlob: vi.fn(async () => ({ blob_id: "blob-1" })),
}));

vi.mock("../state/sessionSupervisor", () => ({
  useSessionSupervisor: () => ({
    refreshQueue: vi.fn(async () => {}),
    refreshSession: vi.fn(async () => {}),
    loadMoreTurns: vi.fn(async () => {}),
    loadTurnTools: vi.fn(async () => {}),
    setSession: vi.fn(),
    upsertOptimisticThreadMessage: vi.fn(),
    removeOptimisticThreadMessage: vi.fn(),
    upsertOptimisticQueuedMessage: vi.fn(),
    removeOptimisticQueuedMessage: vi.fn(),
    addOptimisticQueueRemovalId: vi.fn(),
    removeOptimisticQueueRemovalId: vi.fn(),
  }),
  useSessionEntry: (id: string) => sessionEntries.map[id] ?? null,
  useOpenSession: () => {},
}));

vi.mock("../state/uiStateStore", () => ({
  loadSessionViewPrefsV1: vi.fn(async () => null),
  saveSessionViewPrefsV1: vi.fn(async () => {}),
}));

vi.mock("../components/AskUserQuestionCard", () => ({
  AskUserQuestionCard: () => null,
}));

vi.mock("./sessionView/SessionWorkbenchPane", () => ({
  SessionWorkbenchPane: (props: unknown) => {
    paneSpy(props);
    return <div data-testid="session-workbench-pane" />;
  },
}));

vi.mock("./useWorkbenchThreadViewModelController", () => ({
  useWorkbenchThreadViewModelController: () => ({
    view: { debugEvents: [] },
    listItems: [],
    projectionRevision: 0,
    lastOp: {
      kind: "noop",
      projectionRevision: 0,
      changedItemIds: [],
      remeasureItemIds: [],
    },
    changedItemIds: [],
    remeasureItemIds: [],
  }),
}));

vi.mock("./useSessionTranscriptController", () => ({
  useSessionTranscriptController: () => ({
    threadProjectionOp: {
      kind: "noop",
      projectionRevision: 0,
      changedItemIds: [],
      remeasureItemIds: [],
    },
    itemIdentity: (item: { id: string }) => item.id,
    itemKey: (item: { id: string }) => item.id,
    methodsRef: { current: null },
    context: null,
    initialData: [],
    initialLocation: null,
    onScroll: vi.fn(),
    onRenderedDataChange: vi.fn(),
  }),
}));

vi.mock("./sessionView/useSessionImageDropScope", () => ({
  useSessionImageDropScope: () => ({
    dropScopeRef: { current: null },
    dropActive: false,
  }),
}));

vi.mock("./sessionView/useSessionProviderGuard", () => ({
  useSessionProviderGuard: () => ({
    providerGuardActionError: null,
    providerGuardActionBusy: false,
    providerGuardMemoryLimitMb: null,
    providerGuardHeading: "",
    providerGuardMessage: "",
    providerGuardLimitLabel: "",
    providerGuardProviderLabel: "",
    providerGuardPidLabel: "",
    canRaiseProviderGuard: false,
    raiseProviderGuardLimit: vi.fn(async () => {}),
    disableProviderGuard: vi.fn(async () => {}),
  }),
}));

vi.mock("./sessionView/useStableAskUserQuestionAnswers", () => ({
  useStableAskUserQuestionAnswers: () => new Map(),
}));

vi.mock("./sessionView/useSharedSessionProviderOptions", () => ({
  useSharedSessionProviderOptions: () => ({
    provider_id: "codex",
    workspace_id: "ws-1",
    supports_load: false,
    auth_required: false,
    has_active_auth: true,
    auth_mode: "subscription",
    probed_at: "2026-03-10T00:00:00.000Z",
    models: {
      current_model_id: "gpt-5.4/medium",
      models: [{ id: "gpt-5.4/medium" }],
    },
  }),
}));

vi.mock("../utils/analytics", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../utils/analytics")>();
  return {
    ...actual,
    useFeatureGate: () => false,
  };
});

vi.mock("../utils/useDictationController", () => ({
  useDictationController: () => ({
    dictationRecording: false,
    dictationError: null,
    dictationDebugText: "",
    dictationOnboarding: null,
    dismissDictationOnboarding: vi.fn(),
    backDictationOnboarding: vi.fn(),
    chooseDictationOnboardingLocal: vi.fn(),
    chooseDictationOnboardingCloud: vi.fn(),
    updateDictationOnboardingCloud: vi.fn(),
    submitDictationOnboardingLocal: vi.fn(),
    submitDictationOnboardingCloud: vi.fn(),
    startDictation: vi.fn(async () => {}),
    stopDictation: vi.fn(async () => ""),
  }),
}));

vi.mock("../workbench/store", () => ({
  useWorkbenchStore: () => ({
    focusTask: vi.fn(),
  }),
}));

const sessionId = "session-1";
const postMessageMock = vi.mocked(postMessage);

const buildAttachment = (blobId: string): MessageAttachment => ({
  kind: "image_ref",
  blob_id: blobId,
  mime_type: "image/png",
  name: `${blobId}.png`,
});

beforeEach(() => {
  paneSpy.mockClear();
  postMessageMock.mockClear();
  sessionEntries.map = {
    [sessionId]: {
      sessionId,
      session: {
        id: sessionId,
        task_id: "task-1",
        workspace_id: "ws-1",
        worktree_id: "wt-1",
        provider_id: "codex",
        model_id: "gpt-5.4",
        reasoning_effort: "medium",
        title: "Session 1",
        agent_role: "assistant",
        status: "active",
        execution_environment: "host",
        created_at: "2026-03-10T00:00:00.000Z",
        updated_at: "2026-03-10T00:00:00.000Z",
      },
      acpCurrentModelId: "gpt-5.4/medium",
      turns: [],
      turnToolsByTurnId: {},
      turnToolsLoading: [],
      toolSummariesReady: true,
      hasMoreTurns: false,
      events: [],
      messages: [],
      artifacts: [],
      artifactsLoading: false,
      subagentInvocations: [],
      subagentInvocationsLoading: false,
      stateLoaded: true,
      stateLoading: false,
      turnsRev: 0,
      messagesRev: 0,
      eventsRev: 0,
      queue: [],
      loading: false,
      subscribed: true,
      updatedAtMs: 0,
    },
  };
});

describe("SessionPage draft attachments", () => {
  it("renders thread content from the supervisor threadProjection instead of raw entry transcript fields", async () => {
    const rawMessage: Message = {
      id: "raw-message",
      session_id: sessionId,
      task_id: "task-1",
      turn_id: "raw-turn",
      role: "user",
      content: "raw message",
      attachments: [],
      delivery: "immediate",
      created_at: "2026-03-10T00:00:00.000Z",
    };
    const projectedMessage: Message = {
      ...rawMessage,
      id: "projected-message",
      turn_id: "projected-turn",
      content: "projected message",
    };
    const rawEvent: SessionEvent = {
      seq: 1,
      id: "raw-event",
      session_id: sessionId,
      run_id: "run-1",
      turn_id: "raw-turn",
      event_type: "assistant_chunk",
      payload_json: { content_fragment: "raw event" },
      created_at: "2026-03-10T00:00:00.000Z",
    };
    const projectedEvent: SessionEvent = {
      ...rawEvent,
      seq: 2,
      id: "projected-event",
      turn_id: "projected-turn",
      payload_json: { content_fragment: "projected event" },
    };
    const rawTurn: SessionTurn = {
      turn_id: "raw-turn",
      session_id: sessionId,
      user_message_id: "raw-message",
      status: "running",
      started_at: "2026-03-10T00:00:00.000Z",
      updated_at: "2026-03-10T00:00:00.000Z",
      tool_total: 0,
      tool_pending: 0,
      tool_running: 0,
      tool_completed: 0,
      tool_failed: 0,
    };
    const projectedTurn: SessionTurn = {
      ...rawTurn,
      turn_id: "projected-turn",
      user_message_id: "projected-message",
      status: "completed",
    };
    const threadProjection = buildSessionThreadProjectionFromSnapshot({
      stateLoaded: true,
      turns: [projectedTurn],
      turnsRev: 7,
      messages: [projectedMessage],
      messagesRev: 8,
      events: [projectedEvent],
      eventsRev: 9,
      turnToolsByTurnId: {},
      toolSummariesReady: true,
      projectionRev: 11,
    });

    sessionEntries.map[sessionId] = {
      ...(sessionEntries.map[sessionId] as Record<string, unknown>),
      turns: [rawTurn],
      messages: [rawMessage],
      events: [rawEvent],
      turnsRev: 1,
      messagesRev: 1,
      eventsRev: 1,
      threadProjection,
      projectionRev: 11,
    };

    render(<SessionView sessionId={sessionId} />);

    await waitFor(() => {
      expect(paneSpy).toHaveBeenCalled();
    });

    const props = paneSpy.mock.calls.at(-1)?.[0] as {
      messages: unknown[];
      events: unknown[];
    };

    expect(props.messages).toEqual(threadProjection.messages);
    expect(props.events).toEqual(threadProjection.events);
    expect(props.messages).not.toEqual([rawMessage]);
    expect(props.events).not.toEqual([rawEvent]);
  });

  it("renders persisted draft attachments and routes edits back through the draft callbacks", async () => {
    const firstAttachment = buildAttachment("blob-1");
    const secondAttachment = buildAttachment("blob-2");
    const onDraftAttachmentsChange = vi.fn();

    render(
      <SessionView
        sessionId={sessionId}
        draft={{ text: "hello", modeId: "default", attachments: [firstAttachment] }}
        onDraftChange={vi.fn()}
        onDraftAttachmentsChange={onDraftAttachmentsChange}
      />,
    );

    await waitFor(() => {
      expect(paneSpy).toHaveBeenCalled();
    });

    const props = paneSpy.mock.calls.at(-1)?.[0] as
      | {
          draftAttachments: MessageAttachment[];
          setDraftAttachments: React.Dispatch<React.SetStateAction<MessageAttachment[]>>;
        }
      | undefined;

    expect(props?.draftAttachments).toEqual([firstAttachment]);

    await act(async () => {
      props?.setDraftAttachments((prev) => [...prev, secondAttachment]);
    });

    expect(onDraftAttachmentsChange).toHaveBeenCalledWith([firstAttachment, secondAttachment]);
  });

  it("clears a controlled draft and still posts the message on send", async () => {
    const firstAttachment = buildAttachment("blob-1");

    function DraftHarness() {
      const [draft, setDraft] = React.useState<{
        text: string;
        modeId: "default";
        attachments: MessageAttachment[];
      }>({
        text: "hello",
        modeId: "default",
        attachments: [firstAttachment],
      });

      return (
        <SessionView
          sessionId={sessionId}
          draft={draft}
          onDraftChange={(text) => setDraft((prev) => ({ ...prev, text }))}
          onDraftAttachmentsChange={(attachments) =>
            setDraft((prev) => ({ ...prev, attachments }))
          }
        />
      );
    }

    render(<DraftHarness />);

    await waitFor(() => {
      expect(paneSpy).toHaveBeenCalled();
    });

    await act(async () => {
      const props = paneSpy.mock.calls.at(-1)?.[0] as { sendNow: () => Promise<void> };
      await props.sendNow();
    });

    await waitFor(() => {
      const latest = paneSpy.mock.calls.at(-1)?.[0] as
        | { input: string; draftAttachments: MessageAttachment[] }
        | undefined;
      expect(latest?.input).toBe("");
      expect(latest?.draftAttachments).toEqual([]);
    });

    expect(postMessageMock).toHaveBeenCalledTimes(1);
    expect(postMessageMock.mock.calls[0]?.[0]).toBe(sessionId);
    expect(postMessageMock.mock.calls[0]?.[1]).toBe("hello");
  });
});
