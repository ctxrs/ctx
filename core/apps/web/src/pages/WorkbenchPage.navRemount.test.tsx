import React from "react";
import { render, screen, fireEvent, waitFor, within } from "@testing-library/react";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { VirtuosoMockContext } from "react-virtuoso";
import { describe, it, vi, beforeAll, beforeEach, afterEach } from "vitest";
import type { WorkbenchTab } from "../workbench/types";
import type { SessionSupervisorSnapshot } from "../state/sessionSupervisorCore";
import { resetBrowserResourceUrlCacheForTests } from "../api/browserResourceUrls";
import {
  resetDaemonConnectionStateForTests,
  setDaemonConnection,
} from "../api/daemonConnection";
import WorkbenchPage from "./WorkbenchPage";

const workspaceId = "ws-1";
const taskId = "task-1";
const taskId2 = "task-2";
const sessionId = "session-1";
const sessionId2 = "session-2";
const worktreeId = "worktree-1";
type WorkbenchBootstrapState = "idle" | "loading" | "ready" | "error";

const baseIso = "2024-01-01T00:00:00.000Z";
const buildSessionArtifact = (overrides: Partial<SessionSupervisorSnapshot["sessions"][string]["artifacts"][number]> = {}) => ({
  id: "artifact-1",
  session_id: sessionId,
  task_id: taskId,
  workspace_id: workspaceId,
  worktree_id: worktreeId,
  name: "session-log.bin",
  absolute_path: "/tmp/session-log.bin",
  mime_type: "application/octet-stream",
  bytes: 32,
  created_at: baseIso,
  ...overrides,
});

const buildSessionSnap = (): SessionSupervisorSnapshot => ({
  connection: "connected",
  sessions: {
    [sessionId]: {
      sessionId,
      freshness: "authoritative",
      session: {
        id: sessionId,
        task_id: taskId,
        workspace_id: workspaceId,
        provider_id: "codex",
        worktree_id: worktreeId,
        model_id: "gpt-5",
        title: "Starter session",
        agent_role: "assistant",
        status: "active",
        created_at: baseIso,
      },
      turns: [],
      turnToolsByTurnId: {},
      turnToolsLoading: [],
      toolSummaries: [],
      toolSummariesReady: true,
      hasMoreTurns: false,
      events: [],
      messages: [{
        id: "message-1",
        session_id: sessionId,
        task_id: taskId,
        role: "assistant",
        content: "Hello",
        delivery: "immediate",
        created_at: baseIso,
      }],
      artifacts: [],
      artifactsLoading: false,
      subagentInvocations: [],
      subagentInvocationsLoaded: true,
      subagentInvocationsLoading: false,
      stateLoaded: true,
      stateRev: 1,
      stateLoading: false,
      queue: [],
      loadState: "live",
      loading: false,
      subscribed: true,
      updatedAtMs: 0,
    },
  },
});

const buildWorkspaceSnapshotSnap = () => {
  const tasksById: Record<string, unknown> = {
    [taskId]: {
      id: taskId,
      sortAtMs: Date.parse(baseIso),
      task: {
        id: taskId,
        title: "Starter task",
        created_at: baseIso,
        updated_at: baseIso,
        last_activity_at: baseIso,
        archived_at: null,
        assistant_seen_at: null,
        last_assistant_message_at: null,
      },
      sessions: [
        {
          session: {
            id: sessionId,
            task_id: taskId,
            provider_id: "codex",
            status: "active",
            created_at: baseIso,
          },
          last_message_at: null,
          last_event_seq: null,
          activity: { is_working: false, last_turn_status: null },
          unread: false,
        },
      ],
    },
  };
  return {
  workspaceId,
  initialized: true,
  connection: "connected",
  tasksById,
  activeIds: [taskId],
  archivedIds: [],
  totalActive: 1,
  totalArchived: 0,
  fetchState: { active: "idle", archived: "idle" },
  hasMoreActive: false,
  hasMoreArchived: false,
  archivedLoaded: true,
  };
};

let sessionSnap = buildSessionSnap();
let workspaceSnapshotSnap = buildWorkspaceSnapshotSnap();
let navToken = 0;
let activeTab: WorkbenchTab | null = null;
let activeTaskId = taskId;
let activeSessionId: string | null = sessionId;
let workbenchHydrated = true;
let workbenchProviderBootstrapState: WorkbenchBootstrapState = "ready";
let workbenchProviderBootstrapError: string | null = null;
const focusNewTaskSpy = vi.fn();
const focusTaskSpy = vi.fn(
  (nextTaskId: string, nextSessionId?: string | null, opts?: { source?: string }) => {
    if (opts?.source !== "system") {
      navToken += 1;
    }
    activeTab = {
      id: `tab-${nextTaskId}`,
      kind: "task",
      ref: { taskId: nextTaskId, sessionId: nextSessionId ?? null },
    };
    activeTaskId = nextTaskId;
    activeSessionId = nextSessionId ?? null;
  },
);
const applyTaskUpdateSpy = vi.fn();
const sessionSupervisorMock = {
  bindWorkspaceActiveSnapshotStore: vi.fn(),
  setActiveTaskSessionIds: vi.fn(),
  setWarmSessionIds: vi.fn(),
  setSubscribedSessionIdsSink: vi.fn(),
  setWorkspaceSnapshotState: vi.fn(),
  setWorkspaceSessionHeads: vi.fn(),
  handleWorkspaceEvent: vi.fn(),
  setDiff: vi.fn(),
  loadSessionState: vi.fn(),
  loadSubagentInvocations: vi.fn(),
};
const workspaceSnapshotStoreMock = {
  applyTaskUpdate: applyTaskUpdateSpy,
  ensureArchivedLoaded: vi.fn(),
  getWorktreeRoot: vi.fn(() => null),
  loadMoreActive: vi.fn(),
  loadMoreArchived: vi.fn(),
  subscribe: vi.fn(() => () => {}),
  subscribeEvents: vi.fn(() => () => {}),
  getSnapshot: vi.fn(() => workspaceSnapshotSnap),
  getSessionHeadSnapshot: vi.fn(() => null),
  getSessionHeadsSnapshot: vi.fn(() => ({})),
  setForegroundSessionId: vi.fn(),
  setSubscribedSessions: vi.fn(),
};
const { trackWorkbenchPanelToggledMock } = vi.hoisted(() => ({
  trackWorkbenchPanelToggledMock: vi.fn(),
}));
const { useOpenSessionMock } = vi.hoisted(() => ({
  useOpenSessionMock: vi.fn(),
}));
const { sessionViewMountSpy, sessionViewUnmountSpy } = vi.hoisted(() => ({
  sessionViewMountSpy: vi.fn(),
  sessionViewUnmountSpy: vi.fn(),
}));
const { refreshWorkbenchBootstrapSpy } = vi.hoisted(() => ({
  refreshWorkbenchBootstrapSpy: vi.fn(async () => undefined),
}));
const getInstallMock = vi.hoisted(() =>
  vi.fn(async (_installId?: string): Promise<{ install_id?: string; last_event?: unknown }> => ({})),
);

