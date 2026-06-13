import path from "path";
import { defineConfig } from "playwright/test";
import { createCtxPlaywrightConfig } from "./playwright.shared";

const base = await createCtxPlaywrightConfig("premerge_required");

export default defineConfig({
  ...base,
  testDir: "./e2e",
  testMatch: /tmp-inline-code-inspect\.spec\.ts/,
  timeout: 180_000,
  workers: 1,
  outputDir: path.resolve("e2e/test-results/pretext-inline-debug"),
  reporter: [["dot"]],
  use: {
    ...base.use,
    headless: true,
    screenshot: "only-on-failure",
    trace: "retain-on-failure",
    video: "retain-on-failure",
  },
});
