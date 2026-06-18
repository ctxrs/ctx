import type { Artifact } from "../api/client";
import { artifactPathBaseName } from "../utils/artifactPaths";

export function formatBytes(bytes: number | null | undefined): string {
  if (!bytes || bytes <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB"];
  let idx = 0;
  let value = bytes;
  while (value >= 1024 && idx < units.length - 1) {
    value /= 1024;
    idx += 1;
  }
  return `${value.toFixed(value >= 10 || idx === 0 ? 0 : 1)} ${units[idx]}`;
}

export function displayName(artifact: Artifact): string {
  const name = (artifact.name ?? "").trim();
  if (name) return name;
  return artifactPathBaseName(artifact) || "artifact";
}

function sanitizeFileName(name: string): string {
  const raw = String(name ?? "").trim() || "artifact";
  const noBadChars = raw.replace(/[<>:"/\\|?*\u0000-\u001F]/g, "");
  const collapsed = noBadChars.replace(/\s+/g, "-").replace(/-+/g, "-").replace(/^-+|-+$/g, "");
  return (collapsed || "artifact").slice(0, 80);
}

export function artifactFileName(artifact: Artifact): string {
  const name = (artifact.name ?? "").trim();
  const pathBase = artifactPathBaseName(artifact);
  const pathExt = pathBase.includes(".") ? pathBase.split(".").pop() ?? "" : "";
  if (name && pathExt && !name.includes(".")) {
    return sanitizeFileName(`${name}.${pathExt}`);
  }
  return sanitizeFileName(name || pathBase || "artifact");
}

export function downloadArtifact(artifact: Artifact, url: string) {
  const a = document.createElement("a");
  a.href = url;
  a.download = artifactFileName(artifact);
  a.rel = "noopener";
  a.click();
}

export async function copyArtifactImage(artifact: Artifact, url: string) {
  if (!navigator.clipboard?.write || typeof window.ClipboardItem === "undefined") {
    throw new Error("Clipboard image copy is not supported in this browser.");
  }
  const resp = await fetch(url, { cache: "no-store" });
  if (!resp.ok) throw new Error("Failed to fetch image for clipboard.");
  const blob = await resp.blob();
  const type = blob.type || artifact.mime_type || "image/png";
  await navigator.clipboard.write([new window.ClipboardItem({ [type]: blob })]);
}
