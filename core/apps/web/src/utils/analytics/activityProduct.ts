import type { ExecutionEnvironment } from "@ctx/types";
import type {
  AnalyticsProperties,
  AnalyticsSessionKind,
  AnalyticsSessionLocation,
  AnalyticsSessionRootKind,
} from "./types";
import {
  capture,
  FIRST_TURN_COMPLETED_ONCE_KEY,
  FIRST_TURN_SUBMITTED_ONCE_KEY,
  durationBucketForMs,
  markOnce,
  modelAnalyticsProperties,
  tokenUsageProperties,
} from "./activityShared";

export type ProviderSetupSource =
  | "provider_onboarding"
  | "settings"
  | "workbench"
  | "workspace_setup"
  | "session_auth"
  | "unknown";

export type ProviderInstallFailureKind =
  | "download_failed"
  | "checksum_mismatch"
  | "command_failed"
  | "timeout"
  | "matrix_mismatch"
  | "health_check_failed"
  | "registry_write_failed"
  | "unsupported_target"
  | "request_failed"
  | "user_cancelled"
  | "unknown";

export type ProviderAuthMethod =
  | "subscription_browser"
  | "subscription_token"
  | "subscription_account"
  | "endpoint"
  | "workspace_auth"
  | "unknown";

export type ProviderAuthFailureKind =
  | "request_failed"
  | "browser_open_failed"
  | "timeout"
  | "provider_failed"
  | "verification_failed"
  | "user_cancelled"
  | "unknown";

export type TurnFailureKind =
  | "auth_missing"
  | "auth_failed"
  | "install_missing"
  | "install_failed"
  | "provider_launch_failed"
  | "sandbox_prepare_failed"
  | "network_error"
  | "timeout"
  | "user_cancelled"
  | "unknown";

const normalizeBoundedKey = (value: unknown): string =>
  String(value ?? "")
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "_")
    .replace(/^_+|_+$/g, "");

export const normalizeProviderInstallFailureKind = (
  value: unknown,
): ProviderInstallFailureKind => {
  const key = normalizeBoundedKey(value);
  switch (key) {
    case "download_failed":
    case "checksum_mismatch":
    case "command_failed":
    case "timeout":
    case "matrix_mismatch":
    case "health_check_failed":
    case "registry_write_failed":
    case "unsupported_target":
    case "request_failed":
    case "user_cancelled":
      return key;
    case "invalid_target":
      return "unsupported_target";
    case "cancelled":
    case "canceled":
      return "user_cancelled";
    default:
      return "unknown";
  }
};

export const normalizeProviderAuthFailureKind = (
  value: unknown,
): ProviderAuthFailureKind => {
  const key = normalizeBoundedKey(value);
  switch (key) {
    case "request_failed":
    case "browser_open_failed":
    case "timeout":
    case "provider_failed":
    case "verification_failed":
    case "user_cancelled":
      return key;
    case "failed":
    case "auth_failed":
      return "provider_failed";
    case "cancelled":
    case "canceled":
      return "user_cancelled";
    default:
      return "unknown";
  }
};

export const normalizeTurnFailureKind = (
  value: unknown,
  status?: "failed" | "interrupted",
): TurnFailureKind => {
  const key = normalizeBoundedKey(value);
  if (!key) return status === "interrupted" ? "user_cancelled" : "unknown";
  if (key.includes("auth") && (key.includes("missing") || key.includes("required"))) {
    return "auth_missing";
  }
  if (key.includes("auth") || key.includes("credential") || key.includes("login")) {
    return "auth_failed";
  }
  if (key.includes("install") && (key.includes("missing") || key.includes("required"))) {
    return "install_missing";
  }
  if (key.includes("install")) return "install_failed";
  if (key.includes("sandbox") || key.includes("container") || key.includes("vm")) {
    return "sandbox_prepare_failed";
  }
  if (key.includes("network") || key.includes("connection") || key.includes("fetch")) {
    return "network_error";
  }
  if (key.includes("timeout") || key.includes("timed_out")) return "timeout";
  if (key.includes("cancel") || key.includes("interrupt")) return "user_cancelled";
  if (key.includes("spawn") || key.includes("launch") || key.includes("provider")) {
    return "provider_launch_failed";
  }
  if (status === "interrupted") return "user_cancelled";
  return "unknown";
};

export const trackSessionCreated = (props: {
  providerId: string;
  modelId?: string;
  executionEnvironment?: ExecutionEnvironment;
  sessionRootKind?: AnalyticsSessionRootKind;
  sessionLocation?: AnalyticsSessionLocation;
}): void => {
  capture("session_created", {
    provider_id: props.providerId,
    ...(props.modelId ? { model_id: props.modelId } : {}),
    ...(props.executionEnvironment ? { execution_environment: props.executionEnvironment } : {}),
    ...(props.sessionRootKind ? { session_root_kind: props.sessionRootKind } : {}),
    ...(props.sessionLocation ? { session_location: props.sessionLocation } : {}),
  });
};

