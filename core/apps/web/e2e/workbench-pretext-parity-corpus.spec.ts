import { promises as fs } from "fs";
import { expect, test } from "./fixtures";
import {
  type MarkdownSample,
  measureAssistantParity,
  measureMarkdownParity,
  measureMessageParity,
  measureTurnHeaderParity,
  openWorkbenchShell,
} from "./utils/pretextParity";

const ENFORCE = process.env.CTX_PRETEXT_PARITY_ENFORCE === "1";
const WIDTHS = [540, 620, 788];
const THRESHOLD_WIDTHS = [472, 768, 788];
const MARKDOWN_THRESHOLD_PX = 1;
const ROW_THRESHOLD_PX = 1;

const IMAGE_DATA_BASE64 =
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO2VzJ8AAAAASUVORK5CYII=";

const MARKDOWN_CORPUS: MarkdownSample[] = [
  {
    name: "two-chip-threshold-wrap",
    markdown:
      "I also verified the real workspace is populated. Its active snapshot currently reports `7` active tasks, including `Test Taxonomy`.",
  },
  {
    name: "adjacent-inline-code",
    markdown:
      "Paragraph with `alpha-beta-gamma-delta/ctx/path/one` `second-inline-token/with/path/two` beside prose and punctuation.",
  },
  {
    name: "code-comma-tail",
    markdown: "Reopen `origin/main`, then inspect the queue again.",
  },
  {
    name: "three-chip-prose",
    markdown: "Compare `origin/main`, `Test Taxonomy`, and `ctx serve` before replying.",
  },
  {
    name: "period-after-code-tail",
    markdown: "We shipped `ctx serve`. Then we reopened `origin/main` again.",
  },
  {
    name: "colon-command-chip-after-prose",
    markdown:
      "This slice is in a good state now: the inline hot path split is green, the viewport scroll/controller extraction is green, and `verify:quick` is back to only the same unrelated Rust hard-cap failures. I’m using one short subagent pass now to reassess what the next highest-value cleanup is after these two reductions, so the next move stays architecture-first instead of random file shaving.",
  },
  {
    name: "whitespace-code-comma-tail-long-prose",
    markdown:
      "The current local validation failure is concrete and fixable: `ctx-avf-linux-runtime` has a test helper that assumes Python `shutil.copytree(..., dirs_exist_ok=True)`, which breaks on the Python available in the runner. I’m fixing that first, because nothing else about validation cleanup matters while the required check is red.",
  },
  {
    name: "whitespace-inline-code-prose-tail",
    markdown:
      "The `release updater web e2e` lane was recently hanging because the desktop harness fixtures were returning fake auth tokens and letting real update-check behavior leak through. I fixed the immediate bug, but that exposed test-fixture fragility.",
  },
  {
    name: "list-whitespace-inline-code-prose-tail",
    markdown:
      "- The `release updater web e2e` lane was recently hanging because the desktop harness fixtures were returning fake auth tokens and letting real update-check behavior leak through. I fixed the immediate bug, but that exposed test-fixture fragility.",
  },
  {
    name: "dotted-call-inline-code-prose-tail",
    markdown:
      "The root cause was in main.rs: every `CloseRequested` event called the single app-wide `ConnectionManager.disconnect()`. That tore down the shared local daemon or SSH tunnel for all windows, so the remaining window recovered against a restarted/disconnected daemon and active turns got reconciled as interrupted, which is why conversations looked “paused”.",
  },
  {
    name: "short-dotted-call-inline-code-prose-tail",
    markdown:
      "The observer stays healthy when `observer.disconnect()` moves as one chip before the trailing prose explains why the restart no longer interrupts the session unexpectedly.",
  },
  {
    name: "inline-code-link-prose",
    markdown:
      "Use [`ctx docs`](https://example.com/docs) with `pnpm -C core/apps/web test:e2e:pretext:parity:webkit` and more prose to wrap near the edge.",
  },
  {
    name: "blockquote-list-code",
    markdown:
      "> quoted intro with `inline-code-token`\n>\n> second line with a [link](https://example.com/path)\n\n- bullet with `nested-inline-code`\n- bullet with trailing prose for wrap pressure",
  },
  {
    name: "fenced-code-long-line",
    markdown:
      "Before\n\n```bash\npnpm -C core/apps/web exec playwright test -c playwright.pretext-virtualizer-acceptance.config.ts e2e/workbench-pretext-parity-corpus.spec.ts --browser=webkit --workers=1\n```\n\nAfter",
  },
  {
    name: "table-inline-code",
    markdown:
      "| Env | Command | Note |\n|---|---|---|\n| dev | `pnpm dev` | wraps with prose and punctuation |\n| test | `pnpm -C core/apps/web test:e2e:pretext:parity:chromium` | very long note that should keep wrapping |",
  },
  {
    name: "table-inline-command-threshold",
    markdown:
      "| Left | Token | Note |\n| --- | --- | --- |\n| fragment session | `pnpm -C core/apps/web test:e2e:pretext:parity:webkit` | entry header fragment virtualizer summary fragment thread composer |",
  },
  {
    name: "table-inline-code-prose-tail",
    markdown:
      "| Layout | Primary token | Secondary token |\n|---|---:|---:|\n| wide transcript row | `large-synthetic-token`: 589px sample + padding | `balanced-synthetic-token`: 153px |\n| compact summary row | `standard-fixture-token`: 521px planned width | `shared-fixture-token`: 92px, or `balanced-synthetic-token`: 153px near the seam |",
  },
  {
    name: "list-slash-delimited-prose-token",
    markdown:
      "- **Layout execution**: synthetic width band only, narrow/wide viewport, fast `row-cache` or local fixture data depending on workload, alpha/beta/gamma installed, strict panel/security-state controls, one header/workspace isolation model as needed.",
  },
  {
    name: "hard-break-inline-code",
    markdown:
      "First line with `ctx run --mode strict`\nsecond line with `very-long-inline-token/that/should/wrap` and more prose after it.",
  },
  {
    name: "nested-list-wrap",
    markdown:
      "- outer item with `inline-code-token`\n  - nested item with long prose and `ctx/pretext/pathlike/token`\n  - nested sibling with [docs](https://example.com) and punctuation",
  },
  {
    name: "emoji-cjk-inline-code",
    markdown: "Status check 🙂 with `inline-token-path/segment` and mixed CJK text 你好 世界 to stress shaping.",
  },
  {
    name: "emoji-cjk-inline-code-threshold",
    markdown:
      "summary 🙂 測試 佈局 `core/e2e/pretextVirtualizerRowLayout.ts/sessionThreadDomMeasurement.tsx/apps/turn-header`.",
  },
  {
    name: "blockquote-link-code-tail",
    markdown:
      "> [summary message summary](https://example.com/inline-code/parity/webkit/parity?ref=781) `cargo test -p ctx-store`:",
  },
];

