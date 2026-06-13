import type { Artifact } from "../api/client";
import { getArtifactDocumentFormat } from "./documentArtifacts";

const VIDEO_EXTENSIONS = new Set(["mp4", "mov", "webm", "m4v"]);
const CSV_EXTENSIONS = new Set(["csv"]);

export type ArtifactPreviewKind = "none" | "image" | "video" | "markdown" | "text";

const artifactExtension = (value?: string | null): string => {
  if (!value) return "";
  const parts = value.split(".");
  if (parts.length <= 1) return "";
  return parts[parts.length - 1].toLowerCase();
};

const artifactMime = (artifact: Artifact): string => (artifact.mime_type ?? "").toLowerCase();

export const isVideoArtifact = (artifact: Artifact): boolean => {
  const mime = artifactMime(artifact);
  if (mime.startsWith("video/")) return true;
  const ext = artifactExtension(artifact.absolute_path || artifact.name || "");
  return VIDEO_EXTENSIONS.has(ext);
};

export const isImageArtifact = (artifact: Artifact): boolean => {
  const mime = artifactMime(artifact);
  return mime.startsWith("image/");
};

const isCsvArtifact = (artifact: Artifact): boolean => {
  const mime = artifactMime(artifact);
  if (mime === "text/csv" || mime === "application/csv" || mime.endsWith("+csv")) return true;
  const ext = artifactExtension(artifact.absolute_path || artifact.name || "");
  return CSV_EXTENSIONS.has(ext);
};

export const getArtifactPreviewKind = (artifact: Artifact): ArtifactPreviewKind => {
  const documentFormat = getArtifactDocumentFormat(artifact);
  if (isImageArtifact(artifact)) return "image";
  if (isVideoArtifact(artifact)) return "video";
  if (isCsvArtifact(artifact)) return "none";
  if (documentFormat === "markdown" || documentFormat === "mdx") return "markdown";
  if (documentFormat === "text") return "text";
  return "none";
};

export const isPreviewableArtifact = (artifact: Artifact): boolean =>
  getArtifactPreviewKind(artifact) !== "none";
