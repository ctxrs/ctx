import type { MessageAttachment } from "../api/client";
import { imageFilesToMessageAttachments, isImageFile } from "./messageAttachments";
import { desktopReadBinaryFile, isDesktopApp } from "./desktop";
import { inferImageMimeTypeFromName } from "./imageMime";

const WINDOWS_DRIVE_PATH_RE = /^\/[A-Za-z]:\//;

function basename(input: string): string {
  const normalized = input.replace(/\\/g, "/");
  const value = normalized.split("/").filter(Boolean).pop() ?? "";
  return value.trim();
}

function fileNameFromUrl(url: string): string {
  try {
    const parsed = new URL(url, window.location.href);
    const value = basename(parsed.pathname);
    return value || "image";
  } catch {
    return "image";
  }
}

function extensionMimeType(name: string): string {
  return inferImageMimeTypeFromName(name) ?? "";
}

function pathLooksLikeImage(path: string): boolean {
  const name = basename(path);
  return Boolean(name) && Boolean(extensionMimeType(name));
}

function normalizeFileUrlToPath(url: string): string | null {
  try {
    const parsed = new URL(url);
    if (parsed.protocol !== "file:") return null;
    let pathname = decodeURIComponent(parsed.pathname || "");
    if (WINDOWS_DRIVE_PATH_RE.test(pathname)) pathname = pathname.slice(1);
    if (!pathname.trim()) return null;
    return pathname;
  } catch {
    return null;
  }
}

async function readImageFileFromDesktopPath(path: string): Promise<File | null> {
  if (!isDesktopApp()) return null;
  if (!pathLooksLikeImage(path)) return null;
  try {
    const response = await desktopReadBinaryFile({ path });
    const name = basename(response.path) || basename(path) || "image";
    const file = new File([Uint8Array.from(response.bytes)], name, {
      type: extensionMimeType(name) || "",
    });
    return isImageFile(file) ? file : null;
  } catch {
    return null;
  }
}

async function readImageFileFromUrl(url: string, suggestedName?: string | null): Promise<File | null> {
  try {
    const response = await fetch(url);
    if (!response.ok) return null;
    const blob = await response.blob();
    const fallbackName = (suggestedName ?? fileNameFromUrl(url)).trim() || "image";
    const mimeType = blob.type || extensionMimeType(fallbackName) || "";
    const file = new File([blob], fallbackName, { type: mimeType });
    return isImageFile(file) ? file : null;
  } catch {
    return null;
  }
}

type TransferImageUrlOptions = {
  allowUriList?: boolean;
  allowUriListWhenPlainTextExists?: boolean;
  allowHtmlImageSrc?: boolean;
  allowPlainTextLocalImageRefs?: boolean;
  allowPlainTextRemoteUrls?: boolean;
};

function firstUriListEntry(raw: string): string | null {
  for (const line of raw.split("\n")) {
    const value = line.trim();
    if (!value || value.startsWith("#")) continue;
    return value;
  }
  return null;
}

function htmlImageSrc(html: string): string | null {
  const match = html.match(/<img[^>]*\ssrc=("([^"]+)"|'([^']+)'|([^\s>]+))/i);
  return (match?.[2] ?? match?.[3] ?? match?.[4] ?? "").trim() || null;
}

export function extractFilesFromTransfer(transfer: DataTransfer | null): File[] {
  if (!transfer) return [];
  const out: File[] = [];
  const files = transfer.files ? Array.from(transfer.files) : [];
  out.push(...files);
  const items = transfer.items;
  if (out.length === 0 && items && items.length > 0) {
    for (const item of Array.from(items)) {
      if (item.kind !== "file") continue;
      const file = item.getAsFile?.();
      if (file) out.push(file);
    }
  }
  return out;
}

export function extractFirstUrlFromTransfer(
  transfer: DataTransfer | null,
  options: TransferImageUrlOptions = {},
): string | null {
  if (!transfer) return null;
  const {
    allowUriList = true,
    allowUriListWhenPlainTextExists = true,
    allowHtmlImageSrc = true,
    allowPlainTextLocalImageRefs = true,
    allowPlainTextRemoteUrls = true,
  } = options;

  const text = transfer.getData?.("text/plain") ?? "";
  const trimmedText = text.trim();
  const hasPlainText = trimmedText.length > 0;

  const uriRaw = (transfer.getData?.("text/uri-list") ?? "").trim();
  if (allowUriList && (!hasPlainText || allowUriListWhenPlainTextExists)) {
    const uri = firstUriListEntry(uriRaw);
    if (uri) return uri;
  }

  const html = (transfer.getData?.("text/html") ?? "").trim();
  if (allowHtmlImageSrc && html) {
    const src = htmlImageSrc(html);
    if (src) return src;
  }

  if (
    allowPlainTextLocalImageRefs
    && trimmedText
    && /^(data:image\/|blob:|file:)/i.test(trimmedText)
  ) {
    return trimmedText;
  }
  if (allowPlainTextRemoteUrls && trimmedText && /^https?:/i.test(trimmedText)) return trimmedText;
  return null;
}

export async function imageAttachmentsFromUrl(url: string): Promise<MessageAttachment[]> {
  const fileUrlPath = normalizeFileUrlToPath(url);
  if (fileUrlPath) return imageAttachmentsFromPaths([fileUrlPath]);

  const file = await readImageFileFromUrl(url);
  if (!file) return [];
  return imageFilesToMessageAttachments([file]);
}

export async function imageAttachmentsFromTransfer(transfer: DataTransfer | null): Promise<MessageAttachment[]> {
  const files = extractFilesFromTransfer(transfer);
  if (files.length > 0) return imageFilesToMessageAttachments(files);

  const url = extractFirstUrlFromTransfer(transfer);
  if (!url) return [];
  return imageAttachmentsFromUrl(url);
}

export async function imageAttachmentsFromPaths(paths: string[]): Promise<MessageAttachment[]> {
  const diagnostics = (
    globalThis as typeof globalThis & {
      __ctxDroppedImagePathsCalls?: Array<{
        paths: string[];
        resolvedPaths: string[];
        fileCount: number;
      }>;
    }
  ).__ctxDroppedImagePathsCalls ?? [];
  const files: File[] = [];
  const resolvedPaths: string[] = [];
  for (const path of paths) {
    const trimmed = path.trim();
    if (!trimmed) continue;
    resolvedPaths.push(trimmed);
    const file = await readImageFileFromDesktopPath(trimmed);
    if (file) files.push(file);
  }
  diagnostics.push({ paths: [...paths], resolvedPaths, fileCount: files.length });
  (
    globalThis as typeof globalThis & {
      __ctxDroppedImagePathsCalls?: Array<{
        paths: string[];
        resolvedPaths: string[];
        fileCount: number;
      }>;
    }
  ).__ctxDroppedImagePathsCalls = diagnostics;
  if (files.length === 0) return [];
  return imageFilesToMessageAttachments(files);
}
