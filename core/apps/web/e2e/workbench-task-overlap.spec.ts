import type { APIRequestContext, Page } from "playwright/test";
import { test, expect } from "./fixtures";
import { seedDummyWorkspace } from "./utils/seedDummyWorkspace";
import {
  assertNoVisibleRowOverlap,
  readThreadGeometryBySession,
  requireVisibleSession,
  scrollThreadToFraction,
  waitForStreamingObserved,
} from "./utils/workbenchRowOverlapDiagnostics";

const longBody = Array.from({ length: 160 }, (_, index) => `overlap fixture line ${index + 1}`).join("\n");

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
): Promise<void> {
  const response = await request.post(`/api/sessions/${sessionId}/messages`, {
    data: { content: buildSlowPrompt(marker, index), delivery: "immediate" },
  });
  expect(response.ok(), `failed to send streaming prompt ${marker} ${index}`).toBeTruthy();
}

async function recordHiddenGeometry(
  page: Page,
  sessionId: string,
  label: string,
  debugLogs: string[],
) {
  const mountedSlots = await page.locator(".wb-session-slot").evaluateAll((nodes) =>
    nodes.map((node) => {
      const el = node as HTMLElement;
      const sessionView = el.querySelector('[data-testid="session-view"]') as HTMLElement | null;
      return {
        slotAriaHidden: el.getAttribute("aria-hidden"),
        slotStyle: el.getAttribute("style"),
        sessionId: sessionView?.getAttribute("data-session-id") ?? null,
      };
    }),
  );
  debugLogs.push(`${label}:mountedSlots=${JSON.stringify(mountedSlots)}`);
  if (debugLogs.length > 200) debugLogs.splice(0, debugLogs.length - 200);

  const geometry = await readThreadGeometryBySession(page, sessionId);
  if (!geometry) {
    debugLogs.push(`${label}:not-mounted`);
    return;
  }
  debugLogs.push(
    JSON.stringify({
      label,
      sessionId,
      overlaps: geometry.overlaps.length,
      rows: geometry.visibleRows.map((row) => ({
        id: row.id,
        top: row.top,
        height: row.height,
        knownSize: row.parent?.knownSize ?? null,
        dataIndex: row.parent?.dataIndex ?? null,
      })),
    }),
  );
  if (debugLogs.length > 200) debugLogs.splice(0, debugLogs.length - 200);
}

