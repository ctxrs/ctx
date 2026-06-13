import { describe, expect, it } from "vitest";
import type { ProviderOptions } from "../../api/client";
import { formatWorktreeChipLabel, modelIdsFromOptions } from "./WorkbenchPage.utils";

const baseOptions = (providerId: string): ProviderOptions => ({
  provider_id: providerId,
  workspace_id: "ws-test",
  supports_load: false,
  auth_required: false,
  probed_at: new Date().toISOString(),
});

describe("modelIdsFromOptions", () => {
  it("returns current_model_id when the catalog list is empty", () => {
    const options: ProviderOptions = {
      ...baseOptions("codex"),
      models: {
        current_model_id: "openai/gpt-5.2-codex",
        availableModels: [],
      },
    };

    expect(modelIdsFromOptions(options)).toEqual(["openai/gpt-5.2-codex"]);
  });

  it("prefers current_model_id ahead of the available model list", () => {
    const options: ProviderOptions = {
      ...baseOptions("codex"),
      models: {
        current_model_id: "openai/gpt-5.2-codex",
        availableModels: [
          { modelId: "openai/gpt-5.3-codex" },
          { modelId: "openai/gpt-5.2-codex" },
        ],
      },
    };

    expect(modelIdsFromOptions(options)).toEqual([
      "openai/gpt-5.2-codex",
      "openai/gpt-5.3-codex",
    ]);
  });

  it("falls back to the selected endpoint override when models are unavailable", () => {
    const options: ProviderOptions = {
      ...baseOptions("codex"),
      auth_mode: "endpoint",
      source: {
        provider_id: "codex",
        selected_source_kind: "endpoint",
        selected_endpoint_id: "openrouter",
        endpoints: [
          {
            id: "openrouter",
            provider_id: "codex",
            name: "OpenRouter",
            base_url: "https://openrouter.ai/api/v1",
            api_shape: "openai_responses",
            auth_type: "bearer",
            model_override: "openai/gpt-5.2-codex",
            created_at: "2026-03-10T00:00:00.000Z",
            updated_at: "2026-03-10T00:00:00.000Z",
            last_verification_status: "valid",
            last_verification_at: null,
            last_error: null,
            has_api_key: true,
          },
        ],
      },
    };

    expect(modelIdsFromOptions(options)).toEqual(["openai/gpt-5.2-codex"]);
  });

  it("prefers current_model_id ahead of the selected endpoint override", () => {
    const options: ProviderOptions = {
      ...baseOptions("codex"),
      auth_mode: "endpoint",
      models: {
        current_model_id: "openai/gpt-5.3-codex",
        availableModels: [{ modelId: "openai/gpt-5.2-codex" }],
      },
      source: {
        provider_id: "codex",
        selected_source_kind: "endpoint",
        selected_endpoint_id: "openrouter",
        endpoints: [
          {
            id: "openrouter",
            provider_id: "codex",
            name: "OpenRouter",
            base_url: "https://openrouter.ai/api/v1",
            api_shape: "openai_responses",
            auth_type: "bearer",
            model_override: "openai/gpt-5.2-codex",
            created_at: "2026-03-10T00:00:00.000Z",
            updated_at: "2026-03-10T00:00:00.000Z",
            last_verification_status: "valid",
            last_verification_at: null,
            last_error: null,
            has_api_key: true,
          },
        ],
      },
    };

    expect(modelIdsFromOptions(options)).toEqual([
      "openai/gpt-5.3-codex",
      "openai/gpt-5.2-codex",
    ]);
  });

  it("falls back to the available model list when current_model_id is absent", () => {
    const options: ProviderOptions = {
      ...baseOptions("codex"),
      models: {
        availableModels: [
          { modelId: "openai/gpt-5.3-codex" },
          { modelId: "openai/gpt-5.2-codex" },
        ],
      },
    };

    expect(modelIdsFromOptions(options)).toEqual([
      "openai/gpt-5.3-codex",
      "openai/gpt-5.2-codex",
    ]);
  });

  it("accepts name-only catalog entries when checking preferred_model_id availability", () => {
    const options: ProviderOptions = {
      ...baseOptions("codex"),
      preferred_model_id: "openai/gpt-5.4/xhigh",
      models: {
        availableModels: [
          { name: "openai/gpt-5.4/xhigh" },
          { name: "openai/gpt-5.4/medium" },
        ],
      },
    };

    expect(modelIdsFromOptions(options)).toEqual([
      "openai/gpt-5.4/xhigh",
      "openai/gpt-5.4/medium",
    ]);
  });
});

describe("formatWorktreeChipLabel", () => {
  it("falls back to the worktree id before host environment copy", () => {
    expect(
      formatWorktreeChipLabel({
        worktreePath: "",
        worktreeId: "42fe9bb2-6457-4ab5-9ac4-41a0101243ed",
        executionEnvironment: "host",
      }),
    ).toBe("42fe9bb2");
  });

  it("prefers the worktree path label when the root path is known", () => {
    expect(
      formatWorktreeChipLabel({
        worktreePath: "/home/fixture/.ctx/worktrees/ws-1/layout-fixture-worktree",
        worktreeId: "42fe9bb2-6457-4ab5-9ac4-41a0101243ed",
        executionEnvironment: "host",
      }),
    ).toBe("layout-fixture-w...");
  });
});
