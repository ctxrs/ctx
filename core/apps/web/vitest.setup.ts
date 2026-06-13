import "@testing-library/jest-dom";

function createMemoryStorage(): Storage {
  const store = new Map<string, string>();
  return {
    get length() {
      return store.size;
    },
    clear() {
      store.clear();
    },
    getItem(key: string) {
      return store.has(key) ? store.get(key) ?? null : null;
    },
    key(index: number) {
      if (!Number.isInteger(index) || index < 0 || index >= store.size) return null;
      return Array.from(store.keys())[index] ?? null;
    },
    removeItem(key: string) {
      store.delete(key);
    },
    setItem(key: string, value: string) {
      store.set(key, String(value));
    },
  };
}

function ensureStorage(name: "localStorage" | "sessionStorage"): void {
  const replacement = createMemoryStorage();
  Object.defineProperty(globalThis, name, {
    configurable: true,
    enumerable: true,
    writable: true,
    value: replacement,
  });
  if (typeof window !== "undefined") {
    Object.defineProperty(window, name, {
      configurable: true,
      enumerable: true,
      writable: true,
      value: replacement,
    });
  }
}

ensureStorage("localStorage");
ensureStorage("sessionStorage");

function createCanvasContextStub(): RenderingContext {
  const ctx = {
    fillStyle: "",
    fillRect: () => {},
    clearRect: () => {},
    getImageData: () => ({ data: new Uint8ClampedArray([0, 0, 0, 255]) }),
    putImageData: () => {},
    measureText: () => ({ width: 0 }),
  };
  return ctx as unknown as RenderingContext;
}

// Some UI dependencies (notably xterm) probe canvas APIs at import time.
// JSDOM doesn't implement canvas, so provide a small stub to keep unit tests
// focused on app behavior.
if (typeof HTMLCanvasElement !== "undefined") {
  Object.defineProperty(HTMLCanvasElement.prototype, "getContext", {
    configurable: true,
    value: () => createCanvasContextStub(),
  });
}

if (typeof HTMLElement !== "undefined") {
  if (typeof HTMLElement.prototype.hasPointerCapture !== "function") {
    Object.defineProperty(HTMLElement.prototype, "hasPointerCapture", {
      configurable: true,
      value: () => false,
    });
  }
  if (typeof HTMLElement.prototype.setPointerCapture !== "function") {
    Object.defineProperty(HTMLElement.prototype, "setPointerCapture", {
      configurable: true,
      value: () => {},
    });
  }
  if (typeof HTMLElement.prototype.releasePointerCapture !== "function") {
    Object.defineProperty(HTMLElement.prototype, "releasePointerCapture", {
      configurable: true,
      value: () => {},
    });
  }
}

if (typeof Element !== "undefined" && typeof Element.prototype.scrollIntoView !== "function") {
  Object.defineProperty(Element.prototype, "scrollIntoView", {
    configurable: true,
    value: () => {},
  });
}