const createDeferred = <T,>() => {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
};

const emptyProviderAccounts = {
  active_account_id: null,
  accounts: [],
  logins: [],
};

vi.mock("../api/client", () => ({
  archiveTask: vi.fn(async () => ({})),
  artifactUrl: (sessionId: string, artifactId: string) => `/api/sessions/${sessionId}/artifacts/${artifactId}`,
  createSession: vi.fn(async () => ({})),
  createTask: vi.fn(async () => ({})),
  deleteAmpAccount: vi.fn(async () => emptyProviderAccounts),
  deleteClaudeAccount: vi.fn(async () => emptyProviderAccounts),
  deleteCodexAccount: vi.fn(async () => emptyProviderAccounts),
  deleteCopilotAccount: vi.fn(async () => emptyProviderAccounts),
  deleteCursorAccount: vi.fn(async () => emptyProviderAccounts),
  deleteGeminiAccount: vi.fn(async () => emptyProviderAccounts),
  deleteKimiAccount: vi.fn(async () => emptyProviderAccounts),
  deleteMistralAccount: vi.fn(async () => emptyProviderAccounts),
  deleteProviderHarnessEndpoint: vi.fn(async () => ({})),
  deleteQwenAccount: vi.fn(async () => emptyProviderAccounts),
  deleteTask: vi.fn(async () => ({})),
  getDaemonClientConfig: vi.fn(() => ({
    baseUrl: null,
    wsBaseUrl: null,
    authToken: null,
    runId: null,
  })),
  getHealth: vi.fn(async () => ({
    version: "0.0.0",
    daemon_version: "0.0.0",
    pid: 1,
    data_root: "/tmp/ctx",
    daemon_url: "",
    auth_required: false,
    compatibility: {
      desktop_exact_version: "0.0.0",
      mobile_api_min: 1,
      mobile_api_max: 1,
    },
  })),
  recordClientCounterMetric: vi.fn(),
  recordClientGaugeMetric: vi.fn(),
  recordClientHistogramMetric: vi.fn(),
  subscribeDaemonConfig: vi.fn(() => () => {}),
  getTitleGenerationLocalStatus: vi.fn(async () => ({
    ready: true,
    runtime: { version: "1.0.0", installed: true, path: "/tmp/runtime" },
    model: { model_id: "model", file_name: "model.gguf", installed: true },
    install_id: null,
    install_running: false,
  })),
  checkUpdates: vi.fn(async () => ({
    channel: "stable",
    base_url: "https://example.test",
    current_version: "0.0.0",
    update_available: false,
  })),
  getInstall: getInstallMock,
  getInstallStatuses: vi.fn(async (installIds: string[]) => ({
    installs: await Promise.all(
      installIds.map(async (installId) => {
        const info = await getInstallMock(installId);
        return {
          install_id: installId,
          info: info && typeof info.install_id === "string" ? info : null,
        };
      }),
    ),
  })),
  getProviderOptions: vi.fn(async () => ({})),
  getSessionGitStatusSummary: vi.fn(async () => null),
  getSettings: vi.fn(async () => ({ dictation: { enabled: false } })),
  getWorktree: vi.fn(async () => ({})),
  getWorkspace: vi.fn(async () => ({ id: workspaceId, name: "Mock Workspace", root_path: "/tmp/mock" })),
  idToString: (id: string | null | undefined) => {
    if (id === null || id === undefined) return "";
    if (typeof id !== "string") {
      throw new Error("Expected id to be a string");
    }
    return id;
  },
  installAllProviders: vi.fn(async () => ({})),
  installProvider: vi.fn(async () => ({ install_id: "install-1" })),
  listInstallEvents: vi.fn(async (installId: string) => {
    const info = await getInstallMock(installId);
    return info?.last_event ? [info.last_event] : [];
  }),
  listProviders: vi.fn(async () => []),
  listWorkspaces: vi.fn(async () => []),
  markTaskRead: vi.fn(async () => ({})),
  markTaskUnread: vi.fn(async () => ({})),
  postMessage: vi.fn(async () => ({})),
  refreshProviderHarnessEndpointModels: vi.fn(async () => []),
  selectProviderHarnessSource: vi.fn(async () => ({})),
  setAmpActiveAccount: vi.fn(async () => emptyProviderAccounts),
  setClaudeActiveAccount: vi.fn(async () => emptyProviderAccounts),
  setCodexActiveAccount: vi.fn(async () => emptyProviderAccounts),
  setCopilotActiveAccount: vi.fn(async () => emptyProviderAccounts),
  setCursorActiveAccount: vi.fn(async () => emptyProviderAccounts),
  setGeminiActiveAccount: vi.fn(async () => emptyProviderAccounts),
  setKimiActiveAccount: vi.fn(async () => emptyProviderAccounts),
  setMistralActiveAccount: vi.fn(async () => emptyProviderAccounts),
  setQwenActiveAccount: vi.fn(async () => emptyProviderAccounts),
  unarchiveTask: vi.fn(async () => ({})),
  upsertProviderHarnessEndpoint: vi.fn(async () => ({})),
  updateTaskTitle: vi.fn(async () => ({})),
  verifyProviderForWorkspace: vi.fn(async () => ({})),
}));

vi.mock("../utils/analytics", async () => {
  const actual = await vi.importActual<typeof import("../utils/analytics")>("../utils/analytics");
  return {
    ...actual,
    trackWorkbenchPanelToggled: trackWorkbenchPanelToggledMock,
  };
});

vi.mock("../state/sessionSupervisor", () => ({
  useSessionSupervisor: () => sessionSupervisorMock,
  useSessionLifecycleCoordinator: () => ({
    setWorkspaceSnapshotState: vi.fn(),
  }),
  useSessionCacheSnapshot: () => sessionSnap,
  useSessionEntry: (id: string) => sessionSnap.sessions[id] ?? null,
  useOpenSession: useOpenSessionMock,
}));

