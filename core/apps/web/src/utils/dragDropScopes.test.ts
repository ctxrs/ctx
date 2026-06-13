import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { DesktopDragDropEvent } from "./desktop";

const desktopListenForDragDropMock = vi.hoisted(() => vi.fn());
let nativeDragDropHandler: ((event: DesktopDragDropEvent) => void) | null = null;

vi.mock("./desktop", () => ({
  desktopListenForDragDrop: vi.fn(async (handler: (event: DesktopDragDropEvent) => void) => {
    nativeDragDropHandler = handler;
    desktopListenForDragDropMock(handler);
    return () => {
      nativeDragDropHandler = null;
    };
  }),
}));

function setDevicePixelRatio(value: number) {
  Object.defineProperty(window, "devicePixelRatio", {
    value,
    configurable: true,
  });
}

describe("dragDropScopes native position routing", () => {
  const originalElementFromPoint = document.elementFromPoint;

  beforeEach(async () => {
    vi.resetModules();
    nativeDragDropHandler = null;
    desktopListenForDragDropMock.mockClear();
    document.body.innerHTML = "";
    setDevicePixelRatio(2);
  });

  afterEach(() => {
    document.body.innerHTML = "";
    if (originalElementFromPoint) {
      document.elementFromPoint = originalElementFromPoint;
    } else {
      Reflect.deleteProperty(document, "elementFromPoint");
    }
  });

  it("accepts native drop positions already expressed in logical coordinates", async () => {
    const { registerDropScope } = await import("./dragDropScopes");
    const scopeElement = document.createElement("div");
    document.body.appendChild(scopeElement);
    const onDropPaths = vi.fn();

    document.elementFromPoint = vi.fn((x: number, y: number) =>
      x === 120 && y === 80 ? scopeElement : null,
    ) as typeof document.elementFromPoint;

    registerDropScope({
      element: scopeElement,
      onDropPaths,
    });

    nativeDragDropHandler?.({
      type: "drop",
      paths: ["/tmp/example.png"],
      position: { x: 120, y: 80 },
    });

    expect(onDropPaths).toHaveBeenCalledWith(["/tmp/example.png"], { x: 120, y: 80 });
    expect(document.elementFromPoint).toHaveBeenCalledWith(120, 80);
  });

  it("falls back to physical-to-logical conversion when native positions are device-scaled", async () => {
    const { registerDropScope } = await import("./dragDropScopes");
    const scopeElement = document.createElement("div");
    document.body.appendChild(scopeElement);
    const onDropPaths = vi.fn();

    document.elementFromPoint = vi.fn((x: number, y: number) =>
      x === 100 && y === 50 ? scopeElement : null,
    ) as typeof document.elementFromPoint;

    registerDropScope({
      element: scopeElement,
      onDropPaths,
    });

    nativeDragDropHandler?.({
      type: "drop",
      paths: ["/tmp/example.png"],
      position: { x: 200, y: 100 },
    });

    expect(onDropPaths).toHaveBeenCalledWith(["/tmp/example.png"], { x: 200, y: 100 });
    expect(document.elementFromPoint).toHaveBeenNthCalledWith(1, 200, 100);
    expect(document.elementFromPoint).toHaveBeenNthCalledWith(2, 100, 50);
  });
});