type SoftBreakMarkdownSample = MarkdownSample & {
  width: number;
  widths?: readonly number[];
  browsers?: Array<"chromium" | "webkit">;
};

const SOFT_BREAK_MARKDOWN_REGRESSIONS: SoftBreakMarkdownSample[] = [
  {
    name: "soft-break-path-start-later-fragment",
    width: 518.64,
    markdown: [
      "Buffer session summary ~~turn~~ ⚙️ 你好 世界 `core/blockquote/sessionThreadDomMeasurement.tsx/sessionThreadDomMeasurement.tsx` ~~command~~ ~~command command~~ **context**: `workbenchShell/fixtures/turn-header/table`",
      "Stream browser agent ⚙️ 測試 佈局 header deterministic thread summary buffer entry context ⚙️ 你好 世界 `web/fixtures/core/fixtures`. `pnpm -C core/apps/web test:e2e:pretext:corpus:webkit`",
    ].join("\n"),
  },
  {
    name: "soft-break-mixed-inline-seams",
    width: 445.04,
    markdown: [
      "Fragment fragment command [thread deterministic](https://example.com/webkit/docs/chromium/transcript?ref=917) `pnpm -C core/apps/web test:e2e:pretext:guardrail` **render** 🧪 段落 換行; `sessionMarkdownMeasurement.ts/pretextVirtualizerRowLayout.ts/src/core`",
      "Command message composer buffer *session parity* 🙂 測試 佈局 *virtualizer buffer*; `git status`",
      "Thread inline `sessionMarkdownMeasurement.ts/e2e/e2e/web/turn-header/turn-header` *stream entry browser* `pnpm -C core/apps/web test:e2e:pretext:parity:chromium` 📏 你好 世界. `turn-header/pages/sessionThreadDomMeasurement.tsx/pages/table/e2e`",
      "Parity render probe fragment *layout* `git rev-parse HEAD` **context** buffer stream `e2e/pretextVirtualizerRowLayout.ts/apps/table/table/src`. `pnpm -C core/apps/web test:e2e:pretext:parity:chromium`",
    ].join("\n"),
  },
  {
    name: "soft-break-fresh-line-path-full-chrome",
    width: 516,
    markdown: [
      "Fragment fragment command [thread deterministic](https://example.com/webkit/docs/chromium/transcript?ref=917) `pnpm -C core/apps/web test:e2e:pretext:guardrail` **render** 🧪 段落 換行; `sessionMarkdownMeasurement.ts/pretextVirtualizerRowLayout.ts/src/core`",
      "Command message composer buffer *session parity* 🙂 測試 佈局 *virtualizer buffer*; `git status`",
      "Thread inline `sessionMarkdownMeasurement.ts/e2e/e2e/web/turn-header/turn-header` *stream entry browser* `pnpm -C core/apps/web test:e2e:pretext:parity:chromium` 📏 你好 世界. `turn-header/pages/sessionThreadDomMeasurement.tsx/pages/table/e2e`",
    ].join("\n"),
  },
  {
    name: "soft-break-friendly-boundary-prose-tail",
    width: 445.04,
    markdown: [
      "Delta agent browser entry `sessionThread/src/sessionThread/e2e/sessionThread/table` `table/sessionThread/apps/workbenchShell/fixtures/turn-header` inline pretext parity. `table/web/fixtures/table`",
      "Message deterministic virtualizer `apps/workbenchShell/src/src/sessionMarkdownMeasurement.ts/e2e` *pretext summary turn* padding fragment thread turn context pretext turn buffer summary [stream pretext](https://example.com/transcript/webkit?ref=266). `cargo test -p ctx-http`",
    ].join("\n"),
  },
  {
    name: "soft-break-attached-trailing-plain-path-start",
    width: 445.04,
    markdown: [
      "Render deterministic [command session deterministic](https://example.com/docs/inline-code/measurement?ref=150) *token virtualizer* ~~agent shell~~ session parity shell inline fragment; `e2e/turn-header/workbenchShell/table`",
      "Virtualizer probe render 🙂 段落 換行 *composer browser render* session browser header layout command session layout parity command: `sessionThread/table/web/workbenchShell/apps/blockquote`",
      "Stream token stream buffer entry fragment marker 📏 段落 換行 ~~token token~~ [buffer](https://example.com/parity/streaming-tail?ref=574) 🧪 段落 換行 ~~fragment~~. `ctx run start --mode sandbox`",
    ].join("\n"),
  },
  {
    name: "soft-break-wide-glyph-friendly-boundary",
    width: 445.04,
    browsers: ["webkit"],
    markdown: [
      "Layout composer fragment padding `sessionMarkdownMeasurement.ts/src/sessionThreadDomMeasurement.tsx/web/blockquote/workbenchShell/workbenchShell` **command inline** `cargo test -p ctx-store` ⚙️ 測試 佈局 [agent fragment](https://example.com/transcript/chromium?ref=812) *summary fragment*. `apps/fixtures/src/src/sessionMarkdownMeasurement.ts/pages`",
      "Virtualizer probe render 🙂 段落 換行 *composer browser render* session browser header layout command session layout parity command: `sessionThread/table/web/workbenchShell/apps/blockquote`",
    ].join("\n"),
  },
  {
    name: "soft-break-command-option-hyphen-tail",
    width: 382.48,
    markdown: [
      "Parity render probe fragment *layout* `git rev-parse HEAD` **context** buffer stream `e2e/pretextVirtualizerRowLayout.ts/apps/table/table/src`. `pnpm -C core/apps/web test:e2e:pretext:parity:chromium`",
      "Entry shell agent command buffer agent command inline browser token deterministic padding [agent](https://example.com/streaming-tail/transcript/streaming-tail/transcript?ref=527) [agent composer render](https://example.com/measurement/webkit?ref=119): `pnpm -C core/apps/web test:e2e:pretext:parity:webkit`",
    ].join("\n"),
  },
  {
    name: "soft-break-command-leading-hang",
    width: 382.48,
    markdown: [
      "Layout composer fragment padding `sessionMarkdownMeasurement.ts/src/sessionThreadDomMeasurement.tsx/web/blockquote/workbenchShell/workbenchShell` command inline `cargo test -p ctx-store` agent fragment summary fragment. `apps/fixtures/src/src/sessionMarkdownMeasurement.ts/pages`",
      "Inline stream browser context thread layout padding fragment header probe session summary token inline; `git rev-parse HEAD`",
      "Render deterministic command session deterministic token virtualizer agent shell session parity shell inline fragment; `e2e/turn-header/workbenchShell/table`",
    ].join("\n"),
  },
  {
    name: "soft-break-slash-delimited-prose-token",
    width: 790,
    markdown:
      "I’ve got a second class now: several of the failures in `new-layout 9` are plain network-state markers from a grouped/ungrouped path, not just bad metadata. I’m checking whether those attempts were targeting the render runtime rather than the header row, because that would explain the `measure` `[Errno 101] sample is unreachable` pattern across multiple fixtures.",
  },
  {
    name: "soft-break-styled-slash-mixed-paragraph",
    width: 472,
    markdown: [
      "Context render للالتفاف around the width threshold ทดสอบการตัดคำ pretext context 📏 測試 佈局 ทดสอบการตัดคำ browser:",
      "Pretext header ភាសាខ្មែរគ្មានដកឃ្លា probe stream [deterministic marker](https://example.com/virtualizer/webkit/inline-code/webkit?ref=349) **command marker deterministic** grid/flow delta padding prix : browser summary.",
    ].join("\n"),
  },
  {
    name: "soft-break-link-slash-paragraph",
    width: 788,
    markdown: [
      "Shell virtualizer buffer C⁠D context marker browser context A B command alpha/beta/gamma 📏 測試 佈局 layout A B summary command thread delta​epsilon​zeta message thread:",
      "Parity turn message ⚙️ 你好 世界 日本語行分割 段落 換行 בדיקת עיטוף near the seam [header token session](https://example.com/chromium/assistant/chromium?ref=447) browser padding stream.",
    ].join("\n"),
  },
  {
    name: "soft-break-table-inline-code-slash-tail",
    width: 472,
    markdown: [
      "| kind | content | note |",
      "| --- | --- | --- |",
      "| command | `sessionThreadDomMeasurement.tsx/table/blockquote/sessionThread/core/inline-code`: alpha/beta/gamma | short |",
    ].join("\n"),
  },
  {
    name: "soft-break-table-inline-code-punctuation-tail",
    width: 472,
    markdown: [
      "| Kind | Token | Note |",
      "| --- | --- | --- |",
      "| deterministic | `pnpm -C core/apps/web test:e2e:pretext:parity:webkit`: thread delta message | short |",
    ].join("\n"),
  },
  {
    name: "soft-break-table-inline-code-strict-path-tail",
    width: 472,
    browsers: ["webkit"],
    markdown: [
      "| Kind | Token | Note |",
      "| --- | --- | --- |",
      "| thread | `table/core/core/sessionMarkdownMeasurement.ts/turn-header/turn-header/web`: layout virtualizer virtualizer stream buffer grid/flow | padding layout summary probe browser layout command |",
    ].join("\n"),
  },
  {
    name: "soft-break-list-slash-token-tail",
    width: 788,
    browsers: ["webkit"],
    markdown:
      "- **Layout execution**: synthetic width band only, grid/flow viewport, fast `row-cache` or local fixture data depending on workload, alpha/beta/gamma installed, strict panel/security-state controls, one header/workspace isolation model as needed.",
  },
  {
    name: "soft-break-list-code-continuation-bidi-tail",
    width: 788,
    browsers: ["chromium"],
    markdown:
      "- Turn parity `blockquote/sessionMarkdownMeasurement.ts/sessionMarkdownMeasurement.ts/workbenchShell/table` 🙂 測試 佈局 probe delta​epsilon​zeta summary deterministic A B turn [virtualizer probe](https://example.com/inline-code/chromium/transcript?ref=993) בדיקת עיטוף with code pressure.",
  },
  {
    name: "reported-active-heads-dotted-code-continuation",
    width: 426,
    browsers: ["webkit"],
    markdown:
      "- That effect also depends on `workspaceSnapshot.tasksById`, so active snapshot churn can retrigger prefetch repeatedly.",
  },
  {
    name: "reported-active-heads-hyphenated-token-break",
    width: 456,
    browsers: ["webkit"],
    markdown: [
      "2. **Use active heads for first paint**",
      "   If `active_heads` has a compatible non-empty bounded head, render it immediately as bootstrap/pending-authority. Do not let it overwrite newer authoritative replica state.",
    ].join("\n"),
  },
  {
    name: "reported-layout-fixture-inline-code-thresholds",
    width: 500,
    widths: [384, 408, 464, 488, 500, 504, 728],
    markdown: [
      "A few synthetic layout choices are worth confirming before implementation:",
      "",
      "1. **Scope/name**",
      "   I’d call this `layout-fixtures` or `experiments`, not tie it to a specific environment in the user-facing concept.",
      "   Concrete labels:",
      "   `layout-fixture=true`",
      "   `layout-fixture-pool=experiments`",
      "   `layout-fixture-purpose=<purpose>`",
      "   `layout-fixture-owner=synthetic`",
      "   `layout-fixture-expiry=<epoch>`",
      "",
      "2. **Default TTL**",
      "   I’d default to `6h`, allow `--max-age-hours`, and require an explicit `--retain` or longer TTL for anything above maybe `24h`.",
      "",
      "3. **Default sample type**",
      "   For profiling, I’d default to a dedicated width shape, probably `wide-medium` or whatever is already standard in our fixture generator. For cheap general experiments, `shared-small` is enough, but noisy shared samples are worse for profiling.",
      "",
      "4. **Marker loading**",
      "   I’d make the wrapper read `PUBLIC_FIXTURE_MARKER` from the caller environment. Env still wins.",
      "",
      "5. **Cleanup**",
      "   I’d add `tools/layout-fixture cleanup`, but scheduled execution is a separate step. The tool can be ready now; wiring hourly cleanup can be a follow-up unless you want it in this slice.",
      "",
      "6. **State location**",
      "   I’d use `fixtures/runtime/session-state-<run-id>.json` if that directory is acceptable, otherwise `/tmp/layout-fixture-<run-id>.json`. I prefer fixture-local state for discoverability if it is ignored.",
      "",
      "7. **Run handle**",
      "   Generate a per-run handle and delete it on `down`. That avoids relying on shared mutable state and makes cleanup auditable.",
      "",
      "8. **Fixture level**",
      "   First slice should create a reachable synthetic row with basic tokens, not fully install a product flow. Then profiling scripts can provision task-specific dependencies.",
      "",
      "My recommendation: implement the generic `experiments` pool with labels and TTL cleanup now, no scheduled job yet. Then use it for the layout-load research.",
    ].join("\n"),
  },
  {
    name: "reported-pretext-help-rich-inline-composition",
    width: 788,
    widths: [472, 540, 620, 788],
    markdown: [
      "It does help. It just doesn’t solve the whole problem.",
      "",
      "What pretext gives us:",
      "- fast deterministic text measurement",
      "- segmenting and line-breaking for a prepared text run",
      "- support for things like `white-space`, `overflow-wrap`, grapheme-level breaks, etc.",
      "- a cheap arithmetic layout path instead of DOM measurement",
      "",
      "What it does not give us:",
      "- full browser inline formatting for rich markdown",
      "- DOM/span boundary semantics",
      "- CSS seam behavior between adjacent styled runs",
      "- inline code “chip” chrome behavior",
      "- table/list/block composition",
      "- browser-engine-specific wrap quirks across mixed inline nodes",
      "",
      "The recent bugs were exactly in that glue layer:",
      "- `containerized/sandboxed`",
      "  Our planner treated `/` too much like a break seam in ordinary prose.",
      "- `**Important caveat** ... fast path:`",
      "  Our planner was too willing to drop collapsed seam space after styled text.",
      "",
      "So the blunt answer is:",
      "- pretext solves text measurement and single-run wrapping very well",
      "- our remaining long tail is rich-inline composition parity with browser DOM",
      "- that is one layer above pretext",
    ].join("\n"),
  },
  {
    name: "soft-break-inline-code-overwide-slash-tail",
    width: 148,
    browsers: ["webkit"],
    markdown:
      "`ctx run start --mode sandbox`: agent render turn agent padding alpha/beta/gamma",
  },
  {
    name: "soft-break-styled-seam-collapse-space",
    width: 788,
    markdown: [
      "Yes, that split can make sense.",
      "",
      "I would recommend:",
      "",
      "- **provider package itself**: shorter gate",
      "- **its transitive deps**: longer gate",
      "",
      "Because:",
      "- the top-level provider version is something we are intentionally onboarding and reviewing",
      "- the transitive graph is where a lot of surprise risk lives",
      "",
      "A good starting policy would be:",
      "",
      "- **provider package**: `12-24h`",
      "- **transitive deps**: `72h`",
      "",
      "For a fast-moving provider like Claude, I would lean:",
      "",
      "- `24h` default for the provider itself",
      "- `72h` for deps",
      "",
      "And then add an explicit **expedite path** for an exact top-level provider version when you want it faster.",
      "",
      "**How to do that in practice**",
      "If we use `pnpm`, the clean way is:",
      "",
      "- set global `minimumReleaseAge: 4320` (`72h`)",
      "- add a **version-specific exclusion** for the exact provider version you are intentionally admitting, using `minimumReleaseAgeExclude`",
      "",
      "That means:",
      "- `@anthropic-ai/...@X.Y.Z` can be admitted early",
      "- everything else in the graph still has to be old enough",
      "",
      "So the policy becomes:",
      "",
      "1. provider maintainer wants `claude X.Y.Z`",
      "2. we add an exact-version exception for that top-level package only",
      "3. CI builds it in quarantine",
      "4. deps still obey the stricter age gate",
      "5. if it passes smoke/provenance/manual review, we publish our artifact",
      "",
      "That is much better than lowering the whole graph’s age gate.",
      "",
      "**Important caveat**",
      "Do not make the provider package age gate too small unless you also require stronger checks for that fast path:",
      "",
      "- exact version pin",
      "- provenance/trusted publisher check if available",
      "- package diff review vs previous version",
      "- no unexpected lifecycle-script changes",
      "- quarantined smoke build",
      "- explicit human approval",
      "",
      "So my actual recommendation is:",
      "",
      "- **normal policy**: top-level `24h`, deps `72h`",
      "- **expedite policy**: top-level can go below `24h` only by exact-version exception plus extra checks",
      "- **never** lower the transitive dependency gate just because you want a new provider quickly",
      "",
      "That gives you fast Claude onboarding without turning the whole dependency graph into a same-day supply-chain gamble.",
    ].join("\n"),
  },
  {
    name: "unicode-soft-hyphen-pre-threshold",
    width: 220,
    markdown:
      "Manual hyphenation sample: Deoxy\u00adribo\u00adnucleic acid remains readable near the wrap threshold when the soft hyphen becomes active.",
  },
  {
    name: "unicode-soft-hyphen-threshold",
    width: 222,
    markdown:
      "Manual hyphenation sample: Deoxy\u00adribo\u00adnucleic acid remains readable near the wrap threshold when the soft hyphen becomes active.",
  },
  {
    name: "unicode-arabic-styled-seam",
    width: 144,
    markdown: "هذا **اختبار** للالتفاف around the width threshold",
  },
  {
    name: "unicode-rtl-inline-code",
    width: 258,
    markdown: "RTL sample: בדיקת עיטוף `observer.disconnect()` ליד טקסט עברי בקצה הרוחב.",
  },
  {
    name: "unicode-zero-width-space",
    width: 180,
    markdown: "ZWSP sample: alpha\u200bbeta\u200bgamma should break only at the discretionary boundaries.",
  },
  {
    name: "unicode-thai-implicit-word-break",
    width: 144,
    markdown: "กรุงเทพคือสวยงามและต้องทดสอบการตัดคำในย่อหน้าที่ไม่มีเว้นวรรคมากนัก",
  },
  {
    name: "list-inline-code-prose-tail",
    width: 588,
    markdown:
      "- Command fragment [stream layout](https://example.com/docs/parity/webkit?ref=298) *agent virtualizer* ~~inline~~ `pages/fixtures/sessionMarkdownMeasurement.ts/sessionMarkdownMeasurement.ts/blockquote/inline-code/e2e` render parity composer probe render header render summary;",
  },
  {
    name: "path-inline-code-prose-tail",
    width: 564,
    markdown:
      "Command fragment [stream layout](https://example.com/docs/parity/webkit?ref=298) *agent virtualizer* ~~inline~~ `pages/fixtures/sessionMarkdownMeasurement.ts/sessionMarkdownMeasurement.ts/blockquote/inline-code/e2e` render parity composer probe render header render summary;",
  },
  {
    name: "longer-path-prefix-code-only",
    width: 382.48,
    markdown: "- Delta parity `table/fixtures/sessionThreadDomMeasurement.tsx/workbenchShell`",
  },
  {
    name: "decorated-tail-after-path-code",
    width: 620,
    markdown:
      "- Agent token marker `src/src/pretextVirtualizerRowLayout.ts/core/blockquote` *padding entry* **marker stream**:",
  },
];

