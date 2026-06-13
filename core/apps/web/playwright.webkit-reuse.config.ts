import { defineConfig } from "playwright/test";
import { createCtxPlaywrightConfig } from "./playwright.shared";

const baseURL = process.env.CTX_E2E_BASE_URL ?? "https://127.0.0.1:5173";

process.env.CTX_E2E_BROWSER = "webkit";

const shared = await createCtxPlaywrightConfig("soak", {
  serverMode: "external",
  baseURL,
  ignoreHTTPSErrors: true,
});

export default defineConfig({
  ...shared,
  use: {
    ...shared.use,
    browserName: "webkit",
  },
  webServer: undefined,
});
