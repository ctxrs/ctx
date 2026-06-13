import { isDesktopShellApp } from "../utils/runtime";

export const isDesktopWindow = (): boolean => {
  return isDesktopShellApp();
};

export const normalizeToken = (value: string | null | undefined): string | null => {
  if (value === null || value === undefined) return null;
  const trimmed = String(value).trim();
  return trimmed ? trimmed : null;
};

const trimTrailingSlash = (value: string): string => value.replace(/\/+$/, "");

const parseUrlSafely = (value: string): URL | null => {
  try {
    return new URL(value);
  } catch {
    return null;
  }
};

export const deriveDaemonWsBaseUrl = (baseUrl: string | null): string | null => {
  if (!baseUrl) return null;
  const trimmed = trimTrailingSlash(baseUrl);
  if (!trimmed) return null;
  if (trimmed.startsWith("ws://") || trimmed.startsWith("wss://")) return trimmed;
  if (trimmed.startsWith("https://")) return trimmed.replace(/^https:\/\//, "wss://");
  if (trimmed.startsWith("http://")) return trimmed.replace(/^http:\/\//, "ws://");
  return null;
};

export const normalizeDaemonBaseUrl = (value: string | null | undefined): string | null => {
  if (value === null || value === undefined) return null;
  const trimmed = String(value).trim();
  if (!trimmed) return null;

  if (trimmed.startsWith("ws://")) {
    return trimmed.replace(/^ws:\/\//, "http://").replace(/\/+$/, "");
  }
  if (trimmed.startsWith("wss://")) {
    return trimmed.replace(/^wss:\/\//, "https://").replace(/\/+$/, "");
  }

  const parsed = parseUrlSafely(trimmed);
  if (!parsed) return null;
  if (parsed.protocol !== "http:" && parsed.protocol !== "https:") return null;
  return trimTrailingSlash(parsed.toString());
};

export const normalizeDaemonWsBaseUrl = (
  value: string | null | undefined,
  baseUrlForFallback?: string | null,
): string | null => {
  if (value === null || value === undefined) {
    return deriveDaemonWsBaseUrl(baseUrlForFallback ?? null);
  }
  const trimmed = String(value).trim();
  if (!trimmed) return deriveDaemonWsBaseUrl(baseUrlForFallback ?? null);

  const parsed = parseUrlSafely(trimmed);
  if (!parsed) {
    return deriveDaemonWsBaseUrl(baseUrlForFallback ?? null);
  }
  if (parsed.protocol === "ws:" || parsed.protocol === "wss:") {
    return trimTrailingSlash(parsed.toString());
  }
  if (parsed.protocol === "http:") {
    parsed.protocol = "ws:";
    return trimTrailingSlash(parsed.toString());
  }
  if (parsed.protocol === "https:") {
    parsed.protocol = "wss:";
    return trimTrailingSlash(parsed.toString());
  }
  return deriveDaemonWsBaseUrl(baseUrlForFallback ?? null);
};

export const normalizeRunId = (value: string | null | undefined): string | null => {
  if (value === null || value === undefined) return null;
  const trimmed = String(value).trim();
  return trimmed ? trimmed : null;
};
