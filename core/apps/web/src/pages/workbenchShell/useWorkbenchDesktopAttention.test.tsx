import { act, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ClientSettingsState } from "../../state/clientSettings";
import type { WorkspaceActiveSnapshotItem } from "../../state/workspaceActiveSnapshotStore";
import type { WorkbenchTaskLiveInfo } from "./workbenchTaskActivity";
import { useWorkbenchDesktopAttention } from "./useWorkbenchDesktopAttention";

const desktopSyncWorkspaceAttention = vi.hoisted(() => vi.fn(async () => {}));
const desktopClearWindowAttention = vi.hoisted(() => vi.fn(async () => {}));
const isDesktopApp = vi.hoisted(() => vi.fn(() => true));

let clientSettingsState: ClientSettingsState;
const clientSettingsListeners = new Set<() => void>();

const emitClientSettings = () => {
  for (const listener of clientSettingsListeners) {
    listener();
  }
};

const setClientSettingsState = (next: ClientSettingsState) => {
  clientSettingsState = next;
  emitClientSettings();
};

vi.mock("../../state/clientSettings", () => ({
  getClientSettingsState: () => clientSettingsState,
  subscribeClientSettings: (listener: () => void) => {
    clientSettingsListeners.add(listener);
    return () => clientSettingsListeners.delete(listener);
  },
}));

vi.mock("../../utils/desktop", () => ({
  desktopClearWindowAttention,
  desktopSyncWorkspaceAttention,
  isDesktopApp,
}));

const baseIso = "2026-03-10T00:00:00.000Z";

const makeTaskSummary = (): WorkspaceActiveSnapshotItem =>
  ({
    id: "task-1",
    task: {
      id: "task-1",
      workspace_id: "workspace-1",
      title: "Implement desktop notifications",
      status: "active",
      primary_session_id: "session-1",
      primary_worktree_id: "worktree-1",
      last_assistant_message_at: null,
      assistant_seen_at: null,
      created_at: baseIso,
      updated_at: baseIso,
    },
    sessions: [],
    sortAtMs: Date.parse(baseIso),
  }) as WorkspaceActiveSnapshotItem;

const makeTaskLiveInfo = (overrides: Partial<WorkbenchTaskLiveInfo> = {}): WorkbenchTaskLiveInfo => ({
  workingByTask: new Set<string>(),
  errorByTask: new Set<string>(),
  lastAssistantMsByTask: {},
  ...overrides,
});

