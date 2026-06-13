import { expect, test, type Page } from "./fixtures";
import { seedDummyWorkspace } from "./utils/seedDummyWorkspace";

type MarkdownSample = {
  name: string;
  markdown: string;
};

type E2EWindow = Window & {
  __ctxE2E?: {
    measureMarkdownParity?: (samples: readonly MarkdownSample[], width: number) => Promise<Array<{
      name: string;
      planned: number;
      actual: number;
      delta: number;
    }>>;
    measureMarkdownSelectionText?: (markdown: string, width: number) => Promise<string>;
    installMarkdownScrollProbe?: (markdown: string, width?: number) => Promise<boolean>;
    removeMarkdownScrollProbe?: () => boolean;
  };
};

async function openEmptyWorkspace(page: Page) {
  const seed = await seedDummyWorkspace(page.request, {
    tasks: 0,
    sessionsPerTask: 0,
    turnsPerSession: 0,
  });
  await page.goto(`/workspaces/${seed.workspaceId}?ctxE2E=1`, { waitUntil: "domcontentloaded" });
  await page.waitForFunction(() => {
    const api = (window as E2EWindow).__ctxE2E;
    return (
      typeof api?.measureMarkdownParity === "function" &&
      typeof api?.measureMarkdownSelectionText === "function" &&
      typeof api?.installMarkdownScrollProbe === "function" &&
      typeof api?.removeMarkdownScrollProbe === "function"
    );
  });
}

async function measureMarkdownParity(page: Page, samples: readonly MarkdownSample[], width: number) {
  return page.evaluate(({ samples, width }) => {
    const api = (window as E2EWindow).__ctxE2E?.measureMarkdownParity;
    if (typeof api !== "function") {
      throw new Error("ctxE2E.measureMarkdownParity is unavailable");
    }
    return api(samples, width);
  }, { samples, width });
}

async function measureMarkdownSelectionText(page: Page, markdown: string, width: number) {
  return page.evaluate(({ markdown, width }) => {
    const api = (window as E2EWindow).__ctxE2E?.measureMarkdownSelectionText;
    if (typeof api !== "function") {
      throw new Error("ctxE2E.measureMarkdownSelectionText is unavailable");
    }
    return api(markdown, width);
  }, { markdown, width });
}

test("workbench: deterministic markdown planner matches rendered block geometry", async ({ page }) => {
  test.setTimeout(120000);
  await openEmptyWorkspace(page);

  const samples: MarkdownSample[] = [
    {
      name: "inline-code-list",
      markdown: String.raw`A paragraph with inline code like \`pnpm -C core/apps/web typecheck\` and a long URL \`https://example.com/workspaces/public-replay-20260508#fixture=long-fragment-12345\`.

- bullet with \`fixture-app-5182\`
- second bullet with \`testing/documentation\``,
    },
    {
      name: "fenced-code-short",
      markdown: "Before\n\n```ts\nconst value = 1;\nconsole.log(value);\n```\n\nAfter",
    },
    {
      name: "fenced-code-long-line",
      markdown:
        "Before\n\n```bash\npnpm -C core/apps/web exec playwright test -c playwright.pretext-virtualizer-acceptance.config.ts --grep \"rich markdown assistant rows\"\n```\n\nAfter",
    },
    {
      name: "blockquote-code",
      markdown: "> quoted with `inline code`\n>\n> second line\n\n```txt\nhello\nworld\n```",
    },
    {
      name: "table-inline",
      markdown:
        "| Day | Count | Note |\n|---|---:|---|\n| 2026-04-06 | 8 | `fixture-app-5182` |\n| 2026-04-07 | 13 | `testing/documentation` |",
    },
    {
      name: "nested-list",
      markdown: "- outer\n  - nested item with `inline code`\n  - nested two\n- outer two",
    },
    {
      name: "assistant-tail-inline-code-url",
      markdown: String.raw`You can try it on the fixture app at \`https://example.com/workspaces/public-replay-20260508#fixture=long-fragment-12345\`.`,
    },
  ];

  const result = await measureMarkdownParity(page, samples, 788);
  for (const sample of result) {
    expect(
      Math.abs(sample.delta),
      `${sample.name} drifted by ${sample.delta}px (planned ${sample.planned}, actual ${sample.actual})`,
    ).toBeLessThanOrEqual(0.5);
  }
});

