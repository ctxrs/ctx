import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { DesktopDaemonConnectionSyncResult } from "../api/desktopDaemonConnection";
import type { DesktopConnectionInfo } from "../utils/desktop";

const daemonFetchRawMock = vi.hoisted(() => vi.fn());
const syncDesktopDaemonConnectionFromBridgeMock = vi.hoisted(() => vi.fn());
const desktopGetVersionMock = vi.hoisted(() => vi.fn());

vi.mock("../api/client", () => ({
  daemonFetchRaw: daemonFetchRawMock,
}));

vi.mock("../api/desktopDaemonConnection", () => ({
  syncDesktopDaemonConnectionFromBridge: syncDesktopDaemonConnectionFromBridgeMock,
}));

vi.mock("../utils/desktop", () => ({
  desktopGetVersion: desktopGetVersionMock,
  isDesktopApp: () => true,
}));

const makeDesktopSyncResult = (
  infoOverrides: Partial<DesktopConnectionInfo>,
): DesktopDaemonConnectionSyncResult => ({
  connection: {
    baseUrl: null,
    wsBaseUrl: null,
    authToken: null,
    runId: null,
  },
  info: {
    kind: "local",
    intent: "auto_local_bootstrap" as const,
    local_auto_bootstrap_allowed: true,
    ...infoOverrides,
  },
  synced: true,
  error: null,
});

describe("daemonAvailabilityMonitor", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.clearAllMocks();
    vi.resetModules();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("shares one liveness poller across multiple subscribers", async () => {
    daemonFetchRawMock.mockResolvedValue({
      status: 200,
      body: JSON.stringify({
        daemon_version: "1.2.3",
        compatibility: {
          desktop_exact_version: "1.2.3",
        },
      }),
      content_type: "application/json",
    });
    syncDesktopDaemonConnectionFromBridgeMock.mockResolvedValue(
      makeDesktopSyncResult({ kind: "local" }),
    );
    desktopGetVersionMock.mockResolvedValue("1.2.3");

    const mod = await import("./daemonAvailabilityMonitor");
    const listenerA = vi.fn();
    const listenerB = vi.fn();

    const unsubscribeA = mod.subscribeDaemonAvailability(listenerA);
    const unsubscribeB = mod.subscribeDaemonAvailability(listenerB);
    await mod.checkDaemonAvailabilityNow();

    expect(daemonFetchRawMock).toHaveBeenCalledTimes(1);
    expect(syncDesktopDaemonConnectionFromBridgeMock).toHaveBeenCalledTimes(1);
    expect(desktopGetVersionMock).toHaveBeenCalledTimes(1);
    expect(mod.getDaemonAvailabilitySnapshot().status).toBe("ok");

    await vi.advanceTimersByTimeAsync(20_000);

    expect(daemonFetchRawMock).toHaveBeenCalledTimes(2);
    expect(syncDesktopDaemonConnectionFromBridgeMock).toHaveBeenCalledTimes(2);
    expect(desktopGetVersionMock).toHaveBeenCalledTimes(2);

    unsubscribeA();
    unsubscribeB();
    await vi.advanceTimersByTimeAsync(20_000);

    expect(daemonFetchRawMock).toHaveBeenCalledTimes(2);
  });

  it("classifies daemon and desktop version mismatches", async () => {
    daemonFetchRawMock.mockResolvedValue({
      status: 200,
      body: JSON.stringify({
        daemon_version: "2.0.0",
        compatibility: {
          desktop_exact_version: "2.0.0",
        },
      }),
      content_type: "application/json",
    });
    syncDesktopDaemonConnectionFromBridgeMock.mockResolvedValue(
      makeDesktopSyncResult({ kind: "ssh" }),
    );
    desktopGetVersionMock.mockResolvedValue("1.5.0");

    const mod = await import("./daemonAvailabilityMonitor");
    const unsubscribe = mod.subscribeDaemonAvailability(() => {});
    await mod.checkDaemonAvailabilityNow();

    expect(mod.getDaemonAvailabilitySnapshot()).toMatchObject({
      status: "mismatch",
      desktopKind: "ssh",
      desktopVersion: "1.5.0",
      mismatch: {
        desktop_version: "1.5.0",
        daemon_version: "2.0.0",
        expected_version: "2.0.0",
        kind: "desktop_older",
      },
    });

    unsubscribe();
  });

  it("surfaces pending remote update state from desktop bridge sync", async () => {
    daemonFetchRawMock.mockResolvedValue({
      status: 200,
      body: JSON.stringify({
        ...{
          daemon_version: "1.0.0",
          compatibility: {
            desktop_exact_version: "1.0.0",
          },
        },
      }),
      content_type: "application/json",
    });
    syncDesktopDaemonConnectionFromBridgeMock.mockResolvedValue(
      makeDesktopSyncResult({
        kind: "ssh",
        remote_update_state: "pending",
        remote_update_message: "waiting for idle",
      }),
    );
    desktopGetVersionMock.mockResolvedValue("1.0.0");

    const mod = await import("./daemonAvailabilityMonitor");
    const unsubscribe = mod.subscribeDaemonAvailability(() => {});
    await mod.checkDaemonAvailabilityNow();

    expect(mod.getDaemonAvailabilitySnapshot()).toMatchObject({
      status: "ok",
      remoteUpdateState: "pending",
      remoteUpdateMessage: "waiting for idle",
    });
    expect(syncDesktopDaemonConnectionFromBridgeMock).toHaveBeenCalledWith({
      connectLocalWhenMissing: false,
      reason: "daemon_availability_poll",
    });
    expect(daemonFetchRawMock).toHaveBeenCalledWith("/api/health", undefined, {
      connectLocalWhenMissing: false,
    });

    unsubscribe();
  });

  it("preserves ssh remote update metadata when bridge sync is throttled", async () => {
    daemonFetchRawMock.mockResolvedValue({
      status: 200,
      body: JSON.stringify({
        daemon_version: "1.0.0",
        compatibility: {
          desktop_exact_version: "1.0.0",
        },
      }),
      content_type: "application/json",
    });
    syncDesktopDaemonConnectionFromBridgeMock
      .mockResolvedValueOnce(
        makeDesktopSyncResult({
          kind: "ssh",
          remote_update_state: "pending",
          remote_update_message: "waiting for idle",
        }),
      )
      .mockResolvedValueOnce({
        connection: {
          baseUrl: "http://127.0.0.1:4399",
          wsBaseUrl: "ws://127.0.0.1:4399",
          authToken: "token",
          runId: null,
        },
        info: null,
        synced: false,
        error: null,
      });
    desktopGetVersionMock.mockResolvedValue("1.0.0");

    const mod = await import("./daemonAvailabilityMonitor");
    const unsubscribe = mod.subscribeDaemonAvailability(() => {});
    await mod.checkDaemonAvailabilityNow();
    expect(mod.getDaemonAvailabilitySnapshot()).toMatchObject({
      desktopKind: "ssh",
      remoteUpdateState: "pending",
      remoteUpdateMessage: "waiting for idle",
    });

    await mod.checkDaemonAvailabilityNow();
    expect(mod.getDaemonAvailabilitySnapshot()).toMatchObject({
      desktopKind: "ssh",
      remoteUpdateState: "pending",
      remoteUpdateMessage: "waiting for idle",
    });

    unsubscribe();
  });
});
