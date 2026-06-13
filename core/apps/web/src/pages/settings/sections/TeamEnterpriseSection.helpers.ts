import type { PlanType } from "../entitlementAnalytics";
import type {
  EntitlementFeatureState,
  EntitlementSubjectType,
  MembershipRole,
  TeamEnterpriseCloudState,
} from "../teamEnterpriseSettingsApi";

export type StatusPillTone = "neutral" | "ok" | "warn" | "err";

export type FeatureStatus = {
  label: string;
  tone: StatusPillTone;
};

export function formatPlanLabel(plan: PlanType): string {
  switch (plan) {
    case "free_local":
      return "Free / Local";
    case "pro":
      return "Pro";
    case "team":
      return "Team";
    case "enterprise":
      return "Enterprise";
    default:
      return "Unknown";
  }
}

export function formatScopeLabel(value?: EntitlementSubjectType | null): string {
  switch (value) {
    case "install":
      return "Install";
    case "account":
      return "Account";
    case "org":
      return "Organization";
    default:
      return "Unavailable";
  }
}

export function formatRoleLabel(value?: MembershipRole | null): string {
  switch (value) {
    case "owner":
      return "Owner";
    case "admin":
      return "Admin";
    case "member":
      return "Member";
    default:
      return "Not configured";
  }
}

export function featureStatus(value?: EntitlementFeatureState): FeatureStatus {
  if (value === "enabled") {
    return { label: "Enabled", tone: "ok" };
  }
  if (value === "disabled") {
    return { label: "Disabled", tone: "neutral" };
  }
  return { label: "Unavailable", tone: "warn" };
}

export function roleCanAdmin(role?: MembershipRole | null): boolean {
  return role === "owner" || role === "admin";
}

export function formatDateLabel(value: string | null): string {
  if (!value) return "No date";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return date.toLocaleDateString(undefined, { month: "short", day: "numeric", year: "numeric" });
}

export function activeOrgDescription(cloudState: TeamEnterpriseCloudState): string {
  if (cloudState.orgs.length === 0) return "No organizations are visible for this signed-in account.";
  if (cloudState.orgs.length === 1) return "One organization is visible for this signed-in account.";
  return `${cloudState.orgs.length} organizations are visible for this signed-in account.`;
}
