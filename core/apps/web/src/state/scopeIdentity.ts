import type { HarnessSourceKind, InstallTarget, ProviderOptions } from "../api/clientProviders";
import type { DesktopConnectionInfo } from "../utils/desktop";

export type DaemonTargetScope =
  | { kind: "browser"; baseUrl: string; authTokenFingerprint?: string | null }
  | { kind: "desktop_local"; baseUrl?: string | null }
  | { kind: "desktop_ssh"; host: string; user: string | null; port: number; dataDir: string | null };

export type HostOwnerScope = {
  kind: "host";
  daemon: DaemonTargetScope;
};

export type WorkspaceOwnerScope = {
  kind: "workspace";
  daemon: DaemonTargetScope;
  workspaceId: string;
};

export type OwnerScope = HostOwnerScope | WorkspaceOwnerScope;

export type ProviderInstallScope = {
  owner: OwnerScope;
  providerId: string;
  installTarget: InstallTarget | "unknown";
};

type ProviderAuthMode = Exclude<ProviderOptions["auth_mode"], undefined> | null;

export type ProviderAuthScope = {
  owner: WorkspaceOwnerScope;
  providerId: string;
  authMode: ProviderAuthMode;
  accountIdentity: string | null;
  sourceKind: HarnessSourceKind | null;
  selectedEndpointVersion: string | null;
};

export type ProvisioningScope = {
  daemon: DaemonTargetScope;
  installTarget: InstallTarget;
};

type DaemonTargetScopeTuple =
  | ["browser", string]
  | ["browser", string, string | null]
  | ["desktop_local"]
  | ["desktop_local", string | null]
  | ["desktop_ssh", string, string | null, number, string | null];

type OwnerScopeTuple =
  | ["host", DaemonTargetScopeTuple]
  | ["workspace", DaemonTargetScopeTuple, string];

type ProviderInstallScopeTuple = ["provider_install", OwnerScopeTuple, string, InstallTarget | "unknown"];

type ProviderAuthScopeTuple = [
  "provider_auth",
  OwnerScopeTuple,
  string,
  ProviderAuthMode,
  string | null,
  HarnessSourceKind | null,
  string | null,
];

type ProvisioningScopeTuple = ["provisioning", DaemonTargetScopeTuple, InstallTarget];

const normalizeRequiredString = (value: string, label: string): string => {
  const trimmed = value.trim();
  if (!trimmed) {
    throw new Error(`${label} must be a non-empty string.`);
  }
  return trimmed;
};

const normalizeOptionalString = (value: string | null | undefined): string | null => {
  if (value === null || value === undefined) return null;
  const trimmed = value.trim();
  return trimmed ? trimmed : null;
};

const normalizePositiveInteger = (value: number, label: string): number => {
  if (!Number.isInteger(value) || value <= 0) {
    throw new Error(`${label} must be a positive integer.`);
  }
  return value;
};

const isInstallTarget = (value: unknown): value is InstallTarget =>
  value === "host" || value === "container" || value === "linux-aarch64" || value === "linux-x86_64";

const normalizeInstallTarget = (value: InstallTarget | "unknown", label: string): InstallTarget | "unknown" => {
  if (value === "unknown" || isInstallTarget(value)) return value;
  throw new Error(`${label} must be a supported install target.`);
};

const normalizeStrictInstallTarget = (value: InstallTarget, label: string): InstallTarget => {
  if (isInstallTarget(value)) return value;
  throw new Error(`${label} must be a supported install target.`);
};

const isProviderAuthMode = (value: unknown): value is Exclude<ProviderAuthMode, null> =>
  value === "subscription" || value === "endpoint" || value === "none";

const normalizeProviderAuthMode = (value: ProviderOptions["auth_mode"] | null | undefined): ProviderAuthMode => {
  if (value === null || value === undefined) return null;
  if (isProviderAuthMode(value)) return value;
  throw new Error("authMode must be a supported provider auth mode.");
};

