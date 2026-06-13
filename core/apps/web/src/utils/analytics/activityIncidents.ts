import { capture, captureIncident, unknownEventTypeClass } from "./activityShared";

export const trackRuntimeErrorObserved = (props: {
  errorKey: string;
  severity: "warning" | "error";
  signature: string;
}): void => {
  captureIncident("runtime_error_observed", {
    error_key: props.errorKey,
    severity: props.severity,
    error_signature: props.signature,
  });
};

export const trackSessionLoadFatalObserved = (props: {
  mode: string;
  signature: string;
}): void => {
  captureIncident("session_load_fatal_observed", {
    mode: props.mode,
    error_signature: props.signature,
  });
};

export const trackApiErrorObserved = (props: {
  errorKey: string;
  endpoint: string;
  method: string;
  statusFamily: "2xx" | "3xx" | "4xx" | "5xx" | "none";
  signature: string;
}): void => {
  captureIncident("api_error_observed", {
    error_key: props.errorKey,
    api_endpoint: props.endpoint,
    method: props.method,
    status_family: props.statusFamily,
    error_signature: props.signature,
  });
};

export const trackDesktopWebviewRecoveryObserved = (props: {
  action: "noop" | "reload" | "recreate" | "prompt_restart";
  daemonHealth: "unknown" | "ok" | "down" | "mismatch";
  surface:
    | "main"
    | "workbench"
    | "launcher"
    | "settings"
    | "file_preview"
    | "workspace_setup"
    | "unknown";
  trigger: "native_process_termination" | "heartbeat_timeout";
  suppressionReason?:
    | "recovery_in_progress"
    | "window_not_visible"
    | "window_not_focused"
    | "startup_grace"
    | "no_heartbeat_yet"
    | "daemon_down"
    | "daemon_mismatch";
}): void => {
  capture("desktop_webview_recovery_observed", {
    trigger: props.trigger,
    action: props.action,
    surface: props.surface,
    daemon_health: props.daemonHealth,
    ...(props.suppressionReason ? { suppression_reason: props.suppressionReason } : {}),
  });
};

export const trackForegroundFreshnessSlaMissed = (props: {
  metric: string;
  surface:
    | "final_delivery"
    | "interrupt"
    | "session_switch"
    | "gap_recovery"
    | "workspace_backlog"
    | "foreground_backlog"
    | "desktop_startup";
  bucket: "slight" | "moderate" | "severe";
}): void => {
  captureIncident("foreground_freshness_sla_missed", {
    metric: props.metric,
    surface: props.surface,
    severity_bucket: props.bucket,
  });
};

export const trackForegroundBacklogObserved = (props: {
  lane: "foreground" | "workspace";
  bucket: "over_75ms" | "over_250ms" | "over_1000ms";
}): void => {
  captureIncident("foreground_backlog_observed", {
    lane: props.lane,
    backlog_bucket: props.bucket,
  });
};

export const trackForegroundGapRecoveryObserved = (props: {
  result: "started" | "recovered" | "timeout";
  bucket?: "under_250ms" | "250ms_to_1000ms" | "1000ms_plus";
}): void => {
  captureIncident("foreground_gap_recovery_observed", {
    result: props.result,
    ...(props.bucket ? { duration_bucket: props.bucket } : {}),
  });
};

export const trackRendererBacklogSample = (props: {
  lane: "foreground" | "workspace";
  source: string;
  ageMs: number;
}): void => {
  captureIncident(
    "renderer_backlog_sample",
    {
      lane: props.lane,
      source: props.source,
      backlog_age_ms: Math.max(0, Math.round(props.ageMs)),
    },
    { delivery: "local_only", source: props.source },
  );
};

export const trackRendererBacklogSpike = (props: {
  lane: "foreground" | "workspace";
  source: string;
  ageMs: number;
  thresholdMs: number;
}): void => {
  captureIncident(
    "renderer_backlog_spike",
    {
      lane: props.lane,
      source: props.source,
      backlog_age_ms: Math.max(0, Math.round(props.ageMs)),
      threshold_ms: Math.max(0, Math.round(props.thresholdMs)),
    },
    { source: props.source },
  );
};

