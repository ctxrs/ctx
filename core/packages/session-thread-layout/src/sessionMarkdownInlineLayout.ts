import type { PreparedTextWithSegments } from "@chenglou/pretext";
import { isSealedInlineCodeFragment, splitInlineCodeFragments } from "./inlineCodeFragments";
import {
  measureInlineCodeMinStartTextWidth,
  pushInlineCodeWhitespaceItems,
  pushTextRunItems,
  resolveInlineCodeFont,
  splitTrailingPlainPathTailFragment,
} from "./sessionMarkdownInlineLayoutItems";
import type { SessionMarkdownInlineRun } from "./sessionMarkdownContract";
import {
  buildPreparedContentKey,
  getPreparedTextWithSegments,
  measureCollapsedSpaceWidth,
  measureSingleLineLayout,
  resolveTextRunFont,
  type TextBlockTypography,
} from "./sessionMarkdownMeasurementCore";
import {
  SESSION_THREAD_MARKDOWN_INLINE_CODE_FRAGMENT_CHROME_WIDTH_PX,
} from "./sessionThreadLayoutTokens";
import { isPathLikeOrDottedText } from "./sessionTextTokenClassifier";

const INLINE_CODE_MIN_START_GRAPHEMES = 4;
const INLINE_CODE_PATH_MIN_START_GRAPHEMES = 3;

export type InlineWrapMode = "normal" | "break-word" | "anywhere";

export type PreparedInlineLayoutItem =
  | { kind: "hardBreak" }
  | { kind: "space"; width: number; codeGroupId: number | null; text: string }
  | {
      kind: "segment";
      allowsBreakWord: boolean;
      codeGroupId: number | null;
      codeGroupHasDottedPath: boolean;
      codeGroupHasWhitespace: boolean;
      codeGroupHasTrailingText: boolean;
      codeGroupIsOnlyInlineCodeInSegment: boolean;
      codeGroupStartsAfterText: boolean;
      codeGroupStartsAfterStyledTextSeam: boolean;
      codePartStartsAfterWhitespace: boolean;
      chromeWidth: number;
      endCursor: { segmentIndex: number; graphemeIndex: number };
      fullWidth: number;
      isFirstCodeGroupFragment: boolean;
      startsAfterCodeWhitespace: boolean;
      isFirstPathFragmentAfterHyphenRun: boolean;
      isPathTailFragment: boolean;
      isSealedInlineCodeFragment: boolean;
      minStartTextWidth: number;
      prefersFreshLineStart: boolean;
      prefersFreshLineStartWithoutLeadingHang: boolean;
      startsAfterInlineCodeSeam: boolean;
      startsAfterCollapsedSoftBreak: boolean;
      startsAfterPathLikeInlineCodeSeam: boolean;
      startsStyledTextAfterInlineCodeSeam: boolean;
      startsAfterStyledTextSeam: boolean;
      startsStyledTextAfterBodySeam: boolean;
      font: string;
      hasTrailingStyledText: boolean;
      hasTrailingInlineCode: boolean;
      isDecoratedText: boolean;
      prepared: PreparedTextWithSegments;
      text: string;
    };

