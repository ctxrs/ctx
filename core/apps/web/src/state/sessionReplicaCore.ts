import type {
  Session,
  SessionEvent,
  SessionHead,
  SessionHeadSnapshot,
  WorkspaceActiveSnapshotEvent,
} from "@ctx/types";
import type { WorkspaceActiveSnapshotStreamSource } from "./workspaceActiveSnapshotProtocol";
import { clearAllAssistantStreaming } from "./assistantStreaming";
import { loadSessionHeadV1, saveSessionHeadV1 } from "./uiStateStore";
import { ensureReplicaEventSeq, rebuildReplicaTranscriptAuxState } from "./sessionReplicaTranscript";
import {
  isBoundedSessionHead,
  shouldPreserveExistingTranscriptWindow,
  shouldRepairSessionHeadReplace,
} from "./sessionHeadRepair";
import {
  reconcileActivityInterruptedFromTurns,
  reconcileLatestTurnInterruptedFromActivity,
} from "./sessionSupervisor/cachePolicy";
import type {
  SessionReplicaAppendMode,
  SessionReplicaCommand,
  SessionReplicaConfig,
  SessionReplicaData,
  SessionReplicaFreshnessEvent,
  SessionReplicaHeadSeedMode,
  SessionReplicaPatch,
  SessionReplicaReplaceMode,
  SessionReplicaStreamLane,
} from "./sessionReplicaProtocol";
import { isAuthoritativeSessionReplicaReplace } from "./sessionReplicaProtocol";
import { buildCanonicalReplicaPatch } from "./sessionReplicaPatches";
import {
  clearSessionReplicaGapRepairBaseline,
  createSessionReplicaEntry,
  headToReplicaData,
  isOlderReplicaVersion,
  mergeReplicaEvents,
  mergeReplicaMessages,
  mergeReplicaToolSummaries,
  mergeReplicaTurns,
  normalizeReplicaId,
  sanitizeReplicaHeadForCache,
  snapshotToSessionHead,
  type SessionReplicaApi,
  type SessionReplicaApplyHeadOptions,
  type SessionReplicaEntry,
  type SessionReplicaGapRepairBaseline,
} from "./sessionReplicaCoreSupport";
import { handleSessionReplicaWorkspaceEvent } from "./sessionReplicaCoreEvents";

export class SessionReplicaCore {
  private entries = new Map<string, SessionReplicaEntry>();
  private config: SessionReplicaConfig = {
    eventBufferLimit: 800,
    headLimit: 60,
    recoveryHeadLimit: 5,
    recoveryHeadIncludeEvents: false,
  };
  private gapAlertedSessionIds = new Set<string>();
  private gapRepairBaselineBySessionId = new Map<string, SessionReplicaGapRepairBaseline>();
  private gapRepairInFlightSessionIds = new Set<string>();
  private gapRepairPendingSessionIds = new Set<string>();

  constructor(
    private deps: {
      api: SessionReplicaApi;
      emit: (patches: SessionReplicaPatch[]) => void;
      emitFreshness?: (event: SessionReplicaFreshnessEvent) => void;
    },
  ) {}

  handleCommand = (cmd: SessionReplicaCommand) => {
    switch (cmd.type) {
      case "init":
        if (cmd.config) this.config = cmd.config;
        this.deps.api.setAuth?.(cmd.baseUrl ?? null, cmd.authToken ?? null, cmd.runId ?? null);
        return;
      case "update_auth":
        this.deps.api.setAuth?.(cmd.baseUrl ?? null, cmd.authToken ?? null, cmd.runId ?? null);
        return;
      case "open_session":
        this.openSession(cmd.sessionId, {
          force: cmd.force,
          silent: cmd.silent,
          skipCache: cmd.skipCache,
          skipBoundedBootstrapCache: cmd.skipBoundedBootstrapCache,
          hydrateIfNeeded: cmd.hydrateIfNeeded,
          forceHydrate: cmd.forceHydrate,
        }).catch(() => {});
        return;
      case "close_session":
        this.closeSession(cmd.sessionId);
        return;
      case "drop_session":
        this.dropSession(cmd.sessionId);
        return;
      case "refresh_session":
        this.hydrateSessionHead(cmd.sessionId, {
          force: true,
          silent: true,
          emitOp: "append",
        }).catch(() => {});
        return;
      case "hydrate_session_head":
        this.hydrateSessionHead(cmd.sessionId, {
          force: cmd.force,
          silent: cmd.silent,
        }).catch(() => {});
        return;
      case "seed_head":
        this.seedHead(cmd.sessionId, cmd.head, cmd.mode);
        return;
      case "workspace_event":
        this.handleWorkspaceEvent(cmd.event, cmd.receivedAtMs, cmd.lane, cmd.streamSource);
        return;
      case "set_session":
        this.setSession(cmd.session);
        return;
      default:
        return;
    }
  };

