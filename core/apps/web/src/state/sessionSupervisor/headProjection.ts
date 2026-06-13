import {
  idToString,
  type Message,
  type SessionEvent,
  type SessionHead,
  type SessionHeadSnapshot,
  type SessionState,
  type SessionTurn,
  type SessionTurnTool,
  type SessionTurnToolSummary,
} from "../../api/client";
import { saveSessionHeadV1 } from "../uiStateStore";
import { clearAllAssistantStreaming, clearAssistantStreaming } from "../assistantStreaming";
import { isBoundedSessionHead } from "../sessionHeadRepair";
import { findWorkspaceSessionHead } from "../workspaceActiveSnapshot/projection";
import {
  reconcileActivityFromTurns,
  reconcileLatestTurnInterruptedFromActivity,
  stripPartialEvents,
  stripTurnPartials,
} from "./cachePolicy";
import { asRecord, hasModelList } from "./eventHydration";
import {
  type InternalEntry,
  type SessionLoadState,
  type SessionMode,
  type SessionSupportLoadErrorKey,
} from "./entryState";
import { adoptLoadedStateRevision } from "./supportLoads";
import type { SessionSupervisorWorkspaceSnapshotState } from "./workspaceInputs";

type AcpMeta = {
  models?: unknown;
  modes?: unknown;
  currentModelId?: string;
  commands?: unknown;
  slashCommands?: unknown;
};

export type SessionSupervisorHeadProjectionHost = {
  workspaceSnapshotState: SessionSupervisorWorkspaceSnapshotState;
  workspaceSessionHeadsById: Map<string, SessionHeadSnapshot>;
  stateCacheBySessionId: Map<string, { state: SessionState; stateRev?: number }>;
  publish(): void;
  mergeTurns(entry: InternalEntry, turns: SessionTurn[]): void;
  mergeEvents(entry: InternalEntry, events: SessionEvent[]): void;
  mergeMessages(entry: InternalEntry, messages: Message[]): void;
  applyAcpMeta(
    entry: InternalEntry,
    meta: AcpMeta,
    opts?: { persist?: boolean; syncSharedProviderCatalog?: boolean },
  ): boolean;
  applyAcpMetaFromEvents(entry: InternalEntry, events: SessionEvent[]): boolean;
  applyGitStatusSnapshotFromEvents(entry: InternalEntry, events: SessionEvent[]): boolean;
  ensureProviderOptions(entry: InternalEntry): Promise<void>;
  resolveSessionMode(
    sessionId: string,
    entry?: InternalEntry,
    explicitMode?: SessionMode,
  ): SessionMode | null;
  setSessionLoadState(entry: InternalEntry, next: SessionLoadState): void;
  syncSupportLoadsForOpenSession(entry: InternalEntry): void;
  ensureThoughtCache(entry: InternalEntry): Promise<void>;
  adoptLoadedSubagentInvocationsRevision(entry: InternalEntry, stateRev: number): void;
  clearSupportLoadError(entry: InternalEntry, key: SessionSupportLoadErrorKey): void;
  bumpTurnsRev(entry: InternalEntry): void;
  bumpMessagesRev(entry: InternalEntry): void;
  bumpEventsRev(entry: InternalEntry): void;
};

function resolveActiveSnapshotSeedFreshness(
  this: SessionSupervisorHeadProjectionHost,
  entry: InternalEntry,
): InternalEntry["freshness"] {
  if (entry.freshness === "recovering") return "recovering";
  if (entry.freshness === "authoritative") return "authoritative";
  const snapshotState = this.workspaceSnapshotState;
  const liveConnectedSnapshot =
    snapshotState?.liveSnapshotApplied === true && snapshotState.connection === "connected";
  return liveConnectedSnapshot ? "authoritative" : "bootstrap";
}