vi.mock("../state/workspaceActiveSnapshotStore", () => ({
  WorkspaceActiveSnapshotProvider: ({ children }: { children: React.ReactNode }) => <>{children}</>,
  useWorkspaceActiveSnapshotEvents: () => {},
  useWorkspaceActiveSnapshotSnapshot: () => workspaceSnapshotSnap,
  useWorkspaceActiveSnapshotStore: () => workspaceSnapshotStoreMock,
}));

vi.mock("../workbench/store", () => ({
  WorkbenchStoreProvider: ({ children }: { children: React.ReactNode }) => <>{children}</>,
  NEW_TASK_DRAFT_KEY: "new_task",
  sessionDraftKey: () => "draft-key",
  useWorkbenchStore: () => ({
    focusNewTask: focusNewTaskSpy,
    focusTask: focusTaskSpy,
    setActiveSessionForActiveTask: vi.fn(),
    flushDraft: vi.fn(),
    getActiveTab: () => activeTab,
    getNavToken: () => navToken,
  }),
  useWorkbenchShellSnapshot: () => ({
    workspaceId,
    windowId: "window-1",
    hydrated: workbenchHydrated,
    warnings: [],
    window: {
      v: 1,
      focusedLeafId: "leaf-1",
      layout: { kind: "leaf", id: "leaf-1", tabs: [], activeTabId: "" },
    },
  }),
  useActiveWorkbenchTab: () => null,
  useActiveWorkbenchIds: () => ({ taskId: activeTaskId, sessionId: activeSessionId }),
  useNewTaskDraft: () => ({ value: { text: "", modeId: "default" }, setValue: vi.fn() }),
  useWorkbenchDraft: () => ({ value: { text: "", modeId: "default" }, updatedAtMs: 0, setValue: vi.fn() }),
}));

vi.mock("./workbenchShell/useWorkbenchProviders", () => ({
  useWorkbenchProviders: () => ({
    providersById: {},
    defaultProviderId: "codex",
    providerInstallsById: {},
    providerOptions: {},
    bootstrapState: workbenchProviderBootstrapState,
    bootstrapError: workbenchProviderBootstrapError,
    installAllBusy: false,
    installProviderFromMenu: vi.fn(),
    cancelProviderInstallFromMenu: vi.fn(),
    installAllProvidersFromMenu: vi.fn(),
    ensureProviderAuthSummary: vi.fn(async () => undefined),
    refreshBootstrap: refreshWorkbenchBootstrapSpy,
  }),
}));

vi.mock("./settings/sections/HarnessAuthenticationSection", () => ({
  HarnessAuthenticationSectionView: ({ modalOnly }: { modalOnly?: boolean }) =>
    modalOnly ? <div data-testid="composer-harness-auth-modal">Composer auth recovery UI</div> : null,
}));

vi.mock("../components/WorkbenchComposer", () => ({
  WorkbenchComposer: () => null,
}));

vi.mock("../components/DiffReviewPane", () => ({
  DiffReviewPane: () => null,
}));

vi.mock("./sessionView", () => ({
  SessionView: ({
    sessionId: mockedSessionId,
    hideSessionLoadIssuesBanner,
  }: {
    sessionId: string;
    hideSessionLoadIssuesBanner?: boolean;
  }) => {
    const loadErrors = sessionSnap.sessions[mockedSessionId]?.loadErrors ?? {};
    const issues = [
      loadErrors.state,
      loadErrors.subagentInvocations,
    ].filter((value): value is string => typeof value === "string" && value.length > 0);
    React.useEffect(() => {
      sessionViewMountSpy(mockedSessionId);
      return () => {
        sessionViewUnmountSpy(mockedSessionId);
      };
    }, [mockedSessionId]);
    return (
      <div data-testid="session-view-mock" data-session-id={mockedSessionId}>
        {!hideSessionLoadIssuesBanner && issues.length > 0 ? (
          <div className="banner" data-testid="workbench-session-load-issues">
            <div>Some session details failed to load.</div>
            {issues.map((issue) => (
              <div key={issue}>{issue}</div>
            ))}
            <button
              type="button"
              onClick={() => {
                sessionSupervisorMock.loadSessionState(mockedSessionId, { force: true });
                sessionSupervisorMock.loadSubagentInvocations(mockedSessionId, { force: true });
              }}
            >
              Retry
            </button>
          </div>
        ) : null}
      </div>
    );
  },
  buildWorkbenchThreadViewModel: () => ({ groups: [] }),
}));

beforeAll(() => {
  const globalWithMocks = globalThis as typeof globalThis & {
    localStorage?: Storage;
    ResizeObserver?: typeof ResizeObserver;
  };
  if (typeof globalWithMocks.localStorage?.getItem !== "function") {
    const store = new Map<string, string>();
    globalWithMocks.localStorage = {
      getItem: (key: string) => (store.has(key) ? store.get(key) ?? null : null),
      setItem: (key: string, value: string) => {
        store.set(key, String(value));
      },
      removeItem: (key: string) => {
        store.delete(key);
      },
      clear: () => {
        store.clear();
      },
      key: (index: number) => Array.from(store.keys())[index] ?? null,
      get length() {
        return store.size;
      },
    };
  }
  if (!("ResizeObserver" in globalThis)) {
    class ResizeObserver {
      observe() {}
      unobserve() {}
      disconnect() {}
    }
    globalWithMocks.ResizeObserver = ResizeObserver;
  }
});

