import {
  recordClientCounterMetric,
  recordClientGaugeMetric,
  recordClientHistogramMetric,
} from "../api/client";
import { emitUiDiagnostic } from "./diagnosticsChannel";
import {
  trackForegroundBacklogObserved,
  trackForegroundFreshnessSlaMissed,
  trackForegroundGapRecoveryObserved,
  trackFreshnessRecovered,
  trackRendererBacklogSample,
  trackRendererBacklogSpike,
} from "../utils/analytics";
import {
  backlogBucketForDuration,
  type ForegroundFreshnessQueueLane,
  type ForegroundFreshnessSurface,
  gapBucketForDuration,
  severityBucketForDuration,
} from "./foregroundFreshnessTelemetryBuckets";

const SWITCH_FIRST_PAINT_SLA_MS = 100;
const SWITCH_AUTHORITATIVE_SLA_MS = 200;
const FINAL_WS_TO_DOM_SLA_MS = 50;
const FINAL_INGRESS_TO_DOM_SLA_MS = 150;
const INTERRUPT_TO_PENDING_SLA_MS = 50;
const GAP_RECOVERY_SLA_MS = 1000;
const FOREGROUND_QUEUE_AGE_SLA_MS = 75;
const WORKSPACE_QUEUE_AGE_SLA_MS = 250;
const FOREGROUND_CLIENT_RECEIVE_LAG_SLA_MS = 500;
const WORKSPACE_CLIENT_RECEIVE_LAG_SLA_MS = 1000;
const SESSION_REPLICA_APPLY_LAG_SLA_MS = 100;
const RENDERER_START_TIMEOUT_MS = 1000;
const FIRST_PAINT_TIMEOUT_MS = 500;
const SWITCH_FIRST_PAINT_ERROR_MS = 1000;
const SWITCH_AUTHORITATIVE_ERROR_MS = 2000;
const FINAL_DELIVERY_ERROR_MS = 10_000;
const INTERRUPT_TO_PENDING_ERROR_MS = 1500;
const FOREGROUND_CLIENT_RECEIVE_LAG_ERROR_MS = 20_000;
const WORKSPACE_CLIENT_RECEIVE_LAG_ERROR_MS = 30_000;
const SESSION_REPLICA_APPLY_LAG_ERROR_MS = 20_000;
const FOREGROUND_QUEUE_AGE_ERROR_MS = 20_000;
const WORKSPACE_QUEUE_AGE_ERROR_MS = 30_000;

const SLA_DIAGNOSTIC_DEDUPE_MS = 60_000;
const GAUGE_SAMPLE_INTERVAL_MS = 1000;

type Surface = ForegroundFreshnessSurface;
type QueueLane = ForegroundFreshnessQueueLane;

type PendingSwitch = {
  startedAtMs: number;
  firstPaintRecorded: boolean;
};

type PendingFinal = {
  receivedAtMs: number;
  emittedAtMs: number | null;
  lastEventSeq: number | null;
};

type PendingInterrupt = {
  startedAtMs: number;
  source: "thread_header" | "queued_action";
};

type PendingGapRecovery = {
  startedAtMs: number;
  lane: QueueLane;
};

type DesktopStartupState = {
  windowCreatedAtMs: number | null;
  rendererPingAtMs: number | null;
  daemonReadyAtMs: number | null;
};

const pendingSwitches = new Map<string, PendingSwitch>();
const pendingFinals = new Map<string, PendingFinal>();
const pendingInterrupts = new Map<string, PendingInterrupt>();
const pendingGapRecoveries = new Map<string, PendingGapRecovery>();
const pendingGapRecoveryTimeouts = new Map<string, ReturnType<typeof globalThis.setTimeout>>();
const lastSlaDiagnosticByKey = new Map<string, number>();
const lastGaugeSampleByMetric = new Map<string, number>();
const backlogDegradedSinceByLane = new Map<QueueLane, number>();
const lastBacklogObservedBucketByLane = new Map<QueueLane, ReturnType<typeof backlogBucketForDuration>>();
const seenInvariantKeys = new Set<string>();
const desktopStartupState: DesktopStartupState = {
  windowCreatedAtMs: null,
  rendererPingAtMs: null,
  daemonReadyAtMs: null,
};

