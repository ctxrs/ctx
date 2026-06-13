import type { APIRequestContext, Page } from "playwright/test";
import type { Message, SessionHeadDelta, SessionTurn } from "@ctx/types";
import { test, expect } from "./fixtures";
import { seedDummyWorkspace } from "./utils/seedDummyWorkspace";

type LongTaskSample = {
  startTime: number;
  duration: number;
};

type InputLatencySample = {
  char: string;
  durationMs: number;
  expectedLength: number;
  actualLength: number;
  timedOut: boolean;
};

type SessionDebug = {
  lastEventSeq: number;
  projectionRev: number;
};

type ReplayStats = {
  injected: number;
  failed: number;
  failures: string[];
  startedAtMs: number;
  stoppedAtMs: number;
};

type ReplayStepMetrics = {
  intervalMs: number;
  targetDeltasPerSecond: number;
  injectedDeltasPerSecond: number;
  eventSeqPerSecond: number;
  injected: number;
  failed: number;
  failures: string[];
  missedIntervalTicks: number;
  eventSeqDelta: number;
  projectionRevDelta: number;
  expectedEventSeqDelta: number;
  backlogDeltas: number;
  catchupTimedOut: boolean;
  inputSamples: InputLatencySample[];
  inputRequestedChars: number;
  inputCompletedChars: number;
  inputTimedOut: boolean;
  p95InputMs: number;
  maxInputMs: number;
  longTasks: LongTaskSample[];
  maxLongTaskMs: number;
};

type ReplayBenchmarkMetrics = {
  seededTurns: number;
  messageBytes: number;
  stepDurationMs: number;
  steps: ReplayStepMetrics[];
};

type ReplayConfig = {
  sessionId: string;
  taskId: string;
  intervalMs: number;
  stepIndex: number;
  messageBytes: number;
  baseSeq: number;
  baseProjectionRev: number;
  streamRevBase: number;
  snapshotRevBase: number;
};

type ReplayWindow = Window & {
  __ctxE2E?: {
    workspaceStream?: {
      injectMessage?: (data: unknown) => boolean;
    };
    getVisibleSessionEntryDebug?: () => {
      sessionId?: string | null;
      lastEventSeq?: number | null;
    } | null;
    getVisibleSessionThreadDebug?: () => {
      projectionRev?: number | null;
    } | null;
    getSessionLastEventSeq?: (sessionId: string) => number | null;
    getSessionProjectionRev?: (sessionId: string) => number | null;
  };
  __streamDeltaPerf?: {
    longTasks: LongTaskSample[];
    observer?: PerformanceObserver;
  };
  __streamDeltaReplay?: {
    stop: () => ReplayStats;
    getStats: () => ReplayStats;
  };
};

const composerSelector = ".wb-session-slot[aria-hidden=\"false\"] textarea.wb-active-textarea";
const visibleSessionSelector = ".wb-session-slot[aria-hidden=\"false\"]";
const inputEchoTimeoutMs = 5_000;

test.setTimeout(240_000);
test.skip(
  ({ browserName }) => browserName !== "chromium",
  "nightly synthetic stream-delta replay probe is calibrated for Chromium",
);

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

function readPositiveIntEnv(name: string, fallback: number): number {
  const raw = process.env[name];
  if (!raw) return fallback;
  const parsed = Number.parseInt(raw, 10);
  if (!Number.isFinite(parsed) || parsed <= 0) {
    throw new Error(`${name} must be a positive integer`);
  }
  return parsed;
}

function readRatioEnv(name: string, fallback: number): number {
  const raw = process.env[name];
  if (!raw) return fallback;
  const parsed = Number.parseFloat(raw);
  if (!Number.isFinite(parsed) || parsed <= 0 || parsed > 1) {
    throw new Error(`${name} must be a ratio greater than 0 and less than or equal to 1`);
  }
  return parsed;
}

function readIntervalLevels(): number[] {
  const raw = process.env.CTX_STREAM_DELTA_REPLAY_INTERVALS_MS;
  if (!raw) return [33];
  const levels = raw
    .split(",")
    .map((part) => part.trim())
    .filter((part) => part.length > 0)
    .map((part) => {
      const parsed = Number.parseInt(part, 10);
      if (!Number.isFinite(parsed) || parsed <= 0) {
        throw new Error(`CTX_STREAM_DELTA_REPLAY_INTERVALS_MS contains an invalid interval: ${part}`);
      }
      return parsed;
    });
  if (levels.length === 0) {
    throw new Error("CTX_STREAM_DELTA_REPLAY_INTERVALS_MS must contain at least one interval");
  }
  return levels;
}

