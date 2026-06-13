import type {
  BlobUploadResp,
  DesktopCodexLoginRelayReq,
  DesktopDeepLinkToken,
  DesktopDockRecentLocalWorkspace,
  DesktopEditorSettings,
  DesktopMenuItemStateUpdate,
  DesktopNotificationPermission,
  DesktopOpenExternalUrlReq,
  DesktopOpenFileReq,
  DesktopOpenPathReq,
  DesktopOpenWorkspaceInNewWindowReq,
  DesktopReadBinaryFileResp,
  DesktopRecordWorkbenchRouteReq,
  DesktopRecordWorkspaceVisitReq,
  DesktopSaveTextFileReq,
  DesktopSetDockRecentLocalWorkspacesReq,
  DesktopSetMenuStateReq,
  DesktopSetOpenWorkspacesReq,
  DesktopSetWindowTitleReq,
  DesktopShowSystemNotificationReq,
  DesktopStorageBatchOp,
  DesktopStorageBatchReq,
  DesktopStorageGetReq,
  DesktopStorageNotice,
  DesktopSyncWorkspaceAttentionReq,
  DesktopTaskRouteAckReq,
  DesktopTaskRoutePayload,
  DesktopTitlebarColor,
  DesktopUploadBlobReq,
  DesktopWebviewRecoveryAutomationSnapshot,
  DesktopWebviewRecoveryFaultReq,
  DesktopWebviewRecoveryHeartbeatReq,
  DesktopWebviewRecoveryIncident,
  DesktopGitCloneReq,
  DesktopUpdateChannelSettings,
} from "../generated/desktop-ipc";
import { invoke, invokeDesktopReq, isDesktopApp } from "./desktopCore";

export const desktopPickFolder = async (): Promise<string | null> =>
  invoke<string | null>("desktop_pick_folder");

export const desktopGitClone = async (repo_url: string, dest_parent: string): Promise<string> =>
  invokeDesktopReq<DesktopGitCloneReq, string>("desktop_git_clone", { repo_url, dest_parent });

export const desktopSaveTextFile = async (args: DesktopSaveTextFileReq): Promise<string | null> =>
  invokeDesktopReq<DesktopSaveTextFileReq, string | null>("desktop_save_text_file", args);

export const desktopReadBinaryFile = async (args: DesktopOpenPathReq): Promise<DesktopReadBinaryFileResp> =>
  invokeDesktopReq<DesktopOpenPathReq, DesktopReadBinaryFileResp>(
    "desktop_read_binary_file",
    args,
  );

export const desktopGetDeepLinkToken = async (): Promise<DesktopDeepLinkToken> =>
  invoke<DesktopDeepLinkToken>("desktop_get_deep_link_token");

export const desktopGetVersion = async (): Promise<string> => {
  const mod = await import("@tauri-apps/api/app");
  return mod.getVersion();
};

export const desktopOpenFile = async (req: DesktopOpenFileReq): Promise<void> =>
  invokeDesktopReq<DesktopOpenFileReq, void>("desktop_open_file", req);

export const desktopOpenPath = async (req: DesktopOpenPathReq): Promise<void> =>
  invokeDesktopReq<DesktopOpenPathReq, void>("desktop_open_path", req);

export const desktopOpenDeepLink = async (url: string): Promise<void> =>
  invokeDesktopReq<DesktopOpenExternalUrlReq, void>("desktop_open_deep_link", { url });

export const desktopGetEditorSettings = async (): Promise<DesktopEditorSettings> =>
  invoke<DesktopEditorSettings>("desktop_get_editor_settings");

export const desktopUpdateEditorSettings = async (
  settings: DesktopEditorSettings,
): Promise<DesktopEditorSettings> =>
  invokeDesktopReq<DesktopEditorSettings, DesktopEditorSettings>(
    "desktop_update_editor_settings",
    settings,
  );

export const desktopGetUpdateChannel = async (): Promise<DesktopUpdateChannelSettings> =>
  invoke<DesktopUpdateChannelSettings>("desktop_get_update_channel");

export const desktopUpdateUpdateChannel = async (
  settings: DesktopUpdateChannelSettings,
): Promise<DesktopUpdateChannelSettings> =>
  invokeDesktopReq<DesktopUpdateChannelSettings, DesktopUpdateChannelSettings>(
    "desktop_update_update_channel",
    settings,
  );

export const desktopStartCodexLoginRelay = async (req: DesktopCodexLoginRelayReq): Promise<boolean> =>
  invokeDesktopReq<DesktopCodexLoginRelayReq, boolean>("desktop_start_codex_login_relay", req);

export const desktopStorageGet = async (key: string): Promise<unknown | null> =>
  invokeDesktopReq<DesktopStorageGetReq, unknown | null>("desktop_storage_get", { key });

export const desktopStorageBatch = async (ops: DesktopStorageBatchOp[]): Promise<void> =>
  invokeDesktopReq<DesktopStorageBatchReq, void>("desktop_storage_batch", { ops });

export const desktopStorageConsumeNotice = async (): Promise<DesktopStorageNotice | null> =>
  invoke<DesktopStorageNotice | null>("desktop_storage_consume_notice");

export const desktopWebviewRecoveryHeartbeat = async (
  req: DesktopWebviewRecoveryHeartbeatReq,
): Promise<void> =>
  invokeDesktopReq<DesktopWebviewRecoveryHeartbeatReq, void>(
    "desktop_webview_recovery_heartbeat",
    req,
  );

