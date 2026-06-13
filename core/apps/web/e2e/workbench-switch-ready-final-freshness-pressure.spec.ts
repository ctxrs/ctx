import fs from "fs/promises";
import type { APIRequestContext } from "playwright/test";
import { test, expect } from "./fixtures";
import { clearDiagnostics, getDiagnostics } from "./utils/diagnostics";
import { seedDummyWorkspace } from "./utils/seedDummyWorkspace";

const PROBE_ENABLED = process.env.CTX_SWITCH_FRESHNESS_PROBE === "1";
const TASK_COUNT = Number(process.env.CTX_SWITCH_FRESHNESS_TASKS ?? "8");
const TURNS_PER_SESSION = Number(process.env.CTX_SWITCH_FRESHNESS_TURNS ?? "50");
const MESSAGE_BYTES = Number(process.env.CTX_SWITCH_FRESHNESS_MESSAGE_BYTES ?? "3000");
const TASK_PRESSURE_INTERVAL_MS = Number(
  process.env.CTX_SWITCH_FRESHNESS_TASK_PRESSURE_INTERVAL_MS ?? "5",
);
const TASK_PRESSURE_DURATION_MS = Number(
  process.env.CTX_SWITCH_FRESHNESS_TASK_PRESSURE_DURATION_MS ?? "20000",
);
const SESSION_SEED_PRESSURE_INTERVAL_MS = Number(
  process.env.CTX_SWITCH_FRESHNESS_SESSION_SEED_PRESSURE_INTERVAL_MS ?? "25",
);
const PRESSURE_SETTLE_MS = Number(
  process.env.CTX_SWITCH_FRESHNESS_PRESSURE_SETTLE_MS ?? "500",
);
const FINAL_BODY_LINES = Number(process.env.CTX_SWITCH_FRESHNESS_PROMPT_BODY_LINES ?? "80");
const MAX_SWITCH_TO_DOM_MS = Number(process.env.CTX_SWITCH_FRESHNESS_MAX_SWITCH_TO_DOM_MS ?? "0");
const DEBUG_SAMPLES_ENABLED = process.env.CTX_SWITCH_FRESHNESS_DEBUG_SAMPLES === "1";

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

const finalBody = Array.from(
  { length: FINAL_BODY_LINES },
  (_, index) => `switch freshness body line ${index + 1}`,
).join("\n");

const percentile = (values: number[], p: number): number | null => {
  if (values.length === 0) return null;
  const sorted = values.slice().sort((left, right) => left - right);
  const index = Math.min(sorted.length - 1, Math.max(0, Math.ceil(sorted.length * p) - 1));
  return Math.round(sorted[index]! * 10) / 10;
};

const buildPaddedMessage = (base: string, targetBytes?: number): string => {
  if (!targetBytes || targetBytes <= base.length) return base;
  const padding = targetBytes - base.length;
  if (padding === 1) return `${base} `;
  return `${base} ${"x".repeat(padding - 1)}`;
};

async function pageWait(ms: number): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, ms));
}

async function seedTranscript(
  request: APIRequestContext,
  sessionId: string,
  taskNumber: number,
  turnCount: number,
): Promise<void> {
  const turns = Array.from({ length: turnCount }, (_, index) => ({
    user: `switch seeded user ${taskNumber}.1.${index + 1}`,
    assistant: buildPaddedMessage(
      `switch seeded assistant ${taskNumber}.1.${index + 1}`,
      MESSAGE_BYTES,
    ),
  }));
  const response = await request.post(`/api/dev/sessions/${sessionId}/seed_transcript`, {
    data: {
      session_title: `switch fixture session ${taskNumber}`,
      task_title: `switch fixture task ${taskNumber}`,
      turns,
    },
  });
  expect(response.ok(), `seed transcript failed: ${response.url()}`).toBeTruthy();
}

async function seedForegroundFinal(
  request: APIRequestContext,
  sessionId: string,
  marker: string,
): Promise<void> {
  const response = await request.post(`/api/dev/sessions/${sessionId}/seed_transcript`, {
    data: {
      session_title: "switch ready final foreground",
      turns: [
        {
          user: `switch ready final user ${marker}`,
          assistant: buildPaddedMessage(
            `switch ready final assistant ${marker}\n${finalBody}`,
            MESSAGE_BYTES,
          ),
        },
      ],
    },
  });
  expect(response.ok(), `seed foreground final failed: ${response.url()}`).toBeTruthy();
}

