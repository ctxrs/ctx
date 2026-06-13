import { useCallback, useSyncExternalStore } from "react";
import type { ProviderOptions, Session } from "../../api/client";
import { idToString } from "../../api/client";
import { getProvidersBootstrapSnapshot, subscribeProvidersBootstrap } from "../../state/providersBootstrapStore";

export function useSharedSessionProviderOptions(
  session: Session | null | undefined,
): ProviderOptions | undefined {
  return useSyncExternalStore(
    useCallback((listener) => {
      const workspaceId = idToString(session?.workspace_id);
      return workspaceId ? subscribeProvidersBootstrap(workspaceId, listener) : () => {};
    }, [session?.workspace_id]),
    useCallback(() => {
      const workspaceId = idToString(session?.workspace_id);
      const providerId = String(session?.provider_id ?? "").trim();
      if (!workspaceId || !providerId) return undefined;
      return getProvidersBootstrapSnapshot(workspaceId).provider_options[providerId];
    }, [session?.provider_id, session?.workspace_id]),
    useCallback(() => undefined, []),
  );
}
