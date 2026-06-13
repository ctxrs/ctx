const COMPLEX_INLINE_CODE_DOM_EXACT_COMPLEX_MARKDOWN_LENGTH_THRESHOLD = 140;
const COMPLEX_INLINE_CODE_DOM_EXACT_LONG_MARKDOWN_LENGTH_THRESHOLD = 220;
const MULTILINE_LONG_TOKEN_DOM_EXACT_MARKDOWN_LENGTH_THRESHOLD = 96;
const MULTILINE_LONG_TOKEN_DOM_EXACT_LONGEST_TOKEN_THRESHOLD = 24;

const DOM_EXACT_PLAIN_TEXT_CHAR_THRESHOLD = 6_000;
const DOM_EXACT_PLAIN_TEXT_MULTILINE_CHAR_THRESHOLD = 128;
const DOM_EXACT_PLAIN_TEXT_MULTILINE_LONGEST_LINE_THRESHOLD = 32;

const ASSISTANT_DOM_EXACT_INLINE_CODE_CHAR_THRESHOLD = 96;
const ASSISTANT_DOM_EXACT_LONG_PROSE_CHAR_THRESHOLD = 220;

function readInlineCodeSpans(markdown: string): string[] {
  return Array.from(markdown.matchAll(/`([^`\n]+)`/g), (match) => match[1] ?? "");
}

function countComplexInlineCodeSpans(spans: readonly string[]): number {
  return spans.filter((span) => /[./\\#:=_-]|\s/u.test(span)).length;
}

function containsMarkdownListMarker(markdown: string): boolean {
  return /(?:^|\n)\s*(?:[-+*]|\d+\.)\s/u.test(markdown);
}

function hasDottedInlineCodeSpan(spans: readonly string[]): boolean {
  return spans.some((span) => span.includes(".") && !span.includes("://"));
}

function longestMarkdownTokenLength(markdown: string): number {
  return (markdown.match(/\S+/g) ?? []).reduce(
    (max, token) => Math.max(max, Array.from(token).length),
    0,
  );
}

export function shouldUseRenderedMarkdownMeasurement(markdown: string): boolean {
  if (
    markdown.length >= MULTILINE_LONG_TOKEN_DOM_EXACT_MARKDOWN_LENGTH_THRESHOLD &&
    /\n\s*\n/u.test(markdown) &&
    longestMarkdownTokenLength(markdown) >= MULTILINE_LONG_TOKEN_DOM_EXACT_LONGEST_TOKEN_THRESHOLD
  ) {
    return true;
  }
  if (!markdown.includes("`")) {
    return false;
  }
  const inlineCodeSpans = readInlineCodeSpans(markdown);
  if (inlineCodeSpans.length === 0) {
    return false;
  }
  if (
    containsMarkdownListMarker(markdown) &&
    hasDottedInlineCodeSpan(inlineCodeSpans) &&
    /`\s*[,.;:!?-]?\s+[\p{L}\p{N}]/u.test(markdown) &&
    markdown.length >= 80
  ) {
    return true;
  }
  if (markdown.length >= COMPLEX_INLINE_CODE_DOM_EXACT_LONG_MARKDOWN_LENGTH_THRESHOLD) {
    return true;
  }
  const complexInlineCodeSpanCount = countComplexInlineCodeSpans(inlineCodeSpans);
  return (
    complexInlineCodeSpanCount >= 2 ||
    (complexInlineCodeSpanCount >= 1 &&
      markdown.length >= COMPLEX_INLINE_CODE_DOM_EXACT_COMPLEX_MARKDOWN_LENGTH_THRESHOLD)
  );
}

export function shouldUseExactRenderedPlainTextMeasurement(text: string): boolean {
  if (text.length >= DOM_EXACT_PLAIN_TEXT_CHAR_THRESHOLD) {
    return true;
  }
  if (!text.includes("\n") || text.length < DOM_EXACT_PLAIN_TEXT_MULTILINE_CHAR_THRESHOLD) {
    return false;
  }
  const longestLineLength = text.split("\n").reduce(
    (max, line) => Math.max(max, Array.from(line).length),
    0,
  );
  return longestLineLength >= DOM_EXACT_PLAIN_TEXT_MULTILINE_LONGEST_LINE_THRESHOLD;
}

export function shouldUseRenderedAssistantRowMeasurement(content: string): boolean {
  return (
    (content.includes("`") && content.length >= ASSISTANT_DOM_EXACT_INLINE_CODE_CHAR_THRESHOLD) ||
    content.length >= ASSISTANT_DOM_EXACT_LONG_PROSE_CHAR_THRESHOLD
  );
}