export const trackFreshnessRecovered = (props: {
  lane: "foreground" | "workspace";
  source: string;
  degradedForMs: number;
}): void => {
  captureIncident(
    "freshness_recovered",
    {
      lane: props.lane,
      source: props.source,
      degraded_for_ms: Math.max(0, Math.round(props.degradedForMs)),
    },
    { source: props.source },
  );
};

export const trackWorkerPatchFlush = (props: {
  source: string;
  eventCount: number;
  activeSessionCount: number;
  publishSnapshot: boolean;
  persist: boolean;
  patchBytesEstimate: number;
  oldestEventAgeMs?: number | null;
  oldestForegroundEventAgeMs?: number | null;
}): void => {
  captureIncident(
    "worker_patch_flush",
    {
      source: props.source,
      event_count: Math.max(0, Math.round(props.eventCount)),
      active_session_count: Math.max(0, Math.round(props.activeSessionCount)),
      publish_snapshot: props.publishSnapshot,
      persist: props.persist,
      patch_bytes_estimate: Math.max(0, Math.round(props.patchBytesEstimate)),
      ...(typeof props.oldestEventAgeMs === "number"
        ? { oldest_event_age_ms: Math.max(0, Math.round(props.oldestEventAgeMs)) }
        : {}),
      ...(typeof props.oldestForegroundEventAgeMs === "number"
        ? { oldest_foreground_event_age_ms: Math.max(0, Math.round(props.oldestForegroundEventAgeMs)) }
        : {}),
    },
    { delivery: "local_only", source: props.source },
  );
};

export const trackWorkerPatchApply = (props: {
  source: string;
  eventCount: number;
  applyDurationMs: number;
  publishSnapshot: boolean;
  persist: boolean;
  oldestEventAgeStartMs?: number | null;
  oldestForegroundEventAgeStartMs?: number | null;
}): void => {
  captureIncident(
    "worker_patch_apply",
    {
      source: props.source,
      event_count: Math.max(0, Math.round(props.eventCount)),
      apply_duration_ms: Math.max(0, Math.round(props.applyDurationMs)),
      publish_snapshot: props.publishSnapshot,
      persist: props.persist,
      ...(typeof props.oldestEventAgeStartMs === "number"
        ? { oldest_event_age_start_ms: Math.max(0, Math.round(props.oldestEventAgeStartMs)) }
        : {}),
      ...(typeof props.oldestForegroundEventAgeStartMs === "number"
        ? { oldest_foreground_event_age_start_ms: Math.max(0, Math.round(props.oldestForegroundEventAgeStartMs)) }
        : {}),
    },
    {
      delivery: props.applyDurationMs >= 250 ? "remote" : "local_only",
      source: props.source,
    },
  );
};

export const trackUnknownEventBurst = (props: {
  source: string;
  sessionId: string;
  taskId?: string | null;
  workspaceId?: string | null;
  originalType: string;
  count: number;
  windowMs: number;
}): void => {
  captureIncident(
    "unknown_event_burst",
    {
      source: props.source,
      has_session_scope: Boolean(props.sessionId),
      has_task_scope: Boolean(props.taskId),
      has_workspace_scope: Boolean(props.workspaceId),
      original_type_class: unknownEventTypeClass(props.originalType),
      count: Math.max(0, Math.round(props.count)),
      window_ms: Math.max(0, Math.round(props.windowMs)),
    },
    { source: props.source },
  );
};

export const trackSessionEventVolumeBurst = (props: {
  source: string;
  sessionId: string;
  taskId?: string | null;
  workspaceId?: string | null;
  count: number;
  windowMs: number;
}): void => {
  captureIncident(
    "session_event_volume_burst",
    {
      source: props.source,
      has_session_scope: Boolean(props.sessionId),
      has_task_scope: Boolean(props.taskId),
      has_workspace_scope: Boolean(props.workspaceId),
      count: Math.max(0, Math.round(props.count)),
      window_ms: Math.max(0, Math.round(props.windowMs)),
    },
    { source: props.source },
  );
};

export const trackRendererHeartbeatMissed = (props: {
  source: string;
  missedForMs: number;
  outstandingAcks: number;
}): void => {
  captureIncident(
    "renderer_heartbeat_missed",
    {
      source: props.source,
      missed_for_ms: Math.max(0, Math.round(props.missedForMs)),
      outstanding_acks: Math.max(0, Math.round(props.outstandingAcks)),
    },
    { source: props.source },
  );
};