async function updateTaskTitle(
  request: APIRequestContext,
  taskId: string,
  title: string,
): Promise<void> {
  const response = await request.post(`/api/tasks/${taskId}/title`, {
    data: { title },
  });
  expect(response.ok(), `task title update failed: ${response.url()}`).toBeTruthy();
}

async function markTaskRead(request: APIRequestContext, taskId: string): Promise<void> {
  const response = await request.post(`/api/tasks/${taskId}/mark_read`);
  expect(response.ok(), `mark read failed: ${response.url()}`).toBeTruthy();
}

async function markTaskUnread(request: APIRequestContext, taskId: string): Promise<void> {
  const response = await request.post(`/api/tasks/${taskId}/mark_unread`);
  expect(response.ok(), `mark unread failed: ${response.url()}`).toBeTruthy();
}

function startTaskPressure(
  request: APIRequestContext,
  opts: {
    taskIds: string[];
    intervalMs: number;
    durationMs: number;
  },
): { stop: () => Promise<void> } {
  const taskIds = opts.taskIds.filter((taskId) => taskId.trim().length > 0);
  if (taskIds.length === 0) {
    throw new Error("startTaskPressure requires at least one task id");
  }

  let stopped = false;
  let tick = 0;
  let inflight: Promise<void> = Promise.resolve();

  const sendOnce = async () => {
    if (stopped) return;
    const taskId = taskIds[tick % taskIds.length] ?? "";
    const phase = tick % 3;
    if (phase === 0) {
      await updateTaskTitle(request, taskId, `switch pressure ${taskId.slice(0, 8)} ${tick}`);
    } else if (phase === 1) {
      await markTaskUnread(request, taskId);
    } else {
      await markTaskRead(request, taskId);
    }
    tick += 1;
  };

  const timer = setInterval(() => {
    inflight = inflight.then(sendOnce).catch(() => {
      // Keep the pressure loop alive and let the test fail on its primary assertions.
    });
  }, Math.max(1, opts.intervalMs));

  const stop = async () => {
    if (stopped) return;
    stopped = true;
    clearInterval(timer);
    await inflight;
  };

  if (opts.durationMs > 0) {
    setTimeout(() => {
      void stop();
    }, opts.durationMs);
  }

  return { stop };
}

function startSessionSeedPressure(
  request: APIRequestContext,
  opts: {
    sessionIds: string[];
    turnCount: number;
    startTaskNumber: number;
    intervalMs: number;
  },
): { stop: () => Promise<void> } {
  const sessionIds = opts.sessionIds.filter((sessionId) => sessionId.trim().length > 0);
  if (sessionIds.length === 0) {
    return { stop: async () => {} };
  }

  let stopped = false;
  let cursor = 0;
  let inflight: Promise<void> = Promise.resolve();

  const sendOnce = async () => {
    if (stopped) return;
    const sessionId = sessionIds[cursor] ?? "";
    if (!sessionId) return;
    await seedTranscript(
      request,
      sessionId,
      opts.startTaskNumber + cursor,
      opts.turnCount,
    );
    cursor += 1;
    if (cursor >= sessionIds.length) {
      stopped = true;
      clearInterval(timer);
    }
  };

  const timer = setInterval(() => {
    inflight = inflight.then(sendOnce).catch(() => {
      // Keep the pressure loop alive and let the test fail on its primary assertions.
    });
  }, Math.max(1, opts.intervalMs));

  const stop = async () => {
    if (!stopped) {
      stopped = true;
      clearInterval(timer);
    }
    await inflight;
  };

  return { stop };
}

async function waitForTurnCompletion(
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
    await pageWait(50);
  }
  throw new Error(`background final did not complete for marker ${marker}`);
}

async function focusTask(page: Parameters<typeof test>[0]["page"], taskId: string, sessionId: string) {
  const focused = await page.evaluate(
    ({ nextTaskId, nextSessionId }) => window.__ctxE2E?.focusTask?.(nextTaskId, nextSessionId) ?? false,
    { nextTaskId: taskId, nextSessionId: sessionId },
  );
  expect(focused).toBe(true);
}

