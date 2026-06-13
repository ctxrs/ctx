import type { MessageAttachment } from "../api/client";
import { useBlobResourceUrlState } from "../api/useBrowserResourceUrl";

type MessageAttachmentImageProps = {
  attachment: MessageAttachment;
  className: string;
  alt: string;
  title?: string;
};

export function MessageAttachmentImage({
  attachment,
  className,
  alt,
  title,
}: MessageAttachmentImageProps) {
  const blobResource = useBlobResourceUrlState(attachment.kind === "image_ref" ? attachment.blob_id : null);
  if (attachment.kind !== "image" && attachment.kind !== "image_ref") return null;
  const src = attachment.kind === "image_ref"
    ? blobResource.url
    : `data:${attachment.mime_type};base64,${attachment.data_base64}`;
  if (attachment.kind === "image_ref" && blobResource.status === "unsupported") {
    return (
      <span className={className} role="img" aria-label={alt} title={title ?? blobResource.error}>
        Image unavailable
      </span>
    );
  }
  if (!src) return null;
  return <img className={className} src={src} alt={alt} title={title} />;
}
