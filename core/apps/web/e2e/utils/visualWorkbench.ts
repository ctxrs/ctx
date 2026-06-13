import { expect, type Page } from "playwright/test";
import { selectHarnessBySearch } from "./harnessEndpointAuth";
import { prepareVisualPage, type VisualTheme, type VisualViewportName } from "./visual";

type FeatureFlagWindow = Window & {
  __CTX_FEATURE_FLAGS__?: Record<string, unknown>;
};

export async function openWorkbenchVisualPage(
  page: Page,
  workspaceId: string,
  opts: { theme: VisualTheme; viewport: VisualViewportName },
): Promise<void> {
  await prepareVisualPage(page, {
    theme: opts.theme,
    viewport: opts.viewport,
    route: `/workspaces/${workspaceId}`,
    ready: page.locator(".wb-main"),
  });
}

export function newTaskComposer(page: Page) {
  return page.locator("textarea.wb-composer-textarea").first();
}

export function activeSessionComposer(page: Page) {
  return page.locator(".wb-session-slot textarea.wb-active-textarea");
}

export async function openFirstTaskSession(page: Page): Promise<void> {
  const rows = page.locator(".wb-task-row");
  await expect(rows.first()).toBeVisible({ timeout: 20_000 });
  await rows.first().click();
  await expect(activeSessionComposer(page)).toBeVisible({ timeout: 20_000 });
}

export async function openHarnessMenu(page: Page) {
  const harnessButton = page
    .locator(".wb-new-composer-stack .wb-switcher-harness, .wb-new-composer-stack button[title='Harness'], button[title='Agents']")
    .first();
  await expect(harnessButton).toBeVisible({ timeout: 20_000 });
  await harnessButton.click();
  const menu = page.locator(".wb-harness-menu");
  await expect(menu).toBeVisible({ timeout: 10_000 });
  return menu;
}

export async function selectFakeHarness(page: Page): Promise<void> {
  await selectHarnessBySearch(page, "fake", /fake/i);
}

export async function enableQueuedMessages(page: Page): Promise<void> {
  await page.addInitScript(() => {
    const w = window as FeatureFlagWindow;
    w.__CTX_FEATURE_FLAGS__ = {
      ...(w.__CTX_FEATURE_FLAGS__ ?? {}),
      queued_messages_enabled: true,
    };
  });
}
