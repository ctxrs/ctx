import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { beforeEach, describe, expect, it, vi } from "vitest";
import DiagnosticsPage from "./DiagnosticsPageController";
import {
  applyDaemonDesktopConnection,
  appendDesktopLog,
  checkUpdates,
  getDiagnostics,
} from "../../api/client";
import type { Diagnostics } from "../../api/client";
import {
  desktopApplyAppUpdate,
  desktopCheckAppUpdate,
  desktopGetConnection,
  desktopGetLastAppUpdateAttempt,
  desktopRestartLocalDaemon,
  type DesktopAppUpdateApplyResp,
  type DesktopAppUpdateCheckResp,
  getDesktopPlatform,
  isDesktopApp,
  openExternalLink,
} from "../../utils/desktop";
import {
  appendDownloadAttributionIdToUrl,
  clearPendingDownloadAttributionId,
  createDownloadAttributionId,
  setPendingDownloadAttributionId,
} from "../../utils/analytics";

vi.mock("../../api/client", () => ({
  applyDaemonDesktopConnection: vi.fn(),
  appendDesktopLog: vi.fn(),
  applyAppImageUpdate: vi.fn(),
  checkUpdates: vi.fn(),
  downloadAppImageUpdate: vi.fn(),
  getDiagnostics: vi.fn(),
  openLogsFolder: vi.fn(),
}));

vi.mock("../../utils/desktop", () => ({
  desktopApplyAppUpdate: vi.fn(),
  desktopCheckAppUpdate: vi.fn(),
  desktopGetConnection: vi.fn(),
  desktopGetLastAppUpdateAttempt: vi.fn(),
  desktopRestartLocalDaemon: vi.fn(),
  desktopUpdateRemoteDaemon: vi.fn(),
  getDesktopPlatform: vi.fn(),
  isDesktopApp: vi.fn(),
  openExternalLink: vi.fn(),
}));

vi.mock("../../utils/analytics", () => ({
  appendDownloadAttributionIdToUrl: vi.fn((href: string, downloadId: string) =>
    `${href}${href.includes("?") ? "&" : "?"}ctx_download_id=${downloadId}`),
  clearPendingDownloadAttributionId: vi.fn(async () => {}),
  createDownloadAttributionId: vi.fn(() => "download-test-id"),
  setPendingDownloadAttributionId: vi.fn(async () => true),
}));

const renderPage = (route = "/diagnostics") =>
  render(
    <MemoryRouter
      initialEntries={[route]}
      future={{ v7_startTransition: true, v7_relativeSplatPath: true }}
    >
      <DiagnosticsPage />
    </MemoryRouter>,
  );

const makeDesktopCheckResp = (
  overrides: Partial<DesktopAppUpdateCheckResp> = {},
): DesktopAppUpdateCheckResp => ({
  configured: false,
  available: false,
  current_version: "1.0.0",
  latest_version: null,
  target: "windows-x64",
  endpoint: "https://api.example/functions/v1/releases/stable/latest-tauri.json",
  message: "Native updater is not configured (missing CTX_DESKTOP_UPDATER_PUBKEY).",
  phase: "idle",
  restart_required: false,
  staged: false,
  ...overrides,
});

const makeDesktopApplyResp = (
  overrides: Partial<DesktopAppUpdateApplyResp> = {},
): DesktopAppUpdateApplyResp => ({
  applied: true,
  latest_version: "1.0.1",
  message: "Update takes ~1 second and preserves data. Active agents will be paused.",
  needs_restart: true,
  up_to_date: false,
  ...overrides,
});

