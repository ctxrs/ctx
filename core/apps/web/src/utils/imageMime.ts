const IMAGE_EXTENSION_MIME_TYPES: Record<string, string> = {
  avif: "image/avif",
  bmp: "image/bmp",
  gif: "image/gif",
  jpeg: "image/jpeg",
  jpg: "image/jpeg",
  png: "image/png",
  tif: "image/tiff",
  tiff: "image/tiff",
  webp: "image/webp",
};

export function inferImageMimeTypeFromName(name: string): string | null {
  const ext = name.trim().toLowerCase().split(".").pop() ?? "";
  return IMAGE_EXTENSION_MIME_TYPES[ext] ?? null;
}

export function resolveImageMimeType(type?: string | null, name?: string | null): string {
  const normalized = (type ?? "").trim().toLowerCase().split(";")[0]?.trim() ?? "";
  if (normalized === "image/svg+xml") return "";
  if (normalized.startsWith("image/")) return normalized;
  return inferImageMimeTypeFromName(name ?? "") ?? "";
}