const nowMs = (): number => {
  if (typeof performance !== "undefined" && typeof performance.now === "function") {
    return (performance.timeOrigin ?? Date.now()) + performance.now();
  }
  return Date.now();
};

const finalKey = (sessionId: string, turnId: string): string => `${sessionId}:${turnId}`;

const normalizeGapRecoveryLane = (lane?: QueueLane | null): QueueLane =>
  lane === "workspace" ? "workspace" : "foreground";

const gapRecoveryMetric = (lane: QueueLane, suffix: "ms" | "timeout_count"): string =>
  `workbench.${lane}_gap_recovery_${suffix}`;

const shouldEmitSlaDiagnostic = (key: string): boolean => {
  const currentMs = nowMs();
  const previousMs = lastSlaDiagnosticByKey.get(key);
  if (typeof previousMs === "number" && currentMs - previousMs < SLA_DIAGNOSTIC_DEDUPE_MS) {
    return false;
  }
  lastSlaDiagnosticByKey.set(key, currentMs);
  return true;
};

const maybeEmitGaugeSample = (
  metric: string,
  value: number,
  thresholdMs: number,
  lane: QueueLane,
  context?: Record<string, unknown>,
  errorThresholdMs = thresholdMs * 4,
) => {
  if (!Number.isFinite(value) || value < 0) return;
  const source = typeof context?.source === "string" && context.source.trim() ? context.source : "unknown";
  const currentMs = nowMs();
  const isDegraded = value >= thresholdMs;
  if (isDegraded) {
    const bucket = backlogBucketForDuration(value);
    const previousBucket = lastBacklogObservedBucketByLane.get(lane);
    if (previousBucket !== bucket) {
      lastBacklogObservedBucketByLane.set(lane, bucket);
      trackForegroundBacklogObserved({
        lane,
        bucket,
      });
    }
  } else {
    lastBacklogObservedBucketByLane.delete(lane);
  }
  if (isDegraded && !backlogDegradedSinceByLane.has(lane)) {
    backlogDegradedSinceByLane.set(lane, currentMs);
    trackRendererBacklogSpike({
      lane,
      source,
      ageMs: value,
      thresholdMs,
    });
  }
  const previousMs = lastGaugeSampleByMetric.get(metric);
  if (typeof previousMs === "number" && currentMs - previousMs < GAUGE_SAMPLE_INTERVAL_MS) {
    return;
  }
  lastGaugeSampleByMetric.set(metric, currentMs);
  recordClientGaugeMetric(metric, "ms", value, { lane });
  trackRendererBacklogSample({
    lane,
    source,
    ageMs: value,
  });
  if (!isDegraded) return;
  const key = `${metric}:${lane}`;
  if (!shouldEmitSlaDiagnostic(key)) return;
  emitUiDiagnostic({
    source: "foreground_freshness",
    code: `${metric}.sla_missed`,
    severity: value >= errorThresholdMs ? "error" : "warning",
    message: `${lane} backlog age crossed the freshness budget.`,
    context: {
      metric,
      lane,
      value_ms: Math.round(value),
      threshold_ms: thresholdMs,
      ...(context ?? {}),
    },
  });
  trackForegroundFreshnessSlaMissed({
    metric,
    surface: lane === "foreground" ? "foreground_backlog" : "workspace_backlog",
    bucket: severityBucketForDuration(value),
  });
};

