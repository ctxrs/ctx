import {
  BROWSER_CAPABILITY_REFRESH_MARGIN_MS,
  browserCapabilityExpiresAt,
  deriveBrowserCapabilityToken,
  serializeBrowserCapabilityScope,
  type BrowserCapabilityScope,
} from "./browserCapabilityAuth";
import { getDaemonConnection, type DaemonConnection } from "./daemonConnection";
import { sha256Hex } from "./sha256";

export type BrowserResourceUrlScope = BrowserCapabilityScope;

export type BrowserResourceUrl = {
  url: string;
  expiresAt: number;
  cacheKey: string;
};

export class BrowserResourceUrlError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "BrowserResourceUrlError";
  }
}

type BrowserResourceUrlOptions = {
  connection?: DaemonConnection;
  nowMs?: number;
};

type CacheEntry = BrowserResourceUrl & {
  expiresAtMs: number;
  lastUsedAtMs: number;
};

const MAX_BROWSER_RESOURCE_URL_CACHE_ENTRIES = 512;
const encoder = new TextEncoder();
const cache = new Map<string, CacheEntry>();

const tokenFingerprint = (token: string): string =>
  sha256Hex(encoder.encode(`ctx-browser-resource-url-cache|${token}`)).slice(0, 32);

const httpUrlForPath = (path: string, baseUrl: string | null | undefined): string => {
  if (path.startsWith("http://") || path.startsWith("https://")) return path;
  if (!baseUrl) return path;
  const prefix = baseUrl.replace(/\/+$/, "");
  const pathname = path.startsWith("/") ? path : `/${path}`;
  return `${prefix}${pathname}`;
};

export const browserResourcePathForScope = (scope: BrowserResourceUrlScope): string => {
  switch (scope.kind) {
    case "blob":
      return `/api/blobs/${encodeURIComponent(String(scope.blobId || ""))}`;
    case "session_artifact":
      return `/api/sessions/${encodeURIComponent(String(scope.sessionId || ""))}/artifacts/${encodeURIComponent(
        String(scope.artifactId || ""),
      )}`;
  }
};

const browserResourceUrlCacheKey = (
  scope: BrowserResourceUrlScope,
  connection: DaemonConnection,
): string => {
  const token = connection.authToken;
  if (!token) {
    throw new BrowserResourceUrlError(
      `Cannot create browser resource URL for ${serializeBrowserCapabilityScope(scope)} without a daemon auth token.`,
    );
  }
  return [
    connection.baseUrl ?? "",
    tokenFingerprint(token),
    serializeBrowserCapabilityScope(scope),
  ].join("|");
};

const pruneCache = (nowMs: number): void => {
  for (const [key, entry] of cache.entries()) {
    if (nowMs >= entry.expiresAtMs) {
      cache.delete(key);
    }
  }
  while (cache.size > MAX_BROWSER_RESOURCE_URL_CACHE_ENTRIES) {
    let oldestKey: string | null = null;
    let oldestUsedAt = Number.POSITIVE_INFINITY;
    for (const [key, entry] of cache.entries()) {
      if (entry.lastUsedAtMs < oldestUsedAt) {
        oldestUsedAt = entry.lastUsedAtMs;
        oldestKey = key;
      }
    }
    if (!oldestKey) break;
    cache.delete(oldestKey);
  }
};

export const browserResourceUrlForScope = (
  scope: BrowserResourceUrlScope,
  opts?: BrowserResourceUrlOptions,
): BrowserResourceUrl => {
  const nowMs = opts?.nowMs ?? Date.now();
  const connection = opts?.connection ?? getDaemonConnection();
  const key = browserResourceUrlCacheKey(scope, connection);
  const existing = cache.get(key);
  if (existing && nowMs < existing.expiresAtMs - BROWSER_CAPABILITY_REFRESH_MARGIN_MS) {
    existing.lastUsedAtMs = nowMs;
    return {
      url: existing.url,
      expiresAt: existing.expiresAt,
      cacheKey: existing.cacheKey,
    };
  }

  const authToken = connection.authToken;
  if (!authToken) {
    throw new BrowserResourceUrlError(
      `Cannot create browser resource URL for ${serializeBrowserCapabilityScope(scope)} without a daemon auth token.`,
    );
  }
  const expiresAt = browserCapabilityExpiresAt(nowMs);
  const query = new URLSearchParams();
  query.set("expires_at", String(expiresAt));
  query.set("token", deriveBrowserCapabilityToken(authToken, scope, expiresAt));
  const url = `${httpUrlForPath(browserResourcePathForScope(scope), connection.baseUrl)}?${query.toString()}`;
  const entry: CacheEntry = {
    url,
    expiresAt,
    cacheKey: key,
    expiresAtMs: expiresAt * 1000,
    lastUsedAtMs: nowMs,
  };
  cache.set(key, entry);
  pruneCache(nowMs);
  return {
    url: entry.url,
    expiresAt: entry.expiresAt,
    cacheKey: entry.cacheKey,
  };
};

export const blobResourceUrl = (blobId: string): string =>
  browserResourceUrlForScope({ kind: "blob", blobId: String(blobId || "") }).url;

export const artifactResourceUrl = (sessionId: string, artifactId: string): string =>
  browserResourceUrlForScope({
    kind: "session_artifact",
    sessionId: String(sessionId || ""),
    artifactId: String(artifactId || ""),
  }).url;

export const resetBrowserResourceUrlCacheForTests = (): void => {
  cache.clear();
};
