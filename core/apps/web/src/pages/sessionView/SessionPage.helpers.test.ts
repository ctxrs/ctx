import { describe, expect, it } from "vitest";
import { formatElapsedMs } from "./SessionPage.helpers";

describe("formatElapsedMs", () => {
  it("includes seconds for hour-scale durations", () => {
    expect(formatElapsedMs((1 * 3600 + 24 * 60 + 2) * 1000)).toBe("1h 24m 2s");
  });

  it("includes seconds for minute-scale durations", () => {
    expect(formatElapsedMs((24 * 60 + 2) * 1000)).toBe("24m 2s");
  });

  it("shows whole seconds below a minute", () => {
    expect(formatElapsedMs(2_900)).toBe("2s");
  });
});