beforeEach(() => {
  resetBrowserResourceUrlCacheForTests();
  setDaemonConnection({
    baseUrl: "http://daemon.test",
    authToken: "daemon-secret",
    source: "test",
    mobileSecure: null,
  });
  navToken = 0;
  activeTab = { id: `tab-${taskId}`, kind: "task", ref: { taskId, sessionId } };
  activeTaskId = taskId;
  activeSessionId = sessionId;
  workbenchHydrated = true;
  workbenchProviderBootstrapState = "ready";
  workbenchProviderBootstrapError = null;
  sessionSnap = buildSessionSnap();
  workspaceSnapshotSnap = buildWorkspaceSnapshotSnap();
  trackWorkbenchPanelToggledMock.mockReset();
  sessionSupervisorMock.bindWorkspaceActiveSnapshotStore.mockReset();
  sessionSupervisorMock.setActiveTaskSessionIds.mockReset();
  sessionSupervisorMock.setWarmSessionIds.mockReset();
  sessionSupervisorMock.setSubscribedSessionIdsSink.mockReset();
  sessionSupervisorMock.setWorkspaceSnapshotState.mockReset();
  sessionSupervisorMock.setWorkspaceSessionHeads.mockReset();
  sessionSupervisorMock.handleWorkspaceEvent.mockReset();
  sessionSupervisorMock.setDiff.mockReset();
  sessionSupervisorMock.loadSessionState.mockReset();
  sessionSupervisorMock.loadSubagentInvocations.mockReset();
  useOpenSessionMock.mockReset();
  workspaceSnapshotStoreMock.ensureArchivedLoaded.mockReset();
  workspaceSnapshotStoreMock.getWorktreeRoot.mockReset();
  workspaceSnapshotStoreMock.getWorktreeRoot.mockReturnValue(null);
  workspaceSnapshotStoreMock.loadMoreActive.mockReset();
  workspaceSnapshotStoreMock.loadMoreArchived.mockReset();
  workspaceSnapshotStoreMock.subscribe.mockReset();
  workspaceSnapshotStoreMock.subscribe.mockReturnValue(() => {});
  workspaceSnapshotStoreMock.subscribeEvents.mockReset();
  workspaceSnapshotStoreMock.subscribeEvents.mockReturnValue(() => {});
  workspaceSnapshotStoreMock.getSnapshot.mockReset();
  workspaceSnapshotStoreMock.getSnapshot.mockImplementation(() => workspaceSnapshotSnap);
  workspaceSnapshotStoreMock.getSessionHeadSnapshot.mockReset();
  workspaceSnapshotStoreMock.getSessionHeadSnapshot.mockReturnValue(null);
  workspaceSnapshotStoreMock.getSessionHeadsSnapshot.mockReset();
  workspaceSnapshotStoreMock.getSessionHeadsSnapshot.mockReturnValue({});
  workspaceSnapshotStoreMock.setForegroundSessionId.mockReset();
  workspaceSnapshotStoreMock.setSubscribedSessions.mockReset();
  refreshWorkbenchBootstrapSpy.mockReset();
  sessionViewMountSpy.mockReset();
  sessionViewUnmountSpy.mockReset();
});

afterEach(() => {
  resetBrowserResourceUrlCacheForTests();
  resetDaemonConnectionStateForTests();
  vi.clearAllMocks();
});

function renderWorkbenchPage() {
  return render(
    <VirtuosoMockContext.Provider value={{ itemHeight: 40, viewportHeight: 400 }}>
      <MemoryRouter initialEntries={[`/workspaces/${workspaceId}`]}>
        <Routes>
          <Route path="/workspaces/:id" element={<WorkbenchPage />} />
        </Routes>
      </MemoryRouter>
    </VirtuosoMockContext.Provider>,
  );
}

function getTaskRow(title: string) {
  const row = screen
    .getAllByText(title)
    .map((node) => node.closest(".wb-task-row"))
    .find((node): node is HTMLElement => Boolean(node));
  if (!row) throw new Error(`Missing task row for ${title}`);
  return row;
}

describe("WorkbenchPage bootstrap gate", () => {
  it("shows Loading workspace... while provider bootstrap is still loading", async () => {
    workbenchProviderBootstrapState = "loading";

    renderWorkbenchPage();

    await waitFor(() => {
      expect(screen.getByText("Loading workspace...")).toBeInTheDocument();
      expect(screen.getByTestId("composer-harness-auth-modal")).toBeInTheDocument();
      expect(screen.queryByTestId("session-view-mock")).not.toBeInTheDocument();
    });
  });

  it("shows an explicit bootstrap error and retries on request", async () => {
    workbenchProviderBootstrapState = "error";
    workbenchProviderBootstrapError = "bootstrap failed";

    renderWorkbenchPage();

    await waitFor(() => {
      expect(screen.getByText("Failed to load workspace.")).toBeInTheDocument();
      expect(screen.getByText("bootstrap failed")).toBeInTheDocument();
      expect(screen.getByTestId("composer-harness-auth-modal")).toBeInTheDocument();
    });

    fireEvent.click(screen.getByRole("button", { name: "Retry workspace load" }));

    await waitFor(() => {
      expect(refreshWorkbenchBootstrapSpy).toHaveBeenCalledTimes(1);
    });
  });
});

describe("WorkbenchPage task rename selection", () => {
  it("renders only the active session slot for the selected task", async () => {
    const rendered = renderWorkbenchPage();

    await waitFor(() => {
      expect(screen.getAllByTestId("session-view-mock")).toHaveLength(1);
    });
    expect(screen.getByTestId("session-view-mock")).toHaveAttribute("data-session-id", sessionId);
    expect(document.querySelectorAll(".wb-session-slot")).toHaveLength(1);
    expect(document.querySelector(".wb-session-slot")).toHaveAttribute("aria-hidden", "false");
  });

  it("does not force session mode through WorkbenchPage route-open policy", async () => {
    const rendered = renderWorkbenchPage();

    await waitFor(() => {
      expect(useOpenSessionMock).toHaveBeenCalledWith(sessionId, expect.objectContaining({ watchDiff: false }));
    });
    const latestOptions = useOpenSessionMock.mock.calls.at(-1)?.[1];
    expect(latestOptions).toBeDefined();
    expect(latestOptions).not.toHaveProperty("mode");
  });

  it("keeps rename selection stable on session updates", async () => {
    const selectSpy = vi.spyOn(HTMLInputElement.prototype, "select");
    const ui = (
      <VirtuosoMockContext.Provider value={{ itemHeight: 40, viewportHeight: 400 }}>
        <MemoryRouter initialEntries={[`/workspaces/${workspaceId}`]}>
          <Routes>
            <Route path="/workspaces/:id" element={<WorkbenchPage />} />
          </Routes>
        </MemoryRouter>
      </VirtuosoMockContext.Provider>
    );

    const { rerender } = render(ui);

    const menuButton = await screen.findByRole("button", { name: "More actions" });
    fireEvent.click(menuButton);
    const renameItem = await screen.findByRole("menuitem", { name: "Rename Task" });
    fireEvent.click(renameItem);

    await waitFor(() => expect(selectSpy).toHaveBeenCalledTimes(1));
    expect(await screen.findByLabelText("Rename task")).toBeTruthy();

    sessionSnap = {
      ...sessionSnap,
      sessions: {
        ...sessionSnap.sessions,
        [sessionId]: {
          ...sessionSnap.sessions[sessionId],
          messages: [
            ...sessionSnap.sessions[sessionId].messages,
            {
              id: "message-2",
              session_id: sessionId,
              task_id: taskId,
              role: "assistant",
              content: "Follow-up",
              delivery: "immediate",
              created_at: "2024-01-01T00:00:01.000Z",
            },
          ],
          updatedAtMs: sessionSnap.sessions[sessionId].updatedAtMs + 1,
        },
      },
    };

    rerender(ui);
    await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
    expect(selectSpy).toHaveBeenCalledTimes(1);
  });
});

