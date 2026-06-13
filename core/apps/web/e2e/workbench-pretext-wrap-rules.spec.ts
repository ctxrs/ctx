import { promises as fs } from "fs";
import { expect, test } from "./fixtures";
import {
  measureAssistantParity,
  measureMarkdownParity,
  openWorkbenchShell,
} from "./utils/pretextParity";
import { PRETEXT_WRAP_RULE_CATALOG } from "../src/testdata/pretextWrapRuleCatalog";

const ENFORCE = process.env.CTX_PRETEXT_PARITY_ENFORCE === "1";
const MARKDOWN_THRESHOLD_PX = 1;
const ROW_THRESHOLD_PX = 1;

type MarkdownWrapRuleSummary = {
  ruleId: string;
  title: string;
  family: string;
  evidence: string;
  width: number;
  planned: number;
  actual: number;
  delta: number;
};

type AssistantWrapRuleSummary = {
  ruleId: string;
  title: string;
  family: string;
  evidence: string;
  viewportWidth: number;
  measuredViewportWidth?: number;
  rowWidth?: number;
  planned: number;
  actual: number;
  delta: number;
  debugDomMeasured?: number;
  debugDomDelta?: number;
};

test("workbench: pretext wrap rule markdown parity", async ({ page, browserName }, testInfo) => {
  test.setTimeout(240000);
  test.slow();
  await openWorkbenchShell(page);

  const activeRules = PRETEXT_WRAP_RULE_CATALOG.filter((rule) => rule.browsers.includes(browserName));
  const summary: MarkdownWrapRuleSummary[] = [];

  for (const rule of activeRules) {
    for (const width of rule.markdownWidths) {
      const [measurement] = await measureMarkdownParity(
        page,
        [{ name: rule.id, markdown: rule.markdown }],
        width,
      );
      summary.push({
        ruleId: rule.id,
        title: rule.title,
        family: rule.family,
        evidence: rule.evidence,
        width,
        planned: measurement?.planned ?? 0,
        actual: measurement?.actual ?? 0,
        delta: measurement?.delta ?? 0,
      });
    }
  }

  const reportPath = testInfo.outputPath(`pretext-wrap-rule-markdown-parity-${browserName}.json`);
  await fs.writeFile(
    reportPath,
    JSON.stringify(
      {
        enforce: ENFORCE,
        browserName,
        thresholdPx: MARKDOWN_THRESHOLD_PX,
        ruleCount: activeRules.length,
        summary,
      },
      null,
      2,
    ),
    "utf8",
  );
  await testInfo.attach(`pretext-wrap-rule-markdown-parity-${browserName}.json`, {
    path: reportPath,
    contentType: "application/json",
  });

  if (!ENFORCE) return;

  const failures = summary.filter((entry) => Math.abs(entry.delta) > MARKDOWN_THRESHOLD_PX);
  expect(
    failures,
    `markdown wrap-rule parity failures:\n${failures
      .map(
        (failure) =>
          `- ${failure.ruleId} width ${failure.width}: delta=${failure.delta}px planned=${failure.planned} actual=${failure.actual}`,
      )
      .join("\n")}`,
  ).toEqual([]);
});

test("workbench: pretext wrap rule assistant-row parity", async ({ page, browserName }, testInfo) => {
  test.setTimeout(240000);
  test.slow();
  await openWorkbenchShell(page);

  const activeRules = PRETEXT_WRAP_RULE_CATALOG.filter(
    (rule) => rule.browsers.includes(browserName) && rule.assistantViewportWidths.length > 0,
  );
  const summary: AssistantWrapRuleSummary[] = [];

  for (const rule of activeRules) {
    for (const viewportWidth of rule.assistantViewportWidths) {
      const measurement = await measureAssistantParity(page, {
        content: rule.markdown,
        viewportWidth,
      });
      summary.push({
        ruleId: rule.id,
        title: rule.title,
        family: rule.family,
        evidence: rule.evidence,
        viewportWidth,
        measuredViewportWidth: measurement.viewportWidth,
        rowWidth: measurement.rowWidth,
        planned: measurement.planned,
        actual: measurement.actual,
        delta: measurement.delta,
        debugDomMeasured: measurement.debugDomMeasured,
        debugDomDelta: measurement.debugDomDelta,
      });
    }
  }

  const reportPath = testInfo.outputPath(`pretext-wrap-rule-assistant-parity-${browserName}.json`);
  await fs.writeFile(
    reportPath,
    JSON.stringify(
      {
        enforce: ENFORCE,
        browserName,
        thresholdPx: ROW_THRESHOLD_PX,
        ruleCount: activeRules.length,
        summary,
      },
      null,
      2,
    ),
    "utf8",
  );
  await testInfo.attach(`pretext-wrap-rule-assistant-parity-${browserName}.json`, {
    path: reportPath,
    contentType: "application/json",
  });

  if (!ENFORCE) return;

  const failures = summary.filter((entry) => Math.abs(entry.delta) > ROW_THRESHOLD_PX);
  expect(
    failures,
    `assistant wrap-rule parity failures:\n${failures
      .map(
        (failure) =>
          `- ${failure.ruleId} viewport ${failure.viewportWidth}: delta=${failure.delta}px planned=${failure.planned} actual=${failure.actual}`,
      )
      .join("\n")}`,
  ).toEqual([]);
});
