import { describe, expect, it } from "vitest";
import type { ProviderOptions } from "../api/client";
import { hasConfiguredHarnessAuth } from "./providerAuthStatus";

function baseOptions(): ProviderOptions {
  return {
    provider_id: "codex",
    workspace_id: "ws",
    supports_load: false,
    auth_required: false,
    probed_at: new Date().toISOString(),
  };
}

describe("hasConfiguredHarnessAuth", () => {
  it("requires backend auth readiness for fake provider too", () => {
    expect(hasConfiguredHarnessAuth("fake", undefined)).toBe(false);
    expect(hasConfiguredHarnessAuth("fake", baseOptions())).toBe(false);
    expect(hasConfiguredHarnessAuth("fake", { ...baseOptions(), has_active_auth: true })).toBe(true);
  });

  it("returns true when has_active_auth is true", () => {
    const options = { ...baseOptions(), has_active_auth: true };
    expect(hasConfiguredHarnessAuth("codex", options)).toBe(true);
  });

  it("returns true for selected endpoint source", () => {
    const options = {
      ...baseOptions(),
      source: {
        provider_id: "amp",
        selected_source_kind: "endpoint" as const,
        selected_endpoint_id: "endpoint-1",
        endpoints: [],
      },
    };
    expect(hasConfiguredHarnessAuth("amp", options)).toBe(true);
  });

  it("does not treat subscription selection alone as configured auth", () => {
    const sourceSubscription = {
      ...baseOptions(),
      source: {
        provider_id: "codex",
        selected_source_kind: "subscription" as const,
        selected_endpoint_id: null,
        endpoints: [],
      },
    };
    const authModeSubscription = {
      ...baseOptions(),
      auth_mode: "subscription" as const,
    };
    expect(hasConfiguredHarnessAuth("codex", sourceSubscription)).toBe(false);
    expect(hasConfiguredHarnessAuth("cursor", sourceSubscription)).toBe(false);
    expect(hasConfiguredHarnessAuth("amp", sourceSubscription)).toBe(false);
    expect(hasConfiguredHarnessAuth("codex", authModeSubscription)).toBe(false);
    expect(hasConfiguredHarnessAuth("cursor", authModeSubscription)).toBe(false);
    expect(hasConfiguredHarnessAuth("amp", authModeSubscription)).toBe(false);
  });

  it("returns false when no auth signal is present", () => {
    expect(hasConfiguredHarnessAuth("amp", undefined)).toBe(false);
    expect(hasConfiguredHarnessAuth("amp", baseOptions())).toBe(false);
  });
});
