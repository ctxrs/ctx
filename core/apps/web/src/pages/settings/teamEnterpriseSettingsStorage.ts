import { ENTITLEMENTS_CACHE_KEY } from "../../utils/entitlementsCache";
import { TEAM_ENTERPRISE_ACTIVE_ORG_STORAGE_KEY } from "./teamEnterpriseSettingsApi";

export function readStoredTeamEnterpriseActiveOrgId(): string | null {
  try {
    const value = window.localStorage.getItem(TEAM_ENTERPRISE_ACTIVE_ORG_STORAGE_KEY);
    return value && value.trim() ? value : null;
  } catch {
    return null;
  }
}

export function writeStoredTeamEnterpriseActiveOrgId(value: string | null): void {
  try {
    if (value) {
      window.localStorage.setItem(TEAM_ENTERPRISE_ACTIVE_ORG_STORAGE_KEY, value);
    } else {
      window.localStorage.removeItem(TEAM_ENTERPRISE_ACTIVE_ORG_STORAGE_KEY);
    }
  } catch {
    // localStorage can be unavailable in hardened browser contexts; active org remains in memory.
  }
}

export function teamEnterpriseEntitlementsCacheKey(activeOrgId: string | null): string {
  return activeOrgId ? `${ENTITLEMENTS_CACHE_KEY}:org:${activeOrgId}` : ENTITLEMENTS_CACHE_KEY;
}
