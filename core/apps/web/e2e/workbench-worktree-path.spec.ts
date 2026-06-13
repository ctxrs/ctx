import fs from "fs";
import path from "path";
import type { Page } from "playwright/test";

import { test, expect } from "./fixtures";
import { seedDummyWorkspace } from "./utils/seedDummyWorkspace";

const readId = (value: unknown): string => (typeof value === "string" ? value : "");

const asRecord = (value: unknown): Record<string, unknown> => {
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  return value as Record<string, unknown>;
};

const worktreeSlugFromPath = (worktreePath: string): string => {
  const trimmed = worktreePath.trim().replace(/[\\/]+$/, "");
  if (!trimmed) return "";
  const parts = trimmed.split(/[\\/]/).filter(Boolean);
  const base = parts[parts.length - 1] ?? "";
  const uuidMatch = base.match(/^([0-9a-f]{8})-[0-9a-f-]{27,}$/i);
  if (uuidMatch) return uuidMatch[1];
  if (base.length <= 16) return base;
  return `${base.slice(0, 16)}...`;
};

const captureProofScreenshot = async (page: Page, fileName: string): Promise<void> => {
  const explicitPath = process.env.CTX_WORKTREE_CHIP_SCREENSHOT_PATH?.trim();
  const undeclaredOutputDir = process.env.TEST_UNDECLARED_OUTPUTS_DIR?.trim();
  const screenshotPaths = [
    explicitPath || "",
    undeclaredOutputDir ? path.join(undeclaredOutputDir, fileName) : "",
  ].filter((candidate) => candidate.length > 0);
  for (const screenshotPath of Array.from(new Set(screenshotPaths))) {
    fs.mkdirSync(path.dirname(screenshotPath), { recursive: true });
    await page.screenshot({ path: screenshotPath, fullPage: true });
  }
};

const readClipboardText = async (page: Page): Promise<string> =>
  page.evaluate(() => navigator.clipboard.readText());

test("workbench: worktree slug is visible and copyable for the active session", async ({ page, request }) => {
  await page.addInitScript(() => {
    let clipboardText = "";
    const originalExecCommand = document.execCommand?.bind(document);
    document.execCommand = (commandId: string, showUI?: boolean, value?: string): boolean => {
      if (commandId.toLowerCase() === "copy") {
        const active = document.activeElement;
        if (active instanceof HTMLTextAreaElement || active instanceof HTMLInputElement) {
          clipboardText = active.value;
        }
        return true;
      }
      return originalExecCommand?.(commandId, showUI, value) ?? false;
    };
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: {
        readText: async () => clipboardText,
        writeText: async (nextText: string) => {
          clipboardText = String(nextText);
        },
      },
    });
  });
  const seed = await seedDummyWorkspace(request, {
    tasks: 1,
    sessionsPerTask: 1,
    turnsPerSession: 0,
    createDefaultTrack: false,
  });

  const taskId = seed.taskIds[0];
  const sessionId = seed.sessionIdsByTask[taskId][0];

  const sessionResp = await request.get(`/api/sessions/${sessionId}/snapshot?limit=1`);
  expect(sessionResp.ok()).toBeTruthy();
  const sessionSnapshot = asRecord(await sessionResp.json());
  const headSession = asRecord(asRecord(sessionSnapshot.head).session);
  const summarySession = asRecord(asRecord(sessionSnapshot.summary).session);
  const worktreeId = readId(
    headSession.worktree_id ?? summarySession.worktree_id,
  );
  expect(worktreeId).not.toEqual("");

  const worktreeResp = await request.get(`/api/worktrees/${worktreeId}`);
  expect(worktreeResp.ok()).toBeTruthy();
  const worktree = asRecord(await worktreeResp.json());
  const worktreePath = String(worktree.root_path ?? "");
  expect(worktreePath).not.toEqual("");

  await page.goto(`/workspaces/${seed.workspaceId}`, { waitUntil: "domcontentloaded" });
  const rows = page.locator(".wb-task-row");
  await expect(rows).toHaveCount(1);
  await rows.first().click();

  const worktreeChip = page.getByRole("button", { name: "Copy worktree location" }).first();
  await expect(worktreeChip).toBeVisible({ timeout: 20000 });
  await expect(worktreeChip).toContainText(worktreeSlugFromPath(worktreePath));
  await expect(worktreeChip).toBeEnabled();

  await worktreeChip.click();
  await expect.poll(() => readClipboardText(page)).toBe(worktreePath);

  await page.evaluate(() => navigator.clipboard.writeText(""));
  await page.getByRole("button", { name: "Conversation options" }).click();
  const copyWorktreeMenuItem = page.getByRole("menuitem", { name: "Copy Worktree Location" });
  await expect(copyWorktreeMenuItem).toBeEnabled();
  await copyWorktreeMenuItem.click();
  await expect.poll(() => readClipboardText(page)).toBe(worktreePath);
});

