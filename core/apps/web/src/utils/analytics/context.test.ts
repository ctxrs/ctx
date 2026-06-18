import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("./config", () => ({
  getAnalyticsEnvironment: () => "production",
}));

vi.mock("../runtime", () => ({
  getAppShellKind: () => "desktop",
}));

describe("analytics event context", () => {
  beforeEach(() => {
    delete window.__CTX_DESKTOP_ENV__;
  });

  it("prefers the native desktop environment injected by the app shell", async () => {
    window.__CTX_DESKTOP_ENV__ = {
      os: "linux",
      arch: "x64",
    };

    const { buildEventEnvelope } = await import("./context");

    expect(buildEventEnvelope(1)).toEqual(expect.objectContaining({
      os: "linux",
      arch: "x64",
      surface: "desktop",
      analytics_environment: "production",
    }));
  });

  it("falls back to browser user-agent detection outside the desktop shell", async () => {
    const { buildEventEnvelope } = await import("./context");

    expect(buildEventEnvelope(1)).toEqual(expect.objectContaining({
      os: expect.any(String),
      arch: expect.any(String),
    }));
  });
});
