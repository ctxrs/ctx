export type AnalyticsEnvironment = "staging" | "production";

const readTrimmed = (value: string | undefined): string | undefined => {
  const next = value?.trim();
  return next ? next : undefined;
};

export type ProductionAnalyticsBuildConfig = {
  explicitAnalyticsEnv: string | undefined;
  explicitAppVersion: string | undefined;
  packageVersion: string | undefined;
};

export const validateProductionAnalyticsBuildConfig = ({
  explicitAnalyticsEnv,
  explicitAppVersion,
  packageVersion,
}: ProductionAnalyticsBuildConfig): void => {
  const analyticsEnv = readTrimmed(explicitAnalyticsEnv)?.toLowerCase();
  if (analyticsEnv !== "production") return;

  const appVersion = readTrimmed(explicitAppVersion);
  if (!appVersion) {
    throw new Error(
      "VITE_POSTHOG_ENV=production requires VITE_CTX_APP_VERSION to be set to the desktop release version.",
    );
  }

  if (appVersion === readTrimmed(packageVersion)) {
    throw new Error(
      "VITE_POSTHOG_ENV=production cannot use the web package version as VITE_CTX_APP_VERSION.",
    );
  }
};

export const resolveAnalyticsEnvironment = (
  explicitEnv: string | undefined,
  mode: string | undefined,
): AnalyticsEnvironment => {
  const normalizedMode = String(mode ?? "").trim().toLowerCase();
  if (normalizedMode === "development" || normalizedMode === "dev") return "staging";
  const env = readTrimmed(explicitEnv)?.toLowerCase();
  if (env === "production") return "production";
  if (env === "staging") return "staging";
  return "staging";
};

export const getAnalyticsEnvironment = (): AnalyticsEnvironment =>
  resolveAnalyticsEnvironment(import.meta.env.VITE_POSTHOG_ENV, import.meta.env.MODE);

export const getPostHogProjectId = (): string =>
  readTrimmed(import.meta.env.VITE_POSTHOG_PROJECT_ID) ?? "";

export const getPostHogHost = (): string =>
  readTrimmed(import.meta.env.VITE_POSTHOG_HOST) ?? "";

export const getPostHogUiHost = (): string =>
  readTrimmed(import.meta.env.VITE_POSTHOG_UI_HOST) ?? "";

export const getPostHogKey = (): string =>
  readTrimmed(import.meta.env.VITE_POSTHOG_KEY) ?? "";
