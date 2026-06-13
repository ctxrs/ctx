import { describe, expect, it } from "vitest";
import {
  resolveAnalyticsEnvironment,
  validateProductionAnalyticsBuildConfig,
} from "./config";

describe("resolveAnalyticsEnvironment", () => {
  it("uses explicit override when provided", () => {
    expect(resolveAnalyticsEnvironment("production", "staging")).toBe("production");
    expect(resolveAnalyticsEnvironment("staging", "production")).toBe("staging");
  });

  it("does not infer production analytics from production build mode", () => {
    expect(resolveAnalyticsEnvironment(undefined, "production")).toBe("staging");
    expect(resolveAnalyticsEnvironment(undefined, "prod")).toBe("staging");
  });

  it("forces staging when running in development mode", () => {
    expect(resolveAnalyticsEnvironment(undefined, "development")).toBe("staging");
    expect(resolveAnalyticsEnvironment("production", "development")).toBe("staging");
    expect(resolveAnalyticsEnvironment(undefined, "dev")).toBe("staging");
  });

  it("defaults unknown modes to staging", () => {
    expect(resolveAnalyticsEnvironment(undefined, "staging")).toBe("staging");
    expect(resolveAnalyticsEnvironment(undefined, "preview")).toBe("staging");
  });
});

describe("validateProductionAnalyticsBuildConfig", () => {
  it("allows non-production analytics builds without a release version", () => {
    expect(() =>
      validateProductionAnalyticsBuildConfig({
        explicitAnalyticsEnv: "staging",
        explicitAppVersion: undefined,
        packageVersion: "0.1.0",
      }),
    ).not.toThrow();
  });

  it("requires an explicit app release version for production analytics", () => {
    expect(() =>
      validateProductionAnalyticsBuildConfig({
        explicitAnalyticsEnv: "production",
        explicitAppVersion: undefined,
        packageVersion: "0.1.0",
      }),
    ).toThrow(/VITE_CTX_APP_VERSION/);
  });

  it("rejects the web package version as the production analytics release version", () => {
    expect(() =>
      validateProductionAnalyticsBuildConfig({
        explicitAnalyticsEnv: "production",
        explicitAppVersion: "0.1.0",
        packageVersion: "0.1.0",
      }),
    ).toThrow(/web package version/);
  });

  it("allows production analytics with an explicit desktop release version", () => {
    expect(() =>
      validateProductionAnalyticsBuildConfig({
        explicitAnalyticsEnv: "production",
        explicitAppVersion: "0.64.0",
        packageVersion: "0.1.0",
      }),
    ).not.toThrow();
  });
});
