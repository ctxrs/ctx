import { useCallback } from "react";
import {
  setSessionModel,
  updateWorkspaceProviderModelPreference,
  type Session,
} from "../../api/client";
import { refreshProvidersBootstrap } from "../../state/providersBootstrapStore";
import { errorMessage } from "../../utils/errorMessage";

type SessionSetter = {
  setSession: (session: Session) => void;
};

type UseSessionModelSwitcherOptions = {
  sessionId: string;
  supervisor: SessionSetter;
  setModelSwitchError: (value: string | null) => void;
  setOptimisticModelId: (value: string | null) => void;
};

export function useSessionModelSwitcher({
  sessionId,
  supervisor,
  setModelSwitchError,
  setOptimisticModelId,
}: UseSessionModelSwitcherOptions) {
  return useCallback(async (next: string) => {
    setModelSwitchError(null);
    setOptimisticModelId(next);
    try {
      const updated = await setSessionModel(sessionId, next);
      supervisor.setSession(updated);
      try {
        await updateWorkspaceProviderModelPreference(
          updated.workspace_id,
          updated.provider_id,
          next,
        );
      } catch (error: unknown) {
        setModelSwitchError(
          `Session model updated, but failed to save the new-chat default: ${errorMessage(error)}`,
        );
      }
      void refreshProvidersBootstrap(updated.workspace_id).catch(() => {});
      setOptimisticModelId(null);
    } catch (error: unknown) {
      setOptimisticModelId(null);
      setModelSwitchError(errorMessage(error));
    }
  }, [sessionId, setModelSwitchError, setOptimisticModelId, supervisor]);
}
