import { test, expect } from "./fixtures";
import { mkdtempSync, writeFileSync } from "fs";
import { tmpdir } from "os";
import path from "path";
import { execSync } from "child_process";
import { createWorkspaceAndOpenWorkbench } from "./utils/workbench";
import { selectHarnessBySearch } from "./utils/harnessEndpointAuth";

type LayoutShiftSample = {
  startTime: number;
  value: number;
  hadRecentInput: boolean;
};

type OptimisticWindow = Window & {
  __queuePanelSeen?: boolean;
  __queuePanelObserver?: MutationObserver;
  __layoutShiftEntries?: LayoutShiftSample[];
  __layoutShiftObserver?: PerformanceObserver;
  __sendClickAt?: number;
  __optimisticHeaderSeen?: boolean;
  __optimisticHeaderDisappeared?: boolean;
  __optimisticHeaderDuplicated?: boolean;
  __optimisticHeaderItemId?: string | null;
  __optimisticHeaderAt?: number;
};

test("workbench: optimistic new task message skips queued UI", async ({ page }) => {
  test.setTimeout(120000);
  await page.setViewportSize({ width: 1400, height: 900 });

  const repo = mkdtempSync(path.join(tmpdir(), "ctx-e2e-"));
  execSync("git init", { cwd: repo });
  execSync("git config user.email test@example.com", { cwd: repo });
  execSync("git config user.name Test", { cwd: repo });
  writeFileSync(path.join(repo, "file.txt"), "hello\n");
  execSync("git add .", { cwd: repo });
  execSync("git commit -m init", { cwd: repo });

  const workspaceName = `ws-${Date.now()}`;
  await createWorkspaceAndOpenWorkbench({ page, request: page.request, repo, workspaceName });

  // Choose Fake harness so the test doesn't depend on external agents.
  await selectHarnessBySearch(page, "fake", /fake/i);

  // Stall the first task create request so we can assert the optimistic turn renders
  // immediately (i.e. without waiting on the daemon response).
  let allowFirstCreateTask: (() => void) | null = null;
  const firstCreateTaskGate = new Promise<void>((resolve) => {
    allowFirstCreateTask = resolve;
  });
  let stalledCreateTask = true;
  await page.route("**/api/workspaces/*/tasks", async (route) => {
    if (stalledCreateTask && route.request().method() === "POST") {
      stalledCreateTask = false;
      await firstCreateTaskGate;
    }
    await route.continue();
  });

  const prompt = `optimistic-${Date.now()}`;
  const composer = page.locator("textarea.wb-composer-textarea").first();
  await expect(composer).toBeVisible({ timeout: 20000 });
  await composer.fill(prompt);

  await page.evaluate(() => {
    const w = window as OptimisticWindow;
    w.__queuePanelSeen = false;
    w.__queuePanelObserver?.disconnect?.();
    const queueObserver = new MutationObserver(() => {
      if (document.querySelector(".queue-panel")) {
        w.__queuePanelSeen = true;
      }
    });
    queueObserver.observe(document.body, { childList: true, subtree: true, attributes: true });
    w.__queuePanelObserver = queueObserver;

    w.__layoutShiftEntries = [];
    w.__layoutShiftObserver?.disconnect?.();
    const shiftObserver = new PerformanceObserver((list) => {
      for (const entry of list.getEntries()) {
        const shiftEntry = entry as PerformanceEntry & { value?: number; hadRecentInput?: boolean };
        w.__layoutShiftEntries.push({
          startTime: shiftEntry.startTime,
          value: shiftEntry.value ?? 0,
          hadRecentInput: shiftEntry.hadRecentInput ?? false,
        });
      }
    });
    shiftObserver.observe({ type: "layout-shift", buffered: false });
    w.__layoutShiftObserver = shiftObserver;
  });

  await page.evaluate((promptText: string) => {
    const w = window as OptimisticWindow;
    w.__sendClickAt = performance.now();
    w.__optimisticHeaderSeen = false;
    w.__optimisticHeaderDisappeared = false;
    w.__optimisticHeaderDuplicated = false;
    w.__optimisticHeaderItemId = null;

    const selector = ".wb-session-slot .wb-turn-header-content";
    const monitorWindowMs = 1500;
    const startAt = w.__sendClickAt;

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
      if (!w.__optimisticHeaderSeen && itemIds.length > 0) {
        w.__optimisticHeaderSeen = true;
        w.__optimisticHeaderItemId = itemIds[0];
      }
      if (w.__optimisticHeaderSeen && w.__optimisticHeaderItemId) {
        if (!itemIds.includes(w.__optimisticHeaderItemId)) {
          w.__optimisticHeaderDisappeared = true;
        }
      }
      if (new Set(itemIds).size > 1) w.__optimisticHeaderDuplicated = true;
      if (elapsed < monitorWindowMs) requestAnimationFrame(tick);
    };

    requestAnimationFrame(tick);
  }, prompt);
  await page.getByRole("button", { name: "Send" }).click();

  const header = page
    .locator(".wb-session-slot .wb-turn-header-content")
    .filter({ hasText: prompt })
    .first();
  await expect(header).toBeVisible({ timeout: 2000 });
  const headerItemId = await header.evaluate((node) =>
    node.closest("[data-thread-item-id]")?.getAttribute("data-thread-item-id"),
  );
  expect(headerItemId).toBeTruthy();
  const elapsedMs = await page.evaluate(() => {
    const w = window as OptimisticWindow;
    return performance.now() - Number(w.__sendClickAt ?? 0);
  });
  // Expect a fast optimistic render, but allow some variance across CI/dev machines.
  expect(elapsedMs).toBeLessThan(500);

  // Release the stalled create-task request now that we verified the optimistic UI.
  allowFirstCreateTask?.();

  await page.evaluate(() => {
    (window as OptimisticWindow).__optimisticHeaderAt = performance.now();
  });
  await page.waitForTimeout(400);
  if (headerItemId) {
    await expect(
      page.locator(`[data-thread-item-id="${headerItemId}"] .wb-turn-header-content`),
    ).toBeVisible();
  } else {
    await expect(header).toBeVisible();
  }

  const { queuePanelSeen, shiftAfterHeader, headerDisappeared, headerDuplicated } = await page.evaluate(() => {
    const w = window as OptimisticWindow;
    const seenAt = w.__optimisticHeaderAt ?? 0;
    const entries = Array.isArray(w.__layoutShiftEntries) ? w.__layoutShiftEntries : [];
    const shiftAfterHeader = entries.filter((entry) => entry.startTime >= seenAt).reduce((sum, entry) => sum + entry.value, 0);
    return {
      queuePanelSeen: Boolean(w.__queuePanelSeen),
      shiftAfterHeader,
      headerDisappeared: Boolean(w.__optimisticHeaderDisappeared),
      headerDuplicated: Boolean(w.__optimisticHeaderDuplicated),
    };
  });

  expect(queuePanelSeen).toBe(false);
  expect(headerDisappeared).toBe(false);
  expect(headerDuplicated).toBe(false);
  // This is a canary for the old "optimistic row removed then re-inserted" behavior.
  // In practice CLS values can be slightly noisy across environments, so keep this
  // threshold lenient enough to avoid flakes while still catching large shifts.
  expect(shiftAfterHeader).toBeLessThan(0.01);
  await expect(page.locator(".queue-panel")).toHaveCount(0);
  await expect(page.locator(".queue-item-content").filter({ hasText: prompt })).toHaveCount(0);
});