const recordLatencyMetric = (args: {
  metric: string;
  valueMs: number;
  thresholdMs: number;
  surface: Surface;
  labels?: Record<string, string>;
  diagnosticCode: string;
  message: string;
  context?: Record<string, unknown>;
  errorThresholdMs?: number;
}) => {
  if (!Number.isFinite(args.valueMs) || args.valueMs < 0) return;
  recordClientHistogramMetric(args.metric, "ms", args.valueMs, args.labels);
  if (args.valueMs <= args.thresholdMs) return;
  const key = `${args.metric}:${args.diagnosticCode}`;
  if (shouldEmitSlaDiagnostic(key)) {
    emitUiDiagnostic({
      source: "foreground_freshness",
      code: args.diagnosticCode,
      severity:
        args.valueMs >= (args.errorThresholdMs ?? args.thresholdMs * 4) ? "error" : "warning",
      message: args.message,
      context: {
        metric: args.metric,
        value_ms: Math.round(args.valueMs),
        threshold_ms: args.thresholdMs,
        ...(args.context ?? {}),
      },
    });
  }
  trackForegroundFreshnessSlaMissed({
    metric: args.metric,
    surface: args.surface,
    bucket: severityBucketForDuration(args.valueMs),
  });
};

const recordInvariantCounter = (
  metric: string,
  dedupeKey: string,
  labels?: Record<string, string>,
): void => {
  const normalizedKey = `${metric}:${dedupeKey}`;
  if (seenInvariantKeys.has(normalizedKey)) {
    return;
  }
  seenInvariantKeys.add(normalizedKey);
  recordClientCounterMetric(metric, labels);
};

export const noteSessionSwitchStarted = (fromSessionId: string | null, toSessionId: string | null): void => {
  const targetSessionId = String(toSessionId ?? "").trim();
  if (!targetSessionId) return;
  pendingSwitches.set(targetSessionId, {
    startedAtMs: nowMs(),
    firstPaintRecorded: false,
  });
  recordClientCounterMetric("workbench.session_switch_started_count", {
    has_previous: fromSessionId ? "true" : "false",
  });
};

export const noteSessionSwitchFirstPaint = (sessionId: string): void => {
  const pending = pendingSwitches.get(sessionId);
  if (!pending || pending.firstPaintRecorded) return;
  pending.firstPaintRecorded = true;
  recordLatencyMetric({
    metric: "workbench.switch_to_first_paint_ms",
    valueMs: nowMs() - pending.startedAtMs,
    thresholdMs: SWITCH_FIRST_PAINT_SLA_MS,
    surface: "session_switch",
    diagnosticCode: "switch.first_paint_sla_missed",
    message: "Foreground session switch missed first-paint freshness budget.",
    labels: { phase: "first_paint" },
    context: { session_id: sessionId },
    errorThresholdMs: SWITCH_FIRST_PAINT_ERROR_MS,
  });
};

export const noteSessionSwitchAuthoritative = (sessionId: string): void => {
  const pending = pendingSwitches.get(sessionId);
  if (!pending) return;
  recordLatencyMetric({
    metric: "workbench.switch_to_authoritative_ms",
    valueMs: nowMs() - pending.startedAtMs,
    thresholdMs: SWITCH_AUTHORITATIVE_SLA_MS,
    surface: "session_switch",
    diagnosticCode: "switch.authoritative_sla_missed",
    message: "Foreground session switch missed authoritative freshness budget.",
    labels: { phase: "authoritative" },
    context: { session_id: sessionId },
    errorThresholdMs: SWITCH_AUTHORITATIVE_ERROR_MS,
  });
  pendingSwitches.delete(sessionId);
};

export const noteInterruptClicked = (
  sessionId: string,
  source: PendingInterrupt["source"],
): void => {
  const normalizedSessionId = String(sessionId).trim();
  if (!normalizedSessionId) return;
  pendingInterrupts.set(normalizedSessionId, {
    startedAtMs: nowMs(),
    source,
  });
};

export const noteInterruptPendingVisible = (sessionId: string): void => {
  const pending = pendingInterrupts.get(sessionId);
  if (!pending) return;
  recordLatencyMetric({
    metric: "workbench.interrupt_click_to_pending_ms",
    valueMs: nowMs() - pending.startedAtMs,
    thresholdMs: INTERRUPT_TO_PENDING_SLA_MS,
    surface: "interrupt",
    diagnosticCode: "interrupt.pending_sla_missed",
    message: "Interrupt pending UI missed the freshness budget.",
    labels: { source: pending.source },
    context: { session_id: sessionId, source: pending.source },
    errorThresholdMs: INTERRUPT_TO_PENDING_ERROR_MS,
  });
  pendingInterrupts.delete(sessionId);
};

