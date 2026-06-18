import { beforeEach, describe, expect, it, vi } from "vitest";

const { captureIncidentEventMock, captureProductEventMock } = vi.hoisted(() => ({
  captureIncidentEventMock: vi.fn(),
  captureProductEventMock: vi.fn(),
}));

vi.mock("./client", () => ({
  captureIncidentEvent: captureIncidentEventMock,
  captureProductEvent: captureProductEventMock,
}));

import {
  normalizeTurnFailureKind,
  trackDesktopWebviewRecoveryObserved,
  trackProviderAuthCompleted,
  trackProviderAuthFailed,
  trackProviderAuthStarted,
  trackProviderInstallCompleted,
  trackProviderInstallFailed,
  trackProviderInstallStarted,
  trackTaskCreated,
  trackTurnCompleted,
  trackTurnStarted,
  trackUnknownEventBurst,
  trackUserMessageSent,
  trackWorkspaceCreated,
  trackWorkspaceCreateFailed,
  trackWorkspaceCreateSubmitted,
  trackWorkspaceCreateSucceeded,
  trackWorkspaceLaunchCompleted,
  trackWorkspaceOpened,
  trackWorkspaceRouteOpenedFromPending,
} from "./activity";

describe("usage analytics activity helpers", () => {
  beforeEach(() => {
    captureIncidentEventMock.mockReset();
    captureProductEventMock.mockReset();
    window.localStorage.clear();
    window.sessionStorage.clear();
  });

  it("normalizes task, message, and turn events onto base model ids plus reasoning effort", () => {
    trackTaskCreated({
      providerId: "codex",
      modelId: "gpt-5.4/high",
      executionEnvironment: "sandbox",
    });
    trackUserMessageSent({
      providerId: "codex",
      modelId: "gpt-5.4/high",
      executionEnvironment: "sandbox",
      sessionKind: "primary",
      isFirstTurn: true,
    });
    trackTurnStarted({
      providerId: "codex",
      modelId: "gpt-5.4/high",
      executionEnvironment: "sandbox",
      sessionKind: "subagent",
    });

    expect(captureProductEventMock).toHaveBeenCalledWith(
      "task_created",
      1,
      {
        provider_id: "codex",
        model_id: "gpt-5.4",
        reasoning_effort: "high",
        execution_environment: "sandbox",
        session_kind: "primary",
      },
    );
    expect(captureProductEventMock).toHaveBeenCalledWith(
      "user_message_sent",
      1,
      {
        provider_id: "codex",
        model_id: "gpt-5.4",
        reasoning_effort: "high",
        execution_environment: "sandbox",
        session_kind: "primary",
        is_first_turn: true,
      },
    );
    expect(captureProductEventMock).toHaveBeenCalledWith(
      "turn_started",
      1,
      {
        provider_id: "codex",
        model_id: "gpt-5.4",
        reasoning_effort: "high",
        execution_environment: "sandbox",
        session_kind: "subagent",
      },
    );
  });

  it("flattens bounded token metrics onto turn_completed", () => {
    trackTurnCompleted({
      providerId: "codex",
      modelId: "gpt-5.4/high",
      executionEnvironment: "sandbox",
      sessionKind: "subagent",
      status: "completed",
      durationMs: 2_000,
      metrics: {
        context_tokens_estimate: 120,
        total_input_tokens: 80,
        total_output_tokens: 40,
        context_window_tokens: 200,
        remaining_tokens_estimate: 80,
        remaining_fraction: 0.4,
      },
    });

    expect(captureProductEventMock).toHaveBeenCalledWith(
      "turn_completed",
      1,
      {
        provider_id: "codex",
        model_id: "gpt-5.4",
        reasoning_effort: "high",
        execution_environment: "sandbox",
        status: "completed",
        duration_bucket: "under_15s",
        session_kind: "subagent",
        total_tokens_estimate: 120,
        input_tokens: 80,
        output_tokens: 40,
        context_window_tokens: 200,
        remaining_tokens_estimate: 80,
        remaining_fraction: 0.4,
      },
    );
  });

  it("captures bounded provider setup diagnostics", () => {
    trackProviderInstallStarted({
      providerId: "codex",
      source: "provider_onboarding",
      target: "container",
    });
    trackProviderInstallCompleted({
      providerId: "codex",
      target: "container",
    });
    trackProviderInstallFailed({
      providerId: "claude-crp",
      target: "host",
      installErrorCode: "download_failed",
    });
    trackProviderAuthStarted({
      providerId: "gemini",
      authMethod: "subscription_browser",
    });
    trackProviderAuthCompleted({
      providerId: "gemini",
      authMethod: "subscription_browser",
    });
    trackProviderAuthFailed({
      providerId: "cursor",
      authMethod: "endpoint",
      failureKind: "verification_failed",
    });

    expect(captureProductEventMock).toHaveBeenCalledWith(
      "provider_install_started",
      1,
      {
        provider_id: "codex",
        source: "provider_onboarding",
        target: "container",
      },
    );
    expect(captureProductEventMock).toHaveBeenCalledWith(
      "provider_install_completed",
      1,
      {
        provider_id: "codex",
        source: "provider_onboarding",
        status: "succeeded",
        target: "container",
      },
    );
    expect(captureProductEventMock).toHaveBeenCalledWith(
      "provider_install_failed",
      1,
      {
        provider_id: "claude-crp",
        source: "provider_onboarding",
        status: "failed",
        failure_kind: "download_failed",
        target: "host",
        install_error_kind: "download_failed",
      },
    );
    expect(captureProductEventMock).toHaveBeenCalledWith(
      "provider_auth_started",
      1,
      {
        provider_id: "gemini",
        source: "settings",
        auth_method: "subscription_browser",
      },
    );
    expect(captureProductEventMock).toHaveBeenCalledWith(
      "provider_auth_completed",
      1,
      {
        provider_id: "gemini",
        source: "settings",
        auth_method: "subscription_browser",
        status: "succeeded",
      },
    );
    expect(captureProductEventMock).toHaveBeenCalledWith(
      "provider_auth_failed",
      1,
      {
        provider_id: "cursor",
        source: "settings",
        auth_method: "endpoint",
        status: "failed",
        failure_kind: "verification_failed",
      },
    );
  });

  it("adds bounded failure kinds to failed turn diagnostics", () => {
    trackTurnCompleted({
      providerId: "codex",
      status: "failed",
      failureKind: "auth_failed",
    });

    expect(captureProductEventMock).toHaveBeenCalledWith(
      "turn_completed",
      1,
      {
        provider_id: "codex",
        status: "failed",
        duration_bucket: "unknown",
        failure_kind: "auth_failed",
      },
    );
  });

  it("captures bounded workspace launch failures separately from completion status", () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-06-12T12:00:10.000Z"));

    try {
      trackWorkspaceLaunchCompleted({
        workspaceId: "workspace-1",
        workspaceKind: "local",
        executionMode: "sandbox",
        source: "wizard",
        startedAtMs: Date.parse("2026-06-12T12:00:00.000Z"),
        result: "error",
      });
    } finally {
      vi.useRealTimers();
    }

    expect(captureProductEventMock).toHaveBeenCalledWith(
      "workspace_launch_completed",
      1,
      {
        workspace_kind: "local",
        execution_mode: "sandbox",
        source: "wizard",
        result: "error",
        click_to_launch_ready_ms: 10_000,
      },
    );
    expect(captureProductEventMock).toHaveBeenCalledWith(
      "workspace_launch_failed",
      1,
      {
        workspace_kind: "local",
        execution_mode: "sandbox",
        source: "wizard",
        status: "failed",
        failure_kind: "launch_error",
        click_to_launch_ready_ms: 10_000,
      },
    );
  });

  it("includes execution mode on workspace create and opened events when known", () => {
    trackWorkspaceCreateSubmitted({
      workspaceKind: "local",
      source: "wizard",
      executionMode: "sandbox",
    });
    trackWorkspaceCreateSucceeded({
      workspaceKind: "local",
      source: "wizard",
      executionMode: "sandbox",
    });
    trackWorkspaceCreated({
      workspaceKind: "local",
      executionMode: "sandbox",
    });
    trackWorkspaceCreateFailed({
      workspaceKind: "remote",
      source: "api",
      executionMode: "host",
      failureKind: "request_error",
    });
    trackWorkspaceOpened({
      workspaceKind: "local",
      executionMode: "sandbox",
    });

    expect(captureProductEventMock).toHaveBeenCalledWith(
      "workspace_create_submitted",
      1,
      {
        workspace_kind: "local",
        source: "wizard",
        execution_mode: "sandbox",
      },
    );
    expect(captureProductEventMock).toHaveBeenCalledWith(
      "workspace_create_succeeded",
      1,
      {
        workspace_kind: "local",
        source: "wizard",
        execution_mode: "sandbox",
      },
    );
    expect(captureProductEventMock).toHaveBeenCalledWith(
      "workspace_created",
      1,
      {
        workspace_kind: "local",
        execution_mode: "sandbox",
      },
    );
    expect(captureProductEventMock).toHaveBeenCalledWith(
      "workspace_create_failed",
      1,
      {
        workspace_kind: "remote",
        source: "api",
        execution_mode: "host",
        failure_kind: "request_error",
      },
    );
    expect(captureProductEventMock).toHaveBeenCalledWith(
      "workspace_opened",
      1,
      {
        workspace_kind: "local",
        execution_mode: "sandbox",
      },
    );
  });

  it("returns pending workspace launch context after recording route open", () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-06-12T12:00:05.000Z"));

    let pending: ReturnType<typeof trackWorkspaceRouteOpenedFromPending> = null;
    try {
      trackWorkspaceLaunchCompleted({
        workspaceId: "workspace-1",
        workspaceKind: "local",
        executionMode: "host",
        source: "wizard",
        startedAtMs: Date.parse("2026-06-12T12:00:00.000Z"),
        result: "ready",
        emitEvent: false,
      });

      pending = trackWorkspaceRouteOpenedFromPending("workspace-1");
    } finally {
      vi.useRealTimers();
    }

    expect(pending).toEqual({
      workspaceKind: "local",
      executionMode: "host",
      source: "wizard",
      clickToWorkspaceRouteMs: 5_000,
    });
    expect(captureProductEventMock).toHaveBeenCalledWith(
      "workspace_route_opened",
      1,
      {
        workspace_kind: "local",
        execution_mode: "host",
        source: "wizard",
        click_to_workspace_route_ms: 5_000,
      },
    );
  });

  it("keeps structured interrupted turn failures more specific than cancellation", () => {
    expect(normalizeTurnFailureKind("provider_auth_required", "interrupted")).toBe("auth_missing");
    expect(normalizeTurnFailureKind(undefined, "interrupted")).toBe("user_cancelled");
  });

  it("buckets unknown event original types before remote incident capture", () => {
    trackUnknownEventBurst({
      source: "session_replica_ingest",
      sessionId: "session-123",
      taskId: "task-123",
      workspaceId: "workspace-123",
      originalType: "vendor.unique.event.name.with.unbounded.detail",
      count: 25,
      windowMs: 5_000,
    });

    expect(captureIncidentEventMock).toHaveBeenCalledWith(
      "unknown_event_burst",
      1,
      {
        source: "session_replica_ingest",
        has_session_scope: true,
        has_task_scope: true,
        has_workspace_scope: true,
        original_type_class: "other",
        count: 25,
        window_ms: 5_000,
      },
      { source: "session_replica_ingest" },
    );
  });

  it("captures metadata-only desktop webview recovery events", () => {
    trackDesktopWebviewRecoveryObserved({
      trigger: "heartbeat_timeout",
      action: "recreate",
      surface: "workbench",
      daemonHealth: "ok",
      suppressionReason: "window_not_focused",
    });

    expect(captureProductEventMock).toHaveBeenCalledWith(
      "desktop_webview_recovery_observed",
      1,
      {
        trigger: "heartbeat_timeout",
        action: "recreate",
        surface: "workbench",
        daemon_health: "ok",
        suppression_reason: "window_not_focused",
      },
    );
  });
});
