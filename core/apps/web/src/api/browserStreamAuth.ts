import { sha256Hex } from "./sha256";

export type BrowserStreamScope =
  | { kind: "workspace_active_snapshot"; workspaceId: string }
  | { kind: "workspace_stream"; workspaceId: string }
  | { kind: "workspace_vcs"; workspaceId: string }
  | { kind: "execution_launch"; jobId: string }
  | { kind: "dictation_livekit" }
  | { kind: "provider_install"; installId: string };

const BROWSER_STREAM_TOKEN_TTL_MS = 5 * 60 * 1000;
const encoder = new TextEncoder();

export const serializeBrowserStreamScope = (scope: BrowserStreamScope): string => {
  switch (scope.kind) {
    case "workspace_active_snapshot":
      return `workspace_active_snapshot:${scope.workspaceId}`;
    case "workspace_stream":
      return `workspace_stream:${scope.workspaceId}`;
    case "workspace_vcs":
      return `workspace_vcs:${scope.workspaceId}`;
    case "execution_launch":
      return `execution_launch:${scope.jobId}`;
    case "dictation_livekit":
      return "dictation_livekit";
    case "provider_install":
      return `provider_install:${scope.installId}`;
  }
};

export const deriveBrowserStreamToken = async (
  authToken: string,
  scope: BrowserStreamScope,
  expiresAt: number,
): Promise<string> => {
  return sha256Hex(
    encoder.encode(
      `ctx-browser-stream|${serializeBrowserStreamScope(scope)}|${expiresAt}|${authToken}`,
    ),
  );
};

export const setBrowserStreamQueryToken = async (
  query: URLSearchParams,
  authToken: string | null | undefined,
  scope: BrowserStreamScope,
): Promise<void> => {
  if (!authToken) return;
  const expiresAt = Math.floor((Date.now() + BROWSER_STREAM_TOKEN_TTL_MS) / 1000);
  query.set("expires_at", String(expiresAt));
  query.set("token", await deriveBrowserStreamToken(authToken, scope, expiresAt));
};