const USER_MESSAGE_CORPUS = [
  {
    name: "multi-paragraph-user",
    params: {
      content: [
        "This is a longer user message with a paragraph that should wrap comfortably inside the message bubble.",
        "It also includes `inline-code-token/with/path/segments` plus more prose afterward to keep the wrap pressure realistic.",
        "The final paragraph mentions `pnpm -C core/apps/web test:e2e:pretext:parity:chromium` to mimic real commands.",
      ].join("\n\n"),
      expanded: true,
    },
  },
  {
    name: "threshold-user-tail",
    params: {
      content: [
        "Keep `7` active tasks, including `Test Taxonomy`, aligned before replying.",
        "Compare `origin/main`, `ctx serve`, and `stable lane` again after the transcript reload.",
      ].join("\n\n"),
      expanded: true,
    },
  },
  {
    name: "collapsed-user",
    params: {
      content: Array.from(
        { length: 28 },
        (_, index) => `line ${index + 1} with enough words to wrap and keep the collapsed toggle alive`,
      ).join("\n"),
      expanded: false,
    },
  },
  {
    name: "user-with-images",
    params: {
      content: "two inline screenshots and a path `ctx/pretext/harness/attachment-check`",
      expanded: true,
      attachments: [
        { kind: "image" as const, mime_type: "image/png", data_base64: IMAGE_DATA_BASE64, name: "one.png" },
        { kind: "image" as const, mime_type: "image/png", data_base64: IMAGE_DATA_BASE64, name: "two.png" },
      ],
    },
  },
];