export const trackTaskCreated = (props: {
  providerId: string;
  modelId?: string;
  reasoningEffort?: string | null;
  executionEnvironment?: ExecutionEnvironment;
}): void => {
  capture("task_created", {
    provider_id: props.providerId,
    ...modelAnalyticsProperties(props.modelId, props.reasoningEffort),
    ...(props.executionEnvironment ? { execution_environment: props.executionEnvironment } : {}),
    session_kind: "primary",
  });
};

export const trackProviderSelected = (props: {
  providerId: string;
  source: "session_create" | "provider_switch" | "unknown";
}): void => {
  capture("provider_selected", {
    provider_id: props.providerId,
    source: props.source,
  });
};

export const trackProviderInstallStarted = (props: {
  providerId: string;
  source?: ProviderSetupSource;
  target?: string;
}): void => {
  capture("provider_install_started", {
    provider_id: props.providerId,
    source: props.source ?? "provider_onboarding",
    ...(props.target ? { target: props.target } : {}),
  });
};

export const trackProviderInstallCompleted = (props: {
  providerId: string;
  source?: ProviderSetupSource;
  target?: string;
}): void => {
  capture("provider_install_completed", {
    provider_id: props.providerId,
    source: props.source ?? "provider_onboarding",
    status: "succeeded",
    ...(props.target ? { target: props.target } : {}),
  });
};

export const trackProviderInstallFailed = (props: {
  providerId?: string;
  source?: ProviderSetupSource;
  target?: string;
  failureKind?: ProviderInstallFailureKind;
  installErrorCode?: string;
  status?: "failed" | "cancelled";
}): void => {
  capture("provider_install_failed", {
    ...(props.providerId ? { provider_id: props.providerId } : {}),
    source: props.source ?? "provider_onboarding",
    status: props.status ?? "failed",
    failure_kind: normalizeProviderInstallFailureKind(props.failureKind ?? props.installErrorCode),
    ...(props.target ? { target: props.target } : {}),
    ...(props.installErrorCode ? { install_error_kind: normalizeBoundedKey(props.installErrorCode) } : {}),
  });
};

export const trackProviderAuthStarted = (props: {
  providerId: string;
  source?: ProviderSetupSource;
  authMethod?: ProviderAuthMethod;
}): void => {
  capture("provider_auth_started", {
    provider_id: props.providerId,
    source: props.source ?? "settings",
    auth_method: props.authMethod ?? "unknown",
  });
};

export const trackProviderAuthCompleted = (props: {
  providerId: string;
  source?: ProviderSetupSource;
  authMethod?: ProviderAuthMethod;
}): void => {
  capture("provider_auth_completed", {
    provider_id: props.providerId,
    source: props.source ?? "settings",
    auth_method: props.authMethod ?? "unknown",
    status: "succeeded",
  });
};

export const trackProviderAuthFailed = (props: {
  providerId: string;
  source?: ProviderSetupSource;
  authMethod?: ProviderAuthMethod;
  failureKind?: ProviderAuthFailureKind;
}): void => {
  capture("provider_auth_failed", {
    provider_id: props.providerId,
    source: props.source ?? "settings",
    auth_method: props.authMethod ?? "unknown",
    status: props.failureKind === "user_cancelled" ? "cancelled" : "failed",
    failure_kind: normalizeProviderAuthFailureKind(props.failureKind),
  });
};

export const trackFirstTurnSubmitted = (props: {
  sessionId: string;
  providerId?: string;
  modelId?: string;
}): void => {
  if (!markOnce(FIRST_TURN_SUBMITTED_ONCE_KEY)) return;
  capture("first_turn_submitted", {
    ...(props.providerId ? { provider_id: props.providerId } : {}),
    ...(props.modelId ? { model_id: props.modelId } : {}),
  });
};

export const trackUserMessageSent = (props: {
  providerId?: string;
  modelId?: string;
  reasoningEffort?: string | null;
  executionEnvironment?: ExecutionEnvironment;
  sessionKind?: AnalyticsSessionKind;
  isFirstTurn?: boolean;
}): void => {
  capture("user_message_sent", {
    ...(props.providerId ? { provider_id: props.providerId } : {}),
    ...modelAnalyticsProperties(props.modelId, props.reasoningEffort),
    ...(props.executionEnvironment ? { execution_environment: props.executionEnvironment } : {}),
    ...(props.sessionKind ? { session_kind: props.sessionKind } : {}),
    ...(props.isFirstTurn !== undefined ? { is_first_turn: props.isFirstTurn } : {}),
  });
};

