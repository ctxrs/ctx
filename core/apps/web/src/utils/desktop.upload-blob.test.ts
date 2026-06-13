import { beforeEach, describe, expect, it, vi } from "vitest";

const invokeMock = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/core", () => ({
  invoke: invokeMock,
}));

describe("desktopUploadBlob", () => {
  beforeEach(() => {
    vi.resetModules();
    invokeMock.mockReset();
  });

  it("sends the generated desktop upload request shape through the req envelope", async () => {
    invokeMock.mockResolvedValue({
      blob_id: "blob-1",
      mime_type: "image/png",
    });

    const desktop = await import("./desktop");

    await desktop.desktopUploadBlob({
      bytes: [1, 2, 3],
      mime_type: "image/png",
      name: "paste.png",
    });

    expect(invokeMock).toHaveBeenCalledWith("desktop_upload_blob", {
      req: {
        bytes: [1, 2, 3],
        mime_type: "image/png",
        name: "paste.png",
      },
    });
  });
});
