import path from "path";
import { defineConfig } from "playwright/test";
import { createCtxPlaywrightConfig } from "./playwright.shared";

const base = await createCtxPlaywrightConfig("premerge_required");

export default defineConfig({
  ...base,
  testDir: "./e2e",
  testMatch:
    /workbench-pretext-virtualizer-(acceptance|switch-collapse|bottom-rehit|short-thread)\.spec\.ts/,
  timeout: 180_000,
  workers: 1,
  outputDir: path.resolve("e2e/test-results/pretext-virtualizer-acceptance"),
  reporter: [
    ["dot"],
    ["html", { outputFolder: path.resolve("e2e/playwright-report/pretext-virtualizer-acceptance"), open: "never" }],
  ],
  use: {
    ...base.use,
    headless: true,
    screenshot: "only-on-failure",
    trace: "retain-on-failure",
    video: "retain-on-failure",
  },
});
