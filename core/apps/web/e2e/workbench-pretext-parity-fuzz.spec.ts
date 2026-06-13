import { promises as fs } from "fs";
import { expect, test } from "./fixtures";
import {
  measureAssistantParity,
  measureAssistantStreamingParity,
  measureMarkdownParity,
  measureMessageParity,
  measureTurnHeaderParity,
  openWorkbenchShell,
} from "./utils/pretextParity";
import { generatePretextParityFuzzCorpus } from "./utils/pretextParityFuzz";

const ENFORCE = process.env.CTX_PRETEXT_PARITY_ENFORCE === "1";
const MARKDOWN_THRESHOLD_PX = 1;
const ROW_THRESHOLD_PX = 1;

const DEFAULT_SEED = 20260412;
const DEFAULT_MARKDOWN_CASES = 18;
const DEFAULT_MESSAGE_CASES = 12;
const DEFAULT_ASSISTANT_CASES = 12;
const DEFAULT_TURN_HEADER_CASES = 8;

type MarkdownSummaryEntry = {
  width: number;
  name: string;
  markdown: string;
  planned: number;
  actual: number;
  delta: number;
};

type RowSummaryEntry = {
  kind: "message" | "assistant" | "turn_header";
  width: number;
  name: string;
  content: string;
  planned: number;
  actual: number;
  delta: number;
  expanded?: boolean;
  isComplete?: boolean;
  attachmentCount?: number;
};

type StreamingSummaryEntry = {
  width: number;
  name: string;
  stepIndex: number;
  content: string;
  partialPlanned: number;
  partialActual: number;
  partialDelta: number;
  completePlanned: number;
  completeActual: number;
  completeDelta: number;
  actualDelta: number;
  plannedDelta: number;
  structureEquivalent: boolean;
};

function readEnvInt(name: string, fallback: number): number {
  const raw = process.env[name];
  if (!raw) return fallback;
  const parsed = Number.parseInt(raw, 10);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : fallback;
}

function formatFailures(
  kind: string,
  failures: Array<{ name: string; width: number; delta: number; planned: number; actual: number }>,
): string {
  return `${kind} fuzz parity failures:\n${failures
    .map(
      (failure) =>
        `- width ${failure.width} ${failure.name}: delta=${failure.delta}px planned=${failure.planned} actual=${failure.actual}`,
    )
    .join("\n")}`;
}

function formatStreamingFailures(failures: StreamingSummaryEntry[]): string {
  return `assistant streaming parity failures:\n${failures
    .map(
      (failure) =>
        `- width ${failure.width} ${failure.name} step ${failure.stepIndex}: partialDelta=${failure.partialDelta}px completeDelta=${failure.completeDelta}px actualDelta=${failure.actualDelta}px plannedDelta=${failure.plannedDelta}px structureEquivalent=${failure.structureEquivalent}`,
    )
    .join("\n")}`;
}

