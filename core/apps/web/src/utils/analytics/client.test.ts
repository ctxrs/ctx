import { beforeEach, describe, expect, it, vi } from "vitest";

const {
  initMock,
  isFeatureEnabledMock,
  optInCapturingMock,
  optOutCapturingMock,
  registerMock,
  recordSemanticTelemetryEventMock,
  setSemanticTelemetryRemoteEnabledMock,
} = vi.hoisted(() => ({
  initMock: vi.fn(),
  isFeatureEnabledMock: vi.fn(() => false),
  optInCapturingMock: vi.fn(),
  optOutCapturingMock: vi.fn(),
  registerMock: vi.fn(),
  recordSemanticTelemetryEventMock: vi.fn(),
  setSemanticTelemetryRemoteEnabledMock: vi.fn(),
}));

vi.mock("posthog-js", () => ({
  default: {
    init: initMock,
    isFeatureEnabled: isFeatureEnabledMock,
    opt_in_capturing: optInCapturingMock,
    opt_out_capturing: optOutCapturingMock,
    register: registerMock,
  },
}));

vi.mock("../../api/client", () => ({
  recordSemanticTelemetryEvent: recordSemanticTelemetryEventMock,
  setSemanticTelemetryRemoteEnabled: setSemanticTelemetryRemoteEnabledMock,
}));

vi.mock("./config", () => ({
  getAnalyticsEnvironment: () => "production",
  getPostHogHost: () => "https://t.ctx.rs",
  getPostHogKey: () => "phc_test_key",
  getPostHogProjectId: () => "317085",
  getPostHogUiHost: () => "https://us.posthog.com",
}));

vi.mock("./identity", () => ({
  getInstallId: () => "install-test",
}));

vi.mock("../runtime", () => ({
  getAppShellKind: () => "desktop",
}));

describe("analytics client", () => {
  beforeEach(() => {
    vi.resetModules();
    initMock.mockReset();
    isFeatureEnabledMock.mockReset();
    isFeatureEnabledMock.mockReturnValue(false);
    optInCapturingMock.mockReset();
    optOutCapturingMock.mockReset();
    registerMock.mockReset();
    recordSemanticTelemetryEventMock.mockReset();
    setSemanticTelemetryRemoteEnabledMock.mockReset();
  });

  it("captures remote semantic product events through the ctx emitter", async () => {
    const mod = await import("./client");

    mod.setAnalyticsEnabled(true);
    const accepted = mod.captureProductEvent("foreground_backlog_observed", 2, {
      provider_id: "codex",
      backlog_ms: 1800,
      env_target: "local",
      workspace_id: "workspace-raw",
      sessionId: "session-raw",
    });

    expect(accepted).toBe(true);
    expect(setSemanticTelemetryRemoteEnabledMock).toHaveBeenCalledWith(true);
    expect(recordSemanticTelemetryEventMock).toHaveBeenCalledWith(expect.objectContaining({
      event_name: "foreground_backlog_observed",
      event_version: 2,
      plane: "product",
      delivery: "remote",
      origin_runtime: "desktop",
      origin_install_id: "install-test",
      surface: "desktop",
      env_target: "local",
      properties: expect.objectContaining({
        provider_id: "codex",
        backlog_ms: 1800,
        analytics_environment: "production",
      }),
    }));
    expect(recordSemanticTelemetryEventMock.mock.calls[0]?.[0].properties).not.toHaveProperty("workspace_id");
    expect(recordSemanticTelemetryEventMock.mock.calls[0]?.[0].properties).not.toHaveProperty("sessionId");
  });

  it("does not initialize PostHog until analytics is enabled", async () => {
    const mod = await import("./client");

    mod.initAnalytics();

    expect(initMock).not.toHaveBeenCalled();

    mod.setAnalyticsEnabled(true);

    expect(initMock).toHaveBeenCalledTimes(1);
  });

  it("drops remote captures when analytics is disabled", async () => {
    const mod = await import("./client");

    mod.setAnalyticsEnabled(false);
    const accepted = mod.captureAnalyticsEvent("foreground_backlog_observed", { backlog_ms: 1800 });

    expect(accepted).toBe(false);
    expect(setSemanticTelemetryRemoteEnabledMock).toHaveBeenCalledWith(false);
    expect(recordSemanticTelemetryEventMock).not.toHaveBeenCalled();
  });

  it("still records local-only incident events when remote analytics is disabled", async () => {
    const mod = await import("./client");

    mod.setAnalyticsEnabled(false);
    const accepted = mod.captureIncidentEvent(
      "renderer_backlog_sample",
      1,
      { queue_age_ms: 3200, env_target: "remote" },
      { delivery: "local_only", source: "worker_patch" },
    );

    expect(accepted).toBe(true);
    expect(recordSemanticTelemetryEventMock).toHaveBeenCalledWith(expect.objectContaining({
      event_name: "renderer_backlog_sample",
      plane: "incident",
      delivery: "local_only",
      source: "worker_patch",
      env_target: "remote",
      properties: expect.objectContaining({
        queue_age_ms: 3200,
      }),
    }));
  });
});
