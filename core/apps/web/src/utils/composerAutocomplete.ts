export type ComposerAutocompleteKind = "slash" | "at";

export type ComposerAutocompleteToken = {
  kind: ComposerAutocompleteKind;
  start: number;
  end: number;
  query: string;
};

const isWhitespace = (ch: string): boolean => ch === " " || ch === "\n" || ch === "\t";

const prevWhitespaceIndex = (text: string, cursor: number): number => {
  for (let i = Math.min(cursor, text.length) - 1; i >= 0; i--) {
    if (isWhitespace(text[i] ?? "")) return i;
  }
  return -1;
};

const nextWhitespaceIndex = (text: string, cursor: number): number => {
  for (let i = Math.min(cursor, text.length); i < text.length; i++) {
    if (isWhitespace(text[i] ?? "")) return i;
  }
  return text.length;
};

export function detectComposerAutocompleteToken(
  text: string,
  cursor: number,
): ComposerAutocompleteToken | null {
  const safeCursor = Math.max(0, Math.min(cursor, text.length));
  const prevWs = prevWhitespaceIndex(text, safeCursor);
  const tokenStart = prevWs + 1;
  if (tokenStart < 0 || tokenStart >= text.length) return null;

  const trigger = text[tokenStart];
  const kind: ComposerAutocompleteKind | null =
    trigger === "/" ? "slash" : trigger === "@" ? "at" : null;
  if (!kind) return null;

  if (tokenStart > 0 && !isWhitespace(text[tokenStart - 1] ?? "")) {
    return null;
  }

  const tokenEnd = nextWhitespaceIndex(text, safeCursor);
  const nextChar = text[tokenStart + 1] ?? "";
  if (nextChar === trigger) {
    return null;
  }

  const queryStart = tokenStart + 1;
  const queryEnd = Math.min(tokenEnd, safeCursor);
  const query = text.slice(queryStart, queryEnd);

  return { kind, start: tokenStart, end: tokenEnd, query };
}

export function applyComposerAutocompleteCompletion(
  text: string,
  token: ComposerAutocompleteToken,
  replacement: string,
): { nextText: string; nextCursor: number } {
  const start = Math.max(0, Math.min(token.start, text.length));
  const end = Math.max(start, Math.min(token.end, text.length));

  const before = text.slice(0, start);
  const after = text.slice(end);
  const needsSpace = after.length === 0 ? true : !isWhitespace(after[0] ?? "");
  const insert = needsSpace ? `${replacement} ` : replacement;
  const nextText = `${before}${insert}${after}`;
  const nextCursor = before.length + insert.length;
  return { nextText, nextCursor };
}
