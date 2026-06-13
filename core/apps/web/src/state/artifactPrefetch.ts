import { idToString, type Artifact } from "../api/client";
import { artifactResourceUrl } from "../api/browserResourceUrls";
import { isImageArtifact, isVideoArtifact } from "../utils/artifacts";

const readTunableInt = (key: string, fallback: number) => {
  try {
    if (typeof window === "undefined") return fallback;
    const raw = window.localStorage.getItem(key);
    if (!raw) return fallback;
    const parsed = Number.parseInt(raw, 10);
    if (!Number.isFinite(parsed) || parsed <= 0) return fallback;
    return parsed;
  } catch {
    return fallback;
  }
};

const PREFETCH_BUDGET_BYTES = readTunableInt(
  "contextArtifactPrefetchBudgetBytes",
  64 * 1024 * 1024,
);

const artifactByteSize = (artifact: Artifact): number => {
  const size = Number(artifact.bytes ?? 0);
  if (!Number.isFinite(size) || size <= 0) return 0;
  return size;
};

const shouldPrefetchArtifact = (artifact: Artifact): boolean => {
  if (artifact.missing) return false;
  return isImageArtifact(artifact) || isVideoArtifact(artifact);
};

class ArtifactPrefetcher {
  private sessionId: string | null = null;
  private prefetchedBytesById = new Map<string, number>();
  private inflight = new Map<string, AbortController>();
  private usedBytes = 0;
  private reservedBytes = 0;
  private generation = 0;

  reset(nextSessionId: string | null = null) {
    for (const controller of this.inflight.values()) {
      controller.abort();
    }
    this.inflight.clear();
    this.prefetchedBytesById.clear();
    this.usedBytes = 0;
    this.reservedBytes = 0;
    this.sessionId = nextSessionId;
    this.generation += 1;
  }

  prefetch(sessionId: string | null, artifacts: Artifact[], active: boolean) {
    if (!active || !sessionId) {
      if (this.sessionId) {
        this.reset(null);
      }
      return;
    }
    if (this.sessionId !== sessionId) {
      this.reset(sessionId);
    }
    const activeIds = new Set(
      artifacts
        .map((artifact) => idToString(artifact.id))
        .filter((id): id is string => Boolean(id)),
    );
    this.prunePrefetched(activeIds);
    const run = this.generation;
    void this.prefetchInternal(sessionId, artifacts, run);
  }

  private async prefetchInternal(sessionId: string, artifacts: Artifact[], run: number) {
    let remaining = PREFETCH_BUDGET_BYTES - this.usedBytes - this.reservedBytes;
    if (remaining <= 0) return;

    for (const artifact of artifacts) {
      if (this.sessionId !== sessionId || this.generation !== run) return;
      if (!shouldPrefetchArtifact(artifact)) continue;
      const artifactId = idToString(artifact.id);
      if (!artifactId) continue;
      if (this.prefetchedBytesById.has(artifactId) || this.inflight.has(artifactId)) continue;
      const size = artifactByteSize(artifact);
      if (!size || size > remaining) continue;
      remaining -= size;
      this.reservedBytes += size;
      let url: string;
      try {
        url = artifactResourceUrl(sessionId, artifactId);
      } catch {
        this.reservedBytes = Math.max(0, this.reservedBytes - size);
        continue;
      }
      void this.fetchArtifact(artifactId, url, size, sessionId, run);
    }
  }

  private async fetchArtifact(
    artifactId: string,
    url: string,
    size: number,
    sessionId: string,
    run: number,
  ) {
    const controller = new AbortController();
    this.inflight.set(artifactId, controller);
    try {
      const resp = await fetch(url, {
        cache: "force-cache",
        credentials: "same-origin",
        signal: controller.signal,
      });
      if (!resp.ok) return;
      await resp.arrayBuffer();
      if (this.sessionId === sessionId && this.generation === run) {
        this.prefetchedBytesById.set(artifactId, size);
        this.usedBytes += size;
      }
    } catch {
      // ignore prefetch errors
    } finally {
      this.inflight.delete(artifactId);
      this.reservedBytes = Math.max(0, this.reservedBytes - size);
    }
  }

  private prunePrefetched(activeIds: Set<string>) {
    for (const [artifactId, size] of this.prefetchedBytesById.entries()) {
      if (!activeIds.has(artifactId)) {
        this.prefetchedBytesById.delete(artifactId);
        this.usedBytes = Math.max(0, this.usedBytes - size);
      }
    }
  }
}

export const artifactPrefetcher = new ArtifactPrefetcher();
