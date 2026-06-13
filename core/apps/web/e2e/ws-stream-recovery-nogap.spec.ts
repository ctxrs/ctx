import { test, expect } from "./fixtures";
import {
  postImmediateMessageAndWaitForCompletion,
  seedDummyWorkspace,
} from "./utils/seedDummyWorkspace";

type E2EWindow = Window & {
  __ctxE2E?: {
    getSessionHeadMessages?: (sessionId: string) => string[];
    workspaceStream?: {
      close?: () => void;
      getConnectionState?: () => string | null;
    };
  };
};

test("ws: recovery keeps all streamed messages across tasks", async ({ page, request }) => {
  await page.addInitScript(() => {
    window.sessionStorage.setItem("ctxE2E", "1");
  });

  const seed = await seedDummyWorkspace(request, {
    tasks: 4,
    sessionsPerTask: 1,
    turnsPerSession: 2,
    throttleMs: 2,
    includeToolSummaries: false,
    seedTranscriptDirect: true,
  });

  await page.goto(`/workspaces/${seed.workspaceId}?ctxE2E=1`, { waitUntil: "domcontentloaded" });
  const search = page.getByTestId("workbench-task-search");
  await expect(search).toBeVisible({ timeout: 30_000 });

  await expect
    .poll(async () =>
      page.evaluate(() => typeof (window as E2EWindow).__ctxE2E?.getSessionHeadMessages === "function"),
    )
    .toBe(true);

  await expect
    .poll(async () => page.evaluate(() => (window as E2EWindow).__ctxE2E?.workspaceStream?.getConnectionState?.()))
    .toBe("connected");

  await page.evaluate(() => {
    (window as E2EWindow).__ctxE2E?.workspaceStream?.close?.();
  });

  await expect
    .poll(async () => page.evaluate(() => (window as E2EWindow).__ctxE2E?.workspaceStream?.getConnectionState?.()))
    .toBe("disconnected");

  const taskIds = seed.taskIds.slice(0, 3);
  const sessionIds = taskIds.map((taskId) => seed.sessionIdsByTask[taskId][0]);
  const perSession = 4;

  for (let sessionIndex = 0; sessionIndex < sessionIds.length; sessionIndex += 1) {
    const sessionId = sessionIds[sessionIndex];
    for (let messageIndex = 0; messageIndex < perSession; messageIndex += 1) {
      const marker = `gap-${sessionIndex + 1}-${messageIndex + 1}`;
      await postImmediateMessageAndWaitForCompletion(request, sessionId, marker);
    }
  }

  const expectedBySession = sessionIds.map((_, sessionIndex) =>
    Array.from({ length: perSession }, (_, messageIndex) => `gap-${sessionIndex + 1}-${messageIndex + 1}`),
  );

  await expect
    .poll(async () =>
      page.evaluate(
        ({ sessionIds: ids, expected }) => {
          const api = (window as E2EWindow).__ctxE2E;
          if (!api || typeof api.getSessionHeadMessages !== "function") {
            return expected.flat();
          }
          const missing: string[] = [];
          for (let i = 0; i < ids.length; i += 1) {
            const messages: string[] = api.getSessionHeadMessages(ids[i]) ?? [];
            for (const marker of expected[i]) {
              if (!messages.some((content) => typeof content === "string" && content.includes(marker))) {
                missing.push(marker);
              }
            }
          }
          return missing;
        },
        { sessionIds, expected: expectedBySession },
      ),
    )
    .toEqual([]);
});
