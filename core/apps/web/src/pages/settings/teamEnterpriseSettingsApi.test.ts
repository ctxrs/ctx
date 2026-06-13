import { describe, expect, it } from "vitest";
import {
  fetchEntitlementsSnapshot,
  fetchTeamEnterpriseCloudState,
  invokeTeamEnterpriseAdminAction,
  startTeamBillingCheckout,
} from "./teamEnterpriseSettingsApi";

const unavailableMessage =
  "Hosted team and enterprise services are not included in the public ADE export.";

describe("teamEnterpriseSettingsApi", () => {
  it("fails admin actions explicitly in the public export", async () => {
    await expect(
      invokeTeamEnterpriseAdminAction(null, { action: "create_organization", name: "Example" }),
    ).rejects.toThrow(unavailableMessage);
  });

  it("fails hosted state refresh explicitly in the public export", async () => {
    await expect(
      fetchTeamEnterpriseCloudState({ client: null, requestedActiveOrgId: null }),
    ).rejects.toThrow(unavailableMessage);
  });

  it("fails entitlement refresh explicitly in the public export", async () => {
    await expect(fetchEntitlementsSnapshot({ client: null, activeOrgId: null })).rejects.toThrow(
      unavailableMessage,
    );
  });

  it("fails team checkout explicitly in the public export", async () => {
    await expect(
      startTeamBillingCheckout({
        client: null,
        organizationId: "org_local",
        billingSubjectId: "subject_local",
        interval: "month",
        returnPath: "/settings#general",
      }),
    ).rejects.toThrow(unavailableMessage);
  });
});
