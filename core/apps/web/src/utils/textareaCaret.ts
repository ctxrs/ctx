export function getTextareaCaretRect(textarea: HTMLTextAreaElement): DOMRect | null {
  const value = textarea.value ?? "";
  const selectionStart = textarea.selectionStart ?? value.length;
  const textBefore = value.slice(0, Math.max(0, Math.min(selectionStart, value.length)));

  const style = window.getComputedStyle(textarea);
  const div = document.createElement("div");
  const span = document.createElement("span");

  div.style.position = "absolute";
  div.style.visibility = "hidden";
  div.style.whiteSpace = "pre-wrap";
  div.style.wordWrap = "break-word";
  div.style.overflow = "hidden";

  div.style.fontFamily = style.fontFamily;
  div.style.fontSize = style.fontSize;
  div.style.fontWeight = style.fontWeight;
  div.style.fontStyle = style.fontStyle;
  div.style.letterSpacing = style.letterSpacing;
  div.style.lineHeight = style.lineHeight;
  div.style.textTransform = style.textTransform;
  div.style.textIndent = style.textIndent;
  div.style.padding = style.padding;
  div.style.border = style.border;
  div.style.boxSizing = style.boxSizing;
  div.style.width = style.width;

  div.scrollTop = textarea.scrollTop;
  div.scrollLeft = textarea.scrollLeft;

  div.textContent = textBefore;
  span.textContent = "\u200b";
  div.appendChild(span);
  document.body.appendChild(div);

  try {
    const spanRect = span.getBoundingClientRect();
    const divRect = div.getBoundingClientRect();
    const taRect = textarea.getBoundingClientRect();

    const left = taRect.left + (spanRect.left - divRect.left) - textarea.scrollLeft;
    const top = taRect.top + (spanRect.top - divRect.top) - textarea.scrollTop;
    const height = parseFloat(style.lineHeight || "16") || 16;
    return new DOMRect(left, top, 0, height);
  } finally {
    document.body.removeChild(div);
  }
}
