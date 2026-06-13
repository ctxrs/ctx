import { test, expect } from "./fixtures";
import { mkdtempSync, rmSync, writeFileSync } from "fs";
import { tmpdir } from "os";
import path from "path";
import { execSync } from "child_process";
import { createWorkspaceAndOpenWorkbench } from "./utils/workbench";
import { selectHarnessBySearch } from "./utils/harnessEndpointAuth";
import type { APIRequestContext, Page } from "playwright/test";

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

  await selectHarnessBySearch(page, "fake", /fake/i);

  await page.locator("textarea.wb-composer-textarea").first().fill(prompt);
  await page.getByRole("button", { name: "Send" }).click();

  const sessionComposer = page.locator(".wb-session-slot[aria-hidden=\"false\"] textarea.wb-active-textarea");
  await expect(sessionComposer).toBeVisible({ timeout: 20_000 });

  const readId = (v: unknown): string => (typeof v === "string" ? v : "");

  const asRecord = (value: unknown): Record<string, unknown> =>
    value && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : {};

  const asArray = (value: unknown): unknown[] => (Array.isArray(value) ? value : []);

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
        const sessions = asArray(taskSummary.sessions).map((entry) => asRecord(entry));
        const sessionSummary = sessions.length > 0 ? sessions[sessions.length - 1] : {};
        const primarySessionId = readId(asRecord(taskSummary.task).primary_session_id);
        sessionId = readId(asRecord(sessionSummary.session).id) || primarySessionId;
        return sessionId;
      },
      { timeout: 20_000 },
    )
    .not.toBe("");

  const headResp = await request.get(`/api/sessions/${sessionId}/snapshot?limit=1`);
  expect(headResp.ok()).toBeTruthy();
  const snapshot = asRecord(await headResp.json());
  const head = asRecord(snapshot.head);
  const summary = asRecord(snapshot.summary);
  const session = asRecord(head.session ?? summary.session);
  const worktreeId = readId(session?.worktree_id);
  expect(worktreeId).toBeTruthy();
  const wtResp = await request.get(`/api/worktrees/${worktreeId}`);
  expect(wtResp.ok()).toBeTruthy();
  const wt = asRecord(await wtResp.json());
  const worktreeRoot = String(wt?.root_path ?? "");
  expect(worktreeRoot).toBeTruthy();

  return { sessionId, worktreeRoot, workspaceId, worktreeId };
}

async function waitForWorktreeSummary(opts: {
  request: APIRequestContext;
  workspaceId: string;
  worktreeId: string;
  timeoutMs?: number;
}) {
  const { request, workspaceId, worktreeId, timeoutMs = 20_000 } = opts;
  await expect
    .poll(
      async () => {
        const resp = await request.get(`/api/workspaces/${workspaceId}/active_snapshot?limit=5`);
        if (!resp.ok()) return null;
        const snapshot = (await resp.json()) as {
          worktree_vcs_snapshots?: Array<{
            worktree_id?: string;
            compute_state?: string;
            summary?: { file_count?: number | null; line_count?: number | null };
          }>;
        };
        const entry = snapshot?.worktree_vcs_snapshots?.find((item) => item?.worktree_id === worktreeId);
        if (!entry) return null;
        if (entry?.compute_state !== "ready") return null;
        const fileCount = entry?.summary?.file_count ?? null;
        const lineCount = entry?.summary?.line_count ?? null;
        if (fileCount === null && lineCount === null) return null;
        return Number(fileCount ?? lineCount);
      },
      { timeout: timeoutMs },
    )
    .toBeGreaterThan(0);
}

async function waitForNoRepoUnavailable(opts: {
  request: APIRequestContext;
  workspaceId: string;
  worktreeId: string;
  timeoutMs?: number;
}) {
  const { request, workspaceId, worktreeId, timeoutMs = 20_000 } = opts;
  await expect
    .poll(
      async () => {
        const resp = await request.get(`/api/workspaces/${workspaceId}/active_snapshot?limit=5`);
        if (!resp.ok()) return false;
        const snapshot = (await resp.json()) as {
          worktree_vcs_snapshots?: Array<{
            worktree_id?: string;
            available?: boolean;
            unavailable_reason?: string;
          }>;
        };
        const entry = snapshot?.worktree_vcs_snapshots?.find((item) => item?.worktree_id === worktreeId);
        if (!entry) return false;
        return entry?.available === false && entry?.unavailable_reason === "no_repo";
      },
      { timeout: timeoutMs },
    )
    .toBeTruthy();
}