async function captureSwitchDebugState(
  page: Parameters<typeof test>[0]["page"],
  sessionId: string,
  marker: string,
) {
  return page.evaluate(
    ({ activeSessionId, text }) => {
      const bridge = window.__ctxE2E;
      const session = document.querySelector('.wb-session-slot[aria-hidden="false"]');
      const textContent = session?.textContent ?? "";
      const headMessages = bridge?.getSessionHeadMessages?.(activeSessionId) ?? [];
      const headUserMessages = bridge?.getSessionHeadUserMessages?.(activeSessionId) ?? [];
      const visibleEntry = bridge?.getVisibleSessionEntryDebug?.() ?? null;
      const visibleThread = bridge?.getVisibleSessionThreadDebug?.() ?? null;
      return {
        workspaceConnection: bridge?.getWorkspaceSnapshot?.()?.connection ?? null,
        workspaceHeadHasMarker: headMessages.some((message) => message.includes(text)),
        workspaceHeadUserHasMarker: headUserMessages.some((message) => message.includes(text)),
        workspaceHeadLastEventSeq: bridge?.getSessionLastEventSeq?.(activeSessionId) ?? null,
        visibleEntrySessionId: visibleEntry?.sessionId ?? null,
        visibleEntryHasMarker:
          Array.isArray(visibleEntry?.messageContents) &&
          visibleEntry.messageContents.some((message) => message.includes(text)),
        visibleEntryLastEventSeq: visibleEntry?.lastEventSeq ?? null,
        visibleThreadSessionId: visibleThread?.sessionId ?? null,
        visibleThreadProjectionRev: visibleThread?.projectionRev ?? null,
        visibleThreadTurnsStamp: visibleThread?.turnsStamp ?? null,
        visibleThreadMessagesStamp: visibleThread?.messagesStamp ?? null,
        visibleThreadHasMarker:
          Array.isArray(visibleThread?.assistantContents) &&
          visibleThread.assistantContents.some((message) => message.includes(text)),
        visibleThreadTail: Array.isArray(visibleThread?.assistantContents)
          ? visibleThread.assistantContents.slice(-3)
          : [],
        visibleThreadLastItemIds: Array.isArray(visibleThread?.listItemIds)
          ? visibleThread.listItemIds.slice(-12)
          : [],
        visibleTextHasMarker: textContent.includes(text),
        visibleTextTail: textContent.slice(-2000),
      };
    },
    { activeSessionId: sessionId, text: marker },
  );
}

test.use({ browserName: "chromium" });