function percentile(values: readonly number[], p: number): number {
  if (values.length === 0) return 0;
  const sorted = values.slice().sort((left, right) => left - right);
  const index = Math.min(sorted.length - 1, Math.max(0, Math.ceil(sorted.length * p) - 1));
  return sorted[index] ?? 0;
}

async function waitForVisibleSession(page: Page, sessionId: string): Promise<void> {
  await expect(page.locator(`${visibleSessionSelector} [data-testid="session-view"]`).first()).toHaveAttribute(
    "data-session-id",
    sessionId,
    { timeout: 20_000 },
  );
}

async function installLongTaskProbe(page: Page): Promise<void> {
  await page.evaluate(() => {
    const w = window as ReplayWindow;
    w.__streamDeltaPerf?.observer?.disconnect();
    const state: NonNullable<ReplayWindow["__streamDeltaPerf"]> = { longTasks: [] };
    w.__streamDeltaPerf = state;
    if (typeof PerformanceObserver === "undefined") return;
    try {
      const observer = new PerformanceObserver((list) => {
        for (const entry of list.getEntries()) {
          state.longTasks.push({ startTime: entry.startTime, duration: entry.duration });
        }
      });
      observer.observe({ type: "longtask", buffered: true });
      state.observer = observer;
    } catch {
      // Browser support varies; input latency remains the required signal.
    }
  });
}

async function readAndClearLongTasks(page: Page): Promise<LongTaskSample[]> {
  return page.evaluate(() => {
    const w = window as ReplayWindow;
    const state = w.__streamDeltaPerf;
    const samples = state?.longTasks.slice() ?? [];
    if (state) {
      state.longTasks = [];
    }
    return samples;
  });
}

async function readSessionDebug(page: Page, sessionId: string): Promise<SessionDebug> {
  return page.evaluate((visibleSessionId) => {
    const bridge = (window as ReplayWindow).__ctxE2E;
    const visible = bridge?.getVisibleSessionEntryDebug?.() ?? null;
    const thread = bridge?.getVisibleSessionThreadDebug?.() ?? null;
    const visibleLastEventSeq =
      typeof visible?.lastEventSeq === "number" && visible.sessionId === visibleSessionId ? visible.lastEventSeq : 0;
    const storedLastEventSeq = bridge?.getSessionLastEventSeq?.(visibleSessionId) ?? 0;
    const storedProjectionRev = bridge?.getSessionProjectionRev?.(visibleSessionId) ?? 0;
    return {
      lastEventSeq: Math.max(visibleLastEventSeq, storedLastEventSeq),
      projectionRev: Math.max(
        typeof thread?.projectionRev === "number" ? thread.projectionRev : 0,
        storedProjectionRev,
      ),
    };
  }, sessionId);
}

async function typeWithLatency(page: Page, text: string): Promise<InputLatencySample[]> {
  const composer = page.locator(composerSelector);
  await expect(composer).toBeVisible({ timeout: 20_000 });
  await composer.fill("");
  await composer.focus();

  const samples: InputLatencySample[] = [];
  let expected = "";
  for (const char of text) {
    expected += char;
    const startedAt = Date.now();
    await page.keyboard.type(char);
    let actual = "";
    let matched = false;
    const deadline = startedAt + inputEchoTimeoutMs;
    while (Date.now() < deadline) {
      actual = await composer.inputValue();
      if (actual === expected) {
        matched = true;
        break;
      }
      await sleep(25);
    }
    if (!matched) {
      actual = await composer.inputValue();
    }
    samples.push({
      char,
      durationMs: Date.now() - startedAt,
      expectedLength: expected.length,
      actualLength: actual.length,
      timedOut: !matched,
    });
    if (!matched) {
      break;
    }
    await sleep(40);
  }
  return samples;
}

async function waitForSessionEventSeqAtLeast(
  page: Page,
  sessionId: string,
  targetSeq: number,
  timeoutMs: number,
): Promise<SessionDebug> {
  const deadline = Date.now() + timeoutMs;
  let debug = await readSessionDebug(page, sessionId);
  while (debug.lastEventSeq < targetSeq && Date.now() < deadline) {
    await sleep(100);
    debug = await readSessionDebug(page, sessionId);
  }
  return debug;
}

