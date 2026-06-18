import { execSync } from "child_process";
import { mkdtempSync, writeFileSync } from "fs";
import { tmpdir } from "os";
import path from "path";
import type { APIRequestContext } from "playwright/test";

type NumberRange = { min: number; max: number };

type SeedOptions = {
  tasks: number;
  sessionsPerTask: number | NumberRange;
  turnsPerSession: number;
  workspaceName?: string;
  repoRoot?: string;
  throttleMs?: number;
  messageBytes?: number | NumberRange;
  messageBodyLines?: number | NumberRange;
  messageLinePrefix?: string;
  messagePrefix?: string;
  includeToolSummaries?: boolean;
  toolSummariesPerTurn?: number;
  toolSummaryFixtures?: Array<{
    kind: string;
    title?: string;
    input?: unknown;
    output_text?: string;
  }>;
  awaitTurnCompletion?: boolean;
  completionTimeoutMs?: number;
  seedTranscriptDirect?: boolean;
  directSeedBatchSize?: number;
  directSeedMaterializedTailTurns?: number;
  sessionSource?: {
    providerId: string;
    modelId: string;
    executionEnvironment: string;
  };
};

type SeedResult = {
  workspaceId: string;
  taskIds: string[];
  sessionIdsByTask: Record<string, string[]>;
};

type StreamOptions = {
  sessionIds: string[];
  intervalMs?: number;
  durationMs?: number;
  completionTimeoutMs?: number;
  messageBytes?: number | NumberRange;
  messagePrefix?: string;
  includeToolSummaries?: boolean;
  toolSummariesPerTurn?: number;
  toolSummaryFixtures?: Array<{
    kind: string;
    title?: string;
    input?: unknown;
    output_text?: string;
  }>;
};

type StreamStats = {
  sent: number;
  failures: string[];
};

type PostedMessage = {
  id?: string;
  turn_id?: string | null;
};

type SessionEventRecord = {
  seq?: number;
  event_type?: string;
  turn_id?: string | null;
  payload_json?: unknown;
};

type SessionEventsPage = {
  events?: SessionEventRecord[];
  next_cursor?: number | null;
  has_more?: boolean;
};

const parseCount = (value: number | NumberRange, index: number): number => {
  if (typeof value === "number") return value;
  const span = Math.max(0, value.max - value.min);
  return value.min + (index % (span + 1));
};

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

const DEFAULT_TOOL_FIXTURES = [
  { kind: "execute", title: "Run pwd", input: { command: "pwd" } },
  { kind: "search", title: "Searched context", input: { query: "context" } },
  { kind: "execute", title: "Explored .ctx", input: { command: "ls .ctx" } },
  { kind: "read", title: "Read .ctx", input: { path: ".ctx" } },
  {
    kind: "execute",
    title: "Explored specs",
    input: { command: "ls .ctx/ctx-pack/specs" },
  },
  {
    kind: "execute",
    title: "Run ./scripts/supercat.sh .ctx/ctx-pack/specs",
    input: { command: "./scripts/supercat.sh .ctx/ctx-pack/specs" },
  },
  { kind: "search", title: "Searched workbench", input: { query: "workbench" } },
  { kind: "read", title: "Read ctx-pack", input: { path: ".ctx/ctx-pack" } },
];

const chunkFixtures = (fixtures: SeedOptions["toolSummaryFixtures"], count: number, offset: number) => {
  const list = (fixtures && fixtures.length > 0 ? fixtures : DEFAULT_TOOL_FIXTURES).slice();
  if (list.length === 0 || count <= 0) return [];
  const out = [];
  for (let i = 0; i < count; i++) {
    out.push(list[(offset + i) % list.length]);
  }
  return out;
};

const buildToolMarker = (fixtures: SeedOptions["toolSummaryFixtures"]) => {
  if (!fixtures || fixtures.length === 0) return "";
  return `\n[[tool_calls]]\n${JSON.stringify(fixtures)}\n[[/tool_calls]]`;
};

const buildPaddedMessage = (base: string, targetBytes?: number): string => {
  if (!targetBytes || targetBytes <= base.length) return base;
  const padding = targetBytes - base.length;
  if (padding === 1) return `${base} `;
  return `${base} ${"x".repeat(padding - 1)}`;
};

const buildMultilineMessage = (
  base: string,
  linePrefix: string,
  lineCount: number,
  taskIndex: number,
  sessionIndex: number,
  turnIndex: number,
): string => {
  if (lineCount <= 0) return base;
  const lines = Array.from(
    { length: lineCount },
    (_, lineIndex) =>
      `${linePrefix} ${taskIndex + 1}.${sessionIndex + 1}.${turnIndex + 1}.${lineIndex + 1}`,
  );
  return `${base}\n${lines.join("\n")}`;
};

