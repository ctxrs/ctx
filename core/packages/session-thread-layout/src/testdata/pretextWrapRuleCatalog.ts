export type PretextWrapRuleBrowser = "chromium" | "webkit";

export type PretextWrapRuleCategory =
  | "white_space"
  | "unicode_boundary"
  | "token_class"
  | "inline_seam"
  | "container_context";

export type PretextWrapRuleEvidence =
  | "spec_clear"
  | "browser_agree"
  | "browser_diverge"
  | "repo_composition";

export type PretextWrapRuleSource = {
  label: string;
  url: string;
  localPath?: string;
};

export type PretextWrapRuleEntry = {
  id: string;
  title: string;
  category: PretextWrapRuleCategory;
  family: string;
  evidence: PretextWrapRuleEvidence;
  summary: string;
  markdown: string;
  markdownWidths: readonly number[];
  assistantViewportWidths: readonly number[];
  browsers: readonly PretextWrapRuleBrowser[];
  planners: readonly string[];
  unitCoverage: readonly string[];
  e2eCoverage: readonly string[];
  fuzzCoverage: readonly string[];
  sources: readonly PretextWrapRuleSource[];
  regressionTaskIds?: readonly string[];
};

const COMMON_BROWSERS = ["chromium", "webkit"] as const;

export const PRETEXT_WRAP_RULE_CATALOG: readonly PretextWrapRuleEntry[] = [
  {
    id: "ws-collapse-normal",
    title: "Collapsed whitespace in normal inline prose",
    category: "white_space",
    family: "collapsed_space",
    evidence: "spec_clear",
    summary:
      "Body markdown uses normal whitespace collapsing. Multiple ordinary spaces should measure like one collapsed seam space, not like preserved preformatted content.",
    markdown: "alpha   beta",
    markdownWidths: [120, 220],
    assistantViewportWidths: [1280],
    browsers: COMMON_BROWSERS,
    planners: ["sessionPlainTextMeasurement.ts", "sessionMarkdownInlineLayout.ts"],
    unitCoverage: ["sessionMarkdownWrapRules.test.ts"],
    e2eCoverage: ["workbench-pretext-wrap-rules.spec.ts"],
    fuzzCoverage: ["pretextWrapRuleFuzz.ts"],
    sources: [
      {
        label: "CSS Text 3 white-space property",
        url: "https://www.w3.org/TR/css-text-3/#white-space-property",
        localPath: "/tmp/pretext-wrap-rules/specs/css-text-3.html",
      },
      {
        label: "WPT css-text white-space",
        url: "https://github.com/web-platform-tests/wpt/tree/master/css/css-text/white-space",
        localPath: "/tmp/pretext-wrap-rules/wpt/css/css-text/white-space",
      },
    ],
  },
  {
    id: "hard-break-forced-line",
    title: "Markdown hard breaks force a new line",
    category: "white_space",
    family: "hard_break",
    evidence: "spec_clear",
    summary:
      "Markdown hard breaks must behave like forced line breaks inside otherwise normal inline content, including when inline code appears on either side.",
    markdown: "First line with `ctx run --mode strict`  \nsecond line with prose after the forced break.",
    markdownWidths: [260, 420],
    assistantViewportWidths: [1280],
    browsers: COMMON_BROWSERS,
    planners: ["sessionMarkdownContract.ts", "sessionMarkdownInlineMeasurement.ts"],
    unitCoverage: ["pretextVirtualizerRowLayout.test.ts", "sessionMarkdownWrapRules.test.ts"],
    e2eCoverage: ["workbench-pretext-wrap-rules.spec.ts"],
    fuzzCoverage: ["pretextWrapRuleFuzz.ts"],
    sources: [
      {
        label: "CSS Text 3 segment break transformation",
        url: "https://www.w3.org/TR/css-text-3/#line-break-transform",
        localPath: "/tmp/pretext-wrap-rules/specs/css-text-3.html",
      },
      {
        label: "WPT css-text white-space",
        url: "https://github.com/web-platform-tests/wpt/tree/master/css/css-text/white-space",
        localPath: "/tmp/pretext-wrap-rules/wpt/css/css-text/white-space",
      },
    ],
  },
  {
    id: "grapheme-cluster-preservation",
    title: "Emergency wrapping must preserve grapheme clusters",
    category: "unicode_boundary",
    family: "grapheme_cluster",
    evidence: "spec_clear",
    summary:
      "Emergency wrapping may break long tokens, but not inside a grapheme cluster or emoji ZWJ sequence. The planner must match browser cluster boundaries.",
    markdown:
      "cluster 👨‍👩‍👧‍👦👨‍👩‍👧‍👦👨‍👩‍👧‍👦 with `core/apps/web/src/pages/sessionThread/sessionThreadDomMeasurement.tsx` tail prose",
    markdownWidths: [140, 220],
    assistantViewportWidths: [1240],
    browsers: COMMON_BROWSERS,
    planners: ["sessionTextMeasurement.ts", "sessionMarkdownMeasurementCore.ts"],
    unitCoverage: ["sessionMarkdownWrapRules.test.ts"],
    e2eCoverage: ["workbench-pretext-wrap-rules.spec.ts"],
    fuzzCoverage: ["pretextWrapRuleFuzz.ts"],
    sources: [
      {
        label: "Unicode UAX #29 grapheme cluster boundaries",
        url: "https://unicode.org/reports/tr29/#Grapheme_Cluster_Boundaries",
        localPath: "/tmp/pretext-wrap-rules/unicode/uax29.html",
      },
      {
        label: "Unicode GraphemeBreakTest.txt",
        url: "https://www.unicode.org/Public/16.0.0/ucd/auxiliary/GraphemeBreakTest.txt",
        localPath: "/tmp/pretext-wrap-rules/unicode/ucd-16.0.0/auxiliary/GraphemeBreakTest.txt",
      },
      {
        label: "WPT css-text overflow-wrap",
        url: "https://github.com/web-platform-tests/wpt/tree/master/css/css-text/overflow-wrap",
        localPath: "/tmp/pretext-wrap-rules/wpt/css/css-text/overflow-wrap",
      },
    ],
  },
  {
    id: "overflow-wrap-long-inline-token",
    title: "Long inline tokens use emergency wrap rules",
    category: "token_class",
    family: "long_token",
    evidence: "spec_clear",
    summary:
      "Long tokens that exceed the available width must fall back to emergency breaking behavior while still honoring grapheme boundaries and inline code chrome.",
    markdown:
      "Paragraph with `alpha-beta-gamma-delta/ctx/path/one/with/a/very/long/suffix` and prose after the wrap threshold.",
    markdownWidths: [180, 260, 382, 788],
    assistantViewportWidths: [1280],
    browsers: COMMON_BROWSERS,
    planners: ["sessionMarkdownInlineCodeFit.ts", "sessionMarkdownInlineMeasurement.ts"],
    unitCoverage: ["pretextVirtualizerRowLayout.test.ts"],
    e2eCoverage: ["workbench-pretext-wrap-rules.spec.ts", "workbench-pretext-parity-corpus.spec.ts"],
    fuzzCoverage: ["pretextWrapRuleFuzz.ts", "pretextParityFuzz.ts"],
    sources: [
      {
        label: "CSS Text 3 overflow-wrap property",
        url: "https://www.w3.org/TR/css-text-3/#overflow-wrap-property",
        localPath: "/tmp/pretext-wrap-rules/specs/css-text-3.html",
      },
      {
        label: "WPT css-text overflow-wrap",
        url: "https://github.com/web-platform-tests/wpt/tree/master/css/css-text/overflow-wrap",
        localPath: "/tmp/pretext-wrap-rules/wpt/css/css-text/overflow-wrap",
      },
    ],
  },
  {
    id: "inline-code-prose-tail-whitespace",
    title: "Whitespace-separated inline code to prose tail seam",
    category: "inline_seam",
    family: "inline_code_tail",
    evidence: "browser_agree",
    summary:
      "Ordinary trailing prose after non-path-like inline code should not lose synthetic current-line width. The browser treats the whitespace seam as normal prose flow.",
    markdown:
      "The `release updater web e2e` lane was recently hanging because the desktop harness fixtures were returning fake auth tokens and letting real update-check behavior leak through. I fixed the immediate bug, but that exposed test-fixture fragility.",
    markdownWidths: [764, 788],
    assistantViewportWidths: [1380],
    browsers: COMMON_BROWSERS,
    planners: ["sessionMarkdownInlineMeasurementContext.ts", "sessionMarkdownInlineMeasurementTextPlacement.ts"],
    unitCoverage: ["sessionMarkdownMeasurement.debug.test.ts"],
    e2eCoverage: ["workbench-pretext-wrap-rules.spec.ts", "workbench-pretext-parity-corpus.spec.ts"],
    fuzzCoverage: ["pretextWrapRuleFuzz.ts", "pretextParityFuzz.ts"],
    sources: [
      {
        label: "CSS Inline 3 line boxes",
        url: "https://drafts.csswg.org/css-inline-3/#line-box",
        localPath: "/tmp/pretext-wrap-rules/specs/css-inline-3.html",
      },
      {
        label: "WPT css-inline model",
        url: "https://github.com/web-platform-tests/wpt/tree/master/css/css-inline/model",
        localPath: "/tmp/pretext-wrap-rules/wpt/css/css-inline/model",
      },
    ],
    regressionTaskIds: ["abf3e350-5e27-4ad3-aa55-4c9c7e4f69c6"],
  },
  {
    id: "continued-inline-code-tail-chrome",
    title: "Continued inline-code line keeps continuation chrome before prose",
    category: "inline_seam",
    family: "inline_code_tail",
    evidence: "browser_agree",
    summary:
      "When a wrapped inline-code group continues at the start of a new line, the following prose tail should not reclaim the same-line tail seam allowance. The continuation still occupies visible chip chrome before prose flow resumes.",
    markdown:
      "- Turn parity `blockquote/sessionMarkdownMeasurement.ts/sessionMarkdownMeasurement.ts/workbenchShell/table` 🙂 測試 佈局 probe delta​epsilon​zeta summary deterministic A B turn [virtualizer probe](https://example.com/inline-code/chromium/transcript?ref=993) בדיקת עיטוף with code pressure.",
    markdownWidths: [788],
    assistantViewportWidths: [1380],
    browsers: ["chromium"],
    planners: ["sessionMarkdownInlineMeasurementTextPlacement.ts", "sessionMarkdownInlineCodeFit.ts"],
    unitCoverage: ["sessionMarkdownInlineMeasurementTextPlacement.test.ts"],
    e2eCoverage: ["workbench-pretext-wrap-rules.spec.ts", "workbench-pretext-parity-corpus.spec.ts"],
    fuzzCoverage: ["pretextWrapRuleFuzz.ts", "pretextParityFuzz.ts"],
    sources: [
      {
        label: "CSS Inline 3 line boxes",
        url: "https://drafts.csswg.org/css-inline-3/#line-box",
        localPath: "/tmp/pretext-wrap-rules/specs/css-inline-3.html",
      },
      {
        label: "WPT css-inline model",
        url: "https://github.com/web-platform-tests/wpt/tree/master/css/css-inline/model",
        localPath: "/tmp/pretext-wrap-rules/wpt/css/css-inline/model",
      },
    ],
    regressionTaskIds: ["366fdd5f-6dca-4dba-b1a7-c63f530e156d"],
  },
  {
    id: "inline-code-dotted-call-continuation",
    title: "Dotted call continuation stays whole at the wrap seam",
    category: "inline_seam",
    family: "dotted_call",
    evidence: "browser_agree",
    summary:
      "A dotted method-call continuation like `disconnect()` should move whole to the next line when the leading dotted fragment has already consumed the margin.",
    markdown:
      "The root cause was in main.rs: every `CloseRequested` event called the single app-wide `ConnectionManager.disconnect()`. That tore down the shared local daemon or SSH tunnel for all windows, so the remaining window recovered against a restarted/disconnected daemon and active turns got reconciled as interrupted, which is why conversations looked “paused”.",
    markdownWidths: [788],
    assistantViewportWidths: [1380],
    browsers: COMMON_BROWSERS,
    planners: ["sessionMarkdownInlineCodeFit.ts", "sessionMarkdownInlineMeasurement.ts"],
    unitCoverage: ["sessionMarkdownMeasurement.debug.test.ts", "pretextVirtualizerRowLayout.test.ts"],
    e2eCoverage: ["workbench-pretext-wrap-rules.spec.ts", "workbench-pretext-parity-corpus.spec.ts"],
    fuzzCoverage: ["pretextWrapRuleFuzz.ts", "pretextParityFuzz.ts"],
    sources: [
      {
        label: "CSS Inline 3 line boxes",
        url: "https://drafts.csswg.org/css-inline-3/#line-box",
        localPath: "/tmp/pretext-wrap-rules/specs/css-inline-3.html",
      },
      {
        label: "WPT css-inline model",
        url: "https://github.com/web-platform-tests/wpt/tree/master/css/css-inline/model",
        localPath: "/tmp/pretext-wrap-rules/wpt/css/css-inline/model",
      },
    ],
    regressionTaskIds: ["71817369-37a0-4d98-86f1-7c1a8cb118fb"],
  },
  {
    id: "inline-code-dotted-call-continuation-spare-width",
    title: "Strict engines need spare width after a dotted inline-code fragment",
    category: "inline_seam",
    family: "dotted_call",
    evidence: "browser_diverge",
    summary:
      "WebKit can move a continuation like `tasksById` to the next line after a prior `workspaceSnapshot.` chip even when the continuation's measured body width barely fits.",
    markdown:
      "- That effect also depends on `workspaceSnapshot.tasksById`, so active snapshot churn can retrigger prefetch repeatedly.",
    markdownWidths: [423, 426, 427],
    assistantViewportWidths: [],
    browsers: ["webkit"],
    planners: ["sessionMarkdownInlineCodeFit.ts", "sessionMarkdownInlineMeasurementCodePlacement.ts"],
    unitCoverage: ["pretextVirtualizerRowLayout.test.ts"],
    e2eCoverage: ["workbench-pretext-wrap-rules.spec.ts", "workbench-pretext-parity-corpus.spec.ts"],
    fuzzCoverage: ["pretextWrapRuleFuzz.ts", "pretextParityFuzz.ts"],
    sources: [
      {
        label: "CSS Inline 3 inline formatting context",
        url: "https://drafts.csswg.org/css-inline-3/#intro",
        localPath: "/tmp/pretext-wrap-rules/specs/css-inline-3.html",
      },
      {
        label: "WPT css-inline model",
        url: "https://github.com/web-platform-tests/wpt/tree/master/css/css-inline/model",
        localPath: "/tmp/pretext-wrap-rules/wpt/css/css-inline/model",
      },
    ],
    regressionTaskIds: ["8369651e-97fc-4de6-b142-90f4e3cca38b"],
  },
  {
    id: "slash-delimited-prose-token",
    title: "Slash-delimited prose token moves whole when it fits fresh",
    category: "token_class",
    family: "slash_prose",
    evidence: "browser_agree",
    summary:
      "Ordinary slash-delimited prose like `containerized/sandboxed` is not a URL or absolute path. The planner must not split it more aggressively than the browser at threshold widths.",
    markdown:
      "I’ve got a second class now: several of the failures in `new-workspace 9` are plain network-denied installs from a containerized/sandboxed path, not just bad metadata. I’m checking whether those attempts were targeting the sandbox runtime rather than host, because that would explain the `pip` `[Errno 101] Network is unreachable` pattern across multiple providers.",
    markdownWidths: [790],
    assistantViewportWidths: [1380],
    browsers: COMMON_BROWSERS,
    planners: ["sessionMarkdownInlineLayout.ts", "sessionMarkdownInlineMeasurement.ts"],
    unitCoverage: ["sessionMarkdownInlineLayout.test.ts"],
    e2eCoverage: ["workbench-pretext-wrap-rules.spec.ts", "workbench-pretext-parity-corpus.spec.ts"],
    fuzzCoverage: ["pretextWrapRuleFuzz.ts", "pretextParityFuzz.ts"],
    sources: [
      {
        label: "CSS Text 3 line-breaking rules",
        url: "https://www.w3.org/TR/css-text-3/#line-breaking",
        localPath: "/tmp/pretext-wrap-rules/specs/css-text-3.html",
      },
      {
        label: "Unicode UAX #14 line breaking algorithm",
        url: "https://unicode.org/reports/tr14/",
        localPath: "/tmp/pretext-wrap-rules/unicode/uax14.html",
      },
      {
        label: "WPT css-text line-breaking",
        url: "https://github.com/web-platform-tests/wpt/tree/master/css/css-text/line-breaking",
        localPath: "/tmp/pretext-wrap-rules/wpt/css/css-text/line-breaking",
      },
    ],
    regressionTaskIds: ["abf3e350-5e27-4ad3-aa55-4c9c7e4f69c6"],
  },
  {
    id: "hyphenated-prose-token-in-tokenized-run",
    title: "Hyphenated prose keeps browser break opportunities after tokenization",
    category: "token_class",
    family: "hyphenated_word",
    evidence: "spec_clear",
    summary:
      "When a prose run has to be tokenized around slash-sensitive words, ordinary hyphenated words still need break opportunities after the hyphen.",
    markdown: [
      "2. **Use active heads for first paint**",
      "   If `active_heads` has a compatible non-empty bounded head, render it immediately as bootstrap/pending-authority. Do not let it overwrite newer authoritative replica state.",
    ].join("\n"),
    markdownWidths: [456, 488, 512],
    assistantViewportWidths: [],
    browsers: ["webkit"],
    planners: ["sessionMarkdownInlineLayout.ts", "sessionMarkdownInlineMeasurement.ts"],
    unitCoverage: ["sessionMarkdownInlineLayout.test.ts"],
    e2eCoverage: ["workbench-pretext-wrap-rules.spec.ts", "workbench-pretext-parity-corpus.spec.ts"],
    fuzzCoverage: ["pretextWrapRuleFuzz.ts", "pretextParityFuzz.ts"],
    sources: [
      {
        label: "CSS Text 3 line-breaking rules",
        url: "https://www.w3.org/TR/css-text-3/#line-breaking",
        localPath: "/tmp/pretext-wrap-rules/specs/css-text-3.html",
      },
      {
        label: "Unicode UAX #14 hyphen line break classes",
        url: "https://unicode.org/reports/tr14/",
        localPath: "/tmp/pretext-wrap-rules/unicode/uax14.html",
      },
      {
        label: "WPT css-text line-breaking",
        url: "https://github.com/web-platform-tests/wpt/tree/master/css/css-text/line-breaking",
        localPath: "/tmp/pretext-wrap-rules/wpt/css/css-text/line-breaking",
      },
    ],
    regressionTaskIds: ["8369651e-97fc-4de6-b142-90f4e3cca38b"],
  },
  {
    id: "styled-seam-collapsed-space",
    title: "Collapsed seam space after styled text still counts at wrap time",
    category: "inline_seam",
    family: "styled_seam",
    evidence: "browser_agree",
    summary:
      "Adjacent styled-to-body prose seams keep a real collapsed inter-word space. The planner must not drop that seam space during wrap-fit decisions.",
    markdown: [
      "**Important caveat**",
      "Do not make the provider package age gate too small unless you also require stronger checks for that fast path:",
    ].join("\n"),
    markdownWidths: [788],
    assistantViewportWidths: [1380],
    browsers: COMMON_BROWSERS,
    planners: ["sessionMarkdownInlineMeasurementContext.ts", "sessionMarkdownInlineLayout.ts"],
    unitCoverage: ["sessionMarkdownInlineLayout.test.ts"],
    e2eCoverage: ["workbench-pretext-wrap-rules.spec.ts", "workbench-pretext-parity-corpus.spec.ts"],
    fuzzCoverage: ["pretextWrapRuleFuzz.ts", "pretextParityFuzz.ts"],
    sources: [
      {
        label: "CSS Inline 3 inline formatting context",
        url: "https://drafts.csswg.org/css-inline-3/#intro",
        localPath: "/tmp/pretext-wrap-rules/specs/css-inline-3.html",
      },
      {
        label: "WPT css-inline model",
        url: "https://github.com/web-platform-tests/wpt/tree/master/css/css-inline/model",
        localPath: "/tmp/pretext-wrap-rules/wpt/css/css-inline/model",
      },
    ],
    regressionTaskIds: ["abf3e350-5e27-4ad3-aa55-4c9c7e4f69c6"],
  },
  {
    id: "table-cell-inline-content",
    title: "Mixed inline table-cell content remains one inline paragraph",
    category: "container_context",
    family: "table_inline",
    evidence: "repo_composition",
    summary:
      "Markdown table cells with inline code followed by prose must normalize into one inline paragraph, not multiple block-height fragments.",
    markdown: [
      "| Use | Provider A monthly | Provider B monthly |",
      "|---|---:|---:|",
      "| 16 vCPU / 64 GB remote box | `large-dedicated`: ~$589 compute + disk | `balanced-dedicated`: ~$153 |",
    ].join("\n"),
    markdownWidths: [540, 788],
    assistantViewportWidths: [1380],
    browsers: COMMON_BROWSERS,
    planners: ["sessionMarkdownContract.ts", "sessionMarkdownBlockMeasurement.ts"],
    unitCoverage: ["sessionMarkdownContract.test.ts"],
    e2eCoverage: ["workbench-pretext-wrap-rules.spec.ts", "workbench-pretext-parity-corpus.spec.ts"],
    fuzzCoverage: ["pretextWrapRuleFuzz.ts", "pretextParityFuzz.ts"],
    sources: [
      {
        label: "GFM tables in our markdown contract",
        url: "https://github.com/remarkjs/remark-gfm",
      },
      {
        label: "WPT css-inline model",
        url: "https://github.com/web-platform-tests/wpt/tree/master/css/css-inline/model",
        localPath: "/tmp/pretext-wrap-rules/wpt/css/css-inline/model",
      },
    ],
    regressionTaskIds: ["366fdd5f-6dca-4dba-b1a7-c63f530e156d"],
  },
  {
    id: "list-item-wrap-inset",
    title: "List marker inset changes available wrap width",
    category: "container_context",
    family: "list_inset",
    evidence: "spec_clear",
    summary:
      "List-item content wraps inside the post-marker content box, not the full paragraph width. Planner width reduction and DOM width must agree.",
    markdown:
      "- outer item with `inline-code-token`\n  - nested item with long prose and `ctx/pretext/pathlike/token`\n  - nested sibling with [docs](https://example.com) and punctuation",
    markdownWidths: [320, 420, 472, 540],
    assistantViewportWidths: [1320],
    browsers: COMMON_BROWSERS,
    planners: ["sessionMarkdownBlockMeasurement.ts", "sessionThreadMeasurementContract.ts"],
    unitCoverage: ["pretextVirtualizerRowLayout.test.ts"],
    e2eCoverage: ["workbench-pretext-wrap-rules.spec.ts", "workbench-pretext-parity-corpus.spec.ts"],
    fuzzCoverage: ["pretextWrapRuleFuzz.ts", "pretextParityFuzz.ts"],
    sources: [
      {
        label: "CSS Inline 3 line boxes",
        url: "https://drafts.csswg.org/css-inline-3/#line-box",
        localPath: "/tmp/pretext-wrap-rules/specs/css-inline-3.html",
      },
      {
        label: "WPT css-inline model",
        url: "https://github.com/web-platform-tests/wpt/tree/master/css/css-inline/model",
        localPath: "/tmp/pretext-wrap-rules/wpt/css/css-inline/model",
      },
    ],
  },
  {
    id: "pathlike-inline-code-fresh-line-start",
    title: "Path-like inline code can prefer a fresh-line start",
    category: "inline_seam",
    family: "pathlike_code_start",
    evidence: "browser_agree",
    summary:
      "Long path-like inline code near a threshold can fit better by moving whole to a fresh line rather than partially hanging on the current line.",
    markdown:
      "Command fragment [stream layout](https://example.com/docs/parity/webkit?ref=298) *agent virtualizer* ~~inline~~ `pages/fixtures/sessionMarkdownMeasurement.ts/sessionMarkdownMeasurement.ts/blockquote/inline-code/e2e` render parity composer probe render header render summary;",
    markdownWidths: [564, 588, 620],
    assistantViewportWidths: [1380],
    browsers: COMMON_BROWSERS,
    planners: ["sessionMarkdownInlineCodeFit.ts", "sessionMarkdownInlineMeasurement.ts"],
    unitCoverage: ["pretextVirtualizerRowLayout.test.ts"],
    e2eCoverage: ["workbench-pretext-wrap-rules.spec.ts", "workbench-pretext-parity-corpus.spec.ts"],
    fuzzCoverage: ["pretextWrapRuleFuzz.ts", "pretextParityFuzz.ts"],
    sources: [
      {
        label: "CSS Text 3 overflow-wrap property",
        url: "https://www.w3.org/TR/css-text-3/#overflow-wrap-property",
        localPath: "/tmp/pretext-wrap-rules/specs/css-text-3.html",
      },
      {
        label: "WPT css-text overflow-wrap",
        url: "https://github.com/web-platform-tests/wpt/tree/master/css/css-text/overflow-wrap",
        localPath: "/tmp/pretext-wrap-rules/wpt/css/css-text/overflow-wrap",
      },
    ],
  },
  {
    id: "attached-trailing-plain-after-pathlike-code",
    title: "Attached trailing plain text after path-like code keeps the browser seam",
    category: "inline_seam",
    family: "pathlike_attached_tail",
    evidence: "browser_agree",
    summary:
      "When path-like inline code is followed by attached trailing plain text, the planner must honor the same seam/continuation behavior the browser uses at the line edge.",
    markdown:
      "- Agent token marker `src/src/pretextVirtualizerRowLayout.ts/core/blockquote` *padding entry* **marker stream**:",
    markdownWidths: [620],
    assistantViewportWidths: [1320],
    browsers: COMMON_BROWSERS,
    planners: ["sessionMarkdownInlineCodeFit.ts", "sessionMarkdownInlineMeasurementTextPlacement.ts"],
    unitCoverage: ["pretextVirtualizerRowLayout.test.ts"],
    e2eCoverage: ["workbench-pretext-wrap-rules.spec.ts", "workbench-pretext-parity-corpus.spec.ts"],
    fuzzCoverage: ["pretextWrapRuleFuzz.ts", "pretextParityFuzz.ts"],
    sources: [
      {
        label: "CSS Inline 3 inline formatting context",
        url: "https://drafts.csswg.org/css-inline-3/#intro",
        localPath: "/tmp/pretext-wrap-rules/specs/css-inline-3.html",
      },
      {
        label: "WPT css-inline model",
        url: "https://github.com/web-platform-tests/wpt/tree/master/css/css-inline/model",
        localPath: "/tmp/pretext-wrap-rules/wpt/css/css-inline/model",
      },
    ],
  },
  {
    id: "ordered-list-inline-code-chip-threshold",
    title: "Ordered-list inline-code chips include cloned decoration at threshold widths",
    category: "inline_seam",
    family: "ordered_fixture_list",
    evidence: "browser_agree",
    summary:
      "Synthetic recommendation lists often contain standalone inline-code chips and indented prose. Wrapped inline-code fragments use cloned decoration, so the planner must include the visible chip edge when deciding if the prose tail still fits.",
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
    ].join("\n"),
    markdownWidths: [384, 408, 464, 488, 500, 504, 728],
    assistantViewportWidths: [1240, 1380],
    browsers: COMMON_BROWSERS,
    planners: ["sessionMarkdownInlineMeasurementContext.ts", "sessionMarkdownInlineMeasurementTextPlacement.ts"],
    unitCoverage: ["pretextVirtualizerRowLayout.test.ts"],
    e2eCoverage: ["workbench-pretext-wrap-rules.spec.ts", "workbench-pretext-parity-corpus.spec.ts"],
    fuzzCoverage: ["pretextWrapRuleFuzz.ts", "pretextParityFuzz.ts"],
    sources: [
      {
        label: "CSS Inline 3 line boxes",
        url: "https://drafts.csswg.org/css-inline-3/#line-box",
        localPath: "/tmp/pretext-wrap-rules/specs/css-inline-3.html",
      },
      {
        label: "CSS Fragmentation 3 box-decoration-break",
        url: "https://www.w3.org/TR/css-break-3/#break-decoration",
      },
      {
        label: "WPT css-inline model",
        url: "https://github.com/web-platform-tests/wpt/tree/master/css/css-inline/model",
        localPath: "/tmp/pretext-wrap-rules/wpt/css/css-inline/model",
      },
    ],
    regressionTaskIds: ["55aaa419-4bb6-4248-b5b0-4813044dbb5f"],
  },
] as const;

export function getPretextWrapRuleById(ruleId: string): PretextWrapRuleEntry {
  const rule = PRETEXT_WRAP_RULE_CATALOG.find((candidate) => candidate.id === ruleId);
  if (rule == null) {
    throw new Error(`Unknown pretext wrap rule: ${ruleId}`);
  }
  return rule;
}

export function listPretextWrapRuleIds(): readonly string[] {
  return PRETEXT_WRAP_RULE_CATALOG.map((rule) => rule.id);
}
