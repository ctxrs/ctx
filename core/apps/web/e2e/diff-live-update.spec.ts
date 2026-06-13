import { test, expect } from "./fixtures";
import { mkdtempSync, writeFileSync } from "fs";
import { tmpdir } from "os";
import path from "path";
import { execSync } from "child_process";
import type { APIRequestContext, Page } from "@playwright/test";
import { createWorkspaceAndOpenWorkbench } from "./utils/workbench";
import { selectHarnessBySearch } from "./utils/harnessEndpointAuth";

const asRecord = (value: unknown): Record<string, unknown> => {
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  return value as Record<string, unknown>;
};

const asArray = (value: unknown): unknown[] => (Array.isArray(value) ? value : []);

const readId = (value: unknown): string => (typeof value === "string" ? value : "");

async function createWorkspaceAndStartRun(opts: {
  page: Page;
  request: APIRequestContext;
  repo: string;
  workspaceName: string;
  prompt: string;
}) {
  const { page, repo, workspaceName, prompt, request } = opts;
  const workspaceId = await createWorkspaceAndOpenWorkbench({
    page,
    request,
    repo,
    workspaceName,
  });

  // Choose Fake harness so the test doesn't depend on external agents.
  await selectHarnessBySearch(page, "fake", /fake/i);

  await page.locator("textarea.wb-composer-textarea").first().fill(prompt);
  await page.getByRole("button", { name: "Send" }).click();

  const sessionComposer = page.locator(".wb-session-slot[aria-hidden=\"false\"] textarea.wb-active-textarea");
  await expect(sessionComposer).toBeVisible({ timeout: 20_000 });

  let taskId = "";
  let sessionId = "";
  await expect
    .poll(
      async () => {
        const resp = await request.get(`/api/workspaces/${workspaceId}/active_snapshot`);
        if (!resp.ok()) return "";
        const snapshot = asRecord(await resp.json());
        const active = asRecord(snapshot.active);
        const taskSummary = asRecord(asArray(active.tasks)[0]);
        if (!taskSummary) return "";
        taskId = readId(asRecord(taskSummary.task).id);
        const sessions = asArray(taskSummary.sessions);
        const sessionSummary = asRecord(sessions[sessions.length - 1]);
        const primarySessionId = readId(asRecord(taskSummary.task).primary_session_id);
        sessionId = readId(asRecord(sessionSummary.session).id) || primarySessionId;
        return sessionId;
      },
      { timeout: 20_000 }
    )
    .not.toBe("");

  const headResp = await request.get(`/api/sessions/${sessionId}/snapshot?limit=1`);
  expect(headResp.ok()).toBeTruthy();
  const snapshot = asRecord(await headResp.json());
  const session = asRecord(asRecord(snapshot.head).session ?? asRecord(snapshot.summary).session);
  const worktreeId = readId(session.worktree_id);
  expect(worktreeId).toBeTruthy();
  const wtResp = await request.get(`/api/worktrees/${worktreeId}`);
  expect(wtResp.ok()).toBeTruthy();
  const wt = asRecord(await wtResp.json());
  const worktreeRoot = String(wt.root_path ?? "");
  expect(worktreeRoot).toBeTruthy();

  return { sessionId, worktreeRoot };
}

async function waitForSessionCompletion(opts: { request: APIRequestContext; sessionId: string; timeoutMs?: number }) {
  const { request, sessionId, timeoutMs = 30_000 } = opts;
  await expect
    .poll(
      async () => {
        const resp = await request.get(`/api/sessions/${sessionId}/snapshot?include_events=1&limit=60`);
        if (!resp.ok()) return "";
        const data = asRecord(await resp.json());
        const summaryStatus = asRecord(asRecord(data.summary).activity).last_turn_status ?? null;
        if (summaryStatus && !["running", "queued"].includes(String(summaryStatus))) return "done";
        const head = asRecord(data.head);
        const headStatus = asRecord(head.activity).last_turn_status ?? null;
        if (headStatus && !["running", "queued"].includes(String(headStatus))) return "done";
        const turns = asArray(head.turns).map((turn) => asRecord(turn));
        const lastTurn = turns[turns.length - 1];
        const lastStatus = lastTurn?.status ?? null;
        if (lastStatus && !["running", "queued"].includes(String(lastStatus))) return "done";
        return "";
      },
      { timeout: timeoutMs },
    )
    .not.toBe("");
}

test("workbench: diff updates mid-turn", async ({ page, request }) => {
  const repo = mkdtempSync(path.join(tmpdir(), "ctx-e2e-"));
  execSync("git init", { cwd: repo });
  execSync("git config user.email test@example.com", { cwd: repo });
  execSync("git config user.name Test", { cwd: repo });
  writeFileSync(path.join(repo, "file.txt"), "hello\n");
  execSync("git add .", { cwd: repo });
  execSync("git commit -m init", { cwd: repo });

  const workspaceName = `ws-${Date.now()}`;
  const taskTitle = "slow-diff-test";

  const { worktreeRoot, sessionId } = await createWorkspaceAndStartRun({
    page,
    request,
    repo,
    workspaceName,
    prompt: taskTitle,
  });

  writeFileSync(path.join(worktreeRoot, "file.txt"), "hello\nchanged while running\n");

  const diffResp = await request.get(`/api/sessions/${sessionId}/diff`);
  expect(diffResp.ok()).toBeTruthy();
  const diff = await diffResp.json();
  expect(String(diff?.diff ?? "")).toContain("file.txt");
});

test("workbench: diff updates for manual edits while idle", async ({ page, request }) => {
  const repo = mkdtempSync(path.join(tmpdir(), "ctx-e2e-"));
  execSync("git init", { cwd: repo });
  execSync("git config user.email test@example.com", { cwd: repo });
  execSync("git config user.name Test", { cwd: repo });
  writeFileSync(path.join(repo, "file.txt"), "hello\n");
  execSync("git add .", { cwd: repo });
  execSync("git commit -m init", { cwd: repo });

  const workspaceName = `ws-${Date.now()}`;
  const taskTitle = "idle-diff-test";
  const { sessionId, worktreeRoot } = await createWorkspaceAndStartRun({
    page,
    request,
    repo,
    workspaceName,
    prompt: taskTitle,
  });

  await waitForSessionCompletion({ request, sessionId });

  // Simulate user editing the worktree outside the agent.
  writeFileSync(path.join(worktreeRoot, "file.txt"), "hello\nchanged while idle\n");

  const diffResp = await request.get(`/api/sessions/${sessionId}/diff`);
  expect(diffResp.ok()).toBeTruthy();
  const diff = await diffResp.json();
  expect(String(diff?.diff ?? "")).toContain("file.txt");
});
