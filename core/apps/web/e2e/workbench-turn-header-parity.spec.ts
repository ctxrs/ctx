import { expect, test } from "./fixtures";
import {
  measureTurnHeaderParity,
  openWorkbenchShell,
} from "./utils/pretextParity";

const TURN_HEADER_FIXTURE = [
  "- [Predicting the Popularity of Social News Posts](https://cs229.stanford.edu/proj2012/MaguireMichelson-PredictingThePopularityOfSocialNewsPosts.pdf) reports `85% accuracy`, but that was a much easier binary setup on a small old dataset: “popular” was `>100` upvotes and “unpopular” was `<50`, with domain and posting-time features. I would treat that as a classroom proof-of-concept, not strong evidence of a robust front-page predictor.",
  "- The strongest paper I found is [Popularity and Quality in Social News Aggregators](https://archives.iw3c2.org/www2015/documents/proceedings/companion/p815.pdf). On HN it gets out-of-sample `R² ≈ 0.65` for vote dynamics and finds estimated quality correlates strongly with observed score (`Spearman ≈ 0.80`). But that model uses time-series vote/position data after submission. It is not “read the title and content before posting, then predict front-page probability.”",
].join("\n");

const TURN_HEADER_COMMAND_FIXTURE = [
  "Marker render marker inline pretext https://example.com/transcript/inline-code/transcript/inline-code?ref=293 pretextVirtualizerRowLayout.ts/fixtures/fixtures/core/workbenchShell/core/pages.",
  "Session entry entry layout pnpm -C core/apps/web test:e2e:pretext:guardrail inline-code/inline-code/sessionThread/src/sessionThreadDomMeasurement.tsx.",
].join("\n");

const TURN_HEADER_URL_THRESHOLD_FIXTURE = [
  "Composer turn header pretext session https://example.com/transcript/transcript/docs/streaming-tail?ref=972 fixtures/web/fixtures/web.",
  "Command buffer summary ctx serve core/e2e/sessionThread/workbenchShell/inline-code.",
].join("\n");

const TURN_HEADER_URL_AFTER_PROSE_FIXTURE = [
  "Message summary context https://example.com/assistant/streaming-tail/streaming-tail?ref=656 e2e/fixtures/src/web/pages/turn-header.",
  "Summary layout thread pnpm -C core/apps/web test:e2e:pretext:parity:chromium workbenchShell/pretextVirtualizerRowLayout.ts/inline-code/e2e.",
  "Marker stream entry summary session 測試 佈局 🙂.",
].join("\n");

const TURN_HEADER_QUERY_TAIL_FIXTURE = [
  "Entry buffer layout https://example.com/chromium/docs/parity?ref=734 sessionMarkdownMeasurement.ts/pretextVirtualizerRowLayout.ts/pages/table.",
  "Turn parity deterministic padding git status git status.",
  "Turn session entry 你好 世界 🧪.",
].join("\n");

test("workbench: expanded turn-header planner matches rendered height for URL-heavy transcript text", async ({
  page,
}) => {
  test.setTimeout(120000);
  await openWorkbenchShell(page);

  const measurement = await measureTurnHeaderParity(page, { content: TURN_HEADER_FIXTURE });

  expect(
    Math.abs(measurement.delta),
    `turn_header drifted by ${measurement.delta}px (planned ${measurement.planned}, actual ${measurement.actual})`,
  ).toBeLessThanOrEqual(1);
});

test("workbench: expanded turn-header planner matches rendered height for path and command plain text", async ({
  page,
}) => {
  test.setTimeout(120000);
  await openWorkbenchShell(page);

  const measurement = await measureTurnHeaderParity(page, { content: TURN_HEADER_COMMAND_FIXTURE });

  expect(
    Math.abs(measurement.delta),
    `turn_header drifted by ${measurement.delta}px (planned ${measurement.planned}, actual ${measurement.actual})`,
  ).toBeLessThanOrEqual(1);
});

test("workbench: expanded turn-header planner matches rendered height for url threshold seams", async ({
  page,
}) => {
  test.setTimeout(120000);
  await openWorkbenchShell(page);

  const measurement = await measureTurnHeaderParity(page, {
    content: TURN_HEADER_URL_THRESHOLD_FIXTURE,
    viewportWidth: 620,
  });

  expect(
    Math.abs(measurement.delta),
    `turn_header drifted by ${measurement.delta}px (planned ${measurement.planned}, actual ${measurement.actual})`,
  ).toBeLessThanOrEqual(1);
});

test("workbench: expanded turn-header planner matches rendered height for url continuation after prose", async ({
  page,
}) => {
  test.setTimeout(120000);
  await openWorkbenchShell(page);

  const measurement = await measureTurnHeaderParity(page, {
    content: TURN_HEADER_URL_AFTER_PROSE_FIXTURE,
    viewportWidth: 540,
  });

  expect(
    Math.abs(measurement.delta),
    `turn_header drifted by ${measurement.delta}px (planned ${measurement.planned}, actual ${measurement.actual})`,
  ).toBeLessThanOrEqual(1);
});

test("workbench: expanded turn-header planner matches rendered height for wrapped query-tail path seams", async ({
  page,
}) => {
  test.setTimeout(120000);
  await openWorkbenchShell(page);

  const measurement = await measureTurnHeaderParity(page, {
    content: TURN_HEADER_QUERY_TAIL_FIXTURE,
    viewportWidth: 472,
  });

  expect(
    Math.abs(measurement.delta),
    `turn_header drifted by ${measurement.delta}px (planned ${measurement.planned}, actual ${measurement.actual})`,
  ).toBeLessThanOrEqual(1);
});