describe("useWorkbenchDesktopAttention", () => {
  beforeEach(() => {
    desktopSyncWorkspaceAttention.mockClear();
    desktopClearWindowAttention.mockClear();
    isDesktopApp.mockReturnValue(true);
    clientSettingsListeners.clear();
    clientSettingsState = {
      loaded: false,
      settings: {
        v: 3,
        desktopNotifications: {
          turnCompleted: true,
          turnFailed: true,
          badgeUnreadCount: true,
        },
        telemetry: {
          clientEnabled: true,
        },
      },
    };
  });

  afterEach(() => {
    vi.clearAllMocks();
  });

  it("waits for client settings to load before syncing attention", async () => {
    renderHook(() =>
      useWorkbenchDesktopAttention({
        workspaceId: "workspace-1",
        activeTaskIds: ["task-1"],
        tasksById: {
          "task-1": makeTaskSummary(),
        },
        taskLiveInfo: makeTaskLiveInfo({
          lastAssistantMsByTask: {
            "task-1": Date.parse("2026-03-10T00:01:00.000Z"),
          },
        }),
      }),
    );

    await Promise.resolve();
    expect(desktopSyncWorkspaceAttention).not.toHaveBeenCalled();

    act(() => {
      setClientSettingsState({
        loaded: true,
        settings: clientSettingsState.settings,
      });
    });

    await waitFor(() => {
      expect(desktopSyncWorkspaceAttention).toHaveBeenCalledWith({
        workspace_id: "workspace-1",
        unread_primary_task_count: 1,
        has_unread_error: false,
      });
    });
  });

  it("does not clear attention during rerenders before unmount", async () => {
    clientSettingsState = {
      loaded: true,
      settings: {
        v: 3,
        desktopNotifications: {
          turnCompleted: true,
          turnFailed: true,
          badgeUnreadCount: true,
        },
        telemetry: {
          clientEnabled: true,
        },
      },
    };

    const { rerender } = renderHook(
      ({ taskLiveInfo }: { taskLiveInfo: WorkbenchTaskLiveInfo }) =>
        useWorkbenchDesktopAttention({
          workspaceId: "workspace-1",
          activeTaskIds: ["task-1"],
          tasksById: {
            "task-1": makeTaskSummary(),
          },
          taskLiveInfo,
        }),
      {
        initialProps: {
          taskLiveInfo: makeTaskLiveInfo(),
        },
      },
    );

    await waitFor(() => {
      expect(desktopSyncWorkspaceAttention).toHaveBeenCalledWith({
        workspace_id: "workspace-1",
        unread_primary_task_count: 0,
        has_unread_error: false,
      });
    });
    expect(desktopClearWindowAttention).not.toHaveBeenCalled();

    desktopSyncWorkspaceAttention.mockClear();

    rerender({
      taskLiveInfo: makeTaskLiveInfo({
        errorByTask: new Set(["task-1"]),
        lastAssistantMsByTask: {
          "task-1": Date.parse("2026-03-10T00:01:00.000Z"),
        },
      }),
    });

    await waitFor(() => {
      expect(desktopSyncWorkspaceAttention).toHaveBeenCalledWith({
        workspace_id: "workspace-1",
        unread_primary_task_count: 1,
        has_unread_error: true,
      });
    });
    expect(desktopClearWindowAttention).not.toHaveBeenCalled();
  });

  it("does not sync badge attention for unread tasks that are still working", async () => {
    clientSettingsState = {
      loaded: true,
      settings: {
        v: 3,
        desktopNotifications: {
          turnCompleted: true,
          turnFailed: true,
          badgeUnreadCount: true,
        },
        telemetry: {
          clientEnabled: true,
        },
      },
    };

    renderHook(() =>
      useWorkbenchDesktopAttention({
        workspaceId: "workspace-1",
        activeTaskIds: ["task-1"],
        tasksById: {
          "task-1": makeTaskSummary(),
        },
        taskLiveInfo: makeTaskLiveInfo({
          workingByTask: new Set(["task-1"]),
          errorByTask: new Set(["task-1"]),
          lastAssistantMsByTask: {
            "task-1": Date.parse("2026-03-10T00:01:00.000Z"),
          },
        }),
      }),
    );

    await waitFor(() => {
      expect(desktopSyncWorkspaceAttention).toHaveBeenCalledWith({
        workspace_id: "workspace-1",
        unread_primary_task_count: 0,
        has_unread_error: false,
      });
    });
  });

  it("syncs zero attention when badge notifications are disabled and clears on unmount", async () => {
    clientSettingsState = {
      loaded: true,
      settings: {
        v: 3,
        desktopNotifications: {
          turnCompleted: true,
          turnFailed: true,
          badgeUnreadCount: false,
        },
        telemetry: {
          clientEnabled: true,
        },
      },
    };

    const { unmount } = renderHook(() =>
      useWorkbenchDesktopAttention({
        workspaceId: "workspace-1",
        activeTaskIds: ["task-1"],
        tasksById: {
          "task-1": makeTaskSummary(),
        },
        taskLiveInfo: makeTaskLiveInfo({
          errorByTask: new Set(["task-1"]),
          lastAssistantMsByTask: {
            "task-1": Date.parse("2026-03-10T00:01:00.000Z"),
          },
        }),
      }),
    );

    await waitFor(() => {
      expect(desktopSyncWorkspaceAttention).toHaveBeenCalledWith({
        workspace_id: "workspace-1",
        unread_primary_task_count: 0,
        has_unread_error: false,
      });
    });

    unmount();

    await waitFor(() => {
      expect(desktopClearWindowAttention).toHaveBeenCalledTimes(1);
    });
  });
});
