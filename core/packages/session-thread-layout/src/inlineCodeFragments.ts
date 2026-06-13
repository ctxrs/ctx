const INLINE_CODE_HYPHEN_PREFIX_MAX_GRAPHEMES = 6;
const INLINE_CODE_HYPHEN_FOLLOWING_MAX_GRAPHEMES = 8;
const INLINE_CODE_SLASH_COALESCE_MAX_GRAPHEMES = 8;
const INLINE_CODE_SLASH_FOLLOWING_COALESCE_MAX_GRAPHEMES = 12;
function countGraphemes(text: string): number {
  return Array.from(text).length;
}

export function splitInlineCodeFragments(text: string): string[] {
  if (text.length === 0) {
    return [];
  }

  const rawFragments: string[] = [];
  let current = "";
  for (const char of text) {
    current += char;
    if (char === "-" || char === "." || char === "/" || char === "\\") {
      rawFragments.push(current);
      current = "";
    }
  }
  if (current.length > 0) {
    rawFragments.push(current);
  }

  const hyphenCoalescedFragments: string[] = [];
  for (const fragment of rawFragments) {
    const previous = hyphenCoalescedFragments[hyphenCoalescedFragments.length - 1] ?? null;
    const preceding = hyphenCoalescedFragments[hyphenCoalescedFragments.length - 2] ?? null;
    const shouldCoalesceHyphenPrefix =
      previous != null &&
      previous.endsWith("-") &&
      (fragment.endsWith("/") || fragment.endsWith("\\") || /[\\/]$/.test(preceding ?? "")) &&
      countGraphemes(previous) <= INLINE_CODE_HYPHEN_PREFIX_MAX_GRAPHEMES &&
      countGraphemes(fragment) <= INLINE_CODE_HYPHEN_FOLLOWING_MAX_GRAPHEMES;
    if (shouldCoalesceHyphenPrefix) {
      hyphenCoalescedFragments[hyphenCoalescedFragments.length - 1] = `${previous}${fragment}`;
      continue;
    }
    hyphenCoalescedFragments.push(fragment);
  }

  const fragments: string[] = [];
  for (const fragment of hyphenCoalescedFragments) {
    const previous = fragments[fragments.length - 1] ?? null;
    const shouldCoalesceWithPrevious =
      previous != null &&
      /[\\/]$/.test(previous) &&
      countGraphemes(previous) <= INLINE_CODE_SLASH_COALESCE_MAX_GRAPHEMES &&
      countGraphemes(fragment) <= INLINE_CODE_SLASH_FOLLOWING_COALESCE_MAX_GRAPHEMES;
    if (shouldCoalesceWithPrevious) {
      fragments[fragments.length - 1] = `${previous}${fragment}`;
      continue;
    }
    fragments.push(fragment);
  }

  return fragments;
}

export function isSealedInlineCodeFragment(fragment: string): boolean {
  return fragment.endsWith("-") || fragment.endsWith(".") || fragment.endsWith("/") || fragment.endsWith("\\");
}
