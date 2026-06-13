import { useEffect, useMemo, useState } from "react";
import { BROWSER_CAPABILITY_REFRESH_MARGIN_MS } from "./browserCapabilityAuth";
import {
  BrowserResourceUrlError,
  browserResourceUrlForScope,
  type BrowserResourceUrlScope,
} from "./browserResourceUrls";
import { useDaemonConnection } from "./useDaemonConnection";

export type BrowserResourceUrlHookState =
  | { status: "none"; url: null; error: null; expiresAt: null }
  | { status: "ready"; url: string; error: null; expiresAt: number }
  | { status: "unsupported"; url: null; error: string; expiresAt: null };

const UNSUPPORTED_BROWSER_RESOURCE_URL_MESSAGE =
  "Resource preview is unavailable for this connection.";

export const useBrowserResourceUrlState = (
  scope: BrowserResourceUrlScope | null,
): BrowserResourceUrlHookState => {
  const connection = useDaemonConnection();
  const [refreshNonce, setRefreshNonce] = useState(0);
  const kind = scope?.kind ?? null;
  const blobId = scope?.kind === "blob" ? scope.blobId : "";
  const sessionId = scope?.kind === "session_artifact" ? scope.sessionId : "";
  const artifactId = scope?.kind === "session_artifact" ? scope.artifactId : "";

  const state = useMemo<BrowserResourceUrlHookState>(() => {
    try {
      if (kind === "blob") {
        const resource = browserResourceUrlForScope({ kind, blobId }, { connection });
        return { status: "ready", url: resource.url, error: null, expiresAt: resource.expiresAt };
      }
      if (kind === "session_artifact") {
        const resource = browserResourceUrlForScope({ kind, sessionId, artifactId }, { connection });
        return { status: "ready", url: resource.url, error: null, expiresAt: resource.expiresAt };
      }
      return { status: "none", url: null, error: null, expiresAt: null };
    } catch (err: unknown) {
      if (err instanceof BrowserResourceUrlError) {
        return {
          status: "unsupported",
          url: null,
          error: UNSUPPORTED_BROWSER_RESOURCE_URL_MESSAGE,
          expiresAt: null,
        };
      }
      throw err;
    }
  }, [artifactId, blobId, connection, kind, refreshNonce, sessionId]);

  useEffect(() => {
    if (state.status !== "ready") return;
    const refreshAtMs = state.expiresAt * 1000 - BROWSER_CAPABILITY_REFRESH_MARGIN_MS;
    const delayMs = Math.max(0, refreshAtMs - Date.now());
    const timeout = window.setTimeout(() => {
      setRefreshNonce((value) => value + 1);
    }, delayMs);
    return () => window.clearTimeout(timeout);
  }, [state.expiresAt, state.status]);

  return state;
};

export const useBrowserResourceUrl = (scope: BrowserResourceUrlScope | null): string | null =>
  useBrowserResourceUrlState(scope).url;

export const useBlobResourceUrl = (blobId: string | null | undefined): string | null =>
  useBrowserResourceUrl(blobId ? { kind: "blob", blobId: String(blobId) } : null);

export const useBlobResourceUrlState = (
  blobId: string | null | undefined,
): BrowserResourceUrlHookState =>
  useBrowserResourceUrlState(blobId ? { kind: "blob", blobId: String(blobId) } : null);

export const useArtifactResourceUrl = (
  sessionId: string | null | undefined,
  artifactId: string | null | undefined,
): string | null =>
  useBrowserResourceUrl(
    sessionId && artifactId
      ? {
        kind: "session_artifact",
        sessionId: String(sessionId),
        artifactId: String(artifactId),
      }
      : null,
  );

export const useArtifactResourceUrlState = (
  sessionId: string | null | undefined,
  artifactId: string | null | undefined,
): BrowserResourceUrlHookState =>
  useBrowserResourceUrlState(
    sessionId && artifactId
      ? {
        kind: "session_artifact",
        sessionId: String(sessionId),
        artifactId: String(artifactId),
      }
      : null,
  );
