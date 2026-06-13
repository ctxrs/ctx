import { describe, expect, it } from "vitest";
import { shouldTrackEntitlementActivated } from "./entitlementAnalytics";

describe("shouldTrackEntitlementActivated", () => {
  it("does not track during first load when prior plan is unknown", () => {
    expect(shouldTrackEntitlementActivated(null, "pro")).toBe(false);
  });

  it("does not track when next plan is unknown", () => {
    expect(shouldTrackEntitlementActivated("free_local", null)).toBe(false);
    expect(shouldTrackEntitlementActivated("pro", null)).toBe(false);
  });

  it("tracks only free to paid transitions", () => {
    expect(shouldTrackEntitlementActivated("free_local", "pro")).toBe(true);
    expect(shouldTrackEntitlementActivated("free_local", "team")).toBe(true);
    expect(shouldTrackEntitlementActivated("pro", "team")).toBe(false);
    expect(shouldTrackEntitlementActivated("team", "enterprise")).toBe(false);
    expect(shouldTrackEntitlementActivated("pro", "free_local")).toBe(false);
  });
});
