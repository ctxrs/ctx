import { test, expect } from "./fixtures";
import { mkdtempSync, writeFileSync } from "fs";
import { tmpdir } from "os";
import path from "path";
import { execFileSync, execSync } from "child_process";
import { createWorkspaceAndOpenWorkbench, waitForWorkbenchProjectionReady } from "./utils/workbench";
import { selectHarnessBySearch } from "./utils/harnessEndpointAuth";

const asRecord = (value: unknown): Record<string, unknown> => {
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  return value as Record<string, unknown>;
};

const asArray = (value: unknown): unknown[] => (Array.isArray(value) ? value : []);

const readId = (value: unknown): string => (typeof value === "string" ? value : "");

test("workbench: refresh keeps selection, even for older sessions", async ({ page }) => {
  const repo = mkdtempSync(path.join(tmpdir(), "ctx-e2e-"));
  execSync("git init", { cwd: repo });
  execSync("git config user.email test@example.com", { cwd: repo });
  execSync("git config user.name Test", { cwd: repo });
  writeFileSync(path.join(repo, "file.txt"), "hello\n");
  execSync("git add .", { cwd: repo });
  execSync("git commit -m init", { cwd: repo });

  const workspaceName = `ws-${Date.now()}`;

  const workspaceId = await createWorkspaceAndOpenWorkbench({
    page,
    request: page.request,
    repo,
    workspaceName,
    debug: true,
  });

  // Choose Fake harness so the test doesn't depend on external agents.
  await selectHarnessBySearch(page, "fake", /fake/i);
  await expect(
    page.locator('button[title="Agents"] .wb-switcher-label').first(),
  ).toHaveText(/fake/i, { timeout: 20000 });

  await page.locator("textarea.wb-composer-textarea").first().fill("hello refresh");
  await expect(page.getByRole("button", { name: "Send" })).toBeEnabled({ timeout: 20000 });
  await page.getByRole("button", { name: "Send" }).click();

  const sessionComposer = page.locator(".wb-session-slot[aria-hidden=\"false\"] textarea.wb-active-textarea");
  try {
    await expect(sessionComposer).toBeVisible({ timeout: 20000 });
  } catch (err) {
    await page.screenshot({ path: path.join(tmpdir(), "ctx-e2e-session-missing.png"), fullPage: true });
    throw err;
  }
  await expect(
    page.locator(".wb-session .wb-assistant-entry").filter({ hasText: "done: hello refresh" }).first(),
  ).toBeVisible({ timeout: 20000 });

  const snapshotResp = await page.request.get(`/api/workspaces/${workspaceId}/active_snapshot`);
  expect(snapshotResp.ok()).toBeTruthy();
  const snapshot = asRecord(await snapshotResp.json());
  const activeTasks = asArray(asRecord(snapshot.active).tasks).map((task) => asRecord(task));
  const taskSummary =
    activeTasks.find((task) => String(asRecord(task.task).title ?? "") === "hello refresh") ?? activeTasks[0];
  expect(taskSummary).toBeTruthy();
  const taskId = readId(asRecord(taskSummary?.task).id);
  const sessionSummary = asRecord(asArray(taskSummary?.sessions)[0]);
  const primarySessionId = readId(asRecord(asRecord(taskSummary?.primary_session).session).id);
  let sessionId = readId(asRecord(sessionSummary.session).id) || primarySessionId || readId(asRecord(taskSummary?.task).primary_session_id);
  if (!sessionId) {
    const sessionsResp = await page.request.get(`/api/tasks/${taskId}/sessions`);
    if (sessionsResp.ok()) {
      const sessions = asArray(await sessionsResp.json()).map((session) => asRecord(session));
      sessionId = readId(sessions[0]?.id);
    }
  }
  expect(sessionId).toBeTruthy();

  const healthResp = await page.request.get("/api/health");
  expect(healthResp.ok()).toBeTruthy();
  const health = asRecord(await healthResp.json());
  const dataRoot = String(health.data_root ?? "");
  expect(dataRoot).toBeTruthy();

  const dbPath = path.join(dataRoot, "db", "workspaces", workspaceId!, "db.sqlite");
  const sqliteJson = <T extends Record<string, unknown>>(sql: string): T[] => {
    const out = execFileSync("sqlite3", ["-json", dbPath, sql], { encoding: "utf8" }).trim();
    const parsed = out ? (JSON.parse(out) as unknown) : [];
    return Array.isArray(parsed) ? (parsed as T[]) : [];
  };

  const shiftMs = 2 * 24 * 60 * 60 * 1000;
  const shift = (iso: string) => new Date(Date.parse(iso) - shiftMs).toISOString();

  const taskRow = sqliteJson<{ created_at: string; updated_at: string }>(
    `SELECT created_at, updated_at FROM tasks WHERE id='${taskId}'`,
  )[0];
  const sessionRow = sqliteJson<{ created_at: string; updated_at: string }>(
    `SELECT created_at, updated_at FROM sessions WHERE id='${sessionId}'`,
  )[0];
  const messageRows = sqliteJson<{ id: string; created_at: string }>(
    `SELECT id, created_at FROM messages WHERE session_id='${sessionId}'`,
  );
  const eventRows = sqliteJson<{ id: string; created_at: string }>(
    `SELECT id, created_at FROM session_events WHERE session_id='${sessionId}'`,
  );

  const sqlUpdates = [
    `PRAGMA busy_timeout=5000;`,
    `BEGIN;`,
    `UPDATE tasks SET created_at='${shift(String(taskRow.created_at))}', updated_at='${shift(String(taskRow.updated_at))}' WHERE id='${taskId}';`,
    `UPDATE sessions SET created_at='${shift(String(sessionRow.created_at))}', updated_at='${shift(String(sessionRow.updated_at))}' WHERE id='${sessionId}';`,
    ...messageRows.map((r) => `UPDATE messages SET created_at='${shift(String(r.created_at))}' WHERE id='${String(r.id)}';`),
    ...eventRows.map((r) => `UPDATE session_events SET created_at='${shift(String(r.created_at))}' WHERE id='${String(r.id)}';`),
    `COMMIT;`,
  ].join("\n");

  execFileSync("sqlite3", [dbPath, sqlUpdates]);

  await page.reload();

  const urlAfter = new URL(page.url());
  expect(urlAfter.searchParams.get("task")).toBeNull();
  expect(urlAfter.searchParams.get("track")).toBeNull();
  expect(urlAfter.searchParams.get("session")).toBeNull();

  try {
    await expect(page.locator(".wb-session-slot[aria-hidden=\"false\"] textarea.wb-active-textarea")).toBeVisible({ timeout: 20000 });
  } catch (err) {
    await page.screenshot({ path: path.join(tmpdir(), "ctx-e2e-after-reload-session-missing.png"), fullPage: true });
    throw err;
  }
  await waitForWorkbenchProjectionReady(page, { timeout: 30_000 });
  await expect(page.locator(".wb-session .wb-assistant-entry").filter({ hasText: "done: hello refresh" })).toBeVisible({
    timeout: 20000,
  });

  const composer = page.locator(".wb-session-slot[aria-hidden=\"false\"] textarea.wb-active-textarea");
  await composer.click();
  await composer.type("hello again");
  await expect(composer).toHaveValue("hello again", { timeout: 20000 });
  const sendButton = page.locator(".wb-session-slot[aria-hidden=\"false\"] button[aria-label=\"Send\"]");
  await expect(sendButton).toBeEnabled({ timeout: 20000 });
  await sendButton.click();
  const threadScroller = page.locator('.wb-session-slot[aria-hidden="false"] .wb-thread-scroller').first();
  await expect(threadScroller).toBeVisible({ timeout: 20_000 });
  await threadScroller.evaluate((root) => {
    const el = root as HTMLElement;
    el.scrollTop = el.scrollHeight;
  });
  await expect(page.locator(".wb-session")).toContainText("done: hello again", { timeout: 15000 });
});
