import type {
  Message,
  Session,
  SessionActivityState,
  SessionEvent,
  SessionHeadSnapshot,
  SessionHeadWindow,
  SessionSummaryCheckpoint,
  SessionTurn,
  SessionTurnToolSummary,
  WorkspaceActiveSnapshotEvent,
} from "@ctx/types";
import type { GitStatusSummary } from "../api/client";
import type { AssistantStreamingState } from "./assistantStreaming";
import type { WorkspaceActiveSnapshotStreamSource } from "./workspaceActiveSnapshotProtocol";

export type SessionReplicaConfig = {
  eventBufferLimit: number;
  headLimit: number;
  recoveryHeadLimit?: number;
  recoveryHeadIncludeEvents?: boolean;
};

export type SessionReplicaStreamLane = "foreground" | "workspace";

export type SessionReplicaFreshnessState = "bootstrap" | "authoritative" | "recovering";

export type SessionReplicaHeadSeedMode = "bootstrap_seed" | "repair_replace";

export type SessionReplicaReplaceMode =
  | SessionReplicaHeadSeedMode
  | "authoritative_replace";

export type SessionReplicaAppendMode =
  | "stream_delta"
  | "head_refresh"
  | "metadata_update";

export type SessionReplicaCanonicalAppendMode = Exclude<SessionReplicaAppendMode, "stream_delta">;

export const isAuthoritativeSessionReplicaReplace = (
  mode: SessionReplicaReplaceMode | null | undefined,
): boolean => mode === "authoritative_replace" || mode === "repair_replace";

export type SessionReplicaCommand =
  | {
      type: "init";
      config: SessionReplicaConfig;
      baseUrl?: string | null;
      authToken?: string | null;
      runId?: string | null;
    }
  | {
      type: "update_auth";
      baseUrl?: string | null;
      authToken?: string | null;
      runId?: string | null;
    }
  | {
      type: "open_session";
      sessionId: string;
      force?: boolean;
      silent?: boolean;
      skipCache?: boolean;
      skipBoundedBootstrapCache?: boolean;
      hydrateIfNeeded?: boolean;
      forceHydrate?: boolean;
    }
  | { type: "close_session"; sessionId: string }
  | { type: "drop_session"; sessionId: string }
  | { type: "refresh_session"; sessionId: string }
  | { type: "hydrate_session_head"; sessionId: string; force?: boolean; silent?: boolean }
  | { type: "seed_head"; sessionId: string; head: SessionHeadSnapshot; mode: SessionReplicaHeadSeedMode }
  | {
      type: "workspace_event";
      event: WorkspaceActiveSnapshotEvent;
      lane?: SessionReplicaStreamLane;
      receivedAtMs?: number | null;
      streamSource?: WorkspaceActiveSnapshotStreamSource | null;
    }
  | { type: "set_session"; session: Session };

export type SessionReplicaData = {
  session?: Session;
  activity?: SessionActivityState | null;
  freshness?: SessionReplicaFreshnessState;
  acpMeta?: {
    models?: unknown;
    modes?: unknown;
    currentModelId?: string;
    commands?: unknown;
    slashCommands?: unknown;
  };
  turns?: SessionTurn[];
  turnsRev?: number;
  assistantStreamingByTurnId?: Record<string, AssistantStreamingState>;
  assistantStreamingRev?: number;
  messages?: Message[];
  removedMessageIds?: string[];
  messagesRev?: number;
  events?: SessionEvent[];
  eventsRev?: number;
  toolSummaries?: SessionTurnToolSummary[];
  headWindow?: SessionHeadWindow | null;
  summaryCheckpoint?: SessionSummaryCheckpoint | null;
  lastEventSeq?: number;
  projectionRev?: number;
  hasMoreTurns?: boolean;
  stateRev?: number;
  gitStatusSummary?: GitStatusSummary | null;
  loading?: boolean;
  error?: string | null;
  turnsHydrated?: boolean;
  stateLoaded?: boolean;
  stateLoading?: boolean;
  subagentNotice?: boolean;
  replaceMode?: SessionReplicaReplaceMode;
  appendMode?: SessionReplicaAppendMode;
};

export type SessionReplicaPatch =
  | { op: "append"; sessionId: string; data: SessionReplicaData & { appendMode: SessionReplicaAppendMode } }
  | { op: "replace"; sessionId: string; data: SessionReplicaData }
  | { op: "evict"; sessionId: string; data: { eventsBeforeSeq?: number } };

export type SessionReplicaFreshnessEvent =
  | {
      type: "final_delta_received";
      sessionId: string;
      turnId: string | null;
      emittedAtMs: number | null;
      lastEventSeq: number | null;
    }
  | {
      type: "replica_delta_applied";
      sessionId: string;
      emittedAtMs: number | null;
      receivedAtMs: number | null;
      streamSource?: WorkspaceActiveSnapshotStreamSource | null;
      lastEventSeq: number | null;
      eventType: string;
    }
  | {
      type: "gap_recovery_started";
      sessionId: string;
      reason: string | null;
      lane?: SessionReplicaStreamLane;
    }
  | { type: "gap_recovery_finished"; sessionId: string }
  | {
      type: "gap_repair_mismatch";
      sessionId: string;
      baselineLastEventSeq: number | null;
      repairedLastEventSeq: number | null;
    }
  | {
      type: "projection_or_seq_regression";
      sessionId: string;
      dimension: "last_event_seq" | "projection_rev";
      incoming: number;
      existing: number;
    }
  | {
      type: "stale_head_delta_dropped";
      sessionId: string;
      dimension: "last_event_seq" | "projection_rev";
      incoming: number;
      existing: number;
    };

export type SessionReplicaWorkerMessage =
  | {
      type: "patches";
      patches: SessionReplicaPatch[];
    }
  | {
      type: "freshness_event";
      event: SessionReplicaFreshnessEvent;
    };