test("workbench: diff fetch failure does not render false no-changes state", async ({ page, request }) => {
  const repo = mkdtempSync(path.join(tmpdir(), "ctx-e2e-"));
  execSync("git init -b main", { cwd: repo });
  execSync("git config user.email test@example.com", { cwd: repo });
  execSync("git config user.name Test", { cwd: repo });
  writeFileSync(path.join(repo, "file.txt"), "hello\n");
  execSync("git add .", { cwd: repo });
  execSync("git commit -m init", { cwd: repo });

  const { worktreeRoot, workspaceId, worktreeId } = await createWorkspaceAndStartRun({
    page,
    request,
    repo,
    workspaceName: `ws-${Date.now()}`,
    prompt: "diff fetch failure",
  });

  writeFileSync(path.join(worktreeRoot, "file.txt"), "hello\nphase1\n");
  await waitForWorktreeSummary({ request, workspaceId, worktreeId });

  await page.route(/\/api\/sessions\/[^/]+\/diff$/, async (route) => {
    await route.fulfill({
      status: 500,
      contentType: "application/json",
      body: JSON.stringify({ error: "simulated diff fetch failure" }),
    });
  });

  const diffButton = page.getByRole("button", { name: "Toggle diff view" });
  const diffBadge = diffButton.locator(".wb-icon-badge");
  await expect(diffBadge).toHaveText("1", { timeout: 20_000 });
  await diffButton.click();

  const diffPane = page.locator(".wb-right-pane.wb-diff");
  await expect(diffPane).toBeVisible({ timeout: 10_000 });
  await expect(diffPane.locator(".cursor-diff-file-header").filter({ hasText: "file.txt" })).toBeVisible({
    timeout: 20_000,
  });
  await expect(diffPane).toContainText("Failed to load diff content", { timeout: 20_000 });
  await expect(diffPane.getByText("No changes.")).toHaveCount(0);
  await expect(diffPane.getByText("No changes on this worktree.")).toHaveCount(0);
});

test("workbench: no-repo diff unavailable is non-fatal and stable", async ({ page, request }) => {
  const repo = mkdtempSync(path.join(tmpdir(), "ctx-e2e-"));
  execSync("git init -b main", { cwd: repo });
  execSync("git config user.email test@example.com", { cwd: repo });
  execSync("git config user.name Test", { cwd: repo });
  writeFileSync(path.join(repo, "file.txt"), "hello\n");
  execSync("git add .", { cwd: repo });
  execSync("git commit -m init", { cwd: repo });

  const { sessionId, worktreeRoot, workspaceId, worktreeId } = await createWorkspaceAndStartRun({
    page,
    request,
    repo,
    workspaceName: `ws-${Date.now()}`,
    prompt: "no-repo-unavailable",
  });

  rmSync(path.join(worktreeRoot, ".git"), { recursive: true, force: true });
  await waitForNoRepoUnavailable({ request, workspaceId, worktreeId });

  let diffRequestCount = 0;
  await page.route(/\/api\/sessions\/[^/]+\/diff$/, async (route) => {
    diffRequestCount += 1;
    await route.continue();
  });

  const diffButton = page.getByRole("button", { name: "Toggle diff view" });
  await expect(diffButton.locator(".wb-icon-badge")).toHaveCount(0);
  await diffButton.click();

  const diffPane = page.locator(".wb-right-pane.wb-diff");
  await expect(diffPane).toBeVisible({ timeout: 10_000 });
  await expect(diffPane).toContainText("No git repo detected for this workspace yet.", { timeout: 20_000 });
  await expect(diffPane.getByText("Failed to load diff content")).toHaveCount(0);
  await expect.poll(() => diffRequestCount, { timeout: 5_000 }).toBe(0);

  // Poll snapshot multiple times to simulate rev churn and verify the pane does not start refetching.
  for (let i = 0; i < 5; i += 1) {
    const resp = await request.get(`/api/workspaces/${workspaceId}/active_snapshot?limit=5`);
    expect(resp.ok()).toBeTruthy();
  }
  await expect.poll(() => diffRequestCount, { timeout: 2_000 }).toBe(0);

  const diffResp = await request.get(`/api/sessions/${sessionId}/diff`);
  expect(diffResp.ok()).toBeTruthy();
  const diff = (await diffResp.json()) as { available?: boolean; unavailable_reason?: string };
  expect(diff?.available).toBe(false);
  expect(diff?.unavailable_reason).toBe("no_repo");
});
