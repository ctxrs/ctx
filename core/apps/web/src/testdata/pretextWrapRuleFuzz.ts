import {
  PRETEXT_WRAP_RULE_CATALOG,
  type PretextWrapRuleEntry,
} from "./pretextWrapRuleCatalog";

type MarkdownSample = {
  name: string;
  markdown: string;
};

type AssistantParityParams = {
  content: string;
  isComplete?: boolean;
  viewportWidth?: number;
};

class SeededRandom {
  private state: number;

  constructor(seed: number) {
    this.state = seed >>> 0;
  }

  next(): number {
    let state = this.state + 0x6d2b79f5;
    state = Math.imul(state ^ (state >>> 15), state | 1);
    state ^= state + Math.imul(state ^ (state >>> 7), state | 61);
    this.state = state ^ (state >>> 14);
    return (this.state >>> 0) / 4294967296;
  }

  int(min: number, max: number): number {
    return min + Math.floor(this.next() * (max - min + 1));
  }

  pick<T>(values: readonly T[]): T {
    return values[this.int(0, values.length - 1)]!;
  }
}

export type GeneratedWrapRuleMarkdownSample = MarkdownSample & {
  ruleId: string;
  family: string;
  markdownWidths: readonly number[];
};

export type GeneratedWrapRuleAssistantSample = {
  name: string;
  ruleId: string;
  family: string;
  assistantViewportWidths: readonly number[];
  params: AssistantParityParams;
};

export type GeneratedPretextWrapRuleFuzzCorpus = {
  seed: number;
  markdownSamples: readonly GeneratedWrapRuleMarkdownSample[];
  assistantSamples: readonly GeneratedWrapRuleAssistantSample[];
};

const WORDS = [
  "planner",
  "browser",
  "inline",
  "seam",
  "threshold",
  "virtualizer",
  "transcript",
  "wrap",
  "delta",
  "deterministic",
  "parity",
  "render",
  "summary",
  "token",
  "layout",
  "message",
  "context",
  "buffer",
  "line",
  "boundary",
] as const;

const PROSE_TAILS = [
  "before the final browser-width pass lands.",
  "so the transcript no longer drifts at the margin.",
  "while the assistant row still measures deterministically.",
  "without reintroducing DOM correction in production.",
  "and the next width scan stays clean in both engines.",
] as const;

const INLINE_CODE_TAILS = [
  "layout parity sweep",
  "row measurement smoke",
  "snapshot drift guard",
  "preview width gate",
  "wrap rule probe",
] as const;

const LAYOUT_STRESS_LABELS = [
  "layout-case=alpha",
  "layout-pool=synthetic",
  "layout-purpose=<purpose>",
  "layout-owner=fixture",
  "layout-expiry=<epoch>",
  "PUBLIC_FIXTURE_MARKER",
  "fixtures/config/layout.env",
  "tools/layout-fixture cleanup",
  "fixtures/runtime/session-state-<run-id>.json",
  "/tmp/layout-fixture-<run-id>.json",
  "--max-age-hours",
  "--retain",
] as const;

const DOTTED_CALLS = [
  "ConnectionManager.disconnect()",
  "observer.disconnect()",
  "workspaceSnapshot.tasksById",
  "report.finalize()",
  "queue.accept()",
  "runtime.resume()",
] as const;

const SLASH_PROSE_TOKENS = [
  "grouped/ungrouped",
  "header/workspace",
  "grid/flow",
  "panel/security-state",
  "network-state/runtime",
  "bootstrap/render-ready",
  "snapshot/head-bootstrap",
] as const;

const PATHLIKE_TOKENS = [
  "pages/fixtures/sessionMarkdownMeasurement.ts/sessionMarkdownMeasurement.ts/blockquote/inline-code/e2e",
  "src/src/pretextVirtualizerRowLayout.ts/core/blockquote",
  "core/apps/web/src/pages/sessionThread/sessionThreadDomMeasurement.tsx",
  "sessionThread/sessionMarkdownMeasurement.ts/e2e/workbench-pretext-wrap-rules.spec.ts",
  "core/apps/web/e2e/utils/pretextWrapRuleFuzz.ts",
] as const;

const BOLD_HEADS = [
  "Important caveat",
  "Fresh rule",
  "Planner note",
  "Boundary warning",
  "Wrap detail",
] as const;

const TABLE_CODES = ["wide-sample-16", "compact-sample-04", "balanced-row-32", "wrap rule token", "observer.disconnect()"] as const;

const EMOJI_CLUSTERS = ["👨‍👩‍👧‍👦", "👩🏽‍💻", "🏳️‍🌈", "⚙️", "🙂"] as const;

function slugify(value: string): string {
  return value.replace(/[^a-z0-9]+/gi, "-").replace(/^-+|-+$/g, "").toLowerCase();
}

function words(rng: SeededRandom, min: number, max: number): string {
  return Array.from({ length: rng.int(min, max) }, () => rng.pick(WORDS)).join(" ");
}

