import { test, expect } from "./fixtures";
import { createTempGitRepo } from "./utils/testRepo";
import { createWorkspaceAndOpenWorkbench } from "./utils/workbench";
import { selectHarnessBySearch } from "./utils/harnessEndpointAuth";

test("workbench assistant markdown link visibly underlines under modifier state", async ({ page }, testInfo) => {
  test.setTimeout(120_000);

  const repo = createTempGitRepo({
    prefix: "ctx-e2e-markdown-link-",
    files: [{ path: "README.md", content: "markdown link visual fixture\n" }],
  });

  await createWorkspaceAndOpenWorkbench({
    page,
    request: page.request,
    repo,
    workspaceName: `ws-markdown-link-${Date.now()}`,
  });

  await selectHarnessBySearch(page, "fake", /fake/i);

  const linkText = "https://example.com/docs";
  await page.locator("textarea.wb-composer-textarea").first().fill(`[docs](${linkText})`);
  await page.getByRole("button", { name: "Send" }).click();

  const assistantEntry = page
    .locator(".wb-session-slot .wb-assistant-entry")
    .first();
  await expect(assistantEntry).toBeVisible({ timeout: 20_000 });
  const renderedLink = assistantEntry.getByRole("link", { name: "docs" });
  await expect(renderedLink).toBeVisible({ timeout: 20_000 });

  const modifierKey = process.platform === "darwin" ? "Meta" : "Control";
  await page.keyboard.down(modifierKey);
  await page.waitForTimeout(120);

  const decoration = await renderedLink.evaluate((node) => {
    const style = window.getComputedStyle(node);
    return {
      line: style.textDecorationLine,
      thickness: style.textDecorationThickness,
      offset: style.textUnderlineOffset,
      color: style.color,
    };
  });

  expect(decoration.line).toContain("underline");
  expect(["1px", "auto"]).toContain(decoration.thickness);
  expect(["2px", "auto"]).toContain(decoration.offset);

  const screenshotPath = testInfo.outputPath("markdown-link-modifier-real-ui.png");
  await assistantEntry.screenshot({ path: screenshotPath });
  console.log(`real-ui markdown link underline screenshot: ${screenshotPath}`);

  await page.keyboard.up(modifierKey);
});
