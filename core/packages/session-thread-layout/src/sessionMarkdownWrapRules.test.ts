import { describe, expect, it } from "vitest";
import {
  PRETEXT_WRAP_RULE_CATALOG,
  getPretextWrapRuleById,
} from "./testdata/pretextWrapRuleCatalog";
import { measureSessionMarkdownDocument } from "./sessionMarkdownMeasurement";
import { BODY_LINE_HEIGHT_PX } from "./sessionMarkdownMeasurementCore";

describe("sessionMarkdownWrapRules", () => {
  it("collapses ordinary spaces in body prose like the browser's normal white-space mode", () => {
    const rule = getPretextWrapRuleById("ws-collapse-normal");

    const collapsed = measureSessionMarkdownDocument("alpha beta", 220);
    const repeated = measureSessionMarkdownDocument(rule.markdown, 220);

    expect(repeated).toBe(collapsed);
  });

  it("treats markdown hard breaks as forced line breaks in inline content", () => {
    const rule = getPretextWrapRuleById("hard-break-forced-line");

    const height = measureSessionMarkdownDocument(rule.markdown, 420);

    expect(height).toBe(BODY_LINE_HEIGHT_PX * 2);
  });

  it("keeps every cataloged rule tied to executable coverage", () => {
    const ids = new Set<string>();

    for (const rule of PRETEXT_WRAP_RULE_CATALOG) {
      expect(ids.has(rule.id), `duplicate wrap rule id ${rule.id}`).toBe(false);
      ids.add(rule.id);
      expect(rule.markdown.trim().length, `${rule.id} needs a markdown sample`).toBeGreaterThan(0);
      expect(rule.markdownWidths.length, `${rule.id} needs markdown widths`).toBeGreaterThan(0);
      expect(rule.browsers.length, `${rule.id} needs browser coverage`).toBeGreaterThan(0);
      expect(rule.planners.length, `${rule.id} needs planner ownership`).toBeGreaterThan(0);
      expect(rule.unitCoverage.length, `${rule.id} needs unit coverage`).toBeGreaterThan(0);
      expect(rule.e2eCoverage.length, `${rule.id} needs e2e coverage`).toBeGreaterThan(0);
      expect(rule.fuzzCoverage.length, `${rule.id} needs fuzz coverage`).toBeGreaterThan(0);
      expect(rule.sources.length, `${rule.id} needs source references`).toBeGreaterThan(0);
    }
  });

  it("keeps threshold incidents represented by width sweeps instead of one-off widths", () => {
    const rule = getPretextWrapRuleById("ordered-list-inline-code-chip-threshold");

    expect(rule.markdownWidths.length).toBeGreaterThanOrEqual(5);
    expect(rule.fuzzCoverage).toContain("pretextWrapRuleFuzz.ts");
  });
});
