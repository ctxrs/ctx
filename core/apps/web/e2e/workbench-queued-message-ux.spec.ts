import { test, expect } from "./fixtures";
import { mkdtempSync, writeFileSync } from "fs";
import { tmpdir } from "os";
import path from "path";
import { execSync } from "child_process";
import type { Page, Response } from "@playwright/test";
import { createWorkspaceAndOpenWorkbench, waitForWorkbenchProjectionReady } from "./utils/workbench";
import { selectHarnessBySearch } from "./utils/harnessEndpointAuth";

test.describe.skip("workbench queued message UX", () => {
test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => {
    (window as Window & { __CTX_FEATURE_FLAGS__?: Record<string, unknown> }).__CTX_FEATURE_FLAGS__ = {
      queued_messages_enabled: true,
    };
  });
});

const setupRunningSession = async (page: Page) => {
  const repo = mkdtempSync(path.join(tmpdir(), "ctx-e2e-"));
  execSync("git init", { cwd: repo });
  execSync("git config user.email test@example.com", { cwd: repo });
  execSync("git config user.name Test", { cwd: repo });
  writeFileSync(path.join(repo, "file.txt"), "hello\n");
  execSync("git add .", { cwd: repo });
  execSync("git commit -m init", { cwd: repo });

  const workspaceName = `ws-${Date.now()}`;
  await createWorkspaceAndOpenWorkbench({ page, request: page.request, repo, workspaceName, debug: true });

  await selectHarnessBySearch(page, "fake", /fake/i);

  const slowMessage = `slow-diff-test
[[tool_calls]]
[
  {"kind":"execute","title":"t1","input":{"command":"echo 1"}},
  {"kind":"execute","title":"t2","input":{"command":"echo 2"}},
  {"kind":"execute","title":"t3","input":{"command":"echo 3"}},
  {"kind":"execute","title":"t4","input":{"command":"echo 4"}}
]
[[/tool_calls]]`;

  await page.locator("textarea.wb-composer-textarea").first().fill(slowMessage);
  const createTaskResp = page.waitForResponse((resp: Response) => {
    if (resp.request().method() !== "POST") return false;
    return /\/api\/workspaces\/[^/]+\/tasks$/.test(resp.url()) && resp.status() === 200;
  });
  await page.getByRole("button", { name: "Send" }).click();
  await createTaskResp;

  await expect(page.locator(".wb-session-slot[aria-hidden=\"false\"] textarea.wb-active-textarea")).toBeVisible({ timeout: 20_000 });
  await expect(page.locator(".wb-session button[aria-label=\"Stop\"]")).toBeVisible({ timeout: 20_000 });
  await waitForWorkbenchProjectionReady(page, {
    requireActiveTurn: true,
    requireAuthoritative: true,
    timeout: 30_000,
  });
};

const queueMessage = async (page: Page, text: string) => {
  const sessionComposer = page.locator(".wb-session-slot[aria-hidden=\"false\"] textarea.wb-active-textarea");
  await sessionComposer.fill(text);
  const sendResp = page.waitForResponse((resp: Response) => {
    if (resp.request().method() !== "POST") return false;
    if (!/\/api\/sessions\/[^/]+\/messages$/.test(resp.url())) return false;
    const body = resp.request().postData() ?? "";
    return body.includes(text);
  });
  await page.locator(".wb-session-slot[aria-hidden=\"false\"] button[aria-label=\"Send\"]").click();
  const resp = await sendResp;
  expect(resp.status(), `queue message POST failed: ${resp.url()}`).toBe(200);
};

const delayDeleteMessage = async (page: Page, delayMs: number) => {
  await page.route("**/api/sessions/*/messages/*", async (route) => {
    const req = route.request();
    if (req.method() !== "DELETE") return route.continue();
    await new Promise((resolve) => setTimeout(resolve, delayMs));
    return route.continue();
  });
};

