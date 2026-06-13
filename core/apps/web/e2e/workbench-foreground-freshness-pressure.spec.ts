import fs from "fs/promises";
import type { APIRequestContext } from "playwright/test";
import { test, expect } from "./fixtures";
import { clearDiagnostics, getDiagnostics } from "./utils/diagnostics";
import { seedDummyWorkspace, startStreamingMessages } from "./utils/seedDummyWorkspace";

const PROBE_ENABLED = process.env.CTX_FOREGROUND_FRESHNESS_PROBE === "1";
const LAG_PROOF_ENABLED = process.env.CTX_FOREGROUND_LAG_PROOF === "1";
const GUARDRAIL_ENABLED = process.env.CTX_FOREGROUND_FRESHNESS_GUARDRAIL === "1";
const TASK_COUNT = Number(process.env.CTX_FOREGROUND_FRESHNESS_TASKS ?? "10");
const TURNS_PER_SESSION = Number(process.env.CTX_FOREGROUND_FRESHNESS_TURNS ?? "2");
const MESSAGE_BYTES = Number(process.env.CTX_FOREGROUND_FRESHNESS_MESSAGE_BYTES ?? "2200");
const BACKGROUND_WAVES = Number(process.env.CTX_FOREGROUND_FRESHNESS_BACKGROUND_WAVES ?? "2");
const BACKGROUND_WAVE_DELAY_MS = Number(
  process.env.CTX_FOREGROUND_FRESHNESS_BACKGROUND_WAVE_DELAY_MS ?? "1200",
);
const BACKGROUND_STREAM_INTERVAL_MS = Number(
  process.env.CTX_FOREGROUND_FRESHNESS_BACKGROUND_STREAM_INTERVAL_MS ?? "0",
);
const BACKGROUND_STREAM_DURATION_MS = Number(
  process.env.CTX_FOREGROUND_FRESHNESS_BACKGROUND_STREAM_DURATION_MS ?? "0",
);
const PROMPT_BODY_LINES = Number(process.env.CTX_FOREGROUND_FRESHNESS_PROMPT_BODY_LINES ?? "90");
const PROBE_RUNS = Number(process.env.CTX_FOREGROUND_FRESHNESS_PROBE_RUNS ?? "3");
const HEAD_POLL_MS = Number(process.env.CTX_FOREGROUND_FRESHNESS_HEAD_POLL_MS ?? "50");
const MAX_BACKEND_TO_DOM_MS = Number(process.env.CTX_FOREGROUND_FRESHNESS_MAX_BACKEND_TO_DOM_MS ?? "0");
const LAG_PROOF_STREAMERS = Number(process.env.CTX_FOREGROUND_LAG_PROOF_STREAMERS ?? "3");
const LAG_PROOF_STREAM_INTERVAL_MS = Number(
  process.env.CTX_FOREGROUND_LAG_PROOF_STREAM_INTERVAL_MS
    ?? process.env.CTX_FOREGROUND_FRESHNESS_BACKGROUND_STREAM_INTERVAL_MS
    ?? "5",
);
const LAG_PROOF_STREAM_DURATION_MS = Number(
  process.env.CTX_FOREGROUND_LAG_PROOF_STREAM_DURATION_MS
    ?? process.env.CTX_FOREGROUND_FRESHNESS_BACKGROUND_STREAM_DURATION_MS
    ?? "30000",
);
const LAG_PROOF_PRESSURE_SETTLE_MS = Number(
  process.env.CTX_FOREGROUND_LAG_PROOF_PRESSURE_SETTLE_MS ?? "2000",
);
const LAG_PROOF_OVERLOAD_VISIBILITY_TIMEOUT_MS = Number(
  process.env.CTX_FOREGROUND_LAG_PROOF_OVERLOAD_VISIBILITY_TIMEOUT_MS ?? "45000",
);
const LAG_PROOF_RECOVERY_VISIBILITY_TIMEOUT_MS = Number(
  process.env.CTX_FOREGROUND_LAG_PROOF_RECOVERY_VISIBILITY_TIMEOUT_MS ?? "20000",
);
const LAG_PROOF_OVERLOAD_WINDOW_MS = Number(
  process.env.CTX_FOREGROUND_LAG_PROOF_OVERLOAD_WINDOW_MS ?? "15000",
);
const LAG_PROOF_RECOVERY_WINDOW_MS = Number(
  process.env.CTX_FOREGROUND_LAG_PROOF_RECOVERY_WINDOW_MS ?? "5000",
);
const LAG_PROOF_RECOVERY_TIMEOUT_MS = Number(
  process.env.CTX_FOREGROUND_LAG_PROOF_RECOVERY_TIMEOUT_MS ?? "15000",
);
const LAG_PROOF_MIN_OVERLOAD_FOREGROUND_QUEUE_MS = Number(
  process.env.CTX_FOREGROUND_LAG_PROOF_MIN_OVERLOAD_FOREGROUND_QUEUE_MS ?? "500",
);
const LAG_PROOF_MIN_OVERLOAD_WORKSPACE_QUEUE_MS = Number(
  process.env.CTX_FOREGROUND_LAG_PROOF_MIN_OVERLOAD_WORKSPACE_QUEUE_MS ?? "250",
);
const LAG_PROOF_MAX_RECOVERY_FOREGROUND_QUEUE_P95_MS = Number(
  process.env.CTX_FOREGROUND_LAG_PROOF_MAX_RECOVERY_FOREGROUND_QUEUE_P95_MS ?? "100",
);
const LAG_PROOF_MAX_RECOVERY_WORKSPACE_QUEUE_P95_MS = Number(
  process.env.CTX_FOREGROUND_LAG_PROOF_MAX_RECOVERY_WORKSPACE_QUEUE_P95_MS ?? "250",
);
const LAG_PROOF_MAX_RECOVERY_BACKEND_TO_DOM_MS = Number(
  process.env.CTX_FOREGROUND_LAG_PROOF_MAX_RECOVERY_BACKEND_TO_DOM_MS ?? "2000",
);
const LAG_PROOF_MIN_OVERLOAD_BACKEND_TO_DOM_MS = Number(
  process.env.CTX_FOREGROUND_LAG_PROOF_MIN_OVERLOAD_BACKEND_TO_DOM_MS ?? "1000",
);

