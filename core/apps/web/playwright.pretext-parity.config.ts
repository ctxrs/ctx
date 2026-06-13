import path from "path";
import { defineConfig } from "playwright/test";
import { createCtxPlaywrightConfig } from "./playwright.shared";

const base = await createCtxPlaywrightConfig("premerge_required");

export default defineConfig({
  ...base,
  testDir: "./e2e",
  testMatch:
    /workbench-(pretext-(parity-(corpus|fuzz)|wrap-rules|wrap-rule-fuzz)|markdown-parity|message-row-parity|turn-header-parity)\.spec\.ts/,
  timeout: 180_000,
  workers: 1,
  outputDir: path.resolve("e2e/test-results/pretext-parity"),
  reporter: [
    ["dot"],
    ["html", { outputFolder: path.resolve("e2e/playwright-report/pretext-parity"), open: "never" }],
  ],
  use: {
    ...base.use,
    headless: true,
    screenshot: "only-on-failure",
    trace: "retain-on-failure",
    video: "retain-on-failure",
  },
});