function pruneOmittedNonTerminalTurns(
  this: SessionSupervisorHeadProjectionHost,
  entry: InternalEntry,
  incomingTurns: SessionTurn[],
) {
  if (incomingTurns.length === 0 || entry.turns.length === 0) return;
  const support = entry.support;
  const retainedTurnIds = new Set(
    incomingTurns
      .map((turn) => idToString(turn.turn_id))
      .filter((turnId): turnId is string => turnId.length > 0),
  );
  if (retainedTurnIds.size === 0) return;

  const removedTurnIds = new Set<string>();
  entry.turns = entry.turns.filter((turn) => {
    const turnId = idToString(turn.turn_id);
    if (!turnId || retainedTurnIds.has(turnId)) return true;
    if (turn.status !== "running" && turn.status !== "starting" && turn.status !== "queued") return true;
    removedTurnIds.add(turnId);
    return false;
  });

  if (removedTurnIds.size === 0) return;

  entry.messages = entry.messages.filter((message) => {
    const turnId = idToString(message.turn_id ?? "");
    return !turnId || !removedTurnIds.has(turnId);
  });
  entry.queue = entry.messages.filter((message) => message.delivery === "queued");
  entry.toolSummaries = entry.toolSummaries.filter((summary) => {
    const turnId = idToString(summary.turn_id);
    return !turnId || !removedTurnIds.has(turnId);
  });

  for (const turnId of removedTurnIds) {
    delete support.turnToolsByTurnId[turnId];
    delete support.turnToolsHydratedByTurnId[turnId];
    entry.startedTurnIds.delete(turnId);
    entry.toolIdsByTurn.delete(turnId);
    clearAssistantStreaming(entry, turnId);
  }

  for (const turnId of removedTurnIds) {
    support.turnToolsLoadingSet.delete(turnId);
  }
  entry.oldestTurnSeq = entry.turns[0]?.start_seq ?? entry.oldestTurnSeq;
  this.bumpTurnsRev(entry);
  this.bumpMessagesRev(entry);
  entry.updatedAtMs = Date.now();
}

export function seedHeadFromActiveSnapshot(
  this: SessionSupervisorHeadProjectionHost,
  entry: InternalEntry,
): boolean {
  const head = findWorkspaceSessionHead(
    this.workspaceSnapshotState,
    this.workspaceSessionHeadsById,
    entry.sessionId,
  );
  if (!head) return false;
  const nextSeq = typeof head.last_event_seq === "number" ? head.last_event_seq : -1;
  const prevSeq = typeof entry.lastEventSeq === "number" ? entry.lastEventSeq : -1;
  if (entry.turnsHydrated && prevSeq >= nextSeq) {
    if (!entry.session) {
      entry.session = head.session;
      return true;
    }
    return false;
  }
  const strippedEvents =
    (head.events?.length ?? 0) === 0 && (head.head_window?.event_limit ?? 0) === 0;
  applyHead.call(this, entry, head as SessionHead, {
    fromCache: strippedEvents,
    freshness: resolveActiveSnapshotSeedFreshness.call(this, entry),
  });
  return true;
}