export const clearInterruptPendingMetric = (sessionId: string): void => {
  pendingInterrupts.delete(sessionId);
};

export const noteFinalDeltaReceived = (args: {
  sessionId: string;
  turnId: string | null | undefined;
  emittedAtMs?: number | null;
  lastEventSeq?: number | null;
}): void => {
  const sessionId = String(args.sessionId).trim();
  const turnId = String(args.turnId ?? "").trim();
  if (!sessionId || !turnId) return;
  pendingFinals.set(finalKey(sessionId, turnId), {
    receivedAtMs: nowMs(),
    emittedAtMs:
      typeof args.emittedAtMs === "number" && Number.isFinite(args.emittedAtMs) ? args.emittedAtMs : null,
    lastEventSeq:
      typeof args.lastEventSeq === "number" && Number.isFinite(args.lastEventSeq)
        ? args.lastEventSeq
        : null,
  });
};

export const noteClientReceiveLag = (
  lane: QueueLane,
  lagMs: number,
  context?: Record<string, unknown>,
): void => {
  const streamSource =
    typeof context?.stream_source === "string" && context.stream_source.trim()
      ? context.stream_source
      : "unknown";
  recordLatencyMetric({
    metric: "workbench.client_receive_lag_ms",
    valueMs: lagMs,
    thresholdMs:
      lane === "foreground" ? FOREGROUND_CLIENT_RECEIVE_LAG_SLA_MS : WORKSPACE_CLIENT_RECEIVE_LAG_SLA_MS,
    surface: lane === "foreground" ? "foreground_backlog" : "workspace_backlog",
    labels: { lane, stream_source: streamSource },
    diagnosticCode: `client_receive_lag.${lane}.sla_missed`,
    message: `${lane} stream event missed the daemon-to-browser receive budget.`,
    context: {
      lane,
      ...(context ?? {}),
    },
    errorThresholdMs:
      lane === "foreground"
        ? FOREGROUND_CLIENT_RECEIVE_LAG_ERROR_MS
        : WORKSPACE_CLIENT_RECEIVE_LAG_ERROR_MS,
  });
};

export const noteWorkspaceEventAge = (
  lane: QueueLane,
  ageMs: number,
  context?: Record<string, unknown>,
): void => {
  if (!Number.isFinite(ageMs) || ageMs < 0) return;
  const streamSource =
    typeof context?.stream_source === "string" && context.stream_source.trim()
      ? context.stream_source
      : "unknown";
  const eventType =
    typeof context?.event_type === "string" && context.event_type.trim()
      ? context.event_type
      : "unknown";
  recordClientHistogramMetric("workbench.workspace_event_age_ms", "ms", ageMs, {
    lane,
    stream_source: streamSource,
    event_type: eventType,
  });
};

export const noteWorkspaceStreamEventObserved = (
  lane: QueueLane,
  eventType: string,
): void => {
  const normalizedEventType = String(eventType).trim() || "unknown";
  recordClientCounterMetric("workbench.workspace_stream_event_count", {
    lane,
    event_type: normalizedEventType,
  });
};

export const noteSessionReplicaApplyLag = (
  lagMs: number,
  context?: Record<string, unknown>,
): void => {
  const lagSource =
    typeof context?.lag_source === "string" && context.lag_source.trim()
      ? context.lag_source
      : "unknown";
  const op =
    typeof context?.op === "string" && context.op.trim()
      ? context.op
      : typeof context?.event_type === "string" && context.event_type.trim()
        ? context.event_type
        : "unknown";
  recordLatencyMetric({
    metric: "workbench.session_replica_apply_lag_ms",
    valueMs: lagMs,
    thresholdMs: SESSION_REPLICA_APPLY_LAG_SLA_MS,
    surface: "foreground_backlog",
    labels: {
      op,
      lag_source: lagSource,
    },
    diagnosticCode: "session_replica.apply_lag_sla_missed",
    message: "Session replica patches missed the apply freshness budget.",
    context,
    errorThresholdMs: SESSION_REPLICA_APPLY_LAG_ERROR_MS,
  });
};