type SessionHeadResponse = {
  turns?: Array<{
    turn_id?: string;
    user_message_id?: string;
    status?: string;
  }>;
  messages?: Array<{
    id?: string;
    turn_id?: string;
    role?: string;
    content?: string;
  }>;
};

type SlowPromptTool = {
  kind: string;
  title: string;
  input: Record<string, string>;
  output_text: string;
};

type TelemetryMetricSummary = {
  name?: string;
  count?: number;
  sum?: number;
  min?: number | null;
  max?: number | null;
  p50?: number | null;
  p95?: number | null;
  p99?: number | null;
  labels?: Record<string, string>;
};

type TelemetrySummaryResponse = {
  metrics?: TelemetryMetricSummary[];
  window_ms?: number | null;
};

type MetricRollup = {
  count: number;
  sum: number;
  min: number | null;
  max: number | null;
  p50: number | null;
  p95: number | null;
  p99: number | null;
};

type LagProbeOutcome = {
  marker: string;
  turnId: string;
  backendReadyAtMs: number;
  domVisibleAtMs: number | null;
  backendToDomMs: number | null;
  timedOut: boolean;
};

const slowPromptBody = Array.from(
  { length: PROMPT_BODY_LINES },
  (_, index) => `foreground freshness body line ${index + 1}`,
).join("\n");

function percentile(values: number[], p: number): number | null {
  if (values.length === 0) return null;
  const sorted = values.slice().sort((left, right) => left - right);
  const index = Math.min(sorted.length - 1, Math.max(0, Math.ceil(sorted.length * p) - 1));
  return Math.round(sorted[index]! * 10) / 10;
}

const buildSlowPrompt = (marker: string, label: string): string => {
  const tools: SlowPromptTool[] = Array.from({ length: 4 }, (_, index) => ({
    kind: "execute",
    title: `${label} tool ${index + 1}`,
    input: { command: `printf '${label}-${index + 1}'` },
    output_text: `${label} output ${index + 1}`,
  }));
  return `slow-diff-test stream-assistant-partials emit-thought ${marker}
${slowPromptBody}
[[tool_calls]]
${JSON.stringify(tools)}
[[/tool_calls]]`;
};

const buildQuickPrompt = (marker: string): string => `quick-lag-proof ${marker}`;

async function pageWait(ms: number): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, ms));
}

async function sendSessionMessage(
  request: APIRequestContext,
  sessionId: string,
  content: string,
): Promise<void> {
  const response = await request.post(`/api/sessions/${sessionId}/messages`, {
    data: {
      content,
      delivery: "immediate",
    },
  });
  expect(response.ok(), `message send failed: ${response.url()}`).toBeTruthy();
}