function initRepo(): string {
  const repo = mkdtempSync(path.join(tmpdir(), "ctx-e2e-fixture-"));
  execSync("git init", { cwd: repo });
  execSync("git config user.email test@example.com", { cwd: repo });
  execSync("git config user.name Test", { cwd: repo });
  writeFileSync(path.join(repo, "README.md"), "fixture\n");
  execSync("git add .", { cwd: repo });
  execSync("git commit -m init", { cwd: repo });
  return repo;
}

async function apiPost<T>(request: APIRequestContext, url: string, data: unknown): Promise<T> {
  const resp = await request.post(url, { data });
  if (!resp.ok()) {
    const body = await resp.text().catch(() => "");
    const suffix = body.trim() ? `: ${body.trim()}` : "";
    throw new Error(`seed request failed: ${url} (${resp.status()})${suffix}`);
  }
  return (await resp.json()) as T;
}

const isTurnAlreadyRunningResponse = (status: number, body: string): boolean =>
  status === 409 && body.toLowerCase().includes("a turn is already running");

async function postImmediateMessageWithRetry(
  request: APIRequestContext,
  sessionId: string,
  content: string,
  opts?: { timeoutMs?: number; pollMs?: number },
): Promise<PostedMessage> {
  const url = `/api/sessions/${sessionId}/messages`;
  const timeoutMs = opts?.timeoutMs ?? 15_000;
  const pollMs = opts?.pollMs ?? 100;
  const start = Date.now();
  while (true) {
    const resp = await request.post(url, {
      data: {
        content,
        delivery: "immediate",
      },
    });
    if (resp.ok()) {
      return (await resp.json()) as PostedMessage;
    }
    const body = await resp.text().catch(() => "");
    if (isTurnAlreadyRunningResponse(resp.status(), body) && Date.now() - start <= timeoutMs) {
      await sleep(pollMs);
      continue;
    }
    const suffix = body.trim() ? `: ${body.trim()}` : "";
    throw new Error(`seed request failed: ${url} (${resp.status()})${suffix}`);
  }
}

async function apiGet<T>(request: APIRequestContext, url: string): Promise<T> {
  const resp = await request.get(url);
  if (!resp.ok()) {
    throw new Error(`seed request failed: ${url} (${resp.status()})`);
  }
  return (await resp.json()) as T;
}

const asRecord = (value: unknown): Record<string, unknown> =>
  value && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : {};

const turnFinishedStatus = (event: SessionEventRecord): string | null => {
  if (event.event_type === "turn_interrupted") return "interrupted";
  if (event.event_type !== "turn_finished") return null;
  const status = asRecord(event.payload_json).status;
  return typeof status === "string" ? status.toLowerCase() : null;
};

const pageNextSeq = (page: SessionEventsPage, events: SessionEventRecord[]): number | null => {
  if (typeof page.next_cursor === "number" && Number.isFinite(page.next_cursor)) {
    return page.next_cursor;
  }
  const seqs = events
    .map((event) => event.seq)
    .filter((seq): seq is number => typeof seq === "number" && Number.isFinite(seq));
  return seqs.length > 0 ? Math.max(...seqs) : null;
};

async function currentSessionEventSeq(
  request: APIRequestContext,
  sessionId: string,
): Promise<number> {
  const eventsPage = await apiGet<SessionEventsPage>(
    request,
    `/api/sessions/${sessionId}/events?tail=1&include_transient=1`,
  );
  const events = Array.isArray(eventsPage.events) ? eventsPage.events : [];
  return pageNextSeq(eventsPage, events) ?? 0;
}

