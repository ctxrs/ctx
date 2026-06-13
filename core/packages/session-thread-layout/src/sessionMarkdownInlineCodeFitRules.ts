import { browserAllowsInlineCodeLeadingHang } from "./sessionMarkdownBrowserProfile";
import type { PreparedInlineLayoutItem } from "./sessionMarkdownInlineLayout";

type InlineSegmentItem = Extract<PreparedInlineLayoutItem, { kind: "segment" }>;
export type InlineContinuationSlackItem = Pick<
  InlineSegmentItem,
  | "codeGroupHasDottedPath"
  | "codeGroupHasTrailingText"
  | "codeGroupStartsAfterText"
  | "codeGroupStartsAfterStyledTextSeam"
  | "isPathTailFragment"
  | "isSealedInlineCodeFragment"
  | "text"
>;

export type InlineCodeBoundaryFit = {
  consumedWidth: number;
  endedAtGroupEnd: boolean;
  endedInsideFragment: boolean;
  lastFragmentText: string | null;
  nextFragmentText: string | null;
  nextStartsAfterCodeWhitespace: boolean;
};

export type InlineCodeTrailingPlainInfo = {
  width: number;
  text: string;
  hasFollowingInlineCode: boolean;
  isDecoratedText: boolean;
  startsAfterCollapsedSoftBreak: boolean;
};

export function isShortExtensionPathLikeFragment(text: string | null | undefined): boolean {
  if (typeof text !== "string") {
    return false;
  }
  if (!/[\\/]$/.test(text) || text.includes(".") || /\s/.test(text)) {
    return false;
  }
  return Array.from(text).length <= 16;
}

export const INLINE_CODE_ENGINE_PROSE_START_FOLLOWING_FRAGMENT_SLACK_PX = 16;
const INLINE_CODE_CONTINUATION_FIT_SLACK_PX = 5;
const INLINE_CODE_PATH_TAIL_CONTINUATION_FIT_SLACK_PX = 1;
export const INLINE_CODE_PATH_DELIMITER_CONTINUATION_MIN_SPARE_PX = 1;
export const INLINE_CODE_DOTTED_CALL_CONTINUATION_MIN_SPARE_PX = 4;
const INLINE_CODE_COLON_COMMAND_FRAGMENT_SLACK_PX = 1;
const INLINE_CODE_PROSE_START_SEAM_GUARD_PX = 4;
const INLINE_CODE_STANDALONE_HYPHEN_FRAGMENT_SLACK_PX = 2;
const DOTTED_CALL_SPARE_MIN_STEM_GRAPHEMES = 12;

export function allowsChromiumDottedBoundaryHang(params: {
  boundaryRemainingWidth: number;
  chromeWidth: number;
  fullWidth: number;
}): boolean {
  // Chromium's cloned inline-code decoration can effectively hang one chip edge
  // at a dotted path boundary, but not arbitrary text. Keep the allowance capped
  // to the code chrome so this cannot mask real wrapping pressure.
  return params.fullWidth <= params.boundaryRemainingWidth + params.chromeWidth + 0.01;
}

export function shouldApplyInlineCodeSoftBreakTextStartGuard(params: {
  text: string;
  startsAfterCollapsedSoftBreak: boolean;
  startsAfterPathLikeInlineCodeSeam: boolean;
  startsAfterInlineCodeSeam: boolean;
  startsStyledTextAfterInlineCodeSeam: boolean;
  lastFragmentEndedWithPathDelimiter: boolean;
  lastFragmentEndedWithHyphen: boolean;
}): boolean {
  const startsAtSoftBreakPathSeam =
    params.startsAfterCollapsedSoftBreak &&
    params.startsAfterPathLikeInlineCodeSeam &&
    (params.startsAfterInlineCodeSeam || params.startsStyledTextAfterInlineCodeSeam);
  if (!startsAtSoftBreakPathSeam) {
    return false;
  }
  return params.lastFragmentEndedWithPathDelimiter || params.lastFragmentEndedWithHyphen;
}

export function shouldBreakBeforeWhitespaceSeparatedInlineCodeFragment(params: {
  lineHasContent: boolean;
  startsAfterCodeWhitespace: boolean;
  reservedWidth: number;
  remainingWidth: number;
  fragmentWidth: number;
  slackPx?: number;
}): boolean {
  return (
    params.lineHasContent &&
    params.startsAfterCodeWhitespace &&
    params.reservedWidth + params.fragmentWidth > params.remainingWidth + (params.slackPx ?? 0) + 0.01
  );
}

export function shouldBreakBeforePathDelimiterNearFitContinuation(params: {
  lineHasContent: boolean;
  sameCodeGroupContinuation: boolean;
  lastFragmentEndedWithPathDelimiter: boolean;
  startsAfterCodeWhitespace: boolean;
  isSealedInlineCodeFragment: boolean;
  reservedWidth: number;
  remainingWidth: number;
  fragmentWidth: number;
  slackPx?: number;
}): boolean {
  return (
    params.lineHasContent &&
    params.sameCodeGroupContinuation &&
    params.lastFragmentEndedWithPathDelimiter &&
    !params.startsAfterCodeWhitespace &&
    !params.isSealedInlineCodeFragment &&
    params.reservedWidth + params.fragmentWidth + INLINE_CODE_PATH_DELIMITER_CONTINUATION_MIN_SPARE_PX >
      params.remainingWidth + (params.slackPx ?? 0) + 0.01
  );
}