export function prepareInlineLayoutItems(params: {
  runs: readonly SessionMarkdownInlineRun[];
  typography: TextBlockTypography;
  cacheKeyPrefix: string;
  wrapMode?: InlineWrapMode;
}): PreparedInlineLayoutItem[] {
  const items: PreparedInlineLayoutItem[] = [];
  const inlineCodeFont = resolveInlineCodeFont(params.typography.body);
  const runHasRenderableText = (run: SessionMarkdownInlineRun): boolean =>
    run.kind === "text" && /\S/.test(run.text);
  const runUsesStyledSeam = (
    run: Extract<SessionMarkdownInlineRun, { kind: "text" }>,
  ): boolean => run.style !== "body" || run.deleted;
  const textRunStartsAfterStyledTextSeam = (
    runIndex: number,
    run: Extract<SessionMarkdownInlineRun, { kind: "text" }>,
  ): boolean => {
    if (runUsesStyledSeam(run) || !runHasRenderableText(run)) {
      return false;
    }
    for (let candidateIndex = runIndex - 1; candidateIndex >= 0; candidateIndex -= 1) {
      const candidate = params.runs[candidateIndex]!;
      if (candidate.kind === "hardBreak") {
        break;
      }
      if (candidate.kind === "inlineCode") {
        return false;
      }
      if (!runHasRenderableText(candidate)) {
        continue;
      }
      return runUsesStyledSeam(candidate);
    }
    return false;
  };
  const textRunStartsStyledTextAfterBodySeam = (
    runIndex: number,
    run: Extract<SessionMarkdownInlineRun, { kind: "text" }>,
  ): boolean => {
    if (!runUsesStyledSeam(run) || !runHasRenderableText(run)) {
      return false;
    }
    for (let candidateIndex = runIndex - 1; candidateIndex >= 0; candidateIndex -= 1) {
      const candidate = params.runs[candidateIndex]!;
      if (candidate.kind === "hardBreak") {
        break;
      }
      if (candidate.kind === "inlineCode") {
        return false;
      }
      if (!runHasRenderableText(candidate)) {
        continue;
      }
      return !runUsesStyledSeam(candidate);
    }
    return false;
  };
  const textRunStartsStyledTextAfterInlineCodeSeam = (
    runIndex: number,
    run: Extract<SessionMarkdownInlineRun, { kind: "text" }>,
  ): boolean => {
    if (!runUsesStyledSeam(run) || !runHasRenderableText(run)) {
      return false;
    }
    for (let candidateIndex = runIndex - 1; candidateIndex >= 0; candidateIndex -= 1) {
      const candidate = params.runs[candidateIndex]!;
      if (candidate.kind === "hardBreak") {
        break;
      }
      if (candidate.kind === "inlineCode") {
        return true;
      }
      if (runHasRenderableText(candidate)) {
        return false;
      }
    }
    return false;
  };
  const textRunStartsAfterInlineCodeSeam = (
    runIndex: number,
    run: Extract<SessionMarkdownInlineRun, { kind: "text" }>,
  ): boolean => {
    if (run.style !== "body" || !runHasRenderableText(run)) {
      return false;
    }
    for (let candidateIndex = runIndex - 1; candidateIndex >= 0; candidateIndex -= 1) {
      const candidate = params.runs[candidateIndex]!;
      if (candidate.kind === "hardBreak") {
        break;
      }
      if (candidate.kind === "inlineCode") {
        return true;
      }
      if (runHasRenderableText(candidate)) {
        return false;
      }
    }
    return false;
  };
  const textRunStartsAfterPathLikeInlineCodeSeam = (
    runIndex: number,
    run: Extract<SessionMarkdownInlineRun, { kind: "text" }>,
  ): boolean => {
    if (!runHasRenderableText(run)) {
      return false;
    }
    for (let candidateIndex = runIndex - 1; candidateIndex >= 0; candidateIndex -= 1) {
      const candidate = params.runs[candidateIndex]!;
      if (candidate.kind === "hardBreak") {
        break;
      }
      if (candidate.kind === "inlineCode") {
        return isPathLikeOrDottedText(candidate.text);
      }
      if (runHasRenderableText(candidate)) {
        return false;
      }
    }
    return false;
  };
  const textRunHasTrailingInlineCode = (runIndex: number): boolean => {
    for (let candidateIndex = runIndex + 1; candidateIndex < params.runs.length; candidateIndex += 1) {
      const candidate = params.runs[candidateIndex]!;
      if (candidate.kind === "hardBreak") {
        return false;
      }
      if (candidate.kind === "inlineCode") {
        return true;
      }
    }
    return false;
  };
  const textRunHasTrailingStyledText = (runIndex: number): boolean => {
    for (let candidateIndex = runIndex + 1; candidateIndex < params.runs.length; candidateIndex += 1) {
      const candidate = params.runs[candidateIndex]!;
      if (candidate.kind === "hardBreak" || candidate.kind === "inlineCode") {
        return false;
      }
      if (!runHasRenderableText(candidate)) {
        continue;
      }
      return runUsesStyledSeam(candidate);
    }
    return false;
  };
  const codeGroupStartsAfterText = (runIndex: number): boolean => {
    for (let candidateIndex = runIndex - 1; candidateIndex >= 0; candidateIndex -= 1) {
      const candidate = params.runs[candidateIndex]!;
      if (candidate.kind === "hardBreak") {
        return false;
      }
      if (runHasRenderableText(candidate)) {
        return true;
      }
    }
    return false;
  };
  const codeGroupStartsAfterStyledTextSeam = (runIndex: number): boolean => {
    for (let candidateIndex = runIndex - 1; candidateIndex >= 0; candidateIndex -= 1) {
      const candidate = params.runs[candidateIndex]!;
      if (candidate.kind === "hardBreak") {
        return false;
      }
      if (candidate.kind === "inlineCode") {
        return false;
      }
      if (!runHasRenderableText(candidate)) {
        continue;
      }
      return runUsesStyledSeam(candidate);
    }
    return false;
  };
  const codeGroupHasTrailingText = (runIndex: number): boolean => {
    for (let candidateIndex = runIndex + 1; candidateIndex < params.runs.length; candidateIndex += 1) {
      const candidate = params.runs[candidateIndex]!;
      if (candidate.kind === "hardBreak") {
        return false;
      }
      if (runHasRenderableText(candidate)) {
        return true;
      }
    }
    return false;
  };
  const codeGroupIsOnlyInlineCodeInSegment = (runIndex: number): boolean => {
    for (let candidateIndex = runIndex - 1; candidateIndex >= 0; candidateIndex -= 1) {
      const candidate = params.runs[candidateIndex]!;
      if (candidate.kind === "hardBreak") {
        break;
      }
      if (candidate.kind === "inlineCode") {
        return false;
      }
    }
    for (let candidateIndex = runIndex + 1; candidateIndex < params.runs.length; candidateIndex += 1) {
      const candidate = params.runs[candidateIndex]!;
      if (candidate.kind === "hardBreak") {
        break;
      }
      if (candidate.kind === "inlineCode") {
        return false;
      }
    }
    return true;
  };

  for (let index = 0; index < params.runs.length; index += 1) {
    const run = params.runs[index]!;
    if (run.kind === "hardBreak") {
      items.push({ kind: "hardBreak" });
      continue;
    }

    if (run.kind === "inlineCode") {
      if (run.text.length === 0) {
        continue;
      }
      const codeGroupId = index;
      const codeGroupHasDottedPath =
        (run.text.includes("/") || run.text.includes("\\")) && run.text.includes(".");
      const codeGroupHasWhitespace = run.parts.some((part) => /\s/.test(part));
      const startsAfterText = codeGroupStartsAfterText(index);
      const startsAfterStyledTextSeam = codeGroupStartsAfterStyledTextSeam(index);
      const hasTrailingText = codeGroupHasTrailingText(index);
      const isOnlyInlineCodeInSegment = codeGroupIsOnlyInlineCodeInSegment(index);
      let firstCodeGroupFragment = true;
      for (let partIndex = 0; partIndex < run.parts.length; partIndex += 1) {
        const part = run.parts[partIndex]!;
        if (part.length === 0) {
          continue;
        }
        if (/^\s+$/.test(part)) {
          pushInlineCodeWhitespaceItems(items, {
            text: part,
            font: inlineCodeFont,
            codeGroupId,
            cacheKeyPrefix: `${params.cacheKeyPrefix}:${run.kind}:${index}:${partIndex}`,
          });
          continue;
        }
        const fragments = splitInlineCodeFragments(part);
        const expandedFragments = fragments.flatMap((fragment, fragmentIndex) => {
          const previous = fragments[fragmentIndex - 1] ?? null;
          const shouldSplitTrailingPlainPathTail =
            hasTrailingText &&
            fragmentIndex === fragments.length - 1 &&
            previous != null &&
            /[\\/]$/.test(previous) &&
            fragment.includes("-") &&
            !fragment.endsWith("-") &&
            !fragment.includes("/") &&
            !fragment.includes("\\");
          return shouldSplitTrailingPlainPathTail
            ? splitTrailingPlainPathTailFragment(fragment)
            : [fragment];
        });
        const startsAfterCodeWhitespace = partIndex > 0 && /^\s+$/.test(run.parts[partIndex - 1] ?? "");
        let sawHyphenFragment = false;
        let sawPathFragment = false;
        for (let fragmentIndex = 0; fragmentIndex < expandedFragments.length; fragmentIndex += 1) {
          const fragment = expandedFragments[fragmentIndex]!;
          const isPathFragment = fragment.includes("/") || fragment.includes("\\");
          const isFirstPathFragmentAfterHyphenRun =
            isPathFragment && sawHyphenFragment && !sawPathFragment;
          const prepared = getPreparedTextWithSegments(
            buildPreparedContentKey(
              `${params.cacheKeyPrefix}:${run.kind}:${index}:${partIndex}:${fragmentIndex}`,
              fragment,
            ),
            fragment,
            inlineCodeFont,
            "pre-wrap",
          );
          const wholeLine = measureSingleLineLayout(prepared);
          if (wholeLine == null) {
            continue;
          }
          items.push({
            kind: "segment",
            allowsBreakWord: false,
            codeGroupId,
            codeGroupHasDottedPath,
            codeGroupHasWhitespace,
            codeGroupHasTrailingText: hasTrailingText,
            codeGroupIsOnlyInlineCodeInSegment: isOnlyInlineCodeInSegment,
            codeGroupStartsAfterText: startsAfterText,
            codeGroupStartsAfterStyledTextSeam: startsAfterStyledTextSeam,
            codePartStartsAfterWhitespace: startsAfterCodeWhitespace,
            chromeWidth: SESSION_THREAD_MARKDOWN_INLINE_CODE_FRAGMENT_CHROME_WIDTH_PX,
            endCursor: wholeLine.end,
            fullWidth: wholeLine.width,
            isFirstCodeGroupFragment: firstCodeGroupFragment,
            startsAfterCodeWhitespace: startsAfterCodeWhitespace && fragmentIndex === 0,
            isFirstPathFragmentAfterHyphenRun,
            isPathTailFragment:
              sawPathFragment &&
              !isPathFragment &&
              !fragment.endsWith(".") &&
              !fragment.endsWith("-"),
            isSealedInlineCodeFragment: isSealedInlineCodeFragment(fragment),
            minStartTextWidth:
              firstCodeGroupFragment
                ? measureInlineCodeMinStartTextWidth(
                    part,
                    inlineCodeFont,
                    part.includes("/") || part.includes("\\") || part.includes(".")
                      ? INLINE_CODE_PATH_MIN_START_GRAPHEMES
                      : INLINE_CODE_MIN_START_GRAPHEMES,
                  )
                : 0,
            prefersFreshLineStart: fragment.includes("/") || fragment.includes("\\") || fragment.endsWith("."),
            prefersFreshLineStartWithoutLeadingHang:
              firstCodeGroupFragment && startsAfterText && codeGroupHasWhitespace,
            startsAfterInlineCodeSeam: false,
            startsAfterCollapsedSoftBreak: false,
            startsAfterPathLikeInlineCodeSeam: false,
            startsStyledTextAfterInlineCodeSeam: false,
            startsAfterStyledTextSeam: false,
            startsStyledTextAfterBodySeam: false,
            font: inlineCodeFont,
            hasTrailingStyledText: false,
            hasTrailingInlineCode: false,
            isDecoratedText: false,
            prepared,
            text: fragment,
          });
          firstCodeGroupFragment = false;
          sawHyphenFragment ||= fragment.endsWith("-");
          sawPathFragment ||= isPathFragment;
        }
      }
      continue;
    }

    const font = resolveTextRunFont(run, params.typography);
    const collapsedSpaceWidth = measureCollapsedSpaceWidth(font);
    pushTextRunItems(items, {
      text: run.text,
      font,
      cacheKeyPrefix: `${params.cacheKeyPrefix}:${run.kind}:${index}`,
      collapsedSpaceWidth,
      wrapMode: params.wrapMode ?? "normal",
      startsAfterInlineCodeSeam: textRunStartsAfterInlineCodeSeam(index, run),
      startsAfterCollapsedSoftBreak: false,
      startsAfterPathLikeInlineCodeSeam: textRunStartsAfterPathLikeInlineCodeSeam(index, run),
      startsStyledTextAfterInlineCodeSeam: textRunStartsStyledTextAfterInlineCodeSeam(index, run),
      startsAfterStyledTextSeam: textRunStartsAfterStyledTextSeam(index, run),
      startsStyledTextAfterBodySeam: textRunStartsStyledTextAfterBodySeam(index, run),
      hasTrailingStyledText: textRunHasTrailingStyledText(index),
      hasTrailingInlineCode: textRunHasTrailingInlineCode(index),
      isDecoratedText: runUsesStyledSeam(run),
    });
  }

  return items;
}