test("workbench: worktree id is visible when worktree detail is unavailable", async ({ page, request }) => {
  const seed = await seedDummyWorkspace(request, {
    tasks: 1,
    sessionsPerTask: 1,
    turnsPerSession: 0,
    createDefaultTrack: false,
  });

  const taskId = seed.taskIds[0];
  const sessionId = seed.sessionIdsByTask[taskId][0];

  const sessionResp = await request.get(`/api/sessions/${sessionId}/snapshot?limit=1`);
  expect(sessionResp.ok()).toBeTruthy();
  const sessionSnapshot = asRecord(await sessionResp.json());
  const headSession = asRecord(asRecord(sessionSnapshot.head).session);
  const summarySession = asRecord(asRecord(sessionSnapshot.summary).session);
  const worktreeId = readId(
    headSession.worktree_id ?? summarySession.worktree_id,
  );
  expect(worktreeId).not.toEqual("");

  const archiveResp = await request.post(`/api/tasks/${taskId}/archive`, {});
  expect(archiveResp.ok()).toBeTruthy();

  await page.route("**/api/worktrees/**", async (route) => {
    const url = new URL(route.request().url());
    if (url.pathname === `/api/worktrees/${worktreeId}`) {
      await route.fulfill({
        status: 404,
        contentType: "application/json",
        body: JSON.stringify({ error: "worktree detail intentionally unavailable" }),
      });
      return;
    }
    await route.continue();
  });

  await page.goto(`/workspaces/${seed.workspaceId}`, { waitUntil: "domcontentloaded" });
  await expect(page.locator(".wb-main")).toBeVisible({ timeout: 20_000 });

  const archivedToggle = page.getByRole("button", { name: "Archived Tasks" });
  await archivedToggle.waitFor({ timeout: 10_000 });
  if ((await archivedToggle.getAttribute("aria-expanded")) !== "true") {
    await archivedToggle.click();
  }

  const archivedRow = page.locator(".wb-task-row-archived", { hasText: "fixture task 1" }).first();
  await expect(archivedRow).toBeVisible({ timeout: 20_000 });
  await archivedRow.click();

  const worktreeChip = page.getByRole("button", { name: "Copy worktree location" }).first();
  await expect(worktreeChip).toBeVisible({ timeout: 20_000 });
  await expect(worktreeChip).toContainText(worktreeSlugFromPath(worktreeId));
  await expect(worktreeChip).not.toContainText("Session worktree");

  await captureProofScreenshot(page, "worktree-id-fallback-proof.png");
});

test("workbench: worktree slug stays visible in single-track view", async ({ page, request }) => {
  const seed = await seedDummyWorkspace(request, {
    tasks: 1,
    sessionsPerTask: 1,
    turnsPerSession: 0,
    createDefaultTrack: false,
  });

  const taskId = seed.taskIds[0];
  const sessionId = seed.sessionIdsByTask[taskId][0];

  const sessionResp = await request.get(`/api/sessions/${sessionId}/snapshot?limit=1`);
  expect(sessionResp.ok()).toBeTruthy();
  const sessionSnapshot = asRecord(await sessionResp.json());
  const headSession = asRecord(asRecord(sessionSnapshot.head).session);
  const summarySession = asRecord(asRecord(sessionSnapshot.summary).session);
  const worktreeId = readId(
    headSession.worktree_id ?? summarySession.worktree_id,
  );
  expect(worktreeId).not.toEqual("");

  const worktreeResp = await request.get(`/api/worktrees/${worktreeId}`);
  expect(worktreeResp.ok()).toBeTruthy();
  const worktree = asRecord(await worktreeResp.json());
  const worktreePath = String(worktree.root_path ?? "");
  expect(worktreePath).not.toEqual("");

  await page.goto(`/workspaces/${seed.workspaceId}`, { waitUntil: "domcontentloaded" });
  const rows = page.locator(".wb-task-row");
  await expect(rows).toHaveCount(1);
  await rows.first().click();

  const worktreeChip = page.getByRole("button", { name: "Copy worktree location" }).first();
  await expect(worktreeChip).toBeVisible({ timeout: 20000 });
  await expect(worktreeChip).toContainText(worktreeSlugFromPath(worktreePath));
});
