import { test, expect } from "./fixtures";
import fs from "fs";
import path from "path";
import pixelmatch from "pixelmatch";
import { PNG } from "pngjs";

const VISUAL_CSS = `
  :root {
    --accent: rgb(73 240 120);
    --bg: rgb(10 13 11);
    --fg: rgb(216 224 217);
  }

  body {
    margin: 0;
    padding: 24px;
    background: var(--bg);
    color: var(--fg);
    font: 24px/1.55 ui-monospace, "SF Mono", Menlo, Monaco, Consolas, monospace;
  }

  .wb-assistant-body a {
    color: inherit;
    text-decoration-color: currentColor;
  }

  .ctx-file-link {
    color: inherit;
    text-decoration: none;
  }

  .code-token-path,
  .code-token-url {
    text-decoration: none;
  }

  .ctx-markdown-link.ctx-modifier-hover,
  .ctx-file-link.ctx-modifier-hover,
  .code-token-path.ctx-modifier-hover {
    color: inherit;
    text-decoration: underline;
    text-decoration-thickness: 1px;
    text-underline-offset: 2px;
    cursor: pointer;
  }

  .code-token-url.ctx-modifier-hover {
    color: inherit;
    text-decoration: underline;
    text-decoration-thickness: 1px;
    text-underline-offset: 2px;
    cursor: pointer;
  }

  .surface {
    width: 560px;
    padding: 20px 24px;
    border: 1px solid rgba(73, 240, 120, 0.18);
    border-radius: 12px;
    background:
      linear-gradient(180deg, rgba(73, 240, 120, 0.06), rgba(73, 240, 120, 0.02)),
      rgba(8, 11, 9, 0.96);
    box-shadow: 0 18px 44px rgba(0, 0, 0, 0.35);
  }

  .label {
    margin-bottom: 12px;
    color: rgba(216, 224, 217, 0.72);
    font-size: 13px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }
`;

test("assistant markdown links visibly underline under modifier state", async ({ page }, testInfo) => {
  await page.setViewportSize({ width: 680, height: 260 });
  await page.setContent(`
    <style>${VISUAL_CSS}</style>
    <div class="surface">
      <div class="label">Modifier Active</div>
      <div class="wb-assistant-body">
        Open <a class="ctx-markdown-link ctx-modifier-hover" href="https://example.com/docs">https://example.com/docs</a>
      </div>
    </div>
  `);

  const link = page.getByRole("link", { name: "https://example.com/docs" });
  await expect(link).toBeVisible();

  const decoration = await link.evaluate((node) => {
    const style = window.getComputedStyle(node);
    return {
      color: style.color,
      line: style.textDecorationLine,
      thickness: style.textDecorationThickness,
      offset: style.textUnderlineOffset,
    };
  });

  expect(decoration.line).toContain("underline");
  expect(decoration.color).toBe("rgb(216, 224, 217)");
  expect(decoration.thickness).toBe("1px");
  expect(decoration.offset).toBe("2px");

  const screenshotPath = testInfo.outputPath("markdown-link-modifier-underline.png");
  await page.locator(".surface").screenshot({ path: screenshotPath });
  const actual = PNG.sync.read(fs.readFileSync(screenshotPath));
  const baselinePath = path.join(__dirname, "fixtures", "markdown-link-modifier-underline.baseline.png");
  const baseline = PNG.sync.read(fs.readFileSync(baselinePath));

  expect(actual.width).toBe(baseline.width);
  expect(actual.height).toBe(baseline.height);

  const diff = new PNG({ width: actual.width, height: actual.height });
  const mismatchPixels = pixelmatch(actual.data, baseline.data, diff.data, actual.width, actual.height, {
    threshold: 0.1,
  });
  expect(mismatchPixels).toBe(0);
  console.log(`markdown link underline screenshot: ${screenshotPath}`);
});
