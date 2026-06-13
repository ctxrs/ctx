export type ReleaseArtifact = {
  url_path?: string;
  sha256?: string;
};

const asRecord = (value: unknown): Record<string, unknown> => {
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  return value as Record<string, unknown>;
};

const asArtifact = (value: unknown): ReleaseArtifact | null => {
  const rec = asRecord(value);
  if (typeof rec.url_path !== "string" || !rec.url_path.trim()) return null;
  return {
    url_path: rec.url_path,
    sha256: typeof rec.sha256 === "string" ? rec.sha256 : undefined,
  };
};

export const preferredDesktopArtifactFromManifest = (
  manifest: unknown,
  platformKey: string | null | undefined,
): ReleaseArtifact | null => {
  const key = String(platformKey ?? "").trim();
  if (!key) return null;
  const platforms = asRecord(asRecord(manifest).platforms);
  const platformEntry = asRecord(platforms[key]);
  const order = key.startsWith("linux-")
    ? ["appimage", "desktop", "deb"]
    : key.startsWith("macos-")
      ? ["desktop", "dmg", "zip"]
      : key.startsWith("windows-")
        ? ["desktop", "nsis", "msi", "exe", "zip"]
        : ["desktop", "appimage", "deb", "dmg", "msi", "nsis", "exe", "zip"];
  for (const kind of order) {
    const artifact = asArtifact(platformEntry[kind]);
    if (artifact) return artifact;
  }
  return null;
};

export const joinBaseAndPath = (baseUrl: string, urlPath: string): string => {
  return `${baseUrl.replace(/\/+$/, "")}${urlPath}`;
};
