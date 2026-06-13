import { useEffect, useRef } from "react";

import type { SessionCacheEntry } from "../../state/sessionSupervisor";
import { isReplicaAuthority } from "../../state/sessionSupervisor/config";
import {
  noteSessionSwitchAuthoritative,
  noteSessionSwitchStarted,
} from "../../state/foregroundFreshnessTelemetry";
import { getLoadTestTelemetry } from "../../utils/loadTestTelemetry";

export const resolveMeasuredSessionSwitchId = (
  activeSessionId: string | null,
  isOptimisticSessionId: boolean,
): string | null => {
  const normalized = typeof activeSessionId === "string" ? activeSessionId.trim() : "";
  if (!normalized || isOptimisticSessionId) {
    return null;
  }
  return normalized;
};

type UseWorkbenchSessionSwitchMetricsArgs = {
  activeEntry: SessionCacheEntry | null;
  activeSessionId: string | null;
  isOptimisticSessionId: boolean;
};

export function useWorkbenchSessionSwitchMetrics({
  activeEntry,
  activeSessionId,
  isOptimisticSessionId,
}: UseWorkbenchSessionSwitchMetricsArgs): void {
  const lastSessionSwitchRef = useRef<string | null>(null);
  const lastFreshnessSessionSwitchRef = useRef<string | null>(null);
  const loadTestTelemetry = getLoadTestTelemetry();
  const measuredSessionSwitchId = resolveMeasuredSessionSwitchId(activeSessionId, isOptimisticSessionId);

  useEffect(() => {
    if (!loadTestTelemetry?.enabled) return;
    const nextId = measuredSessionSwitchId;
    if (lastSessionSwitchRef.current === nextId) return;
    loadTestTelemetry.startSessionSwitch(lastSessionSwitchRef.current, nextId);
    lastSessionSwitchRef.current = nextId;
  }, [loadTestTelemetry, measuredSessionSwitchId]);

  useEffect(() => {
    const nextId = measuredSessionSwitchId;
    if (lastFreshnessSessionSwitchRef.current === nextId) return;
    noteSessionSwitchStarted(lastFreshnessSessionSwitchRef.current, nextId);
    lastFreshnessSessionSwitchRef.current = nextId;
  }, [measuredSessionSwitchId]);

  useEffect(() => {
    if (!loadTestTelemetry?.enabled) return;
    if (!measuredSessionSwitchId || !activeEntry || activeEntry.loading) return;
    loadTestTelemetry.updateVisibleSessionSwitchState(measuredSessionSwitchId, {
      subscribedWhenActive: activeEntry.subscribed,
      authoritativeWhenActive: isReplicaAuthority(activeEntry.freshness),
      httpRehydrateSeen: Boolean(activeEntry.fetching?.head),
    });
    loadTestTelemetry.finishSessionSwitch(measuredSessionSwitchId);
  }, [
    activeEntry?.fetching?.head,
    activeEntry?.freshness,
    activeEntry?.loading,
    activeEntry?.subscribed,
    activeEntry?.updatedAtMs,
    loadTestTelemetry,
    measuredSessionSwitchId,
  ]);

  useEffect(() => {
    if (
      !measuredSessionSwitchId ||
      !activeEntry ||
      activeEntry.loading ||
      !isReplicaAuthority(activeEntry.freshness)
    ) {
      return;
    }
    noteSessionSwitchAuthoritative(measuredSessionSwitchId);
  }, [activeEntry?.freshness, activeEntry?.loading, activeEntry?.updatedAtMs, measuredSessionSwitchId]);
}
