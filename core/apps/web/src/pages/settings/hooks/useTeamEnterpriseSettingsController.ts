import type { Dispatch, SetStateAction } from "react";
import { useCallback, useState } from "react";
import type { BillingInterval } from "../billingCheckoutFlow";
import type { PlanType } from "../entitlementAnalytics";
import type { SectionId } from "../SettingsPage.types";
import {
  DEFAULT_TEAM_ENTERPRISE_POLICY_DRAFT,
  type EntitlementsSnapshot,
  type MembershipRole,
  type TeamEnterpriseCloudState,
  type TeamEnterprisePolicyDraft,
} from "../teamEnterpriseSettingsApi";

const HOSTED_TEAM_ENTERPRISE_UNAVAILABLE =
  "Hosted team and enterprise services are not included in the public ADE export.";

type HostedAccountUser = { email?: string | null } | null;

type RefreshEntitlements = (opts?: {
  force?: boolean;
  silent?: boolean;
  activeOrgId?: string | null;
}) => Promise<EntitlementsSnapshot | null>;

export type SettingsTeamEnterpriseController = {
  billingUser: HostedAccountUser;
  entitlementsBusy: boolean;
  plan: PlanType;
  entitlements: EntitlementsSnapshot | null;
  cloudState: TeamEnterpriseCloudState;
  cloudBusy: boolean;
  cloudError: string | null;
  actionBusy: boolean;
  actionError: string | null;
  actionNotice: string | null;
  orgName: string;
  setOrgName: (value: string) => void;
  inviteEmail: string;
  setInviteEmail: (value: string) => void;
  inviteRole: MembershipRole;
  setInviteRole: (value: MembershipRole) => void;
  seatTarget: string;
  setSeatTarget: (value: string) => void;
  policyDraft: TeamEnterprisePolicyDraft;
  setPolicyDraft: (value: TeamEnterprisePolicyDraft) => void;
  onRefresh: () => Promise<void>;
  onSelectOrg: (orgId: string) => Promise<void>;
  onCreateOrg: () => Promise<void>;
  onInviteMember: () => Promise<void>;
  onAcceptInvite: (inviteToken: string) => Promise<void>;
  onUpdateSeats: () => Promise<void>;
  onSavePolicy: () => Promise<void>;
  onStartTeamCheckout: (interval: BillingInterval) => Promise<void>;
  onRequestEnterpriseSetup: () => Promise<void>;
};

type Params = {
  active: SectionId;
  hostedServicesClient: unknown;
  billingUser: HostedAccountUser;
  billingReturnPath: string;
  entitlementsBusy: boolean;
  plan: PlanType;
  entitlements: EntitlementsSnapshot | null;
  requestedActiveOrgId: string | null;
  setRequestedActiveOrgId: Dispatch<SetStateAction<string | null>>;
  refreshEntitlements: RefreshEntitlements;
};

const EMPTY_TEAM_ENTERPRISE_CLOUD_STATE: TeamEnterpriseCloudState = {
  orgs: [],
  activeOrgId: null,
  activeOrg: null,
  billingSubjectId: null,
  invites: [],
  subscriptions: [],
  featureGrants: [],
  adminState: null,
  memberDirectoryAvailable: false,
};

export function useTeamEnterpriseSettingsController(params: Params): SettingsTeamEnterpriseController {
  const [actionError, setActionError] = useState<string | null>(null);
  const [orgName, setOrgName] = useState("");
  const [inviteEmail, setInviteEmail] = useState("");
  const [inviteRole, setInviteRole] = useState<MembershipRole>("member");
  const [seatTarget, setSeatTarget] = useState("");
  const [policyDraft, setPolicyDraft] = useState<TeamEnterprisePolicyDraft>(
    DEFAULT_TEAM_ENTERPRISE_POLICY_DRAFT,
  );

  void params.active;
  void params.hostedServicesClient;
  void params.billingReturnPath;
  void params.requestedActiveOrgId;
  void params.setRequestedActiveOrgId;
  void params.refreshEntitlements;

  const reportUnavailable = useCallback(async () => {
    setActionError(HOSTED_TEAM_ENTERPRISE_UNAVAILABLE);
  }, []);

  return {
    billingUser: params.billingUser,
    entitlementsBusy: params.entitlementsBusy,
    plan: params.plan,
    entitlements: params.entitlements,
    cloudState: EMPTY_TEAM_ENTERPRISE_CLOUD_STATE,
    cloudBusy: false,
    cloudError: null,
    actionBusy: false,
    actionError,
    actionNotice: null,
    orgName,
    setOrgName,
    inviteEmail,
    setInviteEmail,
    inviteRole,
    setInviteRole,
    seatTarget,
    setSeatTarget,
    policyDraft,
    setPolicyDraft,
    onRefresh: reportUnavailable,
    onSelectOrg: reportUnavailable,
    onCreateOrg: reportUnavailable,
    onInviteMember: reportUnavailable,
    onAcceptInvite: reportUnavailable,
    onUpdateSeats: reportUnavailable,
    onSavePolicy: reportUnavailable,
    onStartTeamCheckout: reportUnavailable,
    onRequestEnterpriseSetup: reportUnavailable,
  };
}
