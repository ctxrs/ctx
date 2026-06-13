import React from "react";
import { act, render, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ProviderOptions } from "../api/client";
import { SessionView } from "./SessionPage";
import { deleteMessage, interruptSession, postMessage } from "../api/client";

type Deferred<T> = {
  promise: Promise<T>;
  resolve: (value: T | PromiseLike<T>) => void;
  reject: (reason?: unknown) => void;
};

function createDeferred<T>(): Deferred<T> {
  let resolve!: (value: T | PromiseLike<T>) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

const {
  paneSpy,
  sessionEntries,
  featureGateMock,
  sharedProviderOptionsState,
  setSessionModelMock,
  updateWorkspaceProviderModelPreferenceMock,
  setSessionSpy,
  refreshProvidersBootstrapMock,
} = vi.hoisted(() => ({
  paneSpy: vi.fn(),
  sessionEntries: { map: {} as Record<string, unknown> },
  featureGateMock: vi.fn(() => false),
  sharedProviderOptionsState: {
    value: {
      provider_id: "codex",
      workspace_id: "ws-1",
      supports_load: false,
      auth_required: false,
      has_active_auth: true,
      auth_mode: "subscription",
      probed_at: "2026-03-10T00:00:00.000Z",
      models: {
        current_model_id: "gpt-5.4/medium",
        models: [{ id: "gpt-5.4/medium" }, { id: "gpt-5.4/xhigh" }],
      },
    } as ProviderOptions | undefined,
  },
  setSessionModelMock: vi.fn(async () => ({})),
  updateWorkspaceProviderModelPreferenceMock: vi.fn(async () => ({})),
  setSessionSpy: vi.fn(),
  refreshProvidersBootstrapMock: vi.fn(async () => undefined),
}));

vi.mock("../api/client", () => ({
  deleteMessage: vi.fn(async () => ({})),
  postMessage: vi.fn(async () => ({})),
  setSessionModel: setSessionModelMock,
  updateWorkspaceProviderModelPreference: updateWorkspaceProviderModelPreferenceMock,
  authenticateSession: vi.fn(async () => ({})),
  idToString: (id: string | null | undefined) => {
    if (id === null || id === undefined) return "";
    return String(id);
  },
  interruptSession: vi.fn(async () => ({})),
  recordClientHistogramMetric: vi.fn(async () => ({})),
  uploadBlob: vi.fn(async () => ({ blob_id: "blob-1" })),
}));

vi.mock("../state/providersBootstrapStore", () => ({
  refreshProvidersBootstrap: refreshProvidersBootstrapMock,
}));

vi.mock("../state/sessionSupervisor", () => ({
  useSessionSupervisor: () => ({
    refreshQueue: vi.fn(async () => {}),
    refreshSession: vi.fn(async () => {}),
    loadMoreTurns: vi.fn(async () => {}),
    loadTurnTools: vi.fn(async () => {}),
    setSession: setSessionSpy,
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
  useSharedSessionProviderOptions: () => sharedProviderOptionsState.value,
}));

vi.mock("../utils/analytics", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../utils/analytics")>();
  return {
    ...actual,
    useFeatureGate: featureGateMock,
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
const deleteMessageMock = vi.mocked(deleteMessage);
const interruptSessionMock = vi.mocked(interruptSession);
const postMessageMock = vi.mocked(postMessage);

beforeEach(() => {
  paneSpy.mockClear();
  deleteMessageMock.mockClear();
  deleteMessageMock.mockResolvedValue({});
  interruptSessionMock.mockClear();
  interruptSessionMock.mockResolvedValue({});
  postMessageMock.mockClear();
  featureGateMock.mockReset();
  featureGateMock.mockReturnValue(false);
  setSessionModelMock.mockReset();
  setSessionModelMock.mockResolvedValue({});
  updateWorkspaceProviderModelPreferenceMock.mockReset();
  updateWorkspaceProviderModelPreferenceMock.mockResolvedValue({});
  setSessionSpy.mockReset();
  refreshProvidersBootstrapMock.mockReset();
  setSessionSpy.mockImplementation((updated: unknown) => {
    const current = sessionEntries.map[sessionId] as { session?: unknown } | undefined;
    if (!current) return;
    sessionEntries.map[sessionId] = {
      ...current,
      session: updated,
    };
  });
  sharedProviderOptionsState.value = {
    provider_id: "codex",
    workspace_id: "ws-1",
    supports_load: false,
    auth_required: false,
    has_active_auth: true,
    auth_mode: "subscription",
    probed_at: "2026-03-10T00:00:00.000Z",
    models: {
      current_model_id: "gpt-5.4/medium",
      models: [{ id: "gpt-5.4/medium" }, { id: "gpt-5.4/xhigh" }],
    },
  };
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
        reasoning_effort: "xhigh",
        title: "Session 1",
        agent_role: "assistant",
        status: "active",
        execution_environment: "host",
        created_at: "2026-03-10T00:00:00.000Z",
        updated_at: "2026-03-10T00:00:00.000Z",
      },
      acpModels: {
        current_model_id: "gpt-5.4/medium",
        models: [{ id: "gpt-5.4/medium" }, { id: "gpt-5.4/xhigh" }],
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

describe("SessionPage reasoning effort", () => {
  it("uses session-owned reasoning effort for the active session selector instead of ACP current model defaults", async () => {
    render(<SessionView sessionId={sessionId} />);

    await waitFor(() => {
      expect(paneSpy).toHaveBeenCalled();
    });

    const lastCall = paneSpy.mock.calls.at(-1)?.[0] as {
      currentModelId?: string;
      availableModels?: Array<{ id: string }>;
    } | undefined;
    expect(lastCall?.currentModelId).toBe("gpt-5.4/xhigh");
    expect(lastCall?.availableModels?.map((model) => model.id)).toEqual([
      "gpt-5.4/medium",
      "gpt-5.4/xhigh",
    ]);
  });

  it("prefers a versioned Claude label for active-session display", async () => {
    sharedProviderOptionsState.value = {
      provider_id: "claude",
      workspace_id: "ws-1",
      supports_load: false,
      auth_required: false,
      has_active_auth: true,
      auth_mode: "subscription",
      probed_at: "2026-03-10T00:00:00.000Z",
      models: {
        current_model_id: "opus/high",
        models: [
          { id: "opus/high", name: "Opus 4.7 (High)" },
          { id: "opus/medium", name: "Opus 4.7 (Medium)" },
        ],
      },
    };
    sessionEntries.map[sessionId] = {
      ...(sessionEntries.map[sessionId] ?? {}),
      session: {
        id: sessionId,
        task_id: "task-1",
        workspace_id: "ws-1",
        worktree_id: "wt-1",
        provider_id: "claude",
        model_id: "opus",
        reasoning_effort: "high",
        title: "Session 1",
        agent_role: "assistant",
        status: "active",
        execution_environment: "host",
        created_at: "2026-03-10T00:00:00.000Z",
        updated_at: "2026-03-10T00:00:00.000Z",
      },
      acpModels: {
        current_model_id: "claude-opus-4-7/high",
        models: [
          { id: "opus/high", name: "Opus 4.7 (High)" },
          { id: "opus/medium", name: "Opus 4.7 (Medium)" },
          { id: "claude-opus-4-7/high" },
        ],
      },
      acpCurrentModelId: "claude-opus-4-7/high",
    };

    render(<SessionView sessionId={sessionId} />);

    await waitFor(() => {
      expect(paneSpy).toHaveBeenCalled();
    });

    const lastCall = paneSpy.mock.calls.at(-1)?.[0] as {
      currentModelId?: string;
      currentModelDisplayLabel?: string;
    } | undefined;
    expect(lastCall?.currentModelId).toBe("opus/high");
    expect(lastCall?.currentModelDisplayLabel).toBe("Opus 4.7");
  });

  it("prefers the resolved Claude runtime model over the default alias label", async () => {
    sharedProviderOptionsState.value = {
      provider_id: "claude",
      workspace_id: "ws-1",
      supports_load: false,
      auth_required: false,
      has_active_auth: true,
      auth_mode: "subscription",
      probed_at: "2026-03-10T00:00:00.000Z",
      models: {
        current_model_id: "default/high",
        models: [
          { id: "default/high", name: "Default (Sonnet 4.6) (High)" },
          { id: "default/medium", name: "Default (Sonnet 4.6) (Medium)" },
        ],
      },
    };
    sessionEntries.map[sessionId] = {
      ...(sessionEntries.map[sessionId] ?? {}),
      session: {
        id: sessionId,
        task_id: "task-1",
        workspace_id: "ws-1",
        worktree_id: "wt-1",
        provider_id: "claude",
        model_id: "default",
        reasoning_effort: "high",
        title: "Session 1",
        agent_role: "assistant",
        status: "active",
        execution_environment: "host",
        created_at: "2026-03-10T00:00:00.000Z",
        updated_at: "2026-03-10T00:00:00.000Z",
      },
      acpModels: {
        current_model_id: "claude-sonnet-4-6/high",
        models: [
          { id: "default/high", name: "Default (Sonnet 4.6) (High)" },
          { id: "claude-sonnet-4-6/high" },
        ],
      },
      acpCurrentModelId: "claude-sonnet-4-6/high",
    };

    render(<SessionView sessionId={sessionId} />);

    await waitFor(() => {
      expect(paneSpy).toHaveBeenCalled();
    });

    const lastCall = paneSpy.mock.calls.at(-1)?.[0] as {
      currentModelId?: string;
      currentModelDisplayLabel?: string;
    } | undefined;
    expect(lastCall?.currentModelId).toBe("default/high");
    expect(lastCall?.currentModelDisplayLabel).toBe("Sonnet 4.6");
  });

  it("prefers the shared provider catalog over cached ACP model metadata for available options", async () => {
    sharedProviderOptionsState.value = {
      provider_id: "codex",
      workspace_id: "ws-1",
      supports_load: false,
      auth_required: false,
      has_active_auth: true,
      auth_mode: "subscription",
      probed_at: "2026-03-10T00:00:00.000Z",
      models: {
        current_model_id: "gpt-5.4/medium",
        models: [
          { id: "gpt-5.4/low" },
          { id: "gpt-5.4/medium" },
          { id: "gpt-5.4/xhigh" },
        ],
      },
    };
    const existingSessionEntry = sessionEntries.map[sessionId];
    if (!existingSessionEntry) {
      throw new Error("missing session entry for ACP fallback test");
    }
    sessionEntries.map[sessionId] = {
      ...existingSessionEntry,
      acpModels: {
        current_model_id: "gpt-5.4/medium",
        models: [{ id: "gpt-5.4/medium" }, { id: "gpt-5.4/xhigh" }],
      },
    };

    render(<SessionView sessionId={sessionId} />);

    await waitFor(() => {
      expect(paneSpy).toHaveBeenCalled();
    });

    const lastCall = paneSpy.mock.calls.at(-1)?.[0] as {
      currentModelId?: string;
      availableModels?: Array<{ id: string }>;
    } | undefined;
    expect(lastCall?.currentModelId).toBe("gpt-5.4/xhigh");
    expect(lastCall?.availableModels?.map((model) => model.id)).toEqual([
      "gpt-5.4/low",
      "gpt-5.4/medium",
      "gpt-5.4/xhigh",
    ]);
  });

  it("keeps active-session model and effort options from ACP metadata when shared provider options are unavailable", async () => {
    sharedProviderOptionsState.value = undefined;

    render(<SessionView sessionId={sessionId} />);

    await waitFor(() => {
      expect(paneSpy).toHaveBeenCalled();
    });

    const lastCall = paneSpy.mock.calls.at(-1)?.[0] as {
      currentModelId?: string;
      availableModels?: Array<{ id: string }>;
    } | undefined;
    expect(lastCall?.currentModelId).toBe("gpt-5.4/xhigh");
    expect(lastCall?.availableModels?.map((model) => model.id)).toEqual([
      "gpt-5.4/medium",
      "gpt-5.4/xhigh",
    ]);
  });

  it("blocks sends when local active-turn state is only bootstrap and queueing is disabled", async () => {
    const existingEntry = (sessionEntries.map[sessionId] ?? {}) as Record<string, unknown>;
    sessionEntries.map[sessionId] = {
      ...existingEntry,
      freshness: "bootstrap",
      activity: { is_working: true, last_turn_status: "running" },
    };

    render(<SessionView sessionId={sessionId} />);

    await waitFor(() => {
      expect(paneSpy).toHaveBeenCalled();
    });

    await act(async () => {
      const props = paneSpy.mock.calls.at(-1)?.[0] as { setInput: (value: string) => void };
      props.setInput("hello from bootstrap");
    });

    await act(async () => {
      const props = paneSpy.mock.calls.at(-1)?.[0] as { sendNow: () => Promise<void> };
      await props.sendNow();
    });

    await waitFor(() => {
      const props = paneSpy.mock.calls.at(-1)?.[0] as { sendError?: string | null };
      expect(props.sendError).toBe("A turn is already running. Stop it or wait for it to finish.");
    });

    expect(postMessageMock).not.toHaveBeenCalled();
  });

  it("queues delivery when the running turn is only bootstrap and queued messages are enabled", async () => {
    featureGateMock.mockReturnValue(true);

    const existingEntry = (sessionEntries.map[sessionId] ?? {}) as Record<string, unknown>;
    sessionEntries.map[sessionId] = {
      ...existingEntry,
      freshness: "bootstrap",
      activity: { is_working: true, last_turn_status: "running" },
    };

    render(<SessionView sessionId={sessionId} />);

    await waitFor(() => {
      expect(paneSpy).toHaveBeenCalled();
    });

    await act(async () => {
      const props = paneSpy.mock.calls.at(-1)?.[0] as { setInput: (value: string) => void };
      props.setInput("hello from bootstrap queue gate");
    });

    await act(async () => {
      const props = paneSpy.mock.calls.at(-1)?.[0] as { sendNow: () => Promise<void> };
      await props.sendNow();
    });

    expect(postMessageMock).toHaveBeenCalledTimes(1);
    expect(postMessageMock.mock.calls[0]?.[1]).toBe("hello from bootstrap queue gate");
    expect(postMessageMock.mock.calls[0]?.[2]).toBe("queued");
  });

  it("queues delivery locally when the running turn is authoritative", async () => {
    featureGateMock.mockReturnValue(true);

    const existingEntry = (sessionEntries.map[sessionId] ?? {}) as Record<string, unknown>;
    sessionEntries.map[sessionId] = {
      ...existingEntry,
      freshness: "authoritative",
      activity: { is_working: true, last_turn_status: "running" },
    };

    render(<SessionView sessionId={sessionId} />);

    await waitFor(() => {
      expect(paneSpy).toHaveBeenCalled();
    });

    await act(async () => {
      const props = paneSpy.mock.calls.at(-1)?.[0] as { setInput: (value: string) => void };
      props.setInput("hello from authoritative queue gate");
    });

    await act(async () => {
      const props = paneSpy.mock.calls.at(-1)?.[0] as { sendNow: () => Promise<void> };
      await props.sendNow();
    });

    expect(postMessageMock).toHaveBeenCalledTimes(1);
    expect(postMessageMock.mock.calls[0]?.[1]).toBe("hello from authoritative queue gate");
    expect(postMessageMock.mock.calls[0]?.[2]).toBe("queued");
  });

  it("hides queued panel data when the queued-messages experiment is disabled", async () => {
    featureGateMock.mockReturnValue(false);

    const existingEntry = (sessionEntries.map[sessionId] ?? {}) as Record<string, unknown>;
    sessionEntries.map[sessionId] = {
      ...existingEntry,
      queue: [
        {
          id: "queued-1",
          session_id: sessionId,
          turn_id: "turn-queued-1",
          task_id: "task-1",
          role: "user",
          content: "hidden queued message",
          created_at: "2026-04-10T00:00:00.000Z",
          delivery: "queued",
          attachments: [],
        },
      ],
    };

    render(<SessionView sessionId={sessionId} />);

    await waitFor(() => {
      expect(paneSpy).toHaveBeenCalled();
    });

    const props = paneSpy.mock.calls.at(-1)?.[0] as { queueForPanel: unknown[] };
    expect(props.queueForPanel).toEqual([]);
  });

  it("clears interrupt-pending state if queued-send deletion fails after interrupt succeeds", async () => {
    featureGateMock.mockReturnValue(true);
    deleteMessageMock.mockRejectedValueOnce(new Error("500 delete failed"));

    const existingEntry = (sessionEntries.map[sessionId] ?? {}) as Record<string, unknown>;
    sessionEntries.map[sessionId] = {
      ...existingEntry,
      freshness: "authoritative",
      activity: { is_working: true, last_turn_status: "running" },
      queue: [
        {
          id: "queued-1",
          session_id: sessionId,
          turn_id: "turn-queued-1",
          task_id: "task-1",
          role: "user",
          content: "send queued now",
          created_at: "2026-04-10T00:00:00.000Z",
          delivery: "queued",
          attachments: [],
        },
      ],
    };

    render(<SessionView sessionId={sessionId} />);

    await waitFor(() => {
      expect(paneSpy).toHaveBeenCalled();
    });

    await act(async () => {
      const props = paneSpy.mock.calls.at(-1)?.[0] as {
        onSendQueuedNow: (message: {
          id: string;
          session_id: string;
          turn_id: string;
          task_id: string;
          role: string;
          content: string;
          created_at: string;
          delivery: string;
          attachments: unknown[];
        }) => Promise<void>;
        queueForPanel: Array<{
          id: string;
          session_id: string;
          turn_id: string;
          task_id: string;
          role: string;
          content: string;
          created_at: string;
          delivery: string;
          attachments: unknown[];
        }>;
      };
      await props.onSendQueuedNow(props.queueForPanel[0]!);
    });

    expect(interruptSessionMock).toHaveBeenCalledWith(sessionId);
    expect(deleteMessageMock).toHaveBeenCalledWith(sessionId, "queued-1");
    await waitFor(() => {
      const props = paneSpy.mock.calls.at(-1)?.[0] as {
        interruptPending?: boolean;
        sendError?: string | null;
      };
      expect(props.interruptPending).toBe(false);
      expect(props.sendError).toBe("500 delete failed");
    });
  });

  it("unblocks retry when the latest turn has already failed even if activity is still marked running", async () => {
    const existingEntry = (sessionEntries.map[sessionId] ?? {}) as Record<string, unknown>;
    sessionEntries.map[sessionId] = {
      ...existingEntry,
      freshness: "authoritative",
      activity: { is_working: true, last_turn_status: "running" },
      turns: [
        {
          turn_id: "turn-failed",
          session_id: sessionId,
          user_message_id: "message-failed",
          status: "failed",
          start_seq: 1,
          end_seq: 2,
          started_at: "2026-03-10T00:00:00.000Z",
          updated_at: "2026-03-10T00:00:02.000Z",
          assistant_partial: null,
          thought_partial: null,
          metrics_json: null,
          failure: { message: "OAuth token has expired." },
          tool_total: 0,
          tool_pending: 0,
          tool_running: 0,
          tool_completed: 0,
          tool_failed: 0,
        },
      ],
      events: [
        {
          seq: 2,
          id: "event-turn-failed",
          session_id: sessionId,
          turn_id: "turn-failed",
          event_type: "turn_finished",
          payload_json: { status: "failed", message: "OAuth token has expired." },
          created_at: "2026-03-10T00:00:02.000Z",
        },
      ],
    };

    render(<SessionView sessionId={sessionId} />);

    await waitFor(() => {
      expect(paneSpy).toHaveBeenCalled();
    });

    const settledCall = paneSpy.mock.calls.at(-1)?.[0] as {
      hasActiveTurn?: boolean;
      setInput: (value: string) => void;
    };
    expect(settledCall.hasActiveTurn).toBe(false);

    await act(async () => {
      settledCall.setInput("retry after failure");
    });

    await act(async () => {
      const latestCall = paneSpy.mock.calls.at(-1)?.[0] as {
        sendNow: () => Promise<void>;
      };
      await latestCall.sendNow();
    });

    expect(postMessageMock).toHaveBeenCalledTimes(1);
    expect(postMessageMock.mock.calls[0]?.[1]).toBe("retry after failure");
    expect(postMessageMock.mock.calls[0]?.[2]).toBeUndefined();
  });

  it("optimistically updates the selected model immediately and clears the override after success", async () => {
    const deferred = createDeferred<{
      id: string;
      task_id: string;
      workspace_id: string;
      worktree_id: string;
      provider_id: string;
      model_id: string;
      reasoning_effort: string;
      title: string;
      agent_role: string;
      status: string;
      execution_environment: string;
      created_at: string;
      updated_at: string;
    }>();
    setSessionModelMock.mockReturnValueOnce(deferred.promise);

    render(<SessionView sessionId={sessionId} />);

    await waitFor(() => {
      expect(paneSpy).toHaveBeenCalled();
    });

    const initialCall = paneSpy.mock.calls.at(-1)?.[0] as {
      currentModelId?: string;
      onSetModelId?: (next: string) => Promise<void>;
    };
    expect(initialCall.currentModelId).toBe("gpt-5.4/xhigh");

    await act(async () => {
      void initialCall.onSetModelId?.("gpt-5.4/medium");
    });

    await waitFor(() => {
      const optimisticCall = paneSpy.mock.calls.at(-1)?.[0] as { currentModelId?: string };
      expect(optimisticCall.currentModelId).toBe("gpt-5.4/medium");
    });
    expect(setSessionModelMock).toHaveBeenCalledWith(sessionId, "gpt-5.4/medium");

    deferred.resolve({
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
    });

    await waitFor(() => {
      const settledCall = paneSpy.mock.calls.at(-1)?.[0] as { currentModelId?: string };
      expect(settledCall.currentModelId).toBe("gpt-5.4/medium");
      expect(setSessionSpy).toHaveBeenCalled();
    });
    expect(updateWorkspaceProviderModelPreferenceMock).toHaveBeenCalledWith(
      "ws-1",
      "codex",
      "gpt-5.4/medium",
    );
    expect(refreshProvidersBootstrapMock).toHaveBeenCalledWith("ws-1");
  });

  it("reverts the optimistic model selection and surfaces a model-switch error on failure", async () => {
    const deferred = createDeferred<never>();
    setSessionModelMock.mockReturnValueOnce(deferred.promise);

    render(<SessionView sessionId={sessionId} />);

    await waitFor(() => {
      expect(paneSpy).toHaveBeenCalled();
    });

    const initialCall = paneSpy.mock.calls.at(-1)?.[0] as {
      currentModelId?: string;
      modelSwitchError?: string | null;
      onSetModelId?: (next: string) => Promise<void>;
    };
    expect(initialCall.currentModelId).toBe("gpt-5.4/xhigh");
    expect(initialCall.modelSwitchError).toBeNull();

    await act(async () => {
      void initialCall.onSetModelId?.("gpt-5.4/medium");
    });

    await waitFor(() => {
      const optimisticCall = paneSpy.mock.calls.at(-1)?.[0] as { currentModelId?: string };
      expect(optimisticCall.currentModelId).toBe("gpt-5.4/medium");
    });

    deferred.reject(new Error("timed out waiting for session model update"));

    await waitFor(() => {
      const revertedCall = paneSpy.mock.calls.at(-1)?.[0] as {
        currentModelId?: string;
        modelSwitchError?: string | null;
      };
      expect(revertedCall.currentModelId).toBe("gpt-5.4/xhigh");
      expect(revertedCall.modelSwitchError).toBe("timed out waiting for session model update");
    });
  });
});