function makeFamilyMarkdown(rng: SeededRandom, family: string): string {
  switch (family) {
    case "collapsed_space":
      return `${words(rng, 1, 2)}   ${words(rng, 1, 2)} ${rng.pick(PROSE_TAILS)}`;
    case "hard_break":
      return `First line with \`${rng.pick(INLINE_CODE_TAILS)}\`  \nsecond line with ${words(rng, 3, 5)} ${rng.pick(PROSE_TAILS)}`;
    case "grapheme_cluster":
      return `${words(rng, 2, 4)} ${rng.pick(EMOJI_CLUSTERS)}${rng.pick(EMOJI_CLUSTERS)}${rng.pick(EMOJI_CLUSTERS)} with \`${rng.pick(PATHLIKE_TOKENS)}\` ${rng.pick(PROSE_TAILS)}`;
    case "long_token":
      return `Paragraph with \`${rng.pick(PATHLIKE_TOKENS)}/${slugify(words(rng, 2, 4))}/${slugify(words(rng, 2, 4))}\` and ${words(rng, 3, 5)} ${rng.pick(PROSE_TAILS)}`;
    case "inline_code_tail":
      return `The \`${rng.pick(INLINE_CODE_TAILS)}\` lane was recently hanging because ${words(rng, 6, 9)} ${rng.pick(PROSE_TAILS)}`;
    case "dotted_call":
      return `The root cause was that \`${rng.pick(DOTTED_CALLS)}\` crossed the margin while ${words(rng, 5, 8)} ${rng.pick(PROSE_TAILS)}`;
    case "slash_prose":
      return `${words(rng, 4, 6)} from a ${rng.pick(SLASH_PROSE_TOKENS)} path, not just bad metadata, because ${words(rng, 5, 8)} ${rng.pick(PROSE_TAILS)}`;
    case "styled_seam":
      return `**${rng.pick(BOLD_HEADS)}**\n${words(rng, 10, 15)} ${rng.pick(PROSE_TAILS)}`;
    case "table_inline":
      return [
        "| Use | Value | Note |",
        "|---|---|---|",
        `| ${words(rng, 2, 3)} | \`${rng.pick(TABLE_CODES)}\`: ${words(rng, 2, 4)} | ${words(rng, 5, 8)} ${rng.pick(PROSE_TAILS)} |`,
      ].join("\n");
    case "list_inset":
      return `- ${words(rng, 4, 6)} \`${rng.pick(PATHLIKE_TOKENS)}\`\n  - ${words(rng, 5, 8)} ${rng.pick(PROSE_TAILS)}`;
    case "pathlike_code_start":
      return `${words(rng, 4, 6)} \`${rng.pick(PATHLIKE_TOKENS)}\` ${words(rng, 4, 7)} ${rng.pick(PROSE_TAILS)}`;
    case "pathlike_attached_tail":
      return `- ${words(rng, 3, 5)} \`${rng.pick(PATHLIKE_TOKENS)}\` **${words(rng, 1, 2)}**:`;
    case "ordered_fixture_list":
      return [
        `${words(rng, 5, 8)} before implementation:`,
        "",
        `1. **${rng.pick(BOLD_HEADS)}**`,
        `   ${words(rng, 5, 8)} \`${rng.pick(LAYOUT_STRESS_LABELS)}\` or \`${rng.pick(LAYOUT_STRESS_LABELS)}\` ${rng.pick(PROSE_TAILS)}`,
        `   \`${rng.pick(LAYOUT_STRESS_LABELS)}\``,
        `   \`${rng.pick(LAYOUT_STRESS_LABELS)}\``,
        "",
        `2. **${rng.pick(BOLD_HEADS)}**`,
        `   ${words(rng, 7, 10)} \`${rng.pick(LAYOUT_STRESS_LABELS)}\`, ${words(rng, 5, 7)} \`${rng.pick(LAYOUT_STRESS_LABELS)}\`.`,
      ].join("\n");
    default:
      return `${words(rng, 5, 8)} ${rng.pick(PROSE_TAILS)}`;
  }
}

function pickRule(index: number, totalCount: number): PretextWrapRuleEntry {
  const count = Math.max(1, totalCount);
  const catalogIndex =
    count >= PRETEXT_WRAP_RULE_CATALOG.length
      ? index % PRETEXT_WRAP_RULE_CATALOG.length
      : Math.floor((index * PRETEXT_WRAP_RULE_CATALOG.length) / count);
  return PRETEXT_WRAP_RULE_CATALOG[catalogIndex]!;
}

function generateMarkdownSample(
  rng: SeededRandom,
  rule: PretextWrapRuleEntry,
  index: number,
): GeneratedWrapRuleMarkdownSample {
  return {
    name: `wrap-rule-generated-${index}-${rule.id}`,
    ruleId: rule.id,
    family: rule.family,
    markdownWidths: rule.markdownWidths,
    markdown: makeFamilyMarkdown(rng, rule.family),
  };
}

function generateAssistantSample(
  rng: SeededRandom,
  rule: PretextWrapRuleEntry,
  index: number,
): GeneratedWrapRuleAssistantSample {
  const markdownSample = generateMarkdownSample(rng, rule, index);
  return {
    name: `wrap-rule-assistant-${index}-${markdownSample.ruleId}`,
    ruleId: markdownSample.ruleId,
    family: markdownSample.family,
    assistantViewportWidths: rule.assistantViewportWidths,
    params: {
      content: markdownSample.markdown,
      isComplete: true,
    },
  };
}

export function generatePretextWrapRuleFuzzCorpus(options?: {
  seed?: number;
  markdownCount?: number;
  assistantCount?: number;
}): GeneratedPretextWrapRuleFuzzCorpus {
  const seed = options?.seed ?? 20260421;
  const rng = new SeededRandom(seed);
  const markdownCount = options?.markdownCount ?? 8;
  const assistantCount = options?.assistantCount ?? 6;

  return {
    seed,
    markdownSamples: Array.from({ length: markdownCount }, (_, index) => {
      const rule = pickRule(index, markdownCount);
      return generateMarkdownSample(rng, rule, index + 1);
    }),
    assistantSamples: Array.from({ length: assistantCount }, (_, index) => {
      const rule = pickRule(index, assistantCount);
      return generateAssistantSample(rng, rule, index + 1);
    }),
  };
}
