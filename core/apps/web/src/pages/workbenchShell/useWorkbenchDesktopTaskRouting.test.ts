import { describe, expect, it, vi } from "vitest";
import type { PersistedWorkbenchWindowV1 } from "../../workbench/types";
import type { WorkbenchStore } from "../../workbench/store";
import {
  applyDesktopTaskRoutePayload,
  buildWorkbenchRoutePublishReq,
  collectWorkbenchRouteTasks,
  workbenchDesktopRouteConnectionSignature,
} from "./useWorkbenchDesktopTaskRouting";

const windowState: PersistedWorkbenchWindowV1 = {
  v: 1,
  focusedLeafId: "leaf-a",
  layout: {
    kind: "split",
    id: "split-a",
    direction: "horizontal",
    ratio: 0.5,
    first: {
      kind: "leaf",
      id: "leaf-a",
      activeTabId: "tab-a",
      tabs: [
        {
          id: "tab-a",
          kind: "task",
          ref: { taskId: "task-b", sessionId: "session-b" },
        },
        {
          id: "tab-duplicate",
          kind: "task",
          ref: { taskId: "task-b", sessionId: "session-b" },
        },
      ],
    },
    second: {
      kind: "leaf",
      id: "leaf-b",
      activeTabId: "tab-b",
      tabs: [
        {
          id: "tab-b",
          kind: "task",
          ref: { taskId: "task-a", sessionId: null },
        },
        {
          id: "tab-new",
          kind: "new_task",
        },
      ],
    },
  },
};

describe("useWorkbenchDesktopTaskRouting helpers", () => {
  it("collects every open task tab as stable task/session pairs", () => {
    expect(collectWorkbenchRouteTasks(windowState.layout)).toEqual([
      { task_id: "task-a" },
      { task_id: "task-b", session_id: "session-b" },
    ]);
  });

  it("builds a publish request with active task and all open tasks", () => {
    expect(
      buildWorkbenchRoutePublishReq({
        activeSessionId: "session-b",
        activeTaskId: "task-b",
        windowState,
        workspaceId: " workspace-1 ",
        workspaceName: " Demo Workspace ",
      }),
    ).toEqual({
      active_session_id: "session-b",
      active_task_id: "task-b",
      open_tasks: [
        { task_id: "task-a" },
        { task_id: "task-b", session_id: "session-b" },
      ],
      workspace_id: "workspace-1",
      workspace_label: "Demo Workspace",
    });
  });

  it("applies matching native task route payloads through the workbench store", () => {
    const focusTask = vi.fn<WorkbenchStore["focusTask"]>(() => true);
    const store: Pick<WorkbenchStore, "focusTask" | "getNavToken"> = {
      focusTask,
      getNavToken: () => 42,
    };

    expect(
      applyDesktopTaskRoutePayload({
        payload: {
          route_id: "route-1",
          session_id: "session-1",
          task_id: "task-1",
          workspace_id: "workspace-1",
        },
        workbenchStore: store,
        workspaceId: "workspace-1",
      }),
    ).toBe(true);
    expect(focusTask).toHaveBeenCalledWith("task-1", "session-1", {
      navToken: 42,
      source: "system",
    });
  });

  it("ignores native task route payloads for other workspaces", () => {
    const focusTask = vi.fn<WorkbenchStore["focusTask"]>(() => true);
    const store: Pick<WorkbenchStore, "focusTask" | "getNavToken"> = {
      focusTask,
      getNavToken: () => 42,
    };

    expect(
      applyDesktopTaskRoutePayload({
        payload: {
          route_id: "route-1",
          task_id: "task-1",
          workspace_id: "workspace-other",
        },
        workbenchStore: store,
        workspaceId: "workspace-1",
      }),
    ).toBe(false);
    expect(focusTask).not.toHaveBeenCalled();
  });

  it("changes the publish signature when daemon connection scope changes", () => {
    const localSignature = workbenchDesktopRouteConnectionSignature({
      authToken: "token",
      baseUrl: "http://127.0.0.1:1",
      runId: "run-1",
      source: "desktop",
      targetScope: { kind: "desktop_local" },
      wsBaseUrl: "ws://127.0.0.1:1",
    });
    const sshSignature = workbenchDesktopRouteConnectionSignature({
      authToken: "token",
      baseUrl: "http://127.0.0.1:2",
      runId: "run-2",
      source: "desktop",
      targetScope: {
        dataDir: null,
        host: "host",
        kind: "desktop_ssh",
        port: 8787,
        user: null,
      },
      wsBaseUrl: "ws://127.0.0.1:2",
    });

    expect(localSignature).not.toEqual(sshSignature);
  });
});
