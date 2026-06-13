import { describe, expect, it } from "vitest";

import { defaultSessionVerbosityForProvider } from "./sessionVerbosity";

describe("defaultSessionVerbosityForProvider", () => {
  it("defaults Claude providers to verbose", () => {
    expect(defaultSessionVerbosityForProvider("claude")).toBe("verbose");
    expect(defaultSessionVerbosityForProvider("claude-crp")).toBe("verbose");
    expect(defaultSessionVerbosityForProvider(" Claude-CRP ")).toBe("verbose");
  });

  it("defaults other providers to default", () => {
    expect(defaultSessionVerbosityForProvider("codex")).toBe("default");
    expect(defaultSessionVerbosityForProvider("gemini")).toBe("default");
    expect(defaultSessionVerbosityForProvider(null)).toBe("default");
  });
});
