import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ResourceUtilization, Workspace } from "../../../api/client";
import { getResourceUtilization } from "../../../api/client";
import { useSettingsResourceUtilizationController } from "./useSettingsResourceUtilizationController";

vi.mock("../../../api/client", async () => {
  const actual = await vi.importActual<typeof import("../../../api/client")>("../../../api/client");
  return {
    ...actual,
    getResourceUtilization: vi.fn(),
  };
});

const workspaces = [{ id: "workspace-1" }] as Workspace[];

describe("useSettingsResourceUtilizationController", () => {
  const getResourceUtilizationMock = vi.mocked(getResourceUtilization);

  beforeEach(() => {
    vi.useFakeTimers();
    getResourceUtilizationMock.mockReset();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  const flushAsync = async () => {
    await act(async () => {
      await Promise.resolve();
    });
  };

  it("polls only when the resource utilization section is active", async () => {
    getResourceUtilizationMock.mockResolvedValue({ processes: [] } as unknown as ResourceUtilization);
    const { result } = renderHook(() =>
      useSettingsResourceUtilizationController({
        active: "resource_utilization",
        workspaceId: "workspace-1",
        workspaces,
      }),
    );

    await flushAsync();
    expect(getResourceUtilizationMock).toHaveBeenCalledWith("workspace-1");

    await act(async () => {
      vi.advanceTimersByTime(3000);
      await Promise.resolve();
    });

    expect(getResourceUtilizationMock).toHaveBeenCalledTimes(2);

    act(() => {
      result.current.onToggleExpanded(42);
    });
    expect(result.current.expandedProcessPids[42]).toBe(true);
  });

  it("does not start polling for other sections", () => {
    renderHook(() =>
      useSettingsResourceUtilizationController({
        active: "billing",
        workspaceId: "workspace-1",
        workspaces,
      }),
    );

    act(() => {
      vi.advanceTimersByTime(5000);
    });

    expect(getResourceUtilizationMock).not.toHaveBeenCalled();
  });
});
