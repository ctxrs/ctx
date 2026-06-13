import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  appendDownloadAttributionIdToUrl,
  consumePendingDownloadAttributionId,
  getPendingDownloadAttributionId,
  normalizeDownloadAttributionId,
  setPendingDownloadAttributionId,
} from "./downloadAttribution";
import { desktopStorageBatch, desktopStorageGet, isDesktopApp } from "../desktop";

const { isDesktopAppMock, desktopStorageGetMock, desktopStorageBatchMock } = vi.hoisted(() => ({
  isDesktopAppMock: vi.fn(() => false),
  desktopStorageGetMock: vi.fn(async () => null),
  desktopStorageBatchMock: vi.fn(async () => {}),
}));

vi.mock("../desktop", () => ({
  isDesktopApp: isDesktopAppMock,
  desktopStorageGet: desktopStorageGetMock,
  desktopStorageBatch: desktopStorageBatchMock,
}));

describe("download attribution helpers", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(isDesktopApp).mockReturnValue(false);
    vi.mocked(desktopStorageGet).mockResolvedValue(null);
    vi.mocked(desktopStorageBatch).mockResolvedValue();
    window.history.replaceState({}, "", "/");
    window.localStorage.clear();
  });

  it("normalizes valid IDs and rejects invalid values", () => {
    expect(normalizeDownloadAttributionId("abc-123")).toBe("abc-123");
    expect(normalizeDownloadAttributionId("a:b.c_d-1")).toBe("a:b.c_d-1");
    expect(normalizeDownloadAttributionId("")).toBeNull();
    expect(normalizeDownloadAttributionId("a b")).toBeNull();
  });

  it("appends ctx_download_id query param to artifact URLs", () => {
    const url = appendDownloadAttributionIdToUrl(
      "https://api.example/functions/v1/download/stable/1.0.1/app.exe",
      "dl-123",
    );
    expect(url).toBe(
      "https://api.example/functions/v1/download/stable/1.0.1/app.exe?ctx_download_id=dl-123",
    );
  });

  it("consumes ctx_download_id from URL query once", async () => {
    window.history.replaceState({}, "", "/?ctx_download_id=query-42");
    const consumed = await consumePendingDownloadAttributionId();
    expect(consumed).toBe("query-42");
    expect(window.location.search).toBe("");
    const next = await consumePendingDownloadAttributionId();
    expect(next).toBeNull();
  });

  it("consumes pending local storage ID once", async () => {
    const stored = await setPendingDownloadAttributionId("pending-777");
    expect(stored).toBe(true);
    const consumed = await consumePendingDownloadAttributionId();
    expect(consumed).toBe("pending-777");
    const next = await consumePendingDownloadAttributionId();
    expect(next).toBeNull();
  });

  it("reads pending local storage ID without consuming it", async () => {
    const stored = await setPendingDownloadAttributionId("peek-001");
    expect(stored).toBe(true);
    const peek = await getPendingDownloadAttributionId();
    expect(peek).toBe("peek-001");
    const consumed = await consumePendingDownloadAttributionId();
    expect(consumed).toBe("peek-001");
  });

  it("consumes local fallback ID in desktop mode when desktop storage read fails", async () => {
    vi.mocked(isDesktopApp).mockReturnValue(true);
    vi.mocked(desktopStorageBatch)
      .mockRejectedValueOnce(new Error("desktop write unavailable"))
      .mockResolvedValue(undefined);
    const stored = await setPendingDownloadAttributionId("desktop-fallback-1");
    expect(stored).toBe(true);

    vi.mocked(desktopStorageGet).mockRejectedValueOnce(new Error("desktop read unavailable"));
    const consumed = await consumePendingDownloadAttributionId();
    expect(consumed).toBe("desktop-fallback-1");
  });
});