const isHarnessSourceKind = (value: unknown): value is HarnessSourceKind =>
  value === "subscription" || value === "endpoint";

const normalizeHarnessSourceKind = (value: HarnessSourceKind | null | undefined): HarnessSourceKind | null => {
  if (value === null || value === undefined) return null;
  if (isHarnessSourceKind(value)) return value;
  throw new Error("sourceKind must be a supported harness source kind.");
};

const readRequiredString = (value: unknown): string | null => {
  if (typeof value !== "string") return null;
  const trimmed = value.trim();
  return trimmed ? trimmed : null;
};

const readOptionalString = (value: unknown): string | null | undefined => {
  if (value === null) return null;
  if (value === undefined) return undefined;
  if (typeof value !== "string") return undefined;
  const trimmed = value.trim();
  return trimmed ? trimmed : null;
};

const readPositiveInteger = (value: unknown): number | null => {
  return typeof value === "number" && Number.isInteger(value) && value > 0 ? value : null;
};

const daemonTargetScopeToTuple = (scope: DaemonTargetScope): DaemonTargetScopeTuple => {
  switch (scope.kind) {
    case "browser":
      return scope.authTokenFingerprint === undefined
        ? ["browser", scope.baseUrl]
        : ["browser", scope.baseUrl, scope.authTokenFingerprint ?? null];
    case "desktop_local":
      return scope.baseUrl === undefined
        ? ["desktop_local"]
        : ["desktop_local", scope.baseUrl ?? null];
    case "desktop_ssh":
      return ["desktop_ssh", scope.host, scope.user, scope.port, scope.dataDir];
  }
};

const daemonTargetScopeFromTuple = (value: unknown): DaemonTargetScope | null => {
  if (!Array.isArray(value)) return null;
  const kind = value[0];
  if (kind === "browser") {
    const baseUrl = readRequiredString(value[1]);
    if (!baseUrl) return null;
    if (value.length === 2) {
      return createBrowserDaemonTargetScope(baseUrl);
    }
    if (value.length === 3) {
      const authTokenFingerprint = readOptionalString(value[2]);
      return authTokenFingerprint === undefined
        ? null
        : createBrowserDaemonTargetScope(baseUrl, authTokenFingerprint);
    }
    return null;
  }
  if (kind === "desktop_local") {
    if (value.length === 1) {
      return createDesktopLocalDaemonTargetScope();
    }
    if (value.length === 2) {
      const baseUrl = readOptionalString(value[1]);
      return baseUrl === undefined ? null : createDesktopLocalDaemonTargetScope(baseUrl);
    }
    return null;
  }
  if (kind === "desktop_ssh") {
    const host = readRequiredString(value[1]);
    const user = readOptionalString(value[2]);
    const port = readPositiveInteger(value[3]);
    const dataDir = readOptionalString(value[4]);
    if (!host || user === undefined || port === null || dataDir === undefined) return null;
    return createDesktopSshDaemonTargetScope({ host, user, port, dataDir });
  }
  return null;
};

const ownerScopeToTuple = (scope: OwnerScope): OwnerScopeTuple => {
  switch (scope.kind) {
    case "host":
      return ["host", daemonTargetScopeToTuple(scope.daemon)];
    case "workspace":
      return ["workspace", daemonTargetScopeToTuple(scope.daemon), scope.workspaceId];
  }
};

const ownerScopeFromTuple = (value: unknown): OwnerScope | null => {
  if (!Array.isArray(value)) return null;
  const kind = value[0];
  const daemon = daemonTargetScopeFromTuple(value[1]);
  if (!daemon) return null;
  if (kind === "host") {
    return value.length === 2 ? createHostOwnerScope(daemon) : null;
  }
  if (kind === "workspace") {
    const workspaceId = readRequiredString(value[2]);
    return workspaceId ? createWorkspaceOwnerScope(daemon, workspaceId) : null;
  }
  return null;
};

const parseSerialized = <T>(value: string | null | undefined, parser: (raw: unknown) => T | null): T | null => {
  if (typeof value !== "string") return null;
  const trimmed = value.trim();
  if (!trimmed) return null;
  try {
    return parser(JSON.parse(trimmed));
  } catch {
    return null;
  }
};

