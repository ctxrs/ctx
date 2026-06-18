import type { Artifact } from "../api/client";

export function artifactDisplayPath(artifact: Artifact): string {
  return (artifact.relative_path ?? artifact.absolute_path ?? artifact.name ?? "").trim();
}

export function artifactPathBaseName(artifact: Artifact): string {
  const path = artifactDisplayPath(artifact);
  return path.split(/[\\/]/).filter(Boolean).pop() ?? "";
}

export function artifactPathExtension(artifact: Artifact): string {
  const baseName = artifactPathBaseName(artifact);
  return baseName.includes(".") ? baseName.split(".").pop()?.toLowerCase() ?? "" : "";
}

export function artifactIdentityKey(artifact: Artifact): string {
  return artifact.id || artifactDisplayPath(artifact) || "artifact";
}