export function resolveInlineCodeWhitespaceSeparatedFragmentSlackPx(params: {
  lineHasContent: boolean;
  startsAfterCodeWhitespace: boolean;
  fragmentText: string;
}): number {
  if (
    browserAllowsInlineCodeLeadingHang() &&
    params.lineHasContent &&
    params.startsAfterCodeWhitespace &&
    params.fragmentText !== "-" &&
    params.fragmentText.endsWith("-") &&
    !params.fragmentText.includes("/") &&
    !params.fragmentText.includes("\\") &&
    !/\s/.test(params.fragmentText)
  ) {
    return INLINE_CODE_CONTINUATION_FIT_SLACK_PX;
  }
  if (
    !browserAllowsInlineCodeLeadingHang() &&
    params.lineHasContent &&
    params.startsAfterCodeWhitespace &&
    params.fragmentText.includes(":") &&
    !params.fragmentText.includes("/") &&
    !/\s/.test(params.fragmentText)
  ) {
    return INLINE_CODE_COLON_COMMAND_FRAGMENT_SLACK_PX;
  }
  return browserAllowsInlineCodeLeadingHang() &&
    params.lineHasContent &&
    params.startsAfterCodeWhitespace &&
    params.fragmentText === "-"
    ? INLINE_CODE_STANDALONE_HYPHEN_FRAGMENT_SLACK_PX
    : 0;
}

export function resolveInlineCodeContinuationFitSlackPx(params: {
  lineHasContent: boolean;
  atLineBreakBoundary: boolean;
  sameCodeGroupContinuation: boolean;
  startsAfterCodeWhitespace: boolean;
  lastFragmentEndedWithDot: boolean;
  lastFragmentEndedWithHyphen: boolean;
  lastFragmentEndedWithPathDelimiter: boolean;
  item: InlineContinuationSlackItem;
}): number {
  const shouldDisableChromiumNonDelimitedPathTailSlack =
    browserAllowsInlineCodeLeadingHang() &&
    params.item.isPathTailFragment &&
    !params.lastFragmentEndedWithPathDelimiter;
  const shouldDisableForEngine =
    shouldDisableChromiumNonDelimitedPathTailSlack ||
    !browserAllowsInlineCodeLeadingHang() &&
    (params.item.isPathTailFragment ||
      params.item.codeGroupHasDottedPath ||
      params.item.isSealedInlineCodeFragment ||
      params.item.text.includes("/") ||
      params.item.text.includes("\\"));
  if (
    !params.lineHasContent ||
    !params.atLineBreakBoundary ||
    !params.sameCodeGroupContinuation ||
    params.startsAfterCodeWhitespace ||
    (params.lastFragmentEndedWithDot &&
      !params.item.text.includes(".") &&
      !params.item.text.includes("/") &&
      !params.item.text.includes("\\") &&
      !params.item.isPathTailFragment) ||
    params.lastFragmentEndedWithHyphen ||
    (params.lastFragmentEndedWithPathDelimiter && params.item.codeGroupStartsAfterStyledTextSeam) ||
    (params.lastFragmentEndedWithPathDelimiter &&
      params.item.codeGroupStartsAfterText &&
      params.item.codeGroupHasTrailingText) ||
    (params.lastFragmentEndedWithPathDelimiter && !browserAllowsInlineCodeLeadingHang()) ||
    shouldDisableForEngine
  ) {
    return 0;
  }
  const chromiumPathTailSlackPx =
    browserAllowsInlineCodeLeadingHang() &&
    params.lastFragmentEndedWithPathDelimiter &&
    !params.item.isSealedInlineCodeFragment
      ? INLINE_CODE_PATH_TAIL_CONTINUATION_FIT_SLACK_PX
      : 0;
  // Chromium sometimes keeps a short terminal path tail on the current line
  // after a slash boundary, but that tolerance is much smaller than the
  // generic same-group continuation slack.
  if (
    chromiumPathTailSlackPx > 0 &&
    !params.item.isSealedInlineCodeFragment
  ) {
    return chromiumPathTailSlackPx;
  }
  return INLINE_CODE_CONTINUATION_FIT_SLACK_PX + chromiumPathTailSlackPx;
}

export function resolveInlineCodeProseStartSeamGuardPx(params: {
  startsAtLineStart: boolean;
  item: InlineContinuationSlackItem & Pick<InlineSegmentItem, "codeGroupHasWhitespace" | "codeGroupStartsAfterText" | "isFirstCodeGroupFragment" | "prefersFreshLineStart">;
}): number {
  if (
    params.startsAtLineStart ||
    !browserAllowsInlineCodeLeadingHang() ||
    !params.item.isFirstCodeGroupFragment ||
    !params.item.codeGroupStartsAfterText ||
    !params.item.prefersFreshLineStart ||
    params.item.codeGroupHasWhitespace
  ) {
    return 0;
  }
  return INLINE_CODE_PROSE_START_SEAM_GUARD_PX;
}