const ASSISTANT_CORPUS = [
  {
    name: "two-chip-threshold-wrap",
    params: {
      content:
        "I also verified the real workspace is populated. Its active snapshot currently reports `7` active tasks, including `Test Taxonomy`.",
    },
  },
  {
    name: "wrapped-inline-code",
    params: {
      content:
        "begin agent message with plain text and now `inline-thing-that-actually-gets-really-long-so-much-so-that-it-wraps-to-multiple-lines/core/apps/web/src/pages/sessionThread/sessionMarkdownMeasurement.ts` after the prose",
    },
  },
  {
    name: "adjacent-inline-code",
    params: {
      content:
        "multiple chips `first-very-long-token/with/path/a` and `second-even-longer-token/with/path/b` in one sentence with trailing prose",
    },
  },
  {
    name: "code-comma-tail",
    params: {
      content: "Reopen `origin/main`, then inspect the queue again.",
    },
  },
  {
    name: "three-chip-prose",
    params: {
      content: "Compare `origin/main`, `Test Taxonomy`, and `ctx serve` before replying.",
    },
  },
  {
    name: "period-after-code-tail",
    params: {
      content: "We shipped `ctx serve`. Then we reopened `origin/main` again.",
    },
  },
  {
    name: "colon-command-chip-after-prose",
    params: {
      content:
        "This slice is in a good state now: the inline hot path split is green, the viewport scroll/controller extraction is green, and `verify:quick` is back to only the same unrelated Rust hard-cap failures. I’m using one short subagent pass now to reassess what the next highest-value cleanup is after these two reductions, so the next move stays architecture-first instead of random file shaving.",
    },
  },
  {
    name: "whitespace-code-comma-tail-long-prose",
    params: {
      content:
        "The current local validation failure is concrete and fixable: `ctx-avf-linux-runtime` has a test helper that assumes Python `shutil.copytree(..., dirs_exist_ok=True)`, which breaks on the Python available in the runner. I’m fixing that first, because nothing else about validation cleanup matters while the required check is red.",
    },
  },
  {
    name: "inline-code-link-mix",
    params: {
      content:
        "Check [`docs`](https://example.com/docs) and run `pnpm -C core/apps/web test:e2e:pretext:parity:webkit` before replying.",
    },
  },
  {
    name: "blockquote-and-code",
    params: {
      content: "> quote with `inline code`\n>\n> second line\n\nfollowed by prose after the quote for more wrapping.",
    },
  },
  {
    name: "list-fence-mix",
    params: {
      content: "- bullet one with `inline-code-token`\n- bullet two\n\n```ts\nconst value = 'ctx';\nconsole.log(value);\n```",
    },
  },
  {
    name: "streaming-tail",
    params: {
      content: "Before\n\n- partial item with `inline-tail-token/with/path`",
      isComplete: false,
    },
  },
];

