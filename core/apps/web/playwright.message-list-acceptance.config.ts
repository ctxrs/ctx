import { defineConfig } from "playwright/test";

const baseURL = process.env.MESSAGE_LIST_ACCEPTANCE_BASE_URL ?? "https://127.0.0.1:5194";
const authToken =
  process.env.MESSAGE_LIST_WORKSPACE_TOKEN ??
  process.env.MESSAGE_LIST_AUTH_TOKEN ??
  process.env.CTX_E2E_AUTH_TOKEN ??
  "74978489-8632-45bb-b60f-aa01a288c84e";
const workers = Number(process.env.MESSAGE_LIST_ACCEPTANCE_WORKERS ?? "1");

export default defineConfig({
  testDir: "./e2e",
  timeout: 120_000,
  expect: {
    timeout: 20_000,
  },
  workers: Number.isFinite(workers) && workers > 0 ? workers : 1,
  outputDir: "./e2e/test-results/message-list-acceptance",
  reporter: [[process.env.CTX_E2E_REPORTER ?? "dot"]],
  use: {
    baseURL,
    headless: true,
    ignoreHTTPSErrors: true,
    extraHTTPHeaders: {
      authorization: `Bearer ${authToken}`,
    },
    screenshot: "only-on-failure",
    trace: "retain-on-failure",
    video: "off",
  },
});
