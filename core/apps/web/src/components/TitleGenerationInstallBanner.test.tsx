import React from "react";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { TitleGenerationInstallBanner } from "./TitleGenerationInstallBanner";
import { getTitleGenerationLocalStatus, type TitleGenerationLocalStatus } from "../api/client";

type InstallSnapshot = Record<string, {
  installId: string;
  providerId: string | null;
  state: "running" | "failed" | "succeeded" | "cancelled";
  pct: number | null;
  target?: "host" | "container" | "linux-aarch64" | "linux-x86_64";
  errorCode?: string;
  error?: string;
  lastEvent: {
    install_id: string;
    provider_id: string;
    at: string;
    stage: string;
    message: string;
    level: "info" | "warning" | "error" | "success";
    bytes?: number;
    total_bytes?: number;
    error_code?: string;
  } | null;
  events: Array<{
    install_id: string;
    provider_id: string;
    at: string;
    stage: string;
    message: string;
    level: "info" | "warning" | "error" | "success";
    bytes?: number;
    total_bytes?: number;
    error_code?: string;
  }>;
  historyLoaded: boolean;
  updatedAtMs: number;
}>;

type InstallListener = (snapshot: InstallSnapshot) => void;

const installMonitorState = vi.hoisted(() => ({
  snapshot: {} as InstallSnapshot,
  listeners: new Set<InstallListener>(),
}));

const observeInstallMock = vi.hoisted(() => vi.fn(() => () => {}));

const emitInstallSnapshot = (snapshot: InstallSnapshot) => {
  installMonitorState.snapshot = snapshot;
  for (const listener of installMonitorState.listeners) {
    listener(snapshot);
  }
};

vi.mock("../api/client", () => ({
  getTitleGenerationLocalStatus: vi.fn(),
}));

vi.mock("../state/installProgressMonitor", () => ({
  observeInstall: observeInstallMock,
  subscribeInstallProgress: (listener: InstallListener) => {
    installMonitorState.listeners.add(listener);
    listener(installMonitorState.snapshot);
    return () => {
      installMonitorState.listeners.delete(listener);
    };
  },
}));

const buildLocalStatus = (overrides?: Partial<TitleGenerationLocalStatus>): TitleGenerationLocalStatus => ({
  ready: false,
  runtime: {
    version: "1.0.0",
    installed: true,
    path: "/tmp/runtime",
  },
  model: {
    model_id: "ggml-org/Qwen3-1.7B-GGUF",
    file_name: "model.gguf",
    installed: false,
    version: null,
    sha256: null,
    size_bytes: null,
    installed_at: null,
  },
  install_id: "install-1",
  install_running: true,
  ...overrides,
});

const runningInstallSnapshot: InstallSnapshot = {
  "install-1": {
    installId: "install-1",
    providerId: "title_generation_local",
    state: "running",
    pct: 40,
    lastEvent: {
      install_id: "install-1",
      provider_id: "title_generation_local",
      at: "2026-02-20T00:00:01Z",
      stage: "download_model",
      message: "Downloading model file",
      level: "info",
      bytes: 4,
      total_bytes: 10,
    },
    events: [
      {
        install_id: "install-1",
        provider_id: "title_generation_local",
        at: "2026-02-20T00:00:01Z",
        stage: "download_model",
        message: "Downloading model file",
        level: "info",
        bytes: 4,
        total_bytes: 10,
      },
    ],
    historyLoaded: true,
    updatedAtMs: 1,
  },
};

describe("TitleGenerationInstallBanner", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    localStorage.clear();
    installMonitorState.listeners.clear();
    emitInstallSnapshot({});
  });

  it("renders running local titling download progress from the shared install monitor", async () => {
    vi.mocked(getTitleGenerationLocalStatus).mockResolvedValue(buildLocalStatus());
    emitInstallSnapshot(runningInstallSnapshot);

    render(<TitleGenerationInstallBanner />);

    expect(await screen.findByText("Session titling model download in progress.")).toBeInTheDocument();
    expect(await screen.findByText("Downloading… 40%")).toBeInTheDocument();
    expect(screen.getByRole("progressbar", { name: "Session titling install progress" })).toHaveAttribute("aria-valuenow", "40");
    expect(observeInstallMock).toHaveBeenCalledWith(
      "install-1",
      expect.objectContaining({
        loadHistory: true,
      }),
    );
  });

  it("persists dismissal per install id across remount", async () => {
    vi.mocked(getTitleGenerationLocalStatus).mockResolvedValue(buildLocalStatus());
    emitInstallSnapshot(runningInstallSnapshot);

    const rendered = render(<TitleGenerationInstallBanner />);
    expect(await screen.findByText("Session titling model download in progress.")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Dismiss" }));
    await waitFor(() => {
      expect(screen.queryByText("Session titling model download in progress.")).not.toBeInTheDocument();
    });

    rendered.unmount();
    render(<TitleGenerationInstallBanner />);

    await waitFor(() => {
      expect(screen.queryByText("Session titling model download in progress.")).not.toBeInTheDocument();
    });
  });
});