test("workbench: pretext generated markdown fuzz parity", async ({ page }, testInfo) => {
  test.setTimeout(240000);
  test.slow();
  await openWorkbenchShell(page);

  const corpus = generatePretextParityFuzzCorpus({
    seed: readEnvInt("CTX_PRETEXT_FUZZ_SEED", DEFAULT_SEED),
    markdownCount: readEnvInt("CTX_PRETEXT_FUZZ_MARKDOWN_CASES", DEFAULT_MARKDOWN_CASES),
    messageCount: readEnvInt("CTX_PRETEXT_FUZZ_MESSAGE_CASES", DEFAULT_MESSAGE_CASES),
    assistantCount: readEnvInt("CTX_PRETEXT_FUZZ_ASSISTANT_CASES", DEFAULT_ASSISTANT_CASES),
    turnHeaderCount: readEnvInt("CTX_PRETEXT_FUZZ_TURN_HEADER_CASES", DEFAULT_TURN_HEADER_CASES),
  });

  const summary: MarkdownSummaryEntry[] = [];
  for (const width of corpus.widths) {
    const measurements = await measureMarkdownParity(page, corpus.markdownSamples, width);
    summary.push(
      ...measurements.map((measurement, index) => ({
        width,
        markdown: corpus.markdownSamples[index]!.markdown,
        ...measurement,
      })),
    );
  }

  const report = {
    enforce: ENFORCE,
    seed: corpus.seed,
    thresholdPx: MARKDOWN_THRESHOLD_PX,
    widths: corpus.widths,
    counts: {
      markdown: corpus.markdownSamples.length,
    },
    samples: summary,
  };
  const reportPath = testInfo.outputPath("pretext-markdown-fuzz-parity.json");
  await fs.writeFile(reportPath, JSON.stringify(report, null, 2), "utf8");
  await testInfo.attach("pretext-markdown-fuzz-parity.json", {
    path: reportPath,
    contentType: "application/json",
  });

  if (!ENFORCE) return;

  const failures = summary.filter((entry) => Math.abs(entry.delta) > MARKDOWN_THRESHOLD_PX);
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

test("workbench: pretext generated threshold seam fuzz parity", async ({ page }, testInfo) => {
  test.setTimeout(240000);
  test.slow();
  await openWorkbenchShell(page);

  const corpus = generatePretextParityFuzzCorpus({
    seed: readEnvInt("CTX_PRETEXT_FUZZ_SEED", DEFAULT_SEED),
    markdownCount: readEnvInt("CTX_PRETEXT_FUZZ_MARKDOWN_CASES", DEFAULT_MARKDOWN_CASES),
    messageCount: readEnvInt("CTX_PRETEXT_FUZZ_MESSAGE_CASES", DEFAULT_MESSAGE_CASES),
    assistantCount: readEnvInt("CTX_PRETEXT_FUZZ_ASSISTANT_CASES", DEFAULT_ASSISTANT_CASES),
    turnHeaderCount: readEnvInt("CTX_PRETEXT_FUZZ_TURN_HEADER_CASES", DEFAULT_TURN_HEADER_CASES),
  });

  const markdownSummary: MarkdownSummaryEntry[] = [];
  for (const width of corpus.threshold.widths) {
    const measurements = await measureMarkdownParity(page, corpus.threshold.markdownSamples, width);
    markdownSummary.push(
      ...measurements.map((measurement, index) => ({
        width,
        markdown: corpus.threshold.markdownSamples[index]!.markdown,
        ...measurement,
      })),
    );
  }

  const rowSummary: RowSummaryEntry[] = [];
  for (const width of corpus.threshold.widths) {
    for (const sample of corpus.threshold.messageSamples) {
      const measurement = await measureMessageParity(page, {
        ...sample.params,
        viewportWidth: width,
      });
      rowSummary.push({
        kind: "message",
        width,
        name: sample.name,
        content: sample.params.content,
        expanded: sample.params.expanded,
        attachmentCount: sample.params.attachments?.length ?? 0,
        ...measurement,
      });
    }

    for (const sample of corpus.threshold.assistantSamples) {
      const measurement = await measureAssistantParity(page, {
        ...sample.params,
        viewportWidth: width,
      });
      rowSummary.push({
        kind: "assistant",
        width,
        name: sample.name,
        content: sample.params.content,
        isComplete: sample.params.isComplete ?? true,
        ...measurement,
      });
    }
  }

  const report = {
    enforce: ENFORCE,
    seed: corpus.seed,
    thresholdPx: ROW_THRESHOLD_PX,
    widths: corpus.threshold.widths,
    counts: {
      markdown: corpus.threshold.markdownSamples.length,
      message: corpus.threshold.messageSamples.length,
      assistant: corpus.threshold.assistantSamples.length,
    },
    markdownSamples: markdownSummary,
    rowSamples: rowSummary,
  };
  const reportPath = testInfo.outputPath("pretext-threshold-seam-fuzz-parity.json");
  await fs.writeFile(reportPath, JSON.stringify(report, null, 2), "utf8");
  await testInfo.attach("pretext-threshold-seam-fuzz-parity.json", {
    path: reportPath,
    contentType: "application/json",
  });

  if (!ENFORCE) return;

  const failures = [
    ...markdownSummary
      .filter((entry) => Math.abs(entry.delta) > MARKDOWN_THRESHOLD_PX)
      .map((entry) => ({
        name: `markdown:${entry.name}`,
        width: entry.width,
        delta: entry.delta,
        planned: entry.planned,
        actual: entry.actual,
      })),
    ...rowSummary
      .filter((entry) => Math.abs(entry.delta) > ROW_THRESHOLD_PX)
      .map((entry) => ({
        name: `${entry.kind}:${entry.name}`,
        width: entry.width,
        delta: entry.delta,
        planned: entry.planned,
        actual: entry.actual,
      })),
  ];
  expect(failures, formatFailures("threshold seam", failures)).toEqual([]);
});

test("workbench: pretext generated transcript row fuzz parity", async ({ page }, testInfo) => {
  test.setTimeout(240000);
  test.slow();
  await openWorkbenchShell(page);

  const corpus = generatePretextParityFuzzCorpus({
    seed: readEnvInt("CTX_PRETEXT_FUZZ_SEED", DEFAULT_SEED),
    markdownCount: readEnvInt("CTX_PRETEXT_FUZZ_MARKDOWN_CASES", DEFAULT_MARKDOWN_CASES),
    messageCount: readEnvInt("CTX_PRETEXT_FUZZ_MESSAGE_CASES", DEFAULT_MESSAGE_CASES),
    assistantCount: readEnvInt("CTX_PRETEXT_FUZZ_ASSISTANT_CASES", DEFAULT_ASSISTANT_CASES),
    turnHeaderCount: readEnvInt("CTX_PRETEXT_FUZZ_TURN_HEADER_CASES", DEFAULT_TURN_HEADER_CASES),
  });

  const summary: RowSummaryEntry[] = [];

  for (const width of corpus.widths) {
    for (const sample of corpus.messageSamples) {
      const measurement = await measureMessageParity(page, {
        ...sample.params,
        viewportWidth: width,
      });
      summary.push({
        kind: "message",
        width,
        name: sample.name,
        content: sample.params.content,
        expanded: sample.params.expanded,
        attachmentCount: sample.params.attachments?.length ?? 0,
        ...measurement,
      });
    }

    for (const sample of corpus.assistantSamples) {
      const measurement = await measureAssistantParity(page, {
        ...sample.params,
        viewportWidth: width,
      });
      summary.push({
        kind: "assistant",
        width,
        name: sample.name,
        content: sample.params.content,
        isComplete: sample.params.isComplete ?? true,
        ...measurement,
      });
    }

    for (const sample of corpus.turnHeaderSamples) {
      const measurement = await measureTurnHeaderParity(page, {
        ...sample.params,
        viewportWidth: width,
      });
      summary.push({
        kind: "turn_header",
        width,
        name: sample.name,
        content: sample.params.content,
        ...measurement,
      });
    }
  }

  const report = {
    enforce: ENFORCE,
    seed: corpus.seed,
    thresholdPx: ROW_THRESHOLD_PX,
    widths: corpus.widths,
    counts: {
      message: corpus.messageSamples.length,
      assistant: corpus.assistantSamples.length,
      turnHeader: corpus.turnHeaderSamples.length,
    },
    samples: summary,
  };
  const reportPath = testInfo.outputPath("pretext-row-fuzz-parity.json");
  await fs.writeFile(reportPath, JSON.stringify(report, null, 2), "utf8");
  await testInfo.attach("pretext-row-fuzz-parity.json", {
    path: reportPath,
    contentType: "application/json",
  });

  if (!ENFORCE) return;

  const failures = summary.filter((entry) => Math.abs(entry.delta) > ROW_THRESHOLD_PX);
  expect(
    failures,
    formatFailures(
      "row",
      failures.map((failure) => ({
        name: `${failure.kind}:${failure.name}`,
        width: failure.width,
        delta: failure.delta,
        planned: failure.planned,
        actual: failure.actual,
      })),
    ),
  ).toEqual([]);
});

test("workbench: pretext generated assistant streaming fuzz parity", async ({ page }, testInfo) => {
  test.setTimeout(240000);
  test.slow();
  await openWorkbenchShell(page);

  const corpus = generatePretextParityFuzzCorpus({
    seed: readEnvInt("CTX_PRETEXT_FUZZ_SEED", DEFAULT_SEED),
    markdownCount: readEnvInt("CTX_PRETEXT_FUZZ_MARKDOWN_CASES", DEFAULT_MARKDOWN_CASES),
    messageCount: readEnvInt("CTX_PRETEXT_FUZZ_MESSAGE_CASES", DEFAULT_MESSAGE_CASES),
    assistantCount: readEnvInt("CTX_PRETEXT_FUZZ_ASSISTANT_CASES", DEFAULT_ASSISTANT_CASES),
    turnHeaderCount: readEnvInt("CTX_PRETEXT_FUZZ_TURN_HEADER_CASES", DEFAULT_TURN_HEADER_CASES),
  });

  const summary: StreamingSummaryEntry[] = [];
  for (const width of corpus.widths) {
    for (const sample of corpus.assistantStreamingSamples) {
      const measurement = await measureAssistantStreamingParity(page, {
        ...sample.params,
        viewportWidth: width,
      });
      summary.push(
        ...measurement.steps.map((step, index) => ({
          width,
          name: sample.name,
          stepIndex: index,
          content: step.content,
          partialPlanned: step.partial.planned,
          partialActual: step.partial.actual,
          partialDelta: step.partial.delta,
          completePlanned: step.complete.planned,
          completeActual: step.complete.actual,
          completeDelta: step.complete.delta,
          actualDelta: step.actualDelta,
          plannedDelta: step.plannedDelta,
          structureEquivalent: step.structureEquivalent,
        })),
      );
    }
  }

  const report = {
    enforce: ENFORCE,
    thresholdPx: ROW_THRESHOLD_PX,
    widths: corpus.widths,
    counts: {
      assistantStreaming: corpus.assistantStreamingSamples.length,
    },
    samples: summary,
  };
  const reportPath = testInfo.outputPath("pretext-assistant-streaming-fuzz-parity.json");
  await fs.writeFile(reportPath, JSON.stringify(report, null, 2), "utf8");
  await testInfo.attach("pretext-assistant-streaming-fuzz-parity.json", {
    path: reportPath,
    contentType: "application/json",
  });

  if (!ENFORCE) return;

  const failures = summary.filter(
    (entry) =>
      Math.abs(entry.partialDelta) > ROW_THRESHOLD_PX ||
      Math.abs(entry.completeDelta) > ROW_THRESHOLD_PX ||
      Math.abs(entry.actualDelta) > ROW_THRESHOLD_PX ||
      Math.abs(entry.plannedDelta) > ROW_THRESHOLD_PX ||
      !entry.structureEquivalent,
  );
  expect(failures, formatStreamingFailures(failures)).toEqual([]);
});