  private emitPatches(patches: SessionReplicaPatch[]): void {
    if (patches.length > 0) this.deps.emit(patches);
  }

  private emitPatch(
    op: "append",
    sessionId: string,
    data: SessionReplicaData & { appendMode: SessionReplicaAppendMode },
  ): void;
  private emitPatch(op: "replace", sessionId: string, data: SessionReplicaData): void;
  private emitPatch(op: "evict", sessionId: string, data: { eventsBeforeSeq?: number }): void;
  private emitPatch(
    op: SessionReplicaPatch["op"],
    sessionId: string,
    data: SessionReplicaData | { eventsBeforeSeq?: number },
  ): void {
    const id = normalizeReplicaId(sessionId);
    if (!id) return;
    this.emitPatches([{ op, sessionId: id, data } as SessionReplicaPatch]);
  }

  private ensureEntry(sessionId: string): SessionReplicaEntry {
    const id = normalizeReplicaId(sessionId);
    const existing = this.entries.get(id);
    if (existing) return existing;
    const entry = createSessionReplicaEntry(id);
    this.entries.set(id, entry);
    return entry;
  }

  private authoritativeReplaceModeForHead(
    entry: SessionReplicaEntry,
    head: SessionHead | SessionHeadSnapshot,
  ): SessionReplicaReplaceMode {
    return isBoundedSessionHead(head) || shouldRepairSessionHeadReplace(entry, head)
      ? "repair_replace"
      : "authoritative_replace";
  }

  private ensureEventSeq(entry: SessionReplicaEntry, event: SessionEvent): SessionEvent {
    return ensureReplicaEventSeq(entry, event);
  }

  private normalizeEvents(entry: SessionReplicaEntry, events: SessionEvent[]): SessionEvent[] {
    if (events.length === 0) return events;
    return events.map((event) => this.ensureEventSeq(entry, event));
  }

  private setSession(session: Session): void {
    const sessionId = normalizeReplicaId(session.id);
    if (!sessionId) return;
    const entry = this.ensureEntry(sessionId);
    entry.session = session;
    this.emitPatch("append", sessionId, { session, appendMode: "metadata_update" });
  }

  private closeSession(sessionId: string): void {
    const id = normalizeReplicaId(sessionId);
    if (!id) return;
    const entry = this.entries.get(id);
    if (!entry) return;
    entry.requestToken += 1;
    entry.loading = false;
  }

  private dropSession(sessionId: string): void {
    const id = normalizeReplicaId(sessionId);
    if (!id) return;
    this.entries.delete(id);
    this.clearGapRepairBaseline(id);
    this.gapRepairInFlightSessionIds.delete(id);
    this.gapRepairPendingSessionIds.delete(id);
  }

  private clearGapRepairBaseline(sessionId: string): void {
    clearSessionReplicaGapRepairBaseline(this.gapRepairBaselineBySessionId, sessionId);
  }

