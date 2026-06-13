export function TeamEnterpriseSection(_props: {
  hostedServicesConfigured: boolean;
  billingUser: unknown;
  entitlementsBusy: boolean;
  plan: "free_local" | "pro" | "team" | "enterprise";
  entitlements: unknown;
  cloudState: unknown;
  cloudBusy: boolean;
  cloudError: string | null;
  actionBusy: boolean;
  actionError: string | null;
  actionNotice: string | null;
  orgName: string;
  onOrgNameChange: (value: string) => void;
  inviteEmail: string;
  onInviteEmailChange: (value: string) => void;
  inviteRole: "owner" | "admin" | "member";
  onInviteRoleChange: (value: "owner" | "admin" | "member") => void;
  seatTarget: string;
  onSeatTargetChange: (value: string) => void;
  policyDraft: unknown;
  onPolicyDraftChange: (value: unknown) => void;
  onRefresh: () => void | Promise<void>;
  onSelectOrg: (orgId: string) => void | Promise<void>;
  onCreateOrg: () => void | Promise<void>;
  onInviteMember: () => void | Promise<void>;
  onAcceptInvite: (inviteToken: string) => void | Promise<void>;
  onUpdateSeats: () => void | Promise<void>;
  onSavePolicy: () => void | Promise<void>;
  onStartTeamCheckout: (interval: "month" | "year") => void | Promise<void>;
  onRequestEnterpriseSetup: () => void | Promise<void>;
}) {
  return (
    <div className="settings-empty">
      Hosted team, enterprise, and organization administration are not included in the public ADE
      export.
    </div>
  );
}
