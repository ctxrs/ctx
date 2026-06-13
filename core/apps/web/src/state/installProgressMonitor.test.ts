import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { type InstallInfo } from "../api/client";
import {
  clearInstallProgress,
  getInstallProgressSnapshot,
  observeInstall,
} from "./installProgressMonitor";
import {
  clearProviderInstallProgress,
  getProviderInstallProgressSnapshot,
  resolveProviderInstallProgressSession,
} from "./providerInstallProgressStore";
import {
  getInstallStatuses,
  listInstallEvents,
} from "../api/client";

vi.mock("../api/client", async (importOriginal) => {
  const original = await importOriginal<typeof import("../api/client")>();
  return {
    ...original,
    getInstallStatuses: vi.fn(),
    listInstallEvents: vi.fn(),
  };
});

const buildInstallInfo = (
  installId: string,
  providerId: string,
  overrides?: Partial<InstallInfo>,
): InstallInfo => ({
  install_id: installId,
  provider_id: providerId,
  state: "running",
  started_at: "2026-03-09T00:00:00Z",
  last_event: {
    install_id: installId,
    provider_id: providerId,
    at: "2026-03-09T00:00:01Z",
    stage: "download",
    message: `Downloading ${providerId}`,
    level: "info",
    bytes: 4,
    total_bytes: 10,
  },
  ...overrides,
});

describe("installProgressMonitor", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.clearAllMocks();
    clearInstallProgress();
    clearProviderInstallProgress();
  });

  afterEach(() => {
    clearInstallProgress();
    clearProviderInstallProgress();
    vi.useRealTimers();
  });

  it("batches concurrent observed installs into one poll request per tick", async () => {
    vi.mocked(getInstallStatuses).mockResolvedValue({
      installs: [
        {
          install_id: "install-1",
          info: buildInstallInfo("install-1", "codex"),
        },
        {
          install_id: "install-2",
          info: buildInstallInfo("install-2", "claude-crp"),
        },
      ],
    });

    const stopCodex = observeInstall("install-1", { providerId: "codex" });
    const stopClaude = observeInstall("install-2", { providerId: "claude-crp" });

    await vi.advanceTimersByTimeAsync(0);
    expect(getInstallStatuses).toHaveBeenCalledTimes(1);
    expect(getInstallStatuses).toHaveBeenNthCalledWith(1, ["install-1", "install-2"]);

    await vi.advanceTimersByTimeAsync(900);
    expect(getInstallStatuses).toHaveBeenCalledTimes(2);
    expect(getInstallStatuses).toHaveBeenNthCalledWith(2, ["install-1", "install-2"]);

    stopCodex();
    stopClaude();
  });

  it("loads history once and mirrors provider aliases into the provider progress store", async () => {
    vi.mocked(listInstallEvents).mockResolvedValue([
      {
        install_id: "install-1",
        provider_id: "codex",
        at: "2026-03-09T00:00:00Z",
        stage: "download",
        message: "Preparing download",
        level: "info",
      },
    ]);
    vi.mocked(getInstallStatuses).mockResolvedValue({
      installs: [
        {
          install_id: "install-1",
          info: buildInstallInfo("install-1", "codex", {
            last_event: {
              install_id: "install-1",
              provider_id: "codex",
              at: "2026-03-09T00:00:02Z",
              stage: "download",
              message: "Downloading codex",
              level: "info",
              bytes: 6,
              total_bytes: 10,
            },
          }),
        },
      ],
    });

    const stop = observeInstall("install-1", {
      providerId: "codex",
      loadHistory: true,
      initialState: { state: "running" },
    });

    await vi.advanceTimersByTimeAsync(0);

    expect(listInstallEvents).toHaveBeenCalledTimes(1);
    expect(resolveProviderInstallProgressSession(getProviderInstallProgressSnapshot(), "codex")?.installId).toBe("install-1");

    const snapshot = getInstallProgressSnapshot();
    expect(snapshot["install-1"]?.historyLoaded).toBe(true);
    expect(snapshot["install-1"]?.events).toHaveLength(2);

    stop();

    expect(resolveProviderInstallProgressSession(getProviderInstallProgressSnapshot(), "codex")).toBeUndefined();
  });

  it("treats info-null status rows as terminal after the daemon loses install state", async () => {
    vi.mocked(getInstallStatuses)
      .mockResolvedValueOnce({
        installs: [
          {
            install_id: "install-1",
            info: buildInstallInfo("install-1", "codex"),
          },
        ],
      })
      .mockResolvedValueOnce({
        installs: [
          {
            install_id: "install-1",
            info: null,
          },
        ],
      });

    const stop = observeInstall("install-1", {
      providerId: "codex",
      initialState: { state: "running" },
    });

    await vi.advanceTimersByTimeAsync(0);

    expect(getInstallProgressSnapshot()["install-1"]).toMatchObject({
      state: "running",
      pct: 30,
      errorCode: undefined,
      error: undefined,
    });
    expect(resolveProviderInstallProgressSession(getProviderInstallProgressSnapshot(), "codex")).toMatchObject({
      installId: "install-1",
      state: "running",
      pct: 30,
    });

    await vi.advanceTimersByTimeAsync(900);

    const snapshot = getInstallProgressSnapshot();
    expect(snapshot["install-1"]).toMatchObject({
      state: "failed",
      pct: 30,
      errorCode: "unknown",
      error: "Install is no longer tracked by the daemon. Retry from this screen.",
    });
    expect(snapshot["install-1"]?.lastEvent?.message).toBe("Downloading codex");
    expect(snapshot["install-1"]?.events).toHaveLength(1);
    expect(resolveProviderInstallProgressSession(getProviderInstallProgressSnapshot(), "codex")).toMatchObject({
      installId: "install-1",
      state: "failed",
      pct: 30,
      errorCode: "unknown",
      error: "Install is no longer tracked by the daemon. Retry from this screen.",
    });

    await vi.advanceTimersByTimeAsync(900);
    expect(getInstallStatuses).toHaveBeenCalledTimes(2);

    stop();
  });
});
