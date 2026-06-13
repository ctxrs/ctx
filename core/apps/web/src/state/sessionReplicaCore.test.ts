import { beforeEach, describe, expect, it, vi } from "vitest";
import type {
  Message,
  Session,
  SessionActivityState,
  SessionEvent,
  SessionHead,
  SessionHeadSnapshot,
  SessionTurn,
  WorkspaceActiveSnapshotEvent,
} from "@ctx/types";
import { waitForCondition } from "../testUtils/waitForCondition";
import { SessionReplicaCore } from "./sessionReplicaCore";
import type { SessionReplicaFreshnessEvent, SessionReplicaPatch } from "./sessionReplicaProtocol";
import { loadSessionHeadV1 } from "./uiStateStore";

vi.mock("./uiStateStore", () => ({
  clearSessionHeadV1: vi.fn(async () => {}),
  clearSessionHistoryPagesV1: vi.fn(async () => {}),
  loadSessionHeadV1: vi.fn(async () => null),
  saveSessionHeadV1: vi.fn(async () => {}),
}));

const mkSession = (sessionId: string): Session => ({
  id: sessionId,
  task_id: "task-1",
  workspace_id: "ws-1",
  worktree_id: "wt-1",
  provider_id: "fake",
  model_id: "fake-model",
  title: "Session",
  agent_role: "assistant",
  status: "active",
});

const mkHead = (
  sessionId: string,
  messageText = "hello",
  lastEventSeq = 1,
): SessionHeadSnapshot => {
  const now = new Date().toISOString();
  const message: Message = {
    id: `m-${sessionId}`,
    session_id: sessionId,
    task_id: "task-1",
    role: "assistant",
    content: messageText,
    delivery: "immediate",
    created_at: now,
  };
  return {
    session: mkSession(sessionId),
    turns: [] as SessionTurn[],
    events: [] as SessionEvent[],
    messages: [message],
    last_event_seq: lastEventSeq,
    state_rev: lastEventSeq,
    has_more_turns: false,
    has_more_history: false,
    history_cursor: null,
  };
};

const mkCompletedTurn = (
  sessionId: string,
  turnId: string,
  startSeq: number,
  createdAt: string,
): SessionTurn => ({
  turn_id: turnId,
  session_id: sessionId,
  status: "completed",
  start_seq: startSeq,
  started_at: createdAt,
  updated_at: createdAt,
  tool_total: 0,
  tool_pending: 0,
  tool_running: 0,
  tool_completed: 0,
  tool_failed: 0,
});

const mkMessageForTurn = (
  sessionId: string,
  messageId: string,
  turnId: string,
  content: string,
  createdAt: string,
): Message => ({
  id: messageId,
  session_id: sessionId,
  task_id: "task-1",
  turn_id: turnId,
  role: "assistant",
  content,
  delivery: "immediate",
  created_at: createdAt,
});

const loadSessionHeadV1Mock = vi.mocked(loadSessionHeadV1);