export const createBrowserDaemonTargetScope = (
  baseUrl: string,
  authTokenFingerprint?: string | null,
): DaemonTargetScope => {
  const normalizedBaseUrl = normalizeRequiredString(baseUrl, "baseUrl");
  const normalizedFingerprint = normalizeOptionalString(authTokenFingerprint);
  return normalizedFingerprint
    ? {
      kind: "browser",
      baseUrl: normalizedBaseUrl,
      authTokenFingerprint: normalizedFingerprint,
    }
    : {
      kind: "browser",
      baseUrl: normalizedBaseUrl,
    };
};

export const createDesktopLocalDaemonTargetScope = (baseUrl?: string | null): DaemonTargetScope => {
  const normalizedBaseUrl = normalizeOptionalString(baseUrl);
  return normalizedBaseUrl
    ? {
      kind: "desktop_local",
      baseUrl: normalizedBaseUrl,
    }
    : {
      kind: "desktop_local",
    };
};

export const createDesktopSshDaemonTargetScope = (args: {
  host: string;
  user?: string | null;
  port: number;
  dataDir?: string | null;
}): DaemonTargetScope => ({
  kind: "desktop_ssh",
  host: normalizeRequiredString(args.host, "host"),
  user: normalizeOptionalString(args.user),
  port: normalizePositiveInteger(args.port, "port"),
  dataDir: normalizeOptionalString(args.dataDir),
});

export const cloneDaemonTargetScope = (scope: DaemonTargetScope): DaemonTargetScope => {
  switch (scope.kind) {
    case "browser":
      return createBrowserDaemonTargetScope(scope.baseUrl, scope.authTokenFingerprint);
    case "desktop_local":
      return createDesktopLocalDaemonTargetScope(scope.baseUrl);
    case "desktop_ssh":
      return createDesktopSshDaemonTargetScope(scope);
  }
};

export const daemonTargetScopeFromDesktopConnectionInfo = (
  info: Pick<DesktopConnectionInfo, "kind" | "host" | "user" | "remote_port" | "remote_data_dir"> | null | undefined,
): DaemonTargetScope | null => {
  if (!info) return null;
  if (info.kind === "local") {
    return createDesktopLocalDaemonTargetScope();
  }
  if (info.kind === "ssh") {
    const host = normalizeOptionalString(info.host);
    const port = readPositiveInteger(info.remote_port);
    if (!host || port === null) return null;
    return createDesktopSshDaemonTargetScope({
      host,
      user: normalizeOptionalString(info.user),
      port,
      dataDir: normalizeOptionalString(info.remote_data_dir),
    });
  }
  return null;
};

export const sameDaemonTargetScope = (lhs: DaemonTargetScope, rhs: DaemonTargetScope): boolean => {
  if (lhs.kind !== rhs.kind) return false;
  switch (lhs.kind) {
    case "browser":
      return rhs.kind === "browser"
        && lhs.baseUrl === rhs.baseUrl
        && (lhs.authTokenFingerprint ?? null) === (rhs.authTokenFingerprint ?? null);
    case "desktop_local":
      return rhs.kind === "desktop_local"
        && (lhs.baseUrl ?? null) === (rhs.baseUrl ?? null);
    case "desktop_ssh":
      return rhs.kind === "desktop_ssh"
        && lhs.host === rhs.host
        && lhs.user === rhs.user
        && lhs.port === rhs.port
        && lhs.dataDir === rhs.dataDir;
  }
};

export const serializeDaemonTargetScope = (scope: DaemonTargetScope): string =>
  JSON.stringify(daemonTargetScopeToTuple(scope));

export const deserializeDaemonTargetScope = (value: string | null | undefined): DaemonTargetScope | null =>
  parseSerialized(value, daemonTargetScopeFromTuple);

