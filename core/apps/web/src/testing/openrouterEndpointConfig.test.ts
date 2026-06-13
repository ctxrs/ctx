import { describe, expect, it } from "vitest";

import {
  openRouterEndpointName,
  resolveEndpointModelOverrideTarget,
} from "../../e2e/utils/openrouterEndpointConfig";

describe("resolveEndpointModelOverrideTarget", () => {
  it("creates a dedicated OpenRouter endpoint instead of mutating an unrelated selected endpoint", () => {
    const result = resolveEndpointModelOverrideTarget({
      providerId: "codex",
      endpoints: [
        {
          id: "openai-endpoint",
          name: "codex-default",
          base_url: "https://api.openai.com/v1",
          auth_type: "api_key",
          model_override: "gpt-4.1",
        },
      ],
      selectedEndpointId: "openai-endpoint",
      modelOverride: "openai/gpt-4.1-mini",
      endpointBaseUrl: "https://openrouter.ai/api/v1",
      endpointApiKey: "sk-or-v1-test",
    });

    expect(result.selectedEndpoint).toEqual({});
    expect(result.upsertPayload).toEqual({
      name: openRouterEndpointName("codex"),
      base_url: "https://openrouter.ai/api/v1",
      auth_type: "api_key",
      api_key: "sk-or-v1-test",
      model_override: "openai/gpt-4.1-mini",
    });
  });

  it("updates the existing named OpenRouter endpoint when it already exists", () => {
    const result = resolveEndpointModelOverrideTarget({
      providerId: "codex",
      endpoints: [
        {
          id: "or-endpoint",
          name: "codex-openrouter",
          base_url: "https://openrouter.ai/api/v1",
          auth_type: "api_key",
          api_shape: "openai",
          model_override: "openai/gpt-4.1",
        },
      ],
      selectedEndpointId: "or-endpoint",
      modelOverride: "openai/gpt-4.1-mini",
      endpointBaseUrl: "https://openrouter.ai/api/v1",
      endpointApiKey: "sk-or-v1-test",
    });

    expect(result.selectedEndpoint).toMatchObject({
      id: "or-endpoint",
      name: "codex-openrouter",
    });
    expect(result.upsertPayload).toEqual({
      endpoint_id: "or-endpoint",
      name: "codex-openrouter",
      base_url: "https://openrouter.ai/api/v1",
      auth_type: "api_key",
      api_key: "sk-or-v1-test",
      api_shape: "openai",
      model_override: "openai/gpt-4.1-mini",
    });
  });

  it("reuses the selected endpoint for model-only updates when no endpoint credentials are supplied", () => {
    const result = resolveEndpointModelOverrideTarget({
      providerId: "codex",
      endpoints: [
        {
          id: "selected-endpoint",
          name: "codex-default",
          base_url: "https://api.openai.com/v1",
          auth_type: "api_key",
          model_override: "gpt-4.1",
        },
      ],
      selectedEndpointId: "selected-endpoint",
      modelOverride: "gpt-4.1-mini",
    });

    expect(result.selectedEndpoint).toMatchObject({
      id: "selected-endpoint",
      name: "codex-default",
    });
    expect(result.upsertPayload).toEqual({
      endpoint_id: "selected-endpoint",
      name: "codex-default",
      base_url: "https://api.openai.com/v1",
      auth_type: "api_key",
      model_override: "gpt-4.1-mini",
    });
  });
});