describe("WorkbenchPage keyboard shortcuts", () => {
  it("opens new task on Cmd/Ctrl+N even when the event was already prevented", async () => {
    renderWorkbenchPage();

    const shortcutEvent = new KeyboardEvent("keydown", {
      key: "n",
      metaKey: true,
      bubbles: true,
      cancelable: true,
    });
    shortcutEvent.preventDefault();
    window.dispatchEvent(shortcutEvent);

    await waitFor(() => {
      expect(focusNewTaskSpy).toHaveBeenCalledTimes(1);
    });
  });
});

describe("WorkbenchPage archive navigation", () => {
  it("does not refocus new task after navigation during archive", async () => {
    workspaceSnapshotSnap = {
      ...workspaceSnapshotSnap,
      tasksById: {
        ...workspaceSnapshotSnap.tasksById,
        [taskId2]: {
          id: taskId2,
          sortAtMs: Date.parse("2024-01-01T00:00:02.000Z"),
          task: {
            id: taskId2,
            title: "Second task",
            created_at: baseIso,
            updated_at: baseIso,
            last_activity_at: baseIso,
            archived_at: null,
            assistant_seen_at: null,
            last_assistant_message_at: null,
          },
          sessions: [
            {
              session: {
                id: sessionId2,
                task_id: taskId2,
                provider_id: "codex",
                status: "active",
                created_at: baseIso,
              },
              last_message_at: null,
              last_event_seq: null,
              activity: { is_working: false, last_turn_status: null },
              unread: false,
            },
          ],
        },
      },
      activeIds: [taskId, taskId2],
      totalActive: 2,
    };

    const { archiveTask } = await import("../api/client");
    const archiveDeferred = createDeferred<Awaited<ReturnType<typeof archiveTask>>>();
    vi.mocked(archiveTask).mockReturnValueOnce(archiveDeferred.promise);

    const ui = (
      <VirtuosoMockContext.Provider value={{ itemHeight: 40, viewportHeight: 400 }}>
        <MemoryRouter initialEntries={[`/workspaces/${workspaceId}`]}>
          <Routes>
            <Route path="/workspaces/:id" element={<WorkbenchPage />} />
          </Routes>
        </MemoryRouter>
      </VirtuosoMockContext.Provider>
    );

    render(ui);

    const starterRow = getTaskRow("Starter task");
    const archiveButton = within(starterRow).getByRole("button", { name: "Archive" });
    fireEvent.click(archiveButton);

    const dialog = await screen.findByRole("dialog", { name: "Archive confirmation" });
    fireEvent.click(within(dialog).getByRole("button", { name: "Archive" }));

    await waitFor(() => expect(archiveTask).toHaveBeenCalledTimes(1));

    fireEvent.click(within(getTaskRow("Second task")).getByText("Second task"));
    fireEvent.click(within(getTaskRow("Starter task")).getByText("Starter task"));

    archiveDeferred.resolve({
      id: taskId,
      workspace_id: workspaceId,
      title: "Starter task",
      status: "completed",
      created_at: baseIso,
      updated_at: baseIso,
      archived_at: baseIso,
    });

    await waitFor(() => expect(applyTaskUpdateSpy).toHaveBeenCalledTimes(1));
    expect(focusNewTaskSpy).not.toHaveBeenCalled();
  });
});

describe("WorkbenchPage session boundary remount", () => {
  it("remounts the session view when switching to a different task session", async () => {
    sessionSnap = {
      ...sessionSnap,
      sessions: {
        ...sessionSnap.sessions,
        [sessionId2]: {
          sessionId: sessionId2,
          freshness: "authoritative",
          session: {
            id: sessionId2,
            task_id: taskId2,
            workspace_id: workspaceId,
            provider_id: "codex",
            worktree_id: worktreeId,
            model_id: "gpt-5",
            title: "Second session",
            agent_role: "assistant",
            status: "active",
            created_at: baseIso,
          },
          turns: [],
          turnToolsByTurnId: {},
          turnToolsLoading: [],
          toolSummaries: [],
          toolSummariesReady: true,
          hasMoreTurns: false,
          events: [],
          messages: [],
          artifacts: [],
          artifactsLoading: false,
          subagentInvocations: [],
          subagentInvocationsLoaded: true,
          subagentInvocationsLoading: false,
          stateLoaded: true,
          stateRev: 1,
          stateLoading: false,
          queue: [],
          loadState: "live",
          loading: false,
          subscribed: true,
          updatedAtMs: 0,
        },
      },
    };

    workspaceSnapshotSnap = {
      ...workspaceSnapshotSnap,
      tasksById: {
        ...workspaceSnapshotSnap.tasksById,
        [taskId2]: {
          id: taskId2,
          sortAtMs: Date.parse("2024-01-01T00:00:02.000Z"),
          task: {
            id: taskId2,
            title: "Second task",
            created_at: baseIso,
            updated_at: baseIso,
            last_activity_at: baseIso,
            archived_at: null,
            assistant_seen_at: null,
            last_assistant_message_at: null,
          },
          sessions: [
            {
              session: {
                id: sessionId2,
                task_id: taskId2,
                provider_id: "codex",
                status: "active",
                created_at: baseIso,
              },
              last_message_at: null,
              last_event_seq: null,
              activity: { is_working: false, last_turn_status: null },
              unread: false,
            },
          ],
        },
      },
      activeIds: [taskId, taskId2],
      totalActive: 2,
    };

    const rendered = renderWorkbenchPage();

    await waitFor(() => {
      expect(screen.getByTestId("session-view-mock")).toHaveAttribute("data-session-id", sessionId);
    });
    expect(sessionViewMountSpy).toHaveBeenCalledWith(sessionId);

    fireEvent.click(within(getTaskRow("Second task")).getByText("Second task"));
    rendered.rerender(
      <VirtuosoMockContext.Provider value={{ itemHeight: 40, viewportHeight: 400 }}>
        <MemoryRouter initialEntries={[`/workspaces/${workspaceId}`]}>
          <Routes>
            <Route path="/workspaces/:id" element={<WorkbenchPage />} />
          </Routes>
        </MemoryRouter>
      </VirtuosoMockContext.Provider>,
    );

    await waitFor(() => {
      expect(screen.getByTestId("session-view-mock")).toHaveAttribute("data-session-id", sessionId2);
    });

    expect(sessionViewUnmountSpy).toHaveBeenCalledWith(sessionId);
    expect(sessionViewMountSpy).toHaveBeenCalledWith(sessionId2);
  });
});