export const createHostOwnerScope = (daemon: DaemonTargetScope): HostOwnerScope => ({
  kind: "host",
  daemon: cloneDaemonTargetScope(daemon),
});

export const createWorkspaceOwnerScope = (
  daemon: DaemonTargetScope,
  workspaceId: string,
): WorkspaceOwnerScope => ({
  kind: "workspace",
  daemon: cloneDaemonTargetScope(daemon),
  workspaceId: normalizeRequiredString(workspaceId, "workspaceId"),
});

export const cloneOwnerScope = (scope: OwnerScope): OwnerScope => {
  switch (scope.kind) {
    case "host":
      return createHostOwnerScope(scope.daemon);
    case "workspace":
      return createWorkspaceOwnerScope(scope.daemon, scope.workspaceId);
  }
};

export const sameOwnerScope = (lhs: OwnerScope, rhs: OwnerScope): boolean => {
  if (lhs.kind !== rhs.kind) return false;
  if (!sameDaemonTargetScope(lhs.daemon, rhs.daemon)) return false;
  if (lhs.kind === "workspace" && rhs.kind === "workspace") {
    return lhs.workspaceId === rhs.workspaceId;
  }
  return true;
};

export const serializeOwnerScope = (scope: OwnerScope): string => JSON.stringify(ownerScopeToTuple(scope));

export const deserializeOwnerScope = (value: string | null | undefined): OwnerScope | null =>
  parseSerialized(value, ownerScopeFromTuple);

export const createProviderInstallScope = (args: {
  owner: OwnerScope;
  providerId: string;
  installTarget: InstallTarget | "unknown";
}): ProviderInstallScope => ({
  owner: cloneOwnerScope(args.owner),
  providerId: normalizeRequiredString(args.providerId, "providerId"),
  installTarget: normalizeInstallTarget(args.installTarget, "installTarget"),
});

export const sameProviderInstallScope = (lhs: ProviderInstallScope, rhs: ProviderInstallScope): boolean =>
  sameOwnerScope(lhs.owner, rhs.owner)
  && lhs.providerId === rhs.providerId
  && lhs.installTarget === rhs.installTarget;

export const serializeProviderInstallScope = (scope: ProviderInstallScope): string =>
  JSON.stringify([
    "provider_install",
    ownerScopeToTuple(scope.owner),
    scope.providerId,
    scope.installTarget,
  ] satisfies ProviderInstallScopeTuple);

export const deserializeProviderInstallScope = (value: string | null | undefined): ProviderInstallScope | null =>
  parseSerialized(value, (raw) => {
    if (!Array.isArray(raw) || raw[0] !== "provider_install") return null;
    const owner = ownerScopeFromTuple(raw[1]);
    const providerId = readRequiredString(raw[2]);
    const installTarget = raw[3];
    if (!owner || !providerId || (installTarget !== "unknown" && !isInstallTarget(installTarget))) return null;
    return createProviderInstallScope({
      owner,
      providerId,
      installTarget,
    });
  });

export const providerAuthSelectedEndpointVersion = (
  options: ProviderOptions | undefined,
): string | null => {
  if (!options || options.source?.selected_source_kind !== "endpoint") return null;
  const endpointId = options.source.selected_endpoint_id;
  if (!endpointId) return null;
  const endpoint = options.source.endpoints.find((candidate) => candidate.id === endpointId);
  if (!endpoint) return endpointId;
  return [
    endpoint.id,
    endpoint.updated_at,
    endpoint.base_url ?? "",
    endpoint.has_api_key ? "1" : "0",
    endpoint.model_override ?? "",
  ].join(":");
};

export const createProviderAuthScope = (args: {
  owner: WorkspaceOwnerScope;
  providerId: string;
  authMode: ProviderOptions["auth_mode"] | null | undefined;
  accountIdentity: string | null | undefined;
  sourceKind: HarnessSourceKind | null | undefined;
  selectedEndpointVersion: string | null | undefined;
}): ProviderAuthScope => ({
  owner: createWorkspaceOwnerScope(args.owner.daemon, args.owner.workspaceId),
  providerId: normalizeRequiredString(args.providerId, "providerId"),
  authMode: normalizeProviderAuthMode(args.authMode),
  accountIdentity: normalizeOptionalString(args.accountIdentity),
  sourceKind: normalizeHarnessSourceKind(args.sourceKind),
  selectedEndpointVersion: normalizeOptionalString(args.selectedEndpointVersion),
});

