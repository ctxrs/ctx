import { describe, expect, it } from "vitest";
import {
  defaultEndpointProviderPresetForHarness,
  defaultShapeForHarnessProvider,
  getHarnessEndpointProviderPreset,
  HARNESS_ENDPOINT_PROVIDER_PRESETS,
  normalizeOptionalBaseUrl,
  nextDefaultEndpointName,
  nextTokenEndpointName,
  supportsOptionalBaseUrlForHarness,
} from "./harnessEndpointProviders";

describe("harnessEndpointProviders", () => {
  it("returns harness-specific default provider presets", () => {
    expect(defaultEndpointProviderPresetForHarness("codex")).toBe("openai");
    expect(defaultEndpointProviderPresetForHarness("claude-crp")).toBe("anthropic");
    expect(defaultEndpointProviderPresetForHarness("gemini")).toBe("google_ai_studio");
    expect(defaultEndpointProviderPresetForHarness("kimi")).toBe("openrouter");
    expect(defaultEndpointProviderPresetForHarness("cursor")).toBe("other");
  });

  it("keeps endpoint presets alphabetized and pins Other as final option", () => {
    const withoutOther = HARNESS_ENDPOINT_PROVIDER_PRESETS.filter((preset) => preset.id !== "other");
    const labels = withoutOther.map((preset) => preset.label);
    const sorted = [...labels].sort((left, right) => left.localeCompare(right, undefined, { sensitivity: "base" }));
    expect(labels).toEqual(sorted);
    expect(HARNESS_ENDPOINT_PROVIDER_PRESETS[HARNESS_ENDPOINT_PROVIDER_PRESETS.length - 1]?.id).toBe("other");
  });

  it("maps default harness shapes", () => {
    expect(defaultShapeForHarnessProvider("codex")).toBe("openai_responses");
    expect(defaultShapeForHarnessProvider("claude-crp")).toBe("anthropic_messages");
  });

  it("falls back to other for unknown provider ids", () => {
    const other = getHarnessEndpointProviderPreset("does-not-exist");
    expect(other.id).toBe("other");
    expect(other.base_url).toBeNull();
  });

  it("uses anthropic v1 base URL for claude-compatible endpoints", () => {
    const anthropic = getHarnessEndpointProviderPreset("anthropic");
    expect(anthropic.base_url).toBe("https://api.anthropic.com/v1");
  });

  it("uses the Cohere OpenAI-compatibility base URL", () => {
    const cohere = getHarnessEndpointProviderPreset("cohere");

    expect(cohere.base_url).toBe("https://api.cohere.com/compatibility/v1");
    expect(cohere.recommended_api_shape).toBe("openai_responses");
  });

  it("attaches provider logos for dropdown rendering", () => {
    const openai = getHarnessEndpointProviderPreset("openai");
    const other = getHarnessEndpointProviderPreset("other");
    expect(openai.logo_src).toContain("OpenAI.svg");
    expect(openai.invert_in_dark).toBe(true);
    expect(other.logo_src).toBeNull();
  });

  it("builds incremental default endpoint names by provider", () => {
    expect(nextDefaultEndpointName("openai", [])).toBe("OpenAI 1");
    expect(nextDefaultEndpointName("openai", ["OpenAI 1", "OpenAI 3"])).toBe("OpenAI 2");
    expect(nextDefaultEndpointName("anthropic", ["Anthropic 1", "Anthropic 2"])).toBe("Anthropic 3");
  });

  it("uses endpoint fallback naming for unknown provider ids", () => {
    expect(nextDefaultEndpointName("not-real", [])).toBe("Other 1");
  });

  it("builds unique token endpoint names per provider", () => {
    expect(nextTokenEndpointName("copilot", [])).toBe("copilot token");
    expect(nextTokenEndpointName("copilot", ["copilot token"])).toBe("copilot token 2");
    expect(nextTokenEndpointName("copilot", ["Copilot Token", "copilot token 2"])).toBe("copilot token 3");
  });

  it("flags providers that support optional base URL", () => {
    expect(supportsOptionalBaseUrlForHarness("cody")).toBe(true);
    expect(supportsOptionalBaseUrlForHarness("pi")).toBe(false);
    expect(supportsOptionalBaseUrlForHarness("cursor")).toBe(false);
    expect(supportsOptionalBaseUrlForHarness("copilot")).toBe(false);
  });

  it("normalizes optional base URL inputs", () => {
    expect(normalizeOptionalBaseUrl("  ")).toBeNull();
    expect(normalizeOptionalBaseUrl(" https://sourcegraph.example.com ")).toBe("https://sourcegraph.example.com");
  });
});
