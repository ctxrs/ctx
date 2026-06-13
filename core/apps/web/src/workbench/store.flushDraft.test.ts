import { describe, expect, it, vi } from "vitest";

vi.mock("./persistence", async () => {
  const actual = await vi.importActual<typeof import("./persistence")>("./persistence");
  return {
    ...actual,
    saveWorkbenchDraftV1: vi.fn(async () => {}),
  };
});

describe("WorkbenchStore.flushDraft", () => {
  const imageAttachment = {
    kind: "image_ref" as const,
    blob_id: "blob-1",
    mime_type: "image/png",
    name: "mock.png",
  };

  it("persists the current draft immediately and cancels the pending debounce", async () => {
    vi.useFakeTimers();
    try {
      const persistence = await import("./persistence");
      const saveWorkbenchDraftV1 = vi.mocked(persistence.saveWorkbenchDraftV1);
      const { WorkbenchStore } = await import("./store");
      const store = new WorkbenchStore("ws-1");
      store.setDraft("k1", { text: "hello", modeId: "default", attachments: [imageAttachment] });
      store.setDraft("k1", { text: "", modeId: "default", attachments: [imageAttachment] });

      expect(saveWorkbenchDraftV1).toHaveBeenCalledTimes(0);

      await store.flushDraft("k1");

      expect(saveWorkbenchDraftV1).toHaveBeenCalledTimes(1);
      const [_workspaceId, _key, draft] = saveWorkbenchDraftV1.mock.calls[0] ?? [];
      expect(draft).toEqual(
        expect.objectContaining({
          text: "",
          modeId: "default",
          attachments: [imageAttachment],
          updatedAtMs: expect.any(Number),
        }),
      );

      vi.runAllTimers();
      expect(saveWorkbenchDraftV1).toHaveBeenCalledTimes(1);
    } finally {
      vi.useRealTimers();
    }
  }, 20000);
});
