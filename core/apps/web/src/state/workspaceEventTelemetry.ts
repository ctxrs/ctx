import type { WorkspaceActiveSnapshotEvent } from "@ctx/types";
import type { WorkspaceActiveSnapshotStreamSource } from "./workspaceActiveSnapshotProtocol";

const receivedAtByEvent = new WeakMap<object, number>();
const streamSourceByEvent = new WeakMap<object, WorkspaceActiveSnapshotStreamSource>();

export const markWorkspaceEventReceivedAt = (
  event: WorkspaceActiveSnapshotEvent,
  receivedAtMs: number,
): void => {
  if (!Number.isFinite(receivedAtMs)) return;
  receivedAtByEvent.set(event, receivedAtMs);
};

export const markWorkspaceEventStreamSource = (
  event: WorkspaceActiveSnapshotEvent,
  streamSource: WorkspaceActiveSnapshotStreamSource,
): void => {
  streamSourceByEvent.set(event, streamSource);
};

export const readWorkspaceEventReceivedAt = (
  event: WorkspaceActiveSnapshotEvent,
): number | null => {
  const value = receivedAtByEvent.get(event);
  return typeof value === "number" && Number.isFinite(value) ? value : null;
};

export const readWorkspaceEventStreamSource = (
  event: WorkspaceActiveSnapshotEvent,
): WorkspaceActiveSnapshotStreamSource | null => streamSourceByEvent.get(event) ?? null;
