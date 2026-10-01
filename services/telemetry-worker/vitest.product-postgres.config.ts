import { defineConfig } from "vitest/config";
export default defineConfig({test:{environment:"node",include:["test/product-telemetry-postgres.integration.mjs"],testTimeout:30_000}});
