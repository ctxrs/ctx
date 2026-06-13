import { describe, expect, it } from "vitest";
import { resolveHarnessInstallCandidateStatus, type HarnessInstallProviderRow } from "./wizardTypes";

const baseCandidate = (): HarnessInstallProviderRow => ({
  providerId: "cursor",
  label: "Cursor",
  installed: false,
  healthy: false,
  installSupported: true,
  installRunning: false,
});

describe("resolveHarnessInstallCandidateStatus", () => {
  it("keeps installable blocked candidates ready to start before a local install session exists", () => {
    expect(resolveHarnessInstallCandidateStatus({
      ...baseCandidate(),
      blocked: true,
    })).toBe("ready_to_start");
  });

  it("still reports running when an install is in progress even if the candidate is blocked", () => {
    expect(resolveHarnessInstallCandidateStatus({
      ...baseCandidate(),
      blocked: true,
      installRunning: true,
    })).toBe("running");
  });
});