export const createProviderAuthScopeFromOptions = (
  owner: WorkspaceOwnerScope,
  providerId: string,
  options: ProviderOptions | undefined,
): ProviderAuthScope =>
  createProviderAuthScope({
    owner,
    providerId,
    authMode: options?.auth_mode ?? null,
    accountIdentity: options?.account_identity ?? null,
    sourceKind: options?.source?.selected_source_kind ?? null,
    selectedEndpointVersion: providerAuthSelectedEndpointVersion(options),
  });

export const sameProviderAuthScope = (lhs: ProviderAuthScope, rhs: ProviderAuthScope): boolean =>
  sameOwnerScope(lhs.owner, rhs.owner)
  && lhs.providerId === rhs.providerId
  && lhs.authMode === rhs.authMode
  && lhs.accountIdentity === rhs.accountIdentity
  && lhs.sourceKind === rhs.sourceKind
  && lhs.selectedEndpointVersion === rhs.selectedEndpointVersion;

export const serializeProviderAuthScope = (scope: ProviderAuthScope): string =>
  JSON.stringify([
    "provider_auth",
    ownerScopeToTuple(scope.owner),
    scope.providerId,
    scope.authMode,
    scope.accountIdentity,
    scope.sourceKind,
    scope.selectedEndpointVersion,
  ] satisfies ProviderAuthScopeTuple);

export const deserializeProviderAuthScope = (value: string | null | undefined): ProviderAuthScope | null =>
  parseSerialized(value, (raw) => {
    if (!Array.isArray(raw) || raw[0] !== "provider_auth") return null;
    const owner = ownerScopeFromTuple(raw[1]);
    const providerId = readRequiredString(raw[2]);
    const authMode = raw[3];
    const accountIdentity = readOptionalString(raw[4]);
    const sourceKind = raw[5];
    const selectedEndpointVersion = readOptionalString(raw[6]);
    if (
      !owner
      || owner.kind !== "workspace"
      || !providerId
      || (!isProviderAuthMode(authMode) && authMode !== null)
      || accountIdentity === undefined
      || (!isHarnessSourceKind(sourceKind) && sourceKind !== null)
      || selectedEndpointVersion === undefined
    ) {
      return null;
    }
    return createProviderAuthScope({
      owner,
      providerId,
      authMode,
      accountIdentity,
      sourceKind,
      selectedEndpointVersion,
    });
  });

export const createProvisioningScope = (
  daemon: DaemonTargetScope,
  installTarget: InstallTarget,
): ProvisioningScope => ({
  daemon: cloneDaemonTargetScope(daemon),
  installTarget: normalizeStrictInstallTarget(installTarget, "installTarget"),
});

export const sameProvisioningScope = (lhs: ProvisioningScope, rhs: ProvisioningScope): boolean =>
  sameDaemonTargetScope(lhs.daemon, rhs.daemon)
  && lhs.installTarget === rhs.installTarget;

export const serializeProvisioningScope = (scope: ProvisioningScope): string =>
  JSON.stringify([
    "provisioning",
    daemonTargetScopeToTuple(scope.daemon),
    scope.installTarget,
  ] satisfies ProvisioningScopeTuple);

export const deserializeProvisioningScope = (value: string | null | undefined): ProvisioningScope | null =>
  parseSerialized(value, (raw) => {
    if (!Array.isArray(raw) || raw[0] !== "provisioning") return null;
    const daemon = daemonTargetScopeFromTuple(raw[1]);
    const installTarget = raw[2];
    if (!daemon || !isInstallTarget(installTarget)) return null;
    return createProvisioningScope(daemon, installTarget);
  });
