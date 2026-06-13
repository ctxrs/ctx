import { describe, expect, it } from "vitest";

import {
  bundledOnlyModeAppliesToProvider,
  classifyInstallSmokeFailureCategory,
  shouldSkipBundledOnlyInstall,
  shouldRetryInstallSmokeFirstTurnFailure,
} from "./runtimeInstallSmoke";

describe("runtime install smoke helpers", () => {
  it("requires bundled-only mode before treating a provider as preseeded", () => {
    expect(
      bundledOnlyModeAppliesToProvider("goose", {
        CTX_E2E_BUNDLED_ONLY: "0",
        CTX_E2E_BUNDLED_ONLY_PROVIDERS: "goose",
      }),
    ).toBe(false);
  });

  it("treats an empty bundled-only provider list as all providers", () => {
    expect(
      bundledOnlyModeAppliesToProvider("goose", {
        CTX_E2E_BUNDLED_ONLY: "1",
        CTX_E2E_BUNDLED_ONLY_PROVIDERS: " , ",
      }),
    ).toBe(true);
  });

  it("skips explicit installs only for listed bundled-only providers when enabled", () => {
    const env = {
      CTX_E2E_BUNDLED_ONLY: "1",
      CTX_E2E_BUNDLED_ONLY_PROVIDERS: "goose,openhands",
      CTX_E2E_INSTALL_SMOKE_SKIP_BUNDLED_ONLY_INSTALLS: "1",
    };

    expect(shouldSkipBundledOnlyInstall("goose", env)).toBe(true);
    expect(shouldSkipBundledOnlyInstall("codex", env)).toBe(false);
  });

  it("does not skip installs unless the lane explicitly opts in", () => {
    expect(
      shouldSkipBundledOnlyInstall("goose", {
        CTX_E2E_BUNDLED_ONLY: "1",
        CTX_E2E_BUNDLED_ONLY_PROVIDERS: "goose",
        CTX_E2E_INSTALL_SMOKE_SKIP_BUNDLED_ONLY_INSTALLS: "0",
      }),
    ).toBe(false);
  });

  it("classifies OpenRouter high-demand first-turn failures as external outages", () => {
    expect(
      classifyInstallSmokeFailureCategory(
        "first_turn",
        "runtime install smoke session failed: [error] We're currently experiencing high demand, which may cause temporary errors.",
        null,
      ),
    ).toBe("external_outage");
  });

  it("classifies OpenRouter account/policy failures as environment issues", () => {
    expect(
      classifyInstallSmokeFailureCategory(
        "first_turn",
        "runtime install smoke session failed: [error] unexpected status 402 Payment Required: Insufficient credits.",
        null,
      ),
    ).toBe("environment");
    expect(
      classifyInstallSmokeFailureCategory(
        "first_turn",
        "runtime install smoke session failed: [error] unexpected status 404 Not Found: No endpoints available matching your guardrail restrictions and data policy.",
        null,
      ),
    ).toBe("environment");
  });

  it("retries only bounded first-turn external outages", () => {
    expect(shouldRetryInstallSmokeFirstTurnFailure("first_turn", "external_outage", 1, 3)).toBe(true);
    expect(shouldRetryInstallSmokeFirstTurnFailure("first_turn_request", "external_outage", 2, 3)).toBe(true);
    expect(shouldRetryInstallSmokeFirstTurnFailure("first_turn", "external_outage", 3, 3)).toBe(false);
    expect(shouldRetryInstallSmokeFirstTurnFailure("first_turn", "environment", 1, 3)).toBe(false);
    expect(shouldRetryInstallSmokeFirstTurnFailure("verify", "external_outage", 1, 3)).toBe(false);
  });
});