describe("DiagnosticsPage updates", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(createDownloadAttributionId).mockReturnValue("download-test-id");
    vi.mocked(appendDownloadAttributionIdToUrl).mockImplementation((href: string, downloadId: string) =>
      `${href}${href.includes("?") ? "&" : "?"}ctx_download_id=${downloadId}`);
    vi.mocked(setPendingDownloadAttributionId).mockResolvedValue(true);
    vi.mocked(clearPendingDownloadAttributionId).mockResolvedValue();
    vi.mocked(appendDesktopLog).mockResolvedValue(undefined);
    const diagnostics: Diagnostics = {
      daemon: {
        version: "1.0.0",
        daemon_version: "1.0.0",
        pid: 123,
        daemon_url: "http://127.0.0.1:4399",
        data_root: "/tmp/ctx",
        auth_required: false,
        compatibility: {
          desktop_exact_version: "1.0.0",
          mobile_api_min: 1,
          mobile_api_max: 1,
        },
      },
      platform: { os: "linux", arch: "x86_64" },
      logs: { dir: "/tmp/ctx/logs", files: [] },
      providers: [],
      managed_installs: {},
    };
    vi.mocked(getDiagnostics).mockResolvedValue(diagnostics);
    vi.mocked(isDesktopApp).mockReturnValue(true);
    vi.mocked(desktopGetConnection).mockResolvedValue({ kind: "local" });
    vi.mocked(getDesktopPlatform).mockResolvedValue("windows");
    vi.mocked(openExternalLink).mockResolvedValue(true);
    vi.mocked(desktopGetLastAppUpdateAttempt).mockResolvedValue(null);
    vi.mocked(desktopCheckAppUpdate).mockResolvedValue(makeDesktopCheckResp());
    vi.mocked(desktopApplyAppUpdate).mockResolvedValue(makeDesktopApplyResp());
  });

  it("opens desktop artifact URL for non-linux platforms", async () => {
    vi.mocked(checkUpdates).mockResolvedValue({
      channel: "stable",
      base_url: "https://api.example/functions/v1",
      platform: "windows-x64",
      current_version: "1.0.0",
      latest_version: "1.0.1",
      update_available: true,
      platform_supported: true,
      manifest: {
        platforms: {
          "windows-x64": {
            nsis: {
              url_path: "/download/stable/1.0.1/ctx_1.0.1_windows-x64.exe",
              sha256: "abc",
            },
          },
        },
      },
    });

    renderPage();
    fireEvent.click(await screen.findByRole("button", { name: "Check updates" }));

    const openButton = await screen.findByRole("button", { name: "Open latest desktop download" });
    fireEvent.click(openButton);

    await waitFor(() => {
      expect(openExternalLink).toHaveBeenCalledTimes(1);
      const [openedUrl] = vi.mocked(openExternalLink).mock.calls[0] ?? [];
      expect(typeof openedUrl).toBe("string");
      expect(String(openedUrl)).toContain(
        "https://api.example/functions/v1/download/stable/1.0.1/ctx_1.0.1_windows-x64.exe",
      );
      expect(String(openedUrl)).toContain("ctx_download_id=");
    });
    expect(setPendingDownloadAttributionId).toHaveBeenCalledTimes(1);
  });

  it("does not persist pending attribution id in non-desktop sessions", async () => {
    vi.mocked(isDesktopApp).mockReturnValue(false);
    vi.mocked(checkUpdates).mockResolvedValue({
      channel: "stable",
      base_url: "https://api.example/functions/v1",
      platform: "windows-x64",
      current_version: "1.0.0",
      latest_version: "1.0.1",
      update_available: true,
      platform_supported: true,
      manifest: {
        platforms: {
          "windows-x64": {
            nsis: {
              url_path: "/download/stable/1.0.1/ctx_1.0.1_windows-x64.exe",
              sha256: "abc",
            },
          },
        },
      },
    });

    renderPage();
    fireEvent.click(await screen.findByRole("button", { name: "Check updates" }));
    fireEvent.click(await screen.findByRole("button", { name: "Open latest desktop download" }));

    await waitFor(() => {
      expect(openExternalLink).toHaveBeenCalledTimes(1);
    });
    expect(setPendingDownloadAttributionId).not.toHaveBeenCalled();
  });

  it("shows unsupported-platform notice when no desktop artifact exists", async () => {
    vi.mocked(checkUpdates).mockResolvedValue({
      channel: "stable",
      base_url: "https://api.example/functions/v1",
      platform: "windows-x64",
      current_version: "1.0.0",
      latest_version: "1.0.1",
      update_available: false,
      platform_supported: false,
      manifest: { platforms: {} },
    });

    renderPage();
    fireEvent.click(await screen.findByRole("button", { name: "Check updates" }));

    expect(
      await screen.findByText(
        "Update metadata found, but this platform currently has no desktop artifact.",
      ),
    ).toBeInTheDocument();
  });

  it("shows Linux AppImage controls when the platform is linux", async () => {
    vi.mocked(checkUpdates).mockResolvedValue({
      channel: "stable",
      base_url: "https://api.example/functions/v1",
      platform: "linux-x64",
      current_version: "1.0.0",
      latest_version: "1.0.1",
      update_available: true,
      platform_supported: true,
      manifest: {
        platforms: {
          "linux-x64": {
            appimage: {
              url_path: "/download/stable/1.0.1/ctx_1.0.1_amd64.AppImage",
              sha256: "abc",
            },
          },
        },
      },
    });

    renderPage();
    fireEvent.click(await screen.findByRole("button", { name: "Check updates" }));

    expect(await screen.findByRole("button", { name: "Download AppImage update" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Open latest desktop download" })).not.toBeInTheDocument();
  });

  it("uses native updater action when configured for non-linux platforms", async () => {
    vi.mocked(desktopCheckAppUpdate).mockResolvedValue(
      makeDesktopCheckResp({
        configured: true,
        available: true,
        latest_version: "1.0.1",
        message: null,
      }),
    );
    vi.mocked(checkUpdates).mockResolvedValue({
      channel: "stable",
      base_url: "https://api.example/functions/v1",
      platform: "windows-x64",
      current_version: "1.0.0",
      latest_version: "1.0.1",
      update_available: true,
      platform_supported: true,
      manifest: {
        platforms: {
          "windows-x64": {
            nsis: {
              url_path: "/download/stable/1.0.1/ctx_1.0.1_windows-x64.exe",
              sha256: "abc",
            },
          },
        },
      },
    });

    renderPage();
    fireEvent.click(await screen.findByRole("button", { name: "Check updates" }));

    const installButton = await screen.findByRole("button", { name: "Install desktop update" });
    fireEvent.click(installButton);

    await waitFor(() => {
      expect(desktopApplyAppUpdate).toHaveBeenCalledWith({ downloadId: expect.any(String) });
    });
  });

  it("clears pending attribution when native updater does not apply an update", async () => {
    vi.mocked(desktopApplyAppUpdate).mockResolvedValue(
      makeDesktopApplyResp({
        applied: false,
        latest_version: null,
        message: "No desktop app update is currently available.",
        needs_restart: false,
        up_to_date: true,
      }),
    );
    vi.mocked(desktopCheckAppUpdate).mockResolvedValue(
      makeDesktopCheckResp({
        configured: true,
        available: true,
        latest_version: "1.0.1",
        message: null,
      }),
    );
    vi.mocked(checkUpdates).mockResolvedValue({
      channel: "stable",
      base_url: "https://api.example/functions/v1",
      platform: "windows-x64",
      current_version: "1.0.0",
      latest_version: "1.0.1",
      update_available: true,
      platform_supported: true,
      manifest: {
        platforms: {
          "windows-x64": {
            nsis: {
              url_path: "/download/stable/1.0.1/ctx_1.0.1_windows-x64.exe",
              sha256: "abc",
            },
          },
        },
      },
    });

    renderPage();
    fireEvent.click(await screen.findByRole("button", { name: "Check updates" }));
    fireEvent.click(await screen.findByRole("button", { name: "Install desktop update" }));

    await waitFor(() => {
      expect(clearPendingDownloadAttributionId).toHaveBeenCalledTimes(1);
    });
  });

  it("reapplies daemon client config after local daemon restart", async () => {
    vi.spyOn(window, "confirm").mockReturnValue(true);
    vi.mocked(desktopGetConnection)
      .mockResolvedValueOnce({
        kind: "local",
        base_url: "http://127.0.0.1:4399",
        browser_query_secret: "tok-old",
      })
      .mockResolvedValueOnce({
        kind: "local",
        base_url: "http://127.0.0.1:4401",
        browser_query_secret: "tok-new",
      });
    vi.mocked(desktopRestartLocalDaemon).mockResolvedValue({ kind: "local" });
    vi.mocked(checkUpdates).mockResolvedValue({
      channel: "stable",
      base_url: "https://api.example/functions/v1",
      platform: "windows-x64",
      current_version: "1.0.0",
      latest_version: "1.0.1",
      update_available: false,
      platform_supported: true,
      manifest: { platforms: {} },
    });

    renderPage();
    const restartButton = await screen.findByRole("button", { name: "Restart local daemon" });
    fireEvent.click(restartButton);

    await waitFor(() => {
      expect(desktopRestartLocalDaemon).toHaveBeenCalledTimes(1);
      expect(applyDaemonDesktopConnection).toHaveBeenCalledWith({
        kind: "local",
        base_url: "http://127.0.0.1:4401",
        browser_query_secret: "tok-new",
      });
    });
  });

  it("auto-runs update check when opened with check_updates query", async () => {
    vi.mocked(checkUpdates).mockResolvedValue({
      channel: "stable",
      base_url: "https://api.example/functions/v1",
      platform: "windows-x64",
      current_version: "1.0.0",
      latest_version: "1.0.1",
      update_available: true,
      platform_supported: true,
      manifest: { platforms: {} },
    });

    renderPage("/diagnostics?check_updates=1");

    await waitFor(() => {
      expect(checkUpdates).toHaveBeenCalledTimes(1);
    });
  });
});