const THRESHOLD_MARKDOWN_SAMPLE_NAMES = new Set([
  "two-chip-threshold-wrap",
  "code-comma-tail",
  "three-chip-prose",
  "period-after-code-tail",
  "colon-command-chip-after-prose",
]);
const THRESHOLD_USER_SAMPLE_NAMES = new Set(["threshold-user-tail"]);
const THRESHOLD_ASSISTANT_SAMPLE_NAMES = new Set([
  "two-chip-threshold-wrap",
  "code-comma-tail",
  "three-chip-prose",
  "period-after-code-tail",
  "colon-command-chip-after-prose",
]);

const TURN_HEADER_CORPUS = [
  {
    name: "url-heavy",
    params: {
      content: [
        "- https://example.com/a/really/long/path/that/keeps/wrapping?token=12345 should not drift when wrapped inside the turn header bubble.",
        "- Follow-up line with fixtures/worktrees/public-replay/root-alpha/session-thread path pressure.",
      ].join("\n"),
    },
  },
  {
    name: "multiline-wrap",
    params: {
      content: [
        "Need follow-up on the long transcript planner path and the completion row interaction.",
        "Also verify that narrow widths keep the header height in sync after wrap pressure.",
      ].join("\n"),
    },
  },
  {
    name: "url-command-path-threshold",
    params: {
      content: [
        "Message summary context https://example.com/assistant/streaming-tail/streaming-tail?ref=656 e2e/fixtures/src/web/pages/turn-header.",
        "Summary layout thread pnpm -C core/apps/web test:e2e:pretext:parity:chromium workbenchShell/pretextVirtualizerRowLayout.ts/inline-code/e2e.",
        "Marker stream entry summary session 測試 佈局 🙂.",
      ].join("\n"),
    },
  },
];

