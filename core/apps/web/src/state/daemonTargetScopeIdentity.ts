import { getDaemonConnection } from "../api/daemonConnection";
import {
  cloneDaemonTargetScope,
  createBrowserDaemonTargetScope,
  createDesktopLocalDaemonTargetScope,
  type DaemonTargetScope,
} from "./scopeIdentity";

const fingerprintAuthToken = (value: string | null | undefined): string | null => {
  const token = typeof value === "string" ? value.trim() : "";
  if (!token) return null;

  let hash = 2166136261;
  for (let index = 0; index < token.length; index += 1) {
    hash ^= token.charCodeAt(index);
    hash = Math.imul(hash, 16777619);
  }

  return `tok_${(hash >>> 0).toString(16).padStart(8, "0")}`;
};

export const getDaemonIdentityScopeOrNull = (): DaemonTargetScope | null => {
  const connection = getDaemonConnection();
  const targetScope = connection.targetScope ?? null;
  if (!targetScope) return null;

  switch (targetScope.kind) {
    case "browser":
      return createBrowserDaemonTargetScope(
        connection.baseUrl ?? targetScope.baseUrl,
        fingerprintAuthToken(connection.authToken),
      );
    case "desktop_local":
      return createDesktopLocalDaemonTargetScope(connection.baseUrl ?? targetScope.baseUrl ?? null);
    case "desktop_ssh":
      return cloneDaemonTargetScope(targetScope);
  }
};
