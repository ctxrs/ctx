import { afterEach, describe, expect, it, vi } from "vitest";

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

import {
  clipboardHasImagePayload,
  extractImageFilesFromClipboardTransfer,
  imageAttachmentsFromClipboardTransfer,
} from "./pastedImageAttachments";

describe("pastedImageAttachments", () => {
  afterEach(() => {
    vi.restoreAllMocks();
    uploadBlobMock.mockImplementation(async (file: File) => ({
      blob_id: `blob-${file.name || "image"}`,
      mime_type: file.type || "image/png",
      name: file.name || "image.png",
    }));
  });

  it("extracts image files from clipboard files", async () => {
    const transfer = {
      files: [
        new File([Uint8Array.from([137, 80, 78, 71])], "screenshot.png", {
          type: "image/png",
        }),
      ],
      items: [],
    } as unknown as DataTransfer;

    expect(clipboardHasImagePayload(transfer)).toBe(true);
    expect(extractImageFilesFromClipboardTransfer(transfer).map((file) => file.name)).toEqual([
      "screenshot.png",
    ]);

    const attachments = await imageAttachmentsFromClipboardTransfer(transfer);
    expect(attachments).toHaveLength(1);
    expect(attachments[0]).toMatchObject({
      kind: "image_ref",
      mime_type: "image/png",
      name: "screenshot.png",
      blob_id: "blob-screenshot.png",
    });
  });

  it("falls back to clipboard items when files are empty", async () => {
    const transfer = {
      files: [],
      items: [
        {
          kind: "file",
          type: "image/jpeg",
          getAsFile: () =>
            new File([Uint8Array.from([255, 216, 255, 224])], "clipboard.jpg", {
              type: "image/jpeg",
            }),
        },
      ],
    } as unknown as DataTransfer;

    expect(clipboardHasImagePayload(transfer)).toBe(true);
    const attachments = await imageAttachmentsFromClipboardTransfer(transfer);
    expect(attachments).toHaveLength(1);
    expect(attachments[0]).toMatchObject({
      kind: "image_ref",
      mime_type: "image/jpeg",
      name: "clipboard.jpg",
      blob_id: "blob-clipboard.jpg",
    });
  });

  it("synthesizes a stable name for unnamed clipboard images", () => {
    const transfer = {
      files: [new File([Uint8Array.from([137, 80, 78, 71])], "", { type: "image/png" })],
      items: [],
    } as unknown as DataTransfer;

    const files = extractImageFilesFromClipboardTransfer(transfer);
    expect(files).toHaveLength(1);
    expect(files[0]?.name).toBe("pasted-image-1.png");
  });

  it("ignores non-image clipboard payloads", async () => {
    const transfer = {
      files: [new File(["hello"], "notes.txt", { type: "text/plain" })],
      items: [
        {
          kind: "string",
          type: "text/plain",
          getAsFile: () => null,
        },
      ],
    } as unknown as DataTransfer;

    expect(clipboardHasImagePayload(transfer)).toBe(false);
    expect(extractImageFilesFromClipboardTransfer(transfer)).toEqual([]);
    await expect(imageAttachmentsFromClipboardTransfer(transfer)).resolves.toEqual([]);
  });

  it("accepts pasted html image sources", async () => {
    vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(Uint8Array.from([137, 80, 78, 71]), {
        status: 200,
        headers: { "Content-Type": "image/png" },
      }),
    );

    const transfer = {
      files: [],
      items: [],
      getData(type: string) {
        if (type === "text/html") return '<img src="https://example.test/copied.png" alt="copied image">';
        if (type === "text/plain") return "copied image";
        return "";
      },
    } as unknown as DataTransfer;

    expect(clipboardHasImagePayload(transfer)).toBe(true);
    const attachments = await imageAttachmentsFromClipboardTransfer(transfer);
    expect(attachments).toHaveLength(1);
    expect(attachments[0]).toMatchObject({
      kind: "image_ref",
      mime_type: "image/png",
      name: "copied.png",
      blob_id: "blob-copied.png",
    });
    expect(globalThis.fetch).toHaveBeenCalledWith("https://example.test/copied.png");
  });

  it("accepts uri-list image sources when there is no plain text", async () => {
    vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(Uint8Array.from([137, 80, 78, 71]), {
        status: 200,
        headers: { "Content-Type": "image/png" },
      }),
    );

    const transfer = {
      files: [],
      items: [],
      getData(type: string) {
        if (type === "text/uri-list") return "https://example.test/copied.png";
        return "";
      },
    } as unknown as DataTransfer;

    expect(clipboardHasImagePayload(transfer)).toBe(true);
    const attachments = await imageAttachmentsFromClipboardTransfer(transfer);
    expect(attachments).toHaveLength(1);
    expect(attachments[0]?.name).toBe("copied.png");
  });
});