async function waitForForegroundTurnCompletion(
  request: APIRequestContext,
  sessionId: string,
  marker: string,
  timeoutMs: number,
): Promise<{ backendReadyAtMs: number; turnId: string }> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const response = await request.get(`/api/sessions/${sessionId}/head`);
    expect(response.ok(), `head request failed: ${response.url()}`).toBeTruthy();
    const payload = (await response.json()) as SessionHeadResponse;
    const messages = Array.isArray(payload.messages) ? payload.messages : [];
    const turns = Array.isArray(payload.turns) ? payload.turns : [];
    const userMessage = messages.find(
      (message) =>
        message?.role === "user" &&
        typeof message?.content === "string" &&
        message.content.includes(marker),
    );
    if (userMessage?.id) {
      const turn = turns.find((entry) => entry?.user_message_id === userMessage.id);
      const assistantMessage = messages.find(
        (message) =>
          message?.turn_id === turn?.turn_id &&
          message?.role === "assistant" &&
          typeof message?.content === "string" &&
          message.content.includes(marker),
      );
      const status = String(turn?.status ?? "").toLowerCase();
      if (assistantMessage?.content && (status === "completed" || status === "done")) {
        return {
          backendReadyAtMs: Date.now(),
          turnId: String(turn?.turn_id ?? ""),
        };
      }
    }
    await pageWait(HEAD_POLL_MS);
  }
  throw new Error(`foreground turn did not complete for marker ${marker}`);
}

async function runBackgroundPressure(
  request: APIRequestContext,
  sessionIds: string[],
  runNumber: number,
): Promise<void> {
  for (let wave = 0; wave < BACKGROUND_WAVES; wave += 1) {
    await Promise.all(
      sessionIds.map((sessionId, index) =>
        sendSessionMessage(
          request,
          sessionId,
          buildSlowPrompt(
            `background-pressure-${runNumber}-${wave + 1}-${index + 1}`,
            `bg-${runNumber}-${wave + 1}-${index + 1}`,
          ),
        ),
      ),
    );
    if (wave < BACKGROUND_WAVES - 1) {
      await pageWait(BACKGROUND_WAVE_DELAY_MS);
    }
  }
}

async function readTelemetryMetric(
  request: APIRequestContext,
  metric: string,
  windowMs: number,
): Promise<MetricRollup> {
  const response = await request.get(
    `/api/telemetry/summary?metric=${encodeURIComponent(metric)}&window_ms=${windowMs}`,
  );
  expect(response.ok(), `telemetry summary failed: ${response.url()}`).toBeTruthy();
  const payload = (await response.json()) as TelemetrySummaryResponse;
  const metrics = Array.isArray(payload.metrics) ? payload.metrics : [];
  const numericValues = (key: keyof MetricRollup) =>
    metrics
      .map((entry) => entry[key as keyof TelemetryMetricSummary])
      .filter((value): value is number => typeof value === "number" && Number.isFinite(value));
  const counts = numericValues("count");
  const sums = numericValues("sum");
  const mins = numericValues("min");
  const maxes = numericValues("max");
  const p50s = numericValues("p50");
  const p95s = numericValues("p95");
  const p99s = numericValues("p99");
  return {
    count: counts.reduce((total, value) => total + value, 0),
    sum: sums.reduce((total, value) => total + value, 0),
    min: mins.length > 0 ? Math.min(...mins) : null,
    max: maxes.length > 0 ? Math.max(...maxes) : null,
    p50: p50s.length > 0 ? Math.max(...p50s) : null,
    p95: p95s.length > 0 ? Math.max(...p95s) : null,
    p99: p99s.length > 0 ? Math.max(...p99s) : null,
  };
}

function rollupUpperBound(rollup: MetricRollup): number {
  if (rollup.count === 0) return 0;
  return rollup.p95 ?? rollup.max ?? 0;
}

async function readLoadTestSnapshot(page: Parameters<typeof test>[0]["page"]) {
  return page.evaluate(() => window.__ctxLoadTestTelemetry?.getSnapshot?.() ?? null);
}

