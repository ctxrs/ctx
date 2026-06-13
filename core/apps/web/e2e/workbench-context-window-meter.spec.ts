import { test, expect } from "./fixtures";
import { mkdtempSync, writeFileSync } from "fs";
import { execSync } from "child_process";
import { tmpdir } from "os";
import path from "path";
import type { APIRequestContext } from "playwright/test";
import { createWorkspaceAndOpenWorkbench } from "./utils/workbench";
import { selectHarnessBySearch } from "./utils/harnessEndpointAuth";
import { waitForTerminalState } from "../src/testing/providerRuntime";

const asRecord = (value: unknown): Record<string, unknown> => {
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  return value as Record<string, unknown>;
};

async function apiPost<T>(request: APIRequestContext, url: string, data: unknown): Promise<T> {
  const response = await request.post(url, { data });
  expect(response.ok(), `request failed for ${url} (${response.status()})`).toBe(true);
  return (await response.json()) as T;
}

async function waitForSessionSnapshotReady(
  request: APIRequestContext,
  sessionId: string,
  options: { timeoutMs?: number; pollMs?: number } = {},
): Promise<void> {
  const timeoutMs = options.timeoutMs ?? 20_000;
  const pollMs = options.pollMs ?? 250;
  const deadline = Date.now() + timeoutMs;
  let lastStatus = "no response";
  while (Date.now() < deadline) {
    const response = await request.get(`/api/sessions/${sessionId}/snapshot?limit=1`);
    if (response.ok()) {
      return;
    }
    lastStatus = `${response.status()}`;
    await new Promise((resolve) => setTimeout(resolve, pollMs));
  }
  throw new Error(`session ${sessionId} snapshot was not ready before timeout (last status ${lastStatus})`);
}

const SEEDED_HARNESS_CASE = {
  providerId: "codex",
  modelId: "gpt-5.4/medium",
  title: "Codex seeded meter",
} as const;

const SHARED_CONTEXT_WINDOW = {
  context_tokens_estimate: 50_000,
  context_window_tokens: 200_000,
  remaining_tokens_estimate: 150_000,
  remaining_fraction: 0.75,
} as const;

const EXPECTED_CONTEXT_WINDOW_SUMMARY = "25% · 50k/200k";

test("workbench: context window meter renders for a live fake-provider session", async ({ page, request }) => {
  test.setTimeout(120_000);

  const repo = mkdtempSync(path.join(tmpdir(), "ctx-e2e-"));
  execSync("git init", { cwd: repo });
  execSync("git config user.email test@example.com", { cwd: repo });
  execSync("git config user.name Test", { cwd: repo });
  writeFileSync(path.join(repo, "file.txt"), "hello\n");
  execSync("git add .", { cwd: repo });
  execSync("git commit -m init", { cwd: repo });

  await createWorkspaceAndOpenWorkbench({
    page,
    request: page.request,
    repo,
    workspaceName: `ws-${Date.now()}`,
  });
  await selectHarnessBySearch(page, "fake", /fake/i);
  await expect(page.locator(".wb-new-composer-card .wb-context-window")).toHaveCount(0);

  let forceStaleFirstSnapshot = true;
  await page.route("**/api/sessions/*/snapshot**", async (route) => {
    if (!forceStaleFirstSnapshot) {
      await route.continue();
      return;
    }
    forceStaleFirstSnapshot = false;
    const response = await route.fetch();
    const snapshot = asRecord(await response.json());
    const head = asRecord(snapshot.head);
    const staleHead = {
      ...head,
      turns: [],
      messages: [],
      events: [],
      tool_summaries: [],
      has_more_turns: false,
      last_event_seq: 0,
    };
    await route.fulfill({
      response,
      body: JSON.stringify({ ...snapshot, head: staleHead }),
    });
  });

  const prompt = "slow-diff-test 0123456789";
  const createTaskResponsePromise = page.waitForResponse((response) =>
    response.request().method() === "POST"
    && /\/api\/workspaces\/[^/]+\/tasks$/.test(response.url()),
  );
  await page.locator("textarea.wb-composer-textarea").first().fill(prompt);
  await page.getByRole("button", { name: "Send" }).click();
  const createTaskResponse = await createTaskResponsePromise;
  expect(createTaskResponse.ok()).toBe(true);
  const sessionId = String((await createTaskResponse.json() as { primary_session_id?: string | null }).primary_session_id ?? "");
  expect(sessionId).not.toBe("");

  const rows = page.locator(".wb-task-row");
  await expect(rows).toHaveCount(1, { timeout: 20_000 });
  await rows.first().click();

  const activeTextarea = page.locator(".wb-session-slot textarea.wb-active-textarea");
  await expect(activeTextarea).toBeVisible({ timeout: 20_000 });
  const terminal = await waitForTerminalState(request, sessionId, {
    timeoutMs: 60_000,
    pollMs: 1_000,
  });
  expect(terminal.terminalStatus, terminal.errorMessage ?? "fake-provider run did not complete").toBe("completed");

  const contextWindow = page.locator(".wb-session-slot .wb-context-window");
  await expect(contextWindow).toBeVisible({ timeout: 20_000 });
  await expect(contextWindow).toHaveText("7% · 7/100", { timeout: 20_000 });
  await expect(contextWindow).toHaveAttribute("title", "Context Window: 7% · 7/100");
});

