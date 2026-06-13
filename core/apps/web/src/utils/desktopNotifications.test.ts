import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  ensureDesktopNotificationPermission,
  getDesktopNotificationPermission,
  requestDesktopNotificationPermission,
  sendDesktopNotification,
} from "./desktopNotifications";

const isDesktopApp = vi.hoisted(() => vi.fn());
const desktopGetNotificationPermission = vi.hoisted(() => vi.fn());
const desktopRequestNotificationPermission = vi.hoisted(() => vi.fn());
const desktopShowSystemNotification = vi.hoisted(() => vi.fn());

vi.mock("./desktop", () => ({
  isDesktopApp,
  desktopGetNotificationPermission,
  desktopRequestNotificationPermission,
  desktopShowSystemNotification,
}));

describe("desktopNotifications", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    isDesktopApp.mockReturnValue(true);
    desktopGetNotificationPermission.mockResolvedValue("granted");
    desktopRequestNotificationPermission.mockResolvedValue("granted");
    desktopShowSystemNotification.mockResolvedValue(undefined);
  });

  it("returns unsupported when not running in the desktop app", async () => {
    isDesktopApp.mockReturnValue(false);

    await expect(getDesktopNotificationPermission()).resolves.toBe("unsupported");
    await expect(requestDesktopNotificationPermission()).resolves.toBe("unsupported");
  });

  it("requests permission only when the current status is default", async () => {
    desktopGetNotificationPermission.mockResolvedValue("default");
    desktopRequestNotificationPermission.mockResolvedValue("granted");

    await expect(ensureDesktopNotificationPermission()).resolves.toBe(true);
    expect(desktopRequestNotificationPermission).toHaveBeenCalledTimes(1);
  });

  it("does not send notifications when permission is denied", async () => {
    desktopGetNotificationPermission.mockResolvedValue("denied");

    await expect(
      sendDesktopNotification({
        kind: "turn_completed",
        title: "Turn completed",
        workspaceId: "ws-1",
        taskId: "task-1",
      }),
    ).resolves.toBe(false);
    expect(desktopShowSystemNotification).not.toHaveBeenCalled();
  });

  it("sends notifications when permission is granted", async () => {
    desktopGetNotificationPermission.mockResolvedValue("granted");

    await expect(
      sendDesktopNotification({
        kind: "turn_failed",
        title: "Turn failed",
        body: "Session title",
        workspaceId: "ws-1",
        taskId: "task-1",
        sessionId: "session-1",
      }),
    ).resolves.toBe(true);
    expect(desktopShowSystemNotification).toHaveBeenCalledWith({
      kind: "turn_failed",
      title: "Turn failed",
      body: "Session title",
      workspace_id: "ws-1",
      task_id: "task-1",
      session_id: "session-1",
    });
  });
});
