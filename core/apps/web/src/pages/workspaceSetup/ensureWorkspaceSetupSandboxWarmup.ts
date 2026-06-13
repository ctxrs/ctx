import type { MutableRefObject } from "react";

import { startRuntimePrewarm } from "../../api/client";
import {
  serializeWorkspaceSetupRouteScope,
  type WorkspaceSetupRouteScope,
} from "./workflowTypes";

type EnsureWorkspaceSetupSandboxWarmupArgs = {
  desktopApp: boolean;
  location: "local" | "remote";
  routeScope: WorkspaceSetupRouteScope;
  sandboxWarmupTargetKeyRef: MutableRefObject<string | null>;
  connectDaemonForImport: (locationOverride?: "local" | "remote") => Promise<void>;
};

export async function ensureWorkspaceSetupSandboxWarmup({
  desktopApp,
  location,
  routeScope,
  sandboxWarmupTargetKeyRef,
  connectDaemonForImport,
}: EnsureWorkspaceSetupSandboxWarmupArgs): Promise<void> {
  if (!desktopApp || location !== "local" || routeScope.containerSelection !== "sandbox") {
    if (location !== "local" || routeScope.containerSelection !== "sandbox") {
      sandboxWarmupTargetKeyRef.current = null;
    }
    return;
  }
  const routeKey = serializeWorkspaceSetupRouteScope(routeScope);
  if (sandboxWarmupTargetKeyRef.current === routeKey) {
    return;
  }
  sandboxWarmupTargetKeyRef.current = routeKey;
  try {
    await connectDaemonForImport("local");
    await startRuntimePrewarm("launch_ready");
  } catch (error) {
    if (sandboxWarmupTargetKeyRef.current === routeKey) {
      sandboxWarmupTargetKeyRef.current = null;
    }
    // Route planning should not be blocked by speculative sandbox warmup.
    // Actual workspace launch remains the authoritative readiness gate and
    // surfaces actionable errors if the runtime cannot start.
    console.warn("workspace setup sandbox warmup failed", error);
  }
}