test("workbench: context window meter live-updates before the turn finishes", async ({ page, request }) => {
  test.setTimeout(120_000);

  const repo = mkdtempSync(path.join(tmpdir(), "ctx-e2e-live-meter-"));
  execSync("git init", { cwd: repo });
  execSync("git config user.email test@example.com", { cwd: repo });
  execSync("git config user.name Test", { cwd: repo });
  writeFileSync(path.join(repo, "file.txt"), "hello\n");
  execSync("git add .", { cwd: repo });
  execSync("git commit -m init", { cwd: repo });

  await createWorkspaceAndOpenWorkbench({
    page,
    request: page.request,
    repo,
    workspaceName: `ws-live-meter-${Date.now()}`,
  });
  await selectHarnessBySearch(page, "fake", /fake/i);

  const prompt = "slow-diff-test emit-live-context-window";
  const createTaskResponsePromise = page.waitForResponse((response) =>
    response.request().method() === "POST"
    && /\/api\/workspaces\/[^/]+\/tasks$/.test(response.url()),
  );
  await page.locator("textarea.wb-composer-textarea").first().fill(prompt);
  await page.getByRole("button", { name: "Send" }).click();
  const createTaskResponse = await createTaskResponsePromise;
  expect(createTaskResponse.ok()).toBe(true);
  const sessionId = String((await createTaskResponse.json() as { primary_session_id?: string | null }).primary_session_id ?? "");
  expect(sessionId).not.toBe("");

  const rows = page.locator(".wb-task-row");
  await expect(rows).toHaveCount(1, { timeout: 20_000 });
  await rows.first().click();

  const activeTextarea = page.locator(".wb-session-slot textarea.wb-active-textarea");
  await expect(activeTextarea).toBeVisible({ timeout: 20_000 });

  const contextWindow = page.locator(".wb-session-slot .wb-context-window");
  await expect(contextWindow).toHaveText("25% · 25/100", { timeout: 20_000 });
  await expect(contextWindow).toHaveAttribute("title", "Context Window: 25% · 25/100");
  await expect(page.getByText("Working")).toBeVisible();
  await expect(page.getByRole("button", { name: "Stop" })).toBeVisible();

  const terminal = await waitForTerminalState(request, sessionId, {
    timeoutMs: 60_000,
    pollMs: 1_000,
  });
  expect(terminal.terminalStatus, terminal.errorMessage ?? "fake-provider run did not complete").toBe("completed");
  await expect(contextWindow).toHaveText("25% · 25/100", { timeout: 20_000 });
});

test("workbench: seeded context window metrics render in the workbench UI", async ({ page, request }) => {
  test.setTimeout(120_000);

  const repo = mkdtempSync(path.join(tmpdir(), "ctx-e2e-provider-meter-"));
  execSync("git init", { cwd: repo });
  execSync("git config user.email test@example.com", { cwd: repo });
  execSync("git config user.name Test", { cwd: repo });
  writeFileSync(path.join(repo, "file.txt"), "hello\n");
  execSync("git add .", { cwd: repo });
  execSync("git commit -m init", { cwd: repo });

  const workspaceId = await createWorkspaceAndOpenWorkbench({
    page,
    request,
    repo,
    workspaceName: `provider-meter-${Date.now()}`,
  });

  const task = await apiPost<{ id: string; primary_session_id?: string | null }>(request, `/api/workspaces/${workspaceId}/tasks`, {
    title: SEEDED_HARNESS_CASE.title,
    default_session: {
      provider_id: SEEDED_HARNESS_CASE.providerId,
      model_id: SEEDED_HARNESS_CASE.modelId,
      execution_environment: "host",
    },
  });
  const session = { id: task.primary_session_id };
  if (!session.id) throw new Error(`seeded task ${task.id} did not include a primary session`);
  await waitForSessionSnapshotReady(request, session.id);
  await apiPost(request, `/api/dev/sessions/${session.id}/seed_transcript`, {
    turns: [
      {
        user: `seed ${SEEDED_HARNESS_CASE.providerId}`,
        assistant: `assistant ${SEEDED_HARNESS_CASE.providerId}`,
        context_window: SHARED_CONTEXT_WINDOW,
      },
    ],
  });

  await page.reload({ waitUntil: "domcontentloaded" });
  await expect(page.locator(".wb-main")).toBeVisible({ timeout: 20_000 });

  const rows = page.locator(".wb-task-row");
  await expect(rows).toHaveCount(1, { timeout: 20_000 });

  const row = rows.filter({ hasText: SEEDED_HARNESS_CASE.title }).first();
  await expect(row).toBeVisible({ timeout: 20_000 });
  await row.click();
  const contextWindow = page.locator(".wb-session-slot .wb-context-window");
  await expect(contextWindow).toBeVisible({ timeout: 20_000 });
  await expect(contextWindow).toHaveText(EXPECTED_CONTEXT_WINDOW_SUMMARY, { timeout: 20_000 });
  await expect(contextWindow).toHaveAttribute(
    "title",
    `Context Window: ${EXPECTED_CONTEXT_WINDOW_SUMMARY}`,
  );
});
