import { beforeEach, describe, expect, it, vi } from "vitest";

const { getInstallIdMock } = vi.hoisted(() => ({
  getInstallIdMock: vi.fn(() => "install-test"),
}));

vi.mock("./identity", () => ({
  getInstallId: getInstallIdMock,
}));

describe("experiment exposure dedupe store", () => {
  beforeEach(() => {
    vi.resetModules();
    window.localStorage.clear();
    getInstallIdMock.mockReset();
    getInstallIdMock.mockReturnValue("install-test");
  });

  it("tracks and deduplicates within a session", async () => {
    const mod = await import("./experimentExposureDedup");
    expect(mod.hasTrackedExperimentExposure("queued_messages_enabled", "enabled")).toBe(false);
    mod.markExperimentExposureTracked("queued_messages_enabled", "enabled");
    expect(mod.hasTrackedExperimentExposure("queued_messages_enabled", "enabled")).toBe(true);
    expect(mod.hasTrackedExperimentExposure("queued_messages_enabled", "disabled")).toBe(false);
  });

  it("deduplicates across module reloads using localStorage", async () => {
    const first = await import("./experimentExposureDedup");
    first.markExperimentExposureTracked("queued_messages_enabled", "enabled");
    vi.resetModules();
    const second = await import("./experimentExposureDedup");
    expect(second.hasTrackedExperimentExposure("queued_messages_enabled", "enabled")).toBe(true);
  });
});