test("workbench: vertical wheel over code blocks and tables still scrolls the transcript", async ({ page }) => {
  test.setTimeout(120000);
  await openEmptyWorkspace(page);

  await page.evaluate((markdown) => {
    return (window as E2EWindow).__ctxE2E?.installMarkdownScrollProbe?.(markdown, 788) ?? Promise.resolve(false);
  }, [
    "Before",
    "",
    "```bash",
    "pnpm -C core/apps/web exec playwright test -c playwright.pretext-virtualizer-acceptance.config.ts --grep \"rich markdown assistant rows\"",
    "```",
    "",
    "| Day | Count | Note |",
    "|---|---:|---|",
    "| 2026-04-06 | 8 | a very wide table cell that should require horizontal scrolling |",
    "| 2026-04-07 | 13 | another very wide table cell that should require horizontal scrolling |",
    "",
    "After",
  ].join("\n"));

  const scroller = page.locator("#markdown-scroll-probe [data-pretext-virtualizer-list='1']");
  await expect(scroller).toBeVisible();

  const codeBlock = page.locator("#markdown-scroll-probe .codeblock-pre");
  await expect(codeBlock).toBeVisible();
  const beforeCodeWheel = await scroller.evaluate((el) => el.scrollTop);
  await codeBlock.hover();
  await page.mouse.wheel(0, -240);
  await expect
    .poll(async () => scroller.evaluate((el) => el.scrollTop))
    .toBeLessThan(beforeCodeWheel);

  await scroller.evaluate((el) => {
    el.scrollTop = 520;
    el.dispatchEvent(new Event("scroll", { bubbles: true }));
  });
  const table = page.locator("#markdown-scroll-probe .wb-md-table-scroll");
  await expect(table).toBeVisible();
  const beforeTableWheel = await scroller.evaluate((el) => el.scrollTop);
  await table.hover();
  await page.mouse.wheel(0, -240);
  await expect
    .poll(async () => scroller.evaluate((el) => el.scrollTop))
    .toBeLessThan(beforeTableWheel);

  await page.evaluate(() => {
    (window as E2EWindow).__ctxE2E?.removeMarkdownScrollProbe?.();
  });
});

test("workbench: normal markdown selection text includes explicit list markers", async ({ page }) => {
  test.setTimeout(120000);
  await openEmptyWorkspace(page);

  const selectionText = await measureMarkdownSelectionText(
    page,
    ["1. first item", "2. second item", "", "- bullet item"].join("\n"),
    788,
  );

  expect(selectionText).toContain("1.");
  expect(selectionText).toContain("2.");
  expect(selectionText).toContain("•");
  expect(selectionText).toContain("bullet item");
});

test("workbench: markdown fuzz anchor samples stay in parity", async ({ page }) => {
  test.setTimeout(120000);
  await openEmptyWorkspace(page);

  const samples: MarkdownSample[] = [
    {
      name: "generated-md-9-list-fence",
      markdown:
        "- Context virtualizer virtualizer *probe* agent turn parity [summary](https://example.com/inline-code/virtualizer/transcript/measurement?ref=747).\n- Pretext browser **browser delta** `web/inline-code/src/apps/e2e/blockquote/inline-code` 📏 你好 世界 summary fragment browser entry [message layout parity](https://example.com/assistant/assistant/transcript?ref=833) ~~marker~~:\n- Token summary 🙂 段落 換行 [session context header](https://example.com/streaming-tail/webkit/streaming-tail/measurement?ref=907) `blockquote/fixtures/pretextVirtualizerRowLayout.ts/blockquote/core/web` [token parity inline](https://example.com/parity/inline-code/webkit?ref=309):\n- Render agent marker session ~~pretext header~~ *render session* summary entry message layout *token shell fragment* *summary buffer token* composer stream browser summary turn parity.\n\n```ts\nconst token = 'render-render-entry';\nconsole.log('turn-header/sessionThread/sessionThreadDomMeasurement.tsx/workbenchShell/pages/pretextVirtualizerRowLayout.ts/src', token);\n```",
    },
    {
      name: "generated-md-14-table-heading-blockquote",
      markdown: [
        "| Kind | Token | Note |",
        "|---|---|---|",
        "| agent | `apps/e2e/e2e/core/web/pretextVirtualizerRowLayout.ts` | entry agent virtualizer browser |",
        "| fragment session | `pnpm -C core/apps/web test:e2e:pretext:parity:webkit` | entry header fragment virtualizer summary fragment thread composer |",
        "",
        "## Context browser",
        "",
        "Entry probe browser ~~session~~ *marker layout fragment* 🙂 你好 世界.",
        "",
        "> Turn padding stream ~~summary~~ 🙂 段落 換行 **thread** [inline](https://example.com/assistant/transcript?ref=259);",
        ">",
        "> Padding composer render **context** ~~deterministic~~ `pnpm -C core/apps/web test:e2e:pretext:parity:webkit` summary delta *render agent inline*.",
      ].join("\n"),
    },
  ];

  const result = await measureMarkdownParity(page, samples, 540);
  for (const sample of result) {
    expect(
      Math.abs(sample.delta),
      `${sample.name} drifted by ${sample.delta}px (planned ${sample.planned}, actual ${sample.actual})`,
    ).toBeLessThanOrEqual(0.5);
  }
});