async function seedThroughputSession(
  request: APIRequestContext,
  opts: {
    seededTurns: number;
    messageBytes: number;
  },
): Promise<{ workspaceId: string; taskId: string; sessionId: string }> {
  const seed = await seedDummyWorkspace(request, {
    tasks: 1,
    sessionsPerTask: 1,
    turnsPerSession: opts.seededTurns,
    seedTranscriptDirect: true,
    messagePrefix: "stream-delta-replay seed",
    messageBytes: { min: opts.messageBytes, max: opts.messageBytes + 256 },
    messageBodyLines: { min: 4, max: 8 },
    includeToolSummaries: true,
    toolSummariesPerTurn: 3,
  });
  const taskId = seed.taskIds[0] ?? "";
  const sessionId = taskId ? seed.sessionIdsByTask[taskId]?.[0] ?? "" : "";
  if (!taskId || !sessionId) {
    throw new Error("failed to resolve seeded synthetic replay session");
  }
  return { workspaceId: seed.workspaceId, taskId, sessionId };
}

async function ensureReplayInjectionReady(page: Page): Promise<void> {
  await expect
    .poll(
      () =>
        page.evaluate(() => {
          const bridge = (window as ReplayWindow).__ctxE2E;
          return typeof bridge?.workspaceStream?.injectMessage === "function";
        }),
      { timeout: 20_000 },
    )
    .toBe(true);
}

async function startSyntheticReplay(page: Page, config: ReplayConfig): Promise<void> {
  const started = await page.evaluate((replayConfig) => {
    const w = window as ReplayWindow;
    w.__streamDeltaReplay?.stop();
    let stopped = false;
    let index = 0;
    let injected = 0;
    let failed = 0;
    const failures: string[] = [];
    const startedAtMs = Date.now();
    let stoppedAtMs = startedAtMs;

    const buildPayload = (messageIndex: number): string => {
      const base = `synthetic stream-delta replay step=${replayConfig.stepIndex + 1} message=${messageIndex + 1}`;
      if (base.length >= replayConfig.messageBytes) return base;
      return `${base}\n${"x".repeat(replayConfig.messageBytes - base.length - 1)}`;
    };

    const snapshot = (): ReplayStats => ({
      injected,
      failed,
      failures: failures.slice(),
      startedAtMs,
      stoppedAtMs,
    });

    const sendOnce = () => {
      if (stopped) return;
      const inject = w.__ctxE2E?.workspaceStream?.injectMessage;
      if (typeof inject !== "function") {
        failed += 1;
        failures.push("workspace stream injection hook is unavailable");
        return;
      }
      const messageIndex = index;
      index += 1;
      const seq = replayConfig.baseSeq + messageIndex + 1;
      const projectionRev = replayConfig.baseProjectionRev + messageIndex + 1;
      const now = new Date(Date.now()).toISOString();
      const turnId = `synthetic-replay-turn-${replayConfig.stepIndex}-${messageIndex}`;
      const messageId = `synthetic-replay-message-${replayConfig.stepIndex}-${messageIndex}`;
      const turn: SessionTurn = {
        turn_id: turnId,
        session_id: replayConfig.sessionId,
        run_id: null,
        user_message_id: messageId,
        status: "completed",
        start_seq: seq,
        end_seq: seq,
        started_at: now,
        updated_at: now,
        assistant_partial: null,
        thought_partial: null,
        metrics_json: null,
        tool_total: 0,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 0,
        tool_failed: 0,
      };
      const message: Message = {
        id: messageId,
        session_id: replayConfig.sessionId,
        task_id: replayConfig.taskId,
        turn_id: turnId,
        turn_sequence: seq,
        order_seq: seq,
        role: "user",
        content: buildPayload(messageIndex),
        attachments: [],
        delivery: "immediate",
        created_at: now,
      };
      const delta: SessionHeadDelta = {
        session_id: replayConfig.sessionId,
        last_event_seq: seq,
        projection_rev: projectionRev,
        state_rev: projectionRev,
        emitted_at_ms: Date.now(),
        activity: { is_working: false, last_turn_status: "completed" },
        turn,
        message,
        tool_summaries: [],
      };
      const frame = {
        type: "heads_batch",
        rev: replayConfig.streamRevBase + messageIndex + 1,
        snapshot_rev: replayConfig.snapshotRevBase + messageIndex + 1,
        deltas: [delta],
      };
      const ok = inject(JSON.stringify(frame));
      if (ok) {
        injected += 1;
      } else {
        failed += 1;
        failures.push("workspace stream injection hook rejected the frame");
      }
    };

    const timer = window.setInterval(sendOnce, replayConfig.intervalMs);
    sendOnce();
    w.__streamDeltaReplay = {
      getStats: snapshot,
      stop: () => {
        stopped = true;
        window.clearInterval(timer);
        stoppedAtMs = Date.now();
        return snapshot();
      },
    };
    return true;
  }, config);
  expect(started).toBe(true);
}

