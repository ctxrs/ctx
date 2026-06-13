import type {
  SessionHeadSnapshot,
  Task,
  WorkspaceActiveSnapshotEvent,
} from "@ctx/types";
import type { SessionSubscriptionCursor } from "./sessionSubscription";
import type { PersistedWorkspaceActiveSnapshotV1 } from "./uiStateStore";
import type {
  WorkspaceActiveSnapshotItem,
  WorkspaceActiveSnapshotState,
} from "./workspaceActiveSnapshot/storeTypes";

export type WorkspaceActiveSnapshotCommand =
  | {
      type: "init";
      workspaceId: string;
      connectionSeq: number;
      authToken?: string | null;
      baseUrl?: string | null;
      wsBaseUrl?: string | null;
      runId?: string | null;
      installId?: string | null;
      originRuntime?: "web" | "desktop" | "mobile_shell";
      e2eEnabled?: boolean;
    }
  | {
      type: "update_auth";
      connectionSeq: number;
      authToken?: string | null;
      baseUrl?: string | null;
      wsBaseUrl?: string | null;
      runId?: string | null;
    }
  | {
      type: "seed_cache";
      snapshot: PersistedWorkspaceActiveSnapshotV1;
    }
  | { type: "set_subscribed_sessions"; sessions: SessionSubscriptionCursor[] }
  | { type: "set_foreground_session_id"; sessionId: string | null }
  | { type: "ensure_archived_loaded" }
  | { type: "load_more_archived" }
  | { type: "apply_task_update"; task: Task }
  | { type: "e2e_set_enabled"; enabled: boolean }
  | { type: "e2e_close_stream" }
  | { type: "e2e_set_drop_messages"; drop: boolean }
  | { type: "e2e_inject_stream_message"; data: unknown }
  | { type: "heartbeat_ack"; token: string };

export type WorkspaceActiveSnapshotPatch = {
  snapshot?: WorkspaceActiveSnapshotState;
  shell?: Partial<
    Pick<
      WorkspaceActiveSnapshotState,
      | "initialized"
      | "liveSnapshotApplied"
      | "connection"
      | "activeIds"
      | "archivedIds"
      | "totalActive"
      | "totalArchived"
      | "archivedRev"
      | "fetchState"
      | "hasMoreActive"
      | "hasMoreArchived"
      | "archivedLoaded"
    >
  >;
  taskUpserts?: Record<string, WorkspaceActiveSnapshotItem>;
  taskDeletes?: string[];
  sessionHeadUpserts?: Record<string, SessionHeadSnapshot>;
  sessionHeadDeletes?: string[];
  worktreeRootUpserts?: Record<string, string>;
  worktreeRootDeletes?: string[];
  events: WorkspaceActiveSnapshotEvent[];
  eventReceivedAtMs?: Array<number | null>;
  eventStreamSources?: Array<WorkspaceActiveSnapshotStreamSource | null>;
  snapshotRev: number;
  archivedRev: number;
  activeSessionIds: string[];
  publishSnapshot?: boolean;
  persist: boolean;
  oldestEventReceivedAtMs?: number | null;
  oldestForegroundEventReceivedAtMs?: number | null;
};

export type WorkspaceActiveSnapshotStreamSource = "live" | "replay";

export type WorkspaceActiveSnapshotStreamTelemetry = {
  lane: "foreground" | "workspace";
  eventType: string;
  sessionId: string | null;
  emittedAtMs: number | null;
  receivedAtMs: number;
  streamSource: WorkspaceActiveSnapshotStreamSource;
};

export type WorkspaceActiveSnapshotWorkerMessage =
  | {
      type: "patch";
      patch: WorkspaceActiveSnapshotPatch;
    }
  | {
      type: "stream_event_telemetry";
      telemetry: WorkspaceActiveSnapshotStreamTelemetry;
    }
  | {
      type: "heartbeat_ping";
      token: string;
      sentAtMs: number;
    }
  | {
      type: "heartbeat_missed";
      missedForMs: number;
      outstandingAcks: number;
    };