export function applyHead(
  this: SessionSupervisorHeadProjectionHost,
  entry: InternalEntry,
  head: SessionHead,
  opts?: {
    fromCache?: boolean;
    freshness?: InternalEntry["freshness"];
  },
) {
  const support = entry.support;
  const previousTurns = entry.turns;
  entry.headFromCache = Boolean(opts?.fromCache);
  entry.session = head.session;
  if ("activity" in head) {
    entry.activity = head.activity ?? null;
  }
  if (opts?.freshness) {
    entry.freshness = opts.freshness;
  }
  clearAllAssistantStreaming(entry);
  if (!entry.mode) {
    const resolvedMode = this.resolveSessionMode(entry.sessionId, entry);
    if (resolvedMode) {
      entry.mode = resolvedMode;
    }
  }
  entry.summaryCheckpoint = head.summary_checkpoint ?? null;
  entry.headWindow = head.head_window ?? null;
  entry.turnsHydrated = true;
  entry.hasMoreTurns = head.has_more_turns;
  entry.lastEventSeq = head.last_event_seq;
  const headRecord = asRecord(head);
  const headStateRev = headRecord?.state_rev ?? headRecord?.stateRev;
  if (typeof headStateRev === "number") {
    entry.stateRev = headStateRev;
    support.stateAppliedRev = adoptLoadedStateRevision(
      support.stateLoaded,
      support.stateAppliedRev,
      headStateRev,
    );
    this.adoptLoadedSubagentInvocationsRevision(entry, headStateRev);
  }
  this.mergeTurns(entry, head.turns ?? []);
  if (reconcileLatestTurnInterruptedFromActivity(entry.turns, entry.activity)) {
    this.bumpTurnsRev(entry);
  }
  entry.activity = reconcileActivityFromTurns(entry.activity, entry.turns);
  // Historical snapshot hydration should not emit fresh analytics events.
  // Live analytics are produced via event-driven paths (replica append patches).
  // See: core/apps/web/src/state/sessionSupervisorCore.analytics.test.ts
  if (isBoundedSessionHead(head)) {
    pruneOmittedNonTerminalTurns.call(this, entry, head.turns ?? []);
  }
  this.mergeEvents(entry, head.events ?? []);
  this.mergeMessages(entry, head.messages ?? []);
  this.applyAcpMetaFromEvents(entry, head.events ?? []);
  this.applyGitStatusSnapshotFromEvents(entry, head.events ?? []);
  if (!entry.acpModels || !hasModelList(entry.acpModels)) {
    void this.ensureProviderOptions(entry);
  }
  const incomingToolSummaries = head.tool_summaries;
  if (Array.isArray(incomingToolSummaries) && incomingToolSummaries.length > 0) {
    entry.toolSummaries = incomingToolSummaries;
  } else if (entry.toolSummaries.length === 0) {
    entry.toolSummaries = incomingToolSummaries ?? [];
  }
  if (head.tool_summaries && head.tool_summaries.length > 0) {
    const hydrated = support.turnToolsHydratedByTurnId;
    const nextByTurn: Record<string, SessionTurnTool[]> = {};
    for (const summary of head.tool_summaries) {
      const turnId = idToString(summary.turn_id);
      if (!turnId) continue;
      if (hydrated[turnId]) continue;
      const list = nextByTurn[turnId] ?? [];
      list.push({
        session_id: summary.session_id,
        tool_call_id: summary.tool_call_id,
        turn_id: summary.turn_id,
        tool_kind: summary.tool_kind,
        provider_tool_name: summary.provider_tool_name ?? null,
        title: summary.title,
        subtitle: summary.subtitle ?? null,
        status: summary.status,
        input_json: summary.input_preview ?? null,
        output_text: null,
        order_seq: summary.order_seq,
        input_truncated: summary.input_truncated ?? null,
        input_original_bytes: summary.input_original_bytes ?? null,
        output_truncated: summary.output_truncated ?? null,
        output_original_bytes: summary.output_original_bytes ?? null,
        first_event_seq: summary.first_event_seq ?? null,
        created_at: summary.created_at,
        updated_at: summary.updated_at,
        summary_only: true,
      } as SessionTurnTool & { summary_only: boolean });
      nextByTurn[turnId] = list;
    }
    if (Object.keys(nextByTurn).length > 0) {
      support.turnToolsByTurnId = {
        ...support.turnToolsByTurnId,
        ...nextByTurn,
      };
      for (const turnId of Object.keys(nextByTurn)) {
        if (!hydrated[turnId]) hydrated[turnId] = false;
      }
    }
  }
  support.toolSummariesReady = true;
  entry.error = undefined;
  this.setSessionLoadState(entry, "live");
  this.syncSupportLoadsForOpenSession(entry);
  void this.ensureThoughtCache(entry);
  entry.updatedAtMs = Date.now();
  this.publish();
}

