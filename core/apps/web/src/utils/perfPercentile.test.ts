import { describe, expect, it } from "vitest";
import {
  minSamplesForDistinctPercentile,
  percentile,
  percentileSelectsMaximum,
} from "./perfPercentile";

describe("perfPercentile", () => {
  it("uses the existing nearest-rank percentile behavior", () => {
    expect(percentile([10, 16, 12, 5059], 0.5)).toBe(12);
    expect(percentile([10, 16, 12, 5059], 0.95)).toBe(5059);
    expect(percentile([], 0.95)).toBeNull();
  });

  it("detects when a percentile assertion collapses to a max assertion", () => {
    expect(percentileSelectsMaximum(4, 0.95)).toBe(true);
    expect(percentileSelectsMaximum(19, 0.95)).toBe(true);
    expect(percentileSelectsMaximum(20, 0.95)).toBe(false);
    expect(minSamplesForDistinctPercentile(0.95)).toBe(20);
  });
});