type MarkdownSummaryEntry = {
  width: number;
  name: string;
  planned: number;
  actual: number;
  delta: number;
};

type RowSummaryEntry = {
  kind: "message" | "assistant" | "turn_header";
  width: number;
  name: string;
  planned: number;
  actual: number;
  delta: number;
};

function formatFailures(kind: string, failures: Array<{ name: string; width: number; delta: number; planned: number; actual: number }>): string {
  return `${kind} parity failures:\n${failures
    .map((failure) => `- width ${failure.width} ${failure.name}: delta=${failure.delta}px planned=${failure.planned} actual=${failure.actual}`)
    .join("\n")}`;
}

test("workbench: pretext markdown corpus parity sweep", async ({ page }, testInfo) => {
  test.setTimeout(180000);
  test.slow();
  await openWorkbenchShell(page);

  const summary: MarkdownSummaryEntry[] = [];
  for (const width of WIDTHS) {
    const measurements = await measureMarkdownParity(page, MARKDOWN_CORPUS, width);
    summary.push(...measurements.map((measurement) => ({ width, ...measurement })));
  }

  const report = {
    enforce: ENFORCE,
    thresholdPx: MARKDOWN_THRESHOLD_PX,
    widths: WIDTHS,
    samples: summary,
  };
  const reportPath = testInfo.outputPath("pretext-markdown-corpus-parity.json");
  await fs.writeFile(reportPath, JSON.stringify(report, null, 2), "utf8");
  await testInfo.attach("pretext-markdown-corpus-parity.json", {
    path: reportPath,
    contentType: "application/json",
  });

  if (!ENFORCE) return;

  const failures = summary.filter((entry) => Math.abs(entry.delta) > MARKDOWN_THRESHOLD_PX);
  expect(
    failures,
    formatFailures("markdown", failures.map((failure) => ({
      name: failure.name,
      width: failure.width,
      delta: failure.delta,
      planned: failure.planned,
      actual: failure.actual,
    }))),
  ).toEqual([]);
});

