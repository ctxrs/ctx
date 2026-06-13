import type { APIRequestContext, Page, TestInfo } from "playwright/test";
import { test, expect } from "./fixtures";
import { seedDummyWorkspace } from "./utils/seedDummyWorkspace";
import {
  assertNoVisibleRowOverlap,
  failWithThreadDiagnostics,
  readStreamingEvidence,
  requireThreadOverflow,
  requireVisibleSession,
  waitForStreamingObserved,
} from "./utils/workbenchRowOverlapDiagnostics";

const longBody = Array.from({ length: 160 }, (_, index) => `overlap fixture line ${index + 1}`).join("\n");

type SessionHeadResponse = {
  turns?: Array<{ status?: unknown; user_message_id?: unknown }>;
};

const terminalTurnStatuses = new Set(["completed", "done", "failed", "interrupted"]);

const buildSlowPrompt = (marker: string, index: number) => {
  const toolCalls = Array.from({ length: 4 }, (_, toolIndex) => ({
    kind: "execute",
    title: `${marker} tool ${toolIndex + 1}`,
    input: { command: `printf '${marker}-${index}-${toolIndex + 1}'` },
    output_text: `${marker} output ${toolIndex + 1}`,
  }));
  return `slow-diff-test stream-assistant-partials emit-thought ${marker} ${index}
${longBody}
[[tool_calls]]
${JSON.stringify(toolCalls)}
[[/tool_calls]]`;
};

async function sendStreamingPrompt(
  request: APIRequestContext,
  sessionId: string,
  marker: string,
  index: number,
): Promise<string> {
  const response = await request.post(`/api/sessions/${sessionId}/messages`, {
    data: { content: buildSlowPrompt(marker, index), delivery: "immediate" },
  });
  expect(response.ok(), `failed to send streaming prompt ${marker} ${index}`).toBeTruthy();
  const payload = (await response.json()) as { id?: unknown };
  const messageId = typeof payload.id === "string" ? payload.id : "";
  expect(messageId, `streaming prompt ${marker} ${index} response did not include a message id`).toBeTruthy();
  return messageId;
}

async function monitorNoOverlapUntilCompleted(
  request: APIRequestContext,
  page: Page,
  testInfo: TestInfo,
  sessionId: string,
  userMessageId: string,
  debugLogs: string[],
  prefix: string,
  timeoutMs = 70_000,
): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  let samples = 0;
  let lastHeadError = "";
  let lastTurnStatus = "";
  let lastVisibleStatuses: string[] = [];
  while (Date.now() < deadline) {
    samples += 1;
    await assertNoVisibleRowOverlap(page, {
      debugLogs,
      expectedSessionId: sessionId,
      prefix: "active-overlap",
      step: `${prefix}-${samples}`,
      testInfo,
    });

    try {
      const response = await request.get(`/api/sessions/${sessionId}/head`);
      if (!response.ok()) {
        lastHeadError = `head request failed with status ${response.status()}`;
      } else {
        const payload = (await response.json()) as SessionHeadResponse;
        const turns = Array.isArray(payload.turns) ? payload.turns : [];
        const matchingTurnStatuses = turns
          .filter((entry) => entry.user_message_id === userMessageId)
          .map((entry) => typeof entry.status === "string" ? entry.status : "")
          .filter(Boolean);
        lastTurnStatus = matchingTurnStatuses.join(",");
        if (matchingTurnStatuses.some((status) => terminalTurnStatuses.has(status))) {
          return;
        }
      }
    } catch (error) {
      lastHeadError = error instanceof Error ? error.message : String(error);
    }

    const evidence = await readStreamingEvidence(page, sessionId);
    lastVisibleStatuses = evidence?.statusTexts ?? [];
    const latestVisibleStatus = lastVisibleStatuses.at(-1)?.toLowerCase() ?? "";
    if (terminalTurnStatuses.has(latestVisibleStatus)) {
      return;
    }

    await page.waitForTimeout(250);
  }
  await failWithThreadDiagnostics(page, {
    debugLogs,
    expectedSessionId: sessionId,
    prefix: "active-overlap",
    step: `${prefix}-completion-timeout`,
    extra: {
      lastHeadError,
      lastTurnStatus,
      lastVisibleStatuses,
      userMessageId,
    },
    testInfo,
    message: `active-session streaming did not complete during ${prefix}`,
  });
}

async function runStreamingOverlapCheck(
  page: Page,
  request: APIRequestContext,
  testInfo: TestInfo,
  sessionId: string,
  debugLogs: string[],
  marker: string,
  index: number,
): Promise<void> {
  const userMessageId = await sendStreamingPrompt(request, sessionId, marker, index);
  await waitForStreamingObserved(page, {
    debugLogs,
    expectedSessionId: sessionId,
    prefix: "active-overlap",
    step: `${marker.toLowerCase()}-${index}-streaming`,
    testInfo,
  });
  await monitorNoOverlapUntilCompleted(
    request,
    page,
    testInfo,
    sessionId,
    userMessageId,
    debugLogs,
    `${marker.toLowerCase()}-${index}`,
  );
  await assertNoVisibleRowOverlap(page, {
    debugLogs,
    expectedSessionId: sessionId,
    prefix: "active-overlap",
    step: `${marker.toLowerCase()}-${index}-complete`,
    testInfo,
  });
}

test("workbench: active session streaming never overlaps visible rows", async ({ page, request }, testInfo) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1000, height: 650 });

  const seed = await seedDummyWorkspace(request, {
    tasks: 1,
    sessionsPerTask: 1,
    turnsPerSession: 6,
    throttleMs: 0,
    messagePrefix: "overlap seed",
    messageBodyLines: 64,
    messageLinePrefix: "overlap seed deterministic line",
    includeToolSummaries: true,
    toolSummariesPerTurn: 3,
    seedTranscriptDirect: true,
  });

  const taskId = seed.taskIds[0];
  const sessionId = seed.sessionIdsByTask[taskId!]?.[0];
  expect(taskId).toBeTruthy();
  expect(sessionId).toBeTruthy();

  const debugLogs: string[] = [];
  page.on("console", (message) => {
    const text = message.text();
    if (text.includes("[MessageList]")) {
      debugLogs.push(text);
      if (debugLogs.length > 200) debugLogs.shift();
    }
  });

  await page.goto(`/workspaces/${seed.workspaceId}?debug=1`, { waitUntil: "domcontentloaded" });
  const task = page.locator(".wb-task-row").filter({ hasText: "fixture task 1" }).first();
  await expect(task).toBeVisible({ timeout: 30_000 });
  await task.click();
  await requireVisibleSession(page, {
    debugLogs,
    expectedSessionId: sessionId!,
    prefix: "active-overlap",
    step: "initial-session",
    testInfo,
  });
  await requireThreadOverflow(page, {
    debugLogs,
    expectedSessionId: sessionId!,
    minOverflow: 300,
    prefix: "active-overlap",
    step: "initial-overflow",
    testInfo,
  });
  await assertNoVisibleRowOverlap(page, {
    debugLogs,
    expectedSessionId: sessionId!,
    prefix: "active-overlap",
    step: "initial",
    testInfo,
  });

  await runStreamingOverlapCheck(page, request, testInfo, sessionId!, debugLogs, "ACTIVE-OVERLAP", 1);
  await runStreamingOverlapCheck(page, request, testInfo, sessionId!, debugLogs, "ACTIVE-OVERLAP", 2);
});