function summarizeBrowserLoadTest(
  loadTest:
    | {
      long_tasks?: Array<{ duration_ms?: number }>;
      memory_samples?: Array<{ used_js_heap_size?: number }>;
    }
    | null
    | undefined,
) {
  const longTasks = Array.isArray(loadTest?.long_tasks) ? loadTest.long_tasks : [];
  const memorySamples = Array.isArray(loadTest?.memory_samples) ? loadTest.memory_samples : [];
  const usedHeapSizes = memorySamples
    .map((sample) => sample?.used_js_heap_size)
    .filter((value): value is number => typeof value === "number" && Number.isFinite(value));
  return {
    longTaskCount: longTasks.length,
    maxLongTaskMs: longTasks.reduce((max, entry) => Math.max(max, entry?.duration_ms ?? 0), 0),
    peakUsedJsHeapSize: usedHeapSizes.length > 0 ? Math.max(...usedHeapSizes) : 0,
    lastUsedJsHeapSize: usedHeapSizes.length > 0 ? usedHeapSizes[usedHeapSizes.length - 1] ?? 0 : 0,
  };
}

function startLagProofStreamers(
  request: APIRequestContext,
  sessionIds: string[],
): Array<{ stop: () => Promise<void> }> {
  return Array.from({ length: Math.max(1, LAG_PROOF_STREAMERS) }, (_, index) =>
    startStreamingMessages(request, {
      sessionIds,
      intervalMs: LAG_PROOF_STREAM_INTERVAL_MS,
      durationMs: LAG_PROOF_STREAM_DURATION_MS,
      messageBytes: MESSAGE_BYTES,
      includeToolSummaries: true,
      toolSummariesPerTurn: 4,
      messagePrefix: `lag-proof-stream-${index + 1}`,
    }),
  );
}

function formatUnknownError(error: unknown): string {
  if (error instanceof Error) return error.message;
  return String(error);
}

async function runForegroundLagProbe(
  page: Parameters<typeof test>[0]["page"],
  request: APIRequestContext,
  sessionId: string,
  marker: string,
  timeoutMs: number,
  content: string,
): Promise<LagProbeOutcome> {
  await sendSessionMessage(request, sessionId, content);
  const { backendReadyAtMs, turnId } = await waitForForegroundTurnCompletion(
    request,
    sessionId,
    marker,
    timeoutMs,
  );
  try {
    const domVisibleAtHandle = await page.waitForFunction(
      ({ text }) => {
        const session = document.querySelector('.wb-session-slot[aria-hidden="false"]');
        return session?.textContent?.includes(text) ? Date.now() : null;
      },
      { text: marker },
      { timeout: timeoutMs },
    );
    const domVisibleAt = await domVisibleAtHandle.jsonValue();
    expect(typeof domVisibleAt).toBe("number");
    return {
      marker,
      turnId,
      backendReadyAtMs,
      domVisibleAtMs: Number(domVisibleAt),
      backendToDomMs: Number(domVisibleAt) - backendReadyAtMs,
      timedOut: false,
    };
  } catch {
    return {
      marker,
      turnId,
      backendReadyAtMs,
      domVisibleAtMs: null,
      backendToDomMs: null,
      timedOut: true,
    };
  }
}

