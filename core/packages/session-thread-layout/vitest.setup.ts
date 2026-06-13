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

ensureStorage("localStorage");
ensureStorage("sessionStorage");

if (typeof globalThis.OffscreenCanvas === "undefined") {
  class OffscreenCanvasStub {
    constructor(
      public width: number,
      public height: number,
    ) {}

    getContext() {
      return createCanvasContextStub();
    }
  }

  Object.defineProperty(globalThis, "OffscreenCanvas", {
    configurable: true,
    writable: true,
    value: OffscreenCanvasStub,
  });
}

if (typeof HTMLCanvasElement !== "undefined") {
  Object.defineProperty(HTMLCanvasElement.prototype, "getContext", {
    configurable: true,
    value: () => createCanvasContextStub(),
  });
}
