import { afterEach, describe, expect, it, vi } from "vitest";
import { describeClipboardCopyFailure, tryCopyTextToClipboard } from "./clipboard";

type MutableNavigator = Omit<Navigator, "clipboard"> & {
  clipboard?: Partial<Clipboard> & Pick<Clipboard, "writeText">;
};

const originalExecCommand = document.execCommand;
const previousTauri = (globalThis as typeof globalThis & { __TAURI__?: unknown }).__TAURI__;

afterEach(() => {
  document.execCommand = originalExecCommand;
  Object.defineProperty(navigator, "clipboard", {
    value: undefined,
    configurable: true,
  });
  const globals = globalThis as typeof globalThis & { __TAURI__?: unknown };
  if (previousTauri === undefined) {
    delete globals.__TAURI__;
  } else {
    globals.__TAURI__ = previousTauri;
  }
  vi.restoreAllMocks();
});

describe("clipboard helpers", () => {
  it("classifies NotAllowedError as blocked", async () => {
    document.execCommand = vi.fn(() => false);
    const navClipboard: MutableNavigator["clipboard"] = {
      writeText: vi.fn(async () => {
        throw new DOMException("Write permission denied.", "NotAllowedError");
      }),
    };
    Object.defineProperty(navigator, "clipboard", {
      value: navClipboard,
      configurable: true,
    });

    await expect(tryCopyTextToClipboard("hello")).resolves.toEqual({
      ok: false,
      reason: "blocked",
      error: expect.any(DOMException),
    });
  });

  it("classifies unexpected clipboard errors as failed", async () => {
    document.execCommand = vi.fn(() => false);
    const navClipboard: MutableNavigator["clipboard"] = {
      writeText: vi.fn(async () => {
        throw new Error("Clipboard bridge crashed.");
      }),
    };
    Object.defineProperty(navigator, "clipboard", {
      value: navClipboard,
      configurable: true,
    });

    await expect(tryCopyTextToClipboard("hello")).resolves.toEqual({
      ok: false,
      reason: "failed",
      error: expect.any(Error),
    });
  });

  it("uses desktop-aware blocked guidance", () => {
    expect(describeClipboardCopyFailure({ ok: false, reason: "blocked" }, { action: "copy transcript to the clipboard" })).toBe(
      "Clipboard access is blocked. Use HTTPS or copy manually.",
    );

    (globalThis as typeof globalThis & { __TAURI__?: unknown }).__TAURI__ = {};

    expect(describeClipboardCopyFailure({ ok: false, reason: "blocked" }, { action: "copy transcript to the clipboard" })).toBe(
      "Clipboard access is blocked.",
    );
  });
});
