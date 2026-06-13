import { test, expect } from "./fixtures";
import { seedDummyWorkspace } from "./utils/seedDummyWorkspace";

type E2EWindow = Window & {
  __ctxE2E?: {
    workspaceStream?: {
      getConnectionState?: () => string | null;
      setDropMessages?: (drop: boolean) => void;
    };
  };
};

test("workbench: recovers when workspace stream misses events", async ({ page }) => {
  // Enable E2E hooks for the workspace stream worker.
  await page.addInitScript(() => {
    window.sessionStorage.setItem("ctxE2E", "1");
  });

  const seed = await seedDummyWorkspace(page.request, {
    tasks: 1,
    sessionsPerTask: 1,
    turnsPerSession: 0,
  });
  const workspaceId = seed.workspaceId;
  await page.goto(`/workspaces/${workspaceId}?ctxE2E=1`, { waitUntil: "domcontentloaded" });

  await expect
    .poll(async () =>
      page.evaluate(() => typeof (window as E2EWindow).__ctxE2E?.workspaceStream?.getConnectionState === "function"),
    )
    .toBe(true);

  const rows = page.locator(".wb-task-row");
  const createTask = async (title: string) => {
    const resp = await page.request.post(`/api/workspaces/${workspaceId}/tasks`, {
      data: {
        title,
        default_session: { provider_id: "fake", model_id: "fake-model", execution_environment: "host" },
      },
    });
    expect(resp.ok()).toBe(true);
    const task = (await resp.json()) as { id?: string; primary_session_id?: string | null };
    const taskId = String(task?.id ?? "");
    expect(taskId).toBeTruthy();
    const sessionId = String(task?.primary_session_id ?? "");
    expect(sessionId).toBeTruthy();
    const msgResp = await page.request.post(`/api/sessions/${sessionId}/messages`, {
      data: { content: `fixture msg ${title}`, delivery: "immediate" },
    });
    expect(msgResp.ok()).toBe(true);
  };

  expect(workspaceId).toBeTruthy();
  await expect(rows).toHaveCount(1, { timeout: 20_000 });

  await expect
    .poll(async () => page.evaluate(() => (window as E2EWindow).__ctxE2E?.workspaceStream?.getConnectionState?.()))
    .toBe("connected");

  // Drop workspace stream messages so the client misses the "task 2" update.
  await page.evaluate(() => {
    (window as E2EWindow).__ctxE2E?.workspaceStream?.setDropMessages?.(true);
  });

  await createTask("fixture task 2");

  // Re-enable message delivery, then create another task so the next received seq reveals a gap and
  // triggers the client's recovery path (snapshot reload).
  await page.evaluate(() => {
    (window as E2EWindow).__ctxE2E?.workspaceStream?.setDropMessages?.(false);
  });

  await createTask("fixture task 3");

  await expect(rows).toHaveCount(3, { timeout: 60_000 });
  await expect(rows.filter({ hasText: "fixture task 2" }).first()).toBeVisible({ timeout: 60_000 });
});
