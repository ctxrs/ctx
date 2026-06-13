import type {
  Artifact,
  GitStatusSummary,
  Message,
  Session,
  SessionEvent,
  SessionHeadWindow,
  SessionSummaryCheckpoint,
  SessionTurn,
  SessionTurnTool,
  SessionTurnToolSummary,
  SubagentInvocation,
} from "../../api/client";
import type { SessionActivityState } from "@ctx/types";
import type { SessionThreadProjection } from "../sessionThreadProjection/types";
import type { AssistantStreamingState } from "../assistantStreaming";

export type ConnectionStatus = "connecting" | "connected" | "disconnected" | "idle";

export type SessionMode = "active" | "archived";

export type SessionLoadState = "pending_hydration" | "live" | "recovering" | "fatal";

export type SessionFreshnessState = "bootstrap" | "authoritative" | "replica" | "recovering";

export type SessionRecoverySubscriptionPolicy = "reset" | "preserve";

export type SessionSupportLoadErrorKey = "state" | "subagentInvocations";

export type SessionSupportLoadErrors = Partial<Record<SessionSupportLoadErrorKey, string>>;

export type SessionSupervisorSnapshot = {
  connection: ConnectionStatus;
  sessions: Record<string, SessionCacheEntry>;
};

export type SessionOverlayState = {
  optimisticThreadMessages: Message[];
  optimisticQueuedMessages: Message[];
  optimisticQueueRemovalIds: string[];
  overlayRev: number;
};

export type SessionSupportState = {
  turnToolsByTurnId: Record<string, SessionTurnTool[]>;
  turnToolsLoadingSet: Set<string>;
  turnToolsHydratedByTurnId: Record<string, boolean>;
  toolSummariesReady: boolean;
  artifacts: Artifact[];
  subagentInvocations: SubagentInvocation[];
  subagentInvocationsLoaded: boolean;
  subagentInvocationsLoading: boolean;
  subagentInvocationsFetchToken: number;
  subagentInvocationsFetchedAtMs?: number;
  subagentInvocationsAppliedRev?: number;
  stateLoaded: boolean;
  stateLoading: boolean;
  stateAppliedRev?: number;
  stateFetchToken: number;
  loadErrors: SessionSupportLoadErrors;
  supportFreshnessEpoch: number;
  stateAutoLoadKey?: string;
  subagentAutoLoadKey?: string;
  diff?: string;
  gitStatusSummary?: GitStatusSummary | null;
  fetching: {
    head: boolean;
    history: boolean;
  };
};

export type SessionCacheEntry = {
  sessionId: string;
  mode?: SessionMode;
  loadState: SessionLoadState;
  freshness: SessionFreshnessState;
  session?: Session;
  activity?: SessionActivityState | null;
  acpModels?: unknown;
  acpModes?: unknown;
  acpCurrentModelId?: string;
  acpCommands?: unknown;
  acpSlashCommands?: unknown;
  turns: SessionTurn[];
  turnToolsByTurnId: Record<string, SessionTurnTool[]>;
  turnToolsLoading: string[];
  toolSummaries: SessionTurnToolSummary[];
  toolSummariesReady: boolean;
  hasMoreTurns: boolean;
  events: SessionEvent[];
  messages: Message[];
  messagesRev?: number;
  turnsRev?: number;
  eventsRev?: number;
  artifacts: Artifact[];
  artifactsLoading: boolean;
  subagentInvocations: SubagentInvocation[];
  subagentInvocationsLoaded?: boolean;
  subagentInvocationsLoading: boolean;
  stateLoaded: boolean;
  stateLoading: boolean;
  stateRev?: number;
  assistantStreamingByTurnId?: Record<string, AssistantStreamingState>;
  assistantStreamingRev?: number;
  loadErrors?: SessionSupportLoadErrors;
  queue: Message[];
  optimisticThreadMessages?: Message[];
  optimisticQueuedMessages?: Message[];
  optimisticQueueRemovalIds?: string[];
  overlayRev?: number;
  diff?: string;
  gitStatusSummary?: GitStatusSummary | null;
  summaryCheckpoint?: SessionSummaryCheckpoint | null;
  headWindow?: SessionHeadWindow | null;
  projectionRev?: number;
  threadProjection?: SessionThreadProjection;
  lastEventSeq?: number;
  loading: boolean;
  error?: string;
  subscribed: boolean;
  oldestTurnSeq?: number;
  fetching?: {
    head: boolean;
    history: boolean;
  };
  updatedAtMs: number;
};

type ThoughtCacheEntry = {
  key: string;
  event: SessionEvent;
  updatedAtMs?: number;
};

export type OpenOptions = {
  watchDiff?: boolean;
  force?: boolean;
  silent?: boolean;
  mode?: SessionMode;
};