describe("WorkbenchPage nav status indicator", () => {
  it("shows spinner for canonical primary running activity, but not queued follow-ups or subagent activity", async () => {
    const starterSummary = workspaceSnapshotSnap.tasksById[taskId] as {
      task: Record<string, unknown>;
      sessions: Array<Record<string, unknown>>;
    };
    const starterSession = sessionSnap.sessions[sessionId];
    if (!starterSession?.session) throw new Error("Missing starter session");
    const starterSessionMeta = starterSession.session;

    workspaceSnapshotSnap = {
      ...workspaceSnapshotSnap,
      tasksById: {
        ...workspaceSnapshotSnap.tasksById,
        [taskId]: {
          ...starterSummary,
          task: {
            ...starterSummary.task,
            primary_session_id: sessionId,
            assistant_seen_at: baseIso,
            last_assistant_message_at: "2024-01-01T00:00:02.000Z",
          },
          primarySessionId: sessionId,
          sessions: [
            {
              ...starterSummary.sessions[0],
              activity: { is_working: true, last_turn_status: "running" },
              last_message_at: "2024-01-01T00:00:02.000Z",
            },
          ],
        },
      },
    };
    sessionSnap = {
      ...sessionSnap,
      sessions: {
        ...sessionSnap.sessions,
        [sessionId]: {
          ...starterSession,
          turns: [
            {
              turn_id: "turn-running",
              session_id: sessionId,
              run_id: null,
              user_message_id: "message-1",
              status: "running",
              start_seq: 1,
              end_seq: null,
              started_at: baseIso,
              updated_at: baseIso,
              assistant_partial: null,
              thought_partial: null,
              metrics_json: null,
              tool_total: 0,
              tool_pending: 0,
              tool_running: 0,
              tool_completed: 0,
              tool_failed: 0,
            },
          ],
        },
      },
    };

    const ui = renderWorkbenchPage();

    await screen.findAllByText("Starter task");
    const row = getTaskRow("Starter task");
    expect(row.querySelector(".wb-task-spinner")).not.toBeNull();
    expect(row.querySelector(".wb-task-status-dot-unread")).toBeNull();

    sessionSnap = {
      ...sessionSnap,
      sessions: {
        ...sessionSnap.sessions,
        [sessionId]: {
          ...sessionSnap.sessions[sessionId],
          turns: [
            {
              turn_id: "turn-queued",
              session_id: sessionId,
              run_id: null,
              user_message_id: "message-1",
              status: "queued",
              start_seq: 1,
              end_seq: null,
              started_at: baseIso,
              updated_at: baseIso,
              assistant_partial: null,
              thought_partial: null,
              metrics_json: null,
              tool_total: 0,
              tool_pending: 0,
              tool_running: 0,
              tool_completed: 0,
              tool_failed: 0,
            },
          ],
          messages: [
            {
              ...sessionSnap.sessions[sessionId].messages[0],
              created_at: "2024-01-01T00:00:03.000Z",
            },
          ],
          updatedAtMs: 3,
        },
        "session-subagent": {
          ...sessionSnap.sessions[sessionId],
          sessionId: "session-subagent",
          session: {
            ...starterSessionMeta,
            id: "session-subagent",
            parent_session_id: sessionId,
            relationship: "sub_agent",
            updated_at: "2024-01-01T00:00:04.000Z",
          },
          turns: [
            {
              turn_id: "subagent-running",
              session_id: "session-subagent",
              run_id: null,
              user_message_id: "message-subagent",
              status: "running",
              start_seq: 2,
              end_seq: null,
              started_at: "2024-01-01T00:00:04.000Z",
              updated_at: "2024-01-01T00:00:04.000Z",
              assistant_partial: null,
              thought_partial: null,
              metrics_json: null,
              tool_total: 0,
              tool_pending: 0,
              tool_running: 0,
              tool_completed: 0,
              tool_failed: 0,
            },
          ],
          updatedAtMs: 4,
        },
      },
    };
    workspaceSnapshotSnap = {
      ...workspaceSnapshotSnap,
      tasksById: {
        ...workspaceSnapshotSnap.tasksById,
        [taskId]: {
          ...(workspaceSnapshotSnap.tasksById[taskId] as Record<string, unknown>),
          task: {
            ...((workspaceSnapshotSnap.tasksById[taskId] as { task: Record<string, unknown> }).task ?? {}),
            assistant_seen_at: "2024-01-01T00:00:03.000Z",
            last_assistant_message_at: "2024-01-01T00:00:03.000Z",
          },
          sessions: [
            {
              ...((workspaceSnapshotSnap.tasksById[taskId] as { sessions: Array<Record<string, unknown>> }).sessions[0] ??
                {}),
              activity: { is_working: true, last_turn_status: "queued" },
              last_message_at: "2024-01-01T00:00:03.000Z",
            },
          ],
        },
      },
    };

    ui.rerender(
      <VirtuosoMockContext.Provider value={{ itemHeight: 40, viewportHeight: 400 }}>
        <MemoryRouter initialEntries={[`/workspaces/${workspaceId}`]}>
          <Routes>
            <Route path="/workspaces/:id" element={<WorkbenchPage />} />
          </Routes>
        </MemoryRouter>
      </VirtuosoMockContext.Provider>,
    );

    await waitFor(() => {
      const rerenderedRow = getTaskRow("Starter task");
      expect(rerenderedRow.querySelector(".wb-task-spinner")).toBeNull();
      expect(rerenderedRow.querySelector(".wb-task-status-dot-unread")).toBeNull();
    });
  });

  it("shows the archive spinner while archive is pending", async () => {
    const starterSummary = workspaceSnapshotSnap.tasksById[taskId] as {
      task: Record<string, unknown>;
    };
    workspaceSnapshotSnap = {
      ...workspaceSnapshotSnap,
      tasksById: {
        ...workspaceSnapshotSnap.tasksById,
        [taskId]: {
          ...(workspaceSnapshotSnap.tasksById[taskId] as Record<string, unknown>),
          task: {
            ...starterSummary.task,
            primary_session_id: sessionId,
          },
        },
      },
    };

    const { archiveTask } = await import("../api/client");
    const archiveDeferred = createDeferred<Awaited<ReturnType<typeof archiveTask>>>();
    vi.mocked(archiveTask).mockReturnValueOnce(archiveDeferred.promise);

    renderWorkbenchPage();

    const row = getTaskRow("Starter task");
    fireEvent.click(within(row).getByRole("button", { name: "Archive" }));

    const dialog = await screen.findByRole("dialog", { name: "Archive confirmation" });
    fireEvent.click(within(dialog).getByRole("button", { name: "Archive" }));

    await waitFor(() => expect(archiveTask).toHaveBeenCalledTimes(1));
    const spinner = getTaskRow("Starter task").querySelector(".wb-task-spinner");
    expect(spinner).not.toBeNull();
    expect(spinner?.classList.contains("wb-task-spinner-archive")).toBe(true);

    archiveDeferred.resolve({
      id: taskId,
      workspace_id: workspaceId,
      title: "Starter task",
      status: "completed",
      created_at: baseIso,
      updated_at: baseIso,
      archived_at: baseIso,
    });
    await waitFor(() => expect(applyTaskUpdateSpy).toHaveBeenCalled());
  });
});