test("workbench: switching into a ready final stays fresh under pressure", async ({
  page,
  request,
}, testInfo) => {
  test.skip(!PROBE_ENABLED, "Set CTX_SWITCH_FRESHNESS_PROBE=1 to run the switch freshness probe.");
  test.setTimeout(240_000);

  const seed = await seedDummyWorkspace(request, {
    tasks: TASK_COUNT,
    sessionsPerTask: 1,
    turnsPerSession: 0,
    sessionSource: {
      providerId: "codex",
      modelId: "gpt-5.4",
      executionEnvironment: "host",
    },
  });

  const foregroundTaskId = seed.taskIds[0] ?? "";
  const foregroundSessionId = seed.sessionIdsByTask[foregroundTaskId]?.[0] ?? "";
  const activeTaskId = seed.taskIds[1] ?? "";
  const activeSessionId = seed.sessionIdsByTask[activeTaskId]?.[0] ?? "";
  expect(foregroundSessionId).not.toBe("");
  expect(activeSessionId).not.toBe("");
  await seedTranscript(request, activeSessionId, 2, TURNS_PER_SESSION);
  const backgroundSeedSessionIds = seed.taskIds
    .slice(2)
    .map((taskId) => seed.sessionIdsByTask[taskId]?.[0] ?? "")
    .filter(Boolean);

  await page.setViewportSize({ width: 1440, height: 960 });
  await page.goto(`/workspaces/${seed.workspaceId}?ctxE2E=1&loadtest=1`, {
    waitUntil: "domcontentloaded",
  });

  const rows = page.locator(".wb-task-row");
  const sessionView = page.locator('.wb-session-slot[aria-hidden="false"]');
  await expect(rows).toHaveCount(TASK_COUNT, { timeout: 30_000 });

  await focusTask(page, activeTaskId, activeSessionId);
  await expect(sessionView).toBeVisible({ timeout: 30_000 });
  await expect(page.locator(".wb-session-slot textarea.wb-active-textarea")).toBeVisible({
    timeout: 30_000,
  });

  await clearDiagnostics(page);
  const taskPressure = startTaskPressure(request, {
    taskIds: seed.taskIds.slice(1),
    intervalMs: TASK_PRESSURE_INTERVAL_MS,
    durationMs: TASK_PRESSURE_DURATION_MS,
  });
  const sessionSeedPressure = startSessionSeedPressure(request, {
    sessionIds: backgroundSeedSessionIds,
    turnCount: TURNS_PER_SESSION,
    startTaskNumber: 3,
    intervalMs: SESSION_SEED_PRESSURE_INTERVAL_MS,
  });

  try {
    const marker = `switch-ready-final-${Date.now()}`;
    await pageWait(PRESSURE_SETTLE_MS);
    await seedForegroundFinal(request, foregroundSessionId, marker);
    const { backendReadyAtMs, turnId } = await waitForTurnCompletion(
      request,
      foregroundSessionId,
      marker,
      10_000,
    );

    const switchStartedAtMs = Date.now();
    await focusTask(page, foregroundTaskId, foregroundSessionId);
    const debugSamplesPromise = DEBUG_SAMPLES_ENABLED
      ? (async () => {
          const samples: Array<{ label: string; elapsedMs: number; state: unknown }> = [];
          const checkpoints = [
            { label: "250ms", waitMs: 250 },
            { label: "1000ms", waitMs: 1000 },
          ];
          for (const checkpoint of checkpoints) {
            await pageWait(checkpoint.waitMs);
            samples.push({
              label: checkpoint.label,
              elapsedMs: Date.now() - switchStartedAtMs,
              state: await captureSwitchDebugState(page, foregroundSessionId, marker),
            });
          }
          return samples;
        })()
      : Promise.resolve([]);
    let domVisibleAtMs;
    try {
      domVisibleAtMs = await page.waitForFunction(
        ({ text }) => {
          const session = document.querySelector('.wb-session-slot[aria-hidden="false"]');
          return session?.textContent?.includes(text) ? Date.now() : null;
        },
        { text: marker },
        { timeout: 30_000 },
      );
    } catch (error) {
      const debugState = await captureSwitchDebugState(page, foregroundSessionId, marker);
      await testInfo.attach("switch-ready-final-debug.json", {
        body: JSON.stringify(debugState, null, 2),
        contentType: "application/json",
      });
      await fs.writeFile(
        testInfo.outputPath("switch-ready-final-debug.json"),
        JSON.stringify(debugState, null, 2),
        "utf8",
      );
      console.log(`switch freshness debug: ${JSON.stringify(debugState)}`);
      throw error;
    }
    const domVisibleAt = await domVisibleAtMs.jsonValue();
    expect(typeof domVisibleAt).toBe("number");
    const diagnostics = await getDiagnostics(page);
    const summary = {
      workspaceId: seed.workspaceId,
      taskCount: TASK_COUNT,
      turnsPerSession: TURNS_PER_SESSION,
      taskPressureIntervalMs: TASK_PRESSURE_INTERVAL_MS,
      taskPressureDurationMs: TASK_PRESSURE_DURATION_MS,
      sessionSeedPressureIntervalMs: SESSION_SEED_PRESSURE_INTERVAL_MS,
      pressureSettleMs: PRESSURE_SETTLE_MS,
      backgroundSeedSessionCount: backgroundSeedSessionIds.length,
      backendReadyAtMs,
      switchStartedAtMs,
      domVisibleAtMs: Number(domVisibleAt),
      switchToDomMs: Number(domVisibleAt) - switchStartedAtMs,
      backendReadyToDomMs: Number(domVisibleAt) - backendReadyAtMs,
      marker,
      turnId,
      diagnosticCodes: diagnostics.map((entry) => `${entry.source}:${entry.code}:${entry.severity}`),
      p95SwitchToDomMs: percentile([Number(domVisibleAt) - switchStartedAtMs], 0.95),
    };

    await testInfo.attach("switch-ready-final-freshness.json", {
      body: JSON.stringify(summary, null, 2),
      contentType: "application/json",
    });
    await fs.writeFile(
      testInfo.outputPath("switch-ready-final-freshness.json"),
      JSON.stringify(summary, null, 2),
      "utf8",
    );
    const debugSamples = await debugSamplesPromise;
    if (debugSamples.length > 0) {
      await testInfo.attach("switch-ready-final-debug-samples.json", {
        body: JSON.stringify(debugSamples, null, 2),
        contentType: "application/json",
      });
      console.log(`switch freshness debug samples: ${JSON.stringify(debugSamples)}`);
    }

    console.log(`switch freshness summary: ${JSON.stringify(summary)}`);

    if (MAX_SWITCH_TO_DOM_MS > 0) {
      expect(summary.switchToDomMs).toBeLessThanOrEqual(MAX_SWITCH_TO_DOM_MS);
    }
  } finally {
    await taskPressure.stop();
    await sessionSeedPressure.stop();
  }
});
