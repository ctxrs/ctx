import { describe, expect, it } from "vitest";
import { generatePretextWrapRuleFuzzCorpus } from "./pretextWrapRuleFuzz";
import { PRETEXT_WRAP_RULE_CATALOG } from "./pretextWrapRuleCatalog";

describe("generatePretextWrapRuleFuzzCorpus", () => {
  it("can emit generated samples for every cataloged wrap-rule family", () => {
    const corpus = generatePretextWrapRuleFuzzCorpus({
      seed: 20260422,
      markdownCount: PRETEXT_WRAP_RULE_CATALOG.length,
      assistantCount: PRETEXT_WRAP_RULE_CATALOG.length,
    });

    const markdownFamilies = new Set(corpus.markdownSamples.map((sample) => sample.family));
    const assistantFamilies = new Set(corpus.assistantSamples.map((sample) => sample.family));

    for (const rule of PRETEXT_WRAP_RULE_CATALOG) {
      expect(markdownFamilies.has(rule.family), `${rule.family} missing markdown fuzz sample`).toBe(true);
      expect(assistantFamilies.has(rule.family), `${rule.family} missing assistant fuzz sample`).toBe(true);
    }
  });

  it("preserves each sampled rule's width candidates for threshold scans", () => {
    const corpus = generatePretextWrapRuleFuzzCorpus({
      seed: 20260422,
      markdownCount: PRETEXT_WRAP_RULE_CATALOG.length,
      assistantCount: PRETEXT_WRAP_RULE_CATALOG.length,
    });

    for (const sample of corpus.markdownSamples) {
      const rule = PRETEXT_WRAP_RULE_CATALOG.find((candidate) => candidate.id === sample.ruleId);
      expect(rule, `${sample.ruleId} should resolve to a catalog rule`).toBeDefined();
      expect(sample.markdownWidths).toEqual(rule?.markdownWidths);
      expect(sample.markdown.trim().length).toBeGreaterThan(0);
    }

    for (const sample of corpus.assistantSamples) {
      const rule = PRETEXT_WRAP_RULE_CATALOG.find((candidate) => candidate.id === sample.ruleId);
      expect(rule, `${sample.ruleId} should resolve to a catalog rule`).toBeDefined();
      expect(sample.assistantViewportWidths).toEqual(rule?.assistantViewportWidths);
      expect(sample.params.content.trim().length).toBeGreaterThan(0);
    }
  });
});
