import { beforeEach, describe, expect, it, vi } from "vitest";
import { deriveBrowserStreamToken } from "./browserStreamAuth";

vi.mock("./daemonConnection", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./daemonConnection")>();
  return {
    ...actual,
    getDaemonConnection: vi.fn(() => ({
      baseUrl: null,
      wsBaseUrl: null,
      authToken: null,
      runId: null,
      source: null,
    })),
    getDaemonWsUrl: vi.fn((path: string, query?: URLSearchParams) => {
      const qs = query?.toString();
      return qs ? `ws://daemon.test${path}?${qs}` : `ws://daemon.test${path}`;
    }),
  };
});

import { buildExecutionLaunchWsUrl } from "./clientWorkspaces";
import { getDaemonConnection, getDaemonWsUrl } from "./daemonConnection";

describe("clientWorkspaces websocket urls", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("builds execution launch stream URL on canonical daemon host with a scoped query token", async () => {
    vi.spyOn(Date, "now").mockReturnValueOnce(1_700_000_000_000);
    vi.mocked(getDaemonConnection).mockReturnValueOnce({
      baseUrl: null,
      wsBaseUrl: null,
      authToken: "token-1",
      runId: null,
      source: null,
    });

    const expectedExpiresAt = Math.floor((1_700_000_000_000 + 5 * 60 * 1000) / 1000);
    const expectedToken = await deriveBrowserStreamToken("token-1", {
      kind: "execution_launch",
      jobId: "job-1",
    }, expectedExpiresAt);
    const url = await buildExecutionLaunchWsUrl("job-1");
    expect(url).toBe(
      `ws://daemon.test/api/execution/launch/stream?job_id=job-1&expires_at=${expectedExpiresAt}&token=${expectedToken}`,
    );
    expect(url).not.toContain("token=token-1");
    expect(vi.mocked(getDaemonWsUrl)).toHaveBeenCalledTimes(1);
  });

  it("builds execution launch stream URL without token when auth is absent", async () => {
    vi.spyOn(Date, "now").mockReturnValueOnce(1_700_000_000_000);
    vi.mocked(getDaemonConnection).mockReturnValueOnce({
      baseUrl: null,
      wsBaseUrl: null,
      authToken: null,
      runId: null,
      source: null,
    });

    const url = await buildExecutionLaunchWsUrl("job-2");
    expect(url).toContain("ws://daemon.test/api/execution/launch/stream");
    expect(url).toContain("job_id=job-2");
    expect(url).not.toContain("token=");
    expect(vi.mocked(getDaemonWsUrl)).toHaveBeenCalledTimes(1);
  });

  it("derives scoped stream tokens when Web Crypto is unavailable", async () => {
    const originalCrypto = globalThis.crypto;
    vi.stubGlobal("crypto", {});

    try {
      await expect(
        deriveBrowserStreamToken(
          "daemon-secret",
          { kind: "workspace_active_snapshot", workspaceId: "ws-plain-http" },
          1_700_000_300,
        ),
      ).resolves.toBe("977dcede70299dcd9cd20c6217b74dc145166b6664bb81238999c5ce06665454");
    } finally {
      vi.stubGlobal("crypto", originalCrypto);
    }
  });
});