export const noteSessionReplicaEventAge = (
  ageMs: number,
  context?: Record<string, unknown>,
): void => {
  if (!Number.isFinite(ageMs) || ageMs < 0) return;
  const eventType =
    typeof context?.event_type === "string" && context.event_type.trim()
      ? context.event_type
      : "unknown";
  const streamSource =
    typeof context?.stream_source === "string" && context.stream_source.trim()
      ? context.stream_source
      : "unknown";
  recordClientHistogramMetric("workbench.session_replica_event_age_ms", "ms", ageMs, {
    op: eventType,
    stream_source: streamSource,
  });
};

export const noteSessionReplicaApplyDuration = (
  durationMs: number,
  context?: Record<string, unknown>,
): void => {
  const patchCount =
    typeof context?.patch_count === "number" && Number.isFinite(context.patch_count)
      ? String(context.patch_count)
      : "unknown";
  const op =
    typeof context?.op === "string" && context.op.trim()
      ? context.op
      : "mixed";
  recordLatencyMetric({
    metric: "workbench.session_replica_apply_duration_ms",
    valueMs: durationMs,
    thresholdMs: SESSION_REPLICA_APPLY_LAG_SLA_MS,
    surface: "foreground_backlog",
    labels: {
      patch_count: patchCount,
      op,
    },
    diagnosticCode: "session_replica.apply_duration_sla_missed",
    message: "Session replica patches missed the apply duration budget.",
    context,
    errorThresholdMs: SESSION_REPLICA_APPLY_LAG_ERROR_MS,
  });
};

export const noteFinalVisible = (sessionId: string, turnIds: readonly string[]): void => {
  const normalizedSessionId = String(sessionId).trim();
  if (!normalizedSessionId || turnIds.length === 0) return;
  const currentMs = nowMs();
  for (const rawTurnId of turnIds) {
    const turnId = String(rawTurnId).trim();
    if (!turnId) continue;
    const key = finalKey(normalizedSessionId, turnId);
    const pending = pendingFinals.get(key);
    if (!pending) continue;
    recordLatencyMetric({
      metric: "workbench.final_ws_to_dom_ms",
      valueMs: currentMs - pending.receivedAtMs,
      thresholdMs: FINAL_WS_TO_DOM_SLA_MS,
      surface: "final_delivery",
      diagnosticCode: "final.ws_to_dom_sla_missed",
      message: "Foreground final message missed the websocket-to-DOM freshness budget.",
      context: {
        session_id: normalizedSessionId,
        turn_id: turnId,
        last_event_seq: pending.lastEventSeq,
      },
      errorThresholdMs: FINAL_DELIVERY_ERROR_MS,
    });
    if (typeof pending.emittedAtMs === "number") {
      recordLatencyMetric({
        metric: "workbench.final_ingress_to_dom_ms",
        valueMs: currentMs - pending.emittedAtMs,
        thresholdMs: FINAL_INGRESS_TO_DOM_SLA_MS,
        surface: "final_delivery",
        diagnosticCode: "final.ingress_to_dom_sla_missed",
        message: "Foreground final message missed the daemon-ingress-to-DOM freshness budget.",
        context: {
          session_id: normalizedSessionId,
          turn_id: turnId,
          last_event_seq: pending.lastEventSeq,
        },
        errorThresholdMs: FINAL_DELIVERY_ERROR_MS,
      });
    }
    pendingFinals.delete(key);
  }
};