export function applyToolSummaries(
  this: SessionSupervisorHeadProjectionHost,
  entry: InternalEntry,
  summaries: SessionTurnToolSummary[],
) {
  const support = entry.support;
  const hydrated = support.turnToolsHydratedByTurnId;
  let changed = false;
  const nextByTurn: Record<string, SessionTurnTool[]> = {};
  if (entry.toolSummaries !== summaries) {
    entry.toolSummaries = summaries;
    changed = true;
  }

  for (const summary of summaries) {
    const turnId = idToString(summary.turn_id);
    if (!turnId) continue;
    if (hydrated[turnId]) continue;
    const list = nextByTurn[turnId] ?? [];
    list.push({
      session_id: summary.session_id,
      tool_call_id: summary.tool_call_id,
      turn_id: summary.turn_id,
      tool_kind: summary.tool_kind ?? null,
      provider_tool_name: summary.provider_tool_name ?? null,
      title: summary.title ?? null,
      subtitle: summary.subtitle ?? null,
      status: summary.status ?? null,
      input_json: summary.input_preview ?? null,
      output_text: null,
      order_seq: summary.order_seq,
      input_truncated: summary.input_truncated ?? null,
      input_original_bytes: summary.input_original_bytes ?? null,
      output_truncated: summary.output_truncated ?? null,
      output_original_bytes: summary.output_original_bytes ?? null,
      first_event_seq: summary.first_event_seq ?? null,
      created_at: summary.created_at,
      updated_at: summary.updated_at,
      summary_only: true,
    } as SessionTurnTool & { summary_only: boolean });
    nextByTurn[turnId] = list;
  }

  for (const [turnId, incoming] of Object.entries(nextByTurn)) {
    const existing = support.turnToolsByTurnId[turnId] ?? [];
    const seen = new Set(existing.map((tool) => String(tool.tool_call_id)));
    const merged = existing.slice();
    let turnChanged = false;
    for (const tool of incoming) {
      const key = String(tool.tool_call_id);
      if (seen.has(key)) continue;
      seen.add(key);
      merged.push(tool);
      turnChanged = true;
    }
    if (turnChanged) {
      support.turnToolsByTurnId = {
        ...support.turnToolsByTurnId,
        [turnId]: merged,
      };
      if (!hydrated[turnId]) hydrated[turnId] = false;
      changed = true;
    }
  }

  if (changed) {
    support.toolSummariesReady = true;
    entry.updatedAtMs = Date.now();
    this.publish();
  }
}

export function applyState(
  this: SessionSupervisorHeadProjectionHost,
  entry: InternalEntry,
  state: SessionState | null,
  stateRev?: number,
) {
  if (!state) return;
  const support = entry.support;
  if (
    typeof stateRev === "number" &&
    typeof entry.stateRev === "number" &&
    stateRev < entry.stateRev
  ) {
    return;
  }
  support.stateLoaded = true;
  support.stateLoading = false;
  this.clearSupportLoadError(entry, "state");
  if (typeof stateRev === "number") {
    entry.stateRev = stateRev;
    support.stateAppliedRev = stateRev;
  } else {
    support.stateAppliedRev = undefined;
  }
  support.artifacts = Array.isArray(state.artifacts) ? state.artifacts : [];
  support.gitStatusSummary = state.git_status ?? null;
  syncStateCache.call(this, entry, stateRev);
}

export function syncStateCache(
  this: SessionSupervisorHeadProjectionHost,
  entry: InternalEntry,
  stateRev?: number,
) {
  this.stateCacheBySessionId.set(entry.sessionId, {
    state: {
      artifacts: entry.support.artifacts.slice(),
      git_status: buildStateGitStatusSummary(this, entry),
    },
    stateRev: typeof stateRev === "number" ? stateRev : entry.support.stateAppliedRev,
  });
}

