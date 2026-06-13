import { uploadBlob, type MessageAttachment } from "../api/client";

const IMAGE_EXT_RE = /\.(avif|bmp|gif|jpe?g|png|tiff?|webp)$/i;
const SVG_EXT_RE = /\.svg$/i;
export const MAX_MESSAGE_IMAGE_ATTACHMENT_BYTES = 25 * 1024 * 1024;
const MAX_MESSAGE_IMAGE_ATTACHMENT_MIB = MAX_MESSAGE_IMAGE_ATTACHMENT_BYTES / (1024 * 1024);

export function isImageFile(file: File): boolean {
  const name = (file.name || "").trim();
  const type = (file.type || "").toLowerCase().split(";")[0]?.trim() ?? "";
  if (type === "image/svg+xml" || SVG_EXT_RE.test(name)) return false;
  if (type.startsWith("image/")) return true;
  if (!name) return false;
  return IMAGE_EXT_RE.test(name);
}

export function imageAttachmentSizeError(name?: string | null): string {
  const label = (name ?? "").trim();
  const prefix = label ? `${label} is too large.` : "Image attachment is too large.";
  return `${prefix} Image attachments must be ${MAX_MESSAGE_IMAGE_ATTACHMENT_MIB} MiB or smaller.`;
}

function assertSupportedImageFileSize(file: File): void {
  if (file.size > MAX_MESSAGE_IMAGE_ATTACHMENT_BYTES) {
    throw new Error(imageAttachmentSizeError(file.name));
  }
}

async function hasSvgPayload(file: File): Promise<boolean> {
  const prefix = await readFileTextPrefix(file);
  const normalized = prefix.replace(/^\uFEFF/, "").trimStart().toLowerCase();
  return normalized.startsWith("<svg") || normalized.includes("<svg");
}

function readFileTextPrefix(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onerror = () => reject(reader.error ?? new Error("Failed to read image attachment prefix"));
    reader.onload = () => resolve(typeof reader.result === "string" ? reader.result : "");
    reader.readAsText(file.slice(0, 1024));
  });
}

export async function imageFilesToBlobRefAttachments(files: File[]): Promise<MessageAttachment[]> {
  const imageFiles = files.filter(isImageFile);
  for (const file of imageFiles) {
    assertSupportedImageFileSize(file);
  }

  const next: MessageAttachment[] = [];
  for (const file of imageFiles) {
    if (await hasSvgPayload(file)) continue;
    const uploaded = await uploadBlob(file);
    next.push({
      kind: "image_ref",
      blob_id: uploaded.blob_id,
      mime_type: uploaded.mime_type,
      name: uploaded.name ?? file.name,
    });
  }
  return next;
}

export async function imageFilesToMessageAttachments(files: File[]): Promise<MessageAttachment[]> {
  return imageFilesToBlobRefAttachments(files);
}
