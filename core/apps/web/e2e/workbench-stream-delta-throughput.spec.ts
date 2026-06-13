import type { APIRequestContext, Page } from "playwright/test";
import { test, expect } from "./fixtures";
import { seedDummyWorkspace } from "./utils/seedDummyWorkspace";
import {
  attachWorkspaceStreamShapeCapture,
  type StreamDeltaShapeStats,
} from "./utils/workspaceStreamShapeCapture";

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

type ChurnStats = {
  sent: number;
  skippedTicks: number;
  failures: string[];
  startedAtMs: number;
  stoppedAtMs: number;
};

type StepMetrics = {
  intervalMs: number;
  targetMessagesPerSecond: number;
  achievedMessagesPerSecond: number;
  eventSeqPerSecond: number;
  sent: number;
  skippedTicks: number;
  failures: string[];
  eventSeqDelta: number;
  projectionRevDelta: number;
  eventSeqAdvanced: boolean;
  debugReadTimedOut: boolean;
  inputSamples: InputLatencySample[];
  inputRequestedChars: number;
  inputCompletedChars: number;
  inputTimedOut: boolean;
  p95InputMs: number;
  maxInputMs: number;
  longTasks: LongTaskSample[];
  maxLongTaskMs: number;
};

type BenchmarkMetrics = {
  seededTurns: number;
  messageBytes: number;
  stepDurationMs: number;
  maxInFlight: number;
  streamShape: StreamDeltaShapeStats;
  steps: StepMetrics[];
};

type StreamDeltaPerfWindow = Window & {
  __streamDeltaPerf?: {
    longTasks: LongTaskSample[];
    observer?: PerformanceObserver;
  };
};

const composerSelector = ".wb-session-slot[aria-hidden=\"false\"] textarea.wb-active-textarea";
const visibleSessionSelector = ".wb-session-slot[aria-hidden=\"false\"]";
const inputEchoTimeoutMs = 5_000;

test.setTimeout(240_000);
test.skip(
  ({ browserName }) => browserName !== "chromium",
  "nightly stream-delta throughput probe is calibrated for Chromium",
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
  const raw = process.env.CTX_STREAM_DELTA_PERF_INTERVALS_MS;
  if (!raw) return [500];
  const levels = raw
    .split(",")
    .map((part) => part.trim())
    .filter((part) => part.length > 0)
    .map((part) => {
      const parsed = Number.parseInt(part, 10);
      if (!Number.isFinite(parsed) || parsed <= 0) {
        throw new Error(`CTX_STREAM_DELTA_PERF_INTERVALS_MS contains an invalid interval: ${part}`);
      }
      return parsed;
    });
  if (levels.length === 0) {
    throw new Error("CTX_STREAM_DELTA_PERF_INTERVALS_MS must contain at least one interval");
  }
  return levels;
}

function percentile(values: readonly number[], p: number): number {
  if (values.length === 0) return 0;
  const sorted = values.slice().sort((left, right) => left - right);
  const index = Math.min(sorted.length - 1, Math.max(0, Math.ceil(sorted.length * p) - 1));
  return sorted[index] ?? 0;
}

function buildPayload(stepIndex: number, messageIndex: number, targetBytes: number): string {
  const base = `stream-delta-throughput step=${stepIndex + 1} message=${messageIndex + 1}`;
  if (base.length >= targetBytes) return base;
  return `${base}\n${"x".repeat(targetBytes - base.length - 1)}`;
}

async function postMessage(
  request: APIRequestContext,
  sessionId: string,
  content: string,
): Promise<void> {
  const resp = await request.post(`/api/sessions/${sessionId}/messages`, {
    data: {
      content,
      delivery: "immediate",
    },
  });
  if (!resp.ok()) {
    throw new Error(`message send failed for ${sessionId}: ${resp.status()}`);
  }
}

