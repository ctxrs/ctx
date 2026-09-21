import { describe, expect, test } from "vitest";

import wrangler from "../wrangler.toml?raw";

const PRODUCTION_REDIRECT = "https://api.ctx.rs/storage/v1/object/public/releases";

describe("release API deployment configuration", () => {
  test("deploys the additive v2 route on ctx hosts without migrating ADE routes", () => {
    for (const host of ["cli.ctx.rs", "api.ctx.rs"]) {
      expect(wrangler).toContain(`pattern = "${host}/functions/v2/releases/*"`);
      expect(wrangler).toContain(`pattern = "${host}/functions/v1/releases/*"`);
    }
    expect(wrangler).not.toContain("api.ade.ctx.rs/functions/v2/");
  });

  test("pins the live artifact redirect in the named production environment", () => {
    const production = wrangler.match(/^\[env\.prod\][\s\S]*$/mu)?.[0];

    expect(production).toBeDefined();
    expect(production).toMatch(/^\[env\.prod\.vars\]$/mu);
    expect(production).toContain(
      `RELEASE_ARTIFACT_REDIRECT_BASE_URL = "${PRODUCTION_REDIRECT}"`,
    );
    expect(wrangler.split("\n").filter(
      (line) => line === `RELEASE_ARTIFACT_REDIRECT_BASE_URL = "${PRODUCTION_REDIRECT}"`,
    )).toHaveLength(2);
  });
});
