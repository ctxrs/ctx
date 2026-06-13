import { useCallback, useEffect, useState } from "react";
import {
  clearRestartReadyDismissedVersion,
  readRestartReadyDismissedVersion,
  readRestartRequiredVersion,
  writeRestartReadyDismissedVersion,
} from "./storage";
import { normalizeOptionalString } from "./version";

type UseRestartReadyDismissalArgs = {
  desktopStateHydrated: boolean;
  isDesktop: boolean;
  latestVersion: string | null | undefined;
  restartRequired: boolean;
};

export const useRestartReadyDismissal = ({
  desktopStateHydrated,
  isDesktop,
  latestVersion,
  restartRequired,
}: UseRestartReadyDismissalArgs) => {
  const [dismissedRestartReadyVersion, setDismissedRestartReadyVersion] =
    useState<string>(() => readRestartReadyDismissedVersion());
  const restartReadyDismissKey =
    normalizeOptionalString(latestVersion) || readRestartRequiredVersion();

  const clearDismissedRestartReadyVersion = useCallback(() => {
    clearRestartReadyDismissedVersion();
    setDismissedRestartReadyVersion("");
  }, []);

  const dismissRestartReady = useCallback((): boolean => {
    if (!restartReadyDismissKey) return false;
    writeRestartReadyDismissedVersion(restartReadyDismissKey);
    setDismissedRestartReadyVersion(restartReadyDismissKey);
    return true;
  }, [restartReadyDismissKey]);

  useEffect(() => {
    if (!dismissedRestartReadyVersion) return;
    if (!restartRequired) {
      if (isDesktop && !desktopStateHydrated) return;
      clearDismissedRestartReadyVersion();
      return;
    }
    if (
      restartReadyDismissKey &&
      dismissedRestartReadyVersion !== restartReadyDismissKey
    ) {
      clearDismissedRestartReadyVersion();
    }
  }, [
    clearDismissedRestartReadyVersion,
    desktopStateHydrated,
    dismissedRestartReadyVersion,
    isDesktop,
    restartReadyDismissKey,
    restartRequired,
  ]);

  return {
    clearDismissedRestartReadyVersion,
    dismissedRestartReadyVersion,
    dismissRestartReady,
    restartReadyDismissKey,
  };
};
