import { describe, expect, it } from "vitest";

import {
  isOpenRouterBaseUrl,
  preferredModelIdFromEndpointSummary,
  validateHarnessEndpointConfigForOwnerScope,
} from "./capabilities";

describe("preferredModelIdFromEndpointSummary", () => {
  it("prefers model_override over manual and catalog models", () => {
    expect(preferredModelIdFromEndpointSummary({
      model_override: " openai/gpt-4.1-mini ",
      manual_model_ids: ["anthropic/claude-sonnet-4.5"],
      model_catalog_models: [{ id: "google/gemini-2.5-pro" }],
    })).toBe("openai/gpt-4.1-mini");
  });

  it("falls back to manual and catalog models when needed", () => {
    expect(preferredModelIdFromEndpointSummary({
      model_override: " ",
      manual_model_ids: [" ", "openai/gpt-4.1-mini"],
      model_catalog_models: [{ id: "google/gemini-2.5-pro" }],
    })).toBe("openai/gpt-4.1-mini");
    expect(preferredModelIdFromEndpointSummary({
      model_override: null,
      manual_model_ids: [],
      model_catalog_models: [{ id: " google/gemini-2.5-pro " }],
    })).toBe("google/gemini-2.5-pro");
  });
});

describe("validateHarnessEndpointConfigForOwnerScope", () => {
  it("accepts OpenRouter base URLs and rejects non-OpenRouter Goose configs in any scope", () => {
    expect(isOpenRouterBaseUrl("https://openrouter.ai/api/v1")).toBe(true);
    expect(isOpenRouterBaseUrl("https://openrouter.example/api/v1")).toBe(false);
    expect(validateHarnessEndpointConfigForOwnerScope({
      ownerScopeKind: "host",
      providerId: "goose",
      baseUrl: "https://api.openai.com/v1",
      manualModelIds: ["openai/gpt-4.1-mini"],
      existingPreferredModelId: null,
    })).toBe("Goose currently requires an OpenRouter base URL.");
    expect(validateHarnessEndpointConfigForOwnerScope({
      ownerScopeKind: "workspace",
      providerId: "goose",
      baseUrl: "https://api.openai.com/v1",
      manualModelIds: ["openai/gpt-4.1-mini"],
      existingPreferredModelId: null,
    })).toBe("Goose currently requires an OpenRouter base URL.");
  });

  it("requires a concrete model for cline/goose/openhands saves in any scope", () => {
    for (const ownerScopeKind of ["host", "workspace"] as const) {
      for (const providerId of ["cline", "goose", "openhands"]) {
        expect(validateHarnessEndpointConfigForOwnerScope({
          ownerScopeKind,
          providerId,
          baseUrl: "https://openrouter.ai/api/v1",
          manualModelIds: [],
          existingPreferredModelId: null,
        })).toBe("Configure at least one manual model slug before saving this endpoint.");
      }
    }
  });

  it("allows saves with a valid base URL and an existing or manual model", () => {
    expect(validateHarnessEndpointConfigForOwnerScope({
      ownerScopeKind: "host",
      providerId: "cline",
      baseUrl: "https://openrouter.ai/api/v1",
      manualModelIds: ["openai/gpt-4.1-mini"],
      existingPreferredModelId: null,
    })).toBeNull();
    expect(validateHarnessEndpointConfigForOwnerScope({
      ownerScopeKind: "workspace",
      providerId: "goose",
      baseUrl: "https://openrouter.ai/api/v1",
      manualModelIds: ["openai/gpt-4.1-mini"],
      existingPreferredModelId: null,
    })).toBeNull();
    expect(validateHarnessEndpointConfigForOwnerScope({
      ownerScopeKind: "host",
      providerId: "openhands",
      baseUrl: "https://openrouter.ai/api/v1",
      manualModelIds: [],
      existingPreferredModelId: "openai/gpt-4.1-mini",
    })).toBeNull();
  });
});
