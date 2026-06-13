import type { DesktopConnectionInfo } from "../utils/desktop";
import type { DaemonTargetScope } from "../state/scopeIdentity";

export type DaemonConnection = {
  baseUrl: string | null;
  wsBaseUrl: string | null;
  authToken: string | null;
  runId: string | null;
  source?: string | null;
  targetScope?: DaemonTargetScope | null;
  mobileSecure?: MobileSecureConnection | null;
};

export type DaemonConnectionUpdate = {
  baseUrl?: string | null;
  wsBaseUrl?: string | null;
  authToken?: string | null;
  runId?: string | null;
  source?: string | null;
  targetScope?: DaemonTargetScope | null;
  mobileSecure?: MobileSecureConnection | null;
};

export type SetDaemonConnectionOptions = {
  persistBaseUrl?: boolean;
  clearPersistedBaseUrl?: boolean;
  persistAuthToken?: boolean;
  clearPersistedAuthToken?: boolean;
};

export type DaemonConnectionReadiness = {
  hasBaseUrl: boolean;
  hasAuthToken: boolean;
  hasMobileSecure: boolean;
  isReady: boolean;
  missing: "base" | "auth" | null;
};

export type MobileSecureConnection = {
  kind: "managed_tunnel";
  deviceId: string;
  daemonPublicKey: string;
  pairingRequestEncryption: string;
  nextSeq: number;
};

export type StoredMobileSecureConnectionV1 = {
  kind: "managed_tunnel";
  deviceId: string;
  daemonPublicKey: string;
  pairingRequestEncryption: string;
  nextSeq: number;
};

export type StoredDaemonConnectionV1 = {
  v: 1;
  baseUrl: string | null;
  wsBaseUrl: string | null;
  authToken: string | null;
  source?: string | null;
  targetScope?: string | null;
  mobileSecure?: StoredMobileSecureConnectionV1 | null;
};

export type PersistedDaemonBaseV1 = {
  v: 1;
  baseUrl: string | null;
  wsBaseUrl: string | null;
  targetScope?: string | null;
};

export type ParsedStoredDaemonConnection = {
  baseUrl: string | null;
  wsBaseUrl: string | null;
  authToken: string | null;
  source: string | null;
  targetScope: DaemonTargetScope | null;
  mobileSecure: MobileSecureConnection | null;
};

export type ParsedPersistedDaemonBase = {
  baseUrl: string | null;
  wsBaseUrl: string | null;
  targetScope: DaemonTargetScope | null;
};

export type DesktopDaemonConnectionInfoLike = {
  kind?: DesktopConnectionInfo["kind"] | null;
  base_url?: string | null;
  browser_query_secret?: string | null;
  token?: string | null;
  host?: string | null;
  user?: string | null;
  remote_port?: number | null;
  remote_data_dir?: string | null;
};
