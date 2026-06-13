import { beforeEach, describe, expect, it, vi } from "vitest";

const uiStateGet = vi.hoisted(() => vi.fn());
const uiStateSet = vi.hoisted(() => vi.fn());
const uiStateDelete = vi.hoisted(() => vi.fn());

vi.mock("./uiStateStore", () => ({
  uiStateGet,
  uiStateSet,
  uiStateDelete,
}));

const loadModule = async () => {
  vi.resetModules();
  return import("./clientSettings");
};

describe("clientSettings", () => {
  beforeEach(() => {
    uiStateGet.mockReset();
    uiStateSet.mockReset();
    uiStateDelete.mockReset();
  });

  it("loads persisted v3 settings", async () => {
    uiStateGet.mockImplementation(async (key: string) => {
      if (key === "client.settings.v3") {
        return {
          v: 3,
          desktopNotifications: {
            turnCompleted: false,
            turnFailed: true,
            badgeUnreadCount: false,
          },
          telemetry: {
            clientEnabled: false,
          },
        };
      }
      return null;
    });
    const { loadClientSettings } = await loadModule();
    const state = await loadClientSettings();

    expect(uiStateGet).toHaveBeenCalledWith("client.settings.v3");
    expect(state.settings.desktopNotifications).toEqual({
      turnCompleted: false,
      turnFailed: true,
      badgeUnreadCount: false,
    });
    expect(state.settings.telemetry).toEqual({
      clientEnabled: false,
    });
  });

  it("migrates persisted v2 settings into v3", async () => {
    uiStateGet.mockImplementation(async (key: string) => {
      if (key === "client.settings.v3") return null;
      if (key === "client.settings.v2") {
        return {
          v: 2,
          desktopNotifications: {
            turnCompleted: false,
            turnFailed: true,
            badgeUnreadCount: false,
          },
        };
      }
      return null;
    });
    const { loadClientSettings } = await loadModule();
    const state = await loadClientSettings();

    expect(state.settings).toEqual({
      v: 3,
      desktopNotifications: {
        turnCompleted: false,
        turnFailed: true,
        badgeUnreadCount: false,
      },
      telemetry: {
        clientEnabled: true,
      },
    });
    expect(uiStateSet).toHaveBeenCalledWith("client.settings.v3", state.settings);
    expect(uiStateDelete).toHaveBeenCalledWith("client.settings.v2");
  });

  it("uses enabled defaults for new installs", async () => {
    uiStateGet.mockResolvedValue(null);
    const { loadClientSettings } = await loadModule();
    const state = await loadClientSettings();

    expect(state.settings.desktopNotifications).toEqual({
      turnCompleted: true,
      turnFailed: true,
      badgeUnreadCount: true,
    });
    expect(state.settings.telemetry).toEqual({
      clientEnabled: true,
    });
  });

  it("persists v3 updates", async () => {
    uiStateGet.mockResolvedValue(null);
    const { updateClientSettings } = await loadModule();

    await updateClientSettings({
      desktopNotifications: {
        turnCompleted: false,
        turnFailed: true,
        badgeUnreadCount: false,
      },
      telemetry: {
        clientEnabled: true,
      },
    });
  });

  it("persists telemetry preference updates alongside existing settings", async () => {
    uiStateGet.mockResolvedValue(null);
    const { updateClientSettings } = await loadModule();

    await updateClientSettings({
      telemetry: {
        clientEnabled: false,
      },
    });

    expect(uiStateSet).toHaveBeenCalledWith("client.settings.v3", {
      v: 3,
      desktopNotifications: {
        turnCompleted: true,
        turnFailed: true,
        badgeUnreadCount: true,
      },
      telemetry: {
        clientEnabled: false,
      },
    });
  });
});