async function waitForBacklogRecovery(
  request: APIRequestContext,
): Promise<{
  settledWithinMs: number | null;
  foregroundBacklog: MetricRollup;
  workspaceBacklog: MetricRollup;
  gapRecoveryMs: MetricRollup;
  gapRecoveryTimeoutCount: MetricRollup;
  workspaceStreamResetCount: MetricRollup;
}> {
  const startedAt = Date.now();
  let foregroundBacklog = await readTelemetryMetric(
    request,
    "workbench.foreground_queue_age_ms",
    LAG_PROOF_RECOVERY_WINDOW_MS,
  );
  let workspaceBacklog = await readTelemetryMetric(
    request,
    "workbench.workspace_backlog_age_ms",
    LAG_PROOF_RECOVERY_WINDOW_MS,
  );
  let gapRecoveryMs = await readTelemetryMetric(
    request,
    "workbench.foreground_gap_recovery_ms",
    LAG_PROOF_RECOVERY_WINDOW_MS,
  );
  let gapRecoveryTimeoutCount = await readTelemetryMetric(
    request,
    "workbench.foreground_gap_recovery_timeout_count",
    LAG_PROOF_RECOVERY_WINDOW_MS,
  );
  let workspaceStreamResetCount = await readTelemetryMetric(
    request,
    "workbench.workspace_stream_reset_count",
    LAG_PROOF_RECOVERY_WINDOW_MS,
  );
  while (Date.now() - startedAt < LAG_PROOF_RECOVERY_TIMEOUT_MS) {
    const foregroundP95 = rollupUpperBound(foregroundBacklog);
    const workspaceP95 = rollupUpperBound(workspaceBacklog);
    if (
      foregroundP95 <= LAG_PROOF_MAX_RECOVERY_FOREGROUND_QUEUE_P95_MS &&
      workspaceP95 <= LAG_PROOF_MAX_RECOVERY_WORKSPACE_QUEUE_P95_MS
    ) {
      return {
        settledWithinMs: Date.now() - startedAt,
        foregroundBacklog,
        workspaceBacklog,
        gapRecoveryMs,
        gapRecoveryTimeoutCount,
        workspaceStreamResetCount,
      };
    }
    await pageWait(500);
    foregroundBacklog = await readTelemetryMetric(
      request,
      "workbench.foreground_queue_age_ms",
      LAG_PROOF_RECOVERY_WINDOW_MS,
    );
    workspaceBacklog = await readTelemetryMetric(
      request,
      "workbench.workspace_backlog_age_ms",
      LAG_PROOF_RECOVERY_WINDOW_MS,
    );
    gapRecoveryMs = await readTelemetryMetric(
      request,
      "workbench.foreground_gap_recovery_ms",
      LAG_PROOF_RECOVERY_WINDOW_MS,
    );
    gapRecoveryTimeoutCount = await readTelemetryMetric(
      request,
      "workbench.foreground_gap_recovery_timeout_count",
      LAG_PROOF_RECOVERY_WINDOW_MS,
    );
    workspaceStreamResetCount = await readTelemetryMetric(
      request,
      "workbench.workspace_stream_reset_count",
      LAG_PROOF_RECOVERY_WINDOW_MS,
    );
  }
  return {
    settledWithinMs: null,
    foregroundBacklog,
    workspaceBacklog,
    gapRecoveryMs,
    gapRecoveryTimeoutCount,
    workspaceStreamResetCount,
  };
}

test.use({ browserName: "chromium" });

