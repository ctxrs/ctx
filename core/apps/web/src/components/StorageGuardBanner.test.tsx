import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { beforeEach, describe, expect, it, vi } from "vitest";
import StorageGuardBanner from "./StorageGuardBanner";
import { getHealth } from "../api/client";

vi.mock("../api/client", async (importOriginal) => {
  const original = await importOriginal<typeof import("../api/client")>();
  return {
    ...original,
    getHealth: vi.fn(),
  };
});

const baseHealth = {
  version: "1.0.0",
  daemon_version: "1.0.0",
  pid: 1,
  data_root: "/tmp/ctx",
  daemon_url: "http://127.0.0.1:4399",
  auth_required: false,
  compatibility: {
    desktop_exact_version: "1.0.0",
    desktop_build_id: "build-1",
    desktop_dev_instance_id: "dev-instance-1",
    mobile_api_min: 1,
    mobile_api_max: 1,
  },
  storage: {
    level: "normal" as const,
    warning_threshold_bytes: 2 * 1024 * 1024 * 1024,
    emergency_threshold_bytes: 1024 * 1024 * 1024,
    reserve_bytes: 512 * 1024 * 1024,
    reserve_file_active: true,
    updated_at: "2026-03-15T00:00:00Z",
    active: null,
  },
};

const renderBanner = (route = "/") =>
  render(
    <MemoryRouter initialEntries={[route]} future={{ v7_startTransition: true, v7_relativeSplatPath: true }}>
      <Routes>
        <Route path="/__geometry_harness" element={<StorageGuardBanner />} />
        <Route path="/" element={<StorageGuardBanner />} />
        <Route path="/diagnostics" element={<div>Diagnostics page</div>} />
      </Routes>
    </MemoryRouter>,
  );

describe("StorageGuardBanner", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(getHealth).mockResolvedValue(baseHealth);
  });

  it("renders warning copy when free space falls below the warning threshold", async () => {
    vi.mocked(getHealth).mockResolvedValue({
      ...baseHealth,
      storage: {
        ...baseHealth.storage,
        level: "warning",
        active: {
          label: "CTX data root",
          path: "/tmp/ctx",
          mount_point: "/",
          free_bytes: Math.round(1.8 * 1024 * 1024 * 1024),
          total_bytes: 100 * 1024 * 1024 * 1024,
        },
      },
    });

    renderBanner();

    expect(await screen.findByTestId("storage-guard-snackbar")).toBeInTheDocument();
    expect(screen.getByText("Storage is getting low.")).toBeInTheDocument();
    expect(screen.getByText(/1.8 GiB left on CTX data root/)).toBeInTheDocument();
  });

  it("renders emergency copy and navigates to diagnostics", async () => {
    vi.mocked(getHealth).mockResolvedValue({
      ...baseHealth,
      storage: {
        ...baseHealth.storage,
        level: "emergency",
        reserve_file_active: false,
        active: {
          label: "active worktree",
          path: "/Volumes/work/repo",
          mount_point: "/Volumes/work",
          free_bytes: Math.round(0.9 * 1024 * 1024 * 1024),
          total_bytes: 100 * 1024 * 1024 * 1024,
        },
      },
    });

    renderBanner();

    expect(await screen.findByText("Storage emergency. Active agent sessions were interrupted.")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Open Diagnostics" }));
    await waitFor(() => {
      expect(screen.getByText("Diagnostics page")).toBeInTheDocument();
    });
  });

  it("dismisses the current banner state", async () => {
    vi.mocked(getHealth).mockResolvedValue({
      ...baseHealth,
      storage: {
        ...baseHealth.storage,
        level: "warning",
        active: {
          label: "temp storage",
          path: "/tmp",
          mount_point: "/",
          free_bytes: Math.round(1.9 * 1024 * 1024 * 1024),
          total_bytes: 100 * 1024 * 1024 * 1024,
        },
      },
    });

    renderBanner();

    expect(await screen.findByTestId("storage-guard-snackbar")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Dismiss storage notice" }));
    await waitFor(() => {
      expect(screen.queryByTestId("storage-guard-snackbar")).not.toBeInTheDocument();
    });
  });

  it("does not poll storage health on the geometry harness route", async () => {
    renderBanner("/__geometry_harness");
    await new Promise((resolve) => window.setTimeout(resolve, 25));
    expect(vi.mocked(getHealth)).not.toHaveBeenCalled();
    expect(screen.queryByTestId("storage-guard-snackbar")).not.toBeInTheDocument();
  });
});
