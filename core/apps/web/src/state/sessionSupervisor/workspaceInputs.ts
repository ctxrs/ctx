import type { SessionHeadSnapshot, WorkspaceActiveSnapshotEvent } from "../../api/client";
import type { SessionSubscriptionCursor } from "../sessionSubscription";
import type { WorkspaceActiveSnapshotState } from "../workspaceActiveSnapshotStore";

export type SessionSupervisorSubscribedSessionIdsSink = ((sessions: SessionSubscriptionCursor[]) => void) | null;

export type SessionSupervisorWorkspaceSnapshotState = WorkspaceActiveSnapshotState | null;

export type SessionSupervisorWorkspaceSessionHeads = Record<string, SessionHeadSnapshot>;

export type SessionSupervisorWorkspaceEvent = WorkspaceActiveSnapshotEvent;