async function readReplayStats(page: Page): Promise<ReplayStats> {
  const stats = await page.evaluate(() => {
    const replay = (window as ReplayWindow).__streamDeltaReplay;
    return replay?.getStats() ?? null;
  });
  if (!stats) {
    throw new Error("synthetic replay has not started");
  }
  return stats;
}

async function waitForFirstInjected(page: Page, timeoutMs: number): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while ((await readReplayStats(page)).injected < 1 && Date.now() < deadline) {
    await sleep(100);
  }
}

async function stopSyntheticReplay(page: Page): Promise<ReplayStats> {
  const stats = await page.evaluate(() => {
    const replay = (window as ReplayWindow).__streamDeltaReplay;
    if (!replay) return null;
    return replay.stop();
  });
  if (!stats) {
    throw new Error("synthetic replay has not started");
  }
  return stats;
}

async function runReplayStep(
  page: Page,
  sessionId: string,
  taskId: string,
  opts: {
    intervalMs: number;
    stepIndex: number;
    stepDurationMs: number;
    messageBytes: number;
  },
): Promise<ReplayStepMetrics> {
  const before = await readSessionDebug(page, sessionId);
  await readAndClearLongTasks(page);
  const baseSeq = Math.max(before.lastEventSeq, 1_000_000 + opts.stepIndex * 1_000_000);
  const baseProjectionRev = Math.max(
    before.projectionRev,
    1_000_000 + opts.stepIndex * 1_000_000,
  );

  await startSyntheticReplay(page, {
    sessionId,
    taskId,
    intervalMs: opts.intervalMs,
    stepIndex: opts.stepIndex,
    messageBytes: opts.messageBytes,
    baseSeq,
    baseProjectionRev,
    streamRevBase: baseSeq,
    snapshotRevBase: baseSeq,
  });
  const inputText = `synthetic stream delta replay ${opts.stepIndex + 1} stays responsive`;
  let inputSamples: InputLatencySample[] = [];
  let stats = await readReplayStats(page);
  try {
    await waitForFirstInjected(page, 15_000);
    const stepStartedAtMs = Date.now();
    inputSamples = await typeWithLatency(page, inputText);
    const remainingMs = opts.stepDurationMs - (Date.now() - stepStartedAtMs);
    if (remainingMs > 0) {
      await page.waitForTimeout(remainingMs);
    }
  } finally {
    stats = await stopSyntheticReplay(page);
  }

  const targetSeq = baseSeq + stats.injected;
  const after = await waitForSessionEventSeqAtLeast(page, sessionId, targetSeq, 30_000);
  const longTasks = await readAndClearLongTasks(page);
  const elapsedMs = Math.max(1, stats.stoppedAtMs - stats.startedAtMs);
  const elapsedSeconds = elapsedMs / 1000;
  const durations = inputSamples.map((sample) => sample.durationMs);
  const expectedTicks = Math.floor(elapsedMs / opts.intervalMs) + 1;
  const eventSeqDelta = Math.max(0, Math.min(stats.injected, after.lastEventSeq - baseSeq));
  const projectionRevDelta = Math.max(0, Math.min(stats.injected, after.projectionRev - baseProjectionRev));
  const backlogDeltas = Math.max(0, stats.injected - eventSeqDelta);
  const inputTimedOut = inputSamples.some((sample) => sample.timedOut);
  const inputCompletedChars = inputSamples.filter((sample) => !sample.timedOut).length;

  return {
    intervalMs: opts.intervalMs,
    targetDeltasPerSecond: 1000 / opts.intervalMs,
    injectedDeltasPerSecond: stats.injected / elapsedSeconds,
    eventSeqPerSecond: Math.max(0, eventSeqDelta) / elapsedSeconds,
    injected: stats.injected,
    failed: stats.failed,
    failures: stats.failures,
    missedIntervalTicks: Math.max(0, expectedTicks - stats.injected - stats.failed),
    eventSeqDelta,
    projectionRevDelta,
    expectedEventSeqDelta: stats.injected,
    backlogDeltas,
    catchupTimedOut: eventSeqDelta < stats.injected,
    inputSamples,
    inputRequestedChars: inputText.length,
    inputCompletedChars,
    inputTimedOut,
    p95InputMs: percentile(durations, 0.95),
    maxInputMs: Math.max(0, ...durations),
    longTasks,
    maxLongTaskMs: Math.max(0, ...longTasks.map((entry) => entry.duration)),
  };
}