export function resolveInlineCodeWrapChromeWidth(params: {
  allowLeadingHang?: boolean;
  chromeWidth: number;
  codeGroupHasWhitespace: boolean;
  codeGroupStartsAfterText: boolean;
  chargedChrome: boolean;
  isFirstCodeGroupFragment: boolean;
  prefersFreshLineStart: boolean;
  lineHasContent: boolean;
  startsAtLineStart: boolean;
}): number {
  if (params.chargedChrome) {
    return 0;
  }
  // Chromium continuation lines of path-like inline code still keep a visible
  // chip edge, but they do not consume the full leading chrome width of a
  // brand-new chip. Charging half the edge matches the wrapped DOM more
  // closely than treating every continuation line like a fresh code start.
  if (
    browserAllowsInlineCodeLeadingHang() &&
    params.startsAtLineStart &&
    !params.isFirstCodeGroupFragment &&
    params.codeGroupStartsAfterText &&
    !params.codeGroupHasWhitespace
  ) {
    return params.chromeWidth / 2;
  }
  // Chromium lets one edge of the outer <code> chip hang on the first visual
  // slice of a continuous path-like code group when that slice starts after
  // prose on the same line. We only opt into that discount from the current-
  // line fit path when the full code group does not fit inline; preferred-start
  // and whole-group widths still charge full chrome so near-threshold prose
  // math stays conservative.
  if (
    (params.allowLeadingHang ?? false) &&
    browserAllowsInlineCodeLeadingHang() &&
    !params.startsAtLineStart &&
    params.codeGroupStartsAfterText &&
    params.isFirstCodeGroupFragment &&
    (params.prefersFreshLineStart || params.codeGroupHasWhitespace)
  ) {
    return params.chromeWidth / 2;
  }
  return params.chromeWidth;
}

export function shouldBreakBeforePartialSealedDottedPathContinuation(params: {
  currentCodeGroupStartFragmentText: string | null;
  fullWidth: number;
  guardedRemainingWidth: number;
  item: Pick<InlineSegmentItem, "codeGroupHasDottedPath" | "codeGroupStartsAfterText" | "isPathTailFragment" | "text">;
  lastFragmentText: string | null;
  sameCodeGroupContinuation: boolean;
}): boolean {
  return (
    params.sameCodeGroupContinuation &&
    params.currentCodeGroupStartFragmentText != null &&
    params.currentCodeGroupStartFragmentText === params.lastFragmentText &&
    isShortExtensionPathLikeFragment(params.lastFragmentText) &&
    params.item.codeGroupStartsAfterText &&
    params.item.codeGroupHasDottedPath &&
    !params.item.text.includes("/") &&
    !params.item.text.includes("\\") &&
    (params.item.text.includes(".") || params.item.isPathTailFragment) &&
    params.fullWidth > params.guardedRemainingWidth + 0.01
  );
}

export function shouldBreakBeforePartialDottedStemPathTailContinuation(params: {
  fullWidth: number;
  guardedRemainingWidth: number;
  item: Pick<InlineSegmentItem, "codeGroupHasDottedPath" | "codeGroupStartsAfterText" | "text">;
  lastFragmentText: string | null;
  sameCodeGroupContinuation: boolean;
}): boolean {
  return (
    params.sameCodeGroupContinuation &&
    params.item.codeGroupStartsAfterText &&
    params.item.codeGroupHasDottedPath &&
    /[\\/]/.test(params.item.text) &&
    /\.$/.test(params.lastFragmentText ?? "") &&
    params.fullWidth > params.guardedRemainingWidth + 0.01
  );
}

export function shouldBreakBeforePartialDottedCallContinuation(params: {
  fragmentWidth: number;
  fullWidth: number;
  guardedRemainingWidth: number;
  item: Pick<InlineSegmentItem, "isPathTailFragment" | "isSealedInlineCodeFragment" | "text">;
  lastFragmentText: string | null;
  maxWidth: number;
  minSparePx?: number;
  sameCodeGroupContinuation: boolean;
}): boolean {
  const dottedStem = (params.lastFragmentText ?? "").replace(/\.$/, "");
  const shouldReserveDottedSpare =
    /^[\p{L}_]/u.test(dottedStem) &&
    Array.from(dottedStem).length >= DOTTED_CALL_SPARE_MIN_STEM_GRAPHEMES;
  const minSparePx = shouldReserveDottedSpare ? Math.max(0, params.minSparePx ?? 0) : 0;
  return (
    params.sameCodeGroupContinuation &&
    /\.$/.test(params.lastFragmentText ?? "") &&
    !params.item.isSealedInlineCodeFragment &&
    !params.item.isPathTailFragment &&
    !params.item.text.includes(".") &&
    !params.item.text.includes("/") &&
    !params.item.text.includes("\\") &&
    params.fullWidth + minSparePx > params.guardedRemainingWidth + 0.01 &&
    params.fragmentWidth <= params.maxWidth + 0.01
  );
}