export const noteGapRecoveryStarted = (
  sessionId: string,
  reason?: string | null,
  lane?: QueueLane | null,
): void => {
  const normalizedSessionId = String(sessionId).trim();
  if (!normalizedSessionId) return;
  const recoveryLane = normalizeGapRecoveryLane(lane);
  const existingTimeout = pendingGapRecoveryTimeouts.get(normalizedSessionId);
  if (existingTimeout) {
    globalThis.clearTimeout(existingTimeout);
  }
  pendingGapRecoveries.set(normalizedSessionId, { startedAtMs: nowMs(), lane: recoveryLane });
  pendingGapRecoveryTimeouts.set(
    normalizedSessionId,
    globalThis.setTimeout(() => {
      const pending = pendingGapRecoveries.get(normalizedSessionId);
      if (!pending) {
        pendingGapRecoveryTimeouts.delete(normalizedSessionId);
        return;
      }
      const metric = gapRecoveryMetric(pending.lane, "timeout_count");
      recordClientCounterMetric(metric);
      if (pending.lane === "foreground") {
        trackForegroundGapRecoveryObserved({ result: "timeout" });
        trackForegroundFreshnessSlaMissed({
          metric,
          surface: "gap_recovery",
          bucket: "severe",
        });
      }
      if (shouldEmitSlaDiagnostic(`${pending.lane}_gap_recovery.timeout:${normalizedSessionId}`)) {
        emitUiDiagnostic({
          source: "foreground_freshness",
          code: `${pending.lane}_gap_recovery.timeout`,
          severity: "error",
          message:
            pending.lane === "foreground"
              ? "Foreground session gap recovery exceeded the timeout budget."
              : "Workspace session gap recovery exceeded the timeout budget.",
          context: {
            session_id: normalizedSessionId,
            lane: pending.lane,
            threshold_ms: GAP_RECOVERY_SLA_MS,
          },
        });
      }
      pendingGapRecoveryTimeouts.delete(normalizedSessionId);
    }, GAP_RECOVERY_SLA_MS),
  );
  recordClientCounterMetric(`workbench.${recoveryLane}_rehydrate_count`);
  if (recoveryLane === "foreground") {
    trackForegroundGapRecoveryObserved({ result: "started" });
  }
  emitUiDiagnostic({
    source: "foreground_freshness",
    code: `${recoveryLane}_gap_recovery.started`,
    severity: "info",
    message:
      recoveryLane === "foreground"
        ? "Foreground session entered gap recovery."
        : "Workspace session entered gap recovery.",
    context: {
      session_id: normalizedSessionId,
      lane: recoveryLane,
      ...(reason ? { reason } : {}),
    },
  });
};

export const noteGapRecoveryFinished = (sessionId: string): void => {
  const normalizedSessionId = String(sessionId).trim();
  const pending = pendingGapRecoveries.get(normalizedSessionId);
  const timeoutId = pendingGapRecoveryTimeouts.get(normalizedSessionId);
  if (timeoutId) {
    globalThis.clearTimeout(timeoutId);
    pendingGapRecoveryTimeouts.delete(normalizedSessionId);
  }
  if (!pending) return;
  const durationMs = nowMs() - pending.startedAtMs;
  const metric = gapRecoveryMetric(pending.lane, "ms");
  if (pending.lane === "foreground") {
    recordLatencyMetric({
      metric,
      valueMs: durationMs,
      thresholdMs: GAP_RECOVERY_SLA_MS,
      surface: "gap_recovery",
      diagnosticCode: "foreground_gap_recovery.sla_missed",
      message: "Foreground session gap recovery missed the freshness budget.",
      context: {
        session_id: normalizedSessionId,
        lane: pending.lane,
      },
    });
    trackForegroundGapRecoveryObserved({
      result: "recovered",
      bucket: gapBucketForDuration(durationMs),
    });
  } else {
    recordClientHistogramMetric(metric, "ms", durationMs);
    if (
      durationMs > GAP_RECOVERY_SLA_MS &&
      shouldEmitSlaDiagnostic(`workspace_gap_recovery.sla_missed:${normalizedSessionId}`)
    ) {
      emitUiDiagnostic({
        source: "foreground_freshness",
        code: "workspace_gap_recovery.sla_missed",
        severity: durationMs >= GAP_RECOVERY_SLA_MS * 4 ? "error" : "warning",
        message: "Workspace session gap recovery missed the freshness budget.",
        context: {
          metric,
          session_id: normalizedSessionId,
          lane: pending.lane,
          value_ms: Math.round(durationMs),
          threshold_ms: GAP_RECOVERY_SLA_MS,
        },
      });
    }
  }
  pendingGapRecoveries.delete(normalizedSessionId);
};

