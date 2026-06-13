import type { SessionHeadSnapshot } from "../../api/client";
import type { SessionReplicaHeadSeedMode } from "../sessionReplicaProtocol";
import { shouldRepairSessionHeadReplace } from "../sessionHeadRepair";
import { findWorkspaceSessionHead } from "../workspaceActiveSnapshot/projection";
import { isReplicaAuthority, shouldSkipBoundedActiveSnapshotSeed } from "./config";
import type { InternalEntry } from "./entryState";
import type { SessionSupervisorWorkspaceSnapshotState } from "./workspaceInputs";

type ActiveSnapshotSeedHost = {
  workspaceSnapshotState: SessionSupervisorWorkspaceSnapshotState;
  workspaceSessionHeadsById: Map<string, SessionHeadSnapshot>;
  dispatchSeedHead(cmd: {
    type: "seed_head";
    sessionId: string;
    head: SessionHeadSnapshot;
    mode: SessionReplicaHeadSeedMode;
  }): void;
};

export function canSeedReplicaFromActiveSnapshot(
  entry: InternalEntry,
  opts?: { allowRecoveringRefresh?: boolean },
): boolean {
  if (opts?.allowRecoveringRefresh && entry.freshness === "recovering") {
    return true;
  }
  return (
    !isReplicaAuthority(entry.freshness) &&
    !entry.turnsHydrated &&
    entry.messages.length === 0 &&
    entry.events.length === 0
  );
}

export function shouldRepairReplicaFromActiveSnapshot(
  entry: InternalEntry,
  head: SessionHeadSnapshot,
): boolean {
  return shouldRepairSessionHeadReplace(entry, head);
}

export function classifyActiveSnapshotSeedMode(
  entry: InternalEntry,
  head: SessionHeadSnapshot,
  opts?: { allowRecoveringRefresh?: boolean },
): SessionReplicaHeadSeedMode | null {
  if (shouldRepairReplicaFromActiveSnapshot(entry, head)) {
    return "repair_replace";
  }
  if (canSeedReplicaFromActiveSnapshot(entry, opts)) {
    if (!shouldSkipBoundedActiveSnapshotSeed(entry, head)) {
      return "bootstrap_seed";
    }
    return null;
  }
  return null;
}

export function seedReplicaFromActiveSnapshot(
  host: ActiveSnapshotSeedHost,
  sessionId: string,
  entry: InternalEntry,
  opts?: { allowRecoveringRefresh?: boolean; allowRepairReplace?: boolean },
): boolean {
  const head = findWorkspaceSessionHead(
    host.workspaceSnapshotState,
    host.workspaceSessionHeadsById,
    sessionId,
  );
  if (!head) return false;
  const recoveringBootstrap =
    (entry.freshness === "recovering" || entry.loadState === "recovering") &&
    !shouldSkipBoundedActiveSnapshotSeed(entry, head);
  if (recoveringBootstrap) {
    host.dispatchSeedHead({ type: "seed_head", sessionId, head, mode: "bootstrap_seed" });
    return true;
  }
  const mode = classifyActiveSnapshotSeedMode(entry, head, {
    allowRecoveringRefresh: opts?.allowRecoveringRefresh,
  });
  if (!mode) return false;
  if (mode === "repair_replace" && !opts?.allowRepairReplace) return false;
  host.dispatchSeedHead({ type: "seed_head", sessionId, head, mode });
  return true;
}
