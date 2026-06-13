type BlobLike = {
  text?: () => Promise<string>;
  arrayBuffer?: () => Promise<ArrayBuffer>;
  slice?: (...args: unknown[]) => unknown;
  size?: number;
};

const asBlobLike = (data: unknown): BlobLike | null => {
  if (!data || typeof data !== "object") return null;
  const candidate = data as BlobLike;
  const hasBlobShape =
    typeof candidate.text === "function" ||
    typeof candidate.arrayBuffer === "function" ||
    (typeof candidate.slice === "function" && typeof candidate.size === "number") ||
    Object.prototype.toString.call(data) === "[object Blob]" ||
    Object.prototype.toString.call(data) === "[object File]";
  return hasBlobShape ? candidate : null;
};

export async function parseWsJson(data: unknown): Promise<unknown | null> {
  let text: string | null = null;

  if (typeof data === "string") {
    text = data;
  } else if (asBlobLike(data)) {
    // Blob-like (WebSocket implementations vary; some WebViews deliver text frames as Blob).
    const blobLike = asBlobLike(data);
    if (!blobLike) return null;
    if (typeof blobLike.text === "function") {
      text = await blobLike.text();
    } else if (typeof blobLike.arrayBuffer === "function") {
      const ab = await blobLike.arrayBuffer();
      text = new TextDecoder().decode(new Uint8Array(ab));
    } else if (typeof globalThis.Response === "function") {
      text = await new globalThis.Response(blobLike as Blob).text();
    } else if (typeof globalThis.FileReader === "function") {
      text = await new Promise<string>((resolve, reject) => {
        const reader = new globalThis.FileReader();
        reader.onload = () => resolve(String(reader.result ?? ""));
        reader.onerror = () => reject(reader.error ?? new Error("Failed to read Blob"));
        reader.readAsText(blobLike as Blob);
      });
    } else {
      return null;
    }
  } else if (data instanceof ArrayBuffer) {
    text = new TextDecoder().decode(new Uint8Array(data));
  } else if (ArrayBuffer.isView(data)) {
    text = new TextDecoder().decode(new Uint8Array(data.buffer, data.byteOffset, data.byteLength));
  } else {
    return null;
  }

  if (text === null) return null;

  try {
    return JSON.parse(text);
  } catch {
    return null;
  }
}