test("workbench: pretext threshold seam parity sweep", async ({ page }) => {
  test.setTimeout(180000);
  test.slow();
  await openWorkbenchShell(page);

  const markdownSamples = MARKDOWN_CORPUS.filter((sample) => THRESHOLD_MARKDOWN_SAMPLE_NAMES.has(sample.name));
  const userSamples = USER_MESSAGE_CORPUS.filter((sample) => THRESHOLD_USER_SAMPLE_NAMES.has(sample.name));
  const assistantSamples = ASSISTANT_CORPUS.filter((sample) => THRESHOLD_ASSISTANT_SAMPLE_NAMES.has(sample.name));

  const markdownSummary: MarkdownSummaryEntry[] = [];
  const rowSummary: RowSummaryEntry[] = [];

  for (const width of THRESHOLD_WIDTHS) {
    const markdownMeasurements = await measureMarkdownParity(page, markdownSamples, width);
    markdownSummary.push(...markdownMeasurements.map((measurement) => ({ width, ...measurement })));

    for (const sample of userSamples) {
      const measurement = await measureMessageParity(page, {
        ...sample.params,
        viewportWidth: width,
      });
      rowSummary.push({ kind: "message", width, name: sample.name, ...measurement });
    }

    for (const sample of assistantSamples) {
      const measurement = await measureAssistantParity(page, {
        ...sample.params,
        viewportWidth: width,
      });
      rowSummary.push({ kind: "assistant", width, name: sample.name, ...measurement });
    }
  }

  if (!ENFORCE) return;

  const markdownFailures = markdownSummary.filter((entry) => Math.abs(entry.delta) > MARKDOWN_THRESHOLD_PX);
  expect(
    markdownFailures,
    formatFailures(
      "threshold markdown",
      markdownFailures.map((failure) => ({
        name: failure.name,
        width: failure.width,
        delta: failure.delta,
        planned: failure.planned,
        actual: failure.actual,
      })),
    ),
  ).toEqual([]);

  const rowFailures = rowSummary.filter((entry) => Math.abs(entry.delta) > ROW_THRESHOLD_PX);
  expect(
    rowFailures,
    formatFailures(
      "threshold rows",
      rowFailures.map((failure) => ({
        name: `${failure.kind}:${failure.name}`,
        width: failure.width,
        delta: failure.delta,
        planned: failure.planned,
        actual: failure.actual,
      })),
    ),
  ).toEqual([]);
});