type InternalEntryBase = Omit<
  SessionCacheEntry,
  | "turnToolsByTurnId"
  | "turnToolsLoading"
  | "toolSummariesReady"
  | "artifacts"
  | "artifactsLoading"
  | "subagentInvocations"
  | "subagentInvocationsLoaded"
  | "subagentInvocationsLoading"
  | "stateLoaded"
  | "stateLoading"
  | "loadErrors"
  | "optimisticThreadMessages"
  | "optimisticQueuedMessages"
  | "optimisticQueueRemovalIds"
  | "overlayRev"
  | "diff"
  | "gitStatusSummary"
  | "threadProjection"
  | "fetching"
>;

export type InternalEntry = InternalEntryBase & {
  refCount: number;
  recoverySubscriptionPolicy?: SessionRecoverySubscriptionPolicy;
  warmUntilMs: number;
  historyExtended: boolean;
  acpMetaUpdatedAtMs?: number;
  seqSet: Set<number>;
  nextTransientSeq: number;
  startedTurnIds: Set<string>;
  turnsHydrated: boolean;
  oldestTurnSeq?: number;
  toolStatusByKey: Map<string, string>;
  toolIdsByTurn: Map<string, Set<string>>;
  turnsRev: number;
  messagesRev: number;
  eventsRev: number;
  assistantStreamingByTurnId: Record<string, AssistantStreamingState>;
  assistantStreamingRev: number;
  stateRev?: number;
  headFromCache: boolean;
  thoughtCacheByKey: Record<string, ThoughtCacheEntry>;
  thoughtCacheLoaded: boolean;
  thoughtCacheLoading: boolean;
  thoughtCacheDirty: boolean;
  thoughtCacheOwnerTaskKey?: string;
  thoughtCacheLoadToken: number;
  overlay: SessionOverlayState;
  support: SessionSupportState;
};

export function createInternalEntry(
  sessionId: string,
  opts: { transientSeqStart: number; warmTtlMs: number },
): InternalEntry {
  return {
    sessionId,
    mode: undefined,
    loadState: "pending_hydration",
    freshness: "bootstrap",
    session: undefined,
    activity: null,
    acpModels: undefined,
    acpModes: undefined,
    acpCurrentModelId: undefined,
    acpCommands: undefined,
    acpSlashCommands: undefined,
    turns: [],
    toolSummaries: [],
    hasMoreTurns: true,
    events: [],
    eventsRev: 0,
    messages: [],
    messagesRev: 0,
    turnsRev: 0,
    assistantStreamingByTurnId: {},
    assistantStreamingRev: 0,
    stateRev: undefined,
    queue: [],
    summaryCheckpoint: null,
    headWindow: null,
    projectionRev: undefined,
    lastEventSeq: undefined,
    loading: false,
    error: undefined,
    subscribed: false,
    recoverySubscriptionPolicy: undefined,
    updatedAtMs: Date.now(),
    refCount: 0,
    warmUntilMs: Date.now() + opts.warmTtlMs,
    acpMetaUpdatedAtMs: undefined,
    seqSet: new Set<number>(),
    nextTransientSeq: opts.transientSeqStart,
    startedTurnIds: new Set<string>(),
    turnsHydrated: false,
    oldestTurnSeq: undefined,
    toolStatusByKey: new Map(),
    toolIdsByTurn: new Map(),
    headFromCache: false,
    historyExtended: false,
    thoughtCacheByKey: {},
    thoughtCacheLoaded: false,
    thoughtCacheLoading: false,
    thoughtCacheDirty: false,
    thoughtCacheOwnerTaskKey: undefined,
    thoughtCacheLoadToken: 0,
    overlay: {
      optimisticThreadMessages: [],
      optimisticQueuedMessages: [],
      optimisticQueueRemovalIds: [],
      overlayRev: 0,
    },
    support: {
      turnToolsByTurnId: {},
      turnToolsLoadingSet: new Set(),
      turnToolsHydratedByTurnId: {},
      toolSummariesReady: false,
      artifacts: [],
      subagentInvocations: [],
      subagentInvocationsLoaded: false,
      subagentInvocationsLoading: false,
      subagentInvocationsFetchToken: 0,
      subagentInvocationsFetchedAtMs: undefined,
      subagentInvocationsAppliedRev: undefined,
      stateLoaded: false,
      stateLoading: false,
      stateAppliedRev: undefined,
      stateFetchToken: 0,
      loadErrors: {},
      supportFreshnessEpoch: 0,
      stateAutoLoadKey: undefined,
      subagentAutoLoadKey: undefined,
      diff: undefined,
      gitStatusSummary: null,
      fetching: {
        head: false,
        history: false,
      },
    },
  };
}
