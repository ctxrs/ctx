import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { TeamEnterpriseSection } from "./TeamEnterpriseSection";

describe("TeamEnterpriseSection", () => {
  it("renders the public-export unavailable state", () => {
    render(
      <TeamEnterpriseSection
        hostedServicesConfigured={false}
        billingUser={null}
        entitlementsBusy={false}
        plan="free_local"
        entitlements={null}
        cloudState={null}
        cloudBusy={false}
        cloudError={null}
        actionBusy={false}
        actionError={null}
        actionNotice={null}
        orgName=""
        onOrgNameChange={vi.fn()}
        inviteEmail=""
        onInviteEmailChange={vi.fn()}
        inviteRole="member"
        onInviteRoleChange={vi.fn()}
        seatTarget=""
        onSeatTargetChange={vi.fn()}
        policyDraft={null}
        onPolicyDraftChange={vi.fn()}
        onRefresh={vi.fn()}
        onSelectOrg={vi.fn()}
        onCreateOrg={vi.fn()}
        onInviteMember={vi.fn()}
        onAcceptInvite={vi.fn()}
        onUpdateSeats={vi.fn()}
        onSavePolicy={vi.fn()}
        onStartTeamCheckout={vi.fn()}
        onRequestEnterpriseSetup={vi.fn()}
      />,
    );

    expect(screen.getByText(/Hosted team, enterprise, and organization administration/i)).toBeInTheDocument();
  });
});
