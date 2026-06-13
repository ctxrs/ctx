type CapturePolicyInput = {
  settingsLoaded: boolean;
  telemetryEnabled: boolean;
  isDev: boolean;
  isTest: boolean;
  isCi: boolean;
  isLocalWebOrigin: boolean;
  devCaptureFlag: string | undefined;
};

const explicitDevCaptureEnabled = (value: string | undefined): boolean =>
  ["1", "true", "yes", "on"].includes(String(value ?? "").trim().toLowerCase());

export const computeAnalyticsCaptureEnabled = (input: CapturePolicyInput): boolean => {
  if (!input.settingsLoaded) return false;
  if (!input.telemetryEnabled) return false;
  if (input.isDev || input.isTest || input.isCi || input.isLocalWebOrigin) {
    return explicitDevCaptureEnabled(input.devCaptureFlag);
  }
  return true;
};
