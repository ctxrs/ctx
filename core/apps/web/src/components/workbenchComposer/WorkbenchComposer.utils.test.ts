import { describe, expect, it } from "vitest";

import {
  buildModelsForProvider,
  buildModelsFromCatalogPayload,
  modelIdFromProviderOptions,
  nextAutoSeededModelId,
  shouldShowLoadingProviderModels,
} from "./WorkbenchComposer.utils";

describe("buildModelsForProvider", () => {
  it("does not inject a browser-only gemini fallback when provider options are missing", () => {
    expect(buildModelsForProvider("gemini")).toEqual([]);
  });

  it("builds gemini models directly from daemon provider options", () => {
    expect(buildModelsForProvider("gemini", {
      provider_id: "gemini",
      workspace_id: "ws-test",
      supports_load: false,
      auth_required: false,
      probed_at: "2026-03-10T00:00:00.000Z",
      models: {
        availableModels: [
          { modelId: "auto-gemini-3", name: "Auto (Gemini 3)" },
          { modelId: "gemini-3-pro-preview", name: "Gemini 3 Pro Preview" },
        ],
      },
    })).toEqual([
      { id: "auto-gemini-3", name: "Auto (Gemini 3)" },
      { id: "gemini-3-pro-preview", name: "Gemini 3 Pro Preview" },
    ]);
  });

  it("renders Codex subscription model names from lowercase slugs", () => {
    expect(buildModelsForProvider("codex", {
      provider_id: "codex",
      workspace_id: "ws-test",
      supports_load: false,
      auth_required: false,
      probed_at: "2026-03-10T00:00:00.000Z",
      models: {
        models: [
          { id: "gpt-5.5/medium", name: "GPT-5.5 (medium)" },
          { id: "gpt-5.4-mini/xhigh", name: "GPT-5.4-Mini (xhigh)" },
          { id: "gpt-5.3-codex-spark/high", name: "GPT-5.3-Codex-Spark (high)" },
        ],
      },
    })).toEqual([
      { id: "gpt-5.5/medium", name: "gpt-5.5" },
      { id: "gpt-5.4-mini/xhigh", name: "gpt-5.4-mini" },
      { id: "gpt-5.3-codex-spark/high", name: "gpt-5.3-codex-spark" },
    ]);
  });

  it("leaves non-slug Codex endpoint model names alone", () => {
    expect(buildModelsForProvider("codex", {
      provider_id: "codex",
      workspace_id: "ws-test",
      supports_load: false,
      auth_required: false,
      probed_at: "2026-03-10T00:00:00.000Z",
      models: {
        models: [
          { id: "openai/gpt-5.2", name: "GPT-5.2" },
          { id: "anthropic/claude-sonnet-4.6", name: "Claude Sonnet 4.6" },
        ],
      },
    })).toEqual([
      { id: "openai/gpt-5.2", name: "GPT-5.2" },
      { id: "anthropic/claude-sonnet-4.6", name: "Claude Sonnet 4.6" },
    ]);
  });
});

describe("buildModelsFromCatalogPayload", () => {
  it("parses ACP model payloads directly", () => {
    expect(buildModelsFromCatalogPayload({
      current_model_id: "gpt-5.4/medium",
      models: [{ id: "gpt-5.4/medium" }, { id: "gpt-5.4/xhigh" }],
    })).toEqual([
      { id: "gpt-5.4/medium", name: undefined },
      { id: "gpt-5.4/xhigh", name: undefined },
    ]);
  });
});

describe("modelIdFromProviderOptions", () => {
  it("prefers a saved preferred_model_id when it exists in the available catalog", () => {
    expect(modelIdFromProviderOptions({
      provider_id: "codex",
      workspace_id: "ws-test",
      supports_load: false,
      auth_required: false,
      preferred_model_id: "gpt-5.4/xhigh",
      probed_at: "2026-03-10T00:00:00.000Z",
      models: {
        current_model_id: "gpt-5.4/medium",
        models: [{ id: "gpt-5.4/medium" }, { id: "gpt-5.4/xhigh" }],
      },
    })).toBe("gpt-5.4/xhigh");
  });

  it("ignores a saved preferred_model_id when the available catalog does not contain it", () => {
    expect(modelIdFromProviderOptions({
      provider_id: "codex",
      workspace_id: "ws-test",
      supports_load: false,
      auth_required: false,
      preferred_model_id: "gpt-5.4/xhigh",
      probed_at: "2026-03-10T00:00:00.000Z",
      models: {
        current_model_id: "gpt-5.4/medium",
        models: [{ id: "gpt-5.4/medium" }, { id: "gpt-5.4/high" }],
      },
    })).toBe("gpt-5.4/medium");
  });
});

describe("nextAutoSeededModelId", () => {
  it("seeds when the draft model is still blank", () => {
    expect(nextAutoSeededModelId("", "gpt-5.4/xhigh", null)).toBe("gpt-5.4/xhigh");
  });

  it("updates a previously auto-seeded model when a saved preference arrives later", () => {
    expect(nextAutoSeededModelId("gpt-5.4/medium", "gpt-5.4/xhigh", "gpt-5.4/medium")).toBe(
      "gpt-5.4/xhigh",
    );
  });

  it("does not override an explicit user-selected model", () => {
    expect(nextAutoSeededModelId("gpt-5.4/high", "gpt-5.4/xhigh", "gpt-5.4/medium")).toBeNull();
  });
});