  private applyHead(
    entry: SessionReplicaEntry,
    head: SessionHead | SessionHeadSnapshot,
    emitOp: "append" | "replace" = "replace",
    opts?: SessionReplicaApplyHeadOptions,
  ): void {
    const authoritative = isAuthoritativeSessionReplicaReplace(opts?.replaceMode);
    const preservingRepairReplace = opts?.replaceMode === "repair_replace";
    const previousFreshness = entry.freshness;
    const data = headToReplicaData(head);
    let turns = data.turns ?? [];
    let messages = data.messages ?? [];
    entry.events = this.normalizeEvents(entry, entry.events);
    let events = this.normalizeEvents(entry, data.events ?? []);
    const incomingSeq = typeof data.lastEventSeq === "number" ? data.lastEventSeq : -1;
    const existingSeq = typeof entry.lastEventSeq === "number" ? entry.lastEventSeq : -1;
    const incomingProjectionRev =
      typeof data.projectionRev === "number" ? data.projectionRev : null;
    const existingProjectionRev =
      typeof entry.projectionRev === "number" ? entry.projectionRev : null;
    const incomingIsOlder = isOlderReplicaVersion(
      incomingSeq >= 0 ? incomingSeq : null,
      incomingProjectionRev,
      existingSeq >= 0 ? existingSeq : null,
      existingProjectionRev,
    );
    const incomingIsNarrower =
      (!authoritative || preservingRepairReplace) &&
      (turns.length < entry.turns.length ||
        messages.length < entry.messages.length ||
        events.length < entry.events.length ||
        (preservingRepairReplace &&
          shouldPreserveExistingTranscriptWindow(entry, {
            turns,
            messages,
            head_window: data.headWindow ?? undefined,
          })));
    let toolSummaries = data.toolSummaries ?? entry.toolSummaries;
    if (incomingIsOlder || (!authoritative && existingSeq > incomingSeq)) {
      turns = mergeReplicaTurns(turns, entry.turns);
      messages = mergeReplicaMessages(messages, entry.messages);
      events = mergeReplicaEvents(events, entry.events);
      toolSummaries = mergeReplicaToolSummaries(data.toolSummaries ?? [], entry.toolSummaries);
    } else if (incomingIsNarrower) {
      turns = mergeReplicaTurns(entry.turns, turns);
      messages = mergeReplicaMessages(entry.messages, messages);
      events = mergeReplicaEvents(entry.events, events);
      toolSummaries = mergeReplicaToolSummaries(entry.toolSummaries, data.toolSummaries ?? []);
    }

    entry.session = data.session ?? entry.session;
    if (data.activity !== undefined) {
      const existingActivitySeq =
        typeof entry.activityLastEventSeq === "number" ? entry.activityLastEventSeq : null;
      const existingActivityProjectionRev =
        typeof entry.activityProjectionRev === "number" ? entry.activityProjectionRev : null;
      const incomingActivityIsOlder = isOlderReplicaVersion(
        incomingSeq >= 0 ? incomingSeq : null,
        incomingProjectionRev,
        existingActivitySeq,
        existingActivityProjectionRev,
      );
      if (!incomingActivityIsOlder) {
        entry.activity = data.activity ?? null;
        entry.activityLastEventSeq = incomingSeq >= 0 ? incomingSeq : entry.activityLastEventSeq;
        entry.activityProjectionRev = incomingProjectionRev ?? entry.activityProjectionRev;
        if (reconcileLatestTurnInterruptedFromActivity(entry.turns, entry.activity)) {
          entry.turnsRev += 1;
        }
        entry.activity = reconcileActivityInterruptedFromTurns(entry.activity, entry.turns);
      }
    }
    if (opts?.freshness) {
      let nextFreshness = opts.freshness;
      if (previousFreshness === "recovering" && opts.freshness !== "recovering") {
        const baseline = this.gapRepairBaselineBySessionId.get(entry.sessionId);
        const repairedLastEventSeq = incomingSeq >= 0 ? incomingSeq : null;
        const effectiveRepairedLastEventSeq =
          incomingSeq >= 0
            ? Math.max(existingSeq, incomingSeq)
            : existingSeq >= 0
              ? existingSeq
              : null;
        const authoritativeRepair = opts.freshness === "authoritative";
        const repairEpoch =
          typeof opts.gapRepairEpoch === "number" && Number.isFinite(opts.gapRepairEpoch)
            ? opts.gapRepairEpoch
            : null;
        const staleEpochRepair =
          baseline &&
          repairEpoch !== null &&
          typeof baseline.epoch === "number" &&
          baseline.epoch !== repairEpoch;
        const repairMissedBaseline =
          baseline &&
          typeof baseline.lastEventSeq === "number" &&
          (typeof effectiveRepairedLastEventSeq !== "number" ||
            effectiveRepairedLastEventSeq < baseline.lastEventSeq);
        if (!authoritativeRepair) {
          nextFreshness = "recovering";
        } else if (repairMissedBaseline && !staleEpochRepair) {
          this.emitFreshnessEvent({
            type: "gap_repair_mismatch",
            sessionId: entry.sessionId,
            baselineLastEventSeq: baseline?.lastEventSeq ?? null,
            repairedLastEventSeq: effectiveRepairedLastEventSeq ?? repairedLastEventSeq,
          });
          nextFreshness = "recovering";
        } else if (repairMissedBaseline && staleEpochRepair) {
          nextFreshness = "recovering";
        } else {
          this.clearGapRepairBaseline(entry.sessionId);
          this.emitFreshnessEvent({ type: "gap_recovery_finished", sessionId: entry.sessionId });
        }
      }
      entry.freshness = nextFreshness;
    }
    if (data.summaryCheckpoint !== undefined && !incomingIsOlder) {
      entry.summaryCheckpoint = data.summaryCheckpoint ?? null;
    }
    if (data.headWindow !== undefined && !incomingIsOlder) {
      entry.headWindow = data.headWindow ?? null;
    }
    if (data.stateRev !== undefined) {
      entry.stateRev =
        typeof entry.stateRev === "number"
          ? Math.max(entry.stateRev, data.stateRev)
          : data.stateRev;
    }
    if (data.projectionRev !== undefined) {
      entry.projectionRev =
        typeof entry.projectionRev === "number"
          ? Math.max(entry.projectionRev, data.projectionRev)
          : data.projectionRev;
    }
    entry.turns = turns;
    entry.turnsRev += 1;
    if (authoritative || (!incomingIsOlder && !incomingIsNarrower)) {
      clearAllAssistantStreaming(entry);
    }
    entry.messages = messages;
    entry.messagesRev += 1;
    entry.events = events;
    entry.eventsRev += 1;
    entry.toolSummaries = toolSummaries;
    entry.lastEventSeq =
      incomingSeq >= 0 ? Math.max(existingSeq, incomingSeq) : entry.lastEventSeq;
    entry.hasMoreTurns = data.hasMoreTurns ?? entry.hasMoreTurns;
    entry.hydrated = true;
    rebuildReplicaTranscriptAuxState(entry);
    if (emitOp === "append") {
      this.emitPatch("append", entry.sessionId, buildCanonicalReplicaPatch(entry, {
        ...opts,
        appendMode: opts?.appendMode ?? "head_refresh",
      }));
      return;
    }
    this.emitPatch("replace", entry.sessionId, buildCanonicalReplicaPatch(entry, {
      replaceMode: opts?.replaceMode,
    }));
  }