export const noteWorkspaceStreamReset = (): void => {
  recordClientCounterMetric("workbench.workspace_stream_reset_count");
};

export const noteQueueAgeSample = (
  lane: QueueLane,
  ageMs: number,
  context?: Record<string, unknown>,
): void => {
  const thresholdMs = lane === "foreground" ? FOREGROUND_QUEUE_AGE_SLA_MS : WORKSPACE_QUEUE_AGE_SLA_MS;
  if (Number.isFinite(ageMs) && ageMs >= 0 && ageMs < thresholdMs) {
    const degradedSince = backlogDegradedSinceByLane.get(lane);
    if (typeof degradedSince === "number") {
      backlogDegradedSinceByLane.delete(lane);
      lastBacklogObservedBucketByLane.delete(lane);
      const source = typeof context?.source === "string" && context.source.trim() ? context.source : "unknown";
      trackFreshnessRecovered({
        lane,
        source,
        degradedForMs: Math.max(0, nowMs() - degradedSince),
      });
    }
  }
  maybeEmitGaugeSample(
    lane === "foreground" ? "workbench.foreground_queue_age_ms" : "workbench.workspace_backlog_age_ms",
    ageMs,
    thresholdMs,
    lane,
    context,
    lane === "foreground" ? FOREGROUND_QUEUE_AGE_ERROR_MS : WORKSPACE_QUEUE_AGE_ERROR_MS,
  );
};

export const noteLateChunkAfterTerminal = (turnId: string): void => {
  const normalizedTurnId = String(turnId).trim();
  if (!normalizedTurnId) return;
  recordInvariantCounter("workbench.late_chunk_after_terminal_count", normalizedTurnId);
};

export const noteProjectionOrSeqRegression = (
  sessionId: string,
  dimension: "last_event_seq" | "projection_rev",
  incoming: number,
  existing: number,
): void => {
  const normalizedSessionId = String(sessionId).trim();
  if (!normalizedSessionId) return;
  recordInvariantCounter(
    "workbench.projection_or_seq_regression_count",
    `${normalizedSessionId}:${dimension}:${incoming}:${existing}`,
    { dimension },
  );
};

export const noteStaleHeadDeltaDropped = (
  sessionId: string,
  dimension: "last_event_seq" | "projection_rev",
  incoming: number,
  existing: number,
): void => {
  const normalizedSessionId = String(sessionId).trim();
  if (!normalizedSessionId) return;
  recordInvariantCounter(
    "workbench.stale_head_delta_dropped_count",
    `${normalizedSessionId}:${dimension}:${incoming}:${existing}`,
    { dimension },
  );
};

export const noteGapRepairMismatch = (
  sessionId: string,
  baselineLastEventSeq: number | null,
  repairedLastEventSeq: number | null,
): void => {
  const normalizedSessionId = String(sessionId).trim();
  if (!normalizedSessionId) return;
  recordInvariantCounter(
    "workbench.gap_repair_mismatch_count",
    `${normalizedSessionId}:${baselineLastEventSeq ?? "none"}:${repairedLastEventSeq ?? "none"}`,
  );
};

