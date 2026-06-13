import { createCtxPlaywrightConfig } from "./playwright.shared";

export default await createCtxPlaywrightConfig(
  "load",
  process.env.CTX_E2E_BASE_URL ? { serverMode: "external" } : {},
);
