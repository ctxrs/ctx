import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Session, SessionEvent, SessionHead, SessionTurn } from "../api/client";
import { SessionSupervisor, type SessionCacheEntry } from "./sessionSupervisorCore";
import { resetTurnOutcomeTrackingForTests } from "../utils/analytics/turnOutcomeDedup";
import { resetTurnStartTrackingForTests } from "../utils/analytics/turnStartDedup";
import {
  applyHead,
  type SessionSupervisorHeadProjectionHost,
} from "./sessionSupervisor/headProjection";
import {
  applyReplicaPatches,
  type SessionSupervisorReplicaPatchHost,
} from "./sessionSupervisor/replicaPatchApply";
import type { SessionReplicaPatch } from "./sessionReplicaProtocol";
import type { InternalEntry } from "./sessionSupervisor/entryState";

const trackTurnStarted = vi.hoisted(() => vi.fn());
const trackTurnCompleted = vi.hoisted(() => vi.fn());
const trackProviderRunCompleted = vi.hoisted(() => vi.fn());
const trackFirstTurnCompleted = vi.hoisted(() => vi.fn());
const ensureThoughtCacheMock = vi.hoisted(() => vi.fn(async () => {}));

vi.mock("../utils/analytics", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../utils/analytics")>();
  return {
    ...actual,
    trackTurnStarted,
    trackTurnCompleted,
    trackProviderRunCompleted,
    trackFirstTurnCompleted,
  };
});

vi.mock("./sessionSupervisor/thoughtCache", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./sessionSupervisor/thoughtCache")>();
  return {
    ...actual,
    ensureThoughtCache: ensureThoughtCacheMock,
  };
});

const baseIso = "2024-01-01T00:00:00.000Z";

type SessionSupervisorInternals = {
  ensureEntry: (sessionId: string) => SessionCacheEntry;
  ensureTurnFromEvent: (entry: SessionCacheEntry, event: SessionEvent) => boolean;
};

const asSupervisorInternals = (supervisor: SessionSupervisor): SessionSupervisorInternals =>
  supervisor as unknown as SessionSupervisorInternals;

const asHeadHost = (supervisor: SessionSupervisor): SessionSupervisorHeadProjectionHost =>
  supervisor as unknown as SessionSupervisorHeadProjectionHost;

const asReplicaPatchHost = (supervisor: SessionSupervisor): SessionSupervisorReplicaPatchHost =>
  supervisor as unknown as SessionSupervisorReplicaPatchHost;

const asInternalEntry = (entry: SessionCacheEntry): InternalEntry =>
  entry as unknown as InternalEntry;

const applyStreamDeltaPatch = (
  supervisor: SessionSupervisor,
  data: SessionReplicaPatch["data"],
) => {
  applyReplicaPatches(asReplicaPatchHost(supervisor), [{
    op: "append",
    sessionId: "session-1",
    data: {
      appendMode: "stream_delta",
      ...data,
    },
  }]);
};

const setupSupervisorWithSession = () => {
  const supervisor = new SessionSupervisor();
  const internals = asSupervisorInternals(supervisor);
  const entry = internals.ensureEntry("session-1");
  const session: Session = {
    id: "session-1",
    task_id: "task-1",
    workspace_id: "workspace-1",
    worktree_id: "worktree-1",
    provider_id: "codex",
    model_id: "gpt-5",
    agent_role: "implementer",
    status: "active",
    title: "Demo session",
    created_at: baseIso,
  };
  entry.session = session;

  return { supervisor, entry };
};

const setupEntry = () => {
  const { supervisor, entry } = setupSupervisorWithSession();
  const internals = asSupervisorInternals(supervisor);

  const startEvent: SessionEvent = {
    seq: 1,
    id: "ev-1",
    session_id: "session-1",
    turn_id: "turn-1",
    event_type: "turn_started",
    payload_json: {},
    created_at: baseIso,
  };
  internals.ensureTurnFromEvent(entry, startEvent);

  return { supervisor, entry, turnId: "turn-1" };
};

const buildRunningTurn = (turnId: string): SessionTurn => ({
  turn_id: turnId,
  session_id: "session-1",
  run_id: null,
  user_message_id: null,
  status: "running",
  start_seq: 1,
  end_seq: null,
  started_at: baseIso,
  updated_at: baseIso,
  assistant_partial: "",
  thought_partial: "",
  metrics_json: null,
  tool_total: 0,
  tool_pending: 0,
  tool_running: 0,
  tool_completed: 0,
  tool_failed: 0,
});