describe("WorkbenchPage title generation install banner", () => {
  it("shows progress while local title model install is running", async () => {
    const { getSettings, getTitleGenerationLocalStatus, getInstall } = await import("../api/client");
    vi.mocked(getSettings).mockResolvedValue({
      title_generation: {
        mode: "local",
        remote: {
          base_url: "https://openrouter.ai/api/v1",
          api_key: "",
          model: "google/gemini-3-flash-preview",
          use_json: true,
        },
        local: {
          model_id: "ggml-org/Qwen3-1.7B-GGUF",
          use_json: true,
        },
      },
    } as never);
    vi.mocked(getTitleGenerationLocalStatus).mockResolvedValue({
      ready: false,
      runtime: { version: "0.0.0-test", installed: true, path: "/tmp/runtime" },
      model: {
        model_id: "ggml-org/Qwen3-1.7B-GGUF",
        file_name: "Qwen3-1.7B-GGUF.gguf",
        installed: false,
      },
      install_id: "install-1",
      install_running: true,
    } as never);
    vi.mocked(getInstall).mockResolvedValue({
      install_id: "install-1",
      provider_id: "title_generation_local",
      state: "running",
      started_at: "2026-02-20T00:00:00Z",
      last_event: {
        install_id: "install-1",
        provider_id: "title_generation_local",
        at: "2026-02-20T00:00:01Z",
        stage: "download_model",
        message: "Downloading model file",
        level: "info",
        bytes: 5,
        total_bytes: 10,
      },
    } as never);

    const ui = (
      <VirtuosoMockContext.Provider value={{ itemHeight: 40, viewportHeight: 400 }}>
        <MemoryRouter initialEntries={[`/workspaces/${workspaceId}`]}>
          <Routes>
            <Route path="/workspaces/:id" element={<WorkbenchPage />} />
          </Routes>
        </MemoryRouter>
      </VirtuosoMockContext.Provider>
    );

    render(ui);

    expect(await screen.findByText("Session titling model download in progress.")).toBeInTheDocument();
    expect(await screen.findByText("Downloading… 38%")).toBeInTheDocument();
  });
});