const buildStateGitStatusSummary = (
  host: SessionSupervisorHeadProjectionHost,
  entry: InternalEntry,
): SessionState["git_status"] => {
  const summary = entry.support.gitStatusSummary;
  const cached = host.stateCacheBySessionId.get(entry.sessionId)?.state.git_status ?? null;
  if (!summary) return null;

  const summaryLine =
    typeof summary?.summary_line === "string"
      ? summary.summary_line
      : typeof summary?.summaryLine === "string"
        ? summary.summaryLine
        : typeof summary?.summary === "string"
          ? summary.summary
          : "";
  if (!summaryLine) return null;

  const readNumber = (value: unknown, fallback: number): number => {
    if (typeof value === "number" && Number.isFinite(value)) return value;
    return fallback;
  };

  return {
    summary_line: summaryLine,
    branch: typeof summary?.branch === "string" ? summary.branch : cached?.branch ?? null,
    upstream: typeof summary?.upstream === "string" ? summary.upstream : cached?.upstream ?? null,
    ahead: readNumber(summary?.ahead, cached?.ahead ?? 0),
    behind: readNumber(summary?.behind, cached?.behind ?? 0),
    detached: typeof summary?.detached === "boolean" ? summary.detached : cached?.detached ?? false,
    staged: readNumber(summary?.staged, cached?.staged ?? 0),
    unstaged: readNumber(summary?.unstaged, cached?.unstaged ?? 0),
    untracked: readNumber(summary?.untracked, cached?.untracked ?? 0),
  };
};

export async function persistHead(
  entry: InternalEntry,
) {
  if (!entry.session) return;
  const turns = stripTurnPartials(entry.turns);
  const events = stripPartialEvents(entry.events);
  const head = {
    session: entry.session,
    turns,
    events,
    messages: entry.messages,
    tool_summaries: entry.toolSummaries,
    last_event_seq: entry.lastEventSeq ?? 0,
    activity: entry.activity ?? undefined,
    has_more_turns: entry.hasMoreTurns,
    summary_checkpoint: entry.summaryCheckpoint ?? null,
    head_window: entry.headWindow ?? undefined,
  };
  await saveSessionHeadV1(entry.sessionId, head);
}

export function applyActiveSnapshotHead(
  this: SessionSupervisorHeadProjectionHost,
  entry: InternalEntry,
  head: SessionHead | SessionHeadSnapshot,
): boolean {
  entry.mode = "active";
  const nextSeq = typeof head.last_event_seq === "number" ? head.last_event_seq : -1;
  const prevSeq = typeof entry.lastEventSeq === "number" ? entry.lastEventSeq : -1;
  if (entry.turnsHydrated && prevSeq >= nextSeq) {
    if (!entry.session) {
      entry.session = head.session;
      return true;
    }
    return false;
  }
  applyHead.call(this, entry, head as SessionHead, {
    freshness: resolveActiveSnapshotSeedFreshness.call(this, entry),
  });
  void persistHead.call(this, entry);
  entry.error = undefined;
  this.setSessionLoadState(entry, "live");
  return true;
}

export function resetEntryProjectionForReplace(
  this: SessionSupervisorHeadProjectionHost,
  entry: InternalEntry,
  opts?: { skipPublish?: boolean },
) {
  const support = entry.support;
  entry.activity = null;
  entry.turns = [];
  entry.events = [];
  entry.messages = [];
  entry.queue = [];
  clearAllAssistantStreaming(entry);
  this.bumpTurnsRev(entry);
  this.bumpEventsRev(entry);
  this.bumpMessagesRev(entry);
  support.turnToolsByTurnId = {};
  support.turnToolsHydratedByTurnId = {};
  support.turnToolsLoadingSet.clear();
  entry.toolStatusByKey.clear();
  entry.toolIdsByTurn.clear();
  entry.historyExtended = false;
  entry.seqSet.clear();
  entry.startedTurnIds.clear();
  entry.turnsHydrated = false;
  support.toolSummariesReady = false;
  entry.hasMoreTurns = true;
  this.setSessionLoadState(entry, "recovering");
  entry.updatedAtMs = Date.now();
  if (!opts?.skipPublish) {
    this.publish();
  }
}
