import { afterEach, describe, expect, it, vi } from "vitest";

const loadTelemetryModule = async () => {
  vi.resetModules();
  window.history.replaceState(null, "", "/workspaces/ws-test?loadtest=1");
  return import("./loadTestTelemetry");
};

afterEach(() => {
  vi.restoreAllMocks();
});

describe("loadTestTelemetry", () => {
  it("classifies timer-throttled event loop gaps when RAF kept advancing", async () => {
    const { shouldClassifyEventLoopGapAsTimerThrottle } = await loadTelemetryModule();

    expect(
      shouldClassifyEventLoopGapAsTimerThrottle({
        gapMs: 11056,
        documentHidden: false,
        visibilityState: "visible",
        timeSinceLastRafMs: 75,
      }),
    ).toBe(true);
    expect(
      shouldClassifyEventLoopGapAsTimerThrottle({
        gapMs: 11056,
        documentHidden: false,
        visibilityState: "visible",
        timeSinceLastRafMs: 1200,
      }),
    ).toBe(false);
  });

  it("records user-visible session switch timing", async () => {
    let now = 1000;
    vi.spyOn(performance, "now").mockImplementation(() => now);
    Object.defineProperty(performance, "timeOrigin", {
      configurable: true,
      value: 10_000,
    });

    const { initLoadTestTelemetry } = await loadTelemetryModule();
    const telemetry = initLoadTestTelemetry();
    expect(telemetry).not.toBeNull();

    telemetry?.startVisibleSessionSwitch({
      fromSessionId: "session-a",
      toSessionId: "session-b",
      taskId: "task-b",
      targetIndex: 1,
      source: "pointer",
      subscribedAtClick: true,
      authoritativeAtClick: true,
    });
    now = 1042;
    telemetry?.updateVisibleSessionSwitchState("session-b", {
      subscribedWhenActive: true,
      authoritativeWhenActive: true,
      httpRehydrateSeen: false,
    });
    telemetry?.markVisibleSessionSwitchVisible("session-b");
    now = 1075;
    telemetry?.markVisibleSessionSwitchStable("session-b");

    expect(telemetry?.getSnapshot().visible_session_switches).toEqual([
      {
        from_session_id: "session-a",
        to_session_id: "session-b",
        task_id: "task-b",
        target_index: 1,
        source: "pointer",
        started_at_ms: 11_000,
        visible_at_ms: 11_042,
        stable_at_ms: 11_075,
        click_to_visible_ms: 42,
        click_to_stable_ms: 75,
        subscribed_at_click: true,
        authoritative_at_click: true,
        subscribed_when_active: true,
        authoritative_when_active: true,
        http_rehydrate_seen: false,
        status: "stable",
      },
    ]);
    expect(telemetry?.getSummary().visible_switch_ms).toEqual({
      count: 1,
      p50: 42,
      p95: 42,
      p99: 42,
    });
    telemetry?.stop();
  });

  it("abandons pending visible switches when a newer switch starts", async () => {
    let now = 2000;
    vi.spyOn(performance, "now").mockImplementation(() => now);
    Object.defineProperty(performance, "timeOrigin", {
      configurable: true,
      value: 20_000,
    });

    const { initLoadTestTelemetry } = await loadTelemetryModule();
    const telemetry = initLoadTestTelemetry();

    telemetry?.startVisibleSessionSwitch({
      fromSessionId: "session-a",
      toSessionId: "session-b",
      taskId: "task-b",
      source: "keyboard",
    });
    now = 2010;
    telemetry?.startVisibleSessionSwitch({
      fromSessionId: "session-a",
      toSessionId: "session-c",
      taskId: "task-c",
      source: "pointer",
    });

    expect(telemetry?.getSnapshot().visible_session_switches).toEqual([
      {
        from_session_id: "session-a",
        to_session_id: "session-b",
        task_id: "task-b",
        source: "keyboard",
        started_at_ms: 22_000,
        status: "abandoned",
      },
    ]);
    telemetry?.stop();
  });
});
