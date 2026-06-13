// Some providers/models emit citations using private-use unicode wrapper characters, e.g.:
//   \uE200cite\uE202turn0search3\uE201
// These IDs are not URLs; without a separate "sources" mapping they can't be resolved in the UI.
// For now, we strip these markers from display.

const CITE_START = "\uE200";
const CITE_END = "\uE201";
const CITE_SEP = "\uE202";

export function stripCitationMarkers(input: string): string {
  const text = String(input ?? "");
  if (!text) return "";

  let out = "";
  let i = 0;
  while (i < text.length) {
    const start = text.indexOf(CITE_START, i);
    if (start < 0) {
      out += text.slice(i);
      break;
    }
    out += text.slice(i, start);
    const end = text.indexOf(CITE_END, start + 1);
    if (end < 0) {
      // Unterminated marker (often from truncated/streaming output). Drop the remainder to avoid
      // leaking citation payloads like "cite...turn0search3".
      break;
    }
    // Drop the whole marker envelope (including payload).
    i = end + 1;
  }

  // Also drop any stray PUA markers if they appear outside the normal envelope.
  return out.replaceAll(CITE_START, "").replaceAll(CITE_SEP, "").replaceAll(CITE_END, "");
}