export const noteSwitchStaleVisible = (
  taskId: string,
  visibleSessionId: string,
  targetSessionId: string,
): void => {
  const normalizedTaskId = String(taskId).trim();
  const normalizedVisibleSessionId = String(visibleSessionId).trim();
  const normalizedTargetSessionId = String(targetSessionId).trim();
  if (!normalizedTaskId || !normalizedVisibleSessionId || !normalizedTargetSessionId) return;
  recordInvariantCounter(
    "workbench.switch_stale_visible_count",
    `${normalizedTaskId}:${normalizedVisibleSessionId}:${normalizedTargetSessionId}`,
  );
};

export const noteNavThreadActivityMismatch = (
  taskId: string,
  sessionId: string,
  navWorking: boolean,
  threadWorking: boolean,
): void => {
  const normalizedTaskId = String(taskId).trim();
  const normalizedSessionId = String(sessionId).trim();
  if (!normalizedTaskId || !normalizedSessionId) return;
  recordInvariantCounter(
    "workbench.nav_thread_activity_mismatch_count",
    `${normalizedTaskId}:${normalizedSessionId}:${navWorking}:${threadWorking}`,
  );
};

export const noteDesktopWindowCreated = (windowCreatedAtMs: number): void => {
  if (!Number.isFinite(windowCreatedAtMs)) return;
  desktopStartupState.windowCreatedAtMs = windowCreatedAtMs;
};

export const noteDesktopRendererPing = (): void => {
  const currentMs = nowMs();
  desktopStartupState.rendererPingAtMs = currentMs;
  if (typeof desktopStartupState.windowCreatedAtMs === "number") {
    recordLatencyMetric({
      metric: "desktop.window_create_to_renderer_ping_ms",
      valueMs: currentMs - desktopStartupState.windowCreatedAtMs,
      thresholdMs: RENDERER_START_TIMEOUT_MS,
      surface: "desktop_startup",
      diagnosticCode: "desktop.renderer_ping_sla_missed",
      message: "Desktop renderer ping missed startup budget.",
    });
  }
};

export const noteDesktopFirstPaint = (): void => {
  const currentMs = nowMs();
  if (typeof desktopStartupState.rendererPingAtMs === "number") {
    recordLatencyMetric({
      metric: "desktop.renderer_ping_to_first_paint_ms",
      valueMs: currentMs - desktopStartupState.rendererPingAtMs,
      thresholdMs: FIRST_PAINT_TIMEOUT_MS,
      surface: "desktop_startup",
      diagnosticCode: "desktop.first_paint_sla_missed",
      message: "Desktop renderer first paint missed startup budget.",
    });
  }
};

export const noteDesktopDaemonReady = (): void => {
  desktopStartupState.daemonReadyAtMs = nowMs();
  recordClientCounterMetric("desktop.daemon_ready_count");
};

export const noteDesktopRendererTimeout = (): void => {
  recordClientCounterMetric("desktop.renderer_start_timeout_count");
  if (!shouldEmitSlaDiagnostic("desktop.renderer_start_timeout")) return;
  emitUiDiagnostic({
    source: "desktop_startup",
    code: "desktop.renderer_start_timeout",
    severity: "error",
    message: "Desktop renderer did not become responsive before startup timeout.",
  });
  trackForegroundFreshnessSlaMissed({
    metric: "desktop.renderer_start_timeout_count",
    surface: "desktop_startup",
    bucket: "severe",
  });
};

export const resetForegroundFreshnessTelemetryForTests = (): void => {
  for (const timeoutId of pendingGapRecoveryTimeouts.values()) {
    globalThis.clearTimeout(timeoutId);
  }
  pendingSwitches.clear();
  pendingFinals.clear();
  pendingInterrupts.clear();
  pendingGapRecoveries.clear();
  pendingGapRecoveryTimeouts.clear();
  lastSlaDiagnosticByKey.clear();
  lastGaugeSampleByMetric.clear();
  backlogDegradedSinceByLane.clear();
  lastBacklogObservedBucketByLane.clear();
  seenInvariantKeys.clear();
  desktopStartupState.windowCreatedAtMs = null;
  desktopStartupState.rendererPingAtMs = null;
  desktopStartupState.daemonReadyAtMs = null;
};