test("workbench: synthetic stream-delta replay keeps active session input responsive", async ({ page, request }, testInfo) => {
  await page.setViewportSize({ width: 1400, height: 900 });

  const seededTurns = readPositiveIntEnv("CTX_STREAM_DELTA_REPLAY_TURNS", 180);
  const messageBytes = readPositiveIntEnv("CTX_STREAM_DELTA_REPLAY_MESSAGE_BYTES", 900);
  const stepDurationMs = readPositiveIntEnv("CTX_STREAM_DELTA_REPLAY_STEP_DURATION_MS", 6_000);
  const maxP95InputMs = readPositiveIntEnv("CTX_STREAM_DELTA_REPLAY_MAX_P95_INPUT_MS", 500);
  const maxInputMs = readPositiveIntEnv("CTX_STREAM_DELTA_REPLAY_MAX_INPUT_MS", 1_500);
  const maxLongTaskMs = readPositiveIntEnv("CTX_STREAM_DELTA_REPLAY_MAX_LONG_TASK_MS", 750);
  const minInjectedRatio = readRatioEnv("CTX_STREAM_DELTA_REPLAY_MIN_INJECTED_RATIO", 0.5);
  const intervalsMs = readIntervalLevels();

  const seed = await seedThroughputSession(request, { seededTurns, messageBytes });

  await page.goto(`/workspaces/${seed.workspaceId}?ctxE2E=1&loadtest=1`, { waitUntil: "domcontentloaded" });
  await expect(page.locator(".wb-task-row")).toHaveCount(1, { timeout: 20_000 });
  const focused = await page.evaluate(
    ({ focusTaskId, focusSessionId }) => window.__ctxE2E?.focusTask?.(focusTaskId, focusSessionId) ?? false,
    { focusTaskId: seed.taskId, focusSessionId: seed.sessionId },
  );
  expect(focused).toBe(true);
  await waitForVisibleSession(page, seed.sessionId);
  await ensureReplayInjectionReady(page);
  await expect(page.locator(composerSelector)).toBeVisible({ timeout: 20_000 });
  await installLongTaskProbe(page);

  const steps: ReplayStepMetrics[] = [];
  for (const [stepIndex, intervalMs] of intervalsMs.entries()) {
    const step = await runReplayStep(page, seed.sessionId, seed.taskId, {
      intervalMs,
      stepIndex,
      stepDurationMs,
      messageBytes,
    });
    steps.push(step);
  }

  const metrics: ReplayBenchmarkMetrics = {
    seededTurns,
    messageBytes,
    stepDurationMs,
    steps,
  };

  await testInfo.attach("stream-delta-replay-throughput-metrics.json", {
    body: JSON.stringify(metrics, null, 2),
    contentType: "application/json",
  });

  console.log(`[stream-delta-replay-throughput] ${JSON.stringify(metrics)}`);

  for (const step of steps) {
    const expectedMinimumInjected = Math.max(
      1,
      Math.floor((stepDurationMs / step.intervalMs) * minInjectedRatio),
    );
    expect(step.failures).toEqual([]);
    expect(step.failed).toBe(0);
    expect(step.injected).toBeGreaterThanOrEqual(expectedMinimumInjected);
    expect(step.catchupTimedOut).toBe(false);
    expect(step.backlogDeltas).toBe(0);
    expect(step.eventSeqDelta).toBeGreaterThanOrEqual(step.injected);
    expect(step.projectionRevDelta).toBeGreaterThanOrEqual(step.injected);
    expect(step.inputTimedOut).toBe(false);
    expect(step.inputCompletedChars).toBe(step.inputRequestedChars);
    expect(step.p95InputMs).toBeLessThanOrEqual(maxP95InputMs);
    expect(step.maxInputMs).toBeLessThanOrEqual(maxInputMs);
    expect(step.maxLongTaskMs).toBeLessThanOrEqual(maxLongTaskMs);
  }
});
