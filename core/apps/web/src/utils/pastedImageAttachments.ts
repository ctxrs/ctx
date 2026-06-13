import type { MessageAttachment } from "../api/client";
import { imageFilesToMessageAttachments, isImageFile } from "./messageAttachments";
import {
  extractFirstUrlFromTransfer,
  imageAttachmentsFromUrl,
} from "./droppedImageAttachments";

const MIME_EXTENSION_BY_TYPE: Record<string, string> = {
  "image/avif": "avif",
  "image/bmp": "bmp",
  "image/gif": "gif",
  "image/jpeg": "jpg",
  "image/png": "png",
  "image/tiff": "tiff",
  "image/webp": "webp",
};

function makeClipboardImageName(file: File, index: number): string {
  const type = String(file.type || "").trim().toLowerCase();
  const ext = MIME_EXTENSION_BY_TYPE[type] ?? "png";
  return `pasted-image-${index + 1}.${ext}`;
}

function normalizeClipboardImageFileName(file: File, index: number): File {
  const currentName = String(file.name || "").trim();
  if (currentName) return file;
  return new File([file], makeClipboardImageName(file, index), {
    type: file.type,
    lastModified: file.lastModified,
  });
}

export function extractImageFilesFromClipboardTransfer(transfer: DataTransfer | null): File[] {
  if (!transfer) return [];

  const directFiles = Array.from(transfer.files ?? []).filter(isImageFile);
  if (directFiles.length > 0) {
    return directFiles.map((file, index) => normalizeClipboardImageFileName(file, index));
  }

  const next: File[] = [];
  for (const item of Array.from(transfer.items ?? [])) {
    if (item.kind !== "file") continue;
    const file = item.getAsFile?.();
    if (!file || !isImageFile(file)) continue;
    next.push(file);
  }
  return next.map((file, index) => normalizeClipboardImageFileName(file, index));
}

function clipboardImageUrl(transfer: DataTransfer | null): string | null {
  return extractFirstUrlFromTransfer(transfer, {
    allowUriList: true,
    allowUriListWhenPlainTextExists: false,
    allowHtmlImageSrc: true,
    allowPlainTextLocalImageRefs: true,
    allowPlainTextRemoteUrls: false,
  });
}

export function clipboardHasImagePayload(transfer: DataTransfer | null): boolean {
  return extractImageFilesFromClipboardTransfer(transfer).length > 0 || clipboardImageUrl(transfer) !== null;
}

export async function imageAttachmentsFromClipboardTransfer(
  transfer: DataTransfer | null,
): Promise<MessageAttachment[]> {
  const files = extractImageFilesFromClipboardTransfer(transfer);
  if (files.length > 0) return imageFilesToMessageAttachments(files);

  const url = clipboardImageUrl(transfer);
  if (!url) return [];
  return imageAttachmentsFromUrl(url);
}