test("workbench: sending selected new-task text clears lingering selection state", async ({ page }) => {
  test.setTimeout(120000);
  await page.setViewportSize({ width: 1400, height: 900 });

  const repo = mkdtempSync(path.join(tmpdir(), "ctx-e2e-"));
  execSync("git init", { cwd: repo });
  execSync("git config user.email test@example.com", { cwd: repo });
  execSync("git config user.name Test", { cwd: repo });
  writeFileSync(path.join(repo, "file.txt"), "hello\n");
  execSync("git add .", { cwd: repo });
  execSync("git commit -m init", { cwd: repo });

  const workspaceName = `ws-${Date.now()}`;
  await createWorkspaceAndOpenWorkbench({ page, request: page.request, repo, workspaceName });
  await selectHarnessBySearch(page, "fake", /fake/i);

  const prompt = `alpha beta gamma ${Date.now()}`;
  const composer = page.locator("textarea.wb-composer-textarea").first();
  await expect(composer).toBeVisible({ timeout: 20000 });
  await composer.fill(prompt);
  await composer.evaluate((el: HTMLTextAreaElement) => {
    el.focus();
    const start = el.value.indexOf("beta");
    const end = start + "beta".length;
    el.setSelectionRange(start, end);
    el.dispatchEvent(new Event("select", { bubbles: true }));
  });

  await composer.press("Enter");

  const sentHeader = page
    .locator(".wb-session-slot .wb-turn-header-content")
    .filter({ hasText: prompt })
    .first();
  await expect(sentHeader).toBeVisible({ timeout: 10000 });

  const selectionState = await page.evaluate(() => {
    const selectedTextareas = Array.from(document.querySelectorAll("textarea"))
      .map((node) => node as HTMLTextAreaElement)
      .filter((node) => (node.selectionStart ?? 0) !== (node.selectionEnd ?? 0))
      .map((node) => ({
        className: node.className,
        value: node.value,
        selectionStart: node.selectionStart,
        selectionEnd: node.selectionEnd,
        selectedText: node.value.slice(node.selectionStart ?? 0, node.selectionEnd ?? 0),
      }));
    return {
      globalSelection: window.getSelection()?.toString() ?? "",
      selectedTextareas,
    };
  });

  expect(selectionState.globalSelection).toBe("");
  expect(selectionState.selectedTextareas).toEqual([]);
});