async function waitForTurnFinishedEvent(
  request: APIRequestContext,
  sessionId: string,
  turnId: string,
  opts?: { timeoutMs?: number; shouldStop?: () => boolean; pollMs?: number; afterSeq?: number },
): Promise<void> {
  const timeoutMs = opts?.timeoutMs ?? 15_000;
  const pollMs = opts?.pollMs ?? 100;
  const start = Date.now();
  let afterSeq: number | null = opts?.afterSeq ?? null;
  let useTail = afterSeq === null;
  while (true) {
    if (opts?.shouldStop?.()) {
      return;
    }
    const eventsUrl = useTail
      ? `/api/sessions/${sessionId}/events?tail=1000&include_transient=1`
      : `/api/sessions/${sessionId}/events?after_seq=${afterSeq ?? 0}&limit=1000&include_transient=1`;
    const eventsPage = await apiGet<SessionEventsPage>(
      request,
      eventsUrl,
    );
    const wasTail = useTail;
    useTail = false;
    const events = Array.isArray(eventsPage.events) ? eventsPage.events : [];
    const completed = events.some((event) => {
      if (String(event.turn_id ?? "") !== turnId) return false;
      const status = turnFinishedStatus(event);
      return status === "completed" || status === "done";
    });
    if (completed) {
      return;
    }
    const nextSeq = pageNextSeq(eventsPage, events);
    if (nextSeq !== null) {
      afterSeq = Math.max(afterSeq ?? nextSeq, nextSeq);
    }
    if (!wasTail && eventsPage.has_more) {
      continue;
    }
    if (Date.now() - start > timeoutMs) {
      throw new Error(`turn completion timeout for session ${sessionId} turn ${turnId}`);
    }
    await sleep(pollMs);
  }
}

export async function waitForMessageTurnCompletion(
  request: APIRequestContext,
  sessionId: string,
  messageId: string,
  opts?: { timeoutMs?: number; shouldStop?: () => boolean; pollMs?: number; turnId?: string; afterSeq?: number },
): Promise<void> {
  if (opts?.turnId) {
    await waitForTurnFinishedEvent(request, sessionId, opts.turnId, opts);
    return;
  }
  const timeoutMs = opts?.timeoutMs ?? 15_000;
  const start = Date.now();
  while (true) {
    if (opts?.shouldStop?.()) {
      return;
    }
    const head = await apiGet<{
      turns: Array<{ status: string; tool_total?: number | null; user_message_id?: string | null }>;
      tool_summaries?: unknown[];
    }>(request, `/api/sessions/${sessionId}/head`);
    const turns = Array.isArray(head?.turns) ? head.turns : [];
    const turn = turns.find((entry) => entry.user_message_id === messageId);
    if (turn && (turn.status === "completed" || turn.status === "done")) {
      return;
    }
    if (Date.now() - start > timeoutMs) {
      throw new Error(`turn completion timeout for session ${sessionId} message ${messageId}`);
    }
    await sleep(opts?.pollMs ?? 100);
  }
}

export async function postImmediateMessageAndWaitForCompletion(
  request: APIRequestContext,
  sessionId: string,
  content: string,
  opts?: { timeoutMs?: number; pollMs?: number },
): Promise<{ id: string }> {
  const afterSeq = await currentSessionEventSeq(request, sessionId);
  const savedMessage = await postImmediateMessageWithRetry(request, sessionId, content, opts);
  if (!savedMessage.id) {
    throw new Error(`seeded message for session ${sessionId} did not include an id`);
  }
  if (!savedMessage.turn_id) {
    throw new Error(`seeded message for session ${sessionId} did not include a turn_id`);
  }
  await waitForMessageTurnCompletion(request, sessionId, savedMessage.id, {
    ...opts,
    afterSeq,
    turnId: savedMessage.turn_id,
  });
  return { id: savedMessage.id };
}

