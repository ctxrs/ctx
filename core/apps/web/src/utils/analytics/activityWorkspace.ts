import { capture, PENDING_WORKSPACE_LAUNCH_KEY_PREFIX } from "./activityShared";

export const trackAppOpened = (props?: { downloadId?: string }): void => {
  capture("app_opened", {
    ...(props?.downloadId ? { download_id: props.downloadId } : {}),
  });
};

export const trackWorkspaceCreated = (workspaceKind: "local" | "remote"): void => {
  capture("workspace_created", { workspace_kind: workspaceKind });
};

export const trackWorkspaceCreateSubmitted = (props: {
  workspaceKind: "local" | "remote";
  source: "wizard" | "launcher" | "api" | "unknown";
}): void => {
  capture("workspace_create_submitted", {
    workspace_kind: props.workspaceKind,
    source: props.source,
  });
};

export const trackWorkspaceCreateSucceeded = (props: {
  workspaceKind: "local" | "remote";
  source: "wizard" | "launcher" | "api" | "unknown";
}): void => {
  capture("workspace_create_succeeded", {
    workspace_kind: props.workspaceKind,
    source: props.source,
  });
};

export const trackWorkspaceCreateFailed = (props: {
  workspaceKind: "local" | "remote";
  source: "wizard" | "launcher" | "api" | "unknown";
  failureKind: "network_error" | "request_error" | "unknown";
}): void => {
  capture("workspace_create_failed", {
    workspace_kind: props.workspaceKind,
    source: props.source,
    failure_kind: props.failureKind,
  });
};

export const trackWorkspaceOpened = (workspaceKind: "local" | "remote"): void => {
  capture("workspace_opened", { workspace_kind: workspaceKind });
};

type PendingWorkspaceLaunch = {
  workspace_id: string;
  workspace_kind: "local" | "remote";
  execution_mode: "host" | "sandbox";
  source: "wizard" | "launcher" | "api" | "unknown";
  started_at_ms: number;
};

const pendingWorkspaceLaunchKey = (workspaceId: string): string =>
  `${PENDING_WORKSPACE_LAUNCH_KEY_PREFIX}${workspaceId}`;

const readPendingWorkspaceLaunch = (workspaceId: string): PendingWorkspaceLaunch | null => {
  if (typeof window === "undefined") return null;
  try {
    const raw = window.sessionStorage.getItem(pendingWorkspaceLaunchKey(workspaceId));
    if (!raw) return null;
    return JSON.parse(raw) as PendingWorkspaceLaunch;
  } catch {
    return null;
  }
};

const writePendingWorkspaceLaunch = (pending: PendingWorkspaceLaunch): void => {
  if (typeof window === "undefined") return;
  try {
    window.sessionStorage.setItem(
      pendingWorkspaceLaunchKey(pending.workspace_id),
      JSON.stringify(pending),
    );
  } catch {
    // ignore
  }
};

const clearPendingWorkspaceLaunch = (workspaceId: string): void => {
  if (typeof window === "undefined") return;
  try {
    window.sessionStorage.removeItem(pendingWorkspaceLaunchKey(workspaceId));
  } catch {
    // ignore
  }
};

export const trackWorkspaceLaunchCompleted = (props: {
  workspaceId: string;
  workspaceKind: "local" | "remote";
  executionMode: "host" | "sandbox";
  source: "wizard" | "launcher" | "api" | "unknown";
  startedAtMs: number;
  result: "ready" | "error";
  failureKind?: "launch_error" | "unknown";
  emitEvent?: boolean;
  persistPendingRoute?: boolean;
}): void => {
  if (props.emitEvent ?? true) {
    const clickToLaunchReadyMs = Math.max(0, Date.now() - props.startedAtMs);
    capture("workspace_launch_completed", {
      workspace_kind: props.workspaceKind,
      execution_mode: props.executionMode,
      source: props.source,
      result: props.result,
      click_to_launch_ready_ms: clickToLaunchReadyMs,
    });
    if (props.result === "error") {
      capture("workspace_launch_failed", {
        workspace_kind: props.workspaceKind,
        execution_mode: props.executionMode,
        source: props.source,
        status: "failed",
        failure_kind: props.failureKind ?? "launch_error",
        click_to_launch_ready_ms: clickToLaunchReadyMs,
      });
    }
  }
  if (props.result === "ready") {
    if (props.persistPendingRoute ?? true) {
      writePendingWorkspaceLaunch({
        workspace_id: props.workspaceId,
        workspace_kind: props.workspaceKind,
        execution_mode: props.executionMode,
        source: props.source,
        started_at_ms: props.startedAtMs,
      });
    }
    return;
  }
  clearPendingWorkspaceLaunch(props.workspaceId);
};

export const trackWorkspaceRouteOpenedFromPending = (workspaceId: string): void => {
  const pending = readPendingWorkspaceLaunch(workspaceId);
  if (!pending) return;
  clearPendingWorkspaceLaunch(workspaceId);
  capture("workspace_route_opened", {
    workspace_kind: pending.workspace_kind,
    execution_mode: pending.execution_mode,
    source: pending.source,
    click_to_workspace_route_ms: Math.max(0, Date.now() - pending.started_at_ms),
  });
};

export const trackWizardStarted = (props: {
  wizardKey: "workspace_setup";
}): void => {
  capture("wizard_started", {
    wizard_key: props.wizardKey,
  });
};

export const trackWizardStepViewed = (props: {
  wizardKey: "workspace_setup";
  stepKey: string;
  stepIndex: number;
}): void => {
  capture("wizard_step_viewed", {
    wizard_key: props.wizardKey,
    step_key: props.stepKey,
    step_index: props.stepIndex,
  });
};

export const trackWizardStepCompleted = (props: {
  wizardKey: "workspace_setup";
  stepKey: string;
  stepIndex: number;
}): void => {
  capture("wizard_step_completed", {
    wizard_key: props.wizardKey,
    step_key: props.stepKey,
    step_index: props.stepIndex,
  });
};

export const trackWizardCompleted = (props: {
  wizardKey: "workspace_setup";
  workspaceKind: "local" | "remote" | "unknown";
}): void => {
  capture("wizard_completed", {
    wizard_key: props.wizardKey,
    workspace_kind: props.workspaceKind,
  });
};

export const trackWizardAbandoned = (props: {
  wizardKey: "workspace_setup";
  lastStepKey: string;
  lastStepIndex: number;
}): void => {
  capture("wizard_abandoned", {
    wizard_key: props.wizardKey,
    last_step_key: props.lastStepKey,
    last_step_index: props.lastStepIndex,
  });
};
