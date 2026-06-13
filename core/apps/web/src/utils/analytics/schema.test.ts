import { describe, expect, it } from "vitest";
import { sanitizeAnalyticsProperties } from "./schema";

describe("sanitizeAnalyticsProperties", () => {
  it("keeps supported scalar values", () => {
    const out = sanitizeAnalyticsProperties({
      provider_id: "codex",
      duration_ms: 123,
      enabled: true,
    });
    expect(out).toEqual({
      provider_id: "codex",
      duration_ms: 123,
      enabled: true,
    });
  });

  it("drops forbidden and unsupported fields", () => {
    const out = sanitizeAnalyticsProperties({
      prompt_text: "secret",
      api_key: "nope",
      auth_token: "still nope",
      total_tokens_estimate: 42,
      input_tokens: 21,
      nested: { a: 1 },
      list: ["x"],
      ok_key: "ok",
    });
    expect(out).toEqual({
      total_tokens_estimate: 42,
      input_tokens: 21,
      ok_key: "ok",
    });
  });

  it("truncates very long strings", () => {
    const long = "x".repeat(1000);
    const out = sanitizeAnalyticsProperties({ note: long });
    expect(typeof out.note).toBe("string");
    expect((out.note as string).length).toBe(512);
  });
});
