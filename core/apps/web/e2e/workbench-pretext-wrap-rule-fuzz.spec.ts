import { promises as fs } from "fs";
import { expect, test } from "./fixtures";
import {
  measureAssistantParity,
  measureMarkdownParity,
  openWorkbenchShell,
} from "./utils/pretextParity";
import { generatePretextWrapRuleFuzzCorpus } from "./utils/pretextWrapRuleFuzz";

const ENFORCE = process.env.CTX_PRETEXT_PARITY_ENFORCE === "1";
const THRESHOLD_PX = 1;
const DEFAULT_SEED = 20260421;
const DEFAULT_MARKDOWN_CASES = 8;
const DEFAULT_ASSISTANT_CASES = 6;
const DEFAULT_WIDTHS_PER_SAMPLE = 2;

test.use({
  screenshot: "off",
  trace: "off",
  video: "off",
});

type MarkdownSummaryEntry = {
  width: number;
  name: string;
  ruleId: string;
  family: string;
  markdown: string;
  planned: number;
  actual: number;
  delta: number;
};

type AssistantSummaryEntry = {
  width: number;
  name: string;
  ruleId: string;
  family: string;
  content: string;
  planned: number;
  actual: number;
  delta: number;
};

function readEnvInt(name: string, fallback: number): number {
  const raw = process.env[name];
  if (!raw) return fallback;
  const parsed = Number.parseInt(raw, 10);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : fallback;
}

function selectDeterministicWidth(widths: readonly number[], seed: number, offset: number): number {
  if (widths.length === 0) {
    throw new Error("wrap-rule fuzz sample is missing width candidates");
  }
  const index = Math.abs((seed + offset) % widths.length);
  return widths[index]!;
}

function selectDeterministicWidths(
  widths: readonly number[],
  seed: number,
  offset: number,
  count: number,
): readonly number[] {
  if (widths.length === 0) {
    throw new Error("wrap-rule fuzz sample is missing width candidates");
  }
  const selected = new Set<number>();
  for (let index = 0; index < Math.max(1, count); index += 1) {
    selected.add(selectDeterministicWidth(widths, seed, offset + index));
  }
  return [...selected];
}

function formatFailures(
  kind: string,
  failures: Array<{ name: string; width: number; delta: number; planned: number; actual: number }>,
): string {
  return `${kind} wrap-rule fuzz parity failures:\n${failures
    .map(
      (failure) =>
        `- width ${failure.width} ${failure.name}: delta=${failure.delta}px planned=${failure.planned} actual=${failure.actual}`,
    )
    .join("\n")}`;
}

test("workbench: pretext wrap-rule markdown fuzz parity", async ({ page }, testInfo) => {
  test.setTimeout(240000);
  test.slow();
  await openWorkbenchShell(page);

  const corpus = generatePretextWrapRuleFuzzCorpus({
    seed: readEnvInt("CTX_PRETEXT_WRAP_RULE_FUZZ_SEED", DEFAULT_SEED),
    markdownCount: readEnvInt("CTX_PRETEXT_WRAP_RULE_FUZZ_MARKDOWN_CASES", DEFAULT_MARKDOWN_CASES),
    assistantCount: readEnvInt("CTX_PRETEXT_WRAP_RULE_FUZZ_ASSISTANT_CASES", DEFAULT_ASSISTANT_CASES),
  });
  const widthsPerSample = readEnvInt("CTX_PRETEXT_WRAP_RULE_FUZZ_WIDTHS_PER_SAMPLE", DEFAULT_WIDTHS_PER_SAMPLE);

  const summary: MarkdownSummaryEntry[] = [];
  for (const [index, sample] of corpus.markdownSamples.entries()) {
    for (const width of selectDeterministicWidths(sample.markdownWidths, corpus.seed, index, widthsPerSample)) {
      const [measurement] = await measureMarkdownParity(page, [sample], width);
      summary.push({
        width,
        name: sample.name,
        ruleId: sample.ruleId,
        family: sample.family,
        markdown: sample.markdown,
        planned: measurement!.planned,
        actual: measurement!.actual,
        delta: measurement!.delta,
      });
    }
  }

  const reportPath = testInfo.outputPath("pretext-wrap-rule-markdown-fuzz-parity.json");
  await fs.writeFile(
    reportPath,
    JSON.stringify(
      {
        enforce: ENFORCE,
        seed: corpus.seed,
        thresholdPx: THRESHOLD_PX,
        counts: {
          markdown: corpus.markdownSamples.length,
        },
        widthsPerSample,
        samples: summary,
      },
      null,
      2,
    ),
    "utf8",
  );
  await testInfo.attach("pretext-wrap-rule-markdown-fuzz-parity.json", {
    path: reportPath,
    contentType: "application/json",
  });

  if (!ENFORCE) return;

  const failures = summary.filter((entry) => Math.abs(entry.delta) > THRESHOLD_PX);
  expect(
    failures,
    formatFailures(
      "markdown",
      failures.map((failure) => ({
        name: failure.name,
        width: failure.width,
        delta: failure.delta,
        planned: failure.planned,
        actual: failure.actual,
      })),
    ),
  ).toEqual([]);
});

