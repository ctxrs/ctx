import { beforeEach, describe, expect, it, vi } from "vitest";

const uploadBlobMock = vi.hoisted(() =>
  vi.fn(async (file: File) => ({
    blob_id: `blob-${file.name || "image"}`,
    mime_type: file.type || "image/png",
    name: file.name || "image.png",
  })),
);

vi.mock("../api/client", () => ({
  uploadBlob: uploadBlobMock,
}));

describe("messageAttachments", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
    uploadBlobMock.mockImplementation(async (file: File) => ({
      blob_id: `blob-${file.name || "image"}`,
      mime_type: file.type || "image/png",
      name: file.name || "image.png",
    }));
  });

  it("rejects oversized images before any uploads start", async () => {
    const {
      MAX_MESSAGE_IMAGE_ATTACHMENT_BYTES,
      imageAttachmentSizeError,
      imageFilesToBlobRefAttachments,
    } = await import("./messageAttachments");
    const small = new File([new Uint8Array([1, 2, 3])], "small.png", { type: "image/png" });
    const oversized = new File([new Uint8Array(MAX_MESSAGE_IMAGE_ATTACHMENT_BYTES + 1)], "large.png", {
      type: "image/png",
    });

    await expect(imageFilesToBlobRefAttachments([small, oversized])).rejects.toThrow(
      imageAttachmentSizeError("large.png"),
    );
    expect(uploadBlobMock).not.toHaveBeenCalled();
  });

  it("rejects svg image attachments", async () => {
    const { imageFilesToBlobRefAttachments, isImageFile } = await import("./messageAttachments");
    const svg = new File(["<svg></svg>"], "icon.svg", { type: "image/svg+xml" });
    const mislabeledSvg = new File(["<svg></svg>"], "mislabeled.png", { type: "image/png" });

    expect(isImageFile(svg)).toBe(false);
    expect(isImageFile(mislabeledSvg)).toBe(true);
    await expect(imageFilesToBlobRefAttachments([svg, mislabeledSvg])).resolves.toEqual([]);
    expect(uploadBlobMock).not.toHaveBeenCalled();
  });
});
