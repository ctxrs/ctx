import { expect, test } from "./fixtures";

test.use({ browserName: "chromium" });

test("diff editor expands to full height (no inner vertical scroll)", async ({ page }) => {
  await page.goto("/__cursor_diff_demo?state=big&lines=220");

  const toggle = page.getByRole("button", { name: /file diff/i }).first();
  await toggle.click();
  await expect(toggle).toHaveAttribute("aria-expanded", "true");

  const editorShell = page.locator(".cursor-diff-editor-shell").first();
  await expect(editorShell).toBeVisible();

  const scrollable = editorShell.locator(".monaco-scrollable-element").first();
  await expect(scrollable).toBeVisible();

  const initial = await scrollable.evaluate((el) => ({
    clientHeight: el.clientHeight,
    scrollTop: el.scrollTop,
  }));

  expect(initial.clientHeight).toBeGreaterThan(1000);
  const editorBox = await page.locator(".cursor-diff-editor-shell .monaco-editor").first().boundingBox();
  expect(editorBox?.height ?? 0).toBeGreaterThan(1000);

  const docHeight = await page.evaluate(() => document.documentElement.scrollHeight);
  const viewportHeight = await page.evaluate(() => window.innerHeight);
  expect(docHeight).toBeGreaterThan(viewportHeight + 500);

  const beforePageY = await page.evaluate(() => window.scrollY);
  const box = await scrollable.boundingBox();
  if (box) await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.wheel(0, 900);

  const after = await scrollable.evaluate((el) => el.scrollTop);
  const afterPageY = await page.evaluate(() => window.scrollY);

  expect(after).toBe(initial.scrollTop);
  expect(afterPageY).toBeGreaterThan(beforePageY);

  await page.screenshot({ path: "test-results/tmp-diff-editor-big.png", fullPage: true });
});
