import type { Page } from "playwright/test";
import { test, expect } from "./fixtures";
import { seedDummyWorkspace, startStreamingMessages } from "./utils/seedDummyWorkspace";

type InputLatencySample = {
  char: string;
  durationMs: number;
};

type LongTaskSample = {
  startTime: number;
  duration: number;
};

type VisibleThreadDebug = {
  sessionId: string | null;
  assistantStreamingStamp: string;
  workbenchThreadOpKind: string;
  threadProjectionOpKind: string;
  assistantContents: string[];
};

type StreamingDebugSample = {
  index: number;
  assistantLength: number;
  assistantStreamingStamp: string;
  workbenchThreadOpKind: string;
  threadProjectionOpKind: string;
};

type ContentionMetrics = {
  samples: InputLatencySample[];
  streamingSamples: StreamingDebugSample[];
  p95InputMs: number;
  maxInputMs: number;
  longTasks: LongTaskSample[];
  maxLongTaskMs: number;
};

type ContentionWindow = Window & {
  __streamingOverlayContention?: {
    longTasks: LongTaskSample[];
    observer?: PerformanceObserver;
  };
};

const composerSelector = ".wb-session-slot[aria-hidden=\"false\"] textarea.wb-active-textarea";
const visibleSessionSelector = ".wb-session-slot[aria-hidden=\"false\"]";

test.setTimeout(150_000);

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

function percentile(values: readonly number[], p: number): number {
  if (values.length === 0) return 0;
  const sorted = values.slice().sort((left, right) => left - right);
  const index = Math.min(sorted.length - 1, Math.max(0, Math.ceil(sorted.length * p) - 1));
  return sorted[index] ?? 0;
}

async function sendStreamingPrompt(
  request: Parameters<typeof test>[0]["request"],
  sessionId: string,
  marker: string,
): Promise<void> {
  const resp = await request.post(`/api/sessions/${sessionId}/messages`, {
    data: {
      content: `slow-diff-test stream-assistant-partials ${marker}\n${"streaming contention body\n".repeat(120)}`,
      delivery: "immediate",
    },
  });
  expect(resp.ok(), `failed to send streaming prompt for ${sessionId}`).toBeTruthy();
}

async function waitForVisiblePrompt(page: Page, marker: string): Promise<void> {
  await expect(page.locator(visibleSessionSelector).getByText(marker).first()).toBeVisible({
    timeout: 20_000,
  });
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
    const w = window as ContentionWindow;
    w.__streamingOverlayContention?.observer?.disconnect();
    const state: NonNullable<ContentionWindow["__streamingOverlayContention"]> = { longTasks: [] };
    w.__streamingOverlayContention = state;
    if (typeof PerformanceObserver === "undefined") return;
    try {
      const observer = new PerformanceObserver((list) => {
        for (const entry of list.getEntries()) {
          state.longTasks.push({
            startTime: entry.startTime,
            duration: entry.duration,
          });
        }
      });
      observer.observe({ type: "longtask", buffered: true });
      state.observer = observer;
    } catch {
      // Browser support varies; input latency remains the required signal.
    }
  });
}

async function readLongTasks(page: Page): Promise<LongTaskSample[]> {
  return page.evaluate(() => {
    const w = window as ContentionWindow;
    w.__streamingOverlayContention?.observer?.disconnect();
    return w.__streamingOverlayContention?.longTasks ?? [];
  });
}

async function clearLongTaskSamples(page: Page): Promise<void> {
  await page.evaluate(() => {
    const w = window as ContentionWindow;
    const state = w.__streamingOverlayContention;
    if (state) {
      state.longTasks = [];
    }
  });
}

async function readVisibleThreadDebug(page: Page): Promise<VisibleThreadDebug | null> {
  return page.evaluate(() => window.__ctxE2E?.getVisibleSessionThreadDebug?.() ?? null);
}

function assistantTextLength(debug: VisibleThreadDebug | null): number {
  return debug?.assistantContents.join("").length ?? 0;
}

async function typeWithLatency(
  page: Page,
  text: string,
): Promise<{ inputSamples: InputLatencySample[]; streamingSamples: StreamingDebugSample[] }> {
  const composer = page.locator(composerSelector);
  await expect(composer).toBeVisible({ timeout: 20_000 });
  await composer.fill("");
  await composer.focus();

  const inputSamples: InputLatencySample[] = [];
  const streamingSamples: StreamingDebugSample[] = [];
  let expected = "";
  for (let index = 0; index < text.length; index += 1) {
    const char = text[index] ?? "";
    expected += char;
    const startedAt = Date.now();
    await page.keyboard.type(char);
    await expect
      .poll(async () => composer.inputValue(), { timeout: 5_000 })
      .toBe(expected);
    inputSamples.push({ char, durationMs: Date.now() - startedAt });
    const debug = await readVisibleThreadDebug(page);
    streamingSamples.push({
      index,
      assistantLength: assistantTextLength(debug),
      assistantStreamingStamp: debug?.assistantStreamingStamp ?? "",
      workbenchThreadOpKind: debug?.workbenchThreadOpKind ?? "",
      threadProjectionOpKind: debug?.threadProjectionOpKind ?? "",
    });
    await sleep(75);
  }
  return { inputSamples, streamingSamples };
}

