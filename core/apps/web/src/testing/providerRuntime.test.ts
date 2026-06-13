import fs from "fs";
import os from "os";
import path from "path";
import { describe, expect, it, vi } from "vitest";
import type { JsonRequestLike, JsonResponseLike } from "./providerRuntime";
import {
  bundledOnlyModeAppliesToProvider,
  ensureProviderInstalledAndHealthy,
  normalizeWriteFileContents,
  resolveSessionWorktreeRoot,
  resolveWorkspaceProviderModelId,
  verifyProviderForWorkspace,
  waitForSessionWorkspaceFileContents,
  waitForWorkspaceFileContents,
  waitForTerminalState,
} from "./providerRuntime";

const jsonResponse = (status: number, body: unknown): JsonResponseLike => ({
  ok: () => status >= 200 && status < 300,
  status: () => status,
  json: async () => body,
  text: async () => (typeof body === "string" ? body : JSON.stringify(body)),
});

const createRequest = (
  getImpl: JsonRequestLike["get"],
  postImpl: JsonRequestLike["post"],
): JsonRequestLike => ({
  get: getImpl,
  post: postImpl,
});

const readyUsability = {
  usable: true,
  status: "ready",
  blocking_provider_ids: [],
  recommended_action: "none",
};

const installableUsability = {
  usable: false,
  status: "installable",
  blocking_provider_ids: [],
  recommended_action: "install",
};

const blockedUsability = (reason: string, blockingProviderIds: string[] = []) => ({
  usable: false,
  status: "blocked",
  reason,
  blocking_provider_ids: blockingProviderIds,
  recommended_action: blockingProviderIds.length > 0 ? "resolve_dependency" : "configure_runtime",
});