test("workbench: foreground final freshness stays tight under background pressure", async ({
  page,
  request,
}, testInfo) => {
  test.skip(
    !PROBE_ENABLED && !GUARDRAIL_ENABLED,
    "Set CTX_FOREGROUND_FRESHNESS_PROBE=1 or CTX_FOREGROUND_FRESHNESS_GUARDRAIL=1 to run the foreground freshness pressure probe.",
  );
  test.setTimeout(300_000);

  const seed = await seedDummyWorkspace(request, {
    tasks: TASK_COUNT,
    sessionsPerTask: 1,
    turnsPerSession: TURNS_PER_SESSION,
    throttleMs: 1,
    messageBytes: MESSAGE_BYTES,
    messagePrefix: "freshness fixture msg",
  });

  const foregroundTaskId = seed.taskIds[0] ?? "";
  const foregroundSessionId = seed.sessionIdsByTask[foregroundTaskId]?.[0] ?? "";
  expect(foregroundSessionId).not.toBe("");
  const backgroundSessionIds = seed.taskIds
    .slice(1)
    .map((taskId) => seed.sessionIdsByTask[taskId]?.[0] ?? "")
    .filter(Boolean);
  expect(backgroundSessionIds.length).toBeGreaterThan(0);

  await page.setViewportSize({ width: 1440, height: 960 });
  await page.goto(`/workspaces/${seed.workspaceId}?ctxE2E=1&loadtest=1`, {
    waitUntil: "domcontentloaded",
  });

  const rows = page.locator(".wb-task-row");
  const sessionView = page.locator('.wb-session-slot[aria-hidden="false"]');
  await expect(rows).toHaveCount(TASK_COUNT, { timeout: 30_000 });

  const openForegroundTask = async () => {
    const focused = await page.evaluate(
      ({ taskId, sessionId }) => window.__ctxE2E?.focusTask?.(taskId, sessionId) ?? false,
      { taskId: foregroundTaskId, sessionId: foregroundSessionId },
    );
    expect(focused).toBe(true);
    await expect(sessionView).toContainText(/freshness fixture msg 1\.1\./i, { timeout: 30_000 });
    await expect(page.locator(".wb-session-slot textarea.wb-active-textarea")).toBeVisible({
      timeout: 30_000,
    });
  };

  const runs: Array<{
    run: number;
    backendToDomMs: number;
    marker: string;
    turnId: string;
    diagnosticCodes: string[];
  }> = [];

  for (let run = 0; run < PROBE_RUNS; run += 1) {
    await openForegroundTask();
    await clearDiagnostics(page);
    const backgroundPressure = runBackgroundPressure(request, backgroundSessionIds, run + 1);
    const streamer =
      BACKGROUND_STREAM_INTERVAL_MS > 0 && BACKGROUND_STREAM_DURATION_MS > 0
        ? startStreamingMessages(request, {
          sessionIds: backgroundSessionIds,
          intervalMs: BACKGROUND_STREAM_INTERVAL_MS,
          durationMs: BACKGROUND_STREAM_DURATION_MS,
          messageBytes: MESSAGE_BYTES,
          includeToolSummaries: true,
          toolSummariesPerTurn: 4,
          messagePrefix: `background-stream-${run + 1}`,
        })
        : null;
    await pageWait(200);

    try {
      const marker = `foreground-freshness-${run + 1}-${Date.now()}`;
      await sendSessionMessage(
        request,
        foregroundSessionId,
        buildSlowPrompt(marker, `foreground-${run + 1}`),
      );

      const { backendReadyAtMs, turnId } = await waitForForegroundTurnCompletion(
        request,
        foregroundSessionId,
        marker,
        45_000,
      );
      const domVisibleAtMs = await page.waitForFunction(
        ({ text }) => {
          const session = document.querySelector('.wb-session-slot[aria-hidden="false"]');
          return session?.textContent?.includes(text) ? Date.now() : null;
        },
        { text: marker },
        { timeout: 45_000 },
      );
      const domVisibleAt = await domVisibleAtMs.jsonValue();
      expect(typeof domVisibleAt).toBe("number");
      const diagnostics = await getDiagnostics(page);
      runs.push({
        run: run + 1,
        backendToDomMs: Number(domVisibleAt) - backendReadyAtMs,
        marker,
        turnId,
        diagnosticCodes: diagnostics.map((entry) => `${entry.source}:${entry.code}:${entry.severity}`),
      });
      await backgroundPressure;
    } finally {
      if (streamer) {
        await streamer.stop();
      }
    }
  }

  const backendToDomMs = runs.map((entry) => entry.backendToDomMs);
  const summary = {
    workspaceId: seed.workspaceId,
    taskCount: TASK_COUNT,
    turnsPerSession: TURNS_PER_SESSION,
    backgroundWaves: BACKGROUND_WAVES,
    backgroundWaveDelayMs: BACKGROUND_WAVE_DELAY_MS,
    backgroundStreamIntervalMs: BACKGROUND_STREAM_INTERVAL_MS,
    backgroundStreamDurationMs: BACKGROUND_STREAM_DURATION_MS,
    probeRuns: runs,
    backendToDomMs,
    p50Ms: percentile(backendToDomMs, 0.5),
    p95Ms: percentile(backendToDomMs, 0.95),
    maxMs: backendToDomMs.length > 0 ? Math.max(...backendToDomMs) : null,
  };

  await testInfo.attach("foreground-freshness-pressure.json", {
    body: JSON.stringify(summary, null, 2),
    contentType: "application/json",
  });
  await fs.writeFile(
    testInfo.outputPath("foreground-freshness-pressure.json"),
    JSON.stringify(summary, null, 2),
    "utf8",
  );

  console.log(`foreground freshness summary: ${JSON.stringify(summary)}`);

  if (GUARDRAIL_ENABLED || MAX_BACKEND_TO_DOM_MS > 0) {
    const thresholdMs = GUARDRAIL_ENABLED ? 150 : MAX_BACKEND_TO_DOM_MS;
    expect(summary.p95Ms ?? Infinity).toBeLessThanOrEqual(thresholdMs);
  }
});

