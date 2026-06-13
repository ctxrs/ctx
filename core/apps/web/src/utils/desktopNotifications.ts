import {
  desktopGetNotificationPermission,
  desktopRequestNotificationPermission,
  desktopShowSystemNotification,
  isDesktopApp,
  type DesktopNotificationKind,
  type DesktopNotificationPermission,
} from "./desktop";

export type { DesktopNotificationPermission } from "./desktop";

export type DesktopNotificationPayload = {
  body?: string;
  kind: DesktopNotificationKind;
  sessionId?: string;
  taskId: string;
  title: string;
  workspaceId: string;
};

export async function getDesktopNotificationPermission(): Promise<DesktopNotificationPermission> {
  if (!isDesktopApp()) return "unsupported";
  try {
    return await desktopGetNotificationPermission();
  } catch (err) {
    console.warn("desktop notifications permission status failed", err);
    return "unsupported";
  }
}

export async function requestDesktopNotificationPermission(): Promise<DesktopNotificationPermission> {
  if (!isDesktopApp()) return "unsupported";
  try {
    return await desktopRequestNotificationPermission();
  } catch (err) {
    console.warn("desktop notifications permission request failed", err);
    return "unsupported";
  }
}

export async function ensureDesktopNotificationPermission(): Promise<boolean> {
  const permission = await getDesktopNotificationPermission();
  if (permission === "granted") return true;
  if (permission !== "default") return false;
  return (await requestDesktopNotificationPermission()) === "granted";
}

export async function sendDesktopNotification(payload: DesktopNotificationPayload): Promise<boolean> {
  if (!isDesktopApp()) return false;
  try {
    const granted = await ensureDesktopNotificationPermission();
    if (!granted) return false;
    await desktopShowSystemNotification({
      kind: payload.kind,
      title: payload.title,
      body: payload.body,
      workspace_id: payload.workspaceId,
      task_id: payload.taskId,
      session_id: payload.sessionId ?? null,
    });
    return true;
  } catch (err) {
    console.warn("desktop notification send failed", err);
    return false;
  }
}
