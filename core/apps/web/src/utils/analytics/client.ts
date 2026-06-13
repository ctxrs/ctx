import posthog from "posthog-js";
import type { SemanticTelemetryDelivery, SemanticTelemetryEvent, SemanticTelemetryPlane } from "@ctx/types";
import { recordSemanticTelemetryEvent, setSemanticTelemetryRemoteEnabled } from "../../api/client";
import { getPostHogHost, getPostHogKey, getPostHogProjectId, getPostHogUiHost } from "./config";
import { getInstallId } from "./identity";
import { buildEventEnvelope, getAnalyticsSurface } from "./context";
import { sanitizeAnalyticsProperties } from "./schema";
import type { AnalyticsProperties } from "./types";

type FeatureFlagListener = () => void;

type FeatureFlagOverrides = Record<string, boolean>;
type FeatureFlagGlobals = {
  __CTX_FEATURE_FLAGS__?: unknown;
};

type PostHogFlagMethods = {
  onFeatureFlags?: (callback: () => void) => void;
  reloadFeatureFlags?: () => void;
};

const asFlagMethods = (): PostHogFlagMethods =>
  posthog as unknown as PostHogFlagMethods;
const featureFlagListeners = new Set<FeatureFlagListener>();

let initAttempted = false;
let initResolved = false;
let captureEnabled = false;

const readFeatureOverrides = (): FeatureFlagOverrides | null => {
  if (typeof globalThis === "undefined") return null;
  const raw = (globalThis as typeof globalThis & FeatureFlagGlobals).__CTX_FEATURE_FLAGS__;
  if (!raw || typeof raw !== "object") return null;
  return raw as FeatureFlagOverrides;
};

const notifyFeatureFlags = () => {
  for (const listener of featureFlagListeners) listener();
};

export const initAnalytics = (): void => {
  if (!captureEnabled) return;
  if (initAttempted) return;
  initAttempted = true;

  if (typeof window === "undefined") return;
  const key = getPostHogKey().trim();
  const host = getPostHogHost().trim();
  if (!key || !host) return;

  posthog.init(key, {
    api_host: host,
    ui_host: getPostHogUiHost().trim() || host,
    person_profiles: "identified_only",
    autocapture: false,
    capture_pageview: false,
    capture_pageleave: false,
    opt_out_capturing_by_default: !captureEnabled,
    loaded: () => {
      initResolved = true;
      const projectId = getPostHogProjectId().trim();
      posthog.register({
        install_id: getInstallId(),
        ...(projectId ? { posthog_project_id: projectId } : {}),
      });
      const flagMethods = asFlagMethods();
      flagMethods.onFeatureFlags?.(() => {
        notifyFeatureFlags();
      });
      flagMethods.reloadFeatureFlags?.();
      notifyFeatureFlags();
    },
  });
};

export const setAnalyticsEnabled = (enabled: boolean): void => {
  captureEnabled = enabled;
  setSemanticTelemetryRemoteEnabled(enabled);
  if (!enabled) {
    if (initAttempted) {
      posthog.opt_out_capturing();
    }
    return;
  }
  if (!initAttempted) {
    initAnalytics();
    return;
  }
  posthog.opt_in_capturing();
};

export const isAnalyticsCaptureEnabled = (): boolean => captureEnabled;

type CaptureSemanticEventOptions = {
  plane?: SemanticTelemetryPlane;
  delivery?: SemanticTelemetryDelivery;
  source?: string;
};

const createSemanticTelemetryEvent = (
  eventName: string,
  envelope: AnalyticsProperties,
  options?: CaptureSemanticEventOptions,
): SemanticTelemetryEvent | null => {
  if (!eventName.trim()) return null;
  const {
    occurred_at,
    app_version,
    os,
    arch,
    surface,
    event_version,
    ...properties
  } = envelope;
  const envTargetRaw = properties.env_target;
  const envTarget =
    envTargetRaw === "local" || envTargetRaw === "worktree" || envTargetRaw === "remote"
      ? envTargetRaw
      : null;
  return {
    event_id: typeof crypto !== "undefined" && typeof crypto.randomUUID === "function"
      ? crypto.randomUUID()
      : `${Date.now()}-${Math.random().toString(16).slice(2)}`,
    event_name: eventName.trim(),
    event_version: typeof event_version === "number" ? event_version : 1,
    occurred_at: typeof occurred_at === "string" ? occurred_at : new Date().toISOString(),
    plane: options?.plane ?? "product",
    delivery: options?.delivery ?? "remote",
    origin_runtime: getAnalyticsSurface(),
    origin_install_id: getInstallId(),
    app_version: typeof app_version === "string" ? app_version : "0.0.0",
    os: typeof os === "string" ? os : "unknown",
    arch: typeof arch === "string" ? arch : "unknown",
    surface: surface === "desktop" || surface === "mobile_shell" || surface === "web" ? surface : null,
    env_target: envTarget,
    source: options?.source?.trim() ? options.source.trim() : null,
    properties,
  };
};

const captureSemanticEvent = (
  eventName: string,
  eventVersion: number,
  rawProperties: Record<string, unknown>,
  options?: CaptureSemanticEventOptions,
): boolean => {
  const envelope = buildEventEnvelope(eventVersion, sanitizeAnalyticsProperties(rawProperties));
  const event = createSemanticTelemetryEvent(eventName, envelope, options);
  if (!event) return false;
  const localOnly = event.delivery === "local_only";
  if (!captureEnabled && !localOnly) return false;
  recordSemanticTelemetryEvent(event);
  return true;
};

export const captureAnalyticsEvent = (
  eventName: string,
  rawProperties: Record<string, unknown>,
  options?: CaptureSemanticEventOptions,
): boolean => {
  return captureSemanticEvent(eventName, 1, rawProperties, options);
};

export const captureProductEvent = (
  eventName: string,
  eventVersion: number,
  properties: Record<string, unknown> = {},
): boolean => {
  return captureSemanticEvent(eventName, eventVersion, properties);
};

export const captureIncidentEvent = (
  eventName: string,
  eventVersion: number,
  properties: Record<string, unknown> = {},
  options?: Omit<CaptureSemanticEventOptions, "plane">,
): boolean => {
  return captureSemanticEvent(eventName, eventVersion, properties, {
    ...options,
    plane: "incident",
  });
};

export const checkFeatureGate = (gate: string, fallback = false): boolean => {
  return evaluateFeatureGate(gate, fallback).value;
};

export const evaluateFeatureGate = (
  gate: string,
  fallback = false,
): { value: boolean; reason: "override" | "posthog" | "fallback" } => {
  const overrides = readFeatureOverrides();
  if (overrides && Object.prototype.hasOwnProperty.call(overrides, gate)) {
    return { value: Boolean(overrides[gate]), reason: "override" };
  }
  if (!initResolved) return { value: fallback, reason: "fallback" };
  const evaluated = posthog.isFeatureEnabled(gate);
  if (typeof evaluated !== "boolean") {
    return { value: fallback, reason: "fallback" };
  }
  return { value: evaluated, reason: "posthog" };
};

export const subscribeFeatureFlags = (listener: FeatureFlagListener): (() => void) => {
  featureFlagListeners.add(listener);
  return () => {
    featureFlagListeners.delete(listener);
  };
};
