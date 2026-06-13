import type { MobileConnectionProfile, MobileDeviceRegistration } from "@ctx/types";
import { apiAny } from "./clientBase";

export type CreateMobileProfileRequest = {
  label: string;
  base_url: string;
  scopes: string[];
};

export type CreateMobileProfileResponse = {
  profile: MobileConnectionProfile;
  token: string;
  qr_payload: Record<string, unknown>;
};

export type MobileTunnelState = "idle" | "running" | "error";

export type MobileAccessStatus = {
  enabled: boolean;
  tunnel_id?: string | null;
  public_base_url?: string | null;
  relay_base_url?: string | null;
  daemon_public_key?: string | null;
  tunnel_state: MobileTunnelState;
  last_error?: string | null;
};

export type EnableMobileAccessResponse = {
  status: MobileAccessStatus;
  qr_payload: Record<string, unknown>;
  pairing_expires_at: string;
};

export const listMobileConnectionProfiles = () =>
  apiAny<MobileConnectionProfile[]>(`/api/mobile/connection_profiles`);

export const createMobileConnectionProfile = (payload: CreateMobileProfileRequest) =>
  apiAny<CreateMobileProfileResponse>(`/api/mobile/connection_profiles`, {
    method: "POST",
    body: JSON.stringify(payload),
  });

export const deleteMobileConnectionProfile = (id: string) =>
  apiAny<void>(`/api/mobile/connection_profiles/${id}`, { method: "DELETE" });

export const listMobileDevicesForProfile = (profileId: string) =>
  apiAny<MobileDeviceRegistration[]>(`/api/mobile/connection_profiles/${profileId}/devices`);

export const getMobileAccessStatus = () => apiAny<MobileAccessStatus>(`/api/mobile/access/status`);

export const enableMobileAccess = () =>
  apiAny<EnableMobileAccessResponse>(`/api/mobile/access/enable`, {
    method: "POST",
    body: JSON.stringify({}),
  });

export const disableMobileAccess = () =>
  apiAny<void>(`/api/mobile/access/disable`, {
    method: "POST",
    body: JSON.stringify({}),
  });