describe("SessionReplicaCore", () => {
  beforeEach(() => {
    loadSessionHeadV1Mock.mockReset();
    loadSessionHeadV1Mock.mockResolvedValue(null);
  });

  it("skips bounded cached bootstrap heads when requested during active open", async () => {
    const sessionId = "session-bounded-bootstrap-skip";
    const cachedHead = {
      session: mkSession(sessionId),
      turns: [] as SessionTurn[],
      events: [] as SessionEvent[],
      messages: [
        {
          id: `m-${sessionId}-cached`,
          session_id: sessionId,
          task_id: "task-1",
          role: "assistant",
          content: "cached-bootstrap",
          delivery: "immediate",
          created_at: new Date().toISOString(),
        },
      ],
      last_event_seq: 1,
      has_more_turns: true,
      head_window: {
        turn_limit: 40,
        message_limit: 120,
        event_limit: 120,
        byte_limit: 1024 * 1024,
        turn_count: 40,
        message_count: 40,
        event_count: 40,
        bytes: 4096,
      },
    } satisfies SessionHead;
    const authoritativeHead = {
      ...mkHead(sessionId, "authoritative-head"),
      last_event_seq: 2,
      state_rev: 2,
      head_window: {
        turn_limit: 40,
        message_limit: 120,
        event_limit: 120,
        byte_limit: 1024 * 1024,
        turn_count: 40,
        message_count: 40,
        event_count: 40,
        bytes: 4096,
      },
    } satisfies SessionHeadSnapshot;
    loadSessionHeadV1Mock.mockResolvedValueOnce({
      v: 1,
      sessionId,
      updatedAtMs: Date.now(),
      head: cachedHead,
    });
    const getSessionHead = vi.fn(async () => authoritativeHead);
    const patches: SessionReplicaPatch[] = [];
    const core = new SessionReplicaCore({
      api: { getSessionHead },
      emit: (next) => patches.push(...next),
    });

    core.handleCommand({ type: "init", config: { eventBufferLimit: 100, headLimit: 50 } });
    core.handleCommand({
      type: "open_session",
      sessionId,
      hydrateIfNeeded: true,
      skipBoundedBootstrapCache: true,
    });

    await waitForCondition(() =>
      patches.some(
        (patch) =>
          patch.op !== "evict" &&
          patch.sessionId === sessionId &&
          patch.data.messages?.some((message) => message.content === "authoritative-head"),
      ),
    );

    expect(getSessionHead).toHaveBeenCalledTimes(1);
    expect(
      patches.some(
        (patch) =>
          patch.op !== "evict" &&
          patch.sessionId === sessionId &&
          patch.data.freshness === "bootstrap" &&
          patch.data.messages?.some((message) => message.content === "cached-bootstrap"),
      ),
    ).toBe(false);
  });

  it("keeps open_session stream-only by default", async () => {
    const getSessionHead = vi.fn();
    const patches: SessionReplicaPatch[] = [];
    const core = new SessionReplicaCore({
      api: { getSessionHead },
      emit: (next) => patches.push(...next),
    });

    core.handleCommand({ type: "init", config: { eventBufferLimit: 100, headLimit: 50 } });
    core.handleCommand({ type: "open_session", sessionId: "session-stream-only" });

    await waitForCondition(() => patches.length > 0);
    expect(getSessionHead).not.toHaveBeenCalled();
    const latest = patches[patches.length - 1];
    if (latest?.op === "evict") {
      throw new Error("expected append/replace patch");
    }
    expect(latest?.data?.error).toBeFalsy();
  });

  it("lets forced hydration supersede an in-flight open_session load", async () => {
    const sessionId = "session-force-hydrate-supersedes-loading";
    let resolveFirstHead!: (value: SessionHeadSnapshot) => void;
    const firstHeadPromise = new Promise<SessionHeadSnapshot>((resolve) => {
      resolveFirstHead = resolve;
    });
    const getSessionHead = vi
      .fn<() => Promise<SessionHeadSnapshot>>()
      .mockImplementationOnce(() => firstHeadPromise)
      .mockResolvedValueOnce(mkHead(sessionId, "second-head", 2));
    const patches: SessionReplicaPatch[] = [];
    const core = new SessionReplicaCore({
      api: { getSessionHead },
      emit: (next) => patches.push(...next),
    });

    core.handleCommand({ type: "init", config: { eventBufferLimit: 100, headLimit: 50 } });
    core.handleCommand({ type: "open_session", sessionId, hydrateIfNeeded: true });
    await waitForCondition(() => getSessionHead.mock.calls.length === 1);

    core.handleCommand({ type: "open_session", sessionId, forceHydrate: true });
    await waitForCondition(() => getSessionHead.mock.calls.length === 2);
    await waitForCondition(() =>
      patches.some(
        (patch) =>
          patch.op !== "evict" &&
          patch.sessionId === sessionId &&
          patch.data.messages?.some((message) => message.content === "second-head"),
      ),
    );

    resolveFirstHead(mkHead(sessionId, "stale-first-head", 1));
    await Promise.resolve();
    expect(
      patches.some(
        (patch) =>
          patch.op !== "evict" &&
          patch.sessionId === sessionId &&
          patch.data.messages?.some((message) => message.content === "stale-first-head"),
      ),
    ).toBe(false);
  });

  it("emits explicit lifecycle replace modes for bootstrap seeds and authoritative repairs", () => {
    const sessionId = "session-replace-modes";
    const patches: SessionReplicaPatch[] = [];
    const core = new SessionReplicaCore({
      api: { getSessionHead: vi.fn() },
      emit: (next) => patches.push(...next),
    });

    core.handleCommand({ type: "init", config: { eventBufferLimit: 100, headLimit: 50 } });
    core.handleCommand({
      type: "seed_head",
      sessionId,
      head: mkHead(sessionId, "bootstrap-head"),
      mode: "bootstrap_seed",
    });
    core.handleCommand({
      type: "seed_head",
      sessionId,
      head: {
        ...mkHead(sessionId, "repair-head"),
        last_event_seq: 2,
        projection_rev: 2,
        activity: { is_working: false, last_turn_status: "completed" },
      },
      mode: "repair_replace",
    });

    const replacePatches = patches.filter(
      (patch): patch is Exclude<SessionReplicaPatch, { op: "evict" }> =>
        patch.sessionId === sessionId && patch.op === "replace",
    );
    expect(replacePatches).toHaveLength(2);
    expect(replacePatches[0]?.data.replaceMode).toBe("bootstrap_seed");
    expect(replacePatches[0]?.data.freshness).toBe("bootstrap");
    expect(replacePatches[1]?.data.replaceMode).toBe("repair_replace");
    expect(replacePatches[1]?.data.freshness).toBe("authoritative");
  });

  it("accepts repair_replace heads when last_event_seq advances even if projection_rev trails", () => {
    const sessionId = "session-repair-seq-dominates";
    const patches: SessionReplicaPatch[] = [];
    const core = new SessionReplicaCore({
      api: { getSessionHead: vi.fn() },
      emit: (next) => patches.push(...next),
    });
    const createdAt = "2026-03-09T00:00:01.000Z";

    core.handleCommand({ type: "init", config: { eventBufferLimit: 100, headLimit: 50 } });
    core.handleCommand({
      type: "seed_head",
      sessionId,
      head: {
        session: mkSession(sessionId),
        turns: [
          {
            turn_id: "turn-1",
            session_id: sessionId,
            run_id: "run-1",
            user_message_id: "message-1",
            status: "running",
            start_seq: 1,
            end_seq: null,
            started_at: createdAt,
            updated_at: createdAt,
            assistant_partial: "",
            thought_partial: "",
            metrics_json: null,
            tool_total: 0,
            tool_pending: 0,
            tool_running: 0,
            tool_completed: 0,
            tool_failed: 0,
          },
        ],
        events: [] as SessionEvent[],
        messages: [] as Message[],
        activity: { is_working: true, last_turn_status: "running" },
        last_event_seq: 4,
        projection_rev: 7,
        state_rev: 7,
        has_more_turns: false,
        has_more_history: false,
        history_cursor: null,
      },
      mode: "bootstrap_seed",
    });
    core.handleCommand({
      type: "seed_head",
      sessionId,
      head: {
        session: mkSession(sessionId),
        turns: [
          {
            turn_id: "turn-1",
            session_id: sessionId,
            run_id: "run-1",
            user_message_id: "message-1",
            status: "completed",
            start_seq: 1,
            end_seq: 5,
            started_at: createdAt,
            updated_at: "2026-03-09T00:00:05.000Z",
            assistant_partial: null,
            thought_partial: "",
            metrics_json: null,
            tool_total: 0,
            tool_pending: 0,
            tool_running: 0,
            tool_completed: 0,
            tool_failed: 0,
          },
        ],
        events: [] as SessionEvent[],
        messages: [] as Message[],
        activity: { is_working: false, last_turn_status: "completed" },
        last_event_seq: 5,
        projection_rev: 6,
        state_rev: 7,
        has_more_turns: false,
        has_more_history: false,
        history_cursor: null,
      },
      mode: "repair_replace",
    });

    const latest = [...patches].reverse().find(
      (patch) => patch.sessionId === sessionId && patch.op === "replace",
    );
    if (!latest || latest.op === "evict") {
      throw new Error("expected repair replace patch");
    }

    expect(latest.data.replaceMode).toBe("repair_replace");
    expect(latest.data.lastEventSeq).toBe(5);
    expect(latest.data.activity).toEqual({ is_working: false, last_turn_status: "completed" });
    expect(latest.data.turns?.[0]?.status).toBe("completed");
    expect(latest.data.turns?.[0]?.end_seq).toBe(5);
  });

  it("keeps terminal turns when merging an older cached bootstrap head", async () => {
    const sessionId = "session-bootstrap-cache-merge";
    const patches: SessionReplicaPatch[] = [];
    const getSessionHead = vi.fn();
    const core = new SessionReplicaCore({
      api: { getSessionHead },
      emit: (next) => patches.push(...next),
    });
    const createdAt = "2026-03-09T00:00:01.000Z";

    loadSessionHeadV1Mock.mockResolvedValueOnce({
      v: 1,
      sessionId,
      updatedAtMs: Date.now(),
      head: {
        session: mkSession(sessionId),
        turns: [
          {
            turn_id: "turn-1",
            session_id: sessionId,
            run_id: "run-1",
            user_message_id: "message-1",
            status: "running",
            start_seq: 1,
            end_seq: null,
            started_at: createdAt,
            updated_at: createdAt,
            assistant_partial: "",
            thought_partial: "",
            metrics_json: null,
            tool_total: 0,
            tool_pending: 0,
            tool_running: 0,
            tool_completed: 0,
            tool_failed: 0,
          },
        ],
        events: [] as SessionEvent[],
        messages: [] as Message[],
        activity: { is_working: true, last_turn_status: "running" },
        last_event_seq: 4,
        projection_rev: 6,
        has_more_turns: false,
        head_window: undefined,
        summary_checkpoint: null,
      } satisfies SessionHead,
    });

    core.handleCommand({ type: "init", config: { eventBufferLimit: 100, headLimit: 50 } });
    core.handleCommand({
      type: "seed_head",
      sessionId,
      head: {
        session: mkSession(sessionId),
        turns: [
          {
            turn_id: "turn-1",
            session_id: sessionId,
            run_id: "run-1",
            user_message_id: "message-1",
            status: "completed",
            start_seq: 1,
            end_seq: 5,
            started_at: createdAt,
            updated_at: "2026-03-09T00:00:05.000Z",
            assistant_partial: null,
            thought_partial: "",
            metrics_json: null,
            tool_total: 0,
            tool_pending: 0,
            tool_running: 0,
            tool_completed: 0,
            tool_failed: 0,
          },
        ],
        events: [] as SessionEvent[],
        messages: [] as Message[],
        activity: { is_working: false, last_turn_status: "completed" },
        last_event_seq: 5,
        projection_rev: 7,
        state_rev: 7,
        has_more_turns: false,
        has_more_history: false,
        history_cursor: null,
      },
      mode: "repair_replace",
    });
    core.handleCommand({ type: "open_session", sessionId, force: true });

    await waitForCondition(() =>
      patches.some(
        (patch) =>
          patch.sessionId === sessionId &&
          patch.op === "replace" &&
          patch.data.freshness === "bootstrap",
      ),
    );

    const latest = [...patches].reverse().find(
      (patch) => patch.sessionId === sessionId && patch.op === "replace",
    );
    if (!latest || latest.op === "evict") {
      throw new Error("expected bootstrap replace patch");
    }

    expect(getSessionHead).not.toHaveBeenCalled();
    expect(latest.data.activity).toEqual({ is_working: false, last_turn_status: "completed" });
    expect(latest.data.turns?.[0]?.status).toBe("completed");
    expect(latest.data.turns?.[0]?.end_seq).toBe(5);
  });

  it("refetches /head on session_gap", async () => {
    const head = mkHead("session-gap");
    const getSessionHead = vi.fn(async () => head);
    const core = new SessionReplicaCore({
      api: { getSessionHead },
      emit: () => {},
    });

    core.handleCommand({ type: "init", config: { eventBufferLimit: 100, headLimit: 50 } });
    core.handleCommand({ type: "open_session", sessionId: "session-gap" });
    core.handleCommand({ type: "hydrate_session_head", sessionId: "session-gap" });

    await waitForCondition(() => getSessionHead.mock.calls.length === 1);
    const alertSpy = vi.spyOn(window, "alert").mockImplementation(() => {});

    const gapEvent: WorkspaceActiveSnapshotEvent = {
      type: "session_gap",
      workspace_id: "ws-1",
      snapshot_rev: 2,
      session_id: "session-gap",
      after_seq: 5,
    };
    core.handleCommand({ type: "workspace_event", event: gapEvent });

    await waitForCondition(() => getSessionHead.mock.calls.length === 2);
    expect(getSessionHead).toHaveBeenCalledTimes(2);
    alertSpy.mockRestore();
  });

  it("waits for the paired stream seed when replay session_gap declares seed_follows", async () => {
    const sessionId = "session-gap-replay-seed";
    const initialHead = mkHead(sessionId, "before-gap", 3);
    const seededHead = mkHead(sessionId, "seeded-recovery", 8);
    const getSessionHead = vi.fn(async () => initialHead);
    const patches: SessionReplicaPatch[] = [];
    const freshnessEvents: SessionReplicaFreshnessEvent[] = [];
    const core = new SessionReplicaCore({
      api: { getSessionHead },
      emit: (next) => patches.push(...next),
      emitFreshness: (event) => freshnessEvents.push(event),
    });

    core.handleCommand({ type: "init", config: { eventBufferLimit: 100, headLimit: 50 } });
    core.handleCommand({ type: "open_session", sessionId });
    core.handleCommand({ type: "hydrate_session_head", sessionId });

    await waitForCondition(() => getSessionHead.mock.calls.length === 1);

    core.handleCommand({
      type: "workspace_event",
      event: {
        type: "session_gap",
        workspace_id: "ws-1",
        snapshot_rev: 2,
        session_id: sessionId,
        after_seq: 5,
        reason: "replay_limit_exceeded",
        seed_follows: true,
      },
    });

    await waitForCondition(() =>
      freshnessEvents.some(
        (event) =>
          event.type === "gap_recovery_started" &&
          event.sessionId === sessionId &&
          event.reason === "replay_limit_exceeded",
      ),
    );
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(getSessionHead).toHaveBeenCalledTimes(1);
    expect(freshnessEvents).not.toEqual(
      expect.arrayContaining([{ type: "gap_recovery_finished", sessionId }]),
    );

    core.handleCommand({
      type: "workspace_event",
      event: {
        type: "session_head_seed",
        workspace_id: "ws-1",
        snapshot_rev: 3,
        head: seededHead,
      },
    });

    await waitForCondition(() =>
      freshnessEvents.some((event) => event.type === "gap_recovery_finished" && event.sessionId === sessionId),
    );

    expect(getSessionHead).toHaveBeenCalledTimes(1);
    const seedPatch = [...patches].reverse().find(
      (patch) =>
        patch.sessionId === sessionId &&
        patch.op === "replace" &&
        patch.data.messages?.some((message) => message.content === "seeded-recovery"),
    );
    if (!seedPatch || seedPatch.op === "evict") {
      throw new Error("expected stream seed repair patch");
    }
    expect(seedPatch.data.freshness).toBe("authoritative");
  });

  it("falls back to /head when a paired stream seed does not cover the gap cursor", async () => {
    const sessionId = "session-gap-replay-seed-under-baseline";
    const initialHead = mkHead(sessionId, "before-gap", 3);
    const underBaselineSeed = mkHead(sessionId, "under-baseline-seed", 4);
    const recoveredHead = mkHead(sessionId, "fresh-repair-head", 7);
    const getSessionHead = vi
      .fn(async () => recoveredHead)
      .mockResolvedValueOnce(initialHead)
      .mockResolvedValueOnce(recoveredHead);
    const patches: SessionReplicaPatch[] = [];
    const freshnessEvents: SessionReplicaFreshnessEvent[] = [];
    const core = new SessionReplicaCore({
      api: { getSessionHead },
      emit: (next) => patches.push(...next),
      emitFreshness: (event) => freshnessEvents.push(event),
    });

    core.handleCommand({ type: "init", config: { eventBufferLimit: 100, headLimit: 50 } });
    core.handleCommand({ type: "hydrate_session_head", sessionId });

    await waitForCondition(() => getSessionHead.mock.calls.length === 1);

    core.handleCommand({
      type: "workspace_event",
      event: {
        type: "session_gap",
        workspace_id: "ws-1",
        snapshot_rev: 2,
        session_id: sessionId,
        after_seq: 6,
        reason: "replay_limit_exceeded",
        seed_follows: true,
      },
    });

    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(getSessionHead).toHaveBeenCalledTimes(1);

    core.handleCommand({
      type: "workspace_event",
      event: {
        type: "session_head_seed",
        workspace_id: "ws-1",
        snapshot_rev: 3,
        head: underBaselineSeed,
      },
    });

    await waitForCondition(() => getSessionHead.mock.calls.length === 2);
    await waitForCondition(() =>
      freshnessEvents.some((event) => event.type === "gap_recovery_finished" && event.sessionId === sessionId),
    );

    expect(getSessionHead).toHaveBeenLastCalledWith(sessionId, 5, false, { minEventSeq: 6 });
    expect(freshnessEvents).not.toEqual(
      expect.arrayContaining([
        expect.objectContaining({ type: "gap_repair_mismatch", sessionId }),
      ]),
    );
    const seedPatch = [...patches].reverse().find(
      (patch) =>
        patch.sessionId === sessionId &&
        patch.op === "replace" &&
        patch.data.messages?.some((message) => message.content === "under-baseline-seed"),
    );
    if (!seedPatch || seedPatch.op === "evict") {
      throw new Error("expected under-baseline seed patch");
    }
    expect(seedPatch.data.freshness).toBe("recovering");
    const recoveredPatch = [...patches].reverse().find(
      (patch) =>
        patch.sessionId === sessionId &&
        patch.op === "replace" &&
        patch.data.messages?.some((message) => message.content === "fresh-repair-head"),
    );
    if (!recoveredPatch || recoveredPatch.op === "evict") {
      throw new Error("expected HTTP repair patch");
    }
    expect(recoveredPatch.data.freshness).toBe("authoritative");
    expect(recoveredPatch.data.lastEventSeq).toBe(7);
  });

  it("queues a newer repair when a seeded gap arrives during an older fallback repair", async () => {
    const sessionId = "session-gap-replay-seed-overlap";
    const initialHead = mkHead(sessionId, "before-gap", 3);
    const underBaselineSeed = mkHead(sessionId, "under-baseline-seed", 4);
    const staleFirstRepair = mkHead(sessionId, "stale-first-repair", 7);
    const recoveredHead = mkHead(sessionId, "fresh-second-repair", 10);
    let resolveFirstRepair: (value: SessionHeadSnapshot) => void = () => {
      throw new Error("first repair resolver was not initialized");
    };
    let resolveSecondRepair: (value: SessionHeadSnapshot) => void = () => {
      throw new Error("second repair resolver was not initialized");
    };
    const getSessionHead = vi
      .fn(async () => initialHead)
      .mockResolvedValueOnce(initialHead)
      .mockImplementationOnce(
        () =>
          new Promise<SessionHeadSnapshot>((resolve) => {
            resolveFirstRepair = resolve;
          }),
      )
      .mockImplementationOnce(
        () =>
          new Promise<SessionHeadSnapshot>((resolve) => {
            resolveSecondRepair = resolve;
          }),
      );
    const freshnessEvents: SessionReplicaFreshnessEvent[] = [];
    const core = new SessionReplicaCore({
      api: { getSessionHead },
      emit: () => {},
      emitFreshness: (event) => freshnessEvents.push(event),
    });

    core.handleCommand({ type: "init", config: { eventBufferLimit: 100, headLimit: 50 } });
    core.handleCommand({ type: "hydrate_session_head", sessionId });
    await waitForCondition(() => getSessionHead.mock.calls.length === 1);

    core.handleCommand({
      type: "workspace_event",
      event: {
        type: "session_gap",
        workspace_id: "ws-1",
        snapshot_rev: 2,
        session_id: sessionId,
        after_seq: 6,
        reason: "replay_limit_exceeded",
        seed_follows: true,
      },
    });
    core.handleCommand({
      type: "workspace_event",
      event: {
        type: "session_head_seed",
        workspace_id: "ws-1",
        snapshot_rev: 3,
        head: underBaselineSeed,
      },
    });
    await waitForCondition(() => getSessionHead.mock.calls.length === 2);

    core.handleCommand({
      type: "workspace_event",
      event: {
        type: "session_gap",
        workspace_id: "ws-1",
        snapshot_rev: 4,
        session_id: sessionId,
        after_seq: 9,
        reason: "replay_limit_exceeded",
        seed_follows: true,
      },
    });
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(getSessionHead).toHaveBeenCalledTimes(2);

    resolveFirstRepair(staleFirstRepair);
    await waitForCondition(() => getSessionHead.mock.calls.length === 3);
    expect(freshnessEvents).not.toEqual(
      expect.arrayContaining([
        expect.objectContaining({ type: "gap_repair_mismatch", sessionId }),
      ]),
    );
    expect(freshnessEvents).not.toEqual(
      expect.arrayContaining([{ type: "gap_recovery_finished", sessionId }]),
    );

    resolveSecondRepair(recoveredHead);
    await waitForCondition(() =>
      freshnessEvents.some((event) => event.type === "gap_recovery_finished" && event.sessionId === sessionId),
    );
    expect(freshnessEvents).not.toEqual(
      expect.arrayContaining([
        expect.objectContaining({ type: "gap_repair_mismatch", sessionId }),
      ]),
    );
  });

  it("starts /head fallback when a promised replay seed does not arrive", async () => {
    const sessionId = "session-gap-replay-seed-missing";
    const initialHead = mkHead(sessionId, "before-gap", 3);
    const recoveredHead = mkHead(sessionId, "missing-seed-repair", 7);
    const getSessionHead = vi
      .fn(async () => recoveredHead)
      .mockResolvedValueOnce(initialHead)
      .mockResolvedValueOnce(recoveredHead);
    const freshnessEvents: SessionReplicaFreshnessEvent[] = [];
    const core = new SessionReplicaCore({
      api: { getSessionHead },
      emit: () => {},
      emitFreshness: (event) => freshnessEvents.push(event),
    });

    core.handleCommand({ type: "init", config: { eventBufferLimit: 100, headLimit: 50 } });
    core.handleCommand({ type: "hydrate_session_head", sessionId });
    await waitForCondition(() => getSessionHead.mock.calls.length === 1);

    core.handleCommand({
      type: "workspace_event",
      event: {
        type: "session_gap",
        workspace_id: "ws-1",
        snapshot_rev: 2,
        session_id: sessionId,
        after_seq: 6,
        reason: "replay_limit_exceeded",
        seed_follows: true,
      },
    });

    await new Promise((resolve) => setTimeout(resolve, 150));
    await waitForCondition(() => getSessionHead.mock.calls.length === 2);
    await waitForCondition(() =>
      freshnessEvents.some((event) => event.type === "gap_recovery_finished" && event.sessionId === sessionId),
    );
    expect(freshnessEvents).not.toEqual(
      expect.arrayContaining([
        expect.objectContaining({ type: "gap_repair_mismatch", sessionId }),
      ]),
    );
  });

  it("repairs session_gap with compact active tail instead of full event head", async () => {
    const sessionId = "session-gap-compact-tail";
    const initialHead = mkHead(sessionId, "before-gap", 100);
    const repairedHead = {
      ...mkHead(sessionId, "tail-repair", 120),
      has_more_turns: true,
      head_window: {
        turn_limit: 5,
        message_limit: 200,
        event_limit: 0,
        byte_limit: 256_000,
        turn_count: 5,
        message_count: 10,
        event_count: 0,
        bytes: 4096,
        truncated: true,
      },
    } satisfies SessionHeadSnapshot;
    const getSessionHead = vi
      .fn(async () => initialHead)
      .mockResolvedValueOnce(initialHead)
      .mockResolvedValueOnce(repairedHead);
    const core = new SessionReplicaCore({
      api: { getSessionHead },
      emit: () => {},
    });

    core.handleCommand({ type: "init", config: { eventBufferLimit: 100, headLimit: 60 } });
    core.handleCommand({ type: "hydrate_session_head", sessionId });
    await waitForCondition(() => getSessionHead.mock.calls.length === 1);
    const initialResult = getSessionHead.mock.results[0];
    if (initialResult?.type === "return") {
      await initialResult.value;
    }
    await new Promise((resolve) => setTimeout(resolve, 0));

    core.handleCommand({
      type: "workspace_event",
      event: {
        type: "session_gap",
        workspace_id: "ws-1",
        snapshot_rev: 2,
        session_id: sessionId,
        after_seq: 119,
      },
    });

    await waitForCondition(() => getSessionHead.mock.calls.length >= 2);
    expect(getSessionHead.mock.calls[0]).toEqual([sessionId, 60, true]);
    expect(getSessionHead.mock.calls[1]).toEqual([sessionId, 5, false, { minEventSeq: 119 }]);
  });

  it("repairs bounded /head hydrates instead of dropping the visible transcript tail", async () => {
    const sessionId = "session-head-bounded-hydrate-repair";
    const olderAt = "2026-03-09T00:00:01.000Z";
    const newerAt = "2026-03-09T00:00:02.000Z";
    const latestAt = "2026-03-09T00:00:03.000Z";
    const fullHead: SessionHeadSnapshot = {
      session: mkSession(sessionId),
      turns: [
        mkCompletedTurn(sessionId, "turn-1", 1, olderAt),
        mkCompletedTurn(sessionId, "turn-2", 3, newerAt),
      ],
      events: [] as SessionEvent[],
      messages: [
        mkMessageForTurn(sessionId, "m-1", "turn-1", "older", olderAt),
        mkMessageForTurn(sessionId, "m-2", "turn-2", "newer", newerAt),
      ],
      last_event_seq: 10,
      projection_rev: 10,
      state_rev: 10,
      has_more_turns: false,
      has_more_history: false,
      history_cursor: null,
      head_window: {
        turn_limit: 0,
        message_limit: 0,
        event_limit: 0,
        byte_limit: 0,
        turn_count: 2,
        message_count: 2,
        event_count: 0,
        bytes: 512,
        truncated: false,
      },
    };
    const shiftedHead: SessionHeadSnapshot = {
      ...fullHead,
      turns: [
        fullHead.turns[1]!,
        mkCompletedTurn(sessionId, "turn-3", 5, latestAt),
      ],
      messages: [
        fullHead.messages[1]!,
        mkMessageForTurn(sessionId, "m-3", "turn-3", "latest", latestAt),
      ],
      last_event_seq: 20,
      projection_rev: 20,
      state_rev: 20,
      has_more_turns: true,
      head_window: {
        turn_limit: 2,
        message_limit: 2,
        event_limit: 0,
        byte_limit: 4096,
        turn_count: 2,
        message_count: 2,
        event_count: 0,
        bytes: 512,
        truncated: true,
      },
    };
    const getSessionHead = vi.fn(async () => shiftedHead);
    const patches: SessionReplicaPatch[] = [];
    const core = new SessionReplicaCore({
      api: { getSessionHead },
      emit: (next) => patches.push(...next),
    });

    core.handleCommand({ type: "init", config: { eventBufferLimit: 100, headLimit: 50 } });
    core.handleCommand({ type: "seed_head", sessionId, head: fullHead, mode: "repair_replace" });
    core.handleCommand({ type: "hydrate_session_head", sessionId, force: true });

    await waitForCondition(() =>
      patches.some(
        (patch) =>
          patch.sessionId === sessionId &&
          patch.op === "replace" &&
          patch.data.lastEventSeq === 20,
      ),
    );

    const lastPatch = [...patches]
      .reverse()
      .find((patch) => patch.sessionId === sessionId && patch.op === "replace");
    if (!lastPatch || lastPatch.op === "evict") {
      throw new Error("expected replace patch");
    }
    expect(getSessionHead).toHaveBeenCalledWith(sessionId, 50, true);
    expect(lastPatch.data.replaceMode).toBe("repair_replace");
    expect(lastPatch.data.messages?.map((message) => message.id)).toEqual(["m-1", "m-2", "m-3"]);
    expect(lastPatch.data.turns?.map((turn) => turn.turn_id)).toEqual(["turn-1", "turn-2", "turn-3"]);
  });

  it("coalesces repeated session_gap repairs while the first compact repair is in flight", async () => {
    const sessionId = "session-gap-coalesce";
    const initialHead = mkHead(sessionId, "before-gap", 100);
    let resolveRepairHead: (value: SessionHeadSnapshot) => void = () => {
      throw new Error("repair resolver was not initialized");
    };
    const getSessionHead = vi
      .fn(async () => initialHead)
      .mockResolvedValueOnce(initialHead)
      .mockImplementationOnce(
        () =>
          new Promise<SessionHeadSnapshot>((resolve) => {
            resolveRepairHead = resolve;
          }),
      );
    const core = new SessionReplicaCore({
      api: { getSessionHead },
      emit: () => {},
    });

    core.handleCommand({ type: "init", config: { eventBufferLimit: 100, headLimit: 60 } });
    core.handleCommand({ type: "hydrate_session_head", sessionId });
    await waitForCondition(() => getSessionHead.mock.calls.length === 1);
    const initialResult = getSessionHead.mock.results[0];
    if (initialResult?.type === "return") {
      await initialResult.value;
    }
    await new Promise((resolve) => setTimeout(resolve, 0));

    for (let index = 0; index < 5; index += 1) {
      core.handleCommand({
        type: "workspace_event",
        event: {
          type: "session_gap",
          workspace_id: "ws-1",
          snapshot_rev: 2 + index,
          session_id: sessionId,
          after_seq: 119 + index,
        },
      });
    }

    await waitForCondition(() => getSessionHead.mock.calls.length >= 2);
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(getSessionHead).toHaveBeenCalledTimes(2);

    resolveRepairHead(mkHead(sessionId, "tail-repair", 124));
    await waitForCondition(() => getSessionHead.mock.results[1]?.type === "return");
  });

  it("queues a follow-up compact repair when a later session_gap arrives during repair", async () => {
    const sessionId = "session-gap-coalesce-pending";
    const initialHead = mkHead(sessionId, "before-gap", 100);
    let resolveRepairHead: (value: SessionHeadSnapshot) => void = () => {
      throw new Error("repair resolver was not initialized");
    };
    const getSessionHead = vi
      .fn(async () => initialHead)
      .mockResolvedValueOnce(initialHead)
      .mockImplementationOnce(
        () =>
          new Promise<SessionHeadSnapshot>((resolve) => {
            resolveRepairHead = resolve;
          }),
      )
      .mockResolvedValueOnce(mkHead(sessionId, "tail-repair-follow-up", 124));
    const freshnessEvents: SessionReplicaFreshnessEvent[] = [];
    const core = new SessionReplicaCore({
      api: { getSessionHead },
      emit: () => {},
      emitFreshness: (event) => freshnessEvents.push(event),
    });

    core.handleCommand({ type: "init", config: { eventBufferLimit: 100, headLimit: 60 } });
    core.handleCommand({ type: "hydrate_session_head", sessionId });
    await waitForCondition(() => getSessionHead.mock.calls.length === 1);
    const initialResult = getSessionHead.mock.results[0];
    if (initialResult?.type === "return") {
      await initialResult.value;
    }
    await new Promise((resolve) => setTimeout(resolve, 0));

    core.handleCommand({
      type: "workspace_event",
      event: {
        type: "session_gap",
        workspace_id: "ws-1",
        snapshot_rev: 2,
        session_id: sessionId,
        after_seq: 119,
      },
    });
    await waitForCondition(() => getSessionHead.mock.calls.length === 2);

    core.handleCommand({
      type: "workspace_event",
      event: {
        type: "session_gap",
        workspace_id: "ws-1",
        snapshot_rev: 3,
        session_id: sessionId,
        after_seq: 124,
      },
    });
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(getSessionHead).toHaveBeenCalledTimes(2);

    resolveRepairHead(mkHead(sessionId, "tail-repair-before-latest-gap", 119));
    await waitForCondition(() => getSessionHead.mock.calls.length === 3);

    expect(getSessionHead.mock.calls[1]).toEqual([sessionId, 5, false, { minEventSeq: 119 }]);
    expect(getSessionHead.mock.calls[2]).toEqual([sessionId, 5, false, { minEventSeq: 124 }]);
    await waitForCondition(() =>
      freshnessEvents.some((event) => event.type === "gap_recovery_finished" && event.sessionId === sessionId),
    );
  });

  it("recovers from session_gap via authoritative /head rehydrate and resumed deltas", async () => {
    const sessionId = "session-gap-recovery";
    const initialHead = mkHead(sessionId, "before-gap");
    const recoveredHead = {
      ...mkHead(sessionId, "recovered-from-head"),
      last_event_seq: 2,
      state_rev: 2,
    };
    const getSessionHead = vi
      .fn(async () => initialHead)
      .mockResolvedValueOnce(initialHead)
      .mockResolvedValueOnce(recoveredHead);
    const patches: SessionReplicaPatch[] = [];
    const freshnessEvents: SessionReplicaFreshnessEvent[] = [];
    const core = new SessionReplicaCore({
      api: { getSessionHead },
      emit: (next) => patches.push(...next),
      emitFreshness: (event) => freshnessEvents.push(event),
    });

    core.handleCommand({ type: "init", config: { eventBufferLimit: 100, headLimit: 50 } });
    core.handleCommand({ type: "open_session", sessionId });
    core.handleCommand({ type: "hydrate_session_head", sessionId });

    await waitForCondition(() => getSessionHead.mock.calls.length === 1);

    core.handleCommand({
      type: "workspace_event",
      event: {
        type: "session_gap",
        workspace_id: "ws-1",
        snapshot_rev: 2,
        session_id: sessionId,
        after_seq: 1,
      },
    });

    await waitForCondition(() => getSessionHead.mock.calls.length === 2);

    const now = new Date().toISOString();
    const deltaMessage: Message = {
      id: "m-delta",
      session_id: sessionId,
      task_id: "task-1",
      turn_id: "turn-delta",
      role: "assistant",
      content: "recovered-from-delta",
      delivery: "immediate",
      created_at: now,
    };
    const deltaEvent: SessionEvent = {
      seq: 3,
      id: "e-delta",
      session_id: sessionId,
      turn_id: "turn-delta",
      event_type: "assistant_message_inserted",
      payload_json: {
        message_id: deltaMessage.id,
        content: deltaMessage.content,
        delivery: deltaMessage.delivery,
      },
      created_at: now,
    };
    core.handleCommand({
      type: "workspace_event",
      event: {
        type: "session_head_delta",
        workspace_id: "ws-1",
        snapshot_rev: 3,
        delta: {
          session_id: sessionId,
          last_event_seq: 3,
          state_rev: 3,
          event: deltaEvent,
          message: deltaMessage,
        },
      },
    });

    await waitForCondition(() => {
      const headPatch = patches.find(
        (patch) =>
          patch.sessionId === sessionId &&
          patch.op !== "evict" &&
          patch.data?.messages?.some((message) => message.content === "recovered-from-head") &&
          patch.data?.lastEventSeq === 2,
      );
      const deltaPatch = patches.find(
        (patch) =>
          patch.sessionId === sessionId &&
          patch.op === "append" &&
          patch.data?.messages?.some((message) => message.content === "recovered-from-delta") &&
          patch.data?.lastEventSeq === 3,
      );
      return Boolean(headPatch && deltaPatch);
    });

    expect(getSessionHead).toHaveBeenCalledTimes(2);
    expect(freshnessEvents).toEqual(
      expect.arrayContaining([
        {
          type: "gap_recovery_started",
          sessionId,
          reason: null,
        },
        {
          type: "gap_recovery_finished",
          sessionId,
        },
        {
          type: "final_delta_received",
          sessionId,
          turnId: "turn-delta",
          emittedAtMs: null,
          lastEventSeq: 3,
        },
      ]),
    );
  });

  it("keeps session_gap recovery open when /head does not cover the gap cursor", async () => {
    const sessionId = "session-gap-stale-head";
    const initialHead = mkHead(sessionId, "before-gap", 3);
    const staleHead = mkHead(sessionId, "stale-repair-head", 3);
    const recoveredHead = mkHead(sessionId, "fresh-repair-head", 6);
    const getSessionHead = vi
      .fn(async () => initialHead)
      .mockResolvedValueOnce(initialHead)
      .mockResolvedValueOnce(staleHead)
      .mockResolvedValueOnce(recoveredHead);
    const patches: SessionReplicaPatch[] = [];
    const freshnessEvents: SessionReplicaFreshnessEvent[] = [];
    const core = new SessionReplicaCore({
      api: { getSessionHead },
      emit: (next) => patches.push(...next),
      emitFreshness: (event) => freshnessEvents.push(event),
    });

    core.handleCommand({ type: "init", config: { eventBufferLimit: 100, headLimit: 50 } });
    core.handleCommand({ type: "hydrate_session_head", sessionId });

    await waitForCondition(() => getSessionHead.mock.calls.length === 1);

    core.handleCommand({
      type: "workspace_event",
      event: {
        type: "session_gap",
        workspace_id: "ws-1",
        snapshot_rev: 2,
        session_id: sessionId,
        after_seq: 5,
      },
    });

    await waitForCondition(() =>
      freshnessEvents.some(
        (event) =>
          event.type === "gap_repair_mismatch" &&
          event.sessionId === sessionId &&
          event.baselineLastEventSeq === 5 &&
          event.repairedLastEventSeq === 3,
      ),
    );

    expect(freshnessEvents).not.toEqual(
      expect.arrayContaining([{ type: "gap_recovery_finished", sessionId }]),
    );
    const staleRepairPatch = [...patches].reverse().find(
      (patch) =>
        patch.sessionId === sessionId &&
        patch.op === "replace" &&
        patch.data.messages?.some((message) => message.content === "stale-repair-head"),
    );
    if (!staleRepairPatch || staleRepairPatch.op === "evict") {
      throw new Error("expected stale repair patch");
    }
    expect(staleRepairPatch.data.freshness).toBe("recovering");

    core.handleCommand({ type: "hydrate_session_head", sessionId, force: true });

    await waitForCondition(() =>
      freshnessEvents.some((event) => event.type === "gap_recovery_finished" && event.sessionId === sessionId),
    );
    const recoveredRepairPatch = [...patches].reverse().find(
      (patch) =>
        patch.sessionId === sessionId &&
        patch.op === "replace" &&
        patch.data.messages?.some((message) => message.content === "fresh-repair-head"),
    );
    if (!recoveredRepairPatch || recoveredRepairPatch.op === "evict") {
      throw new Error("expected fresh repair patch");
    }
    expect(recoveredRepairPatch.data.freshness).toBe("authoritative");
    expect(recoveredRepairPatch.data.lastEventSeq).toBe(6);
  });

  it("clears session_gap recovery when local live state already covers a stale repair head", async () => {
    const sessionId = "session-gap-local-live-covers-stale-repair";
    const initialHead = mkHead(sessionId, "live-before-gap", 10);
    const staleHead = mkHead(sessionId, "stale-repair-head", 8);
    const getSessionHead = vi
      .fn(async () => initialHead)
      .mockResolvedValueOnce(initialHead)
      .mockResolvedValueOnce(staleHead);
    const patches: SessionReplicaPatch[] = [];
    const freshnessEvents: SessionReplicaFreshnessEvent[] = [];
    const core = new SessionReplicaCore({
      api: { getSessionHead },
      emit: (next) => patches.push(...next),
      emitFreshness: (event) => freshnessEvents.push(event),
    });

    core.handleCommand({ type: "init", config: { eventBufferLimit: 100, headLimit: 50 } });
    core.handleCommand({ type: "hydrate_session_head", sessionId });
    await waitForCondition(() => getSessionHead.mock.calls.length === 1);

    core.handleCommand({
      type: "workspace_event",
      event: {
        type: "session_gap",
        workspace_id: "ws-1",
        snapshot_rev: 2,
        session_id: sessionId,
        after_seq: 8,
      },
    });

    await waitForCondition(() =>
      freshnessEvents.some((event) => event.type === "gap_recovery_finished" && event.sessionId === sessionId),
    );

    expect(getSessionHead).toHaveBeenLastCalledWith(sessionId, 5, false, { minEventSeq: 10 });
    expect(freshnessEvents).not.toEqual(
      expect.arrayContaining([
        expect.objectContaining({ type: "gap_repair_mismatch", sessionId }),
      ]),
    );
    const repairPatch = [...patches].reverse().find(
      (patch) =>
        patch.sessionId === sessionId &&
        patch.op === "replace" &&
        patch.data.messages?.some((message) => message.content === "live-before-gap"),
    );
    if (!repairPatch || repairPatch.op === "evict") {
      throw new Error("expected merged repair patch");
    }
    expect(repairPatch.data.freshness).toBe("authoritative");
    expect(repairPatch.data.lastEventSeq).toBe(10);
  });

  it("does not clear session_gap recovery from cached bootstrap head data", async () => {
    const sessionId = "session-gap-bootstrap-cache";
    const initialHead = mkHead(sessionId, "before-gap", 3);
    const cachedHead = mkHead(sessionId, "cached-bootstrap-after-gap", 6);
    let repairHeadRequested = false;
    const getSessionHead = vi
      .fn(async () => initialHead)
      .mockResolvedValueOnce(initialHead)
      .mockImplementationOnce(
        () =>
          new Promise<SessionHeadSnapshot>(() => {
            repairHeadRequested = true;
          }),
      );
    loadSessionHeadV1Mock.mockResolvedValueOnce({
      v: 1,
      sessionId,
      updatedAtMs: Date.now(),
      head: cachedHead,
    });
    const patches: SessionReplicaPatch[] = [];
    const freshnessEvents: SessionReplicaFreshnessEvent[] = [];
    const core = new SessionReplicaCore({
      api: { getSessionHead },
      emit: (next) => patches.push(...next),
      emitFreshness: (event) => freshnessEvents.push(event),
    });

    core.handleCommand({ type: "init", config: { eventBufferLimit: 100, headLimit: 50 } });
    core.handleCommand({ type: "hydrate_session_head", sessionId });
    await waitForCondition(() => getSessionHead.mock.calls.length === 1);

    core.handleCommand({
      type: "workspace_event",
      event: {
        type: "session_gap",
        workspace_id: "ws-1",
        snapshot_rev: 2,
        session_id: sessionId,
        after_seq: 5,
      },
    });
    await waitForCondition(() => repairHeadRequested);

    core.handleCommand({ type: "open_session", sessionId, force: true });
    await waitForCondition(() =>
      patches.some(
        (patch) =>
          patch.sessionId === sessionId &&
          patch.op === "replace" &&
          patch.data.messages?.some((message) => message.content === "cached-bootstrap-after-gap"),
      ),
    );

    expect(freshnessEvents).not.toEqual(
      expect.arrayContaining([{ type: "gap_recovery_finished", sessionId }]),
    );
    expect(freshnessEvents).not.toEqual(
      expect.arrayContaining([
        expect.objectContaining({ type: "gap_repair_mismatch", sessionId }),
      ]),
    );
    const cachedPatch = [...patches].reverse().find(
      (patch) =>
        patch.sessionId === sessionId &&
        patch.op === "replace" &&
        patch.data.messages?.some((message) => message.content === "cached-bootstrap-after-gap"),
    );
    if (!cachedPatch || cachedPatch.op === "evict") {
      throw new Error("expected cached bootstrap patch");
    }
    expect(cachedPatch.data.freshness).toBe("recovering");
  });

  it("keeps session_gap recovery open until the authoritative /head repair resolves", async () => {
    const sessionId = "session-gap-delta-before-head";
    const initialHead = mkHead(sessionId, "before-gap");
    const recoveredHead = {
      ...mkHead(sessionId, "recovered-from-head"),
      last_event_seq: 4,
      state_rev: 4,
    };
    let resolveRepairHead: (value: SessionHeadSnapshot) => void = () => {
      throw new Error("pending repair head resolver was not initialized");
    };
    const getSessionHead = vi
      .fn(async () => initialHead)
      .mockResolvedValueOnce(initialHead)
      .mockImplementationOnce(
        () =>
          new Promise<SessionHeadSnapshot>((resolve) => {
            resolveRepairHead = resolve;
          }),
      );
    const patches: SessionReplicaPatch[] = [];
    const freshnessEvents: SessionReplicaFreshnessEvent[] = [];
    const core = new SessionReplicaCore({
      api: { getSessionHead },
      emit: (next) => patches.push(...next),
      emitFreshness: (event) => freshnessEvents.push(event),
    });

    core.handleCommand({ type: "init", config: { eventBufferLimit: 100, headLimit: 50 } });
    core.handleCommand({ type: "open_session", sessionId });
    core.handleCommand({ type: "hydrate_session_head", sessionId });

    await waitForCondition(() => getSessionHead.mock.calls.length === 1);

    core.handleCommand({
      type: "workspace_event",
      event: {
        type: "session_gap",
        workspace_id: "ws-1",
        snapshot_rev: 2,
        session_id: sessionId,
        after_seq: 2,
      },
    });

    await waitForCondition(() => getSessionHead.mock.calls.length === 2);

    const now = new Date().toISOString();
    const deltaMessage: Message = {
      id: "m-delta-before-head",
      session_id: sessionId,
      task_id: "task-1",
      turn_id: "turn-delta-before-head",
      role: "assistant",
      content: "delta-before-head",
      delivery: "immediate",
      created_at: now,
    };
    core.handleCommand({
      type: "workspace_event",
      event: {
        type: "session_head_delta",
        workspace_id: "ws-1",
        snapshot_rev: 3,
        delta: {
          session_id: sessionId,
          last_event_seq: 3,
          state_rev: 3,
          message: deltaMessage,
        },
      },
    });

    await waitForCondition(() =>
      patches.some(
        (patch) =>
          patch.sessionId === sessionId &&
          patch.op === "append" &&
          patch.data?.appendMode === "stream_delta" &&
          patch.data?.messages?.some((message) => message.content === "delta-before-head"),
      ),
    );
    const deltaPatch = [...patches].reverse().find(
      (patch) =>
        patch.sessionId === sessionId &&
        patch.op === "append" &&
        patch.data?.appendMode === "stream_delta",
    );
    if (!deltaPatch || deltaPatch.op === "evict") {
      throw new Error("expected stream delta patch");
    }
    expect(deltaPatch.data.freshness).toBe("recovering");
    expect(freshnessEvents).not.toEqual(
      expect.arrayContaining([{ type: "gap_recovery_finished", sessionId }]),
    );

    resolveRepairHead(recoveredHead);

    await waitForCondition(() =>
      freshnessEvents.some((event) => event.type === "gap_recovery_finished" && event.sessionId === sessionId),
    );

    const repairPatch = [...patches].reverse().find(
      (patch) =>
        patch.sessionId === sessionId &&
        patch.op === "replace" &&
        patch.data.freshness === "authoritative",
    );
    if (!repairPatch || repairPatch.op === "evict") {
      throw new Error("expected authoritative repair patch");
    }
    expect(repairPatch.data.lastEventSeq).toBe(4);
    expect(repairPatch.data.messages?.map((message: Message) => message.content)).toEqual(
      expect.arrayContaining(["recovered-from-head"]),
    );
  });

  it("hydrates /head only when explicitly requested", async () => {
    const head = mkHead("session-archived");
    const getSessionHead = vi.fn(async () => head);
    const patches: SessionReplicaPatch[] = [];
    const core = new SessionReplicaCore({
      api: { getSessionHead },
      emit: (next) => patches.push(...next),
    });
    core.handleCommand({ type: "init", config: { eventBufferLimit: 100, headLimit: 50 } });

    core.handleCommand({ type: "open_session", sessionId: "session-archived" });
    await waitForCondition(() => patches.length > 0);
    expect(getSessionHead).not.toHaveBeenCalled();

    core.handleCommand({ type: "hydrate_session_head", sessionId: "session-archived" });
    await waitForCondition(() => getSessionHead.mock.calls.length === 1);
    expect(getSessionHead).toHaveBeenCalledTimes(1);
  });

  it("refetches /head on explicit refresh even after authoritative hydration", async () => {
    const sessionId = "session-refresh";
    const initialHead = mkHead(sessionId, "initial");
    const refreshedHead = {
      ...mkHead(sessionId, "refreshed"),
      last_event_seq: 2,
      state_rev: 2,
    };
    const getSessionHead = vi
      .fn(async () => initialHead)
      .mockResolvedValueOnce(initialHead)
      .mockResolvedValueOnce(refreshedHead);
    const patches: SessionReplicaPatch[] = [];
    const core = new SessionReplicaCore({
      api: { getSessionHead },
      emit: (next) => patches.push(...next),
    });

    core.handleCommand({ type: "init", config: { eventBufferLimit: 100, headLimit: 50 } });
    core.handleCommand({ type: "hydrate_session_head", sessionId });
    await waitForCondition(() => getSessionHead.mock.calls.length === 1);

    core.handleCommand({ type: "refresh_session", sessionId });
    await waitForCondition(() => getSessionHead.mock.calls.length === 2);

    const latest = [...patches].reverse().find(
      (patch: SessionReplicaPatch) =>
        patch.sessionId === sessionId
        && patch.op !== "evict"
        && patch.data.messages?.some((message) => message.content === "refreshed"),
    );
    if (!latest || latest.op === "evict") {
      throw new Error("expected refreshed authoritative patch");
    }

    expect(getSessionHead).toHaveBeenCalledTimes(2);
    expect(latest.data.freshness).toBe("authoritative");
    expect(latest.data.lastEventSeq).toBe(2);
  });

  it("applies session_head_seed events", async () => {
    const getSessionHead = vi.fn();
    const patches: SessionReplicaPatch[] = [];
    const core = new SessionReplicaCore({
      api: { getSessionHead },
      emit: (next) => patches.push(...next),
    });
    core.handleCommand({ type: "init", config: { eventBufferLimit: 100, headLimit: 50 } });

    const seedHead = mkHead("session-seed", "from-seed");
    const seedEvent: WorkspaceActiveSnapshotEvent = {
      type: "session_head_seed",
      workspace_id: "ws-1",
      snapshot_rev: 1,
      head: seedHead,
    };
    core.handleCommand({ type: "workspace_event", event: seedEvent });

    await waitForCondition(() => patches.some((patch) => patch.sessionId === "session-seed"));
    const lastPatch = patches.filter((patch) => patch.sessionId === "session-seed").slice(-1)[0];
    if (lastPatch?.op === "evict") {
      throw new Error("expected append/replace patch");
    }
    expect(lastPatch?.data?.messages?.[0]?.content).toBe("from-seed");
  });

  it("emits repair_replace for bounded session_head_seed events", async () => {
    const getSessionHead = vi.fn();
    const patches: SessionReplicaPatch[] = [];
    const core = new SessionReplicaCore({
      api: { getSessionHead },
      emit: (next) => patches.push(...next),
    });
    core.handleCommand({ type: "init", config: { eventBufferLimit: 100, headLimit: 50 } });

    const seedHead = {
      ...mkHead("session-seed-bounded", "from-bounded-seed"),
      head_window: {
        turn_limit: 0,
        message_limit: 50,
        event_limit: 800,
        byte_limit: 200_000,
        turn_count: 1,
        message_count: 1,
        event_count: 0,
        bytes: 256,
        truncated: true,
      },
    };
    const seedEvent: WorkspaceActiveSnapshotEvent = {
      type: "session_head_seed",
      workspace_id: "ws-1",
      snapshot_rev: 1,
      head: seedHead,
    };
    core.handleCommand({ type: "workspace_event", event: seedEvent });

    await waitForCondition(() => patches.some((patch) => patch.sessionId === "session-seed-bounded"));
    const lastPatch = patches.filter((patch) => patch.sessionId === "session-seed-bounded").slice(-1)[0];
    if (lastPatch?.op === "evict") {
      throw new Error("expected append/replace patch");
    }
    expect(lastPatch?.data?.replaceMode).toBe("repair_replace");
    expect(lastPatch?.data?.messages?.[0]?.content).toBe("from-bounded-seed");
  });

  it("repairs narrower unbounded session_head_seed events instead of dropping loaded history", async () => {
    const sessionId = "session-seed-covered-history";
    const patches: SessionReplicaPatch[] = [];
    const core = new SessionReplicaCore({
      api: { getSessionHead: vi.fn() },
      emit: (next) => patches.push(...next),
    });
    core.handleCommand({ type: "init", config: { eventBufferLimit: 100, headLimit: 50 } });

    const fullHead: SessionHeadSnapshot = {
      session: mkSession(sessionId),
      turns: [
        {
          turn_id: "turn-1",
          session_id: sessionId,
          status: "completed",
          start_seq: 1,
          started_at: "2026-03-09T00:00:01.000Z",
          updated_at: "2026-03-09T00:00:01.000Z",
          tool_total: 0,
          tool_pending: 0,
          tool_running: 0,
          tool_completed: 0,
          tool_failed: 0,
        },
        {
          turn_id: "turn-2",
          session_id: sessionId,
          status: "completed",
          start_seq: 3,
          started_at: "2026-03-09T00:00:02.000Z",
          updated_at: "2026-03-09T00:00:02.000Z",
          tool_total: 0,
          tool_pending: 0,
          tool_running: 0,
          tool_completed: 0,
          tool_failed: 0,
        },
      ],
      events: [] as SessionEvent[],
      messages: [
        {
          id: "m-1",
          session_id: sessionId,
          task_id: "task-1",
          turn_id: "turn-1",
          role: "assistant",
          content: "older",
          delivery: "immediate",
          created_at: "2026-03-09T00:00:01.000Z",
        },
        {
          id: "m-2",
          session_id: sessionId,
          task_id: "task-1",
          turn_id: "turn-2",
          role: "assistant",
          content: "newer",
          delivery: "immediate",
          created_at: "2026-03-09T00:00:02.000Z",
        },
      ],
      last_event_seq: 10,
      projection_rev: 7,
      state_rev: 7,
      has_more_turns: false,
      has_more_history: false,
      history_cursor: null,
      head_window: {
        turn_limit: 0,
        message_limit: 0,
        event_limit: 0,
        byte_limit: 0,
        turn_count: 2,
        message_count: 2,
        event_count: 0,
        bytes: 512,
        truncated: false,
      },
    };
    core.handleCommand({ type: "seed_head", sessionId, head: fullHead, mode: "repair_replace" });

    const compactHead: SessionHeadSnapshot = {
      ...fullHead,
      turns: [fullHead.turns[1]!],
      messages: [fullHead.messages[1]!],
      last_event_seq: 20,
      projection_rev: 9,
    };
    core.handleCommand({
      type: "workspace_event",
      event: {
        type: "session_head_seed",
        workspace_id: "ws-1",
        snapshot_rev: 2,
        head: compactHead,
      },
    });

    await waitForCondition(() =>
      patches.some(
        (patch) =>
          patch.sessionId === sessionId &&
          patch.op === "replace" &&
          patch.data.lastEventSeq === 20,
      ),
    );

    const lastPatch = [...patches]
      .reverse()
      .find((patch) => patch.sessionId === sessionId && patch.op === "replace");
    if (!lastPatch || lastPatch.op === "evict") {
      throw new Error("expected replace patch");
    }
    expect(lastPatch.data.replaceMode).toBe("repair_replace");
    expect(lastPatch.data.messages?.map((message) => message.id)).toEqual(["m-1", "m-2"]);
    expect(lastPatch.data.turns?.map((turn) => turn.turn_id)).toEqual(["turn-1", "turn-2"]);
  });

  it("repairs shifted bounded session_head_seed windows instead of dropping loaded history", async () => {
    const sessionId = "session-seed-shifted-history";
    const patches: SessionReplicaPatch[] = [];
    const core = new SessionReplicaCore({
      api: { getSessionHead: vi.fn() },
      emit: (next) => patches.push(...next),
    });
    core.handleCommand({ type: "init", config: { eventBufferLimit: 100, headLimit: 50 } });

    const fullHead: SessionHeadSnapshot = {
      session: mkSession(sessionId),
      turns: [
        {
          turn_id: "turn-1",
          session_id: sessionId,
          status: "completed",
          start_seq: 1,
          started_at: "2026-03-09T00:00:01.000Z",
          updated_at: "2026-03-09T00:00:01.000Z",
          tool_total: 0,
          tool_pending: 0,
          tool_running: 0,
          tool_completed: 0,
          tool_failed: 0,
        },
        {
          turn_id: "turn-2",
          session_id: sessionId,
          status: "completed",
          start_seq: 3,
          started_at: "2026-03-09T00:00:02.000Z",
          updated_at: "2026-03-09T00:00:02.000Z",
          tool_total: 0,
          tool_pending: 0,
          tool_running: 0,
          tool_completed: 0,
          tool_failed: 0,
        },
      ],
      events: [] as SessionEvent[],
      messages: [
        {
          id: "m-1",
          session_id: sessionId,
          task_id: "task-1",
          turn_id: "turn-1",
          role: "assistant",
          content: "older",
          delivery: "immediate",
          created_at: "2026-03-09T00:00:01.000Z",
        },
        {
          id: "m-2",
          session_id: sessionId,
          task_id: "task-1",
          turn_id: "turn-2",
          role: "assistant",
          content: "newer",
          delivery: "immediate",
          created_at: "2026-03-09T00:00:02.000Z",
        },
      ],
      last_event_seq: 10,
      projection_rev: 7,
      state_rev: 7,
      has_more_turns: false,
      has_more_history: false,
      history_cursor: null,
      head_window: {
        turn_limit: 0,
        message_limit: 0,
        event_limit: 0,
        byte_limit: 0,
        turn_count: 2,
        message_count: 2,
        event_count: 0,
        bytes: 512,
        truncated: false,
      },
    };
    core.handleCommand({ type: "seed_head", sessionId, head: fullHead, mode: "repair_replace" });

    const shiftedHead: SessionHeadSnapshot = {
      ...fullHead,
      turns: [
        fullHead.turns[1]!,
        {
          ...fullHead.turns[1]!,
          turn_id: "turn-3",
          start_seq: 5,
          started_at: "2026-03-09T00:00:03.000Z",
          updated_at: "2026-03-09T00:00:03.000Z",
        },
      ],
      messages: [
        fullHead.messages[1]!,
        {
          ...fullHead.messages[1]!,
          id: "m-3",
          turn_id: "turn-3",
          content: "latest",
          created_at: "2026-03-09T00:00:03.000Z",
        },
      ],
      last_event_seq: 20,
      projection_rev: 9,
      head_window: {
        turn_limit: 0,
        message_limit: 50,
        event_limit: 800,
        byte_limit: 200_000,
        turn_count: 2,
        message_count: 2,
        event_count: 0,
        bytes: 512,
        truncated: true,
      },
    };
    core.handleCommand({
      type: "workspace_event",
      event: {
        type: "session_head_seed",
        workspace_id: "ws-1",
        snapshot_rev: 2,
        head: shiftedHead,
      },
    });

    await waitForCondition(() =>
      patches.some(
        (patch) =>
          patch.sessionId === sessionId &&
          patch.op === "replace" &&
          patch.data.lastEventSeq === 20,
      ),
    );

    const lastPatch = [...patches]
      .reverse()
      .find((patch) => patch.sessionId === sessionId && patch.op === "replace");
    if (!lastPatch || lastPatch.op === "evict") {
      throw new Error("expected replace patch");
    }
    expect(lastPatch.data.replaceMode).toBe("repair_replace");
    expect(lastPatch.data.messages?.map((message) => message.id)).toEqual(["m-1", "m-2", "m-3"]);
    expect(lastPatch.data.turns?.map((turn) => turn.turn_id)).toEqual(["turn-1", "turn-2", "turn-3"]);
  });

  it("preserves history when a bounded session_head_seed window has no overlap", async () => {
    const sessionId = "session-seed-disjoint-history";
    const patches: SessionReplicaPatch[] = [];
    const core = new SessionReplicaCore({
      api: { getSessionHead: vi.fn() },
      emit: (next) => patches.push(...next),
    });
    core.handleCommand({ type: "init", config: { eventBufferLimit: 100, headLimit: 50 } });

    const oldHead = mkHead(sessionId, "older");
    oldHead.messages[0] = {
      ...oldHead.messages[0]!,
      id: "m-old",
      turn_id: "turn-old",
      created_at: "2026-03-09T00:00:01.000Z",
    };
    oldHead.turns = [
      {
        turn_id: "turn-old",
        session_id: sessionId,
        status: "completed",
        start_seq: 1,
        started_at: "2026-03-09T00:00:01.000Z",
        updated_at: "2026-03-09T00:00:01.000Z",
        tool_total: 0,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 0,
        tool_failed: 0,
      },
    ];
    oldHead.last_event_seq = 10;
    oldHead.projection_rev = 10;
    core.handleCommand({ type: "seed_head", sessionId, head: oldHead, mode: "repair_replace" });

    const compactHead: SessionHeadSnapshot = {
      ...mkHead(sessionId, "latest"),
      turns: [
        {
          turn_id: "turn-new",
          session_id: sessionId,
          status: "completed",
          start_seq: 20,
          started_at: "2026-03-09T00:00:20.000Z",
          updated_at: "2026-03-09T00:00:20.000Z",
          tool_total: 0,
          tool_pending: 0,
          tool_running: 0,
          tool_completed: 0,
          tool_failed: 0,
        },
      ],
      messages: [
        {
          id: "m-new",
          session_id: sessionId,
          task_id: "task-1",
          turn_id: "turn-new",
          role: "user",
          content: "latest",
          delivery: "immediate",
          created_at: "2026-03-09T00:00:20.000Z",
        },
      ],
      last_event_seq: 21,
      projection_rev: 21,
      head_window: {
        turn_limit: 1,
        message_limit: 1,
        event_limit: 40,
        byte_limit: 4096,
        turn_count: 1,
        message_count: 1,
        event_count: 0,
        bytes: 512,
        truncated: true,
      },
    };
    core.handleCommand({
      type: "workspace_event",
      event: {
        type: "session_head_seed",
        workspace_id: "ws-1",
        snapshot_rev: 2,
        head: compactHead,
      },
    });

    await waitForCondition(() =>
      patches.some(
        (patch) =>
          patch.sessionId === sessionId &&
          patch.op === "replace" &&
          patch.data.lastEventSeq === 21,
      ),
    );

    const lastPatch = [...patches]
      .reverse()
      .find((patch) => patch.sessionId === sessionId && patch.op === "replace");
    if (!lastPatch || lastPatch.op === "evict") {
      throw new Error("expected replace patch");
    }
    expect(lastPatch.data.replaceMode).toBe("repair_replace");
    expect(lastPatch.data.messages?.map((message) => message.id)).toEqual(["m-old", "m-new"]);
    expect(lastPatch.data.turns?.map((turn) => turn.turn_id)).toEqual(["turn-old", "turn-new"]);
  });

  it("preserves newer streamed state when an older /head hydrate resolves later", async () => {
    const sessionId = "session-stale-head";
    let resolveHead: (value: SessionHeadSnapshot | null) => void = () => {
      throw new Error("pending /head resolver was not initialized");
    };
    const getSessionHead = vi.fn(
      () =>
        new Promise<SessionHeadSnapshot | null>((resolve) => {
          resolveHead = resolve;
        }),
    );
    const patches: SessionReplicaPatch[] = [];
    const core = new SessionReplicaCore({
      api: { getSessionHead },
      emit: (next) => patches.push(...next),
    });

    core.handleCommand({ type: "init", config: { eventBufferLimit: 100, headLimit: 50 } });
    core.handleCommand({ type: "open_session", sessionId });
    core.handleCommand({ type: "hydrate_session_head", sessionId });

    await waitForCondition(() => getSessionHead.mock.calls.length === 1);

    const now = new Date().toISOString();
    const deltaMessage: Message = {
      id: "m-live-delta",
      session_id: sessionId,
      task_id: "task-1",
      turn_id: "turn-live",
      role: "assistant",
      content: "from-live-delta",
      delivery: "immediate",
      created_at: now,
    };
    core.handleCommand({
      type: "workspace_event",
      event: {
        type: "session_head_delta",
        workspace_id: "ws-1",
        snapshot_rev: 2,
        delta: {
          session_id: sessionId,
          last_event_seq: 2,
          state_rev: 2,
          message: deltaMessage,
        },
      },
    });

    resolveHead({
      ...mkHead(sessionId, "from-stale-head"),
      last_event_seq: 1,
      state_rev: 1,
    });

    await waitForCondition(() =>
      patches.some(
        (patch) =>
          patch.sessionId === sessionId &&
          patch.op === "replace" &&
          patch.data?.freshness === "authoritative",
      ),
    );

    const authoritativePatch = [...patches].reverse().find(
      (patch: SessionReplicaPatch) =>
        patch.sessionId === sessionId &&
        patch.op === "replace" &&
        patch.data.freshness === "authoritative",
    );
    if (!authoritativePatch || authoritativePatch.op === "evict") {
      throw new Error("expected authoritative replace patch");
    }

    expect(authoritativePatch.data.lastEventSeq).toBe(2);
    expect(authoritativePatch.data.stateRev).toBe(2);
    expect(authoritativePatch.data.freshness).toBe("authoritative");
    expect(authoritativePatch.data.messages?.map((message: Message) => message.content)).toEqual(
      expect.arrayContaining(["from-stale-head", "from-live-delta"]),
    );
  });

  it("ignores summary activity deltas and advances canonical activity from session_head_deltas only", () => {
    const sessionId = "session-activity";
    const patches: SessionReplicaPatch[] = [];
    const core = new SessionReplicaCore({
      api: { getSessionHead: vi.fn() },
      emit: (next) => patches.push(...next),
    });
    const completedActivity: SessionActivityState = {
      is_working: false,
      last_turn_status: "completed",
    };

    core.handleCommand({ type: "init", config: { eventBufferLimit: 100, headLimit: 50 } });
    core.handleCommand({
      type: "workspace_event",
      event: {
        type: "session_summary_delta",
        workspace_id: "ws-1",
        snapshot_rev: 2,
        delta: {
          session_id: sessionId,
          task_id: "task-1",
          activity: completedActivity,
          last_event_seq: 2,
          state_rev: 2,
        },
      },
    });

    expect(
      patches.some(
        (patch) =>
          patch.sessionId === sessionId &&
          patch.op === "append" &&
          patch.data?.activity?.last_turn_status === "completed",
      ),
    ).toBe(false);

    const now = new Date().toISOString();
    const deltaMessage: Message = {
      id: "m-post-summary",
      session_id: sessionId,
      task_id: "task-1",
      turn_id: "turn-post-summary",
      role: "assistant",
      content: "post-summary-delta",
      delivery: "immediate",
      created_at: now,
    };
    core.handleCommand({
      type: "workspace_event",
      event: {
        type: "session_head_delta",
        workspace_id: "ws-1",
        snapshot_rev: 3,
        delta: {
          session_id: sessionId,
          last_event_seq: 3,
          projection_rev: 3,
          state_rev: 3,
          activity: { is_working: true, last_turn_status: "running" },
          message: deltaMessage,
        },
      },
    });

    const latest = [...patches].reverse().find(
      (patch: SessionReplicaPatch) =>
        patch.sessionId === sessionId &&
        patch.op === "append" &&
        Array.isArray(patch.data.messages),
    );
    if (!latest || latest.op === "evict" || !latest.data.messages) {
      throw new Error("expected appended head-delta patch");
    }
    expect(latest.data.messages[0]?.content).toBe("post-summary-delta");
    expect(latest.data.activity).toEqual({ is_working: true, last_turn_status: "running" });
  });

  it("keeps canonical activity interrupted when later deltas report completed or failed", () => {
    const sessionId = "session-interrupt-activity-stable";
    const turnId = "turn-1";
    const createdAt = new Date().toISOString();
    const patches: SessionReplicaPatch[] = [];
    const core = new SessionReplicaCore({
      api: { getSessionHead: vi.fn() },
      emit: (next) => patches.push(...next),
    });

    core.handleCommand({ type: "init", config: { eventBufferLimit: 100, headLimit: 50 } });
    core.handleCommand({
      type: "seed_head",
      sessionId,
      head: {
        session: mkSession(sessionId),
        turns: [
          {
            turn_id: turnId,
            session_id: sessionId,
            run_id: "run-1",
            user_message_id: "message-1",
            status: "running",
            start_seq: 1,
            end_seq: null,
            started_at: createdAt,
            updated_at: createdAt,
            assistant_partial: "",
            thought_partial: "",
            metrics_json: null,
            tool_total: 0,
            tool_pending: 0,
            tool_running: 0,
            tool_completed: 0,
            tool_failed: 0,
          },
        ],
        events: [],
        messages: [],
        last_event_seq: 1,
        projection_rev: 1,
        state_rev: 1,
        activity: { is_working: true, last_turn_status: "running" },
        has_more_turns: false,
        has_more_history: false,
        history_cursor: null,
      },
      mode: "bootstrap_seed",
    });

    core.handleCommand({
      type: "workspace_event",
      event: {
        type: "session_head_delta",
        workspace_id: "ws-1",
        snapshot_rev: 2,
        delta: {
          session_id: sessionId,
          last_event_seq: 2,
          projection_rev: 2,
          state_rev: 2,
          activity: { is_working: false, last_turn_status: "interrupted" },
          event: {
            seq: 2,
            id: "event-2",
            session_id: sessionId,
            run_id: "run-1",
            turn_id: turnId,
            event_type: "turn_interrupted",
            payload_json: { status: "interrupted" },
            transient: false,
            created_at: createdAt,
          },
        },
      },
    });
    core.handleCommand({
      type: "workspace_event",
      event: {
        type: "session_head_delta",
        workspace_id: "ws-1",
        snapshot_rev: 3,
        delta: {
          session_id: sessionId,
          last_event_seq: 3,
          projection_rev: 3,
          state_rev: 3,
          activity: { is_working: false, last_turn_status: "completed" },
          event: {
            seq: 3,
            id: "event-3",
            session_id: sessionId,
            run_id: "run-1",
            turn_id: turnId,
            event_type: "turn_finished",
            payload_json: { status: "completed" },
            transient: false,
            created_at: createdAt,
          },
        },
      },
    });
    core.handleCommand({
      type: "workspace_event",
      event: {
        type: "session_head_delta",
        workspace_id: "ws-1",
        snapshot_rev: 4,
        delta: {
          session_id: sessionId,
          last_event_seq: 4,
          projection_rev: 4,
          state_rev: 4,
          activity: { is_working: false, last_turn_status: "failed" },
          event: {
            seq: 4,
            id: "event-4",
            session_id: sessionId,
            run_id: "run-1",
            turn_id: turnId,
            event_type: "turn_finished",
            payload_json: { status: "failed", message: "cancelled" },
            transient: false,
            created_at: createdAt,
          },
        },
      },
    });

    const latest = [...patches].reverse().find(
      (patch) =>
        patch.sessionId === sessionId &&
        patch.op === "append" &&
        patch.data.activity?.last_turn_status === "interrupted",
    );
    if (!latest || latest.op === "evict") {
      throw new Error("expected interrupted canonical patch");
    }
    expect(latest.data.activity).toEqual({ is_working: false, last_turn_status: "interrupted" });
    expect(latest.data.turns?.at(-1)?.status).toBe("interrupted");
  });

  it("does not clear interrupted activity when later head deltas omit activity", () => {
    const sessionId = "session-interrupt-activity-null-delta";
    const turnId = "turn-1";
    const createdAt = new Date().toISOString();
    const patches: SessionReplicaPatch[] = [];
    const core = new SessionReplicaCore({
      api: { getSessionHead: vi.fn() },
      emit: (next) => patches.push(...next),
    });

    core.handleCommand({ type: "init", config: { eventBufferLimit: 100, headLimit: 50 } });
    core.handleCommand({
      type: "seed_head",
      sessionId,
      head: {
        session: mkSession(sessionId),
        turns: [
          {
            turn_id: turnId,
            session_id: sessionId,
            run_id: "run-1",
            user_message_id: "message-1",
            status: "interrupted",
            start_seq: 1,
            end_seq: 2,
            started_at: createdAt,
            updated_at: createdAt,
            assistant_partial: "",
            thought_partial: "",
            metrics_json: null,
            tool_total: 0,
            tool_pending: 0,
            tool_running: 0,
            tool_completed: 0,
            tool_failed: 0,
          },
        ],
        events: [],
        messages: [],
        last_event_seq: 2,
        projection_rev: 2,
        state_rev: 2,
        activity: { is_working: false, last_turn_status: "interrupted" },
        has_more_turns: false,
        has_more_history: false,
        history_cursor: null,
      },
      mode: "bootstrap_seed",
    });

    core.handleCommand({
      type: "workspace_event",
      event: {
        type: "session_head_delta",
        workspace_id: "ws-1",
        snapshot_rev: 3,
        delta: {
          session_id: sessionId,
          last_event_seq: 3,
          projection_rev: 3,
          state_rev: 3,
          activity: null,
          event: {
            seq: 3,
            id: "event-3",
            session_id: sessionId,
            run_id: "run-1",
            turn_id: turnId,
            event_type: "assistant_message_inserted",
            payload_json: { message_id: "m-2", content: "still interrupted" },
            transient: false,
            created_at: createdAt,
          },
          message: {
            id: "m-2",
            session_id: sessionId,
            task_id: "task-1",
            turn_id: turnId,
            role: "assistant",
            content: "still interrupted",
            delivery: "immediate",
            created_at: createdAt,
          },
        },
      },
    });

    const latest = [...patches].reverse().find(
      (patch) =>
        patch.sessionId === sessionId &&
        patch.op === "append" &&
        patch.data.messages?.some((message) => message.id === "m-2"),
    );
    if (!latest || latest.op === "evict") {
      throw new Error("expected appended message patch");
    }
    expect(latest.data.activity).toEqual({ is_working: false, last_turn_status: "interrupted" });
    expect(latest.data.turns).toBeUndefined();
  });

  it("emits reduced transcript changes for streamed event-only deltas", () => {
    const sessionId = "session-canonical-stream-delta";
    const patches: SessionReplicaPatch[] = [];
    const core = new SessionReplicaCore({
      api: { getSessionHead: vi.fn() },
      emit: (next) => patches.push(...next),
    });
    const createdAt = new Date().toISOString();

    core.handleCommand({ type: "init", config: { eventBufferLimit: 100, headLimit: 50 } });
    core.handleCommand({
      type: "seed_head",
      sessionId,
      head: {
        session: mkSession(sessionId),
        turns: [
          {
            turn_id: "turn-1",
            session_id: sessionId,
            run_id: "run-1",
            user_message_id: "message-1",
            status: "running",
            start_seq: 1,
            end_seq: null,
            started_at: createdAt,
            updated_at: createdAt,
            assistant_partial: "",
            thought_partial: "",
            metrics_json: null,
            tool_total: 0,
            tool_pending: 0,
            tool_running: 0,
            tool_completed: 0,
            tool_failed: 0,
          },
        ],
        events: [],
        messages: [
          {
            id: "message-1",
            session_id: sessionId,
            task_id: "task-1",
            turn_id: "turn-1",
            role: "user",
            content: "queued later",
            delivery: "immediate",
            created_at: createdAt,
          },
        ],
        last_event_seq: 1,
        state_rev: 1,
        activity: { is_working: true, last_turn_status: "running" },
        has_more_turns: false,
        has_more_history: false,
        history_cursor: null,
      },
      mode: "bootstrap_seed",
    });

    core.handleCommand({
      type: "workspace_event",
      event: {
        type: "session_head_delta",
        workspace_id: "ws-1",
        snapshot_rev: 2,
        delta: {
          session_id: sessionId,
          last_event_seq: 2,
          projection_rev: 2,
          state_rev: 2,
          event: {
            seq: 2,
            id: "event-done",
            session_id: sessionId,
            run_id: "run-1",
            turn_id: "turn-1",
            event_type: "done",
            payload_json: {},
            created_at: createdAt,
          },
        },
      },
    });

    const latest = [...patches].reverse().find(
      (patch) => patch.sessionId === sessionId && patch.op === "append" && Array.isArray(patch.data.turns),
    );
    if (!latest || latest.op === "evict" || !latest.data.turns || !latest.data.events) {
      throw new Error("expected reduced append patch");
    }

    expect(latest.data.turnsRev).toBeTypeOf("number");
    expect(latest.data.eventsRev).toBeTypeOf("number");
    expect(latest.data.turns[0]?.status).toBe("completed");
    expect(latest.data.messages).toBeUndefined();
    expect(latest.data.events.map((event) => event.id)).toEqual(["event-done"]);
    expect(latest.data.lastEventSeq).toBe(2);
  });

  it("clears stale assistant streaming overlay when the assistant message delta arrives before the inserted event", () => {
    const sessionId = "session-assistant-stream-cleared-by-message";
    const patches: SessionReplicaPatch[] = [];
    const core = new SessionReplicaCore({
      api: { getSessionHead: vi.fn() },
      emit: (next) => patches.push(...next),
    });
    const createdAt = new Date().toISOString();

    core.handleCommand({ type: "init", config: { eventBufferLimit: 100, headLimit: 50 } });
    core.handleCommand({
      type: "seed_head",
      sessionId,
      head: {
        session: mkSession(sessionId),
        turns: [
          {
            turn_id: "turn-1",
            session_id: sessionId,
            run_id: "run-1",
            user_message_id: "message-1",
            status: "running",
            start_seq: 1,
            end_seq: null,
            started_at: createdAt,
            updated_at: createdAt,
            assistant_partial: null,
            thought_partial: "",
            metrics_json: null,
            tool_total: 0,
            tool_pending: 0,
            tool_running: 0,
            tool_completed: 0,
            tool_failed: 0,
          },
        ],
        events: [],
        messages: [
          {
            id: "message-1",
            session_id: sessionId,
            task_id: "task-1",
            turn_id: "turn-1",
            role: "user",
            content: "hi",
            delivery: "immediate",
            created_at: createdAt,
          },
        ],
        last_event_seq: 1,
        state_rev: 1,
        activity: { is_working: true, last_turn_status: "running" },
        has_more_turns: false,
        has_more_history: false,
        history_cursor: null,
      },
      mode: "bootstrap_seed",
    });

    core.handleCommand({
      type: "workspace_event",
      event: {
        type: "session_head_delta",
        workspace_id: "ws-1",
        snapshot_rev: 2,
        delta: {
          session_id: sessionId,
          last_event_seq: 2,
          projection_rev: 2,
          state_rev: 2,
          event: {
            seq: 2,
            id: "event-assistant-complete",
            session_id: sessionId,
            run_id: "run-1",
            turn_id: "turn-1",
            event_type: "assistant_complete",
            payload_json: {
              full_content: "pong",
              message_id: "provider-msg-1",
              order_seq: 2,
            },
            created_at: createdAt,
          },
        },
      },
    });

    const streamingPatch = [...patches].reverse().find(
      (patch) =>
        patch.sessionId === sessionId &&
        patch.op === "append" &&
        patch.data.assistantStreamingByTurnId?.["turn-1"]?.content === "pong",
    );
    expect(streamingPatch).toBeTruthy();

    core.handleCommand({
      type: "workspace_event",
      event: {
        type: "session_head_delta",
        workspace_id: "ws-1",
        snapshot_rev: 3,
        delta: {
          session_id: sessionId,
          last_event_seq: 3,
          projection_rev: 3,
          state_rev: 3,
          message: {
            id: "assistant-msg-1",
            session_id: sessionId,
            task_id: "task-1",
            turn_id: "turn-1",
            role: "assistant",
            content: "pong",
            delivery: "immediate",
            created_at: createdAt,
            order_seq: 2,
          },
        },
      },
    });

    const latest = [...patches].reverse().find(
      (patch) => patch.sessionId === sessionId && patch.op === "append" && Array.isArray(patch.data.messages),
    );
    if (!latest || latest.op === "evict" || !latest.data.messages) {
      throw new Error("expected canonical append patch with assistant message");
    }

    expect(latest.data.messages.some((message) => message.role === "assistant")).toBe(true);
    expect(latest.data.assistantStreamingByTurnId ?? {}).toEqual({});
  });

  it("preserves assistant streaming continuity across close_session background deltas", () => {
    const sessionId = "session-assistant-stream-preserved-on-close";
    const patches: SessionReplicaPatch[] = [];
    const core = new SessionReplicaCore({
      api: { getSessionHead: vi.fn() },
      emit: (next) => patches.push(...next),
    });
    const createdAt = new Date().toISOString();

    core.handleCommand({ type: "init", config: { eventBufferLimit: 100, headLimit: 50 } });
    core.handleCommand({
      type: "seed_head",
      sessionId,
      head: {
        session: mkSession(sessionId),
        turns: [
          {
            turn_id: "turn-1",
            session_id: sessionId,
            run_id: "run-1",
            user_message_id: "message-1",
            status: "running",
            start_seq: 1,
            end_seq: null,
            started_at: createdAt,
            updated_at: createdAt,
            assistant_partial: null,
            thought_partial: "",
            metrics_json: null,
            tool_total: 0,
            tool_pending: 0,
            tool_running: 0,
            tool_completed: 0,
            tool_failed: 0,
          },
        ],
        events: [],
        messages: [
          {
            id: "message-1",
            session_id: sessionId,
            task_id: "task-1",
            turn_id: "turn-1",
            role: "user",
            content: "hi",
            delivery: "immediate",
            created_at: createdAt,
          },
        ],
        last_event_seq: 1,
        state_rev: 1,
        has_more_turns: false,
        has_more_history: false,
        history_cursor: null,
      },
      mode: "bootstrap_seed",
    });

    core.handleCommand({
      type: "workspace_event",
      event: {
        type: "session_head_delta",
        workspace_id: "ws-1",
        snapshot_rev: 2,
        delta: {
          session_id: sessionId,
          last_event_seq: 2,
          projection_rev: 2,
          state_rev: 2,
          event: {
            seq: 2,
            id: "event-assistant-chunk-1",
            session_id: sessionId,
            run_id: "run-1",
            turn_id: "turn-1",
            event_type: "assistant_chunk",
            payload_json: {
              content_fragment: "Hello ",
            },
            created_at: createdAt,
          },
        },
      },
    });

    core.handleCommand({ type: "close_session", sessionId });

    core.handleCommand({
      type: "workspace_event",
      event: {
        type: "session_head_delta",
        workspace_id: "ws-1",
        snapshot_rev: 3,
        delta: {
          session_id: sessionId,
          last_event_seq: 3,
          projection_rev: 3,
          state_rev: 3,
          event: {
            seq: 3,
            id: "event-assistant-chunk-2",
            session_id: sessionId,
            run_id: "run-1",
            turn_id: "turn-1",
            event_type: "assistant_chunk",
            payload_json: {
              content_fragment: "world",
            },
            created_at: createdAt,
          },
        },
      },
    });

    const latest = [...patches].reverse().find(
      (patch) =>
        patch.sessionId === sessionId &&
        patch.op === "append" &&
        patch.data.assistantStreamingByTurnId?.["turn-1"]?.content != null,
    );
    if (!latest || latest.op === "evict") {
      throw new Error("expected append patch with assistant streaming state");
    }

    expect(latest.data.assistantStreamingByTurnId?.["turn-1"]?.content).toBe("Hello world");
  });

  it("applies stale stream-only assistant chunks when they advance visible streaming text", () => {
    const sessionId = "session-stale-assistant-stream-forward-progress";
    const patches: SessionReplicaPatch[] = [];
    const core = new SessionReplicaCore({
      api: { getSessionHead: vi.fn() },
      emit: (next) => patches.push(...next),
    });
    const createdAt = new Date().toISOString();

    core.handleCommand({ type: "init", config: { eventBufferLimit: 100, headLimit: 50 } });
    core.handleCommand({
      type: "seed_head",
      sessionId,
      head: {
        session: mkSession(sessionId),
        turns: [
          {
            turn_id: "turn-1",
            session_id: sessionId,
            run_id: "run-1",
            user_message_id: "message-1",
            status: "running",
            start_seq: 1,
            end_seq: null,
            started_at: createdAt,
            updated_at: createdAt,
            assistant_partial: null,
            thought_partial: "",
            metrics_json: null,
            tool_total: 0,
            tool_pending: 0,
            tool_running: 0,
            tool_completed: 0,
            tool_failed: 0,
          },
        ],
        events: [],
        messages: [
          {
            id: "message-1",
            session_id: sessionId,
            task_id: "task-1",
            turn_id: "turn-1",
            role: "user",
            content: "hi",
            delivery: "immediate",
            created_at: createdAt,
          },
        ],
        last_event_seq: 10,
        state_rev: 10,
        activity: { is_working: true, last_turn_status: "running" },
        has_more_turns: false,
        has_more_history: false,
        history_cursor: null,
      },
      mode: "bootstrap_seed",
    });
    patches.length = 0;

    const staleChunkDelta = {
      type: "session_head_delta" as const,
      workspace_id: "ws-1",
      snapshot_rev: 11,
      delta: {
        session_id: sessionId,
        last_event_seq: 8,
        projection_rev: 8,
        state_rev: 8,
        event: {
          seq: 8,
          id: "event-stale-assistant-chunk",
          session_id: sessionId,
          run_id: "run-1",
          turn_id: "turn-1",
          event_type: "assistant_chunk" as const,
          payload_json: {
            content_fragment: "stale-visible",
          },
          created_at: createdAt,
        },
      },
    };

    core.handleCommand({ type: "workspace_event", event: staleChunkDelta });

    const latest = [...patches].reverse().find(
      (patch) =>
        patch.sessionId === sessionId &&
        patch.op === "append" &&
        patch.data.appendMode === "stream_delta",
    );
    if (!latest || latest.op === "evict") {
      throw new Error("expected stale assistant chunk to emit a stream delta patch");
    }
    expect(latest.data.assistantStreamingByTurnId?.["turn-1"]?.content).toBe("stale-visible");
    expect(latest.data.lastEventSeq).toBeUndefined();
    expect(latest.data.projectionRev).toBeUndefined();

    const patchCount = patches.length;
    core.handleCommand({ type: "workspace_event", event: staleChunkDelta });
    expect(patches).toHaveLength(patchCount);
  });

  it("applies stale user message events when they introduce a visible turn", () => {
    const sessionId = "session-stale-user-message-forward-progress";
    const patches: SessionReplicaPatch[] = [];
    const core = new SessionReplicaCore({
      api: { getSessionHead: vi.fn() },
      emit: (next) => patches.push(...next),
    });
    const createdAt = new Date().toISOString();

    core.handleCommand({ type: "init", config: { eventBufferLimit: 100, headLimit: 50 } });
    core.handleCommand({
      type: "seed_head",
      sessionId,
      head: {
        session: mkSession(sessionId),
        turns: [
          {
            turn_id: "turn-1",
            session_id: sessionId,
            run_id: "run-1",
            user_message_id: "message-1",
            status: "completed",
            start_seq: 1,
            end_seq: 2,
            started_at: createdAt,
            updated_at: createdAt,
            assistant_partial: null,
            thought_partial: "",
            metrics_json: null,
            tool_total: 0,
            tool_pending: 0,
            tool_running: 0,
            tool_completed: 0,
            tool_failed: 0,
          },
        ],
        events: [],
        messages: [
          {
            id: "message-1",
            session_id: sessionId,
            task_id: "task-1",
            turn_id: "turn-1",
            role: "user",
            content: "old",
            delivery: "immediate",
            created_at: createdAt,
          },
        ],
        last_event_seq: 10,
        state_rev: 10,
        activity: { is_working: true, last_turn_status: "running" },
        has_more_turns: false,
        has_more_history: false,
        history_cursor: null,
      },
      mode: "bootstrap_seed",
    });
    patches.length = 0;

    core.handleCommand({
      type: "workspace_event",
      event: {
        type: "session_head_delta",
        workspace_id: "ws-1",
        snapshot_rev: 11,
        delta: {
          session_id: sessionId,
          last_event_seq: 8,
          projection_rev: 8,
          state_rev: 8,
          event: {
            seq: 8,
            id: "event-stale-user-message",
            session_id: sessionId,
            run_id: "run-2",
            turn_id: "turn-2",
            event_type: "user_message",
            payload_json: {
              message_id: "message-2",
              content: "stale visible user prompt",
              delivery: "immediate",
            },
            created_at: createdAt,
          },
        },
      },
    });

    const latest = [...patches].reverse().find(
      (patch) =>
        patch.sessionId === sessionId &&
        patch.op === "append" &&
        Array.isArray(patch.data.messages) &&
        Array.isArray(patch.data.turns),
    );
    if (!latest || latest.op === "evict" || !latest.data.messages || !latest.data.turns) {
      throw new Error("expected stale user message to emit visible transcript data");
    }
    expect(latest.data.messages.some((message) => message.id === "message-2")).toBe(true);
    expect(latest.data.turns.some((turn) => turn.turn_id === "turn-2")).toBe(true);
    expect(latest.data.lastEventSeq).toBe(10);
    expect(latest.data.projectionRev).toBeUndefined();
  });

  it("keeps stream-only assistant chunks out of durable event buffer eviction", () => {
    const sessionId = "session-assistant-stream-buffer-rollover";
    const patches: SessionReplicaPatch[] = [];
    const core = new SessionReplicaCore({
      api: { getSessionHead: vi.fn() },
      emit: (next) => patches.push(...next),
    });
    const createdAt = new Date().toISOString();

    core.handleCommand({ type: "init", config: { eventBufferLimit: 2, headLimit: 50 } });
    core.handleCommand({
      type: "seed_head",
      sessionId,
      head: {
        session: mkSession(sessionId),
        turns: [
          {
            turn_id: "turn-1",
            session_id: sessionId,
            run_id: "run-1",
            user_message_id: "message-1",
            status: "running",
            start_seq: 1,
            end_seq: null,
            started_at: createdAt,
            updated_at: createdAt,
            assistant_partial: null,
            thought_partial: "",
            metrics_json: null,
            tool_total: 0,
            tool_pending: 0,
            tool_running: 0,
            tool_completed: 0,
            tool_failed: 0,
          },
        ],
        events: [],
        messages: [
          {
            id: "message-1",
            session_id: sessionId,
            task_id: "task-1",
            turn_id: "turn-1",
            role: "user",
            content: "hi",
            delivery: "immediate",
            created_at: createdAt,
          },
        ],
        last_event_seq: 1,
        state_rev: 1,
        has_more_turns: false,
        has_more_history: false,
        history_cursor: null,
      },
      mode: "bootstrap_seed",
    });
    patches.length = 0;

    const fragments = ["a", "b", "c", "d", "e"];
    for (const [index, fragment] of fragments.entries()) {
      const seq = index + 2;
      core.handleCommand({
        type: "workspace_event",
        event: {
          type: "session_head_delta",
          workspace_id: "ws-1",
          snapshot_rev: seq,
          delta: {
            session_id: sessionId,
            last_event_seq: seq,
            projection_rev: seq,
            state_rev: seq,
            event: {
              seq,
              id: `event-assistant-chunk-${seq}`,
              session_id: sessionId,
              run_id: "run-1",
              turn_id: "turn-1",
              event_type: "assistant_chunk",
              payload_json: {
                content_fragment: fragment,
              },
              created_at: createdAt,
            },
          },
        },
      });
    }

    expect(patches.some((patch) => patch.op === "evict")).toBe(false);
    const streamPatches = patches.filter(
      (patch) =>
        patch.op === "append" &&
        patch.sessionId === sessionId &&
        patch.data.appendMode === "stream_delta",
    );
    expect(streamPatches).toHaveLength(fragments.length);
    expect(streamPatches.every((patch) => patch.op === "append" && patch.data.events === undefined)).toBe(true);
    expect(streamPatches.every((patch) => patch.op === "append" && patch.data.turns === undefined)).toBe(true);
    expect(streamPatches.every((patch) => patch.op === "append" && patch.data.messages === undefined)).toBe(true);
    expect(streamPatches.every((patch) => patch.op === "append" && patch.data.lastEventSeq === undefined)).toBe(true);
    expect(streamPatches.every((patch) => patch.op === "append" && patch.data.projectionRev === undefined)).toBe(true);
    expect(streamPatches.every((patch) => patch.op === "append" && patch.data.stateRev === undefined)).toBe(true);
    expect(streamPatches.every((patch) => patch.op === "append" && patch.data.freshness === undefined)).toBe(true);
    const latest = streamPatches.at(-1);
    if (!latest || latest.op === "evict") {
      throw new Error("expected stream delta patch");
    }
    expect(latest.data.assistantStreamingByTurnId?.["turn-1"]?.content).toBe(fragments.join(""));
  });

  it("emits live stream deltas with only new transcript data from a large canonical buffer", () => {
    const sessionId = "session-large-buffer-live-delta";
    const patches: SessionReplicaPatch[] = [];
    const core = new SessionReplicaCore({
      api: { getSessionHead: vi.fn() },
      emit: (next) => patches.push(...next),
    });
    const createdAt = new Date().toISOString();
    const bufferedEvents = Array.from({ length: 100 }, (_, index): SessionEvent => ({
      seq: index + 1,
      id: `event-buffered-${index + 1}`,
      session_id: sessionId,
      run_id: "run-1",
      turn_id: "turn-1",
      event_type: "notice",
      payload_json: { index },
      created_at: createdAt,
    }));
    const bufferedMessages = Array.from({ length: 25 }, (_, index): Message => ({
      id: `message-buffered-${index + 1}`,
      session_id: sessionId,
      task_id: "task-1",
      turn_id: "turn-1",
      role: index % 2 === 0 ? "user" : "assistant",
      content: `buffered ${index + 1}`,
      delivery: "immediate",
      created_at: createdAt,
    }));

    core.handleCommand({ type: "init", config: { eventBufferLimit: 200, headLimit: 50 } });
    core.handleCommand({
      type: "seed_head",
      sessionId,
      head: {
        session: mkSession(sessionId),
        turns: [{
          turn_id: "turn-1",
          session_id: sessionId,
          run_id: "run-1",
          user_message_id: "message-buffered-1",
          status: "running",
          start_seq: 1,
          end_seq: null,
          started_at: createdAt,
          updated_at: createdAt,
          assistant_partial: null,
          thought_partial: "",
          metrics_json: null,
          tool_total: 0,
          tool_pending: 0,
          tool_running: 0,
          tool_completed: 0,
          tool_failed: 0,
        }],
        events: bufferedEvents,
        messages: bufferedMessages,
        last_event_seq: 100,
        state_rev: 100,
        has_more_turns: false,
        has_more_history: false,
        history_cursor: null,
      },
      mode: "bootstrap_seed",
    });
    patches.length = 0;

    const liveMessage: Message = {
      id: "message-live-101",
      session_id: sessionId,
      task_id: "task-1",
      turn_id: "turn-1",
      role: "assistant",
      content: "live answer",
      delivery: "immediate",
      created_at: createdAt,
    };
    core.handleCommand({
      type: "workspace_event",
      event: {
        type: "session_head_delta",
        workspace_id: "ws-1",
        snapshot_rev: 101,
        delta: {
          session_id: sessionId,
          last_event_seq: 101,
          projection_rev: 101,
          state_rev: 101,
          event: {
            seq: 101,
            id: "event-live-101",
            session_id: sessionId,
            run_id: "run-1",
            turn_id: "turn-1",
            event_type: "assistant_message_inserted",
            payload_json: {
              message_id: liveMessage.id,
              content: liveMessage.content,
              delivery: liveMessage.delivery,
            },
            created_at: createdAt,
          },
          message: liveMessage,
        },
      },
    });

    const latest = [...patches].reverse().find(
      (patch) =>
        patch.sessionId === sessionId &&
        patch.op === "append" &&
        patch.data.appendMode === "stream_delta",
    );
    if (!latest || latest.op === "evict") {
      throw new Error("expected stream delta patch");
    }
    expect(latest.data.events?.map((event) => event.id)).toEqual(["event-live-101"]);
    expect(latest.data.messages?.map((message) => message.id)).toEqual(["message-live-101"]);
    expect(JSON.stringify(latest.data).length).toBeLessThan(8_000);
  });

  it("emits authoritative live turn counter clears in stream deltas", () => {
    const sessionId = "session-live-tool-counter-clear";
    const patches: SessionReplicaPatch[] = [];
    const core = new SessionReplicaCore({
      api: { getSessionHead: vi.fn() },
      emit: (next) => patches.push(...next),
    });
    const createdAt = new Date().toISOString();

    core.handleCommand({ type: "init", config: { eventBufferLimit: 100, headLimit: 50 } });
    core.handleCommand({
      type: "seed_head",
      sessionId,
      head: {
        session: mkSession(sessionId),
        turns: [{
          turn_id: "turn-1",
          session_id: sessionId,
          run_id: "run-1",
          user_message_id: "message-1",
          status: "running",
          start_seq: 1,
          end_seq: null,
          started_at: createdAt,
          updated_at: createdAt,
          assistant_partial: null,
          thought_partial: "",
          metrics_json: null,
          tool_total: 1,
          tool_pending: 1,
          tool_running: 1,
          tool_completed: 0,
          tool_failed: 0,
        }],
        events: [],
        messages: [],
        last_event_seq: 1,
        state_rev: 1,
        has_more_turns: false,
        has_more_history: false,
        history_cursor: null,
      },
      mode: "bootstrap_seed",
    });
    patches.length = 0;

    core.handleCommand({
      type: "workspace_event",
      event: {
        type: "session_head_delta",
        workspace_id: "ws-1",
        snapshot_rev: 2,
        delta: {
          session_id: sessionId,
          last_event_seq: 2,
          projection_rev: 2,
          state_rev: 2,
          turn: {
            turn_id: "turn-1",
            session_id: sessionId,
            run_id: "run-1",
            user_message_id: "message-1",
            status: "running",
            start_seq: 1,
            end_seq: null,
            started_at: createdAt,
            updated_at: createdAt,
            assistant_partial: null,
            thought_partial: "",
            metrics_json: null,
            tool_total: 1,
            tool_pending: 0,
            tool_running: 0,
            tool_completed: 1,
            tool_failed: 0,
          },
        },
      },
    });

    const latest = [...patches].reverse().find(
      (patch) =>
        patch.sessionId === sessionId &&
        patch.op === "append" &&
        patch.data.appendMode === "stream_delta",
    );
    if (!latest || latest.op === "evict") {
      throw new Error("expected stream delta patch");
    }
    expect(latest.data.turns?.[0]?.tool_pending).toBe(0);
    expect(latest.data.turns?.[0]?.tool_running).toBe(0);
    expect(latest.data.turns?.[0]?.tool_completed).toBe(1);
  });

  it("renders missing terminal live state even when a repair head advanced the cursor", () => {
    const sessionId = "session-stale-terminal-delta";
    const patches: SessionReplicaPatch[] = [];
    const freshnessEvents: SessionReplicaFreshnessEvent[] = [];
    const core = new SessionReplicaCore({
      api: { getSessionHead: vi.fn() },
      emit: (next) => patches.push(...next),
      emitFreshness: (event) => freshnessEvents.push(event),
    });
    const createdAt = new Date().toISOString();

    core.handleCommand({ type: "init", config: { eventBufferLimit: 100, headLimit: 50 } });
    core.handleCommand({
      type: "seed_head",
      sessionId,
      head: {
        session: mkSession(sessionId),
        turns: [],
        events: [],
        messages: [],
        last_event_seq: 10,
        projection_rev: 10,
        state_rev: 10,
        has_more_turns: false,
        has_more_history: false,
        history_cursor: null,
      },
      mode: "repair_replace",
    });
    patches.length = 0;

    core.handleCommand({
      type: "workspace_event",
      event: {
        type: "session_head_delta",
        workspace_id: "ws-1",
        snapshot_rev: 11,
        delta: {
          session_id: sessionId,
          last_event_seq: 8,
          projection_rev: 8,
          state_rev: 8,
          turn: {
            turn_id: "turn-live",
            session_id: sessionId,
            run_id: "run-live",
            user_message_id: "message-user",
            status: "interrupted",
            start_seq: 6,
            end_seq: 8,
            started_at: createdAt,
            updated_at: createdAt,
            assistant_partial: null,
            thought_partial: "",
            metrics_json: null,
            tool_total: 0,
            tool_pending: 0,
            tool_running: 0,
            tool_completed: 0,
            tool_failed: 0,
          },
          event: {
            seq: 8,
            id: "event-assistant-message-8",
            session_id: sessionId,
            run_id: "run-live",
            turn_id: "turn-live",
            event_type: "assistant_message_inserted",
            payload_json: {
              message_id: "message-live",
              content: "interrupted visibly",
            },
            transient: false,
            created_at: createdAt,
          },
        },
      },
    });

    const latest = [...patches].reverse().find(
      (patch) =>
        patch.sessionId === sessionId &&
        patch.op === "append" &&
        patch.data.appendMode === "stream_delta",
    );
    if (!latest || latest.op === "evict") {
      throw new Error("expected visible stream delta patch");
    }
    expect(latest.data.lastEventSeq).toBe(10);
    expect(latest.data.turns?.[0]?.status).toBe("interrupted");
    expect(latest.data.messages?.[0]?.content).toBe("interrupted visibly");
    expect(freshnessEvents).not.toEqual(expect.arrayContaining([
      expect.objectContaining({ type: "projection_or_seq_regression" }),
      expect.objectContaining({ type: "stale_head_delta_dropped" }),
    ]));
  });

  it("merges visible stale deltas without regressing running activity", () => {
    const sessionId = "session-stale-visible-running-activity";
    const patches: SessionReplicaPatch[] = [];
    const core = new SessionReplicaCore({
      api: { getSessionHead: vi.fn() },
      emit: (next) => patches.push(...next),
    });
    const createdAt = new Date().toISOString();

    core.handleCommand({ type: "init", config: { eventBufferLimit: 100, headLimit: 50 } });
    core.handleCommand({
      type: "seed_head",
      sessionId,
      head: {
        session: mkSession(sessionId),
        turns: [{
          turn_id: "turn-live",
          session_id: sessionId,
          run_id: "run-live",
          user_message_id: "message-user",
          status: "running",
          start_seq: 6,
          end_seq: null,
          started_at: createdAt,
          updated_at: createdAt,
          assistant_partial: null,
          thought_partial: "",
          metrics_json: null,
          tool_total: 1,
          tool_pending: 0,
          tool_running: 1,
          tool_completed: 0,
          tool_failed: 0,
        }],
        events: [],
        messages: [],
        activity: { is_working: true, last_turn_status: "running" },
        last_event_seq: 10,
        projection_rev: 10,
        state_rev: 10,
        has_more_turns: false,
        has_more_history: false,
        history_cursor: null,
      },
      mode: "repair_replace",
    });
    patches.length = 0;

    core.handleCommand({
      type: "workspace_event",
      event: {
        type: "session_head_delta",
        workspace_id: "ws-1",
        snapshot_rev: 11,
        delta: {
          session_id: sessionId,
          last_event_seq: 8,
          projection_rev: 8,
          state_rev: 8,
          activity: { is_working: false, last_turn_status: "completed" },
          turn: {
            turn_id: "turn-live",
            session_id: sessionId,
            run_id: "run-live",
            user_message_id: "message-user",
            status: "completed",
            start_seq: 6,
            end_seq: 8,
            started_at: createdAt,
            updated_at: createdAt,
            assistant_partial: null,
            thought_partial: "",
            metrics_json: null,
            tool_total: 1,
            tool_pending: 0,
            tool_running: 0,
            tool_completed: 1,
            tool_failed: 0,
          },
          event: {
            seq: 8,
            id: "event-assistant-message-8",
            session_id: sessionId,
            run_id: "run-live",
            turn_id: "turn-live",
            event_type: "assistant_message_inserted",
            payload_json: {
              message_id: "message-live",
              content: "visible but older",
            },
            transient: false,
            created_at: createdAt,
          },
        },
      },
    });

    const latest = [...patches].reverse().find(
      (patch) =>
        patch.sessionId === sessionId &&
        patch.op === "append" &&
        patch.data.appendMode === "stream_delta",
    );
    if (!latest || latest.op === "evict") {
      throw new Error("expected visible stream delta patch");
    }
    expect(latest.data.messages?.[0]?.content).toBe("visible but older");
    expect(latest.data.lastEventSeq).toBe(10);
    expect(latest.data.projectionRev).toBe(10);
    expect(latest.data.activity).toBeUndefined();
    expect(latest.data.turns?.[0]?.status).toBe("running");
    expect(latest.data.turns?.[0]?.tool_running).toBe(1);
  });

  it("does not project stale lifecycle events onto newer running turns", () => {
    const sessionId = "session-stale-lifecycle-event";
    const patches: SessionReplicaPatch[] = [];
    const core = new SessionReplicaCore({
      api: { getSessionHead: vi.fn() },
      emit: (next) => patches.push(...next),
    });
    const createdAt = new Date().toISOString();

    core.handleCommand({ type: "init", config: { eventBufferLimit: 100, headLimit: 50 } });
    core.handleCommand({
      type: "seed_head",
      sessionId,
      head: {
        session: mkSession(sessionId),
        turns: [{
          turn_id: "turn-live",
          session_id: sessionId,
          run_id: "run-live",
          user_message_id: "message-user",
          status: "running",
          start_seq: 6,
          end_seq: null,
          started_at: createdAt,
          updated_at: createdAt,
          assistant_partial: null,
          thought_partial: "",
          metrics_json: null,
          tool_total: 1,
          tool_pending: 0,
          tool_running: 1,
          tool_completed: 0,
          tool_failed: 0,
        }],
        events: [],
        messages: [],
        activity: { is_working: true, last_turn_status: "running" },
        last_event_seq: 10,
        projection_rev: 10,
        state_rev: 10,
        has_more_turns: false,
        has_more_history: false,
        history_cursor: null,
      },
      mode: "repair_replace",
    });
    patches.length = 0;

    core.handleCommand({
      type: "workspace_event",
      event: {
        type: "session_head_delta",
        workspace_id: "ws-1",
        snapshot_rev: 11,
        delta: {
          session_id: sessionId,
          last_event_seq: 8,
          projection_rev: 8,
          state_rev: 8,
          activity: { is_working: false, last_turn_status: "completed" },
          turn: {
            turn_id: "turn-live",
            session_id: sessionId,
            run_id: "run-live",
            user_message_id: "message-user",
            status: "completed",
            start_seq: 6,
            end_seq: 8,
            started_at: createdAt,
            updated_at: createdAt,
            assistant_partial: null,
            thought_partial: "",
            metrics_json: null,
            tool_total: 1,
            tool_pending: 0,
            tool_running: 0,
            tool_completed: 1,
            tool_failed: 0,
          },
          event: {
            seq: 8,
            id: "event-turn-finished-8",
            session_id: sessionId,
            run_id: "run-live",
            turn_id: "turn-live",
            event_type: "turn_finished",
            payload_json: { status: "completed" },
            transient: false,
            created_at: createdAt,
          },
        },
      },
    });

    const latest = [...patches].reverse().find(
      (patch) =>
        patch.sessionId === sessionId &&
        patch.op === "append" &&
        patch.data.appendMode === "stream_delta",
    );
    if (!latest || latest.op === "evict") {
      throw new Error("expected stale lifecycle stream delta patch");
    }
    expect(latest.data.lastEventSeq).toBe(10);
    expect(latest.data.projectionRev).toBe(10);
    expect(latest.data.activity).toBeUndefined();
    expect(latest.data.turns?.[0]?.status).toBe("running");
    expect(latest.data.turns?.[0]?.end_seq).toBeNull();
    expect(latest.data.turns?.[0]?.tool_running).toBe(1);
  });

  it("treats same-sequence lower-projection visible deltas as stale", () => {
    const sessionId = "session-same-seq-stale-projection";
    const patches: SessionReplicaPatch[] = [];
    const freshnessEvents: SessionReplicaFreshnessEvent[] = [];
    const core = new SessionReplicaCore({
      api: { getSessionHead: vi.fn() },
      emit: (next) => patches.push(...next),
      emitFreshness: (event) => freshnessEvents.push(event),
    });
    const createdAt = new Date().toISOString();

    core.handleCommand({ type: "init", config: { eventBufferLimit: 100, headLimit: 50 } });
    core.handleCommand({
      type: "seed_head",
      sessionId,
      head: {
        session: mkSession(sessionId),
        turns: [{
          turn_id: "turn-live",
          session_id: sessionId,
          run_id: "run-live",
          user_message_id: "message-user",
          status: "running",
          start_seq: 6,
          end_seq: null,
          started_at: createdAt,
          updated_at: createdAt,
          assistant_partial: null,
          thought_partial: "",
          metrics_json: null,
          tool_total: 1,
          tool_pending: 0,
          tool_running: 1,
          tool_completed: 0,
          tool_failed: 0,
        }],
        events: [],
        messages: [],
        activity: { is_working: true, last_turn_status: "running" },
        last_event_seq: 10,
        projection_rev: 10,
        state_rev: 10,
        has_more_turns: false,
        has_more_history: false,
        history_cursor: null,
      },
      mode: "repair_replace",
    });
    patches.length = 0;

    core.handleCommand({
      type: "workspace_event",
      event: {
        type: "session_head_delta",
        workspace_id: "ws-1",
        snapshot_rev: 11,
        delta: {
          session_id: sessionId,
          last_event_seq: 10,
          projection_rev: 9,
          state_rev: 9,
          activity: { is_working: false, last_turn_status: "completed" },
          turn: {
            turn_id: "turn-live",
            session_id: sessionId,
            run_id: "run-live",
            user_message_id: "message-user",
            status: "completed",
            start_seq: 6,
            end_seq: 10,
            started_at: createdAt,
            updated_at: createdAt,
            assistant_partial: null,
            thought_partial: "",
            metrics_json: null,
            tool_total: 1,
            tool_pending: 0,
            tool_running: 0,
            tool_completed: 1,
            tool_failed: 0,
          },
          message: {
            id: "message-same-seq",
            session_id: sessionId,
            task_id: "task-1",
            turn_id: "turn-live",
            role: "assistant",
            content: "same sequence older projection",
            delivery: "immediate",
            created_at: createdAt,
          },
        },
      },
    });

    const latest = [...patches].reverse().find(
      (patch) =>
        patch.sessionId === sessionId &&
        patch.op === "append" &&
        patch.data.appendMode === "stream_delta",
    );
    if (!latest || latest.op === "evict") {
      throw new Error("expected same-sequence stale stream delta patch");
    }
    expect(latest.data.messages?.[0]?.content).toBe("same sequence older projection");
    expect(latest.data.lastEventSeq).toBe(10);
    expect(latest.data.projectionRev).toBe(10);
    expect(latest.data.activity).toBeUndefined();
    expect(latest.data.turns?.[0]?.status).toBe("running");
    expect(freshnessEvents).not.toEqual(expect.arrayContaining([
      expect.objectContaining({ type: "projection_or_seq_regression" }),
      expect.objectContaining({ type: "stale_head_delta_dropped" }),
    ]));
  });

  it("keeps foreground transcript updates moving through a ctx-ui sized stale repair backlog", () => {
    const sessionId = "session-stale-backlog-foreground";
    const patches: SessionReplicaPatch[] = [];
    const core = new SessionReplicaCore({
      api: { getSessionHead: vi.fn() },
      emit: (next) => patches.push(...next),
    });
    const createdAt = new Date().toISOString();
    const repairCursor = 60_000;
    const backlogDeltas = 12_000;
    const visibleEvery = 400;

    core.handleCommand({ type: "init", config: { eventBufferLimit: 200, headLimit: 50 } });
    core.handleCommand({
      type: "seed_head",
      sessionId,
      head: {
        session: mkSession(sessionId),
        turns: [],
        events: [],
        messages: [],
        last_event_seq: repairCursor,
        projection_rev: repairCursor,
        state_rev: repairCursor,
        has_more_turns: false,
        has_more_history: false,
        history_cursor: null,
      },
      mode: "repair_replace",
    });
    patches.length = 0;

    for (let seq = 1; seq <= backlogDeltas; seq += 1) {
      const isVisibleProgress = seq % visibleEvery === 0;
      const turnId = isVisibleProgress ? `turn-visible-${seq}` : `turn-noise-${seq}`;
      core.handleCommand({
        type: "workspace_event",
        event: {
          type: "session_head_delta",
          workspace_id: "ws-1",
          snapshot_rev: seq + 1,
          delta: {
            session_id: sessionId,
            last_event_seq: seq,
            projection_rev: seq,
            state_rev: seq,
            turn: {
              turn_id: turnId,
              session_id: sessionId,
              run_id: `run-${seq}`,
              user_message_id: `message-user-${seq}`,
              status: isVisibleProgress ? "completed" : "running",
              start_seq: seq,
              end_seq: isVisibleProgress ? seq : null,
              started_at: createdAt,
              updated_at: createdAt,
              assistant_partial: null,
              thought_partial: "",
              metrics_json: null,
              tool_total: 0,
              tool_pending: isVisibleProgress ? 0 : 1,
              tool_running: isVisibleProgress ? 0 : 1,
              tool_completed: 0,
              tool_failed: 0,
            },
            event: isVisibleProgress
              ? {
                  seq,
                  id: `event-visible-${seq}`,
                  session_id: sessionId,
                  run_id: `run-${seq}`,
                  turn_id: turnId,
                  event_type: "assistant_message_inserted",
                  payload_json: {
                    message_id: `message-visible-${seq}`,
                    content: `visible progress ${seq}`,
                  },
                  transient: false,
                  created_at: createdAt,
                }
              : undefined,
          },
        },
      });
    }

    core.handleCommand({
      type: "workspace_event",
      event: {
        type: "session_head_delta",
        workspace_id: "ws-1",
        snapshot_rev: backlogDeltas + 2,
        delta: {
          session_id: sessionId,
          last_event_seq: backlogDeltas + 1,
          projection_rev: backlogDeltas + 1,
          state_rev: backlogDeltas + 1,
          turn: {
            turn_id: "turn-interrupted-final",
            session_id: sessionId,
            run_id: "run-interrupted-final",
            user_message_id: "message-user-final",
            status: "interrupted",
            start_seq: backlogDeltas,
            end_seq: backlogDeltas + 1,
            started_at: createdAt,
            updated_at: createdAt,
            assistant_partial: null,
            thought_partial: "",
            metrics_json: null,
            tool_total: 0,
            tool_pending: 0,
            tool_running: 0,
            tool_completed: 0,
            tool_failed: 0,
          },
          event: {
            seq: backlogDeltas + 1,
            id: "event-interrupted-final",
            session_id: sessionId,
            run_id: "run-interrupted-final",
            turn_id: "turn-interrupted-final",
            event_type: "assistant_message_inserted",
            payload_json: {
              message_id: "message-interrupted-final",
              content: "stop became visible",
            },
            transient: false,
            created_at: createdAt,
          },
        },
      },
    });

    const visiblePatches = patches.filter(
      (patch) =>
        patch.sessionId === sessionId &&
        patch.op === "append" &&
        patch.data.appendMode === "stream_delta" &&
        (patch.data.messages?.length ?? 0) > 0,
    );
    expect(visiblePatches).toHaveLength(backlogDeltas / visibleEvery + 1);
    const firstVisible = visiblePatches[0];
    const lastVisible = visiblePatches[visiblePatches.length - 1];
    if (!firstVisible || firstVisible.op === "evict" || !lastVisible || lastVisible.op === "evict") {
      throw new Error("expected visible stream delta patches");
    }
    expect(firstVisible.data.messages?.[0]?.content).toBe(`visible progress ${visibleEvery}`);
    expect(lastVisible.data.messages?.[0]?.content).toBe("stop became visible");
    expect(lastVisible.data.turns?.[0]?.status).toBe("interrupted");
    expect(lastVisible.data.lastEventSeq).toBe(repairCursor);
  });

  it("drops stale live deltas before they can regress a completed head", () => {
    const sessionId = "session-stale-live-delta";
    const patches: SessionReplicaPatch[] = [];
    const freshnessEvents: SessionReplicaFreshnessEvent[] = [];
    const core = new SessionReplicaCore({
      api: { getSessionHead: vi.fn() },
      emit: (next) => patches.push(...next),
      emitFreshness: (event) => freshnessEvents.push(event),
    });
    const createdAt = new Date().toISOString();

    core.handleCommand({ type: "init", config: { eventBufferLimit: 100, headLimit: 50 } });
    core.handleCommand({
      type: "seed_head",
      sessionId,
      head: {
        session: mkSession(sessionId),
        turns: [{
          turn_id: "turn-1",
          session_id: sessionId,
          run_id: "run-1",
          user_message_id: "message-1",
          status: "completed",
          start_seq: 1,
          end_seq: 8,
          started_at: createdAt,
          updated_at: createdAt,
          assistant_partial: null,
          thought_partial: "",
          metrics_json: null,
          tool_total: 1,
          tool_pending: 0,
          tool_running: 0,
          tool_completed: 1,
          tool_failed: 0,
        }],
        events: [],
        messages: [],
        last_event_seq: 8,
        projection_rev: 8,
        state_rev: 8,
        has_more_turns: false,
        has_more_history: false,
        history_cursor: null,
      },
      mode: "repair_replace",
    });
    patches.length = 0;

    core.handleCommand({
      type: "workspace_event",
      event: {
        type: "session_head_delta",
        workspace_id: "ws-1",
        snapshot_rev: 3,
        delta: {
          session_id: sessionId,
          last_event_seq: 3,
          projection_rev: 3,
          state_rev: 3,
          turn: {
            turn_id: "turn-1",
            session_id: sessionId,
            run_id: "run-1",
            user_message_id: "message-1",
            status: "running",
            start_seq: 1,
            end_seq: null,
            started_at: createdAt,
            updated_at: createdAt,
            assistant_partial: null,
            thought_partial: "",
            metrics_json: null,
            tool_total: 1,
            tool_pending: 1,
            tool_running: 1,
            tool_completed: 0,
            tool_failed: 0,
          },
        },
      },
    });

    expect(patches).toEqual([]);
    expect(freshnessEvents).not.toEqual(expect.arrayContaining([
      expect.objectContaining({ type: "projection_or_seq_regression" }),
    ]));
    expect(freshnessEvents).toEqual(expect.arrayContaining([
      {
        type: "stale_head_delta_dropped",
        sessionId,
        dimension: "last_event_seq",
        incoming: 3,
        existing: 8,
      },
      {
        type: "stale_head_delta_dropped",
        sessionId,
        dimension: "projection_rev",
        incoming: 3,
        existing: 8,
      },
    ]));
  });

  it("emits live message removals as stream delta tombstones", () => {
    const sessionId = "session-live-message-removal";
    const patches: SessionReplicaPatch[] = [];
    const core = new SessionReplicaCore({
      api: { getSessionHead: vi.fn() },
      emit: (next) => patches.push(...next),
    });
    const createdAt = new Date().toISOString();

    core.handleCommand({ type: "init", config: { eventBufferLimit: 100, headLimit: 50 } });
    core.handleCommand({
      type: "seed_head",
      sessionId,
      head: {
        session: mkSession(sessionId),
        turns: [],
        events: [],
        messages: [{
          id: "message-queued-1",
          session_id: sessionId,
          task_id: "task-1",
          turn_id: "turn-1",
          role: "user",
          content: "queued draft",
          delivery: "queued",
          created_at: createdAt,
        }],
        last_event_seq: 1,
        state_rev: 1,
        has_more_turns: false,
        has_more_history: false,
        history_cursor: null,
      },
      mode: "bootstrap_seed",
    });
    patches.length = 0;

    core.handleCommand({
      type: "workspace_event",
      event: {
        type: "session_head_delta",
        workspace_id: "ws-1",
        snapshot_rev: 2,
        delta: {
          session_id: sessionId,
          last_event_seq: 2,
          projection_rev: 2,
          state_rev: 2,
          event: {
            seq: 2,
            id: "event-remove-message-2",
            session_id: sessionId,
            run_id: "run-1",
            turn_id: "turn-1",
            event_type: "message_queue_removed",
            payload_json: { message_id: "message-queued-1" },
            created_at: createdAt,
          },
        },
      },
    });

    const latest = [...patches].reverse().find(
      (patch) =>
        patch.sessionId === sessionId &&
        patch.op === "append" &&
        patch.data.appendMode === "stream_delta",
    );
    if (!latest || latest.op === "evict") {
      throw new Error("expected stream delta patch");
    }
    expect(latest.data.removedMessageIds).toEqual(["message-queued-1"]);
    expect(latest.data.messages).toBeUndefined();
    expect(latest.data.messagesRev).toBeTypeOf("number");
  });

  it("does not restore dropped sessions from stream-only assistant chunks", () => {
    const sessionId = "session-assistant-stream-dropped-explicitly";
    const patches: SessionReplicaPatch[] = [];
    const core = new SessionReplicaCore({
      api: { getSessionHead: vi.fn() },
      emit: (next) => patches.push(...next),
    });
    const createdAt = new Date().toISOString();

    core.handleCommand({ type: "init", config: { eventBufferLimit: 100, headLimit: 50 } });
    core.handleCommand({
      type: "seed_head",
      sessionId,
      head: {
        session: mkSession(sessionId),
        turns: [
          {
            turn_id: "turn-1",
            session_id: sessionId,
            run_id: "run-1",
            user_message_id: "message-1",
            status: "running",
            start_seq: 1,
            end_seq: null,
            started_at: createdAt,
            updated_at: createdAt,
            assistant_partial: null,
            thought_partial: "",
            metrics_json: null,
            tool_total: 0,
            tool_pending: 0,
            tool_running: 0,
            tool_completed: 0,
            tool_failed: 0,
          },
        ],
        events: [],
        messages: [
          {
            id: "message-1",
            session_id: sessionId,
            task_id: "task-1",
            turn_id: "turn-1",
            role: "user",
            content: "hi",
            delivery: "immediate",
            created_at: createdAt,
          },
        ],
        last_event_seq: 1,
        state_rev: 1,
        has_more_turns: false,
        has_more_history: false,
        history_cursor: null,
      },
      mode: "bootstrap_seed",
    });

    core.handleCommand({
      type: "workspace_event",
      event: {
        type: "session_head_delta",
        workspace_id: "ws-1",
        snapshot_rev: 2,
        delta: {
          session_id: sessionId,
          last_event_seq: 2,
          projection_rev: 2,
          state_rev: 2,
          event: {
            seq: 2,
            id: "event-assistant-chunk-1",
            session_id: sessionId,
            run_id: "run-1",
            turn_id: "turn-1",
            event_type: "assistant_chunk",
            payload_json: {
              content_fragment: "Hello ",
            },
            created_at: createdAt,
          },
        },
      },
    });

    core.handleCommand({ type: "drop_session", sessionId });
    const patchCountAfterDrop = patches.length;

    core.handleCommand({
      type: "workspace_event",
      event: {
        type: "session_head_delta",
        workspace_id: "ws-1",
        snapshot_rev: 3,
        delta: {
          session_id: sessionId,
          last_event_seq: 3,
          projection_rev: 3,
          state_rev: 3,
          event: {
            seq: 3,
            id: "event-assistant-chunk-2",
            session_id: sessionId,
            run_id: "run-1",
            turn_id: "turn-1",
            event_type: "assistant_chunk",
            payload_json: {
              content_fragment: "world",
            },
            created_at: createdAt,
          },
        },
      },
    });

    const postDropPatches = patches.slice(patchCountAfterDrop);
    expect(postDropPatches).toEqual([]);
  });

  it("clears stale assistant streaming overlay on repair replace", () => {
    const sessionId = "session-assistant-stream-cleared-by-repair";
    const patches: SessionReplicaPatch[] = [];
    const core = new SessionReplicaCore({
      api: { getSessionHead: vi.fn() },
      emit: (next) => patches.push(...next),
    });
    const createdAt = new Date().toISOString();

    core.handleCommand({ type: "init", config: { eventBufferLimit: 100, headLimit: 50 } });
    core.handleCommand({
      type: "seed_head",
      sessionId,
      head: {
        session: mkSession(sessionId),
        turns: [
          {
            turn_id: "turn-1",
            session_id: sessionId,
            run_id: "run-1",
            user_message_id: "message-1",
            status: "running",
            start_seq: 1,
            end_seq: null,
            started_at: createdAt,
            updated_at: createdAt,
            assistant_partial: null,
            thought_partial: "",
            metrics_json: null,
            tool_total: 0,
            tool_pending: 0,
            tool_running: 0,
            tool_completed: 0,
            tool_failed: 0,
          },
        ],
        events: [],
        messages: [
          {
            id: "message-1",
            session_id: sessionId,
            task_id: "task-1",
            turn_id: "turn-1",
            role: "user",
            content: "hi",
            delivery: "immediate",
            created_at: createdAt,
          },
        ],
        last_event_seq: 1,
        state_rev: 1,
        has_more_turns: false,
        has_more_history: false,
        history_cursor: null,
      },
      mode: "bootstrap_seed",
    });

    core.handleCommand({
      type: "workspace_event",
      event: {
        type: "session_head_delta",
        workspace_id: "ws-1",
        snapshot_rev: 2,
        delta: {
          session_id: sessionId,
          last_event_seq: 2,
          projection_rev: 2,
          state_rev: 2,
          event: {
            seq: 2,
            id: "event-assistant-complete",
            session_id: sessionId,
            run_id: "run-1",
            turn_id: "turn-1",
            event_type: "assistant_complete",
            payload_json: {
              full_content: "pong",
              message_id: "provider-msg-1",
              order_seq: 2,
            },
            created_at: createdAt,
          },
        },
      },
    });

    core.handleCommand({
      type: "seed_head",
      sessionId,
      head: {
        session: mkSession(sessionId),
        turns: [
          {
            turn_id: "turn-1",
            session_id: sessionId,
            run_id: "run-1",
            user_message_id: "message-1",
            status: "completed",
            start_seq: 1,
            end_seq: 3,
            started_at: createdAt,
            updated_at: createdAt,
            assistant_partial: null,
            thought_partial: "",
            metrics_json: null,
            tool_total: 0,
            tool_pending: 0,
            tool_running: 0,
            tool_completed: 0,
            tool_failed: 0,
          },
        ],
        events: [],
        messages: [
          {
            id: "message-1",
            session_id: sessionId,
            task_id: "task-1",
            turn_id: "turn-1",
            role: "user",
            content: "hi",
            delivery: "immediate",
            created_at: createdAt,
          },
          {
            id: "assistant-msg-1",
            session_id: sessionId,
            task_id: "task-1",
            turn_id: "turn-1",
            role: "assistant",
            content: "pong",
            delivery: "immediate",
            created_at: createdAt,
            order_seq: 2,
          },
        ],
        last_event_seq: 3,
        state_rev: 3,
        has_more_turns: false,
        has_more_history: false,
        history_cursor: null,
      },
      mode: "repair_replace",
    });

    const latest = [...patches].reverse().find(
      (patch) => patch.sessionId === sessionId && patch.op === "replace",
    );
    if (!latest || latest.op === "evict") {
      throw new Error("expected repair replace patch");
    }

    expect(latest.data.replaceMode).toBe("repair_replace");
    expect(latest.data.assistantStreamingByTurnId ?? {}).toEqual({});
  });

  it("applies queue events into canonical message state before emitting append patches", () => {
    const sessionId = "session-canonical-queue-delta";
    const patches: SessionReplicaPatch[] = [];
    const core = new SessionReplicaCore({
      api: { getSessionHead: vi.fn() },
      emit: (next) => patches.push(...next),
    });
    const createdAt = new Date().toISOString();

    core.handleCommand({ type: "init", config: { eventBufferLimit: 100, headLimit: 50 } });
    core.handleCommand({
      type: "seed_head",
      sessionId,
      head: {
        session: mkSession(sessionId),
        turns: [] as SessionTurn[],
        events: [] as SessionEvent[],
        messages: [
          {
            id: "message-queue",
            session_id: sessionId,
            task_id: "task-1",
            role: "user",
            content: "queue me",
            delivery: "immediate",
            created_at: createdAt,
          },
        ],
        last_event_seq: 1,
        state_rev: 1,
        has_more_turns: false,
        has_more_history: false,
        history_cursor: null,
      },
      mode: "bootstrap_seed",
    });

    core.handleCommand({
      type: "workspace_event",
      event: {
        type: "session_head_delta",
        workspace_id: "ws-1",
        snapshot_rev: 2,
        delta: {
          session_id: sessionId,
          last_event_seq: 2,
          projection_rev: 2,
          state_rev: 2,
          event: {
            seq: 2,
            id: "event-queue-added",
            session_id: sessionId,
            run_id: "run-1",
            event_type: "message_queue_added",
            payload_json: { message_id: "message-queue" },
            created_at: createdAt,
          },
        },
      },
    });

    const latest = [...patches].reverse().find(
      (patch) => patch.sessionId === sessionId && patch.op === "append" && Array.isArray(patch.data.messages),
    );
    if (!latest || latest.op === "evict" || !latest.data.messages) {
      throw new Error("expected canonical queue append patch");
    }

    expect(latest.data.messages[0]?.delivery).toBe("queued");
    expect(latest.data.messagesRev).toBeTypeOf("number");
    expect(latest.data.lastEventSeq).toBe(2);
  });
});