test("workbench: queued sends do not flash optimistic turn and queue panel clears when started", async ({ page }) => {
  await setupRunningSession(page);

  const queuedText = `queued-msg-${Date.now()}`;

  await page.route("**/api/sessions/*/messages", async (route) => {
    const req = route.request();
    if (req.method() !== "POST") return route.continue();
    const body = req.postData() ?? "";
    if (body.includes(queuedText)) {
      await new Promise((resolve) => setTimeout(resolve, 1500));
    }
    return route.continue();
  });

  await queueMessage(page, queuedText);

  // Queued sends should not appear as an optimistic/pending turn before the server acknowledges them.
  const threadScroller = page.locator(".wb-session-slot .wb-thread-scroller");
  await expect(threadScroller).not.toContainText(queuedText, { timeout: 1200 });

  const queuePanel = page.locator(".wb-session .queue-panel");
  await expect(queuePanel).toBeVisible({ timeout: 20_000 });
  await expect(queuePanel).toContainText(queuedText, { timeout: 20_000 });

  // When the queued message starts running, it should be removed from the queue panel.
  await expect(queuePanel).toHaveCount(0, { timeout: 60_000 });
  await expect(page.locator(".wb-session")).toContainText(queuedText, { timeout: 20_000 });
});

test("workbench: edit queued message hides queue panel immediately", async ({ page }) => {
  await setupRunningSession(page);

  const queuedText = `queued-edit-${Date.now()}`;
  await queueMessage(page, queuedText);

  const queuePanel = page.locator(".wb-session .queue-panel");
  await expect(queuePanel).toBeVisible({ timeout: 20_000 });
  await expect(queuePanel).toContainText(queuedText, { timeout: 20_000 });

  await delayDeleteMessage(page, 1500);

  await queuePanel.getByRole("button", { name: "Edit queued message" }).click();

  await expect(page.locator(".wb-session-slot[aria-hidden=\"false\"] textarea.wb-active-textarea")).toHaveValue(queuedText);
  await expect(queuePanel).toHaveCount(0, { timeout: 800 });
});

test("workbench: trash queued message hides queue panel immediately", async ({ page }) => {
  await setupRunningSession(page);

  const queuedText = `queued-trash-${Date.now()}`;
  await queueMessage(page, queuedText);

  const queuePanel = page.locator(".wb-session .queue-panel");
  await expect(queuePanel).toBeVisible({ timeout: 20_000 });
  await expect(queuePanel).toContainText(queuedText, { timeout: 20_000 });

  await delayDeleteMessage(page, 1500);

  await queuePanel.getByRole("button", { name: "Cancel queued message" }).click();
  await expect(queuePanel).toHaveCount(0, { timeout: 800 });
});

test("workbench: queued list updates when removing the first item", async ({ page }) => {
  await setupRunningSession(page);

  const firstQueued = `queued-first-${Date.now()}`;
  const secondQueued = `queued-second-${Date.now()}`;
  await queueMessage(page, firstQueued);
  const queuePanel = page.locator(".wb-session .queue-panel");
  await expect(queuePanel).toContainText(firstQueued, { timeout: 20_000 });

  await queueMessage(page, secondQueued);
  await expect(queuePanel).toContainText(secondQueued, { timeout: 20_000 });

  await queuePanel.getByRole("button", { name: "Cancel queued message" }).first().click();

  await expect(queuePanel).toContainText(secondQueued, { timeout: 20_000 });
  await expect(queuePanel).not.toContainText(firstQueued, { timeout: 20_000 });
  await expect(queuePanel.getByRole("button", { name: "Send now" })).toBeVisible({ timeout: 20_000 });
});

test("workbench: send now removes queued message immediately", async ({ page }) => {
  await setupRunningSession(page);

  const queuedText = `queued-send-now-${Date.now()}`;
  await queueMessage(page, queuedText);

  const queuePanel = page.locator(".wb-session .queue-panel");
  await expect(queuePanel).toBeVisible({ timeout: 20_000 });
  await expect(queuePanel).toContainText(queuedText, { timeout: 20_000 });

  await delayDeleteMessage(page, 1500);

  await queuePanel.getByRole("button", { name: "Send now" }).click();
  await expect(queuePanel).toHaveCount(0, { timeout: 800 });
});

