import { test, expect } from "./fixtures";
import { mkdtempSync, writeFileSync } from "fs";
import { tmpdir } from "os";
import path from "path";
import { execSync } from "child_process";
import { createWorkspaceAndOpenWorkbench } from "./utils/workbench";
import { selectHarnessBySearch } from "./utils/harnessEndpointAuth";
import { readThreadSurfaceSample } from "./utils/messageListAcceptanceProbes";

type OptimisticWindow = Window & {
  __sendClickAt?: number;
  __optimisticHeaderSeen?: boolean;
  __optimisticHeaderDisappeared?: boolean;
  __optimisticHeaderDuplicated?: boolean;
  __optimisticHeaderItemId?: string | null;
};

async function collectOverlapSamples(page: Parameters<typeof test>[0]["page"], sampleCount = 18) {
  const samples = [];
  for (let index = 0; index < sampleCount; index += 1) {
    samples.push(await readThreadSurfaceSample(page));
    if (index < sampleCount - 1) await page.waitForTimeout(80);
  }
  return samples;
}

test("workbench: optimistic active-session message does not flash", async ({ page }) => {
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

  // Start a new task.
  await page.locator("textarea.wb-composer-textarea").first().fill(`first-${Date.now()}`);
  await page.getByRole("button", { name: "Send" }).click();

  const sessionComposer = page.locator(".wb-session-slot textarea.wb-active-textarea");
  await expect(sessionComposer).toBeVisible({ timeout: 20000 });

  // The session composer allows clicks while a turn is active, but the handler intentionally no-ops
  // unless queued-messages is enabled. Wait for the initial turn to finish so this test is stable.
  const initialStatus = page.locator(".wb-session-slot .wb-turn-status-label").first();
  await expect(initialStatus).toBeVisible({ timeout: 20000 });
  await expect(initialStatus).toHaveText(/completed|failed|interrupted/i, { timeout: 20000 });

  let allowSecondMessagePost: (() => void) | null = null;
  const secondMessagePostGate = new Promise<void>((resolve) => {
    allowSecondMessagePost = resolve;
  });
  let stalledSecondMessage = true;
  await page.route("**/api/sessions/*/messages", async (route) => {
    if (stalledSecondMessage && route.request().method() === "POST") {
      const body = route.request().postData() ?? "";
      if (body.includes(prompt)) {
        stalledSecondMessage = false;
        await secondMessagePostGate;
      }
    }
    await route.continue();
  });

  const prompt = `active-optimistic-${Date.now()}`;
  await sessionComposer.fill(prompt);

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

  await page.locator('.wb-session-slot button[aria-label="Send"]').click();

  const header = page
    .locator(".wb-session-slot .wb-turn-header-content")
    .filter({ hasText: prompt });

  await expect(header).toBeVisible({ timeout: 2000 });
  await expect(header).toHaveCount(1);
  const elapsedMs = await page.evaluate(() => {
    const w = window as OptimisticWindow;
    return performance.now() - Number(w.__sendClickAt ?? 0);
  });
  expect(elapsedMs).toBeLessThan(500);
  const headerItemId = await header.evaluate((node) =>
    node.closest("[data-thread-item-id]")?.getAttribute("data-thread-item-id"),
  );
  expect(headerItemId).toBeTruthy();
  expect(headerItemId).not.toContain("client-");
  const headerId = (headerItemId ?? "").replace("turn-header-", "");
  expect(headerId).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i);
  const overlapSamples = await collectOverlapSamples(page);
  allowSecondMessagePost?.();
  await page.waitForTimeout(400);
  if (headerItemId) {
    await expect(
      page.locator(`[data-thread-item-id="${headerItemId}"] .wb-turn-header-content`),
    ).toBeVisible();
  } else {
    await expect(header).toBeVisible();
  }

  const { headerDisappeared, headerDuplicated } = await page.evaluate(() => {
    const w = window as OptimisticWindow;
    return {
      headerDisappeared: Boolean(w.__optimisticHeaderDisappeared),
      headerDuplicated: Boolean(w.__optimisticHeaderDuplicated),
    };
  });

  expect(
    overlapSamples.some((sample) => sample.overlappingVisiblePairs > 0 || sample.overlappingTextLinePairs > 0),
  ).toBe(false);
  expect(headerDisappeared).toBe(false);
  expect(headerDuplicated).toBe(false);
});