  private async openSession(
    sessionId: string,
    opts?: {
      force?: boolean;
      silent?: boolean;
      minEventSeq?: number;
      skipCache?: boolean;
      skipBoundedBootstrapCache?: boolean;
      hydrateIfNeeded?: boolean;
      forceHydrate?: boolean;
      emitOp?: "append" | "replace";
    },
  ): Promise<void> {
    const id = normalizeReplicaId(sessionId);
    if (!id) return;
    const entry = this.ensureEntry(id);
    const shouldHydrate =
      Boolean(opts?.forceHydrate) ||
      (Boolean(opts?.hydrateIfNeeded) && entry.freshness !== "authoritative");
    if (entry.loading && !opts?.force && !shouldHydrate) return;
    const minSeq = typeof opts?.minEventSeq === "number" ? opts.minEventSeq : undefined;
    if (!opts?.force && entry.hydrated) {
      const entrySeq = typeof entry.lastEventSeq === "number" ? entry.lastEventSeq : -1;
      if ((minSeq === undefined || entrySeq >= minSeq) && !shouldHydrate) {
        if (!opts?.silent) {
          this.emitPatch("append", id, {
            loading: false,
            error: null,
            appendMode: "metadata_update",
          });
        }
        return;
      }
    }

    const token = ++entry.requestToken;
    if (!opts?.skipCache) {
      const cached = await loadSessionHeadV1(id).catch(() => null);
      if (token !== entry.requestToken) return;
      if (cached?.head && (minSeq === undefined || cached.head.last_event_seq >= minSeq)) {
        const shouldSkipBoundedBootstrapCache =
          opts?.skipBoundedBootstrapCache && isBoundedSessionHead(cached.head);
        if (!shouldSkipBoundedBootstrapCache) {
          this.applyHead(entry, cached.head, opts?.emitOp, {
            appendMode: opts?.emitOp === "append" ? "head_refresh" : undefined,
            replaceMode: opts?.emitOp === "append" ? undefined : "bootstrap_seed",
            freshness: "bootstrap",
          });
        }
      }
    }
    if (!opts?.force && entry.hydrated) {
      const entrySeq = typeof entry.lastEventSeq === "number" ? entry.lastEventSeq : -1;
      if ((minSeq === undefined || entrySeq >= minSeq) && !shouldHydrate) {
        if (!opts?.silent) {
          this.emitPatch("append", id, {
            loading: false,
            error: null,
            appendMode: "metadata_update",
          });
        }
        return;
      }
    }

    entry.loading = true;
    if (!opts?.silent) {
      this.emitPatch("append", id, {
        loading: true,
        error: null,
        appendMode: "metadata_update",
      });
    }
    if (!shouldHydrate) {
      entry.loading = false;
      if (!opts?.silent) {
        this.emitPatch("append", id, {
          loading: false,
          error: null,
          appendMode: "metadata_update",
        });
      }
      return;
    }

    try {
      const head = await this.deps.api.getSessionHead(id, this.config.headLimit, true);
      if (token !== entry.requestToken) return;
      if (head) {
        const sessionHead = snapshotToSessionHead(head);
        this.applyHead(entry, sessionHead, opts?.emitOp, {
          appendMode: opts?.emitOp === "append" ? "head_refresh" : undefined,
          replaceMode:
            opts?.emitOp === "append"
              ? undefined
              : this.authoritativeReplaceModeForHead(entry, sessionHead),
          freshness: "authoritative",
        });
        await this.persistHead(entry);
      }
      entry.loading = false;
      if (!opts?.silent) {
        this.emitPatch("append", id, {
          loading: false,
          error: null,
          appendMode: "metadata_update",
        });
      }
    } catch (error) {
      entry.loading = false;
      const message =
        error instanceof Error && error.message
          ? error.message
          : typeof error === "string"
            ? error
            : "request failed";
      if (!opts?.silent) {
        this.emitPatch("append", id, {
          loading: false,
          error: message,
          appendMode: "metadata_update",
        });
      }
    }
  }

