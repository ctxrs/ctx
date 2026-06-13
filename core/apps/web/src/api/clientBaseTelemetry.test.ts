import type { SemanticTelemetryEvent } from "@ctx/types";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  recordClientHistogramMetric,
  recordSemanticTelemetryEvent,
  resetClientBaseTelemetryForTests,
  setSemanticTelemetryRemoteEnabled,
} from "./clientBaseTelemetry";

vi.mock("../utils/desktop", () => ({
  isDesktopApp: () => false,
}));

const fetchMock = vi.fn<(input: RequestInfo | URL, init?: RequestInit) => Promise<Response>>();

const semanticEvent = (
  eventId: string,
  overrides: Partial<SemanticTelemetryEvent> = {},
): SemanticTelemetryEvent => ({
  event_id: eventId,
  event_name: "app_opened",
  event_version: 1,
  occurred_at: "2026-05-06T12:00:00.000Z",
  plane: "product",
  delivery: "remote",
  origin_runtime: "desktop",
  origin_install_id: "install-1",
  app_version: "1.2.3",
  os: "macos",
  arch: "arm64",
  surface: "desktop",
  env_target: "remote",
  source: "test",
  properties: { launch_surface: "desktop" },
  ...overrides,
});

const response = (status: number): Response =>
  new Response(null, { status });

const postedBatchAt = (index: number): { events: SemanticTelemetryEvent[] } => {
  const init = fetchMock.mock.calls[index]?.[1];
  const rawBody = String(init?.body ?? "{}");
  return JSON.parse(rawBody) as { events: SemanticTelemetryEvent[] };
};

const postedClientMetricBatchAt = (index: number): { events: Array<{ name: string; value: number }> } => {
  const init = fetchMock.mock.calls[index]?.[1];
  const rawBody = String(init?.body ?? "{}");
  return JSON.parse(rawBody) as { events: Array<{ name: string; value: number }> };
};

describe("clientBase semantic telemetry", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    fetchMock.mockReset();
    vi.stubGlobal("fetch", fetchMock);
    resetClientBaseTelemetryForTests();
  });

  afterEach(() => {
    resetClientBaseTelemetryForTests();
    vi.unstubAllGlobals();
    vi.useRealTimers();
  });

  it("retains product events and retries after a transport failure", async () => {
    fetchMock
      .mockRejectedValueOnce(new Error("network unavailable"))
      .mockResolvedValueOnce(response(204));

    recordSemanticTelemetryEvent(semanticEvent("event-1"));

    await vi.advanceTimersByTimeAsync(1_000);
    expect(fetchMock).toHaveBeenCalledTimes(1);

    await vi.advanceTimersByTimeAsync(1_000);
    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(postedBatchAt(1).events.map((event) => event.event_id)).toEqual(["event-1"]);

    await vi.advanceTimersByTimeAsync(2_000);
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it("retains product events and retries after a non-2xx response", async () => {
    fetchMock
      .mockResolvedValueOnce(response(500))
      .mockResolvedValueOnce(response(204));

    recordSemanticTelemetryEvent(semanticEvent("event-2"));

    await vi.advanceTimersByTimeAsync(1_000);
    expect(fetchMock).toHaveBeenCalledTimes(1);

    await vi.advanceTimersByTimeAsync(1_000);
    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(postedBatchAt(1).events.map((event) => event.event_id)).toEqual(["event-2"]);
  });

  it("removes queued remote events when analytics is disabled but keeps local-only events", async () => {
    fetchMock.mockResolvedValue(response(204));

    recordSemanticTelemetryEvent(semanticEvent("remote-event"));
    setSemanticTelemetryRemoteEnabled(false);

    await vi.advanceTimersByTimeAsync(1_000);
    expect(fetchMock).not.toHaveBeenCalled();

    recordSemanticTelemetryEvent(semanticEvent("local-event", { delivery: "local_only" }));

    await vi.advanceTimersByTimeAsync(1_000);
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(postedBatchAt(0).events.map((event) => event.event_id)).toEqual(["local-event"]);
  });

  it("keeps the newest client metrics when the bounded queue is saturated", async () => {
    fetchMock.mockResolvedValue(response(204));

    for (let index = 0; index < 205; index += 1) {
      recordClientHistogramMetric(
        index === 204 ? "workbench.interrupt_click_to_pending_ms" : "workbench.client_receive_lag_ms",
        "ms",
        index,
      );
    }

    await vi.advanceTimersByTimeAsync(1_000);

    const batch = postedClientMetricBatchAt(0);
    expect(batch.events).toHaveLength(200);
    expect(batch.events[0]).toEqual(expect.objectContaining({ value: 5 }));
    expect(batch.events.at(-1)).toEqual(
      expect.objectContaining({
        name: "workbench.interrupt_click_to_pending_ms",
        value: 204,
      }),
    );
  });

  it("keeps protected client metrics when later metric floods saturate the queue", async () => {
    fetchMock.mockResolvedValue(response(204));

    recordClientHistogramMetric("workbench.interrupt_click_to_pending_ms", "ms", 1);
    for (let index = 0; index < 205; index += 1) {
      recordClientHistogramMetric("workbench.client_receive_lag_ms", "ms", index);
    }

    await vi.advanceTimersByTimeAsync(1_000);

    const batch = postedClientMetricBatchAt(0);
    expect(batch.events).toHaveLength(200);
    expect(batch.events).toContainEqual(
      expect.objectContaining({
        name: "workbench.interrupt_click_to_pending_ms",
        value: 1,
      }),
    );
  });

  it("retries client metrics after a transport failure", async () => {
    fetchMock
      .mockResolvedValueOnce(response(503))
      .mockResolvedValueOnce(response(204));

    recordClientHistogramMetric("workbench.interrupt_click_to_pending_ms", "ms", 12);

    await vi.advanceTimersByTimeAsync(1_000);
    expect(fetchMock).toHaveBeenCalledTimes(1);

    await vi.advanceTimersByTimeAsync(1_000);
    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(postedClientMetricBatchAt(1).events).toContainEqual(
      expect.objectContaining({
        name: "workbench.interrupt_click_to_pending_ms",
        value: 12,
      }),
    );
  });

  it("keeps protected failed client metrics when the queue refills before retry", async () => {
    let resolveFirstFlush: (response: Response) => void = () => {};
    fetchMock
      .mockImplementationOnce(
        () =>
          new Promise<Response>((resolve) => {
            resolveFirstFlush = resolve;
          }),
      )
      .mockResolvedValueOnce(response(204));

    recordClientHistogramMetric("workbench.interrupt_click_to_pending_ms", "ms", 25);

    await vi.advanceTimersByTimeAsync(1_000);
    expect(fetchMock).toHaveBeenCalledTimes(1);

    for (let index = 0; index < 205; index += 1) {
      recordClientHistogramMetric("workbench.client_receive_lag_ms", "ms", index);
    }
    resolveFirstFlush(response(503));
    await vi.advanceTimersByTimeAsync(1_000);

    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(postedClientMetricBatchAt(1).events).toContainEqual(
      expect.objectContaining({
        name: "workbench.interrupt_click_to_pending_ms",
        value: 25,
      }),
    );
  });
});