describe("providerRuntime", () => {
  it("normalizes trailing whitespace for baseline write-file assertions", () => {
    expect(normalizeWriteFileContents("hi\n")).toBe("hi");
    expect(normalizeWriteFileContents("hi\r\n\r\n")).toBe("hi");
    expect(normalizeWriteFileContents("hi")).toBe("hi");
  });

  it("installs a missing provider against the requested host target and rechecks health", async () => {
    const get = vi.fn<JsonRequestLike["get"]>()
      .mockResolvedValueOnce(jsonResponse(200, {
        installed: false,
        health: "missing",
        diagnostics: ["provider not installed"],
        details: {},
        usability: installableUsability,
      }))
      .mockResolvedValueOnce(jsonResponse(200, {
        state: "running",
        last_event: { stage: "download" },
      }))
      .mockResolvedValueOnce(jsonResponse(200, {
        state: "succeeded",
      }))
      .mockResolvedValueOnce(jsonResponse(200, {
        installed: true,
        health: "ok",
        diagnostics: [],
        details: {
          install_target: "host",
          managed_target: "host",
        },
        usability: readyUsability,
      }));
    const post = vi.fn<JsonRequestLike["post"]>()
      .mockResolvedValueOnce(jsonResponse(200, { install_id: "install-1" }));
    const request = createRequest(get, post);

    const status = await ensureProviderInstalledAndHealthy(request, "gemini", "host", {
      pollMs: 0,
      sleep: async () => {},
    });

    expect(status).toMatchObject({
      installed: true,
      health: "ok",
      details: {
        install_target: "host",
        managed_target: "host",
      },
    });
    expect(get).toHaveBeenNthCalledWith(1, "/api/providers/gemini?target=host", { timeout: 30_000 });
    expect(post).toHaveBeenCalledWith("/api/providers/gemini/install?target=host", {
      data: {},
      timeout: 30_000,
    });
    expect(get).toHaveBeenNthCalledWith(2, "/api/providers/install/install-1", { timeout: 30_000 });
    expect(get).toHaveBeenNthCalledWith(4, "/api/providers/gemini?target=host", { timeout: 30_000 });
  });

  it("treats an empty bundled-only provider list as applying to all providers", () => {
    expect(
      bundledOnlyModeAppliesToProvider("opencode", {
        CTX_E2E_BUNDLED_ONLY: "1",
        CTX_E2E_BUNDLED_ONLY_PROVIDERS: " , ",
      }),
    ).toBe(true);
  });

  it("does not invoke provider install in bundled-only mode", async () => {
    const get = vi.fn<JsonRequestLike["get"]>()
      .mockResolvedValueOnce(jsonResponse(200, {
        installed: false,
        health: "missing",
        diagnostics: ["provider not installed"],
        details: {},
        usability: installableUsability,
      }))
      .mockResolvedValueOnce(jsonResponse(200, {
        installed: false,
        health: "missing",
        diagnostics: ["provider not installed"],
        details: {},
        usability: installableUsability,
      }));
    const post = vi.fn<JsonRequestLike["post"]>();
    const request = createRequest(get, post);

    const originalBundledOnly = process.env.CTX_E2E_BUNDLED_ONLY;
    const originalBundledProviders = process.env.CTX_E2E_BUNDLED_ONLY_PROVIDERS;
    process.env.CTX_E2E_BUNDLED_ONLY = "1";
    process.env.CTX_E2E_BUNDLED_ONLY_PROVIDERS = "opencode";

    try {
      await expect(
        ensureProviderInstalledAndHealthy(request, "opencode", "host", {
          pollMs: 0,
          sleep: async () => {},
        }),
      ).rejects.toThrow("provider opencode is not ready");
    } finally {
      if (originalBundledOnly === undefined) {
        delete process.env.CTX_E2E_BUNDLED_ONLY;
      } else {
        process.env.CTX_E2E_BUNDLED_ONLY = originalBundledOnly;
      }
      if (originalBundledProviders === undefined) {
        delete process.env.CTX_E2E_BUNDLED_ONLY_PROVIDERS;
      } else {
        process.env.CTX_E2E_BUNDLED_ONLY_PROVIDERS = originalBundledProviders;
      }
    }

    expect(post).not.toHaveBeenCalled();
    expect(get).toHaveBeenCalledTimes(2);
  });

  it("treats dependency-blocked providers as not ready for use", async () => {
    const get = vi.fn<JsonRequestLike["get"]>()
      .mockResolvedValueOnce(jsonResponse(200, {
        installed: true,
        health: "ok",
        diagnostics: [
          "provider is not ready until required dependencies are installed: claude-cli",
        ],
        details: {
          ready_for_use: "false",
          pending_dependency_ids: "claude-cli",
        },
        usability: blockedUsability(
          "provider is not ready until required dependencies are installed: claude-cli",
          ["claude-cli"],
        ),
      }));
    const post = vi.fn<JsonRequestLike["post"]>();
    const request = createRequest(get, post);

    await expect(
      ensureProviderInstalledAndHealthy(request, "claude-crp", "host", {
        pollMs: 0,
        sleep: async () => {},
      }),
    ).rejects.toThrow("claude-cli");

    expect(post).not.toHaveBeenCalled();
  });

  it("retries workspace verification until the provider reports ok", async () => {
    const get = vi.fn<JsonRequestLike["get"]>();
    const post = vi.fn<JsonRequestLike["post"]>()
      .mockResolvedValueOnce(jsonResponse(200, {
        status: "error",
        message: "models.list probe timed out",
      }))
      .mockResolvedValueOnce(jsonResponse(200, {
        status: "ok",
        probed_at: "2026-03-06T00:00:00Z",
      }));
    const request = createRequest(get, post);

    const payload = await verifyProviderForWorkspace(request, "ws-1", "gemini", {
      pollMs: 0,
      sleep: async () => {},
    });

    expect(payload).toMatchObject({
      status: "ok",
      probed_at: "2026-03-06T00:00:00Z",
    });
    expect(post).toHaveBeenCalledTimes(2);
  });

  it("waits for provider options to publish a model id instead of using a fallback", async () => {
    const get = vi.fn<JsonRequestLike["get"]>()
      .mockResolvedValueOnce(jsonResponse(200, {
        models: {
          models: [],
        },
      }))
      .mockResolvedValueOnce(jsonResponse(200, {
        models: {
          current_model_id: "gemini-2.5-flash",
        },
      }));
    const post = vi.fn<JsonRequestLike["post"]>();
    const request = createRequest(get, post);

    const modelId = await resolveWorkspaceProviderModelId(request, "ws-1", "gemini", {
      pollMs: 0,
      sleep: async () => {},
    });

    expect(modelId).toBe("gemini-2.5-flash");
    expect(get).toHaveBeenCalledTimes(2);
    expect(get).toHaveBeenNthCalledWith(1, "/api/workspaces/ws-1/providers/gemini/options", { timeout: 30_000 });
  });

  it("returns actionable runtime failure details from terminal events", async () => {
    const get = vi.fn<JsonRequestLike["get"]>()
      .mockResolvedValueOnce(jsonResponse(200, {
        activity: {
          last_turn_status: "failed",
        },
        turns: [
          {
            status: "failed",
          },
        ],
        messages: [],
        session: {
          model_id: "gemini-2.5-flash",
        },
        events: [
          {
            event_type: "session.error",
            payload_json: {
              message: "invalid API key",
            },
          },
        ],
      }));
    const post = vi.fn<JsonRequestLike["post"]>();
    const request = createRequest(get, post);

    const state = await waitForTerminalState(request, "session-1", {
      pollMs: 0,
      sleep: async () => {},
    });

    expect(state).toMatchObject({
      done: true,
      terminalStatus: "failed",
      modelId: "gemini-2.5-flash",
    });
    expect(state.errorMessage).toContain("session.error");
    expect(state.errorMessage).toContain("invalid API key");
  });

  it("waits for a workspace file to appear with exact contents", async () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), "ctx-provider-runtime-file-"));
    const relativePath = "hello.md";
    const filePath = path.join(dir, relativePath);
    let currentTime = 0;
    let wroteFile = false;

    const resolved = await waitForWorkspaceFileContents(dir, relativePath, "hi", {
      timeoutMs: 100,
      pollMs: 0,
      now: () => {
        currentTime += 10;
        return currentTime;
      },
      sleep: async () => {
        if (!wroteFile) {
          fs.writeFileSync(filePath, "hi", "utf8");
          wroteFile = true;
        }
      },
    });

    expect(resolved).toBe(filePath);
  });

  it("resolves a session's managed worktree root", async () => {
    const get = vi.fn<JsonRequestLike["get"]>()
      .mockResolvedValueOnce(jsonResponse(200, {
        head: {
          session: {
            worktree_id: "wt-123",
          },
        },
        summary: {},
      }))
      .mockResolvedValueOnce(jsonResponse(200, {
        root_path: "/tmp/ctx-managed-worktree",
      }));
    const post = vi.fn<JsonRequestLike["post"]>();
    const request = createRequest(get, post);

    const rootPath = await resolveSessionWorktreeRoot(request, "session-1");

    expect(rootPath).toBe("/tmp/ctx-managed-worktree");
    expect(get).toHaveBeenNthCalledWith(1, "/api/sessions/session-1/snapshot?limit=1", { timeout: 30_000 });
    expect(get).toHaveBeenNthCalledWith(2, "/api/worktrees/wt-123", { timeout: 30_000 });
  });

  it("waits for a session file in the managed worktree and reports assistant hints on failure", async () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), "ctx-provider-runtime-session-file-"));
    const get = vi.fn<JsonRequestLike["get"]>()
      .mockResolvedValueOnce(jsonResponse(200, {
        head: {
          session: {
            worktree_id: "wt-456",
          },
        },
        summary: {},
      }))
      .mockResolvedValueOnce(jsonResponse(200, {
        root_path: dir,
      }))
      .mockResolvedValueOnce(jsonResponse(200, {
        messages: [
          {
            role: "assistant",
            content: "provider rejected the request because metadata.client_name was empty",
          },
        ],
        turns: [
          {
            status: "completed",
          },
        ],
        events: [],
      }));
    const post = vi.fn<JsonRequestLike["post"]>();
    const request = createRequest(get, post);
    let currentTime = 0;

    await expect(waitForSessionWorkspaceFileContents(request, "session-2", "hello.md", "hi", {
      timeoutMs: 20,
      pollMs: 0,
      requestTimeoutMs: 30_000,
      now: () => {
        currentTime += 10;
        return currentTime;
      },
      sleep: async () => {},
    })).rejects.toThrow(/session_hint=provider rejected the request because metadata\.client_name was empty/);
  });

  it("accepts a session file with trailing newline when normalized contents match", async () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), "ctx-provider-runtime-session-file-pass-"));
    fs.writeFileSync(path.join(dir, "hello.md"), "hi\n");
    const get = vi.fn<JsonRequestLike["get"]>()
      .mockResolvedValueOnce(jsonResponse(200, {
        head: {
          session: {
            worktree_id: "wt-789",
          },
        },
        summary: {},
      }))
      .mockResolvedValueOnce(jsonResponse(200, {
        root_path: dir,
      }));
    const post = vi.fn<JsonRequestLike["post"]>();
    const request = createRequest(get, post);

    await expect(waitForSessionWorkspaceFileContents(request, "session-3", "hello.md", "hi", {
      timeoutMs: 20,
      pollMs: 0,
      requestTimeoutMs: 30_000,
      now: () => 0,
      sleep: async () => {},
    })).resolves.toContain("hello.md");
  });
});
