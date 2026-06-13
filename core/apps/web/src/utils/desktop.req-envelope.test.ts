import { beforeEach, describe, expect, it, vi } from "vitest";

const invokeMock = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/core", () => ({
  invoke: invokeMock,
}));

const TAURI_GLOBALS = {
  __TAURI__: {},
};

describe("desktop request envelopes", () => {
  beforeEach(() => {
    vi.resetModules();
    invokeMock.mockReset();
    Object.assign(globalThis, TAURI_GLOBALS);
  });

  it("wraps raw desktop commands in req payloads", async () => {
    invokeMock.mockResolvedValue(undefined);

    const desktop = await import("./desktop");

    await desktop.desktopGitClone("https://example.com/repo.git", "/tmp/workspaces");
    await desktop.desktopSaveTextFile({ suggested_name: "notes.md", contents: "hello" });
    await desktop.desktopStorageGet("theme");
    await desktop.desktopStorageBatch([{ kind: "delete", key: "theme" }]);
    await desktop.desktopSetOpenWorkspaces(["ws-1", "ws-2"]);
    await desktop.desktopOpenWorkspaceInNewWindow("ws-3");
    await desktop.desktopSetDockRecentLocalWorkspaces([{ label: "Workspace", root_path: "/tmp/ws" }]);
    await desktop.desktopRecordWorkspaceVisit("ws-4", "Workspace 4");
    await desktop.desktopRecordWorkbenchRoute({
      active_task_id: "task-1",
      open_tasks: [{ task_id: "task-1", session_id: "session-1" }],
      workspace_id: "ws-4",
      workspace_label: "Workspace 4",
    });
    await desktop.desktopConsumePendingTaskRoute();
    await desktop.desktopAckTaskRoute("route-1");
    await desktop.desktopSetTitlebarColor({ r: 1, g: 2, b: 3 });
    await desktop.desktopSetMenuState([{ id: "task.new", enabled: true }]);
    await desktop.desktopSetWindowTitle("ctx");
    await desktop.desktopShowSystemNotification({
      kind: "turn_completed",
      title: "Turn completed",
      body: "Demo task",
      workspace_id: "ws-1",
      task_id: "task-1",
      session_id: "session-1",
    });
    await desktop.desktopSyncWorkspaceAttention({
      workspace_id: "ws-1",
      unread_primary_task_count: 3,
      has_unread_error: true,
    });
    await desktop.desktopUpdateEditorSettings({ target: "cursor" });
    await desktop.openExternalLink("https://example.com/docs");

    expect(invokeMock.mock.calls).toEqual([
      ["desktop_git_clone", { req: { repo_url: "https://example.com/repo.git", dest_parent: "/tmp/workspaces" } }],
      ["desktop_save_text_file", { req: { suggested_name: "notes.md", contents: "hello" } }],
      ["desktop_storage_get", { req: { key: "theme" } }],
      ["desktop_storage_batch", { req: { ops: [{ kind: "delete", key: "theme" }] } }],
      ["desktop_set_open_workspaces", { req: { workspace_ids: ["ws-1", "ws-2"] } }],
      ["desktop_open_workspace_in_new_window", { req: { workspace_id: "ws-3" } }],
      ["desktop_set_dock_recent_local_workspaces", { req: { entries: [{ label: "Workspace", root_path: "/tmp/ws" }] } }],
      ["desktop_record_workspace_visit", { req: { workspace_id: "ws-4", workspace_label: "Workspace 4" } }],
      [
        "desktop_record_workbench_route",
        {
          req: {
            active_task_id: "task-1",
            open_tasks: [{ task_id: "task-1", session_id: "session-1" }],
            workspace_id: "ws-4",
            workspace_label: "Workspace 4",
          },
        },
      ],
      ["desktop_consume_pending_task_route", undefined],
      ["desktop_ack_task_route", { req: { route_id: "route-1" } }],
      ["desktop_set_titlebar_color", { req: { r: 1, g: 2, b: 3 } }],
      ["desktop_set_menu_state", { req: { items: [{ id: "task.new", enabled: true }] } }],
      ["desktop_set_window_title", { req: { title: "ctx" } }],
      [
        "desktop_show_system_notification",
        {
          req: {
            kind: "turn_completed",
            title: "Turn completed",
            body: "Demo task",
            workspace_id: "ws-1",
            task_id: "task-1",
            session_id: "session-1",
          },
        },
      ],
      [
        "desktop_sync_workspace_attention",
        {
          req: {
            workspace_id: "ws-1",
            unread_primary_task_count: 3,
            has_unread_error: true,
          },
        },
      ],
      ["desktop_update_editor_settings", { req: { target: "cursor" } }],
      ["desktop_open_external_url", { req: { url: "https://example.com/docs" } }],
    ]);
  });

  it("wraps updater desktop commands in req payloads", async () => {
    invokeMock.mockResolvedValue(undefined);

    const desktop = await import("./desktop");

    await desktop.desktopUpdateRemoteDaemon({ channel: "canary" });
    await desktop.desktopCheckAppUpdate();
    await desktop.desktopGetAppUpdateState({ channel: "canary" });
    await desktop.desktopApplyAppUpdate({ downloadId: "download-42" });

    expect(invokeMock.mock.calls).toEqual([
      ["desktop_update_remote_daemon", { req: { confirm: true, channel: "canary" } }],
      ["desktop_check_app_update", { req: {} }],
      ["desktop_get_app_update_state", { req: { channel: "canary" } }],
      [
        "desktop_apply_app_update",
        { req: { confirm: true, download_id: "download-42" } },
      ],
    ]);
  });
});
