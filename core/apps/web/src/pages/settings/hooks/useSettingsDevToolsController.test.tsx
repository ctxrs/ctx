import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { devRestartProviders } from "../../../api/client";
import { useSettingsDevToolsController } from "./useSettingsDevToolsController";

vi.mock("../../../api/client", async () => {
  const actual = await vi.importActual<typeof import("../../../api/client")>("../../../api/client");
  return {
    ...actual,
    devRestartProviders: vi.fn(),
  };
});

describe("useSettingsDevToolsController", () => {
  const devRestartProvidersMock = vi.mocked(devRestartProviders);

  beforeEach(() => {
    devRestartProvidersMock.mockReset();
    vi.restoreAllMocks();
  });

  it("requires confirmation before immediate restart", async () => {
    devRestartProvidersMock.mockResolvedValue({ mode: "immediate", results: [] });
    const confirmSpy = vi.spyOn(window, "confirm").mockReturnValue(false);
    const { result } = renderHook(() => useSettingsDevToolsController({ enabled: true }));

    await act(async () => {
      await result.current.onRestart("immediate");
    });

    expect(confirmSpy).toHaveBeenCalledTimes(1);
    expect(devRestartProvidersMock).not.toHaveBeenCalled();
  });

  it("captures restart results for drain restarts", async () => {
    devRestartProvidersMock.mockResolvedValue({
      mode: "drain",
      results: [{ provider_id: "codex", status: "ok", message: "restart_requested" }],
    });
    const { result } = renderHook(() => useSettingsDevToolsController({ enabled: true }));

    await act(async () => {
      await result.current.onRestart("drain");
    });

    await waitFor(() => {
      expect(result.current.restartResults).toEqual([
        { provider_id: "codex", status: "ok", message: "restart_requested" },
      ]);
    });
  });
});