test("workbench: switching between running tasks never overlaps visible rows", async ({ page, request }, testInfo) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1000, height: 650 });

  const seed = await seedDummyWorkspace(request, {
    tasks: 2,
    sessionsPerTask: 1,
    turnsPerSession: 7,
    throttleMs: 0,
    messagePrefix: "overlap seed",
    messageBodyLines: 64,
    messageLinePrefix: "overlap seed deterministic line",
    includeToolSummaries: true,
    toolSummariesPerTurn: 3,
    seedTranscriptDirect: true,
  });

  const [taskAId, taskBId] = seed.taskIds;
  expect(taskAId).toBeTruthy();
  expect(taskBId).toBeTruthy();
  const sessionAId = taskAId ? seed.sessionIdsByTask[taskAId]?.[0] : undefined;
  const sessionBId = taskBId ? seed.sessionIdsByTask[taskBId]?.[0] : undefined;
  expect(sessionAId).toBeTruthy();
  expect(sessionBId).toBeTruthy();

  const debugLogs: string[] = [];
  page.on("console", (message) => {
    const text = message.text();
    if (text.includes("[MessageList]")) {
      debugLogs.push(text);
      if (debugLogs.length > 200) debugLogs.shift();
    }
  });

  await page.goto(`/workspaces/${seed.workspaceId}?debug=1`, { waitUntil: "domcontentloaded" });

  const rows = page.locator(".wb-task-row");
  const taskA = rows.filter({ hasText: "fixture task 1" }).first();
  const taskB = rows.filter({ hasText: "fixture task 2" }).first();

  await expect(rows).toHaveCount(2, { timeout: 30_000 });

  await taskA.click();
  await requireVisibleSession(page, {
    debugLogs,
    expectedSessionId: sessionAId!,
    prefix: "task-overlap",
    step: "alpha-session",
    testInfo,
  });
  const sessionAScrollBefore = await scrollThreadToFraction(page, {
    debugLogs,
    expectedSessionId: sessionAId!,
    fraction: 0.42,
    minOverflow: 300,
    prefix: "task-overlap",
    step: "alpha-scroll",
    testInfo,
  });
  expect(sessionAScrollBefore).toBeGreaterThan(100);
  await assertNoVisibleRowOverlap(page, {
    debugLogs,
    expectedSessionId: sessionAId!,
    prefix: "task-overlap",
    step: "alpha-initial",
    testInfo,
  });

  await taskB.click();
  await requireVisibleSession(page, {
    debugLogs,
    expectedSessionId: sessionBId!,
    prefix: "task-overlap",
    step: "bravo-session",
    testInfo,
  });
  const sessionBScrollBefore = await scrollThreadToFraction(page, {
    debugLogs,
    expectedSessionId: sessionBId!,
    fraction: 0.58,
    minOverflow: 300,
    prefix: "task-overlap",
    step: "bravo-scroll",
    testInfo,
  });
  expect(sessionBScrollBefore).toBeGreaterThan(100);
  await assertNoVisibleRowOverlap(page, {
    debugLogs,
    expectedSessionId: sessionBId!,
    prefix: "task-overlap",
    step: "bravo-initial",
    testInfo,
  });

  await Promise.all([
    sendStreamingPrompt(request, sessionAId!, "ALPHA-SLOW", 1),
    sendStreamingPrompt(request, sessionBId!, "BRAVO-SLOW", 1),
  ]);

  await taskA.click();
  await requireVisibleSession(page, {
    debugLogs,
    expectedSessionId: sessionAId!,
    prefix: "task-overlap",
    step: "alpha-stream-session",
    testInfo,
  });
  await waitForStreamingObserved(page, {
    debugLogs,
    expectedSessionId: sessionAId!,
    prefix: "task-overlap",
    step: "alpha-streaming",
    testInfo,
  });

  await taskB.click();
  await requireVisibleSession(page, {
    debugLogs,
    expectedSessionId: sessionBId!,
    prefix: "task-overlap",
    step: "bravo-stream-session",
    testInfo,
  });
  await waitForStreamingObserved(page, {
    debugLogs,
    expectedSessionId: sessionBId!,
    prefix: "task-overlap",
    step: "bravo-streaming",
    testInfo,
  });

  for (let index = 0; index < 10; index += 1) {
    await recordHiddenGeometry(page, sessionAId!, `alpha-hidden-before-${index + 1}`, debugLogs);
    await taskA.click();
    await requireVisibleSession(page, {
      debugLogs,
      expectedSessionId: sessionAId!,
      prefix: "task-overlap",
      step: `alpha-session-${index + 1}`,
      testInfo,
    });
    await page.waitForTimeout(250);
    await assertNoVisibleRowOverlap(page, {
      debugLogs,
      expectedSessionId: sessionAId!,
      prefix: "task-overlap",
      step: `alpha-${index + 1}`,
      testInfo,
    });

    await recordHiddenGeometry(page, sessionBId!, `bravo-hidden-before-${index + 1}`, debugLogs);
    await taskB.click();
    await requireVisibleSession(page, {
      debugLogs,
      expectedSessionId: sessionBId!,
      prefix: "task-overlap",
      step: `bravo-session-${index + 1}`,
      testInfo,
    });
    await page.waitForTimeout(250);
    await assertNoVisibleRowOverlap(page, {
      debugLogs,
      expectedSessionId: sessionBId!,
      prefix: "task-overlap",
      step: `bravo-${index + 1}`,
      testInfo,
    });
  }
});