test("workbench: send now renders optimistic header before message POST resolves", async ({ page }) => {
  await setupRunningSession(page);

  const queuedText = `queued-send-now-optimistic-${Date.now()}`;
  await queueMessage(page, queuedText);

  const queuePanel = page.locator(".wb-session .queue-panel");
  await expect(queuePanel).toBeVisible({ timeout: 20_000 });
  await expect(queuePanel).toContainText(queuedText, { timeout: 20_000 });

  let allowSendNowPost: (() => void) | null = null;
  const sendNowPostGate = new Promise<void>((resolve) => {
    allowSendNowPost = resolve;
  });
  let stalledSendNowPost = true;
  await page.route("**/api/sessions/*/messages", async (route) => {
    if (stalledSendNowPost && route.request().method() === "POST") {
      const body = route.request().postData() ?? "";
      if (body.includes(queuedText)) {
        stalledSendNowPost = false;
        await sendNowPostGate;
      }
    }
    await route.continue();
  });

  type SendNowWindow = Window & {
    __sendNowClickAt?: number;
    __sendNowHeaderSeen?: boolean;
    __sendNowHeaderDisappeared?: boolean;
    __sendNowHeaderDuplicated?: boolean;
    __sendNowHeaderItemId?: string | null;
  };

  await page.evaluate((promptText: string) => {
    const w = window as SendNowWindow;
    w.__sendNowClickAt = performance.now();
    w.__sendNowHeaderSeen = false;
    w.__sendNowHeaderDisappeared = false;
    w.__sendNowHeaderDuplicated = false;
    w.__sendNowHeaderItemId = null;

    const selector = ".wb-session-slot .wb-turn-header-content";
    const monitorWindowMs = 1500;
    const startAt = w.__sendNowClickAt;

    const getMatches = () =>
      Array.from(document.querySelectorAll(selector))
        .filter((node) => (node.textContent ?? "").includes(promptText))
        .map((node) => ({
          node,
          itemId: node.closest("[data-thread-item-id]")?.getAttribute("data-thread-item-id") ?? null,
        }));

    const tick = () => {
      const elapsed = performance.now() - startAt;
      const matches = getMatches();
      const itemIds = matches.map((match) => match.itemId).filter(Boolean) as string[];
      if (!w.__sendNowHeaderSeen && itemIds.length > 0) {
        w.__sendNowHeaderSeen = true;
        w.__sendNowHeaderItemId = itemIds[0];
      }
      if (w.__sendNowHeaderSeen && w.__sendNowHeaderItemId) {
        if (!itemIds.includes(w.__sendNowHeaderItemId)) {
          w.__sendNowHeaderDisappeared = true;
        }
      }
      if (new Set(itemIds).size > 1) w.__sendNowHeaderDuplicated = true;
      if (elapsed < monitorWindowMs) requestAnimationFrame(tick);
    };

    requestAnimationFrame(tick);
  }, queuedText);

  await queuePanel.getByRole("button", { name: "Send now" }).click();
  await expect(queuePanel).toHaveCount(0, { timeout: 800 });

  const header = page
    .locator(".wb-session-slot .wb-turn-header-content")
    .filter({ hasText: queuedText })
    .first();
  await expect(header).toBeVisible({ timeout: 2000 });
  const elapsedMs = await page.evaluate(() => {
    const w = window as SendNowWindow;
    return performance.now() - Number(w.__sendNowClickAt ?? 0);
  });
  expect(elapsedMs).toBeLessThan(500);

  const headerItemId = await header.evaluate((node) =>
    node.closest("[data-thread-item-id]")?.getAttribute("data-thread-item-id"),
  );
  expect(headerItemId).toBeTruthy();
  allowSendNowPost?.();

  await page.waitForTimeout(400);
  if (headerItemId) {
    await expect(
      page.locator(`[data-thread-item-id="${headerItemId}"] .wb-turn-header-content`),
    ).toBeVisible();
  }

  const { headerDisappeared, headerDuplicated } = await page.evaluate(() => {
    const w = window as SendNowWindow;
    return {
      headerDisappeared: Boolean(w.__sendNowHeaderDisappeared),
      headerDuplicated: Boolean(w.__sendNowHeaderDuplicated),
    };
  });

  expect(headerDisappeared).toBe(false);
  expect(headerDuplicated).toBe(false);
});
});
