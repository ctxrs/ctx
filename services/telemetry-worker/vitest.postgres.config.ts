import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    environment: "node",
    include: ["test/ordinary-blame-postgres.integration.mjs"],
    testTimeout: 30_000,
  },
});