export const desktopWebviewRecoveryConsumeIncidents = async (): Promise<DesktopWebviewRecoveryIncident[]> =>
  invoke<DesktopWebviewRecoveryIncident[]>("desktop_webview_recovery_consume_incidents");

export const desktopTriggerWebviewRecoveryFault = async (
  req: DesktopWebviewRecoveryFaultReq,
): Promise<void> =>
  invokeDesktopReq<DesktopWebviewRecoveryFaultReq, void>(
    "desktop_trigger_webview_recovery_fault",
    req,
  );

export const desktopGetWebviewRecoveryAutomationSnapshot = async (): Promise<DesktopWebviewRecoveryAutomationSnapshot> =>
  invoke<DesktopWebviewRecoveryAutomationSnapshot>(
    "desktop_get_webview_recovery_automation_snapshot",
  );

export const desktopUploadBlob = async (args: DesktopUploadBlobReq): Promise<BlobUploadResp> =>
  invokeDesktopReq<DesktopUploadBlobReq, BlobUploadResp>("desktop_upload_blob", args);

export const desktopSetOpenWorkspaces = async (workspace_ids: string[]): Promise<void> =>
  invokeDesktopReq<DesktopSetOpenWorkspacesReq, void>("desktop_set_open_workspaces", {
    workspace_ids,
  });

export const desktopOpenLauncherInNewWindow = async (): Promise<void> =>
  invoke<void>("desktop_open_launcher_in_new_window");

export const desktopOpenWorkspaceInNewWindow = async (workspace_id: string): Promise<void> =>
  invokeDesktopReq<DesktopOpenWorkspaceInNewWindowReq, void>(
    "desktop_open_workspace_in_new_window",
    { workspace_id },
  );

export const desktopOpenWorkspaceSetupInNewWindow = async (): Promise<void> =>
  invoke<void>("desktop_open_workspace_setup_in_new_window");

export const desktopSetDockRecentLocalWorkspaces = async (
  entries: DesktopDockRecentLocalWorkspace[],
): Promise<void> =>
  invokeDesktopReq<DesktopSetDockRecentLocalWorkspacesReq, void>(
    "desktop_set_dock_recent_local_workspaces",
    { entries },
  );

export const desktopRecordWorkspaceVisit = async (
  workspace_id: string,
  workspace_label: string,
): Promise<void> => {
  if (!isDesktopApp()) return;
  const req: DesktopRecordWorkspaceVisitReq = { workspace_id, workspace_label };
  await invokeDesktopReq<DesktopRecordWorkspaceVisitReq, void>(
    "desktop_record_workspace_visit",
    req,
  );
};

export const desktopRecordWorkbenchRoute = async (
  req: DesktopRecordWorkbenchRouteReq,
): Promise<void> => {
  if (!isDesktopApp()) return;
  await invokeDesktopReq<DesktopRecordWorkbenchRouteReq, void>(
    "desktop_record_workbench_route",
    req,
  );
};

export const desktopConsumePendingTaskRoute = async (): Promise<DesktopTaskRoutePayload | null> => {
  if (!isDesktopApp()) return null;
  return invoke<DesktopTaskRoutePayload | null>("desktop_consume_pending_task_route");
};

export const desktopAckTaskRoute = async (route_id: string): Promise<void> => {
  if (!isDesktopApp()) return;
  const req: DesktopTaskRouteAckReq = { route_id };
  await invokeDesktopReq<DesktopTaskRouteAckReq, void>("desktop_ack_task_route", req);
};

export const desktopSetTitlebarColor = async (color: DesktopTitlebarColor): Promise<void> =>
  invokeDesktopReq<DesktopTitlebarColor, void>("desktop_set_titlebar_color", color);

export const desktopSetMenuState = async (items: DesktopMenuItemStateUpdate[]): Promise<void> =>
  invokeDesktopReq<DesktopSetMenuStateReq, void>("desktop_set_menu_state", { items });

export const desktopSetWindowTitle = async (title: string): Promise<void> => {
  if (!isDesktopApp()) return;
  const req: DesktopSetWindowTitleReq = { title };
  await invokeDesktopReq<DesktopSetWindowTitleReq, void>("desktop_set_window_title", req);
};

export const desktopGetNotificationPermission = async (): Promise<DesktopNotificationPermission> =>
  invoke<DesktopNotificationPermission>("desktop_get_notification_permission");

export const desktopRequestNotificationPermission = async (): Promise<DesktopNotificationPermission> =>
  invoke<DesktopNotificationPermission>("desktop_request_notification_permission");

export const desktopShowSystemNotification = async (
  req: DesktopShowSystemNotificationReq,
): Promise<void> =>
  invokeDesktopReq<DesktopShowSystemNotificationReq, void>(
    "desktop_show_system_notification",
    req,
  );

export const desktopSyncWorkspaceAttention = async (
  req: DesktopSyncWorkspaceAttentionReq,
): Promise<void> =>
  invokeDesktopReq<DesktopSyncWorkspaceAttentionReq, void>(
    "desktop_sync_workspace_attention",
    req,
  );

export const desktopClearWindowAttention = async (): Promise<void> =>
  invoke<void>("desktop_clear_window_attention");
