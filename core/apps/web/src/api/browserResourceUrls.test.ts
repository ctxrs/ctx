import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  BROWSER_CAPABILITY_REFRESH_MARGIN_MS,
  BROWSER_CAPABILITY_TOKEN_TTL_MS,
} from "./browserCapabilityAuth";
import type { DaemonConnection } from "./daemonConnection";

const { getDaemonConnectionMock } = vi.hoisted(() => ({
  getDaemonConnectionMock: vi.fn(),
}));

vi.mock("./daemonConnection", () => ({
  getDaemonConnection: getDaemonConnectionMock,
}));

import {
  browserResourceUrlForScope,
  resetBrowserResourceUrlCacheForTests,
} from "./browserResourceUrls";

const connection = (overrides: Partial<DaemonConnection> = {}): DaemonConnection => ({
  baseUrl: "http://daemon.test",
  wsBaseUrl: "ws://daemon.test",
  authToken: "daemon-secret",
  runId: null,
  targetScope: { kind: "desktop_local" },
  ...overrides,
});

describe("browserResourceUrlForScope", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    resetBrowserResourceUrlCacheForTests();
    getDaemonConnectionMock.mockReturnValue(connection());
  });

  it("reuses the same signed URL across ordinary clock ticks", () => {
    const scope = { kind: "session_artifact", sessionId: "session-1", artifactId: "artifact-1" } as const;
    const first = browserResourceUrlForScope(scope, { nowMs: 1_761_600_000_000 });
    const second = browserResourceUrlForScope(scope, { nowMs: 1_761_600_002_000 });

    expect(second.url).toBe(first.url);
    expect(second.expiresAt).toBe(first.expiresAt);
  });

  it("refreshes the signed URL near token expiry", () => {
    const scope = { kind: "session_artifact", sessionId: "session-1", artifactId: "artifact-1" } as const;
    const first = browserResourceUrlForScope(scope, { nowMs: 1_761_600_000_000 });
    const nearExpiry = 1_761_600_000_000 + BROWSER_CAPABILITY_TOKEN_TTL_MS - BROWSER_CAPABILITY_REFRESH_MARGIN_MS + 1;
    const second = browserResourceUrlForScope(scope, { nowMs: nearExpiry });

    expect(second.url).not.toBe(first.url);
    expect(second.expiresAt).toBeGreaterThan(first.expiresAt);
  });

  it("invalidates by auth token, daemon base URL, and resource scope", () => {
    const scope = { kind: "blob", blobId: "blob-1" } as const;
    const first = browserResourceUrlForScope(scope, { connection: connection(), nowMs: 1_761_600_000_000 });
    const nextAuth = browserResourceUrlForScope(scope, {
      connection: connection({ authToken: "other-secret" }),
      nowMs: 1_761_600_001_000,
    });
    const nextBase = browserResourceUrlForScope(scope, {
      connection: connection({ baseUrl: "http://other-daemon.test" }),
      nowMs: 1_761_600_001_000,
    });
    const nextScope = browserResourceUrlForScope({ kind: "blob", blobId: "blob-2" }, {
      connection: connection(),
      nowMs: 1_761_600_001_000,
    });

    expect(nextAuth.url).not.toBe(first.url);
    expect(nextBase.url).not.toBe(first.url);
    expect(nextScope.url).not.toBe(first.url);
  });

  it("does not place the raw daemon token in the generated URL", () => {
    const result = browserResourceUrlForScope({ kind: "blob", blobId: "blob-1" }, {
      connection: connection({ authToken: "raw-daemon-token" }),
      nowMs: 1_761_600_000_000,
    });

    expect(result.url).toContain("token=");
    expect(result.url).not.toContain("raw-daemon-token");
  });

  it("fails explicitly when no browser capability signing token is available", () => {
    expect(() =>
      browserResourceUrlForScope({ kind: "blob", blobId: "blob-1" }, {
        connection: connection({ authToken: null }),
        nowMs: 1_761_600_000_000,
      })
    ).toThrow(/without a daemon auth token/);
  });
});