export const trackTurnStarted = (props: {
  providerId?: string;
  modelId?: string;
  reasoningEffort?: string | null;
  executionEnvironment?: ExecutionEnvironment;
  sessionKind?: AnalyticsSessionKind;
}): void => {
  capture("turn_started", {
    ...(props.providerId ? { provider_id: props.providerId } : {}),
    ...modelAnalyticsProperties(props.modelId, props.reasoningEffort),
    ...(props.executionEnvironment ? { execution_environment: props.executionEnvironment } : {}),
    ...(props.sessionKind ? { session_kind: props.sessionKind } : {}),
  });
};

export const trackProviderRunCompleted = (props: {
  providerId?: string;
  modelId?: string;
  status: "completed" | "failed" | "interrupted";
  durationMs?: number;
  sessionKind?: AnalyticsSessionKind;
  failureKind?: TurnFailureKind;
}): void => {
  capture("provider_run_completed", {
    ...(props.providerId ? { provider_id: props.providerId } : {}),
    ...(props.modelId ? { model_id: props.modelId } : {}),
    status: props.status,
    duration_bucket: durationBucketForMs(props.durationMs),
    ...(props.sessionKind ? { session_kind: props.sessionKind } : {}),
    ...(props.status !== "completed" ? { failure_kind: props.failureKind ?? "unknown" } : {}),
  });
};

export const trackTurnCompleted = (props: {
  providerId?: string;
  modelId?: string;
  reasoningEffort?: string | null;
  executionEnvironment?: ExecutionEnvironment;
  status: "completed" | "failed" | "interrupted";
  durationMs?: number;
  sessionKind?: AnalyticsSessionKind;
  metrics?: unknown;
  failureKind?: TurnFailureKind;
}): void => {
  capture("turn_completed", {
    ...(props.providerId ? { provider_id: props.providerId } : {}),
    ...modelAnalyticsProperties(props.modelId, props.reasoningEffort),
    ...(props.executionEnvironment ? { execution_environment: props.executionEnvironment } : {}),
    status: props.status,
    duration_bucket: durationBucketForMs(props.durationMs),
    ...(props.sessionKind ? { session_kind: props.sessionKind } : {}),
    ...(props.status !== "completed" ? { failure_kind: props.failureKind ?? "unknown" } : {}),
    ...tokenUsageProperties(props.metrics),
  });
};

export const trackFirstTurnCompleted = (props: {
  sessionId: string;
  providerId?: string;
  status: "completed" | "failed" | "interrupted";
  sessionKind?: AnalyticsSessionKind;
}): void => {
  if (props.status !== "completed") return;
  if (!markOnce(FIRST_TURN_COMPLETED_ONCE_KEY)) return;
  capture("first_turn_completed", {
    ...(props.providerId ? { provider_id: props.providerId } : {}),
    status: props.status,
    ...(props.sessionKind ? { session_kind: props.sessionKind } : {}),
  });
};

export const trackFeatureUsed = (
  featureKey: string,
  extra: AnalyticsProperties = {},
): void => {
  capture("feature_used", { feature_key: featureKey, ...extra });
};

export const trackWorkbenchPanelToggled = (props: {
  panelKey: "terminal" | "diff" | "artifacts" | "sessions";
  open: boolean;
  source: "header_button" | "menu_command" | "unknown";
}): void => {
  capture("workbench_panel_toggled", {
    panel_key: props.panelKey,
    open: props.open,
    source: props.source,
  });
};

export const trackPlanViewed = (entrySurface: string): void => {
  capture("plan_viewed", { entry_surface: entrySurface });
};

export const trackSubscribeCtaClicked = (planTarget: "month" | "year"): void => {
  capture("subscribe_cta_clicked", { plan_target: planTarget });
};

export const trackCheckoutStarted = (planTarget: "month" | "year"): void => {
  capture("checkout_started", { plan_target: planTarget });
};

export const trackEntitlementActivated = (planType: string): void => {
  capture("entitlement_activated", { plan_type: planType });
};

export const trackFeatureGateEvaluated = (props: {
  gateKey: string;
  result: boolean;
  reason: "override" | "posthog" | "fallback";
}): boolean => {
  return capture("feature_gate_evaluated", {
    gate_key: props.gateKey,
    result: props.result ? "enabled" : "disabled",
    reason: props.reason,
  });
};

export const trackExperimentExposure = (props: {
  experimentKey: string;
  variant: string;
  assignmentUnit: "install_id" | "account_id";
}): boolean => {
  return capture("experiment_exposure", {
    experiment_key: props.experimentKey,
    variant: props.variant,
    assignment_unit: props.assignmentUnit,
  });
};
