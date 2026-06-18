import React, { useCallback, useEffect, useMemo, useState } from "react";
import { Copy, Download } from "lucide-react";
import { fetchArtifactText, idToString, type Artifact } from "../api/client";
import { useArtifactResourceUrlState } from "../api/useBrowserResourceUrl";
import { MemoMarkdown } from "../pages/SessionPage.markdown";
import {
  getArtifactPreviewKind,
  isPreviewableArtifact,
} from "../utils/artifacts";
import { artifactDisplayPath, artifactIdentityKey } from "../utils/artifactPaths";
import { buildArtifactDocumentPreview } from "../utils/documentArtifacts";
import { errorMessage } from "../utils/errorMessage";
import { ArtifactViewer } from "./ArtifactViewer";
import { copyArtifactImage, displayName, downloadArtifact, formatBytes } from "./artifactPaneUtils";

type TextPreviewState =
  | { status: "idle" | "loading"; content: string; error: null }
  | { status: "ready"; content: string; error: null }
  | { status: "error"; content: string; error: string };

function ArtifactInlineTextPreview({
  sessionId,
  artifact,
}: {
  sessionId: string;
  artifact: Artifact;
}) {
  const artifactId = idToString(artifact.id);
  const missing = Boolean(artifact.missing);
  const [textPreview, setTextPreview] = useState<TextPreviewState>({
    status: "idle",
    content: "",
    error: null,
  });

  useEffect(() => {
    if (!artifactId || missing) return;
    const controller = new AbortController();
    setTextPreview({ status: "loading", content: "", error: null });
    void fetchArtifactText(sessionId, artifactId, {
      signal: controller.signal,
    })
      .then((content) => {
        setTextPreview({ status: "ready", content, error: null });
      })
      .catch((err: unknown) => {
        if (controller.signal.aborted) return;
        setTextPreview({
          status: "error",
          content: "",
          error: errorMessage(err) || "Failed to load artifact.",
        });
    });
    return () => controller.abort();
  }, [artifactId, missing, sessionId]);

  if (textPreview.status === "loading" || textPreview.status === "idle") {
    return <div className="wb-artifact-inline-status">Loading preview…</div>;
  }

  if (textPreview.status === "error") {
    return <div className="wb-artifact-inline-status">{textPreview.error}</div>;
  }

  const documentPreview = buildArtifactDocumentPreview(artifact, textPreview.content);

  if (documentPreview?.renderKind === "markdown") {
    return (
      <div className="wb-artifact-inline-markdown wb-tool-markdown">
        <MemoMarkdown content={documentPreview.content} />
      </div>
    );
  }

  return <pre className="wb-artifact-inline-text">{documentPreview?.content ?? textPreview.content}</pre>;
}