function startLiveChurn(
  request: APIRequestContext,
  sessionId: string,
  opts: {
    intervalMs: number;
    maxInFlight: number;
    messageBytes: number;
    stepIndex: number;
  },
): { stop: () => Promise<ChurnStats>; getStats: () => ChurnStats } {
  let stopped = false;
  let tick = 0;
  let sent = 0;
  let skippedTicks = 0;
  const failures: string[] = [];
  const inflight = new Set<Promise<void>>();
  const startedAtMs = Date.now();
  let stoppedAtMs = startedAtMs;

  const sendOnce = () => {
    if (stopped) return;
    if (inflight.size >= opts.maxInFlight) {
      skippedTicks += 1;
      return;
    }
    const messageIndex = tick;
    tick += 1;
    const sendPromise = postMessage(
      request,
      sessionId,
      buildPayload(opts.stepIndex, messageIndex, opts.messageBytes),
    )
      .then(() => {
        sent += 1;
      })
      .catch((error: unknown) => {
        const message = error instanceof Error && error.message ? error.message : String(error);
        failures.push(message);
      })
      .finally(() => {
        inflight.delete(sendPromise);
      });
    inflight.add(sendPromise);
  };

  const timer = setInterval(sendOnce, opts.intervalMs);
  sendOnce();

  const snapshot = (): ChurnStats => ({
    sent,
    skippedTicks,
    failures: failures.slice(),
    startedAtMs,
    stoppedAtMs,
  });

  return {
    getStats: snapshot,
    stop: async () => {
      stopped = true;
      clearInterval(timer);
      await Promise.allSettled(Array.from(inflight));
      stoppedAtMs = Date.now();
      return snapshot();
    },
  };
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
    const w = window as StreamDeltaPerfWindow;
    w.__streamDeltaPerf?.observer?.disconnect();
    const state: NonNullable<StreamDeltaPerfWindow["__streamDeltaPerf"]> = { longTasks: [] };
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
    const w = window as StreamDeltaPerfWindow;
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
    const bridge = window.__ctxE2E;
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

async function readSessionDebugWithin(
  page: Page,
  sessionId: string,
  timeoutMs: number,
): Promise<SessionDebug | null> {
  const timeout = sleep(timeoutMs).then(() => null);
  return Promise.race([readSessionDebug(page, sessionId), timeout]);
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
): Promise<{ debug: SessionDebug; readTimedOut: boolean }> {
  const deadline = Date.now() + timeoutMs;
  let debug = await readSessionDebugWithin(page, sessionId, 2_000);
  if (!debug) {
    return { debug: { lastEventSeq: 0, projectionRev: 0 }, readTimedOut: true };
  }
  while (debug.lastEventSeq < targetSeq && Date.now() < deadline) {
    await sleep(100);
    const next = await readSessionDebugWithin(page, sessionId, 2_000);
    if (!next) {
      return { debug, readTimedOut: true };
    }
    debug = next;
  }
  return { debug, readTimedOut: false };
}

async function waitForFirstSent(churn: { getStats: () => ChurnStats }, timeoutMs: number): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (churn.getStats().sent < 1 && Date.now() < deadline) {
    await sleep(100);
  }
}

async function runThroughputStep(
  page: Page,
  request: APIRequestContext,
  sessionId: string,
  opts: {
    intervalMs: number;
    stepIndex: number;
    stepDurationMs: number;
    maxInFlight: number;
    messageBytes: number;
  },
): Promise<StepMetrics> {
  const before = await readSessionDebugWithin(page, sessionId, 2_000);
  if (!before) {
    return {
      intervalMs: opts.intervalMs,
      targetMessagesPerSecond: 1000 / opts.intervalMs,
      achievedMessagesPerSecond: 0,
      eventSeqPerSecond: 0,
      sent: 0,
      skippedTicks: 0,
      failures: ["session debug read timed out before starting throughput step"],
      eventSeqDelta: 0,
      projectionRevDelta: 0,
      eventSeqAdvanced: false,
      debugReadTimedOut: true,
      inputSamples: [],
      inputRequestedChars: 0,
      inputCompletedChars: 0,
      inputTimedOut: false,
      p95InputMs: 0,
      maxInputMs: 0,
      longTasks: [],
      maxLongTaskMs: 0,
    };
  }
  await readAndClearLongTasks(page);

  const churn = startLiveChurn(request, sessionId, {
    intervalMs: opts.intervalMs,
    maxInFlight: opts.maxInFlight,
    messageBytes: opts.messageBytes,
    stepIndex: opts.stepIndex,
  });

  const inputText = `stream delta throughput ${opts.stepIndex + 1} stays responsive`;
  let inputSamples: InputLatencySample[] = [];
  let stats = churn.getStats();
  try {
    await waitForFirstSent(churn, 15_000);
    const stepStartedAtMs = Date.now();
    inputSamples = await typeWithLatency(page, inputText);
    const remainingMs = opts.stepDurationMs - (Date.now() - stepStartedAtMs);
    if (remainingMs > 0) {
      await page.waitForTimeout(remainingMs);
    }
  } finally {
    stats = await churn.stop();
  }
  const afterResult = await waitForSessionEventSeqAtLeast(page, sessionId, before.lastEventSeq + 1, 20_000);
  const after = afterResult.debug;
  const longTasks = afterResult.readTimedOut ? [] : await readAndClearLongTasks(page);
  const elapsedSeconds = Math.max(0.001, (stats.stoppedAtMs - stats.startedAtMs) / 1000);
  const durations = inputSamples.map((sample) => sample.durationMs);
  const inputRequestedChars = inputText.length;
  const inputCompletedChars = inputSamples.filter((sample) => !sample.timedOut).length;
  const inputTimedOut = inputSamples.some((sample) => sample.timedOut);
  const eventSeqDelta = Math.max(0, after.lastEventSeq - before.lastEventSeq);
  const projectionRevDelta = Math.max(0, after.projectionRev - before.projectionRev);

  return {
    intervalMs: opts.intervalMs,
    targetMessagesPerSecond: 1000 / opts.intervalMs,
    achievedMessagesPerSecond: stats.sent / elapsedSeconds,
    eventSeqPerSecond: eventSeqDelta / elapsedSeconds,
    sent: stats.sent,
    skippedTicks: stats.skippedTicks,
    failures: stats.failures,
    eventSeqDelta,
    projectionRevDelta,
    eventSeqAdvanced: after.lastEventSeq > before.lastEventSeq,
    debugReadTimedOut: afterResult.readTimedOut,
    inputSamples,
    inputRequestedChars,
    inputCompletedChars,
    inputTimedOut,
    p95InputMs: percentile(durations, 0.95),
    maxInputMs: Math.max(0, ...durations),
    longTasks,
    maxLongTaskMs: Math.max(0, ...longTasks.map((entry) => entry.duration)),
  };
}