export async function seedDummyWorkspace(
  request: APIRequestContext,
  opts: SeedOptions,
): Promise<SeedResult> {
  const repoRoot = opts.repoRoot ?? initRepo();
  const workspaceName = opts.workspaceName ?? `ws-fixture-${Date.now()}`;
  const workspace = await apiPost<{ id: string }>(request, "/api/workspaces", {
    root_path: repoRoot,
    name: workspaceName,
  });

  const taskIds: string[] = [];
  const sessionIdsByTask: Record<string, string[]> = {};
  const throttle = opts.throttleMs ?? 15;
  const includeToolSummaries = Boolean(opts.includeToolSummaries);
  const toolSummariesPerTurn = opts.toolSummariesPerTurn ?? 6;
  const toolSummaryFixtures = opts.toolSummaryFixtures ?? DEFAULT_TOOL_FIXTURES;
  const messagePrefix = opts.messagePrefix ?? "fixture msg";
  const messageBytes = opts.messageBytes;
  const messageBodyLines = opts.messageBodyLines;
  const messageLinePrefix = opts.messageLinePrefix ?? `${messagePrefix} body`;
  const seedTranscriptDirect = Boolean(opts.seedTranscriptDirect);
  const awaitTurnCompletion = opts.awaitTurnCompletion ?? !seedTranscriptDirect;
  if (opts.awaitTurnCompletion === true && seedTranscriptDirect) {
    throw new Error("seedDummyWorkspace requires either awaitTurnCompletion or seedTranscriptDirect, not both");
  }
  const completionTimeoutMs = opts.completionTimeoutMs ?? 60_000;
  const sessionSource = opts.sessionSource ?? {
    providerId: "fake",
    modelId: "fake-model",
    executionEnvironment: "host",
  };

  for (let i = 0; i < opts.tasks; i++) {
    const task = await apiPost<{ id: string; primary_session_id?: string | null }>(
      request,
      `/api/workspaces/${workspace.id}/tasks`,
      {
        title: `fixture task ${i + 1}`,
        default_session: {
          provider_id: sessionSource.providerId,
          model_id: sessionSource.modelId,
          execution_environment: sessionSource.executionEnvironment,
        },
      },
    );
    taskIds.push(task.id);
    sessionIdsByTask[task.id] = [];

    const requestedSessionCount = parseCount(opts.sessionsPerTask, i);
    const sessionCount = Math.max(1, requestedSessionCount);
    for (let s = 0; s < sessionCount; s++) {
      let sessionId = task.primary_session_id;
      if (!sessionId) throw new Error(`seeded task ${task.id} did not include a primary session`);
      if (s > 0) {
        const session = await apiPost<{ id: string }>(request, `/api/tasks/${task.id}/sessions`, {
          provider_id: sessionSource.providerId,
          model_id: sessionSource.modelId,
          execution_environment: sessionSource.executionEnvironment,
          parent_session_id: task.primary_session_id,
          relationship: "sub_agent",
        });
        sessionId = session.id;
      }
      sessionIdsByTask[task.id].push(sessionId);

      if (s >= requestedSessionCount) continue;

      if (seedTranscriptDirect && opts.turnsPerSession > 0) {
        const turns = [];
        for (let t = 0; t < opts.turnsPerSession; t++) {
          const toolFixtures = includeToolSummaries
            ? chunkFixtures(toolSummaryFixtures, toolSummariesPerTurn, t * toolSummariesPerTurn)
            : [];
          const toolMarker = includeToolSummaries ? buildToolMarker(toolFixtures) : "";
          const baseMessage = `${messagePrefix} ${i + 1}.${s + 1}.${t + 1}`;
          const multilineMessage = buildMultilineMessage(
            baseMessage,
            messageLinePrefix,
            messageBodyLines ? parseCount(messageBodyLines, t) : 0,
            i,
            s,
            t,
          );
          const paddedMessage = buildPaddedMessage(
            multilineMessage,
            messageBytes ? parseCount(messageBytes, t) : undefined,
          );
          const assistantMessage = buildMultilineMessage(
            `assistant ${baseMessage}`,
            `${messageLinePrefix} assistant`,
            messageBodyLines ? parseCount(messageBodyLines, t) : 0,
            i,
            s,
            t,
          );
          turns.push({
            user: `${paddedMessage}${toolMarker}`,
            assistant: assistantMessage,
          });
        }
        const batchSize = Math.max(1, opts.directSeedBatchSize ?? turns.length);
        for (let start = 0; start < turns.length; start += batchSize) {
          const end = Math.min(turns.length, start + batchSize);
          await apiPost(request, `/api/dev/sessions/${sessionId}/seed_transcript`, {
            append: start > 0,
            refresh: end >= turns.length,
            materialize_tail_turns:
              end >= turns.length ? opts.directSeedMaterializedTailTurns : 0,
            turns: turns.slice(start, end),
          });
        }
        if (throttle > 0) {
          await sleep(throttle);
        }
        continue;
      }

      for (let t = 0; t < opts.turnsPerSession; t++) {
        const toolFixtures = includeToolSummaries
          ? chunkFixtures(toolSummaryFixtures, toolSummariesPerTurn, t * toolSummariesPerTurn)
          : [];
        const toolMarker = includeToolSummaries ? buildToolMarker(toolFixtures) : "";
        const baseMessage = `${messagePrefix} ${i + 1}.${s + 1}.${t + 1}`;
        const multilineMessage = buildMultilineMessage(
          baseMessage,
          messageLinePrefix,
          messageBodyLines ? parseCount(messageBodyLines, t) : 0,
          i,
          s,
          t,
        );
        const paddedMessage = buildPaddedMessage(
          multilineMessage,
          messageBytes ? parseCount(messageBytes, t) : undefined,
        );
        const savedMessage = await apiPost<{ id: string }>(request, `/api/sessions/${sessionId}/messages`, {
          content: `${paddedMessage}${toolMarker}`,
          delivery: "immediate",
        });
        if (!savedMessage.id) {
          throw new Error(`seeded message for session ${sessionId} did not include an id`);
        }
        if (awaitTurnCompletion) {
          await waitForMessageTurnCompletion(request, sessionId, savedMessage.id, {
            timeoutMs: completionTimeoutMs,
          });
        }
        if (throttle > 0) {
          await sleep(throttle);
        }
      }
    }
  }

  return { workspaceId: workspace.id, taskIds, sessionIdsByTask };
}