test("workbench: pretext wrap-rule assistant fuzz parity", async ({ page }, testInfo) => {
  test.setTimeout(240000);
  test.slow();
  await openWorkbenchShell(page);

  const corpus = generatePretextWrapRuleFuzzCorpus({
    seed: readEnvInt("CTX_PRETEXT_WRAP_RULE_FUZZ_SEED", DEFAULT_SEED),
    markdownCount: readEnvInt("CTX_PRETEXT_WRAP_RULE_FUZZ_MARKDOWN_CASES", DEFAULT_MARKDOWN_CASES),
    assistantCount: readEnvInt("CTX_PRETEXT_WRAP_RULE_FUZZ_ASSISTANT_CASES", DEFAULT_ASSISTANT_CASES),
  });
  const widthsPerSample = readEnvInt("CTX_PRETEXT_WRAP_RULE_FUZZ_WIDTHS_PER_SAMPLE", DEFAULT_WIDTHS_PER_SAMPLE);

  const summary: AssistantSummaryEntry[] = [];
  for (const [index, sample] of corpus.assistantSamples.entries()) {
    for (const width of selectDeterministicWidths(sample.assistantViewportWidths, corpus.seed, index, widthsPerSample)) {
      const measurement = await measureAssistantParity(page, {
        ...sample.params,
        viewportWidth: width,
      });
      summary.push({
        width,
        name: sample.name,
        ruleId: sample.ruleId,
        family: sample.family,
        content: sample.params.content,
        planned: measurement.planned,
        actual: measurement.actual,
        delta: measurement.delta,
      });
    }
  }

  const reportPath = testInfo.outputPath("pretext-wrap-rule-assistant-fuzz-parity.json");
  await fs.writeFile(
    reportPath,
    JSON.stringify(
      {
        enforce: ENFORCE,
        seed: corpus.seed,
        thresholdPx: THRESHOLD_PX,
        counts: {
          assistant: corpus.assistantSamples.length,
        },
        widthsPerSample,
        samples: summary,
      },
      null,
      2,
    ),
    "utf8",
  );
  await testInfo.attach("pretext-wrap-rule-assistant-fuzz-parity.json", {
    path: reportPath,
    contentType: "application/json",
  });

  if (!ENFORCE) return;

  const failures = summary.filter((entry) => Math.abs(entry.delta) > THRESHOLD_PX);
  expect(
    failures,
    formatFailures(
      "assistant",
      failures.map((failure) => ({
        name: failure.name,
        width: failure.width,
        delta: failure.delta,
        planned: failure.planned,
        actual: failure.actual,
      })),
    ),
  ).toEqual([]);
});
