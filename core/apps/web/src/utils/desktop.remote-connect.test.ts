import { beforeEach, describe, expect, it, vi } from "vitest";

const invokeMock = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/core", () => ({
  invoke: invokeMock,
}));

describe("desktopConnectSsh", () => {
  beforeEach(() => {
    vi.resetModules();
    invokeMock.mockReset();
  });

  it("polls async SSH connect jobs until they succeed", async () => {
    vi.useFakeTimers();
    invokeMock
      .mockResolvedValueOnce("ssh-connect-1")
      .mockResolvedValueOnce({ status: "pending", phase: "starting_remote_daemon" })
      .mockResolvedValueOnce({
        status: "succeeded",
        info: {
          kind: "ssh",
          base_url: "http://127.0.0.1:44099",
          token: "token-1",
          host: "builder.internal",
        },
      })
      .mockResolvedValueOnce({ status: "succeeded" });

    const desktop = await import("./desktop");
    const promise = desktop.desktopConnectSsh({ host: "builder.internal", start_remote: true });

    await vi.advanceTimersByTimeAsync(500);
    await expect(promise).resolves.toEqual({
      kind: "ssh",
      base_url: "http://127.0.0.1:44099",
      token: "token-1",
      host: "builder.internal",
    });

    expect(invokeMock).toHaveBeenNthCalledWith(1, "desktop_connect_ssh_begin", {
      req: { host: "builder.internal", start_remote: true },
    });
    expect(invokeMock).toHaveBeenNthCalledWith(2, "desktop_connect_ssh_poll", {
      req: { job_id: "ssh-connect-1", consume: false },
    });
    expect(invokeMock).toHaveBeenNthCalledWith(3, "desktop_connect_ssh_poll", {
      req: { job_id: "ssh-connect-1", consume: false },
    });
    expect(invokeMock).toHaveBeenNthCalledWith(4, "desktop_connect_ssh_poll", {
      req: { job_id: "ssh-connect-1", consume: true },
    });

    vi.useRealTimers();
  });

  it("surfaces async SSH connect failures and consumes the terminal snapshot", async () => {
    invokeMock
      .mockResolvedValueOnce("ssh-connect-2")
      .mockResolvedValueOnce({
        status: "failed",
        error: "failed to reach remote daemon: permission denied",
      })
      .mockResolvedValueOnce({ status: "failed" });

    const desktop = await import("./desktop");

    await expect(
      desktop.desktopConnectSsh({ host: "builder.internal", start_remote: true }),
    ).rejects.toThrow("failed to reach remote daemon: permission denied");

    expect(invokeMock).toHaveBeenNthCalledWith(1, "desktop_connect_ssh_begin", {
      req: { host: "builder.internal", start_remote: true },
    });
    expect(invokeMock).toHaveBeenNthCalledWith(2, "desktop_connect_ssh_poll", {
      req: { job_id: "ssh-connect-2", consume: false },
    });
    expect(invokeMock).toHaveBeenNthCalledWith(3, "desktop_connect_ssh_poll", {
      req: { job_id: "ssh-connect-2", consume: true },
    });
  });
});
