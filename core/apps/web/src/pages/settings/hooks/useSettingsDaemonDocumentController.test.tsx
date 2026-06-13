import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { getSettings, updateSettings } from "../../../api/client";
import { useSettingsDaemonDocumentController } from "./useSettingsDaemonDocumentController";

vi.mock("../../../api/client", async () => {
  const actual = await vi.importActual<typeof import("../../../api/client")>("../../../api/client");
  return {
    ...actual,
    getSettings: vi.fn(),
    updateSettings: vi.fn(),
  };
});

type SettingsResponse = Awaited<ReturnType<typeof getSettings>>;

const makeSettingsResponse = (overrides: Partial<SettingsResponse> = {}): SettingsResponse => ({
  telemetry: {
    enabled: true,
    endpoint: "",
    source: "configured",
  },
  resource_governance: {
    enabled: true,
    mode: "auto",
    cpu_quota_pct: null,
    memory_high_mb: null,
    memory_max_mb: null,
    effective: null,
    status: null,
  },
  sandboxing: {
    provider_control_mode: "full",
  },
  execution: {
    mode: "host",
    container: {
      network_mode: "llm_only",
      allowlist: [],
      image: null,
      machine: {
        memory_profile: "economy",
        custom_memory_mb: null,
        idle_shutdown_seconds: 3600,
        host_pressure_swap_threshold_mb: 1024,
        target_memory_mb: 4096,
      },
    },
  },
  ...overrides,
});

describe("useSettingsDaemonDocumentController", () => {
  const getSettingsMock = vi.mocked(getSettings);
  const updateSettingsMock = vi.mocked(updateSettings);

  beforeEach(() => {
    vi.useFakeTimers();
    getSettingsMock.mockReset();
    updateSettingsMock.mockReset();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  const flushAsync = async () => {
    await act(async () => {
      await Promise.resolve();
    });
  };

  it("hydrates without immediately saving back the daemon settings document", async () => {
    getSettingsMock.mockResolvedValue(makeSettingsResponse({
      telemetry: { enabled: false, endpoint: "", source: "default" },
    }));

    const { result } = renderHook(() => useSettingsDaemonDocumentController());

    await flushAsync();
    await flushAsync();

    expect(getSettingsMock).toHaveBeenCalledTimes(1);
    expect(result.current.telemetry.enabled).toBe(false);
    expect(result.current.telemetry.source).toBe("default");

    act(() => {
      vi.advanceTimersByTime(1000);
    });

    expect(updateSettingsMock).not.toHaveBeenCalled();
  });

  it("autosaves telemetry changes after hydration", async () => {
    getSettingsMock.mockResolvedValue(makeSettingsResponse());
    updateSettingsMock.mockResolvedValue(makeSettingsResponse({
      telemetry: { enabled: false, endpoint: "", source: "configured" },
    }));

    const { result } = renderHook(() => useSettingsDaemonDocumentController());

    await flushAsync();
    await flushAsync();
    expect(result.current.loaded).toBe(true);
    await flushAsync();

    act(() => {
      result.current.telemetry.setEnabled(false);
    });

    await act(async () => {
      vi.advanceTimersByTime(251);
      await Promise.resolve();
    });

    expect(updateSettingsMock).toHaveBeenCalledWith({
      telemetry: { enabled: false, endpoint: "" },
    });
    expect(result.current.telemetry.source).toBe("configured");
  });

  it("does not save invalid sandbox machine settings", async () => {
    getSettingsMock.mockResolvedValue(makeSettingsResponse());

    const { result } = renderHook(() => useSettingsDaemonDocumentController());

    await flushAsync();
    await flushAsync();
    expect(result.current.loaded).toBe(true);

    act(() => {
      result.current.sandboxing.setMachineIdleShutdownSeconds("59");
      vi.advanceTimersByTime(500);
    });

    expect(updateSettingsMock).not.toHaveBeenCalled();
  });
});
