import { isDesktopApp } from "./desktop";

const legacyCopyText = (text: string): boolean => {
  if (typeof document === "undefined") return false;
  const textarea = document.createElement("textarea");
  textarea.value = text;
  textarea.setAttribute("readonly", "");
  textarea.style.position = "fixed";
  textarea.style.top = "0";
  textarea.style.left = "-9999px";
  textarea.style.opacity = "0";
  document.body.appendChild(textarea);
  textarea.focus();
  textarea.select();
  try {
    return document.execCommand("copy");
  } finally {
    document.body.removeChild(textarea);
  }
};

export type ClipboardCopyResult =
  | { ok: true }
  | { ok: false; reason: "empty" | "blocked" | "unavailable" | "failed"; error?: unknown };

const isClipboardBlockedError = (error: unknown): boolean => {
  if (error instanceof DOMException) {
    if (error.name === "NotAllowedError" || error.name === "SecurityError") return true;
  }
  if (!(error instanceof Error)) return false;
  const message = `${error.name} ${error.message}`.toLowerCase();
  return (
    message.includes("notallowed") ||
    message.includes("permission") ||
    message.includes("secure context") ||
    message.includes("https") ||
    message.includes("denied")
  );
};

export const tryCopyTextToClipboard = async (text: string): Promise<ClipboardCopyResult> => {
  if (!text) return { ok: false, reason: "empty" };
  if (legacyCopyText(text)) return { ok: true };
  if (typeof navigator === "undefined" || !navigator.clipboard?.writeText) {
    return { ok: false, reason: "unavailable" };
  }
  try {
    await navigator.clipboard.writeText(text);
    return { ok: true };
  } catch (error: unknown) {
    return {
      ok: false,
      reason: isClipboardBlockedError(error) ? "blocked" : "failed",
      error,
    };
  }
};

export const describeClipboardCopyFailure = (
  result: Extract<ClipboardCopyResult, { ok: false }>,
  opts?: { action?: string },
): string => {
  const action = String(opts?.action ?? "copy to the clipboard").trim() || "copy to the clipboard";
  if (result.reason === "blocked") {
    return isDesktopApp() ? "Clipboard access is blocked." : "Clipboard access is blocked. Use HTTPS or copy manually.";
  }
  if (result.reason === "empty") return `Nothing to ${action}.`;
  return `Couldn't ${action}.`;
};

export const copyTextToClipboard = async (text: string): Promise<boolean> => {
  const result = await tryCopyTextToClipboard(text);
  return result.ok;
};