describe("WorkbenchPage session support load issues", () => {
  it("loads active session support data and retries from the banner", async () => {
    sessionSnap = {
      ...sessionSnap,
      sessions: {
        ...sessionSnap.sessions,
        [sessionId]: {
          ...sessionSnap.sessions[sessionId],
          loadErrors: {
            state: "Failed to load session state: daemon offline",
            subagentInvocations: "Failed to load subagent invocations: query failed",
          },
        },
      },
    };

    const ui = (
      <VirtuosoMockContext.Provider value={{ itemHeight: 40, viewportHeight: 400 }}>
        <MemoryRouter initialEntries={[`/workspaces/${workspaceId}`]}>
          <Routes>
            <Route path="/workspaces/:id" element={<WorkbenchPage />} />
          </Routes>
        </MemoryRouter>
      </VirtuosoMockContext.Provider>
    );

    render(ui);

    expect(await screen.findByText("Some session details failed to load.")).toBeInTheDocument();
    expect(screen.getByTestId("workbench-session-load-issues")).toHaveClass("banner");
    expect(screen.getByText("Failed to load session state: daemon offline")).toBeInTheDocument();
    expect(screen.getByText("Failed to load subagent invocations: query failed")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Retry" }));

    expect(sessionSupervisorMock.loadSessionState).toHaveBeenCalledWith(sessionId, { force: true });
    expect(sessionSupervisorMock.loadSubagentInvocations).toHaveBeenCalledWith(sessionId, { force: true });
  });

  it("keeps last-known-good artifacts visible when a later state refresh fails", async () => {
    sessionSnap = {
      ...sessionSnap,
      sessions: {
        ...sessionSnap.sessions,
        [sessionId]: {
          ...sessionSnap.sessions[sessionId],
          artifacts: [buildSessionArtifact()],
          loadErrors: {
            state: "Failed to load session state: daemon offline",
          },
        },
      },
    };

    renderWorkbenchPage();

    fireEvent.click(await screen.findByRole("button", { name: "Toggle artifacts" }));

    const artifactsPane = document.querySelector(".wb-artifacts");
    if (!(artifactsPane instanceof HTMLElement)) {
      throw new Error("Expected artifacts pane to render");
    }

    expect(within(artifactsPane).getAllByText("session-log.bin").length).toBeGreaterThan(0);
    expect(within(artifactsPane).queryByRole("alert")).toBeNull();
    expect(screen.getByTestId("workbench-session-load-issues")).toBeInTheDocument();
  });

  it("keeps cached artifacts visible when a warm reopen refresh fails", async () => {
    sessionSnap = {
      ...sessionSnap,
      sessions: {
        ...sessionSnap.sessions,
        [sessionId]: {
          ...sessionSnap.sessions[sessionId],
          stateLoaded: false,
          artifacts: [buildSessionArtifact()],
          loadErrors: {
            state: "Failed to load session state: daemon offline",
          },
        },
      },
    };

    renderWorkbenchPage();

    fireEvent.click(await screen.findByRole("button", { name: "Toggle artifacts" }));

    const artifactsPane = document.querySelector(".wb-artifacts");
    if (!(artifactsPane instanceof HTMLElement)) {
      throw new Error("Expected artifacts pane to render");
    }

    expect(within(artifactsPane).getAllByText("session-log.bin").length).toBeGreaterThan(0);
    expect(within(artifactsPane).queryByRole("alert")).toBeNull();
    expect(screen.getByTestId("workbench-session-load-issues")).toBeInTheDocument();
  });

  it("shows the artifacts pane error when state has not loaded yet", async () => {
    sessionSnap = {
      ...sessionSnap,
      sessions: {
        ...sessionSnap.sessions,
        [sessionId]: {
          ...sessionSnap.sessions[sessionId],
          stateLoaded: false,
          artifacts: [],
          loadErrors: {
            state: "Failed to load session state: daemon offline",
          },
        },
      },
    };

    renderWorkbenchPage();

    fireEvent.click(await screen.findByRole("button", { name: "Toggle artifacts" }));

    const artifactsPane = document.querySelector(".wb-artifacts");
    if (!(artifactsPane instanceof HTMLElement)) {
      throw new Error("Expected artifacts pane to render");
    }

    expect(within(artifactsPane).getByRole("alert")).toHaveTextContent(
      "Failed to load session state: daemon offline",
    );
  });

  it("updates a mounted artifacts pane when session state later gains artifacts", async () => {
    sessionSnap = {
      ...sessionSnap,
      sessions: {
        ...sessionSnap.sessions,
        [sessionId]: {
          ...sessionSnap.sessions[sessionId],
          artifacts: [],
          stateLoaded: true,
          stateRev: 1,
          loadErrors: {},
        },
      },
    };

    const rendered = renderWorkbenchPage();

    fireEvent.click(await screen.findByRole("button", { name: "Toggle artifacts" }));

    const artifactsPane = document.querySelector(".wb-artifacts");
    if (!(artifactsPane instanceof HTMLElement)) {
      throw new Error("Expected artifacts pane to render");
    }

    expect(within(artifactsPane).getByText("No artifacts yet.")).toBeInTheDocument();

    sessionSnap = {
      ...sessionSnap,
      sessions: {
        ...sessionSnap.sessions,
        [sessionId]: {
          ...sessionSnap.sessions[sessionId],
          artifacts: [buildSessionArtifact()],
          stateRev: 2,
        },
      },
    };

    rendered.rerender(
      <VirtuosoMockContext.Provider value={{ itemHeight: 40, viewportHeight: 400 }}>
        <MemoryRouter initialEntries={[`/workspaces/${workspaceId}`]}>
          <Routes>
            <Route path="/workspaces/:id" element={<WorkbenchPage />} />
          </Routes>
        </MemoryRouter>
      </VirtuosoMockContext.Provider>,
    );

    await waitFor(() => {
      expect(within(artifactsPane).getAllByText("session-log.bin").length).toBeGreaterThan(0);
    });
  });

  it("keeps shell support loading passive when the active session state revision changes", async () => {
    const renderWorkbench = () => (
      <VirtuosoMockContext.Provider value={{ itemHeight: 40, viewportHeight: 400 }}>
        <MemoryRouter initialEntries={[`/workspaces/${workspaceId}`]}>
          <Routes>
            <Route path="/workspaces/:id" element={<WorkbenchPage />} />
          </Routes>
        </MemoryRouter>
      </VirtuosoMockContext.Provider>
    );

    const rendered = render(renderWorkbench());

    await screen.findAllByText("Starter task");
    sessionSupervisorMock.loadSessionState.mockClear();
    sessionSupervisorMock.loadSubagentInvocations.mockClear();

    sessionSnap = {
      ...sessionSnap,
      sessions: {
        ...sessionSnap.sessions,
        [sessionId]: {
          ...sessionSnap.sessions[sessionId],
          stateRev: 2,
          updatedAtMs: 1,
        },
      },
    };

    rendered.rerender(renderWorkbench());

    await screen.findAllByText("Starter task");
    expect(sessionSupervisorMock.loadSessionState).not.toHaveBeenCalled();
    expect(sessionSupervisorMock.loadSubagentInvocations).not.toHaveBeenCalled();
  });

  it("keeps shell support loading passive after remount when the supervisor marks it stale", async () => {
    sessionSnap = {
      ...sessionSnap,
      sessions: {
        ...sessionSnap.sessions,
        [sessionId]: {
          ...sessionSnap.sessions[sessionId],
          stateRev: undefined,
        },
      },
    };

    const renderWorkbench = () => (
      <VirtuosoMockContext.Provider value={{ itemHeight: 40, viewportHeight: 400 }}>
        <MemoryRouter initialEntries={[`/workspaces/${workspaceId}`]}>
          <Routes>
            <Route path="/workspaces/:id" element={<WorkbenchPage />} />
          </Routes>
        </MemoryRouter>
      </VirtuosoMockContext.Provider>
    );

    const firstRender = render(renderWorkbench());

    await screen.findAllByText("Starter task");
    expect(sessionSupervisorMock.loadSessionState).not.toHaveBeenCalled();
    expect(sessionSupervisorMock.loadSubagentInvocations).not.toHaveBeenCalled();

    firstRender.unmount();

    sessionSnap = {
      ...sessionSnap,
      sessions: {
        ...sessionSnap.sessions,
        [sessionId]: {
          ...sessionSnap.sessions[sessionId],
          stateLoaded: false,
          subagentInvocationsLoaded: false,
          updatedAtMs: sessionSnap.sessions[sessionId].updatedAtMs + 1,
        },
      },
    };

    render(renderWorkbench());

    await screen.findAllByText("Starter task");
    expect(sessionSupervisorMock.loadSessionState).not.toHaveBeenCalled();
    expect(sessionSupervisorMock.loadSubagentInvocations).not.toHaveBeenCalled();
  });
});

describe("WorkbenchPage panel analytics", () => {
  it("tracks terminal panel toggles from header button", async () => {
    const ui = (
      <VirtuosoMockContext.Provider value={{ itemHeight: 40, viewportHeight: 400 }}>
        <MemoryRouter initialEntries={[`/workspaces/${workspaceId}`]}>
          <Routes>
            <Route path="/workspaces/:id" element={<WorkbenchPage />} />
          </Routes>
        </MemoryRouter>
      </VirtuosoMockContext.Provider>
    );

    render(ui);

    fireEvent.click(await screen.findByRole("button", { name: "Toggle terminal panel" }));

    expect(trackWorkbenchPanelToggledMock).toHaveBeenCalledWith({
      panelKey: "terminal",
      open: true,
      source: "header_button",
    });
  });
});