const buildCompletedTurn = (turnId: string): SessionTurn => ({
  turn_id: turnId,
  session_id: "session-1",
  run_id: null,
  user_message_id: null,
  status: "completed",
  start_seq: 1,
  end_seq: 2,
  started_at: baseIso,
  updated_at: baseIso,
  assistant_partial: "",
  thought_partial: "",
  metrics_json: null,
  tool_total: 0,
  tool_pending: 0,
  tool_running: 0,
  tool_completed: 0,
  tool_failed: 0,
});

describe("SessionSupervisor analytics tracking", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    resetTurnOutcomeTrackingForTests();
    resetTurnStartTrackingForTests();
  });

  it("emits start analytics for a live stream_delta running turn", () => {
    const { supervisor } = setupSupervisorWithSession();
    applyStreamDeltaPatch(supervisor, {
      turns: [buildRunningTurn("turn-1")],
      events: [],
      messages: [],
      turnsRev: 1,
      eventsRev: 1,
      messagesRev: 1,
    });

    expect(trackTurnStarted).toHaveBeenCalledTimes(1);
    expect(trackTurnStarted).toHaveBeenCalledWith(expect.objectContaining({
      providerId: "codex",
      modelId: "gpt-5",
      sessionKind: "primary",
    }));
  });

  it("emits terminal analytics for a live stream_delta completed turn", () => {
    const { supervisor } = setupSupervisorWithSession();
    applyStreamDeltaPatch(supervisor, {
      turns: [{
        ...buildCompletedTurn("turn-1"),
        metrics_json: {
          context_tokens_estimate: 120,
          total_input_tokens: 80,
          total_output_tokens: 40,
        },
      }],
      events: [],
      messages: [],
      turnsRev: 1,
      eventsRev: 1,
      messagesRev: 1,
    });

    expect(trackProviderRunCompleted).toHaveBeenCalledTimes(1);
    expect(trackFirstTurnCompleted).toHaveBeenCalledTimes(1);
    expect(trackTurnCompleted).toHaveBeenCalledTimes(1);
    expect(trackTurnCompleted).toHaveBeenCalledWith(expect.objectContaining({
      status: "completed",
      sessionKind: "primary",
      metrics: expect.objectContaining({
        context_tokens_estimate: 120,
        total_input_tokens: 80,
        total_output_tokens: 40,
      }),
    }));
    expect(trackProviderRunCompleted).toHaveBeenCalledWith(expect.objectContaining({
      durationMs: 0,
      sessionKind: "primary",
    }));
  });

  it("does not emit analytics for metadata-only append patches", () => {
    const { supervisor } = setupSupervisorWithSession();
    applyReplicaPatches(asReplicaPatchHost(supervisor), [{
      op: "append",
      sessionId: "session-1",
      data: {
        appendMode: "metadata_update",
        loading: true,
      },
    }]);

    expect(trackTurnStarted).not.toHaveBeenCalled();
    expect(trackTurnCompleted).not.toHaveBeenCalled();
    expect(trackProviderRunCompleted).not.toHaveBeenCalled();
    expect(trackFirstTurnCompleted).not.toHaveBeenCalled();
  });

  it("does not emit analytics for head_refresh append patches", () => {
    const { supervisor } = setupSupervisorWithSession();
    applyReplicaPatches(asReplicaPatchHost(supervisor), [{
      op: "append",
      sessionId: "session-1",
      data: {
        appendMode: "head_refresh",
        turns: [buildCompletedTurn("turn-1")],
        events: [],
        messages: [],
        turnsRev: 1,
        eventsRev: 1,
        messagesRev: 1,
      },
    }]);

    expect(trackTurnStarted).not.toHaveBeenCalled();
    expect(trackTurnCompleted).not.toHaveBeenCalled();
    expect(trackProviderRunCompleted).not.toHaveBeenCalled();
    expect(trackFirstTurnCompleted).not.toHaveBeenCalled();
  });

  it("marks nested sessions as subagent runs in analytics", () => {
    const { supervisor, entry } = setupSupervisorWithSession();
    entry.session = {
      ...(entry.session as Session),
      parent_session_id: "session-root",
      relationship: "sub_agent",
    };
    applyStreamDeltaPatch(supervisor, {
      turns: [buildCompletedTurn("turn-1")],
      events: [],
      messages: [],
      turnsRev: 1,
      eventsRev: 1,
      messagesRev: 1,
    });

    expect(trackTurnCompleted).toHaveBeenCalledWith(expect.objectContaining({
      sessionKind: "subagent",
    }));
    expect(trackProviderRunCompleted).toHaveBeenCalledWith(expect.objectContaining({
      sessionKind: "subagent",
    }));
    expect(trackFirstTurnCompleted).toHaveBeenCalledWith(expect.objectContaining({
      sessionKind: "subagent",
    }));
  });

  it("does not double count the same terminal turn across repeated stream_delta patches", () => {
    const { supervisor } = setupSupervisorWithSession();
    const patch: SessionReplicaPatch["data"] = {
      turns: [buildCompletedTurn("turn-1")],
      events: [],
      messages: [],
      turnsRev: 1,
      eventsRev: 1,
      messagesRev: 1,
    };

    applyStreamDeltaPatch(supervisor, patch);
    applyStreamDeltaPatch(supervisor, patch);

    expect(trackTurnCompleted).toHaveBeenCalledTimes(1);
    expect(trackProviderRunCompleted).toHaveBeenCalledTimes(1);
    expect(trackFirstTurnCompleted).toHaveBeenCalledTimes(1);
  });

  it("does not emit start analytics during head hydration even if running turns are present", () => {
    const { supervisor, entry } = setupSupervisorWithSession();
    const head: SessionHead = {
      session: entry.session as Session,
      turns: [buildRunningTurn("turn-1")],
      events: [],
      messages: [],
      last_event_seq: 1,
      has_more_turns: false,
    };

    applyHead.call(asHeadHost(supervisor), asInternalEntry(entry), head, {
      freshness: "authoritative",
    });

    expect(trackTurnStarted).not.toHaveBeenCalled();
  });

  it("does not emit analytics across supervisor reloads for hydrated running turns", () => {
    const first = setupSupervisorWithSession();
    const head: SessionHead = {
      session: first.entry.session as Session,
      turns: [buildRunningTurn("turn-1")],
      events: [],
      messages: [],
      last_event_seq: 1,
      has_more_turns: false,
    };

    applyHead.call(asHeadHost(first.supervisor), asInternalEntry(first.entry), head, {
      freshness: "authoritative",
    });
    expect(trackTurnStarted).not.toHaveBeenCalled();

    const second = setupSupervisorWithSession();
    applyHead.call(asHeadHost(second.supervisor), asInternalEntry(second.entry), head, {
      freshness: "authoritative",
    });
    expect(trackTurnStarted).not.toHaveBeenCalled();
  });

  it("does not emit analytics during head hydration even when completed turns are present", () => {
    const { supervisor, entry } = setupSupervisorWithSession();
    const head: SessionHead = {
      session: entry.session as Session,
      turns: [buildCompletedTurn("turn-1")],
      events: [],
      messages: [],
      last_event_seq: 2,
      has_more_turns: false,
    };

    applyHead.call(asHeadHost(supervisor), asInternalEntry(entry), head, {
      freshness: "authoritative",
    });

    expect(trackTurnStarted).not.toHaveBeenCalled();
    expect(trackTurnCompleted).not.toHaveBeenCalled();
    expect(trackProviderRunCompleted).not.toHaveBeenCalled();
    expect(trackFirstTurnCompleted).not.toHaveBeenCalled();
  });

  it("emits start and terminal analytics when replica patches carry completed turns without replaying events", () => {
    const { supervisor } = setupSupervisorWithSession();
    const patch: SessionReplicaPatch = {
      op: "append",
      sessionId: "session-1",
      data: {
        appendMode: "stream_delta",
        turns: [buildCompletedTurn("turn-1")],
        events: [],
        messages: [],
        turnsRev: 1,
        eventsRev: 1,
        messagesRev: 1,
      },
    };

    applyReplicaPatches(asReplicaPatchHost(supervisor), [patch]);

    expect(trackTurnStarted).toHaveBeenCalledTimes(1);
    expect(trackTurnCompleted).toHaveBeenCalledTimes(1);
    expect(trackProviderRunCompleted).toHaveBeenCalledTimes(1);
    expect(trackFirstTurnCompleted).toHaveBeenCalledTimes(1);
    expect(trackProviderRunCompleted).toHaveBeenCalledWith(expect.objectContaining({
      status: "completed",
      sessionKind: "primary",
    }));
  });
});
