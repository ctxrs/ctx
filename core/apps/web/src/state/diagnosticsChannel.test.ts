import { afterEach, describe, expect, it, vi } from "vitest";

const { trackRuntimeErrorObservedMock, trackSessionLoadFatalObservedMock, trackApiErrorObservedMock } =
  vi.hoisted(() => ({
    trackRuntimeErrorObservedMock: vi.fn(),
    trackSessionLoadFatalObservedMock: vi.fn(),
    trackApiErrorObservedMock: vi.fn(),
  }));

vi.mock("../utils/analytics", async () => {
  const actual = await vi.importActual<typeof import("../utils/analytics")>("../utils/analytics");
  return {
    ...actual,
    trackRuntimeErrorObserved: trackRuntimeErrorObservedMock,
    trackSessionLoadFatalObserved: trackSessionLoadFatalObservedMock,
    trackApiErrorObserved: trackApiErrorObservedMock,
  };
});
import {
  clearUiDiagnostics,
  emitUiDiagnostic,
  getUiDiagnostics,
  installGlobalRuntimeDiagnosticHandlers,
  resetUiDiagnosticsForTests,
  setUiDiagnosticPersistenceSink,
  setUiDiagnosticsMaxEventsForTests,
} from "./diagnosticsChannel";

describe("diagnosticsChannel", () => {
  afterEach(() => {
    resetUiDiagnosticsForTests();
    trackRuntimeErrorObservedMock.mockReset();
    trackSessionLoadFatalObservedMock.mockReset();
    trackApiErrorObservedMock.mockReset();
    vi.useRealTimers();
  });

  it("stores structured diagnostics with stable ids", () => {
    emitUiDiagnostic({
      source: "api",
      code: "api.http_error",
      message: "request failed",
    });
    emitUiDiagnostic({
      source: "session_supervisor",
      code: "session.load_fatal",
      message: "session failed",
      fatal: true,
    });

    const events = getUiDiagnostics();
    expect(events).toHaveLength(2);
    expect(events[0].id).toBeLessThan(events[1].id);
    expect(events[0].source).toBe("api");
    expect(events[1].fatal).toBe(true);
    expect(trackSessionLoadFatalObservedMock).toHaveBeenCalledTimes(1);
  });

  it("enforces bounded retention", () => {
    setUiDiagnosticsMaxEventsForTests(2);
    emitUiDiagnostic({ source: "api", code: "a", message: "1" });
    emitUiDiagnostic({ source: "api", code: "b", message: "2" });
    emitUiDiagnostic({ source: "api", code: "c", message: "3" });
    const events = getUiDiagnostics();
    expect(events).toHaveLength(2);
    expect(events[0].code).toBe("b");
    expect(events[1].code).toBe("c");
  });

  it("captures runtime error and unhandled rejection events", () => {
    installGlobalRuntimeDiagnosticHandlers();

    const runtimeError = new ErrorEvent("error", {
      message: "boom",
      filename: "foo.ts",
      lineno: 4,
      colno: 2,
      error: new Error("boom"),
    });
    window.dispatchEvent(runtimeError);

    const rejection = new Event("unhandledrejection") as PromiseRejectionEvent;
    Object.defineProperty(rejection, "reason", {
      value: new Error("nope"),
      configurable: true,
    });
    window.dispatchEvent(rejection);

    const events = getUiDiagnostics();
    expect(events.map((event) => event.code)).toEqual([
      "runtime.error",
      "runtime.unhandled_rejection",
    ]);
    expect(trackRuntimeErrorObservedMock).toHaveBeenCalledTimes(2);
  });

  it("clears events", () => {
    emitUiDiagnostic({ source: "api", code: "x", message: "x" });
    clearUiDiagnostics();
    expect(getUiDiagnostics()).toEqual([]);
  });

  it("tracks API diagnostics with normalized endpoint metadata", () => {
    emitUiDiagnostic({
      source: "api",
      code: "api.http_error",
      message: "HTTP failure",
      context: {
        path: "/api/workspaces/123",
        method: "get",
        status: 502,
      },
    });

    expect(trackApiErrorObservedMock).toHaveBeenCalledTimes(1);
    expect(trackApiErrorObservedMock).toHaveBeenCalledWith(expect.objectContaining({
      endpoint: "/api/workspaces/:id",
      method: "GET",
      statusFamily: "5xx",
    }));
  });

  it("throttles duplicate analytics emissions for the same API diagnostic signature", () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-03-11T00:00:00.000Z"));

    const input = {
      source: "api" as const,
      code: "api.http_error",
      message: "HTTP failure",
      context: {
        path: "/api/workspaces/123",
        method: "get",
        status: 502,
      },
    };

    emitUiDiagnostic(input);
    emitUiDiagnostic(input);

    expect(getUiDiagnostics()).toHaveLength(2);
    expect(trackApiErrorObservedMock).toHaveBeenCalledTimes(1);

    vi.advanceTimersByTime(5 * 60 * 1000 + 1);
    emitUiDiagnostic(input);

    expect(trackApiErrorObservedMock).toHaveBeenCalledTimes(2);
  });

  it("strips query params and normalizes id-like path segments", () => {
    emitUiDiagnostic({
      source: "api",
      code: "api.http_error",
      message: "HTTP failure",
      context: {
        path: "/api/execution/launch/ws_abc123def456/status?job_id=3f11f2d5-1270-4f97-b0bc-8d6707a5ef95",
        method: "post",
        status: 400,
      },
    });

    expect(trackApiErrorObservedMock).toHaveBeenCalledTimes(1);
    expect(trackApiErrorObservedMock).toHaveBeenCalledWith(expect.objectContaining({
      endpoint: "/api/execution/launch/:id/status",
      method: "POST",
      statusFamily: "4xx",
    }));
  });

  it("forwards diagnostics to configured persistence sink", () => {
    const sink = vi.fn();
    setUiDiagnosticPersistenceSink(sink);

    emitUiDiagnostic({
      source: "runtime",
      code: "runtime.error",
      message: "boom",
      severity: "error",
    });

    expect(sink).toHaveBeenCalledTimes(1);
    expect(sink).toHaveBeenCalledWith(expect.objectContaining({
      source: "runtime",
      code: "runtime.error",
      message: "boom",
      severity: "error",
    }));
  });

  it("swallows persistence sink failures", () => {
    setUiDiagnosticPersistenceSink(() => {
      throw new Error("sink failed");
    });

    expect(() =>
      emitUiDiagnostic({
        source: "runtime",
        code: "runtime.error",
        message: "boom",
        severity: "error",
      })).not.toThrow();
  });
});
