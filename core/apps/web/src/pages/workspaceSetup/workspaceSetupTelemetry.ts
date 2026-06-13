import { getSettings, updateSettings } from "../../api/client";
import { getClientSettingsState, loadClientSettings } from "../../state/clientSettings";

const loadClientTelemetryPreference = async (): Promise<boolean> => {
  const state = getClientSettingsState();
  if (state.loaded) {
    return state.settings.telemetry.clientEnabled;
  }
  const loaded = await loadClientSettings();
  return loaded.settings.telemetry.clientEnabled;
};

export const seedDaemonTelemetryPreferenceIfDefault = async (): Promise<void> => {
  const clientEnabled = await loadClientTelemetryPreference();
  const settings = await getSettings();
  const telemetry = settings.telemetry ?? null;
  if (telemetry && telemetry.source !== "default") {
    return;
  }
  const currentEnabled = telemetry?.enabled ?? true;
  if (currentEnabled === clientEnabled) {
    return;
  }
  await updateSettings({
    telemetry: {
      enabled: clientEnabled,
      endpoint: telemetry?.endpoint ?? "",
    },
  });
};