test("workbench: backlog drains and UI recovers after sustained stream pressure", async ({
  page,
  request,
}, testInfo) => {
  test.skip(!LAG_PROOF_ENABLED, "Set CTX_FOREGROUND_LAG_PROOF=1 to run the lag proof.");
  test.setTimeout(360_000);

  const lagTaskCount = Math.max(TASK_COUNT, 12);
  const lagTurnsPerSession = Math.max(TURNS_PER_SESSION, 4);
  const seed = await seedDummyWorkspace(request, {
    tasks: lagTaskCount,
    sessionsPerTask: 1,
    turnsPerSession: lagTurnsPerSession,
    throttleMs: 1,
    messageBytes: MESSAGE_BYTES,
    messagePrefix: "lag proof fixture msg",
  });

  const foregroundTaskId = seed.taskIds[0] ?? "";
  const foregroundSessionId = seed.sessionIdsByTask[foregroundTaskId]?.[0] ?? "";
  expect(foregroundSessionId).not.toBe("");
  const backgroundSessionIds = seed.taskIds
    .slice(1)
    .map((taskId) => seed.sessionIdsByTask[taskId]?.[0] ?? "")
    .filter(Boolean);
  expect(backgroundSessionIds.length).toBeGreaterThan(0);

  await page.setViewportSize({ width: 1440, height: 960 });
  await page.goto(`/workspaces/${seed.workspaceId}?ctxE2E=1&loadtest=1`, {
    waitUntil: "domcontentloaded",
  });
  let pageCrashed = false;
  page.on("crash", () => {
    pageCrashed = true;
  });

  const rows = page.locator(".wb-task-row");
  const sessionView = page.locator('.wb-session-slot[aria-hidden="false"]');
  await expect(rows).toHaveCount(lagTaskCount, { timeout: 30_000 });

  const openForegroundTask = async () => {
    const focused = await page.evaluate(
      ({ taskId, sessionId }) => window.__ctxE2E?.focusTask?.(taskId, sessionId) ?? false,
      { taskId: foregroundTaskId, sessionId: foregroundSessionId },
    );
    expect(focused).toBe(true);
    await expect(sessionView).toContainText(/lag proof fixture msg 1\.1\./i, { timeout: 30_000 });
    await expect(page.locator(".wb-session-slot textarea.wb-active-textarea")).toBeVisible({
      timeout: 30_000,
    });
  };

  await openForegroundTask();
  await clearDiagnostics(page);

  const backgroundPressure = runBackgroundPressure(request, backgroundSessionIds, 1);
  const streamers = startLagProofStreamers(request, backgroundSessionIds);
  let overloadProbe: LagProbeOutcome | null = null;
  let recoveryProbe: LagProbeOutcome | null = null;

  try {
    await pageWait(LAG_PROOF_PRESSURE_SETTLE_MS);
    const overloadMarker = `lag-proof-overload-${Date.now()}`;
    overloadProbe = await runForegroundLagProbe(
      page,
      request,
      foregroundSessionId,
      overloadMarker,
      LAG_PROOF_OVERLOAD_VISIBILITY_TIMEOUT_MS,
      buildSlowPrompt(overloadMarker, "lag-proof-overload"),
    );
  } finally {
    await backgroundPressure;
    for (const streamer of streamers) {
      await streamer.stop();
    }
  }

  const overloadWorkspaceBacklog = await readTelemetryMetric(
    request,
    "workbench.workspace_backlog_age_ms",
    LAG_PROOF_OVERLOAD_WINDOW_MS,
  );
  const overloadForegroundBacklog = await readTelemetryMetric(
    request,
    "workbench.foreground_queue_age_ms",
    LAG_PROOF_OVERLOAD_WINDOW_MS,
  );

  let recoveryMetrics: Awaited<ReturnType<typeof waitForBacklogRecovery>> | null = null;
  let diagnostics: Awaited<ReturnType<typeof getDiagnostics>> = [];
  let browser = summarizeBrowserLoadTest(null);
  let uiAlive = false;
  let recoveryError: string | null = null;
  try {
    await openForegroundTask();
    recoveryMetrics = await waitForBacklogRecovery(request);
    const recoveryMarker = `lag-proof-recovery-${Date.now()}`;
    recoveryProbe = await runForegroundLagProbe(
      page,
      request,
      foregroundSessionId,
      recoveryMarker,
      LAG_PROOF_RECOVERY_VISIBILITY_TIMEOUT_MS,
      buildQuickPrompt(recoveryMarker),
    );
    diagnostics = await getDiagnostics(page);
    const loadTest = await readLoadTestSnapshot(page);
    browser = summarizeBrowserLoadTest(loadTest);
    uiAlive =
      (await rows.count()) === lagTaskCount && (await sessionView.isVisible()) && !page.isClosed();
  } catch (error) {
    recoveryError = formatUnknownError(error);
    uiAlive = false;
    if (!pageCrashed) {
      try {
        diagnostics = await getDiagnostics(page);
      } catch {
        diagnostics = [];
      }
      try {
        const loadTest = await readLoadTestSnapshot(page);
        browser = summarizeBrowserLoadTest(loadTest);
      } catch {
        browser = summarizeBrowserLoadTest(null);
      }
    }
  }
  const overloadObserved =
    Boolean(overloadProbe?.timedOut) ||
    (typeof overloadProbe?.backendToDomMs === "number" &&
      overloadProbe.backendToDomMs >= LAG_PROOF_MIN_OVERLOAD_BACKEND_TO_DOM_MS) ||
    (typeof overloadForegroundBacklog.max === "number" &&
      overloadForegroundBacklog.max >= LAG_PROOF_MIN_OVERLOAD_FOREGROUND_QUEUE_MS) ||
    (typeof overloadWorkspaceBacklog.max === "number" &&
      overloadWorkspaceBacklog.max >= LAG_PROOF_MIN_OVERLOAD_WORKSPACE_QUEUE_MS);
  const recovered =
    recoveryMetrics !== null &&
    recoveryProbe !== null &&
    !recoveryProbe.timedOut &&
    typeof recoveryProbe.backendToDomMs === "number" &&
    recoveryProbe.backendToDomMs <= LAG_PROOF_MAX_RECOVERY_BACKEND_TO_DOM_MS &&
    recoveryMetrics.settledWithinMs !== null &&
    rollupUpperBound(recoveryMetrics.foregroundBacklog) <=
      LAG_PROOF_MAX_RECOVERY_FOREGROUND_QUEUE_P95_MS &&
    rollupUpperBound(recoveryMetrics.workspaceBacklog) <=
      LAG_PROOF_MAX_RECOVERY_WORKSPACE_QUEUE_P95_MS &&
    recoveryMetrics.gapRecoveryTimeoutCount.sum === 0 &&
    recoveryMetrics.workspaceStreamResetCount.sum === 0;
  const errorDiagnostics = diagnostics
    .filter((entry) => {
      if (entry.severity !== "error") return false;
      if (entry.source !== "foreground_freshness") return true;
      return !entry.code.endsWith(".sla_missed");
    })
    .map((entry) => `${entry.source}:${entry.code}:${entry.severity}`);
  const summary = {
    workspaceId: seed.workspaceId,
    config: {
      taskCount: lagTaskCount,
      turnsPerSession: lagTurnsPerSession,
      pressureDurationMs: LAG_PROOF_STREAM_DURATION_MS,
      streamIntervalMs: LAG_PROOF_STREAM_INTERVAL_MS,
      streamers: LAG_PROOF_STREAMERS,
      pressureSettleMs: LAG_PROOF_PRESSURE_SETTLE_MS,
      overloadWindowMs: LAG_PROOF_OVERLOAD_WINDOW_MS,
      recoveryWindowMs: LAG_PROOF_RECOVERY_WINDOW_MS,
    },
    overload: {
      observed: overloadObserved,
      windowMs: LAG_PROOF_OVERLOAD_WINDOW_MS,
      probe: overloadProbe,
      workspaceBacklog: overloadWorkspaceBacklog,
      foregroundBacklog: overloadForegroundBacklog,
    },
    recovery: {
      settledWithinMs: recoveryMetrics?.settledWithinMs ?? null,
      probe: recoveryProbe,
      workspaceBacklog: recoveryMetrics?.workspaceBacklog ?? null,
      foregroundBacklog: recoveryMetrics?.foregroundBacklog ?? null,
      gapRecoveryMs: recoveryMetrics?.gapRecoveryMs ?? null,
      gapRecoveryTimeoutCount: recoveryMetrics?.gapRecoveryTimeoutCount.sum ?? null,
      workspaceStreamResetCount: recoveryMetrics?.workspaceStreamResetCount.sum ?? null,
      error: recoveryError,
    },
    browser,
    diagnostics: diagnostics.map((entry) => `${entry.source}:${entry.code}:${entry.severity}`),
    acceptance: {
      overloadObserved,
      recovered,
      uiAlive,
      pageCrashed,
      errorDiagnostics,
      passed:
        overloadObserved &&
        recovered &&
        uiAlive &&
        !pageCrashed &&
        !recoveryError &&
        errorDiagnostics.length === 0,
    },
  };

  await testInfo.attach("foreground-lag-proof.json", {
    body: JSON.stringify(summary, null, 2),
    contentType: "application/json",
  });
  await fs.writeFile(
    testInfo.outputPath("foreground-lag-proof.json"),
    JSON.stringify(summary, null, 2),
    "utf8",
  );

  console.log(`foreground lag proof summary: ${JSON.stringify(summary)}`);

  expect(summary.acceptance.overloadObserved).toBe(true);
  expect(summary.acceptance.recovered).toBe(true);
  expect(summary.acceptance.uiAlive).toBe(true);
  expect(summary.acceptance.errorDiagnostics).toEqual([]);
});
