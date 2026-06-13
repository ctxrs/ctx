import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { beforeEach, describe, expect, it, vi } from "vitest";
import DaemonAvailabilityOverlay from "./DaemonAvailabilityOverlay";
import { daemonFetchRaw } from "../api/client";
import {
  syncDesktopDaemonConnectionFromBridge,
  type DesktopDaemonConnectionSyncResult,
} from "../api/desktopDaemonConnection";
import { useDaemonBaseUrl } from "../api/useDaemonConnection";
import {
  desktopApplyAppUpdate,
  type DesktopConnectionInfo,
  desktopGetVersion,
  desktopRestartLocalDaemon,
  desktopRestartApp,
  type DesktopAppUpdateApplyResp,
  desktopUpdateRemoteDaemon,
  isDesktopApp,
} from "../utils/desktop";

vi.mock("../api/client", () => ({
  applyDaemonDesktopConnection: vi.fn(),
  daemonFetchRaw: vi.fn(),
}));

vi.mock("../api/desktopDaemonConnection", () => ({
  syncDesktopDaemonConnectionFromBridge: vi.fn(),
}));

vi.mock("../api/useDaemonConnection", () => ({
  useDaemonBaseUrl: vi.fn(),
}));

vi.mock("../utils/desktop", () => ({
  desktopApplyAppUpdate: vi.fn(),
  desktopConnectLocal: vi.fn(),
  desktopGetVersion: vi.fn(),
  desktopRestartLocalDaemon: vi.fn(),
  desktopRestartApp: vi.fn(),
  desktopUpdateRemoteDaemon: vi.fn(),
  isDesktopApp: vi.fn(),
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

const renderOverlay = (path = "/workspaces/ws-1") =>
  render(
    <MemoryRouter
      initialEntries={[path]}
      future={{ v7_startTransition: true, v7_relativeSplatPath: true }}
    >
      <DaemonAvailabilityOverlay />
    </MemoryRouter>,
  );

const baseHealth = {
  version: "1.0.0",
  daemon_version: "1.0.0",
  pid: 123,
  data_root: "/tmp",
  daemon_url: "http://127.0.0.1:4399",
  auth_required: false,
  compatibility: {
    desktop_exact_version: "1.0.0",
    mobile_api_min: 1,
    mobile_api_max: 1,
  },
};

const makeDesktopApplyResp = (
  overrides: Partial<DesktopAppUpdateApplyResp> = {},
): DesktopAppUpdateApplyResp => ({
  applied: true,
  latest_version: "2.0.0",
  message: "updated",
  needs_restart: true,
  up_to_date: false,
  ...overrides,
});

describe("DaemonAvailabilityOverlay", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(useDaemonBaseUrl).mockReturnValue(null);
    vi.mocked(syncDesktopDaemonConnectionFromBridge).mockResolvedValue(
      makeDesktopSyncResult({ kind: "local" }),
    );
    vi.mocked(desktopRestartLocalDaemon).mockResolvedValue({ kind: "local" });
    vi.mocked(desktopRestartApp).mockResolvedValue({
      requested: true,
      message: "restart requested",
    });
    vi.mocked(desktopUpdateRemoteDaemon).mockResolvedValue({ updated: true, message: "ok" });
    vi.mocked(desktopApplyAppUpdate).mockResolvedValue(makeDesktopApplyResp());
    vi.spyOn(window, "confirm").mockReturnValue(true);
  });

  it("shows update-required copy when local data was migrated by a newer app", async () => {
    vi.mocked(isDesktopApp).mockReturnValue(true);
    vi.mocked(desktopGetVersion).mockResolvedValue("0.60.0");
    vi.mocked(syncDesktopDaemonConnectionFromBridge).mockResolvedValue({
      connection: {
        baseUrl: null,
        wsBaseUrl: null,
        authToken: null,
        runId: null,
      },
      info: null,
      synced: false,
      error: "Error: migration 64 was previously applied but is missing in the resolved migrations",
    });

    renderOverlay("/");

    expect(await screen.findByRole("dialog", { name: "Update required" })).toBeInTheDocument();
    expect(screen.getByText("Update Required")).toBeInTheDocument();
    expect(screen.queryByText("Update required")).not.toBeInTheDocument();
    expect(
      screen.getByText(/Your ctx data on this machine was already migrated to a newer version/i),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Update ctx" })).toBeInTheDocument();
    expect(screen.getAllByRole("button")).toHaveLength(1);
    expect(screen.queryByText("Open launcher")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Retry" })).not.toBeInTheDocument();
    expect(vi.mocked(daemonFetchRaw)).not.toHaveBeenCalled();
  });

  it("updates and relaunches from the update-required CTA", async () => {
    vi.mocked(isDesktopApp).mockReturnValue(true);
    vi.mocked(desktopGetVersion).mockResolvedValue("0.60.0");
    vi.mocked(syncDesktopDaemonConnectionFromBridge).mockResolvedValue({
      connection: {
        baseUrl: null,
        wsBaseUrl: null,
        authToken: null,
        runId: null,
      },
      info: null,
      synced: false,
      error: "Error: migration 64 was previously applied but is missing in the resolved migrations",
    });

    renderOverlay();
    fireEvent.click(await screen.findByRole("button", { name: "Update ctx" }));

    await waitFor(() => {
      expect(vi.mocked(desktopApplyAppUpdate)).toHaveBeenCalledTimes(1);
      expect(vi.mocked(desktopApplyAppUpdate).mock.calls[0]).toEqual([]);
      expect(vi.mocked(desktopRestartApp)).toHaveBeenCalledTimes(1);
    });
  });

  it("shows manual install guidance when no compatible update is found", async () => {
    vi.mocked(isDesktopApp).mockReturnValue(true);
    vi.mocked(desktopGetVersion).mockResolvedValue("0.60.0");
    vi.mocked(syncDesktopDaemonConnectionFromBridge).mockResolvedValue({
      connection: {
        baseUrl: null,
        wsBaseUrl: null,
        authToken: null,
        runId: null,
      },
      info: null,
      synced: false,
      error: "Error: migration 64 was previously applied but is missing in the resolved migrations",
    });
    vi.mocked(desktopApplyAppUpdate).mockResolvedValue(
      makeDesktopApplyResp({
        applied: false,
        latest_version: "0.60.0",
        message: "Already up to date.",
        needs_restart: false,
        up_to_date: true,
      }),
    );

    renderOverlay();
    fireEvent.click(await screen.findByRole("button", { name: "Update ctx" }));

    expect(
      await screen.findByText(/No compatible update was found\. Install the latest ctx/i),
    ).toBeInTheDocument();
    expect(vi.mocked(desktopRestartApp)).not.toHaveBeenCalled();
    expect(vi.mocked(daemonFetchRaw)).not.toHaveBeenCalled();
  });

  it("shows mismatch when the daemon is older than the desktop app", async () => {
    vi.mocked(isDesktopApp).mockReturnValue(true);
    vi.mocked(desktopGetVersion).mockResolvedValue("2.0.0");
    vi.mocked(daemonFetchRaw).mockResolvedValue({
      status: 200,
      body: JSON.stringify({
        ...baseHealth,
        daemon_version: "1.0.0",
        compatibility: { ...baseHealth.compatibility, desktop_exact_version: "1.0.0" },
      }),
      content_type: "application/json",
    });

    renderOverlay();
    expect(await screen.findByText("Desktop and daemon are out of sync")).toBeInTheDocument();
    expect(screen.getByText(/local daemon is older than this desktop app/i)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Restart local daemon" })).toBeInTheDocument();
  });

  it("shows mismatch when the desktop app is older than the daemon", async () => {
    vi.mocked(isDesktopApp).mockReturnValue(true);
    vi.mocked(desktopGetVersion).mockResolvedValue("1.0.0");
    vi.mocked(daemonFetchRaw).mockResolvedValue({
      status: 200,
      body: JSON.stringify({
        ...baseHealth,
        daemon_version: "2.0.0",
        compatibility: { ...baseHealth.compatibility, desktop_exact_version: "2.0.0" },
      }),
      content_type: "application/json",
    });

    renderOverlay();
    expect(await screen.findByText("Desktop and daemon are out of sync")).toBeInTheDocument();
    expect(screen.getByText(/desktop app is older than the daemon/i)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Update desktop app" })).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Open diagnostics" })).toBeInTheDocument();
  });

  it("can trigger in-place desktop update from desktop-older mismatch", async () => {
    vi.mocked(isDesktopApp).mockReturnValue(true);
    vi.mocked(desktopGetVersion).mockResolvedValue("1.0.0");
    vi.mocked(daemonFetchRaw).mockResolvedValue({
      status: 200,
      body: JSON.stringify({
        ...baseHealth,
        daemon_version: "2.0.0",
        compatibility: { ...baseHealth.compatibility, desktop_exact_version: "2.0.0" },
      }),
      content_type: "application/json",
    });

    renderOverlay();
    expect(await screen.findByRole("button", { name: "Update desktop app" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Update desktop app" }));
    await waitFor(() => {
      expect(vi.mocked(desktopApplyAppUpdate)).toHaveBeenCalledWith();
    });
    expect(
      await screen.findByText(/restart the desktop app to apply the update/i),
    ).toBeInTheDocument();
  });

  it("shows SSH-specific update action for daemon older mismatch", async () => {
    vi.mocked(isDesktopApp).mockReturnValue(true);
    vi.mocked(syncDesktopDaemonConnectionFromBridge).mockResolvedValue(
      makeDesktopSyncResult({ kind: "ssh" }),
    );
    vi.mocked(desktopGetVersion).mockResolvedValue("2.0.0");
    vi.mocked(daemonFetchRaw).mockResolvedValue({
      status: 200,
      body: JSON.stringify({
        ...baseHealth,
        daemon_version: "1.0.0",
        compatibility: { ...baseHealth.compatibility, desktop_exact_version: "1.0.0" },
      }),
      content_type: "application/json",
    });

    renderOverlay();
    expect(await screen.findByRole("button", { name: "Restart now" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Restart now" }));
    await waitFor(() => {
      expect(vi.mocked(desktopUpdateRemoteDaemon)).toHaveBeenCalledWith();
    });
  });

  it("prompts before restart-now remote daemon update", async () => {
    vi.mocked(isDesktopApp).mockReturnValue(true);
    vi.mocked(syncDesktopDaemonConnectionFromBridge).mockResolvedValue(
      makeDesktopSyncResult({ kind: "ssh" }),
    );
    vi.mocked(desktopGetVersion).mockResolvedValue("2.0.0");
    vi.spyOn(window, "confirm").mockReturnValue(false);
    vi.mocked(daemonFetchRaw).mockResolvedValue({
      status: 200,
      body: JSON.stringify({
        ...baseHealth,
        daemon_version: "1.0.0",
        compatibility: { ...baseHealth.compatibility, desktop_exact_version: "1.0.0" },
      }),
      content_type: "application/json",
    });

    renderOverlay();
    expect(await screen.findByRole("button", { name: "Restart now" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Restart now" }));
    await waitFor(() => {
      expect(vi.mocked(desktopUpdateRemoteDaemon)).not.toHaveBeenCalled();
    });
  });

  it("runs remote daemon update as a single flight across rapid clicks", async () => {
    vi.mocked(isDesktopApp).mockReturnValue(true);
    vi.mocked(syncDesktopDaemonConnectionFromBridge).mockResolvedValue(
      makeDesktopSyncResult({ kind: "ssh" }),
    );
    vi.mocked(desktopGetVersion).mockResolvedValue("2.0.0");
    vi.mocked(daemonFetchRaw).mockResolvedValue({
      status: 200,
      body: JSON.stringify({
        ...baseHealth,
        daemon_version: "1.0.0",
        compatibility: { ...baseHealth.compatibility, desktop_exact_version: "1.0.0" },
      }),
      content_type: "application/json",
    });
    let releaseGate!: () => void;
    const updateGate = new Promise<void>((resolve) => {
      releaseGate = () => resolve();
    });
    vi.mocked(desktopUpdateRemoteDaemon).mockImplementation(async () => {
      await updateGate;
      return { updated: true, message: "ok" };
    });

    renderOverlay();
    const button = await screen.findByRole("button", { name: "Restart now" });
    fireEvent.click(button);
    fireEvent.click(button);
    expect(vi.mocked(desktopUpdateRemoteDaemon)).toHaveBeenCalledTimes(1);
    releaseGate();

    await waitFor(() => {
      expect(vi.mocked(desktopUpdateRemoteDaemon)).toHaveBeenCalledTimes(1);
    });
  });

  it("shows waiting-for-idle state for a pending remote daemon update", async () => {
    vi.mocked(isDesktopApp).mockReturnValue(true);
    vi.mocked(syncDesktopDaemonConnectionFromBridge).mockResolvedValue(
      makeDesktopSyncResult({
        kind: "ssh",
        remote_update_state: "pending",
        remote_update_message:
          "Remote daemon update is queued and will restart automatically when no turns are queued or running.",
      }),
    );
    vi.mocked(desktopGetVersion).mockResolvedValue("2.0.0");
    vi.mocked(daemonFetchRaw).mockResolvedValue({
      status: 200,
      body: JSON.stringify({
        ...baseHealth,
        daemon_version: "1.0.0",
        compatibility: { ...baseHealth.compatibility, desktop_exact_version: "1.0.0" },
      }),
      content_type: "application/json",
    });

    renderOverlay();
    expect(await screen.findByRole("button", { name: "Waiting for idle..." })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Restart now" })).toBeInTheDocument();
    expect(
      screen.getByText(
        "Remote daemon update is queued and will restart automatically when no turns are queued or running.",
      ),
    ).toBeInTheDocument();
  });

  it("shows failed pending remote update details from desktop bridge state", async () => {
    vi.mocked(isDesktopApp).mockReturnValue(true);
    vi.mocked(syncDesktopDaemonConnectionFromBridge).mockResolvedValue(
      makeDesktopSyncResult({
        kind: "ssh",
        remote_update_state: "failed",
        remote_update_message: "remote daemon update failed",
      }),
    );
    vi.mocked(desktopGetVersion).mockResolvedValue("2.0.0");
    vi.mocked(daemonFetchRaw).mockResolvedValue({
      status: 200,
      body: JSON.stringify({
        ...baseHealth,
        daemon_version: "1.0.0",
        compatibility: { ...baseHealth.compatibility, desktop_exact_version: "1.0.0" },
      }),
      content_type: "application/json",
    });

    renderOverlay();
    expect(await screen.findByText("remote daemon update failed")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Restart now" })).toBeInTheDocument();
  });

  it("runs local daemon restart as a single flight across rapid clicks", async () => {
    vi.mocked(isDesktopApp).mockReturnValue(true);
    vi.mocked(syncDesktopDaemonConnectionFromBridge).mockResolvedValue(
      makeDesktopSyncResult({ kind: "local" }),
    );
    vi.mocked(desktopGetVersion).mockResolvedValue("2.0.0");
    vi.mocked(daemonFetchRaw).mockResolvedValue({
      status: 200,
      body: JSON.stringify({
        ...baseHealth,
        daemon_version: "1.0.0",
        compatibility: { ...baseHealth.compatibility, desktop_exact_version: "1.0.0" },
      }),
      content_type: "application/json",
    });
    let releaseGate!: () => void;
    const restartGate = new Promise<void>((resolve) => {
      releaseGate = () => resolve();
    });
    vi.mocked(desktopRestartLocalDaemon).mockImplementation(async () => {
      await restartGate;
      return { kind: "local" };
    });

    renderOverlay();
    const button = await screen.findByRole("button", { name: "Restart local daemon" });
    fireEvent.click(button);
    fireEvent.click(button);
    expect(vi.mocked(desktopRestartLocalDaemon)).toHaveBeenCalledTimes(1);
    releaseGate();

    await waitFor(() => {
      expect(vi.mocked(desktopRestartLocalDaemon)).toHaveBeenCalledTimes(1);
    });
  });

  it("normalizes desktop version prefixes before comparing", async () => {
    vi.mocked(isDesktopApp).mockReturnValue(true);
    vi.mocked(desktopGetVersion).mockResolvedValue("v1.2.3");
    vi.mocked(daemonFetchRaw).mockResolvedValue({
      status: 200,
      body: JSON.stringify({
        ...baseHealth,
        daemon_version: "1.2.3",
        compatibility: { ...baseHealth.compatibility, desktop_exact_version: "1.2.3" },
      }),
      content_type: "application/json",
    });

    const { container } = renderOverlay();
    await waitFor(() => {
      expect(vi.mocked(daemonFetchRaw)).toHaveBeenCalled();
    });
    expect(container.firstChild).toBeNull();
  });

  it("uses non-desktop copy when running in the browser", async () => {
    vi.mocked(isDesktopApp).mockReturnValue(false);
    vi.mocked(daemonFetchRaw).mockResolvedValue({
      status: 503,
      body: JSON.stringify({ error: "nope" }),
      content_type: "application/json",
    });

    renderOverlay();
    expect(await screen.findByText("ctx daemon unavailable")).toBeInTheDocument();
    expect(
      screen.getByText("The daemon is not reachable. Start it, then retry this screen."),
    ).toBeInTheDocument();
    expect(screen.queryByText("Open launcher")).not.toBeInTheDocument();
  });

  it("polls while the workspace setup route suppresses ordinary availability overlays", async () => {
    vi.mocked(isDesktopApp).mockReturnValue(true);
    vi.mocked(desktopGetVersion).mockResolvedValue("2.0.0");
    vi.mocked(daemonFetchRaw).mockResolvedValue({
      status: 503,
      body: JSON.stringify({ error: "unavailable" }),
      content_type: "application/json",
    });

    renderOverlay("/workspace-setup");

    await waitFor(() => {
      expect(vi.mocked(daemonFetchRaw)).toHaveBeenCalledWith(
        "/api/health",
        undefined,
        expect.objectContaining({ connectLocalWhenMissing: false }),
      );
    });
    expect(vi.mocked(syncDesktopDaemonConnectionFromBridge)).toHaveBeenCalled();
    expect(screen.queryByText("ctx daemon unavailable")).not.toBeInTheDocument();
  });

  it("does not poll daemon availability while the geometry harness route suppresses the overlay", async () => {
    vi.mocked(isDesktopApp).mockReturnValue(true);
    vi.mocked(desktopGetVersion).mockResolvedValue("2.0.0");
    vi.mocked(daemonFetchRaw).mockResolvedValue({
      status: 200,
      body: JSON.stringify(baseHealth),
      content_type: "application/json",
    });

    renderOverlay("/__geometry_harness");
    await new Promise((resolve) => window.setTimeout(resolve, 25));

    expect(vi.mocked(daemonFetchRaw)).not.toHaveBeenCalled();
    expect(vi.mocked(syncDesktopDaemonConnectionFromBridge)).not.toHaveBeenCalled();
    expect(screen.queryByText("ctx daemon unavailable")).not.toBeInTheDocument();
  });
});
