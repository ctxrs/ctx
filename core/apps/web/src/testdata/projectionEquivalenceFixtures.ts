import rawFixtures from "./projectionEquivalence.fixtures.json";
import type {
  Message,
  Session,
  SessionEvent,
  SessionHeadDelta,
  SessionHeadSnapshot,
  SessionSnapshotSummary,
  SessionTurn,
  SessionTurnTool,
  Task,
  WorkspaceActiveHeadBatch,
  WorkspaceActiveSnapshot,
  WorkspaceActiveTaskSummary,
} from "@ctx/types";
import {
  buildActiveSnapshot,
  buildAssistantMessage,
  buildHead,
  buildSession,
  buildStableEvents,
  buildSummary,
  buildTask,
  buildToolByTurnId,
  buildToolSummary,
  buildTurn,
  buildUserMessage,
  FIXTURE_TIMES,
  idsFor,
  type ProjectionEntityIds,
} from "../utils/projectionEquivalenceFixtureBuilders";

type ReplayExpectation = {
  afterSeq: number;
  expectedSeqs: number[];
  expectGap: boolean;
  expectedSeedLastEventSeq?: number;
};

type ActiveProjectionFixtureSpec = {
  userContent: string;
  assistantContent: string;
  toolCallId: string;
  toolTitle: string;
  toolKind: string;
  toolInput: Record<string, unknown>;
  toolOutput: string;
  streamAssistantChunk: string;
  orderSeqs: {
    user: number;
    tool: number;
    assistant: number;
  };
  replayExpectations: ReplayExpectation[];
  expected: {
    headLastEventSeq: number;
    summaryLastEventSeq: number;
    persistedEventTypes: string[];
    stableEventTypes: string[];
    renderItemKinds: string[];
  };
};

type SessionGapSeedFixtureSpec = {
  userContent: string;
  assistantContent: string;
  afterSeq: number;
  expected: {
    headLastEventSeq: number;
    reason: string;
  };
};

type ProjectionFixtureFile = {
  activeProjectionEquivalence: ActiveProjectionFixtureSpec;
  sessionGapSeedRehydrate: SessionGapSeedFixtureSpec;
};

const fixtures = rawFixtures as ProjectionFixtureFile;

type ActiveProjectionFixture = {
  workspaceId: string;
  task: Task;
  session: Session;
  turn: SessionTurn;
  userMessage: Message;
  assistantMessage: Message;
  summary: SessionSnapshotSummary;
  head: SessionHeadSnapshot;
  activeSnapshot: WorkspaceActiveSnapshot;
  activeHeads: WorkspaceActiveHeadBatch;
  toolsByTurnId: Record<string, SessionTurnTool[]>;
  partialDelta: SessionHeadDelta;
  replayExpectations: ReplayExpectation[];
  expected: ActiveProjectionFixtureSpec["expected"] & {
    toolCallId: string;
    assistantContent: string;
  };
};

type SessionGapSeedFixture = {
  workspaceId: string;
  task: Task;
  session: Session;
  summary: SessionSnapshotSummary;
  head: SessionHeadSnapshot;
  activeSnapshot: WorkspaceActiveSnapshot;
  activeHeads: WorkspaceActiveHeadBatch;
  gapEvent: {
    type: "session_gap";
    workspace_id: string;
    snapshot_rev: number;
    session_id: string;
    after_seq: number;
    reason: string;
  };
  seedEvent: {
    type: "session_head_seed";
    workspace_id: string;
    snapshot_rev: number;
    head: SessionHeadSnapshot;
  };
  expected: SessionGapSeedFixtureSpec["expected"];
};

function getGapSeedStableEvents(
  ids: ProjectionEntityIds,
  spec: SessionGapSeedFixtureSpec,
): SessionEvent[] {
  return [
    {
      seq: 1,
      id: ids.userEventId,
      session_id: ids.sessionId,
      run_id: null,
      turn_id: ids.turnId,
      event_type: "user_message",
      payload_json: {
        message_id: ids.userMessageId,
        content: spec.userContent,
        attachments: [],
        order_seq: 1,
      },
      created_at: FIXTURE_TIMES.created,
    },
    {
      seq: 4,
      id: ids.assistantEventId,
      session_id: ids.sessionId,
      run_id: null,
      turn_id: ids.turnId,
      event_type: "assistant_complete",
      payload_json: {
        message_id: ids.assistantMessageId,
        content: spec.assistantContent,
        full_content: spec.assistantContent,
        order_seq: 3,
      },
      created_at: FIXTURE_TIMES.updated,
    },
  ];
}

