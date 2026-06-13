const TRUTHY_ENV_VALUES = new Set(["1", "true", "yes", "on"]);

export type RuntimeInstallSmokeFailureCategory =
  | "external_outage"
  | "environment"
  | "product_regression";

export const parseCsv = (value: string | undefined): string[] =>
  String(value ?? "")
    .split(",")
    .map((entry) => entry.trim())
    .filter(Boolean);

export const envTruthy = (value: string | undefined): boolean =>
  TRUTHY_ENV_VALUES.has(String(value ?? "").trim().toLowerCase());

export const bundledOnlyModeAppliesToProvider = (
  providerId: string,
  env: NodeJS.ProcessEnv = process.env,
): boolean => {
  if (!envTruthy(env.CTX_E2E_BUNDLED_ONLY)) {
    return false;
  }

  const providers = parseCsv(env.CTX_E2E_BUNDLED_ONLY_PROVIDERS);
  if (providers.length === 0) {
    return true;
  }

  return providers.includes(providerId.trim());
};

export const shouldSkipBundledOnlyInstall = (
  providerId: string,
  env: NodeJS.ProcessEnv = process.env,
): boolean => {
  if (!envTruthy(env.CTX_E2E_INSTALL_SMOKE_SKIP_BUNDLED_ONLY_INSTALLS)) {
    return false;
  }

  return bundledOnlyModeAppliesToProvider(providerId, env);
};

export const classifyInstallSmokeFailureCategory = (
  stage: string,
  reason: string,
  errorCode: string | null,
): RuntimeInstallSmokeFailureCategory => {
  const normalizedReason = reason.toLowerCase();
  const normalizedCode = String(errorCode || "").toLowerCase();

  if (
    normalizedCode === "download_failed"
    || normalizedCode === "timeout"
    || normalizedReason.includes("rate limit")
    || normalizedReason.includes("high demand")
    || normalizedReason.includes("temporary errors")
    || normalizedReason.includes("429")
    || normalizedReason.includes("503")
    || normalizedReason.includes("502")
    || normalizedReason.includes("service unavailable")
    || normalizedReason.includes("upstream")
    || normalizedReason.includes("gateway")
  ) {
    return "external_outage";
  }

  if (
    normalizedReason.includes("payment required")
    || normalizedReason.includes("insufficient credits")
    || normalizedReason.includes("guardrail restrictions")
    || normalizedReason.includes("data policy")
    || normalizedReason.includes("container runtime")
    || normalizedReason.includes("no space left")
    || normalizedReason.includes("permission denied")
    || normalizedReason.includes("cannot connect")
    || normalizedReason.includes("operation not permitted")
    || normalizedCode === "unsupported_target"
    || normalizedCode === "invalid_target"
  ) {
    return "environment";
  }

  if (stage === "execution_config") {
    return "environment";
  }

  return "product_regression";
};

export const shouldRetryInstallSmokeFirstTurnFailure = (
  stage: string,
  category: RuntimeInstallSmokeFailureCategory,
  attempt: number,
  maxAttempts: number,
): boolean =>
  (stage === "first_turn" || stage === "first_turn_request")
  && category === "external_outage"
  && attempt < maxAttempts;
