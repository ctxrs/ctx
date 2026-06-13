import type { EnableMobileAccessResponse, MobileAccessStatus } from "../../../api/client";
import type { SectionId } from "../SettingsPage.types";

type PlanType = "free_local" | "pro" | "team" | "enterprise";
type HostedAccountUser = { email?: string | null } | null;

type EntitlementsSnapshot = {
  plan_type: PlanType;
  org_id?: string | null;
  billing_subject?: string | null;
  subject_type?: string | null;
  membership_role?: string | null;
  features?: Record<string, string | undefined>;
};

type TeamEnterpriseCloudState = {
  orgs: unknown[];
  activeOrgId: string | null;
  activeOrg: null;
  billingSubjectId: string | null;
  invites: unknown[];
  subscriptions: unknown[];
  featureGrants: unknown[];
  adminState: null;
  memberDirectoryAvailable: boolean;
};

type TeamEnterprisePolicyDraft = {
  providers: string;
  models: string;
  allowPersonalRoutes: boolean;
  sandboxProfile: string;
  networkProfile: string;
  archiveVisibility: string;
};

type SettingsBillingController = {
  checkoutStatus: string | null;
  billingUser: HostedAccountUser;
  billingEmail: string;
  setBillingEmail: (value: string) => void;
  billingPassword: string;
  setBillingPassword: (value: string) => void;
  billingBusy: boolean;
  billingError: string | null;
  entitlementsBusy: boolean;
  plan: PlanType;
  proEnabled: boolean;
  onSignIn: () => Promise<void>;
  onSignUp: () => Promise<void>;
  onSignOut: () => Promise<void>;
  onStartCheckout: (interval: "month" | "year") => Promise<void>;
  onOpenPortal: () => Promise<void>;
};

type SettingsMobileAccessController = {
  billingUser: HostedAccountUser;
  entitlementsBusy: boolean;
  proEnabled: boolean;
  mobileStatus: MobileAccessStatus | null;
  mobileStatusBusy: boolean;
  mobileStatusError: string | null;
  mobileEnableBusy: boolean;
  mobileEnableError: string | null;
  mobileQr: EnableMobileAccessResponse | null;
  onEnable: () => Promise<void>;
  onDisable: () => Promise<void>;
};

type SettingsTeamEnterpriseController = {
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
  inviteRole: "owner" | "admin" | "member";
  setInviteRole: (value: "owner" | "admin" | "member") => void;
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
  onStartTeamCheckout: (interval: "month" | "year") => Promise<void>;
  onRequestEnterpriseSetup: () => Promise<void>;
};

type SettingsAccountController = {
  hostedServicesConfigured: boolean;
  billing: SettingsBillingController;
  mobileAccess: SettingsMobileAccessController;
  teamEnterprise: SettingsTeamEnterpriseController;
};

type Params = {
  active: SectionId;
  billingReturnPath: string;
  checkoutStatus: string | null;
  checkoutSessionId: string | null;
  clearCheckoutStatus: () => void;
};

const noop = async () => {};
const noopValue = () => {};

const emptyCloudState: TeamEnterpriseCloudState = {
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

const emptyPolicyDraft: TeamEnterprisePolicyDraft = {
  providers: "",
  models: "",
  allowPersonalRoutes: false,
  sandboxProfile: "local",
  networkProfile: "local",
  archiveVisibility: "local_only",
};

export function useSettingsAccountController(_params: Params): SettingsAccountController {
  return {
    hostedServicesConfigured: false,
    billing: {
      checkoutStatus: null,
      billingUser: null,
      billingEmail: "",
      setBillingEmail: noopValue,
      billingPassword: "",
      setBillingPassword: noopValue,
      billingBusy: false,
      billingError: null,
      entitlementsBusy: false,
      plan: "free_local",
      proEnabled: false,
      onSignIn: noop,
      onSignUp: noop,
      onSignOut: noop,
      onStartCheckout: noop,
      onOpenPortal: noop,
    },
    mobileAccess: {
      billingUser: null,
      entitlementsBusy: false,
      proEnabled: false,
      mobileStatus: null,
      mobileStatusBusy: false,
      mobileStatusError: null,
      mobileEnableBusy: false,
      mobileEnableError: null,
      mobileQr: null,
      onEnable: noop,
      onDisable: noop,
    },
    teamEnterprise: {
      billingUser: null,
      entitlementsBusy: false,
      plan: "free_local",
      entitlements: null,
      cloudState: emptyCloudState,
      cloudBusy: false,
      cloudError: null,
      actionBusy: false,
      actionError: null,
      actionNotice: null,
      orgName: "",
      setOrgName: noopValue,
      inviteEmail: "",
      setInviteEmail: noopValue,
      inviteRole: "member",
      setInviteRole: noopValue,
      seatTarget: "",
      setSeatTarget: noopValue,
      policyDraft: emptyPolicyDraft,
      setPolicyDraft: noopValue,
      onRefresh: noop,
      onSelectOrg: noop,
      onCreateOrg: noop,
      onInviteMember: noop,
      onAcceptInvite: noop,
      onUpdateSeats: noop,
      onSavePolicy: noop,
      onStartTeamCheckout: noop,
      onRequestEnterpriseSetup: noop,
    },
  };
}

export type {
  SettingsAccountController,
  SettingsBillingController,
  SettingsMobileAccessController,
  SettingsTeamEnterpriseController,
};