  private async hydrateSessionHead(
    sessionId: string,
    opts?: {
      force?: boolean;
      silent?: boolean;
      emitOp?: "append" | "replace";
      headLimit?: number;
      includeEvents?: boolean;
      coalesce?: boolean;
      gapRepairEpoch?: number;
      minEventSeq?: number;
    },
  ): Promise<void> {
    const id = normalizeReplicaId(sessionId);
    if (!id) return;
    const entry = this.ensureEntry(id);
    if (opts?.coalesce && this.gapRepairInFlightSessionIds.has(id)) {
      this.gapRepairPendingSessionIds.add(id);
      return;
    }
    if (entry.loading && !opts?.force) return;
    if (!opts?.force && entry.hydrated) return;

    const gapRepairEpoch =
      typeof opts?.gapRepairEpoch === "number" && Number.isFinite(opts.gapRepairEpoch)
        ? opts.gapRepairEpoch
        : undefined;
    const token = ++entry.requestToken;
    if (opts?.coalesce) {
      this.gapRepairInFlightSessionIds.add(id);
    }
    entry.loading = true;
    if (!opts?.silent) {
      this.emitPatch("append", id, {
        loading: true,
        error: null,
        appendMode: "metadata_update",
      });
    }

    try {
      const minEventSeq =
        typeof opts?.minEventSeq === "number" && Number.isFinite(opts.minEventSeq)
          ? opts.minEventSeq
          : undefined;
      const headLimit = opts?.headLimit ?? this.config.headLimit;
      const includeEvents = opts?.includeEvents ?? true;
      const head =
        minEventSeq === undefined
          ? await this.deps.api.getSessionHead(id, headLimit, includeEvents)
          : await this.deps.api.getSessionHead(id, headLimit, includeEvents, { minEventSeq });
      if (token !== entry.requestToken) return;
      if (head) {
        const sessionHead = snapshotToSessionHead(head);
        this.applyHead(entry, sessionHead, opts?.emitOp, {
          appendMode: opts?.emitOp === "append" ? "head_refresh" : undefined,
          replaceMode:
            opts?.emitOp === "append"
              ? undefined
              : this.authoritativeReplaceModeForHead(entry, sessionHead),
          freshness: "authoritative",
          gapRepairEpoch,
        });
        await this.persistHead(entry);
      }
      entry.loading = false;
      if (!opts?.silent) {
        this.emitPatch("append", id, {
          loading: false,
          appendMode: "metadata_update",
        });
      }
    } catch (error) {
      entry.loading = false;
      const message =
        error instanceof Error && error.message
          ? error.message
          : typeof error === "string"
            ? error
            : "request failed";
      if (!opts?.silent) {
        this.emitPatch("append", id, {
          loading: false,
          error: message,
          appendMode: "metadata_update",
        });
      }
    } finally {
      if (opts?.coalesce) {
        this.gapRepairInFlightSessionIds.delete(id);
        const hasPendingRepair = this.gapRepairPendingSessionIds.delete(id);
        const shouldRunPendingRepair =
          hasPendingRepair &&
          (this.gapRepairBaselineBySessionId.has(id) ||
            this.entries.get(id)?.freshness === "recovering");
        if (shouldRunPendingRepair) {
          const pendingBaseline = this.gapRepairBaselineBySessionId.get(id);
          void this.hydrateSessionHead(id, {
            ...opts,
            gapRepairEpoch: pendingBaseline?.epoch ?? opts.gapRepairEpoch,
            minEventSeq: pendingBaseline?.lastEventSeq ?? opts.minEventSeq,
          }).catch(() => {});
        }
      }
    }
  }