test("workbench: pretext soft-break markdown regressions", async ({ page, browserName }) => {
  test.setTimeout(180000);
  test.slow();
  await openWorkbenchShell(page);

  const summary: MarkdownSummaryEntry[] = [];
  for (const sample of SOFT_BREAK_MARKDOWN_REGRESSIONS.filter((candidate) => {
    return candidate.browsers == null || candidate.browsers.includes(browserName as "chromium" | "webkit");
  })) {
    for (const width of sample.widths ?? [sample.width]) {
      const [measurement] = await measureMarkdownParity(page, [sample], width);
      summary.push({ width, ...measurement });
    }
  }

  if (!ENFORCE) return;

  const failures = summary.filter((entry) => Math.abs(entry.delta) > MARKDOWN_THRESHOLD_PX);
  expect(
    failures,
    formatFailures(
      "soft-break markdown",
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

test("workbench: pretext markdown quote-link-code seam parity", async ({ page }) => {
  test.setTimeout(120000);
  await openWorkbenchShell(page);

  const [measurement] = await measureMarkdownParity(
    page,
    [
      {
        name: "blockquote-link-code-tail",
        markdown:
          "> [summary message summary](https://example.com/inline-code/parity/webkit/parity?ref=781) `cargo test -p ctx-store`:",
      },
    ],
    382.48,
  );

  expect(
    Math.abs(measurement?.delta ?? Number.POSITIVE_INFINITY),
    `markdown drifted by ${measurement?.delta ?? "unknown"}px (planned ${measurement?.planned ?? "?"}, actual ${measurement?.actual ?? "?"})`,
  ).toBeLessThanOrEqual(MARKDOWN_THRESHOLD_PX);
});

test("workbench: pretext sealed inline path threshold parity", async ({ page }) => {
  test.setTimeout(120000);
  await openWorkbenchShell(page);

  const measurements = await measureMarkdownParity(
    page,
    [
      {
        name: "sealed-inline-path-threshold",
        markdown: "`apps/e2e/e2e/core/web/pretextVirtualizerRowLayout.ts`",
      },
    ],
    150,
  );

  if (!ENFORCE) return;

  const failures = measurements.filter((entry) => Math.abs(entry.delta) > MARKDOWN_THRESHOLD_PX);
  expect(
    failures,
    formatFailures(
      "sealed inline path threshold markdown",
      failures.map((failure) => ({
        name: failure.name,
        width: 150,
        delta: failure.delta,
        planned: failure.planned,
        actual: failure.actual,
      })),
    ),
  ).toEqual([]);
});

test("workbench: pretext mixed inline path continuation parity", async ({ page }) => {
  test.setTimeout(120000);
  await openWorkbenchShell(page);

  const cases = [
    {
      width: 440,
      samples: [
        {
          name: "path-after-prose-first-slice",
          markdown:
            "Agent header entry marker *header* stream summary layout entry summary 🙂 測試 佈局 `core/e2e/pretextVirtualizerRowLayout.ts/sessionThreadDomMeasurement.tsx/apps/turn-header` cargo test -p ctx-http.",
        },
      ],
    },
    {
      width: 416,
      samples: [
        {
          name: "path-tail-final-fragment-after-decorated-prose",
          markdown:
            "Probe layout browser `turn-header/sessionMarkdownMeasurement.ts/pages/inline-code/turn-header` context command virtualizer fragment 🧪 測試 佈局 ~~virtualizer~~. `blockquote/fixtures/sessionThread/e2e`",
        },
      ],
    },
    {
      width: 382.48,
      samples: [
        {
          name: "punctuation-seam-command-continuation",
          markdown:
            "Buffer session summary ~~turn~~ ⚙️ 你好 世界 `core/blockquote/sessionThreadDomMeasurement.tsx/sessionThreadDomMeasurement.tsx` ~~command~~ ~~command command~~ **context**: `workbenchShell/fixtures/turn-header/table` Stream browser agent ⚙️ 測試 佈局 header deterministic thread summary buffer entry context ⚙️ 你好 世界 `web/fixtures/core/fixtures`. `pnpm -C core/apps/web test:e2e:pretext:corpus:webkit`",
        },
        {
          name: "path-after-short-prose",
          markdown:
            "Entry command command `turn-header/workbenchShell/blockquote/workbenchShell/src/pages/core`",
        },
        {
          name: "path-tail-continuation",
          markdown: "composer `sessionThread/src/apps/web/inline-code/pretextVirtualizerRowLayout.ts`",
        },
      ],
    },
    {
      width: 121.3333333333,
      samples: [
        {
          name: "narrow-continuation-full-chrome",
          markdown: "`table/src/blockquote/workbenchShell`",
        },
      ],
    },
    {
      width: 181.3333333333,
      samples: [
        {
          name: "table-cell-terminal-path-tail-wrap",
          markdown: "`table/e2e/workbenchShell/apps/web/sessionThread`",
        },
        {
          name: "webkit-sealed-path-cell-width",
          markdown: "`apps/e2e/e2e/core/web/pretextVirtualizerRowLayout.ts`",
        },
      ],
    },
  ] as const;

  const failures: Array<{
    actual: number;
    delta: number;
    name: string;
    planned: number;
    width: number;
  }> = [];

  for (const entry of cases) {
    const measurements = await measureMarkdownParity(page, entry.samples, entry.width);
    failures.push(
      ...measurements
        .filter((sample) => Math.abs(sample.delta) > MARKDOWN_THRESHOLD_PX)
        .map((sample) => ({
          name: sample.name,
          width: entry.width,
          delta: sample.delta,
          planned: sample.planned,
          actual: sample.actual,
        })),
    );
  }

  if (!ENFORCE) return;

  expect(
    failures,
    formatFailures("mixed inline path continuation markdown", failures),
  ).toEqual([]);
});

test("workbench: pretext transcript row corpus parity sweep", async ({ page }, testInfo) => {
  test.setTimeout(180000);
  test.slow();
  await openWorkbenchShell(page);

  const summary: RowSummaryEntry[] = [];

  for (const width of WIDTHS) {
    for (const sample of USER_MESSAGE_CORPUS) {
      const measurement = await measureMessageParity(page, {
        ...sample.params,
        viewportWidth: width,
      });
      summary.push({ kind: "message", width, name: sample.name, ...measurement });
    }

    for (const sample of ASSISTANT_CORPUS) {
      const measurement = await measureAssistantParity(page, {
        ...sample.params,
        viewportWidth: width,
      });
      summary.push({ kind: "assistant", width, name: sample.name, ...measurement });
    }

    for (const sample of TURN_HEADER_CORPUS) {
      const measurement = await measureTurnHeaderParity(page, {
        ...sample.params,
        viewportWidth: width,
      });
      summary.push({ kind: "turn_header", width, name: sample.name, ...measurement });
    }
  }

  const report = {
    enforce: ENFORCE,
    thresholdPx: ROW_THRESHOLD_PX,
    widths: WIDTHS,
    samples: summary,
  };
  const reportPath = testInfo.outputPath("pretext-transcript-row-corpus-parity.json");
  await fs.writeFile(reportPath, JSON.stringify(report, null, 2), "utf8");
  await testInfo.attach("pretext-transcript-row-corpus-parity.json", {
    path: reportPath,
    contentType: "application/json",
  });

  if (!ENFORCE) return;

  const failures = summary.filter((entry) => Math.abs(entry.delta) > ROW_THRESHOLD_PX);
  expect(
    failures,
    formatFailures(
      "transcript rows",
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
