import { captureIncidentEvent, captureProductEvent } from "./client";
import type { AnalyticsProperties } from "./types";
import { parseModelId } from "../modelEffort";

export const FIRST_TURN_SUBMITTED_ONCE_KEY = "ctx.analytics.first_turn_submitted.install_once.v1";
export const FIRST_TURN_COMPLETED_ONCE_KEY = "ctx.analytics.first_turn_completed.install_once.v1";
export const PENDING_WORKSPACE_LAUNCH_KEY_PREFIX = "ctx.analytics.pending_workspace_launch.v1.";

export const markOnce = (key: string): boolean => {
  if (typeof window === "undefined") return true;
  try {
    if (window.localStorage.getItem(key)) return false;
    window.localStorage.setItem(key, "1");
    return true;
  } catch {
    return true;
  }
};

export const capture = (eventName: string, properties: AnalyticsProperties): boolean => {
  return captureProductEvent(eventName, 1, properties);
};

export const captureIncident = (
  eventName: string,
  properties: AnalyticsProperties,
  options?: { delivery?: "remote" | "local_only"; source?: string },
): boolean => {
  return captureIncidentEvent(eventName, 1, properties, options);
};

export const unknownEventTypeClass = (originalType: string): string => {
  const normalized = originalType.trim().toLowerCase();
  if (!normalized || normalized === "unknown") return "unknown";
  if (normalized === "tool.output_delta") return "tool_output_delta";
  if (normalized === "tool.output.delta") return "tool_output_delta_dotted";
  if (normalized.startsWith("tool.") || normalized.startsWith("tool_")) return "tool_other";
  if (normalized.startsWith("message.") || normalized.startsWith("message_")) return "message_other";
  if (normalized.startsWith("session.") || normalized.startsWith("session_")) return "session_other";
  if (normalized.startsWith("turn.") || normalized.startsWith("turn_")) return "turn_other";
  return "other";
};

export const durationBucketForMs = (durationMs: number | undefined): string => {
  if (!Number.isFinite(durationMs) || durationMs === undefined || durationMs < 0) {
    return "unknown";
  }
  if (durationMs < 15_000) return "under_15s";
  if (durationMs < 60_000) return "15s_to_60s";
  if (durationMs < 5 * 60_000) return "1m_to_5m";
  if (durationMs < 15 * 60_000) return "5m_to_15m";
  return "15m_plus";
};

const asRecord = (value: unknown): Record<string, unknown> =>
  value && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : {};

const readFiniteNumber = (
  record: Record<string, unknown>,
  key: string,
): number | undefined => {
  const value = record[key];
  if (typeof value === "number" && Number.isFinite(value)) return value;
  if (typeof value === "string" && value.trim()) {
    const parsed = Number.parseFloat(value);
    if (Number.isFinite(parsed)) return parsed;
  }
  return undefined;
};

export const tokenUsageProperties = (metrics: unknown): AnalyticsProperties => {
  const record = asRecord(metrics);
  const totalTokensEstimate = readFiniteNumber(record, "context_tokens_estimate");
  const inputTokens = readFiniteNumber(record, "total_input_tokens");
  const outputTokens = readFiniteNumber(record, "total_output_tokens");
  const contextWindowTokens = readFiniteNumber(record, "context_window_tokens");
  const remainingTokensEstimate = readFiniteNumber(record, "remaining_tokens_estimate");
  const remainingFraction = readFiniteNumber(record, "remaining_fraction");
  return {
    ...(totalTokensEstimate !== undefined ? { total_tokens_estimate: totalTokensEstimate } : {}),
    ...(inputTokens !== undefined ? { input_tokens: inputTokens } : {}),
    ...(outputTokens !== undefined ? { output_tokens: outputTokens } : {}),
    ...(contextWindowTokens !== undefined ? { context_window_tokens: contextWindowTokens } : {}),
    ...(remainingTokensEstimate !== undefined ? { remaining_tokens_estimate: remainingTokensEstimate } : {}),
    ...(remainingFraction !== undefined ? { remaining_fraction: remainingFraction } : {}),
  };
};

export const modelAnalyticsProperties = (
  modelId: string | undefined,
  reasoningEffort: string | null | undefined,
): AnalyticsProperties => {
  const rawModelId = String(modelId ?? "").trim();
  const parsedModel = rawModelId ? parseModelId(rawModelId) : null;
  const normalizedReasoningEffort = String(reasoningEffort ?? "").trim() || parsedModel?.effort || undefined;
  const normalizedModelId =
    parsedModel?.base && parsedModel.base !== rawModelId
      ? parsedModel.base
      : rawModelId || undefined;
  return {
    ...(normalizedModelId ? { model_id: normalizedModelId } : {}),
    ...(normalizedReasoningEffort ? { reasoning_effort: normalizedReasoningEffort } : {}),
  };
};