describe("shouldShowLoadingProviderModels", () => {
  it("keeps subscription-backed discovery providers in a loading state until models arrive", () => {
    expect(shouldShowLoadingProviderModels("codex", {
      provider_id: "codex",
      workspace_id: "ws-test",
      supports_load: false,
      auth_required: false,
      has_active_auth: true,
      auth_mode: "subscription",
      source: {
        provider_id: "codex",
        selected_source_kind: "subscription",
        selected_endpoint_id: null,
        endpoints: [],
      },
      probed_at: "2026-03-10T00:00:00.000Z",
    })).toBe(true);
  });

  it("keeps cursor in a loading state while subscription model discovery is pending", () => {
    expect(shouldShowLoadingProviderModels("cursor", {
      provider_id: "cursor",
      workspace_id: "ws-test",
      supports_load: false,
      auth_required: false,
      has_active_auth: true,
      auth_mode: "subscription",
      source: {
        provider_id: "cursor",
        selected_source_kind: "subscription",
        selected_endpoint_id: null,
        endpoints: [],
      },
      probed_at: "2026-03-10T00:00:00.000Z",
    })).toBe(true);
  });

  it("keeps amp, gemini, kimi, and qwen in a loading state while discovery is pending", () => {
    for (const providerId of ["amp", "gemini", "kimi", "qwen"]) {
      expect(shouldShowLoadingProviderModels(providerId, {
        provider_id: providerId,
        workspace_id: "ws-test",
        supports_load: false,
        auth_required: false,
        has_active_auth: true,
        auth_mode: "subscription",
        source: {
          provider_id: providerId,
          selected_source_kind: "subscription",
          selected_endpoint_id: null,
          endpoints: [],
        },
        probed_at: "2026-03-10T00:00:00.000Z",
      })).toBe(true);
    }
  });

  it("stops showing loading after a live model probe has already failed", () => {
    expect(shouldShowLoadingProviderModels("qwen", {
      provider_id: "qwen",
      workspace_id: "ws-test",
      supports_load: false,
      probe_ok: false,
      probe_error: "runtime_command_missing: provider=qwen",
      auth_required: false,
      has_active_auth: true,
      auth_mode: "subscription",
      source: {
        provider_id: "qwen",
        selected_source_kind: "subscription",
        selected_endpoint_id: null,
        endpoints: [],
      },
      probed_at: "2026-03-10T00:00:00.000Z",
    })).toBe(false);
  });

  it("does not show loading for endpoint-backed providers without a discovered catalog", () => {
    expect(shouldShowLoadingProviderModels("codex", {
      provider_id: "codex",
      workspace_id: "ws-test",
      supports_load: false,
      auth_required: false,
      has_active_auth: true,
      auth_mode: "endpoint",
      source: {
        provider_id: "codex",
        selected_source_kind: "endpoint",
        selected_endpoint_id: "ep-1",
        endpoints: [
          {
            id: "ep-1",
            provider_id: "codex",
            name: "Primary",
            base_url: "https://api.example.com/v1",
            api_shape: "openai_responses",
            auth_type: "bearer",
            model_override: null,
            created_at: "2026-03-10T00:00:00.000Z",
            updated_at: "2026-03-10T00:00:00.000Z",
            last_verification_status: "valid",
            last_verification_at: "2026-03-10T00:00:00.000Z",
            last_error: null,
            has_api_key: true,
            model_catalog_status: "manual_only",
            model_catalog_fetched_at: null,
            model_catalog_error: null,
            model_catalog_models: [],
            manual_model_ids: [],
            model_catalog_source: null,
          },
        ],
      },
      probed_at: "2026-03-10T00:00:00.000Z",
    })).toBe(false);
  });

  it("stops loading once the provider options already include models", () => {
    expect(shouldShowLoadingProviderModels("claude-crp", {
      provider_id: "claude-crp",
      workspace_id: "ws-test",
      supports_load: false,
      auth_required: false,
      has_active_auth: true,
      auth_mode: "subscription",
      source: {
        provider_id: "claude-crp",
        selected_source_kind: "subscription",
        selected_endpoint_id: null,
        endpoints: [],
      },
      models: {
        models: [{ id: "default/low" }, { id: "default/medium" }],
        current_model_id: "default/medium",
        meta: {
          source_kind: "subscription",
          catalog_source: "runtime_probe_live",
          refresh_pending: false,
        },
      },
      probed_at: "2026-03-10T00:00:00.000Z",
    })).toBe(false);
  });

  it("keeps showing loading while a subscription catalog is still provisional", () => {
    expect(shouldShowLoadingProviderModels("codex", {
      provider_id: "codex",
      workspace_id: "ws-test",
      supports_load: false,
      auth_required: false,
      has_active_auth: true,
      auth_mode: "subscription",
      source: {
        provider_id: "codex",
        selected_source_kind: "subscription",
        selected_endpoint_id: null,
        endpoints: [],
      },
      models: {
        models: [{ id: "gpt-5.4/medium" }],
        current_model_id: "gpt-5.4/medium",
        meta: {
          source_kind: "subscription",
          catalog_source: "codex_bundle_pinned",
          refresh_pending: true,
        },
      },
      probed_at: "2026-03-10T00:00:00.000Z",
    })).toBe(true);
  });
});