export function getActiveProjectionFixture(): ActiveProjectionFixture {
  const spec = fixtures.activeProjectionEquivalence;
  const ids = idsFor("projection");
  const task = buildTask(ids.taskId, ids.workspaceId);
  const session = buildSession(ids.sessionId, ids.taskId, ids.workspaceId);
  const turn = buildTurn(
    ids.sessionId,
    ids.turnId,
    ids.userMessageId,
    spec.expected.headLastEventSeq,
    1,
  );
  const userMessage = buildUserMessage(
    session,
    ids.turnId,
    ids.userMessageId,
    spec.userContent,
    spec.orderSeqs.user,
  );
  const assistantMessage = buildAssistantMessage(
    session,
    ids.turnId,
    ids.assistantMessageId,
    spec.assistantContent,
    spec.orderSeqs.assistant,
  );
  const stableEvents = buildStableEvents(ids.sessionId, ids.turnId, ids, spec);
  const toolSummary = buildToolSummary(ids.sessionId, ids.turnId, spec);
  const head = buildHead(
    session,
    turn,
    [userMessage, assistantMessage],
    stableEvents,
    [toolSummary],
    spec.expected.headLastEventSeq,
  );
  const summary = buildSummary(
    session,
    spec.assistantContent,
    spec.expected.summaryLastEventSeq,
  );
  const activeTask: WorkspaceActiveTaskSummary = {
    task,
    primary_session: summary,
    primary_session_head: head,
    sessions: [summary],
    sort_at: FIXTURE_TIMES.updated,
  };

  return {
    workspaceId: ids.workspaceId,
    task,
    session,
    turn,
    userMessage,
    assistantMessage,
    summary,
    head,
    activeSnapshot: buildActiveSnapshot(ids.workspaceId, activeTask),
    activeHeads: {
      workspace_id: ids.workspaceId,
      snapshot_rev: 4,
      heads: [head],
    },
    toolsByTurnId: buildToolByTurnId(ids.sessionId, ids.turnId, spec),
    partialDelta: {
      session_id: ids.sessionId,
      last_event_seq: 3,
      state_rev: 3,
      event: {
        seq: -1,
        id: ids.partialEventId,
        session_id: ids.sessionId,
        run_id: null,
        turn_id: ids.turnId,
        event_type: "assistant_chunk",
        payload_json: {
          content_fragment: spec.streamAssistantChunk,
          order_seq: spec.orderSeqs.assistant,
        },
        transient: true,
        created_at: FIXTURE_TIMES.assistant,
      },
      turn: null,
      message: null,
      tool_summaries: [],
    },
    replayExpectations: spec.replayExpectations,
    expected: {
      ...spec.expected,
      toolCallId: spec.toolCallId,
      assistantContent: spec.assistantContent,
    },
  };
}

export function getSessionGapSeedFixture(): SessionGapSeedFixture {
  const spec = fixtures.sessionGapSeedRehydrate;
  const ids = idsFor("gap-seed");
  const task = buildTask(ids.taskId, ids.workspaceId);
  const session = buildSession(ids.sessionId, ids.taskId, ids.workspaceId);
  const turn = buildTurn(
    ids.sessionId,
    ids.turnId,
    ids.userMessageId,
    spec.expected.headLastEventSeq,
    0,
  );
  const userMessage = buildUserMessage(session, ids.turnId, ids.userMessageId, spec.userContent, 1);
  const assistantMessage = buildAssistantMessage(session, ids.turnId, ids.assistantMessageId, spec.assistantContent, 3);
  const stableEvents = getGapSeedStableEvents(ids, spec);
  const head = buildHead(
    session,
    turn,
    [userMessage, assistantMessage],
    stableEvents,
    [],
    spec.expected.headLastEventSeq,
  );
  const summary = buildSummary(
    session,
    spec.assistantContent,
    spec.expected.headLastEventSeq,
  );
  const activeTask: WorkspaceActiveTaskSummary = {
    task,
    primary_session: summary,
    primary_session_head: head,
    sessions: [summary],
    sort_at: FIXTURE_TIMES.updated,
  };

  return {
    workspaceId: ids.workspaceId,
    task,
    session,
    summary,
    head,
    activeSnapshot: buildActiveSnapshot(ids.workspaceId, activeTask),
    activeHeads: {
      workspace_id: ids.workspaceId,
      snapshot_rev: 4,
      heads: [head],
    },
    gapEvent: {
      type: "session_gap",
      workspace_id: ids.workspaceId,
      snapshot_rev: 5,
      session_id: ids.sessionId,
      after_seq: spec.afterSeq,
      reason: spec.expected.reason,
    },
    seedEvent: {
      type: "session_head_seed",
      workspace_id: ids.workspaceId,
      snapshot_rev: 5,
      head,
    },
    expected: spec.expected,
  };
}
