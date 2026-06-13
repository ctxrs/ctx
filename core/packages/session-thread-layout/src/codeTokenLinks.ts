export type FileRef = {
  path: string;
  line?: number;
  col?: number;
};

export type UrlRef = {
  url: string;
};

const WINDOWS_DRIVE_RE = /^[A-Za-z]:[\\/]/;
const URL_SCHEME_RE = /^[A-Za-z][A-Za-z0-9+.-]*:\/\//;

export const isAbsolutePath = (path: string): boolean => {
  if (!path) return false;
  if (path === "~" || path.startsWith("~/")) return true;
  if (path.startsWith("/") || path.startsWith("\\")) return true;
  return WINDOWS_DRIVE_RE.test(path);
};

export const splitWhitespaceTokens = (text: string): string[] => {
  if (!text) return [];
  const parts = text.split(/(\s+)/);
  if (parts.length && parts[parts.length - 1] === "") parts.pop();
  return parts;
};

const hasExplicitPathCue = (path: string): boolean => {
  if (!path) return false;
  if (path === "." || path === ".." || path === "~") return true;
  if (path.includes("://")) return false;
  if (path.startsWith("./") || path.startsWith("../") || path.startsWith("~/")) return true;
  if (path.startsWith("/") || path.startsWith("\\")) return true;
  if (WINDOWS_DRIVE_RE.test(path)) return true;
  return path.includes("/") || path.includes("\\");
};

const parseLineColSuffix = (raw: string): FileRef | null => {
  const hashMatch = raw.match(/^(.*)#L(\d+)(?:C(\d+))?$/);
  if (hashMatch) {
    const path = hashMatch[1];
    if (!hasExplicitPathCue(path)) return null;
    const line = Number.parseInt(hashMatch[2], 10);
    const col = hashMatch[3] ? Number.parseInt(hashMatch[3], 10) : undefined;
    return {
      path,
      line: Number.isFinite(line) && line > 0 ? line : undefined,
      col: Number.isFinite(col ?? NaN) && (col ?? 0) > 0 ? col : undefined,
    };
  }

  const colonMatch = raw.match(/^(.*?)(?::(\d+)(?::(\d+))?)$/);
  if (colonMatch) {
    const path = colonMatch[1];
    if (!hasExplicitPathCue(path)) return null;
    const line = Number.parseInt(colonMatch[2], 10);
    const col = colonMatch[3] ? Number.parseInt(colonMatch[3], 10) : undefined;
    return {
      path,
      line: Number.isFinite(line) && line > 0 ? line : undefined,
      col: Number.isFinite(col ?? NaN) && (col ?? 0) > 0 ? col : undefined,
    };
  }

  return null;
};

export const parseFileRefToken = (raw: string): FileRef | null => {
  if (!raw) return null;
  if (raw.includes("://")) return null;
  const withSuffix = parseLineColSuffix(raw);
  if (withSuffix) return withSuffix;
  if (!hasExplicitPathCue(raw)) return null;
  return { path: raw };
};

export const parseUrlToken = (raw: string): UrlRef | null => {
  if (!raw) return null;
  if (!URL_SCHEME_RE.test(raw)) return null;
  try {
    const url = new URL(raw);
    if (url.protocol !== "http:" && url.protocol !== "https:") return null;
    return { url: raw };
  } catch {
    return null;
  }
};
