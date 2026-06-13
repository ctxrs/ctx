import { test } from "./fixtures";
import { promises as fs } from "node:fs";
import path from "node:path";
import { recordMeaningfulPaintShifts } from "./utils/recordMeaningfulPaintShifts";

const TARGET_URL = process.env.MEANINGFUL_REPAINT_URL;
const PRIMARY_TASK_TITLE =
  process.env.MEANINGFUL_REPAINT_PRIMARY_TITLE ?? "lets design our own task titling service. a task title shoul";
const SECONDARY_TASK_TITLE =
  process.env.MEANINGFUL_REPAINT_SECONDARY_TITLE ?? "please check out ~/code/ctx-family. the thing i would li";

test("records meaningful repaint shifts for the provided workspace view", async ({ page }, testInfo) => {
  test.skip(
    !TARGET_URL,
    "Set MEANINGFUL_REPAINT_URL to opt into this capture test (points to an already-running dev server).",
  );

  const captureDir = testInfo.outputPath("meaningful-repaints");
  const result = await recordMeaningfulPaintShifts(
    page,
    async () => {
      await page.goto(TARGET_URL!, { waitUntil: "domcontentloaded" });
      await clickTaskRow(page, PRIMARY_TASK_TITLE);
      await page.waitForTimeout(20_000);
      await clickTaskRow(page, SECONDARY_TASK_TITLE);
      await page.waitForTimeout(20_000);
    },
    {
      dir: captureDir,
    },
  );

  const metadataPath = path.join(captureDir, "meaningful-repaints.json");
  await fs.writeFile(metadataPath, JSON.stringify(result, null, 2));
  await testInfo.attach("meaningful-repaints.json", { path: metadataPath, contentType: "application/json" });
});

async function clickTaskRow(page: Parameters<typeof recordMeaningfulPaintShifts>[0], partialTitle: string) {
  const row = page.locator(".wb-task-row").filter({ hasText: partialTitle }).first();
  await row.waitFor({ state: "visible" });
  await row.click();
}