function ArtifactCard({
  sessionId,
  artifact,
  onOpen,
}: {
  sessionId: string;
  artifact: Artifact;
  onOpen: (next: Artifact) => void;
}) {
  const [copying, setCopying] = useState(false);
  const name = displayName(artifact);
  const missing = Boolean(artifact.missing);
  const artifactId = idToString(artifact.id);
  const resourceUrl = useArtifactResourceUrlState(sessionId, artifactId);
  const url = resourceUrl.url;
  const mimeLabel = artifact.mime_type || "application/octet-stream";
  const meta = `${mimeLabel} · ${formatBytes(artifact.bytes)}`;
  const title = artifactDisplayPath(artifact) || name;
  const previewKind = getArtifactPreviewKind(artifact);
  const isVideo = previewKind === "video";
  const isImage = previewKind === "image";
  const isInlineTextPreview = previewKind === "markdown" || previewKind === "text";
  const canPreview = isPreviewableArtifact(artifact) && !missing;
  const canDownload = Boolean(artifactId) && !missing && Boolean(url);
  const canCopy = Boolean(artifactId) && !missing && Boolean(url) && isImage && !copying;

  const onDownload = useCallback(
    (event?: React.MouseEvent) => {
      event?.stopPropagation();
      if (!canDownload || !url) return;
      downloadArtifact(artifact, url);
    },
    [artifact, canDownload, url],
  );

  const onCopy = useCallback(async () => {
    if (!canCopy || !url) return;
    setCopying(true);
    try {
      await copyArtifactImage(artifact, url);
    } catch (err: unknown) {
      window.alert(errorMessage(err) || "Failed to copy image.");
    } finally {
      setCopying(false);
    }
  }, [artifact, canCopy, url]);

  const stopPreviewOpen = useCallback((event: React.SyntheticEvent) => {
    event.stopPropagation();
  }, []);

  let preview: React.ReactNode = null;
  if (missing) {
    preview = <div className="wb-artifact-missing">Missing on disk</div>;
  } else if ((isVideo || isImage) && resourceUrl.status === "unsupported") {
    preview = <div className="wb-artifact-inline-status">{resourceUrl.error}</div>;
  } else if (isVideo && url) {
    preview = (
      <video
        className="wb-artifact-video"
        autoPlay
        controls
        loop
        muted
        playsInline
        preload="metadata"
        onClick={stopPreviewOpen}
        onPointerDown={stopPreviewOpen}
      >
        <source src={url} type={artifact.mime_type || "video/mp4"} />
      </video>
    );
  } else if (isImage && url) {
    preview = <img className="wb-artifact-image" src={url} alt={name} />;
  } else if (isInlineTextPreview) {
    preview = (
      <ArtifactInlineTextPreview
        sessionId={sessionId}
        artifact={artifact}
      />
    );
  } else {
    preview = <div className="wb-artifact-file">{name}</div>;
  }

  return (
    <div
      className={`wb-artifact-card ${canPreview ? "wb-artifact-card-previewable" : ""}`}
      title={title}
      onClick={canPreview ? () => onOpen(artifact) : undefined}
    >
      <div className="wb-artifact-preview">{preview}</div>
      <div className="wb-artifact-meta">
        <div className="wb-artifact-name-row">
          <div className="wb-artifact-name">{name}</div>
          <div className="wb-artifact-actions">
            <button
              type="button"
              className="wb-artifact-action"
              onClick={onDownload}
              disabled={!canDownload}
              aria-label="Download artifact"
              title={canDownload ? "Download" : resourceUrl.status === "unsupported" ? "Unavailable" : "Missing"}
            >
              <Download size={14} />
            </button>
            {isImage ? (
              <button
                type="button"
                className="wb-artifact-action"
                onClick={(event) => {
                  event.stopPropagation();
                  void onCopy();
                }}
                disabled={!canCopy}
                aria-label="Copy image"
                title={canCopy ? "Copy image" : "Copy unavailable"}
              >
                <Copy size={14} />
              </button>
            ) : null}
          </div>
        </div>
        <div className="wb-artifact-sub">{missing ? "Missing" : meta}</div>
      </div>
    </div>
  );
}

export function ArtifactsPane({
  sessionId,
  artifacts,
  loading,
  error,
  onRetry,
}: {
  sessionId: string;
  artifacts: Artifact[];
  loading?: boolean;
  error?: string | null;
  onRetry?: () => void;
}) {
  const [viewerState, setViewerState] = useState<{ sessionId: string; artifact: Artifact } | null>(null);
  const viewerArtifact = useMemo(() => {
    if (!viewerState || viewerState.sessionId !== sessionId) return null;
    const openArtifactId = idToString(viewerState.artifact.id);
    if (openArtifactId) {
      return artifacts.find((artifact) => idToString(artifact.id) === openArtifactId) ?? null;
    }
    const openArtifactKey = artifactIdentityKey(viewerState.artifact);
    return artifacts.find((artifact) => artifactIdentityKey(artifact) === openArtifactKey) ?? null;
  }, [artifacts, sessionId, viewerState]);

  useEffect(() => {
    if (viewerState && (!viewerArtifact || viewerState.sessionId !== sessionId)) {
      setViewerState(null);
    }
  }, [sessionId, viewerArtifact, viewerState]);

  const rows = useMemo(() => {
    return artifacts.map((artifact) => {
      const artifactId = idToString(artifact.id);
      const key = artifactId || artifactIdentityKey(artifact);
      return (
        <ArtifactCard
          key={key}
          sessionId={sessionId}
          artifact={artifact}
          onOpen={(nextArtifact) => setViewerState({ sessionId, artifact: nextArtifact })}
        />
      );
    });
  }, [artifacts, sessionId]);

  return (
    <div className="wb-artifacts">
      <div className="wb-artifacts-top">
        <div className="wb-artifacts-title">Artifacts</div>
        <div className="wb-artifacts-count">{artifacts.length}</div>
      </div>
      <div className="wb-artifacts-body">
        {loading ? (
          <div className="wb-muted">Loading artifacts…</div>
        ) : error ? (
          <div className="wb-artifacts-error" role="alert">
            <div>{error}</div>
            {onRetry ? (
              <div>
                <button type="button" className="wb-session-load-issues-retry" onClick={onRetry}>
                  Retry
                </button>
              </div>
            ) : null}
          </div>
        ) : artifacts.length === 0 ? (
          <div className="wb-muted">No artifacts yet.</div>
        ) : (
          <div className="wb-artifacts-grid">{rows}</div>
        )}
      </div>
      {viewerArtifact ? (
        <ArtifactViewer
          sessionId={sessionId}
          artifact={viewerArtifact}
          onClose={() => setViewerState(null)}
        />
      ) : null}
    </div>
  );
}