test("workbench: streaming overlay contention does not stall composer input", async ({ page, request }, testInfo) => {
  await page.setViewportSize({ width: 1400, height: 900 });

  const seed = await seedDummyWorkspace(request, {
    tasks: 3,
    sessionsPerTask: 1,
    turnsPerSession: 0,
  });
  const taskId = seed.taskIds[0] ?? "";
  const activeSessionId = taskId ? seed.sessionIdsByTask[taskId]?.[0] ?? "" : "";
  const backgroundSessionIds = seed.taskIds
    .slice(1)
    .map((backgroundTaskId) => seed.sessionIdsByTask[backgroundTaskId]?.[0] ?? "")
    .filter((sessionId) => sessionId.length > 0);
  if (!activeSessionId || backgroundSessionIds.length === 0) {
    throw new Error("failed to resolve seeded active/background sessions");
  }
  await page.goto(`/workspaces/${seed.workspaceId}?ctxE2E=1&loadtest=1`, { waitUntil: "domcontentloaded" });
  const rows = page.locator(".wb-task-row");
  await expect(rows).toHaveCount(3, { timeout: 20_000 });
  const focused = await page.evaluate(
    ({ focusTaskId, focusSessionId }) => window.__ctxE2E?.focusTask?.(focusTaskId, focusSessionId) ?? false,
    { focusTaskId: taskId, focusSessionId: activeSessionId },
  );
  expect(focused).toBe(true);
  await waitForVisibleSession(page, activeSessionId);
  await expect(page.locator(composerSelector)).toBeVisible({ timeout: 20_000 });

  const backgroundStream = startStreamingMessages(request, {
    sessionIds: backgroundSessionIds,
    intervalMs: 180,
    durationMs: 7_000,
    messageBytes: { min: 800, max: 1200 },
    messagePrefix: "slow-diff-test stream-assistant-partials streaming-overlay-background",
  });
  let backgroundStats = backgroundStream.getStats();

  try {
    await installLongTaskProbe(page);
    await expect
      .poll(() => backgroundStream.getStats().sent, { timeout: 10_000 })
      .toBeGreaterThanOrEqual(backgroundSessionIds.length);
    await sendStreamingPrompt(request, activeSessionId, "streaming-overlay-visible");
    await waitForVisiblePrompt(page, "streaming-overlay-visible");
    await page.waitForTimeout(100);
    const streamingBeforeTyping = await readVisibleThreadDebug(page);
    await clearLongTaskSamples(page);
    const { inputSamples, streamingSamples } = await typeWithLatency(page, "streaming input stays responsive");
    const longTasks = await readLongTasks(page);
    const durations = inputSamples.map((sample) => sample.durationMs);
    const initialAssistantLength = assistantTextLength(streamingBeforeTyping);
    const maxAssistantLengthDuringTyping = Math.max(
      initialAssistantLength,
      ...streamingSamples.map((sample) => sample.assistantLength),
    );
    const metrics: ContentionMetrics = {
      samples: inputSamples,
      streamingSamples,
      p95InputMs: percentile(durations, 0.95),
      maxInputMs: Math.max(0, ...durations),
      longTasks,
      maxLongTaskMs: Math.max(0, ...longTasks.map((entry) => entry.duration)),
    };

    await testInfo.attach("streaming-overlay-contention-metrics.json", {
      body: JSON.stringify(metrics, null, 2),
      contentType: "application/json",
    });

    expect(metrics.samples).toHaveLength("streaming input stays responsive".length);
    expect(maxAssistantLengthDuringTyping).toBeGreaterThan(initialAssistantLength);
    expect(
      metrics.streamingSamples.some(
        (sample) =>
          sample.workbenchThreadOpKind === "append_stream" ||
          sample.threadProjectionOpKind === "append_stream",
      ),
    ).toBe(true);
    expect(metrics.p95InputMs).toBeLessThanOrEqual(750);
    expect(metrics.maxInputMs).toBeLessThanOrEqual(2500);
    expect(metrics.maxLongTaskMs).toBeLessThanOrEqual(2500);
  } finally {
    await backgroundStream.stop();
    backgroundStats = backgroundStream.getStats();
  }
  expect(backgroundStats.failures).toEqual([]);
  expect(backgroundStats.sent).toBeGreaterThanOrEqual(backgroundSessionIds.length);
});
