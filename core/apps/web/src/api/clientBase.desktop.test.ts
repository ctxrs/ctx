import type { SemanticTelemetryEvent } from "@ctx/types";
import { beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

const desktopGetConnectionMock = vi.hoisted(() => vi.fn());
const desktopConnectLocalMock = vi.hoisted(() => vi.fn());
const fetchMock = vi.hoisted(() => vi.fn());
const SESSION_CONNECTION_KEY = "ctxDaemonConnectionV1";
const PERSISTED_BASE_KEY = "ctxDaemonConnectionBaseV1";

const okFetchJsonResponse = (body: unknown = { ok: true }, status = 200) =>
  new Response(JSON.stringify(body), {
    status,
    headers: {
      "content-type": "application/json",
    },
  });

const errorFetchJsonResponse = (status: number, body: unknown) =>
  okFetchJsonResponse(body, status);

const fetchHeadersAt = (callIndex: number): Record<string, string> =>
  (fetchMock.mock.calls[callIndex]?.[1]?.headers ?? {}) as Record<string, string>;

const expectNoAuthorizationHeader = (callIndex: number) => {
  const headers = fetchHeadersAt(callIndex);
  const authEntry = Object.entries(headers).find(([key]) => key.toLowerCase() === "authorization");
  expect(authEntry).toBeUndefined();
};

const expectAuthorizationHeader = (callIndex: number, expected: string) => {
  const headers = fetchHeadersAt(callIndex);
  const authEntry = Object.entries(headers).find(([key]) => key.toLowerCase() === "authorization");
  expect(authEntry?.[1]).toBe(expected);
};

const semanticTelemetryEvent = (eventId: string): SemanticTelemetryEvent => ({
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
  source: "desktop-test",
  properties: { launch_surface: "desktop" },
});

vi.mock("../utils/desktop", () => ({
  isDesktopApp: () => true,
  desktopConnectLocal: desktopConnectLocalMock,
  desktopGetConnection: desktopGetConnectionMock,
}));

let clientBaseMod: typeof import("./clientBase");
let daemonConnectionMod: typeof import("./daemonConnection");
let desktopDaemonConnectionMod: typeof import("./desktopDaemonConnection");
let clientBaseTelemetryMod: typeof import("./clientBaseTelemetry");

describe("clientBase desktop connection sync", () => {
  beforeAll(async () => {
    clientBaseMod = await import("./clientBase");
    daemonConnectionMod = await import("./daemonConnection");
    desktopDaemonConnectionMod = await import("./desktopDaemonConnection");
    clientBaseTelemetryMod = await import("./clientBaseTelemetry");
  }, 60_000);

  beforeEach(() => {
    vi.clearAllMocks();
    sessionStorage.clear();
    localStorage.clear();
    const g = globalThis as typeof globalThis & { __TAURI__?: unknown; __TAURI_INTERNALS__?: unknown };
    g.__TAURI__ = {};
    vi.stubGlobal("fetch", fetchMock);
    clientBaseTelemetryMod.resetClientBaseTelemetryForTests();
    desktopDaemonConnectionMod.resetDesktopDaemonConnectionSyncForTests();
    daemonConnectionMod.resetDaemonConnectionStateForTests();
  });

  it("bootstraps a missing local desktop connection via desktopConnectLocal", async () => {
    desktopGetConnectionMock.mockResolvedValueOnce({
      kind: "none",
      intent: "auto_local_bootstrap",
      local_auto_bootstrap_allowed: true,
    });
    desktopConnectLocalMock.mockResolvedValue({
      kind: "local",
      intent: "explicit_local",
      local_auto_bootstrap_allowed: true,
      base_url: "http://127.0.0.1:4399",
      browser_query_secret: "browser-secret-abc",
    });

    const result = await clientBaseMod.syncDesktopDaemonConnectionFromBridge({
      force: true,
      probeHealth: true,
      reason: "test_probe",
    });

    expect(desktopGetConnectionMock).toHaveBeenCalledTimes(1);
    expect(desktopConnectLocalMock).toHaveBeenCalledTimes(1);
    expect(fetchMock).not.toHaveBeenCalled();
    expect(result.config.baseUrl).toBe("http://127.0.0.1:4399");
    expect(result.config.wsBaseUrl).toBe("ws://127.0.0.1:4399");
    expect(result.config.authToken).toBe("browser-secret-abc");
  });

  it("does not auto-connect local after an explicit desktop disconnect", async () => {
    desktopGetConnectionMock.mockResolvedValueOnce({
      kind: "none",
      intent: "explicit_disconnected",
      local_auto_bootstrap_allowed: false,
    });

    const result = await clientBaseMod.syncDesktopDaemonConnectionFromBridge({
      force: true,
      probeHealth: true,
      reason: "test_explicit_disconnect",
    });

    expect(desktopGetConnectionMock).toHaveBeenCalledTimes(1);
    expect(desktopConnectLocalMock).not.toHaveBeenCalled();
    expect(result.config.baseUrl).toBeNull();
    expect(result.config.authToken).toBeNull();
  });

  it("republishes canonical desktop state when a bridge read clears a stale connection", async () => {
    desktopGetConnectionMock.mockResolvedValue({
      kind: "none",
      intent: "explicit_disconnected",
      local_auto_bootstrap_allowed: false,
      base_url: null,
      browser_query_secret: null,
    });

    clientBaseMod.applyDaemonDesktopConnection({
      base_url: "http://127.0.0.1:4399",
      browser_query_secret: "stale-browser-secret",
    });
    expect(clientBaseMod.getDaemonClientConfig()).toMatchObject({
      baseUrl: "http://127.0.0.1:4399",
      authToken: "stale-browser-secret",
    });

    const result = await clientBaseMod.syncDesktopDaemonConnectionFromBridge({
      force: true,
      probeHealth: false,
      reason: "test_clear_stale_connection",
    });

    expect(desktopGetConnectionMock).toHaveBeenCalledTimes(1);
    expect(desktopConnectLocalMock).not.toHaveBeenCalled();
    expect(result.config).toMatchObject({
      baseUrl: null,
      wsBaseUrl: null,
      authToken: null,
    });
    expect(clientBaseMod.getDaemonClientConfig()).toMatchObject({
      baseUrl: null,
      wsBaseUrl: null,
      authToken: null,
    });
  });

  it("uses direct desktop fetch while resyncing desktop bridge state per request", async () => {
    desktopGetConnectionMock.mockResolvedValue({
      kind: "local",
      base_url: "http://127.0.0.1:4399",
      browser_query_secret: "browser-secret-abc",
    });
    fetchMock.mockImplementation(() => Promise.resolve(okFetchJsonResponse({ ok: true })));

    expect(clientBaseMod.getDaemonClientConfig().baseUrl).toBeNull();

    const first = await clientBaseMod.apiAny<{ ok: boolean }>("/api/health");
    const second = await clientBaseMod.apiAny<{ ok: boolean }>("/api/health");

    expect(first.ok).toBe(true);
    expect(second.ok).toBe(true);
    expect(clientBaseMod.getDaemonClientConfig().baseUrl).toBe("http://127.0.0.1:4399");
    expect(clientBaseMod.getDaemonClientConfig().wsBaseUrl).toBe("ws://127.0.0.1:4399");
    expect(desktopGetConnectionMock).toHaveBeenCalledTimes(2);
    expect(desktopConnectLocalMock).not.toHaveBeenCalled();
    expect(fetchMock).toHaveBeenCalledTimes(3);
    expect(fetchMock).toHaveBeenNthCalledWith(
      1,
      "http://127.0.0.1:4399/api/health",
      expect.objectContaining({
        headers: expect.objectContaining({
          "content-type": "application/json",
        }),
      }),
    );
    expect(fetchMock).toHaveBeenNthCalledWith(
      2,
      "http://127.0.0.1:4399/api/workspaces",
      expect.objectContaining({
        method: "GET",
      }),
    );
    expect(fetchMock).toHaveBeenNthCalledWith(
      3,
      "http://127.0.0.1:4399/api/health",
      expect.objectContaining({
        headers: expect.objectContaining({
          "content-type": "application/json",
        }),
      }),
    );
    expectAuthorizationHeader(0, "Bearer browser-secret-abc");
    expectAuthorizationHeader(1, "Bearer browser-secret-abc");
    expectAuthorizationHeader(2, "Bearer browser-secret-abc");
  });

  it("refreshes desktop bridge state before reuse after a later token rotation", async () => {
    const dateNowSpy = vi.spyOn(Date, "now");
    let now = 1_000;
    dateNowSpy.mockImplementation(() => now);
    desktopGetConnectionMock
      .mockResolvedValueOnce({
        kind: "local",
        base_url: "http://127.0.0.1:4399",
        browser_query_secret: "browser-secret-old",
      })
      .mockResolvedValueOnce({
        kind: "local",
        base_url: "http://127.0.0.1:4400",
        browser_query_secret: "browser-secret-new",
      });
    fetchMock.mockImplementation(() => Promise.resolve(okFetchJsonResponse({ ok: true })));

    try {
      await clientBaseMod.apiAny<{ ok: boolean }>("/api/health");

      now = 1_500;
      await clientBaseMod.apiAny<{ ok: boolean }>("/api/providers");

      expect(desktopGetConnectionMock).toHaveBeenCalledTimes(2);
      expect(fetchMock).toHaveBeenNthCalledWith(
        1,
        "http://127.0.0.1:4399/api/health",
        expect.any(Object),
      );
      expect(fetchMock).toHaveBeenNthCalledWith(
        2,
        "http://127.0.0.1:4400/api/providers",
        expect.any(Object),
      );
      expectAuthorizationHeader(0, "Bearer browser-secret-old");
      expectAuthorizationHeader(1, "Bearer browser-secret-new");
      expect(clientBaseMod.getDaemonClientConfig()).toMatchObject({
        baseUrl: "http://127.0.0.1:4400",
        wsBaseUrl: "ws://127.0.0.1:4400",
        authToken: "browser-secret-new",
      });
    } finally {
      dateNowSpy.mockRestore();
    }
  });

  it("refreshes daemon target scope when the desktop bridge reuses the same forwarded base URL", async () => {
    const dateNowSpy = vi.spyOn(Date, "now");
    let now = 1_000;
    dateNowSpy.mockImplementation(() => now);
    desktopGetConnectionMock
      .mockResolvedValueOnce({
        kind: "ssh",
        base_url: "http://127.0.0.1:4399",
        browser_query_secret: "browser-secret-ssh",
        host: "host-a.example",
        user: "alice",
        remote_port: 4399,
        remote_data_dir: "/srv/ctx-a",
      })
      .mockResolvedValueOnce({
        kind: "ssh",
        base_url: "http://127.0.0.1:4399",
        browser_query_secret: "browser-secret-ssh",
        host: "host-b.example",
        user: "alice",
        remote_port: 4399,
        remote_data_dir: "/srv/ctx-a",
      });
    fetchMock.mockImplementation(() => Promise.resolve(okFetchJsonResponse({ ok: true })));

    try {
      await clientBaseMod.apiAny<{ ok: boolean }>("/api/health");
      expect(daemonConnectionMod.getDaemonConnection().targetScope).toMatchObject({
        kind: "desktop_ssh",
        host: "host-a.example",
        user: "alice",
        port: 4399,
        dataDir: "/srv/ctx-a",
      });

      now = 1_500;
      await clientBaseMod.apiAny<{ ok: boolean }>("/api/providers");

      expect(desktopGetConnectionMock).toHaveBeenCalledTimes(2);
      expect(fetchMock).toHaveBeenNthCalledWith(
        1,
        "http://127.0.0.1:4399/api/health",
        expect.any(Object),
      );
      expect(fetchMock).toHaveBeenNthCalledWith(
        2,
        "http://127.0.0.1:4399/api/providers",
        expect.any(Object),
      );
      expectAuthorizationHeader(0, "Bearer browser-secret-ssh");
      expectAuthorizationHeader(1, "Bearer browser-secret-ssh");
      expect(daemonConnectionMod.getDaemonConnection().targetScope).toMatchObject({
        kind: "desktop_ssh",
        host: "host-b.example",
        user: "alice",
        port: 4399,
        dataDir: "/srv/ctx-a",
      });
      expect(String(JSON.parse(sessionStorage.getItem(SESSION_CONNECTION_KEY) ?? "{}").targetScope)).toContain("host-b.example");
      expect(String(JSON.parse(localStorage.getItem(PERSISTED_BASE_KEY) ?? "{}").targetScope)).toContain("host-b.example");
    } finally {
      dateNowSpy.mockRestore();
    }
  });

  it("repopulates canonical session and persisted base storage after a later bridge rotation", async () => {
    sessionStorage.setItem(
      SESSION_CONNECTION_KEY,
      JSON.stringify({
        v: 1,
        baseUrl: "http://127.0.0.1:4399",
        wsBaseUrl: "ws://127.0.0.1:4399",
        authToken: "token-old",
        source: "desktop",
      }),
    );
    localStorage.setItem(
      PERSISTED_BASE_KEY,
      JSON.stringify({
        v: 1,
        baseUrl: "http://127.0.0.1:4399",
        wsBaseUrl: "ws://127.0.0.1:4399",
      }),
    );
    daemonConnectionMod.resetDaemonConnectionStateForTests();
    desktopGetConnectionMock.mockResolvedValue({
      kind: "local",
      base_url: "http://127.0.0.1:4400",
      browser_query_secret: "browser-secret-new",
    });

    expect(clientBaseMod.getDaemonClientConfig()).toMatchObject({
      baseUrl: "http://127.0.0.1:4399",
      wsBaseUrl: "ws://127.0.0.1:4399",
      authToken: null,
    });

    const result = await clientBaseMod.syncDesktopDaemonConnectionFromBridge({
      force: true,
      reason: "test_rotation",
    });

    expect(desktopGetConnectionMock).toHaveBeenCalledTimes(1);
    expect(result.config).toMatchObject({
      baseUrl: "http://127.0.0.1:4400",
      wsBaseUrl: "ws://127.0.0.1:4400",
      authToken: "browser-secret-new",
    });
    expect(JSON.parse(sessionStorage.getItem(SESSION_CONNECTION_KEY) ?? "{}")).toMatchObject({
      v: 1,
      baseUrl: "http://127.0.0.1:4400",
      wsBaseUrl: "ws://127.0.0.1:4400",
      authToken: null,
      source: "desktop",
    });
    expect(JSON.parse(localStorage.getItem(PERSISTED_BASE_KEY) ?? "{}")).toMatchObject({
      v: 1,
      baseUrl: "http://127.0.0.1:4400",
      wsBaseUrl: "ws://127.0.0.1:4400",
    });
  });

  it("re-syncs desktop auth when restore only has a persisted base URL", async () => {
    localStorage.setItem(PERSISTED_BASE_KEY, JSON.stringify({
      v: 1,
      baseUrl: "http://127.0.0.1:4399",
      wsBaseUrl: "ws://127.0.0.1:4399",
    }));
    daemonConnectionMod.resetDaemonConnectionStateForTests();
    desktopGetConnectionMock.mockResolvedValue({
      kind: "local",
      base_url: "http://127.0.0.1:4399",
      browser_query_secret: "browser-secret-abc",
    });
    fetchMock
      .mockResolvedValueOnce(okFetchJsonResponse([]))
      .mockResolvedValueOnce(okFetchJsonResponse({ ok: true }));

    expect(clientBaseMod.getDaemonClientConfig()).toMatchObject({
      baseUrl: "http://127.0.0.1:4399",
      authToken: null,
    });

    const result = await clientBaseMod.apiAny<{ ok: boolean }>("/api/health");

    expect(result.ok).toBe(true);
    expect(desktopGetConnectionMock).toHaveBeenCalledTimes(1);
    expect(fetchMock).toHaveBeenNthCalledWith(
      1,
      "http://127.0.0.1:4399/api/workspaces",
      expect.any(Object),
    );
    expect(fetchMock).toHaveBeenNthCalledWith(
      2,
      "http://127.0.0.1:4399/api/health",
      expect.any(Object),
    );
    expectNoAuthorizationHeader(0);
    expectAuthorizationHeader(1, "Bearer browser-secret-abc");
    expect(clientBaseMod.getDaemonClientConfig()).toMatchObject({
      baseUrl: "http://127.0.0.1:4399",
      authToken: "browser-secret-abc",
    });
  });

  it("repairs stale restored desktop auth before the first API request", async () => {
    localStorage.setItem(PERSISTED_BASE_KEY, JSON.stringify({
      v: 1,
      baseUrl: "http://127.0.0.1:4399",
      wsBaseUrl: "ws://127.0.0.1:4399",
    }));
    daemonConnectionMod.resetDaemonConnectionStateForTests();
    desktopGetConnectionMock.mockResolvedValue({
      kind: "local",
      base_url: "http://127.0.0.1:4399",
      browser_query_secret: "browser-secret-stale",
    });
    desktopConnectLocalMock.mockResolvedValue({
      kind: "local",
      base_url: "http://127.0.0.1:4400",
      browser_query_secret: "browser-secret-fresh",
    });
    fetchMock
      .mockResolvedValueOnce(errorFetchJsonResponse(401, { error: "unauthorized" }))
      .mockResolvedValueOnce(okFetchJsonResponse({ ok: true }));

    const result = await clientBaseMod.apiAny<{ ok: boolean }>("/api/health");

    expect(result.ok).toBe(true);
    expect(desktopGetConnectionMock).toHaveBeenCalledTimes(1);
    expect(desktopConnectLocalMock).toHaveBeenCalledTimes(1);
    expect(fetchMock).toHaveBeenNthCalledWith(
      1,
      "http://127.0.0.1:4399/api/workspaces",
      expect.any(Object),
    );
    expect(fetchMock).toHaveBeenNthCalledWith(
      2,
      "http://127.0.0.1:4400/api/health",
      expect.any(Object),
    );
    expectNoAuthorizationHeader(0);
    expectAuthorizationHeader(1, "Bearer browser-secret-fresh");
    expect(clientBaseMod.getDaemonClientConfig()).toMatchObject({
      baseUrl: "http://127.0.0.1:4400",
      authToken: "browser-secret-fresh",
    });
  });

  it("uses direct desktop fetch after raw fetch preflight on cold start", async () => {
    desktopGetConnectionMock.mockResolvedValue({
      kind: "local",
      base_url: "http://127.0.0.1:4399",
      browser_query_secret: "browser-secret-abc",
    });
    fetchMock.mockResolvedValue(okFetchJsonResponse({ ok: true }));

    const result = await clientBaseMod.daemonFetchRaw("/api/sessions/web", {
      method: "POST",
      body: JSON.stringify({ label: "preflight-check" }),
    });

    expect(result.status).toBe(200);
    expect(desktopGetConnectionMock).toHaveBeenCalledTimes(1);
    expect(fetchMock).toHaveBeenCalledWith(
      "http://127.0.0.1:4399/api/sessions/web",
      expect.objectContaining({
        method: "POST",
        body: JSON.stringify({ label: "preflight-check" }),
      }),
    );
    expectAuthorizationHeader(0, "Bearer browser-secret-abc");
  });

  it("auto-connects local daemon for desktop raw fetch preflight by default", async () => {
    desktopGetConnectionMock.mockResolvedValue({
      kind: "none",
      intent: "auto_local_bootstrap",
      local_auto_bootstrap_allowed: true,
    });
    desktopConnectLocalMock.mockResolvedValue({
      kind: "local",
      intent: "explicit_local",
      local_auto_bootstrap_allowed: true,
      base_url: "http://127.0.0.1:4399",
      browser_query_secret: "browser-secret-raw",
    });
    fetchMock.mockResolvedValue(okFetchJsonResponse({ ok: true }));

    const result = await clientBaseMod.daemonFetchRaw("/api/workspaces/123");

    expect(result.status).toBe(200);
    expect(desktopGetConnectionMock).toHaveBeenCalledTimes(1);
    expect(desktopConnectLocalMock).toHaveBeenCalledTimes(1);
    expect(fetchMock).toHaveBeenCalledWith(
      "http://127.0.0.1:4399/api/workspaces/123",
      expect.objectContaining({
        headers: expect.objectContaining({
          "content-type": "application/json",
        }),
      }),
    );
    expectAuthorizationHeader(0, "Bearer browser-secret-raw");
  });

  it("auto-connects local daemon before semantic telemetry flush", async () => {
    vi.useFakeTimers();
    desktopGetConnectionMock.mockResolvedValue({
      kind: "none",
      intent: "auto_local_bootstrap",
      local_auto_bootstrap_allowed: true,
    });
    desktopConnectLocalMock.mockResolvedValue({
      kind: "local",
      intent: "explicit_local",
      local_auto_bootstrap_allowed: true,
      base_url: "http://127.0.0.1:4399",
      browser_query_secret: "browser-secret-telemetry",
    });
    fetchMock.mockResolvedValue(okFetchJsonResponse({ ok: true }));

    try {
      clientBaseTelemetryMod.recordSemanticTelemetryEvent(semanticTelemetryEvent("semantic-desktop-1"));

      await vi.advanceTimersByTimeAsync(1_000);

      expect(desktopGetConnectionMock).toHaveBeenCalledTimes(1);
      expect(desktopConnectLocalMock).toHaveBeenCalledTimes(1);
      expect(fetchMock).toHaveBeenCalledTimes(1);
      expect(fetchMock).toHaveBeenCalledWith(
        "http://127.0.0.1:4399/api/telemetry/events",
        expect.objectContaining({
          method: "POST",
          keepalive: true,
        }),
      );
      expectAuthorizationHeader(0, "Bearer browser-secret-telemetry");
    } finally {
      vi.useRealTimers();
    }
  });

  it("keeps passive raw desktop health checks from reconnecting a disconnected local daemon", async () => {
    desktopGetConnectionMock.mockResolvedValue({
      kind: "none",
      intent: "auto_local_bootstrap",
      local_auto_bootstrap_allowed: true,
    });

    await expect(
      clientBaseMod.daemonFetchRaw("/api/health", undefined, {
        connectLocalWhenMissing: false,
      }),
    ).rejects.toThrow(
      "Desktop daemon connection is not configured.",
    );

    expect(desktopGetConnectionMock).toHaveBeenCalledTimes(1);
    expect(desktopConnectLocalMock).not.toHaveBeenCalled();
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("preserves caller headers for raw direct desktop fetch requests", async () => {
    desktopGetConnectionMock.mockResolvedValue({
      kind: "local",
      base_url: "http://127.0.0.1:4399",
      browser_query_secret: "browser-secret-abc",
    });
    fetchMock.mockResolvedValue(okFetchJsonResponse({ ok: true }));

    const result = await clientBaseMod.daemonFetchRaw("/api/sessions/web", {
      method: "POST",
      headers: {
        "content-type": "application/json",
        "x-test-case": "raw-fetch-merge",
      },
      body: JSON.stringify({ label: "header-merge" }),
    });

    expect(result.status).toBe(200);
    expect(fetchMock).toHaveBeenCalledWith(
      "http://127.0.0.1:4399/api/sessions/web",
      expect.objectContaining({
        method: "POST",
        body: JSON.stringify({ label: "header-merge" }),
      }),
    );
    expectAuthorizationHeader(0, "Bearer browser-secret-abc");
    expect(fetchHeadersAt(0)["x-test-case"]).toBe("raw-fetch-merge");
  });

  it("strips caller Authorization headers before direct desktop fetches", async () => {
    desktopGetConnectionMock.mockResolvedValue({
      kind: "local",
      base_url: "http://127.0.0.1:4399",
      browser_query_secret: "browser-secret-abc",
    });
    fetchMock.mockResolvedValue(okFetchJsonResponse({ ok: true }));

    const result = await clientBaseMod.daemonFetchRaw("/api/sessions/web", {
      method: "POST",
      headers: {
        Authorization: "Bearer should-not-pass-through",
        "content-type": "application/json",
        "x-test-case": "strip-authorization",
      },
      body: JSON.stringify({ label: "strip-authorization" }),
    });

    expect(result.status).toBe(200);
    expect(fetchMock).toHaveBeenCalledWith(
      "http://127.0.0.1:4399/api/sessions/web",
      expect.objectContaining({
        method: "POST",
        body: JSON.stringify({ label: "strip-authorization" }),
      }),
    );
    expectAuthorizationHeader(0, "Bearer browser-secret-abc");
    expect(fetchHeadersAt(0)["x-test-case"]).toBe("strip-authorization");
  });
});