export function startStreamingMessages(
  request: APIRequestContext,
  opts: StreamOptions,
): { stop: () => Promise<void>; getStats: () => StreamStats } {
  const intervalMs = opts.intervalMs ?? 250;
  const completionTimeoutMs = opts.completionTimeoutMs ?? 60_000;
  const messagePrefix = opts.messagePrefix ?? "stream msg";
  const includeToolSummaries = Boolean(opts.includeToolSummaries);
  const toolSummariesPerTurn = opts.toolSummariesPerTurn ?? 3;
  const toolSummaryFixtures = opts.toolSummaryFixtures ?? DEFAULT_TOOL_FIXTURES;
  const messageBytes = opts.messageBytes;
  const sessionIds = opts.sessionIds;

  if (!sessionIds || sessionIds.length === 0) {
    throw new Error("startStreamingMessages requires at least one session id");
  }

  let stopped = false;
  let tick = 0;
  let inflight: Promise<void> = Promise.resolve();
  let stopPromise: Promise<void> | null = null;
  let sent = 0;
  const failures: string[] = [];
  const inFlightBySession = new Map<string, Promise<void>>();

  const settleSessionTurn = (sessionId: string, messageId: string) => {
    const completion = waitForMessageTurnCompletion(request, sessionId, messageId, {
      timeoutMs: completionTimeoutMs,
      shouldStop: () => stopped,
    })
      .catch((error: unknown) => {
        if (stopped) return;
        const message = error instanceof Error && error.message ? error.message : String(error);
        failures.push(`background stream completion failed: ${message}`);
      })
      .finally(() => {
        if (inFlightBySession.get(sessionId) === completion) {
          inFlightBySession.delete(sessionId);
        }
      });
    inFlightBySession.set(sessionId, completion);
  };

  const sendOnce = async () => {
    if (stopped) return;
    let sessionId: string | null = null;
    for (let offset = 0; offset < sessionIds.length; offset += 1) {
      const candidate = sessionIds[(tick + offset) % sessionIds.length];
      if (!candidate || inFlightBySession.has(candidate)) continue;
      sessionId = candidate;
      tick += offset;
      break;
    }
    if (!sessionId) return;
    const toolFixtures = includeToolSummaries
      ? chunkFixtures(toolSummaryFixtures, toolSummariesPerTurn, tick * toolSummariesPerTurn)
      : [];
    const toolMarker = includeToolSummaries ? buildToolMarker(toolFixtures) : "";
    const baseMessage = `${messagePrefix} ${tick + 1}`;
    const paddedMessage = buildPaddedMessage(
      baseMessage,
      messageBytes ? parseCount(messageBytes, tick) : undefined,
    );
    tick += 1;
    const savedMessage = await apiPost<{ id: string }>(request, `/api/sessions/${sessionId}/messages`, {
      content: `${paddedMessage}${toolMarker}`,
      delivery: "immediate",
    });
    if (!savedMessage.id) {
      throw new Error(`streamed message for session ${sessionId} did not include an id`);
    }
    sent += 1;
    settleSessionTurn(sessionId, savedMessage.id);
  };

  const timer = setInterval(() => {
    inflight = inflight.then(sendOnce).catch((error: unknown) => {
      const message = error instanceof Error && error.message ? error.message : String(error);
      failures.push(`background stream send failed: ${message}`);
    });
  }, intervalMs);

  const stop = () => {
    if (!stopPromise) {
      stopPromise = (async () => {
        stopped = true;
        clearInterval(timer);
        await inflight;
        await Promise.all(inFlightBySession.values());
        if (failures.length > 0) {
          throw new Error(failures.join("; "));
        }
      })();
    }
    return stopPromise;
  };

  if (opts.durationMs && opts.durationMs > 0) {
    setTimeout(() => {
      stop().catch(() => {
        // ignore
      });
    }, opts.durationMs);
  }

  const getStats = () => ({ sent, failures: failures.slice() });
  return { stop, getStats };
}