  private seedHead(
    sessionId: string,
    head: SessionHeadSnapshot,
    mode: SessionReplicaHeadSeedMode,
  ): void {
    const id = normalizeReplicaId(sessionId);
    if (!id) return;
    const entry = this.ensureEntry(id);
    this.applyHead(entry, head, "replace", {
      replaceMode: mode,
      freshness: mode === "repair_replace" ? "authoritative" : "bootstrap",
    });
    entry.hydrated = true;
  }

  private handleWorkspaceEvent(
    evt: WorkspaceActiveSnapshotEvent,
    receivedAtMs?: number | null,
    lane?: SessionReplicaStreamLane,
    streamSource?: WorkspaceActiveSnapshotStreamSource | null,
  ): void {
    handleSessionReplicaWorkspaceEvent(
      {
        entries: this.entries,
        config: this.config,
        gapAlertedSessionIds: this.gapAlertedSessionIds,
        gapRepairBaselineBySessionId: this.gapRepairBaselineBySessionId,
        ensureEntry: (sessionId) => this.ensureEntry(sessionId),
        applyHead: (entry, head, emitOp, opts) => this.applyHead(entry, head, emitOp, opts),
        emitAppendPatch: (sessionId, data) => this.emitPatch("append", sessionId, data),
        emitEvictPatch: (sessionId, data) => this.emitPatch("evict", sessionId, data),
        hydrateSessionHead: (sessionId, opts) => this.hydrateSessionHead(sessionId, opts),
        persistHead: (entry) => this.persistHead(entry),
        emitFreshnessEvent: (event) => this.emitFreshnessEvent(event),
      },
      evt,
      receivedAtMs,
      lane,
      streamSource,
    );
  }

  private emitFreshnessEvent(event: SessionReplicaFreshnessEvent): void {
    this.deps.emitFreshness?.(event);
  }

  private async persistHead(entry: SessionReplicaEntry): Promise<void> {
    if (!entry.session || typeof entry.lastEventSeq !== "number") return;
    const head: SessionHead = {
      session: entry.session,
      turns: entry.turns,
      tool_summaries: entry.toolSummaries,
      events: entry.events,
      messages: entry.messages,
      last_event_seq: entry.lastEventSeq,
      projection_rev: entry.projectionRev,
      activity: entry.activity ?? undefined,
      has_more_turns: entry.hasMoreTurns,
      summary_checkpoint: entry.summaryCheckpoint ?? null,
      head_window: entry.headWindow ?? undefined,
    };
    await saveSessionHeadV1(entry.sessionId, sanitizeReplicaHeadForCache(head)).catch(() => {});
  }
}
