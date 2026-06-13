import { beforeEach, describe, expect, it, vi } from "vitest";

const SESSION_CONNECTION_KEY = "ctxDaemonConnectionV1";
const LOCAL_PERSISTED_BASE_KEY = "ctxDaemonConnectionBaseV1";
const MOBILE_PERSISTED_CONNECTION_KEY = "ctxMobileDaemonConnectionV1";

const setUserAgent = (value: string) => {
  Object.defineProperty(window.navigator, "userAgent", {
    configurable: true,
    value,
  });
};

const setPlatform = (value: string) => {
  Object.defineProperty(window.navigator, "platform", {
    configurable: true,
    value,
  });
};

describe("daemonConnection", () => {
  beforeEach(() => {
    vi.resetModules();
    vi.unstubAllEnvs();
    vi.stubEnv("VITE_CTX_DAEMON_URL", "");
    vi.stubEnv("VITE_CTX_AUTH_TOKEN", "");
    sessionStorage.clear();
    localStorage.clear();
    window.history.replaceState({}, "", "/");
    const g = globalThis as typeof globalThis & { __TAURI_INTERNALS__?: unknown; __TAURI__?: unknown };
    delete g.__TAURI_INTERNALS__;
    delete g.__TAURI__;
    setUserAgent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)");
    setPlatform("MacIntel");
  });

  it("normalizes base/ws urls consistently", async () => {
    const mod = await import("./daemonConnection");

    expect(mod.normalizeDaemonBaseUrl("https://example.com///")).toBe("https://example.com");
    expect(mod.normalizeDaemonBaseUrl("wss://example.com///")).toBe("https://example.com");
    expect(mod.normalizeDaemonWsBaseUrl("https://example.com///")).toBe("wss://example.com");
    expect(mod.normalizeDaemonWsBaseUrl("ws://example.com///")).toBe("ws://example.com");
    expect(mod.deriveDaemonWsBaseUrl("http://127.0.0.1:4399")).toBe("ws://127.0.0.1:4399");
  });

  it("derives canonical readiness from the shared base-plus-token rule", async () => {
    const mod = await import("./daemonConnection");

    expect(mod.getDaemonConnectionReadiness({ baseUrl: null, authToken: null })).toMatchObject({
      hasBaseUrl: false,
      hasAuthToken: false,
      isReady: false,
      missing: "base",
    });
    expect(mod.getDaemonConnectionReadiness({ baseUrl: "http://127.0.0.1:4399", authToken: null })).toMatchObject({
      hasBaseUrl: true,
      hasAuthToken: false,
      isReady: false,
      missing: "auth",
    });
    expect(mod.hasReadyDaemonConnection({ baseUrl: "http://127.0.0.1:4399", authToken: "abc" })).toBe(true);
  });

  it("restores canonical base from persisted canonical storage", async () => {
    localStorage.setItem(
      LOCAL_PERSISTED_BASE_KEY,
      JSON.stringify({
        v: 1,
        baseUrl: "http://127.0.0.1:4399",
        wsBaseUrl: "ws://127.0.0.1:4399",
      }),
    );

    const mod = await import("./daemonConnection");
    const connection = mod.getDaemonConnection();

    expect(connection.baseUrl).toBe("http://127.0.0.1:4399");
    expect(connection.wsBaseUrl).toBe("ws://127.0.0.1:4399");
    expect(connection.authToken).toBeNull();
    expect(connection.targetScope).toEqual({
      kind: "browser",
      baseUrl: "http://127.0.0.1:4399",
    });
    expect(sessionStorage.getItem(SESSION_CONNECTION_KEY)).toContain('"v":1');
  });

  it("notifies subscribers only when values change", async () => {
    const mod = await import("./daemonConnection");
    const listener = vi.fn();
    const unsubscribe = mod.subscribeDaemonConnection(listener);

    mod.setDaemonConnection({ baseUrl: "http://127.0.0.1:4399", authToken: "abc" });
    expect(listener).toHaveBeenCalledTimes(1);
    expect(listener.mock.calls[0][0]).toMatchObject({
      baseUrl: "http://127.0.0.1:4399",
      wsBaseUrl: "ws://127.0.0.1:4399",
      authToken: "abc",
      targetScope: {
        kind: "browser",
        baseUrl: "http://127.0.0.1:4399",
      },
    });

    mod.setDaemonConnection({ baseUrl: "http://127.0.0.1:4399", authToken: "abc" });
    expect(listener).toHaveBeenCalledTimes(1);

    mod.setDaemonConnection({ authToken: "def" });
    expect(listener).toHaveBeenCalledTimes(2);

    unsubscribe();
  });

  it("clears session and persisted base keys atomically", async () => {
    const mod = await import("./daemonConnection");
    mod.setDaemonConnection(
      {
        baseUrl: "http://127.0.0.1:4399",
        authToken: "abc",
      },
      { persistBaseUrl: true },
    );

    expect(localStorage.getItem(LOCAL_PERSISTED_BASE_KEY)).toContain('"v":1');

    mod.clearDaemonConnection({ persistBaseUrl: true, clearPersistedBaseUrl: true });
    const connection = mod.getDaemonConnection();

    expect(connection.baseUrl).toBeNull();
    expect(connection.wsBaseUrl).toBeNull();
    expect(connection.authToken).toBeNull();
    expect(connection.targetScope).toBeNull();
    expect(localStorage.getItem(LOCAL_PERSISTED_BASE_KEY)).toBeNull();
  });

  it("republishes missing desktop bridge state by clearing canonical connection", async () => {
    const mod = await import("./daemonConnection");
    mod.setDaemonConnection(
      {
        baseUrl: "http://127.0.0.1:4399",
        authToken: "abc",
      },
      { persistBaseUrl: true },
    );

    mod.applyDesktopDaemonConnection(null);
    const connection = mod.getDaemonConnection();

    expect(connection.baseUrl).toBeNull();
    expect(connection.wsBaseUrl).toBeNull();
    expect(connection.authToken).toBeNull();
    expect(connection.source).toBe("desktop");
    expect(connection.targetScope).toBeNull();
    expect(localStorage.getItem(LOCAL_PERSISTED_BASE_KEY)).toBeNull();
  });

  it("scrubs persisted desktop auth tokens during restore", async () => {
    sessionStorage.setItem(
      SESSION_CONNECTION_KEY,
      JSON.stringify({
        v: 1,
        baseUrl: "http://127.0.0.1:4399",
        wsBaseUrl: "ws://127.0.0.1:4399",
        authToken: "stale-desktop-token",
        source: "desktop",
      }),
    );

    const mod = await import("./daemonConnection");
    const connection = mod.getDaemonConnection();

    expect(connection).toMatchObject({
      baseUrl: "http://127.0.0.1:4399",
      wsBaseUrl: "ws://127.0.0.1:4399",
      authToken: null,
      source: "desktop",
    });
    expect(JSON.parse(sessionStorage.getItem(SESSION_CONNECTION_KEY) ?? "{}")).toMatchObject({
      v: 1,
      baseUrl: "http://127.0.0.1:4399",
      wsBaseUrl: "ws://127.0.0.1:4399",
      authToken: null,
      source: "desktop",
    });
  });

  it("treats desktop ssh metadata as part of connection identity even when baseUrl is reused", async () => {
    const mod = await import("./daemonConnection");
    const listener = vi.fn();
    const unsubscribe = mod.subscribeDaemonConnection(listener);

    mod.applyDesktopDaemonConnection({
      kind: "ssh",
      base_url: "http://127.0.0.1:4399",
      browser_query_secret: "browser-secret-abc",
      host: "host-a.example",
      user: "alice",
      remote_port: 4399,
      remote_data_dir: "/srv/ctx-a",
    });
    mod.applyDesktopDaemonConnection({
      kind: "ssh",
      base_url: "http://127.0.0.1:4399",
      browser_query_secret: "browser-secret-abc",
      host: "host-b.example",
      user: "alice",
      remote_port: 4399,
      remote_data_dir: "/srv/ctx-a",
    });

    const connection = mod.getDaemonConnection();
    expect(listener).toHaveBeenCalledTimes(2);
    expect(connection).toMatchObject({
      baseUrl: "http://127.0.0.1:4399",
      authToken: "browser-secret-abc",
      source: "desktop",
      targetScope: {
        kind: "desktop_ssh",
        host: "host-b.example",
        user: "alice",
        port: 4399,
        dataDir: "/srv/ctx-a",
      },
    });
    expect(JSON.parse(sessionStorage.getItem(SESSION_CONNECTION_KEY) ?? "{}")).toMatchObject({
      v: 1,
      baseUrl: "http://127.0.0.1:4399",
      authToken: null,
      source: "desktop",
    });
    expect(String(JSON.parse(sessionStorage.getItem(SESSION_CONNECTION_KEY) ?? "{}").targetScope)).toContain("host-b.example");
    expect(String(JSON.parse(localStorage.getItem(LOCAL_PERSISTED_BASE_KEY) ?? "{}").targetScope)).toContain("host-b.example");

    unsubscribe();
  });

  it("ignores legacy split keys when canonical state is absent", async () => {
    sessionStorage.setItem("ctxAuthToken", "legacy-token");
    localStorage.setItem("contextDaemonBaseUrl", "http://127.0.0.1:4399");

    const mod = await import("./daemonConnection");
    const connection = mod.getDaemonConnection();

    expect(connection.baseUrl).toBe(window.location.origin);
    expect(connection.wsBaseUrl).toBe(window.location.origin.replace(/^http/, "ws"));
    expect(connection.authToken).toBeNull();
    expect(sessionStorage.getItem(SESSION_CONNECTION_KEY)).toContain('"v":1');
  });

  it("seeds same-origin base automatically in browser mode", async () => {
    const mod = await import("./daemonConnection");
    const connection = mod.getDaemonConnection();

    expect(connection.baseUrl).toBe(window.location.origin);
    expect(connection.wsBaseUrl).toBe(window.location.origin.replace(/^http/, "ws"));
    expect(connection.targetScope).toEqual({
      kind: "browser",
      baseUrl: window.location.origin,
    });
  });

  it("does not same-origin bootstrap daemon base in desktop windows", async () => {
    const g = globalThis as typeof globalThis & { __TAURI__?: unknown };
    g.__TAURI__ = {};
    const mod = await import("./daemonConnection");
    const connection = mod.getDaemonConnection();

    expect(connection.baseUrl).toBeNull();
    expect(connection.wsBaseUrl).toBeNull();
    expect(connection.targetScope).toBeNull();
  });

  it("restores a fully persisted mobile daemon connection in tauri mobile shells", async () => {
    const g = globalThis as typeof globalThis & { __TAURI__?: unknown };
    g.__TAURI__ = {};
    setUserAgent("Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X)");
    setPlatform("iPhone");
    localStorage.setItem(
      MOBILE_PERSISTED_CONNECTION_KEY,
      JSON.stringify({
        v: 1,
        baseUrl: "https://daemon.example.com",
        wsBaseUrl: "wss://daemon.example.com",
        authToken: "mobile-token",
        source: "mobile_manual_connect",
      }),
    );

    const mod = await import("./daemonConnection");
    const connection = mod.getDaemonConnection();

    expect(connection).toMatchObject({
      baseUrl: "https://daemon.example.com",
      wsBaseUrl: "wss://daemon.example.com",
      authToken: "mobile-token",
      source: "mobile_manual_connect",
      targetScope: {
        kind: "browser",
        baseUrl: "https://daemon.example.com",
      },
    });
  });

  it("restores a persisted managed mobile secure connection in tauri mobile shells", async () => {
    const g = globalThis as typeof globalThis & { __TAURI__?: unknown };
    g.__TAURI__ = {};
    setUserAgent("Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X)");
    setPlatform("iPhone");
    localStorage.setItem(
      MOBILE_PERSISTED_CONNECTION_KEY,
      JSON.stringify({
        v: 1,
        baseUrl: "https://tunnel.ctx.rs/t/tunnel-1",
        wsBaseUrl: "wss://tunnel.ctx.rs/t/tunnel-1",
        authToken: null,
        source: "mobile_managed_qr",
        targetScope: JSON.stringify(["browser", "https://tunnel.ctx.rs/t/tunnel-1"]),
        mobileSecure: {
          kind: "managed_tunnel",
          deviceId: "33333333-3333-3333-3333-333333333333",
          daemonPublicKey: "daemon-public-key",
          pairingRequestEncryption: "x25519-hkdf-sha256-xchacha20poly1305-v1",
          nextSeq: 7,
        },
      }),
    );

    const mod = await import("./daemonConnection");
    const connection = mod.getDaemonConnection();

    expect(connection).toMatchObject({
      baseUrl: "https://tunnel.ctx.rs/t/tunnel-1",
      wsBaseUrl: "wss://tunnel.ctx.rs/t/tunnel-1",
      authToken: null,
      source: "mobile_managed_qr",
      mobileSecure: {
        kind: "managed_tunnel",
        deviceId: "33333333-3333-3333-3333-333333333333",
        nextSeq: 7,
      },
      targetScope: {
        kind: "browser",
        baseUrl: "https://tunnel.ctx.rs/t/tunnel-1",
      },
    });
  });

  it("persists and clears mobile daemon auth when requested from tauri mobile shells", async () => {
    const g = globalThis as typeof globalThis & { __TAURI_INTERNALS__?: unknown };
    g.__TAURI_INTERNALS__ = {};
    setUserAgent("Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X)");
    setPlatform("iPhone");
    const mod = await import("./daemonConnection");

    mod.setDaemonConnection(
      {
        baseUrl: "https://daemon.example.com",
        authToken: "mobile-token",
        source: "mobile_manual_connect",
      },
      { persistBaseUrl: true, persistAuthToken: true },
    );

    expect(JSON.parse(localStorage.getItem(MOBILE_PERSISTED_CONNECTION_KEY) ?? "{}")).toMatchObject({
      v: 1,
      baseUrl: "https://daemon.example.com",
      authToken: "mobile-token",
    });
    expect(JSON.parse(localStorage.getItem(LOCAL_PERSISTED_BASE_KEY) ?? "{}")).toMatchObject({
      v: 1,
      baseUrl: "https://daemon.example.com",
    });

    mod.clearDaemonConnection({
      persistBaseUrl: true,
      clearPersistedBaseUrl: true,
      clearPersistedAuthToken: true,
    });

    expect(localStorage.getItem(MOBILE_PERSISTED_CONNECTION_KEY)).toBeNull();
    expect(localStorage.getItem(LOCAL_PERSISTED_BASE_KEY)).toBeNull();
  });

  it("persists managed mobile secure connections in tauri mobile shells", async () => {
    const g = globalThis as typeof globalThis & { __TAURI_INTERNALS__?: unknown };
    g.__TAURI_INTERNALS__ = {};
    setUserAgent("Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X)");
    setPlatform("iPhone");
    const mod = await import("./daemonConnection");

    mod.setDaemonConnection(
      {
        baseUrl: "https://tunnel.ctx.rs/t/tunnel-1",
        authToken: null,
        source: "mobile_managed_qr",
        mobileSecure: {
          kind: "managed_tunnel",
          deviceId: "33333333-3333-3333-3333-333333333333",
          daemonPublicKey: "daemon-public-key",
          pairingRequestEncryption: "x25519-hkdf-sha256-xchacha20poly1305-v1",
          nextSeq: 1,
        },
      },
      { persistBaseUrl: true, persistAuthToken: true },
    );

    expect(mod.getDaemonConnection()).toMatchObject({
      baseUrl: "https://tunnel.ctx.rs/t/tunnel-1",
      wsBaseUrl: "wss://tunnel.ctx.rs/t/tunnel-1",
      authToken: null,
      source: "mobile_managed_qr",
      mobileSecure: {
        kind: "managed_tunnel",
        deviceId: "33333333-3333-3333-3333-333333333333",
        nextSeq: 1,
      },
      targetScope: {
        kind: "browser",
        baseUrl: "https://tunnel.ctx.rs/t/tunnel-1",
      },
    });
    expect(JSON.parse(localStorage.getItem(MOBILE_PERSISTED_CONNECTION_KEY) ?? "{}")).toMatchObject({
      v: 1,
      baseUrl: "https://tunnel.ctx.rs/t/tunnel-1",
      authToken: null,
      source: "mobile_managed_qr",
      mobileSecure: {
        kind: "managed_tunnel",
        deviceId: "33333333-3333-3333-3333-333333333333",
        nextSeq: 1,
      },
    });
  });

  it("applies dev env daemon url even after same-origin preseed", async () => {
    vi.stubEnv("VITE_CTX_DAEMON_URL", "http://127.0.0.1:4399");
    const mod = await import("./daemonConnection");

    // Ensure preseed happened.
    expect(mod.getDaemonConnection().baseUrl).toBe(window.location.origin);

    mod.bootstrapDaemonConnectionFromRuntime();
    const connection = mod.getDaemonConnection();
    expect(connection.baseUrl).toBe("http://127.0.0.1:4399");
    expect(connection.wsBaseUrl).toBe("ws://127.0.0.1:4399");
  });

  it("refreshes stale stored auth token from dev env", async () => {
    vi.stubEnv("VITE_CTX_AUTH_TOKEN", "fresh-token");
    const mod = await import("./daemonConnection");
    mod.setDaemonConnection({ authToken: "stale-token", source: "test" });
    expect(mod.getDaemonConnection().authToken).toBe("stale-token");

    mod.bootstrapDaemonConnectionFromRuntime();
    expect(mod.getDaemonConnection().authToken).toBe("fresh-token");
  });

  it("keeps fragment token precedence over dev env token", async () => {
    vi.stubEnv("VITE_CTX_AUTH_TOKEN", "env-token");
    const mod = await import("./daemonConnection");
    window.history.replaceState({}, "", "/#token=url-token");

    mod.bootstrapDaemonConnectionFromRuntime();
    const connection = mod.getDaemonConnection();
    expect(connection.authToken).toBe("url-token");
    expect(window.location.search).toBe("");
    expect(window.location.hash).toBe("");
  });

  it("resets stale stored browser base to same-origin when fragment token is present", async () => {
    sessionStorage.setItem(
      SESSION_CONNECTION_KEY,
      JSON.stringify({
        v: 1,
        baseUrl: "http://127.0.0.1:4411",
        wsBaseUrl: "ws://127.0.0.1:4411",
        authToken: "stale-token",
        source: "dev_env",
      }),
    );
    localStorage.setItem(
      LOCAL_PERSISTED_BASE_KEY,
      JSON.stringify({
        v: 1,
        baseUrl: "http://127.0.0.1:4411",
        wsBaseUrl: "ws://127.0.0.1:4411",
      }),
    );

    const mod = await import("./daemonConnection");
    window.history.replaceState({}, "", "/workspaces/ws-1#token=url-token");

    mod.bootstrapDaemonConnectionFromRuntime();

    const connection = mod.getDaemonConnection();
    expect(connection.baseUrl).toBe(window.location.origin);
    expect(connection.wsBaseUrl).toBe(window.location.origin.replace(/^http/, "ws"));
    expect(connection.authToken).toBe("url-token");
    expect(connection.source).toBe("url_token");
    expect(localStorage.getItem(LOCAL_PERSISTED_BASE_KEY)).toBeNull();
    expect(window.location.search).toBe("");
    expect(window.location.hash).toBe("");
  });

  it("keeps explicit dev env daemon target authoritative when fragment token is present", async () => {
    vi.stubEnv("VITE_CTX_DAEMON_URL", "http://127.0.0.1:4399");
    vi.stubEnv("VITE_CTX_AUTH_TOKEN", "env-token");
    sessionStorage.setItem(
      SESSION_CONNECTION_KEY,
      JSON.stringify({
        v: 1,
        baseUrl: "http://127.0.0.1:4411",
        wsBaseUrl: "ws://127.0.0.1:4411",
        authToken: "stale-token",
        source: "dev_env",
      }),
    );

    const mod = await import("./daemonConnection");
    window.history.replaceState({}, "", "/workspaces/ws-1#token=url-token");

    mod.bootstrapDaemonConnectionFromRuntime();

    const connection = mod.getDaemonConnection();
    expect(connection.baseUrl).toBe("http://127.0.0.1:4399");
    expect(connection.wsBaseUrl).toBe("ws://127.0.0.1:4399");
    expect(connection.authToken).toBe("url-token");
    expect(window.location.search).toBe("");
    expect(window.location.hash).toBe("");
  });

  it("ignores query-string tokens during browser bootstrap", async () => {
    vi.stubEnv("VITE_CTX_AUTH_TOKEN", "env-token");
    const mod = await import("./daemonConnection");
    window.history.replaceState({}, "", "/workspaces/ws-1?token=url-token");

    mod.bootstrapDaemonConnectionFromRuntime();

    const connection = mod.getDaemonConnection();
    expect(connection.authToken).toBe("env-token");
    expect(window.location.search).toBe("");
  });
});
