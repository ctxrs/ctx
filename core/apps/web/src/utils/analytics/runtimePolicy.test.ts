import { describe, expect, it } from "vitest";
import { computeAnalyticsCaptureEnabled } from "./runtimePolicy";

describe("computeAnalyticsCaptureEnabled", () => {
  it("defaults to disabled until settings are loaded", () => {
    expect(
      computeAnalyticsCaptureEnabled({
        settingsLoaded: false,
        telemetryEnabled: true,
        isDev: false,
        isTest: false,
        isCi: false,
        isLocalWebOrigin: false,
        devCaptureFlag: undefined,
      }),
    ).toBe(false);
  });

  it("respects telemetry opt-out once loaded", () => {
    expect(
      computeAnalyticsCaptureEnabled({
        settingsLoaded: true,
        telemetryEnabled: false,
        isDev: false,
        isTest: false,
        isCi: false,
        isLocalWebOrigin: false,
        devCaptureFlag: undefined,
      }),
    ).toBe(false);
  });

  it("defaults disabled in dev and supports explicit enable override", () => {
    expect(
      computeAnalyticsCaptureEnabled({
        settingsLoaded: true,
        telemetryEnabled: true,
        isDev: true,
        isTest: false,
        isCi: false,
        isLocalWebOrigin: false,
        devCaptureFlag: undefined,
      }),
    ).toBe(false);
    expect(
      computeAnalyticsCaptureEnabled({
        settingsLoaded: true,
        telemetryEnabled: true,
        isDev: true,
        isTest: false,
        isCi: false,
        isLocalWebOrigin: false,
        devCaptureFlag: "1",
      }),
    ).toBe(true);
  });

  it("requires explicit enable in test and CI contexts", () => {
    expect(
      computeAnalyticsCaptureEnabled({
        settingsLoaded: true,
        telemetryEnabled: true,
        isDev: false,
        isTest: true,
        isCi: false,
        isLocalWebOrigin: false,
        devCaptureFlag: undefined,
      }),
    ).toBe(false);
    expect(
      computeAnalyticsCaptureEnabled({
        settingsLoaded: true,
        telemetryEnabled: true,
        isDev: false,
        isTest: false,
        isCi: true,
        isLocalWebOrigin: false,
        devCaptureFlag: "true",
      }),
    ).toBe(true);
  });

  it("requires explicit enable for production builds served from local web origins", () => {
    expect(
      computeAnalyticsCaptureEnabled({
        settingsLoaded: true,
        telemetryEnabled: true,
        isDev: false,
        isTest: false,
        isCi: false,
        isLocalWebOrigin: true,
        devCaptureFlag: undefined,
      }),
    ).toBe(false);
    expect(
      computeAnalyticsCaptureEnabled({
        settingsLoaded: true,
        telemetryEnabled: true,
        isDev: false,
        isTest: false,
        isCi: false,
        isLocalWebOrigin: true,
        devCaptureFlag: "yes",
      }),
    ).toBe(true);
  });
});