test("workbench: stream-delta throughput keeps active session input responsive", async ({ page, request }, testInfo) => {
  await page.setViewportSize({ width: 1400, height: 900 });
  const streamShape = attachWorkspaceStreamShapeCapture(page);

  const seededTurns = readPositiveIntEnv("CTX_STREAM_DELTA_PERF_TURNS", 180);
  const messageBytes = readPositiveIntEnv("CTX_STREAM_DELTA_PERF_MESSAGE_BYTES", 900);
  const stepDurationMs = readPositiveIntEnv("CTX_STREAM_DELTA_PERF_STEP_DURATION_MS", 6_000);
  const maxInFlight = readPositiveIntEnv("CTX_STREAM_DELTA_PERF_MAX_IN_FLIGHT", 4);
  const maxP95InputMs = readPositiveIntEnv("CTX_STREAM_DELTA_PERF_MAX_P95_INPUT_MS", 500);
  const maxInputMs = readPositiveIntEnv("CTX_STREAM_DELTA_PERF_MAX_INPUT_MS", 1_500);
  const maxLongTaskMs = readPositiveIntEnv("CTX_STREAM_DELTA_PERF_MAX_LONG_TASK_MS", 750);
  const minSentRatio = readRatioEnv("CTX_STREAM_DELTA_PERF_MIN_SENT_RATIO", 0.25);
  const intervalsMs = readIntervalLevels();

  const seed = await seedDummyWorkspace(request, {
    tasks: 1,
    sessionsPerTask: 1,
    turnsPerSession: seededTurns,
    seedTranscriptDirect: true,
    messagePrefix: "stream-delta-throughput seed",
    messageBytes: { min: messageBytes, max: messageBytes + 256 },
    messageBodyLines: { min: 4, max: 8 },
    includeToolSummaries: true,
    toolSummariesPerTurn: 3,
  });
  const taskId = seed.taskIds[0] ?? "";
  const sessionId = taskId ? seed.sessionIdsByTask[taskId]?.[0] ?? "" : "";
  if (!taskId || !sessionId) {
    throw new Error("failed to resolve seeded throughput session");
  }

  await page.goto(`/workspaces/${seed.workspaceId}?ctxE2E=1&loadtest=1`, { waitUntil: "domcontentloaded" });
  await expect(page.locator(".wb-task-row")).toHaveCount(1, { timeout: 20_000 });
  const focused = await page.evaluate(
    ({ focusTaskId, focusSessionId }) => window.__ctxE2E?.focusTask?.(focusTaskId, focusSessionId) ?? false,
    { focusTaskId: taskId, focusSessionId: sessionId },
  );
  expect(focused).toBe(true);
  await waitForVisibleSession(page, sessionId);
  await expect(page.locator(composerSelector)).toBeVisible({ timeout: 20_000 });
  await installLongTaskProbe(page);

  const steps: StepMetrics[] = [];
  for (const [stepIndex, intervalMs] of intervalsMs.entries()) {
    const step = await runThroughputStep(page, request, sessionId, {
      intervalMs,
      stepIndex,
      stepDurationMs,
      maxInFlight,
      messageBytes,
    });
    steps.push(step);
    if (step.debugReadTimedOut || step.inputTimedOut) {
      break;
    }
  }

  const metrics: BenchmarkMetrics = {
    seededTurns,
    messageBytes,
    stepDurationMs,
    maxInFlight,
    streamShape,
    steps,
  };

  await testInfo.attach("stream-delta-throughput-metrics.json", {
    body: JSON.stringify(metrics, null, 2),
    contentType: "application/json",
  });

  console.log(`[stream-delta-throughput] ${JSON.stringify(metrics)}`);

  for (const step of steps) {
    const expectedMinimumSent = Math.max(
      1,
      Math.floor((stepDurationMs / step.intervalMs) * minSentRatio),
    );
    expect(step.failures).toEqual([]);
    expect(step.sent).toBeGreaterThanOrEqual(expectedMinimumSent);
    expect(step.eventSeqDelta).toBeGreaterThan(0);
    expect(step.projectionRevDelta).toBeGreaterThan(0);
    expect(step.eventSeqAdvanced).toBe(true);
    expect(step.debugReadTimedOut).toBe(false);
    expect(step.inputTimedOut).toBe(false);
    expect(step.inputCompletedChars).toBe(step.inputRequestedChars);
    expect(step.p95InputMs).toBeLessThanOrEqual(maxP95InputMs);
    expect(step.maxInputMs).toBeLessThanOrEqual(maxInputMs);
    expect(step.maxLongTaskMs).toBeLessThanOrEqual(maxLongTaskMs);
  }
  expect(streamShape.violations).toEqual([]);
});
