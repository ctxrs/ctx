import { useCallback, useState } from "react";
import type { DevRestartProvidersResult } from "../../../api/client";
import { devRestartProviders } from "../../../api/client";
import { errorMessage } from "../../../utils/errorMessage";

type SettingsDevToolsController = {
  enabled: boolean;
  restartBusy: boolean;
  restartError: string | null;
  restartResults: DevRestartProvidersResult[] | null;
  onRestart: (mode: "drain" | "immediate") => Promise<void>;
};

type Params = {
  enabled: boolean;
};

export function useSettingsDevToolsController({ enabled }: Params): SettingsDevToolsController {
  const [restartBusy, setRestartBusy] = useState(false);
  const [restartError, setRestartError] = useState<string | null>(null);
  const [restartResults, setRestartResults] = useState<DevRestartProvidersResult[] | null>(null);

  const onRestart = useCallback(
    async (mode: "drain" | "immediate") => {
      if (!enabled || restartBusy) return;
      if (mode === "immediate") {
        const confirmed = window.confirm("Immediate restart will interrupt running provider work. Continue?");
        if (!confirmed) return;
      }
      setRestartBusy(true);
      setRestartError(null);
      setRestartResults(null);
      try {
        const response = await devRestartProviders(mode);
        setRestartResults(response.results);
      } catch (error: unknown) {
        setRestartError(errorMessage(error));
      } finally {
        setRestartBusy(false);
      }
    },
    [enabled, restartBusy],
  );

  return {
    enabled,
    restartBusy,
    restartError,
    restartResults,
    onRestart,
  };
}

export type { SettingsDevToolsController };
