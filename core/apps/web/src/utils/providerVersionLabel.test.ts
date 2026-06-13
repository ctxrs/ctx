import { describe, expect, it } from "vitest";
import {
  formatProviderVersionDisplay,
  getMatrixVersionDisplay,
  getProviderVersionDisplay,
} from "./providerVersionLabel";

describe("providerVersionLabel", () => {
  it("prefers upstream detected version and keeps ctx build as secondary", () => {
    const display = getProviderVersionDisplay({
      version: "0.0.0-ctx.3",
      details: {
        matrix_detected_upstream_version: "0.98.0",
      },
    });
    expect(display).toEqual({
      primary: "0.98.0",
      secondary: "ctx build 0.0.0-ctx.3",
    });
    expect(
      formatProviderVersionDisplay({
        version: "0.0.0-ctx.3",
        details: {
          matrix_detected_upstream_version: "0.98.0",
        },
      }),
    ).toBe("0.98.0 (ctx build 0.0.0-ctx.3)");
  });

  it("falls back to detected version when upstream metadata is absent", () => {
    expect(
      formatProviderVersionDisplay({
        version: "1.2.3",
        details: {},
      }),
    ).toBe("1.2.3");
  });

  it("prefers upstream matrix versions when present", () => {
    expect(
      getMatrixVersionDisplay(
        {
          matrix_recommended_version: "0.0.0-ctx.3",
          matrix_recommended_upstream_version: "0.98.0",
          matrix_latest_version: "0.0.0-ctx.4",
          matrix_latest_upstream_version: "0.99.0",
        },
        "recommended",
      ),
    ).toBe("0.98.0");
    expect(
      getMatrixVersionDisplay(
        {
          matrix_latest_version: "0.0.0-ctx.4",
        },
        "latest",
      ),
    ).toBe("0.0.0-ctx.4");
  });
});
