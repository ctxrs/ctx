import type { PluginExtensionRegistry } from "@ctx/types";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { executePluginCommand } from "../../api/clientSystem";
import { resolvePluginCommandMessage } from "./pluginCommandInvocation";

vi.mock("../../api/clientSystem", () => ({
  executePluginCommand: vi.fn(),
}));

const registry: PluginExtensionRegistry = {
  revision: 1,
  commands: [
    {
      plugin_id: "review.tools",
      plugin_name: "Review Tools",
      plugin_version: "0.1.0",
      plugin_path: "/plugins/review/ctx-plugin.json",
      contribution: {
        id: "review",
        title: "Review Diff",
        entrypoint: "main",
      },
    },
  ],
};

describe("pluginCommandInvocation", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("invokes registered plugin slash commands and returns the plugin message", async () => {
    vi.mocked(executePluginCommand).mockResolvedValue({
      plugin_id: "review.tools",
      command_id: "review",
      status: "completed",
      message: "Review this diff",
    });

    await expect(
      resolvePluginCommandMessage({
        text: "/review.tools:review src/index.ts",
        registry,
        workspaceId: "workspace-1",
        taskId: "task-1",
        sessionId: "session-1",
      }),
    ).resolves.toBe("Review this diff");

    expect(executePluginCommand).toHaveBeenCalledWith({
      plugin_id: "review.tools",
      command_id: "review",
      input: "src/index.ts",
      workspace_id: "workspace-1",
      task_id: "task-1",
      session_id: "session-1",
    });
  });

  it("leaves unknown slash commands unchanged", async () => {
    await expect(
      resolvePluginCommandMessage({
        text: "/model gpt-5.5",
        registry,
      }),
    ).resolves.toBe("/model gpt-5.5");

    expect(executePluginCommand).not.toHaveBeenCalled();
  });

  it("surfaces plugin command failures", async () => {
    vi.mocked(executePluginCommand).mockResolvedValue({
      plugin_id: "review.tools",
      command_id: "review",
      status: "failed",
      error: "entrypoint missing",
    });

    await expect(
      resolvePluginCommandMessage({
        text: "/review.tools:review src/index.ts",
        registry,
      }),
    ).rejects.toThrow("entrypoint missing");
  });
});
